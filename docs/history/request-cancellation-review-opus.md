# 请求挂住 / 取消修复 —— 独立评审

> 2026-10-01 · 分支 `req-cancel` · 评审对象 `git diff main...HEAD`（`e97d131` 勘察、`548c5fa` 调研、`02bb3bb` 修复）
> 评审只读：没有改代码和测试。变异实验在 scratchpad 里的一份 `git archive HEAD` 副本上做（`target` 用 APFS clone），本树没有动过。
> 标「实测」的结论附命令输出；标「查证」的来自读代码，附行号；标「推测」的没有验证。

## 结论

**能合并。** 修复命中了勘察确认的两个缺陷：A 是取消盲区，B 是响应头之后 body 停滞没有上界。新错误也没有接进重试，边界守住了。测试是真的：变异实验里，关键的变异都能让测试变红。整条 `cargo test` 和 `cargo clippy --all-targets` 都是绿的。

建议合并前顺手补两处小改动（各 1–3 行），不补也不构成阻塞：

1. **流收到 `[DONE]` 之后，如果连接没关，会在 180 s 后被报成 `Response stalled`，整轮回滚**（实测，§1.4）。也就是说，一个已经完整结束的回答被当成了错误。
2. **`interrupt_turn` 无条件 abort 标题请求**。第一轮按 ESC 时如果保留了部分输出，这个会话就**永远**拿不到模型生成的标题（查证，§2.3）。这是正常路径上的回归。

另有一条建议在合并前拍板：

3. **默认值从 180 s 改成 300 s。** iota 没有请求 reasoning summary，所以 OpenAI 推理模型在思考期间推测是一个字节都不发的；超过 180 s 就会被误杀，而且这一轮不重试、整轮回滚（§1.2）。

最严重的三个发现就是上面这三条。

---

## 1. 正确性：空闲超时会不会误杀正常请求

**结论：机制正确，按字节重置，计时器每个 chunk 新建一个。有两处会误杀：一处是终止事件之后连接没关（实测），一处是 180 s 对静默推理太短（推测）。环境变量的解析没有危险的坑。**

### 1.1 机制

- `src/llm/sse.rs:176-186`：`read_line` 每次 await body 时都把 `idle_elapsed(self.idle)` 放进 `select!` 当第三支，并排在 cancel 之后（`biased`）。每次循环都是一个**新的** sleep，所以计的是相邻两个 chunk 的间隔，跟流的总长没关系。只要来了一个 chunk 就重置，哪怕这个 chunk 不构成完整的一行。
- 计时器只在 `next()` 内部 await 时才走，消费方处理事件花的时间不算进去。这样不会因为 UI 慢而误杀。
- 计时从响应头之后才开始：`Sse` 在 `Client::stream` 拿到头之后才创建（`src/llm/client.rs:279`）。上传大请求、等头这两段仍归 `HEADER_TIMEOUT`，**不受**空闲超时影响。
- 心跳：`:` 注释行（OpenRouter、DeepSeek 等）和 Anthropic 的 `event: ping` 都是字节，都会重置计时器。测试 `a_heartbeat_keeps_a_stream_alive_past_the_idle_bound` 证明了前一种；Anthropic 的 ping 本身就能解析成事件，按事件计也不会误杀。
- 图像流（`src/llm/images.rs:378` 直接 `Sse::new`）不设上界，跟 DIVERGENCES X-64 的描述一致。MCP 不经过 `llm::Client::stream`（`src/mcp/transport.rs` 走自己的 `AuthClient`），不受影响（查证）。

### 1.2 场景逐个过

| 场景 | 判断 | 依据 |
|---|---|---|
| provider 心跳 / ping | 不误杀 | 按字节重置（上节）。测试中 100 ms 一个心跳，阈值 300 ms，整体 900 ms 后事件照常到达 |
| Anthropic extended thinking | 不误杀（推测） | thinking delta 本身就是字节，另外还有 `ping` 事件 |
| **OpenAI 推理模型（Responses / Chat Completions）** | **有误杀风险（推测）** | iota 没有请求 `reasoning.summary`（`grep summary src/llm/responses.rs src/provider/openresponses.rs` 只命中解析与测试），所以推理期间推测没有任何字节。high effort 或 pro 级模型思考超过 3 分钟并不罕见。误杀之后不重试，整轮回滚（`src/repl/run.rs:749`），用户已经付费的推理全部丢掉 |
| 大请求上传后的等待 | 不受影响 | 空闲计时从响应头之后开始；prefill 发生在头之前或之后都只占一个间隔，正常情况下远小于 180 s |
| 本地慢模型 | `0` 只覆盖响应头之后（推测） | 如果本地 server 要等第一个 token 才 flush 响应头（Go `net/http` 的默认行为），挡住它的是 `HEADER_TIMEOUT` 120 s × 3 次，外加 turn 层重放，`IOTA_STREAM_IDLE_TIMEOUT=0` 管不到。提交说明里「0 disables it for a slow local model」只对先发头的 server 成立。这是早就存在的行为，不是本次引入的 |
| 代理 / CDN 缓冲整个 SSE body | 有误杀风险（推测） | 先发头、body 攒完才吐的代理，生成超过阈值就会被杀。Claude Code 把下限钳到 5 min，理由里明确写了 proxy buffering（调研 §1.3） |
| **收到终止事件，但服务器 / 中转没关连接** | **会误报（实测）** | 见 §1.4 |

**建议（值得做）：** 把 `STREAM_IDLE_TIMEOUT`（`src/llm/client.rs:35`）改成 300 s。理由有三：

- Codex（OpenAI 自己的客户端）、opencode、Gemini CLI 都用 300 s；Claude Code 对非直连端点也是 300 s（调研 §1.3、§6）。
- iota 是多 provider、经常走中转的场景，更接近「非直连」。
- ESC 一直有效，空闲超时只是没人值守时的兜底。误杀的代价（付了费的推理丢失、不重试、整轮回滚）远大于晚两分钟发现死流。

改法是改一个常量，再改 `stream_idle_timeout_reads_the_override` 里那一行断言。

### 1.3 `IOTA_STREAM_IDLE_TIMEOUT` 的解析

`src/llm/client.rs:564-570`，`s.trim().parse::<u64>()`：

| 输入 | 结果 | 评价 |
|---|---|---|
| 未设置、`""` | 默认 180 s | 合理（空值等于没设） |
| `"0"`、`"00"` | 关闭 | 合理 |
| `"600"`、`" 600 "`、`"+600"` | 600 s | 合理（Rust 的 `u64::from_str` 接受前导 `+`） |
| `"-5"`、`"abc"`、`"1.5"`、`"3m"`、`"180s"` | **静默**回落到默认值 | 小坑：写成 `5m` 的用户要再被杀一次才会发现没生效。不过报错文案写的是 `=<seconds>`，可以接受 |
| 超大值（如 `18446744073709551615`） | 不会 panic | tokio `sleep` 溢出时用 `checked_add`，退回 `far_future`（tokio `time/sleep.rs:126-128`） |

单测 `client::tests::stream_idle_timeout_reads_the_override` 覆盖了上表除 `+` 和超大值之外的情况。**不建议**为非法值加告警：这里只有这一个读点，加告警的复杂度换不来多少。

### 1.4 实测：`[DONE]` 之后连接不关

在 scratch 副本的 `tests/provider/wire.rs` 里加了一个探针，复用 `head_then`：服务器发 `data: {"ok":1}` 和 `data: [DONE]`，然后连接保持不关，阈值 300 ms：

```
first = Ok(Some("{\"ok\":1}"))
second = Ok(Err(StreamIdle(300ms))) done_seen=true
```

原因：`sse.rs:97-103` 看到 `[DONE]` 后会 `continue; // drain the remainder`，一直读到 EOF。Anthropic 的 `message_stop`（`src/provider/anthropic.rs:291`，当成 `Other` 处理）和 Responses 的 `response.completed`（`src/provider/openresponses.rs:380`）也都是等 EOF 才结束。

修复前这种连接会让 iota 永远挂住；用户按 ESC 时，部分结果保留规则（`interrupt.rs`）会把完整回答**保住**。修复后 180 s 自动报 `Response stalled`，走错误路径 `history.truncate(hist0 - 1)`（`src/repl/run.rs:749`），**一个完整的回答被回滚**。调研 §2.3 和 §7 第 7 条点名过这个坑（Codex #43140）。

出现频率推测不高，规范的 server 会发结束 chunk；但不规范的中转站确实存在（推测）。

**建议（必要，1–3 行）：** 在 `sse.rs` 的空闲分支里，如果 `self.done` 已经为真，就按干净 EOF 返回，不报 `StreamIdle`。Anthropic 和 Responses 在终止事件处直接 `break` 是同一类改动，可以留到下次。

## 2. 取消盲区的修法

**结论：没有引入新的等待或死锁；ESC 后马上发下一条、以及退出这两条路径都已修好（tmux 实测，变异后变红）。但修法是「中断就 abort」而不是「join 时监听取消」，这带来一个正常路径上的回归，并留下一个小盲区。提交说明的描述也不准确。**

### 2.1 修法是什么

- `SessionSlot::abort_title`（`src/repl/state.rs:128-133`）：`take()` 出 JoinHandle 后 `abort()`，**不 await**。被 abort 的任务结果不会再被任何地方 await，JoinHandle 直接丢掉。它带的 `tokio::sync::Mutex` 守卫，会在 runtime 下次 poll 时随 future 一起释放。
- 两个调用点：`interrupt_turn` 开头（`src/repl/run.rs:904`），以及 turn 报错的回滚路径（`src/repl/run.rs:751`）。
- `join_title`（`state.rs:119-123`）**没有改**，仍然是不带 select 的 `h.await`。提交说明写的是「that await now listens for cancel」，**和代码不符**。DIVERGENCES X-64 的描述（「aborts the title pass it started」）是对的。

### 2.2 新的等待或死锁？

- **ESC 后马上发下一条**：`join_title` 这时什么都不用等（handle 已被 take）。会话名在丢弃分支里已经 `unseed`，所以下一条消息会重新 `seed` 并 spawn 新的标题任务（`run.rs:865-885`）。新任务的 `tp.lock().await` 最多等被 abort 的旧任务完成 drop，大约是一次调度的时间，不会死锁（查证）。tmux 场景 27 第 1 段实测：8 s 内收到 `echo: second`。
- **退出（ESC 后空闲时按 Ctrl+C）**：`run.rs:560` 的 `join_title` 同样什么都不用等。场景 27 第 2 段实测：8 s 内 `pane_dead`。
- **`land` 与 `abort` 竞争**：`titler.land` 是同步调用，位于 await 之后，abort 只会在 await 点生效，所以 land 不会只执行一半（查证）。
- **abort 时 drop 正在进行中的请求**：修复前 30 s 的 `timeout` 本来也是这样 drop 的，这不是新路径。

### 2.3 回归：保留部分输出时，标题永远丢失

`interrupt_turn` 在做三态判定**之前**无条件 abort（`run.rs:904`）。如果判定结果是 `persist: true`（已经有部分正文，`interrupt.rs:40-53`），就不会调用 `unseed`，`TitleState.seeded` 保持为真。之后每一次 `seed()` 都在 `if st.seeded { return None }`（`src/repl/title.rs:96`）这里返回，**这个会话再也不会发起模型标题请求**，名字永远停在前 40 个字的占位符上。

触发条件：第一轮按 ESC 时，正文已经开始出了，但标题还没回来。标题请求用的是会话模型（`run.rs:873` `set_model(model)`），推理模型生成标题可能要好几秒到十几秒，所以这种情况推测并不少见。代码里的注释（「a kept partial keeps the placeholder name」）承认了这一点，但没说这个名字会永久保留。

**建议（必要，几行）：** 把 `abort_title()` 移到 `else` 分支里，和 `unseed` 放在一起（只在丢弃整轮时 abort）。卡住的 provider 一个字节都不发，所以 partial 为空，必然走丢弃分支，盲区照样能修好，场景 27 不受影响。有部分输出说明 provider 是活的，标题请求推测也会正常落地。

### 2.4 残留的小盲区

如果一轮**成功**结束，但标题请求卡住了（比如 provider 时好时坏，或者只有单发请求卡住），下一条输入和退出时的 `join_title` 仍然会等满 30 s，期间 ESC 和 Ctrl+C 都没反应。这个缺陷 A 的形态还在，只是触发概率低很多（推测）。

要彻底消掉这个盲区，得让 `join_title` 能被取消，这涉及空 cancel 栈下的按键路由。**不建议**在这次修复里做。在报告里记一笔，并把提交说明的措辞更正为「aborted on interrupt/error」即可。

## 3. 边界：流中途超时没有接进重试

**结论：守住了，而且有两道防线；`StreamIdle` 和 `Cancelled` 的分型是干净的。但提交说明和代码注释里给出的理由不准确。**

### 3.1 两道防线

- **客户端层**：`Client::send` 的重试循环在 `src/llm/client.rs:351` 拿到成功的响应头就 `return Ok(resp)`，body 是在循环外面由 `Sse` 读的，所以 `StreamIdle` 结构上进不了 `should_retry`。`should_retry` 本身对 `StreamIdle` 也返回 false（`client.rs:514-519`，由 `_ => false` 兜住）。
- **turn 层**：`is_retryable`（`src/repl/turn/retry.rs:92`）把 `StreamIdle` 和 `NoEvents`、`Cancelled` 归在同一类，返回 false。这一类错误既不会被 `retry_round`（`retry.rs:52`）重放，也不会被 `Turn::run` 的整轮重放（`src/repl/turn/mod.rs:345`）重放。
- **传播路径**：四个 dialect 都用 `ProviderError::wire(WireOp::Stream, e)` 包装错误（`provider/anthropic.rs:224`、`openai.rs:338`、`google.rs:487`、`openresponses.rs:311`）。`wire()` 只把 `Cancelled` 拆成 `ProviderError::Cancelled`（`provider/error.rs:74-76`），`StreamIdle` 会原样保留到 `is_retryable` 和 `describe_llm`。

### 3.2 分型

| | `LlmError::Cancelled` | `LlmError::StreamIdle(d)` |
|---|---|---|
| 来源 | `sse.rs` 中 cancel 分支（biased，优先于超时） | `sse.rs` 中 idle 分支 |
| provider 层 | `ProviderError::Cancelled` | `ProviderError::Wire{Stream, StreamIdle}` |
| REPL | `interrupt_turn`：`Interrupted.`，不显示红块 | 红块 `Response stalled`（`src/repl/errors.rs:68-72`），正文是 `no data from the provider for 3m0s (stream idle timeout); set IOTA_STREAM_IDLE_TIMEOUT=<seconds> to wait longer, or 0 to never time out` |
| 重试 | 否 | 否 |

场景 27 第 3 段断言了「是错误而不是中断」（`count_all 'Interrupted.'` 为 0）。

### 3.3 理由写得不准（文档层面，非阻塞）

提交说明、`client.rs:266-270` 和 `retry.rs:81-84` 的说法都是「重放会让工具调用执行两次」。在 iota 里这不成立：`retry_round` 重发的是同一个 round 的请求（`tools.rs:62-65` 的注释：send 每轮只组装一次），而这个 round 的工具调用要等流**结束之后**才由 `walk` 执行（`tools.rs:140`）。流中途断掉时，这一轮的工具还没执行。现有代码对流中途的 `Transport` 错误本来就照常重试（`retry.rs:94` `_ => true`），这正说明重放本身是安全的。

不重试 `StreamIdle` 的**真正**好理由是：每次尝试都要先静默 180 s，再乘以 10 次 turn 层重试，会重新造出一个「挂半小时」的体感。决策本身是对的，建议把注释里的理由改成这一条，免得以后有人照着「工具执行两次」去推理别的错误。

## 4. 测试是不是假绿

**结论：核心测试都是真的。变异实验里每个关键变异都让至少一个测试变红。HTTP/2 PING 只靠常量「钉」住，实际上什么都没测到。**

### 4.1 变异实验（scratch 副本，本树未改）

| # | 变异 | 结果 |
|---|---|---|
| M1 | `sse.rs` 空闲分支改成 `idle_elapsed(None)`（等于把超时关掉） | `a_silent_stream_fails_at_the_idle_bound` **FAILED**（`wire.rs:640`，5 s 内没触发）；另外两个仍为绿 |
| M2 | 计时改成**按事件**：每次 `next()` 开头记一个起点，扣掉已经过去的时间 | `a_heartbeat_keeps_a_stream_alive_past_the_idle_bound` **FAILED**（`wire.rs:678`「a heartbeat must not count as silence」）。这证明心跳测试真的区分得出按字节计和按事件计 |
| M1b | `provider/common.rs` 忽略 transport 的 `stream_idle`，固定用 180 s（等于把环境变量的覆盖丢掉 / 阈值调大） | tmux 27 **FAIL** `the idle bound never fired`、`the error names the knob: got '0'` |
| M3 | 删掉 `interrupt_turn` 里的 `abort_title()` | tmux 27 **FAIL** `the next message waited on the title pass`、`the exit waited on the title pass`（这次运行 55 s，原来 7 s） |
| M4 | 在 `is_retryable` 里把 `StreamIdle` 放回可重试 | 单测 `is_retryable_follows_the_status_table` 会红（读代码可知）；tmux 27 **FAIL** `a mid-stream stall is not retried: got '1'`、`the error names the knob: got '0'` |
| M5 | 从 builder 里删掉两行 `http2_keep_alive_*` | `the_default_client_carries_the_connection_bounds` **仍然 ok**，见 §4.3 |

基线（未变异）：`IOTA_TMUX=1 cargo test --test ui_tmux -- tmux_hung_provider` 结果为 `PASS=7 FAIL=0`，耗时 6.79 s。全量 `cargo test` 全部 ok；`cargo clippy --all-targets` 退出码 0。

### 4.2 逐条看

- **黑洞用例**：同时断言了错误类型、`d == idle`、`idle ≤ elapsed < 3 s`、文案里有调节方法、`!should_retry`，以及服务器只被 accept 了一次。M1 下变红。
- **心跳用例**：断言事件在超过阈值两倍之后（≥ 600 ms）仍能到达，并且没有报错。M2 下变红，所以它证明的确实是「心跳算存活」。
  - 小隐患（推测）：服务端和客户端跑在同一个 `#[tokio::test]` current-thread runtime 上，100 ms 心跳对 300 ms 阈值只有 3 倍余量。CI 负载很高时，偶尔有一次 sleep 超过 300 ms 就会 flaky。可以把余量放大一些（比如心跳 100 ms、阈值 1 s），代价是多跑约 2 s。不急。
- **场景 27**：断言的是「8 s 内」看到 `echo: second` / `pane_dead`，是**正向的「不再等待」**，不是「没有报错」。M3 下两条都变红。
  - 小瑕疵：第 3 段的 `Response stalled` 那条在 M4 下仍然 PASS，因为重试时的 busy 行里也有这几个字。好在同一段里的另外两条会把它拦住。
- **`connect_timeout`**：通过 builder 的 `Debug` 输出断言 `connect_timeout: 15s`，并断言 `timeout: ` 只出现一次（即没有整体超时、没有 read 超时）。这能钉住 builder 的配置，但没有测「连黑洞地址 15 s 后报错」。可以接受：行为属于 reqwest，自己再测一遍价值不大。

### 4.3 HTTP/2 PING：常量钉住 ≠ 测到

`the_default_client_carries_the_connection_bounds`（`client.rs:687-697`）对 PING 只断言了 `HTTP2_KEEP_ALIVE_TIMEOUT < HTTP2_KEEP_ALIVE_INTERVAL`，这是常量之间的比较。M5 把 builder 里的两行调用删掉后，测试仍然是绿的。测试注释如实写了「pinned by their constants alone」，没有假装测到了什么，但实际效果就是**这个配置没有任何测试保护**。见 §5 的建议。

## 5. 有没有过度设计

| 项 | 判断 | 理由 |
|---|---|---|
| 字节级空闲超时 | 该有 | 缺陷 B 的直接修复；实现只是 `select!` 多一支（`sse.rs:180-183`）加一个 `idle_elapsed` 函数 |
| `IOTA_STREAM_IDLE_TIMEOUT` | 该有 | 阈值不可能对所有 provider、代理、本地模型都合适，而且误杀之后不重试，用户必须有办法调大。项目里已经有 `IOTA_SHELL_YIELD` 这类先例。最关键的是报错文案里直接写出了变量名，用户需要它的那一刻就能看到（X-55 删掉开关时的理由是「需要的人找不到」，这里不成立）。读点只有一个（`cmd/mod.rs:71`），通过 `HttpTransport.stream_idle` 下传，没有引入全局状态 |
| 状态行 `Waiting for the first token` | 该有 | 一个常量、一个 bool 参数，把「连不上」和「连上了但模型不说话」区分开。这是诊断挂住最便宜的手段（勘察 §3、调研 §7 第 3 条） |
| `connect_timeout` 15 s | 该有，取值合理 | 黑洞路由从「120 s × 3」变成「15 s × 3」。这个值覆盖 TCP + TLS + 代理 CONNECT，15 s 对慢代理也够用，和 Codex 的 WebSocket 建连超时、调研给的 10–30 s 区间一致 |
| **HTTP/2 PING（30 s 间隔、20 s 超时）** | **建议删掉** | 见下文 |

HTTP/2 PING 建议删掉的理由：

1. 它能覆盖的场景（h2 连接中途死掉），空闲超时在 180 s 内也能覆盖，ESC 则随时都能覆盖。它唯一的增量是把发现时间从约 3 分钟缩短到约 50 s。
2. 它引入了一种新的失败模式。hyper 只在连接静默期间发 ping（`hyper-1.11.1/src/proto/h2/ping.rs`，以 `last_read_at` 为基准）：正好是模型长时间思考、没有字节的时候，每 30 s 一个。按 gRPC 风格做 ping 限流的服务端（默认最小间隔 5 min、两次违规就发 `GOAWAY ENHANCE_YOUR_CALM`）会把连接踢掉，表现为流中途出现 `Transport` 错误，然后整轮被重放（推测：没有查到 Anthropic、OpenAI、Google 的 REST 前端是否有这种限流）。
3. Go 原版没有 PING（X-64 的 Go 列），而 Go 原版是能用的产品。
4. M5 证明它没有任何测试保护。

按「最简实现、不拿能用的产品换未完成的复杂度」的原则，应该删掉，连同两个常量和那条常量断言。如果坚持保留，至少把间隔提到 ≥ 60 s，并在 DIVERGENCES 里写明「未实测 ping 限流」。

## 6. 用户的体感闭合了吗

用户的原话是「挂住了、ESC/Ctrl+C 取消不了」。

**已经闭合的：**

- 卡住的 provider 被 ESC 中断之后，下一条消息和退出都会立即生效（缺陷 A 的主路径，tmux 实测）。
- 响应头已到、body 不来的情况，不会再永远停在 `Waiting` 上：状态行先变成 `Waiting for the first token`，最多 180 s 后报出 `Response stalled`，并说明怎么调阈值（缺陷 B）。
- 黑洞地址 15 s 就能暴露出来。

**还剩的（按影响排序）：**

1. **响应头之前的静默仍然最长约 67 分钟**：`HEADER_TIMEOUT` 120 s × 3 次，再乘以 turn 层 11 次（勘察 §4）。这期间状态行大部分时间是 `Waiting for the model`，偶尔闪一下 `retrying (attempt n/10)`。ESC 有效，但不按就要等很久。调研 §7 第 2 条（先短后长的首字节超时）被有意推迟了（X-64「Not done」）。**这是体感上最大的残留**，建议作为下一步单独做。
2. **一轮成功、但标题请求卡住时，仍有 30 s 的按键盲区**（§2.4）。
3. **`StreamIdle` 会丢掉已经流出来的部分正文**：走的是错误回滚，不像 ESC 那样保留部分输出。调研 §5.1 的做法是「已有产出则保留并标注不完整」。这是体验问题，不是本次引入的，可以以后再做。
4. 本地 server 如果要等第一个 token 才发响应头，`=0` 管不到（§1.2，推测）。

## 7. 建议清单（只列必要的）

| 优先级 | 改动 | 位置 | 规模 |
|---|---|---|---|
| 合并前，建议 | 已经看到 `[DONE]` 之后空闲超时触发，按干净 EOF 处理 | `src/llm/sse.rs:180-183` | 1–3 行 + 一个 wire 测试（复用 `head_then`） |
| 合并前，建议 | `abort_title()` 只在丢弃整轮时调用（移进 `else` 分支） | `src/repl/run.rs:904` → `:935` 附近 | 移动 1 行 + 注释 |
| 合并前，拍板 | 默认值 180 s → 300 s | `src/llm/client.rs:35` 及单测一行 | 2 行 |
| 可选 | 删掉 HTTP/2 PING | `client.rs:43-47,586-591,696`；DIVERGENCES X-64 | 删除 |
| 可选 | 更正提交说明与注释的措辞：「await listens for cancel」→「aborted on interrupt/error」；「工具会执行两次」→「重试会乘以静默时长」 | `client.rs:266-270`、`retry.rs:81-84` | 文案 |
| 下一步 | 先短后长的首字节超时（残留 1） | — | 单独立项 |
