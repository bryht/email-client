//! 模拟 Gmail 的 IMAP 服务（明文 TCP，仅用于本地开发；真实 Gmail 使用 993 端口 TLS）。
//!
//! 模拟的 Gmail 行为：
//! - 用邮箱地址 + 应用专用密码登录（`LOGIN` 或 `AUTHENTICATE PLAIN`），密码中的空格可有可无；
//! - 文件夹：`INBOX`、`[Gmail]/All Mail`（\All）、`[Gmail]/Sent Mail`（\Sent）、`[Gmail]/Drafts`（\Drafts）、
//!   `[Gmail]/Spam`（\Junk）、`[Gmail]/Trash`（\Trash），带 SPECIAL-USE 标记；
//! - 同一封邮件同时出现在收件箱（或已发送）和“所有邮件”中，`X-GM-MSGID` 相同，客户端应据此去重；
//! - `X-GM-LABELS`、`X-GM-THRID`。
//!
//! 支持的命令：CAPABILITY、ID、ENABLE、LOGIN、AUTHENTICATE PLAIN、LIST、SELECT、EXAMINE、
//! [UID] SEARCH（ALL / SINCE / UID 范围）、[UID] FETCH、[UID] STORE ±FLAGS (\Seen)、APPEND、
//! NOOP、CHECK、IDLE、CLOSE、LOGOUT。

use crate::store::{Folder, MONTHS, Mailbox, StoredMessage, days_to_ts, imap_date};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

/// 模拟账号的应用专用密码（Gmail 显示为四组四个字母）。
pub const APP_PASSWORD: &str = "abcd efgh ijkl mnop";

const CAPABILITIES: &str = "IMAP4rev1 UNSELECT IDLE NAMESPACE ID CHILDREN X-GM-EXT-1 UIDPLUS ENABLE SPECIAL-USE LITERAL+ AUTH=PLAIN";

/// 运行中的 IMAP 模拟服务。丢弃时停止接受新连接。
pub struct ImapMock {
    pub port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for ImapMock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // 连接一次以唤醒 accept。
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub fn start(mailbox: Mailbox, addr: &str) -> std::io::Result<ImapMock> {
    let listener = TcpListener::bind(addr)?;
    let port = listener.local_addr()?.port();
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let thread = std::thread::spawn(move || {
        for stream in listener.incoming() {
            if stop2.load(Ordering::SeqCst) {
                break;
            }
            if let Ok(stream) = stream {
                let mb = mailbox.clone();
                std::thread::spawn(move || {
                    let _ = Session::new(mb, stream).run();
                });
            }
        }
    });
    Ok(ImapMock {
        port,
        stop,
        thread: Some(thread),
    })
}

/// 可选择的文件夹。
#[derive(Debug, Clone, Copy, PartialEq)]
enum Mbox {
    Inbox,
    AllMail,
    Sent,
    Drafts,
    Spam,
    Trash,
}

impl Mbox {
    const ALL: [(Mbox, &'static str, &'static str); 6] = [
        (Mbox::Inbox, "INBOX", "\\HasNoChildren"),
        (Mbox::AllMail, "[Gmail]/All Mail", "\\All \\HasNoChildren"),
        (Mbox::Drafts, "[Gmail]/Drafts", "\\Drafts \\HasNoChildren"),
        (Mbox::Sent, "[Gmail]/Sent Mail", "\\HasNoChildren \\Sent"),
        (Mbox::Spam, "[Gmail]/Spam", "\\HasNoChildren \\Junk"),
        (Mbox::Trash, "[Gmail]/Trash", "\\HasNoChildren \\Trash"),
    ];

    fn by_name(name: &str) -> Option<Mbox> {
        Self::ALL.iter().find_map(|(m, n, _)| {
            let matches = if *n == "INBOX" {
                name.eq_ignore_ascii_case("INBOX")
            } else {
                name == *n
            };
            matches.then_some(*m)
        })
    }

    fn contains(self, m: &StoredMessage) -> bool {
        if m.deleted {
            return false;
        }
        match self {
            Mbox::Inbox => m.folder == Folder::Inbox,
            Mbox::Sent => m.folder == Folder::Sent,
            Mbox::Drafts => m.folder == Folder::Drafts,
            // Gmail 的“所有邮件”不含草稿以外的特殊文件夹；这里包含收件箱与已发送。
            Mbox::AllMail => matches!(m.folder, Folder::Inbox | Folder::Sent),
            Mbox::Spam | Mbox::Trash => false,
        }
    }
}

struct Session {
    mailbox: Mailbox,
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    authenticated: bool,
    selected: Option<(Mbox, bool)>,
}

/// 把一行命令拆成参数：原子、带引号的字符串、括号列表（原样保留）。
fn tokenize(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c == ' ' {
            chars.next();
        } else if c == '"' {
            chars.next();
            let mut v = String::new();
            while let Some(c) = chars.next() {
                match c {
                    '\\' => {
                        if let Some(n) = chars.next() {
                            v.push(n);
                        }
                    }
                    '"' => break,
                    c => v.push(c),
                }
            }
            out.push(v);
        } else if c == '(' || c == '[' {
            let (open, close) = if c == '(' { ('(', ')') } else { ('[', ']') };
            let mut depth = 0;
            let mut v = String::new();
            for c in chars.by_ref() {
                v.push(c);
                if c == open {
                    depth += 1;
                } else if c == close {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
            }
            out.push(v);
        } else {
            let mut v = String::new();
            while let Some(&c) = chars.peek() {
                if c == ' ' {
                    break;
                }
                // 允许 BODY.PEEK[] 这样带方括号的原子。
                if c == '[' {
                    for c in chars.by_ref() {
                        v.push(c);
                        if c == ']' {
                            break;
                        }
                    }
                    continue;
                }
                v.push(c);
                chars.next();
            }
            out.push(v);
        }
    }
    out
}

/// 解析序列集，如 `1:*`、`3`、`1,4:6`。`max` 用于展开 `*`。
fn parse_set(set: &str, max: u64) -> Vec<(u64, u64)> {
    set.split(',')
        .filter_map(|part| {
            let parse = |s: &str| {
                if s == "*" {
                    Some(max)
                } else {
                    s.parse::<u64>().ok()
                }
            };
            match part.split_once(':') {
                Some((a, b)) => {
                    let (a, b) = (parse(a)?, parse(b)?);
                    Some((a.min(b), a.max(b)))
                }
                None => parse(part).map(|a| (a, a)),
            }
        })
        .collect()
}

fn in_set(ranges: &[(u64, u64)], v: u64) -> bool {
    ranges.iter().any(|&(a, b)| v >= a && v <= b)
}

/// `1-Mar-2026` → 当天 0 点的时间戳。
fn parse_imap_day(s: &str) -> Option<i64> {
    let mut parts = s.trim_matches('"').split('-');
    let d: u32 = parts.next()?.parse().ok()?;
    let m = parts.next()?;
    let m = MONTHS.iter().position(|x| x.eq_ignore_ascii_case(m))? as u32 + 1;
    let y: i64 = parts.next()?.parse().ok()?;
    Some(days_to_ts(y, m, d))
}

/// `SEARCH HEADER 字段 值`：Message-ID 按值精确比较（忽略尖括号），其他字段按子串匹配。
fn header_matches(m: &StoredMessage, field: &str, value: &str) -> bool {
    if field == "message-id" {
        let want = value.trim().trim_start_matches('<').trim_end_matches('>');
        return m.email.message_id.as_deref() == Some(want);
    }
    let end = m
        .raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .unwrap_or(m.raw.len());
    String::from_utf8_lossy(&m.raw[..end])
        .lines()
        .filter_map(|l| l.split_once(':'))
        .any(|(k, v)| {
            k.trim().eq_ignore_ascii_case(field) && v.to_lowercase().contains(&value.to_lowercase())
        })
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

impl Session {
    fn new(mailbox: Mailbox, stream: TcpStream) -> Self {
        let writer = stream.try_clone().expect("clone stream");
        Self {
            mailbox,
            reader: BufReader::new(stream),
            writer,
            authenticated: false,
            selected: None,
        }
    }

    fn send(&mut self, line: &str) -> std::io::Result<()> {
        self.writer.write_all(line.as_bytes())?;
        self.writer.write_all(b"\r\n")
    }

    fn read_line(&mut self) -> std::io::Result<Option<String>> {
        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        Ok(Some(line.trim_end_matches(['\r', '\n']).to_string()))
    }

    /// 读取一行命令；行尾若是 `{n}` 或 `{n+}` 字面量，读取后续字节并接到后面。
    fn read_command(&mut self) -> std::io::Result<Option<(String, Option<Vec<u8>>)>> {
        let Some(line) = self.read_line()? else {
            return Ok(None);
        };
        if let Some(open) = line.rfind('{')
            && line.ends_with('}')
        {
            let spec = &line[open + 1..line.len() - 1];
            let non_sync = spec.ends_with('+');
            if let Ok(n) = spec.trim_end_matches('+').parse::<usize>() {
                if !non_sync {
                    self.send("+ Ready for literal data")?;
                }
                let mut buf = vec![0u8; n];
                std::io::Read::read_exact(&mut self.reader, &mut buf)?;
                // 字面量后的剩余部分（通常为空行）。
                let _ = self.read_line()?;
                return Ok(Some((line[..open].trim_end().to_string(), Some(buf))));
            }
        }
        Ok(Some((line, None)))
    }

    fn run(mut self) -> std::io::Result<()> {
        self.send("* OK Gimap ready for requests from 127.0.0.1 mock")?;
        while let Some((line, literal)) = self.read_command()? {
            let tokens = tokenize(&line);
            if tokens.len() < 2 {
                self.send("* BAD Invalid tag")?;
                continue;
            }
            let tag = tokens[0].clone();
            let mut cmd = tokens[1].to_ascii_uppercase();
            let mut args: Vec<String> = tokens[2..].to_vec();
            let mut uid = false;
            if cmd == "UID" && !args.is_empty() {
                uid = true;
                cmd = args.remove(0).to_ascii_uppercase();
            }
            if !self.dispatch(&tag, &cmd, &args, uid, literal)? {
                break;
            }
        }
        Ok(())
    }

    /// 处理一条命令；返回 false 表示结束会话。
    fn dispatch(
        &mut self,
        tag: &str,
        cmd: &str,
        args: &[String],
        uid: bool,
        literal: Option<Vec<u8>>,
    ) -> std::io::Result<bool> {
        match cmd {
            "CAPABILITY" => {
                self.send(&format!("* CAPABILITY {CAPABILITIES}"))?;
                self.send(&format!("{tag} OK Thats all she wrote! (Success)"))?;
            }
            "ID" => {
                self.send("* ID (\"name\" \"GImap\" \"vendor\" \"Google, Inc.\")")?;
                self.send(&format!("{tag} OK Success"))?;
            }
            "ENABLE" | "NOOP" | "CHECK" => self.send(&format!("{tag} OK Success"))?,
            "LOGOUT" => {
                self.send("* BYE LOGOUT Requested")?;
                self.send(&format!("{tag} OK 73 good day (Success)"))?;
                return Ok(false);
            }
            "LOGIN" => {
                let user = args.first().map(String::as_str).unwrap_or_default();
                let pass = args.get(1).map(String::as_str).unwrap_or_default();
                self.login(tag, user, pass)?;
            }
            "AUTHENTICATE" => {
                if !args
                    .first()
                    .is_some_and(|m| m.eq_ignore_ascii_case("PLAIN"))
                {
                    self.send(&format!("{tag} NO [CANNOT] Unsupported mechanism"))?;
                    return Ok(true);
                }
                let payload = match args.get(1) {
                    Some(p) => p.clone(),
                    None => {
                        self.send("+ ")?;
                        self.read_line()?.unwrap_or_default()
                    }
                };
                let decoded = STANDARD.decode(payload.trim()).unwrap_or_default();
                let parts: Vec<String> = decoded
                    .split(|&b| b == 0)
                    .map(|p| String::from_utf8_lossy(p).into_owned())
                    .collect();
                let (user, pass) = match parts.as_slice() {
                    [_, u, p] => (u.clone(), p.clone()),
                    _ => (String::new(), String::new()),
                };
                self.login(tag, &user, &pass)?;
            }
            _ if !self.authenticated => {
                self.send(&format!("{tag} BAD Command not valid in this state"))?;
            }
            "LIST" | "XLIST" => {
                for (_, name, flags) in Mbox::ALL {
                    self.send(&format!("* LIST ({flags}) \"/\" {}", quote(name)))?;
                }
                self.send("* LIST (\\HasChildren \\Noselect) \"/\" \"[Gmail]\"")?;
                self.send(&format!("{tag} OK Success"))?;
            }
            "SELECT" | "EXAMINE" => {
                let name = args.first().map(String::as_str).unwrap_or_default();
                match Mbox::by_name(name) {
                    Some(mbox) => {
                        let read_only = cmd == "EXAMINE";
                        self.selected = Some((mbox, read_only));
                        let msgs = self.view(mbox);
                        let uidnext = self.mailbox.snapshot().0.len() as u64 + 1;
                        self.send("* FLAGS (\\Answered \\Flagged \\Draft \\Deleted \\Seen $NotPhishing $Phishing)")?;
                        self.send("* OK [PERMANENTFLAGS (\\Answered \\Flagged \\Draft \\Deleted \\Seen $NotPhishing $Phishing \\*)] Flags permitted.")?;
                        self.send("* OK [UIDVALIDITY 1] UIDs valid.")?;
                        self.send(&format!("* {} EXISTS", msgs.len()))?;
                        self.send("* 0 RECENT")?;
                        self.send(&format!("* OK [UIDNEXT {uidnext}] Predicted next UID."))?;
                        let mode = if read_only { "READ-ONLY" } else { "READ-WRITE" };
                        self.send(&format!("{tag} OK [{mode}] {name} selected. (Success)"))?;
                    }
                    None => self.send(&format!(
                        "{tag} NO [NONEXISTENT] Unknown Mailbox: {name} (Failure)"
                    ))?,
                }
            }
            "CLOSE" | "UNSELECT" => {
                self.selected = None;
                self.send(&format!(
                    "{tag} OK Returned to authenticated state. (Success)"
                ))?;
            }
            "IDLE" => {
                self.send("+ idling")?;
                while let Some(line) = self.read_line()? {
                    if line.eq_ignore_ascii_case("DONE") {
                        break;
                    }
                }
                self.send(&format!("{tag} OK IDLE terminated (Success)"))?;
            }
            "APPEND" => self.append(tag, args, literal)?,
            "SEARCH" | "FETCH" | "STORE" => {
                let Some((mbox, read_only)) = self.selected else {
                    self.send(&format!("{tag} BAD Command not valid in this state"))?;
                    return Ok(true);
                };
                match cmd {
                    "SEARCH" => self.search(tag, mbox, args, uid)?,
                    "FETCH" => self.fetch(tag, mbox, args, uid, read_only)?,
                    _ => self.store(tag, mbox, args, uid, read_only)?,
                }
            }
            _ => self.send(&format!("{tag} BAD Unknown command"))?,
        }
        Ok(true)
    }

    fn login(&mut self, tag: &str, user: &str, pass: &str) -> std::io::Result<()> {
        let (owner, _) = self.mailbox.owner();
        let normalize = |p: &str| p.replace(' ', "").to_lowercase();
        if user.eq_ignore_ascii_case(&owner) && normalize(pass) == normalize(APP_PASSWORD) {
            self.authenticated = true;
            self.send(&format!("* CAPABILITY {CAPABILITIES}"))?;
            self.send(&format!("{tag} OK {owner} authenticated (Success)"))
        } else {
            self.send(&format!(
                "{tag} NO [AUTHENTICATIONFAILED] Invalid credentials (Failure)"
            ))
        }
    }

    /// 文件夹中的邮件（按 UID 升序；序号为位置 + 1）。
    fn view(&self, mbox: Mbox) -> Vec<StoredMessage> {
        self.mailbox
            .snapshot()
            .0
            .into_iter()
            .filter(|m| mbox.contains(m))
            .collect()
    }

    fn search(&mut self, tag: &str, mbox: Mbox, args: &[String], uid: bool) -> std::io::Result<()> {
        let msgs = self.view(mbox);
        let max_uid = msgs.last().map(|m| m.n).unwrap_or(0);
        let mut since: Option<i64> = None;
        let mut uid_ranges: Option<Vec<(u64, u64)>> = None;
        let mut header: Option<(String, String)> = None;
        let mut i = 0;
        while i < args.len() {
            match args[i].to_ascii_uppercase().as_str() {
                "ALL" => {}
                "SINCE" => {
                    since = args.get(i + 1).and_then(|d| parse_imap_day(d));
                    if since.is_none() {
                        return self.send(&format!("{tag} BAD Could not parse command"));
                    }
                    i += 1;
                }
                "UID" => {
                    uid_ranges = args.get(i + 1).map(|s| parse_set(s, max_uid));
                    i += 1;
                }
                "HEADER" => {
                    let (Some(field), Some(value)) = (args.get(i + 1), args.get(i + 2)) else {
                        return self.send(&format!("{tag} BAD Could not parse command"));
                    };
                    header = Some((field.to_ascii_lowercase(), value.clone()));
                    i += 2;
                }
                other => {
                    return self.send(&format!("{tag} BAD Unsupported search key {other}"));
                }
            }
            i += 1;
        }
        let hits: Vec<String> = msgs
            .iter()
            .enumerate()
            .filter(|(_, m)| since.is_none_or(|s| m.email.timestamp.unwrap_or(0) >= s))
            .filter(|(_, m)| uid_ranges.as_ref().is_none_or(|r| in_set(r, m.n)))
            .filter(|(_, m)| header.as_ref().is_none_or(|(f, v)| header_matches(m, f, v)))
            .map(|(pos, m)| if uid { m.n } else { pos as u64 + 1 }.to_string())
            .collect();
        if hits.is_empty() {
            self.send("* SEARCH")?;
        } else {
            self.send(&format!("* SEARCH {}", hits.join(" ")))?;
        }
        self.send(&format!("{tag} OK SEARCH completed (Success)"))
    }

    /// 选出序列集对应的 (序号, 邮件)。
    fn select_set(msgs: &[StoredMessage], set: &str, uid: bool) -> Vec<(usize, StoredMessage)> {
        let max = if uid {
            msgs.last().map(|m| m.n).unwrap_or(0)
        } else {
            msgs.len() as u64
        };
        let ranges = parse_set(set, max);
        msgs.iter()
            .enumerate()
            .filter(|(pos, m)| in_set(&ranges, if uid { m.n } else { *pos as u64 + 1 }))
            .map(|(pos, m)| (pos + 1, m.clone()))
            .collect()
    }

    fn flags(m: &StoredMessage) -> String {
        let mut f = Vec::new();
        if m.seen {
            f.push("\\Seen");
        }
        if m.folder == Folder::Drafts {
            f.push("\\Draft");
        }
        format!("({})", f.join(" "))
    }

    fn labels(m: &StoredMessage) -> &'static str {
        match m.folder {
            Folder::Inbox => "(\"\\\\Inbox\")",
            Folder::Sent => "(\"\\\\Sent\")",
            Folder::Drafts => "(\"\\\\Draft\")",
        }
    }

    fn fetch(
        &mut self,
        tag: &str,
        mbox: Mbox,
        args: &[String],
        uid: bool,
        read_only: bool,
    ) -> std::io::Result<()> {
        let (Some(set), Some(items)) = (args.first(), args.get(1)) else {
            return self.send(&format!("{tag} BAD Could not parse command"));
        };
        let items = items.trim_start_matches('(').trim_end_matches(')');
        let wanted: Vec<String> = tokenize(items)
            .into_iter()
            .map(|s| s.to_ascii_uppercase())
            .collect();
        let msgs = self.view(mbox);
        for (seq, m) in Self::select_set(&msgs, set, uid) {
            let mut parts: Vec<String> = Vec::new();
            let mut body: Option<(String, Vec<u8>)> = None;
            if uid {
                parts.push(format!("UID {}", m.n));
            }
            let mut mark_seen = false;
            for item in &wanted {
                match item.as_str() {
                    "UID" if !uid => parts.push(format!("UID {}", m.n)),
                    "FLAGS" => parts.push(format!("FLAGS {}", Self::flags(&m))),
                    "INTERNALDATE" => parts.push(format!(
                        "INTERNALDATE \"{}\"",
                        imap_date(m.email.timestamp.unwrap_or(0))
                    )),
                    "RFC822.SIZE" => parts.push(format!("RFC822.SIZE {}", m.raw.len())),
                    "X-GM-MSGID" => parts.push(format!("X-GM-MSGID {}", m.gm_msgid())),
                    "X-GM-THRID" => parts.push(format!("X-GM-THRID {}", m.gm_msgid())),
                    "X-GM-LABELS" => parts.push(format!("X-GM-LABELS {}", Self::labels(&m))),
                    "BODY.PEEK[]" => body = Some(("BODY[]".into(), m.raw.clone())),
                    "BODY[]" | "RFC822" => {
                        mark_seen = !read_only && !m.seen;
                        let name = if item == "RFC822" { "RFC822" } else { "BODY[]" };
                        body = Some((name.into(), m.raw.clone()));
                    }
                    "BODY.PEEK[HEADER]" | "BODY[HEADER]" | "RFC822.HEADER" => {
                        let end = m
                            .raw
                            .windows(4)
                            .position(|w| w == b"\r\n\r\n")
                            .map(|p| p + 4)
                            .unwrap_or(m.raw.len());
                        let name = if item == "RFC822.HEADER" {
                            "RFC822.HEADER"
                        } else {
                            "BODY[HEADER]"
                        };
                        body = Some((name.into(), m.raw[..end].to_vec()));
                    }
                    "UID" => {}
                    _ => {}
                }
            }
            if mark_seen {
                self.mailbox.set_seen(m.n, true);
                parts.push("FLAGS (\\Seen)".into());
            }
            let head = format!("* {seq} FETCH ({}", parts.join(" "));
            match body {
                Some((name, bytes)) => {
                    let sep = if parts.is_empty() { "" } else { " " };
                    self.writer.write_all(
                        format!("{head}{sep}{name} {{{}}}\r\n", bytes.len()).as_bytes(),
                    )?;
                    self.writer.write_all(&bytes)?;
                    self.writer.write_all(b")\r\n")?;
                }
                None => self.send(&format!("{head})"))?,
            }
        }
        self.send(&format!("{tag} OK Success"))
    }

    fn store(
        &mut self,
        tag: &str,
        mbox: Mbox,
        args: &[String],
        uid: bool,
        read_only: bool,
    ) -> std::io::Result<()> {
        if read_only {
            return self.send(&format!("{tag} NO [READ-ONLY] Mailbox is read-only"));
        }
        let (Some(set), Some(op), Some(flags)) = (args.first(), args.get(1), args.get(2)) else {
            return self.send(&format!("{tag} BAD Could not parse command"));
        };
        let op = op.to_ascii_uppercase();
        let seen = flags.to_ascii_uppercase().contains("\\SEEN");
        let silent = op.ends_with(".SILENT");
        let msgs = self.view(mbox);
        for (seq, m) in Self::select_set(&msgs, set, uid) {
            if seen {
                if op.starts_with('+') {
                    self.mailbox.set_seen(m.n, true);
                } else if op.starts_with('-') {
                    self.mailbox.set_seen(m.n, false);
                }
            }
            if !silent {
                let updated = self
                    .view(mbox)
                    .into_iter()
                    .find(|x| x.n == m.n)
                    .unwrap_or(m);
                self.send(&format!(
                    "* {seq} FETCH (UID {} FLAGS {})",
                    updated.n,
                    Self::flags(&updated)
                ))?;
            }
        }
        let _ = uid;
        self.send(&format!("{tag} OK Success"))
    }

    fn append(
        &mut self,
        tag: &str,
        args: &[String],
        literal: Option<Vec<u8>>,
    ) -> std::io::Result<()> {
        let name = args.first().map(String::as_str).unwrap_or_default();
        let Some(raw) = literal else {
            return self.send(&format!("{tag} BAD APPEND requires a literal"));
        };
        let folder = match Mbox::by_name(name) {
            Some(Mbox::Drafts) => Folder::Drafts,
            Some(Mbox::Sent) => Folder::Sent,
            Some(Mbox::Inbox) | Some(Mbox::AllMail) => Folder::Inbox,
            _ => {
                return self.send(&format!(
                    "{tag} NO [TRYCREATE] Folder doesn't exist. (Failure)"
                ));
            }
        };
        match self.mailbox.append(folder, raw) {
            Some(n) => self.send(&format!("{tag} OK [APPENDUID 1 {n}] (Success)")),
            None => self.send(&format!(
                "{tag} NO [PARSE] Unable to parse message (Failure)"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_imap_commands() {
        assert_eq!(
            tokenize(r#"a1 LOGIN "lin@example.com" "abcd efgh ijkl mnop""#),
            vec!["a1", "LOGIN", "lin@example.com", "abcd efgh ijkl mnop"]
        );
        assert_eq!(
            tokenize("a2 UID FETCH 1:* (UID FLAGS BODY.PEEK[] X-GM-MSGID)"),
            vec![
                "a2",
                "UID",
                "FETCH",
                "1:*",
                "(UID FLAGS BODY.PEEK[] X-GM-MSGID)"
            ]
        );
        assert_eq!(
            tokenize(r#"a3 SELECT "[Gmail]/All Mail""#),
            vec!["a3", "SELECT", "[Gmail]/All Mail"]
        );
        assert_eq!(tokenize("a4 LIST \"\" \"*\""), vec!["a4", "LIST", "", "*"]);
    }

    #[test]
    fn parses_sets_and_dates() {
        assert_eq!(parse_set("1:*", 7), vec![(1, 7)]);
        assert_eq!(parse_set("3,5:4", 9), vec![(3, 3), (4, 5)]);
        assert_eq!(parse_imap_day("1-Mar-2026"), Some(1_772_323_200));
    }
}
