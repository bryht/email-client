//! 本地模拟服务，用于在没有真实账号时开发和测试收发信：
//!
//! - [`graph`]：微软身份平台（OAuth 2.0 授权码 + PKCE）与 Microsoft Graph 邮件接口；
//! - [`imap`]：Gmail 风格的 IMAP（应用专用密码、`[Gmail]/…` 文件夹、`X-GM-MSGID`）；
//! - [`smtp`]：Gmail 风格的 SMTP（发出的邮件自动存入“已发送”）。
//!
//! 三个服务共享同一个 [`store::Mailbox`]，所以通过 SMTP 或 Graph 发出的邮件，
//! 也能在 IMAP 的“已发送”或 Graph 的 `sentitems` 里看到。

pub mod graph;
pub mod imap;
pub mod smtp;
pub mod store;

pub use graph::CLIENT_ID;
pub use imap::APP_PASSWORD;
pub use store::{Folder, Mailbox};

/// 同时启动三个模拟服务。
pub struct MockServers {
    pub mailbox: Mailbox,
    pub graph: graph::GraphMock,
    pub imap: imap::ImapMock,
    pub smtp: smtp::SmtpMock,
}

impl MockServers {
    /// `host` 一般为 `127.0.0.1`；端口为 0 时由系统分配。
    pub fn start(
        mailbox: Mailbox,
        host: &str,
        graph_port: u16,
        imap_port: u16,
        smtp_port: u16,
    ) -> std::io::Result<Self> {
        Ok(Self {
            graph: graph::start(mailbox.clone(), &format!("{host}:{graph_port}"))?,
            imap: imap::start(mailbox.clone(), &format!("{host}:{imap_port}"))?,
            smtp: smtp::start(mailbox.clone(), &format!("{host}:{smtp_port}"))?,
            mailbox,
        })
    }
}
