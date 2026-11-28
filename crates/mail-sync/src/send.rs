//! 发信结果（PLAN 第 1 节“发送中断”）：
//! 发送失败或没有收到确认时，到“已发送”里按 Message-ID 核对，再决定是“已发送”、“未发送（可重试）”
//! 还是“可能已发送”（核对本身也失败，由用户决定是否重发）。

/// 一次发送的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendStatus {
    /// 服务器确认已发送。
    Sent,
    /// 发送时出错，但在“已发送”中找到了这封邮件。
    SentConfirmedAfterError,
    /// 发送失败，且“已发送”中没有这封邮件；可以重试。
    NotSent(String),
    /// 发送结果不明，核对也失败；由用户决定是否重发。
    MaybeSent(String),
}

impl SendStatus {
    pub fn is_sent(&self) -> bool {
        matches!(self, SendStatus::Sent | SendStatus::SentConfirmedAfterError)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendReport {
    /// 不带尖括号的 Message-ID。
    pub message_id: String,
    /// Outlook 的草稿 id（重试发送时使用）。
    pub draft_id: Option<String>,
    pub status: SendStatus,
}
