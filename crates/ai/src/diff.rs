use similar::{ChangeTag, TextDiff};

/// 修改对比中的一段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffSpan {
    Same(String),
    Removed(String),
    Added(String),
}

/// 按词比较两段文字（中日韩文字按字切分）。
///
/// 两处修改之间夹着的很短的相同片段（如空格、"the"）会并入修改，
/// 让整句改写显示为“整段删除 + 整段新增”，而不是逐词交错。
pub fn diff(old: &str, new: &str) -> Vec<DiffSpan> {
    cleanup(raw_diff(old, new))
}

/// 被并入修改的相同片段的最大长度（去掉空白后的字符数）。
const SMALL_SAME: usize = 4;

fn is_small(s: &str) -> bool {
    !s.contains('\n') && s.trim().chars().count() <= SMALL_SAME
}

enum Seg {
    Same(String),
    Change(String, String),
}

fn cleanup(spans: Vec<DiffSpan>) -> Vec<DiffSpan> {
    let mut segs: Vec<Seg> = Vec::new();
    for span in spans {
        match (span, segs.last_mut()) {
            (DiffSpan::Same(s), _) => segs.push(Seg::Same(s)),
            (DiffSpan::Removed(s), Some(Seg::Change(r, _))) => r.push_str(&s),
            (DiffSpan::Added(s), Some(Seg::Change(_, a))) => a.push_str(&s),
            (DiffSpan::Removed(s), _) => segs.push(Seg::Change(s, String::new())),
            (DiffSpan::Added(s), _) => segs.push(Seg::Change(String::new(), s)),
        }
    }
    let mut i = 1;
    while i + 1 < segs.len() {
        let mergeable = matches!(
            (&segs[i - 1], &segs[i], &segs[i + 1]),
            (Seg::Change(..), Seg::Same(s), Seg::Change(..)) if is_small(s)
        );
        if mergeable {
            let Seg::Change(r2, a2) = segs.remove(i + 1) else {
                unreachable!()
            };
            let Seg::Same(s) = segs.remove(i) else {
                unreachable!()
            };
            if let Seg::Change(r, a) = &mut segs[i - 1] {
                r.push_str(&s);
                r.push_str(&r2);
                a.push_str(&s);
                a.push_str(&a2);
            }
        } else {
            i += 1;
        }
    }
    let mut out = Vec::new();
    for seg in segs {
        match seg {
            Seg::Same(s) => out.push(DiffSpan::Same(s)),
            Seg::Change(r, a) => {
                if !r.is_empty() {
                    out.push(DiffSpan::Removed(r));
                }
                if !a.is_empty() {
                    out.push(DiffSpan::Added(a));
                }
            }
        }
    }
    out
}

fn raw_diff(old: &str, new: &str) -> Vec<DiffSpan> {
    let diff = TextDiff::from_unicode_words(old, new);
    let mut spans: Vec<DiffSpan> = Vec::new();
    for change in diff.iter_all_changes() {
        let text = change.value();
        match (change.tag(), spans.last_mut()) {
            (ChangeTag::Equal, Some(DiffSpan::Same(s)))
            | (ChangeTag::Delete, Some(DiffSpan::Removed(s)))
            | (ChangeTag::Insert, Some(DiffSpan::Added(s))) => s.push_str(text),
            (ChangeTag::Equal, _) => spans.push(DiffSpan::Same(text.to_string())),
            (ChangeTag::Delete, _) => spans.push(DiffSpan::Removed(text.to_string())),
            (ChangeTag::Insert, _) => spans.push(DiffSpan::Added(text.to_string())),
        }
    }
    spans
}

/// 用 `[-删除-]` 和 `{+新增+}` 标记渲染修改对比，便于在纯文本中查看。
pub fn render_marked(spans: &[DiffSpan]) -> String {
    let mut out = String::new();
    for span in spans {
        match span {
            DiffSpan::Same(s) => out.push_str(s),
            DiffSpan::Removed(s) => {
                out.push_str("[-");
                out.push_str(s);
                out.push_str("-]");
            }
            DiffSpan::Added(s) => {
                out.push_str("{+");
                out.push_str(s);
                out.push_str("+}");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_word_change() {
        let spans = diff("We can offer 3% off.", "We can offer 10% off.");
        assert_eq!(render_marked(&spans), "We can offer [-3-]{+10+}% off.");
    }

    #[test]
    fn rewritten_sentence_is_shown_as_one_block() {
        let old = "Hi Maya,\nThursday doesn't work for me. Could we do next Tuesday?\nThanks,";
        let new = "Hi Maya,\nThursday at 3 pm works for me.\nThanks,";
        let rendered = render_marked(&diff(old, new));
        assert_eq!(
            rendered,
            "Hi Maya,\nThursday [-doesn't work-]{+at 3 pm works+} for me.[- Could we do next Tuesday?-]\nThanks,"
        );
    }

    #[test]
    fn identical_text_has_only_same_spans() {
        let spans = diff("same text", "same text");
        assert_eq!(spans, vec![DiffSpan::Same("same text".into())]);
    }

    #[test]
    fn handles_chinese() {
        let spans = diff("最多优惠百分之三", "最多优惠百分之五");
        let rendered = render_marked(&spans);
        assert!(rendered.starts_with("最多优惠百分之"), "{rendered}");
        assert!(rendered.contains("[-三-]") && rendered.contains("{+五+}"));
    }
}
