# email-client

Letterdesk（暂名）：用 Rust 编写的开源桌面邮件客户端，按人浏览往来，并用 AI 辅助阅读和回复邮件。方案见 [docs/PLAN.md](docs/PLAN.md)。

当前进度：第一步 **AI 命令行原型**。还没有图形界面，也不连接真实邮箱；它读取 `.eml` 文件，通过本机安装的 Claude Code、Ollama（或实验性的 Codex）找出来信里需要你决定的问题，按你的选择生成回信草稿，并在你修改决定后整篇改写、显示修改对比。

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
