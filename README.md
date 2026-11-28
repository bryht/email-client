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
| M2 | 收信与按人浏览（Outlook / Graph、Gmail / IMAP） | 进行中：对话归组已完成，见“M2 进展” |
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

`crates/desktop`（`letterdesk-desktop`）是界面原型：左侧导航（Mail / People / Calendar）、中间阅读区和“回复建议”面板、右侧写信框。当前只读一封 `.eml`（默认 `samples/html-newsletter.eml`），还没有接入邮箱。

**AI 已接入界面**（`ai_panel.rs`）：点“分析这封邮件”后显示摘要、问题、原句和选项按钮（含“稍后答复”和自写意图输入框），可填补充说明、选语气；“生成草稿”把正文边生成边写进写信框，并提供回译；已有草稿时按钮变为“按新选择改写”，以写信框当前内容（含手改）为基础整篇改写，显示修改对比，由用户“采用”或“放弃”。模型调用在后台线程执行，经通道回到界面。`--autopilot` 会自动走一遍“分析 → 每题选第一个选项 → 生成 → 改选第一个问题 → 改写”，并把结果以 `AUTOPILOT {json}` 打印到标准输出，便于在无界面环境中检查。已用本机 Claude Code 实测：改选会议决定后，修改对比只涉及会议相关的句子，折扣段落保持不变。

**已验证的两项高风险设计**（`letterdesk-desktop --selftest` 自动检查，Linux CI 中在 Xvfb 下运行）：

1. **HTML 来信隔离**（`html.rs`）：ammonia 清洗（保留表格、内联样式和 `<style>`，去掉脚本、事件属性、表单、嵌入框架、`javascript:` 与 `data:` 链接）→ 包装成独立文档并注入 CSP（`default-src 'none'`，图片来源按“自动加载外部图片”开关切换）→ 放进空 `sandbox` 的 iframe。自检用一封恶意来信同时渲染“清洗后”和“未清洗、只靠 CSP + sandbox”两个 iframe，再加一个允许脚本的对照 iframe：只收到对照组的消息，来信中的脚本和事件处理一个都没有运行，应用页面也读不到 iframe 内容。
2. **Rust 负责的写信框**（`composer.rs`）：结论是“输入、光标、输入法交给浏览器的 contenteditable，其余由 Rust 负责”，不引入 JS 编辑器库。格式命令（粗体、斜体、下划线、列表、链接、清除格式）经 `document.execCommand` 执行；每次输入后 Rust 读回 HTML 并用白名单清洗，粘贴后清洗并写回；`html_to_text` 生成发信用的 text/plain 部分；“填入 AI 草稿”把纯文本转成段落。完全用 Rust 自绘编辑器（自己处理光标、选区、输入法）工作量过大，不建议。

**来信链接**：链接在来信 iframe 内导航（`<base target="_self">`），由 Dioxus Desktop 的导航处理拦截，交给系统浏览器打开，应用内不会加载外部页面。`scripts/desktop-link-test.sh` 在 Xvfb 下用 xdotool 真实点击链接，并用假浏览器（`$BROWSER`）记录打开的地址；已在 Linux CI 中运行。

**采用改写时保留格式**：`merge_formatting` 把写信框内容拆成顶层块（段落、列表、引用），与改写后的文字按段落对齐；文字没变的段落沿用原来的 HTML，改动的段落按纯文本重建。已用 `--autopilot` 实测：第一段手动加粗后改选会议决定并采用，粗体保留，只有会议相关段落被替换。限制：同一段落里既有格式又被改写时，这一段的格式会丢失。

**首次询问是否自动分析**（PLAN 第 1 节）：第一次使用某个 AI 连接时，回复建议面板先询问是否开启自动分析，并说明来信内容会发给哪里（云端服务或本机模型）；订阅套餐登录的云端连接默认建议“暂不开启”。选择按连接保存在本地设置文件（`settings.rs`，位于系统配置目录下的 `Letterdesk/settings.json`，可用 `LETTERDESK_CONFIG_DIR` 覆盖）；开启后打开邮件即自动分析，面板标题栏的“自动分析”开关可随时更改。已在 Xvfb 下用真实点击验证：选择被保存，开启后重启会自动分析。

**键盘快捷键**：在 Linux（WebKitGTK）上实测 Ctrl+B 可对选区加粗，纯文本部分同步更新。

**M1 待做**：

1. Windows（WebView2）和 macOS（WKWebView）上验证来信链接：确认 iframe 内的导航同样会触发导航处理并在系统浏览器打开；若不触发，需要改用其他方式（例如清洗时把链接改写为自定义协议，由应用处理）。
2. 撤销/重做：Linux 实测 Ctrl+Z 没有撤销刚输入的文字，需要查明原因（WebKitGTK 的按键绑定，或与读回同步有关），并在 WebView2 上验证。另外，填入 AI 草稿和采用改写时会整体替换编辑区内容，浏览器的撤销记录会被清空；需要应用自己保存“替换前”的版本，提供“撤销 AI 修改”。
3. 输入法与粘贴的真机验证（Windows WebView2）：中文输入法组字时不能被读回和写回打断；`execCommand` 在 WebView2 上的行为。
4. `mailto:` 链接目前交给系统的邮件程序；应改为在 Letterdesk 里新建写给该地址的邮件（依赖 M3 的新建邮件流程）。
5. Windows 和 macOS 上运行 `--selftest`（目前只在 Linux CI 上跑）。

### M2 进展

**对话归组**（`crates/conversation`）：实现 PLAN 第 2 节“新来的一封信放到哪儿”。`group(邮件, 自己的地址)` 是纯函数，每次同步后可整体重算：

- 回复跟随父邮件，中途增减 Cc 也不拆分；
- 转发一律新开对话并记录来源对话（`forwarded_from`），对转发的后续回复留在新对话，即使又抄送了原来的参与人；
- 另起的新邮件按直接参与人（发件人和 To，去掉自己及别名，地址不区分大小写）匹配，只增减 Cc 时沿用同一对话；
- 自己只在 Cc 里时同样按发件人和 To 归组；同一个人的多个地址不合并；回复的父邮件不在本地时按参与人归组；
- 邮件按时间（`mail-core` 新增的 UTC 时间戳）处理，输入顺序颠倒也能正确跟随父邮件；
- 对话内按标准邮件线程分话题，`base_subject` 去掉 Re:/Fwd:/回复：/转发：等前缀；对话按最近往来时间倒序。

12 个单元测试覆盖以上规则，其中一个用 `samples/` 的真实样例。

**按人浏览界面**（`crates/desktop/src/inbox.rs`）：`--mailbox 目录` 读取目录中所有 `.eml`，`--me` 指定自己的地址。左栏按最近往来列出对话：头像（按地址固定颜色）、参与人名字、第二行显示最近一封的 Cc（PLAN 的 `Cc 名字` 样式）或主题、日期。中间是所选对话的时间线：按话题分隔，每封显示 From / To / Cc 和日期；要回复的那封（最近一封别人发来的）默认显示 HTML 原文，其余显示纯文本摘要，可点“查看原文”展开。回复建议面板和写信框针对要回复的那封邮件，换对话时重置。

**模拟服务**（`crates/mock-servers`，命令 `letterdesk-mock`）：在没有真实账号时开发和测试收发信。三个服务共享同一个模拟邮箱（默认载入 `samples/`），路径和协议尽量与真实服务一致，客户端只需换基地址：

- **微软登录 + Microsoft Graph**：OAuth 2.0 授权码 + PKCE（校验 `code_verifier`、只接受回环地址作为回调、刷新令牌只能用一次）；`/v1.0/me`；按文件夹的 `messages/delta`（`Prefer: odata.maxpagesize` 分页、`@odata.nextLink` / `@odata.deltaLink`、删除以 `@removed` 表示）；`/messages/{id}`、`/$value`（MIME 原文）、`PATCH isRead`、`sendMail`。测试接口：`/mock/deliver`（投递新邮件）、`/mock/delete/{id}`、`/mock/expire-tokens`。
- **Gmail IMAP**：应用专用密码登录（`LOGIN` / `AUTHENTICATE PLAIN`）；`[Gmail]/All Mail` 等带 SPECIAL-USE 标记的文件夹；同一封邮件在收件箱和“所有邮件”中 `X-GM-MSGID` 相同；`UID SEARCH SINCE`、`UID FETCH`（含 `X-GM-LABELS`、`BODY.PEEK[]`）、`STORE \Seen`、`APPEND` 草稿、`IDLE`。
- **Gmail SMTP**：`AUTH PLAIN` / `AUTH LOGIN`，发出的邮件和 Gmail 一样自动存入“已发送”。

8 个测试走完整流程（PKCE 登录、分页增量同步、新邮件与删除、下载原文、发信、令牌过期后刷新；IMAP / SMTP 原始对话）。另用 curl 自带的 IMAP/SMTP 客户端验证过：列文件夹、按日期搜索、抓取邮件、发信后出现在“已发送”、错误密码被拒绝。模拟服务为明文连接，仅供本机开发；真实 Gmail 使用 TLS。

**M2 待做**：

1. **Outlook 收信客户端**（`crates/mail-sync`）——**已对着模拟服务完成**：
   - `oauth.rs`：授权码 + PKCE 登录；在回环地址随机端口监听回调，用系统浏览器打开授权页；校验 `state`，防止伪造回调；刷新令牌。
   - `outlook.rs`：`/me`、按文件夹增量同步（跟随 nextLink，保存 deltaLink）、下载原文；访问令牌过期或返回 401 时自动刷新并重试一次。
   - `maildir.rs`：把收件箱、已发送、草稿同步到 `.eml` 目录，删除的邮件移除对应文件，deltaLink 存在目录里的 `sync-state.json`，每个文件夹完成后就保存，中断后可继续。
   - 命令：`letterdesk sync outlook --base http://127.0.0.1:8901 --client-id <ID> --out mail/`（不填 `--base` 时连接真实的微软服务；`--headless-login` 只能用于模拟服务）。之后 `letterdesk-desktop --mailbox mail/ --me <地址>` 即可浏览。
   - 测试：完整流程（浏览器登录 → 首次同步 → 无变化 → 新邮件与删除 → 令牌过期后自动刷新）和伪造回调被拒绝；另用真实命令跑通“模拟服务 → 同步 → 桌面界面”。
   - **还差**：令牌目前只在内存中，每次运行都要重新登录；需要存入系统凭据管理器（`keyring`，Linux 需要 Secret Service）。连接真实账号需要你在 Azure 门户注册一个“公共客户端/本机”应用（允许个人 Microsoft 账号和组织账号，回调 `http://localhost`，委托权限 `Mail.ReadWrite`、`Mail.Send`、`offline_access`、`User.Read`），把**应用（客户端）ID** 给我；客户端 ID 不是机密。HTTPS 目前使用 ureq 内置的 Mozilla 根证书，计划改为系统证书库（企业网络常有自定义根证书）。
2. **Gmail 收信客户端**：IMAP 同步最近 90 天，按 `X-GM-MSGID` 去重、按 SPECIAL-USE 识别文件夹；SMTP 发信（不再另存“已发送”）。可以先对着模拟服务开发；接真实 Gmail 需要开了两步验证的测试账号和能访问 Gmail 的网络（真实服务需 TLS）。
3. **本地存储**（`mail-store`）：SQLite 保存邮件和同步状态（Graph 的 deltaLink、IMAP 的 UIDVALIDITY/UID），tantivy 全文搜索。
4. ~~桌面界面：对话列表与时间线~~ 已完成。

### 下一步

对着模拟服务做 **M2 待做第 2 项（Gmail 收信客户端：IMAP 同步 + SMTP 发信）**，同样输出到 `.eml` 目录；然后做第 3 项（本地存储），并把 Outlook 的令牌存入系统凭据管理器。M1 剩下的待做事项需要 Windows / macOS 真机；M0 未完成的三项可在具备条件的环境中随时补做。

### 已知问题

- 交互式 `reply` 的选项编号由模型给出（`a`、`b`…）；输入恰好等于某个选项编号的自写文字会被当作选项。
- 改写时模型偶尔会润色未改变的句子；修改对比会把这些改动显示出来，由用户决定是否采用。

## 代码结构

| 目录 | 内容 |
|---|---|
| `crates/mail-core` | 解析邮件、去掉引用历史、按“回复所有人”规则确定收件人 |
| `crates/conversation` | 对话归组：按直接参与人组织对话，对话内按话题分线程 |
| `crates/ai` | 提示词、输出校验、修改对比，以及在安全限制下调用本机 CLI |
| `crates/cli` | `letterdesk` 命令行工具 |
| `crates/desktop` | `letterdesk-desktop` 桌面界面原型（Dioxus） |
| `crates/mock-servers` | `letterdesk-mock`：本地模拟的微软登录 + Graph 与 Gmail IMAP/SMTP |
| `crates/mail-sync` | 收信同步：微软登录（PKCE）与 Graph 增量同步到 `.eml` 目录 |
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
cargo run -p letterdesk-desktop -- --mailbox samples --me lin@example.com   # 按人浏览 samples/ 中的全部邮件
cargo run -p letterdesk-desktop                                   # 只打开内置的一封样例
cargo run -p letterdesk-desktop -- --email samples/maya-meeting.eml --backend claude --my-name Lin
cargo run -p letterdesk-desktop -- --selftest                     # 隔离与清洗自检，全部通过时退出码为 0
cargo run -p letterdesk-desktop -- --email samples/maya-meeting.eml --autopilot   # 自动演示 AI 流程
```

## 模拟服务

```sh
cargo run -p mock-servers --bin letterdesk-mock -- --mailbox samples --me lin@example.com
```

启动后会打印各服务地址、模拟的应用（客户端）ID 和 Gmail 应用专用密码。完整演示（另开一个终端）：

```sh
cargo run -p letterdesk-cli -- sync outlook --base http://127.0.0.1:8901 \
  --client-id 00000000-1e77-4de5-8000-000000000001 --out mail/   # 浏览器里会自动完成模拟登录
cargo run -p letterdesk-desktop -- --mailbox mail/ --me lin@example.com
```

例如用 curl 试 IMAP：`curl imap://127.0.0.1:1143/INBOX -u "lin@example.com:abcd efgh ijkl mnop" -X "UID SEARCH ALL"`；投递一封新邮件：`curl --data-binary @samples/receipt.eml http://127.0.0.1:8901/mock/deliver`。

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
