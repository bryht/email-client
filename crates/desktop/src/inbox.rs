//! 按人浏览：左栏对话列表与中间的对话时间线（PLAN 第 2 节）。

use crate::html;
use conversation::{Conversation, base_subject};
use dioxus::prelude::*;
use mail_core::{Email, Mailbox};
use std::collections::HashSet;

fn is_me(m: &Mailbox, me: &[String]) -> bool {
    me.iter().any(|a| a.eq_ignore_ascii_case(m.address.trim()))
}

/// 对话中要回复的那封：最近一封别人发来的邮件；都是自己发的则取最近一封。
pub fn active_message(conv: &Conversation, emails: &[Email], me: &[String]) -> usize {
    conv.messages
        .iter()
        .rev()
        .copied()
        .find(|&i| !is_me(&emails[i].from, me))
        .or(conv.messages.last().copied())
        .unwrap_or(0)
}

fn initials(m: &Mailbox) -> String {
    m.display_name()
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_else(|| "?".into())
}

/// 按地址给头像一个稳定的颜色。
fn avatar_color(m: &Mailbox) -> &'static str {
    const COLORS: [&str; 6] = [
        "#1f5b3f", "#7a4b9c", "#a8571d", "#2f5f8f", "#8f2f4f", "#4f6f2f",
    ];
    let sum: usize = m.normalized().bytes().map(usize::from).sum();
    COLORS[sum % COLORS.len()]
}

fn date_of(e: &Email) -> String {
    e.date
        .as_deref()
        .map(|d| d.chars().take(10).collect())
        .unwrap_or_default()
}

#[derive(Props, Clone, PartialEq)]
pub struct ConversationListProps {
    pub conversations: Vec<Conversation>,
    pub emails: Vec<Email>,
    pub me: Vec<String>,
    pub selected: usize,
    pub on_select: EventHandler<usize>,
}

#[component]
pub fn ConversationList(props: ConversationListProps) -> Element {
    rsx! {
        section { class: "conv-list",
            div { class: "conv-list-title", "PEOPLE" }
            for (i, c) in props.conversations.iter().enumerate() {
                {
                    let latest = &props.emails[*c.messages.last().unwrap_or(&0)];
                    let names = if c.participants.is_empty() {
                        "我自己".to_string()
                    } else {
                        c.participants.iter().map(|m| m.display_name().to_string()).collect::<Vec<_>>().join("、")
                    };
                    let cc: Vec<String> = latest
                        .cc
                        .iter()
                        .filter(|m| !is_me(m, &props.me))
                        .map(|m| m.display_name().to_string())
                        .collect();
                    let second = if cc.is_empty() {
                        base_subject(&latest.subject)
                    } else {
                        format!("Cc {}", cc.join("、"))
                    };
                    let on_select = props.on_select;
                    let class = if i == props.selected { "conv-row selected" } else { "conv-row" };
                    rsx! {
                        div {
                            key: "{c.id}",
                            class: "{class}",
                            onclick: move |_| on_select.call(i),
                            div { class: "avatars",
                                for m in c.participants.iter().take(2) {
                                    span {
                                        class: "avatar",
                                        style: "background:{avatar_color(m)}",
                                        "{initials(m)}"
                                    }
                                }
                            }
                            div { class: "conv-text",
                                div { class: "conv-names",
                                    "{names}"
                                    if c.forwarded_from.is_some() {
                                        span { class: "tag", "转发" }
                                    }
                                }
                                div { class: "conv-second", "{second}" }
                            }
                            div { class: "conv-date", "{date_of(latest)}" }
                        }
                    }
                }
            }
        }
    }
}

#[derive(Props, Clone, PartialEq)]
pub struct TimelineProps {
    pub conversation: Conversation,
    pub emails: Vec<Email>,
    /// 当前要回复的那封邮件，默认展开显示原文。
    pub active: usize,
    pub remote_images: Signal<bool>,
}

fn list(v: &[Mailbox]) -> String {
    v.iter()
        .map(|m| m.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

#[component]
pub fn Timeline(props: TimelineProps) -> Element {
    let mut remote_images = props.remote_images;
    let mut expanded = use_signal(HashSet::<usize>::new);
    let conv = &props.conversation;
    let title = if conv.participants.is_empty() {
        "我自己".to_string()
    } else {
        conv.participants
            .iter()
            .map(|m| m.display_name().to_string())
            .collect::<Vec<_>>()
            .join("、")
    };
    let thread_of = |i: usize| conv.threads.iter().position(|t| t.messages.contains(&i));

    rsx! {
        header { class: "mail-header",
            h1 { "{title}" }
            div { class: "meta",
                "{conv.messages.len()} 封邮件 · {conv.threads.len()} 个话题"
            }
            label { class: "remote-toggle",
                input {
                    r#type: "checkbox",
                    checked: remote_images(),
                    onchange: move |e| remote_images.set(e.checked()),
                }
                " 自动加载外部图片（加载可能向发件人透露阅读时间）"
            }
        }
        div { class: "timeline",
            for (n, &i) in conv.messages.iter().enumerate() {
                {
                    let e = &props.emails[i];
                    let thread = thread_of(i);
                    let new_topic = n == 0 || thread_of(conv.messages[n - 1]) != thread;
                    let subject = thread.map(|t| conv.threads[t].subject.clone()).unwrap_or_default();
                    let is_active = i == props.active;
                    let show_html = is_active || expanded.read().contains(&i);
                    let preview: String = e.new_content().chars().take(600).collect();
                    let srcdoc = html::email_srcdoc(&e.html, remote_images());
                    rsx! {
                        div { key: "{i}",
                            if new_topic {
                                div { class: "topic", "话题：{subject}" }
                            }
                            div { class: if is_active { "msg active" } else { "msg" },
                                div { class: "msg-head",
                                    div {
                                        div { class: "meta", b { "From: " } "{e.from}" }
                                        if !e.to.is_empty() { div { class: "meta", b { "To: " } "{list(&e.to)}" } }
                                        if !e.cc.is_empty() { div { class: "meta", b { "Cc: " } "{list(&e.cc)}" } }
                                    }
                                    div { class: "conv-date", "{date_of(e)}" }
                                }
                                if show_html {
                                    iframe {
                                        id: if is_active { "ld-email-frame" } else { "" },
                                        class: "msg-frame",
                                        // 不给任何许可：不能运行脚本、不能访问父页面、不能弹窗或提交表单。
                                        "sandbox": "",
                                        srcdoc: "{srcdoc}",
                                    }
                                } else {
                                    pre { class: "msg-preview", "{preview}" }
                                    button {
                                        class: "ghost small",
                                        onclick: move |_| { expanded.write().insert(i); },
                                        "查看原文"
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mail(from: &str, ts: &str) -> Email {
        let raw =
            format!("From: {from}\r\nTo: lin@example.com\r\nSubject: s\r\nDate: {ts}\r\n\r\nx");
        Email::parse(raw.as_bytes()).unwrap()
    }

    #[test]
    fn active_message_is_latest_from_someone_else() {
        let emails = vec![
            mail("maya@example.com", "01 Mar 2026 09:00:00 +0000"),
            mail("lin@example.com", "02 Mar 2026 09:00:00 +0000"),
        ];
        let convs = conversation::group(&emails, &["lin@example.com".into()]);
        assert_eq!(
            active_message(&convs[0], &emails, &["LIN@example.com".into()]),
            0
        );
        // 不知道自己的地址时取最近一封。
        assert_eq!(active_message(&convs[0], &emails, &[]), 1);
    }

    #[test]
    fn initials_and_colors_are_stable() {
        let m = Mailbox::new(Some("maya patel"), "maya@example.com");
        assert_eq!(initials(&m), "M");
        assert_eq!(avatar_color(&m), avatar_color(&m.clone()));
        assert_eq!(initials(&Mailbox::new(Some("王磊"), "w@example.cn")), "王");
    }
}
