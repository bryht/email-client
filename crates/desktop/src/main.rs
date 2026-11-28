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
mod reply;
mod selftest;
mod settings;

use ai::cli::{CliKind, Login};
use ai_panel::{AiPanel, AiSettings};
use composer::{Composer, write_editor_html};
use dioxus::desktop::{Config, LogicalSize, WindowBuilder};
use dioxus::prelude::*;
use inbox::{ConversationList, Timeline, active_message};
use mail_core::Email;
use mail_sync::account::AccountConfig;
use reply::ReplyBar;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const STYLE: &str = include_str!("style.css");
const DEFAULT_EMAIL: &[u8] = include_bytes!("../../../samples/html-newsletter.eml");
const USAGE: &str = "用法：letterdesk-desktop [--mailbox 目录] [--account 账号.json] [--email 来信.eml ...] [--me 我的地址 ...] \
[--draft 草稿.txt] [--backend claude|ollama|codex] [--model 模型] [--login api-key|subscription] \
[--allow-unverified] [--my-name 名字] [--autopilot] [--selftest]";

#[derive(Debug, Clone)]
pub struct Args {
    pub emails: Vec<Email>,
    /// 每封邮件的来源标识（Outlook 使用 Graph ID），与 `emails` 一一对应。
    pub ids: Vec<String>,
    /// `--mailbox` 目录；配置了账号时同步到这里。
    pub mailbox_dir: Option<PathBuf>,
    /// `--account` 账号文件。
    pub account: Option<AccountConfig>,
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

fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// 同步邮箱优先读 SQLite；普通 .eml 目录仍可直接浏览。
pub fn load_mailbox(dir: &Path) -> Result<Vec<(Email, String)>, String> {
    let mut emails = Vec::new();
    let stored = mail_store::visit_messages(dir, |key, raw| match Email::parse(&raw) {
        Ok(email) => {
            // 逻辑键沿用 .eml 扩展名；Graph ID 自身可以包含点或路径分隔符。
            let id = key.rsplit_once('.').map_or(key.as_str(), |(id, _)| id);
            emails.push((email, id.to_owned()));
        }
        Err(e) => eprintln!("跳过 {key}：{e}"),
    })
    .map_err(|e| e.to_string())?;
    if stored {
        return Ok(emails);
    }
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("无法读取目录 {}：{e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("eml")))
        .collect();
    paths.sort();
    for p in paths {
        match load_eml(&p) {
            Ok(e) => emails.push((e, stem(&p))),
            Err(err) => eprintln!("跳过：{err}"),
        }
    }
    Ok(emails)
}

fn parse_args() -> Result<Args, String> {
    let mut emails: Vec<(Email, String)> = Vec::new();
    let mut mailbox_dir: Option<PathBuf> = None;
    let mut account: Option<AccountConfig> = None;
    let mut me: Vec<String> = Vec::new();
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
            "--email" => {
                let p = PathBuf::from(value(&mut it, "--email")?);
                emails.push((load_eml(&p)?, stem(&p)));
            }
            "--mailbox" => {
                let dir = PathBuf::from(value(&mut it, "--mailbox")?);
                if dir.exists() {
                    emails.extend(load_mailbox(&dir)?);
                }
                mailbox_dir = Some(dir);
            }
            "--account" => {
                let p = PathBuf::from(value(&mut it, "--account")?);
                account = Some(AccountConfig::load(&p).map_err(|e| e.to_string())?);
            }
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
        emails = vec![(
            Email::parse(selftest::MALICIOUS_EMAIL.as_bytes()).map_err(|e| e.to_string())?,
            "selftest".into(),
        )];
    } else if emails.is_empty() {
        emails.push((
            Email::parse(DEFAULT_EMAIL).map_err(|e| e.to_string())?,
            "sample".into(),
        ));
    }
    if let Some(a) = &account
        && !me.iter().any(|m| m.eq_ignore_ascii_case(a.address()))
    {
        me.push(a.address().to_string());
    }
    let (emails, ids): (Vec<Email>, Vec<String>) = emails.into_iter().unzip();
    let draft = match draft_path {
        Some(p) => Some(std::fs::read_to_string(&p).map_err(|e| format!("无法读取 {p}：{e}"))?),
        None => None,
    };
    Ok(Args {
        emails,
        ids,
        mailbox_dir,
        account,
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
    // 邮件与对应的文件名；同步后整体替换。
    let mut mail = use_signal(|| (args.emails.clone(), args.ids.clone()));
    let me = args.me.clone();
    let conversations = use_memo(move || conversation::group(&mail.read().0, &me));
    let mut selected = use_signal(|| 0usize);
    let remote_images = use_signal(|| true);
    // 写信框内容（清洗后的 HTML），写信框与回复建议面板共用。
    let mut content = use_signal(String::new);
    let mut sync_status = use_signal(|| None::<String>);
    let mut syncing = use_signal(|| false);

    if args.selftest {
        use_future(|| async { selftest::run().await });
    }

    // 同步账号到 --mailbox 目录，然后重新读取。
    let mut sync = move || {
        let (Some(account), Some(dir)) = (args.account.clone(), args.mailbox_dir.clone()) else {
            return;
        };
        if syncing() {
            return;
        }
        syncing.set(true);
        sync_status.set(Some("正在同步……".into()));
        spawn(async move {
            let (tx, rx) = futures_channel::oneshot::channel();
            std::thread::spawn(move || {
                let result = account
                    .sync(&mail_sync::credentials::KeyringStore, &dir)
                    .map_err(|e| e.to_string())
                    .and_then(|stats| load_mailbox(&dir).map(|m| (stats, m)));
                let _ = tx.send(result);
            });
            match rx.await {
                Ok(Ok((stats, loaded))) => {
                    let (emails, ids): (Vec<Email>, Vec<String>) = loaded.into_iter().unzip();
                    if !emails.is_empty() {
                        mail.set((emails, ids));
                    }
                    sync_status.set(Some(format!(
                        "同步完成：新增 {}，删除 {}",
                        stats.downloaded, stats.removed
                    )));
                }
                Ok(Err(e)) => sync_status.set(Some(format!("同步失败：{e}"))),
                Err(_) => sync_status.set(Some("同步失败：后台任务意外结束".into())),
            }
            syncing.set(false);
        });
    };

    let convs = conversations.read().clone();
    let (emails, ids) = mail.read().clone();
    let conv = convs[selected().min(convs.len().saturating_sub(1))].clone();
    let active = active_message(&conv, &emails, &args.me);
    let active_email = emails[active].clone();
    let active_id = ids[active].clone();
    let active_key = format!("{}-{active_id}", conv.id);
    let can_sync = args.account.is_some() && args.mailbox_dir.is_some();

    rsx! {
        style { {STYLE} }
        div { class: "app",
            nav { class: "rail",
                div { class: "rail-item active", "Mail" }
                div { class: "rail-item", "People" }
                div { class: "rail-item", "Calendar" }
            }
            div { class: "conv-col",
                if can_sync {
                    div { class: "sync-bar",
                        button {
                            class: "ghost small",
                            disabled: syncing(),
                            onclick: move |_| sync(),
                            if syncing() { "同步中…" } else { "同步" }
                        }
                        if let Some(s) = sync_status.read().clone() {
                            span { class: "ai-note", "{s}" }
                        }
                    }
                }
                ConversationList {
                    conversations: convs.clone(),
                    emails: emails.clone(),
                    me: args.me.clone(),
                    selected: selected(),
                    on_select: move |i: usize| {
                        if i != selected() {
                            selected.set(i);
                            // 换对话时清空写信框；回复建议面板和回复栏随 key 重建。
                            write_editor_html("");
                            content.set(String::new());
                        }
                    },
                }
            }
            section { class: "reader",
                Timeline {
                    conversation: conv.clone(),
                    emails: emails.clone(),
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
                ReplyBar {
                    key: "{active_key}",
                    email: active_email.clone(),
                    original_id: active_id.clone(),
                    me: args.me.clone(),
                    my_name: args.ai.my_name.clone(),
                    account: args.account.clone(),
                    content,
                    on_sent: move |_| {
                        // 已发送：清空写信框，同步后回信出现在时间线里。
                        write_editor_html("");
                        content.set(String::new());
                        sync();
                    },
                }
                Composer { content, initial: args.draft.clone() }
            }
        }
    }
}
