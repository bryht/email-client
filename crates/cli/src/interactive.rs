//! 交互式回信流程：分析 → 逐个选择 → 生成草稿 → 修改选择并查看修改对比。

use crate::render;
use ai::{
    Analysis, Answer, Assistant, Backend, Decision, DraftBasis, DraftRequest, Question, Tone,
};
use anyhow::Result;
use mail_core::Email;
use std::io::{BufRead, Write};
use std::path::Path;

/// 读一行输入；标准输入结束时返回 `None`。
fn ask(prompt: &str) -> Option<String> {
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line.trim().to_string()),
    }
}

/// 询问对一个问题的决定。返回 `None` 表示跳过（不在回信中回应）。
fn ask_decision(q: &Question) -> Option<Option<Decision>> {
    println!();
    println!("{} {}", render::bold(&format!("[{}]", q.id)), q.ask);
    if let Some(quote) = &q.quote {
        println!("     {}", render::dim(&format!("原句：“{quote}”")));
    }
    for o in &q.options {
        println!("     {}) {}", o.id, o.label);
    }
    println!(
        "     {}",
        render::dim("l) 稍后答复   回车) 跳过   也可以直接输入你的意图（可用中文）")
    );
    let input = ask("  你的选择：")?;
    let decision = if input.is_empty() {
        None
    } else if q.options.iter().any(|o| o.id == input) {
        Some(Decision::Choice { choice_id: input })
    } else if input.eq_ignore_ascii_case("l") {
        Some(Decision::Later)
    } else {
        Some(Decision::Custom { text: input })
    };
    Some(decision)
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

fn ask_tone() -> Option<Option<Tone>> {
    let input = ask("语气：1) 正式  2) 友好  3) 简短  回车) 跟随来信：")?;
    Some(match input.as_str() {
        "1" => Some(Tone::Formal),
        "2" => Some(Tone::Friendly),
        "3" => Some(Tone::Brief),
        _ => None,
    })
}

struct State {
    answers: Vec<Answer>,
    note: Option<String>,
    tone: Option<Tone>,
}

impl State {
    fn request<'a>(
        &'a self,
        email: &'a Email,
        analysis: &'a Analysis,
        my_name: Option<&'a str>,
    ) -> DraftRequest<'a> {
        DraftRequest {
            email,
            analysis,
            answers: &self.answers,
            note: self.note.as_deref(),
            tone: self.tone,
            draft_lang: None,
            my_name,
        }
    }
}

pub fn run<B: Backend>(
    assistant: &Assistant<B>,
    email: &Email,
    me: &[String],
    my_name: Option<&str>,
    out: &Path,
) -> Result<()> {
    render::email_header(email);
    let analysis = render::with_progress(
        &format!("正在用 {} 分析来信", assistant.backend().name()),
        || assistant.analyze(email),
    )?;
    println!();
    render::analysis(&analysis);

    if !analysis.needs_reply {
        let go = ask("AI 判断这封邮件不需要回复。仍要回复吗？[y/N] ").unwrap_or_default();
        if !go.eq_ignore_ascii_case("y") {
            return Ok(());
        }
    }

    let mut state = State {
        answers: Vec::new(),
        note: None,
        tone: None,
    };
    for q in &analysis.questions {
        let Some(decision) = ask_decision(q) else {
            return Ok(());
        };
        set_answer(&mut state.answers, &q.id, decision);
    }
    println!();
    let Some(note) = ask("补充说明（可用中文，回车跳过）：") else {
        return Ok(());
    };
    state.note = Some(note).filter(|n| !n.is_empty());

    let draft = render::with_progress("正在生成草稿", || {
        assistant.draft(&state.request(email, &analysis, my_name))
    })?;
    // 当前草稿所依据的决定；只有之后改变了的决定才会改动草稿。
    let mut basis: DraftBasis = state.request(email, &analysis, my_name).basis();
    std::fs::write(out, &draft.body)?;
    println!();
    render::recipients(&email.reply_all_recipients(me));
    render::draft(&draft);
    println!(
        "{}",
        render::dim(&format!(
            "草稿已保存到 {}。可以直接用编辑器修改；之后的改写会保留你的修改，并以修改对比的方式请你确认。",
            out.display()
        ))
    );

    loop {
        println!();
        let Some(choice) =
            ask("1) 修改某个问题的决定  2) 修改补充说明  3) 改语气  4) 查看当前草稿  q) 完成：")
        else {
            break;
        };
        match choice.as_str() {
            "1" => {
                let ids: Vec<&str> = analysis.questions.iter().map(|q| q.id.as_str()).collect();
                if ids.is_empty() {
                    println!("这封邮件没有需要决定的问题。");
                    continue;
                }
                let Some(id) = ask(&format!("要修改哪个问题（{}）：", ids.join(" / ")))
                else {
                    break;
                };
                let Some(q) = analysis.questions.iter().find(|q| q.id == id) else {
                    println!("没有编号为 {id} 的问题。");
                    continue;
                };
                let Some(decision) = ask_decision(q) else {
                    break;
                };
                set_answer(&mut state.answers, &q.id, decision);
            }
            "2" => {
                let Some(note) = ask("新的补充说明（回车清空）：") else {
                    break;
                };
                state.note = Some(note).filter(|n| !n.is_empty());
            }
            "3" => {
                let Some(tone) = ask_tone() else { break };
                state.tone = tone;
            }
            "4" => {
                let current = std::fs::read_to_string(out)?;
                println!();
                println!("{current}");
                continue;
            }
            "q" | "Q" => break,
            _ => continue,
        }

        let current = std::fs::read_to_string(out)?;
        let revision = match render::with_progress("正在改写草稿", || {
            assistant.revise(&state.request(email, &analysis, my_name), &current, &basis)
        }) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("改写失败：{e}");
                continue;
            }
        };
        println!();
        render::revision(&revision);
        if revision.is_unchanged() {
            continue;
        }
        let accept = ask("采用这次改写？[Y/n] ").unwrap_or_default();
        if accept.is_empty() || accept.eq_ignore_ascii_case("y") {
            std::fs::write(out, &revision.draft.body)?;
            basis = state.request(email, &analysis, my_name).basis();
            println!("已更新 {}", out.display());
        } else {
            println!("保留原草稿。");
        }
    }
    println!("草稿位于 {}。", out.display());
    Ok(())
}
