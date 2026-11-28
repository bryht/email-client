//! 提示词回归检查：用真实 CLI 分析 `samples/` 中的每封邮件，与 `samples/expectations.json` 对照。
//! 默认不运行；修改提示词后在有已登录 CLI 的环境中运行：
//!
//!   LETTERDESK_LIVE=claude cargo test -p ai --test live_samples -- --ignored --nocapture

use ai::Assistant;
use ai::cli::{CliBackend, CliConfig, CliKind};
use mail_core::Email;
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;

#[test]
#[ignore = "需要本机已安装并登录的 CLI，设置 LETTERDESK_LIVE 后运行"]
fn samples_match_expectations() {
    let kind = match std::env::var("LETTERDESK_LIVE").as_deref() {
        Ok("claude") => CliKind::ClaudeCode,
        Ok("ollama") => CliKind::Ollama,
        _ => {
            eprintln!("未设置 LETTERDESK_LIVE，跳过");
            return;
        }
    };
    let mut config = CliConfig::new(kind);
    config.model = std::env::var("LETTERDESK_LIVE_MODEL").ok();
    config.timeout = Duration::from_secs(300);
    let assistant = Assistant::new(CliBackend::new(config).unwrap(), "zh-CN");

    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../samples");
    let expectations: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("expectations.json")).unwrap())
            .unwrap();

    let mut failures = Vec::new();
    for (file, expect) in expectations.as_object().unwrap() {
        if file.starts_with('_') {
            continue;
        }
        let raw = std::fs::read(dir.join(file)).unwrap();
        let email = Email::parse(&raw).unwrap();
        let analysis = match assistant.analyze(&email) {
            Ok(a) => a,
            Err(e) => {
                failures.push(format!("{file}: 分析失败：{e}"));
                continue;
            }
        };
        let n = analysis.questions.len() as u64;
        let range = expect["questions"].as_array().unwrap();
        let (lo, hi) = (range[0].as_u64().unwrap(), range[1].as_u64().unwrap());
        let quotes = analysis
            .questions
            .iter()
            .filter(|q| q.quote.is_some())
            .count();
        println!(
            "{file}: needs_reply={} questions={n} quotes={quotes} language={}",
            analysis.needs_reply, analysis.language
        );
        if analysis.needs_reply != expect["needs_reply"].as_bool().unwrap() {
            failures.push(format!("{file}: needs_reply 为 {}", analysis.needs_reply));
        }
        if n < lo || n > hi {
            failures.push(format!("{file}: 问题数 {n} 不在 {lo}..={hi}"));
        }
        let lang = expect["language"].as_str().unwrap();
        if !analysis.language.to_lowercase().starts_with(lang) {
            failures.push(format!("{file}: 语言为 {}，预期 {lang}", analysis.language));
        }
        if quotes < analysis.questions.len() {
            failures.push(format!(
                "{file}: {} 个问题的原句未能在来信中核对到",
                analysis.questions.len() - quotes
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "回归检查未通过：\n{}",
        failures.join("\n")
    );
}
