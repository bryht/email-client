//! Windows 上 npm 安装的 CLI 是 `.cmd` 批处理文件（`node cli.js %*`）。
//! 这里用同样结构的假 CLI 验证：含换行、引号、百分号、JSON 的参数和
//! 含中文的标准输入能完整传到 node 进程。需要 PATH 中有 node。

#![cfg(windows)]

use ai::cli::{CliBackend, CliConfig, CliKind};
use ai::{Backend, Prompt};
use serde_json::{Value, json};

const ECHO_JS: &str = r#"
let input = '';
process.stdin.setEncoding('utf8');
process.stdin.on('data', c => input += c);
process.stdin.on('end', () => {
  console.log(JSON.stringify({
    type: 'result', is_error: false, result: '',
    structured_output: { argv: process.argv.slice(2), stdin: input }
  }));
});
"#;

#[test]
fn npm_style_cmd_shim_receives_arguments_intact() {
    if std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("PATH 中没有 node，跳过");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("echo.js"), ECHO_JS).unwrap();
    let shim = dir.path().join("fakeclaude.cmd");
    std::fs::write(&shim, "@node \"%~dp0echo.js\" %*\r\n").unwrap();

    let mut config = CliConfig::new(CliKind::ClaudeCode);
    config.program = Some(shim);
    let backend = CliBackend::new(config).unwrap();

    let system = "Rules:\n- Say \"hi\" & keep 100% of facts <safe>\n- Second line | end";
    let user = "邮件正文：周四下午三点可以吗？优惠 10%。";
    let prompt = Prompt {
        system: system.into(),
        user: user.into(),
        schema: json!({"type": "object", "properties": {"a": {"type": "string"}}}),
    };
    let out: Value = backend.complete(&prompt).expect("调用 .cmd 失败");
    let argv: Vec<String> = serde_json::from_value(out["argv"].clone()).unwrap();
    println!("argv = {argv:?}");

    let pos = argv.iter().position(|a| a == "--system-prompt").unwrap();
    assert_eq!(
        argv[pos + 1],
        "Rules: - Say \"hi\" & keep 100% of facts <safe> - Second line | end"
    );
    let pos = argv.iter().position(|a| a == "--json-schema").unwrap();
    let schema: Value = serde_json::from_str(&argv[pos + 1]).unwrap();
    assert_eq!(schema, prompt.schema);
    let pos = argv.iter().position(|a| a == "--tools").unwrap();
    assert_eq!(argv[pos + 1], "");
    assert_eq!(out["stdin"].as_str().unwrap(), user);
}
