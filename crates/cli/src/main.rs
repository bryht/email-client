//! Letterdesk AI 命令行原型。
//!
//! 用法示例：
//!   letterdesk reply samples/maya-meeting.eml
//!   letterdesk analyze samples/maya-meeting.eml --save analysis.json
//!   letterdesk draft samples/maya-meeting.eml --analysis analysis.json --answer q1=b --answer q2=最多5%
//!   letterdesk revise samples/maya-meeting.eml --analysis analysis.json --draft reply.txt --answer q2=a

mod interactive;
mod render;
mod sync;

use ai::cli::{CliBackend, CliConfig, CliKind, Login};
use ai::{Analysis, Answer, Assistant, Decision, DraftBasis, DraftRequest, Tone};
use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use mail_core::Email;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Parser)]
#[command(name = "letterdesk", version, about = "Letterdesk AI 命令行原型")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 同步邮箱到本地 .eml 目录（之后可用 letterdesk-desktop --mailbox 打开）
    Sync {
        #[command(subcommand)]
        account: SyncAccount,
    },
    /// 交互式回信：分析来信、逐个选择、生成草稿，再按新的选择改写并查看修改对比
    Reply {
        /// 来信（.eml 文件）
        eml: PathBuf,
        #[command(flatten)]
        conn: Connection,
        /// 草稿保存位置，默认为当前目录下的 <来信文件名>.draft.txt
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// 分析来信：是否需要回复、需要决定的问题和选项
    Analyze {
        eml: PathBuf,
        #[command(flatten)]
        conn: Connection,
        /// 把分析结果保存为 JSON，供 draft / revise 使用
        #[arg(long)]
        save: Option<PathBuf>,
        /// 以 JSON 输出
        #[arg(long)]
        json: bool,
    },
    /// 按指定的选择生成草稿
    Draft {
        eml: PathBuf,
        #[command(flatten)]
        conn: Connection,
        #[command(flatten)]
        choices: Choices,
        /// 已保存的分析结果；不提供时先分析一次
        #[arg(long)]
        analysis: Option<PathBuf>,
        /// 把草稿正文写入文件
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// 以当前草稿（可含手改内容）为基础整篇改写，显示修改对比
    Revise {
        eml: PathBuf,
        #[command(flatten)]
        conn: Connection,
        #[command(flatten)]
        choices: Choices,
        #[arg(long)]
        analysis: PathBuf,
        /// 当前草稿文件
        #[arg(long)]
        draft: PathBuf,
        /// 采用改写结果并写入此文件（可与 --draft 相同）
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum SyncAccount {
    /// Outlook.com / Microsoft 365（微软登录 + Microsoft Graph）
    Outlook {
        /// 保存邮件的目录
        #[arg(long)]
        out: PathBuf,
        /// 应用（客户端）ID；对着模拟服务时用 letterdesk-mock 打印的 ID
        #[arg(long)]
        client_id: String,
        /// 登录与 Graph 的基地址，例如模拟服务 http://127.0.0.1:8901；不填则连接真实的微软服务
        #[arg(long)]
        base: Option<String>,
        /// 不打开浏览器，由本程序直接完成授权页跳转（只用于模拟服务）
        #[arg(long)]
        headless_login: bool,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum BackendArg {
    Claude,
    Codex,
    Ollama,
}

#[derive(Clone, Copy, ValueEnum)]
enum LoginArg {
    /// API 密钥登录（按量计费）
    ApiKey,
    /// Pro、Max 等订阅套餐登录
    Subscription,
}

#[derive(Clone, Copy, ValueEnum)]
enum ToneArg {
    Formal,
    Friendly,
    Brief,
}

/// AI 连接参数。
#[derive(Args, Clone)]
struct Connection {
    /// 使用的命令行工具
    #[arg(long, value_enum, default_value = "claude")]
    backend: BackendArg,
    /// 模型名称（Ollama 必填，例如 qwen3）
    #[arg(long)]
    model: Option<String>,
    /// 可执行文件路径，默认在 PATH 中查找
    #[arg(long)]
    program: Option<PathBuf>,
    /// 这个 CLI 的登录方式
    #[arg(long, value_enum, default_value = "subscription")]
    login: LoginArg,
    /// 单次调用的超时时间（秒）
    #[arg(long, default_value_t = 180)]
    timeout: u64,
    /// 允许使用尚未确认安全限制的工具（目前是 Codex），仅供开发验证
    #[arg(long)]
    allow_unverified: bool,
    /// 界面语言：问题、选项和回译使用这个语言
    #[arg(long, default_value = "zh-CN")]
    ui_lang: String,
    /// 我的邮箱地址（可重复），用于计算回复所有人时的收件人
    #[arg(long = "me")]
    me: Vec<String>,
    /// 我的名字
    #[arg(long)]
    my_name: Option<String>,
}

/// 非交互模式下的选择。
#[derive(Args, Clone)]
struct Choices {
    /// 对某个问题的决定：q1=a（选项编号）、q1=later（稍后答复）或 q1=任意文字（自己写意图）
    #[arg(long = "answer")]
    answers: Vec<String>,
    /// 补充说明（可用中文）
    #[arg(long)]
    note: Option<String>,
    #[arg(long, value_enum)]
    tone: Option<ToneArg>,
    /// 草稿语言（BCP 47），默认与来信相同
    #[arg(long)]
    draft_lang: Option<String>,
}

impl Connection {
    fn backend(&self) -> Result<CliBackend> {
        let kind = match self.backend {
            BackendArg::Claude => CliKind::ClaudeCode,
            BackendArg::Codex => CliKind::Codex,
            BackendArg::Ollama => CliKind::Ollama,
        };
        let mut config = CliConfig::new(kind);
        config.program = self.program.clone();
        config.model = self.model.clone();
        config.login = match self.login {
            LoginArg::ApiKey => Login::ApiKey,
            LoginArg::Subscription => Login::Subscription,
        };
        config.timeout = Duration::from_secs(self.timeout);
        config.allow_unverified = self.allow_unverified;
        let backend = CliBackend::new(config)?;
        if self.login_notice_needed() {
            eprintln!(
                "提示：{} 按订阅套餐登录处理。订阅套餐面向个人正常使用，使用受服务商条款约束。\
Letterdesk 只调用原样安装的官方 CLI，不读取任何登录凭据。用 API 密钥登录时请加 --login api-key。",
                backend.config().kind.display_name()
            );
        }
        Ok(backend)
    }

    fn login_notice_needed(&self) -> bool {
        matches!(self.login, LoginArg::Subscription) && !matches!(self.backend, BackendArg::Ollama)
    }
}

impl Choices {
    fn tone(&self) -> Option<Tone> {
        self.tone.map(|t| match t {
            ToneArg::Formal => Tone::Formal,
            ToneArg::Friendly => Tone::Friendly,
            ToneArg::Brief => Tone::Brief,
        })
    }

    fn answers(&self, analysis: &Analysis) -> Result<Vec<Answer>> {
        self.answers
            .iter()
            .map(|raw| parse_answer(raw, analysis))
            .collect()
    }
}

/// 解析 `q1=a` 形式的决定。
fn parse_answer(raw: &str, analysis: &Analysis) -> Result<Answer> {
    let (qid, value) = raw
        .split_once('=')
        .with_context(|| format!("决定格式应为 问题编号=选择，例如 q1=a：{raw}"))?;
    let qid = qid.trim();
    let value = value.trim();
    let Some(question) = analysis.questions.iter().find(|q| q.id == qid) else {
        bail!("没有编号为 {qid} 的问题");
    };
    let decision = if question.options.iter().any(|o| o.id == value) {
        Decision::Choice {
            choice_id: value.to_string(),
        }
    } else if value.eq_ignore_ascii_case("later") || value == "稍后" {
        Decision::Later
    } else if value.is_empty() {
        bail!("问题 {qid} 的决定为空");
    } else {
        Decision::Custom {
            text: value.to_string(),
        }
    };
    Ok(Answer {
        question_id: qid.to_string(),
        decision,
    })
}

fn load_email(path: &Path) -> Result<Email> {
    let raw = std::fs::read(path).with_context(|| format!("无法读取 {}", path.display()))?;
    Email::parse(&raw).with_context(|| format!("无法解析 {}", path.display()))
}

fn load_analysis(path: &Path) -> Result<Analysis> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("无法读取 {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("{} 不是有效的分析结果", path.display()))
}

fn main() {
    if let Err(err) = run(Cli::parse()) {
        eprintln!("错误：{err:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Sync { account } => sync::run(account),
        Command::Reply { eml, conn, out } => {
            let email = load_email(&eml)?;
            let assistant = Assistant::new(conn.backend()?, conn.ui_lang.clone());
            let out = out.unwrap_or_else(|| default_draft_path(&eml));
            interactive::run(&assistant, &email, &conn.me, conn.my_name.as_deref(), &out)
        }
        Command::Analyze {
            eml,
            conn,
            save,
            json,
        } => {
            let email = load_email(&eml)?;
            let assistant = Assistant::new(conn.backend()?, conn.ui_lang.clone());
            let analysis =
                render::with_progress("正在分析来信", || assistant.analyze(&email))?;
            if let Some(path) = save {
                std::fs::write(&path, serde_json::to_string_pretty(&analysis)?)?;
                eprintln!("分析结果已保存到 {}", path.display());
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&analysis)?);
            } else {
                render::email_header(&email);
                render::analysis(&analysis);
            }
            Ok(())
        }
        Command::Draft {
            eml,
            conn,
            choices,
            analysis,
            out,
            json,
        } => {
            let email = load_email(&eml)?;
            let assistant = Assistant::new(conn.backend()?, conn.ui_lang.clone());
            let analysis = match analysis {
                Some(path) => load_analysis(&path)?,
                None => render::with_progress("正在分析来信", || assistant.analyze(&email))?,
            };
            let answers = choices.answers(&analysis)?;
            let req = DraftRequest {
                email: &email,
                analysis: &analysis,
                answers: &answers,
                note: choices.note.as_deref(),
                tone: choices.tone(),
                draft_lang: choices.draft_lang.as_deref(),
                my_name: conn.my_name.as_deref(),
            };
            let draft = if json {
                render::with_progress("正在生成草稿", || assistant.draft(&req))?
            } else {
                render::recipients(&email.reply_all_recipients(&conn.me));
                let (draft, shown) = render::streaming("正在生成草稿", false, |on_body| {
                    assistant.draft_streaming(&req, on_body)
                });
                let draft = draft?;
                render::draft_after_stream(&draft, shown);
                draft
            };
            if let Some(path) = out {
                std::fs::write(&path, &draft.body)?;
                save_basis(&path, &req.basis())?;
                eprintln!("草稿已保存到 {}", path.display());
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&draft)?);
            }
            Ok(())
        }
        Command::Revise {
            eml,
            conn,
            choices,
            analysis,
            draft,
            out,
        } => {
            let email = load_email(&eml)?;
            let assistant = Assistant::new(conn.backend()?, conn.ui_lang.clone());
            let analysis = load_analysis(&analysis)?;
            let current = std::fs::read_to_string(&draft)
                .with_context(|| format!("无法读取 {}", draft.display()))?;
            let answers = choices.answers(&analysis)?;
            let req = DraftRequest {
                email: &email,
                analysis: &analysis,
                answers: &answers,
                note: choices.note.as_deref(),
                tone: choices.tone(),
                draft_lang: choices.draft_lang.as_deref(),
                my_name: conn.my_name.as_deref(),
            };
            let basis = load_basis(&draft)?;
            let revision = render::with_progress("正在改写草稿", || {
                assistant.revise(&req, &current, &basis)
            })?;
            render::revision(&revision);
            if let Some(path) = out {
                std::fs::write(&path, &revision.draft.body)?;
                save_basis(&path, &req.basis())?;
                eprintln!("改写结果已写入 {}", path.display());
            }
            Ok(())
        }
    }
}

/// 草稿旁边保存的“生成依据”文件：记录生成这份草稿时的决定，供之后改写比较。
fn basis_path(draft: &Path) -> PathBuf {
    let mut name = draft.as_os_str().to_owned();
    name.push(".basis.json");
    PathBuf::from(name)
}

fn save_basis(draft: &Path, basis: &DraftBasis) -> Result<()> {
    std::fs::write(basis_path(draft), serde_json::to_string_pretty(basis)?)?;
    Ok(())
}

/// 读取草稿的生成依据；没有时视为所有决定都已改变。
fn load_basis(draft: &Path) -> Result<DraftBasis> {
    let path = basis_path(draft);
    if !path.exists() {
        eprintln!(
            "提示：没有找到 {}，将按所有决定都已改变处理。",
            path.display()
        );
        return Ok(DraftBasis::default());
    }
    let text = std::fs::read_to_string(&path)?;
    serde_json::from_str(&text).with_context(|| format!("{} 格式无效", path.display()))
}

fn default_draft_path(eml: &Path) -> PathBuf {
    let stem = eml
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "reply".into());
    PathBuf::from(format!("{stem}.draft.txt"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ai::{Choice, Question};

    fn analysis() -> Analysis {
        Analysis {
            needs_reply: true,
            reason: String::new(),
            language: "en".into(),
            summary: String::new(),
            questions: vec![Question {
                id: "q1".into(),
                ask: "周四能开会吗？".into(),
                quote: None,
                options: vec![Choice {
                    id: "a".into(),
                    label: "可以".into(),
                }],
            }],
        }
    }

    #[test]
    fn parses_answers() {
        let a = analysis();
        assert_eq!(
            parse_answer("q1=a", &a).unwrap().decision,
            Decision::Choice {
                choice_id: "a".into()
            }
        );
        assert_eq!(
            parse_answer("q1=later", &a).unwrap().decision,
            Decision::Later
        );
        assert_eq!(
            parse_answer("q1=改到下周二", &a).unwrap().decision,
            Decision::Custom {
                text: "改到下周二".into()
            }
        );
        assert!(parse_answer("q9=a", &a).is_err());
        assert!(parse_answer("q1", &a).is_err());
        assert!(parse_answer("q1=", &a).is_err());
    }

    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
