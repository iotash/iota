# 别的进程给我的当前会话发消息：业界做法调研与 iota 的方案建议

Status: **Research + 建议** · 日期：2026-10-21 · 基于 `main` = `2929ed4`（v0.7.4 之后）· 分支 `agent-messaging` · 本机：herdr 0.9.5、Claude Code 2.1.306、codex-cli 0.162.1、tmux 3.8、rustc 1.98.0；iota 的 MCP 客户端是 rmcp 3.1.4

**后续（2026-10-22）**：所有者选了 §4.2 B（socket），不走 inbox；v1 已实现，规格与实现见 [`agent-messaging.md`](agent-messaging.md)。与本文的出入：socket 本体在 `~/.iota/run/<pid>.sock`，`<bundle>/sock` 是指向它的链接——§5 第 16 条 (b) 的路径长度实测后不成立（本 worktree 下 110 字节，超过 macOS 的 104）。下文保持调研时的原样。

本文只做调研和设计建议，不改代码。所有者的原话：「我想要调研一下，现在有没有业界比较通用的 agent 通信的方式？比如我的当前 session，其它进程可以向我发送消息。」

## 写作约定

- 每条结论附出处：规范/文档 URL 加版本或日期，`file:line`（iota 源码，按 `2929ed4`），或本机命令加输出。规范原文用英文短句引用，不意译成结论。查不到写「未证实」；从代码推出、没有原文或实测支撑的，标「推断」。
- 以下页面由撰写者本人对着原文复核过：MCP `2025-11-25` 的 elicitation、sampling、transports 三页；MCP `2026-07-28` 的 changelog 与 stdio 两页；Claude Code 的 channels、channels-reference、mcp（"Push messages with channels" 一节）、cross-session-messaging 四页；Codex 的 app-server 页；opencode 仓库的 `packages/core/src/session/inbox.ts`；rmcp 3.1.4 的 `handler/client.rs`、`model/capabilities.rs`；herdr 0.9.5 的 `agent prompt --help` 与 `agent send-keys --help`；第三部分引用的全部 iota 源码。
- 其余出处来自两个调研 agent（协议组、CLI 组）的笔记，每条都带 URL，但没有逐条复核；它们的「未证实」原样保留在第五部分。
- 「注入」在本文里的定义：外部进程把一段文字送进一个**正在运行**的 agent 会话，并且它**成为模型的输入**。只改 UI、只写日志、只在下次启动时带上，都不算。

---

## 0. 一页结论

1. **业界没有一个跨厂商的「本地进程 → 本地 agent 会话」标准。** 四个主要协议各管一段，而且都不是这一段：MCP 管 agent ↔ 工具（新版 2026-07-28 还把 server 主动说话的门关了，§1.1）；A2A 管 agent ↔ agent（跨网络的 task 模型，允许向未完成的 task 再送一条但没定忙时怎么办，§1.2）；ACP 管 agent ↔ 编辑器（v1 一 prompt 一轮，v2 alpha 在改成「插入一条消息」，§1.3）；AG-UI 管 agent ↔ UI（只能开新 run，§1.4）。把「运行中再来一条」写成规则的只有 LangSmith Agent Server 的 `multitask_strategy`（reject / enqueue / interrupt / rollback，§1.5）。
2. **但各家 CLI 的做法在收敛成两个事实标准**（§2）：
   - **每个会话一个本地 socket，一行 JSON 一条消息**：Claude Code 跨会话消息（2.1.224+，`CLAUDE_CODE_MESSAGING_SOCKET`，脚本和 hook 可以直接写）、Codex app-server（`unix://` + `turn/steer`）、OpenClaw gateway（WebSocket `chat.send`）、opencode（HTTP + inbox）。
   - **一个 MCP stdio server 推 `notifications/claude/channel`**：Claude Code channels（research preview），crush 已兼容；靠它接 Telegram / Discord / iMessage / webhook。
   - 忙时语义收敛到「**不打断正在跑的工具，在工具/步边界插入；可选等到空闲**」（Codex steer、opencode `steer|queue`、Amp、pi、OpenClaw、Claude Code「between tool calls」）；身份收敛到「**标明不是用户说的，不能批权限，命令当纯文本**」（Claude Code `<channel source=…>` 与「from another session」、OpenClaw「inter-session data」）。
3. **iota 已经有这条路的八成**（§3）：后台任务完成的通知走的就是 `Ui::enqueue(Input{kind: Notice})`——空闲时立刻唤醒循环，忙时排队、在 round 边界进入、永不打断 tool round，到达模型是 `Role::User` 但不回显、不进历史、不走 slash 派发（`src/repl/run.rs:689-697`、`src/repl/turn/steer.rs:45-70`、`src/provider/model.rs:312`）。缺的只是**进程外的入口**：今天能到达一个运行中 iota 的只有键盘仿真（herdr `agent prompt`、tmux `send-keys`），而且分不清是不是用户说的。
4. **建议（§4）：该做，但小做，而且分两层。**
   - **v1：每个运行中的会话绑一个 Unix socket，一行 JSON 进 `ui.enqueue(Notice)`**；路径导出给子进程（`IOTA_SOCKET`）并随 session 报告给 herdr；发送端**不加新动词**——`iota run <bot> -m "…"` / `iota resume <id> -m "…"` 发现 bundle 被一个活着的进程持有时，改为投递并等一行回执（Claude Code 2.1.285+ 对后台会话的 `--resume <id> "prompt"` 正是这个行为）。这和 bot-mode.md L3 已定的「inbox 目录先行、socket 只在要回复时再做」**相抵触**；理由是回执、对端身份和「herdr / 另一个 iota / 脚本走同一条路」三件事 inbox 都给不了（§4.3）。改不改那条决定，由所有者定。
   - **L4：`claude/channel` 作为 iota 的 MCP 客户端能力**，接 IM 与 webhook。rmcp 3.1.4 已把未知通知分发到 `on_custom_notification`，默认握手就是 channel 契约所依赖的 `initialize` 版（§4.2）。现在不做：research preview、契约可能变、所有者问的是本地进程。
   - **不做**：复用 MCP elicitation / sampling（语义是「server 向用户要表单 / 借模型」，不是送消息；2026-07-28 已改 MRTR 并弃用 sampling；iota 的 D-33 已明确不接 MRTR）；A2A / ACP server；TCP 监听。
   - **更简单的替代**：人在另一个 pane 里戳一下 → herdr `agent prompt`，今天就能用；「等某个进程的结果」→ 让模型用 `background: true` 的 shell job 等它，结果会以 `[background job …]` 回来，零代码。**如果需求只是这两种，现在不用做。**

---

## 1. 协议与标准

先把「段」分清。所有者问的是「我的当前 session，其它进程可以向我发送消息」——这是 **进程 ↔ 正在运行的 agent 会话** 这一段。下面每个协议都先答它解决哪一段，再答它有没有这一段的能力。

### 1.1 MCP（Model Context Protocol）——agent/host ↔ 工具；反方向在新版里被收窄了

**版本**：当前稳定版 **2026-07-28**（RC 2026-05-21）；上一版 **2025-11-25**；更早 2025-06-18、2025-03-26、2024-11-05。治理归 Linux Foundation 下的 Agentic AI Foundation（AAIF，2025-12-09 成立）。draft 的 changelog 目前为空，没有新的反向能力。【官方】[versioning](https://modelcontextprotocol.io/specification/versioning) · [2026-07-28 changelog](https://modelcontextprotocol.io/specification/2026-07-28/changelog) · [LF 新闻稿](https://www.linuxfoundation.org/press/linux-foundation-announces-the-formation-of-the-agentic-ai-foundation)

**两版的「反方向」差别是本题的核心**，要分开说：

| 能力 | 2025-11-25 | 2026-07-28 |
|---|---|---|
| server → client 独立请求（`sampling/createMessage`、`elicitation/create`、`roots/list`） | 是 JSON-RPC request，server 可随时发；stdio 上直接写 stdout，HTTP 上可走 client 开的 **GET 独立流**（"The server **MAY** send JSON-RPC *requests* and *notifications* on the stream. These messages **SHOULD** be unrelated to any concurrently-running JSON-RPC *request* from the client."） | **取消**。改为 MRTR（SEP-2322）：server 在 `tools/call` / `prompts/get` / `resources/read` 的结果里返回 `resultType: "input_required"`，client 处理完**重发原请求**带 `inputResponses`。stdio 原文："The server **MUST NOT** write JSON-RPC *requests* to `stdout`." |
| client 能力声明 | `initialize` 时一次性声明 `sampling` / `elicitation` / `roots` | 每个请求在 `_meta.io.modelcontextprotocol/clientCapabilities` 里带；`initialize` 握手删除（SEP-2575），server 必须实现 `server/discover` |
| 变更通知（`tools/list_changed` 等） | server 主动发 | 只在 client 发了 `subscriptions/listen` 并 opt-in 后才发："The server **MUST NOT** send notification types the client has not explicitly requested." |
| 请求作用域通知（`progress`、`message`） | 随请求 | 不变，但只能走该请求自己的响应流 |
| 未经请求的自定义通知（`notifications/xxx`） | 允许（stdio 上就是一行 JSON） | stdio 页列出 server 只能写三类：响应、与 in-flight 请求相关的通知、`subscriptions/listen` 的通知 |
| HTTP 会话 | `Mcp-Session-Id`，GET 流，`Last-Event-ID` 重放 | 全部删除（SEP-2567）；"MCP is a **stateless protocol**"；"an open connection, such as a STDIO process, is not a conversation or session" |
| Sampling / Roots / Logging | 可用 | **弃用**（SEP-2577，12 个月窗口）："integrate directly with LLM provider APIs instead of Sampling" |

【官方】[2025-11-25 transports](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports) · [2025-11-25 elicitation](https://modelcontextprotocol.io/specification/2025-11-25/client/elicitation) · [2025-11-25 sampling](https://modelcontextprotocol.io/specification/2025-11-25/client/sampling) · [2026-07-28 stdio](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/stdio) · [2026-07-28 MRTR](https://modelcontextprotocol.io/specification/2026-07-28/basic/patterns/mrtr) · [2026-07-28 subscriptions](https://modelcontextprotocol.io/specification/2026-07-28/basic/patterns/subscriptions) · [2026-07-28 basic](https://modelcontextprotocol.io/specification/2026-07-28/basic/index)

**语义与限制（对「往会话里送东西」而言）**：

- `elicitation/create` 不是「送一条消息」，是「在处理某个请求的过程中向用户要一个表单（form）或让用户去开一个 URL（url 模式，2025-11-25 引入）」。client 必须声明 `elicitation` 能力才会收到；client **MUST** 清楚标出是哪个 server 在要、给出 decline / cancel；三种答复 `accept` / `decline` / `cancel`。它的生命周期绑在那次请求上。
- `sampling/createMessage` 是 server 借 client 的模型跑一次**独立的** `messages[]`（server 自带 system prompt、messages、model hints），不是把文字塞进 client 正在进行的对话；规范要求 "there **SHOULD** always be a human in the loop with the ability to deny sampling requests"。2026-07-28 起弃用。
- `notifications/*`：2025-11-25 下 server 可以随时发任意通知（包括自定义方法名），**但规范不定义「对话」**，通知是否进用户正在看的会话完全由 host 决定。2026-07-28 下未经请求的通知不再合法。
- **结论**：MCP 2026-07-28 **没有**「往正在跑的会话里注入一条消息」这一能力（N）；2025-11-25 **部分**有——server 能发，host 爱不爱显示由 host 定。这正是 Claude Code channels 存在的原因（§2.1）。

**鉴权**（HTTP 才有）：MCP server = OAuth 2.1 resource server，client = OAuth 2.1 client；"MCP servers **MUST** implement OAuth 2.0 Protected Resource Metadata (RFC9728)"、"MCP clients **MUST** implement Resource Indicators (RFC 8707)"；2026-07-28 起 DCR（RFC7591）弃用，改 Client ID Metadata Documents。stdio："**SHOULD NOT** follow this specification, and instead retrieve credentials from the environment."【官方】[2026-07-28 authorization](https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization)

**扩展**（都要显式 opt-in，"Extensions are always disabled by default"）：
- **MCP Apps**（`io.modelcontextprotocol/ui`，SEP-1865，Stable，版本 2026-01-26）：工具可带一个 `ui://` HTML 资源，host 在 iframe 里渲染，App ↔ Host 走 postMessage 上的 JSON-RPC。其中 `ui/message`："Host SHOULD add the message to the conversation context, preserving the specified role."——**这是 MCP 体系里唯一一条「往对话里加消息」的规范语句，但方向是 UI iframe → host，不是 server → client**，而且面向 GUI host（Claude Desktop、VS Code、Goose 等）。【官方】[apps.mdx](https://github.com/modelcontextprotocol/ext-apps/blob/main/specification/2026-01-26/apps.mdx) · [overview](https://modelcontextprotocol.io/extensions/apps/overview)
- **Tasks**（`io.modelcontextprotocol/tasks`，SEP-2663）：2025-11-25 是核心里的 experimental，2026-07-28 挪成扩展；`tasks/get` 轮询、`tasks/update` 给输入、`tasks/cancel`；只用于 `tools/call`；"no second connection or unsolicited server-to-client messages required"。即异步任务也是**client 拉**，不是 server 推。【官方】[tasks overview](https://modelcontextprotocol.io/extensions/tasks/overview)

**MCP 固定表**：

| 项 | 结论 |
|---|---|
| 解决哪一段 | agent/host ↔ 工具/数据 |
| 传输 | stdio（行分隔 JSON-RPC）；Streamable HTTP（POST + 按请求的 SSE；2026-07-28 无 GET、无 session、无 resume） |
| 寻址「那个会话」 | 2026-07-28：没有会话，跨请求状态靠显式 handle / `taskId`；2025-11-25：`Mcp-Session-Id` |
| 投递语义 | 请求-响应；请求作用域通知；变更通知需订阅；server 要输入 → `input_required` 让 client 重发 |
| 鉴权 | HTTP：OAuth 2.1 + RFC9728 + RFC8707；stdio：环境变量 |
| 注入正在跑的会话 | **N**（2026-07-28）；**部分**（2025-11-25：server 能发，host 决定是否进对话） |

### 1.2 A2A（Agent2Agent）——agent ↔ agent；有「向未完成的 task 再送一条」，但忙时语义没定

- **谁在推、归谁**：Google 起草并捐给 Linux Foundation；TSC 来自 AWS、Cisco、Google、IBM Research、Microsoft、Salesforce、SAP、ServiceNow；2026-08-27 成为 AAIF 的 Growth Stage 项目。版本 **v1.0.0 2026-03-12**、**v1.0.1 2026-05-28**（当前）；v0.3.0 2025-07-30。【官方】[a2a-protocol.org](https://a2a-protocol.org/latest/) · [releases](https://github.com/a2aproject/A2A/releases) · [LF 新闻稿 2026-04-09](https://linuxfoundation.org/press/a2a-protocol-surpasses-150-organizations-lands-in-major-cloud-platforms-and-sees-enterprise-production-use-in-first-year)
- **模型**：AgentCard、Task、Message、Part、Artifact；`contextId` 分组、`taskId` 标识有状态工作单元。Task 状态机：`INPUT_REQUIRED`、`AUTH_REQUIRED` 是 interrupted；`COMPLETED` / `FAILED` / `CANCELED` / `REJECTED` 是 terminal。"A `SendMessageResponse` MUST contain exactly one of the following: `task`, `message`"。【官方】[specification](https://a2a-protocol.org/latest/specification/)
- **方法**（v1.0 改为 PascalCase，"matching gRPC conventions"）：`SendMessage`、`SendStreamingMessage`、`GetTask`、`ListTasks`、`CancelTask`、`SubscribeToTask`、`*TaskPushNotificationConfig`、`GetExtendedAgentCard`。REST 绑定 `POST /message:send`、`POST /message:stream`、`GET /tasks/{id}`、`POST /tasks/{id}:cancel`、`POST /tasks/{id}:subscribe`。（任务书里写的 `message/send`、`tasks/get` 是 0.2/0.3 的斜杠命名。）
- **传输**：JSON-RPC 2.0 over HTTP(S)、gRPC over HTTP/2 + TLS、HTTP+JSON；流式用 SSE；异步用 webhook push（"Agents MUST attempt delivery at least once for each configured webhook"；client "SHOULD process notifications idempotently"）。每个请求带 `A2A-Version` 头。
- **寻址**：Agent Card（`/.well-known/agent-card.json` 或注册中心）→ `supportedInterfaces[].url`；「那个会话」= `contextId`，「那个任务」= `taskId`。
- **鉴权**：Card 的 `securitySchemes`（API key、HTTP auth、OAuth 2.0、OIDC、mTLS）；"Production deployments MUST use encrypted communication"。
- **能不能往正在跑的 task 里再送一条**：**部分**。`INPUT_REQUIRED` 状态下同 `taskId` 续发是标准路径；对非终态（含 `working`）"Agents MAY accept additional messages for tasks in non-terminal states"——**排队还是打断，规范没定**；终态拒绝。

### 1.3 ACP（Agent Client Protocol，Zed）——agent ↔ 编辑器；v1 不定义并发，v2 在改

- **规范**：[agentclientprotocol.com](https://agentclientprotocol.com)；JSON-RPC over stdio，编辑器把 agent 当子进程。`initialize.protocolVersion` 是整数 MAJOR 版本（现为 `1`）。GitHub Releases 最新 Schema v1.26.0 / Rust crate v1.12.0（2026-10-16），另有 **v2.0.0-alpha.9**。【官方】[architecture](https://agentclientprotocol.com/overview/architecture) · [initialization](https://agentclientprotocol.com/protocol/initialization) · [releases](https://github.com/agentclientprotocol/agent-client-protocol/releases)
- **方法（v1）**：Client → Agent：`initialize`、`authenticate`、`session/new`、`session/prompt`、可选 `session/load`、`session/set_mode`，通知 `session/cancel`；Agent → Client：`session/request_permission`、可选 `fs/*`、`terminal/*`、`elicitation/create`，通知 `session/update`（`user_message_chunk`、`agent_message_chunk`、`tool_call`、`plan`、`notice`、`compaction_*` 等十余种）。【官方】[v1 overview](https://agentclientprotocol.com/protocol/v1/overview) · [schema](https://agentclientprotocol.com/protocol/schema)
- **谁在用**：Clients：Zed、JetBrains、VS Code 扩展、Neovim（CodeCompanion、avante.nvim 等）、Emacs、Obsidian、Sublime、Qt Creator 等；Agents（官方列 43 个）：Gemini CLI、Claude Agent（"via Zed's SDK adapter"）、Codex CLI（via adapter）、OpenCode、Goose、Cline、Cursor、GitHub Copilot、Kiro CLI、OpenHands 等。【官方】[clients](https://agentclientprotocol.com/overview/clients) · [agents](https://agentclientprotocol.com/overview/agents)
- **忙时再送一个 prompt**：
  - v1：只写了 "Once a prompt turn completes, the Client may send another `session/prompt`." 与 `session/cancel` 可随时打断；并发**未定义**。【官方】[prompt-turn](https://agentclientprotocol.com/protocol/prompt-turn)
  - v2（alpha）："`session/prompt` lasts until the Agent inserts the user message into the ACP conversation."（返回 `messageId`，不再等整轮）；`state_update` 报 `running` / `requires_action` / `idle`；"Session updates are not limited to prompt-driven foreground work."；但 "This does not require Agents to support prompts while work is active." RFD 原话 "This RFD does not specify queueing, steering, or whether agents insert new prompts while busy."【官方】[v2 prompt-lifecycle](https://agentclientprotocol.com/protocol/v2/prompt-lifecycle) · [RFD](https://agentclientprotocol.com/rfds/v2/prompt)
- **同名的 IBM/BeeAI ACP（Agent Communication Protocol）**：REST 风格，仓库 2025-08-27 归档，README "ACP is now part of A2A under the Linux Foundation!"。【官方】[i-am-bee/acp](https://github.com/i-am-bee/acp)
- **固定表**：agent ↔ 编辑器 · JSON-RPC/stdio · `sessionId` · v1 一 prompt 一轮、cancel 打断；v2 prompt = 插入消息 · `authenticate` · 注入正在跑的会话：v1 **未定义**，v2 **部分**。

### 1.4 AG-UI（CopilotKit）——agent ↔ UI；只能开新 run

- **规范 1.0**（`MAJOR.MINOR`），CopilotKit 与 LangChain、CrewAI 合作起草；自我定位是三层栈里的 agent ↔ 用户层（MCP 工具、A2A agent 间）。1.0 的发布日期规范页没写，包发布从 2026-10 初起要求 `>=1.0.0`。【官方】[introduction](https://docs.ag-ui.com/introduction) · [spec 1.0](https://docs.ag-ui.com/spec/1.0/index.md) · [releases](https://github.com/ag-ui-protocol/ag-ui/releases)
- **事件**：`RUN_STARTED/FINISHED/ERROR`、`STEP_*`、`TEXT_MESSAGE_START/CONTENT/END`、`TOOL_CALL_START/ARGS/END/RESULT`、`STATE_SNAPSHOT/DELTA`（JSON Patch）、`MESSAGES_SNAPSHOT`、`ACTIVITY_*`、`REASONING_*`、`SUBAGENT_*`、`RAW`、`CUSTOM`。"Every run terminates with either `RunFinished` or `RunError`."【官方】[events](https://docs.ag-ui.com/concepts/events)
- **传输与方向**：HTTP POST `RunAgentInput`（`threadId`、`runId`、`messages`、`tools`、`state`、`resume?`）→ SSE 事件流回来（必选），protobuf 可选；WebSocket 等只是 "MAY carry … over other channels"，无绑定。**用户 → agent 只有 POST 这一条路。**【官方】[transports](https://docs.ag-ui.com/spec/1.0/basic/transports/index.md)
- **中途注入**：**N**。人机回路靠 interrupt/resume："the run ends with an interrupt outcome, and the client starts a new run carrying per-interrupt responses."；同一 thread 并发 run 的规则未定义。【官方】[interrupt-resume](https://docs.ag-ui.com/spec/1.0/basic/patterns/interrupt-resume.md)

### 1.5 框架内的 agent 间通信——真正把「忙时再来一条」写成规则的只有一家

- **LangGraph / LangSmith Agent Server**：开源 LangGraph 里是 `Send`（map-reduce）、`Command`（改状态并路由，multi-agent handoff）、`interrupt()`/resume（同 thread、同 checkpointer，"the runtime restarts the entire node from the beginning"）——都是**轮次边界**。托管的 Agent Server 的 `POST /threads/{thread_id}/runs` 有 `multitask_strategy`，这是本次调研里**唯一**明确定义了「运行中再来一条」四种处理的地方：
  - "Reject … rejects any additional incoming runs while a current run is in progress."
  - "Enqueue … allows the current run to finish before processing any new input."（服务端默认）
  - "Interrupt … halts the current execution and preserves the progress made up to the interruption point."
  - "Rollback … halts the current execution and reverts all progress."
  - "Double texting is a feature of LangSmith Deployment. It is not available in the LangGraph open source framework."
  【官方】[graph-api](https://docs.langchain.com/oss/python/langgraph/graph-api) · [interrupts](https://docs.langchain.com/oss/python/langgraph/interrupts) · [create-background-run](https://docs.langchain.com/langsmith/agent-server-api/thread-runs/create-background-run) · [double-texting](https://docs.langchain.com/langsmith/double-texting)
- **Microsoft AutoGen 0.4 → Microsoft Agent Framework**：AutoGen core 是 actor 模型：`send_message`（同步 RPC，await 返回值）与 `publish_message`（`TopicId` 广播，"cannot be used for request/response"）；寻址 `AgentId(type, key)`；分布式用 gRPC（experimental）。消息层面随时可投，但**没有对话轮次概念**，怎么处理由 handler 定。AutoGen 仓库已归档（"New users should start with Microsoft Agent Framework"），MAF **1.0 GA 2026-04-03**，对外走 MCP + A2A。【官方】[message-and-communication](https://microsoft.github.io/autogen/stable/user-guide/core-user-guide/framework/message-and-communication.html) · [autogen README](https://github.com/microsoft/autogen) · [MAF 1.0](https://devblogs.microsoft.com/agent-framework/microsoft-agent-framework-version-1-0/)
- **CrewAI**：`allow_delegation=True` 给两个工具 `Delegate work to coworker` 与 `Ask question to coworker`，同一轮内的工具调用；文档里没有 mailbox / 消息队列（未审源码）。**N**。【官方】[collaboration](https://docs.crewai.com/en/concepts/collaboration)

### 1.6 AGNTCY / SLIM、NANDA、ANP——消息底座，采用未证实

- **SLIM**（AGNTCY，Cisco 发起，2025-07-29 进 Linux Foundation）："Secure network-level messaging for multi-agent applications with pub/sub, streaming, and MLS encryption."；IETF 草案 **draft-mpsb-agntcy-slim-02**（2026-07-07，Informational，作者全是 Cisco）：gRPC over HTTP/2 与 HTTP/3，unary / 流 / pub/sub / MLS 组，路由节点是零信任中介，命名 `organization/namespace/service/instance`。定位是 A2A 与 MCP 的**传输层**，Rust 实现。SLIM v1.0 2026-02-03；2026-08-11 文章称加了 WebSocket。**有没有人用**：只找到 Orium 一篇外部文章（2026-05-12），无具名生产用户，**未证实**。【官方】[LF 新闻稿](https://www.linuxfoundation.org/press/linux-foundation-welcomes-the-agntcy-project-to-standardize-open-multi-agent-system-infrastructure-and-break-down-ai-agent-silos) · [draft-02](https://www.ietf.org/archive/id/draft-mpsb-agntcy-slim-02.html) · [agntcy/slim](https://github.com/agntcy/slim)
- **NANDA**（MIT Media Lab）：官方页抓取失败（证书错误），只有第三方材料；注册表 + AgentFacts；**无采用数据**。
- **ANP**（Agent Network Protocol）：W3C DID 身份层，ANP 1.2，README "Used by" 为空，IETF 个人草案已过期；**无采用证据**。【官方】[AgentNetworkProtocol](https://github.com/agent-network-protocol/AgentNetworkProtocol)

### 1.7 顺带：OpenAI handoff 与 background mode

- Agents SDK handoff："Handoffs are represented as tools to the LLM."、"Handoffs stay within a single run."——同一 run 内换 agent，不是跨进程消息。【官方】[handoffs](https://openai.github.io/openai-agents-python/handoffs/)
- Responses API background：`background: true` 后只能 poll / cancel / 续流（按 `sequence_number`），**没有「向运行中的 response 追加消息」**。【官方】[background](https://developers.openai.com/api/docs/guides/background.md)

### 1.8 横向对照表

| 协议 | 解决哪一段 | 传输 | 寻址「那个会话」 | 投递语义 | 鉴权 | 注入正在跑的会话 |
|---|---|---|---|---|---|---|
| MCP 2026-07-28 | agent/host ↔ 工具 | stdio；Streamable HTTP（POST + 按请求 SSE） | 无会话；显式 handle / `taskId` | 请求-响应；请求作用域通知；MRTR；订阅流只发 opt-in 变更 | OAuth 2.1（HTTP）/ 环境变量（stdio） | **N** |
| MCP 2025-11-25 | 同上 | stdio；Streamable HTTP（+GET 流、Session-Id、Last-Event-ID） | `Mcp-Session-Id` | 同上 + server 可在 GET 流 / stdout 发独立请求与通知 | 同上 | **部分**（host 决定是否进对话） |
| Claude Code channels（MCP 之上的事实标准，§2.1） | 进程 ↔ 正在跑的 agent 会话 | stdio MCP server 发 `notifications/claude/channel` | 启动时 `--channels` 点名 | 排队，忙时下一轮合并投递 | 配对码 + 发送者白名单 | **Y** |
| A2A 1.0.1 | agent ↔ agent | JSON-RPC/HTTP、gRPC、HTTP+JSON；SSE；webhook | Agent Card → `contextId` + `taskId` | task 状态机；`INPUT_REQUIRED` 续发；非终态续发 MAY | Card `securitySchemes` | **部分**（working 态语义未定） |
| ACP v1（Zed） | agent ↔ 编辑器 | JSON-RPC/stdio | `sessionId` | 一 prompt 一轮；`session/cancel` | `authenticate` | **未定义** |
| ACP v2（alpha） | 同上 | 同上 | `sessionId` + `messageId` | prompt = 插入消息；agent 随时 `session/update` | 同上 | **部分**（忙时接 prompt 不强制） |
| AG-UI 1.0 | agent ↔ UI | HTTP POST + SSE | `threadId` + `runId` | 一 POST 一 run；interrupt → 新 run 带 `resume` | 不定义 | **N** |
| LangGraph OSS | 图内 agent ↔ agent | 进程内 | `thread_id` | Send/Command；interrupt → 同 thread resume | 无 | **N**（轮次边界） |
| LangSmith Agent Server | UI/服务 ↔ 图 | HTTP | `thread_id` + `run_id` | `multitask_strategy`: reject / enqueue（默认）/ interrupt / rollback | 服务端 auth | **Y** |
| AutoGen 0.4 core（已归档） | agent ↔ agent | 进程内；gRPC | `AgentId(type,key)`、`TopicId` | send_message RPC；publish_message 广播 | 无 | **Y**（消息层；无轮次概念） |
| CrewAI | agent ↔ agent | 进程内工具调用 | coworker 名 | delegate / ask | 无 | **N** |
| SLIM | A2A/MCP 的消息底座 | gRPC/HTTP2、HTTP3 | `org/ns/service/instance` | request/reply、pub/sub、流、MLS 组 | DID + MLS + TLS | **Y**（消息层；会话语义归上层） |

**读表的结论**：真正定义了「向正在跑的会话送一条、忙时怎么办」的，只有 **LangSmith Agent Server 的 `multitask_strategy`** 和 **Claude Code channels 的「排队、下一轮合并」**；A2A 与 ACP v2 都允许但没说忙时怎么办；MCP 新版反而把 server 主动说话的门关了。没有一个跨厂商的「本地进程 → 本地 agent 会话」标准；事实标准是 `claude/channel`（crush 已兼容，§2.6）。

## 2. 各家 agent CLI 怎么让外部进程跟「正在运行的会话」说话

每家固定回答七件事：接口、寻址、**运行中能否注入**、到达模型的身份、忙时语义、鉴权、出处。本节的 Claude Code channels / channels-reference / mcp / cross-session-messaging、Codex app-server、opencode `inbox.ts` 六处由撰写者复核过原文，其余来自 CLI 组的笔记。

### 2.1 Claude Code——同一个产品里有四条路，两条身份可区分

**(a) Channels（research preview）**——外部事件 → 运行中会话的正式路。
- **接口**：一个 **stdio** MCP server（Claude Code 以子进程拉起），`initialize` 时声明 `capabilities.experimental['claude/channel']: {}`，之后发 **MCP notification** `notifications/claude/channel`，参数 `content: string` 与 `meta: Record<string,string>`（每个键变成标签属性；键只许字母数字下划线，否则静默丢弃）。原文："Your server needs to: 1. Declare the `claude/channel` capability so Claude Code registers a notification listener 2. Emit `notifications/claude/channel` events when something happens 3. Connect over stdio transport"。
- **启用**：`claude --channels plugin:<name>@<marketplace>`；在 `.mcp.json` 里有不够："Being in `.mcp.json` isn't enough to push messages: a server also has to be named in `--channels`." 自建的要走 `--dangerously-load-development-channels server:<name>`，且 `-p` 与 Agent SDK 下忽略该标志。
- **身份**：`<channel source="webhook" path="/" method="POST">…</channel>` 块进上下文，终端只画一行 `← webhook: build failed on main …`；server 的 `instructions` 在连接时交给模型，说明事件格式与回复方式。
- **忙时语义**：**排队，下一轮一起投递**。原文："Events queue into the session and are processed in order. If several notifications arrive while Claude is busy, they're delivered together on the next turn and Claude handles them as a group." **无 ACK**："Claude Code doesn't acknowledge notifications. The `await` on `mcp.notification()` resolves when the message is written to the transport, not when Claude has processed it."；未注册或被策略拦下时静默丢弃。
- **寻址**：没有会话 id——channel server 是该会话的子进程，天然只属于这一个会话；外部系统寻址的是 channel server 自己开的端口或平台 bot。
- **鉴权**：server 自管发送者白名单（Telegram / Discord 配对码；iMessage 自己给自己发免检）；"An ungated channel is a prompt injection vector."；组织侧 `channelsEnabled` / `allowedChannelPlugins`。权限中继（可选）：server 声明 `claude/channel/permission` 后收 `notifications/claude/channel/permission_request`，回 `notifications/claude/channel/permission { request_id, behavior: allow|deny }`。
- **协议版本坑（和 §1.1 直接相关）**："On the v2 runtime, a channel server that negotiates MCP protocol revision 2026-07-28 can't deliver channel messages, so Claude Code doesn't register it as a channel. A channel server that doesn't support that revision connects on the earlier handshake and registers as before."——即 channel 契约依赖 2025-11-25 及更早的「server 可以主动写通知」模型。
- 【官方】[channels](https://code.claude.com/docs/en/channels) · [channels-reference](https://code.claude.com/docs/en/channels-reference) · [mcp#push-messages-with-channels](https://code.claude.com/docs/en/mcp)

**(b) 跨会话消息（cross-session messaging，v2.1.224+；Windows v2.1.234+；默认开启）**——离所有者的问法最近的一条。
- **接口 / 寻址**：每个会话绑一个 **Unix domain socket**（Windows 命名管道），路径在 `/status` 的 `Peer address` 行（`uds:` 前缀），并导出为环境变量 `CLAUDE_CODE_MESSAGING_SOCKET`（`SessionStart` hook 之前就有）；目录不可接受时退到 `/tmp/cc-socks-<uid>`。模型用 `ListAgents` 发现、`SendMessage` 投递；用户可以 `@名字`。`claude -p` 会话也绑 socket（"a long-running `-p` worker can receive messages"），`--bare` 不绑。
- **外部进程能否写**：**能，文档专门给了口子**："Read this section when … you want a script or hook to post into a session"。首行可以是 `{"type":"auth","token":"<CLAUDE_CODE_MESSAGING_TOKEN>"}`（macOS/Linux 可选，Windows 必须）；30 秒内要发出完整一行。**消息行本身的 JSON 格式文档没给**（未证实）。
- **忙时语义**："The receiving Claude reads the message between tool calls during an active turn, so a running tool is never interrupted. When the receiving session is idle, Claude Code starts a new turn with the message."
- **身份**："Claude Code tells B's Claude that the message came from another session, not from you"；不能批权限（"a message from another session never counts as your consent"）、不能改配置、`/compact` 之类命令当纯文本；终端画一行 `› Message from @api-worker: …`。
- **流控 / 鉴权**：socket 限本 OS 用户；`crossSessionInbound: accept | hold | refuse`；没设时按双方权限模式决定——接收方是 bypassPermissions 时默认 **hold** 等人批（`dialogExpiry` 默认 5 分钟）；own-child（hook / Bash 子进程写回自己会话）按进程证据或 token 放行；最多 hold 100 条、队列 50 条、单条约一百万字符、burst 拒收、重复去重。
- 【官方】[cross-session-messaging](https://code.claude.com/docs/en/cross-session-messaging)

**(c) Agent SDK / `claude -p --input-format stream-json`**：stdin 每行一条 `{"type":"user","message":{…}}`，**排队**顺序处理、可合并进一轮；`interrupt()` 只在流式模式可用，不清队列（"does not clear the message buffer"），2.1.219 起可 `cancel_queued`。身份 = 用户消息。【官方】[streaming-vs-single-mode](https://code.claude.com/docs/en/agent-sdk/streaming-vs-single-mode) · [typescript](https://code.claude.com/docs/en/agent-sdk/typescript)

**(d) `--resume` / `--continue` / `-p`**：加载转录、新进程跑一轮——「下次启动带上」。**例外（v2.1.285+）**：目标是**正在运行的后台会话**时，`claude --resume <id> "check the tests too"` 不加载转录，而是把提示作为那个会话的**下一轮**送过去再 attach（"Sent your prompt to the background session (<id>); opening it…"）；有管道/重定向或 `--model` 等标志时不发，`/` 或 `!` 开头不发，会话正等人回答时不发。【官方】[sessions#resume-a-running-background-session](https://code.claude.com/docs/en/sessions)

**(e) Remote Control / `--cloud`**：厂商中继，"makes outbound HTTPS requests only and never opens inbound ports"；第三方接不进。**(f) Hooks**：只在生命周期事件点能给模型塞 `<system-reminder>`（`additionalContext`），不是随时可用的通道；hook 想随时说话要写 (b) 的 socket。**(g) `claude --bg` / `claude agents`**：没有 `claude send`，就是 (d) 的 `--resume <id> "prompt"`；supervisor 的 socket 协议未公开。【官方】[remote-control](https://code.claude.com/docs/en/remote-control) · [hooks](https://code.claude.com/docs/en/hooks) · [agent-view](https://code.claude.com/docs/en/agent-view)

### 2.2 Codex CLI（OpenAI）——app-server 的 `turn/steer`

- **接口**：`codex app-server` 的 JSON-RPC。传输："`stdio` (`--listen stdio://`, default): newline-delimited JSON (JSONL)"；"`websocket` (`--listen ws://IP:PORT`, experimental and unsupported)"；`--listen unix://` / `unix://PATH`（WebSocket over Unix socket）；`--listen off`。TUI 能挂到远端：`codex --remote ws://… | unix://…`。
- **寻址**：`threadId`（`thread/start` / `thread/resume`）+ `turnId`。默认 Unix socket 路径只在测试夹具里见到 `$CODEX_HOME/app-server-control/app-server.sock`，文档未写（未证实）。
- **运行中注入**：**Y，显式 `turn/steer`**："append user input to the active in-flight turn for a thread; returns the accepted `turnId`"；"Include `expectedTurnId`; it must match the active turn id"；"The request fails if there is no active turn on the thread"；"`turn/steer` doesn't emit a new `turn/started` notification"。`turn/interrupt`："request cancellation of an in-flight turn; success is `{}` and the turn ends with `status: "interrupted"`"。
- **身份**：用户输入（"append user input"）。**忙时语义**：steer 在下一个模型采样边界生效（OpenClaw 从客户端角度印证："Codex app-server gets one batched `turn/steer` and applies it at the next model boundary"）；`turn/start` 撞上活动轮时 core 返回 `NotIdle`，协议有 `ActiveTurnNotSteerable` 错误（"for example `/review` or manual `/compact`"）——推断 `turn/start` 在活动轮期间也按同轮 steer 处理（CLI 组从源码推断，文档未直接写）。ingress 满时 `-32001` "Server overloaded; retry later"。
- **TUI**：Enter = 注入当前轮，Tab = 排到下一轮："Press Enter while Codex is working to inject new instructions into the current turn." / "Press Tab while Codex is working to queue a follow-up prompt…"
- **鉴权**（WebSocket）：`--ws-auth capability-token --ws-token-file …` 或 `signed-bearer-token`；"Non-loopback WebSocket listeners currently allow unauthenticated connections by default during rollout."
- `codex exec resume`：下次启动带上。
- 【官方】[app-server](https://learn.chatgpt.com/docs/app-server) · [developer-commands](https://learn.chatgpt.com/docs/developer-commands.md?surface=cli) · [non-interactive](https://learn.chatgpt.com/docs/non-interactive-mode) · 源码 `codex-rs/app-server/src/main.rs`、`codex-rs/core/src/session/turn_input.rs`、`codex-rs/app-server/src/request_processors/turn_processor.rs`

### 2.3 Gemini CLI——基本只有「下次启动带上」

- `packages/a2a-server`（实验，"All code in this package is experimental and under active development"）：Express + `@a2a-js/sdk`，AgentCard `protocolVersion: '0.3.0'`，`pushNotifications: false`，监听 `localhost:${CODER_AGENT_PORT||0}`；鉴权是占位（Bearer `valid-token` / Basic `admin:password`）。向运行中 task 追加消息走 SDK 默认处理器，忙时语义**未证实**。
- `gemini --acp`：JSON-RPC over stdio，单客户端独占子进程（Zed 用它）；`prompt` 与上一条撞车的处理文档未写（未证实）。
- headless：`-p`、`--resume`、`/chat save|resume`——全是下次启动带上；没有 stdin 流式输入。
- 【官方】[acp-mode.md](https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/acp-mode.md) · [packages/a2a-server](https://github.com/google-gemini/gemini-cli/tree/main/packages/a2a-server) · [headless](https://geminicli.com/docs/cli/headless/)

### 2.4 opencode——TUI 是 server 的客户端，所以外部 POST 天然进会话

- **接口**：`opencode serve [--port 4096] [--hostname 127.0.0.1]`，OpenAPI 在 `GET /doc`，SSE `GET /event` / `GET /global/event`；**TUI 本身是这个 server 的客户端**（`opencode attach <url>`，`opencode run --attach <url>`）。另有一组**驱动 TUI 本身**的端点 `POST /tui/append-prompt`、`/tui/submit-prompt`、`/tui/execute-command`、`/tui/show-toast`："The `/tui` endpoint can be used to drive the TUI through the server."
- **发消息**：文档（v1 路径）`POST /session/:id/message`（同步等整轮）、`POST /session/:id/prompt_async`（204）、`/abort`。仓库（`V2_HTTP_API_AUDIT.md`，2026-09-13）改为 `/api/session/{id}/prompt` + **inbox**：`GET /api/session/{id}/inbox`、`PATCH …/inbox/{inboxID}`（"Consolidated delivery mutation with `delivery: \"steer\" | \"queue\"`"）、`DELETE` 取消。
- **忙时语义**（`packages/core/src/session/inbox.ts`，撰写者复核）："\"steer\" promotes steers only (a step boundary mid-work)," / "while \"input\" also allows one queued input when no steers are waiting (the idle boundary,"；按 `enqueued_seq` 排序；`compaction` / `move` 是控制项，优先于普通输入。`prompt.ts` 里默认 `delivery ?? "steer"`（CLI 组读到，未复核）。
- **身份**：inbox 条目 `type: "user"`（还有 `synthetic` 给非用户来源）；外部 POST 与 TUI 输入走同一条路，**无法区分**。
- **寻址 / 鉴权**：server URL + `sessionID`；`OPENCODE_SERVER_PASSWORD` 开 HTTP basic auth。
- 【官方】[server](https://opencode.ai/docs/server/) · [cli](https://opencode.ai/docs/cli/) · 源码 [inbox.ts](https://github.com/sst/opencode/blob/dev/packages/core/src/session/inbox.ts)

### 2.5 Amp——stdin JSONL 与厂商中继

- `amp -x --stream-json --stream-json-input`："Use `--stream-json-input` to let Amp read messages from stdin until it closes."；每行 `{type:"user", steer?: boolean, message:{…}}`，"Set the top-level `steer` field to `true` if Amp should handle a queued message at the next interruption point."——长驻进程可持续喂。
- TUI 默认 **steer**（"is sent after the current step instead of waiting for the agent to completely finish"），Ctrl+Enter 排队，Esc Esc 打断。
- Cross-Client Access：从 ampcode.com / 手机给**正在 CLI 里跑的线程**发消息，经厂商中继；本地 socket / HTTP API **没有**（未证实）。Runner：`amp --no-tui --runner-id` 等远程派单。
- 【官方】[threads](https://ampcode.com/docs/markdown/threads) · [streaming-json](https://ampcode.com/docs/markdown/cli/streaming-json) · [remote-control](https://ampcode.com/docs/markdown/cli/remote-control) · [runners](https://ampcode.com/docs/markdown/cli/runners)

### 2.6 crush（charmbracelet）——`claude/channel` 的第二个实现

- `internal/agent/tools/mcp/channel.go` 在 transport 包装层截获 `notifications/claude/channel`（go-sdk 会拒绝未知方法，所以在 `channelConn.Read` 里先截）；要求 server 在 initialize 声明 experimental `claude/channel`；校验 fail-closed（`content` ≤ 64 KB，`meta` ≤ 32 项、键须合法 XML 名，保留键 `source` / `xmlns` / `xml` 丢弃）；渲染为 `<channel source="<server>" k="v">content</channel>`；opt-in 是配置 `channel_enabled` 或 `--channels`（README 没写）。
- **怎么进模型**：进程内 TUI 走 `handleChannelMessage → AgentRunChannel(channel, sessionID, prompt)`，塞进当前打开的 session，没有就新建（测试原话："No active session: a pushed event must not be dropped."）；`crush serve` 模式在 server 端注入一次（"a push is processed once no matter how many clients are attached — including zero"）。**身份**：一次 agent turn 的 prompt（用户消息），正文是 `<channel>` 元素，`proto.AgentMessage.Channel` 标来源。
- **忙时**：有 prompt 队列 API（`QueuedPrompts` / `ClearQueue`、`GET …/prompts/queued`），推断排队；注入点是步边界还是轮边界未读到（部分证实）。
- 【官方源码】[channel.go](https://github.com/charmbracelet/crush/blob/main/internal/agent/tools/mcp/channel.go) · `internal/backend/channels.go` · `internal/ui/model/ui.go`

### 2.7 再加几个：pi、aider、Cursor CLI、OpenClaw

- **pi**（badlogic/pi-mono）——忙时语义写得最清楚的一家：`pi --mode rpc` 是 stdin/stdout JSON 的长驻子进程；agent 在跑时 `prompt` **报错**，除非带 `streamingBehavior: "steer" | "followUp"`；steer = "after the current assistant turn finishes executing its tool calls, before the next LLM call"，follow_up = "Delivered only when agent has no more tool calls or steering messages"；TUI Enter = steer、Alt+Enter = follow-up。【官方】[rpc.md](https://github.com/badlogic/pi-mono/blob/main/packages/coding-agent/docs/rpc.md)
- **aider**：`--watch-files`——**文件就是输入通道**：在任意文件里写 `# … AI!` 注释，aider 收集后当指令（编辑器进程写文件即触发）；处理中又出现新注释的行为未写。`--message` 是下次启动带上。【官方】[watch](https://aider.chat/docs/usage/watch.html)
- **Cursor CLI**：`agent -p`、`--resume`、`agent acp`；**无**运行中注入。【官方】[headless](https://cursor.com/docs/cli/headless)
- **OpenClaw**（gateway + sessions）：WebSocket 控制面 `chat.send`；agent 内跨会话工具 `sessions_send(sessionKey, message, mode: steer|followup|resume|notify)`；`messages.queue.mode` 四种——`steer`（默认，"inject messages into the active runtime, including while it is executing tools"，每个工具批次结束后、下一次模型决策前可见）、`followup`（本轮结束后）、`collect`（静默窗合并）、`interrupt`；默认 500 ms debounce、`cap: 20`；`sessions_send` 的消息 "are marked as inter-session data in the receiving prompt"。鉴权：设备配对 + token + scope。【官方】[queue](https://docs.openclaw.ai/concepts/queue) · [session-tool](https://docs.openclaw.ai/concepts/session-tool) · [gateway/protocol](https://docs.openclaw.ai/gateway/protocol)
- **tmux `send-keys`**（万能底线）：`send-keys -t <pane> -l "<text>"` 把字面文本当键盘事件送进任何 TUI；寻址是 pane，鉴权是 tmux socket 的文件权限。代价：TUI 分不清是谁打的，忙时语义就是那个 TUI 对键入的处理，没有送达确认，多行要靠 `-l` / bracketed paste。【官方】[tmux(1)](https://man.openbsd.org/tmux.1)

### 2.8 CLI 能力对照

| 产品 / 机制 | 接口 | 寻址 | 运行中注入 | 到达模型的身份 | 忙时语义 | 鉴权 |
|---|---|---|---|---|---|---|
| Claude Code · channels | MCP stdio 子进程，`notifications/claude/channel` | 无会话 id；server 绑定所在会话 | **Y** | `<channel source=…>` 块，非用户 | 排队，下一轮合并；无 ACK | server 自管白名单 + 组织策略 + `--channels` 点名 |
| Claude Code · 跨会话 socket | 每会话 Unix socket（`CLAUDE_CODE_MESSAGING_SOCKET`，可选 auth 行） | 会话名 / 短 id / `@名` | **Y**（脚本、hook 可写） | 纯文本 + 发送方 + 回复地址，标「来自另一会话」 | 工具调用之间读；空闲开新轮；`accept|hold|refuse` | OS 用户；Windows 需 token |
| Claude Code · SDK / stream-json | stdin JSONL | 持有该进程 stdin | Y | 用户消息 | 排队、可合并；`interrupt()` | 进程所有权 |
| Claude Code · `--resume <id> "p"` | CLI | session id/name | 仅对运行中的 bg 会话（2.1.285+） | 用户消息 | 排队 | 本机 |
| Codex · app-server | JSON-RPC stdio / `ws://` / `unix://` | `threadId` + `expectedTurnId` | **Y：`turn/steer`** | 用户输入 | 下一模型边界生效；`turn/interrupt` 取消 | ws 可选 token；非回环默认无鉴权 |
| Gemini CLI | `--acp`（stdio）；A2A server（实验） | ACP session / A2A task | 部分（忙时未证实） | 用户消息 | 未证实 | 占位 |
| opencode | HTTP + SSE；`/session/:id/…`；v2 inbox；`/tui/*` | server URL + `sessionID` | **Y** | 用户消息（`user` / `synthetic`） | 默认 `steer`（步边界）/ `queue`（空闲） | HTTP basic |
| Amp | stdin JSONL（`steer` 字段）；厂商中继 | 线程 `T-…` | Y | 用户消息 | 默认 steer；Ctrl+Enter 排队 | `AMP_API_KEY` |
| crush | `claude/channel` 兼容；`crush serve` | 被查看的 session | Y | agent turn 的 prompt（`<channel>` 元素） | 有队列；边界未证实 | 未证实 |
| pi | `--mode rpc` stdin/stdout | 单子进程 | Y | 用户消息 | `prompt` 忙时报错，除非 `steer` / `followUp` | 无 |
| aider | `--watch-files` | 工作目录 | 部分（文件触发） | 用户指令 | 未证实 | 无 |
| Cursor CLI | `-p`、`--resume` | chatId | N | — | — | API key |
| OpenClaw | gateway WS `chat.send`；`sessions_send` | `sessionKey` | Y | `sessions_send` 标 inter-session data | `steer` / `followup` / `collect` / `interrupt` | 设备配对 + token |
| tmux `send-keys` / herdr `agent prompt` | 键盘事件 | pane | Y（任何 TUI） | 用户消息，不可区分 | 取决于 TUI | socket 文件权限 |

**读表的结论**：(1) 「能在运行中注入」已经是主流，只有 Gemini / Cursor / aider 基本停在「下次启动带上」。(2) 身份可区分的只有三家：Claude Code 的两条路（`<channel>` 块、「from another session」）与 OpenClaw 的 `sessions_send`；其他都把外部消息**当用户输入**，靠「steer / queue 二选一」补偿。(3) 本地传输清一色是 **socket / 本机端口 + 一行 JSON**，文件信箱只有 Claude Code agent teams 的 `inboxes/*.json` 一个先例，aider 的 `--watch-files` 是另一种形态。

---
## 3. iota 现在有什么

基于 `main` = `2929ed4`（2026-10-21）。每条带 `file:line`。

### 3.1 一条现成的「外部结果注入成用户消息」通道：后台任务通知

**结论：iota 已经有一条完整的、单一的「进程外事件 → 会话输入」路径，只是今天唯一的发送者是自己的 job 注册表。**

链路（交互模式）：

1. **产生**：`shell` 工具的后台子进程结束时，注册表把一个 `JobDone`（`src/shell/jobs.rs:94-109`：id、命令、退出码、耗时、输出文件路径）交给「sink」或「parked」（`src/shell/jobs.rs:10-13` 模块文档：interactive 走 sink 推给 UI，headless 停在 `finished` 等循环来取）。
2. **变成输入**：`job_notice`（`src/repl/run.rs:65-74`）把它包成 `ui::facade::Input { display, text, kind: InputKind::Notice }`。`display` 是一行标题 `[background job b1 finished: exit 0 after 2s] make test`（`src/shell/jobs.rs:661-668`），`text` 是标题加被截断的任务输出（`src/shell/jobs.rs:673-679`）。
3. **投递**：主循环启动时装 sink：`repl.handles.jobs.set_sink(… ui_sink.enqueue(job_notice(&done)))`（`src/repl/run.rs:689-697`）。注释原话：「ONE delivery path, no second queue」。
4. **进队列**：`Ui::enqueue`（`src/ui/facade.rs:908-911`）是 fire-and-forget（`src/ui/runtime/handle.rs:315-319`，往 UI 线程发 `UiMsg::Enqueue`）。事件循环里（`src/ui/runtime/event_loop.rs:599-608`）：有 parked 的 `read_input` 就直接喂给它（空闲循环被唤醒），否则排在已 type-ahead 的内容后面，成为 `Queued::Notice`（`src/ui/runtime/event_loop.rs:322-346`；notice 永远 steerable，slash 命令才会挡住 drain）。
5. **轮中注入**：一轮正在跑时，`Steerer::drain`（`src/repl/turn/steer.rs:45-70`）在**每个 round 边界**取队列（`src/repl/turn/steer.rs:1-4`：「the only place a user message can legally enter mid-turn — a round's tool results must directly follow its calls」）。notice 被渲染成一行暗色标题（`boundary_notice`）并变成 `Message::notice(text)`。
6. **到达模型的身份**：`Message::notice` 是 `Body::Notice`（`src/provider/model.rs:171-177`），发给 provider 时映射为 `Role::User`（`src/provider/model.rs:312`）。也就是说：**模型看到的是一条用户消息**，只是 transcript 不画 `❯` 块、不进历史回溯（`src/ui/facade.rs:19-28`）。
7. **空闲时**：主循环的 `read_input` 收到 notice 后跳过回显，也不走 slash 派发链（`src/repl/run.rs:748-776`），直接当作一轮的输入发出去。host 状态上，这一轮由 notice「持有」（`Presenter::notice_taken`，`src/host/mod.rs:377`）。

同一条路还有第二个进程内发送者：bot 模式的 memory-flush notice（`src/repl/run.rs:1266-1276`，`BotAction::QueueFlush` → `ui.enqueue(Input{kind: Notice})`）。bot-mode.md §3.6 的代码注释就把它叫「现成的外部唤醒通道」（`docs/design/bot-mode.md:455`）。

headless 没有空闲循环，所以同一个事实走另一半：`QuietHost` 自带 jobs 注册表（`src/headless/run.rs:26-37`），工具循环每轮 `push_job_notices` 把 `take_finished()` 里的任务变成 `Message::notice` 追加到历史（`src/headless/run.rs:234-238`），没事可做就 `wait_any` 阻塞等（`src/headless/run.rs:243-248`）。

**对本题的意义**：「别的进程给当前会话发消息」在 iota 内部只差一个发送者——`Ui::enqueue(Input{kind: Notice})` 已经定义了投递语义（空闲立刻唤醒；忙时排队，round 边界进入；永不打断正在进行的 tool round）、显示语义（一行暗色标题、不算用户说的）和模型侧身份（`Role::User`）。缺的只是一个从进程外到 `enqueue` 的入口，以及一个比 `[background job …]` 更通用的标题格式。

### 3.2 herdr 集成：只上报，不接收

- iota 是**客户端**，herdr 是**服务端**：iota 通过 `HERDR_SOCKET_PATH` 指向的 Unix socket，每个请求一行 JSON、读一行回复（`src/host/herdr.rs:402-424` `send_socket`，500 ms 超时，失败只 `debug!`）。上报的内容是生命周期状态（`working` / `blocked` / `idle`）、会话 id 与 bundle 路径、退出释放（`src/host/herdr.rs:1-8`；`docs/DIVERGENCES.md:324` X-47）。没有 Unix socket 的平台改为调 `herdr` CLI（`src/host/herdr.rs:430`）。
- host 层的契约是单向的：`Host` 的能力 trait 是 `StateReporter`、`Notifier`、`SessionReporter`、`ChildEnvironment`（`src/host/mod.rs:137-182`），全部是「告诉宿主」；唯一反向的 `EnvironmentContributor`（`src/host/mod.rs:170`）也只是把 `host: herdr` 和 pane id 写进 `<environment>`，不是消息。
- **但 herdr 侧已经能「往 iota 的 pane 里送话」**（本机实测，`herdr 0.9.5`）：`herdr agent prompt <TARGET> <TEXT> [--wait] [--until idle|working|blocked|done|unknown] [--timeout MS]`，以及更底层的 `herdr agent send-keys`。它把文字作为键盘输入打进 pane；`--wait` 依据的正是 iota 上报的状态——host 层的「job 没结束就保持 Busy」规则就是为它定的（`src/host/mod.rs:31-41`，原话「herdr's `agent prompt --wait` therefore waits for it too」；`docs/design/busy-span.md:464`）。`agent prompt --help` 原文：「If the agent is already blocked, submission is rejected with agent_blocked before any input is sent … It does not track turns: if the agent is already working, that active turn's completion may match.」
- 这条路的本质是 tmux `send-keys`：iota 分不清它和用户敲的字，所以到达模型时是普通用户消息（走 composer → submit），忙时则进 type-ahead 队列并在 round 边界 drain——投递语义和 §3.1 一致，只是显示上是 `❯` 块，而且 `/` 开头会被当 slash 命令。

### 3.3 headless 与 bot 模式各自的位置

- **headless** `iota run [<agent>] -m <msg>`（`src/cmd/args.rs:316-323`；`-` 读 stdin）：一次一条消息，跑完退出，打印回复或 JSON 报告（`src/headless/mod.rs:5-8`）。它是「下次启动时带上」那一类：没有常驻进程，也就没有「运行中的会话」可投递。配 `iota resume <id> -m` 可以往已有会话追加一轮，但如果那个 bundle 正被另一个进程写着，`SessionError::Locked`（`src/session/error.rs:48`；`src/session/lock.rs:1-4` 单写者锁）会直接拒绝——**两个进程不能同时写同一个会话**。
- **bot 模式**（`mode: bot`，`src/cmd/interactive/bot.rs:1-6`；编排状态机 `src/repl/bot.rs:1-30`）是 TUI 常驻的形态 A：进程活着就是 bot 在线，进程退出就下线。它没有新动词、没有入站口；bot-mode.md 把入站列为 **L3**：「inbox 目录 `bots/<name>/inbox/`，运行中的 bot 轮询（或 watch）它，每个文件原子 rename 后读入 `ui.enqueue(Notice)`；`iota run <bot> -m` 在锁被占时把消息写进 inbox 并退出」，Unix socket「只在 inbox 不够用时（例如要拿到回复）再做」（`docs/design/bot-mode.md:610`）；**L4** 才是 `claude/channel` 协议接 IM（`docs/design/bot-mode.md:611`）。§7「明确不做」里定了边界：「本地入站只开 inbox 目录和 Unix socket（L3），不监听 TCP」（`docs/design/bot-mode.md:703`）。

### 3.4 有没有任何「外部投递」的入口

逐项查过（`grep` 整个 `src/`）：

| 入口 | 现状 | 出处 |
|---|---|---|
| Unix socket / TCP 监听 | **无**。进程只做出站连接：herdr socket（`src/host/herdr.rs:408`）；MCP OAuth 回调临时监听 `127.0.0.1` 一次性端口（`src/mcp/auth.rs:594-601`），拿到授权码就关 | — |
| 文件监听 / inbox | **无**。唯一的文件 mtime 检测是 bot 的 `MEMORY.md` 外部编辑重载（`docs/design/bot-mode.md:423`），那是重读 system 段，不是消息 | — |
| 信号 | **只有取消**：SIGINT/SIGTERM/SIGHUP → `CancellationToken`，第二次 SIGTERM 立刻退出 130（`src/cmd/signals.rs:1-17`）。没有 SIGUSR1/2 之类的「去读信箱」信号 | — |
| MCP 反向通道 | **没开**。客户端能力是 `rmcp::model::ClientCapabilities::default()`（`src/mcp/transport.rs:416-422`）——不声明 `sampling`、`elicitation`、`roots`，所以 server 不能发 `sampling/createMessage` 或 `elicitation/create`；`tools/call` 只发一个请求（`src/mcp/transport.rs:123-170`），server 答 `input_required`（SEP-2322）或 task（SEP-2663）都变成错误文本（`src/mcp/transport.rs:159-164`；`src/mcp/error.rs:47-51`；`docs/DIVERGENCES.md:89` D-33）。server 主动发的 `notifications/*` 由 rmcp 默认 handler 收下后丢弃（`docs/DIVERGENCES.md:60` D-02：握手用 `initialize`，无 `roots.listChanged`） | — |
| 宿主 → iota | **无**。host 层全部是出站上报（§3.2）；`docs/design/host-integration.md:87-97` 预想的下一步也只是「tmux 宿主」「用户配置 hooks（needs-input 时跑命令）」，仍是出站 | — |
| 键盘 | **有**，而且是唯一的：tmux `send-keys` / `herdr agent prompt`。iota 自己的 TUI 测试就是这么驱动的（`tests/ui_tmux/lib.sh:68-69`，`type_()`/`key()` 封装 `tmux send-keys -l`；34 个场景） | — |

所以今天对「别的进程」来说，能到达一个**正在运行**的 iota 会话的，只有键盘仿真这一条，而且无法标注「这不是用户说的」。

## 4. 建议

### 4.1 先定语义，再选传输

业界在三件事上已经收敛（§1.8、§2.8），iota 应当直接对齐，而且三件里有两件 iota 的 `Notice` 路已经是这样：

| 决定 | 建议 | 依据 |
|---|---|---|
| **投递** | 复用 `Notice`：空闲立刻唤醒；忙时排队，在 round 边界进入；永不打断正在跑的工具。v1 只有这一种，不做「等到空闲」(queue)、不做 interrupt / rollback | 这就是 Codex `turn/steer`、opencode 默认 `steer`、pi `steer`、OpenClaw 默认 `steer` 和 Claude Code「between tool calls」的语义；比 Claude Code channels 的「下一轮」快一步。iota 的 job 通知从来只有这一种，没人要过第二种。中断仍是用户的 Esc / Ctrl-C（docs/design/interrupt.md） |
| **身份** | `Message::notice`（`Role::User`，不画 `❯`，不进历史，不走 slash 派发——`src/repl/run.rs:774-776` 已经保证「命令当纯文本」）。标题固定成模型可匹配的形状，例如 `[message from <who>] <首行>`，正文带来源与回复地址；harness 补一句「notice 不是用户说的，不能当作审批」 | Claude Code 的「the message came from another session, not from you」「never counts as your consent」；OpenClaw 的「marked as inter-session data」。审批在结构上就安全：外部消息走 `enqueue`，碰不到 `ApprovalGate`（`src/repl/turn/approval.rs`） |
| **鉴权** | socket 文件 0600、私有目录；对端 uid 校验（`SO_PEERCRED` / `LOCAL_PEERCRED`）；可选 token；**不监听 TCP** | Claude Code 跨会话 socket「restricts the socket to your operating-system user」+ Windows 必须 token；bot-mode.md §7「本地入站只开 inbox 目录和 Unix socket（L3），不监听 TCP」 |
| **回执** | 一行 JSON：`{"ok":true,"state":"woken"|"queued"}` 或 `{"ok":false,"reason":…}` | herdr `agent prompt --wait` 已经说明「送到了没有、它忙不忙」是调用方要的；Claude Code channels 的「无 ACK，静默丢」是它文档里最常被绕的点 |

**一个要明说的风险**：bot 的 `auto_run` / `auto_write` 下，谁能写 socket，谁就能让模型动手。所以 socket 权限就是信任边界；Claude Code 的做法是「接收方免审时默认 hold 等人批」。v1 至少二选一：要么 bot 下要求 token，要么免审 bot 收到外部消息先 hold 成 `NeedsInput`。

### 4.2 候选方案

| 候选 | 能覆盖什么 | 代价 | 与 iota 现有架构的契合度 |
|---|---|---|---|
| **A1. 复用 MCP elicitation / sampling** | **覆盖不了本题。** elicitation 是「server 在处理 iota 发起的 `tools/call` 期间向用户要一个表单」，sampling 是「server 借 iota 的模型跑一段独立对话」——都不是外部进程主动送一条消息，而且生命周期绑在那次请求上（§1.1）。2026-07-28 已改成 MRTR、sampling 弃用 | 要支持得实现 `ClientHandler::create_elicitation` + 一个表单 surface，那是另一件事（给 MCP server 要参数用） | iota 明确不接 MRTR（`docs/DIVERGENCES.md:89` D-33），能力声明是默认空（`src/mcp/transport.rs:416-422`，D-02）。**不选** |
| **A2. 实现 `claude/channel`（MCP 通知）** | IM（Telegram / Discord / iMessage 现成插件）、webhook / CI；本地进程也能走（外部进程 POST 给一个本地 channel server，由它转成通知——channels-reference 的 webhook 例子就是这样），crush 已兼容 | 自定义 `ClientHandler`（今天 handler 就是 `ClientInfo`，`src/mcp/transport.rs:416-422`）：读 `ServerCapabilities.experimental["claude/channel"]`（rmcp `model/capabilities.rs:223-225`），在 `on_custom_notification` 里过滤 `notifications/claude/channel`（rmcp `handler/client.rs:76-77` 已把未知通知分发到这里），照 crush 的 fail-closed 规则校验，渲染成 `<channel source=…>` 进 `ui.enqueue(Notice)`；每次运行显式 opt-in（Claude Code 与 crush 都要求「写进配置不够，启动时点名」）；`reply` 就是普通 MCP 工具。**前提**：channel server 必须走 `initialize` 握手——rmcp 默认即是（`LATEST = 2025-11-25`，`model.rs:175`），将来若改 `Auto` / `server/discover`（D-02 提过），channel server 必须留在 legacy，Claude Code 的 `MCP_PROTOCOL_NEGOTIATION=legacy` 就是为此 | 高：MCP manager、stdio 子进程生命周期（mcp-child-death-research）、`ui.enqueue` 都现成。缺点：多一跳；靠 Bun 插件；research preview，"the `--channels` flag syntax and protocol contract may change"；寻址仍是「启动时点名」。**定位 L4（bot-mode.md 已排）** |
| **B. 每会话一个 Unix socket + JSON 行** | 另一个 iota、脚本、cron、hook、herdr（它已经拿到 bundle 目录，`pane.report_agent_session`，可以直接写 socket 而不是 send-keys）；以后 channel server 也可以直接写它 | iota 的**第一个入站面**：一个 listener task（tokio `UnixListener`，放在 `repl` / `cmd` 层，只调 `ui.enqueue`）；一行 JSON 的协议（§4.5）；路径 `<bundle>/sock`（≈50 字节，在 macOS 104 字节限内）；导出 `IOTA_SOCKET` 给子进程（`ChildEnvironment` 的反向，`src/host/mod.rs:182`）；退出删 socket；回执；限流（队列上限、burst、去重，照 Claude Code）；Windows 命名管道或先不支持。**发送端不加动词**：`iota run <bot> -m` / `iota resume <id> -m` 遇到 `SessionError::Locked`（`src/session/error.rs:48`）时改为连 `<bundle>/sock` 投递并打印回执——锁文件里本来就记着持有者 pid（`src/session/lock.rs:6-7`），动词集不变（bot-mode.md §7） | 高：`ui.enqueue` 现成（`src/ui/facade.rs:908-911`），session bundle 现成，herdr 的 session 报告现成，`-m` 的语义本来就是「一条消息」。不契合处只有一点：headless 没有常驻循环，所以 socket 只对 TUI（含 bot）有意义——iota 没有 Claude Code 那种常驻 `-p`，不算损失 |
| **C. 文件信箱 + round 边界读取**（bot-mode.md L3 的选择） | 脚本、cron、第二终端；**bot 不在线时消息留在盘上**，下次启动带上（socket 做不到） | 轮询（round 边界 + 空闲定时）或 watcher（`notify` crate，新依赖）；原子 rename 协议；**无回执、无对端身份（只有目录权限）、无「谁活着」的发现**；herdr 用不上（它要 prompt + wait） | 最小；bot-mode 已画好。但 L3 第 4 条自己承认「要拿到回复就得 socket」 |
| **D. `iota say` 新动词** | 同 B 的发送端 | 一个动词 | **不选**：动词集封闭（bot-mode.md §7）；B 用 `-m` 就够 |
| **E. 什么都不做** | 人戳一下（herdr `agent prompt`、tmux `send-keys`）；等自己启动的进程（job 通知） | 0 | 已经在用 |

### 4.3 推荐

**B 作为 v1；A2 作为 L4；C 不单独做。** 理由：

1. **B 比 C 多的三样，正是「程序给会话送话」和「人给会话送话」的区别**：回执（送到没、它忙不忙）、对端身份（同一 uid 才收；将来可按 pid / token 分级）、一条路给三类调用方（herdr、另一个 iota、脚本）。C 的唯一优势「离线也能留言」B 也不丢：`-m` 对着一个没有活着持有者的 bundle，就按 L3 第 3 条走 headless。
2. **B 是业界收敛的形状**（Claude Code 跨会话、Codex `unix://`、OpenClaw gateway、opencode 本机端口），文件信箱只有 Claude Code agent teams 一个先例，而且那是同一进程树里的内部用途。
3. **代价差不大**：listener + 协议 + `-m` 投递 + 环境变量 + herdr 报告，没有一项需要新依赖或新层；C 的 watcher 反而要新依赖。
4. **这与 bot-mode.md §6 #24（2026-09-30 定的「inbox 是 L3 唯一的入场券」）相抵触。** 本文不改那条决定，只把理由摆出来；所有者定。若维持 inbox，B 的协议与回执格式照样可以留给 L3 第 4 条用。

### 4.4 该不该现在做

**先问需求是哪一种**，三种里只有一种值得写代码：

| 需求 | 今天怎么办 | 要不要做 B |
|---|---|---|
| 人在另一个 pane 想戳一下正在跑的 iota | `herdr agent prompt <pane> "…" --wait`（本机实测可用，0.9.5），或 `tmux send-keys -l`。到达模型是普通用户消息；不要以 `/` 开头 | 不用 |
| 等「iota 自己启动的进程」的结果 | 已有：`background: true` 的 shell job，结束时 `[background job …]` 回来（§3.1）。甚至「等别人写一个 fifo」也能用同一机制：让模型起一个 `cat <fifo>` 的后台 job，写进 fifo 的内容就是那条通知（**未实测**，沙箱下 fifo 是否可用要试） | 不用 |
| 程序 / 另一个 agent 要给正在跑的会话送话，要能区分「不是用户说的」、要回执、不能执行命令 | 没有。键盘路分不清身份、会执行 `/` 命令、没有回执 | **做** |

**结论：值得做，但等第一个真实用例再做**——例如「一个 iota 给另一个 iota 的 bot 报告 CI 结果」或「herdr 想用 socket 代替 send-keys」。到时 v1 的范围就是 §4.2 B 那一行，不含 steer / queue 二选一、不含权限中继、不含跨机器。它是 iota 的第一个入站面，prompt injection 的边界只有「OS 用户 + notice 口径」两道，所以 bot 免审模式下的 hold（§4.1）要一起做。

### 4.5 协议草图（供讨论，不是实现）

一连接一条消息，发完就关；UTF-8，一行一个 JSON 对象：

```
→ {"v":1,"type":"message","from":"ci","text":"build failed on main: …","reply":"/Users/me/.iota/sessions/01K…/sock"}
← {"ok":true,"state":"woken"}            // 空闲，已作为下一轮输入
← {"ok":true,"state":"queued","ahead":1} // 忙，排在 1 条之后，round 边界进入
← {"ok":false,"reason":"blocked"}        // 正在等审批（NeedsInput），照 herdr 的 agent_blocked 拒收
← {"ok":false,"reason":"too-large"|"refused"|"flooded"}
```

`from` 进标题，`reply` 进正文（模型可以用 shell 写回去，或将来给一个 `reply` 工具）；`text` 不解释、不展开 paste 标签、不走 slash。`state` 的两个值直接对应 `UiMsg::Enqueue` 的两条分支（`src/ui/runtime/event_loop.rs:599-608`）；`blocked` 对应 `State::NeedsInput`（`src/host/mod.rs`）。

---

## 5. 查不到或拿不准的地方

协议组与 CLI 组的「未证实 / 矛盾」原样并入，再加本文自己的：

1. **Claude Code 跨会话 socket 的消息行格式**：文档只给了 auth 行 `{"type":"auth","token":"…"}` 与「脚本可以往 socket 写」，没给消息 JSON 的 schema。未证实（§2.1b）。
2. **Claude Code bg supervisor 的 socket 协议**未公开（`claude daemon status` 只打印 "socket directory"）。
3. **Codex `turn/start` 撞上活动轮**按同轮 steer 处理，是 CLI 组从 core 的 `NotIdle` 与 `ActiveTurnNotSteerable` 注释推断的，文档未直接写。Codex Unix socket 默认路径只在测试夹具里见到 `$CODEX_HOME/app-server-control/app-server.sock`。Codex TUI steer 的引入版本，媒体说法互相不一致，未核对 CHANGELOG。
4. **Gemini**：`--experimental-acp` 已废弃改 `--acp` 出自第三方；ACP `prompt` 忙时语义、A2A server 对运行中 task 续发的语义均未证实。geminicli.com 页面称 "Gemini CLI was replaced by Antigravity CLI on June 18, 2026" 对部分套餐生效，未深究。
5. **opencode 文档（v1 路径）与仓库（v2 `/api/session/{id}/prompt` + inbox）不一致**；`prompt.ts` 默认 `delivery ?? "steer"` 由 CLI 组读到，未复核。
6. **Amp** `--stream-json-input` 不带 `steer` 时是否默认排队，文档未说；本地 socket / HTTP API 未见；Cross-Client / runner 传输细节未公开。
7. **crush** 忙时注入点是步边界还是轮边界、`crush serve` 的鉴权，未读到源码。
8. **aider** `--watch-files` 处理中出现新注释的行为未写。
9. **OpenClaw** `chat.send` 的参数未展开。
10. **A2A**：v1.0 规范正文用 PascalCase 方法名，但 v1.0.0 发布说明提到 `tasks/list`（斜杠风格）；规范示例 `A2A-Version: 0.3` 与 Agent Card `protocolVersion: "1.0"` 的关系未解释；对 `working` 态任务再送消息只有 "MAY"，排队 / 打断未定义。
11. **ACP**：v1 并发语义无规则；v2 RFD 明说不规定 queueing / steering；GitHub Releases 未显示年份（2026 为推断）；stdio 之外的传输未见。
12. **AG-UI** 1.0 的发布日期规范页没写；同一 thread 并发 run 的规则未写。
13. **MCP**：draft changelog 为空；2025-11-25 是否已标 Final 未单独核对；MCP Apps 的 host 支持列表取自 overview 页。
14. **Microsoft Agent Framework** GA 日期官方博文 2026-04-03，媒体写 04-02；**SLIM** 无具名生产用户，「前身 AGP」未核对；**NANDA** 官方页抓取失败（证书错误），全是第三方材料；**ANP** 无采用证据；**CrewAI** 无 mailbox 是基于文档未审源码。
15. **LangGraph 前端 SDK 与服务端默认不同**：服务端 `multitask_strategy` 默认 `enqueue`，React 等前端的 submission-queue 默认 `rollback`（来自搜索摘要，未逐页核对）。
16. **本文自己的两处未实测**：(a) 用 `background: true` 的 shell job 等 fifo 当「零代码信箱」，沙箱下是否可用；(b) `<bundle>/sock` 路径在所有者的实际 `~/.iota/sessions/<ULID>/` 下的长度（估算约 50 字节，macOS 上限 104）。
17. **本文拿不准的一个判断**：§4.3 推荐 B 而不是 bot-mode.md 已定的 inbox 目录。调研支持「socket 是业界形状」，但「离线留言」的价值要看 bot 的实际用法；这一条留给所有者。

---

## 材料

- 两个调研 agent 的任务书：协议组（MCP 反方向、A2A、ACP ×2、AG-UI、LangGraph / AutoGen / CrewAI、AGNTCY-SLIM / NANDA / ANP、OpenAI handoff / background，固定六问），CLI 组（Claude Code 七条路、Codex、Gemini、opencode、Amp、crush、Cursor / aider / pi、tmux、OpenClaw，固定七问）。两份笔记留在会话的 scratchpad，没有入库；它们的 URL 已逐条写进 §1、§2，「未证实」合并进 §5。
- 本机实测：`herdr --version`（0.9.5）、`herdr agent --help`、`herdr agent prompt --help`、`herdr agent send-keys --help`；`claude --version`（2.1.306）、`codex --version`（0.162.1）、`tmux -V`（3.8）；rmcp 3.1.4 源码在 `~/.cargo/registry/src/*/rmcp-3.1.4/`。
- iota 源码按 `main` = `2929ed4` 读；相关设计文档：`docs/design/bot-mode.md`（§2.3、§4.3、§5.1 L3/L4 行、§7）、`docs/design/bot-mode-research.md`（§2.2、§3、§6.1 第 6 条）、`docs/design/host-integration.md`、`docs/design/busy-span.md:464`、`docs/design/interrupt.md`、`docs/DIVERGENCES.md`（D-02、D-33、X-47、X-75）。
