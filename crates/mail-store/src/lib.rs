//! 一个同步目录对应一个 SQLite 邮箱。每个文件夹的邮件变更和检查点一起提交。
use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

const DATABASE: &str = "letterdesk.sqlite3";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("本地邮箱数据库：{0}")]
    Database(#[from] rusqlite::Error),
    #[error("同步状态：{0}")]
    State(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("不支持的邮箱数据库版本 {0}")]
    Version(i64),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ImapFolderState {
    /// 只有同一 UIDVALIDITY 内的 UID 才能用于增量同步。
    pub uid_validity: u32,
    /// UID → MIME 记录的键，与旧版检查点兼容。
    pub files: BTreeMap<u32, String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SyncState {
    #[serde(default)]
    pub delta_links: BTreeMap<String, String>,
    /// 迁移时可能被旧文件名改写的 Graph ID，需要按文件夹恢复。
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub refresh_graph_ids: BTreeSet<String>,
    #[serde(default)]
    pub imap: BTreeMap<String, ImapFolderState>,
}

pub struct Store(Connection);

impl Store {
    pub fn open(dir: &Path) -> Result<Self, Error> {
        fs::create_dir_all(dir)?;
        let mut conn = Connection::open(dir.join(DATABASE))?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        match tx.pragma_query_value(None, "user_version", |row| row.get(0))? {
            1 => (),
            0 => import_legacy(&tx, dir)?,
            version => return Err(Error::Version(version)),
        }
        tx.commit()?;
        Ok(Self(conn))
    }

    pub fn begin_folder(&mut self) -> Result<FolderUpdate<'_>, Error> {
        let tx = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let json: String =
            tx.query_row("SELECT json FROM checkpoint WHERE id=1", [], |r| r.get(0))?;
        let state = serde_json::from_str(&json)?;
        Ok(FolderUpdate { tx, state })
    }
}

fn import_legacy(tx: &Transaction<'_>, dir: &Path) -> Result<(), Error> {
    let mut state: SyncState = match fs::read(dir.join("sync-state.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => SyncState::default(),
        Err(e) => return Err(e.into()),
    };
    tx.execute_batch("CREATE TABLE messages (key TEXT PRIMARY KEY NOT NULL, mime BLOB NOT NULL, legacy INTEGER NOT NULL);
                      CREATE TABLE checkpoint (id INTEGER PRIMARY KEY CHECK(id=1), json TEXT NOT NULL);")?;
    let mut ambiguous_ids = false;
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("eml"))
            && fs::metadata(&path)?.is_file()
        {
            let key = path.file_name().unwrap().to_string_lossy();
            // 老版 Graph 文件名中的下划线可能是改写过的服务端 ID。重取一次映射后才可可靠地回复。
            ambiguous_ids |= key.contains('_');
            tx.execute(
                "INSERT INTO messages VALUES (?1, ?2, 1)",
                params![key, fs::read(&path)?],
            )?;
        }
    }
    if ambiguous_ids {
        state
            .refresh_graph_ids
            .extend(state.delta_links.keys().cloned());
    }
    // 缺失的旧 IMAP 文件应由下次同步重新下载。
    let mut exists = tx.prepare("SELECT EXISTS(SELECT 1 FROM messages WHERE key=?1)")?;
    for folder in state.imap.values_mut() {
        let mut present = BTreeMap::new();
        for (uid, key) in std::mem::take(&mut folder.files) {
            if exists.query_row([&key], |r| r.get::<_, bool>(0))? {
                present.insert(uid, key);
            }
        }
        folder.files = present;
    }
    tx.execute(
        "INSERT INTO checkpoint VALUES (1, ?1)",
        [serde_json::to_string(&state)?],
    )?;
    tx.pragma_update(None, "user_version", 1)?;
    Ok(())
}

/// 按键有序读取 MIME，每次只加载一封。没有已提交的数据库时返回 false，调用方仍可读旧目录。
pub fn visit_messages(dir: &Path, mut visit: impl FnMut(String, Vec<u8>)) -> Result<bool, Error> {
    let path = dir.join(DATABASE);
    if !path.try_exists()? {
        return Ok(false);
    }
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    match conn.pragma_query_value(None, "user_version", |row| row.get(0))? {
        0 => return Ok(false),
        1 => (),
        version => return Err(Error::Version(version)),
    }
    let mut query = conn.prepare("SELECT key, mime FROM messages ORDER BY key")?;
    let mut rows = query.query([])?;
    while let Some(row) = rows.next()? {
        visit(row.get(0)?, row.get(1)?);
    }
    Ok(true)
}

pub struct FolderUpdate<'a> {
    tx: Transaction<'a>,
    pub state: SyncState,
}

impl FolderUpdate<'_> {
    pub fn insert(&self, key: &str, mime: &[u8]) -> Result<bool, Error> {
        Ok(self.tx.execute(
            "INSERT INTO messages VALUES (?1, ?2, 0) ON CONFLICT(key) DO NOTHING",
            params![key, mime],
        )? > 0)
    }

    pub fn replace(&self, key: &str, mime: &[u8]) -> Result<(), Error> {
        self.tx.execute("INSERT INTO messages VALUES (?1, ?2, 0) ON CONFLICT(key) DO UPDATE SET mime=excluded.mime, legacy=0", params![key, mime])?;
        Ok(())
    }

    pub fn delete(&self, key: &str) -> Result<bool, Error> {
        Ok(self
            .tx
            .execute("DELETE FROM messages WHERE key=?1", [key])?
            > 0)
    }

    /// 旧客户端把 Graph ID 中的部分字符改为下划线，收到真实 ID 后替换旧键。
    pub fn remove_legacy_alias(&self, key: &str, legacy: &str) -> Result<bool, Error> {
        if key == legacy {
            return Ok(false);
        }
        Ok(self
            .tx
            .execute("DELETE FROM messages WHERE key=?1 AND legacy=1", [legacy])?
            > 0)
    }

    pub fn commit(self) -> Result<(), Error> {
        self.tx.execute(
            "UPDATE checkpoint SET json=?1 WHERE id=1",
            [serde_json::to_string(&self.state)?],
        )?;
        self.tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_messages(dir: &Path) -> Vec<(String, Vec<u8>)> {
        let mut rows = Vec::new();
        assert!(visit_messages(dir, |key, mime| rows.push((key, mime))).unwrap());
        rows
    }

    #[test]
    fn migrates_once_and_rolls_back_uncommitted_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        fs::write(dir.join("gm-4.eml"), [0, 255]).unwrap();
        fs::write(
            dir.join("sync-state.json"),
            r#"{"imap":{"INBOX":{"uid_validity":2,"files":{"3":"gm-4.eml","4":"missing.eml"}}}}"#,
        )
        .unwrap();
        let mut store = Store::open(dir).unwrap();
        {
            let mut update = store.begin_folder().unwrap();
            assert_eq!(update.state.imap["INBOX"].files.len(), 1);
            update.delete("gm-4.eml").unwrap();
            update.insert("gm-5.eml", b"new").unwrap();
            update.state.imap.clear();
        }
        assert_eq!(read_messages(dir), vec![("gm-4.eml".into(), vec![0, 255])]);
        let mut update = store.begin_folder().unwrap();
        assert_eq!(update.state.imap["INBOX"].files[&3], "gm-4.eml");
        update.delete("gm-4.eml").unwrap();
        update.state.imap.clear();
        update.commit().unwrap();
        drop(store);
        let mut reopened = Store::open(dir).unwrap();
        assert!(read_messages(dir).is_empty());
        assert!(reopened.begin_folder().unwrap().state.imap.is_empty());
        assert!(dir.join("gm-4.eml").exists());
    }

    #[test]
    fn recovers_graph_ids_without_overwriting_a_distinct_message() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        fs::write(dir.join("a_b.eml"), b"old").unwrap();
        fs::write(
            dir.join("sync-state.json"),
            br#"{"delta_links":{"inbox":"cursor"}}"#,
        )
        .unwrap();
        let mut store = Store::open(dir).unwrap();
        let update = store.begin_folder().unwrap();
        assert!(update.state.refresh_graph_ids.contains("inbox"));
        assert_eq!(update.state.delta_links["inbox"], "cursor");
        assert!(update.remove_legacy_alias("a/b.eml", "a_b.eml").unwrap());
        update.replace("a/b.eml", b"slash").unwrap();
        update.replace("a_b.eml", b"underscore").unwrap();
        assert!(!update.remove_legacy_alias("a+b.eml", "a_b.eml").unwrap());
        update.commit().unwrap();
        assert_eq!(
            read_messages(dir),
            vec![
                ("a/b.eml".into(), b"slash".to_vec()),
                ("a_b.eml".into(), b"underscore".to_vec()),
            ]
        );
    }

    #[test]
    fn retries_failed_import_after_the_checkpoint_is_repaired() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        fs::write(dir.join("a.eml"), b"mail").unwrap();
        fs::write(dir.join("sync-state.json"), b"{").unwrap();
        assert!(matches!(Store::open(dir), Err(Error::State(_))));
        assert!(!visit_messages(dir, |_, _| panic!("迁移未完成")).unwrap());
        fs::write(dir.join("sync-state.json"), b"{}").unwrap();
        Store::open(dir).unwrap();
        assert_eq!(read_messages(dir), vec![("a.eml".into(), b"mail".to_vec())]);
    }
}
