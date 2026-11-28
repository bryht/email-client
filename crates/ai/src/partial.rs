//! 从尚未生成完的 JSON 文本中取出某个字符串字段的已生成部分，用于流式显示草稿。

/// 在不完整的 JSON 对象文本中查找顶层字符串字段 `field`，返回已经生成的部分（已解码转义）。
/// 字段尚未出现时返回 `None`。
pub fn partial_string_field(json: &str, field: &str) -> Option<String> {
    let key = format!("\"{field}\"");
    let mut search_from = 0;
    let start = loop {
        let pos = search_from + json[search_from..].find(&key)?;
        let rest = json[pos + key.len()..].trim_start();
        if let Some(after_colon) = rest.strip_prefix(':') {
            let value = after_colon.trim_start();
            if let Some(body) = value.strip_prefix('"') {
                break json.len() - body.len();
            }
        }
        search_from = pos + key.len();
    };

    let mut out = String::new();
    let mut chars = json[start..].chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => {
                let Some(e) = chars.next() else {
                    return Some(out);
                };
                match e {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    'b' => out.push('\u{8}'),
                    'f' => out.push('\u{c}'),
                    'u' => {
                        let hex: String = chars.by_ref().take(4).collect();
                        if hex.len() < 4 {
                            return Some(out);
                        }
                        let Ok(code) = u32::from_str_radix(&hex, 16) else {
                            return Some(out);
                        };
                        if (0xD800..0xDC00).contains(&code) {
                            // 高位代理，需要与下一个 \uXXXX 组合。
                            let next: String = chars.by_ref().take(6).collect();
                            let low = next
                                .strip_prefix("\\u")
                                .and_then(|h| u32::from_str_radix(h, 16).ok());
                            match low {
                                Some(low) if next.len() == 6 => {
                                    let combined =
                                        0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                                    if let Some(ch) = char::from_u32(combined) {
                                        out.push(ch);
                                    }
                                }
                                _ => return Some(out),
                            }
                        } else if let Some(ch) = char::from_u32(code) {
                            out.push(ch);
                        }
                    }
                    other => out.push(other),
                }
            }
            c => out.push(c),
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_growing_body() {
        assert_eq!(partial_string_field(r#"{"bo"#, "body"), None);
        assert_eq!(partial_string_field(r#"{"body": "#, "body"), None);
        assert_eq!(
            partial_string_field(r#"{"body": ""#, "body"),
            Some(String::new())
        );
        assert_eq!(
            partial_string_field(r#"{"body": "Hi Maya,\nThurs"#, "body"),
            Some("Hi Maya,\nThurs".into())
        );
        assert_eq!(
            partial_string_field(r#"{"body":"Say \"hi\"","language":"en"}"#, "body"),
            Some("Say \"hi\"".into())
        );
    }

    #[test]
    fn stops_before_incomplete_escape() {
        assert_eq!(
            partial_string_field(r#"{"body": "a\"#, "body"),
            Some("a".into())
        );
        assert_eq!(
            partial_string_field(r#"{"body": "a\u00"#, "body"),
            Some("a".into())
        );
        assert_eq!(
            partial_string_field(r#"{"body": "你好"#, "body"),
            Some("你好".into())
        );
        assert_eq!(
            partial_string_field(r#"{"body": "😀!"#, "body"),
            Some("😀!".into())
        );
    }

    #[test]
    fn ignores_key_text_inside_other_values() {
        let json = r#"{"note": "the \"body\" word", "body": "real"#;
        assert_eq!(partial_string_field(json, "body"), Some("real".into()));
    }
}
