//! 模拟服务共用的邮箱数据：一组带文件夹、已读状态和版本号的邮件。

use mail_core::Email;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// 邮件所在的文件夹。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Folder {
    Inbox,
    Sent,
    Drafts,
}

impl Folder {
    /// Microsoft Graph 的 well-known 文件夹名。
    pub fn graph_name(self) -> &'static str {
        match self {
            Folder::Inbox => "inbox",
            Folder::Sent => "sentitems",
            Folder::Drafts => "drafts",
        }
    }

    pub fn from_graph_name(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "inbox" => Some(Folder::Inbox),
            "sentitems" => Some(Folder::Sent),
            "drafts" => Some(Folder::Drafts),
            _ => None,
        }
    }
}

/// 一封存储的邮件。
#[derive(Debug, Clone)]
pub struct StoredMessage {
    /// 全局序号（从 1 开始），也用作 IMAP UID 和 Gmail 的 X-GM-MSGID 的基础。
    pub n: u64,
    pub folder: Folder,
    pub raw: Vec<u8>,
    pub email: Email,
    pub seen: bool,
    pub deleted: bool,
    /// 最近一次变化时的邮箱版本号，用于增量同步。
    pub version: u64,
}

impl StoredMessage {
    /// Microsoft Graph 的邮件 id（不透明字符串）。
    pub fn graph_id(&self) -> String {
        format!("AAMkAG{:08}", self.n)
    }

    /// Gmail 的 X-GM-MSGID：同一封邮件在所有文件夹里相同。
    pub fn gm_msgid(&self) -> u64 {
        1_790_000_000_000_000_000 + self.n
    }
}

#[derive(Debug, Default)]
pub struct MailboxData {
    pub owner: String,
    pub owner_name: String,
    pub messages: Vec<StoredMessage>,
    pub version: u64,
}

/// 线程安全的共享邮箱。
#[derive(Debug, Clone)]
pub struct Mailbox {
    inner: Arc<Mutex<MailboxData>>,
}

impl Mailbox {
    pub fn new(owner: &str, owner_name: &str) -> Self {
        Self {
            inner: Arc::new(Mutex::new(MailboxData {
                owner: owner.to_lowercase(),
                owner_name: owner_name.to_string(),
                ..Default::default()
            })),
        }
    }

    /// 读取目录中所有 `.eml`：自己发出的放进“已发送”，其余放进收件箱。
    pub fn load_dir(&self, dir: &Path) -> std::io::Result<usize> {
        let mut paths: Vec<_> = std::fs::read_dir(dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("eml")))
            .collect();
        paths.sort();
        let mut count = 0;
        for p in paths {
            let raw = std::fs::read(&p)?;
            if self.add_auto(raw).is_some() {
                count += 1;
            }
        }
        Ok(count)
    }

    pub fn owner(&self) -> (String, String) {
        let d = self.lock();
        (d.owner.clone(), d.owner_name.clone())
    }

    /// 按发件人自动判断文件夹后加入。无法解析的邮件返回 `None`。
    pub fn add_auto(&self, raw: Vec<u8>) -> Option<u64> {
        let email = Email::parse(&raw).ok()?;
        let owner = self.lock().owner.clone();
        let folder = if email.from.address.eq_ignore_ascii_case(&owner) {
            Folder::Sent
        } else {
            Folder::Inbox
        };
        Some(self.add(folder, raw, email))
    }

    /// 投递一封新邮件到收件箱（模拟有人发来邮件）。
    pub fn deliver(&self, raw: Vec<u8>) -> Option<u64> {
        let email = Email::parse(&raw).ok()?;
        Some(self.add(Folder::Inbox, raw, email))
    }

    /// 把自己发出的邮件放进“已发送”。
    pub fn record_sent(&self, raw: Vec<u8>) -> Option<u64> {
        let email = Email::parse(&raw).ok()?;
        Some(self.add(Folder::Sent, raw, email))
    }

    /// 加入指定文件夹（IMAP APPEND，例如保存草稿）。
    pub fn append(&self, folder: Folder, raw: Vec<u8>) -> Option<u64> {
        let email = Email::parse(&raw).ok()?;
        Some(self.add(folder, raw, email))
    }

    fn add(&self, folder: Folder, raw: Vec<u8>, email: Email) -> u64 {
        let mut d = self.lock();
        d.version += 1;
        let n = d.messages.len() as u64 + 1;
        let version = d.version;
        let seen = folder != Folder::Inbox;
        d.messages.push(StoredMessage {
            n,
            folder,
            raw,
            email,
            seen,
            deleted: false,
            version,
        });
        n
    }

    /// 删除一封邮件（保留记录，以便增量同步报告删除）。
    pub fn delete(&self, n: u64) -> bool {
        self.change(n, |m| m.deleted = true)
    }

    pub fn set_seen(&self, n: u64, seen: bool) -> bool {
        self.change(n, |m| m.seen = seen)
    }

    /// 替换原文和/或移动文件夹（例如修改草稿、发送草稿）。
    pub fn update(&self, n: u64, folder: Option<Folder>, raw: Option<Vec<u8>>) -> bool {
        let parsed = match raw {
            Some(raw) => match Email::parse(&raw) {
                Ok(email) => Some((raw, email)),
                Err(_) => return false,
            },
            None => None,
        };
        self.change(n, |m| {
            if let Some((raw, email)) = parsed {
                m.raw = raw;
                m.email = email;
            }
            if let Some(f) = folder {
                m.folder = f;
                m.seen = true;
            }
        })
    }

    /// 修改一封未删除的邮件；只有真的找到时才推进版本号。
    fn change(&self, n: u64, f: impl FnOnce(&mut StoredMessage)) -> bool {
        let mut d = self.lock();
        let version = d.version + 1;
        match d.messages.iter_mut().find(|m| m.n == n && !m.deleted) {
            Some(m) => {
                f(m);
                m.version = version;
                d.version = version;
                true
            }
            None => false,
        }
    }

    /// 读取一份快照，避免长时间持锁。
    pub fn snapshot(&self) -> (Vec<StoredMessage>, u64) {
        let d = self.lock();
        (d.messages.clone(), d.version)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, MailboxData> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Unix 时间戳 → (年, 月, 日, 时, 分, 秒)，UTC。
pub fn civil(ts: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = ts.div_euclid(86_400);
    let secs = ts.rem_euclid(86_400);
    // Howard Hinnant 的 civil_from_days 算法。
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (
        y,
        m,
        d,
        (secs / 3600) as u32,
        ((secs % 3600) / 60) as u32,
        (secs % 60) as u32,
    )
}

/// (年, 月, 日) → 当天 0 点的 Unix 时间戳，UTC。
pub fn days_to_ts(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146_097 + doe - 719_468) * 86_400
}

pub const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// ISO 8601（Graph 使用）：`2026-03-03T10:00:00Z`。
pub fn iso8601(ts: i64) -> String {
    let (y, mo, d, h, mi, s) = civil(ts);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// IMAP INTERNALDATE：`03-Mar-2026 10:00:00 +0000`。
pub fn imap_date(ts: i64) -> String {
    let (y, mo, d, h, mi, s) = civil(ts);
    format!(
        "{d:02}-{}-{y:04} {h:02}:{mi:02}:{s:02} +0000",
        MONTHS[(mo - 1) as usize]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_conversions_round_trip() {
        assert_eq!(iso8601(1_772_532_000), "2026-03-03T10:00:00Z");
        assert_eq!(imap_date(1_772_532_000), "03-Mar-2026 10:00:00 +0000");
        assert_eq!(days_to_ts(2026, 3, 3), 1_772_496_000);
        assert_eq!(days_to_ts(1970, 1, 1), 0);
        assert_eq!(civil(days_to_ts(2024, 2, 29)), (2024, 2, 29, 0, 0, 0));
    }

    #[test]
    fn auto_folder_and_versions() {
        let mb = Mailbox::new("Lin@Example.com", "Lin Chen");
        let inbox = mb
            .add_auto(
                b"From: maya@example.com\r\nTo: lin@example.com\r\nSubject: a\r\n\r\nx".to_vec(),
            )
            .unwrap();
        let sent = mb
            .add_auto(
                b"From: lin@example.com\r\nTo: maya@example.com\r\nSubject: b\r\n\r\ny".to_vec(),
            )
            .unwrap();
        let (msgs, version) = mb.snapshot();
        assert_eq!(msgs[0].folder, Folder::Inbox);
        assert!(!msgs[0].seen);
        assert_eq!(msgs[1].folder, Folder::Sent);
        assert_eq!(version, 2);
        assert!(mb.delete(inbox));
        assert!(!mb.delete(inbox));
        assert!(mb.set_seen(sent, false));
        assert_eq!(mb.snapshot().1, 4);
    }
}
