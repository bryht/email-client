use crate::address::{Mailbox, normalize};
use crate::body::strip_quoted;
use mail_parser::{Address, MessageParser};
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("无法解析邮件内容")]
    Unparseable,
    #[error("邮件缺少发件人")]
    MissingFrom,
}

/// 一封已解析的邮件。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Email {
    pub message_id: Option<String>,
    pub subject: String,
    pub from: Mailbox,
    pub to: Vec<Mailbox>,
    pub cc: Vec<Mailbox>,
    pub reply_to: Vec<Mailbox>,
    /// RFC 3339 格式的发送时间。
    pub date: Option<String>,
    pub in_reply_to: Vec<String>,
    pub references: Vec<String>,
    /// 纯文本正文；只有 HTML 时由解析器转换为文本。
    pub body: String,
    /// HTML 正文；只有纯文本时由解析器转换为简单 HTML。未经清洗，显示前必须隔离处理。
    pub html: String,
}

/// 回复时的默认收件人。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyRecipients {
    pub to: Vec<Mailbox>,
    pub cc: Vec<Mailbox>,
}

impl Email {
    /// 解析一封 RFC 5322 邮件（`.eml` 文件内容）。
    pub fn parse(raw: &[u8]) -> Result<Self, ParseError> {
        let msg = MessageParser::default()
            .parse(raw)
            .ok_or(ParseError::Unparseable)?;

        let from = mailboxes(msg.from())
            .into_iter()
            .next()
            .ok_or(ParseError::MissingFrom)?;

        Ok(Self {
            message_id: msg.message_id().map(String::from),
            subject: msg.subject().unwrap_or_default().to_string(),
            from,
            to: mailboxes(msg.to()),
            cc: mailboxes(msg.cc()),
            reply_to: mailboxes(msg.reply_to()),
            date: msg.date().map(|d| d.to_rfc3339()),
            in_reply_to: text_list(msg.in_reply_to()),
            references: text_list(msg.references()),
            body: msg
                .body_text(0)
                .map(|b| b.replace("\r\n", "\n"))
                .unwrap_or_default(),
            html: msg.body_html(0).map(|b| b.into_owned()).unwrap_or_default(),
        })
    }

    /// 去掉引用历史后的正文。
    pub fn new_content(&self) -> String {
        strip_quoted(&self.body)
    }

    /// 按“默认回复所有人”的规则确定收件人：
    /// To 为原信的 Reply-To（没有时为发件人），Cc 为原信的其他 To 和 Cc；
    /// 去掉自己（`me` 中的地址）和重复地址。
    pub fn reply_all_recipients(&self, me: &[String]) -> ReplyRecipients {
        let mine: Vec<String> = me.iter().map(|a| normalize(a)).collect();
        let mut seen: Vec<String> = Vec::new();
        let mut take = |m: &Mailbox| -> Option<Mailbox> {
            let key = m.normalized();
            if key.is_empty() || mine.contains(&key) || seen.contains(&key) {
                return None;
            }
            seen.push(key);
            Some(m.clone())
        };

        let primary: Vec<Mailbox> = if self.reply_to.is_empty() {
            vec![self.from.clone()]
        } else {
            self.reply_to.clone()
        };
        let to: Vec<Mailbox> = primary.iter().filter_map(&mut take).collect();
        let cc: Vec<Mailbox> = self
            .to
            .iter()
            .chain(self.cc.iter())
            .filter_map(&mut take)
            .collect();
        ReplyRecipients { to, cc }
    }
}

fn mailboxes(addr: Option<&Address<'_>>) -> Vec<Mailbox> {
    let Some(addr) = addr else {
        return Vec::new();
    };
    addr.iter()
        .filter_map(|a| {
            let address = a.address.as_deref()?;
            Some(Mailbox::new(a.name.as_deref(), address))
        })
        .collect()
}

fn text_list(value: &mail_parser::HeaderValue<'_>) -> Vec<String> {
    value
        .as_text_list()
        .map(|list| list.iter().map(|s| s.to_string()).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAYA: &str = "From: Maya Patel <maya@example.com>\r\n\
To: Lin Chen <lin@example.com>\r\n\
Cc: Alex Carter <alex@example.com>\r\n\
Subject: Pilot meeting\r\n\
Date: Tue, 3 Mar 2026 10:00:00 +0000\r\n\
Message-ID: <m2@example.com>\r\n\
In-Reply-To: <m1@example.com>\r\n\
References: <m1@example.com>\r\n\
\r\n\
Looping in Alex. Would Thursday at 3 pm work?\r\n\
\r\n\
On Mon, 2 Mar 2026 at 09:00, Maya <maya@example.com> wrote:\r\n\
> Could we meet this Thursday?\r\n";

    #[test]
    fn parses_headers_and_body() {
        let email = Email::parse(MAYA.as_bytes()).unwrap();
        assert_eq!(email.subject, "Pilot meeting");
        assert_eq!(email.from.address, "maya@example.com");
        assert_eq!(email.from.display_name(), "Maya Patel");
        assert_eq!(email.to.len(), 1);
        assert_eq!(email.cc[0].address, "alex@example.com");
        assert_eq!(email.message_id.as_deref(), Some("m2@example.com"));
        assert_eq!(email.in_reply_to, vec!["m1@example.com"]);
        assert_eq!(
            email.new_content(),
            "Looping in Alex. Would Thursday at 3 pm work?"
        );
    }

    #[test]
    fn reply_all_drops_self_and_keeps_cc() {
        let email = Email::parse(MAYA.as_bytes()).unwrap();
        let r = email.reply_all_recipients(&["LIN@example.com".into()]);
        assert_eq!(r.to.len(), 1);
        assert_eq!(r.to[0].address, "maya@example.com");
        assert_eq!(r.cc.len(), 1);
        assert_eq!(r.cc[0].address, "alex@example.com");
    }

    #[test]
    fn reply_all_uses_reply_to_and_dedupes() {
        let raw = "From: Shop <noreply@shop.com>\r\nReply-To: support@shop.com\r\nTo: me@x.com, support@shop.com\r\nSubject: Order\r\n\r\nHi";
        let email = Email::parse(raw.as_bytes()).unwrap();
        let r = email.reply_all_recipients(&["me@x.com".into()]);
        assert_eq!(r.to, vec![Mailbox::new(None, "support@shop.com")]);
        assert!(r.cc.is_empty());
    }

    #[test]
    fn extracts_html_part() {
        let raw = "From: a@b.com\r\nSubject: x\r\nMIME-Version: 1.0\r\nContent-Type: multipart/alternative; boundary=XX\r\n\r\n--XX\r\nContent-Type: text/plain\r\n\r\nHello\r\n--XX\r\nContent-Type: text/html\r\n\r\n<p><b>Hello</b></p>\r\n--XX--\r\n";
        let email = Email::parse(raw.as_bytes()).unwrap();
        assert_eq!(email.body.trim(), "Hello");
        assert!(email.html.contains("<b>Hello</b>"));
    }

    #[test]
    fn missing_from_is_an_error() {
        let raw = "To: a@b.com\r\nSubject: x\r\n\r\nbody";
        assert!(matches!(
            Email::parse(raw.as_bytes()),
            Err(ParseError::MissingFrom)
        ));
    }
}
