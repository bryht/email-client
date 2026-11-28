//! 模拟 Gmail 的 SMTP 服务（明文 TCP，仅用于本地开发；真实 Gmail 使用 465/587 端口 TLS）。
//!
//! 用邮箱地址 + 应用专用密码登录（`AUTH PLAIN` 或 `AUTH LOGIN`）。与 Gmail 一样，
//! 发出的邮件会自动保存到“已发送”，客户端不应再自行保存一份。

use crate::imap::APP_PASSWORD;
use crate::store::Mailbox;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// 一次成功的投递。
#[derive(Debug, Clone, PartialEq)]
pub struct Envelope {
    pub from: String,
    pub recipients: Vec<String>,
    pub raw: Vec<u8>,
}

pub struct SmtpMock {
    pub port: u16,
    sent: Arc<Mutex<Vec<Envelope>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl SmtpMock {
    /// 目前为止收到的所有邮件（含信封收件人，包括 Bcc）。
    pub fn sent(&self) -> Vec<Envelope> {
        self.sent.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl Drop for SmtpMock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub fn start(mailbox: Mailbox, addr: &str) -> std::io::Result<SmtpMock> {
    let listener = TcpListener::bind(addr)?;
    let port = listener.local_addr()?.port();
    let stop = Arc::new(AtomicBool::new(false));
    let sent = Arc::new(Mutex::new(Vec::new()));
    let (stop2, sent2) = (stop.clone(), sent.clone());
    let thread = std::thread::spawn(move || {
        for stream in listener.incoming() {
            if stop2.load(Ordering::SeqCst) {
                break;
            }
            if let Ok(stream) = stream {
                let (mb, sent) = (mailbox.clone(), sent2.clone());
                std::thread::spawn(move || {
                    let _ = session(mb, sent, stream);
                });
            }
        }
    });
    Ok(SmtpMock {
        port,
        sent,
        stop,
        thread: Some(thread),
    })
}

fn check(mailbox: &Mailbox, user: &str, pass: &str) -> bool {
    let (owner, _) = mailbox.owner();
    let normalize = |p: &str| p.replace(' ', "").to_lowercase();
    user.eq_ignore_ascii_case(&owner) && normalize(pass) == normalize(APP_PASSWORD)
}

fn b64(s: &str) -> String {
    String::from_utf8_lossy(&STANDARD.decode(s.trim()).unwrap_or_default()).into_owned()
}

fn session(
    mailbox: Mailbox,
    sent: Arc<Mutex<Vec<Envelope>>>,
    stream: TcpStream,
) -> std::io::Result<()> {
    let mut out = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    let send = |out: &mut TcpStream, line: &str| -> std::io::Result<()> {
        out.write_all(line.as_bytes())?;
        out.write_all(b"\r\n")
    };
    let read = |reader: &mut BufReader<TcpStream>| -> std::io::Result<Option<String>> {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        Ok(Some(line.trim_end_matches(['\r', '\n']).to_string()))
    };

    send(&mut out, "220 smtp.gmail.com ESMTP mock - gsmtp")?;
    let mut authed = false;
    let mut from: Option<String> = None;
    let mut rcpts: Vec<String> = Vec::new();
    while let Some(line) = read(&mut reader)? {
        let upper = line.to_ascii_uppercase();
        if upper.starts_with("EHLO") {
            send(&mut out, "250-smtp.gmail.com at your service")?;
            send(&mut out, "250-SIZE 35882577")?;
            send(&mut out, "250-8BITMIME")?;
            send(&mut out, "250-AUTH LOGIN PLAIN")?;
            send(&mut out, "250-ENHANCEDSTATUSCODES")?;
            send(&mut out, "250 SMTPUTF8")?;
        } else if upper.starts_with("HELO") {
            send(&mut out, "250 smtp.gmail.com at your service")?;
        } else if upper.starts_with("AUTH PLAIN") {
            let payload = match line.split_whitespace().nth(2) {
                Some(p) => p.to_string(),
                None => {
                    send(&mut out, "334 ")?;
                    read(&mut reader)?.unwrap_or_default()
                }
            };
            let decoded = b64(&payload);
            let parts: Vec<&str> = decoded.split('\0').collect();
            authed = matches!(parts.as_slice(), [_, u, p] if check(&mailbox, u, p));
            if authed {
                send(&mut out, "235 2.7.0 Accepted")?;
            } else {
                send(&mut out, "535-5.7.8 Username and Password not accepted.")?;
                send(&mut out, "535 5.7.8 BadCredentials mock")?;
            }
        } else if upper.starts_with("AUTH LOGIN") {
            send(&mut out, "334 VXNlcm5hbWU6")?;
            let user = b64(&read(&mut reader)?.unwrap_or_default());
            send(&mut out, "334 UGFzc3dvcmQ6")?;
            let pass = b64(&read(&mut reader)?.unwrap_or_default());
            authed = check(&mailbox, &user, &pass);
            if authed {
                send(&mut out, "235 2.7.0 Accepted")?;
            } else {
                send(&mut out, "535 5.7.8 BadCredentials mock")?;
            }
        } else if upper.starts_with("MAIL FROM:") {
            if !authed {
                send(&mut out, "530-5.7.0 Authentication Required.")?;
                send(&mut out, "530 5.7.0 Learn more mock")?;
                continue;
            }
            from = Some(line[10..].trim().trim_matches(['<', '>']).to_string());
            rcpts.clear();
            send(&mut out, "250 2.1.0 OK")?;
        } else if upper.starts_with("RCPT TO:") {
            if from.is_none() {
                send(&mut out, "503 5.5.1 MAIL first.")?;
                continue;
            }
            rcpts.push(line[8..].trim().trim_matches(['<', '>']).to_string());
            send(&mut out, "250 2.1.5 OK")?;
        } else if upper == "DATA" {
            if from.is_none() || rcpts.is_empty() {
                send(&mut out, "503 5.5.1 RCPT first.")?;
                continue;
            }
            send(&mut out, "354 Go ahead")?;
            let mut raw = Vec::new();
            while let Some(l) = read(&mut reader)? {
                if l == "." {
                    break;
                }
                // 去掉点填充（RFC 5321 4.5.2）。
                let l = l
                    .strip_prefix('.')
                    .filter(|_| l.starts_with(".."))
                    .unwrap_or(&l);
                raw.extend_from_slice(l.as_bytes());
                raw.extend_from_slice(b"\r\n");
            }
            mailbox.record_sent(raw.clone());
            sent.lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(Envelope {
                    from: from.take().unwrap_or_default(),
                    recipients: std::mem::take(&mut rcpts),
                    raw,
                });
            send(&mut out, "250 2.0.0 OK mock - gsmtp")?;
        } else if upper == "RSET" {
            from = None;
            rcpts.clear();
            send(&mut out, "250 2.1.5 Flushed")?;
        } else if upper == "NOOP" {
            send(&mut out, "250 2.0.0 OK")?;
        } else if upper == "QUIT" {
            send(&mut out, "221 2.0.0 closing connection")?;
            break;
        } else {
            send(&mut out, "502 5.5.1 Unrecognized command.")?;
        }
    }
    Ok(())
}
