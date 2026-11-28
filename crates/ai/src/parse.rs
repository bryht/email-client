//! 校验模型输出。不符合约定格式的输出直接丢弃。

use crate::error::AiError;
use crate::model::{Analysis, Choice, Draft, Question};
use mail_core::Email;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashSet;

/// 从模型的文本输出中取出 JSON 对象：
/// 支持纯 JSON、```json 代码块，以及前后夹杂说明文字的情况。
pub(crate) fn extract_json(text: &str) -> Result<Value, AiError> {
    let trimmed = text.trim();
    if let Ok(v @ Value::Object(_)) = serde_json::from_str::<Value>(trimmed) {
        return Ok(v);
    }
    let unfenced = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|s| s.strip_suffix("```"))
        .map(str::trim);
    if let Some(inner) = unfenced {
        if let Ok(v @ Value::Object(_)) = serde_json::from_str::<Value>(inner) {
            return Ok(v);
        }
    }
    if let (Some(start), Some(end)) = (trimmed.find('{'), trimmed.rfind('}')) {
        if start < end {
            if let Ok(v @ Value::Object(_)) = serde_json::from_str::<Value>(&trimmed[start..=end]) {
                return Ok(v);
            }
        }
    }
    Err(AiError::InvalidOutput("输出中没有 JSON 对象".into()))
}

#[derive(Deserialize)]
struct RawAnalysis {
    needs_reply: bool,
    reason: String,
    language: String,
    summary: String,
    questions: Vec<RawQuestion>,
}

#[derive(Deserialize)]
struct RawQuestion {
    id: String,
    ask: String,
    #[serde(default)]
    quote: String,
    options: Vec<RawChoice>,
}

#[derive(Deserialize)]
struct RawChoice {
    id: String,
    label: String,
}

fn invalid(e: impl std::fmt::Display) -> AiError {
    AiError::InvalidOutput(e.to_string())
}

/// 把空白压成单个空格，用于核对原句。
fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn analysis(value: Value, email: &Email) -> Result<Analysis, AiError> {
    let raw: RawAnalysis = serde_json::from_value(value).map_err(invalid)?;
    if raw.questions.len() > 6 {
        return Err(invalid("问题超过 6 个"));
    }
    let body = collapse(&email.body);
    let mut ids = HashSet::new();
    let mut questions = Vec::new();
    for q in raw.questions {
        let id = q.id.trim().to_string();
        if id.is_empty() || !ids.insert(id.clone()) {
            return Err(invalid(format!("问题编号为空或重复：{id:?}")));
        }
        if q.ask.trim().is_empty() {
            return Err(invalid(format!("问题 {id} 没有内容")));
        }
        if q.options.is_empty() || q.options.len() > 5 {
            return Err(invalid(format!("问题 {id} 的选项数量不在 1 到 5 之间")));
        }
        let mut option_ids = HashSet::new();
        let mut options = Vec::new();
        for o in q.options {
            let oid = o.id.trim().to_string();
            if oid.is_empty() || !option_ids.insert(oid.clone()) || o.label.trim().is_empty() {
                return Err(invalid(format!("问题 {id} 的选项编号或文字无效")));
            }
            options.push(Choice {
                id: oid,
                label: o.label.trim().to_string(),
            });
        }
        // 原句必须真的出现在来信里，否则不显示，避免“点回原句”指向编造的内容。
        let quote = Some(q.quote.trim().to_string())
            .filter(|s| !s.is_empty())
            .filter(|s| body.contains(&collapse(s)));
        questions.push(Question {
            id,
            ask: q.ask.trim().to_string(),
            quote,
            options,
        });
    }
    // 即使判断为不需要回复，也保留给出的问题，由用户决定是否回复。
    Ok(Analysis {
        needs_reply: raw.needs_reply,
        reason: raw.reason.trim().to_string(),
        language: raw.language.trim().to_string(),
        summary: raw.summary.trim().to_string(),
        questions,
    })
}

#[derive(Deserialize)]
struct RawDraft {
    body: String,
    language: String,
    #[serde(default)]
    back_translation: String,
}

pub(crate) fn draft(value: Value) -> Result<Draft, AiError> {
    let raw: RawDraft = serde_json::from_value(value).map_err(invalid)?;
    let body = raw.body.trim().to_string();
    if body.is_empty() {
        return Err(invalid("草稿正文为空"));
    }
    Ok(Draft {
        body,
        language: raw.language.trim().to_string(),
        back_translation: Some(raw.back_translation.trim().to_string()).filter(|s| !s.is_empty()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_plain_fenced_and_wrapped_json() {
        assert!(extract_json(r#"{"a":1}"#).is_ok());
        assert!(extract_json("```json\n{\"a\":1}\n```").is_ok());
        assert!(extract_json("Here you go:\n{\"a\": {\"b\": 2}}\nThanks").is_ok());
        assert!(extract_json("no json here").is_err());
        assert!(extract_json("[1,2,3]").is_err());
    }
}
