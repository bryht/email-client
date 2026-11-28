//! Gmail：IMAP 收信同步与 SMTP 发信（应用专用密码登录）。
//!
//! 对外是同步接口，内部用一个单线程运行时驱动 async-imap，适合在后台线程中调用。
//!
//! Gmail 的特点与处理方式：
//! - 按 SPECIAL-USE 标记找到收件箱、已发送（\Sent）和草稿（\Drafts），不同步“所有邮件”，避免重复下载；
//! - 同一封邮件可能同时在多个文件夹里，用 `X-GM-MSGID` 作为存储键去重；
//! - 按 UID 增量同步，`UIDVALIDITY` 变化时重新同步该文件夹；
//! - 发信后 Gmail 会自动存入“已发送”，客户端不再另存。

use crate::SyncError;
use crate::compose::{ReplyDraft, build_reply};
use crate::maildir::SyncStats;
use crate::send::{SendReport, SendStatus};
use async_imap::Client;
use async_imap::types::NameAttribute;
use futures_util::StreamExt;
use mail_store::{ImapFolderState, Store};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;

/// Gmail 账号的连接配置。
#[derive(Debug, Clone)]
pub struct GmailConfig {
    pub imap_host: String,
    pub imap_port: u16,
    pub smtp_host: String,
    pub smtp_port: u16,
    /// 是否使用 TLS。真实 Gmail 必须为真；只有本机模拟服务可以为假。
    pub tls: bool,
    pub user: String,
    pub app_password: String,
    /// 只同步最近多少天的邮件；为空时同步全部。
    pub since_days: Option<u32>,
    /// 发送结果不明时，到“已发送”核对前的等待时间（Gmail 保存已发送需要一点时间）。
    pub verify_delay: std::time::Duration,
}

impl GmailConfig {
    /// 真实 Gmail：IMAP 993、SMTP 465，均为 TLS；默认同步最近 90 天。
    pub fn gmail(user: &str, app_password: &str) -> Self {
        Self {
            imap_host: "imap.gmail.com".into(),
            imap_port: 993,
            smtp_host: "smtp.gmail.com".into(),
            smtp_port: 465,
            tls: true,
            user: user.into(),
            app_password: app_password.into(),
            since_days: Some(90),
            verify_delay: std::time::Duration::from_secs(2),
        }
    }

    /// 本机模拟服务（明文）。
    pub fn plain(
        host: &str,
        imap_port: u16,
        smtp_port: u16,
        user: &str,
        app_password: &str,
    ) -> Self {
        Self {
            imap_host: host.into(),
            imap_port,
            smtp_host: host.into(),
            smtp_port,
            tls: false,
            user: user.into(),
            app_password: app_password.into(),
            since_days: None,
            verify_delay: std::time::Duration::ZERO,
        }
    }
}

fn runtime() -> Result<tokio::runtime::Runtime, SyncError> {
    Ok(tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?)
}

fn imap_err(e: impl std::fmt::Display) -> SyncError {
    SyncError::Protocol(format!("IMAP：{e}"))
}

/// 沿用旧目录的逻辑键：有 X-GM-MSGID 时用它（跨文件夹去重），否则用文件夹 + UIDVALIDITY + UID。
fn file_name(gm_msgid: Option<u64>, folder: &str, uid_validity: u32, uid: u32) -> String {
    match gm_msgid {
        Some(id) => format!("gm-{id}.eml"),
        None => {
            let safe: String = folder
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                .collect();
            format!("imap-{safe}-{uid_validity}-{uid}.eml")
        }
    }
}

/// `SINCE` 搜索使用的日期，如 `1-Mar-2026`。
fn since_date(days: u32) -> String {
    let date = (time::OffsetDateTime::now_utc() - time::Duration::days(days as i64)).date();
    let month: String = date.month().to_string().chars().take(3).collect();
    format!("{}-{month}-{}", date.day(), date.year())
}

/// 同步 Gmail 的收件箱、已发送和草稿到 `dir` 下的 SQLite 邮箱。
pub fn sync_gmail_to_dir(config: &GmailConfig, dir: &Path) -> Result<SyncStats, SyncError> {
    std::fs::create_dir_all(dir)?;
    let rt = runtime()?;
    rt.block_on(async {
        let tcp = TcpStream::connect((config.imap_host.as_str(), config.imap_port))
            .await
            .map_err(|e| {
                SyncError::Network(format!(
                    "无法连接 {}:{}：{e}",
                    config.imap_host, config.imap_port
                ))
            })?;
        if config.tls {
            let tls = tls_connect(&config.imap_host, tcp).await?;
            sync_session(Client::new(tls), config, dir).await
        } else {
            sync_session(Client::new(tcp), config, dir).await
        }
    })
}

async fn tls_connect(
    host: &str,
    tcp: TcpStream,
) -> Result<tokio_rustls::client::TlsStream<TcpStream>, SyncError> {
    let mut roots = tokio_rustls::rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = tokio_rustls::rustls::ClientConfig::builder_with_provider(Arc::new(
        tokio_rustls::rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| SyncError::Network(e.to_string()))?
    .with_root_certificates(roots)
    .with_no_client_auth();
    let name = tokio_rustls::rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|e| SyncError::Network(e.to_string()))?;
    tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect(name, tcp)
        .await
        .map_err(|e| SyncError::Network(format!("TLS 握手失败：{e}")))
}

async fn sync_session<T>(
    mut client: Client<T>,
    config: &GmailConfig,
    dir: &Path,
) -> Result<SyncStats, SyncError>
where
    T: AsyncRead + AsyncWrite + Unpin + std::fmt::Debug + Send,
{
    // 读取服务器问候。
    let _ = client.read_response().await;
    let mut session = client
        .login(&config.user, &config.app_password)
        .await
        .map_err(|(e, _)| SyncError::Auth(format!("请检查 Gmail 地址和应用专用密码（{e}）")))?;

    // 按 SPECIAL-USE 找文件夹。
    let mut inbox = "INBOX".to_string();
    let mut sent = None;
    let mut drafts = None;
    {
        let mut names = session.list(None, Some("*")).await.map_err(imap_err)?;
        while let Some(name) = names.next().await {
            let name = name.map_err(imap_err)?;
            for attr in name.attributes() {
                match attr {
                    NameAttribute::Sent => sent = Some(name.name().to_string()),
                    NameAttribute::Drafts => drafts = Some(name.name().to_string()),
                    _ => {}
                }
            }
            if name.name().eq_ignore_ascii_case("INBOX") {
                inbox = name.name().to_string();
            }
        }
    }
    let folders: Vec<String> = std::iter::once(inbox).chain(sent).chain(drafts).collect();

    let mut store = Store::open(dir)?;
    let mut stats = SyncStats::default();
    let query = match config.since_days {
        Some(days) => format!("SINCE {}", since_date(days)),
        None => "ALL".to_string(),
    };

    for folder in &folders {
        let mailbox = session.select(folder).await.map_err(imap_err)?;
        let validity = mailbox.uid_validity.unwrap_or(0);
        let mut update = store.begin_folder()?;
        let mut fstate = update.state.imap.remove(folder).unwrap_or_default();
        let mut obsolete = HashSet::new();
        if fstate.uid_validity != validity {
            // UID 已失效：丢掉旧记录，重新同步这个文件夹。
            obsolete.extend(fstate.files.into_values());
            fstate = ImapFolderState {
                uid_validity: validity,
                files: BTreeMap::new(),
            };
        }

        let current: HashSet<u32> = session.uid_search(&query).await.map_err(imap_err)?;
        let mut new: Vec<u32> = current
            .iter()
            .copied()
            .filter(|u| !fstate.files.contains_key(u))
            .collect();
        new.sort_unstable();
        let gone: Vec<u32> = fstate
            .files
            .keys()
            .copied()
            .filter(|u| !current.contains(u))
            .collect();

        for chunk in new.chunks(50) {
            let set = chunk
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",");
            let mut fetches = session
                .uid_fetch(&set, "(UID FLAGS X-GM-MSGID BODY.PEEK[])")
                .await
                .map_err(imap_err)?;
            while let Some(fetch) = fetches.next().await {
                let fetch = fetch.map_err(imap_err)?;
                let (Some(uid), Some(body)) = (fetch.uid, fetch.body()) else {
                    continue;
                };
                let name = file_name(fetch.gmail_msg_id().copied(), folder, validity, uid);
                if update.insert(&name, body)? {
                    stats.downloaded += 1;
                }
                fstate.files.insert(uid, name);
            }
        }
        for uid in gone {
            if let Some(name) = fstate.files.remove(&uid) {
                obsolete.insert(name);
            }
        }
        update.state.imap.insert(folder.clone(), fstate);
        for other in update.state.imap.values() {
            for name in other.files.values() {
                obsolete.remove(name);
            }
        }
        for name in obsolete {
            if update.delete(&name)? {
                stats.removed += 1;
            }
        }
        update.commit()?;
    }
    let _ = session.logout().await;
    Ok(stats)
}

/// 通过 Gmail SMTP 发出一封已生成的 MIME 邮件。`recipients` 为信封收件人（含 Bcc）。
/// Gmail 会自动把它存入“已发送”，这里不再另存。
pub fn send_gmail(
    config: &GmailConfig,
    from: &str,
    recipients: &[String],
    raw: &[u8],
) -> Result<(), SyncError> {
    use lettre::address::Envelope;
    use lettre::transport::smtp::authentication::Credentials;
    use lettre::{Address, SmtpTransport, Transport};

    let parse = |a: &str| {
        a.parse::<Address>()
            .map_err(|e| SyncError::Protocol(format!("地址无效 {a}：{e}")))
    };
    let to = recipients
        .iter()
        .map(|a| parse(a))
        .collect::<Result<Vec<_>, _>>()?;
    let envelope =
        Envelope::new(Some(parse(from)?), to).map_err(|e| SyncError::Protocol(e.to_string()))?;
    let creds = Credentials::new(config.user.clone(), config.app_password.clone());
    let transport = if config.tls {
        SmtpTransport::relay(&config.smtp_host)
            .map_err(|e| SyncError::Network(e.to_string()))?
            .port(config.smtp_port)
            .credentials(creds)
            .build()
    } else {
        SmtpTransport::builder_dangerous(&config.smtp_host)
            .port(config.smtp_port)
            .credentials(creds)
            .build()
    };
    transport.send_raw(&envelope, raw).map_err(|e| {
        if e.is_permanent() {
            SyncError::Auth(format!("Gmail 拒绝发信：{e}"))
        } else {
            SyncError::Network(format!("发信失败：{e}"))
        }
    })?;
    Ok(())
}

async fn find_in_sent<T>(
    mut client: Client<T>,
    config: &GmailConfig,
    message_id: &str,
) -> Result<bool, SyncError>
where
    T: AsyncRead + AsyncWrite + Unpin + std::fmt::Debug + Send,
{
    let _ = client.read_response().await;
    let mut session = client
        .login(&config.user, &config.app_password)
        .await
        .map_err(|(e, _)| SyncError::Auth(format!("请检查 Gmail 地址和应用专用密码（{e}）")))?;
    let mut sent = None;
    {
        let mut names = session.list(None, Some("*")).await.map_err(imap_err)?;
        while let Some(name) = names.next().await {
            let name = name.map_err(imap_err)?;
            if name
                .attributes()
                .iter()
                .any(|a| matches!(a, NameAttribute::Sent))
            {
                sent = Some(name.name().to_string());
            }
        }
    }
    let Some(sent) = sent else {
        return Err(SyncError::Protocol("找不到“已发送”文件夹".into()));
    };
    session.select(&sent).await.map_err(imap_err)?;
    let hits = session
        .uid_search(format!("HEADER Message-ID \"<{message_id}>\""))
        .await
        .map_err(imap_err)?;
    let _ = session.logout().await;
    Ok(!hits.is_empty())
}

/// “已发送”中是否有这封邮件（按 Message-ID 查找）。
pub fn find_sent_gmail(config: &GmailConfig, message_id: &str) -> Result<bool, SyncError> {
    let rt = runtime()?;
    rt.block_on(async {
        let tcp = TcpStream::connect((config.imap_host.as_str(), config.imap_port))
            .await
            .map_err(|e| {
                SyncError::Network(format!(
                    "无法连接 {}:{}：{e}",
                    config.imap_host, config.imap_port
                ))
            })?;
        if config.tls {
            let tls = tls_connect(&config.imap_host, tcp).await?;
            find_in_sent(Client::new(tls), config, message_id).await
        } else {
            find_in_sent(Client::new(tcp), config, message_id).await
        }
    })
}

/// 回复一封邮件：生成 MIME（含线程信息）并经 SMTP 发出；没有确认时到“已发送”核对。
pub fn reply_gmail(
    config: &GmailConfig,
    original: &mail_core::Email,
    from: &mail_core::Mailbox,
    draft: &ReplyDraft,
) -> Result<SendReport, SyncError> {
    let built = build_reply(original, from, draft)?;
    let status = match send_gmail(config, &from.address, &built.recipients, &built.raw) {
        Ok(()) => SendStatus::Sent,
        // 账号或收件人被拒：没有发出。
        Err(e @ SyncError::Auth(_)) => return Err(e),
        Err(error) => {
            std::thread::sleep(config.verify_delay);
            match find_sent_gmail(config, &built.message_id) {
                Ok(true) => SendStatus::SentConfirmedAfterError,
                Ok(false) => SendStatus::NotSent(error.to_string()),
                Err(e) => SendStatus::MaybeSent(format!("{error}；核对“已发送”也失败：{e}")),
            }
        }
    };
    Ok(SendReport {
        message_id: built.message_id,
        draft_id: None,
        status,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_dedupe_by_gmail_id() {
        assert_eq!(file_name(Some(42), "INBOX", 1, 7), "gm-42.eml");
        assert_eq!(
            file_name(None, "[Gmail]/Sent Mail", 3, 9),
            "imap-_Gmail__Sent_Mail-3-9.eml"
        );
    }

    #[test]
    fn since_date_has_imap_format() {
        let d = since_date(0);
        let parts: Vec<&str> = d.split('-').collect();
        assert_eq!(parts.len(), 3);
        assert!(parts[1].len() == 3 && parts[2].len() == 4, "{d}");
    }
}
