//! Letterdesk 收信同步。
//!
//! - [`oauth`]：微软登录（授权码 + PKCE，本机回调）；
//! - [`outlook`]：Microsoft Graph 按文件夹增量同步、下载原文；
//! - [`credentials`]：令牌与应用专用密码存入系统凭据管理器；
//! - [`gmail`]：Gmail 的 IMAP 增量同步（按 X-GM-MSGID 去重）与 SMTP 发信；
//! - [`maildir`]：把邮件和同步状态写入本地 SQLite 邮箱。
//!
//! 可以对着 `mock-servers` 的模拟服务开发与测试，换成真实地址即可连接真实账号。

pub mod account;
pub mod compose;
pub mod credentials;
pub mod gmail;
pub mod maildir;
pub mod oauth;
pub mod outlook;
pub mod send;

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error(transparent)]
    Store(#[from] mail_store::Error),
    #[error("登录失败：{0}")]
    Auth(String),
    #[error("网络错误：{0}")]
    Network(String),
    #[error("系统凭据管理器不可用：{0}")]
    Credential(String),
    #[error("服务返回了意外的内容：{0}")]
    Protocol(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
