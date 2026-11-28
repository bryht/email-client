//! 本地设置：保存在系统的配置目录中（可用环境变量 `LETTERDESK_CONFIG_DIR` 覆盖）。
//!
//! 目前只保存“自动分析”的选择：按 AI 连接分别记录。没有记录时，界面会先询问用户。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    /// 连接标识 → 是否开启自动分析。
    #[serde(default)]
    pub auto_analyze: BTreeMap<String, bool>,
}

fn config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("LETTERDESK_CONFIG_DIR") {
        return Some(PathBuf::from(dir));
    }
    dirs::config_dir().map(|d| d.join("Letterdesk"))
}

fn settings_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("settings.json"))
}

impl Settings {
    /// 读取设置；文件不存在或无法解析时返回默认值。
    pub fn load() -> Self {
        settings_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = settings_path() else {
            return Err(std::io::Error::other("找不到配置目录"));
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_json() {
        let mut s = Settings::default();
        s.auto_analyze.insert("claude-code".into(), false);
        let text = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<Settings>(&text).unwrap(), s);
        // 旧文件缺字段时用默认值。
        assert_eq!(
            serde_json::from_str::<Settings>("{}").unwrap(),
            Settings::default()
        );
    }
}
