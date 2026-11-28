//! Letterdesk 的 AI 层。
//!
//! 流程：[`Assistant::analyze`] 找出来信里需要用户决定的问题和选项；
//! 用户给出决定后，[`Assistant::draft`] 生成回信草稿；
//! 用户再改选项时，[`Assistant::revise`] 以当前草稿（含手改内容）为基础整篇改写，
//! 并返回修改对比，由用户决定是否采用。
//!
//! 模型通过 [`Backend`] 调用。主要实现是 [`cli::CliBackend`]，
//! 它在隔离条件下调用用户本机安装的 Claude Code、Codex 或 Ollama。

pub mod cli;
mod diff;
mod error;
mod model;
mod parse;
mod prompt;

pub use diff::{DiffSpan, diff, render_marked};
pub use error::AiError;
pub use model::{
    Analysis, Answer, Choice, Decision, Draft, DraftBasis, DraftRequest, Question, Revision, Tone,
};
pub use prompt::Prompt;

use mail_core::Email;
use serde_json::Value;

/// 一个可以完成 AI 任务的后端。
pub trait Backend {
    /// 显示给用户的名称。
    fn name(&self) -> &str;

    /// 执行一次任务，返回符合 `prompt.schema` 的 JSON 对象。
    fn complete(&self, prompt: &Prompt) -> Result<Value, AiError>;
}

impl<B: Backend + ?Sized> Backend for &B {
    fn name(&self) -> &str {
        (**self).name()
    }
    fn complete(&self, prompt: &Prompt) -> Result<Value, AiError> {
        (**self).complete(prompt)
    }
}

impl<B: Backend + ?Sized> Backend for Box<B> {
    fn name(&self) -> &str {
        (**self).name()
    }
    fn complete(&self, prompt: &Prompt) -> Result<Value, AiError> {
        (**self).complete(prompt)
    }
}

/// 面向界面的 AI 助手。
pub struct Assistant<B> {
    backend: B,
    /// 界面语言（BCP 47，如 `zh-CN`）。问题和选项用这个语言显示。
    ui_lang: String,
}

impl<B: Backend> Assistant<B> {
    pub fn new(backend: B, ui_lang: impl Into<String>) -> Self {
        Self {
            backend,
            ui_lang: ui_lang.into(),
        }
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// 分析来信：是否需要回复、需要用户决定哪些问题、各有哪些选项。
    pub fn analyze(&self, email: &Email) -> Result<Analysis, AiError> {
        let prompt = prompt::analyze(email, &self.ui_lang);
        let value = self.backend.complete(&prompt)?;
        parse::analysis(value, email)
    }

    /// 按用户的决定生成回信草稿。
    pub fn draft(&self, req: &DraftRequest<'_>) -> Result<Draft, AiError> {
        let prompt = prompt::draft(req, &self.ui_lang, None);
        let value = self.backend.complete(&prompt)?;
        parse::draft(value)
    }

    /// 以当前草稿为基础整篇改写，并给出修改对比。
    ///
    /// `current` 是草稿的当前内容，可能包含用户手改的部分；`basis` 是生成当前草稿时的决定。
    /// 只有与 `basis` 相比改变了的决定会改动草稿，其余内容（含手改）要求原样保留。
    /// 改写结果不会自动采用。
    pub fn revise(
        &self,
        req: &DraftRequest<'_>,
        current: &str,
        basis: &DraftBasis,
    ) -> Result<Revision, AiError> {
        let prompt = prompt::draft(req, &self.ui_lang, Some((current, basis)));
        let value = self.backend.complete(&prompt)?;
        let draft = parse::draft(value)?;
        let changes = diff(current, &draft.body);
        Ok(Revision { draft, changes })
    }
}

#[cfg(test)]
mod tests;
