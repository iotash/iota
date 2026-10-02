# 请求挂住与取消：其它 coding agent 的做法调研

调研日期：2026-10-01。范围：API 请求「连上了但不返回 / 流到一半不动」时各家怎么处理，尤其是用户按
ESC / Ctrl+C 取消这条路径。本文只调研，不改代码。

取证方式与可信度约定：

- **开源产品**（Codex CLI、opencode、crush、aider、Gemini CLI）直接读源码，链接是带 commit 的永久链接，
  行号在调研当天核对过。
- **闭源产品**（Claude Code、Cursor）只能引官方文档、changelog、issue 与论坛帖；文档没写的线路行为一律标
  **「未证实」**。
- 标 **「推论」** 的句子是我把两条有来源的事实拼起来得出的，不是来源原话。
- 文中所有默认值都是调研当天的版本；这些数值改得很勤（Claude Code 的流空闲阈值一年内从 90 s 改到 5 min），
  抄的时候抄**机制**，别抄数字。

## 0. 结论先行

1. **成熟产品都把「等响应头」和「流中途静默」当成两种不同的挂住，各配一个计时器**，而且都不对流式请求用
   总超时（overall deadline）。流空闲阈值的行业共识是 2–5 分钟，首字节阈值是 1–5 分钟。
2. **取消的标准实现就是丢弃 future / abort signal / cancel context，三大栈的官方文档都承诺这会断开连接**
   （HTTP/1 关 TCP，HTTP/2 发 `RST_STREAM`）。真正的坑不在「drop 断不断」，而在「有没有别的 task 还握着
   这条流」。
3. **重试与否看「响应走到哪一步」而不是看 HTTP 方法**：还没产出任何内容就重试；已经完成过文本块或工具调用就
   不重试、保留已有产出并明说「可能不完整」——理由是重放会让工具调用执行两次。
4. **挂住时最差的 UX 是沉默**。Claude Code 20 s 无数据就在 spinner 上写「在等 API · N 秒后重试」，Codex 的
   重试通知注释直接写着目的：别让用户盯着一个看似冻住的屏幕。

---

## 1. Claude Code

闭源；以下全部来自官方文档、官方 changelog 与 GitHub issue。

### 1.1 ESC / Ctrl+C 的语义

- `Esc`：「Stop the current response or tool call mid-turn so you can redirect. Claude keeps the work done
  so far.」有排队消息时会接着发出去；有对话框时 `Esc` 只关对话框。
  来源：[Interactive mode · Keyboard shortcuts](https://code.claude.com/docs/en/interactive-mode#keyboard-shortcuts)
- `Ctrl+C`：「Interrupts a running operation. If nothing is running, the first press clears the prompt input
  and a second press exits Claude Code」——即**有任务时取消本轮，空闲时两次才退出**。来源同上。
- Agent SDK 侧对应的是 `abortController` 选项（「Controller for cancelling operations」）和 `Query.interrupt()`。
  来源：[Agent SDK reference - TypeScript](https://code.claude.com/docs/en/agent-sdk/typescript)

### 1.2 ESC 是否真的 abort 连接

**未证实**（文档没有一句话描述 ESC 在线路层做了什么）。能找到的间接证据：

- 实测侧证：issue #83238 的作者统计了「请求在响应头之前卡住 → 用户按 Esc → 重发」的时间线，卡了 2–9 分钟的请求
  在 `[Request interrupted by user]` 之后重试 12–27 s 就完成。说明 ESC **在还没收到响应头时也能把请求打断**，
  且下一次请求没有继续卡在同一条坏连接上。
  来源：[anthropics/claude-code#83238](https://github.com/anthropics/claude-code/issues/83238)
- 它底层用的 Anthropic TypeScript SDK 公开的取消手段就是 abort：「If you need to cancel a stream, you can
  `break` from the loop or call `stream.controller.abort()`.」
  来源：[TypeScript SDK](https://platform.claude.com/docs/en/cli-sdks-libraries/sdks/typescript)
- 官方对看门狗的措辞是「aborts the stalled connection」，且 v2.1.214 的 changelog 有一条「Changed keep-alive
  connection pooling to disable after a stale-connection error, so retries open a fresh socket」。
  来源：[Error reference · Automatic retries](https://code.claude.com/docs/en/errors#automatic-retries)、
  [CHANGELOG.md](https://github.com/anthropics/claude-code/blob/main/CHANGELOG.md)

### 1.3 超时：四个独立计时器 + 一个总超时

官方原话：「Claude Code runs four independent timers that abort a streaming model response when it goes quiet,
so a dead connection fails and retries instead of hanging.」

| 计时器 | 触发条件 | 默认值 |
|---|---|---|
| First-byte deadline | 发出请求后迟迟没有响应头 | 直连 Anthropic API 180 s，其它 300 s，**每 32 KB 请求体再加 1 s** |
| Event-level watchdog | 没有解析出任何响应事件 | 300 s |
| Byte-level watchdog | 线路上没有任何字节到达（**SSE keep-alive ping 也算字节**） | 直连 180 s，其它 300 s |
| Body idle timeout | 5 分钟没有字节（用于前三者不覆盖的 provider） | 5 min |

来源：[Network configuration · Streaming idle watchdogs](https://code.claude.com/docs/en/network-config#streaming-idle-watchdogs)

- 总超时 `API_TIMEOUT_MS` 默认 600000（10 分钟）。
  来源：[Environment variables](https://code.claude.com/docs/en/env-vars)
- `CLAUDE_STREAM_IDLE_TIMEOUT_MS` 显式设置时**最小被钳到 5 分钟**，「lower values are silently clamped to absorb
  extended thinking pauses and proxy buffering」。来源同上。
- 首字节超时的**重试用更长的 deadline**：第一次用 first-byte deadline，重试等到 `API_TIMEOUT_MS − 1 s`，
  「so that the retry can outlast a proxy or gateway that holds the response until generation completes」。
  两次都没应答才报 `API Error: No response from API (waited 3m, then 10m on the retry)`。
  来源：[Error reference · No response from API](https://code.claude.com/docs/en/errors#no-response-from-api)

这些机制是被 issue 一条条逼出来的，演进顺序本身就是教训（版本号取自 changelog 条目所在小节）：

| 版本 | 变化 |
|---|---|
| 2.1.84 | 新增 `CLAUDE_STREAM_IDLE_TIMEOUT_MS`，当时默认 **90 s** |
| 2.1.105 | 「streams now abort after 5 minutes of no data and retry non-streaming instead of hanging indefinitely」 |
| 2.1.196 | 流空闲看门狗对所有 provider 默认开启 |
| 2.1.214 | stale-connection 错误后禁用 keep-alive 连接池，重试走新 socket |
| 2.1.243 | 「Fixed sessions going silent for 10+ minutes when the Anthropic API never starts a response: the request now times out after ~3 minutes, retries once」 |

来源：[CHANGELOG.md](https://github.com/anthropics/claude-code/blob/main/CHANGELOG.md)。
（文档正文把首字节超时记为「Requires v2.1.242 or later」，与 changelog 小节差一个版本号，以哪个为准**未证实**。）

### 1.4 文档与 issue 里怎么描述「卡住」

- [#25979](https://github.com/anthropics/claude-code/issues/25979)（2026-02 开，2026-08 以 completed 关闭）：
  流中途静默，进程停在 `epoll_wait`，「The UI shows a spinner … indefinitely. No error is surfaced. The session
  cannot recover — the only fix is kill -9.」作者的诉求就是给 SSE 加 read timeout。
- [#83238](https://github.com/anthropics/claude-code/issues/83238)（2026-08 开，仍 open）：响应头之前卡住时
  「the byte watchdog wraps `response.body`, so it is not armed yet at this point」，只剩 600 s 的总超时兜底，
  结果是「a completely silent spinner」。这正是后来 first-byte deadline 要补的洞。
- [#86074](https://github.com/anthropics/claude-code/issues/86074)（`claude -p`，not planned 关闭）：TCP 被
  NAT 静默丢弃后无人值守任务挂了 4 小时，靠机器睡眠唤醒才把僵尸 socket 杀掉。
- [#28482](https://github.com/anthropics/claude-code/issues/28482)：交互式下的 workaround 是按 Esc 再重发，
  但 headless / 远程没有等价物——**「靠用户按 ESC」不能当作唯一的恢复手段**。
- 反方向的教训：阈值设短了会误杀。[#46987](https://github.com/anthropics/claude-code/issues/46987)、
  [#53730](https://github.com/anthropics/claude-code/issues/53730) 都是「Stream idle timeout - partial
  response received」频繁误触发的抱怨（后者点名 plan mode 的长思考）。

### 1.5 挂住时的 UX 与重试

- **20 秒无数据**即在 spinner 显示 `Waiting for API response · will retry in … · check your network`，
  「The request hasn't failed yet: the countdown runs to the point where Claude Code aborts the stalled
  connection.」重试期间显示 `Retrying in Ns · attempt x/y`。
  来源：[Error reference · What you see while Claude Code retries or waits](https://code.claude.com/docs/en/errors#what-you-see-while-claude-code-retries-or-waits)
- 重试预算默认 10 次、指数退避（`CLAUDE_CODE_MAX_RETRIES`，上限 15）。
  来源：[Error reference · Tune retry behavior](https://code.claude.com/docs/en/errors#tune-retry-behavior)
- **按响应进度分级**（这是全文最值得抄的一张决策表）：

| 失败发生在 | 处理 |
|---|---|
| 任何响应内容之前（5xx / overloaded / 超时 / 断连） | 按退避重试 |
| 响应头到了但一直没内容，或思考结束后还没开始文本/工具调用时停滞 | abort 后**只重发一次**，不占 10 次预算 |
| 响应头始终不来 | 到 first-byte deadline abort，**每个请求最多重发一次** |
| 已完成过一个文本块或工具调用之后 | **不重试**，保留已完成的产出、执行已完成的工具调用，追加「The response above may be incomplete」 |
| 响应已经完整之后 | 当正常结束，不提示 |

不重试的理由官方写得很直白：「Claude Code doesn't re-run the request, because that could execute the same
tool calls twice.」
来源：[Error reference · Automatic retries](https://code.claude.com/docs/en/errors#automatic-retries)、
[The response above may be incomplete](https://code.claude.com/docs/en/errors#the-response-above-may-be-incomplete)

---

## 2. OpenAI Codex CLI

开源（Rust）。以下基于 commit
[`444da31`](https://github.com/openai/codex/tree/444da310e108da16aaeb18fd790b0ac464f08aca)（2026-10-01）。

### 2.1 Esc / Ctrl+C 的语义

- `Esc` 在任务运行中发送 `Op::Interrupt`（测试断言原文：「expected Esc to send Op::Interrupt while a task is
  running」）；有弹窗时 `Esc` 只关弹窗。
  来源：[`tui/src/bottom_pane/mod.rs#L3727-L3730`](https://github.com/openai/codex/blob/444da310e108da16aaeb18fd790b0ac464f08aca/codex-rs/tui/src/bottom_pane/mod.rs#L3727-L3730)
- `Ctrl+C`：有可取消的工作时发 `Op::Interrupt`（**取消本轮**），没有时直接**退出**。
  来源：[`tui/src/chatwidget/interaction.rs#L502-L573`](https://github.com/openai/codex/blob/444da310e108da16aaeb18fd790b0ac464f08aca/codex-rs/tui/src/chatwidget/interaction.rs#L502-L573)
- 「连按两次才退出」做过又关掉了，注释原话：「requiring a double press to quit feels janky in practice」。
  来源：[`tui/src/bottom_pane/mod.rs#L230-L235`](https://github.com/openai/codex/blob/444da310e108da16aaeb18fd790b0ac464f08aca/codex-rs/tui/src/bottom_pane/mod.rs#L230-L235)

### 2.2 取消怎么落到请求上

1. `Op::Interrupt` → `interrupt_task` → `abort_all_tasks(TurnAbortReason::Interrupted)`。
   来源：[`core/src/session/mod.rs#L4952-L4959`](https://github.com/openai/codex/blob/444da310e108da16aaeb18fd790b0ac464f08aca/codex-rs/core/src/session/mod.rs#L4952-L4959)
2. **先协作式取消，再硬杀**：`cancellation_token.cancel()`，等任务自己收尾 **100 ms**
   （`GRACEFULL_INTERRUPTION_TIMEOUT_MS`），不管收没收完都 `task.handle.abort()`。
   来源：[`core/src/tasks/mod.rs#L926-L953`](https://github.com/openai/codex/blob/444da310e108da16aaeb18fd790b0ac464f08aca/codex-rs/core/src/tasks/mod.rs#L926-L953)
3. 请求层的两个 await 点都和取消 token 赛跑：发请求（含等响应头）是
   `client_session.stream(...).or_cancel(&cancellation_token)`，读流是 `stream.next().or_cancel(...)`。
   `or_cancel` 就是一个 `tokio::select!`。
   来源：[`core/src/session/turn.rs#L2576-L2588`](https://github.com/openai/codex/blob/444da310e108da16aaeb18fd790b0ac464f08aca/codex-rs/core/src/session/turn.rs#L2576-L2588)、
   [`turn.rs#L2634-L2641`](https://github.com/openai/codex/blob/444da310e108da16aaeb18fd790b0ac464f08aca/codex-rs/core/src/session/turn.rs#L2634-L2641)、
   [`async-utils/src/lib.rs#L32-L37`](https://github.com/openai/codex/blob/444da310e108da16aaeb18fd790b0ac464f08aca/codex-rs/async-utils/src/lib.rs#L32-L37)
4. SSE 是在一个**独立 task** 里读的，所以上面的 drop 本身碰不到 HTTP body；它靠 `tx_event.closed() => return`
   这一支感知「接收端没了」并退出，从而释放 body。
   来源：[`codex-api/src/sse/responses.rs#L535-L570`](https://github.com/openai/codex/blob/444da310e108da16aaeb18fd790b0ac464f08aca/codex-rs/codex-api/src/sse/responses.rs#L535-L570)

**推论**：第 4 步释放 body 之后连接是否真的断开，Codex 自己没有注释说明；依据见 §4.2 的 hyper 文档
（HTTP/1 关连接、HTTP/2 发 `RST_STREAM`）。

### 2.3 超时

- **流空闲超时**：`stream_idle_timeout_ms`，默认 300000（5 分钟），每个 provider 可配。实现是对每次
  `stream.next()` 套 `tokio::time::timeout`，超时报 `idle timeout waiting for SSE`。
  来源：[`model-provider-info/src/lib.rs#L63-L72`](https://github.com/openai/codex/blob/444da310e108da16aaeb18fd790b0ac464f08aca/codex-rs/model-provider-info/src/lib.rs#L63-L72)、
  上面的 `responses.rs`、
  [Configuration Reference](https://learn.chatgpt.com/docs/config-file/config-reference)（`developers.openai.com/codex/config-reference` 现在 308 到这里）
- **总超时：没有**。provider 构造请求时 `timeout: None`。
  来源：[`codex-client/src/provider.rs#L79-L89`](https://github.com/openai/codex/blob/444da310e108da16aaeb18fd790b0ac464f08aca/codex-rs/codex-client/src/provider.rs#L79-L89)
- **首字节超时：主路径上没找到**。旁证是 guardian 子系统自己补了一层并留了注释：「The SSE idle timeout starts
  after headers arrive. Bound that wait too.」——说明空闲超时不覆盖等响应头这一段。主采样路径是否在别处有界，
  **未证实**（我只确认了上面两处）。
  来源：[`ext/guardian-v2/.../connection_pool.rs#L479`](https://github.com/openai/codex/blob/444da310e108da16aaeb18fd790b0ac464f08aca/codex-rs/ext/guardian-v2/src/async_scorer/sampler/connection_pool.rs#L479)
- WebSocket 连接超时 15 s（`DEFAULT_WEBSOCKET_CONNECT_TIMEOUT_MS`），来源同 `lib.rs#L63-L72`。
- 已知缺陷：[openai/codex#43140](https://github.com/openai/codex/issues/43140)（open）——收到终止事件
  `response.failed` 后仍等 EOF，服务器不关连接时真正的错误会被 5 分钟后的 idle timeout 顶替。
  教训：**空闲超时不能代替「看到终止事件就立刻返回」**。

### 2.4 重试与 UX

- 两层预算：`request_max_retries` 默认 4（拿到响应头之前的 HTTP 层），`stream_max_retries` 默认 5（流断了重连）。
  来源同 `lib.rs#L63-L72`。
- 流重试时向 UI 发 `Reconnecting... {n}/{max}`，注释原话：「Surface retry information to any UI/front-end so
  the user understands what is happening instead of staring at a seemingly frozen screen.」重试的 sleep 也包在
  `or_cancel` 里，**退避等待期间按 Esc 同样立即生效**。
  来源：[`core/src/responses_retry.rs#L141-L166`](https://github.com/openai/codex/blob/444da310e108da16aaeb18fd790b0ac464f08aca/codex-rs/core/src/responses_retry.rs#L141-L166)、
  [`turn.rs#L1716-L1728`](https://github.com/openai/codex/blob/444da310e108da16aaeb18fd790b0ac464f08aca/codex-rs/core/src/session/turn.rs#L1716-L1728)
- 重试耗尽后还会尝试从 WebSocket 降级到 HTTPS 再来一轮；另有 feature flag 控制的无限重连
  （`Reconnecting... waiting for network`）。来源同 `responses_retry.rs`。

---

## 3. 其它产品

### 3.1 opencode

commit [`0112a92`](https://github.com/sst/opencode/tree/0112a92c416f5ad833d96e7a8308441f0a875d94)（2026-10-01）。

- **同时有首字节超时和流空闲超时**，都在自定义 `fetch` 包装层实现：`headerTimeout` 默认 300 s（可设 `false`
  关闭），`chunkTimeout`（注释写明是「SSE idle」）默认 300 s，另有可选的总 `timeout`。空闲超时的做法是包一层
  `ReadableStream`，每次 `reader.read()` 配一个 `setTimeout`，到点 `ctl.abort()` + `reader.cancel()`，报
  `SSE read timed out`。还显式传 `timeout: false` 关掉 Bun 自带的 fetch 超时，避免两套计时器打架。
  来源：[`provider/provider.ts#L37-L126`](https://github.com/sst/opencode/blob/0112a92c416f5ad833d96e7a8308441f0a875d94/packages/opencode/src/provider/provider.ts#L37-L126)
- 两种超时都映射成**可重试**错误（`isRetryable: true`），重试最多 5 次，初始 2 s、倍增、无 `retry-after` 时封顶 30 s。
  每次重试前把次数、原因和下次重试的时刻写进会话状态，供界面显示。
  来源：[`session/message-v2.ts#L660-L680`](https://github.com/sst/opencode/blob/0112a92c416f5ad833d96e7a8308441f0a875d94/packages/opencode/src/session/message-v2.ts#L660-L680)、
  [`session/retry.ts#L26-L31`](https://github.com/sst/opencode/blob/0112a92c416f5ad833d96e7a8308441f0a875d94/packages/opencode/src/session/retry.ts#L26-L31)、
  [`retry.ts#L183-L207`](https://github.com/sst/opencode/blob/0112a92c416f5ad833d96e7a8308441f0a875d94/packages/opencode/src/session/retry.ts#L183-L207)
- 取消：`Esc` 绑 `session_interrupt`，**要按两次**（第一次显示「esc again to interrupt」，5 秒内第二次才生效）。
  请求侧用 `Effect.acquireRelease` 托管一个 `AbortController`，作用域结束或被中断时 `ctrl.abort()`。
  来源：[`cli/cmd/run/footer.ts#L964-L966`](https://github.com/sst/opencode/blob/0112a92c416f5ad833d96e7a8308441f0a875d94/packages/opencode/src/cli/cmd/run/footer.ts#L964-L966)、
  [`session/llm.ts#L357-L364`](https://github.com/sst/opencode/blob/0112a92c416f5ad833d96e7a8308441f0a875d94/packages/opencode/src/session/llm.ts#L357-L364)

### 3.2 crush

commit [`76cc5c5`](https://github.com/charmbracelet/crush/tree/76cc5c574e15072b15aaed0f4f843a5711fae0d9)（2026-09-30）。

- **一个配置项、两种语义**：`request_timeout` 对非流式请求是硬 deadline，对流式请求是**空闲超时**——每收到一个
  part 就 `timer.Reset`，「so a slow but actively streaming response is never killed」；并且「Both the initial
  connection and gaps between parts share the same budget」，即首字节和流空闲共用同一个阈值。
  来源：[`internal/agent/request_timeout.go`](https://github.com/charmbracelet/crush/blob/76cc5c574e15072b15aaed0f4f843a5711fae0d9/internal/agent/request_timeout.go)
- 默认 2 分钟（`DefaultRequestTimeout`），0 关闭。（同文件的 jsonschema tag 写的是 `default=60`，与常量不一致，
  以常量为准。）
  来源：[`internal/config/config.go#L476-L498`](https://github.com/charmbracelet/crush/blob/76cc5c574e15072b15aaed0f4f843a5711fae0d9/internal/config/config.go#L476-L498)
- **超时与用户取消严格分型**：计时器用 `context.WithCancelCause` 带上自己的错误，事后用 `context.Cause(ctx)`
  判断是不是自己触发的，「so callers never mistake a timeout for a user cancellation」。超时给用户的文案带着
  怎么调大的提示（`The model stopped sending data for 2m0s. Increase the limit with …`）。
  来源同 `request_timeout.go`、
  [`internal/agent/agent.go#L1215-L1223`](https://github.com/charmbracelet/crush/blob/76cc5c574e15072b15aaed0f4f843a5711fae0d9/internal/agent/agent.go#L1215-L1223)
- 取消：`Esc` **按两次**（2 秒窗口，提示「press again to cancel」）→ `AgentCancel` → 调用该会话的
  `context.CancelFunc`。
  来源：[`internal/ui/model/ui.go#L5285-L5346`](https://github.com/charmbracelet/crush/blob/76cc5c574e15072b15aaed0f4f843a5711fae0d9/internal/ui/model/ui.go#L5285-L5346)、
  [`internal/agent/agent.go#L2096-L2120`](https://github.com/charmbracelet/crush/blob/76cc5c574e15072b15aaed0f4f843a5711fae0d9/internal/agent/agent.go#L2096-L2120)
- 超时之后是否自动重试：**未证实**（重试循环在依赖库 fantasy 里，没有读）。

### 3.3 aider

commit [`5dc9490`](https://github.com/Aider-AI/aider/tree/5dc9490bb35f9729ef2c95d00a19ccd30c26339c)。

- **只有总超时**：`request_timeout = 600` 作为 `timeout` 传给 litellm，`--timeout` 可改。没有首字节或流空闲
  超时（在 `models.py` / `base_coder.py` 里未见）。
  来源：[`aider/models.py#L26-L28`](https://github.com/Aider-AI/aider/blob/5dc9490bb35f9729ef2c95d00a19ccd30c26339c/aider/models.py#L26-L28)、
  [`models.py#L1020-L1021`](https://github.com/Aider-AI/aider/blob/5dc9490bb35f9729ef2c95d00a19ccd30c26339c/aider/models.py#L1020-L1021)、
  [`aider/args.py#L157-L162`](https://github.com/Aider-AI/aider/blob/5dc9490bb35f9729ef2c95d00a19ccd30c26339c/aider/args.py#L157-L162)
- 取消：`Ctrl+C` 即 Python 的 `KeyboardInterrupt`，打断当前回复，并把 `^C KeyboardInterrupt` 作为一条 user
  消息、`I see that you interrupted my previous reply.` 作为一条 assistant 消息**写进对话历史**；2 秒内再按一次
  才退出（`^C again to exit`）。
  来源：[`aider/coders/base_coder.py#L986-L1000`](https://github.com/Aider-AI/aider/blob/5dc9490bb35f9729ef2c95d00a19ccd30c26339c/aider/coders/base_coder.py#L986-L1000)、
  [`base_coder.py#L1575-L1583`](https://github.com/Aider-AI/aider/blob/5dc9490bb35f9729ef2c95d00a19ccd30c26339c/aider/coders/base_coder.py#L1575-L1583)
- `Ctrl+C` 之后底层连接是否立刻关闭：**未证实**（同步 Python，靠异常展开和 GC）。
- 重试：`Timeout` 等异常标记为可重试，延迟从 0.125 s 起每次翻倍，超过 `RETRY_TIMEOUT = 60` s 就放弃；每次打印
  `Retrying in N seconds...`。
  来源：[`base_coder.py#L1449-L1491`](https://github.com/Aider-AI/aider/blob/5dc9490bb35f9729ef2c95d00a19ccd30c26339c/aider/coders/base_coder.py#L1449-L1491)、
  [`aider/exceptions.py#L52-L56`](https://github.com/Aider-AI/aider/blob/5dc9490bb35f9729ef2c95d00a19ccd30c26339c/aider/exceptions.py#L52-L56)

### 3.4 Gemini CLI

commit [`c6bccb7`](https://github.com/google-gemini/gemini-cli/tree/c6bccb7ecbf6d8368d995455dd725ed34466faad)（2026-09-30）。

- **首字节 60 s + 流空闲 5 分钟，直接用 HTTP 库自带的两个旋钮**：给 undici 的全局 dispatcher 设
  `headersTimeout = 60000`、`bodyTimeout = 300000`（注释：「We keep body timeout high for LLM streaming
  responses」）。undici 文档对 `bodyTimeout` 的定义是「Monitors the time between consecutive body chunks」，
  所以它是空闲超时而不是总超时。
  来源：[`packages/core/src/utils/fetch.ts#L33-L34`](https://github.com/google-gemini/gemini-cli/blob/c6bccb7ecbf6d8368d995455dd725ed34466faad/packages/core/src/utils/fetch.ts#L33-L34)、
  [`fetch.ts#L216-L236`](https://github.com/google-gemini/gemini-cli/blob/c6bccb7ecbf6d8368d995455dd725ed34466faad/packages/core/src/utils/fetch.ts#L216-L236)、
  [undici `Client` 文档](https://github.com/nodejs/undici/blob/main/docs/docs/api/Client.md)
- 取消：`Esc` → `cancelOngoingRequest` → `abortControllerRef.current.abort()`，同一个 signal 传给请求和工具调用。
  单击即取消。
  来源：[`packages/cli/src/ui/hooks/useGeminiStream.ts#L870-L880`](https://github.com/google-gemini/gemini-cli/blob/c6bccb7ecbf6d8368d995455dd725ed34466faad/packages/cli/src/ui/hooks/useGeminiStream.ts#L870-L880)、
  [`useGeminiStream.ts#L945-L952`](https://github.com/google-gemini/gemini-cli/blob/c6bccb7ecbf6d8368d995455dd725ed34466faad/packages/cli/src/ui/hooks/useGeminiStream.ts#L945-L952)
- 重试：连接阶段默认最多 10 次（初始 5 s、封顶 30 s），`UND_ERR_HEADERS_TIMEOUT` / `UND_ERR_BODY_TIMEOUT` 都在
  可重试错误码里；**流中途**的错误单独限为最多 3 次重试，其中网络类错误在 `signal.aborted` 时不重试。
  来源：[`packages/core/src/utils/retry.ts#L20-L60`](https://github.com/google-gemini/gemini-cli/blob/c6bccb7ecbf6d8368d995455dd725ed34466faad/packages/core/src/utils/retry.ts#L20-L60)、
  [`packages/core/src/core/geminiChat.ts#L104-L108`](https://github.com/google-gemini/gemini-cli/blob/c6bccb7ecbf6d8368d995455dd725ed34466faad/packages/core/src/core/geminiChat.ts#L104-L108)、
  [`geminiChat.ts#L718-L771`](https://github.com/google-gemini/gemini-cli/blob/c6bccb7ecbf6d8368d995455dd725ed34466faad/packages/core/src/core/geminiChat.ts#L718-L771)

### 3.5 Cursor

闭源，也没有公开文档描述超时与重试。超时阈值、是否有首字节/空闲超时、重试策略：**全部未证实**。论坛上能确认的
只有可见现象与官方给的排障手段：

- 现象：长时间显示「Taking longer than expected…」；最终以 toast `Connection Error: The connection stalled` /
  `The connection stalled. Please try again.` 结束——即**超时后报错让用户自己重试**，没有看到自动重试的描述。
  来源：[Connection Error / Stalled Agent requests for hours](https://forum.cursor.com/t/connection-error-stalled-agent-requests-for-hours/149395)
- 官方人员给的第一条排障建议是**关 HTTP/2**（`"cursor.general.disableHttp2": true`，或 Settings → Network →
  HTTP Compatibility Mode → HTTP/1.1），并让用户跑内置的 Network Diagnostics；其中一例的诊断是「the long-lived
  chat stream's health checks are timing out」。
  来源同上、[Agent is stuck with "Taking longer than expected"](https://forum.cursor.com/t/agent-is-stuck-with-taking-longer-than-expected/163904)

---

## 4. 通用做法

### 4.1 HTTP 客户端的超时清单

| 手法 | 管哪一段 | reqwest | Go `net/http` | 备注 |
|---|---|---|---|---|
| connect timeout | 建连 | `connect_timeout`，默认无 | `Dialer.Timeout`（默认无）、`TLSHandshakeTimeout` | 黑洞路由最先在这里暴露 |
| 首字节 / header timeout | 请求发完到响应头 | 无专门选项，自己套 `tokio::time::timeout` | `Transport.ResponseHeaderTimeout` | undici 叫 `headersTimeout` |
| read / idle timeout | 相邻两次读之间 | `read_timeout`，默认无 | 无现成项，靠 context + 定时器（crush 即如此） | undici 叫 `bodyTimeout` |
| overall timeout | 从建连到 body 读完 | `timeout`，默认无 | `Client.Timeout` | **会杀死慢但活着的流** |
| 连接池空闲超时 | 池里闲置的连接 | `pool_idle_timeout`，默认 90 s | `IdleConnTimeout` | 少复用一条可能已被中间盒丢弃的连接 |
| TCP keep-alive | 内核层探活 | `tcp_keepalive*`，**默认已开**：15 s 空闲、15 s 间隔、3 次 | `Dialer.KeepAlive`，默认 15 s | 防 NAT 静默丢弃 |
| HTTP/2 PING | 协议层探活 | `http2_keep_alive_interval` / `_timeout`，默认关 | `HTTP2Config.SendPingTimeout` / `PingTimeout` | ping 无应答则关连接 |
| SSE 心跳 | 应用层探活（服务器发） | — | — | 注释行或 `ping` 事件 |

依据：

- reqwest：`timeout` 是「applied from when the request starts connecting until the response body has finished.
  Also considered a total deadline.」；`read_timeout` 是「applies to each read operation, and resets after a
  successful read. This is more appropriate for detecting stalled connections when the size isn't known
  beforehand.」HTTP/2 keep-alive：「If the ping is not acknowledged within the timeout, the connection will be
  closed.」TCP keep-alive 的默认值文档没写，要看源码里的 `Config` 默认（`Some(15 s)` / `Some(15 s)` / `Some(3)`）。
  来源：[reqwest `ClientBuilder`](https://docs.rs/reqwest/latest/reqwest/struct.ClientBuilder.html)、
  [`client.rs#L1455-L1462`](https://github.com/seanmonstar/reqwest/blob/0b7eb50770b7226d63b1da7e874bccef1569b5bb/src/async_impl/client.rs#L1455-L1462)、
  [`client.rs#L299-L306`](https://github.com/seanmonstar/reqwest/blob/0b7eb50770b7226d63b1da7e874bccef1569b5bb/src/async_impl/client.rs#L299-L306)
- Go：`Client.Timeout`「includes connection time, any redirects, and reading the response body. The timer
  remains running after Get, Head, Post, or Do return and will interrupt reading of the Response.Body.」；
  `ResponseHeaderTimeout`「specifies the amount of time to wait for a server's response headers after fully
  writing the request … This time does not include the time to read the response body.」
  HTTP/2：`PingTimeout`「is the timeout after which a connection will be closed if a response to a ping is not
  received.」
  来源：[`client.go#L92-L102`](https://github.com/golang/go/blob/577b91f3412310075064c203b766706805af8e7e/src/net/http/client.go#L92-L102)、
  [`transport.go#L185-L232`](https://github.com/golang/go/blob/577b91f3412310075064c203b766706805af8e7e/src/net/http/transport.go#L185-L232)、
  [`http.go#L277-L285`](https://github.com/golang/go/blob/577b91f3412310075064c203b766706805af8e7e/src/net/http/http.go#L277-L285)、
  [`net/dial.go#L127-L183`](https://github.com/golang/go/blob/577b91f3412310075064c203b766706805af8e7e/src/net/dial.go#L127-L183)
- undici：`headersTimeout`「the parser waits to receive the complete HTTP headers before the request times
  out」、`bodyTimeout`「Monitors the time between consecutive body chunks」，两者默认都是 300 s。
  来源：[undici `Client` 文档](https://github.com/nodejs/undici/blob/main/docs/docs/api/Client.md)
- SSE 心跳：WHATWG 规范的作者建议——「Legacy proxy servers are known to, in certain cases, drop HTTP
  connections after a short timeout. To protect against such proxy servers, authors can include a comment line
  (one starting with a ':' character) every 15 seconds or so.」Anthropic 的流里对应的是 `ping` 事件
  （「Event streams may also include any number of `ping` events.」）。
  来源：[HTML Standard · Server-sent events · Authoring notes](https://html.spec.whatwg.org/multipage/server-sent-events.html#authoring-notes)、
  [Streaming Messages](https://platform.claude.com/docs/en/build-with-claude/streaming)
- TCP keep-alive：Anthropic 文档——「Some networks may drop idle connections after a variable period of time,
  which can cause the request to fail or time out without receiving a response from Anthropic.」「setting a TCP
  socket keep-alive can reduce the impact of idle connection timeouts on some networks.」官方 SDK 默认就设。
  来源：[Errors · Long requests](https://platform.claude.com/docs/en/api/errors#long-requests)

两条从上面推出来的工程含义：

- **空闲超时必须按「字节」计，而不是按「解析出的事件」计**，否则心跳会被忽略而误杀。Claude Code 在网关连接上
  就踩过：「it counted only parsed response events there」导致 ping 还在来却报超时，v2.1.222 修掉。
  来源：[Error reference](https://code.claude.com/docs/en/errors#the-response-above-may-be-incomplete)
- **空闲阈值不能比「模型最长的合法沉默」短**。Claude Code 把最小值钳到 5 分钟，理由是 extended thinking 的停顿
  和代理缓冲；它早期的 90 s 默认值后来被放宽（见 §1.3）。

### 4.2 取消的正确实现：丢弃 / abort 到底断不断连接

**断。三个栈的官方文档都把它写成了承诺。**

- **hyper（reqwest 的底层）**：「Futures returned by hyper are cancel safe: dropping a future before it
  completes is the supported way to cancel the operation.」
  - HTTP/1：「has no in-protocol way to abort a single request without affecting the shared connection, so
    dropping an in-flight request future **closes the underlying TCP connection**.」
  - HTTP/2：「resets the single stream with `RST_STREAM` (`CANCEL` error code) and notifies the peer immediately
    … The shared connection stays usable for other in-flight and future requests.」

  来源：[`hyper/src/lib.rs#L33-L52`](https://github.com/hyperium/hyper/blob/c954d80cdcb91ae8faa0aa743639a13498f74b7c/src/lib.rs#L33-L52)、
  [`client/conn/http1.rs#L216-L223`](https://github.com/hyperium/hyper/blob/c954d80cdcb91ae8faa0aa743639a13498f74b7c/src/client/conn/http1.rs#L216-L223)、
  [`client/conn/http2.rs#L148-L155`](https://github.com/hyperium/hyper/blob/c954d80cdcb91ae8faa0aa743639a13498f74b7c/src/client/conn/http2.rs#L148-L155)
- **tokio**：`select!` 的定义就是「returning when the first branch completes, cancelling the remaining
  branches」，而取消即 drop。所以「`select!` 一支是 cancel token，一支是请求 future」等价于上面的 drop。
  来源：[`tokio::select!`](https://docs.rs/tokio/latest/tokio/macro.select.html)
- **Go**：「For an outgoing client request, the context controls the entire lifetime of a request and its
  response: obtaining a connection, sending the request, and reading the response headers and body.」HTTP/1
  路径上取消的实现是 `persistConn.cancelRequest` 直接 `closeLocked`——关连接。
  来源：[`request.go#L884-L888`](https://github.com/golang/go/blob/577b91f3412310075064c203b766706805af8e7e/src/net/http/request.go#L884-L888)、
  [`transport.go#L2317-L2322`](https://github.com/golang/go/blob/577b91f3412310075064c203b766706805af8e7e/src/net/http/transport.go#L2317-L2322)
- **Web / Node**：`AbortController.abort()`「is able to abort fetch requests, the consumption of any response
  bodies, or streams.」OpenAI 的 SDK 文档补了一句关键的：signal 可以取消请求，「including while its response
  body is being read」。
  来源：[MDN · AbortController.abort()](https://developer.mozilla.org/en-US/docs/Web/API/AbortController/abort)、
  [openai-node README · Timeouts](https://github.com/openai/openai-node/blob/master/README.md#timeouts)

**但有三个会让「drop 了却没断」的坑：**

1. **别的 task 还握着这条流。** 把 SSE 读取 spawn 成独立 task 之后，上层 drop 的只是 channel 接收端。Codex 的
   解法是读循环里多 select 一支 `tx_event.closed()`（§2.2 第 4 步）。
2. **HTTP/2 在请求体还没发完时 drop，曾经不发 `RST_STREAM`。** hyper 的
   [#4040](https://github.com/hyperium/hyper/issues/4040)：「Since `pipe_task` still holds the `SendStream`,
   the h2 crate considers the stream alive and does **not** send `RST_STREAM`.」由
   [#4042](https://github.com/hyperium/hyper/pull/4042) 修复，2026-03-31 合并。
   依赖版本早于这个修复时，大请求体 + 超时/取消会让流一直占着流控窗口。（具体从哪个 hyper 版本起包含该修复
   没有核对；iota 锁定的 hyper 1.11.1 源码里已有 `send_reset(h2::Reason::CANCEL)` 这条路径。）
3. **协作式取消可能等不到。** 任务如果卡在一个不看 cancel token 的 await 上就永远不收尾。Codex 的兜底是
   「cancel → 等 100 ms → `handle.abort()`」（§2.2 第 2 步）。

---

## 5. 失败与重试

### 5.1 连上了但一直不返回：重试，还是报错让用户决定

各家的答案其实一致：**有限次自动重试，同时把「正在重试」摆在明面上，并保证用户随时能打断。** 差别只在次数：

| 产品 | 首字节超时后 | 流空闲超时后 |
|---|---|---|
| Claude Code | 自动重发**一次**，第二次放宽到接近 10 分钟，再不行才报错 | 视进度：未产出内容则重发一次；已有产出则不重试、保留并提示不完整 |
| Codex CLI | （主路径无首字节超时） | 当作流断开，最多 5 次，显示 `Reconnecting... n/5` |
| opencode | 可重试，最多 5 次 | 可重试，最多 5 次 |
| Gemini CLI | 可重试（连接阶段最多 10 次） | 可重试，但流中途最多 3 次 |
| aider | 总超时可重试，退避累计到 60 s 为止 | —（无此机制） |
| crush | 报错并提示如何调大阈值；是否重试未证实 | 同左 |
| Cursor | 报错「Please try again」，由用户重试；自动重试未证实 | 同左 |

来源见各产品小节。

Claude Code「首字节重试时放宽 deadline」这一招值得单独记一笔：第一次用短阈值是为了快速甩掉坏连接，第二次用长
阈值是为了不误杀「把整个响应攒完才吐」的代理/网关。两种故障用同一个阈值是区分不开的。

### 5.2 重试与幂等

- HTTP 层的规矩：RFC 9110 §9.2.2——「A client SHOULD NOT automatically retry a request with a non-idempotent
  method unless it has some means to know that the request semantics are actually idempotent, regardless of
  the method, or some means to detect that the original request was never applied.」Go 的 `Transport` 照此实现：
  只在「连接此前成功用过、这次遇到网络错误」且请求幂等时自动重试，幂等的判定是方法为
  GET/HEAD/OPTIONS/TRACE/QUERY，或带了 `Idempotency-Key` / `X-Idempotency-Key` 头。
  来源：[RFC 9110 §9.2.2](https://www.rfc-editor.org/rfc/rfc9110.html#section-9.2.2)、
  [`transport.go#L90-L97`](https://github.com/golang/go/blob/577b91f3412310075064c203b766706805af8e7e/src/net/http/transport.go#L90-L97)
- LLM SDK 的实际做法是**直接重试 POST**：Anthropic 与 OpenAI 的官方 SDK 默认都对连接错误、408、409、429、
  ≥500 重试 2 次，并且「requests that time out are retried twice by default」。
  来源：[Anthropic TypeScript SDK · Retries / Timeouts](https://platform.claude.com/docs/en/cli-sdks-libraries/sdks/typescript)、
  [openai-node README · Retries](https://github.com/openai/openai-node/blob/master/README.md#retries)
- **推论**：这之所以可接受，是因为一次补全请求在服务端没有用户可见的副作用——副作用发生在**客户端执行工具调用**
  的那一刻。所以 agent 的幂等边界不在 HTTP 方法上，而在「这次响应里有没有已经完成（或已开始）的工具调用」。
  Claude Code 的分级表（§1.5）就是这条边界的直接编码，理由原文：「because that could execute the same tool calls
  twice」。它的环境变量文档还给了一个反例：非流式回退在某些代理后面会「produce duplicate tool execution」，
  所以留了 `CLAUDE_CODE_DISABLE_NONSTREAMING_FALLBACK`。
  来源：[Error reference · Automatic retries](https://code.claude.com/docs/en/errors#automatic-retries)、
  [Environment variables](https://code.claude.com/docs/en/env-vars)
- 已收到的部分内容怎么办：Anthropic 的建议是保留，而不是丢弃重来——把已收到的部分带进下一次请求让模型续写；
  但「Tool use and extended thinking blocks cannot be partially recovered. You can resume streaming from the
  most recent text block.」
  来源：[Streaming Messages · Error recovery](https://platform.claude.com/docs/en/build-with-claude/streaming#error-recovery)
- 重试的代价不只是副作用：被超时掐掉又重发的请求，服务端是否继续生成并计费，**未证实**（没有找到官方说明）。

---

## 6. 横向对比表

| 产品 | 取消机制 | 有没有超时、是哪一种 | 挂住时的 UX | 重试策略 |
|---|---|---|---|---|
| **Claude Code** | `Esc` 单击取消本轮；`Ctrl+C` 有任务时取消、空闲时两次退出。线路层是否 abort：未证实（间接证据指向是） | 首字节 180/300 s（+1 s/32 KB）；字节级空闲 180/300 s；事件级空闲 300 s；总超时 10 min | 20 s 无数据即显示 `Waiting for API response · will retry in …`；重试时显示倒计时与 `attempt x/y` | 默认 10 次指数退避；按响应进度分级，完成过文本块/工具调用后不重试 |
| **Codex CLI** | `Esc` / `Ctrl+C` 发 `Op::Interrupt`；cancel token → 100 ms 宽限 → 硬 abort；请求与读流都 `select!` 取消 | 流空闲 300 s；无总超时；主路径首字节超时未见 | `Reconnecting... n/5`；首次 WebSocket 重连在 release 版里不提示 | HTTP 层 4 次 + 流层 5 次；耗尽后 WebSocket→HTTPS 降级 |
| **opencode** | `Esc` 两次（5 s 窗口）；`AbortController` 随作用域释放 | 首字节 300 s；流空闲 300 s；总超时可选 | 会话状态里带重试次数、原因、下次重试时刻 | 超时可重试，最多 5 次，2 s 起倍增 |
| **crush** | `Esc` 两次（2 s 窗口）；`context.CancelFunc` | 一个 `request_timeout`（默认 2 min）：流式=空闲超时（含首字节），非流式=硬 deadline | 报 `Request timed out` 并提示如何调大 | 未证实 |
| **aider** | `Ctrl+C` = `KeyboardInterrupt`，打断并写入历史；2 s 内再按退出 | 只有总超时 600 s | 逐次打印 `Retrying in N seconds...` | 0.125 s 起倍增，累计到 60 s 放弃 |
| **Gemini CLI** | `Esc` 单击；`AbortController.abort()` | 首字节 60 s；流空闲 300 s（undici `headersTimeout` / `bodyTimeout`） | 发 retry-attempt 事件给 UI | 连接阶段最多 10 次；流中途最多 3 次 |
| **Cursor** | 未证实 | 未证实（现象上存在「connection stalled」超时） | `Taking longer than expected…` → `The connection stalled. Please try again.` | 未证实（看到的是让用户重试） |

---

## 7. 对 iota 的启示：可复用的成熟做法

先交代现状（读代码所得，未改动）：

- 已有首字节超时：`HEADER_TIMEOUT = 120 s`（`src/llm/client.rs:29`），套在 `http.execute` 外面，并与取消 token
  `select!`（`src/llm/client.rs:287-305`）。
- 首字节超时可重试，`DEFAULT_RETRIES = 2`（`src/llm/client.rs:27`、`:479`）——**每次都是同一个 120 s**，
  所以最坏情况是 3 × 120 s ≈ 6 分钟的沉默之后才报错。
- 读 SSE 只和取消 token `select!`，**没有空闲超时**（`src/llm/sse.rs:160-164`）；流建立之后服务器不再发字节，
  就只能等用户按 ESC。
- `default_http_client()` 是裸的 `Client::builder().build()`（`src/llm/client.rs:525-532`），所以拿到的是
  reqwest 0.13.4 的默认值（查的是 `Cargo.lock` 锁定版本的源码）：TCP keep-alive **已开**（15 s / 15 s / 3 次）、
  连接池空闲 90 s；**没有** connect timeout、read timeout、总超时，HTTP/2 PING 关闭。
- 取消路径本身是对的：`select!` 丢弃 `execute` future / `body.next()`，按 §4.2 的 hyper 文档这会关 TCP 或发
  `RST_STREAM`；锁定的 hyper 1.11.1 自带这段 cancel-safety 文档，也包含 #4042 的修复。SSE 读取没有 spawn 成
  独立 task，所以不存在 §4.2 的第 1 个坑。

### 值得抄

1. **给 SSE 读加字节级空闲超时**（Claude Code、Codex、opencode、Gemini CLI、crush 全都有，只有 aider 没有）。
   - 按**字节**重置而不是按解析出的事件重置，这样 provider 的 ping / 注释行心跳自动算存活（§4.1）。
   - 阈值取 2–5 分钟并可配置，0 关闭（本地慢模型用）。别设成几十秒：Claude Code 的 90 s 被放宽过。
   - 实现上就是 `src/llm/sse.rs:160-164` 那个 `select!` 再多一支 `sleep`，改动面很小。
2. **首字节超时改成「短—长」两段，而不是三次等长**（Claude Code 的做法，§5.1）。第一次短阈值甩掉坏连接，
   重试一次并放宽，仍无应答就报错。把最坏沉默从 6 分钟压到「短阈值 + 一次可见的重试」。大请求体按体积加时
   （Claude Code 是每 32 KB 加 1 s）。
3. **沉默超过约 20 秒就在状态行说出来**，并带上倒计时和「esc 取消」（Claude Code 的
   `Waiting for API response · will retry in …`；Codex 的 `Reconnecting... n/m`）。这是成本最低、体感收益最大的
   一条，而且与超时阈值怎么定无关。
4. **重试决策按响应进度分级**（§1.5 的表）：没产出内容 → 重试；已完成过文本块或工具调用 → 不重试，保留已有产出
   并标注「可能不完整」。iota 目前「只重试到拿到响应头为止」已经天然落在安全一侧；加了空闲超时之后要守住这条
   边界，不要顺手把流中途的超时也接进重试循环。
5. **超时错误与用户取消分型**（crush 的 `context.Cause`）。两者最终都表现为「请求被掐断」，但一个要提示/重试，
   一个要安静结束。iota 已有 `LlmError::HeaderTimeout` 与 `LlmError::Cancelled` 之分，新加的空闲超时照此单列
   一个变体，文案里带上怎么调阈值。
6. **取消要有硬兜底**：协作式 cancel 之后给一个很短的宽限期（Codex 是 100 ms），到点就 abort task。防的是
   某个 await 没接 cancel token 导致 ESC 无效——这类 bug 的表现和「请求挂住」一模一样。
7. **看到终止事件立刻返回，不等 EOF**（Codex #43140 的反面教材）。空闲超时只是兜底，不能替代协议层的结束判定。
8. **连接层的廉价保险**：加 `connect_timeout`（10–30 s）；用了 HTTP/2 再加 `http2_keep_alive_interval` /
   `_timeout`，让坏掉的连接在几十秒内被协议层发现，而不是等空闲超时。TCP keep-alive 不用加——reqwest 默认
   已开，Anthropic 官方 SDK 也是默认设（§4.1）。但 TCP keep-alive 只能发现「对端/路径死了」，发现不了
   「连接活着、服务器就是不发数据」，所以它替代不了第 1 条。
9. **超时/连接错误后的重试走新连接**（Claude Code v2.1.214）。#83238 的实测是坏的池化连接卡 9 分钟，换连接
   12 秒完成。reqwest 下 drop 一个 HTTP/1 请求本来就会关连接；HTTP/2 多路复用时坏的是整条连接，只 reset 一个
   stream 不够，这正是 HTTP/2 PING 要解决的。
10. **ESC 单击即取消**（Claude Code、Codex、Gemini CLI）。iota 保持单击。

### 不适合终端 CLI，别抄

- **对流式请求用总超时**（aider 的 600 s、reqwest 的 `timeout`）。它分不清「慢但活着」和「死了」，长回复会被
  误杀；crush 的注释和 reqwest 的文档都明说该用空闲超时。
- **十次以上的静默重试**。Claude Code 默认 10 次、`CLAUDE_CODE_RETRY_WATCHDOG` 下 300 次，Codex 有无限重连
  flag——这些是给无人值守任务准备的。交互式 CLI 里人就坐在屏幕前，少量重试 + 明确倒计时 + 尽快把决定权交回去
  更合适。如果将来 headless 模式要用，单独开关。
- **双击 ESC 才取消**（opencode、crush）。它防的是误触，代价是在「请求挂住、用户已经急了」的时刻多一步。
  Codex 试过把退出改成双击，又以「feels janky」关掉了。
- **非流式回退**（Claude Code 流失败后改用非流式重发）。官方自己承认在某些代理后会重复执行工具，还需要再配
  一套超时；收益主要在企业网关场景，复杂度不值。
- **aider 式把 `^C` 写成对话消息**。它适合 aider「一问一答改文件」的模型，但在有工具调用的 agent loop 里会和
  未配对的 `tool_use` 搅在一起；iota 应沿用自己现有的中断记录方式。
- **GUI 式的网络诊断面板**（Cursor）。终端里对应物是一个 `http1_only` 之类的配置项加一条清楚的报错，够了。

### 未决问题（调研没能回答的）

- Claude Code 的 ESC 在线路层的确切行为（闭源，只有间接证据）。
- Codex 主采样路径等响应头是否在别处有界。
- 被客户端 abort 的请求，各家服务端是否停止生成与计费。
- 各 provider 的心跳间隔（Anthropic 有 `ping` 事件但文档没给频率；OpenAI、Google 的流是否有心跳没有查）。
  这直接决定 iota 的空闲阈值能设多短，定值之前应当用抓包实测。
