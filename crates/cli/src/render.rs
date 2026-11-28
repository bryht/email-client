//! 终端输出。

use ai::{Analysis, DiffSpan, Draft, Revision};
use mail_core::{Email, ReplyRecipients};
use std::io::{IsTerminal, Write};
use std::time::Instant;

fn color() -> bool {
    std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none()
}

fn paint(code: &str, text: &str) -> String {
    if color() {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

pub fn bold(text: &str) -> String {
    paint("1", text)
}

pub fn dim(text: &str) -> String {
    paint("2", text)
}

/// 执行耗时操作并在标准错误上显示进度。
pub fn with_progress<T, E>(label: &str, f: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
    let start = Instant::now();
    eprint!("{label}…");
    let _ = std::io::stderr().flush();
    let result = f();
    let secs = start.elapsed().as_secs_f32();
    match &result {
        Ok(_) => eprintln!(" 完成（{secs:.1} 秒）"),
        Err(_) => eprintln!(" 失败（{secs:.1} 秒）"),
    }
    result
}

fn list(v: &[mail_core::Mailbox]) -> String {
    v.iter()
        .map(|m| m.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn email_header(email: &Email) {
    println!("{} {}", bold("From:"), email.from);
    if !email.to.is_empty() {
        println!("{} {}", bold("To:"), list(&email.to));
    }
    if !email.cc.is_empty() {
        println!("{} {}", bold("Cc:"), list(&email.cc));
    }
    println!("{} {}", bold("Subject:"), email.subject);
    if let Some(date) = &email.date {
        println!("{} {}", bold("Date:"), date);
    }
    println!();
    println!("{}", email.new_content());
    println!();
}

pub fn analysis(a: &Analysis) {
    println!("{} {}", bold("摘要："), a.summary);
    let verdict = if a.needs_reply {
        "需要回复"
    } else {
        "不需要回复"
    };
    println!("{} {verdict}（{}）", bold("判断："), a.reason);
    for q in &a.questions {
        println!();
        println!("{} {}", bold(&format!("[{}]", q.id)), q.ask);
        if let Some(quote) = &q.quote {
            println!("     {}", dim(&format!("原句：“{quote}”")));
        }
        let options: Vec<String> = q
            .options
            .iter()
            .map(|o| format!("{}) {}", o.id, o.label))
            .collect();
        println!("     {}", options.join("   "));
    }
    println!();
}

pub fn recipients(r: &ReplyRecipients) {
    println!("{} {}", bold("To:"), list(&r.to));
    if !r.cc.is_empty() {
        println!("{} {}", bold("Cc:"), list(&r.cc));
    }
}

/// 执行流式生成：先显示标题，再把正文边生成边打印。
/// 返回结果以及正文是否已经显示过（不支持流式输出的后端不会显示）。
pub fn streaming<T, E>(
    label: &str,
    faint: bool,
    f: impl FnOnce(&mut dyn FnMut(&str)) -> Result<T, E>,
) -> (Result<T, E>, bool) {
    let start = Instant::now();
    eprintln!("{label}…");
    let mut shown = 0usize;
    let mut out = std::io::stdout();
    let result = f(&mut |body: &str| {
        // 正文只会变长；打印新增的部分。
        if let Some(new) = body.get(shown..) {
            let text = if faint { dim(new) } else { new.to_string() };
            let _ = write!(out, "{text}");
            let _ = out.flush();
            shown = body.len();
        }
    });
    if shown > 0 {
        println!();
    }
    let secs = start.elapsed().as_secs_f32();
    match &result {
        Ok(_) => eprintln!("完成（{secs:.1} 秒）"),
        Err(_) => eprintln!("失败（{secs:.1} 秒）"),
    }
    (result, shown > 0)
}

/// 显示草稿；正文已在流式生成时显示过的，只补充语言和回译。
pub fn draft_after_stream(d: &Draft, body_shown: bool) {
    if body_shown {
        println!("{}", dim(&format!("（草稿语言：{}）", d.language)));
        if let Some(back) = &d.back_translation {
            println!();
            println!("{}", dim("── 回译 ──"));
            println!("{}", dim(back));
        }
        println!();
    } else {
        draft(d);
    }
}

pub fn draft(d: &Draft) {
    println!("{}", bold(&format!("── 草稿（{}）──", d.language)));
    println!("{}", d.body);
    if let Some(back) = &d.back_translation {
        println!();
        println!("{}", dim("── 回译 ──"));
        println!("{}", dim(back));
    }
    println!();
}

pub fn diff(spans: &[DiffSpan]) -> String {
    if !color() {
        return ai::render_marked(spans);
    }
    let mut out = String::new();
    for span in spans {
        match span {
            DiffSpan::Same(s) => out.push_str(s),
            DiffSpan::Removed(s) => out.push_str(&paint("31;9", s)),
            DiffSpan::Added(s) => out.push_str(&paint("32;4", s)),
        }
    }
    out
}

pub fn revision(r: &Revision) {
    if r.is_unchanged() {
        println!("改写结果与当前草稿相同。");
        return;
    }
    println!("{}", bold("── 修改对比 ──"));
    if !color() {
        println!("{}", dim("[-删除-] {+新增+}"));
    }
    println!("{}", diff(&r.changes));
    if let Some(back) = &r.draft.back_translation {
        println!();
        println!("{}", dim("── 改写后的回译 ──"));
        println!("{}", dim(back));
    }
    println!();
}
