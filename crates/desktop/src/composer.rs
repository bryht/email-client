//! 写信框：浏览器的 contenteditable 负责输入、光标和输入法；
//! 格式命令、内容清洗、纯文本转换和草稿填入由 Rust 负责，不使用 JS 编辑器库。

use crate::html::{clean_composer_html, html_to_text, text_to_html};
use dioxus::prelude::*;

pub const EDITOR_ID: &str = "ld-composer";

/// 把 Rust 字符串变成安全的 JS 字符串字面量。
pub fn js_string(s: &str) -> String {
    serde_json::to_string(s).expect("string serializes")
}

/// 读取编辑区当前 HTML（未清洗）。
pub async fn read_editor_html() -> Option<String> {
    let js = format!(
        "const el = document.getElementById({id}); return el ? el.innerHTML : '';",
        id = js_string(EDITOR_ID)
    );
    document::eval(&js)
        .await
        .ok()
        .and_then(|v| v.as_str().map(String::from))
}

/// 用（已清洗的）HTML 替换编辑区内容，并把光标放到末尾。
pub fn write_editor_html(html: &str) {
    let js = format!(
        "const el = document.getElementById({id});
         if (el) {{
           el.innerHTML = {html};
           el.focus();
           const r = document.createRange(); r.selectNodeContents(el); r.collapse(false);
           const s = window.getSelection(); s.removeAllRanges(); s.addRange(r);
         }}",
        id = js_string(EDITOR_ID),
        html = js_string(html)
    );
    document::eval(&js);
}

/// 格式命令。由浏览器在当前选区上执行。
#[derive(Clone, Copy, PartialEq)]
pub enum Format {
    Bold,
    Italic,
    Underline,
    BulletList,
    NumberedList,
    Link,
    Clear,
}

impl Format {
    fn command(self) -> &'static str {
        match self {
            Format::Bold => "bold",
            Format::Italic => "italic",
            Format::Underline => "underline",
            Format::BulletList => "insertUnorderedList",
            Format::NumberedList => "insertOrderedList",
            Format::Link => "createLink",
            Format::Clear => "removeFormat",
        }
    }
}

fn apply_format(format: Format, value: Option<&str>) {
    let js = format!(
        "document.getElementById({id})?.focus(); document.execCommand({cmd}, false, {val});",
        id = js_string(EDITOR_ID),
        cmd = js_string(format.command()),
        val = value.map(js_string).unwrap_or_else(|| "null".into())
    );
    document::eval(&js);
}

#[derive(Props, Clone, PartialEq)]
pub struct ComposerProps {
    /// 填入编辑区的 AI 草稿（纯文本）。
    pub draft: String,
}

#[component]
pub fn Composer(props: ComposerProps) -> Element {
    // Rust 一侧保存的内容：始终是清洗后的 HTML。
    let mut content = use_signal(String::new);
    let mut link_url = use_signal(|| String::from("https://"));

    let sync = move || {
        spawn(async move {
            if let Some(raw) = read_editor_html().await {
                content.set(clean_composer_html(&raw));
            }
        });
    };

    let draft = props.draft.clone();
    let fill_draft = move |_| {
        let html = text_to_html(&draft);
        write_editor_html(&html);
        content.set(html);
    };

    let format_button = move |label: &'static str, title: &'static str, format: Format| {
        rsx! {
            button {
                class: "tool",
                title: "{title}",
                // 按下时阻止默认行为，避免编辑区失去选区。
                onmousedown: move |e| e.prevent_default(),
                onclick: move |_| {
                    apply_format(format, None);
                    sync();
                },
                "{label}"
            }
        }
    };

    let plain = html_to_text(&content.read());

    rsx! {
        div { class: "composer-pane",
            div { class: "toolbar",
                {format_button("B", "粗体", Format::Bold)}
                {format_button("I", "斜体", Format::Italic)}
                {format_button("U", "下划线", Format::Underline)}
                {format_button("•", "项目符号列表", Format::BulletList)}
                {format_button("1.", "编号列表", Format::NumberedList)}
                {format_button("⌫", "清除格式", Format::Clear)}
                input {
                    class: "link-input",
                    value: "{link_url}",
                    oninput: move |e| link_url.set(e.value()),
                }
                button {
                    class: "tool",
                    title: "把选中文字设为链接",
                    onmousedown: move |e| e.prevent_default(),
                    onclick: move |_| {
                        let url = link_url.read().clone();
                        if url.starts_with("https://") || url.starts_with("http://") || url.starts_with("mailto:") {
                            apply_format(Format::Link, Some(&url));
                            sync();
                        }
                    },
                    "链接"
                }
                span { class: "spacer" }
                button { class: "primary", onclick: fill_draft, "填入 AI 草稿" }
            }
            div {
                id: EDITOR_ID,
                class: "editor",
                contenteditable: "true",
                spellcheck: "true",
                oninput: move |_| sync(),
                onpaste: move |_| {
                    // 粘贴的内容可能带样式、脚本或事件属性：等浏览器插入后读回，清洗，再写回。
                    spawn(async move {
                        let wait = document::eval("await new Promise(r => setTimeout(r, 0)); return true;");
                        let _ = wait.await;
                        if let Some(raw) = read_editor_html().await {
                            let clean = clean_composer_html(&raw);
                            if clean != raw {
                                write_editor_html(&clean);
                            }
                            content.set(clean);
                        }
                    });
                },
            }
            details { class: "plain",
                summary { "纯文本部分（发信时作为 text/plain，共 {plain.chars().count()} 字）" }
                pre { "{plain}" }
            }
        }
    }
}
