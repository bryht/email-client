//! 通过用户本机安装的命令行工具调用模型。
//!
//! 每次调用都遵守 Letterdesk 的 CLI 安全限制：
//! - 用工具自身的参数关闭全部工具（命令执行、文件读写、联网等）；
//! - 在每次新建的空临时目录中运行，结束后删除；
//! - 不加载 MCP 服务器、插件和用户自己的配置；
//! - 只接受符合约定格式的 JSON 输出，其余丢弃。
//!
//! 做不到这些限制的工具标记为 [`Safety::Unverified`]，默认不允许使用。
//! 客户端只调用原样安装的官方 CLI，不读取、不保存任何登录凭据。

use crate::Backend;
use crate::error::AiError;
use crate::parse::extract_json;
use crate::prompt::Prompt;
use serde_json::Value;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use wait_timeout::ChildExt;

/// 支持的命令行工具。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliKind {
    ClaudeCode,
    Codex,
    Ollama,
}

impl CliKind {
    pub fn display_name(self) -> &'static str {
        match self {
            CliKind::ClaudeCode => "Claude Code",
            CliKind::Codex => "Codex",
            CliKind::Ollama => "Ollama",
        }
    }

    /// 默认的可执行文件名。
    pub fn default_program(self) -> &'static str {
        match self {
            CliKind::ClaudeCode => "claude",
            CliKind::Codex => "codex",
            CliKind::Ollama => "ollama",
        }
    }

    /// 这个工具能否满足全部安全限制。
    pub fn safety(self) -> Safety {
        match self {
            CliKind::ClaudeCode | CliKind::Ollama => Safety::Enforced,
            CliKind::Codex => Safety::Unverified(
                "尚未确认能完全关闭 Codex 的命令执行工具；只读沙箱仍可能读取临时目录之外的文件",
            ),
        }
    }
}

/// 安全限制的满足情况。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Safety {
    Enforced,
    Unverified(&'static str),
}

/// CLI 的登录方式，由用户在添加连接时标明。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Login {
    /// 用 API 密钥登录，按量计费。
    ApiKey,
    /// 用 Pro、Max 等订阅套餐登录。使用受服务商条款约束。
    Subscription,
}

/// 一个 CLI 连接的配置。
#[derive(Debug, Clone)]
pub struct CliConfig {
    pub kind: CliKind,
    /// 可执行文件；为空时用默认名称在 PATH 中查找。
    pub program: Option<PathBuf>,
    pub model: Option<String>,
    pub login: Login,
    pub timeout: Duration,
    /// 允许使用未能确认安全限制的工具（仅供开发验证）。
    pub allow_unverified: bool,
}

impl CliConfig {
    pub fn new(kind: CliKind) -> Self {
        Self {
            kind,
            program: None,
            model: None,
            login: Login::Subscription,
            timeout: Duration::from_secs(180),
            allow_unverified: false,
        }
    }
}

/// 要执行的命令及其标准输入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub stdin: String,
}

/// 通过 CLI 调用模型的后端。
#[derive(Debug, Clone)]
pub struct CliBackend {
    config: CliConfig,
    name: String,
}

impl CliBackend {
    pub fn new(config: CliConfig) -> Result<Self, AiError> {
        if let Safety::Unverified(reason) = config.kind.safety() {
            if !config.allow_unverified {
                return Err(AiError::Unsafe {
                    tool: config.kind.display_name().into(),
                    reason: reason.into(),
                });
            }
        }
        if config.kind == CliKind::Ollama && config.model.is_none() {
            return Err(AiError::Config(
                "使用 Ollama 时需要指定模型，例如 --model qwen3".into(),
            ));
        }
        let name = match &config.model {
            Some(m) => format!("{} ({m})", config.kind.display_name()),
            None => config.kind.display_name().to_string(),
        };
        Ok(Self { config, name })
    }

    pub fn config(&self) -> &CliConfig {
        &self.config
    }

    /// 生成要执行的命令。单独拆出，便于检查安全参数。
    pub fn invocation(&self, prompt: &Prompt) -> Invocation {
        self.invocation_with(prompt, false)
    }

    /// 工具能否边生成边输出。
    pub fn supports_streaming(&self) -> bool {
        matches!(self.config.kind, CliKind::ClaudeCode | CliKind::Ollama)
    }

    /// 生成要执行的命令；`stream` 为真时请求流式输出。
    pub fn invocation_with(&self, prompt: &Prompt, stream: bool) -> Invocation {
        let c = &self.config;
        let program: OsString = c
            .program
            .clone()
            .map(Into::into)
            .unwrap_or_else(|| c.kind.default_program().into());
        let mut args: Vec<OsString> = Vec::new();
        let mut push = |s: &str| args.push(s.into());
        let stdin = match c.kind {
            CliKind::ClaudeCode => {
                push("--print");
                push("--output-format");
                if stream {
                    // 逐段输出事件；结构化结果以 input_json_delta 片段到达。
                    push("stream-json");
                    push("--verbose");
                    push("--include-partial-messages");
                } else {
                    push("json");
                }
                // 关闭全部内置工具。
                push("--tools");
                push("");
                // 只用这里给出的（空的）MCP 配置，忽略用户和项目里的 MCP 服务器。
                push("--strict-mcp-config");
                push("--mcp-config");
                push(r#"{"mcpServers":{}}"#);
                // 只加载项目级设置；运行目录是空的临时目录，因此不会加载任何设置或钩子。
                push("--setting-sources");
                push("project");
                push("--disable-slash-commands");
                push("--no-session-persistence");
                if c.login == Login::ApiKey {
                    // 用 API 密钥登录时可用最小模式：跳过钩子、插件和 CLAUDE.md。
                    push("--bare");
                }
                push("--system-prompt");
                push(&prompt.system);
                push("--json-schema");
                push(&prompt.schema.to_string());
                if let Some(model) = &c.model {
                    push("--model");
                    push(model);
                }
                prompt.user.clone()
            }
            CliKind::Codex => {
                push("exec");
                push("--sandbox");
                push("read-only");
                push("--skip-git-repo-check");
                push("--color");
                push("never");
                if let Some(model) = &c.model {
                    push("--model");
                    push(model);
                }
                // 提示词从标准输入读取。
                push("-");
                prompt.combined()
            }
            CliKind::Ollama => {
                push("run");
                push(c.model.as_deref().unwrap_or_default());
                push("--format");
                push("json");
                prompt.combined()
            }
        };
        Invocation {
            program,
            args,
            stdin,
        }
    }

    fn parse_output(&self, stdout: &str) -> Result<Value, AiError> {
        match self.config.kind {
            CliKind::ClaudeCode => self.claude_envelope(extract_json(stdout)?),
            CliKind::Codex | CliKind::Ollama => extract_json(stdout),
        }
    }

    /// 解析 `stream-json` 输出：取最后一个 `result` 事件。
    fn parse_stream_output(&self, stdout: &str) -> Result<Value, AiError> {
        match self.config.kind {
            CliKind::ClaudeCode => {
                let result = stdout
                    .lines()
                    .rev()
                    .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
                    .find(|v| v.get("type").and_then(Value::as_str) == Some("result"))
                    .ok_or_else(|| AiError::InvalidOutput("Claude Code 输出缺少结果".into()))?;
                self.claude_envelope(result)
            }
            CliKind::Codex | CliKind::Ollama => extract_json(stdout),
        }
    }

    fn claude_envelope(&self, envelope: Value) -> Result<Value, AiError> {
        if envelope.get("is_error").and_then(Value::as_bool) == Some(true) {
            let detail = envelope
                .get("result")
                .and_then(Value::as_str)
                .unwrap_or("未知错误")
                .to_string();
            return Err(AiError::Failed {
                tool: self.name.clone(),
                code: None,
                detail,
            });
        }
        if let Some(v @ Value::Object(_)) = envelope.get("structured_output") {
            return Ok(v.clone());
        }
        match envelope.get("result").and_then(Value::as_str) {
            Some(text) => extract_json(text),
            None => Err(AiError::InvalidOutput("Claude Code 输出缺少结果".into())),
        }
    }
}

/// 把 Claude Code `stream-json` 输出的事件拼成已生成的 JSON 文本。
#[derive(Default)]
struct ClaudeStream {
    line: String,
    generated: String,
}

impl ClaudeStream {
    /// 输入一段标准输出；有新内容时返回目前为止生成的文本。
    fn feed(&mut self, chunk: &str) -> Option<&str> {
        self.line.push_str(chunk);
        let mut changed = false;
        while let Some(pos) = self.line.find('\n') {
            let line: String = self.line.drain(..=pos).collect();
            let Ok(event) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            if event.get("type").and_then(Value::as_str) != Some("stream_event") {
                continue;
            }
            let delta = &event["event"]["delta"];
            let piece = match delta.get("type").and_then(Value::as_str) {
                Some("input_json_delta") => delta.get("partial_json"),
                Some("text_delta") => delta.get("text"),
                _ => None,
            };
            if let Some(piece) = piece.and_then(Value::as_str) {
                self.generated.push_str(piece);
                changed = true;
            }
        }
        changed.then_some(self.generated.as_str())
    }
}

impl Backend for CliBackend {
    fn name(&self) -> &str {
        &self.name
    }

    fn complete(&self, prompt: &Prompt) -> Result<Value, AiError> {
        let inv = self.invocation(prompt);
        let stdout = run_isolated(&inv, self.config.timeout, &self.name, &mut |_| {})?;
        self.parse_output(&stdout)
    }

    fn complete_streaming(
        &self,
        prompt: &Prompt,
        on_partial: &mut dyn FnMut(&str),
    ) -> Result<Value, AiError> {
        if !self.supports_streaming() {
            return self.complete(prompt);
        }
        let inv = self.invocation_with(prompt, true);
        let stdout = match self.config.kind {
            CliKind::ClaudeCode => {
                let mut stream = ClaudeStream::default();
                run_isolated(&inv, self.config.timeout, &self.name, &mut |chunk| {
                    if let Some(text) = stream.feed(chunk) {
                        on_partial(text);
                    }
                })?
            }
            _ => {
                let mut generated = String::new();
                run_isolated(&inv, self.config.timeout, &self.name, &mut |chunk| {
                    generated.push_str(chunk);
                    on_partial(&generated);
                })?
            }
        };
        self.parse_stream_output(&stdout)
    }
}

/// 在新建的空临时目录中运行命令，结束后删除目录。
/// 标准输出每到达一段就交给 `on_chunk`（按完整的 UTF-8 字符切分），最后返回全部输出。
fn run_isolated(
    inv: &Invocation,
    timeout: Duration,
    tool: &str,
    on_chunk: &mut dyn FnMut(&str),
) -> Result<String, AiError> {
    let workdir = tempfile::Builder::new()
        .prefix("letterdesk-ai-")
        .tempdir()?;
    let program = resolve_program(&inv.program);
    let args = batch_safe_args(&program, &inv.args);

    let mut child = Command::new(&program)
        .args(&args)
        .current_dir(workdir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                AiError::NotInstalled(inv.program.to_string_lossy().into_owned())
            }
            _ => AiError::Io(e),
        })?;

    let mut stdin = child.stdin.take().expect("stdin is piped");
    let input = inv.stdin.clone();
    let writer = thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
    });
    let mut out = child.stdout.take().expect("stdout is piped");
    let (tx, rx) = mpsc::channel::<String>();
    let reader = thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let mut pending: Vec<u8> = Vec::new();
        loop {
            match out.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    pending.extend_from_slice(&buf[..n]);
                    let valid = match std::str::from_utf8(&pending) {
                        Ok(s) => s.len(),
                        Err(e) => e.valid_up_to(),
                    };
                    if valid > 0 {
                        let text = String::from_utf8_lossy(&pending[..valid]).into_owned();
                        pending.drain(..valid);
                        if tx.send(text).is_err() {
                            break;
                        }
                    }
                }
            }
        }
        if !pending.is_empty() {
            let _ = tx.send(String::from_utf8_lossy(&pending).into_owned());
        }
    });
    let mut err = child.stderr.take().expect("stderr is piped");
    let err_reader = thread::spawn(move || {
        let mut s = String::new();
        let _ = err.read_to_string(&mut s);
        s
    });

    let deadline = Instant::now() + timeout;
    let mut stdout = String::new();
    let kill = |child: &mut std::process::Child| {
        let _ = child.kill();
        let _ = child.wait();
        AiError::Timeout {
            tool: tool.into(),
            timeout,
        }
    };
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining) {
            Ok(chunk) => {
                on_chunk(&chunk);
                stdout.push_str(&chunk);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => return Err(kill(&mut child)),
        }
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    let status = match child.wait_timeout(remaining)? {
        Some(status) => status,
        None => return Err(kill(&mut child)),
    };
    let _ = writer.join();
    let _ = reader.join();
    let stderr = err_reader.join().unwrap_or_default();
    drop(workdir);

    if !status.success() {
        let detail = tail(if stderr.trim().is_empty() {
            &stdout
        } else {
            &stderr
        });
        return Err(AiError::Failed {
            tool: tool.into(),
            code: status.code(),
            detail,
        });
    }
    Ok(stdout)
}

fn tail(s: &str) -> String {
    let lines: Vec<&str> = s.trim().lines().collect();
    let start = lines.len().saturating_sub(5);
    lines[start..].join("\n")
}

/// 在 Windows 上，npm 安装的命令是 `.cmd` 文件，需要显式查找。
/// npm 在 Windows 上安装的命令是 `.cmd` 批处理文件（内容类似 `node cli.js %*`）。
/// cmd.exe 无法可靠传递含换行的参数，Rust 也会拒绝执行，
/// 因此目标是批处理文件时把参数里的换行换成空格。只有系统提示含换行，压平后含义不变。
fn batch_safe_args(program: &std::path::Path, args: &[OsString]) -> Vec<OsString> {
    let is_batch = program
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"));
    if !is_batch {
        return args.to_vec();
    }
    args.iter()
        .map(|a| match a.to_str() {
            Some(s) if s.contains(['\n', '\r']) => s
                .split(['\n', '\r'])
                .filter(|l| !l.trim().is_empty())
                .collect::<Vec<_>>()
                .join(" ")
                .into(),
            _ => a.clone(),
        })
        .collect()
}

#[cfg(windows)]
fn resolve_program(program: &std::ffi::OsStr) -> PathBuf {
    let p = PathBuf::from(program);
    if p.extension().is_some() || p.components().count() > 1 {
        return p;
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            for ext in ["exe", "cmd", "bat"] {
                let candidate = dir.join(&p).with_extension(ext);
                if candidate.is_file() {
                    return candidate;
                }
            }
        }
    }
    p
}

#[cfg(not(windows))]
fn resolve_program(program: &std::ffi::OsStr) -> PathBuf {
    PathBuf::from(program)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn prompt() -> Prompt {
        Prompt {
            system: "SYSTEM".into(),
            user: "USER".into(),
            schema: json!({"type": "object"}),
        }
    }

    fn args(inv: &Invocation) -> Vec<String> {
        inv.args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn has_pair(args: &[String], flag: &str, value: &str) -> bool {
        args.windows(2).any(|w| w[0] == flag && w[1] == value)
    }

    #[test]
    fn claude_invocation_disables_tools_and_external_config() {
        let backend = CliBackend::new(CliConfig::new(CliKind::ClaudeCode)).unwrap();
        let inv = backend.invocation(&prompt());
        let a = args(&inv);
        assert_eq!(inv.program, "claude");
        assert!(a.contains(&"--print".to_string()));
        assert!(has_pair(&a, "--tools", ""));
        assert!(a.contains(&"--strict-mcp-config".to_string()));
        assert!(has_pair(&a, "--mcp-config", r#"{"mcpServers":{}}"#));
        assert!(has_pair(&a, "--setting-sources", "project"));
        assert!(a.contains(&"--disable-slash-commands".to_string()));
        assert!(a.contains(&"--no-session-persistence".to_string()));
        assert!(has_pair(&a, "--system-prompt", "SYSTEM"));
        assert!(!a.contains(&"--bare".to_string()));
        // 邮件内容只经标准输入传入，不出现在命令行参数里。
        assert_eq!(inv.stdin, "USER");
        assert!(!a.iter().any(|x| x.contains("USER")));
    }

    #[test]
    fn claude_api_key_login_uses_bare_mode() {
        let mut config = CliConfig::new(CliKind::ClaudeCode);
        config.login = Login::ApiKey;
        let inv = CliBackend::new(config).unwrap().invocation(&prompt());
        assert!(args(&inv).contains(&"--bare".to_string()));
    }

    #[test]
    fn codex_is_refused_unless_explicitly_allowed() {
        let err = CliBackend::new(CliConfig::new(CliKind::Codex)).unwrap_err();
        assert!(matches!(err, AiError::Unsafe { .. }));
        let mut config = CliConfig::new(CliKind::Codex);
        config.allow_unverified = true;
        let inv = CliBackend::new(config).unwrap().invocation(&prompt());
        assert!(has_pair(&args(&inv), "--sandbox", "read-only"));
        assert_eq!(inv.stdin, "SYSTEM\n\nUSER");
    }

    #[test]
    fn ollama_requires_model_and_requests_json() {
        assert!(CliBackend::new(CliConfig::new(CliKind::Ollama)).is_err());
        let mut config = CliConfig::new(CliKind::Ollama);
        config.model = Some("qwen3".into());
        let inv = CliBackend::new(config).unwrap().invocation(&prompt());
        assert_eq!(args(&inv), vec!["run", "qwen3", "--format", "json"]);
    }

    #[test]
    fn parses_claude_structured_output_and_errors() {
        let backend = CliBackend::new(CliConfig::new(CliKind::ClaudeCode)).unwrap();
        let ok = r#"{"type":"result","is_error":false,"result":"","structured_output":{"x":1}}"#;
        assert_eq!(backend.parse_output(ok).unwrap(), json!({"x": 1}));
        let text = r#"{"type":"result","is_error":false,"result":"```json\n{\"x\":2}\n```"}"#;
        assert_eq!(backend.parse_output(text).unwrap(), json!({"x": 2}));
        let err = r#"{"type":"result","is_error":true,"result":"Not logged in"}"#;
        assert!(matches!(
            backend.parse_output(err),
            Err(AiError::Failed { .. })
        ));
    }

    #[test]
    fn claude_streaming_invocation_requests_partial_events() {
        let backend = CliBackend::new(CliConfig::new(CliKind::ClaudeCode)).unwrap();
        let a = args(&backend.invocation_with(&prompt(), true));
        assert!(has_pair(&a, "--output-format", "stream-json"));
        assert!(a.contains(&"--verbose".to_string()));
        assert!(a.contains(&"--include-partial-messages".to_string()));
        assert!(has_pair(&a, "--tools", ""));
    }

    #[test]
    fn claude_stream_events_are_assembled() {
        let events = [
            r#"{"type":"system","subtype":"init"}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_start","content_block":{"type":"tool_use","name":"StructuredOutput"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"input_json_delta","partial_json":"{\"body\": \"Hi Ma"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"input_json_delta","partial_json":"ya\"}"}}}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"result":"","structured_output":{"body":"Hi Maya"}}"#,
        ];
        let all = events.join("\n") + "\n";
        let mut stream = ClaudeStream::default();
        let mut seen = Vec::new();
        // 故意按不对齐的位置切分，模拟管道里的任意分段。
        for chunk in all.as_bytes().chunks(37) {
            if let Some(text) = stream.feed(std::str::from_utf8(chunk).unwrap()) {
                seen.push(text.to_string());
            }
        }
        assert_eq!(seen.last().unwrap(), r#"{"body": "Hi Maya"}"#);
        assert_eq!(
            crate::partial_string_field(&seen[0], "body").as_deref(),
            Some("Hi Ma")
        );
        let backend = CliBackend::new(CliConfig::new(CliKind::ClaudeCode)).unwrap();
        assert_eq!(
            backend.parse_stream_output(&all).unwrap(),
            json!({"body": "Hi Maya"})
        );
    }

    #[test]
    fn batch_files_get_single_line_args() {
        let args: Vec<OsString> = vec![
            "--system-prompt".into(),
            "line one\nline two\r\n- three".into(),
        ];
        let flat = batch_safe_args(std::path::Path::new("C:/npm/claude.CMD"), &args);
        assert_eq!(flat[1], OsString::from("line one line two - three"));
        let same = batch_safe_args(std::path::Path::new("/usr/bin/claude"), &args);
        assert_eq!(same, args);
    }

    #[test]
    fn missing_program_is_reported() {
        let mut config = CliConfig::new(CliKind::ClaudeCode);
        config.program = Some("letterdesk-no-such-cli".into());
        let backend = CliBackend::new(config).unwrap();
        assert!(matches!(
            backend.complete(&prompt()),
            Err(AiError::NotInstalled(_))
        ));
    }
}
