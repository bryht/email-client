//! `letterdesk sync`：把邮箱同步到本地 `.eml` 目录。

use crate::SyncAccount;
use anyhow::{Context, Result, bail};
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
        } => {
            let config = match &base {
                Some(b) => OutlookConfig::with_base(b, b, &client_id),
                None => OutlookConfig::microsoft(&client_id),
            };
            if headless_login && config.authority.contains("login.microsoftonline.com") {
                bail!("--headless-login 只能用于模拟服务；真实账号必须在浏览器中登录");
            }
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
            let mut client = OutlookClient::new(config, tokens);
            let (name, mail) = client.me()?;
            eprintln!("已登录：{name} <{mail}>");
            let stats = sync_outlook_to_dir(&mut client, &out)?;
            eprintln!(
                "同步完成：下载 {} 封，删除 {} 封，邮件保存在 {}",
                stats.downloaded,
                stats.removed,
                out.display()
            );
            eprintln!(
                "查看：cargo run -p letterdesk-desktop -- --mailbox {} --me {mail}",
                out.display()
            );
            // 令牌目前只保存在内存中；计划存入系统凭据管理器，见 README。
            Ok(())
        }
    }
}
