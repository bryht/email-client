//! 用原始 IMAP / SMTP 对话检查 Gmail 模拟服务。

use mock_servers::{APP_PASSWORD, Mailbox, imap, smtp};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;

fn mailbox() -> Mailbox {
    let mb = Mailbox::new("lin@example.com", "Lin Chen");
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples");
    mb.load_dir(&dir).unwrap();
    mb
}

struct Client {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    n: u32,
}

impl Client {
    fn connect(port: u16) -> Self {
        let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let mut c = Client {
            writer: stream.try_clone().unwrap(),
            reader: BufReader::new(stream),
            n: 0,
        };
        let greeting = c.line();
        assert!(greeting.starts_with("* OK"), "{greeting}");
        c
    }

    fn line(&mut self) -> String {
        let mut s = String::new();
        self.reader.read_line(&mut s).unwrap();
        s.trim_end().to_string()
    }

    /// 发送命令，返回 (未加标记的响应，含字面量内容；结束行)。
    fn cmd(&mut self, command: &str) -> (Vec<String>, String) {
        self.n += 1;
        let tag = format!("a{}", self.n);
        write!(self.writer, "{tag} {command}\r\n").unwrap();
        let mut lines = Vec::new();
        loop {
            let mut l = self.line();
            if let Some(open) = l.rfind('{')
                && l.ends_with('}')
            {
                let n: usize = l[open + 1..l.len() - 1].parse().unwrap();
                let mut buf = vec![0u8; n];
                self.reader.read_exact(&mut buf).unwrap();
                l.push_str(&String::from_utf8_lossy(&buf));
                l.push_str(&self.line());
            }
            if l.starts_with(&format!("{tag} ")) {
                return (lines, l);
            }
            lines.push(l);
        }
    }
}

#[test]
fn gmail_imap_behaviour() {
    let mock = imap::start(mailbox(), "127.0.0.1:0").unwrap();
    let mut c = Client::connect(mock.port);

    let (lines, done) = c.cmd("CAPABILITY");
    assert!(done.contains("OK"));
    assert!(lines[0].contains("X-GM-EXT-1") && lines[0].contains("SPECIAL-USE"));

    let (_, done) = c.cmd("SELECT INBOX");
    assert!(done.contains("BAD"), "must log in first: {done}");
    let (_, done) = c.cmd("LOGIN lin@example.com \"wrong password\"");
    assert!(done.contains("NO [AUTHENTICATIONFAILED]"));
    // 应用专用密码带不带空格都可以。
    let (_, done) = c.cmd(&format!(
        "LOGIN lin@example.com {}",
        APP_PASSWORD.replace(' ', "")
    ));
    assert!(done.contains("OK"), "{done}");

    let (lines, _) = c.cmd("LIST \"\" \"*\"");
    assert!(
        lines
            .iter()
            .any(|l| l.contains("\\All") && l.contains("[Gmail]/All Mail"))
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("\\Sent") && l.contains("[Gmail]/Sent Mail"))
    );
    assert!(lines.iter().any(|l| l.contains("\\Drafts")));

    let (lines, done) = c.cmd("SELECT INBOX");
    assert!(done.contains("[READ-WRITE]"));
    assert!(lines.iter().any(|l| l.contains("[UIDVALIDITY 1]")));
    let exists: usize = lines
        .iter()
        .find(|l| l.ends_with("EXISTS"))
        .unwrap()
        .split(' ')
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    assert!(exists >= 8);

    // 只要 3 月 10 日以后的邮件。
    let (lines, _) = c.cmd("UID SEARCH SINCE 10-Mar-2026");
    let recent: Vec<u64> = lines[0]
        .trim_start_matches("* SEARCH")
        .split_whitespace()
        .map(|n| n.parse().unwrap())
        .collect();
    assert!(!recent.is_empty() && recent.len() < exists, "{recent:?}");

    // 抓取一封：带 X-GM-MSGID、标签和原文，BODY.PEEK 不改变已读状态。
    let uid = recent[0];
    let (lines, done) = c.cmd(&format!(
        "UID FETCH {uid} (UID FLAGS INTERNALDATE RFC822.SIZE X-GM-MSGID X-GM-LABELS BODY.PEEK[])"
    ));
    assert!(done.contains("OK"));
    let fetch = &lines[0];
    assert!(fetch.contains(&format!("UID {uid}")));
    assert!(fetch.contains("FLAGS ()"), "{fetch}");
    assert!(fetch.contains("X-GM-LABELS (\"\\\\Inbox\")"), "{fetch}");
    assert!(fetch.contains("From:"));
    let msgid: String = fetch
        .split("X-GM-MSGID ")
        .nth(1)
        .unwrap()
        .split(' ')
        .next()
        .unwrap()
        .to_string();

    // 同一封邮件在“所有邮件”里有相同的 X-GM-MSGID。
    c.cmd("EXAMINE \"[Gmail]/All Mail\"");
    let (lines, _) = c.cmd(&format!("UID FETCH {uid} (X-GM-MSGID)"));
    assert!(
        lines[0].contains(&format!("X-GM-MSGID {msgid}")),
        "{}",
        lines[0]
    );
    // 只读打开时不能改标记。
    let (_, done) = c.cmd(&format!("UID STORE {uid} +FLAGS (\\Seen)"));
    assert!(done.contains("NO"));

    // 标为已读。
    c.cmd("SELECT INBOX");
    let (lines, _) = c.cmd(&format!("UID STORE {uid} +FLAGS (\\Seen)"));
    assert!(lines[0].contains("\\Seen"));

    // 保存草稿。
    let draft = "From: lin@example.com\r\nTo: maya@example.com\r\nSubject: Draft\r\n\r\nWIP\r\n";
    c.n += 1;
    write!(
        c.writer,
        "a{} APPEND \"[Gmail]/Drafts\" (\\Draft) {{{}}}\r\n",
        c.n,
        draft.len()
    )
    .unwrap();
    assert!(c.line().starts_with('+'));
    write!(c.writer, "{draft}\r\n").unwrap();
    assert!(c.line().contains("OK [APPENDUID"));
    let (lines, _) = c.cmd("EXAMINE \"[Gmail]/Drafts\"");
    assert!(lines.iter().any(|l| l == "* 1 EXISTS"));

    let (lines, done) = c.cmd("LOGOUT");
    assert!(lines[0].starts_with("* BYE") && done.contains("OK"));
}

#[test]
fn gmail_smtp_sends_and_saves_to_sent() {
    let mb = mailbox();
    let mock = smtp::start(mb.clone(), "127.0.0.1:0").unwrap();
    let stream = TcpStream::connect(("127.0.0.1", mock.port)).unwrap();
    let mut w = stream.try_clone().unwrap();
    let mut r = BufReader::new(stream);
    let mut line = || {
        let mut s = String::new();
        r.read_line(&mut s).unwrap();
        s.trim_end().to_string()
    };
    assert!(line().starts_with("220"));
    write!(w, "EHLO letterdesk\r\n").unwrap();
    loop {
        let l = line();
        if l.starts_with("250 ") {
            break;
        }
    }
    write!(w, "MAIL FROM:<lin@example.com>\r\n").unwrap();
    assert!(line().starts_with("530"), "must authenticate first");
    line();
    use base64::Engine;
    let plain = base64::engine::general_purpose::STANDARD
        .encode(format!("\0lin@example.com\0{APP_PASSWORD}"));
    write!(w, "AUTH PLAIN {plain}\r\n").unwrap();
    assert!(line().starts_with("235"));
    write!(w, "MAIL FROM:<lin@example.com>\r\nRCPT TO:<maya@example.com>\r\nRCPT TO:<secret-bcc@example.com>\r\nDATA\r\n").unwrap();
    assert!(line().starts_with("250"));
    assert!(line().starts_with("250"));
    assert!(line().starts_with("250"));
    assert!(line().starts_with("354"));
    write!(w, "From: Lin <lin@example.com>\r\nTo: maya@example.com\r\nSubject: Sent via SMTP\r\nMessage-ID: <smtp-1@x>\r\n\r\nLine one\r\n..starts with a dot\r\n.\r\n").unwrap();
    assert!(line().starts_with("250"));
    write!(w, "QUIT\r\n").unwrap();
    assert!(line().starts_with("221"));

    let sent = mock.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0].recipients,
        vec!["maya@example.com", "secret-bcc@example.com"]
    );
    let text = String::from_utf8_lossy(&sent[0].raw);
    assert!(
        text.contains("\r\n.starts with a dot\r\n"),
        "dot-unstuffing: {text}"
    );
    // 和 Gmail 一样自动出现在“已发送”。
    let (messages, _) = mb.snapshot();
    let saved = messages
        .iter()
        .find(|m| m.email.subject == "Sent via SMTP")
        .unwrap();
    assert_eq!(saved.folder, mock_servers::Folder::Sent);
}
