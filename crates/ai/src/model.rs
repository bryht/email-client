use crate::diff::DiffSpan;
use mail_core::Email;
use serde::{Deserialize, Serialize};

/// 对一封来信的分析结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Analysis {
    /// 是否需要用户回复。
    pub needs_reply: bool,
    /// 判断理由（界面语言）。
    pub reason: String,
    /// 来信使用的语言（BCP 47）。
    pub language: String,
    /// 来信内容摘要（界面语言）。
    pub summary: String,
    /// 需要用户决定的问题。
    pub questions: Vec<Question>,
}

/// 来信中需要用户表态的一个问题。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Question {
    pub id: String,
    /// 问题（界面语言）。
    pub ask: String,
    /// 来信中对应的原句。校验后确认在来信正文中出现；找不到时为 `None`。
    pub quote: Option<String>,
    pub options: Vec<Choice>,
}

/// 一个回复选项。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Choice {
    pub id: String,
    /// 选项文字（界面语言）。
    pub label: String,
}

/// 用户对一个问题的决定。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Decision {
    /// 选了某个选项。
    Choice { choice_id: String },
    /// 自己写的意图（可用中文）。
    Custom { text: String },
    /// 暂时拿不准，回信中说明稍后答复。
    Later,
}

/// 用户对某个问题的回答。没有回答的问题不在回信中回应。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    pub question_id: String,
    pub decision: Decision,
}

/// 回信语气。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    Formal,
    Friendly,
    Brief,
}

impl Tone {
    pub(crate) fn instruction(self) -> &'static str {
        match self {
            Tone::Formal => "formal and polite",
            Tone::Friendly => "warm and friendly",
            Tone::Brief => "short and to the point",
        }
    }
}

/// 生成或改写草稿所需的信息。
#[derive(Debug, Clone)]
pub struct DraftRequest<'a> {
    pub email: &'a Email,
    pub analysis: &'a Analysis,
    pub answers: &'a [Answer],
    /// 用户补充的意图（可用中文）。
    pub note: Option<&'a str>,
    pub tone: Option<Tone>,
    /// 草稿语言；为空时用来信语言。
    pub draft_lang: Option<&'a str>,
    /// 用户自己的名字，用于落款前的称呼判断；签名由客户端另加。
    pub my_name: Option<&'a str>,
}

impl DraftRequest<'_> {
    /// 记录这次请求所用的决定，作为之后改写时的比较基准。
    pub fn basis(&self) -> DraftBasis {
        DraftBasis {
            answers: self.answers.to_vec(),
            note: self.note.map(String::from),
            tone: self.tone,
        }
    }
}

/// 生成当前草稿时所用的决定。改写时与新的决定比较，
/// 只有改变了的决定才允许改动草稿中的对应内容，其余内容（含用户手改）保持原样。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftBasis {
    pub answers: Vec<Answer>,
    pub note: Option<String>,
    pub tone: Option<Tone>,
}

impl DraftBasis {
    pub(crate) fn answer(&self, question_id: &str) -> Option<&Decision> {
        self.answers
            .iter()
            .find(|a| a.question_id == question_id)
            .map(|a| &a.decision)
    }
}

/// 一份回信草稿。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Draft {
    /// 正文，不含主题和签名。
    pub body: String,
    /// 草稿语言（BCP 47）。
    pub language: String,
    /// 草稿翻回界面语言的译文；草稿语言与界面语言相同时为 `None`。
    pub back_translation: Option<String>,
}

/// 一次整篇改写的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct Revision {
    pub draft: Draft,
    /// 当前草稿与改写结果的差异。
    pub changes: Vec<DiffSpan>,
}

impl Revision {
    /// 改写是否带来了任何变化。
    pub fn is_unchanged(&self) -> bool {
        self.changes.iter().all(|s| matches!(s, DiffSpan::Same(_)))
    }
}
