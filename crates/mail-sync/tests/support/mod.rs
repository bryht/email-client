use std::path::Path;

pub fn read_messages(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut messages = Vec::new();
    assert!(mail_store::visit_messages(dir, |key, raw| messages.push((key, raw))).unwrap());
    messages
}

/// 把首次同步的真实检查点写成旧版格式，之后的增量同步从这个目录继续。
pub fn legacy_copy(dir: &Path, state: &mail_store::SyncState) -> tempfile::TempDir {
    let old = tempfile::tempdir().unwrap();
    for (key, raw) in read_messages(dir) {
        std::fs::write(old.path().join(key), raw).unwrap();
    }
    std::fs::write(
        old.path().join("sync-state.json"),
        serde_json::to_vec(state).unwrap(),
    )
    .unwrap();
    old
}
