//! HTML 处理：来信正文的隔离显示，以及写信框内容的清洗与转换。
//!
//! 来信显示有三层防护：
//! 1. 用 ammonia 清洗（去掉脚本、事件属性、表单、嵌入框架、`javascript:` 链接等）；
//! 2. 包装成独立文档并注入严格的 CSP（禁止脚本、限制图片来源、禁止表单提交）；
//! 3. 放进不带任何许可的 `sandbox` iframe（不能运行脚本，来源不透明，碰不到应用接口）。

use std::borrow::Cow;
use std::collections::HashSet;

/// 清洗来信 HTML。保留排版所需的表格、内联样式和 `<style>`，去掉一切可执行内容。
pub fn sanitize_email(html: &str) -> String {
    let mut b = ammonia::Builder::default();
    b.rm_clean_content_tags(&["style"])
        .add_tags(&["style", "font", "center", "span", "div"])
        .add_generic_attributes(&[
            "style",
            "align",
            "valign",
            "width",
            "height",
            "bgcolor",
            "border",
            "cellpadding",
            "cellspacing",
            "color",
            "face",
            "size",
            "class",
        ])
        .add_tag_attributes("img", &["src", "alt", "width", "height"])
        .url_schemes(HashSet::from(["http", "https", "mailto", "cid", "data"]))
        .link_rel(Some("noopener noreferrer"))
        .attribute_filter(|element, attribute, value| {
            // data: 只允许用于图片，不允许作为链接。
            if element == "a" && attribute == "href" && value.trim_start().starts_with("data:") {
                return None;
            }
            Some(Cow::Borrowed(value))
        });
    b.clean(html).to_string()
}

/// 把（已清洗的）正文包装成 iframe 的 `srcdoc` 文档，并注入 CSP。
/// `allow_remote_images` 为假时只允许内嵌图片（`cid:`、`data:`）。
pub fn wrap_email_document(body_html: &str, allow_remote_images: bool) -> String {
    let img_src = if allow_remote_images {
        "data: cid: https: http:"
    } else {
        "data: cid:"
    };
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\">\
<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; img-src {img_src}; \
style-src 'unsafe-inline'; font-src data:; form-action 'none'; base-uri 'none'\">\
<base target=\"_blank\">\
<style>body{{font-family:system-ui,-apple-system,'Segoe UI',sans-serif;margin:16px;color:#1f2328;\
line-height:1.5;word-wrap:break-word}}img{{max-width:100%;height:auto}}</style>\
</head><body>{body_html}</body></html>"
    )
}

/// 清洗并包装来信，得到可以放进 sandbox iframe 的 `srcdoc`。
pub fn email_srcdoc(html: &str, allow_remote_images: bool) -> String {
    wrap_email_document(&sanitize_email(html), allow_remote_images)
}

/// 写信框只保留简单格式：段落、换行、粗体、斜体、下划线、列表、引用和链接。
pub fn clean_composer_html(html: &str) -> String {
    let mut b = ammonia::Builder::empty();
    b.add_tags(&[
        "p",
        "div",
        "br",
        "b",
        "strong",
        "i",
        "em",
        "u",
        "ul",
        "ol",
        "li",
        "a",
        "blockquote",
    ])
    .add_clean_content_tags(&["script", "style"])
    .add_tag_attributes("a", &["href"])
    .url_schemes(HashSet::from(["http", "https", "mailto"]))
    .link_rel(Some("noopener noreferrer"));
    b.clean(html).to_string()
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// 纯文本（如 AI 草稿）转为写信框 HTML：空行分段，段内换行用 `<br>`。
pub fn text_to_html(text: &str) -> String {
    let normalized = text.replace("\r\n", "\n");
    normalized
        .split("\n\n")
        .map(|p| p.trim_matches('\n'))
        .filter(|p| !p.trim().is_empty())
        .map(|p| {
            let lines: Vec<String> = p.split('\n').map(escape).collect();
            format!("<p>{}</p>", lines.join("<br>"))
        })
        .collect()
}

fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let Some(semi) = rest.find(';').filter(|&i| i <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..semi];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some(' '),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// 写信框 HTML（已清洗）转为纯文本，作为发信时 `text/plain` 部分。
/// 段落之间空一行，列表项以 `- ` 或 `1. ` 开头，链接文字后附上地址。
pub fn html_to_text(html: &str) -> String {
    let mut out = String::new();
    // 列表栈：None 为无序列表，Some(n) 为有序列表的下一个编号。
    let mut lists: Vec<Option<usize>> = Vec::new();
    let mut link: Option<String> = None;
    let mut rest = html;

    let ensure_blank_line = |out: &mut String| {
        if out.is_empty() {
            return;
        }
        while !out.ends_with("\n\n") {
            out.push('\n');
        }
    };
    let ensure_newline = |out: &mut String| {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
    };

    while !rest.is_empty() {
        if let Some(stripped) = rest.strip_prefix('<') {
            let end = stripped.find('>').map(|i| i + 1).unwrap_or(stripped.len());
            let tag = &stripped[..end.saturating_sub(1)];
            rest = &stripped[end..];
            let closing = tag.starts_with('/');
            let name: String = tag
                .trim_start_matches('/')
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
                .to_ascii_lowercase();
            match (name.as_str(), closing) {
                ("br", _) => out.push('\n'),
                ("p" | "div" | "blockquote", false) => {
                    if lists.is_empty() {
                        ensure_blank_line(&mut out)
                    } else {
                        ensure_newline(&mut out)
                    }
                }
                ("p" | "div" | "blockquote", true) => ensure_newline(&mut out),
                ("ul", false) => {
                    ensure_newline(&mut out);
                    lists.push(None);
                }
                ("ol", false) => {
                    ensure_newline(&mut out);
                    lists.push(Some(1));
                }
                ("ul" | "ol", true) => {
                    lists.pop();
                    ensure_newline(&mut out);
                }
                ("li", false) => {
                    ensure_newline(&mut out);
                    let depth = lists.len().saturating_sub(1);
                    out.push_str(&"  ".repeat(depth));
                    match lists.last_mut() {
                        Some(Some(n)) => {
                            out.push_str(&format!("{n}. "));
                            *n += 1;
                        }
                        _ => out.push_str("- "),
                    }
                }
                ("a", false) => {
                    link = tag
                        .split("href=\"")
                        .nth(1)
                        .and_then(|h| h.split('"').next())
                        .map(decode_entities);
                }
                ("a", true) => {
                    if let Some(href) = link.take() {
                        let shown = href.trim_start_matches("mailto:");
                        if !out.ends_with(shown) {
                            out.push_str(&format!(" <{href}>"));
                        }
                    }
                }
                _ => {}
            }
        } else {
            let end = rest.find('<').unwrap_or(rest.len());
            let text = decode_entities(&rest[..end]).replace(['\n', '\r'], " ");
            out.push_str(&text);
            rest = &rest[end..];
        }
    }
    out.lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MALICIOUS: &str = r##"<p>Hello</p>
<script>parent.__pwned='script'</script>
<img src="x" onerror="parent.__pwned='onerror'">
<svg onload="parent.__pwned='svg'"></svg>
<a href="javascript:parent.__pwned='link'">click</a>
<a href="data:text/html,<script>alert(1)</script>">data link</a>
<meta http-equiv="refresh" content="0;url=https://attacker.example/">
<form action="https://attacker.example/"><input name="p"></form>
<iframe src="https://attacker.example/"></iframe>
<object data="https://attacker.example/x.swf"></object>
<table bgcolor="#eee"><tr><td style="color:red">cell</td></tr></table>
<img src="https://tracker.example/pixel.gif" alt="pixel">"##;

    #[test]
    fn email_sanitizer_removes_executable_content() {
        let clean = sanitize_email(MALICIOUS);
        for bad in [
            "<script",
            "onerror",
            "onload",
            "javascript:",
            "data:text/html",
            "<meta",
            "<form",
            "<input",
            "<iframe",
            "<object",
            "__pwned",
        ] {
            assert!(!clean.contains(bad), "仍包含 {bad}: {clean}");
        }
        // 排版和普通图片保留。
        assert!(clean.contains("<table"));
        assert!(clean.contains("style=\"color:red\""));
        assert!(clean.contains("https://tracker.example/pixel.gif"));
        assert!(clean.contains("rel=\"noopener noreferrer\""));
    }

    #[test]
    fn email_document_has_csp_and_remote_image_switch() {
        let on = email_srcdoc("<p>x</p>", true);
        assert!(on.contains("Content-Security-Policy"));
        assert!(on.contains("default-src 'none'"));
        assert!(on.contains("img-src data: cid: https: http:"));
        let off = email_srcdoc("<p>x</p>", false);
        assert!(off.contains("img-src data: cid:;"));
        assert!(!off.contains("https: http:"));
    }

    #[test]
    fn composer_keeps_simple_formatting_only() {
        let dirty = r#"<p><b>Hi</b> <span style="color:red" onclick="x()">there</span></p><script>bad()</script><style>p{}</style><p><a href="javascript:x()">bad</a> <a href="https://example.com/a?b=1&amp;c=2">ok</a></p><ul><li>one</li></ul>"#;
        let clean = clean_composer_html(dirty);
        assert_eq!(
            clean,
            "<p><b>Hi</b> there</p><p><a rel=\"noopener noreferrer\">bad</a> <a href=\"https://example.com/a?b=1&amp;c=2\" rel=\"noopener noreferrer\">ok</a></p><ul><li>one</li></ul>"
        );
    }

    #[test]
    fn text_and_html_round_trip() {
        let text = "Hi Maya,\n\nThursday works.\nSee you then.\n\nBest,";
        let html = text_to_html(text);
        assert_eq!(
            html,
            "<p>Hi Maya,</p><p>Thursday works.<br>See you then.</p><p>Best,</p>"
        );
        assert_eq!(html_to_text(&html), text);
        assert_eq!(text_to_html("a < b & c"), "<p>a &lt; b &amp; c</p>");
    }

    #[test]
    fn html_to_text_handles_lists_links_and_entities() {
        let html = "<p>Points:</p><ol><li>first</li><li>second &amp; more</li></ol><ul><li>dot</li></ul><p>See <a href=\"https://example.com\">site</a> or <a href=\"mailto:a@b.com\">a@b.com</a>.</p><div>Chrome&nbsp;line<br>next</div>";
        assert_eq!(
            html_to_text(html),
            "Points:\n1. first\n2. second & more\n- dot\n\nSee site <https://example.com> or a@b.com.\n\nChrome line\nnext"
        );
    }
}
