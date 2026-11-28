//! 账号配置（JSON 文件）与基于它的同步、回信操作，供命令行和桌面界面共用。
//!
//! 账号文件只保存“连到哪里”，不保存任何机密；令牌和应用专用密码来自系统凭据管理器
//! （Gmail 也可用环境变量 `LETTERDESK_GMAIL_APP_PASSWORD`）。
//!
//! ```json
//! { "kind": "gmail", "address": "lin@example.com", "mock": "127.0.0.1:1143:1025" }
//! { "kind": "outlook", "address": "lin@outlook.com", "client_id": "…", "base": "http://127.0.0.1:8901" }
//! ```

use crate::SyncError;
use crate::compose::ReplyDraft;
use crate::credentials::{
    SecretStore, load_gmail_password, load_outlook_tokens, save_outlook_tokens,
};
use crate::gmail::{GmailConfig, reply_gmail, sync_gmail_to_dir};
use crate::maildir::{SyncStats, sync_outlook_to_dir};
use crate::oauth::{OutlookConfig, begin_login};
use crate::outlook::OutlookClient;
use crate::send::SendReport;
use mail_core::{Email, Mailbox};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AccountConfig {
    Outlook {
        address: String,
        client_id: String,
        /// 登录与 Graph 的基地址；为空时连接真实的微软服务。
        #[serde(default)]
        base: Option<String>,
    },
    Gmail {
        address: String,
        /// 本机模拟服务（明文）：`主机:IMAP端口:SMTP端口`；为空时连接真实的 Gmail。
        #[serde(default)]
        mock: Option<String>,
        /// 只同步最近多少天；为空时 90 天，0 表示全部。
        #[serde(default)]
        days: Option<u32>,
    },
}

impl AccountConfig {
    pub fn load(path: &Path) -> Result<Self, SyncError> {
        let text = std::fs::read_to_string(path)?;
        serde_json::from_str(&text)
            .map_err(|e| SyncError::Protocol(format!("账号文件 {} 格式无效：{e}", path.display())))
    }

    pub fn address(&self) -> &str {
        match self {
            AccountConfig::Outlook { address, .. } | AccountConfig::Gmail { address, .. } => {
                address
            }
        }
    }

    fn outlook_config(&self) -> Option<OutlookConfig> {
        match self {
            AccountConfig::Outlook {
                client_id, base, ..
            } => Some(match base {
                Some(b) => OutlookConfig::with_base(b, b, client_id),
                None => OutlookConfig::microsoft(client_id),
            }),
            _ => None,
        }
    }

    fn gmail_config(&self, store: &dyn SecretStore) -> Result<Option<GmailConfig>, SyncError> {
        let AccountConfig::Gmail {
            address,
            mock,
            days,
        } = self
        else {
            return Ok(None);
        };
        let password = match std::env::var("LETTERDESK_GMAIL_APP_PASSWORD") {
            Ok(p) if !p.trim().is_empty() => p,
            _ => load_gmail_password(store, address)?.ok_or_else(|| {
                SyncError::Auth(format!(
                    "没有保存 {address} 的应用专用密码，请先运行 letterdesk sync gmail --user {address} 输入一次"
                ))
            })?,
        };
        let mut config = match mock {
            Some(spec) => {
                let parts: Vec<&str> = spec.split(':').collect();
                let [host, imap, smtp] = parts.as_slice() else {
                    return Err(SyncError::Protocol(
                        "mock 的格式是 主机:IMAP端口:SMTP端口".into(),
                    ));
                };
                let port = |p: &str| {
                    p.parse::<u16>()
                        .map_err(|_| SyncError::Protocol(format!("端口无效：{p}")))
                };
                GmailConfig::plain(host, port(imap)?, port(smtp)?, address, &password)
            }
            None => GmailConfig::gmail(address, &password),
        };
        config.since_days = match days {
            Some(0) => None,
            Some(d) => Some(*d),
            None => config.since_days,
        };
        Ok(Some(config))
    }

    /// 取得已登录的 Outlook 客户端：优先用保存的令牌；没有或失效时打开系统浏览器登录。
    fn outlook_client(&self, store: &dyn SecretStore) -> Result<OutlookClient, SyncError> {
        let config = self.outlook_config().expect("outlook account");
        if let Some(tokens) = load_outlook_tokens(store, self.address()).unwrap_or(None) {
            let mut client = OutlookClient::new(config.clone(), tokens);
            if client.me().is_ok() {
                return Ok(client);
            }
        }
        let login = begin_login(&config)?;
        let _ = webbrowser::open(&login.authorize_url);
        let tokens = login.finish(&config, Duration::from_secs(300))?;
        Ok(OutlookClient::new(config, tokens))
    }

    /// 同步到 `dir`。Outlook 同步后保存轮换后的令牌（保存失败不影响同步结果）。
    pub fn sync(&self, store: &dyn SecretStore, dir: &Path) -> Result<SyncStats, SyncError> {
        match self {
            AccountConfig::Outlook { .. } => {
                let mut client = self.outlook_client(store)?;
                let stats = sync_outlook_to_dir(&mut client, dir)?;
                let _ = save_outlook_tokens(store, self.address(), client.tokens());
                Ok(stats)
            }
            AccountConfig::Gmail { .. } => {
                let config = self.gmail_config(store)?.expect("gmail account");
                sync_gmail_to_dir(&config, dir)
            }
        }
    }

    /// 回复 `original`。`original_id` 是同步目录中的文件名（不含扩展名），Outlook 用它找到原信。
    pub fn reply(
        &self,
        store: &dyn SecretStore,
        original: &Email,
        original_id: &str,
        from_name: Option<&str>,
        draft: &ReplyDraft,
    ) -> Result<SendReport, SyncError> {
        match self {
            AccountConfig::Outlook { .. } => {
                let mut client = self.outlook_client(store)?;
                let report = client.reply_all(original_id, draft)?;
                let _ = save_outlook_tokens(store, self.address(), client.tokens());
                Ok(report)
            }
            AccountConfig::Gmail { .. } => {
                let config = self.gmail_config(store)?.expect("gmail account");
                let from = Mailbox::new(from_name, self.address());
                reply_gmail(&config, original, &from, draft)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::{MemoryStore, save_gmail_password};

    #[test]
    fn parses_account_files() {
        let g: AccountConfig = serde_json::from_str(
            r#"{"kind":"gmail","address":"lin@example.com","mock":"127.0.0.1:1143:1025","days":0}"#,
        )
        .unwrap();
        assert_eq!(g.address(), "lin@example.com");
        let o: AccountConfig = serde_json::from_str(
            r#"{"kind":"outlook","address":"lin@outlook.com","client_id":"abc"}"#,
        )
        .unwrap();
        assert!(matches!(o, AccountConfig::Outlook { base: None, .. }));
        assert!(
            serde_json::from_str::<AccountConfig>(r#"{"kind":"yahoo","address":"x"}"#).is_err()
        );
    }

    #[test]
    fn gmail_password_comes_from_store_and_days_zero_means_all() {
        // SAFETY: 测试中只读取该环境变量；确保它未设置，以便从存储中取得密码。
        unsafe { std::env::remove_var("LETTERDESK_GMAIL_APP_PASSWORD") };
        let account: AccountConfig = serde_json::from_str(
            r#"{"kind":"gmail","address":"lin@example.com","mock":"127.0.0.1:1:2","days":0}"#,
        )
        .unwrap();
        let store = MemoryStore::default();
        assert!(matches!(
            account.gmail_config(&store),
            Err(SyncError::Auth(_))
        ));
        save_gmail_password(&store, "lin@example.com", "abcd efgh ijkl mnop").unwrap();
        let config = account.gmail_config(&store).unwrap().unwrap();
        assert_eq!(
            (config.imap_port, config.smtp_port, config.tls),
            (1, 2, false)
        );
        assert_eq!(config.since_days, None);
        assert_eq!(config.app_password, "abcd efgh ijkl mnop");
    }
}
