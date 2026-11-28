//! 生成回信的 MIME（PLAN 第 1 节“默认回复所有人”之后，由用户确认的收件人和正文）。
//!
//! - 主题：原主题前加 `Re: `（已有回复前缀时不重复）；
//! - 线程：`In-Reply-To` 为原信的 Message-ID，`References` 为原信的 References 加原信 Message-ID，
//!   对方的邮件客户端据此把回信放进原线程；
//! - 正文：纯文本与 HTML 两部分（multipart/alternative）；
//! - `Message-ID` 事先生成并返回，发送结果不明时据此到“已发送”核对。

use crate::SyncError;
use lettre::message::{Mailbox as LMailbox, MultiPart};
use lettre::{Address, Message};
use mail_core::{Email, Mailbox};

/// 用户确认后的回信内容。
#[derive(Debug, Clone, PartialEq)]
pub struct ReplyDraft {
    pub to: Vec<Mailbox>,
    pub cc: Vec<Mailbox>,
    /// 纯文本正文（含签名）。
    pub text: String,
    /// HTML 正文（写信框清洗后的 HTML，含签名）。
    pub html: String,
}

/// 生成好的邮件。
#[derive(Debug, Clone, PartialEq)]
pub struct BuiltMessage {
    /// 不带尖括号的 Message-ID。
    pub message_id: String,
    pub subject: String,
    pub raw: Vec<u8>,
    /// 信封收件人（To + Cc 的地址）。
    pub recipients: Vec<String>,
}

/// 回信主题：已有 `Re:` / `回复：` 等前缀时保持原样。
pub fn reply_subject(subject: &str) -> String {
    let lower = subject.trim().to_lowercase();
    if ["re:", "回复:", "回复：", "答复:", "答复："]
        .iter()
        .any(|p| lower.starts_with(p))
    {
        subject.trim().to_string()
    } else {
        format!("Re: {}", subject.trim())
    }
}

fn mailbox(m: &Mailbox) -> Result<LMailbox, SyncError> {
    let address: Address = m
        .address
        .parse()
        .map_err(|e| SyncError::Protocol(format!("地址无效 {}：{e}", m.address)))?;
    Ok(LMailbox::new(m.name.clone(), address))
}

fn new_message_id(from: &Mailbox) -> String {
    let domain = from
        .address
        .rsplit_once('@')
        .map(|(_, d)| d)
        .filter(|d| !d.is_empty())
        .unwrap_or("letterdesk.local");
    format!("{}@{domain}", uuid::Uuid::new_v4().simple())
}

/// 生成对 `original` 的回信。
pub fn build_reply(
    original: &Email,
    from: &Mailbox,
    draft: &ReplyDraft,
) -> Result<BuiltMessage, SyncError> {
    if draft.to.is_empty() && draft.cc.is_empty() {
        return Err(SyncError::Protocol("回信没有收件人".into()));
    }
    let subject = reply_subject(&original.subject);
    let message_id = new_message_id(from);
    let mut builder = Message::builder()
        .from(mailbox(from)?)
        .subject(subject.clone())
        .message_id(Some(format!("<{message_id}>")));
    if let Some(parent) = &original.message_id {
        builder = builder.in_reply_to(format!("<{parent}>"));
        let refs: Vec<String> = original
            .references
            .iter()
            .chain(std::iter::once(parent))
            .map(|r| format!("<{r}>"))
            .collect();
        builder = builder.references(refs.join(" "));
    }
    for m in &draft.to {
        builder = builder.to(mailbox(m)?);
    }
    for m in &draft.cc {
        builder = builder.cc(mailbox(m)?);
    }
    let message = builder
        .multipart(MultiPart::alternative_plain_html(
            draft.text.clone(),
            draft.html.clone(),
        ))
        .map_err(|e| SyncError::Protocol(format!("无法生成邮件：{e}")))?;
    let recipients = draft
        .to
        .iter()
        .chain(draft.cc.iter())
        .map(|m| m.address.clone())
        .collect();
    Ok(BuiltMessage {
        message_id,
        subject,
        raw: message.formatted(),
        recipients,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn original() -> Email {
        Email::parse(
            "From: Maya Patel <maya@example.com>\r\nTo: Lin Chen <lin@example.com>\r\nCc: Alex Carter <alex@example.com>\r\nSubject: =?utf-8?b?6K+V54K55oql5Lu3?= pilot\r\nMessage-ID: <m2@example.com>\r\nReferences: <m1@example.com>\r\nIn-Reply-To: <m1@example.com>\r\n\r\nWould Thursday work?\r\n"
                .as_bytes(),
        )
        .unwrap()
    }

    #[test]
    fn reply_threads_and_round_trips() {
        let orig = original();
        let me = Mailbox::new(Some("Lin Chen"), "lin@example.com");
        let draft = ReplyDraft {
            to: vec![Mailbox::new(Some("Maya Patel"), "maya@example.com")],
            cc: vec![Mailbox::new(Some("Alex Carter"), "alex@example.com")],
            text: "Hi Maya,\n\n周四可以。\n\nBest,\nLin".into(),
            html: "<p>Hi Maya,</p><p><b>周四</b>可以。</p><p>Best,<br>Lin</p>".into(),
        };
        let built = build_reply(&orig, &me, &draft).unwrap();
        assert!(built.message_id.ends_with("@example.com"));
        assert_eq!(
            built.recipients,
            vec!["maya@example.com", "alex@example.com"]
        );

        let parsed = Email::parse(&built.raw).unwrap();
        assert_eq!(parsed.subject, "Re: 试点报价 pilot");
        assert_eq!(
            parsed.message_id.as_deref(),
            Some(built.message_id.as_str())
        );
        assert_eq!(parsed.in_reply_to, vec!["m2@example.com"]);
        assert_eq!(parsed.references, vec!["m1@example.com", "m2@example.com"]);
        assert_eq!(parsed.to[0].address, "maya@example.com");
        assert_eq!(parsed.cc[0].display_name(), "Alex Carter");
        assert!(parsed.body.contains("周四可以。"), "{}", parsed.body);
        assert!(parsed.html.contains("<b>周四</b>"), "{}", parsed.html);
    }

    #[test]
    fn subject_prefix_is_not_repeated() {
        assert_eq!(reply_subject("Re: Pilot"), "Re: Pilot");
        assert_eq!(reply_subject("回复：合同"), "回复：合同");
        assert_eq!(reply_subject("Pilot"), "Re: Pilot");
    }

    #[test]
    fn reply_needs_recipients() {
        let draft = ReplyDraft {
            to: vec![],
            cc: vec![],
            text: "x".into(),
            html: "<p>x</p>".into(),
        };
        let me = Mailbox::new(None, "lin@example.com");
        assert!(build_reply(&original(), &me, &draft).is_err());
    }
}
