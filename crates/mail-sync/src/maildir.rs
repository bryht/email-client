//! 把同步结果保存为 `.eml` 目录，并在目录中的 `sync-state.json` 记录同步状态。

use crate::SyncError;
use crate::outlook::OutlookClient;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Outlook 同步的文件夹（Graph 的 well-known 名称）。
pub const OUTLOOK_FOLDERS: [&str; 3] = ["inbox", "sentitems", "drafts"];

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SyncState {
    /// 文件夹 → deltaLink。
    #[serde(default)]
    pub delta_links: BTreeMap<String, String>,
}

impl SyncState {
    fn path(dir: &Path) -> PathBuf {
        dir.join("sync-state.json")
    }

    pub fn load(dir: &Path) -> Self {
        std::fs::read_to_string(Self::path(dir))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(Self::path(dir), text)
    }
}

/// 一次同步的统计。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyncStats {
    pub downloaded: usize,
    pub removed: usize,
}

/// 文件名只保留安全字符，避免服务器返回的 id 构成路径。
fn file_name(id: &str) -> String {
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
/// 新邮件下载为 `<id>.eml`，删除的邮件移除对应文件；完成后保存 deltaLink。
pub fn sync_outlook_to_dir(client: &mut OutlookClient, dir: &Path) -> Result<SyncStats, SyncError> {
    std::fs::create_dir_all(dir)?;
    let mut state = SyncState::load(dir);
    let mut stats = SyncStats::default();
    for folder in OUTLOOK_FOLDERS {
        let link = state.delta_links.get(folder).cloned();
        let changes = client.sync_folder(folder, link.as_deref())?;
        for m in &changes.upserted {
            let path = dir.join(file_name(&m.id));
            if !path.exists() {
                let raw = client.download(&m.id)?;
                std::fs::write(&path, raw)?;
                stats.downloaded += 1;
            }
        }
        for id in &changes.removed {
            let path = dir.join(file_name(id));
            if path.exists() {
                std::fs::remove_file(path)?;
                stats.removed += 1;
            }
        }
        // 每个文件夹完成后就保存，中途失败时下次从这里继续。
        state
            .delta_links
            .insert(folder.to_string(), changes.delta_link);
        state.save(dir)?;
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_are_sanitized() {
        assert_eq!(file_name("AAMkAG00000001"), "AAMkAG00000001.eml");
        assert_eq!(file_name("../../etc/passwd"), "______etc_passwd.eml");
        assert_eq!(file_name("a+b/c="), "a_b_c_.eml");
    }
}
