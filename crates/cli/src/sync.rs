//! `letterdesk sync`：把邮箱同步到本地 SQLite 邮箱。

use crate::SyncAccount;
use anyhow::{Context, Result, bail};
use mail_sync::credentials::{
    KeyringStore, forget as forget_credentials, load_gmail_password, load_outlook_tokens,
    save_gmail_password, save_outlook_tokens,
};
use mail_sync::gmail::{GmailConfig, sync_gmail_to_dir};
use mail_sync::maildir::sync_outlook_to_dir;
use mail_sync::oauth::{OutlookConfig, begin_login};
use mail_sync::outlook::OutlookClient;
use std::time::Duration;

pub fn run(account: SyncAccount) -> Result<()> {
    match account {
        SyncAccount::Outlook {
            out,
            client_id,
            base,
            headless_login,
            user,
            forget,
        } => {
            let store = KeyringStore;
            if forget {
                let Some(user) = &user else {
                    bail!("--forget 需要同时提供 --user");
                };
                forget_credentials(&store, user)?;
                eprintln!("已删除 {user} 保存的登录。");
                return Ok(());
            }
            let config = match &base {
                Some(b) => OutlookConfig::with_base(b, b, &client_id),
                None => OutlookConfig::microsoft(&client_id),
            };
            if headless_login && config.authority.contains("login.microsoftonline.com") {
                bail!("--headless-login 只能用于模拟服务；真实账号必须在浏览器中登录");
            }

            // 先试已保存的登录。
            let mut saved = None;
            if let Some(user) = &user {
                match load_outlook_tokens(&store, user) {
                    Ok(Some(tokens)) => {
                        let mut client = OutlookClient::new(config.clone(), tokens);
                        match client.me() {
                            Ok(_) => {
                                eprintln!("使用系统凭据管理器中保存的登录。");
                                saved = Some(client);
                            }
                            Err(e) => eprintln!("保存的登录已失效（{e}），需要重新登录。"),
                        }
                    }
                    Ok(None) => {}
                    Err(e) => eprintln!("提示：{e}，本次不使用保存的登录。"),
                }
            }

            let mut client = match saved {
                Some(c) => c,
                None => {
                    let login = begin_login(&config)?;
                    eprintln!("正在打开浏览器登录微软账号……");
                    eprintln!(
                        "如果浏览器没有自动打开，请手动访问：\n{}\n",
                        login.authorize_url
                    );
                    let url = login.authorize_url.clone();
                    if headless_login {
                        std::thread::spawn(move || {
                            let agent: ureq::Agent = ureq::Agent::config_builder().build().into();
                            let _ = agent.get(&url).call();
                        });
                    } else if let Err(e) = webbrowser::open(&url) {
                        eprintln!("无法自动打开浏览器：{e}");
                    }
                    let tokens = login
                        .finish(&config, Duration::from_secs(300))
                        .context("登录未完成")?;
                    OutlookClient::new(config, tokens)
                }
            };
            let (name, mail) = client.me()?;
            eprintln!("已登录：{name} <{mail}>");
            let stats = sync_outlook_to_dir(&mut client, &out)?;
            eprintln!(
                "同步完成：下载 {} 封，删除 {} 封，邮件保存在 {}",
                stats.downloaded,
                stats.removed,
                out.display()
            );
            // 刷新令牌每次使用后都会更换，同步后保存最新的。
            match save_outlook_tokens(&store, &mail, client.tokens()) {
                Ok(()) => eprintln!("登录已保存到系统凭据管理器，下次可用 --user {mail} 免登录。"),
                Err(e) => eprintln!("提示：{e}，登录未保存，下次需要重新登录。"),
            }
            eprintln!(
                "查看：cargo run -p letterdesk-desktop -- --mailbox {} --me {mail}",
                out.display()
            );
            Ok(())
        }
        SyncAccount::Gmail {
            out,
            user,
            days,
            all,
            mock,
            forget,
        } => {
            let store = KeyringStore;
            if forget {
                forget_credentials(&store, &user)?;
                eprintln!("已删除 {user} 保存的应用专用密码。");
                return Ok(());
            }
            // 应用专用密码：环境变量 → 系统凭据管理器 → 提示输入。
            let mut typed = false;
            let password = match std::env::var("LETTERDESK_GMAIL_APP_PASSWORD") {
                Ok(p) if !p.trim().is_empty() => p,
                _ => match load_gmail_password(&store, &user) {
                    Ok(Some(p)) => {
                        eprintln!("使用系统凭据管理器中保存的应用专用密码。");
                        p
                    }
                    other => {
                        if let Err(e) = other {
                            eprintln!("提示：{e}。");
                        }
                        eprint!("请输入 {user} 的应用专用密码：");
                        let mut line = String::new();
                        std::io::stdin().read_line(&mut line)?;
                        typed = true;
                        line.trim().to_string()
                    }
                },
            };
            let mut config = match &mock {
                Some(spec) => {
                    let parts: Vec<&str> = spec.split(':').collect();
                    let [host, imap, smtp] = parts.as_slice() else {
                        bail!("--mock 的格式是 主机:IMAP端口:SMTP端口");
                    };
                    GmailConfig::plain(host, imap.parse()?, smtp.parse()?, &user, &password)
                }
                None => GmailConfig::gmail(&user, &password),
            };
            config.since_days = if all { None } else { Some(days) };
            eprintln!(
                "正在同步 {user}（{}）……",
                if all {
                    "全部邮件".to_string()
                } else {
                    format!("最近 {days} 天")
                }
            );
            let stats = sync_gmail_to_dir(&config, &out)?;
            eprintln!(
                "同步完成：下载 {} 封，删除 {} 封，邮件保存在 {}",
                stats.downloaded,
                stats.removed,
                out.display()
            );
            if typed {
                match save_gmail_password(&store, &user, &password) {
                    Ok(()) => eprintln!("应用专用密码已保存到系统凭据管理器。"),
                    Err(e) => eprintln!("提示：{e}，密码未保存，下次需要重新输入。"),
                }
            }
            eprintln!(
                "查看：cargo run -p letterdesk-desktop -- --mailbox {} --me {user}",
                out.display()
            );
            Ok(())
        }
    }
}
