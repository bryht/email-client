//! Letterdesk 桌面界面原型（M1）。
//!
//! 验证两件高风险的事：
//! 1. HTML 来信在清洗 + CSP + sandbox iframe 三层隔离下显示，碰不到应用接口；
//! 2. 写信框由 Rust 负责格式命令、清洗和纯文本转换，不使用 JS 编辑器库。
//!
//! 用法：
//!   letterdesk-desktop [--email 来信.eml] [--draft 草稿.txt]
//!   letterdesk-desktop --selftest     # 加载恶意样例并自动检查，输出 JSON 结果后退出

mod composer;
mod html;
mod selftest;

use composer::Composer;
use dioxus::desktop::{Config, LogicalSize, WindowBuilder};
use dioxus::prelude::*;
use mail_core::Email;
use std::sync::OnceLock;

const STYLE: &str = include_str!("style.css");
const DEFAULT_EMAIL: &[u8] = include_bytes!("../../../samples/html-newsletter.eml");
const DEFAULT_DRAFT: &str = "Hi Maya,\n\nThanks for the update. Thursday at 3 pm works for me.\n\nOn the discount, the most we can offer for the pilot is 5%.\n\nBest regards,";

#[derive(Debug, Clone)]
struct Args {
    email: Email,
    draft: String,
    selftest: bool,
}

static ARGS: OnceLock<Args> = OnceLock::new();

fn parse_args() -> Result<Args, String> {
    let mut email_path = None;
    let mut draft_path = None;
    let mut selftest = false;
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--email" => email_path = Some(it.next().ok_or("--email 需要文件路径")?),
            "--draft" => draft_path = Some(it.next().ok_or("--draft 需要文件路径")?),
            "--selftest" => selftest = true,
            "-h" | "--help" => {
                println!(
                    "用法：letterdesk-desktop [--email 来信.eml] [--draft 草稿.txt] [--selftest]"
                );
                std::process::exit(0);
            }
            other => return Err(format!("未知参数：{other}")),
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
        Some(p) => std::fs::read_to_string(&p).map_err(|e| format!("无法读取 {p}：{e}"))?,
        None => DEFAULT_DRAFT.to_string(),
    };
    Ok(Args {
        email,
        draft,
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
            }
            section { class: "compose",
                h2 { "回复 {email.from.display_name()}" }
                Composer { draft: args.draft.clone() }
            }
        }
    }
}
