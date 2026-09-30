# Bot 模式调研：持续、不换上下文的 agent 形态

Status: **Research** · 日期：2026-09-30 · 用途：为 iota 在普通 chat、agent mode 之外新增第三种模式「bot 模式」提供依据

本文只做调研，不含设计决定。

## 写作约定

- 每条结论后面附来源链接。「【官方】」指厂商文档、官方博客、帮助中心、仓库源码，或厂商员工在官方论坛的回复。「【媒体】」指第三方报道，「【非官方】」指逆向分析。
- 查不到、或来源不足以支撑的内容，标「未证实」。
- 链接在 2026-09-30 打开并核对过。其中以下几页由撰写者本人复核过原文：Grok Bot overview、ChatGPT dots tasks-and-memory、ChatGPT automations、OpenClaw heartbeat、Claude Code channels、Claude Code remote-control、opencode server、Hermes memory，以及 Codex 的 `app-server-daemon/README.md` 和 `compact.rs`。
- developers.openai.com/codex/* 已 308 重定向到 learn.chatgpt.com/docs/*，下文用重定向后的地址。help.openai.com 对直接抓取返回 403，是经转抓读的原文。

每个对象都回答同样五个问题：

| 编号 | 问题 |
|---|---|
| ① | 连续性：对话连续性靠什么（会话文件 / 记忆库 / 常驻进程） |
| ② | 满窗：上下文满了怎么办 |
| ③ | 主动：能不能主动干活（后台 / 定时 / 事件触发） |
| ④ | 接入：从哪里接入（终端 / 手机 / IM / Web） |
| ⑤ | 隔离：权限与隔离怎么处理 |

---

## 1. xAI Grok

### 1.1 Grok Bot（2026-08 发布，beta）

这是和「bot 模式」最直接对应的产品。

- **是什么**：每个 Bot 是一个「AI teammate」，有名字、职责，以及随时间累积的上下文，跑在一台带浏览器、文件系统和终端的持久云电脑上。官方原话：「A named Bot keeps its memory, files, browser sessions, and preferences across sessions instead of resetting on every task.」【官方】[overview](https://docs.x.ai/grok-bot/overview) · [发布博客](https://x.ai/news/introducing-grok-bot)
- **身份与人格**：创建时填名字、主要职责和描述，之后可以改名字、标签、描述和头像。长期规则写在描述里。【官方】[bots](https://docs.x.ai/grok-bot/bots) · [get-started](https://docs.x.ai/grok-bot/get-started)
- **和普通 chat 的区别**：Grok Bot 独立于 Grok chat，额度也单独算。账号体系挂在 Cursor 上：要有 Cursor 付费或 Teams 计划，或者把 SuperGrok 关联到 Cursor 账号。【官方】[发布博客](https://x.ai/news/introducing-grok-bot) · [get-started](https://docs.x.ai/grok-bot/get-started)

五问：

- ① **连续性**：靠三样东西：每个 Bot 各自的会话、记忆（稳定偏好、角色上下文、既往工作摘要），以及按账号持久化的云电脑（文件、浏览器 cookie、登录态）。Bot 之间的会话和记忆互相隔离，要交接上下文，靠共享文件或直接交接。官方也提醒：「Memory is not a substitute for an authoritative source」。【官方】[overview](https://docs.x.ai/grok-bot/overview) · [computer-and-apps](https://docs.x.ai/grok-bot/computer-and-apps) · [bots](https://docs.x.ai/grok-bot/bots)
- ② **满窗**：Cursor 论坛上的 staff 说，接近上限时会自动摘要，但没有手动 compact，同一个 Bot 下也不能新开会话，而且没有排期。正式文档里未见说明。【官方 staff】[论坛](https://forum.cursor.com/t/grok-bot-prune-compact-an-agent-s-context-without-creating-a-new-bot/168333)
- ③ **主动**：能。先把做法存成 skill，再用 routine 按时间表运行（带时区），或者由事件触发（Slack 消息、GitHub 通知）。「Background routines can run while your laptop is closed」。每个 Bot 最多 50 个 routine，每个 routine 保留最近 20 次运行记录。如果用户长期不回应 check-in，Bot 可能暂停 routine。【官方】[skills-routines-and-automations](https://docs.x.ai/grok-bot/skills-routines-and-automations)
- ④ **接入**：桌面端（macOS/Windows/Linux）和 iOS/Android，各端共用同一套 Bot、会话和 routine。【官方】[get-started](https://docs.x.ai/grok-bot/get-started) · [mobile](https://docs.x.ai/grok-bot/mobile)
  - 发布博客只写了「Desktop and iOS」，与文档不一致。
  - 通过 X、Telegram、API 接入：**未证实**。
- ⑤ **隔离**：
  - 用户之间：每个用户一台 Firecracker microVM。
  - 同一用户名下：所有 Bot 共用一台电脑。官方原话：「separate work surfaces, not separate security boundaries」。
  - 审批：Auto Review 覆盖 shell、插件、routine 和触发器的写操作，以及子 agent，选项是 Allow once / Always allow / Deny。
  - 身份与凭据：Bot 的权限不超过它的主人，没有独立的机器身份；OAuth token 留在后端，不下发给 Bot。
  - 【官方】[security](https://docs.x.ai/grok-bot/security) · [teams-and-enterprises](https://docs.x.ai/grok-bot/teams-and-enterprises)

### 1.2 grok.com 的 Memory、Workspaces 与 Tasks

- **Memory**：2025-04 上线，跨会话记住偏好。回答下方会显示「Referenced chats」，用户可以删除引用、逐条删除记忆，或整体关闭记忆。Workspaces 把一组聊天、文件和自定义指令归到一起。【媒体】[Social Media Today](https://www.socialmediatoday.com/news/grok-adds-conversation-memory-to-customize-future-responses/745718/) · [Maginative](https://www.maginative.com/article/xai-upgrades-grok-with-personalized-memory-and-custom-workspaces/)
  - 官方 FAQ 只提到数据控制的位置。【官方】[FAQ](https://docs.x.ai/grok/faq)
- **Tasks（定时任务）**：可以设为一次、每天、每周、每月运行。【媒体】[TestingCatalog](https://www.testingcatalog.com/grok-set-to-gain-tasks-feature-for-periodical-execution/)
  - 限额和邮件触发的说法互相矛盾，官方页面缺失，均**未证实**。

### 1.3 X 上的 @grok

- 在帖子里 @grok，它会公开回复。【媒体】[heise](https://www.heise.de/en/news/Grok-can-now-also-be-marked-in-posts-on-X-and-replies-10309008.html)
- 一篇论文观察到：它由提及触发，把上游帖子作为上下文，没有发现跨次交互的持久记忆。【论文】[arXiv 2605.19720](https://arxiv.org/html/2605.19720v1)
- 官方对它的记忆机制没有说明，**未证实**。

五问：① 靠线程上下文，按次无状态（未证实） · ③ 只有被 @ 时才响应 · ④ 只在 X。

Companions（Ani 等角色）已在 2026-09 起下线，人格设定并入 Skills。这一点只有媒体报道，没有官方公告日期。【媒体】[RoboRhythms](https://www.roborhythms.com/grok-companions-discontinued/)

---

## 2. OpenAI：Codex 与 dots

一句话：OpenAI 真正常驻的 agent 是**云端的 dots**。本地这一侧最接近常驻的是 **Codex app-server daemon**。Codex CLI 本身不是常驻进程，靠会话文件接续上下文。

### 2.1 Codex CLI

- ① **连续性**：会话文件。
  - 每个会话是一个追加写的 rollout JSONL，路径为 `~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<id>.jsonl`。【官方】[recorder.rs](https://github.com/openai/codex/blob/main/codex-rs/rollout/src/recorder.rs)
  - `codex resume [--last|--all]`、`codex fork` / `/fork` 用来接续和分叉。【官方】[commands](https://learn.chatgpt.com/docs/developer-commands?surface=cli)
  - `codex exec resume` 在非交互模式下接续；加 `--ephemeral` 则不写文件。【官方】[non-interactive](https://learn.chatgpt.com/docs/non-interactive-mode)
  - 可选的 Memories 功能默认关闭：在后台把空闲足够久的旧会话提炼到 `~/.codex/memories/`，会跳过仍在进行的会话，并对密钥脱敏。【官方】[memories](https://learn.chatgpt.com/docs/customization/memories)
  - AGENTS.md 从全局配置到 git 根再到 cwd 逐级拼接，默认上限 32 KiB。【官方】[agents-md](https://learn.chatgpt.com/docs/agent-configuration/agents-md)
- ② **满窗**：自动压缩。
  - 触发阈值是 `model_auto_compact_token_limit`，压缩提示词可以通过 `compact_prompt` 自定义，另有 `PreCompact` / `PostCompact` hook，手动命令是 `/compact`。【官方】[config-reference](https://learn.chatgpt.com/docs/config-file/config-reference)
  - 源码里的做法：新历史 = 初始上下文 + 从近到远保留约 20k token 的用户消息（`COMPACT_USER_MESSAGE_MAX_TOKENS = 20_000`）+ 一条以 `SUMMARY_PREFIX` 开头的摘要。靠这个前缀识别哪条消息是摘要。【官方】[compact.rs](https://github.com/openai/codex/blob/main/codex-rs/core/src/compact.rs)
- ③ **主动**：CLI 本身不能。文档明确说定时任务要在 ChatGPT 网页或桌面 app 里建。【官方】[automations](https://learn.chatgpt.com/docs/automations)
- ④ **接入**：终端和脚本。`codex exec --json` 输出 JSONL 事件流。【官方】[non-interactive](https://learn.chatgpt.com/docs/non-interactive-mode)
- ⑤ **隔离**：沙箱模式（`read-only` / `workspace-write` / `danger-full-access`）和审批策略是两个独立维度。默认不联网，由 OS 级沙箱强制执行。【官方】[approvals-security](https://learn.chatgpt.com/docs/agent-approvals-security)

### 2.2 app-server 与 daemon（本地常驻层）

- `codex mcp-server` 已移除，官方指引改用 app-server。【官方】[mcp-server](https://learn.chatgpt.com/docs/mcp-server)
- app-server 是 JSON-RPC 协议，传输可选 stdio JSONL、WebSocket 或 Unix socket。客户端用 `thread/start` 开线程，然后持续读通知。`codex --remote ws://…` 可以让 TUI 连到另一台机器上的 app-server。非回环地址的 WebSocket 默认不鉴权，需要自己配 token。【官方】[app-server](https://learn.chatgpt.com/docs/app-server)
- **daemon**：
  - 命令有 `codex app-server daemon start|stop|restart|enable-remote-control`。它服务的是桌面、手机这类远程客户端，用文件锁和 socket 让实例互相发现。
  - TUI 能自动挂到已在运行的 daemon 上，挂不上就退回内嵌的 server。
  - 支持自动更新；关停时有 `shutdownGraceSeconds` 宽限期，默认 60 秒，范围 0–300。
  - README 原话：「Shared clients use the environment inherited when the daemon started … per-client environment isolation is not provided.」整个 daemon 标注为实验性。
  - 【官方】[app-server-daemon/README.md](https://github.com/openai/codex/blob/main/codex-rs/app-server-daemon/README.md)
- **Codex Remote**：在手机上的 ChatGPT app 里发任务、审批、看 diff，任务实际在已连接的电脑上执行，电脑要保持在线。【官方】[remote](https://learn.chatgpt.com/docs/remote)

### 2.3 Codex Cloud、Slack、GitHub、Linear

- **Cloud**：
  - 每个任务有自己的工作区，已有任务会保留未提交的改动；新任务从环境快照起步。【官方】[cloud-environments](https://learn.chatgpt.com/docs/environments/cloud-environments)
  - 用户电脑睡眠时任务照样跑。【官方】[cloud](https://learn.chatgpt.com/docs/cloud)
  - 云端任务满窗时怎么处理：**未证实**。
- **Slack**：在频道里 @ChatGPT 发起，代码任务会转给 Codex Cloud。同一线程、同一账号的后续消息会接着同一个任务和环境跑。【官方】[slack](https://learn.chatgpt.com/docs/third-party/slack)
- **GitHub**：`@codex review` 做审查，`@codex fix …` 做修复，审查规则写在 AGENTS.md 里。【官方】[github](https://learn.chatgpt.com/docs/third-party/github)
- **Linear**：把 issue 指派给 Codex，或在评论里 @ 它，就会开一个云端对话，之后在同一个评论线程里续接。【官方】[linear](https://learn.chatgpt.com/docs/third-party/linear)

### 2.4 Scheduled tasks（原名 Codex automations）

这一节对 bot 模式最有参考价值。

- **两种定时任务**：
  - 独立型：每次运行开一个新对话，结果进 Scheduled 收件箱。
  - 对话内型：「return to that chat on a schedule … uses the chat's existing context」，可以按分钟级间隔轮询。文档建议这类任务的提示词写清三件事：每轮做什么、什么情况才汇报、什么时候停。
  - 调度规则支持 RRULE。
  - 【官方】[automations](https://learn.chatgpt.com/docs/automations)
- **运行位置**：
  - 桌面 app 在本地项目目录或独立的 worktree 里跑，要求 app 在运行。
  - 网页版在云端跑，两次运行之间不保留本地文件夹。
  - 【官方】[automations](https://learn.chatgpt.com/docs/automations)
- **事件触发**：事件源有 Gmail、Slack、GitHub，仅网页和手机端支持。事件触发与定时不能组合使用。【官方】[automations](https://learn.chatgpt.com/docs/automations)

### 2.5 dots（2026，always-on agent）

- 跑在云端，有自己的电脑和浏览器，用户设备关机也照样工作。可以从 ChatGPT（桌面和手机）、Slack、Teams、语音通话接入。【官方】[dots](https://learn.chatgpt.com/docs/dots)
- ① **连续性**：初始上下文取自 ChatGPT 记忆。之后 dot 自己记笔记，记的是「preferences, decisions, and ongoing work」，不是完整对话。换渠道找它还是同一个 dot，笔记共享；但各渠道看到的对话彼此独立。【官方】[tasks-and-memory](https://learn.chatgpt.com/docs/dots/tasks-and-memory)
- ② **满窗**：文档只说「context available for an interaction is a selection of this information」，具体机制**未证实**。
- ③ **主动**：「decide when to pause and wake up to continue」，也就是 dot 自己决定何时暂停、何时醒来，不必事先排好时间表。另支持定时和事件监听，但光把它连上 Slack 不会自动开始监控。【官方】[tasks-and-memory](https://learn.chatgpt.com/docs/dots/tasks-and-memory)
- ⑤ **隔离**：连接渠道、连接应用、连接电脑是三项彼此独立的授权。电脑同时只能连一台。在私聊里得到的信息，要先征得用户同意，才会分享给别人。【官方】[dots](https://learn.chatgpt.com/docs/dots)

---

## 3. 同类 coding agent

### 3.1 Claude Code

- ① **连续性**：
  - 会话写入 `~/.claude/projects/<project>/<id>.jsonl`，默认保留 30 天。【官方】[sessions](https://code.claude.com/docs/en/sessions)
  - `--continue` / `--resume` 在同一个 ID 上继续追加；`--fork-session` 复制出一个新 ID。【官方】[how-claude-code-works](https://code.claude.com/docs/en/how-claude-code-works)
  - 跨会话记忆有两层：
    - 人写的 CLAUDE.md，分多级、按目录拼接；
    - Claude 自己写的 auto memory，放在 `~/.claude/projects/<project>/memory/`，同一仓库的各个 worktree 共用。启动时只加载 MEMORY.md 的前 200 行或 25KB，其余 topic 文件按需读取。
    - 【官方】[memory](https://code.claude.com/docs/en/memory)
- ② **满窗**：先清旧的工具输出，再做摘要；可以用 `/compact <focus>` 或 CLAUDE.md 里的 Compact Instructions 控制摘要重点。压缩后会从磁盘重新注入这些内容：CLAUDE.md、auto memory、plan、最近改过的文件、skill 正文。后台命令和子 agent 不受影响，继续跑。【官方】[context-window](https://code.claude.com/docs/en/context-window)
- ③ **主动**：
  - `/loop` 和 Cron 只在会话内生效，要求进程在跑，循环任务 7 天后过期。【官方】[scheduled-tasks](https://code.claude.com/docs/en/scheduled-tasks)
  - `claude --bg` / `claude agents` 由 supervisor 托管，每个会话一个进程，关掉终端也照样跑。【官方】[agent-view](https://code.claude.com/docs/en/agent-view)
  - Routines 在云端跑，由定时、API 或 GitHub 事件触发。【官方】[routines](https://code.claude.com/docs/en/routines)
  - 其他入口：hooks【官方】[hooks](https://code.claude.com/docs/en/hooks)、GitHub Actions【官方】[github-actions](https://code.claude.com/docs/en/github-actions)
- ④ **接入**：
  - **Remote Control**：用 claude.ai/code 或手机 App 驱动本机上的会话。原话：「makes outbound HTTPS requests only and never opens inbound ports … registers with the Anthropic API and polls for work」。
    - server 模式 `claude remote-control` 支持 `--spawn same-dir|worktree|session`，并发上限默认 32，断网约 10 分钟后退出。
    - 【官方】[remote-control](https://code.claude.com/docs/en/remote-control)
  - **Channels**（research preview）：一个 MCP server 把外部事件推进**已经打开的**会话，事件以 `<channel source=…>` 的形式到达模型。
    - 自带 Telegram、Discord、iMessage 和 webhook；必须在 `--channels` 里点名才生效，写进 `.mcp.json` 不够。
    - 原话：「Events only arrive while the session is open, so for an always-on setup you run Claude in a background process」。
    - 【官方】[channels](https://code.claude.com/docs/en/channels)
  - 云会话和 Slack 都是开新的云端会话，不接续本地会话。【官方】[on-the-web](https://code.claude.com/docs/en/claude-code-on-the-web) · [slack](https://code.claude.com/docs/en/slack)
- ⑤ **隔离**：
  - 权限模式配合 OS 沙箱：macOS 用 Seatbelt，Linux 用 bubblewrap。【官方】[sandboxing](https://code.claude.com/docs/en/sandboxing)
  - Channels 的发送者白名单靠配对码建立。权限审批可以转发到 IM 上，此时「Anyone who can reply through the channel can approve or deny tool use」。【官方】[channels](https://code.claude.com/docs/en/channels)

### 3.2 Cursor

- ① **连续性**：本地 Memories 功能从 2.1.x 起被移除，官方建议导出成 Rules。【官方 staff】[论坛](https://forum.cursor.com/t/are-my-memories-gone/144057) Automations 另有跨次运行的记忆，存在 `MEMORIES.md` 里。【官方】[automations](https://cursor.com/docs/cloud-agent/automations)
- ② **满窗**：自动总结，也可以手动 `/summarize`。【官方】[changelog 1.6](https://cursor.com/changelog/1-6)
- ③ **主动**：Automations 可以由 cron、GitHub/GitLab、Slack、Webhook、Linear、Sentry、PagerDuty 触发。【官方】[automations](https://cursor.com/docs/cloud-agent/automations)
- ④ **接入**：桌面、网页、iOS / Android PWA、Slack、Linear、API。【官方】[cloud-agent](https://cursor.com/docs/cloud-agent)
- ⑤ **隔离**：云端 agent 每个一台独立 VM。【官方】[help](https://cursor.com/help/ai-features/cloud-agents) Automations 可以用服务账号身份运行。【官方】[automations](https://cursor.com/docs/cloud-agent/automations)
- 本地会话的存储格式：**未证实**。

### 3.3 opencode

- ① **连续性**：会话、消息、part 存在 SQLite 里。【官方】[sql.ts](https://github.com/anomalyco/opencode/blob/dev/packages/core/src/session/sql.ts)
  - 排障文档还写着旧的 JSON 目录布局，与源码不一致，当前以源码为准。【官方】[troubleshooting](https://opencode.ai/docs/troubleshooting/)
  - `/share` 会把会话同步到公网链接。【官方】[share](https://opencode.ai/docs/share/)
- ② **满窗**：`compaction.auto` 默认开启。`prune` 从后往前保护约 40k token 的工具输出，再往前的清空。【官方】[config](https://opencode.ai/docs/config/) · [compaction.ts](https://github.com/anomalyco/opencode/blob/dev/packages/opencode/src/session/compaction.ts)
- ③ **主动**：只有 GitHub Actions 集成（评论、事件、cron）。【官方】[github](https://opencode.ai/docs/github/) 本地常驻调度：**未证实**。
- ④ **接入**：`opencode serve` 是 headless HTTP server，**TUI 本身就是它的客户端**。
  - 提供 OpenAPI 3.1（`/doc`）和 SSE 事件流（`/global/event`），默认监听 `127.0.0.1:4096`。
  - `attach` / `web` / `acp` 等其他客户端也连到这个 server。
  - 【官方】[server](https://opencode.ai/docs/server/) · [cli](https://opencode.ai/docs/cli/)
- ⑤ **隔离**：权限规则分 allow / ask / deny，大多数默认 allow，`.env` 默认 deny。server 可以用 `OPENCODE_SERVER_PASSWORD` 开 Basic Auth。文档没有提沙箱。【官方】[permissions](https://opencode.ai/docs/permissions/)

### 3.4 crush

- ① **连续性**：项目下 `.crush/crush.db`（SQLite）。【官方】[connect.go](https://github.com/charmbracelet/crush/blob/main/internal/db/connect.go)
- ② **满窗**：
  - 上下文窗口大于 200k 时，剩余不足 20k token 就触发自动 summarize；否则在剩余 20% 时触发。
  - 可以用 `disable_auto_summarize` 关闭。
  - 【官方】[agent.go](https://github.com/charmbracelet/crush/blob/main/internal/agent/agent.go)
- ③④ **主动与接入**：
  - 有隐藏的 `--channels` 标志，**兼容 Claude Code 的 `claude/channel` 协议**。【官方】[channel.go](https://github.com/charmbracelet/crush/blob/main/internal/agent/tools/mcp/channel.go)
  - 有实验性的 `crush server` 客户端/服务端模式。【官方】[root.go](https://github.com/charmbracelet/crush/blob/main/internal/cmd/root.go)
  - 定时能力：**未证实**。
- ⑤ **隔离**：工具审批，外加 `--yolo` 跳过审批。【官方】[README](https://github.com/charmbracelet/crush) 沙箱：**未证实**。

### 3.5 Amp

- ① **连续性**：
  - Thread 存在云端，有 URL，Web、CLI、iOS、macOS 通用；可见性分四档。
  - 可以用 `@T-…` 引用别的 thread，`amp threads continue` 接回一个 thread。
  - 【官方】[threads](https://ampcode.com/docs/markdown/threads)
- ② **满窗**：由专门的系统模型负责压缩。【官方】[models](https://ampcode.com/docs/markdown/models-and-subagents)
  - 官方更推荐 **handoff**：开一个新 thread，只带上相关上下文。【官方】[threads](https://ampcode.com/docs/markdown/threads)
  - 压缩的触发阈值：**未证实**。
- ③ **主动**：
  - 每个 thread 有一台独享的云机器（Orb），空闲时休眠，唤醒后状态还在。
  - Automations 在**原 thread 里**续跑，保留上下文。
  - 【官方】[orbs](https://ampcode.com/docs/markdown/orbs) · [automations](https://ampcode.com/docs/markdown/orbs/automations)
- ④ **接入**：
  - Cross-Client Access：在 Web 或手机上继续本地正在跑的 CLI thread，可以要求 passkey 验证。
  - Runner：`amp --no-tui --runner-id` 把任意一台机器变成执行端。
  - Slack 里可以 @Amp。
  - 【官方】[remote-control](https://ampcode.com/docs/markdown/cli/remote-control) · [runners](https://ampcode.com/docs/markdown/cli/runners) · [slack](https://ampcode.com/docs/markdown/puck/slack-integration)
- ⑤ **隔离**：Orb 是隔离环境；`remoteThreadCreation` 默认关闭。【官方】[settings](https://ampcode.com/docs/markdown/cli/settings) 本地是否逐条审批：**未证实**。

### 3.6 Gemini CLI 与 Aider（简）

- **Gemini CLI**：
  - 会话自动保存，用 `--resume` 恢复；`/compress` 把整个上下文替换成摘要。【官方】[session-management](https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/session-management.md) · [commands](https://github.com/google-gemini/gemini-cli/blob/main/docs/reference/commands.md)
  - 实验性的 Auto Memory 在后台挖掘历史对话，**生成候选记忆放进收件箱，等人审批**。【官方】[auto-memory](https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/auto-memory.md)
- **Aider**：`--restore-chat-history` 默认关闭；超过 `--max-chat-history-tokens` 就做总结；`--watch-files` 监听代码里的 AI 注释。【官方】[options](https://aider.chat/docs/config/options.html)

---

## 4. 通用 LLM 产品的长期记忆

### 4.1 ChatGPT

- ① **连续性**：分两层。
  - **Saved memories**：用户明确要求记住的，或模型判断有用的内容。它与聊天记录分开存放，删掉原对话，记忆仍然保留。
  - **Reference chat history**：从过往对话里提炼出的信息。
  - 【官方】[memory](https://help.openai.com/en/articles/8590148-memory-in-chatgpt)
  - 官方的说法是「查找相关上下文」。【官方】[reference](https://help.openai.com/en/articles/11146739-how-does-reference-saved-memories-work)
  - 逆向分析给出的是另一个结论：每轮**全量常驻注入**，包括元数据、约 40 条近期对话里的用户消息、可见的记忆，以及定期生成的用户画像，没有 RAG。【非官方】[Khemani](https://www.shloked.com/writing/chatgpt-memory-bitter-lesson)
  - 两者不一致，当前实现以哪个为准：**未证实**。
- ② **满窗**：
  - App 里的长对话怎么处理：**未证实**。第三方观察是旧消息被静默丢弃，最后提示达到最大长度。【非官方】[TechRadar](https://www.techradar.com/ai-platforms-assistants/chatgpt/i-pushed-chatgpt-toward-its-hidden-chat-limit-heres-what-actually-happens-when-you-reach-it)
  - Responses API 支持 `compact_threshold` 自动压缩，也可以手动调 `/responses/compact`，压缩产物是加密的 compaction item。【官方】[compaction](https://developers.openai.com/api/docs/guides/compaction)
  - Conversations API 负责持久化会话。【官方】[conversation-state](https://developers.openai.com/api/docs/guides/conversation-state)
- ③ **主动**：
  - Scheduled Tasks 可以一次性或周期运行，同时活跃的任务数按套餐为 3/5/10/15 个。【官方】[scheduled-tasks](https://help.openai.com/en/articles/10291617-scheduled-tasks-in-chatgpt)
  - Pulse 每晚根据记忆和聊天做异步研究（Pro 用户，移动端预览）。【官方】[Pulse](https://openai.com/index/introducing-chatgpt-pulse/)
- ⑤ **隔离**：
  - Project-only memory：项目内外互不引用。【官方】[projects](https://help.openai.com/en/articles/10169521-projects-in-chatgpt)
  - 临时聊天不写记忆。【官方】[temporary chat](https://help.openai.com/en/articles/8914046-temporary-chat-faq)
  - 用户可以查看、更正、删除记忆；回答的 Sources 里会标出用到了哪条记忆。【官方】[memory](https://help.openai.com/en/articles/8590148-memory-in-chatgpt)

### 4.2 Claude（claude.ai 与 API）

- ① **连续性**：
  - Chat search 用 RAG 检索过往对话，范围按项目划分。
  - 记忆**在聊天过程中按主题（Topics）持续写入**，用户可以逐条编辑。
  - 【官方】[chat search & memory](https://support.claude.com/en/articles/11817273-using-claude-s-chat-search-and-memory-to-build-on-previous-context) · [blog](https://claude.com/blog/memory)
  - 逆向分析：记忆以 XML 形式常驻注入，历史对话通过 `conversation_search` / `recent_chats` 两个工具按需检索。【非官方】[Gupta](https://manthanguptaa.in/posts/claude_memory/)
- ② **满窗**：
  - App：开启代码执行时，会自动总结较早的消息。【官方】[usage limits](https://support.claude.com/en/articles/11647753-how-do-usage-and-length-limits-work)
  - API 有三种机制：
    - memory tool（`memory_20250818`）：在客户端执行，操作 `/memories` 目录。【官方】[memory tool](https://platform.claude.com/docs/en/agents-and-tools/tool-use/memory-tool)
    - context editing：默认在 100k 时清理旧的工具调用，清理前提醒模型先把要紧信息写进记忆。【官方】[context editing](https://platform.claude.com/docs/en/build-with-claude/context-editing)
    - 服务端 compaction。【官方】[compaction](https://platform.claude.com/docs/en/build-with-claude/compaction)
- ③ **主动**：Cowork 定时任务在云端运行。【官方】[schedule](https://support.claude.com/en/articles/13854387-schedule-recurring-tasks-in-claude-cowork)
- ⑤ **隔离**：每个项目有独立的记忆空间；Incognito 对话不写记忆；企业管理员可以整体关闭记忆。【官方】[chat search & memory](https://support.claude.com/en/articles/11817273-using-claude-s-chat-search-and-memory-to-build-on-previous-context)

### 4.3 Gemini

- ① **连续性**：
  - Saved Info 是用户显式写入的指令，每次对话都带上。【官方】[saved info](https://support.google.com/gemini/answer/16598625?hl=en&co=GENIE.Platform%3DAndroid)
  - 引用过往对话的开关归在 Personal Intelligence > Memory 下。【官方】[memory](https://support.google.com/gemini/answer/16598469?hl=en&co=GENIE.Platform%3DDesktop)
  - 检索方式（常驻注入还是工具检索）：**未证实**。
- ② **满窗**：**未证实**。
- ③ **主动**：Scheduled actions，同时最多 10 个。【官方】[scheduled actions](https://support.google.com/gemini/answer/16316416?hl=en&co=GENIE.Platform%3DDesktop)
- ⑤ **隔离**：
  - Gems 不使用过往对话记忆，也不使用 Saved Info。【官方】[memory](https://support.google.com/gemini/answer/16598469?hl=en&co=GENIE.Platform%3DDesktop)
  - 临时聊天最多保留 72 小时。【官方】[blog](https://blog.google/products-and-platforms/products/gemini/temporary-chats-privacy-controls/)

### 4.4 横向小结

| 维度 | 三家的做法 |
|---|---|
| **存什么** | 都分两层：一层是用户可见、可编辑的事实或指令条目，另一层是从历史对话派生的信息。 |
| **怎么检索** | 常驻注入与工具检索并存。Claude 的做法是记忆常驻、历史对话走工具检索，这一点官方和逆向分析都能对上。Claude API 的 memory tool 把存储交给开发者自己管。 |
| **何时写入** | 趋势是「聊天中自动写入 + 用户可以编辑」。Gemini CLI 则是先生成候选、等人审批。 |
| **隔离** | 项目级记忆隔离，加上临时 / 无痕对话，已是三家标配。 |

---

## 5. 常驻 agent 进程 + 多端接入：开源项目

### 5.1 OpenClaw（原 Warelay → Clawdbot → Moltbot）

- **沿革**：2025-11 以 Warelay 发布，2026-01 两次改名，最终定名 OpenClaw，现由基金会管理。【媒体】[Wikipedia](https://en.wikipedia.org/wiki/OpenClaw)
- **形态**：一个本地常驻的 **Gateway** 作为控制面，统管会话、工具、事件和渠道。接入 20 多个 IM 渠道，另有原生 app 和 CLI/TUI。【官方】[README](https://github.com/openclaw/openclaw)
- ① **连续性**：
  - 会话存在 SQLite。
  - 默认 `dmScope: "main"`，也就是所有 DM 共用一个主会话；可以改成 per-peer / per-channel-peer 等，文档推荐 per-channel-peer。
  - reset 策略可选 none / daily / idle。
  - 【官方】[session](https://docs.openclaw.ai/concepts/session)
  - 记忆是工作区里的 Markdown 文件：USER.md、MEMORY.md 开局加载，`memory/YYYY-MM-DD.md` 按需做混合检索。【官方】[memory](https://docs.openclaw.ai/concepts/memory)
- ② **满窗**：
  - 自动 compaction，压缩前**先插一轮 memory flush**，提醒 agent 把要点写进记忆文件。
  - 另有只在内存里做的工具结果 pruning。
  - 【官方】[compaction](https://docs.openclaw.ai/concepts/compaction)
  - 后台 Dreaming 把日记提炼进 MEMORY.md。【官方】[memory](https://docs.openclaw.ai/concepts/memory)
- ③ **主动**：
  - **heartbeat** 默认每 30 分钟一次，默认在主会话里跑。
  - 可以设 `isolatedSession: true` 让每次在新会话里跑，省 token。
  - 旧的 `HEARTBEAT_OK` 应答仍然兼容：出现在回复开头或结尾时会被识别，如果剩下的内容很少，这条回复就不发出去。
  - 【官方】[heartbeat](https://docs.openclaw.ai/gateway/heartbeat)
- ⑤ **隔离**：
  - `dmPolicy` 可选 pairing（默认）/ allowlist / open / disabled。
  - Gateway 默认只绑 loopback。
  - 信任模型是「一个 gateway 一个信任边界」，不防互相敌对的多租户。
  - 【官方】[security](https://docs.openclaw.ai/gateway/security)
  - 沙箱默认关闭，只隔离工具执行（Docker），Gateway 本身在宿主机上。【官方】[sandboxing](https://docs.openclaw.ai/gateway/sandboxing)

### 5.2 Letta（原 MemGPT）

- **理论源头**：MemGPT 仿操作系统，用分层内存加中断做虚拟上下文管理。【论文】[arXiv 2310.08560](https://arxiv.org/abs/2310.08560)
- **现状**：V1 API server 已退役，主线换成 letta-code，内含 harness、TUI、App Server 和渠道。【官方】[letta README](https://github.com/letta-ai/letta) · [letta-code](https://github.com/letta-ai/letta-code)
- ① **连续性**：
  - core memory blocks 钉在 system prompt 里，agent 用工具改写。
  - 全部状态存在数据库里，被压缩掉的内容仍能通过 API 取回。
  - 【官方】[stateful agents](https://docs.letta.com/v1-sdk/concepts/stateful-agents/)
- ② **满窗**：sleep-time agent 在后台异步整理共享的 memory block。【官方】[sleeptime](https://docs.letta.com/guides/agents/architectures/sleeptime/) · [memory](https://docs.letta.com/letta-agent/memory)
- ③ **主动**：支持 heartbeat、cron 和 agent 自己排程。Local schedule 只在进程开着时生效。【官方】[scheduling](https://docs.letta.com/letta-code/scheduling)
- ④⑤ **接入与隔离**：
  - `letta server --channels` 承载 Telegram、Slack、Discord 等渠道。
  - DM 策略有 pairing、allowlist、open；多数平台默认 pairing，Slack 默认 open。
  - route 把平台上的 chat 显式绑定到「agent + conversation」。
  - 【官方】[channels](https://docs.letta.com/letta-code/channels)

### 5.3 OpenHands

- ① **连续性**：事件溯源，用追加写、不可变的事件日志保存会话。【官方】[conversation](https://docs.openhands.dev/sdk/arch/conversation)
- ② **满窗**：`LLMSummarizingCondenser` 保留前 `keep_first` 个事件，中间部分换成摘要。【官方】[condenser](https://docs.openhands.dev/sdk/guides/context-condenser)
- ③ **主动**：主仓已改名 Agent Canvas，自称「always-on engineering team」，automation 可以定时或由 webhook 触发，每个会话一个 Docker 容器。【官方】[README](https://github.com/OpenHands/OpenHands)
- ④ **接入**：Web、CLI、Slack、Jira、GitHub。在 Slack 线程里只有发起人能继续追问。【官方】[slack](https://docs.openhands.dev/openhands/usage/cloud/slack-installation)
- ⑤ **隔离**：Agent Server 走 HTTP/WS，用 `X-Session-API-Key` 鉴权，文档警告不要把它无鉴权地暴露到公网。【官方】[agent-server](https://docs.openhands.dev/sdk/arch/agent-server)
- 多用户之间的数据隔离：**未证实**。

### 5.4 Hermes Agent（Nous Research）

- **形态**：一个 gateway 进程接 Telegram、Discord、Slack、WhatsApp、Signal 和 CLI，自带 cron，终端后端有 7 种可选。【官方】[README](https://github.com/NousResearch/hermes-agent)
- ① **连续性**：
  - MEMORY.md 上限 2200 字符，USER.md 上限 1375 字符。
  - **开局以冻结快照注入 system prompt，目的是保住前缀缓存**：本会话内写入的记忆立即落盘，但要到下一会话才可见。
  - 超限时 memory 工具直接报错，由 agent 自己合并精简，不做静默丢弃。
  - 【官方】[memory](https://hermes-agent.nousresearch.com/docs/user-guide/features/memory)
- ② **满窗**：会话存在 SQLite 里，会话键按「平台 + chat」区分。压缩时**新开一个续会话**，用 `parent_session_id` 串成链。【官方】[sessions](https://hermes-agent.nousresearch.com/docs/user-guide/sessions)
- ⑤ **隔离**：
  - 没配 allowlist 时默认拒绝所有人；也可以用 8 位配对码，1 小时过期。
  - 危险命令需要审批，另有一份 YOLO 模式也绕不过去的黑名单。
  - 【官方】[security](https://hermes-agent.nousresearch.com/docs/user-guide/security)

### 5.5 LangGraph / LangSmith Agent Server

- checkpointer 按 `thread_id` 保存状态快照。【官方】[persistence](https://docs.langchain.com/oss/python/langgraph/persistence)
- 满窗时可以 trim、delete，或用 `SummarizationMiddleware` 做摘要。【官方】[short-term memory](https://docs.langchain.com/oss/python/langchain/short-term-memory)
- cron 可以绑定到某个 thread，也可以每次新开 thread。【官方】[cron](https://docs.langchain.com/langsmith/cron-jobs)
- 只提供 REST 接口，没有 IM 渠道。【官方】[agent-server](https://docs.langchain.com/langsmith/agent-server)

### 5.6 记忆层与 IM 桥

- **Mem0**：分 User、Session、Agent 三级记忆，存储是向量加图。【官方】[mem0](https://github.com/mem0ai/mem0)
- **Zep / Graphiti**：时序知识图谱，事实过时时标记失效而不删除。【官方】[graphiti](https://github.com/getzep/graphiti)
- 两者都只解决 ①。
- **cc-connect**：一个进程把 Claude Code、Codex 等 10 多种 agent 接到飞书、钉钉、Telegram、Slack 等 13 个平台。【官方】[README](https://github.com/chenhg5/cc-connect)
  - 它保存 `agent_session_id`，空闲时杀掉 agent 进程，下一条消息到来时再 resume。
  - **`allow_from` 默认是 `"*"`**，这是安全上的反例。
  - 【官方】[config.example.toml](https://github.com/chenhg5/cc-connect/blob/main/config.example.toml)
- **claude-code-telegram**：按用户和项目目录持久化会话，用 `ALLOWED_USERS` 做白名单，访问范围限制在 `APPROVED_DIRECTORY` 内，支持 webhook 和 cron。【官方】[README](https://github.com/RichardAtCT/claude-code-telegram)

### 5.7 架构取舍小结

| 取舍 | 两种做法 | 结论 |
|---|---|---|
| **单进程 gateway 还是服务化** | 个人 agent（OpenClaw、Hermes、letta server、cc-connect）是一个常驻进程统管渠道、会话和调度；OpenHands、LangSmith 是 REST/WS 服务，每个会话一个容器 | 个人 agent 的信任边界就是一个操作者；要多租户，就多开几个实例 |
| **记忆放文件还是数据库** | 人可读、agent 可改的记忆放 Markdown 或 git（OpenClaw、Hermes、Letta MemFS）；转录和元数据放 SQLite | 趋势是两者并用 |
| **主会话还是每渠道会话** | OpenClaw 默认所有 DM 共用一个主会话，单人使用最连贯，多人会串上下文；Hermes、cc-connect 按 chat 分会话；Letta 用显式 route | — |
| **安全默认值** | IM 入口多数默认 pairing 或 allowlist | cc-connect 默认 `*`、Letta 的 Slack 默认 open 是反例；执行隔离一般只把工具放进容器 |

---

## 6. 对 iota 的启示：可复用的成熟模式

先对齐 iota 现状。以下来自本仓库文档，不需要外部来源：

- 会话是追加写的 JSONL，磁盘是唯一真相源，见 `docs/design/session-format.md`。
- compaction 只改内存中的视图，不删磁盘上的内容，见 `docs/design/context-compaction.md`。
- agent mode 的 AGENTS.md 和 skills 作为 send-time volatile overlay 注入，见 `docs/design/agent-mode.md`。
- 已有 `headless` 和 `host` 模块。

所以「不换上下文」的地基已经有了。bot 模式要补的是四样：**长生命周期、外部输入、主动唤醒、记忆沉淀**。

### 6.1 值得抄的模式

1. **一个 bot 就是一条永不结束的会话，会话文件是唯一真相源。**
   - 各家的共同做法：Codex 的 rollout JSONL + resume、Claude Code 的 jsonl + `--continue`、Hermes 和 OpenClaw 的 SQLite 会话。
   - iota 可以直接复用现有的 session bundle，给 bot 起一个稳定的名字（相当于固定的 session id），进程重启后原地接上。进程只是缓存，文件才是 bot 本体。
   - 这和 Grok Bot「named Bot keeps its memory … across sessions」的产品语义一致。

2. **压缩：保留最近的原文 + 带前缀的摘要 + 从磁盘重新注入规则。**
   - Codex 保留约 20k token 的用户消息原文，加一条带 `SUMMARY_PREFIX` 的摘要。
   - Claude Code 压缩后从磁盘重新注入 CLAUDE.md、memory 和 plan。
   - iota 的 overlay 本来就不进历史，天然满足「压缩后规则不丢」。
   - bot 会反复压缩，所以要显式区分「摘要消息」和普通消息，避免摘要里套摘要，越压越糊。

3. **压缩前先做 memory flush。**
   - OpenClaw 在 compaction 前插一轮，让 agent 把要点写进记忆文件。
   - Claude API 的 context editing 在清理前提醒模型写记忆。
   - 对长期运行的 bot 来说，这是防止「压掉关键事实」最便宜的保险。

4. **记忆分两层：常驻的小文件 + 按需检索的大文件，常驻层开局冻结。**
   - Hermes 的 MEMORY.md / USER.md 有硬上限，开局以冻结快照注入以保住前缀缓存，会话中途写入的内容下一轮才生效。
   - Claude Code 启动时只加载 MEMORY.md 头部，其余 topic 文件按需读。
   - OpenClaw 的 MEMORY.md 开局加载，日记按需检索。
   - 这和 iota 现有 overlay 的 mtime 缓存思路（字节不变就保住 cache）完全同构。超限时报错并让 agent 自己合并精简（Hermes 的做法），比静默截断可控。

5. **对话内定时 / heartbeat：往同一个会话注入一条「持久提示词」。**
   - ChatGPT 的对话内 scheduled task 回到原对话、沿用已有上下文，文档要求提示词写清每轮做什么、什么时候汇报、什么时候停。
   - OpenClaw 的 heartbeat 默认在主会话里跑；应答为 `HEARTBEAT_OK` 这类「无事」回复时不向用户发出。
   - Amp 的 automation 在原 thread 续跑。
   - 这是「持续不换上下文」最直接的主动能力。
   - 可以同时抄 OpenClaw 的 `isolatedSession` 开关：巡检类任务可以不污染主会话。

6. **外部事件推进「已经打开的会话」（channels 模式）。**
   - Claude Code 的 channels 以 MCP server 身份推送事件，事件以带来源标签的 `<channel source=…>` 块到达模型；必须按会话显式启用，写进配置不等于启用。
   - crush 已经兼容同一个 `claude/channel` 协议，它正在成为事实标准。
   - iota 已经有 MCP 客户端，实现这个协议能直接复用 Telegram、Discord、iMessage、webhook 等现成的 channel 插件，不用自己写 IM 适配。

7. **IM 入口默认配对加白名单，权限审批可以转发到 IM。**
   - OpenClaw、Letta、Hermes、Claude Code channels 都默认 pairing 或 allowlist。
   - Claude Code 明确警告：能在渠道里回话的人，就能审批工具调用。
   - cc-connect 默认 `*` 是反例。
   - iota 的 bot 模式应当默认拒绝所有人，靠配对码加人，并且把「谁能审批」和「谁能说话」分成两个名单。

8. **常驻核心与前端分离，TUI 只是其中一个客户端（中期）。**
   - opencode 的 serve/attach：OpenAPI + SSE，默认只监听 127.0.0.1，可选口令。
   - Codex 的 app-server daemon：socket 发现，TUI 自动挂上已在运行的 daemon，挂不上退回内嵌 server。
   - crush 也在往 `crush server` 走。
   - 对 iota 来说，「TUI 找得到 daemon 就挂上，找不到就内嵌」的退路最适合渐进引入：普通模式行为不变，bot 模式才起 daemon。

9. **后台托管进程，一个 bot 一个进程。**
   - Claude Code 的 `claude --bg` + `claude agents` 用 supervisor 托管，关掉终端照样跑，需要人介入时在列表里标出来。
   - iota 现有的 `host` 模块已经有 NeedsInput 状态和 attention ping 的语义，可以直接映射到「bot 在等你」。

10. **渠道共享记忆，但各渠道的可见对话隔离；授权分层。**
    - dots：同一个 dot 跨渠道共享笔记，但各渠道看到的对话独立，渠道、应用、电脑三项授权彼此独立。
    - OpenClaw 的 `dmScope` 也给了可选粒度。
    - 如果 iota 的 bot 同时接终端和 IM，建议默认「单一主会话、单一操作者」（最连贯），同时预留按渠道分会话的开关。

### 6.2 不适合终端 CLI 的模式

- **云端常驻计算机或 VM**（Grok Bot 的 Firecracker、dots、Amp Orbs、Cursor 和 Codex Cloud）：iota 没有后端，也不该有。「笔记本合上照样跑」这种能力只能靠用户自己的机器或服务器常开，文档应该如实写明这一点。
- **厂商中继的远程控制**（Claude Code Remote Control、Codex Remote、Amp Cross-Client）：要靠厂商账号体系和中继服务。iota 能做的是本地 socket 或 localhost HTTP，加可选的口令；远程访问交给用户自己的 SSH、Tailscale 或 IM channel。
- **不透明的服务端记忆**：ChatGPT 的全量画像注入、Gemini 的 Personal Intelligence、OpenAI Responses 加密的 compaction item。这些不可审计，也和 iota「磁盘可读、可 diff」的原则冲突。iota 应坚持用明文 Markdown 存记忆。
- **向量库或知识图谱记忆层**（Mem0、Zep）：对单用户终端工具来说太重，要引入服务或 embedding 依赖。先用文件加 grep 或关键词检索，够用再说。
- **多租户 gateway**：OpenClaw 自己都声明不防敌对多租户。iota 的 bot 应明确定位为「一个操作者、一个信任边界」。
- **agent 自主决定何时醒来**（dots 的 pause/wake）：依赖云端调度器，iota 本地没有等价物。能实现的近似是 agent 通过工具登记下一次唤醒时间，由 daemon 执行，但这应放在显式的 heartbeat/cron 之后再考虑。
- **后台自动挖掘历史对话写记忆**（Codex Memories、Letta sleep-time、OpenClaw Dreaming）：要额外花 token，还可能写错。如果要做，学 Gemini CLI 的 Auto Memory：生成候选、进收件箱、等人审批，不自动落库。

### 6.3 仍需自己验证的开放问题

- 多次压缩后信息衰减的程度：各家都没有公开评测。建议 iota 在自己的 fake provider 上做长跑实验。
- heartbeat 的 token 成本：OpenClaw 的对策是 `isolatedSession` 和 `lightContext`，说明这个成本是真问题，但没有公开数据，**未证实**。
- channels 协议在 research preview 期间可能变动（Claude Code 文档原话），iota 的实现需要一层适配。

---

## 附：未证实 / 有矛盾汇总

- Grok：
  - Bot 的平台范围：发布博客只写 Desktop 和 iOS，文档写 iOS 和 Android。
  - Bot 满窗机制：只有论坛上 staff 的一句话。
  - Grok Tasks 的限额与触发方式：没有官方页面。
  - @grok 的跨帖记忆。
  - Companions 的下线日期：只有媒体报道。
- OpenAI：
  - Codex Cloud 和 dots 在满窗时怎么处理。
  - ChatGPT App 长对话的截断策略。
  - ChatGPT 记忆当前是全量注入还是按需检索：官方措辞与逆向结论不一致。
  - ChatGPT Agent 是否已被 dots 取代。
- Cursor：本地会话的存储格式。
- opencode：本地常驻调度；存储是只有 SQLite，还是 SQLite 与 JSON 并存（文档与源码不一致）。
- crush：定时能力、沙箱。
- Amp：压缩阈值、本地是否逐条审批。
- Gemini：记忆检索机制、满窗处理。
- OpenHands：多用户数据隔离。
- 本次未调研：AutoGPT platform、n8n AI agent。
