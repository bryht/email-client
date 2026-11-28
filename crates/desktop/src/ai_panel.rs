//! “回复建议”面板：分析来信、点选方向、流式生成草稿到写信框；
//! 已有草稿时改选项，显示修改对比，由用户采用或放弃。
//!
//! 模型调用是阻塞的，放在后台线程执行，结果经通道回到界面。

use crate::composer::{EDITOR_ID, js_string, write_editor_html};
use crate::html::{clean_composer_html, html_to_text, merge_formatting, text_to_html};
use ai::cli::{CliBackend, CliConfig, CliKind, Login};
use ai::{
    AiError, Analysis, Answer, Assistant, Decision, DiffSpan, Draft, DraftBasis, DraftRequest,
    Revision, Tone,
};
use dioxus::prelude::*;
use futures_channel::{mpsc, oneshot};
use futures_util::StreamExt;
use mail_core::Email;

/// AI 连接设置（来自命令行参数）。
#[derive(Debug, Clone, PartialEq)]
pub struct AiSettings {
    pub kind: CliKind,
    pub model: Option<String>,
    pub login: Login,
    pub allow_unverified: bool,
    pub ui_lang: String,
    pub my_name: Option<String>,
    /// 自动演示：分析后为每个问题选第一个选项并生成草稿（用于截图和演示）。
    pub autopilot: bool,
}

impl AiSettings {
    fn assistant(&self) -> Result<Assistant<CliBackend>, AiError> {
        let mut config = CliConfig::new(self.kind);
        config.model = self.model.clone();
        config.login = self.login;
        config.allow_unverified = self.allow_unverified;
        Ok(Assistant::new(
            CliBackend::new(config)?,
            self.ui_lang.clone(),
        ))
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Phase {
    Idle,
    Analyzing,
    Ready,
    Failed(String),
}

enum Msg {
    Body(String),
    Drafted(Result<(Draft, DraftBasis), String>),
    Revised(Result<(Revision, DraftBasis), String>),
}

/// 当前选择的快照，在后台线程里组装 `DraftRequest`。
#[derive(Clone)]
struct Job {
    email: Email,
    analysis: Analysis,
    answers: Vec<Answer>,
    note: String,
    tone: Option<Tone>,
    settings: AiSettings,
}

impl Job {
    fn request(&self) -> DraftRequest<'_> {
        DraftRequest {
            email: &self.email,
            analysis: &self.analysis,
            answers: &self.answers,
            note: Some(self.note.as_str()).filter(|n| !n.trim().is_empty()),
            tone: self.tone,
            draft_lang: None,
            my_name: self.settings.my_name.as_deref(),
        }
    }
}

fn set_answer(answers: &mut Vec<Answer>, question_id: &str, decision: Option<Decision>) {
    answers.retain(|a| a.question_id != question_id);
    if let Some(decision) = decision {
        answers.push(Answer {
            question_id: question_id.to_string(),
            decision,
        });
    }
}

#[derive(Props, Clone, PartialEq)]
pub struct AiPanelProps {
    pub email: Email,
    pub settings: AiSettings,
    /// 写信框内容（清洗后的 HTML），与写信框共用。
    pub content: Signal<String>,
}

#[component]
pub fn AiPanel(props: AiPanelProps) -> Element {
    let email = use_signal(|| props.email.clone());
    let settings = use_signal(|| props.settings.clone());
    let mut content = props.content;

    let mut phase = use_signal(|| Phase::Idle);
    let mut analysis = use_signal(|| None::<Analysis>);
    let mut answers = use_signal(Vec::<Answer>::new);
    let mut note = use_signal(String::new);
    let mut tone = use_signal(|| None::<Tone>);
    let mut busy = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);
    let mut basis = use_signal(|| None::<DraftBasis>);
    let mut back_translation = use_signal(|| None::<String>);
    let mut pending = use_signal(|| None::<(Revision, DraftBasis)>);

    let mut analyze = move || {
        phase.set(Phase::Analyzing);
        error.set(None);
        let email = email.read().clone();
        let settings = settings.read().clone();
        spawn(async move {
            let (tx, rx) = oneshot::channel();
            std::thread::spawn(move || {
                let result = settings
                    .assistant()
                    .and_then(|a| a.analyze(&email))
                    .map_err(|e| e.to_string());
                let _ = tx.send(result);
            });
            match rx.await {
                Ok(Ok(a)) => {
                    analysis.set(Some(a));
                    answers.set(Vec::new());
                    basis.set(None);
                    pending.set(None);
                    phase.set(Phase::Ready);
                }
                Ok(Err(e)) => phase.set(Phase::Failed(e)),
                Err(_) => phase.set(Phase::Failed("后台任务意外结束".into())),
            }
        });
    };

    let mut generate = move || {
        let Some(analysis_now) = analysis.read().clone() else {
            return;
        };
        let job = Job {
            email: email.read().clone(),
            analysis: analysis_now,
            answers: answers.read().clone(),
            note: note.read().clone(),
            tone: tone(),
            settings: settings.read().clone(),
        };
        let base = basis.read().clone();
        let current = html_to_text(&content.read());
        busy.set(true);
        error.set(None);
        pending.set(None);

        let (tx, mut rx) = mpsc::unbounded::<Msg>();
        std::thread::spawn(move || {
            let req = job.request();
            let assistant = match job.settings.assistant() {
                Ok(a) => a,
                Err(e) => {
                    let _ = tx.unbounded_send(Msg::Drafted(Err(e.to_string())));
                    return;
                }
            };
            match base {
                // 还没有草稿：流式生成，正文边生成边写进写信框。
                None => {
                    let body_tx = tx.clone();
                    let result = assistant.draft_streaming(&req, &mut |body| {
                        let _ = body_tx.unbounded_send(Msg::Body(body.to_string()));
                    });
                    let msg = result.map(|d| (d, req.basis())).map_err(|e| e.to_string());
                    let _ = tx.unbounded_send(Msg::Drafted(msg));
                }
                // 已有草稿（可能含手改）：整篇改写，只改动变化了的决定，再由用户确认。
                Some(base) => {
                    let result = assistant.revise(&req, &current, &base);
                    let msg = result.map(|r| (r, req.basis())).map_err(|e| e.to_string());
                    let _ = tx.unbounded_send(Msg::Revised(msg));
                }
            }
        });

        spawn(async move {
            while let Some(msg) = rx.next().await {
                match msg {
                    Msg::Body(body) => write_editor_html(&text_to_html(&body)),
                    Msg::Drafted(Ok((draft, new_basis))) => {
                        let html = text_to_html(&draft.body);
                        write_editor_html(&html);
                        content.set(html);
                        back_translation.set(draft.back_translation);
                        basis.set(Some(new_basis));
                    }
                    Msg::Revised(Ok(rev)) => {
                        if rev.0.is_unchanged() {
                            error.set(Some("改写结果与当前草稿相同。".into()));
                        } else {
                            pending.set(Some(rev));
                        }
                    }
                    Msg::Drafted(Err(e)) | Msg::Revised(Err(e)) => error.set(Some(e)),
                }
            }
            busy.set(false);
        });
    };

    let mut accept = move || {
        let Some((rev, new_basis)) = pending.take() else {
            return;
        };
        // 没变的段落沿用原来的格式，只重建改动的段落。
        let html = merge_formatting(&content.read(), &rev.draft.body);
        write_editor_html(&html);
        content.set(html);
        back_translation.set(rev.draft.back_translation);
        basis.set(Some(new_basis));
    };

    // 自动演示（--autopilot）：分析 → 每题选第一个选项 → 生成草稿 → 模拟手改（第一段加粗）
    // → 改选第一个问题 → 改写 → 采用，并把结果以 JSON 打印到标准输出，便于在无界面环境中检查。
    if props.settings.autopilot {
        use_future(move || async move {
            analyze();
            while phase() == Phase::Analyzing {
                pause(300).await;
            }
            let Some(a) = analysis.read().clone() else {
                return;
            };
            let first_choice = |q: &ai::Question, i: usize| {
                q.options.get(i).map(|o| Decision::Choice {
                    choice_id: o.id.clone(),
                })
            };
            let mut picks = Vec::new();
            for q in &a.questions {
                set_answer(&mut picks, &q.id, first_choice(q, 0));
            }
            answers.set(picks);
            generate();
            pause(500).await;
            while busy() {
                pause(300).await;
            }

            let js = format!(
                "const el = document.getElementById({id}); const p = el.querySelector('p');
                 if (p) {{ const r = document.createRange(); r.selectNodeContents(p);
                   const s = window.getSelection(); s.removeAllRanges(); s.addRange(r);
                   document.execCommand('bold'); }}
                 return el.innerHTML;",
                id = js_string(EDITOR_ID)
            );
            if let Ok(serde_json::Value::String(raw)) = document::eval(&js).await {
                content.set(clean_composer_html(&raw));
            }
            let before = content.read().clone();

            let Some(q) = a.questions.first() else {
                return;
            };
            set_answer(&mut answers.write(), &q.id, first_choice(q, 1));
            generate();
            pause(500).await;
            while busy() {
                pause(300).await;
            }
            let revision = pending
                .read()
                .as_ref()
                .map(|(r, _)| ai::render_marked(&r.changes));
            accept();
            let report = serde_json::json!({
                "questions": a.questions.len(),
                "draft_html_before": before,
                "revision": revision,
                "final_html": content.read().clone(),
                "error": error.read().clone(),
            });
            println!("AUTOPILOT {report}");
        });
    }

    let subscription_notice =
        props.settings.login == Login::Subscription && props.settings.kind != CliKind::Ollama;
    let has_draft = basis.read().is_some();

    rsx! {
        div { class: "ai-panel",
            div { class: "ai-head",
                span { class: "ai-title", "回复建议" }
                span { class: "ai-conn", "{conn_label(&props.settings)}" }
                span { class: "spacer" }
                button {
                    class: "ghost",
                    disabled: phase() == Phase::Analyzing,
                    onclick: move |_| analyze(),
                    if analysis.read().is_some() { "重新分析" } else { "分析这封邮件" }
                }
            }
            if subscription_notice {
                div { class: "ai-note", "该连接按订阅套餐登录处理，使用受服务商条款约束；Letterdesk 不读取任何登录凭据。" }
            }
            match phase() {
                Phase::Idle => rsx! { div { class: "ai-note", "点“分析这封邮件”，找出需要你决定的问题。" } },
                Phase::Analyzing => rsx! { div { class: "ai-note", "正在分析…" } },
                Phase::Failed(e) => rsx! { div { class: "ai-error", "分析失败：{e}" } },
                Phase::Ready => rsx! {},
            }
            if let Some(a) = analysis.read().clone() {
                div { class: "ai-summary", "{a.summary}" }
                if !a.needs_reply {
                    div { class: "ai-note", "判断：不需要回复（{a.reason}）" }
                }
                for q in a.questions.clone() {
                    {
                        let current = answers.read().iter().find(|x| x.question_id == q.id).map(|x| x.decision.clone());
                        let custom_text = match &current { Some(Decision::Custom { text }) => text.clone(), _ => String::new() };
                        let qid_later = q.id.clone();
                        let qid_custom = q.id.clone();
                        let is_later = current == Some(Decision::Later);
                        rsx! {
                            div { class: "question", key: "{q.id}",
                                div { class: "ask", "{q.ask}" }
                                if let Some(quote) = q.quote.clone() {
                                    div { class: "quote", "“{quote}”" }
                                }
                                div { class: "chips",
                                    for o in q.options.clone() {
                                        {
                                            let selected = current == Some(Decision::Choice { choice_id: o.id.clone() });
                                            let qid = q.id.clone();
                                            let oid = o.id.clone();
                                            rsx! {
                                                button {
                                                    key: "{o.id}",
                                                    class: if selected { "chip selected" } else { "chip" },
                                                    onclick: move |_| {
                                                        let d = if selected { None } else { Some(Decision::Choice { choice_id: oid.clone() }) };
                                                        set_answer(&mut answers.write(), &qid, d);
                                                    },
                                                    "{o.label}"
                                                }
                                            }
                                        }
                                    }
                                    button {
                                        class: if is_later { "chip selected" } else { "chip" },
                                        onclick: move |_| {
                                            let d = if is_later { None } else { Some(Decision::Later) };
                                            set_answer(&mut answers.write(), &qid_later, d);
                                        },
                                        "稍后答复"
                                    }
                                }
                                input {
                                    class: "custom",
                                    placeholder: "或者直接写你的意思（可用中文）",
                                    value: "{custom_text}",
                                    oninput: move |e| {
                                        let text = e.value();
                                        let d = if text.trim().is_empty() { None } else { Some(Decision::Custom { text }) };
                                        set_answer(&mut answers.write(), &qid_custom, d);
                                    },
                                }
                            }
                        }
                    }
                }
                div { class: "ai-actions",
                    input {
                        class: "custom",
                        placeholder: "补充说明（可用中文）",
                        value: "{note}",
                        oninput: move |e| note.set(e.value()),
                    }
                    select {
                        onchange: move |e| tone.set(match e.value().as_str() {
                            "formal" => Some(Tone::Formal),
                            "friendly" => Some(Tone::Friendly),
                            "brief" => Some(Tone::Brief),
                            _ => None,
                        }),
                        option { value: "", "语气：跟随来信" }
                        option { value: "formal", "正式" }
                        option { value: "friendly", "友好" }
                        option { value: "brief", "简短" }
                    }
                    button {
                        class: "primary",
                        disabled: busy(),
                        onclick: move |_| generate(),
                        if busy() { "生成中…" } else if has_draft { "按新选择改写" } else { "生成草稿" }
                    }
                }
            }
            if let Some(e) = error.read().clone() {
                div { class: "ai-error", "{e}" }
            }
            if let Some((rev, _)) = pending.read().clone() {
                div { class: "revision",
                    div { class: "ai-title", "修改对比" }
                    div { class: "diff",
                        for (i, span) in rev.changes.iter().enumerate() {
                            match span {
                                DiffSpan::Same(s) => rsx! { span { key: "{i}", "{s}" } },
                                DiffSpan::Removed(s) => rsx! { del { key: "{i}", "{s}" } },
                                DiffSpan::Added(s) => rsx! { ins { key: "{i}", "{s}" } },
                            }
                        }
                    }
                    div { class: "ai-actions",
                        button { class: "primary", onclick: move |_| accept(), "采用" }
                        button { class: "ghost", onclick: move |_| pending.set(None), "放弃" }
                    }
                }
            }
            if let Some(back) = back_translation.read().clone() {
                details { class: "plain",
                    summary { "回译（核对草稿意思）" }
                    pre { "{back}" }
                }
            }
        }
    }
}

fn conn_label(s: &AiSettings) -> String {
    let name = s.kind.display_name();
    match &s.model {
        Some(m) => format!("{name} · {m}"),
        None => name.to_string(),
    }
}

async fn pause(ms: u32) {
    let _ = document::eval(&format!(
        "await new Promise(r => setTimeout(r, {ms})); return 0;"
    ))
    .await;
}
