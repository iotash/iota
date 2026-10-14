# 「进行中」指示器的跨度：别家怎么做，iota 的 busy 怎么覆盖整轮

Status: **已定（2026-10-14，项目所有者：第 5 点 B，其余全 A）· 未实现** · 日期：2026-10-14 · 基线：main `a7f142f` · 分支 `busy-audit`

所有者的原话：「执行期状态行应该也加吧，理想的状态是这个 busy 状态从我发送指令开始，一直到它所有的输出完毕以及 jobs 结束才停止，先调研下其它的 coding agent」。

需求拆成两条：

1. 工具执行期间，状态行也要有 busy。这一条所有者已经拍板，推翻了同分支审计里「不加」的建议。
2. 理想跨度：从按下发送开始，到所有输出完毕，并且 jobs 结束，才停。

本文做三件事：§1 别家怎么做（每家回答七问）；§2 iota 现状（直接引用同分支的 busy 审计）；§3 提案与拍板结果。**不含实现，不改代码。**

**最终形状（2026-10-14 定）**：轮内有三层——状态行的整轮层（从确认发送到这一轮落地）、转录区的当前活动层（widget）、状态行末尾的 jobs 段；**轮一结束，整轮层就消失，轮外只剩静态的 jobs 段**（不加 spinner）。需求第 2 条里的「jobs 结束才停」没有采纳到屏幕上：所有者在 §3.7 第 5 点选了 B，原话「5 跟其它大多数 agent 保持一致」。host 层（OSC 9;4、herdr）在 job 运行期间仍报 Busy，不变（§3.3）。

## 写作约定

- 证据标记：
  - **【官方】**：厂商文档、仓库源码、官方 changelog。
  - **【本机实测】**：本机装的二进制，包括跑 `--help`、`strings` 抽取，以及 iota 在私有 tmux 里实跑。
  - **【包内源码】**：下载官方发布包后读其中打包好的 JS（没有安装、没有运行）。
  - **【推断】**：本文自己的推理。
  - **【查不到】**：官方资料没说或无法核实，不用常识补。
- 本机装了 **Claude Code 2.1.299** 和 **Codex CLI 0.162.1**。其余几家都没装，只看源码、文档或发布包。
- 源码行号都钉在具体提交上，提交号写在各节标题里。链接在 2026-10-14 打开。
- iota 的代码坐标指 `a7f142f`。

每家回答同样七问：

| 编号 | 问题 |
|---|---|
| ① | 有没有「进行中」指示器，画在哪 |
| ② | 什么时候开始：按回车、请求发出，还是首 token |
| ③ | 什么时候结束：(a) 工具执行中还在不在；(b) 等用户输入时消不消失；(c) 后台进程还在跑时消不消失，有没有独立的 jobs 指示；(d) 一轮结束后界面上还剩什么 |
| ④ | 有没有计时，在哪 |
| ⑤ | 可打断提示在哪，和指示器是不是同一个元素 |
| ⑥ | 「N 个后台任务」这类聚合，和主指示器什么关系 |
| ⑦ | 出处 |

---

## 1. 别家怎么做

### 1.0 一览（边界对照）

| 工具 | ① 位置 | ② 起点 | ③a 工具执行中 | ③b 等用户 | ③c 后台在跑、轮已结束 | ④ 计时 | ⑤ 中断提示 | ⑥ 后台聚合 |
|---|---|---|---|---|---|---|---|---|
| Claude Code 2.1.299 | 输入框上方的 spinner 行，另有终端标题、OSC 9;4 | 主循环一忙就出现（提交后），不等首 token | **在**；工具另有自己的行 `Running… (12s · timeout 2m)` | 隐藏（系统弹框打开时） | spinner **消失**；footer 显示 `N background` 药丸；OSC 9;4 **保持** | spinner 行有，扣除暂停；16 s 后才显示 | footer，**不同元素** | footer 药丸，与 spinner 独立 |
| Codex CLI 0.162.1 | 输入框上方的 status 行 `• Working (12s • esc to interrupt)` | `TurnStarted` 通知到达，不是按回车 | **在**；exec cell 自带闪烁 `•`，无计时 | 隐藏，计时暂停 | 主行**消失**；单独一行 `N background terminal(s) running · /ps … /stop …` | status 行，可暂停 | **同一元素**（同一对括号） | 轮内拼在主行后面，轮外单独一行 |
| Gemini CLI | 输入框上方 StatusRow `⠋ Thinking... (esc to cancel, 12s)` | 提交并预处理后、请求前 | **在**；工具行另有 spinner | 隐藏，计时冻结 | 主行**消失**；上下文摘要里有 `N Background process(es)` | 主行，只在 Responding 时走 | **同一元素** | 常驻摘要，与主行独立 |
| Aider | 光标行上的 `\r` spinner `Waiting for <model>` | 请求发出前（0.5 s 后才显示） | **没有**（首 token 就停） | 没有 | 没有后台概念 | 无 | 无（Ctrl-C 后才提示） | 无 |
| opencode | 输入框下方 footer 左侧 spinner + `esc interrupt` | 服务端 `session.execution.started` | **在**；工具块自带 spinner | 隐藏（权限框替换整个 prompt） | 主 spinner **消失**；footer 右侧 `N subagents · N shells`；工具块保留 spinner 加 `Background` 徽章 | 进行中无，完成后显示时长 | **同一行** | footer 右侧，与 spinner 并列 |
| Crush | 对话流里的回合动画（带计时）+ busy 期间的输入框占位文字 + 帮助栏 `esc cancel` | 提交即开始，早于请求 | 回合层（占位文字、`esc`、OSC）**在**；带计时的动画在出正文或工具调用时停【推断：执行期无动画】 | 回合层仍在，工具显示 `Requesting permission...` | 回合层**消失**；只剩静态 `Job (Start)` 记录 | 回合动画后缀 | 帮助栏，**不同元素** | 无后台聚合（只有 `N Queued`） |
| Copilot CLI 1.0.97 | 输入框下方的提示行，被 `ThinkingAnimation` 替换 | 提交时（发送前） | **在**；标签换成模型的 intent，否则 `Working` | 隐藏（权限框替换输入区） | **保持**，标签改为 `Waiting for background agents/shells`，直到 `session.idle` | 主指示器无 | **同一行** | 空闲时提示行显示 `N background /tasks`，与指示器共用一行、互斥 |
| Cursor CLI 2026.10.14 | 输入框上方 topStatus `⠋ Thinking 1.2k tokens` | 提交时（`isSubmitting`） | **在**；标签换成工具动词 | 提问类弹框时隐藏；普通审批时【查不到】 | 主指示器**消失**；prompt bar 有 `N tasks` | 主指示器无；Tasks 面板有 | 输入框内右侧 `ctrl+c to stop`，**不同元素** | prompt bar，与主指示器可同时出现 |
| Amp | 状态标签集 `Sending`…`Running Tools`…【包内字符串】 | 【推断】提交即开始 | 有 `Running Tools` 标签【推断：在】 | 有 `awaiting_approval` 状态【推断：改标签】 | 【查不到】 | 【查不到】 | 文档说 Esc Esc 打断；屏幕上画不画【查不到】 | 【查不到】 |

**「整轮 + 后台」一起算的先例只有一家：Copilot CLI。** 它的指示器在轮结束后继续保持，标签变成 `Waiting for background shells`，一直到后台跑完。其余几家都是：轮一结束主指示器就消失，后台由一个**独立的、静态的计数段**接手。Claude Code 是半个例外：它的屏幕 spinner 会消失，但终端标签页的 OSC 9;4 进度一直保持到后台 agent/workflow 结束（2.1.261 的修复）。

**把「整轮」和「当前活动」分成两层的：** Claude Code、Codex、opencode、Gemini CLI（单行主指示器加工具行自己的 spinner），Crush 是另一种拆法。Copilot 和 Cursor 只有一行，「当前活动」体现为标签替换。§1.10 画了形状。

### 1.1 Claude Code（本机 2.1.299）

取证：`strings -n 6 ~/.local/share/claude/versions/2.1.299`，再在抽出的文本里按正则截取上下文。没有跑真实模型。CHANGELOG 来自 https://raw.githubusercontent.com/anthropics/claude-code/main/CHANGELOG.md （版本号按 `## 版本` 标题对应）。

- ① **共四处。**
  - spinner 行，在转录区和输入框之间。【本机实测】
  - 终端标题的 busy 字形：2.1.228 "Updated terminal title busy-spinner glyphs"。【官方】
  - OSC 9;4 标签页进度：设置 `terminalProgressBarEnabled` 的描述是 "Emit OSC 9;4 progress sequences during long operations"。【官方】
  - OSC 7501（2.1.295 起）："show whether Claude Code is working, waiting on you, or done"。【官方】
- ② **提交后主循环一忙就出现，不等首 token。**
  - 显示条件是 `oc.isLoading`，它取自 `ns.isMainLoopBusy||…getMainThreadQueueLength()>0`。【本机实测】
  - 模式依次是 `["tool-input","tool-use","requesting","responding","thinking"]`，其中 `stream_request_start` 进入 `"requesting"`。【本机实测】
  - 是否就在按 Enter 的那一刻出现：精确时点【查不到】。
- ③a **工具执行期间 spinner 在，进入 `"tool-use"` 模式。**
  - 开了 `showToolCallTimer` 且工具跑满 2 s 后，状态文本变成 `running tool for …`（`if(u>=2000)return{kind:"tool-running"…}`），这段文本里没有工具名。【本机实测】
  - 工具名和计时在工具自己的转录行里：`` `(${a} · timeout ${f})` ``。2.1.210 起加了 "a live elapsed-time counter to the collapsed tool summary line"。【本机实测】【官方】
  - 相关修复：2.1.152 "Fixed the loading spinner showing "still thinking"… while a tool is running"；2.1.179 "the spinner no longer gets stuck at "running tool""。【官方】
- ③b **系统发起的对话框打开时，spinner 隐藏。**
  - 条件：`TS=!Hc&&!Ym&&!vp&&oc.isLoading&&!ps&&(!pw||fw||Bs||rq)`。其中 `Ym` = 打开的弹框里有非用户发起的；弹框种类包括 `permission_prompt`、`permission_ask_user_question`。【本机实测】
  - 状态机里「等用户」单独一态：`{kind:"awaiting_user"}`。【本机实测】
  - 排除集合 `FM` 的成员没解出来，所以「权限提示一定隐藏 spinner」只到【推断】。
- ③c **spinner 只看主循环，不管后台。**
  - 后台任务用 footer 药丸显示：`` `${C} ${h?"bg":"background"}` ``，需要用户处理时带一个圆点。【本机实测】
  - Ctrl+B 绑定 `"task:background"`，文档原文："Backgrounds Bash commands and agents"，见 https://code.claude.com/docs/en/interactive-mode 。【官方】
  - 后台 workflow 在输入框下方有自己的一行（2.1.281 "shows the name, a progress bar, … elapsed time, total tokens"）。【官方】
- ③d **一轮结束后还剩：**
  - footer 的 `N background` 药丸，以及 agent/workflow 行。
  - OSC 9;4 保持：2.1.261 "Fixed the terminal progress indicator … showing the session as finished while a background workflow or agent was still running"。【官方，本文已核原文】
  - 开了 `showTurnDuration` 时，还有一行 "Cooked for Nm Ns"。
  - 另：2.1.128 "Fixed terminal progress indicator (OSC 9;4) flickering off between tool calls — stays visible across the full turn"。【官方，本文已核原文】
- ④ **spinner 行有计时，暂停时间扣掉。**
  - 算法是 `now − loadingStartTime − totalPausedMs`。【本机实测】
  - 有显示门槛：`elapsed>16000`，或已有 token、thinking 等内容时才出现。【本机实测】
  - 工具行另有自己的计时。
- ⑤ **中断提示在 footer，和 spinner 不是同一个元素。**
  - 代码：`else if(ee){…Uce({chord:wt,action:"interrupt"})}`，其中 `ee` 就是 `isLoading`。忙时这个位置显示中断，闲时显示 shift+tab 的 cycle 提示。【本机实测】
  - 2.1.169 "Fixed footer hints (e.g. "esc to interrupt") not showing for users with a custom statusline"。【官方】
- ⑥ **footer 的 `N background`（窄屏 `N bg`）和 spinner 互相独立。**
  - 计数来自任务表。【本机实测】
  - 2.1.3 修过 "mismatch between background task count in status bar and items shown in tasks dialog"。【官方】
  - 「N background tasks running」原文【查不到】。
- **两层：是。** 整轮由 spinner 行表示，当前工具由它自己的转录行表示，各自有计时；后台是第三层，在 footer。

```
⏺ Bash(npm test)                                   ← 转录区：当前工具，自带计时
  ⎿  Running… (12s · timeout 2m) (ctrl+b to run in background)
✻ Thinking… (34s · ↓ 1.2k tokens · thinking)      ← spinner 行：整轮
╭ > ──────────────────────────────────────╮
  esc to interrupt · 2 background                  ← footer：中断提示 + 后台计数
```

### 1.2 OpenAI Codex CLI（本机 0.162.1 = tag `rust-v0.162.1` = `092d3acd`）

下文的 `B` = https://github.com/openai/codex/blob/092d3acd6bec3e3a14bdc7e7a2810ab628ab759d/codex-rs/tui/src 。

- ① **有，是 composer 上方的一行 status。**
  - 文件头注释："A live task status row rendered above the composer while the agent is busy."（`B/status_indicator_widget.rs#L1-L7`），header 默认 "Working"（`#L97-L99`）。
  - 拼法（`#L222-L268`）：动画 `•`、header、`({elapsed} • esc to interrupt)`、可选的 ` · inline_message`；下面可以跟最多 3 行 `└ details`（`#L45-L46`）。【官方】
  - 终端标题有 Working / Waiting / Thinking / Ready 四态，只在任务运行时转 spinner（`B/chatwidget/status_surfaces.rs#L1007-L1023`）。OSC 9;4【查不到】。
- ② **由 `TurnStarted` 通知点亮，不是按回车。**
  - 按 Enter 只置 `user_turn_pending_start = true`（`B/chatwidget/input_submission.rs#L498-L500`）。
  - `on_task_started()` → `set_task_running` → `ensure_status_indicator`，计时归零（`B/chatwidget/protocol.rs#L103-L106`、`B/chatwidget/turn_runtime.rs#L77-L115`）。
  - 例外：`/compact` 和 review 是本地立即显示的（`B/chatwidget/slash_dispatch.rs#L338-L348`）。【官方】
  - **首 token 到达后它会被隐藏**：`hide_status_indicator()`，注释 "hide the inline status row so transcript content replaces it"（`B/chatwidget/streaming.rs#L639`）；commentary 结束而轮还在跑时恢复（`#L170-L185`）。【官方】
- ③a **在。** `command_lifecycle.rs#L256-L257` 注释："Ensure the status indicator is visible while the command runs."
  - header 不换成命令名。
  - exec cell 在转录区有自己的闪烁 `•`/`◦` 和 "Running" 标题（`B/exec_cell/render.rs#L203-L210`），运行中**没有计时**。
  - 例外：agent 轮询后台终端时，header 改成 "Waiting for background terminal"，details 显示命令（`B/chatwidget/command_lifecycle.rs#L90-L104`）。【官方】
- ③b **隐藏。** view_stack 非空时，只画栈顶的 view，status 行和 footer 都不画（`B/bottom_pane/mod.rs#L2223-L2228`）。approval、request_user_input、MCP elicitation 入栈时调 `pause_status_timer_for_modal()`（`#L795`、`#L1880`、`#L1907`）。【官方】
- ③c/③d **轮结束后主行消失。**
  - `set_task_running(false)` → `hide_status_indicator()`（`B/bottom_pane/mod.rs#L1352-L1355`）。
  - 改为单独一行 footer：`"{count} background terminal{plural} running · /ps to view · /stop to close"`（`B/bottom_pane/unified_exec_footer.rs#L45-L67`），只有计数，没有名字、计时和 spinner。
  - 转录区留一条 "Worked for …" 分隔。
  - `on_task_complete` 清 `running_commands`，但不清 `unified_exec_processes`（`B/chatwidget/turn_runtime.rs#L181-L185`）。【官方】
- ④ **计时在 status 行括号里，是独立的 `StatusTimer`。**
  - 注释 "Pause-aware status time owned independently of the optional status row"（`B/status_indicator_widget/timer.rs#L1-L57`）。
  - modal 出现时暂停；流式输出把这一行隐藏时，计时不停。【官方】
- ⑤ **同一元素、同一对括号**（`B/status_indicator_widget.rs#L246-L253`）。inline 消息刻意排在它后面，"so that core interrupt affordances stay in a fixed visual location"（`#L255-L259`）。【官方】
- ⑥ **轮内拼进主行，轮外单独一行，二者互斥。**
  - 轮内作为 inline 后缀：`• Working (0s • esc to interrupt) · 1 background terminal running · /ps to view · /stop to close`（`B/bottom_pane/mod.rs#L1688-L1693`）。
  - 没有主行时单独一行（`#L2255-L2261`）。【官方】
- ⑦ **本机实测**：`strings` 在 codex 二进制里各找到 1 次 "Waiting for background terminal" 和 " to interrupt)"。官方文档没讲指示器和 `/ps`、`/stop`【查不到】。

```
  • Running cargo test            ← exec cell：当前命令（转录区，自带闪烁，无计时）
    └ …output tail
• Working (12s • esc to interrupt) · 1 background terminal running · /ps to view · /stop to close
› composer
──── 轮结束后 ────
  1 background terminal running · /ps to view · /stop to close
› composer
```

### 1.3 Gemini CLI（`google-gemini/gemini-cli@42c51822`）

下文的 `G` = https://github.com/google-gemini/gemini-cli/blob/42c51822084ac25cd5d97ac0023822c5235f88c8/packages/cli/src 。

- ① **`LoadingIndicator` 画在输入框上方的 `StatusRow` 里。**
  - 位置：`G/ui/components/StatusRow.tsx#L144-L158`。
  - 内容：spinner、主文案（thought subject / `'Thinking...'` / 重试文案）、`(esc to cancel, Ns)`（`G/ui/components/LoadingIndicator.tsx#L67-L130`）。
  - 终端标题也跟着变：`'⏲  Working…'`、`'✋  Action Required'`、`'◇  Ready'`（`G/utils/windowTitle.ts#L58-L79`）。【官方】
- ② **提交、预处理（斜杠命令和 @ 文件）之后，请求发出之前**：`setIsResponding(true)`（`G/ui/hooks/useGeminiStream.ts#L1904-L1929`）。【官方】
- ③a **在。** 工具处于 Executing/Scheduled/Validating，或已完成但结果还没交回模型，都算 `Responding`（`G/ui/hooks/useGeminiStream.ts#L186-L224`）。工具行另有一个 spinner（`G/ui/components/messages/ToolShared.tsx#L162-L167`），两个同时转。【官方】
- ③b **隐藏，计时冻结。**
  - 等确认时状态是 `WaitingForConfirmation`，显示条件要求 `Responding && !hasPendingActionRequired`（`G/ui/hooks/useComposerStatus.ts#L53-L56`）。
  - 冻结的值是 `retainedElapsedTime`，回到 Responding 时清零重计（`G/ui/hooks/useLoadingIndicator.ts#L64-L84`）。【官方】
- ③c/③d **后台进程不让主行继续亮。**
  - 移到后台的 shell，工具调用以 Success 结束，输出 "Command moved to background (PID: …)"（`G/ui/hooks/useExecutionLifecycle.ts#L576`），轮进入 Idle。
  - 轮结束后只剩上下文摘要里的 `N Background process(es)`，加上 Ctrl+B 打开的面板。【官方】
- ④ 主行括号里，`useTimer` 每秒加 1，只在 Responding 时走（`G/ui/hooks/useTimer.ts#L15-L65`）。【官方】
- ⑤ **同一元素** `` `(esc to cancel, ${…})` ``，确认态没有（`G/ui/components/LoadingIndicator.tsx#L78-L81`）。【官方】
- ⑥ `` `${backgroundProcessCount} Background process…` ``（`G/ui/components/ContextSummaryDisplay.tsx#L97-L103`），在常驻的上下文摘要里，与主行独立。【官方】

```
⠋ Thinking... (esc to cancel, 12s)        ← StatusRow：整轮（确认态隐藏）
… 2 MCP servers | 1 Background process    ← 上下文摘要：常驻
```

### 1.4 Aider（`Aider-AI/aider@5dc9490b`）

下文的 `A` = https://github.com/Aider-AI/aider/blob/5dc9490bb35f9729ef2c95d00a19ccd30c26339c/aider 。

- ① **单行 `\r` 扫描式 spinner，写在光标行上**（`A/waiting.py#L144`）。
  - 主请求的文案：`WaitingSpinner("Waiting for " + self.main_model.name)`（`A/coders/base_coder.py#L1440-L1441`）。
  - 提交信息、repo map 各有自己的 spinner。【官方】
- ② **请求发出前**（`A/coders/base_coder.py#L1430-L1441`）；0.5 s 后才显示（`A/waiting.py#L113`）；非 TTY 不画。【官方】
- ③ **第一个内容 chunk 到达就停**（`A/coders/base_coder.py#L1954-L1955`）。
  - (a) shell 命令在 spinner 停止之后同步执行，输出逐字符打出，没有指示（`A/coders/base_coder.py#L2473-L2476`、`A/run_cmd.py#L76-L82`）。
  - (b) 确认发生在 spinner 停止之后。
  - (c)(d) 没有后台作业的概念。【官方】
- ④⑤⑥ 无计时；中断提示只在按了 Ctrl-C 之后打印 `"^C again to exit"`（`A/coders/base_coder.py#L986-L1000`）；无聚合。【官方】
- 文档里讲指示器的部分【查不到】。

### 1.5 opencode（`anomalyco/opencode@165cd2e8`）

下文的 `O` = https://github.com/anomalyco/opencode/blob/165cd2e832574ccdea3b311fe1a0f9db2e5c883e/packages 。当前 TUI 在 `packages/tui/src`，会话状态只剩 `"idle" | "running"` 两态。

- ① **输入框下方 footer 左侧**：`status() === "running"` 时显示 spinner 加 `PromptInterruptStatus`（`O/tui/src/component/prompt/index.tsx#L1888-L1904`）。【官方】
- ② **服务端事件 `session.execution.started` 到达时**置 running（`O/client/src/solid/data.ts#L1052-L1053`）。
  - TUI 没有提交即乐观置位的代码。
  - 这个事件相对「请求发出 / 首 token」在什么时点发出：【查不到】（没读服务端源码）。
- ③ `succeeded/failed/interrupted` 之后置 idle（`O/client/src/solid/data.ts#L1068-L1071`）。
  - (a) **在**；工具块各有 spinner（`O/tui/src/routes/session/index.tsx#L2583-L2596`）。工具跑满 3 s 出提示 "Press <key> to move running work to the background"（`#L1778-L1808`、`#L138`）。
  - (b) **隐藏**：`PermissionPrompt` 替换整个 `<Prompt>`（`#L1496-L1527`）。
  - (c) 工具已完成而 shell 还在跑时，工具块保留 spinner，加徽章 `"Background"`（`#L2833`、`#L2987-L2989`）。
  - (d) 主 spinner 随 idle 消失【推断】；footer 右侧留 `"N subagents · N shells"`（`O/tui/src/feature-plugins/prompt/footer.tsx#L20-L33`、`#L67-L85`）。【官方】
- ④ **进行中无计时**；完成后助手消息尾部显示时长和 tok/s（`O/tui/src/routes/session/index.tsx#L1908`、`#L1931-L1932`）；重试时有倒计时（`#L2389-L2408`）。【官方】
- ⑤ **同一行**：`esc interrupt`，按一次后变成 `esc again to interrupt`，5 s 内按两次才真正打断（`O/tui/src/component/prompt/index.tsx#L108-L145`、`#L514-L527`）。【官方】
- ⑥ footer 右侧的计数，只有数量，和主 spinner 同一行、各自独立。【官方】

```
transcript │   ⠋ Reading file… src/a.ts              ← 工具级 spinner（活动层）
           │   ⠋ npm run dev   [Background]           ← 轮结束后仍转
footer     │ ▮▮▯ esc interrupt          1 subagent · 2 shells · 12K (6%)
           │ └ 回合层：running 才有 ┘    └ 后台聚合：idle 后仍在 ┘
```

### 1.6 Crush（`charmbracelet/crush@a48dabea`）

下文的 `C` = https://github.com/charmbracelet/crush/blob/a48dabea1a3562b24c39cc7f0dec10e867f8499b/internal 。

- ① **有好几处：**
  - 对话流里的回合动画，带计时后缀，标签 `"Thinking"` / `"Summarizing"`（`C/ui/chat/pending.go#L42-L59`、`C/ui/chat/assistant.go#L768-L775`）。
  - 每个工具自己的动画，无计时（`C/ui/chat/tools.go#L204-L211`）。
  - busy 期间输入框的占位文字 `"Working..."` 等（`C/ui/model/ui.go#L1608-L1609`、`#L5107-L5113`）。
  - 帮助栏 `esc cancel`（`#L3888-L3897`）。
  - OSC 7501 Working/Blocked（`C/ui/model/program_status.go#L17-L38`）。【官方】
- ② **提交即开始，早于请求**：`common.StartTurn` 加 `showPendingTurn`。排队的提示不另起时钟："A queued prompt … keeps the running turn's spinner and clock"（`C/ui/model/ui.go#L5312-L5328`）。【官方】
- ③ 回合级在 `RunComplete` 时结束（`C/ui/model/ui.go#L876-L877`）。回合动画在出正文或工具调用时就停（`C/ui/chat/assistant.go#L799-L805`）。
  - (a) 参数生成完毕后，工具行只显示静态的 `"Waiting for tool response..."`（`C/ui/chat/tools.go#L551-L552`）。所以执行期对话流里没有动画【推断】；占位文字、`esc`、OSC Working 仍在，因为 `IsBusy` 看的是 `activeRequests`（`C/agent/agent.go#L2179-L2193`）。
  - (b) 工具显示 `"Requesting permission..."`，OSC 报 Blocked（`C/ui/model/program_status.go#L20-L23`）。
  - (c)(d) `bash` 支持 `run_in_background`，跑满 `auto_background_after`（默认 60 s）自动转后台（`C/agent/tools/bash.go#L29-L30`、`#L375-L381`）。回合结束后回合层全部消失，只剩一条静态的 `Job (Start)` 记录；`internal/ui` 里找不到持续的作业指示。【官方 + 推断】
- ④ 回合动画的后缀 `common.Elapsed(sessionID)`，从提交算起（`C/ui/common/timer.go#L9-L58`）。【官方】
- ⑤ **帮助栏，不同元素**；按一次后变成 `"press again to cancel"`（`C/ui/model/keys.go#L205-L208`）。【官方】
- ⑥ 后台作业聚合【查不到】；只有排队数 `"%d Queued"`（`C/ui/model/pills.go#L50-L61`）。
- 讲指示器的文档【查不到】。

### 1.7 GitHub Copilot CLI（发布包 1.0.97；changelog 1.0.96 及以前）

包内源码：npm `@github/copilot-darwin-arm64@1.0.97`，从原生二进制的 SEA blob 里解出内嵌包，读其中的 `cli-main.js`。没有安装、没有运行。Changelog：https://raw.githubusercontent.com/github/copilot-cli/main/changelog.md 。

- ① **`ThinkingAnimation` 作为 `activityIndicator`，替换输入框下方的提示行。**
  - 文字取值：`e.mode==="executing"?"Executing":e.mode==="cancelling"?"Cancelling":e.currentIntent??"Working"`；流式时追加 `· {bytes}`。【包内源码】
- ② **提交时**：`q.send` 之前就置 busy；`assistant.turn_start` 再置一次。【包内源码】
- ③a **在**；标签是模型给出的 intent，否则 "Working"。【包内源码】
- ③b **隐藏**：有权限请求时，整个输入区被权限对话框替换。【包内源码】
- ③c **保持**：
  - `assistant.idle` 之后，标签改成 `"Waiting for background agents"` 或 `"Waiting for background shells"`；只有 `session.idle` 才清掉。【包内源码】
  - 1.0.20（2026-04-07）："Spinner stays active until background agents and shell commands finish, and user input remains available throughout"。【官方，本文已核原文】
  - 1.0.86（2026-09-17）："The status row now says it is waiting for background shells, instead of "Working", when a turn ends while an attached background shell such as a dev server is still running."。【官方，本文已核原文】
- ③d detached 的后台任务改由提示行的计数显示（见 ⑥）。
- ④ **主指示器无计时**；子 agent 的时间线条目有（1.0.87）。【包内源码 + 官方】
- ⑤ **同一行**，紧跟右侧：`esc` 加 `interrupt` / `stop agents`，后面可接 `· enqueue`。1.0.36（2026-04-24）："Require double Esc to cancel in-flight work"。【包内源码 + 官方，本文已核原文】
- ⑥ `` `${m} background /tasks` ``，只算 `running` 且 `background` 的任务。它在提示行里，和指示器共用一行位置、互斥。【包内源码 + 推断】

```
轮进行中:          ◐ Working · 12.3 KB                 esc interrupt · ⏎ enqueue
模型已停、后台在跑: ◐ Waiting for background shells      esc stop agents
空闲:              interactive · 2 background /tasks
```

### 1.8 Cursor CLI（发布包 `2026.10.14-0f9edde`）

包内源码：从 https://cursor.com/install 得到 `https://downloads.cursor.com/lab/2026.10.14-0f9edde/darwin/arm64/agent-cli-package.tar.gz`，读 `dist-package/9627.index.js`、`1591.index.js`。

- ① **输入框上方的 topStatus 行**：spinner、粗体标签、灰色 token 数。
  - 标签：`ur?"Thinking":fn?"Summarizing":hr??Un`，其中 `hr` 是最近一次工具调用的动词，`Un` 是 `"Composing"` 或 `"Working"`。【包内源码】
- ② **提交时**：条件是 `isGenerating||isSummarizing||isSubmitting`。【包内源码】
- ③a **在**，标签换成工具动词。【包内源码】
- ③b 有 `hasPromptDialog`（askQuestion / switchMode / sessionDecision）时返回 null。普通工具审批不在这个条件里，审批时是否显示【查不到】。【包内源码】
- ③c/③d 轮结束后主指示器消失，prompt bar 显示 `N tasks`。后台任务完成会触发新的一轮，时间线里显示 "Finished · N background tasks"。【包内源码】
- ④ 主指示器无计时；Tasks 面板里选中的运行项每秒刷新时长。【包内源码】
- ⑤ **不同元素**：输入框内右侧 `rightPlaceholder:Me?"ctrl+c to stop":void 0`。【包内源码】
- ⑥ `` `${ln} task${…}` ``，在 prompt bar 标题行（如 `Plan · 2 tasks`），每 500 ms 轮询；和主指示器可同时出现。【包内源码】
- 文档 https://cursor.com/docs/cli/shell-mode 只写了 "cancel with Ctrl+C"，指示器本身【查不到】。

### 1.9 Amp（发布包 `@ampcode/cli-darwin-arm64@0.0.1791950451-g165091`）

Bun 字节码，读不出逻辑，只能拿到字符串常量。

- ① 状态标签集：`Connecting`、`Preparing Thread`、`Auto-Compacting`、`Sending`、`Waiting`、`Thinking`、`Streaming`、`Running Tools`、`Cancelling`；状态键有 `awaiting_approval`、`running_tools`、`waitingForActorElapsed`。【包内字符串】画在哪、怎么渲染【查不到】。
- ② 有 `Sending` 标签，所以【推断】请求发出时已经在显示。
- ③a 有 `Running Tools` 标签，【推断】在。③b 有 `awaiting_approval`，【推断】改标签。③c/③d【查不到】。
- ④ 只有 `waitingForActorElapsed`，界面上显不显示【查不到】。
- ⑤ 文档原文 "hit Esc Esc to interrupt the agent and send immediately"（https://ampcode.com/news/neo ，2026-05-06）；屏幕上画不画【查不到】。
- ⑥【查不到】。

### 1.10 共性与分歧（本文的归纳）

1. **起点。** 多数在提交时就亮：Claude Code、Gemini、Crush、Copilot、Cursor。Codex 要等 `TurnStarted`，opencode 要等服务端事件，Aider 在请求发出前。**没有一家等首 token。**
2. **工具执行期。** 除 Aider（以及执行期没有动画的 Crush 对话流）外，**主指示器都在**。当前是哪个工具，有两种表达：
   - 主行标签换成工具名或动词：Cursor、Copilot（intent）；Codex 只在轮询后台终端时换。
   - 主行保持通用词，工具名放在转录区自己的行里：Claude Code、Codex、Gemini、opencode。
3. **等用户。** **全部隐藏主指示器**：Claude Code、Codex、Gemini、opencode、Copilot，原因都是弹框替换了那块区域。Codex 和 Claude Code 把这段时间从计时里扣掉，Gemini 冻结后再清零。
4. **轮结束后后台还在跑。** 只有 **Copilot 让主指示器继续**，并把标签换成 "Waiting for background …"。其余家都切成一个**静态计数段**（Codex 单独一行、Gemini 摘要、opencode footer 右侧、Cursor prompt bar、Claude Code footer 药丸），都没有 spinner，只有数量（Codex、Gemini、opencode、Claude Code 都不显示名字或计时）。Claude Code 的 OSC 9;4 标签页进度则一直保持到后台结束。
5. **中断提示。** 和主指示器是同一元素的：Codex、Gemini、opencode、Copilot。不同元素的：Claude Code（footer）、Crush（帮助栏）、Cursor（输入框内）。**只要主指示器在，中断提示就在**，二者生死相同。
6. **计时。** 主指示器带计时的：Claude Code、Codex、Gemini、Crush。不带的：opencode、Copilot、Cursor。**计的都是整轮，不是当前阶段**（Gemini 例外：确认后清零）。

---

## 2. iota 的现状

底稿是同分支的 busy 审计（2026-10-14，审计结论在本会话的汇报里，没有另存文档）。本节只列提案要用到的部分。

### 2.1 状态行 busy：只覆盖「等模型」

- **数据**：`BusyView { label, detail, since }`（`src/ui/render/frame.rs:55-63`）。
  - `UiMsg::BusyOn` 每次都把 `since` 重置为现在（`src/ui/runtime/event_loop.rs:534-543`），所以**计时是阶段计时，不是整轮计时**。
  - `BusyOff` 无条件清空（`:552-555`），**不计数、不分层**。同时只能有一个 busy。
- **升降点**（审计第 1 节）：
  - 每个流式轮次开头，`RenderSink::new` 设 `PHASE_WAITING`（`src/repl/turn/mod.rs:768-769`）。
  - `watch_phases` 在三个标签之间切换：Sending request（≥128 KiB，`src/repl/turn/phases.rs:21`）、Waiting for the first token、退回 Waiting（`:91-118`）。
  - **第一个正文字节**（`mod.rs:863`）、**第一个推理字节**（`:878`）、**第一个带名字的工具参数 delta**（`:929`）都会 `phases.end()`；`finish()` 兜底（`:823`）。
  - 重试的退避期间有 busy（`src/repl/turn/retry.rs:50-62`）。
  - `/model` 列模型期间有 busy（`src/repl/catalog.rs:422-429`）。
- **渲染**：
  - 状态行段序 model · tokens · ctx · debug · **busy** · jobs（`frame.rs:300-322`）。
  - 用时 ≥ 2 s 才显示计时（`BUSY_ELAPSED_MIN`，`frame.rs:45`、`:293`）；有取消作用域时追加 `(ESC to cancel)`（`:296-298`）。
  - 底部区只放一个，优先级 surface > desc > status（`event_loop.rs:903-907`），所以审批框或补全描述行出现时，busy 被整行替换（审计第 4 节的帧）。
  - 只有 inline 一种模式，`src` 里查不到全屏模式。
- **工具执行期从不升**：`walk()`、`surface_call`、`run_parallel_batch`（`src/repl/turn/tools.rs:187-395`）都不调用 `ui.busy`；`src/tool/` 和 `src/mcp/` 里 grep `.busy(` 也是零。代码注释写明这是有意的：「the thinking widget takes over from the status spinner」（`mod.rs:878`）、「the widget takes over from the status spinner」（`mod.rs:929`）。

### 2.2 转录区 widget：覆盖「当前活动」

- **数据**：region 的预览标签加 `since`（`src/ui/render/frame.rs:131-168`）。渲染成 `⠦ <label>`，下面一行是 `⎿ <detail> · <elapsed> · ESC to cancel`（`:163-165`）。
- **生命周期跟着 activity group**（`src/repl/render/group.rs`）：
  - 升起：`ensure_widget`（`:72-80`），调用方有 `open_call`（`:311-319`）、`open_showcase`（`:382-387`）、`open_thinking`（`:437-451`）。
  - 每完成一个调用，`finish_call` 把标签改成 `Working…`（`:374`）。
  - 等用户时，`pause_for_input` 冻结 widget 的时钟（`:490-499`）。
  - `reset_turn` 在轮末收尾（`src/repl/render/transcript.rs:275-285`）。
- **所以下一轮等模型时，两者同时出现**：widget 显示「`Working…` · 1 tool · 7s」，状态行显示「Waiting for the first token」（审计第 4 节的帧）。widget 的时钟在这段时间里一直在走，把等模型的时间也算了进去。
- **压缩**用的是同一个 widget：`raise_widget("Compacting context…")` / `close_widget()`（`src/repl/commands/compact.rs:348`、`:358`；`transcript.rs:379-399`）。

### 2.3 jobs 段：后台层，已经存在

- 状态行的最后一段，只在有运行中的 job 时出现。
  - 一个 job 显示 id、命令和时钟，如 `job b1 sleep 8; echo hi 5s`；多个显示数量加最老那个的时钟（`frame.rs:316-320`、`:353-`）。
  - 每 `JOB_TICK` = 1 s 重画一次（`event_loop.rs:92`）。
- 数据来自 jobs 注册表的 watch：`ui_watch.set_jobs(set)`（`src/repl/run.rs:715`）。
- **它已经横跨轮内和轮外**：轮内显示在 busy 后面（`… · ⠙ Waiting for the first token (ESC to cancel) · job b1 sleep 8; … 2s`），轮外单独存在（审计第 4 节 yield、bg 两组帧）。
- 它没有 spinner，也没有 `ESC` 提示。

### 2.4 host 层：已经实现了「整轮 + jobs」

这一条是第 8 点（同一状态源）的地基：所有者原话里的「整轮 + jobs」跨度，iota 在 host 层上 2026-09-24 就定了，也实现了。屏幕上最终没有沿用这个跨度（§3.7 第 5 点选 B，见 §3.3），host 层的规则不变。

- `src/host/mod.rs` 模块文档原文："A running background job keeps the host at Busy (decided 2026-09-24). What a host is shown is the turn's state with one rewrite: while the turn is `Idle` and at least one shell job runs (`Presenter::set_jobs`), the host sees `Busy`. A job's result still comes back as a turn of its own — the notice wakes the loop — so the conversation is not done until the job is"。
- 计算在 `Shown::effective`（`src/host/mod.rs:227-232`）：`Some(State::Idle) if self.jobs + self.owed > 0 => Some(State::Busy)`。其中：
  - `jobs` 由 `set_jobs` 设置（`:282-284`）；
  - `owed` 是已经结束、通知还没被主循环取走的 job，由 `job_ended` 增加（`:289-294`）、`notice_taken` 减少（`:298-301`）。它补的是注册表先删 job、后送通知之间那段空档，否则 host 会闪一下 Idle。
- **轮的状态在这里设置**：
  - `run_turn` 开头 `set_state(Busy)`（`src/repl/turn/mod.rs:304`）。
  - 主循环醒来时 `Idle`（`src/repl/run.rs:764`）。
  - 中断后 `Idle`（`:940`），出错 `Error`（`:952`），成功 `Idle`（`:1022`）。
  - 审批时 `NeedsInput`/`Busy`（`src/repl/turn/approval.rs:56`、`:77`）；提问类工具同样（`src/repl/turn/tools.rs:309`、`:327`）。
- **job 的输入**：注册表 watch 里 `job_ended`/`set_jobs`（`src/repl/run.rs:711-712`）；通知被取走时 `notice_taken`（`run.rs:768`，轮内则在 `tools.rs:168`）。
- **对外的效果**：OSC 9;4 一直是 3（busy），直到最后一个 job 的通知轮结束（tmux 场景 25 的 8.9 节；DIVERGENCES X-47）。

### 2.5 ESC 的作用域

- 整轮的取消作用域在 `run_turn` 里经 `start_stream` 压栈（`src/ui/runtime/handle.rs:344-351`），`sink.done()` 弹出（`src/repl/turn/mod.rs:316`）。所以**整个 `run_turn` 期间 `scopes_active` 都为真**，busy 尾巴上的 `(ESC to cancel)` 在轮内一直成立。
- 每个工具调用再压一层作用域。ESC 取消最内层，即当前调用；Ctrl+C 取消整轮（`src/repl/turn/tools.rs:264-276` 的注释与代码）。

### 2.6 回车到 `run_turn` 之间

主循环 `read_input` 返回（`src/repl/run.rs:741`）后，一条发给模型的消息要先经过下面几步，才进入 `run_turn`（`run.rs:918`）：

- 分发链；
- `roll_day`（`:882`，bot 换日时可能跑 flush）；
- `offer_before_send` 压缩提议（`:889-891`）：可能弹确认框，确认后跑压缩，压缩有自己的 widget；
- `title_now`（`:904`）；
- `pres.dark_background()`（`:912`）：cmux 下是一次 RPC，最长 `CMUX_RPC_TIMEOUT` = 1 s（`src/host/cmux.rs:38`）。

这一段**目前没有 busy**，host 也还是 Idle。

### 2.7 审计里的真 bug：会被这次改动掩盖，不会被修好

- **现象**（审计第 4 节，真机帧）：工具参数的 delta 先于名字到达（匿名），之后模型又吐了正文。这时工具执行期间 **widget 和 busy 都没有**。
- **原因链**：
  1. `content()` 在 Composing 状态下仍调 `mark_content`（`src/repl/turn/mod.rs:852`），把 `content_open` 置真，正文进 spill。
  2. `close_block` 只在 `block` 存在时调 `close_content`（`:795`），`render_spill` 走 `content_block()`（`:816`），没人把它清掉。
  3. `walk` 的 `open_call` 因此被延后（`src/repl/render/group.rs:313`），直到 `finish_call` 那段兜底的 `ensure_widget`（`:340`）才把 widget 升起来，时钟还从那一刻算起（帧里显示 `<1s`）。
  - 轻症是名字正常、调用之后有正文：widget 停在 composing 标签 `[shell …]`，执行期一直不换成真正的标题。
- **和本提案的关系**：
  - 状态行整轮 busy 上线后，这段时间至少会显示「Running tools」，症状就不再是「什么都没有」。
  - 但 widget 晚到和时钟错误都还在，而且**更难被发现**。
  - 所以修复应当并进来，并排在最前面（§3.4、§3.7 第 7 点）。

---

## 3. 提案

### 3.1 形状：三层，各说各的

| 层 | 在哪 | 说什么 | 跨度 |
|---|---|---|---|
| **整轮层**（新） | 状态行 busy 段 | **这一轮处在哪个阶段、总共多久、能不能打断** | 从接收发送（§3.2）到这一轮结束 |
| **当前活动层**（现有） | 转录区 widget | **是哪个调用、它的输出、它用了多久** | 跟 activity group（§2.2），不变 |
| **后台层**（现有 jobs 段，不变） | 状态行最后一段 | **哪个 job、多久** | job 运行期间；静态，没有 spinner（§3.3） |

状态行在三个时刻的样子（宽度 100，`⠴` 为动画；③ 没有动画，只有 jobs 段自己的时钟每秒走一格）：

```
① 轮内·等模型      fake · ↑ 11 ↓ 7 · 0% / 128k · ⠴ Waiting for the first token  14s (ESC to cancel)
② 轮内·工具执行中  fake · ↑ 11 ↓ 7 · 0% / 128k · ⠴ Running tools  21s (ESC to cancel) · job b1 cargo test 3s
③ 轮外·job 还在跑  fake · ↑ 22 ↓ 14 · 0% / 128k · job b1 cargo test 1m 12s
```

同一时刻②的转录区不变：

```
⠦ [shell cargo test]
  ⎿ 12s · ESC to cancel
```

**整轮层的阶段词表。** 状态行只说阶段，不说工具名；工具名由 widget 说。这和 Claude Code「`running tool for Ns`，不带工具名」、Codex「header 保持 Working」一致（§1.10 第 2 点）。

| 阶段 | 标签 | 现在 | 来源 |
|---|---|---|---|
| 请求在途 | Waiting for the model | 有 | `phases.rs:14` |
| 响应头已到 | Waiting for the first token | 有 | `phases.rs:18` |
| 大请求体上传 | Sending request · x / y | 有 | `phases.rs:20` |
| 重试退避 | `<错误> — retrying (attempt n/10)` | 有 | `retry.rs:50-52` |
| 正文流式中 | **Responding** | 无（现在 `end()`） | 新 |
| 推理流式中 | **Thinking** | 无（现在 `end()`） | 新 |
| 工具参数生成中 / 工具执行中 | **Running tools** | 无 | 新 |
| 压缩（发送前提议） | **Compacting** | 无（只有 widget） | 新 |

等用户（审批框、提问类工具）时，底部区本来就被 surface 占着，状态行看不见（`event_loop.rs:903-907`），不需要单独的标签。各家也都是在这时把指示器藏起来（§1.10 第 3 点）。

**计时：整轮计时，不是阶段计时。** 现在每次 `BusyOn` 都重置 `since`（`event_loop.rs:539`），一换阶段就从 0 开始。这是**判断**：把轮的总时长放在整轮层，把单个调用的时长留给 widget，才不会两边说同一件事。各家主指示器的计时也都是整轮（§1.10 第 6 点）。轮内阶段切换时只改标签，不动时钟。

**后台层（③）的形状。** 就是今天的 jobs 段，原样保留：轮外没有 spinner，没有 `(ESC to cancel)`。理由见 §3.3。

### 3.2 起点

**推荐：主循环确认这条输入要发给模型的那一刻。** 也就是分发链走完、进入 `roll_day` 之前（`src/repl/run.rs:882` 前），host 的 `set_state(Busy)` 也挪到这里。

- **理由**：
  - 用户按下回车之后，下一件可见的事就应该是「它在忙」。
  - 回车到 `run_turn` 之间那几步（§2.6）里，最长的是 cmux RPC（1 s）和压缩，现在既没有 busy，host 也是 Idle。
  - 提交即亮也是多数家的做法：Claude Code、Gemini、Crush、Copilot、Cursor（§1.10 第 1 点）。
- **代价**：
  - 起点和 `run_turn` 分开了，需要一个跨越这两处的守卫，而不是现在 `run_turn` 里的局部守卫。
  - 压缩提议的确认框出现时，底部区被它占着，busy 看不见。这和审批时的规则一致。
- **不推荐在 UI 按键层乐观置位**（Crush 的做法）。斜杠命令、`/new`、`/model` 也是从那里来的，按键层还分不清哪条输入会变成一轮。
- **轮内追加输入**（steer / type-ahead）不起新轮：它只排队（`src/repl/turn/tools.rs:162-170` 在轮次边界取走）。整轮层照常继续，不会出现两个轮重叠（§3.5）。

### 3.3 终点，以及「job 还要跑十分钟，状态行显示什么」

**「所有输出完毕」**指 `run_turn` 返回，并且主循环已经把这一轮的结果落地。成功路径落地在 `src/repl/run.rs:1022` 那一处 `Idle` 之前，中断和出错路径分别在 `:940`、`:952`。**从这一刻起整轮层结束，状态行上不再有 spinner。**

**「jobs 结束」不再是整轮层的终点**（§3.7 第 5 点，已决定 B）。轮外还在跑的 job，只由今天的静态 jobs 段表示：哪个 job（或几个）、跑了多久，每 `JOB_TICK` = 1 s 重画一次（`src/ui/runtime/event_loop.rs:92`），没有 spinner，没有 ESC 提示。

**问题：轮结束后 job 还要跑十分钟，状态行该显示什么？**

- **决定：只显示静态 jobs 段。** spinner 转的应当是「模型或工具此刻在干活」；轮结束后，iota 只是在等 job 的结果。这段等待交给 jobs 段的计时去说。
- **先例**（§1.10 第 4 点）：
  - Codex、Gemini CLI、opencode、Cursor CLI 都是：轮一结束主指示器就消失，后台换成静态计数段。Claude Code 屏幕上的 spinner 也会消失，footer 留 `N background`。
  - 只有 Copilot CLI 让指示器一直转，并把标签改成 "Waiting for background shells"（1.0.20、1.0.86）。
  - 所有者选了与多数一致。
- **和 host 层的关系：有意不一致，并且有先例。**
  - host 层 2026-09-24 定的规则不变：轮已 Idle 而 `jobs + owed > 0` 时，host 仍是 Busy（`src/host/mod.rs:227-232`；DIVERGENCES X-47）。理由是 job 的结果会作为一轮回到模型（`src/repl/run.rs:692-697`），`herdr agent prompt --wait` 要等它。
  - 所以轮外有 job 时，标签页的 OSC 9;4 和 herdr 徽章仍是「忙」，状态行上只有 jobs 段。这和 Claude Code 一样：它的屏幕 spinner 消失，OSC 9;4 却保持到后台结束（2.1.261）。
  - 两者并不矛盾：host 说的是「这次对话还没完」，状态行的整轮层说的是「此刻有一轮在跑」，jobs 段则说明为什么对话还没完。
- **「通知未取走」的空档。** job 结束、注册表把它删掉，到它的通知轮开始，中间有一段空档（`owed > 0`，`src/host/mod.rs:289-301`）。状态行在这段空档里没有 jobs 段，也没有整轮层。主循环一取到通知就起一轮，整轮层随之出现（§3.2 的起点）。空档通常只是一次主循环唤醒的时间【推断】，不另加标签。
- **代价**：所有者原话里的「jobs 结束才停」没有体现到屏幕上，屏幕和 OSC 9;4 在这段时间里说法不同。好处是安静：dev server 这种不会结束的 job 不会让状态行一直转。轮外也不需要额外的 120 ms 重画（`SPINNER_TICK`，`event_loop.rs:87`）。
- **未采纳的方案**（§3.7 第 5 点 A、C）：jobs 段前面加 spinner 转到 `jobs + owed == 0`；或者转一段时间后停。选项与利弊原样保留在 §3.7。

### 3.4 两层怎么分工，才不会重复到让人困惑

| 问题 | 整轮层（状态行） | 当前活动层（widget） |
|---|---|---|
| 现在在干什么 | 阶段：Waiting / Responding / Thinking / Running tools / Compacting | 具体对象：`[shell cargo test]`、`Thinking`、`Working…`、`Compacting context…` |
| 用了多久 | 整轮总时长 | 这个 group（或这次压缩）的时长；等用户时冻结（`group.rs:490-499`） |
| 能不能打断 | `(ESC to cancel)` | 去掉（见下） |
| 后台 | jobs 段（轮内外都在） | 只在 yield 的那一刻出现一行回执（`⎿ still running after 2s → background job b1`） |

- **工具名只出现在 widget 里，阶段只出现在状态行里。** 唯一会「说同一件事」的是 Thinking：widget 的标签是 `Thinking`（`group.rs:437-451`），状态行也是 Thinking。这时 widget 还多一个 token 计数，状态行多一个整轮计时，两者不完全重复，可以接受。这是**判断**。
- **ESC 提示收归状态行一处。** 现在等模型时两处都有（`frame.rs:159` 与 `:297`），整轮 busy 上线后两处会一直同时出现。多数家只有一处，Codex 还特意把它放在一个「fixed visual location」（§1.2 ⑤）。代价是：widget 的 ESC 取消的是当前调用（最内层作用域，§2.5），状态行的提示不再精确地说明 ESC 会取消什么。所以这也列为拍板项（§3.7 第 6 点）。
- **widget 的 `Working…` 不动。** 它是 group 在两次调用之间的标签（`group.rs:374`），等模型时由状态行说 Waiting…，两者分工清楚。

### 3.5 和已有机制的关系、边界情形

- **busy 的底层要从「单个」改成「整轮守卫 + 阶段标签」。** 这是实现上最大的一处。现在 `BusyOff` 无条件清空（`event_loop.rs:552-555`），轮内任何一个阶段守卫结束都会把整轮 busy 一起抹掉。提案的模型是：
  - 整轮守卫持有 busy 和时钟；
  - `Phases` 和 `retry_round` 只改标签；
  - `/model` 列模型发生在轮外（输入在轮内会排队到轮后，X-75 的「typed mid-stream it queues」同理），保持自己的守卫。
  
  怎么做是实现的事，本文只指出：**不能在现有的单槽 `BusyOn/BusyOff` 上叠**。
- **整轮层和 host 来自同一个状态源**（§3.7 第 8 点，已决定 A）。host 的 `Shown` 已经同时记着轮的状态（`requested`）和 job 的计数（`jobs`、`owed`），由 `effective` 推出 host 看到的状态（`src/host/mod.rs:212-232`）。状态行的整轮层取同一处的**轮状态**，即 `effective` 改写之前的那一个；host 取改写之后的那一个。jobs 段照旧来自注册表的 watch（`src/repl/run.rs:715`）。这样「此刻有没有一轮在跑」只算一次，状态行和 OSC 9;4 在轮内不会漂移；轮外两者的差别（host 仍 Busy、状态行只有 jobs 段）是 §3.3 定下的，写在同一处规则里，而不是两处各算各的。
- **压缩。**
  - 发送前的压缩落在整轮层内（起点在 `offer_before_send` 之前，§3.2），标签 Compacting。
  - 输入 `/compact` 触发的压缩不是一轮，仍只有 widget。
- **bot 模式。**
  - flush 轮（`flush == true`）是主循环自己说话，但它确实占着主循环，所以整轮层照常显示。
  - host 层今天也在 `run_turn` 里置 Busy，不区分 flush。
- **headless `iota run`**：没有状态行，不受影响；host 的行为不变（`src/cmd/mod.rs:625`、`:683`）。

**边界情形：**

| 情形 | 整轮层 | 后台层 | 依据 |
|---|---|---|---|
| ESC / Ctrl+C 打断 | 结束（`run.rs:940` 那一刻） | job 不受影响，继续，静态 | 打断只取消轮的作用域（§2.5） |
| 轮出错 | 结束（`run.rs:952`）；红块落地 | 继续，静态 | host 报 `Error`；有 job 时状态行仍有 jobs 段 |
| 轮结束时还有 N 个 job | 结束，状态行上不再有 spinner | 静态 `N jobs <最老的时钟>`（多 job 形状已有，`frame.rs:316-320`）；host 仍 Busy | §3.3 |
| 多个 job 先后结束 | 每个通知各起一轮，整轮层逐轮出现 | 还有 job 在跑时一直在，静态；最后一个结束后消失，不显示 `finished` | §3.3；`src/host/mod.rs:289-301` |
| 后台还在跑时用户又发了一条 | 整轮层出现，jobs 段排在它后面（形状同 §3.1 ②） | 继续 | 现在的段序（`frame.rs:300-322`） |
| 轮内用户又发了一条 | 不起新轮，整轮层继续 | — | steer 在轮次边界取走（`tools.rs:162-170`）；不存在两个轮重叠 |
| `/new` | 不在轮内（轮内输入会排队到轮后，X-75） | job 继续，通知进新会话（X-75），后台层跨过 `/new` 不变 | DIVERGENCES X-75 |
| 审批 / 提问框 | 被 surface 遮住；计时是否扣掉见 §3.7 第 4 点 | 一同被遮住 | `event_loop.rs:903-907` |
| 窄终端 | 截断从尾部吃起，jobs 段的命令先让位 | 同上 | `frame.rs:316-318`、`:325-343` |

### 3.6 实施顺序（建议）

1. **先修 §2.7 的 bug。** 最小改法是：`content()` 在 Composing 状态下不调 `mark_content`（`src/repl/turn/mod.rs:850-853`）；或者在 `close_block` 里，`marked && block.is_none()` 时补一次 `close_content()`。配一个 tmux 场景（trailing text）。
2. 把 busy 的底层改成「整轮守卫 + 阶段标签」，补全 Responding / Thinking / Running tools / Compacting，计时改为整轮，ESC 提示按拍板结果处理。
3. 起点挪到 `run.rs:882` 前，host 的 `Busy` 一起挪过去。
4. 整轮层改为与 host 同一状态源（§3.5）；jobs 段不改（§3.3）。
5. 文档：DIVERGENCES 新增一条，TUI-VERIFY 补帧，CHANGELOG 在实现时再写。

### 3.7 拍板的点

2026-10-14 项目所有者裁决：第 1、2、3、4、6、7、8 点按推荐选 A；第 5 点选 B，原话「5 跟其它大多数 agent 保持一致」。每点末尾标了结论；选项与利弊原样保留，那是决策的依据。

每点给出选项、利弊和推荐。

1. **起点。**
   - (A) 主循环确认这条输入要发给模型之后、`roll_day` 之前（`run.rs:882` 前），host 的 Busy 一起挪。好处：覆盖压缩、cmux RPC 等全部准备工作，和多数家一致。坏处：守卫要跨两个函数。**推荐 A。**
   - (B) `run_turn` 开头（`mod.rs:304`）。好处：改动最小，和现在的 host 一致。坏处：回车后最多 1 s（cmux）加上压缩那段时间仍然没有 busy。
   - (C) UI 按 Enter 的那一刻。好处：最早。坏处：分不清斜杠命令和真正发给模型的消息。
   **已决定（2026-10-14，项目所有者）：A。主循环确认发送时亮，host 的 Busy 一起挪。**
2. **正文流式输出期间，状态行显不显示。**
   - (A) 显示 Responding。好处：符合所有者「直到所有输出完毕」的原话；Copilot、Crush、opencode 也都不撤。坏处：正文本身已经是进度，多一个标签。**推荐 A**，理由是所有者的原话。
   - (B) 撤掉，正文出现后再在工具或推理时回来。这是 Codex（`streaming.rs#L639`）和 Claude Code（`pw`）的做法。好处：干净。坏处：这正是现在「有时有、有时没有」的来源之一。
   **已决定（2026-10-14，项目所有者）：A。正文流式期间显示 Responding。**
3. **工具执行期的标签。**
   - (A) `Running tools`，不带名字。好处：不和 widget 重复，与 Claude Code、Codex 一致。坏处：信息少。**推荐 A。**
   - (B) 带工具名（`Running shell`）。这是 Cursor 的做法。好处：只看状态行就知道在跑什么。坏处：和 widget 说同一件事；并行批次要另想办法。
   - (C) 通用的 `Working`。好处：最简单。坏处：区分不出等模型和跑工具。
   **已决定（2026-10-14，项目所有者）：A。`Running tools`，不带名字。**
4. **整轮计时遇到等用户怎么算。**
   - (A) 扣掉等用户的时间，与 widget 的冻结（`group.rs:490-499`）一致，也是 Codex 和 Claude Code 的做法。**推荐 A。**
   - (B) 不扣，墙钟时间。好处：简单。坏处：和 widget 对不上。
   - (C) 维持现在的阶段计时。坏处：每换一次阶段从 0 开始，不能回答「这一轮多久了」。
   **已决定（2026-10-14，项目所有者）：A。整轮计时，扣掉等用户的时间。**
5. **轮结束后还有 job。**
   - (A) jobs 段前面加 spinner，没有 ESC 提示，一直到 `jobs + owed == 0`。好处：符合所有者的理想跨度，与 host 层一致，有 Copilot 先例。坏处：dev server 这种 job 会让它一直转，轮外还要每 120 ms 重画一次（`SPINNER_TICK`，`event_loop.rs:87`）。**推荐 A。**
   - (B) jobs 段保持静态（今天的样子），只把「通知未取走」的空档补上。这是 Codex、Gemini、opencode、Cursor 的做法。好处：安静。坏处：不满足「jobs 结束才停」。
   - (C) A 加上一个上限：job 跑过某个时长后 spinner 变成静态字形。好处：两头兼顾。坏处：多一个阈值要解释。
   **已决定（2026-10-14，项目所有者）：B。原话「5 跟其它大多数 agent 保持一致」：轮一结束整轮层就消失，jobs 段保持今天的静态样子，不加 spinner。** B 里「把『通知未取走』的空档补上」那半句，正文（§3.3）按「不另加标签」处理：空档通常只有一次主循环唤醒的长度【推断】，通知轮一开始整轮层就出现；host 层的 `owed` 规则照旧，只管 OSC 9;4 和 herdr。
6. **ESC 提示放在哪。**
   - (A) 只放状态行，widget 去掉。好处：一处、位置固定。坏处：不再精确说明 ESC 取消的是当前调用。**推荐 A。**
   - (B) 两处都留（现在等模型时的样子）。坏处：整轮 busy 上线后两处会一直同时出现。
   - (C) 只放 widget。坏处：等模型时没有 widget（第一轮没有工具时），提示就没了。
   **已决定（2026-10-14，项目所有者）：A。只放状态行，widget 去掉。**
7. **§2.7 的 bug 并不并进来。**
   - (A) 并进来，作为第一个提交。好处：整轮 busy 会把这个 bug 的症状遮住，不先修就更难发现。**推荐 A。**
   - (B) 另起分支。好处：范围清楚。坏处：两个分支都碰 `RenderSink`，要排序。
   **已决定（2026-10-14，项目所有者）：A。并进来，作为第一个提交。**
8. **状态源。**
   - (A) 整轮层、后台层和 host 状态由同一个地方推出（从 `Shown::effective` 抽出来）。好处：OSC 9;4、herdr 和状态行永远一致。坏处：要动 host 层的接口。**推荐 A。**
   - (B) UI 自己再算一遍。好处：不动 host。坏处：两处规则会漂移，正是这次要解决的那类问题。
   **已决定（2026-10-14，项目所有者）：A。** 第 5 点选 B 之后，A 里的「后台层」不再由这个状态源驱动：状态行的整轮层取 `Shown` 里的轮状态，host 取 `effective` 改写后的状态，jobs 段照旧来自注册表的 watch（§3.5）。

### 3.8 不同意或拿不准的地方

- **第 5 点：我原先推荐 A，所有者选了 B，我认同。** 推荐 A 时我就不踏实（九家里只有 Copilot 这么做，dev server 一直转的代价是实打实的）。B 留下的唯一张力是：轮外有 job 时，屏幕（只有 jobs 段）和 host（OSC 9;4、herdr 仍是忙）说法不同。§3.3 把它解释为两层各说各的，Claude Code 也是同样的组合。如果日后有人报「标签页说忙、状态行没 spinner」，答案在 §3.3，不必回头改 host 层。
- **「等用户时隐藏」我没有提议去改。** 底部区单槽是从 Go 原版来的设计（`frame.rs:66` 与 `:217` 的注释引用 model.go:1049-1058），各家也都这么做。但这意味着所有者说的「从发送一直到完毕」在审批期间仍有一段空档。如果他连这段也要看见，就得改单槽规则，那是另一份提案。
- **opencode 的起点**是服务端事件，它相对请求发出的时点我没核到；**Cursor** 普通工具审批时显不显示、**Amp** 的大部分行为，都是【查不到】。
- **Claude Code 权限提示时隐藏 spinner** 只到【推断】，因为排除集合 `FM` 的成员没解出来。
- **整轮计时的显示门槛**沿用现在的 2 s（`BUSY_ELAPSED_MIN`）还是改长（Claude Code 是 16 s），本文没有提议，留给实现时看帧再定。
