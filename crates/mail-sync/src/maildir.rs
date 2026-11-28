//! 按文件夹向 SQLite 提交 Outlook 的邮件与增量检查点。

use crate::SyncError;
use crate::outlook::OutlookClient;
use mail_store::Store;
use std::collections::BTreeSet;
use std::path::Path;

/// Outlook 同步的文件夹（Graph 的 well-known 名称）。
pub const OUTLOOK_FOLDERS: [&str; 3] = ["inbox", "sentitems", "drafts"];

/// 一次同步的统计。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyncStats {
    pub downloaded: usize,
    pub removed: usize,
}

/// 文件名只保留安全字符，避免服务器返回的 id 构成路径。
fn legacy_file_name(id: &str) -> String {
    let safe: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{safe}.eml")
}

/// 增量同步 Outlook 的收件箱、已发送和草稿到 `dir`。
pub fn sync_outlook_to_dir(client: &mut OutlookClient, dir: &Path) -> Result<SyncStats, SyncError> {
    let mut store = Store::open(dir)?;
    let mut stats = SyncStats::default();
    for folder in OUTLOOK_FOLDERS {
        let mut update = store.begin_folder()?;
        let link = update.state.delta_links.get(folder);
        let mut ids = BTreeSet::new();
        if update.state.refresh_graph_ids.remove(folder) {
            // 先恢复不可逆的旧文件名，再用原检查点追回期间的新增与删除。
            ids.extend(
                client
                    .sync_folder(folder, None)?
                    .upserted
                    .into_iter()
                    .map(|m| m.id),
            );
        }
        let changes = client.sync_folder(folder, link.map(String::as_str))?;
        ids.extend(changes.upserted.into_iter().map(|m| m.id));
        for id in &changes.removed {
            ids.remove(id);
            let key = format!("{id}.eml");
            let legacy_removed = update.remove_legacy_alias(&key, &legacy_file_name(id))?;
            if update.delete(&key)? || legacy_removed {
                stats.removed += 1;
            }
        }
        for id in ids {
            let raw = client.download(&id)?;
            let key = format!("{id}.eml");
            update.remove_legacy_alias(&key, &legacy_file_name(&id))?;
            update.replace(&key, &raw)?;
            stats.downloaded += 1;
        }
        update
            .state
            .delta_links
            .insert(folder.to_string(), changes.delta_link);
        update.commit()?;
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_file_names_are_sanitized() {
        assert_eq!(legacy_file_name("AAMkAG00000001"), "AAMkAG00000001.eml");
        assert_eq!(legacy_file_name("../../etc/passwd"), "______etc_passwd.eml");
        assert_eq!(legacy_file_name("a+b/c="), "a_b_c_.eml");
    }
}
