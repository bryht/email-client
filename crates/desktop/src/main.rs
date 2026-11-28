//! Letterdesk 桌面界面原型（M1）。
//!
//! 验证两件高风险的事：
//! 1. HTML 来信在清洗 + CSP + sandbox iframe 三层隔离下显示，碰不到应用接口；
//! 2. 写信框由 Rust 负责格式命令、清洗和纯文本转换，不使用 JS 编辑器库。
//!
//! 用法：
//!   letterdesk-desktop [--email 来信.eml] [--draft 草稿.txt]
//!                      [--backend claude|ollama|codex] [--model 模型] [--login api-key|subscription]
//!   letterdesk-desktop --selftest     # 加载恶意样例并自动检查，输出 JSON 结果后退出

mod ai_panel;
mod composer;
mod html;
mod selftest;
mod settings;

use ai::cli::{CliKind, Login};
use ai_panel::{AiPanel, AiSettings};
use composer::Composer;
use dioxus::desktop::{Config, LogicalSize, WindowBuilder};
use dioxus::prelude::*;
use mail_core::Email;
use std::sync::OnceLock;

const STYLE: &str = include_str!("style.css");
const DEFAULT_EMAIL: &[u8] = include_bytes!("../../../samples/html-newsletter.eml");
const USAGE: &str = "用法：letterdesk-desktop [--email 来信.eml] [--draft 草稿.txt] \
[--backend claude|ollama|codex] [--model 模型] [--login api-key|subscription] [--allow-unverified] \
[--my-name 名字] [--autopilot] [--selftest]";

#[derive(Debug, Clone)]
struct Args {
    email: Email,
    /// 启动时放进写信框的草稿（纯文本）。
    draft: Option<String>,
    ai: AiSettings,
    selftest: bool,
}

static ARGS: OnceLock<Args> = OnceLock::new();

fn parse_args() -> Result<Args, String> {
    let mut email_path = None;
    let mut draft_path = None;
    let mut selftest = false;
    let mut ai = AiSettings {
        kind: CliKind::ClaudeCode,
        model: None,
        login: Login::Subscription,
        allow_unverified: false,
        ui_lang: "zh-CN".into(),
        my_name: None,
        autopilot: false,
    };
    let mut it = std::env::args().skip(1);
    let value = |it: &mut std::iter::Skip<std::env::Args>, flag: &str| {
        it.next().ok_or_else(|| format!("{flag} 需要一个值"))
    };
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--email" => email_path = Some(value(&mut it, "--email")?),
            "--draft" => draft_path = Some(value(&mut it, "--draft")?),
            "--backend" => {
                ai.kind = match value(&mut it, "--backend")?.as_str() {
                    "claude" => CliKind::ClaudeCode,
                    "ollama" => CliKind::Ollama,
                    "codex" => CliKind::Codex,
                    other => return Err(format!("不支持的 --backend：{other}")),
                }
            }
            "--model" => ai.model = Some(value(&mut it, "--model")?),
            "--login" => {
                ai.login = match value(&mut it, "--login")?.as_str() {
                    "api-key" => Login::ApiKey,
                    "subscription" => Login::Subscription,
                    other => return Err(format!("不支持的 --login：{other}")),
                }
            }
            "--allow-unverified" => ai.allow_unverified = true,
            "--ui-lang" => ai.ui_lang = value(&mut it, "--ui-lang")?,
            "--my-name" => ai.my_name = Some(value(&mut it, "--my-name")?),
            "--autopilot" => ai.autopilot = true,
            "--selftest" => selftest = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("未知参数：{other}\n{USAGE}")),
        }
    }
    let raw = if selftest {
        selftest::MALICIOUS_EMAIL.as_bytes().to_vec()
    } else {
        match email_path {
            Some(p) => std::fs::read(&p).map_err(|e| format!("无法读取 {p}：{e}"))?,
            None => DEFAULT_EMAIL.to_vec(),
        }
    };
    let email = Email::parse(&raw).map_err(|e| format!("无法解析来信：{e}"))?;
    let draft = match draft_path {
        Some(p) => Some(std::fs::read_to_string(&p).map_err(|e| format!("无法读取 {p}：{e}"))?),
        None => None,
    };
    Ok(Args {
        email,
        draft,
        ai,
        selftest,
    })
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("错误：{e}");
            std::process::exit(2);
        }
    };
    let selftest = args.selftest;
    ARGS.set(args).expect("args set once");

    let window = WindowBuilder::new()
        .with_title("Letterdesk")
        .with_inner_size(LogicalSize::new(1280.0, 820.0));
    let mut config = Config::new()
        .with_window(window)
        .with_menu(None)
        .with_disable_context_menu(false);
    if selftest {
        config = config.with_custom_head(selftest::HEAD_PROBE.to_string());
    }
    dioxus::LaunchBuilder::desktop()
        .with_cfg(config)
        .launch(App);
}

#[component]
fn App() -> Element {
    let args = ARGS.get().expect("args");
    let email = args.email.clone();
    let mut remote_images = use_signal(|| true);
    let srcdoc = html::email_srcdoc(&email.html, remote_images());
    // 写信框内容（清洗后的 HTML），写信框与回复建议面板共用。
    let content = use_signal(String::new);

    if args.selftest {
        use_future(|| async { selftest::run().await });
    }

    let to = email
        .to
        .iter()
        .map(|m| m.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let cc = email
        .cc
        .iter()
        .map(|m| m.to_string())
        .collect::<Vec<_>>()
        .join(", ");

    rsx! {
        style { {STYLE} }
        div { class: "app",
            nav { class: "rail",
                div { class: "rail-item active", "Mail" }
                div { class: "rail-item", "People" }
                div { class: "rail-item", "Calendar" }
            }
            section { class: "reader",
                header { class: "mail-header",
                    h1 { "{email.subject}" }
                    div { class: "meta", b { "From: " } "{email.from}" }
                    if !to.is_empty() { div { class: "meta", b { "To: " } "{to}" } }
                    if !cc.is_empty() { div { class: "meta", b { "Cc: " } "{cc}" } }
                    label { class: "remote-toggle",
                        input {
                            r#type: "checkbox",
                            checked: remote_images(),
                            onchange: move |e| remote_images.set(e.checked()),
                        }
                        " 自动加载外部图片（加载可能向发件人透露阅读时间）"
                    }
                }
                iframe {
                    id: "ld-email-frame",
                    class: "email-frame",
                    // 不给任何许可：不能运行脚本、不能访问父页面、不能弹窗或提交表单。
                    "sandbox": "",
                    srcdoc: "{srcdoc}",
                }
                if args.selftest {
                    // 只在自检时出现：不经清洗、只靠 CSP + sandbox 的第二道防线。
                    iframe {
                        id: "ld-raw-frame",
                        class: "email-frame raw",
                        "sandbox": "",
                        srcdoc: html::wrap_email_document(&email.html, false),
                    }
                    // 对照组：允许脚本的 iframe 必须能发出消息，证明检测本身有效。
                    iframe {
                        id: "ld-control-frame",
                        class: "email-frame raw",
                        "sandbox": "allow-scripts",
                        srcdoc: "<script>parent.postMessage('control-ran','*')</script>",
                    }
                }
                if !args.selftest {
                    AiPanel { email: email.clone(), settings: args.ai.clone(), content }
                }
            }
            section { class: "compose",
                h2 { "回复 {email.from.display_name()}" }
                Composer { content, initial: args.draft.clone() }
            }
        }
    }
}
