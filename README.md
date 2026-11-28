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
| M0 | AI 命令行原型 | 完成；Ollama、Codex、Windows 真机实测待具备条件的环境补做 |
| M1 | Dioxus 界面原型：Rust 自研富文本编辑器、HTML 邮件隔离 | 进行中：两项高风险验证已通过，见“M1 进展” |
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
  - 流式输出：`Assistant::draft_streaming` / `revise_streaming` 在生成过程中回调已生成的正文。Claude Code 用 `--output-format stream-json --include-partial-messages`，从 `input_json_delta` 片段拼出 JSON，再用 `partial_string_field` 取出不完整 JSON 里的 `body`；Ollama 直接读取流式 JSON 文本。
  - Windows：目标是 npm 的 `.cmd` 批处理文件时，把参数里的换行压平（`batch_safe_args`），避免 cmd.exe 无法传递多行参数。
  - 提示词把邮件和草稿放在带随机标记的不可信数据分隔符内；输出按 JSON Schema 校验。
- `letterdesk` 命令行：`analyze`、`draft`、`revise`、交互式 `reply`；草稿边生成边显示；`draft` / `revise --out` 会在草稿旁保存 `<草稿>.basis.json` 作为生成依据。
- 样例邮件 8 封（`samples/`）及分析预期（`samples/expectations.json`）。
- 测试：单元测试 35 个；Windows 专用测试（`crates/ai/tests/windows_cmd_shim.rs`，用与 npm 相同结构的假 `.cmd` 验证含换行、引号、`%`、`&`、JSON 的参数和中文标准输入能完整传递）；对真实 CLI 的安全验收测试（`live_cli.rs`）和提示词回归检查（`live_samples.rs`），后两者默认忽略。
- CI：Windows / macOS / Linux 上运行 fmt、clippy、测试；`cargo-deny` 检查依赖许可与安全漏洞。

**已验证**（本机 Claude Code 2.1）：Maya 样例的分析、中文决定生成英文草稿与回译、手改 5%→3% 后只改会议决定时 3% 被保留、收据判断为不需要回复、法语邀请生成法语草稿；两项安全验收测试通过；8 封样例的回归检查全部符合预期且原句全部核对通过；流式输出在约 3 秒时开始显示正文（完整结果约 5 秒）。Windows CI 上假 `.cmd` 测试通过。

### M0 未完成（当前环境做不了，需要换环境）

1. **Ollama 实测**：本云端环境的网络策略拦截了 ollama.com 和 GitHub Releases 的下载，无法安装。需在装有 Ollama 的电脑上运行：`LETTERDESK_LIVE=ollama LETTERDESK_LIVE_MODEL=<模型> cargo test -p ai --test live_cli --test live_samples -- --ignored`，并试一遍 `letterdesk reply --backend ollama --model <模型>`。重点确认 `ollama run --format json` 的输出能稳定通过校验、流式输出能逐步显示正文。
2. **Codex**：需要 OpenAI 账号登录。确认能否完全关闭命令执行工具并限制文件读取；能做到就把 `CliKind::Codex.safety()` 改为 `Enforced` 并补测试，做不到就保持默认拒绝。同时补充 PLAN 第 4 节的 Codex 条款说明（需要读 OpenAI 的官方说明）。
3. **Windows 真机**：CI 已用假 `.cmd` 验证参数传递；仍建议在 Windows 上用真实的 npm 版 `claude` 跑一次 `live_cli`。

### M1 进展

`crates/desktop`（`letterdesk-desktop`）是界面原型：左侧导航（Mail / People / Calendar）、中间阅读区、右侧写信框。当前只读一封 `.eml`（默认 `samples/html-newsletter.eml`），还没有接入 AI 和邮箱。

**已验证的两项高风险设计**（`letterdesk-desktop --selftest` 自动检查，Linux CI 中在 Xvfb 下运行）：

1. **HTML 来信隔离**（`html.rs`）：ammonia 清洗（保留表格、内联样式和 `<style>`，去掉脚本、事件属性、表单、嵌入框架、`javascript:` 与 `data:` 链接）→ 包装成独立文档并注入 CSP（`default-src 'none'`，图片来源按“自动加载外部图片”开关切换）→ 放进空 `sandbox` 的 iframe。自检用一封恶意来信同时渲染“清洗后”和“未清洗、只靠 CSP + sandbox”两个 iframe，再加一个允许脚本的对照 iframe：只收到对照组的消息，来信中的脚本和事件处理一个都没有运行，应用页面也读不到 iframe 内容。
2. **Rust 负责的写信框**（`composer.rs`）：结论是“输入、光标、输入法交给浏览器的 contenteditable，其余由 Rust 负责”，不引入 JS 编辑器库。格式命令（粗体、斜体、下划线、列表、链接、清除格式）经 `document.execCommand` 执行；每次输入后 Rust 读回 HTML 并用白名单清洗，粘贴后清洗并写回；`html_to_text` 生成发信用的 text/plain 部分；“填入 AI 草稿”把纯文本转成段落。完全用 Rust 自绘编辑器（自己处理光标、选区、输入法）工作量过大，不建议。

**M1 待做**：

1. 把 `ai` 接进界面：来信下显示问题与选项，点选后流式生成草稿到写信框，改选项时显示修改对比（用 `DiffSpan` 渲染）。
2. 来信中的链接：现在 sandbox 会拦截点击；需要改为在系统浏览器中打开（例如由 Rust 在清洗时给链接加标记，点击由应用接管）。
3. 输入法与粘贴的真机验证（Windows WebView2）：中文输入法组字时不能被读回和写回打断；`execCommand` 在 WebView2 上的行为。
4. 键盘快捷键（Ctrl+B 等）、撤销/重做在写回后的表现。
5. Windows 和 macOS 上运行 `--selftest`（目前只在 Linux CI 上跑）。

### 下一步

继续 M1：先做“M1 待做”第 1 项，把 AI 选项和流式草稿接进界面，再处理链接打开。M0 未完成的三项可在具备条件的环境中随时补做。

### 已知问题

- 交互式 `reply` 的选项编号由模型给出（`a`、`b`…）；输入恰好等于某个选项编号的自写文字会被当作选项。
- 改写时模型偶尔会润色未改变的句子；修改对比会把这些改动显示出来，由用户决定是否采用。

## 代码结构

| 目录 | 内容 |
|---|---|
| `crates/mail-core` | 解析邮件、去掉引用历史、按“回复所有人”规则确定收件人 |
| `crates/ai` | 提示词、输出校验、修改对比，以及在安全限制下调用本机 CLI |
| `crates/cli` | `letterdesk` 命令行工具 |
| `crates/desktop` | `letterdesk-desktop` 桌面界面原型（Dioxus） |
| `samples/` | 样例邮件（会议与报价、法语邀请、中文合同、多问题、群发确认、转发、收据、恶意指令）及分析预期 |

## 构建

需要 Rust 1.88 及以上：

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

## 桌面界面原型

Linux 需要先安装 WebKitGTK：`sudo apt-get install libwebkit2gtk-4.1-dev libxdo-dev libgtk-3-dev`；Windows（WebView2）和 macOS 无需额外安装。

```sh
cargo run -p letterdesk-desktop                                   # 默认打开 samples/html-newsletter.eml
cargo run -p letterdesk-desktop -- --email samples/maya-meeting.eml --draft reply.txt
cargo run -p letterdesk-desktop -- --selftest                     # 隔离与清洗自检，全部通过时退出码为 0
```

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

修改提示词后，运行回归检查，确认 `samples/` 中每封邮件的分析结果仍符合 `samples/expectations.json`：

```sh
LETTERDESK_LIVE=claude cargo test -p ai --test live_samples -- --ignored --nocapture
```

## 许可证

本项目以 [GNU General Public License v3.0 or later](LICENSE)（GPL-3.0-or-later）发布。
