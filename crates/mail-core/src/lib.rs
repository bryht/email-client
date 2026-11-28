//! Letterdesk 的邮件领域模型。
//!
//! 第一阶段只覆盖 AI 原型需要的部分：解析一封 `.eml`、提取正文、
//! 按“默认回复所有人”的规则确定收件人。

mod address;
mod body;
mod message;

pub use address::Mailbox;
pub use body::strip_quoted;
pub use message::{Email, ParseError, ReplyRecipients};
