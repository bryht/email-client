# AI 邮件客户端 · 产品与技术计划（草案）

> 状态：**计划 / 讨论阶段**，尚未开始实现。本文记录目前的 brainstorm 结论和待定问题，后续讨论会持续更新。

## 1. 一句话定位

一个用 **Rust** 写的跨平台桌面邮件客户端，主打 **"自动" 的 AI 辅助**，尤其帮助**非英语母语者快速读懂和回复英文邮件**；以**远低于同类产品的价格**获取用户。

### 要解决的痛点

- 回复英文邮件很慢：读得慢、写得慢、担心语气不地道、担心漏答对方的问题。
- 现有邮件客户端的 AI 支持弱，或者价格很贵（如 Superhuman 约 $30/月）。
- 传统邮件按"主题"组织，和人沟通的上下文被拆得很散。

## 2. 已确定的方向

| 维度 | 决定 |
|---|---|
| 邮箱支持 | Gmail、Outlook / Microsoft 365、自建 IMAP |
| 协议底座 | 统一用 **IMAP + SMTP**；Gmail / Outlook 走 OAuth2（XOAUTH2），自建邮箱走密码 / 应用专用密码 |
| 界面形态 | **桌面 GUI**（要给普通用户用，且必须能渲染 HTML 邮件），不做终端 TUI |
| 开发语言 | 尽量纯 Rust（前后端） |
| AI | 云端模型为主，支持多家：Anthropic（Claude）、OpenAI 兼容接口、Ollama（本地 + Ollama Cloud） |
| 交互理念 | "自动"：收到邮件 → 自动分析 → 问几个方向 → 自动生成草稿 → 人确认 |
| 浏览方式 | **对话式**：按参与人归组成对话流，而不是按主题 |
| 商业策略 | 低价获客，本地优先、无服务器，降低边际成本 |
| 用户 | 面向公众，不只是自用 |

## 3. 核心功能

### 3.1 "自动" 回信流程

```
新邮件到达
  → ① 分拣（便宜小模型）：需要回复？通知 / 营销 / 真人来信？
  → ② 需要回复的：后台预先分析，提取"决策点"
  → 用户打开邮件时，方向卡片已准备好（无感等待）
  → ③ 用户点选方向 / 用中文补一句话
  → ④ 流式生成英文草稿 + 中文回译
  → ⑤ 用户确认 → 发送 / 存为草稿
```

**问"决策点"，而不是泛泛地问语气。** 示例：

> 对方问了 2 件事：
> 1. 周四下午 3 点开会可以吗？ `[可以]` `[不行，改时间]` `[待定]`
> 2. 报价能否降 10%？ `[接受]` `[最多 5%]` `[不降]`
>
> 还有要补充的吗？（中文即可）`______`
>
> 语气：`正式` · `友好` · `简短`

点两下就能得到一封逐条回应、语气得体的英文回信。

**原则：AI 永远不自动发送**，只生成草稿，人确认后才发出。

### 3.2 英文邮件副驾驶

1. **对照阅读**：英文来信旁显示中文翻译 + 三行摘要。
2. **中文意图 → 英文邮件**：输入"周四不行，改下周二，合同我会提前发"，输出地道英文邮件。
3. **一键改语气**：更正式 / 更随意 / 更委婉 / 更坚定 / 更简短。
4. **中文回译核对**：生成的英文下附一行中文回译，不用逐字读英文就能确认意思没偏。
5. **润色自己写的英文**：保留原意，只修语法和措辞，并标出改了什么、为什么。
6. **常用场景模板**：催款、跟进、婉拒、约会议、致歉……支持变量和 AI 填空槽（如 `{{客户名}}`、`{{ai: 根据上一封写一句寒暄}}`）。
7. **风格记忆**：从已发邮件中学习用户的称呼、签名、写信习惯（检索相似邮件做 few-shot）。

### 3.3 从邮件中提取结构化信息

用结构化输出（JSON schema）提取：

- **日程** → 加入 Google Calendar / Outlook Calendar，或生成通用 `.ics`（Google Calendar 权限不是"受限"级别，审核比 Gmail 简单）
- **待办与截止日期**
- **航班 / 酒店 / 快递单号** → 行程卡片
- **账单与金额**
- **附件处理提醒**（如需要签字的合同）

日历集成计划放在后期版本。

### 3.4 对话式浏览

参考产品：Spike（聊天式邮件）、Delta Chat（基于邮件协议的聊天应用）。

**两层数据模型：**

```
Conversation（对话流，视图层，按参与人归组）
  └── Thread（话题，标准邮件线程，依据 References / In-Reply-To）
        └── Message（单封邮件）
```

**归属规则（初稿）：**

1. **是回复**（In-Reply-To 指向已知邮件）**且不是转发** → 跟随父邮件进入同一对话流；中途 CC 新的人也不拆分，对话流成员只增不减。
2. **是转发**（主题含 `Fwd:` / `FW:` / `转发:`，或正文有转发标记）→ 新开对话流，保留"来自某对话"的链接。
3. **是新邮件**（非回复）→ 按核心参与人集合（发件人 + 收件人 + CC，去掉自己）查找已有对话流；完全匹配则并入，否则新建。

**需要处理的问题：**

- **身份合并**：同一个人多个地址；用户自己多个账号 / 别名；大小写与 `+tag` 归一化。
- **同一流内话题混杂**：用主题分隔条 / 话题标签区分，支持按话题筛选。
- **AI 上下文按 Thread 取，不按整个 Conversation 取**，避免话题串味和浪费 token。
- **在流里回复时回哪一封**：默认最新一封，并正确设置 In-Reply-To / References，让对方在 Outlook / Gmail 中看到的仍是正常线程。对话视图只是本地展示方式，协议层完全标准。
- **去除引用和签名**（"On … wrote:"、签名、免责声明）：对话视图的体验核心和技术难点，先用规则处理，AI 兜底。
- **长篇正式邮件**：不塞进气泡，显示为卡片（一行 AI 摘要，点开看全文）。
- **非对话邮件**：邮件列表、通知、`noreply` 地址进入单独的"信息流 / 通知"区。
- **大群邮件**（如超过 8 人）：用类似群聊 / 频道的样式。

## 4. 技术方案

### 4.1 GUI 框架选型

硬需求：**渲染 HTML 邮件** + **富文本写信框**。

| 框架 | 渲染方式 | HTML 邮件 | 富文本编辑 | 备注 |
|---|---|---|---|---|
| **Dioxus** | 系统 WebView，写法类似 React | ✅ 天然支持 | 可做（contenteditable） | **首选**：全 Rust 代码，保留 Web 渲染能力 |
| **Tauri + Leptos** | WebView + WASM 前端 | ✅ | 同上 | **备选**：打包、自动更新、插件生态更完善 |
| GPUI（Zed） | GPU 自绘 | ❌ 需嵌入 WebView | 强 | 性能和质感最好，但 HTML 邮件是硬伤 |
| iced | GPU 自绘 | ❌ | 弱 | COSMIC 桌面在用 |
| Slint | 自绘 | ❌ | 弱 | 偏嵌入式，注意授权条款 |
| egui | 即时模式 | ❌ | 弱 | 适合工具类，不适合消费级产品 |

对话流界面（气泡 + 卡片）用 HTML/CSS 实现最自然，这也是选 WebView 类方案的理由。

### 4.2 Cargo workspace 结构（草图）

```
email-client/
├── crates/
│   ├── mail-core/     # 领域模型：Account, Message, Thread, Conversation, Draft
│   ├── mail-sync/     # IMAP 同步 (async-imap)，IDLE 推送，增量同步
│   ├── mail-store/    # SQLite (sqlx) + 全文搜索 (tantivy)
│   ├── mail-send/     # SMTP 发信 (lettre)
│   ├── mail-auth/     # OAuth2 (Gmail / Outlook)，密钥存储 (keyring)
│   ├── conversation/  # 按参与人归组、身份合并、引用 / 签名剥离
│   ├── ai/            # 统一的 AI 接口 (trait)、多家适配器、流式输出
│   ├── extract/       # 结构化提取：日程、待办、行程、账单
│   ├── templates/     # minijinja 模板 + AI 填空槽
│   └── app/           # Dioxus 桌面应用
```

**主要依赖：** `tokio`、`async-imap`、`mail-parser`、`lettre`、`sqlx`、`tantivy`、`reqwest` + SSE 解析、`oauth2`、`keyring`、`minijinja`、`dioxus`。

### 4.3 AI 层

```rust
pub enum ProviderKind {
    Anthropic,        // Claude Messages API
    OpenAiCompatible, // OpenAI / DeepSeek / OpenRouter / 通义 / Kimi …（填 base_url）
    Ollama,           // 本地 localhost:11434 或 Ollama Cloud（同一协议 + API key）
}

#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn complete_stream(
        &self,
        req: CompletionRequest,
    ) -> Result<BoxStream<'static, Result<String>>>;
}

pub enum Task {
    Triage        { message: Message },
    DecisionPoints{ thread: Thread },
    DraftReply    { thread: Thread, choices: Vec<Choice>, note: Option<String>, tone: Tone },
    ExpandIntent  { thread: Option<Thread>, intent_zh: String },
    Rewrite       { text: String, instruction: String },
    Translate     { text: String, target: Lang },
    Extract       { message: Message },
}
```

- **用户自带 API key（BYOK）**：存于系统钥匙串，不经过我们的服务器。
- **按任务选模型**：分拣、提取用便宜快速的模型；正文生成用更强的模型。
- **一律流式输出**。

### 4.4 隐私与安全

- 本地优先：邮件数据只存在用户电脑上。
- 自动分析可以关闭，并可按联系人 / 文件夹设置规则；敏感对象可指定只用本地模型。
- **提示注入防护**：邮件正文是不可信内容，AI 只输出文本，不给它发信 / 转发等工具权限。
- OAuth token、API key 存系统钥匙串。

## 5. 认证与平台审核

| 服务商 | 方式 | 注意事项 |
|---|---|---|
| Gmail | OAuth2 XOAUTH2，scope `https://mail.google.com/` | 属于**受限 scope**：未验证应用最多 100 个测试用户；公开发布需 Google 验证 + **CASA 安全评估（每年一次，有成本）** |
| Outlook / M365 | Azure 应用注册，`IMAP.AccessAsUser.All`、`SMTP.Send`、`offline_access` | 已禁用普通密码登录；部分企业租户需要管理员同意 |
| 自建 IMAP | 用户名 + 密码 / 应用专用密码 | 最简单 |

**Gmail 早期策略：** 测试用户名额 + 应用专用密码（需开两步验证）+ 允许高级用户填自己的 OAuth Client ID；用户量起来后再做 CASA。

## 6. 商业模式（初步）

- **免费 / 极低价 + 自带 key**：AI 费用由用户承担；用 Ollama Cloud、DeepSeek 等便宜模型，每月成本可能只有几块钱。
- **低价订阅 + 托管 AI 额度**：面向不想折腾 key 的普通用户，需要严格控制 token 成本。
- 定价目标：同类产品的 1/5 ~ 1/10。
- **成本控制**：只对真人来信做自动分析，跳过通知 / 营销 / 群发；分拣用小模型。
- **风险**：Gmail CASA 年度成本需要一定付费用户规模才能摊薄 → 早期可先主推 Outlook + 自建 IMAP。

## 7. 路线图（草案）

| 版本 | 内容 |
|---|---|
| v0.1 | workspace 骨架；IMAP 只读同步（密码登录）；SQLite 缓存；`ai` crate 三个适配器；命令行原型跑通"读线程 → 决策点 → 英文草稿"，打磨 prompt |
| v0.2 | Dioxus 桌面界面：对话流视图、HTML 邮件阅读、对照翻译、写信面板（中文意图 → 英文，流式） |
| v0.3 | 自动流程（分拣 + 预生成方向卡片）；Outlook OAuth；Gmail OAuth（测试用户）；SMTP 发送 / 写回草稿箱 |
| v0.4 | 模板、风格记忆、改语气、多账号 |
| v0.5+ | 结构化提取 + 日历集成；打包发布、自动更新；托管 AI 额度与付费 |

## 8. 待讨论问题

1. **自动分析范围**：所有真人来信都分析，还是只分析英文邮件？（影响成本和隐私默认值）
2. **转发边界**：A 把 B 的邮件转发给 C，C 回复时又抄送了 B，这封邮件回到原来的流，还是留在新流？
3. **定价模式**：免费自带 key + 低价托管额度的组合，还是一次性买断？具体价位？
4. **平台优先级**：先做 macOS 还是 Windows？（打包、签名、测试精力先集中在一个平台）
5. **最终 GUI 框架**：Dioxus 还是 Tauri + Leptos？需要做一个小原型验证富文本编辑和 HTML 邮件渲染。
6. **竞品差异化**：主打"非英语母语者的英文邮件"，还是更泛的 AI 邮件客户端？
7. **对话流里的写信体验**：像聊天一样短输入框，还是展开为完整写信窗口？两者如何切换？
