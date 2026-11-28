//! 回复栏：收件人、抄送（默认“回复所有人”）和“发送”。
//! 发送在后台线程执行；结果按 PLAN 第 1 节“发送中断”显示。

use crate::composer::read_editor_html;
use crate::html::{clean_composer_html, html_to_text};
use dioxus::prelude::*;
use futures_channel::oneshot;
use mail_core::{Email, Mailbox};
use mail_sync::account::AccountConfig;
use mail_sync::compose::ReplyDraft;
use mail_sync::credentials::KeyringStore;
use mail_sync::send::SendStatus;

/// `Maya Patel <maya@example.com>, alex@example.com` → 地址列表。
pub fn parse_list(s: &str) -> Result<Vec<Mailbox>, String> {
    s.split([',', ';', '，', '；'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (name, addr) = match (p.rfind('<'), p.rfind('>')) {
                (Some(a), Some(b)) if a < b => {
                    (Some(p[..a].trim().trim_matches('"')), p[a + 1..b].trim())
                }
                _ => (None, p),
            };
            let valid = addr
                .split_once('@')
                .is_some_and(|(l, d)| !l.is_empty() && d.contains('.'));
            if valid {
                Ok(Mailbox::new(name.filter(|n| !n.is_empty()), addr))
            } else {
                Err(format!("地址无效：{p}"))
            }
        })
        .collect()
}

pub fn format_list(list: &[Mailbox]) -> String {
    list.iter()
        .map(|m| m.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Props, Clone, PartialEq)]
pub struct ReplyBarProps {
    pub email: Email,
    /// 原信在同步目录中的文件名（不含扩展名），Outlook 用它找到原信。
    pub original_id: String,
    pub me: Vec<String>,
    pub my_name: Option<String>,
    pub account: Option<AccountConfig>,
    /// 写信框内容（清洗后的 HTML）。
    pub content: Signal<String>,
    /// 确认已发送后调用（例如触发同步，让回信出现在时间线里）。
    pub on_sent: EventHandler<()>,
}

#[component]
pub fn ReplyBar(props: ReplyBarProps) -> Element {
    let defaults = props.email.reply_all_recipients(&props.me);
    let mut to = use_signal(|| format_list(&defaults.to));
    let mut cc = use_signal(|| format_list(&defaults.cc));
    // (样式, 文字, 是否可重试)
    let mut status = use_signal(|| None::<(&'static str, String, bool)>);
    let mut busy = use_signal(|| false);
    let email = use_signal(|| props.email.clone());
    let account = use_signal(|| props.account.clone());
    let mut content = props.content;
    let on_sent = props.on_sent;
    let original_id = use_signal(|| props.original_id.clone());
    let my_name = use_signal(|| props.my_name.clone());

    let mut send = move || {
        if busy() {
            return;
        }
        let Some(account) = account.read().clone() else {
            return;
        };
        let original = email.read().clone();
        let original_id = original_id.read().clone();
        let my_name = my_name.read().clone();
        busy.set(true);
        spawn(async move {
            // 发送时直接读取编辑区，保证发出的就是屏幕上看到的内容（最后几个字可能还没同步）。
            let html = match read_editor_html().await {
                Some(raw) => clean_composer_html(&raw),
                None => content.read().clone(),
            };
            content.set(html.clone());
            let text = html_to_text(&html);
            let fail = |msg: String| (Some(("error", msg, false)), None);
            let checked = if text.trim().is_empty() {
                fail("写信框是空的。".into())
            } else {
                match (parse_list(&to.read()), parse_list(&cc.read())) {
                    (Ok(t), Ok(c)) if t.is_empty() && c.is_empty() => fail("请填写收件人。".into()),
                    (Ok(t), Ok(c)) => (
                        None,
                        Some(ReplyDraft {
                            to: t,
                            cc: c,
                            text,
                            html,
                        }),
                    ),
                    (Err(e), _) | (_, Err(e)) => fail(e),
                }
            };
            let draft = match checked {
                (_, Some(draft)) => draft,
                (shown, None) => {
                    status.set(shown);
                    busy.set(false);
                    return;
                }
            };
            status.set(Some(("pending", "正在发送……".into(), false)));
            let (tx, rx) = oneshot::channel();
            std::thread::spawn(move || {
                let result = account.reply(
                    &KeyringStore,
                    &original,
                    &original_id,
                    my_name.as_deref(),
                    &draft,
                );
                let _ = tx.send(result);
            });
            let shown = match rx.await {
                Ok(Ok(report)) => match report.status {
                    SendStatus::Sent => Some(("ok", "已发送。".to_string(), false)),
                    SendStatus::SentConfirmedAfterError => Some((
                        "ok",
                        "已发送（连接中断后在“已发送”中核对确认）。".to_string(),
                        false,
                    )),
                    SendStatus::NotSent(e) => Some(("error", format!("未发送：{e}"), true)),
                    SendStatus::MaybeSent(e) => Some((
                        "warn",
                        format!(
                            "可能已发送：{e}。重试前请先在其他设备上查看“已发送”，以免重复发送。"
                        ),
                        true,
                    )),
                },
                Ok(Err(e)) => Some(("error", format!("发送失败：{e}"), true)),
                Err(_) => Some(("error", "后台任务意外结束".to_string(), true)),
            };
            let sent = shown.as_ref().is_some_and(|(kind, _, _)| *kind == "ok");
            status.set(shown);
            busy.set(false);
            if sent {
                on_sent.call(());
            }
        });
    };

    let has_account = props.account.is_some();
    rsx! {
        div { class: "reply-bar",
            label { class: "field",
                span { "收件人" }
                input { value: "{to}", oninput: move |e| to.set(e.value()) }
            }
            label { class: "field",
                span { "抄送" }
                input { value: "{cc}", oninput: move |e| cc.set(e.value()) }
            }
            div { class: "send-row",
                button {
                    class: "primary",
                    disabled: busy() || !has_account,
                    onclick: move |_| send(),
                    if busy() { "发送中…" } else { "发送" }
                }
                if !has_account {
                    span { class: "ai-note", "没有配置账号（--account），只能撰写，不能发送。" }
                }
                if let Some((kind, text, retry)) = status.read().clone() {
                    span { class: "send-status {kind}", "{text}" }
                    if retry && !busy() {
                        button { class: "ghost", onclick: move |_| send(), "重试" }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_recipient_lists() {
        let list = parse_list("Maya Patel <maya@example.com>, alex@example.com").unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].display_name(), "Maya Patel");
        assert_eq!(list[1].address, "alex@example.com");
        assert!(parse_list("not-an-address").is_err());
        assert!(parse_list("  ").unwrap().is_empty());
        let round = format_list(&list);
        assert_eq!(parse_list(&round).unwrap(), list);
    }
}
