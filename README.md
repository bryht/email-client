# email-client

Letterdesk（暂名）：用 Rust 编写的开源桌面邮件客户端，按人浏览往来，并用 AI 辅助阅读和回复邮件。方案见 [docs/PLAN.md](docs/PLAN.md)。

当前阶段：第一步 **AI 命令行原型**。还没有图形界面，也不连接真实邮箱；它读取 `.eml` 文件，通过本机安装的 Claude Code、Ollama（或实验性的 Codex）找出来信里需要你决定的问题，按你的选择生成回信草稿，并在你修改决定后整篇改写、显示修改对比。

## 实施进度（交接说明）

> 这一节是给下一次实施（人或 AI Agent）的交接说明。**每次实施结束时更新这一节**：把完成的事项移到“已完成”，写下验证方式和新发现的问题，再把“下一步”调整为接下来最该做的事。

### 工作约定

- 产品与设计决定以 [docs/PLAN.md](docs/PLAN.md) 为准；与它不一致时先改方案，再改代码。
- 直接提交并推送到 `main`，不开 PR。推送前在本地运行：`cargo fmt --all`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`。推送后确认 GitHub Actions 的 CI 通过。
- 涉及 AI 调用的改动，在有已登录 CLI 的环境中运行一次安全验收测试（见下文“测试”）。
- 文档和界面文字用简体中文；代码标识符用英文。

### 里程碑

| # | 里程碑 | 状态 |
|---|---|---|
| M0 | AI 命令行原型 | 基本完成，见下方“未完成” |
| M1 | Dioxus 界面原型：Rust 自研富文本编辑器、HTML 邮件隔离 | 未开始 |
| M2 | 收信与按人浏览（Outlook / Graph、Gmail / IMAP） | 未开始 |
| M3 | 发信与草稿同步 | 未开始 |
| M4 | 多账号、Outlook 日历（只读）、签名、通知与托盘 | 未开始 |
| M5 | 签名安装包，GitHub Releases 与 winget 发布 | 未开始 |

### M0 已完成

- `mail-core`：解析 `.eml`；去掉引用历史（`strip_quoted`）；按“回复所有人”规则确定收件人（Reply-To 优先，去掉自己和重复地址）。
- `ai`：
  - `Assistant::analyze`：判断是否需要回复，列出问题、选项和原句；原句必须在来信中出现，否则丢弃。
  - `Assistant::draft`：按决定生成草稿和回译；决定可以是选项、自写意图（可中文）或“稍后答复”。
  - `Assistant::revise`：以当前草稿为基础整篇改写，返回修改对比。传入生成依据 `DraftBasis`（生成当前草稿时的决定），提示词把每个问题标为 `[CHANGED]` 或 `[UNCHANGED]`，只改动变化了的决定，保留用户手改内容。
  - `cli::CliBackend`：调用本机 Claude Code / Ollama / Codex，遵守 PLAN 第 4 节的安全限制；Codex 标记为未验证，默认拒绝。
  - 提示词把邮件和草稿放在带随机标记的不可信数据分隔符内；输出按 JSON Schema 校验。
- `letterdesk` 命令行：`analyze`、`draft`、`revise`、交互式 `reply`；`draft` / `revise --out` 会在草稿旁保存 `<草稿>.basis.json` 作为生成依据。
- 样例邮件（`samples/`）、单元测试（29 个）、对真实 CLI 的安全验收测试（`crates/ai/tests/live_cli.rs`，默认忽略）。
- CI：Windows / macOS / Linux 上运行 fmt、clippy、测试；`cargo-deny` 检查依赖许可与安全漏洞。

**已验证**（本机 Claude Code 2.1）：Maya 样例的分析、中文决定生成英文草稿与回译、手改 5%→3% 后只改会议决定时 3% 被保留、收据判断为不需要回复、法语邀请生成法语草稿；两项安全验收测试通过。

### M0 未完成

1. **Ollama 实测**：用 `LETTERDESK_LIVE=ollama LETTERDESK_LIVE_MODEL=<模型>` 跑安全验收测试，并用样例邮件试一遍 `reply`。确认 `ollama run --format json` 的输出能稳定通过校验。
2. **Codex**：确认能否完全关闭命令执行工具并限制文件读取；能做到就把 `CliKind::Codex.safety()` 改为 `Enforced` 并补测试，做不到就保持默认拒绝。同时补充 PLAN 第 4 节的 Codex 条款说明（需要读 OpenAI 的官方说明）。
3. **Windows 验证**：npm 安装的 `claude` 是 `.cmd` 文件，`resolve_program` 会找到它，但 Rust 调用 `.cmd` 时对含引号、换行的参数（`--system-prompt`、`--json-schema`）可能拒绝执行或转义出错。需要在 Windows 上实测；必要时改为把系统提示并入标准输入，或在 CLI 支持时改用从文件读取提示词的参数。
4. **流式输出**：Claude Code 可用 `--output-format stream-json`；界面阶段需要边生成边显示草稿。
5. **提示词回归语料**：扩充 `samples/`（多问题、群发确认出席、带抄送与转发、长线程、中文来信），为分析和改写建立可重复的检查。

### 下一步

按顺序：先完成上面的 M0 未完成 1 到 3，再开始 M1（Dioxus 界面原型）。M1 的第一个任务是验证两件高风险的事：用 Rust 在 Dioxus 中自研富文本写信框（基于 contenteditable）是否可行，以及 HTML 邮件放在 `sandbox` iframe 加严格 CSP 中、碰不到应用接口。

### 已知问题

- 交互式 `reply` 的选项编号由模型给出（`a`、`b`…）；输入恰好等于某个选项编号的自写文字会被当作选项。
- 改写时模型偶尔会润色未改变的句子；修改对比会把这些改动显示出来，由用户决定是否采用。

## 代码结构

| 目录 | 内容 |
|---|---|
| `crates/mail-core` | 解析邮件、去掉引用历史、按“回复所有人”规则确定收件人 |
| `crates/ai` | 提示词、输出校验、修改对比，以及在安全限制下调用本机 CLI |
| `crates/cli` | `letterdesk` 命令行工具 |
| `samples/` | 样例邮件：会议与报价、法语邀请、收据、恶意指令 |

## 构建

需要 Rust 1.85 及以上：

```sh
cargo build --release
```

## 使用

需要先安装并登录至少一个 AI 命令行工具：

- **Claude Code**（默认）：安装后运行一次 `claude` 完成登录。
- **Ollama**：安装后下载模型（如 `ollama pull qwen3`），使用时加 `--backend ollama --model qwen3`。
- **Codex**：尚未确认能完全关闭其命令执行工具，默认不允许使用；仅供开发验证时可加 `--backend codex --allow-unverified`。

交互式回信（推荐先试这个）：

```sh
letterdesk reply samples/maya-meeting.eml --me lin@example.com --my-name Lin
```

流程：显示来信 → 列出需要你决定的问题和选项 → 逐个选择（可直接输入中文意图，`l` 表示稍后答复，回车跳过）→ 生成英文草稿和中文回译，保存到 `maya-meeting.draft.txt`。之后可以修改某个决定、补充说明或语气，工具会以当前草稿为基础整篇改写，并显示修改对比由你确认。你直接在草稿文件里做的修改会被保留。

分步使用：

```sh
# 分析并保存结果
letterdesk analyze samples/maya-meeting.eml --save analysis.json

# 按选择生成草稿：q1=a 选选项，q1=later 稍后答复，q1=任意文字 表示自己的意图
letterdesk draft samples/maya-meeting.eml --analysis analysis.json \
  --answer "q1=周四不行，改到下周二上午" --answer "q2=最多优惠 5%" --out reply.txt

# 改了决定后整篇改写并查看修改对比；--out 表示采用改写结果
letterdesk revise samples/maya-meeting.eml --analysis analysis.json \
  --draft reply.txt --answer q1=a --answer "q2=最多优惠 5%" --out reply.txt
```

其他常用参数：`--ui-lang en`（问题和回译改用英文）、`--tone formal|friendly|brief`、`--draft-lang fr`、`--login api-key`（CLI 用 API 密钥登录时）。完整说明见 `letterdesk --help`。

### 关于登录方式

Letterdesk 只调用你原样安装的官方 CLI，不读取、不保存任何登录凭据。CLI 用 API 密钥登录时按量计费；用 Pro、Max 等订阅套餐登录时，使用受服务商条款约束（订阅套餐面向个人正常使用）。详见 [PLAN.md 第 4 节](docs/PLAN.md#4-邮件模型与费用)。

## 安全限制

邮件内容是不可信的。每次调用 CLI 时：

- 关闭全部工具（Claude Code：`--tools ""`），不加载 MCP 服务器、用户设置、钩子和斜杠命令；
- 在新建的空临时目录中运行，结束后删除；邮件内容只经标准输入传入；
- 邮件放在带随机标记的分隔符内，提示词要求模型不执行其中的任何指令；
- 只接受符合 JSON Schema 的输出，并核对“原句”确实出现在来信中，不合格的输出直接丢弃。

## 测试

```sh
cargo test --workspace
```

对真实 CLI 的安全验收测试默认不运行，需要本机已安装并登录对应工具：

```sh
LETTERDESK_LIVE=claude cargo test -p ai --test live_cli -- --ignored --nocapture
LETTERDESK_LIVE=ollama LETTERDESK_LIVE_MODEL=qwen3 cargo test -p ai --test live_cli -- --ignored
```

它会验证：恶意来信无法让 CLI 创建文件或泄露本地文件内容；即使任务本身要求使用工具，CLI 也无法创建文件。

## 许可证

本项目以 [GNU General Public License v3.0 or later](LICENSE)（GPL-3.0-or-later）发布。
