/// 去掉回复邮件里引用的历史内容，只保留这封邮件新写的部分。
///
/// 规则（第一版，后续再补充）：
/// - 以 `>` 开头的行是引用；
/// - 遇到常见的引用开头（如 `On ... wrote:`、`-----Original Message-----`、
///   `在 ... 写道：`）时，其后的内容都视为历史邮件。
pub fn strip_quoted(text: &str) -> String {
    let mut kept: Vec<&str> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if is_quote_header(trimmed) {
            break;
        }
        if trimmed.starts_with('>') {
            continue;
        }
        kept.push(line.trim_end());
    }
    while kept.last().is_some_and(|l| l.trim().is_empty()) {
        kept.pop();
    }
    while kept.first().is_some_and(|l| l.trim().is_empty()) {
        kept.remove(0);
    }
    kept.join("\n")
}

fn is_quote_header(line: &str) -> bool {
    let lower = line.to_lowercase();
    (lower.starts_with("on ") && lower.ends_with("wrote:"))
        || lower.starts_with("-----original message-----")
        || lower.starts_with("---------- forwarded message")
        || (line.starts_with('在') && (line.ends_with("写道：") || line.ends_with("写道:")))
        || (lower.starts_with("le ") && lower.ends_with("a écrit :"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_gmail_style_quote() {
        let text = "Sounds good, see you then.\n\nOn Mon, 3 Mar 2026 at 09:00, Lin <lin@example.com> wrote:\n> Could we meet?\n";
        assert_eq!(strip_quoted(text), "Sounds good, see you then.");
    }

    #[test]
    fn strips_inline_quote_lines() {
        let text = "> earlier\nNew line\n> more\nSecond";
        assert_eq!(strip_quoted(text), "New line\nSecond");
    }

    #[test]
    fn strips_outlook_and_chinese_headers() {
        assert_eq!(strip_quoted("Hi\n-----Original Message-----\nold"), "Hi");
        assert_eq!(strip_quoted("好的\n在 2026年3月3日 写道：\n旧内容"), "好的");
    }

    #[test]
    fn keeps_plain_text() {
        assert_eq!(strip_quoted("\n\nHello\nWorld\n\n"), "Hello\nWorld");
    }
}
