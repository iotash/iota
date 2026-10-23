# 429 与 ESC：额度用尽时不再自己转圈，ESC 真的停下

Status: **设计稿，未实现** · 日期：2026-10-23 · 基于 `main` = `2929ed4` · 分支 `rate-limit-cancel` · 拟定 DIVERGENCES X-84

所有者的报告（2026-10-23）：`~/Work/nikon`，会话 `n66gj2gg3nqr`，provider `glm`、模型 `glm-5.3`（anthropic 方言），撞上 GLM 的 5 小时用量上限。两个症状：

1. iota 一直在重试：每个后台 shell 任务结束（一条 host notice）都开一个新 turn，每个 turn 爬一遍 429 重试梯子，再打一块 `✗ Rate limited (429)`。看起来没完没了。
2. 按 ESC 打出 `Interrupted.`，但排在被取消的 turn 后面的任务通知立刻又开新 turn、又是新的 429。

## 写作约定

- 代码坐标都指 `2929ed4`。「梯子」= `retry_round` 的外层重试（`repl::turn::retry`）；「线路层」= `llm::client::Client::send_payload` 自己的重试。
- 「通知」只指 host notice（`InputKind::Notice`，后台任务结束）；bot 的 memory-flush notice 另说（§5.6）。
- 「键入」= 用户在输入框里按 Enter 的东西（`InputKind::Typed`）。

## 0. 结论

- **一个新状态、住在一个地方**：`Repl` 上加一个 `Gate { hold: Option<Hold>, held: Vec<Input> }`，`Hold` 只有两种——`Interrupted`（用户按了 ESC，自动工作停到用户再开口）和 `RateLimited { next, step }`（一个 turn 以 429 收场，自动工作停到 `next`）。主循环在 `read_input` 拿到通知时先问 gate：有 hold 就**收起来**（打一行灰字），不开 turn。没有新配置项。
- **谁能放行**：用户键入的消息永远照发，并把收起的通知按原序带在它前面；`RateLimited` 另有一个计时器到点自己放行一次（「探测」）；`/model` 真的换了模型时把 `RateLimited` 的计时器拨到现在。一次成功的回复清掉 `RateLimited`。`Interrupted` 只有键入能清。
- **梯子**：429 单独一档——最多 3 次、指数退避（2/4/8 s）；`retry-after` ≤ 60 s 就睡它，> 60 s 不睡、直接失败并把它当成放行时刻；hold 期间的 turn 遇 429 不爬梯子（线路层那 2 次照旧）。5xx 与传输错误的 10 次线性梯子不动。线路层也不再睡 > 60 s 的 `retry-after`。
- **不解析供应商正文**。区分「瞬时限流」与「额度用尽」靠的是**结果**而不是正文：梯子爬完还是 429 就进 `RateLimited`；有没有 `retry-after` 只决定第一次探测的时刻。没有它（GLM 就没有），探测按 1 → 2 → 4 → … 分钟翻倍、封顶 30 分钟。5 小时额度一共约 13 次探测、每次最多 3 个请求，对比今天每条通知 33 个请求。
- **ESC**：UI 层不改——`fire_cancel` 已经把键入的东西折回输入框、把通知留在队列里（`event_loop.rs:774-776`）；改的是主循环：被中断后 gate 进 `Interrupted`，队列里的通知一条条被收起，而不是一条条开 turn。被中断的 turn 如果整个被丢弃且它的输入是通知，这条通知也收起来，不丢。
- 供应商自己的消息（带重置时间）照旧是错误块的 detail 行；错误块加一行 hint 说 iota 接下来做什么；每条被收起的通知一行灰字说它在等什么。状态行不动（§8 Q3）。

## 1. 失败链（每个接缝的 file:line）

### 1.1 症状 1：每条通知一架梯子

1. 后台任务结束 → `jobs.set_sink` 的回调 `ui_sink.enqueue(job_notice(&done))`（`src/repl/run.rs:689-697`，`job_notice` 在 `:68-74`）。
2. `UiMsg::Enqueue`：有挂着的 `read_input` 就直接交给它，否则排进 `queue`（`src/ui/runtime/event_loop.rs:599-608`）。
3. 主循环 `ui.read_input(&root_cancel)`（`run.rs:740-747`）把通知当成下一条输入。`notice` 只决定不回显、不过斜杠命令分派（`run.rs:748-751`、`:777`），然后 `set_state(Busy)`、`history.push(Body::Notice)`、`TurnEngine::run`（`run.rs:889`、`:905-909`、`:925-929`）——**没有任何东西问「上一个 turn 是不是刚因为 429 死掉」**。
4. 每个模型调用都包在 `retry_round` 里（`src/repl/turn/mod.rs:404-411`、`src/repl/turn/tools.rs:70-80`）。`is_retryable` 对 429 返回真（`src/repl/turn/retry.rs:105`），梯子最多 `MAX_RETRIES = 10`（`retry.rs:14`）、第 n 次等 n × 1 s（`retry.rs:18`、`:52-61`），总共约 55 s。
5. 每一次「尝试」又是线路层的一次 `send_payload`：`DEFAULT_RETRIES = 2`（`src/llm/client.rs:27`），`should_retry` 对 429 为真（`client.rs:511-529`），`retry_delay` 没有头就 0.5/1 s 退避（`client.rs:534-563`）。**一条通知 = 11 × 3 = 33 个请求**。线路层读得到 `retry-after`，但 `StatusError` 不带头（`client.rs:604-631`，构造在 `:358-366`），外层梯子看不见它。
6. 梯子爬完 → `Err(failure)` 分支（`run.rs:950-994`）：`set_state(State::Error)` + `notify(Kind::Failed)`（每条通知一次桌面/host 提醒）、`error_block`，然后 `history.truncate(hist0 - 1)`（`run.rs:987`）——**这条通知连同任务结果一起被丢掉**，模型永远不知道这个任务跑完了。
7. 错误块：`describe_status` 把 429 映射成 `Rate limited (429)`、hint 为空（`src/repl/errors.rs:118-146`，`:132`）；detail 是信封里的 `message`（`errors.rs:153` `envelope_message`），GLM 的那句「已达到 5 小时的使用上限……将在 20:22:40 重置」就在这里，这部分是对的。
8. 回到第 3 步：下一个任务结束，再来一遍。会话里 b218、b219 正是如此。

### 1.2 症状 2：ESC 之后通知照样开 turn

1. ESC → `Model::fire_cancel`（`event_loop.rs:767-798`）：取消 turn 的 token；**只有键入的条目**折回输入框（`:777-794`），通知按设计留在队列——注释原话「Notices stay queued so the next read still delivers them」（`:774-776`）。
2. 梯子中途：`retry_round` 的 `biased` select 让取消赢过退避计时器，返回 `ChatError::Interrupted`（`retry.rs:52-60`）；流式中途：`stream_round` 把流错误映射成中断。两条路都落到 `run.rs:941-949`：`interrupt_turn`（`run.rs:1323-1371`，打 `Interrupted.` 在 `:1346`）、`set_state(Idle)`。**这里没有留下任何状态**。
3. 回到 `read_input`：队列非空，`ReadReq` 立刻把队首交出来（`event_loop.rs:589-597`）——就是那条留下的通知 → 新 turn → 新 429。会话里 b220、b221 打出 `Interrupted.` 是用户又按了两次 ESC 才停住。
4. 跑着的 turn 内部，`Steerer::drain` 在轮次边界走同一个队列（`src/repl/turn/steer.rs:45-70`，`UiMsg::TakeQueued` 在 `event_loop.rs:615-624`）——这条路本身没问题（通知随用户的 turn 进去是对的），但说明通知只有**一条**投递路径，修复也只该在这条路径的入口（主循环）上做。

### 1.3 已核实的事实

- GLM 的 429 **没有** `retry-after` / `retry-after-ms`（委托方 2026-10-23 16:32 CST 用 curl 打 `https://open.bigmodel.cn/api/anthropic/v1/messages` 核过）。正文：`{"type":"error","error":{"type":"rate_limit_error","code":"1308","message":"[1308][已达到 5 小时的使用上限。您的限额将在 2026-10-23 20:22:40 重置。][...]"},"request_id":"..."}`。`type` 是 Anthropic 的通用 `rate_limit_error`——瞬时限流和额度用尽**同一个 type**，区别只在供应商私有的 `code` 和中文正文里。
- 连续的 user 角色消息在 anthropic 方言里合成一条（`src/provider/anthropic.rs:94-97`），gemini 同理（`src/provider/google.rs:318-330`）。所以把收起的通知放在用户消息**前面**、作为独立的 `Body::Notice` 消息推进历史是安全的，与 `Steerer::drain` 今天推「通知 + 用户消息」的形状相同。
- 委托书说「键入的消息 ESC 后照发，与 Claude Code 一致」——Claude Code 确实照发（§2.1），但 **iota 今天不是**：今天键入的排队消息在 ESC 时折回输入框，不自动发送（`event_loop.rs:764-766` 的文档注释：「interrupt means "the situation changed", so nothing auto-sends」）。本设计保留今天的行为（折回），见 §6。

## 2. 成熟工具怎么做

来源：2026-10-23 的浅克隆 `openai/codex` @`d681745`、`anomalyco/opencode`（`sst/opencode` 重定向至此）@`74f8d48`、`charmbracelet/crush` @`9ee8985` + 其 LLM 层 `charmbracelet/fantasy` v0.45.1；`anthropic-sdk-typescript` main 的 `src/client.ts`；code.claude.com 文档（`errors.md`、`interactive-mode.md`、`hooks.md`）。行号指这些快照。Claude Code 本体闭源，只能引文档。

### 2.1 各家怎么做

| | 429 重试 | 瞬时 vs 额度怎么分 | 依赖供应商私有信号？ | 长 `retry-after` | 用户看到 / 是否自动恢复 | 中断时排队的东西 |
|---|---|---|---|---|---|---|
| **Claude Code** | CLI 最多 10 次指数退避（`errors.md` › Automatic retries，`CLAUDE_CODE_MAX_RETRIES`）；SDK 默认 2 次，`x-should-retry` 优先，读 `retry-after-ms`/`retry-after`，0.5 s·2ⁿ 封顶 8 s（`client.ts` L1834-1927） | 看**响应头**：真正的额度响应带 unified quota 头，没有就是「Server is temporarily limiting requests (not your usage limit)」并重试（`errors.md` L860-867）；网关花费上限发 `x-should-retry: false` | 是——Anthropic 自家的 unified quota 头 | SDK main 睡任何正值（旧版 v0.30.0 只认 < 60 s，`core.ts:605`） | `You've hit your session limit · resets 3:45pm`；订阅用户可**自动等到重置**：脚注 `Continuing automatically at 3:45pm · esc to cancel`，到点让模型继续（不重发上一条），最多连续 2 次，超过 24 h 不等（`interactive-mode.md` L739-746） | ESC 后「If you have messages queued, Claude Code sends them next」（`interactive-mode.md` L36）；后台任务的 `task-notification` 是会自己开 turn 的注入 turn；ESC 是否压住待发的 task-notification **未核实** |
| **Codex CLI** | HTTP 层 `retry_429: false`（`model-provider-info/src/lib.rs:457-463`），由 turn 循环重试：200 ms·2ⁿ ±10%，无上限（`core/src/responses_retry.rs`、`async-utils/src/backoff.rs`） | 读 429 **正文**：`error.type == "usage_limit_reached"` → `UsageLimitReached`（带 `resets_at`）；`usage_not_included`；`insufficient_quota` 及一组 `code` → `QuotaExceeded`；三者都不重试（`codex-api/src/api_bridge.rs:185-244`） | 是——OpenAI 的 body 码；流错误还从文本里抠「try again in Ns」（`sse/responses_error.rs:90-105`） | 睡到 `Retry-After`，无上限 | `You've hit your usage limit… try again at <time>`；没有自动等待/恢复。**字段缺失且无 `Retry-After`（正是 GLM）→ 直接以 `exceeded retry limit, last status: 429` 结束，不重试**（`api_bridge.rs:241`、`protocol/src/error.rs:442-444`） | TUI `on_interrupted_turn` 把排队的用户消息**并回输入框**、不发（`tui/src/chatwidget/input_restore.rs:358-448`）；core `abort_all_tasks` 清 turn 内 pending 后调 `maybe_start_turn_for_pending_work()`，`trigger_turn` 的邮件**中断后仍会开新 turn**（`core/src/tasks/mod.rs:573-601`）；子 agent 完成邮件默认 `trigger_turn=false`，搭下一个 turn（`agent/control/completion.rs:117`）；出错后自动发下一条排队消息，**除非** `rate_limit_recovery_pending`（`input_flow.rs:244-250`） |
| **opencode** | 10 次，2 s·2ⁿ 每段封顶 10 s（约 84 s，`packages/core/src/model-retry.ts`）；`x-should-retry` 优先（`provider-error.ts:85-88`） | `classifyProviderFailure`：402、`QUOTA_CODES`，或 429 时正文匹配 `QUOTA_TEXT` 正则（`insufficient quota|quota exceeded|usage limit|limit exhausted|package has expired…`，`provider-error.ts:173`，注释点名 Z.ai） | 是——码表 + 英文正则；中文正文匹配不上 → 当作限流重试约 84 s 后失败 | `max(退避, retry-after)`，`retry-after` 封顶 15 min，睡 | `attempt N, retrying in Xs`；不解析重置时间、不自动恢复 | 双击 ESC = `interrupt({resume:true})`：下一个 steer 投递项（子 agent 完成就是）**立即唤醒新 run**（`execution.ts:150-167`、`session/session.ts:283-318`）；排队的 prompt（`queue` 投递）留着；用户中断还取消后台 job（`execution.ts:125-127`） |
| **Crush**（fantasy） | 3 次，5/10/20 s（`retry.go:113`）；408/409/429/5xx，`x-should-retry`（`errors.go:81-99`） | 不分 | 否 | `retry-after` 只在 < 60 s 或 < 当前退避时采用，否则忽略、用退避（`retry.go:19-55`） | 重试后显示错误；无重置时间、无自动恢复 | ESC 清空排队 prompt（`internal/agent/agent.go:2021-2075`、`ui/model/ui.go` `AgentClearQueue`）；没找到后台任务自动开 turn（模型用 `job_output` 轮询；未穷尽核查） |

### 2.2 读出来的东西

1. **每家都能在「认得出」的时候停下，但「认得出」全靠供应商私有信号**：Claude Code 靠 Anthropic 的 quota 头，Codex 靠 OpenAI 的 body 码，opencode 靠码表加英文正则（显式为 Z.ai 写）。GLM 的 429 三样都不给：没有 unified 头、`type` 是通用的 `rate_limit_error`、私有的是 `code: "1308"` 和中文正文。照搬任何一家，在 GLM 上都退化成「当作瞬时限流」——Codex 例外，它在无头无码时**一次都不重试**直接失败。iota 已经接 6 种方言、更多中转站，写码表是一条没有尽头的路。
2. **能自动恢复的只有 Claude Code，而且是因为它拿得到可靠的重置时刻**；它还给恢复加了上限（连续最多 2 次、24 h 外不等）。没有重置时刻的人都选择「显示错误、等用户」。本设计的探测是两者之间的一步：有 `retry-after` 就像 Claude Code 那样到点再试；没有就按翻倍的间隔试，代价与事件数无关。
3. **长 `retry-after` 没有共识**：SDK main 和 Codex 无上限地睡、opencode 封顶 15 min 地睡、Crush 和旧 SDK 对 ≥ 60 s 直接不理。在交互式 CLI 里「无声地睡几小时」与本 bug 的症状一样糟（看起来卡死），所以本设计取「≤ 60 s 睡、> 60 s 不睡而是交给 hold 定时」——既不忽略这个信息，也不让 turn 挂几个小时。
4. **中断后谁开 turn**：Codex（`trigger_turn` 邮件）和 opencode（resume 唤醒，故意的）的**自动**消息会在中断后自己开 turn；Claude Code 的 task-notification 会自己开 turn，但 ESC 是否压住它未核实；但 Codex 专门为限流加了 `rate_limit_recovery_pending`，挡住出错后的自动发送——这正是本 bug 缺的那道闸。iota 的不同在于后台 shell 任务很常见、每个都发通知，而用户按 ESC 的意思是「停」，所以本设计让中断后的通知等用户开口。
5. **中断时键入的排队消息**：Claude Code 照发，Codex 并回输入框，opencode 留着，Crush 丢弃。iota 今天与 Codex 相同（并回），本设计不动（§8 Q4）。

## 3. 设计

### 3.1 状态与它住的地方

```rust
// src/repl/run.rs（或 src/repl/gate.rs，一个 ~120 行的小模块，纯逻辑、可单测）
pub(crate) struct Gate {
    hold: Option<Hold>,
    /// 收起的通知，按到达顺序。只有 InputKind::Notice 且不是 flush notice 的会进来。
    held: Vec<Input>,
}
pub(crate) enum Hold {
    /// 用户中断了一个 turn：自动工作停到用户下一次键入。没有计时器。
    Interrupted,
    /// 一个 turn 以 429 收场：自动工作停到 `next`，届时用收起的通知探测一次。
    RateLimited { next: Instant, step: Duration },
}
```

它挂在 `Repl` 上（与 `conv`、`session`、`handles` 并列），**不进 UI 层**：UI 的队列和 `fire_cancel` 不改；也不进 `TurnCtx` 以外的地方（只往 `TurnCtx` 加一个只读的 `rate_limited: bool`，§3.4）。不持久化：resume 回来没有 hold——额度窗口是时间的事，不是会话的事；进程退出时后台任务本来就被 `kill_all`（`run.rs:1050`），也没有通知可收。

常量（不是配置项）：

| 常量 | 值 | 位置 | 用途 |
|---|---|---|---|
| `RATE_RETRIES` | 3 | `repl/turn/retry.rs` | 429 的梯子级数 |
| `RATE_BACKOFF` | 2 s，第 n 次 × 2ⁿ⁻¹（2/4/8 s） | `repl/turn/retry.rs` | 429 没给 `retry-after` 时的退避 |
| `RETRY_AFTER_CAP` | 60 s | `llm/client.rs`（两层共用） | `retry-after` 超过它就不睡、直接交上去 |
| `PROBE_FIRST` / `PROBE_CAP` | 60 s / 30 min | gate 模块 | 没有 `retry-after` 时的探测间隔：翻倍、封顶 |

为什么 60 s：Crush（fantasy）与旧版 Anthropic SDK 用的就是这条线（§2.1）；更要紧的是，对交互式 CLI，一分钟以上的「转圈」已经该让用户知道并能做选择了。与它们不同，超过 60 s 的值不被忽略，而是成为 hold 的放行时刻（§2.2 第 3 条）。

### 3.2 状态机

| 从 \ 事件 | turn 成功 | turn 以 429 失败 | turn 被中断 | turn 以其他错误失败 | 计时到点（held 非空） | `/model` 换了模型 | 来一条通知 |
|---|---|---|---|---|---|---|---|
| `None` | `None` | `RateLimited` | `Interrupted` | `None` | — | — | 开 turn（今天） |
| `RateLimited` | `None` | `RateLimited`，`step` 翻倍 | `Interrupted` | `None` | 探测 turn | `next = now` | 收起（已过 `next` 则直接探测） |
| `Interrupted` | `None` | `RateLimited` | `Interrupted` | `None` | — | 不变 | 收起 |

只有键入消息和探测能在 hold 中开 turn；上表「turn」列对它们和无 hold 时的普通通知 turn 一样适用。


完整的转移规则：

1. **收**：主循环拿到一条通知（非 flush），`gate.hold.is_some()` → `gate.held.push(input)`，打一行灰字（§3.5），`pres.notice_held()`（§3.6），`continue`。这一步放在 `set_state(Idle)`（`run.rs:764`）之前——hold 的那一刻 host 该看到的状态是失败 turn 留下的 `Error` 或中断留下的 `Idle`，不该被一条收起的通知改写。
2. **键入放行**：主循环拿到键入消息并走到消息路径（斜杠命令不算，它们 `continue` 在 `run.rs:777-863`），且 `held` 非空 → 先把 `held` 全部按序推进历史（每条 `tr.boundary_notice(display)` + `Message::notice(text)`，`ctxm.note`），再推用户消息。`hold` 先设为 `None`、但记下这次 turn 带了哪些通知（`carried`），失败时放回。
3. **计时放行（探测）**：`hold == RateLimited` 且 `held` 非空时，主循环的 `read_input` 与 `next` 赛跑（§3.3）。计时赢 → 取出全部 `held`，最后一条当作这次 turn 的输入（`Body::Notice`），前面的按序先推进历史；这是一个普通的通知 turn，只是 `rate_limited = true`（不爬 429 梯子）。
4. **结果**（对键入 turn 和探测 turn 一样，对普通通知 turn 也一样）：
   - `Ok` → `hold = None`。turn 跑着时到达的通知不经 gate（`Steerer` 在轮次边界带进去，决策表 #19），所以此刻 `held` 必为空。
   - `Err` 且是 429（`ChatError` 里 `LlmError::Status(se)` 的 `se.status == 429`，包括 `AfterOutput` 包着的）→ `hold = RateLimited { next, step }`：`next = now + se.retry_after`（有且 > 0 时）否则 `now + step`；`step` 首次为 `PROBE_FIRST`，此后每次 429 翻倍、封顶 `PROBE_CAP`；**由键入引发的 429 不把 `next` 往前挪**（`next = max(旧 next, 新算的)`），用户的手动尝试不重置探测节奏。`carried` 与本 turn 的输入（若是通知、且 turn 被整个回滚）放回 `held` 的**最前面**，保持原序。
   - `Err` 非 429 → `hold = None`；`carried` 随回滚一起丢掉，与今天对任何失败 turn 里的通知、键入消息的处理相同（`run.rs:987`）。**不**把它们重新排队：持续性的 400 会让重新排队的通知一条条开 turn、一条条失败，正是本 bug 的形状。失败 turn 的通知该去哪是另一个问题（§6、§8 Q2）。
   - 中断 → `hold = Interrupted`（无论之前是什么——用户接管了，计时器作废）。`interrupt_turn` 返回 `false`（整个 turn 被丢弃）时，`carried` 与本 turn 的输入（若是通知）放回 `held` 最前面；返回 `true`（部分保留）时它们已经在历史里，不放回。
5. **`/model`**（`run.rs:801-805`）：`cmd_model` 前后比较 `provider.kind()` + `provider.model()`；变了且 `hold == RateLimited` → `next = now`、`step = PROBE_FIRST`。下一圈主循环立即探测（在新模型上）。`Interrupted` 不受 `/model` 影响。没变（ESC 掉了选择器、选了同一个）→ 什么都不做。
6. `/new`、`/compact`、`/session` 等不碰 gate。

第 1 步的准确条件是 `hold == Interrupted || (hold == RateLimited && now < next)`：`RateLimited` 而 `held` 为空时不挂计时器（没有要放的东西），之后来的第一条通知若已过了 `next`，它本身就是探测，直接以 `rate_limited = true` 开 turn。

### 3.3 主循环怎么同时等输入和计时器

不能把 `read_input` 的 future 放进 `select!` 然后丢掉：它在 `ReadReq` 时已经在 UI 里挂了一个 waiter，丢掉 future 不会撤销它，之后的 Enter 会被送进一个死掉的 oneshot、**消失**（`src/ui/runtime/handle.rs:216-222` 的注释正是为这个「eager revoke」写的）。所以按 handle 自己的取消协议来：

```rust
let wait = root_cancel.child_token();
let timer = gate.deadline().map(|at| { let w = wait.clone(); tokio::spawn(async move { sleep_until(at).await; w.cancel(); }) });
let got = ui.read_input(&wait).await;
if let Some(t) = timer { t.abort(); }
match got {
    Ok(input) => input,
    Err(UiError::Interrupted) if wait.is_cancelled() && !root_cancel.is_cancelled() => gate.release_probe(), // 计时放行
    Err(_) => break 'main Ok(()), // 与今天一致：空闲 Ctrl+C/Ctrl+D、关闭
}
```

空闲时的 Ctrl+C 走 `fail_waiter_interrupted`（`event_loop.rs:799-803`），`wait` 没被取消，所以仍然退出——区分条件是「`wait` 被取消了」而不是错误类型。

### 3.4 梯子（问题 a）

在 `retry_round`（`retry.rs:30-72`）里，429 走自己的一档；签名只多一个只读输入：

- `StatusError` 加 `retry_after: Option<Duration>`（`client.rs:604-631`）。在 `send_payload` 构造它的地方（`client.rs:358-366`）从已有的 `headers` 里解析——把 `retry_delay` 里解析 `retry-after-ms` / `retry-after`（秒、HTTP-date）的那段（`client.rs:535-555`）抽成 `fn retry_after(&HeaderMap) -> Option<Duration>`，两处共用。`Display` 不变，所以每条按文本钉的错误断言都不动。
- 线路层：`should_retry`（`client.rs:511-529`）在 `retry_after > RETRY_AFTER_CAP` 时返回 `false`——不睡几个小时，把错误交上去。这也修了 headless 下「一个 5 小时的 `retry-after` 让进程静静睡 5 小时」的潜在问题。`x-should-retry` 照旧优先。
- 外层（`retry.rs` `retry_round`）：错误是 429 时，最多 `RATE_RETRIES = 3` 次；等待 = `retry_after`（若有且 ≤ 60 s）否则 `RATE_BACKOFF × 2ⁿ⁻¹`；`retry_after > 60 s` 不等、立即返回（线路层已经没重试它）。busy 行：`Rate limited (429) — retrying in 37s (attempt 1/3)`（有 `retry_after` 时带秒数），否则保持今天的 `Rate limited (429) — retrying (attempt 1/3)`。取消照旧赢过计时器（`biased` select，`retry.rs:52-60`）。
- `TurnCtx` 加 `rate_limited: bool`（`src/repl/turn/mod.rs:95-101` 旁，与 `can_retry` 并列），由主循环在开 turn 时按 gate 填。为真时 429 **零级梯子**：第一个 429（线路层已重试过 2 次）就失败。`retry_round` 的 `allowed: bool` 换成一个三值 `Retries { All, NoRateLimit, None }`——三处调用点（`mod.rs:406`、`tools.rs:72`、测试）各改一个实参。
- 5xx、传输错误、带内错误：`MAX_RETRIES = 10`、线性 1 s，**不动**。

请求数对照（一条通知遇上 GLM 5 小时额度，无 `retry-after`）：今天 11 × 3 = 33 个请求、约 60 s、一块红色错误、一次 host 失败提醒；之后**每条**通知重复。改后：首个 turn 4 × 3 = 12 个请求、约 15 s、一块红色错误；之后每次探测 3 个请求、一行灰字；5 小时内约 13 次探测（1+2+4+8+16 = 31 min，此后每 30 min），合计约 50 个请求——而且与任务通知的数量无关。

### 3.5 用户看到什么（问题 d）

| 状态 | 状态行 | transcript |
|---|---|---|
| 梯子在爬（429，无 `retry-after`） | busy 行 `Rate limited (429) — retrying (attempt n/3)` | 无 |
| 梯子在爬（429，`retry-after` 37 s） | busy 行 `Rate limited (429) — retrying in 37s (attempt n/3)` | 无 |
| 梯子中途恢复 | — | 灰字 `⟳ Rate limited (429) — recovered after n attempt(s)`（今天的） |
| 梯子放弃 → `RateLimited` | 状态行不变；host 状态 `Error` | 红块 `✗ Rate limited (429)`，detail = 供应商原话（GLM 的重置时间在里面），**hint 新增**：`Background-job notices will wait; iota tries again at 17:42. Send a message to try now, or /model to switch.`（时刻取 `next`，本地时间 `%H:%M`；`retry-after` 给出的时刻同样格式） |
| `RateLimited` 中来一条通知 | — | 灰字 `⏸ [background job b219 finished …] — held until 17:42`（行首是通知原有的 headline） |
| 探测开始 | busy 照常 | 每条被放行的通知按今天的方式回显 headline（`tr.notice`），前面一行灰字 `▶ Trying again with 2 held notice(s)` |
| 探测又 429 | host 状态保持 `Error`，**不再** `notify(Failed)` | **不**再打红块；一行灰字 `⏸ Still rate limited (429) — 2 notice(s) held; next try 18:12`。供应商原话在第一块红块里，已经在屏幕上 |
| 探测成功 | — | 那个 turn 的正常输出；hold 清除不额外打字 |
| 中断 → `Interrupted` | — | 今天的 `Interrupted.`；之后每条被收起的通知一行灰字 `⏸ [background job …] — held; it goes with your next message` |
| 键入放行 | — | 被带上的通知按 `tr.boundary_notice` 回显 headline，然后 `❯` 用户块（与 `Steerer::drain` 的顺序一致） |

状态行（`StatusData`，`src/ui/facade.rs:45-62`）不加段：§8 Q3。

### 3.6 host（herdr / cmux）

`Presenter::job_ended` 让每条欠着的通知把 host 撑在 `Busy`，`notice_taken` 只减计数、不发状态（`src/host/mod.rs:368-381`）。收起一条通知 = 这条通知「被处理了」，但没有 turn 来报告状态，所以需要一个 `notice_held()`：减计数**并**重算、发出有效状态（`update` 的现有路径）。否则 hold 期间 host 会一直显示 `Busy`——正是用户以为「它还在跑」的样子。

### 3.7 中断：梯子中途 vs 流式中途（问题 e）

两者的结果相同，因为二者都汇到 `run.rs:941` 的同一个分支：

- **梯子中途**（在退避里按 ESC）：`retry_round` 返回 `Interrupted`（`retry.rs:52-60`），这一轮没有任何文本，`interrupt_turn` 按 interrupt 表决定保留或丢弃（`docs/design/interrupt.md`）；gate → `Interrupted`；即使这次失败的原因是 429，也**不**进 `RateLimited`——用户的意图优先，且 `Interrupted` 比 `RateLimited` 严格（没有计时器）。
- **流式中途**：partial 照今天保留；gate → `Interrupted`。
- 两种情况下，排在 UI 队列里的通知都留在队列（UI 不改），随后被主循环逐条读出并收起——收起发生在中断后的同一个事件循环周期里，屏幕上 `Interrupted.` 下面紧跟着几行 `⏸ … held` 灰字，说明了它们去哪了。
- 键入的排队消息照今天折回输入框（`event_loop.rs:777-794`）。用户按 Enter 发出 → 键入放行（§3.2 第 2 步），收起的通知随它一起进去。
- 中断时**正在处理**的那条通知（这个 turn 的输入），如果 turn 被整个丢弃，放回 `held`；否则它已在历史里。

## 4. 决策表

「通知」= 后台任务结束的 host notice；「键入」= 用户在输入框里按 Enter 的消息。「梯子」见 §3.4。

| # | 情形 | 梯子 | 之后的 gate | 收起的通知 | 屏幕 |
|---|---|---|---|---|---|
| 1 | 429，`retry-after` ≤ 60 s，重试成功 | 睡 `retry-after`，≤ 3 次 | 不变（成功 → `None`） | 不涉及 | busy 行带秒数；一行 `⟳ … recovered` |
| 2 | 429，`retry-after` ≤ 60 s，3 次都失败 | 同上 | `RateLimited`，`next = now + 最后一次的 retry-after` | 本 turn 的通知输入放回 | 红块 + hint（时刻） |
| 3 | 429，`retry-after` > 60 s | 线路层不睡、外层不等，立刻失败 | `RateLimited`，`next = now + retry-after`（就是重置时刻，一次探测） | 同上 | 红块 + hint（重置时刻） |
| 4 | 429，没有 `retry-after`（GLM） | 2/4/8 s，3 次 | `RateLimited`，`next = now + 60 s`，`step` 翻倍封顶 30 min | 同上 | 红块 + hint（下次尝试时刻） |
| 5 | 连续 429（探测又失败） | hold 中零级梯子（线路层 2 次） | `RateLimited`，`step` 翻倍，`next = now + step`（或 `retry-after`） | 原样放回 | 一行灰字 `⏸ Still rate limited …`，无红块、无 host 提醒 |
| 6 | `RateLimited` 中来通知 | — | 不变 | 收起；`now ≥ next` 时直接当探测跑 | 一行 `⏸ … held until HH:MM` |
| 7 | `RateLimited` 中用户键入 | hold 中零级梯子 | 成功 → `None`；429 → `RateLimited`（`next` 不提前）；中断 → `Interrupted` | 全部带在消息前面；失败时放回 | 成功：正常；429：红块（用户发起的，给全信息） |
| 8 | `RateLimited` 中用户键入斜杠命令（`/jobs`、`/status`……） | — | 不变 | 不放行 | 命令自己的输出 |
| 9 | `/model` 换成别的模型（`RateLimited`） | — | `next = now`、`step` 复位 → 立即在新模型上探测 | 探测带走 | `▶ Trying again with n held notice(s)` |
| 10 | `/model` 未换 / 在 `Interrupted` | — | 不变 | 不放行 | — |
| 11 | 之后任一请求成功 | — | `None` | （已随该 turn 发出） | 正常输出 |
| 12 | 流式中途 ESC，队列里有通知 | — | `Interrupted` | 队列里的逐条收起；本 turn 输入若是通知且 turn 被整个丢弃，放回 | `Interrupted.` + 每条一行 `⏸ … held; it goes with your next message` |
| 13 | 梯子中途 ESC（429 退避中），队列里有通知 | 取消赢过计时器 | `Interrupted`（不是 `RateLimited`） | 同 12 | 同 12 |
| 14 | `Interrupted` 中来通知 | — | 不变 | 收起 | 一行 `⏸ … held; …` |
| 15 | `Interrupted` 后用户键入 | 正常梯子 | 按该 turn 结果（成功 `None`、429 `RateLimited`、中断 `Interrupted`） | 全部带在消息前面 | 通知 headline，然后 `❯` 块 |
| 16 | ESC 时有键入的排队消息 | — | `Interrupted` | — | 折回输入框（今天的行为），不自动发 |
| 17 | 非 429 的失败（5xx 爬完、400……） | 不变 | `None` | 带上的随回滚丢掉（今天对失败 turn 的处理；§8 Q2） | 今天的红块 |
| 18 | bot 的 flush notice | 不变 | 不读也不改 gate | 从不收起 | 今天的 |
| 19 | 一个跑着的键入 turn 中途任务结束 | — | — | 不经 gate：`Steerer::drain` 照旧在轮次边界带进去 | 今天的 `boundary_notice` |

## 5. 不变式与边角

1. 一条通知要么在 UI 队列、要么在 `gate.held`、要么在历史里——任何时刻恰好一处；hold 不丢通知（非 429 失败回滚丢通知是今天的行为，不在本次范围，§6）。
2. hold 只拦**开 turn**，不拦**进 turn**：跑着的 turn 照旧用 `Steerer` 收队列里的通知（决策表 #19）。这条保证「用户自己在用的时候」行为一点不变。
3. 计时器只在 `RateLimited && !held.is_empty()` 时挂；它只能放行，不能开一个空 turn。
4. 被收起的通知放回 `held` 时插在最前面，保持任务结束的原始顺序。
5. headless（`iota run`）没有主循环、没有通知、没有梯子（X-82 的说明），只受线路层 `RETRY_AFTER_CAP` 的影响。
6. bot 的 flush notice 不经 gate（`is_flush_notice`，`run.rs:759`）：它是循环自己的、best-effort，失败时 compaction 照跑（`run.rs:958-962`）。

## 6. 不改的东西

- **UI 层**：`fire_cancel`、`UiMsg::Enqueue` / `TakeQueued` / `ReadReq`、队列的排序与 `Queued` 类型（`event_loop.rs:589-624`、`:767-798`）。通知留在队列里由主循环决定去处，这条分工正确，错的是主循环的决定。
- **键入消息在 ESC 时折回输入框**，不自动发送（`event_loop.rs:764-794`）。委托书以为今天是「ESC 后照发」并说要保留——两者不一致时以代码为准、保留代码的行为；是否改成 Claude Code 的样子是另一个问题（§8 Q4）。
- **5xx / 传输错误 / 带内错误的梯子**：10 次、线性 1 s（`retry.rs:14`、`:18`）。X-82 的 `AfterOutput`、`StreamIdle` 不重试的规则。
- **线路层的退避形状**（0.5 s × 2ⁿ 封顶 8 s、jitter）与 `DEFAULT_RETRIES = 2`（`client.rs:27`）；只加 60 s 封顶。
- **错误块的 headline 与 detail**：仍是 `Rate limited (429)` + 供应商原话；只加 hint。
- **不解析供应商正文**：不读 `code: "1308"`、不从中文里抠重置时间；`StatusError.body` 只用于显示。
- **非 429 失败时的回滚**（`run.rs:987`）——今天它连同通知一起丢掉，本设计不碰（§8 Q2）。
- **`Steerer`**（`steer.rs`）：跑着的 turn 照旧在轮次边界收通知。
- **状态行**（`StatusData`）不加段（§8 Q3）。
- **持久化**：hold 不进会话文件；resume 从 `None` 开始。
- 没有新配置项。

## 7. 测试清单

### 7.1 单元

`src/llm/client.rs` 的 `tests`：
- `retry_after` 解析：`retry-after-ms: 1500` → 1.5 s；`retry-after: 37` → 37 s；`retry-after: 0.5` → 0.5 s；HTTP-date 未来 → 余量；过去、`0`、负数、垃圾 → `None`。
- `StatusError::new` 带上 `retry_after`；它的 `Display` 与改前逐字节相同（钉一条现有样例）。
- `should_retry`：429 + `retry-after: 61` → `false`；429 + `retry-after: 60` → `true`；`x-should-retry: true` + `retry-after: 3600` → `true`（显式头优先，保持现状）；无头 429 → `true`。
- `send_payload`（本地 TCP 桩，暂停时钟）：429 + `retry-after: 18000` 一次请求就返回，不睡。

`src/repl/turn/retry.rs` 的 `tests`（暂停时钟，沿用 `Busy` 记录器）：
- `a_rate_limit_climbs_three_rungs_with_doubling_backoff`：无头 429 → 4 次调用，busy 行 `Rate limited (429) — retrying (attempt 1/3)`…`3/3`，两次之间的虚拟时间 2/4/8 s。
- `a_short_retry_after_is_slept_and_shown`：`retry_after = 37 s` → busy 行带 `retrying in 37s`，下一次调用在 37 s 后。
- `a_long_retry_after_is_not_slept`：`retry_after = 61 s` → 1 次调用，无 busy 行。
- `under_a_hold_a_rate_limit_is_not_retried`：`Retries::NoRateLimit` + 429 → 1 次调用；同一模式下 503 照旧爬 10 级。
- `is_retryable_follows_the_status_table` 保持绿（429 仍可重试，档位不同而已）。
- `an_interrupt_during_the_backoff_wins_over_the_timer` 加一个 429 变体。

gate 模块（纯逻辑，无 I/O）：
- 每条转移：`None` + 429 → `RateLimited{next=now+60s}`；有 `retry_after` → `next = now + retry_after`；连续 429 → `step` 60 → 120 → … → 1800 封顶；键入引发的 429 不把 `next` 提前；成功 → `None`；中断从任一状态 → `Interrupted`。
- `held` 放回在最前、保序；`deadline()` 在 `held` 空时为 `None`；`Interrupted` 永远没有 deadline。
- `/model` 变化 → `next = now`、`step` 复位；未变 → 不动；`Interrupted` 下不动。

`src/repl/errors.rs` 的 `tests`：429 的 hint 文本（有时刻 / 由调用方给时刻）；GLM 的那条正文（`code: 1308`）detail 仍是整句供应商原话。

`src/host/mod.rs` 的 `tests`：`job_ended` 后 `notice_held` → 发出 `Idle`（或失败 turn 留下的 `Error`），不再是 `Busy`。

### 7.2 repl 集成（`tests/repl/`，新文件 `tests/repl/rate_limit.rs`，注册进 `tests/repl/main.rs`）

用 `tests/repl/jobs.rs` 已有的「`enqueue` 一条 `InputKind::Notice`」写法 + 返回 429 的 FakeProvider：
- `a_rate_limited_notice_holds_the_ones_behind_it`：通知 1 → 429（梯子 4 次调用）→ 再 `enqueue` 通知 2、3 → provider 调用数**不再增加**；transcript 有一块 `✗ Rate limited (429)`、两行 `⏸ … held until`。
- `the_probe_releases_every_held_notice_in_one_turn`：接上条，推进暂停时钟 60 s → 一个 turn，历史里三条 `Body::Notice` 按 1、2、3 的顺序，provider 只被调 1 次（零级梯子，FakeProvider 不经线路层）；成功后 gate 为 `None`。
- `a_failed_probe_backs_off_and_prints_one_line`：探测又 429 → 无第二块红块、一行 `⏸ Still rate limited`；120 s 后再探测。
- `a_typed_message_carries_the_held_notices_ahead_of_it`：hold 中键入 `go` → 历史为 notice…notice、user `go`；transcript 顺序为 headline 行、然后 `❯ go`。
- `a_model_switch_probes_at_once`：hold 中 `/model` 换模型 → 立即一个 turn，无需推进时钟。
- `esc_holds_the_notices_queued_behind_the_turn`：通知 turn 流到一半被中断（`tests/repl/host.rs:318-330` 的 `Answer::Interrupt` 装置）→ 队列里的两条通知被收起，provider 不再被调用；随后键入 → 一个 turn 带着它们。
- `an_interrupted_notice_turn_keeps_its_notice`：通知 turn 在出字前被取消（整个丢弃）→ 这条通知在 `held` 里，随下一次键入进入历史；会话文件里恰好一次。
- `a_failed_rate_limited_turn_never_loses_its_notice`：通知 turn 429 失败 → 会话文件里此时没有它，探测成功后有且仅有一次。
- `the_flush_notice_is_never_held`：bot 会话里 hold 中排入 flush notice → 照跑（参照 `tests/repl/bot_flush.rs` 的装置）。
- 回归：`tests/repl/jobs.rs::an_idle_notice_runs_a_turn_without_the_user_echo` 不变（无 hold 时行为不变）。

### 7.3 tmux 场景（`tests/ui_tmux/scenarios/35-rate-limit-hold.sh`）

值得做一个：问题本身就是「屏幕上看起来没完没了」。mock 已有 `fail:<status>:<n>`（`tests/ui_tmux/mock.rs:34-36`、`:276-302`）和 `background: true` 的工具调用（`mock.rs:487`）。脚本：让模型起两个 `sleep 1` 的后台任务、回复里带 `fail:429:99`，等两个任务结束 → 断言 `capture-pane -p` 里恰好一处 `✗ Rate limited (429)`、一行 `⏸ … held until`；再起一个任务后按 ESC 的变体断言 `Interrupted.` 下面是 `⏸ … held; it goes with your next message` 且之后 5 s 内没有新的 `Rate limited`。mock 若要按「第几个请求」而非「哪条消息」失败，需要给 `fail:` 加一个全局计数形式——这是测试装置的改动，计入实现 PR。

## 8. 给所有者的问题

**Q1. 没有 `retry-after` 时，要不要自动探测？**（推荐：要，1 分钟起翻倍、封顶 30 分钟）
另一个选项是「收起直到用户开口」：最省请求，但一个真的只是瞬时（每分钟）限流、而梯子 15 s 没扛过去的批处理就会停到用户回来。翻倍探测对 5 小时额度的总代价约 13 次 × 3 请求，与通知数量无关；对瞬时限流 1 分钟内恢复。

**Q2. 非 429 失败时被回滚的通知，要不要也留住？**（推荐：本次不做，另开一条）
今天任何失败（5xx 爬完、400、上下文超限……）都会 `truncate` 掉通知，任务结果从此对模型不可见（`run.rs:987`）。本设计只保证 hold 自己收起的通知不因 429 丢失；非 429 失败时 `carried` 随回滚丢掉（决策表 #17），与今天一致。根治需要决定「失败 turn 的通知去哪」——简单地重新排队会在持续性 400 上造成和本 bug 同形的循环，需要单独设计。

**Q3. 状态行要不要加一段 `⏸ 2 held · 17:42`？**（推荐：先不加）
transcript 里每条收起都有一行、错误块的 hint 写了时刻，信息是完整的；加段要动 `StatusData` 和渲染（UI 层），违背「只在一层长」。若实际使用中发现灰字被后续输出冲走、用户找不到状态，再加。

**Q4. ESC 时键入的排队消息：维持折回输入框，还是像 Claude Code 那样照发？**（推荐：维持折回）
委托书写的是「照发（今天的行为，与 Claude Code 一致）」。Claude Code 确实照发（`interactive-mode.md` L36），Codex 并回输入框、Crush 丢弃（§2.1）；但 iota 今天的代码是折回（`event_loop.rs:764-794`，从 Go 版 `model.go:529-549` 继承的有意设计：「interrupt means the situation changed」）。本设计不碰这一点；若要改，是一条独立的 UI 变更。

## 9. DIVERGENCES 草稿（X-84）

| id | area | Go behaviour | Rust behaviour | why |
|---|---|---|---|---|
| X-84 | a 429 the ladder could not clear, and host notices behind an interrupted turn | `retryRound` (`chat/run.go:1417-1447`) climbs the same 10-rung linear ladder (attempt × 1 s) for a 429 as for a 5xx and never sees `retry-after` (only the transport loop, `internal/llm/client.go:188-212`, reads it — and sleeps whatever it says, however long); a turn that gives up rolls back and the loop reads its next input. In iota a finished background job's notice is an input (`run.rs` job sink → `Ui::enqueue`), so with a hard quota every notice opened a turn, climbed 11 × 3 = 33 requests, printed a `✗ Rate limited (429)` block and pinged the host; and `fire_cancel` (`event_loop.rs:767-798`) leaves notices queued on purpose, so ESC was answered by the next notice's turn | **The ladder.** `StatusError` carries `retry_after` (from `retry-after-ms` / `retry-after` seconds / HTTP-date, the parse `retry_delay` already had); the transport loop does not sleep one over `RETRY_AFTER_CAP` (60 s) and hands it up. `retry_round` gives a 429 its own rung count, `RATE_RETRIES` = 3, waiting `retry_after` when ≤ 60 s and 2/4/8 s otherwise; one over 60 s fails at once. 5xx and transport faults keep the 10-rung linear ladder. **The gate.** `Repl` holds a `Gate { hold: Option<Hold>, held }`: a turn that ends in 429 sets `RateLimited { next, step }` (`next` = now + `retry_after`, else now + `step`; `step` 60 s doubling to 30 min); an interrupted turn sets `Interrupted`; a successful one clears it. While held, a notice `read_input` delivers is put aside with one dim `⏸ … held` line and the host settled (`Presenter::notice_held`) instead of opening a turn; a running turn still takes notices at its round boundaries (`Steerer`, unchanged). A typed message always goes, carrying every held notice ahead of it; `RateLimited` also releases them on its own at `next` (the main loop's `read_input` runs under a child token a timer cancels — never a dropped future, which would strand the parked waiter) as one turn with no 429 ladder, and a `/model` switch to another model moves `next` to now. A failed release prints one dim `⏸ Still rate limited (429) — n notice(s) held; next try HH:MM`, no red block and no host ping. A notice whose turn failed with 429, or was discarded whole by an interrupt, goes back to `held` at the front. The 429 block keeps the provider's own message as its detail and gains a hint naming the next try. Nothing parses a provider's body: whether a 429 was transient or a quota is learned from whether the ladder cleared it; `retry-after` only sets when to look again | reported 2026-10-23 on a `glm-5.3` session (anthropic dialect) that hit GLM's 5-hour cap: every background-job notice opened a turn and a new 429 block; ESC printed `Interrupted.` and the next queued notice started another. GLM's 429 has no `retry-after` (curl, 2026-10-23) and its body's `type` is the generic `rate_limit_error` for a per-minute limit and a 5-hour quota alike — the difference lives only in a vendor `code` and Chinese prose, so a body parser would be a GLM parser. Claude Code (Anthropic's unified quota headers), Codex CLI (OpenAI body codes such as `usage_limit_reached`) and opencode (a code table plus an English regex written for Z.ai) each stop on a hard limit only when they recognise it by a vendor signal, and GLM sends none of theirs; Claude Code alone resumes on its own, at a reset time its headers give it (docs/design/rate-limit-and-interrupt.md §2). iota learns the limit from the ladder's outcome instead, and paces its own retries without vendor codes. Typed messages queued at ESC still fold back into the composer (unchanged). Pinned by `llm::client::tests` (the `retry_after` parse, the 60 s cap, `Display` unchanged), `repl::turn::retry::tests` (`a_rate_limit_climbs_three_rungs_with_doubling_backoff`, `a_short_retry_after_is_slept_and_shown`, `a_long_retry_after_is_not_slept`, `under_a_hold_a_rate_limit_is_not_retried`), the gate's transition table, `tests/repl/rate_limit.rs` (held notices, one probe turn carrying all of them in order, backoff with one dim line, a typed message carrying them, `/model` probing at once, ESC holding the queue, no notice lost to a 429 or a discarded interrupt) and tmux scenario 35 (one `✗ Rate limited (429)` block for two finished jobs; after ESC, `⏸ … held` lines and no new 429) |

## 10. CHANGELOG（Unreleased）

```markdown
### Fixed

- **Hitting a provider's rate limit or usage cap no longer turns into an
  endless loop, and Esc really stops.** When a request was refused with
  429 and retrying didn't help, every background job that finished started
  a new attempt and printed another `Rate limited (429)` error. Now iota
  stops on its own: notices from finished jobs wait (each one says so in a
  dim line), and iota tries once more later — when the provider says it may
  (`retry-after`), otherwise after a minute, then less and less often, at
  most every 30 minutes. Sending a message tries at once and takes the
  waiting notices with it; switching to another model with `/model` tries
  at once on that model. The provider's own message, with its reset time if
  it gives one, is still shown. Pressing Esc now also holds the notices
  queued behind the turn you stopped: they go with your next message
  instead of starting new turns. A rate limit is retried at most three
  times (it used to be ten), and a `retry-after` longer than a minute is no
  longer waited out silently.
```
