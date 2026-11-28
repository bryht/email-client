//! Letterdesk 桌面界面原型。
//!
//! - 左栏：按人列出对话（`conversation::group` 的结果）；
//! - 中间：所选对话的时间线（按话题分隔，来信 HTML 在清洗 + CSP + sandbox iframe 中隔离显示）
//!   与“回复建议”面板；
//! - 右侧：Rust 负责格式与清洗的写信框。
//!
//! 用法：
//!   letterdesk-desktop [--mailbox 目录] [--email 来信.eml ...] [--me 我的地址 ...] [--draft 草稿.txt]
//!                      [--backend claude|ollama|codex] [--model 模型] [--login api-key|subscription]
//!   letterdesk-desktop --selftest     # 加载恶意样例并自动检查，输出 JSON 结果后退出

mod ai_panel;
mod composer;
mod html;
mod inbox;
mod selftest;
mod settings;

use ai::cli::{CliKind, Login};
use ai_panel::{AiPanel, AiSettings};
use composer::{Composer, write_editor_html};
use dioxus::desktop::{Config, LogicalSize, WindowBuilder};
use dioxus::prelude::*;
use inbox::{ConversationList, Timeline, active_message};
use mail_core::Email;
use std::path::Path;
use std::sync::OnceLock;

const STYLE: &str = include_str!("style.css");
const DEFAULT_EMAIL: &[u8] = include_bytes!("../../../samples/html-newsletter.eml");
const USAGE: &str = "用法：letterdesk-desktop [--mailbox 目录] [--email 来信.eml ...] [--me 我的地址 ...] \
[--draft 草稿.txt] [--backend claude|ollama|codex] [--model 模型] [--login api-key|subscription] \
[--allow-unverified] [--my-name 名字] [--autopilot] [--selftest]";

#[derive(Debug, Clone)]
pub struct Args {
    pub emails: Vec<Email>,
    /// 自己的地址和别名，用于归组和计算回复对象。
    pub me: Vec<String>,
    /// 启动时放进写信框的草稿（纯文本）。
    pub draft: Option<String>,
    pub ai: AiSettings,
    pub selftest: bool,
}

static ARGS: OnceLock<Args> = OnceLock::new();

fn load_eml(path: &Path) -> Result<Email, String> {
    let raw = std::fs::read(path).map_err(|e| format!("无法读取 {}：{e}", path.display()))?;
    Email::parse(&raw).map_err(|e| format!("无法解析 {}：{e}", path.display()))
}

fn load_mailbox(dir: &Path) -> Result<Vec<Email>, String> {
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("无法读取目录 {}：{e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("eml")))
        .collect();
    paths.sort();
    let mut emails = Vec::new();
    for p in paths {
        match load_eml(&p) {
            Ok(e) => emails.push(e),
            Err(err) => eprintln!("跳过：{err}"),
        }
    }
    Ok(emails)
}

fn parse_args() -> Result<Args, String> {
    let mut emails = Vec::new();
    let mut me = Vec::new();
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
            "--email" => emails.push(load_eml(Path::new(&value(&mut it, "--email")?))?),
            "--mailbox" => emails.extend(load_mailbox(Path::new(&value(&mut it, "--mailbox")?))?),
            "--me" => me.push(value(&mut it, "--me")?),
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
    if selftest {
        emails =
            vec![Email::parse(selftest::MALICIOUS_EMAIL.as_bytes()).map_err(|e| e.to_string())?];
    } else if emails.is_empty() {
        emails.push(Email::parse(DEFAULT_EMAIL).map_err(|e| e.to_string())?);
    }
    let draft = match draft_path {
        Some(p) => Some(std::fs::read_to_string(&p).map_err(|e| format!("无法读取 {p}：{e}"))?),
        None => None,
    };
    Ok(Args {
        emails,
        me,
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
    let conversations = use_hook(|| conversation::group(&args.emails, &args.me));
    let mut selected = use_signal(|| 0usize);
    let remote_images = use_signal(|| true);
    // 写信框内容（清洗后的 HTML），写信框与回复建议面板共用。
    let mut content = use_signal(String::new);

    if args.selftest {
        use_future(|| async { selftest::run().await });
    }

    let conv = conversations[selected().min(conversations.len() - 1)].clone();
    let active = active_message(&conv, &args.emails, &args.me);
    let active_email = args.emails[active].clone();
    let active_key = format!("{}-{active}", conv.id);

    rsx! {
        style { {STYLE} }
        div { class: "app",
            nav { class: "rail",
                div { class: "rail-item active", "Mail" }
                div { class: "rail-item", "People" }
                div { class: "rail-item", "Calendar" }
            }
            ConversationList {
                conversations: conversations.clone(),
                emails: args.emails.clone(),
                me: args.me.clone(),
                selected: selected(),
                on_select: move |i: usize| {
                    if i != selected() {
                        selected.set(i);
                        // 换对话时清空写信框；回复建议面板随 key 重建。
                        write_editor_html("");
                        content.set(String::new());
                    }
                },
            }
            section { class: "reader",
                Timeline {
                    conversation: conv.clone(),
                    emails: args.emails.clone(),
                    active,
                    remote_images,
                }
                if args.selftest {
                    // 只在自检时出现：不经清洗、只靠 CSP + sandbox 的第二道防线。
                    iframe {
                        id: "ld-raw-frame",
                        class: "email-frame raw",
                        "sandbox": "",
                        srcdoc: html::wrap_email_document(&active_email.html, false),
                    }
                    // 对照组：允许脚本的 iframe 必须能发出消息，证明检测本身有效。
                    iframe {
                        id: "ld-control-frame",
                        class: "email-frame raw",
                        "sandbox": "allow-scripts",
                        srcdoc: "<script>parent.postMessage('control-ran','*')</script>",
                    }
                } else {
                    AiPanel {
                        key: "{active_key}",
                        email: active_email.clone(),
                        settings: args.ai.clone(),
                        content,
                    }
                }
            }
            section { class: "compose",
                h2 { "回复 {active_email.from.display_name()}" }
                Composer { content, initial: args.draft.clone() }
            }
        }
    }
}
