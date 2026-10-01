# 请求取消修复独立评审

评审日期：2026-10-01。对象仅为 `git diff main...HEAD`，HEAD 为 `02bb3bb5ffdcde76f8678d4a14cbb4c9ac1d2917`，包括勘察 `e97d131`、调研 `548c5fa`、修复 `02bb3bb`。先读了两份前置报告；下述源码行号均指此 HEAD，而非勘察时的旧行号。

**结论：暂不建议合并。** 字节级空闲计时、`StreamIdle` 分型与中断后取消标题的方向正确，已有主要测试也确实有效；但新的自动超时会丢弃已经执行的工具记录，协议已经结束的回答仍会被判超时，标题单独挂住的取消盲区仍在。前三项均有本次独立实测，详见 §6。

本次没有修改代码或测试，没有 commit；唯一评审产物为本文件。实测只连接本机 mock，使用现有 debug 二进制及临时配置，额外驱动脚本通过标准输入运行，临时配置清理；未调用真实模型。未把整个仓库的其它问题纳入评审。

## 0. 最重要的三个发现

| 优先级 | 发现与归因 | 最小建议 |
|---|---|---|
| **P1，阻塞** | **新超时走普通失败回滚，已经完成的工具调用及结果从历史消失。** `src/llm/sse.rs:180` 新增错误出口；`src/repl/turn/mod.rs:463` 只对用户中断保留 partial，普通错误在 `src/repl/run.rs:750` 截掉整轮。回滚代码是旧的，但让停滞自动进入它是本次新增行为。实测工具执行成功后下一轮停滞，第三次请求已没有调用及结果（R-TOOL）。 | 为 `StreamIdle` 保留已完成工具轮次和可用文本，仍显示失败及“回答可能不完整”，仍禁止自动重试；不要伪装成 `Cancelled`。 |
| **P1，阻塞** | **已收到终止标记仍等待 EOF，最终把完整回答判为失败。** `src/llm/sse.rs:97` 收到 `[DONE]` 后明确继续 drain，新 `:180` 把后续静默变成错误；R-DONE 已复现。旧代码会挂住，新代码会超时并回滚已完成回答。 | 按协议终止流；OpenAI 在 `[DONE]` 结束，其它方言识别各自终止事件。不要仅见 `finish_reason` 就丢掉可能随后到达的 usage。 |
| **P2，闭合缺口** | **主回答成功、只有标题请求挂住时，下一条输入/退出仍等待约 30 秒，ESC/Ctrl+C 无效。** `src/repl/run.rs:560`、`:580` 仍无条件 join；新增 abort 仅覆盖失败和中断。R-TITLE-NEXT 实测两次主请求间隔 **30.002 s**，R-TITLE-EXIT 连按 Ctrl+C 仍不能及时退出。此为原盲区未覆盖的分支，并非新增死锁。 | 用户要退出或进行需 join 的下一步时，允许放弃尚未完成的标题，保留占位名；避免把标题的完成作为输入/退出前提。加成功主请求＋挂住标题的两个用例即可。 |

这不是要求顺便重构所有错误恢复。前两项是新超时接入现有生命周期时必须处理的边界，第三项决定这次能否宣称消除了“取消不了”的体感。

## 1. 正确性：空闲超时与正常慢请求

**结论：按收到的 body chunk 计时是合适的，能容忍持续有数据的长响应；不能据此保证 180 秒不会误杀正常请求。另有上表中的终止帧误判。**

**依据：** `Client::stream` 在响应头之后创建带计时器的 SSE（`src/llm/client.rs:277`）；`Sse::read_line` 每次等待 `body.next()` 都重新开始计时，取消分支优先（`src/llm/sse.rs:174`）；不是整轮/整个响应的 deadline。应用层解析在它之后。

| 场景 | 判断与依据 | 失败场景及建议 |
|---|---|---|
| SSE `: ping` 注释心跳 | **覆盖。** 字节先进入 `read_line`，注释之后才在 `src/llm/sse.rs:109` 被忽略。心跳测试一次 `sse.next()` 跨过八个注释，约 900 ms 后才返回事件，超过 300 ms 界限（`tests/provider/wire.rs:667`）。 | 心跳必须真的到达客户端且间隔小于阈值。保留这种测试，勿改为只按已解析内容续期。 |
| Anthropic `event: ping` / `data: {"type":"ping"}` | **代码覆盖。** `src/llm/anthropic.rs:613` 先读 SSE，`:627` 才跳过 ping；它不需要产生正文才续期。 | 本次没有真实 provider 抓包；调研 `request-cancellation-research.md:533` 也承认未核实各家心跳频率。不能把“会接受 ping”写成“所有 provider 都保证及时发送 ping”。 |
| HTTP/2 PING / TCP keep-alive | **不是 SSE 心跳，不能续期 body timer。** 新设置在 HTTP client builder，SSE 只观察 `resp.bytes_stream()`（`src/llm/client.rs:279`、`:587`）。 | 对端持续回复 HTTP/2 ACK，但模型不发 body，180 秒仍会报错，这是两个计时器的不同职责。不要将协议探活当作模型进度。 |
| 模型长时间思考 | 推理 delta 也是字节，会续期；“屏幕没有正文”不等于线路没有字节（`src/provider/openai.rs:355`、`src/repl/turn/mod.rs:846`）。**是否真的完全静默取决于 provider，未实测。** | **推测：** 若 provider 在内部思考时不输出推理/心跳，或思考摘要集中发送，合法的超过 180 秒空隙也会失败。默认 180 秒处于调研区间，不构成不会误杀的证据；保留调大/关闭能力，勿以自动重试补救。没有实测证据，不要求另建按模型配置系统。 |
| 大请求上传后等待 | 新 idle timer **不计上传**。但已有 `HEADER_TIMEOUT=120s` 包的是整个 `http.execute(req)`（`src/llm/client.rs:29`、`:336`），含连接、上传和等头，不是上传结束后重新给 120 秒。 | 上传花 100 秒，只剩约 20 秒等头；CDN 若连头一起缓冲，新 timer 根本未启动。`IOTA_STREAM_IDLE_TIMEOUT=0` 无法解决。属旧首部计时策略，需如实记录，不要称为所有慢请求已解决；本修复不必顺带改重试策略。 |
| 本地慢模型 | 头后可用 `0` 关闭 SSE 空闲限制，取消仍可用，`a_zero_idle_bound_never_times_a_stream_out` 已验证。 | 先思考再发头的本地服务仍受 120 秒限制。文案应说“关闭流空闲超时”，不要说“永不超时”。 |
| 代理/CDN 缓冲 | 只能按**客户端可见**的字节判断；上游发了心跳、代理没转发，客户端仍会超时。 | **推测：** 头已转发而 body 缓冲超过 180 秒会误判；连头也缓冲则先触发 header timeout。优先让部署关闭 SSE 缓冲或调大现有阈值，不建议为本修复添加自动协议降级或更多探测开关。 |
| 正常持续输出很久 | 间隔小于阈值即可无限持续，无整体超时；机制正确。 | 仅心跳永远持续、任务永远不完成时也能无限等。这是字节空闲的定义，不建议因此加入会误杀长响应的总 deadline。 |
| 已完成但 body 未 EOF | **确定错误，R-DONE 实测。** `[DONE]` 后 `continue` drain（`src/llm/sse.rs:97`）；OpenAI provider 等 `next()==None` 才返回（`src/provider/openai.rs:331`）。Responses 的 Completed 分支也仅记录 usage（`src/provider/openresponses.rs:380`），Anthropic 将 message_stop 归入 Other（`src/provider/anthropic.rs:291`）。 | 修协议终止条件后再依赖 idle 兜底。OpenAI 已实测；其它两条是静态确认的同类路径，未做线路复现。调研 `request-cancellation-research.md` §7 第 7 条已经指出这一边界。 |

### 环境变量解析与 0

**结论：常见输入的实现一致且无负数绕过；容易误解的是非法值静默回到 180 秒，以及 0 只关闭一种超时。**

依据：`src/llm/client.rs:564` 使用 `trim().parse::<u64>()`；在进程边缘经 `Env` 注入（`src/cmd/mod.rs:71`），不在底层读全局环境。`Env::var` 将空字符串视为未设置（`src/app/env.rs:57`）。

| 输入 | 实际语义 |
|---|---|
| 未设置、空字符串、纯空白 | 默认 180 秒 |
| `0`、` 0 `、`00`、`+0` | `None`，关闭 SSE 空闲限制 |
| `600`、` 600 `、`+600` | 600 秒 |
| `abc`、`-5`、`1.5`、`3m`、大于 `u64::MAX` 的数字 | 解析失败，静默使用 180 秒；不是禁用，也不报配置错误 |
| 合法但极大的 `u64` | 接受为极大 Duration；不是负数溢出。锁定的 Tokio 1.53.1 `src/time/sleep.rs:123` 用 `checked_add`，溢出改用 far_future，不能据此声称这里必然 panic。 |

`stream_idle_timeout_reads_the_override` 覆盖缺省、0、带空格的 600 与五类非法输入，实测通过；表中 `+`、溢出和纯空白为解析实现的静态判断，未另写测试。

**失败场景：** 用户填 `5m` 以为等五分钟，实际三分钟；设 `0` 后仍可能遇到 header/connect/HTTP2 超时。**建议：** 文档写清“整数秒，非法值回退 180，0 仅禁用流空闲计时”，将 `src/llm/error.rs:55` 的 `0 to never time out` 收窄为 `0 to disable the stream idle timeout`。不必为这一项增加配置层级或严厉的启动拒绝。

## 2. 取消盲区：等待、任务结果与退出

**结论：新增 abort 本身没有引入 await 或网络等待，原勘察的“ESC 后马上发下一条”和“ESC 后 Ctrl+C 退出”已修好；成功 turn 后的标题 join 盲区未修好。**

**依据与结果去向：**

- `src/repl/state.rs:128` 用 `title_task.take()` 取走唯一句柄，再 `h.abort()`；以后 `join_title` 在 `:120` 看到 None。没有另一处继续 await 被取消句柄的结果或 JoinError；标题任务返回 `()`（`:88`），生成失败/超时本就降为占位名（`src/repl/run.rs:878`）。
- 中断在 `src/repl/run.rs:904` 调用 abort；普通失败在 `:751` 也调用。`Interrupted.` 在 abort 之后才输出（`:916`），因此场景 27 看到此行再发送下一条不是在抢 abort 的时序。
- `abort()` 是请求 Tokio 在调度时取消，不保证调用返回瞬间任务已经销毁。这里挂住的是可丢弃的网络 await，标题 provider 的 async mutex 在任务退出后释放（`src/repl/run.rs:876`）。下一次标题任务可能等它一次调度收尾，**没有发现新的 30 秒网络等待或锁环**。不应把“丢句柄”夸大成对任意同步阻塞代码的硬终止。
- 丢弃整轮时 `SessionTitle::unseed` 增加 generation，迟到标题不能再落到新 seed（`src/repl/title.rs:113`、`:127`）；保留 partial 时占位名保留是合理取舍，不值得为标题添加复杂取消状态机。
- 场景 27 的下一条回答和进程退出都有时间上界断言，实测 `PASS=7 FAIL=0`，见 §4、§6。

**失败场景：** 主流迅速正常结束而标题 unary 不返回。成功分支没有 abort，之后在 `src/repl/run.rs:580` 或 `:560` join。此时没有 turn cancel 栈；Ctrl+C 只能唤醒 read_input waiter，而主循环正在 join，ESC 无 scope 可取消（`src/ui/input/keys.rs:35`、`:46`）。R-TITLE-NEXT 与 R-TITLE-EXIT 均复现，不能宣称 “ESC always lands”。

**建议：** 不再为非必要标题阻塞用户的下一步和退出；可复用当前 abort 并保留占位名。若仍需等待任务销毁以保证 writer 顺序，应只等取消后的收尾，不能继续等模型结果，也不能仅给原来的无界 join 再套一个长超时。补上两个“主流成功、标题挂住”场景即可。

## 3. 重试边界、错误分型与已完成工作的保留

**结论：`StreamIdle` 自身守住了客户端与 turn 两层不重试边界，和用户取消分型干净；但没有守住超时后的历史保留。另需区别本次同时开启的 HTTP/2 PING 超时。**

**依据：**

1. `DEFAULT_RETRIES=2`（`src/llm/client.rs:27`）只控制 `send_payload`：拿到 `<400` 响应头即在 `:351` 返回；body 的 SSE timer 位于外层 `stream` 所返回的对象中（`:279`），不会倒流进入这个循环。即使直接调用 `should_retry(StreamIdle, …)`，`:519` 也返回 false。
2. provider 不擦掉类型：`ProviderError::wire` 只将 `Cancelled` 转成独立取消，其它错误仍携带 `LlmError`（`src/provider/error.rs:74`）。`StreamIdle` 在 `src/repl/turn/retry.rs:92` 明确不可重试；轮内 `retry_round` 在 `:45` 退出，外层 `TurnEngine` 同样调用 `is_retryable`（`src/repl/turn/mod.rs:345`）。
3. `src/llm/sse.rs:177` 取消优先；`LlmError::StreamIdle` 和 `Cancelled` 在 `src/llm/error.rs:51`、`:66` 分列，UI 显示 `Response stalled`（`src/repl/errors.rs:68`），不伪装成 `Interrupted.`。场景 27 的第三段确实断言了后者不存在。
4. `a_silent_stream_fails_at_the_idle_bound` 断言错误类型、时长、`should_retry==false` 和服务端只接受一次连接；`is_retryable_follows_the_status_table` 单独钉住 turn 分类。两者实测通过。

**失败场景一（确定，阻塞）：** 工具轮完成，下一轮解释结果时停滞。`tool_loop` 已把调用写入 history 并执行工具（`src/repl/turn/tools.rs:112`、`:135`、`:140`）；但 `StreamIdle` 不属于用户中断，`src/repl/turn/mod.rs:471` 丢 partial，最终 `src/repl/run.rs:750` 截掉用户消息、调用和结果，并恢复预算。R-TOOL 确认下一次请求不再含工具记录，屏幕上的已执行输出却仍然存在。

**后果：** 本次没有观察到自动执行两次，不能这样报告；但**推测**用户说“继续/重试”后，模型看不到已完成动作，会有再次执行的风险。自动重试禁用不等于工作记录可以删除。修复前用户 ESC 至少可走 `finalize_interrupt` 保留已完成工具轮（`src/repl/turn/interrupt.rs:29`）；新的自动超时不应比手动取消破坏更多状态。

**建议：** 对停滞结果沿用已有的“保留完成轮次和可用 partial”的原则，提示不完整并保持非重试错误，不改变错误身份。用一次无副作用工具＋第二轮停滞＋下一条请求检查历史的测试固定它；不要求重构所有 provider 错误。

**失败场景二（静态路径确认，未作 HTTP/2 线路实测）：** 本次新开的 PING 超时会以 reqwest body error 进入 `src/llm/sse.rs:187` 的 `Transport`，不是 `StreamIdle`；`src/repl/turn/retry.rs:94` 对它仍返回 true。因此“所有流中途的超时都绝不重试”比代码实际保证的范围大。头后的错误不会进 `DEFAULT_RETRIES`，但会进 turn 的 round 重试。

这里不能直接推断普通客户端工具重复：工具在完整 round 后才执行，而 round 重试保留之前完成轮次；也未实测 PING 丢 ACK。但它确实不符合题设对流中途超时的严格非重试要求。**建议：** 本次若无意处理 HTTP/2 这条错误路径，可暂缓新增 PING，仅保留 SSE timer 与 connect timeout；若保留，需明确使头后的该超时不重放，并以真实路径验证，勿只靠错误名称推断。

## 4. 测试是否假绿

**结论：黑洞、字节心跳、0 关闭和取消盲区的主要测试不是“只要不崩就绿”；但测试范围较窄，连接层测试也不等于线路验证。**

| 检查 | 依据、失败场景与建议 |
|---|---|
| 黑洞对 timeout 开关是否敏感 | **已亲自做关闭对照。** 先运行原场景 27，7 项通过；然后保持场景脚本及二进制不变，通过 `IOTA_BIN='env IOTA_STREAM_IDLE_TIMEOUT=0 /…/target/debug/iota'` 在子进程启动时覆盖环境，原脚本即使 export 2 也由该进程参数覆盖。同一本地 mock 上变为 **5 PASS / 2 FAIL，exit 1**：`the idle bound never fired`、`the error names the knob: got '0', want '1'`。没有修改源文件或测试文件。 |
| wire 黑洞测试本身 | `tests/provider/wire.rs:635` 显式注入 300 ms，外层 5 秒 watchdog，断言 `[300ms,3s)` 与 `StreamIdle(300ms)`。直接给 `cargo test` 设置环境变量 0 **不会**修改这个注入值，不能冒充 mutation test；本次用上述真实进程对照验证同类黑洞行为。 |
| 心跳是否真的算存活 | `tests/provider/wire.rs:669` 每 100 ms 发纯注释，最后才给一个 data event；`:682` 断言耗时至少 600 ms。它能发现把 300 ms 当整次 `sse.next()` deadline 的实现错误，确实证明注释会续期；与黑洞失败用例合起来才能排除“计时器根本没开”。它不证明 Anthropic 实网频率、H2 ACK 或 CDN 缓冲，勿扩大结论。 |
| 0 关闭是否可取消 | `tests/provider/wire.rs:691` 从真实解析函数取得 None，1200 ms 内不得完成，随后 token 取消必须在 2 秒内得到 `Cancelled`。覆盖了关闭后的取消，不是假绿。 |
| 场景 27 是否断言不再等待 | `tests/ui_tmux/scenarios/27-hung-provider.sh:18` 的 FAST=80；`:34` 必须读到**模型的 `echo: second`**，不是用户输入回显；`:52` 必须 `pane_dead`。每轮 poll 间隔 100 ms（`tests/ui_tmux/lib.sh:200`），名义约 8 秒、远小于 30 秒，证明原长等待不再发生。它不证明毫秒级及时性，也不覆盖主流成功而标题挂住。建议加该缺失分支，没必要做脆弱的 50 ms UI 门槛。 |
| 场景 27 的不重试断言 | `:73` 数的是 UI 的 `retrying (attempt`，不是 HTTP 请求计数；单独看证据较弱。结合 wire 的 accept counter 与两处 retry 分支代码，目前足以确认 `StreamIdle`；对“工具执行后停滞”还缺历史保留断言，R-TOOL 已揭示漏项。 |
| connect_timeout 测试 | `the_default_client_carries_the_connection_bounds`（`src/llm/client.rs:687`）检查 builder Debug 有 connect_timeout 且没有整体/read timeout，是配置接线测试。测试未制造真实 DNS/TLS/SYN 超时；200 头后静默也不会触发 connect timeout。不能声称 15 秒断连已实测。 |
| HTTP/2 PING 测试 | 同一个测试只断言 `TIMEOUT < INTERVAL`（`:696`），根本没检查 builder 收到 PING 参数；删掉 `:589`、`:590` 的调用，这些断言照样成立（静态判断，未改源码做 mutation）。本次 mock 全是 HTTP/1.1，PING 发送、ACK、丢 ACK、错误归类均未线路测试，应明确记为未验证，而非列成通过。不要为一次修复引入大套网络故障框架；能用小型 h2 fixture 就测，否则暂缓这项附加改动。 |

有一次未正确开启 L4 gate 的运行打印了 `SKIP: IOTA_TMUX not set`，没有把它计为通过；改用 `IOTA_TMUX_REQUIRED=1` 后沙箱内因 mock 无法 bind 失败，经申请在沙箱外完整跑通。现有 CI 也强制该变量（`docs/ARCHITECTURE.md:316`），不存在依赖静默 SKIP 来宣称场景已验证的必要。

## 5. 改动是否过度设计，连接取值是否合理

**结论：一个环境变量与一个阶段文案没有明显过度设计；15 秒 connect 合理，HTTP/2 PING 是有解释但证据不足的附加行为，不能写成无条件无害。**

**依据：** 当前树没有找到适用的 `AGENTS.md` 或单独 `POLICY.md`；按现有 binding `docs/ARCHITECTURE.md` 与 `docs/DIVERGENCES.md` 评判，不虚构额外批准规则。前者 `:7` 要求可见字符串变化记入 DIVERGENCES，后者 `:321` 的 X-64 已记录本次文案、timer、连接和标题差异；`DIVERGENCES.md:39` 的 I-02 要求统一 header timeout、不加整体 timeout，本次仍满足。环境从边缘注入符合 `ARCHITECTURE.md:122`；layering 四项实测通过，没有新依赖。

| 改动 | 判断、失败场景与必要建议 |
|---|---|
| `IOTA_STREAM_IDLE_TIMEOUT` | 本地慢模型和代理的静默上限不一，单一覆盖出口有具体用途，且错误提示可发现它；不必再加 CLI flag、provider YAML、自动探测三套入口。非法值回退与 0 的范围按 §1 说明即可。 |
| 180 秒默认值 | 位于调研的 2–5 分钟区间，不算激进的几十秒；但调研自身明确提到 extended thinking/代理曾促使阈值放宽（研究文档 `:76`、`:108`），故“在区间内”不能证明适配所有模型。当前没有真实慢模型证据要求固定改成另一数字，也不应声称完全不误杀。 |
| `connect_timeout=15s` | 符合调研提出的 10–30 秒；只限制建连，不把上传或长回复限制在 15 秒，且缩短坏路由等待。**推测：** 极慢 DNS/TLS 或代理握手也可能被切掉。保持一个固定合理值即可，不建议再加一组连接参数环境变量。 |
| HTTP/2 interval=30s、timeout=20s | 对活跃连接中失联的 peer，约 30 秒无入站活动后探测，再给 20 秒 ACK，比 180 秒 SSE 更早发现死连接；reqwest 默认 `while_idle=false`，不会为了空闲池不断探活。依据锁定依赖 reqwest 0.13.4 `src/async_impl/client.rs:1630`、`:1656`，hyper 1.11.1 `src/proto/h2/ping.rs:451`。正常长思考只要 peer 回复 ACK 就不受这条限制。 |
| PING 的代价与范围 | `RunContext` 的 client 也给 MCP（`src/cmd/mod.rs:43`），不是只改变 LLM；**推测：** 中间设备对 PING 不兼容或 peer 的事件循环长期阻塞，会提前断开本来可能恢复的流。`src/llm/client.rs:585` 的“none … can cut a slow but live stream short”过强。更重要的是 §3 的 `Transport` 重试路径未处理。若不补验证/边界，暂缓 PING 比新建开关体系更小。 |
| `Waiting for the first token` | `src/repl/turn/phases.rs:104` 依据成功头布尔值切换，不混淆“已发完上传”和“已收到头”；配套 phase 测试及 L4 验证齐全。对定位头前/头后停滞有用，改动规模合理。只是改阶段名，不等于已有停滞倒计时或完整恢复提示；不要求本次再造 watchdog UI。 |

## 6. 本次验证记录与可复核复现

### 6.1 原有测试与检查

| 命令 | 实测结果 |
|---|---|
| `cargo test --test provider wire:: -- --nocapture` | **13 passed**，含 silent/heartbeat/zero/cancel/header timeout |
| `cargo test --lib llm:: -- --nocapture` | **48 passed**，含解析、错误文案、连接 builder、progress |
| `cargo test --lib repl::turn:: -- --nocapture` | **45 passed**，含 retry 分类、phase、interrupt 与工具轮测试 |
| `IOTA_TMUX_REQUIRED=1 cargo test --test ui_tmux tmux_hung_provider -- --nocapture` | 沙箱外 **1 passed**，tmux 3.7c，场景 **PASS=7 FAIL=0 WARTS=0**；总测试 6.74 s |
| `cargo test --test layering` | **4 passed** |
| `cargo fmt --check` | exit 0 |

没有跑整个仓库 CI、真实 provider、HTTP/2 或网络故障注入；以上结果不代表这些部分经过验证。

### 6.2 黑洞关闭对照

运行未修改的 `tests/ui_tmux/scenarios/27-hung-provider.sh`，使用本机 Python TCP mock：`hang:` 的流式请求回 `200 text/event-stream`、chunked 头后静默；标题 unary 不回头；普通流返回 `echo: <prompt>`、`[DONE]` 与 chunked 终止块。复用了原 `tests/ui_tmux/lib.sh`，只让 `IOTA_BIN` 在对照组加 `env IOTA_STREAM_IDLE_TIMEOUT=0`，没有变更脚本中的断言。

```text
CASE scenario27-control exit=0 elapsed=4.691s
PASS=7 FAIL=0 WARTS=0

CASE scenario27-idle0 exit=1 elapsed=12.037s
FAIL: the idle bound never fired
FAIL: the error names the knob: got '0', want '1'
PASS=5 FAIL=2 WARTS=0
```

这是对真实二进制环境注入链路的负对照；并非编辑 timer 实现后重跑 wire 测试。仅改变进程环境就使原有断言变红，满足本次“关掉超时自己试一次”的要求。

### 6.3 R-DONE：协议完成后不结束 HTTP body

本机 mock 按顺序发下列 SSE 数据，每段使用合法 HTTP/1.1 chunk framing，但最后**不发 `0\r\n\r\n`，保持响应体打开**；标题 unary 正常返回。被测进程设置 `IOTA_STREAM_IDLE_TIMEOUT=1` 加速重现。

```text
data: {"choices":[{"delta":{"content":"echo: donehold"},"finish_reason":null}]}

data: {"choices":[{"delta":{},"finish_reason":"stop"}]}

data: [DONE]

```

屏幕先显示完整正文，随后出现：

```text
echo: donehold
✗ Response stalled
  stream error: no data from the provider for 1s (stream idle timeout)
```

再发送 `inspect-history`，mock 检查新请求得到：

```text
DONEHOLD next-request roles=['user'] prior_donehold_present=False
```

整个场景耗时 1.755 s。不是测试断言文件中的新测试；这是本次独立驱动的线路观测。1 秒用于缩时，默认 180 秒走相同分支。

### 6.4 R-TITLE-NEXT / R-TITLE-EXIT：主请求完成、只有标题挂住

Mock 对 `titleonly-*` 的流式主请求立即返回 `echo`、`[DONE]`、HTTP body 终止；仅其 unary 标题请求保持无响应。未覆盖 idle 环境变量。待主回答完成、界面 settle 后，分别发送下一条或 Ctrl+C。

```text
CASE title-next exit=0 elapsed=30.610s
NEXT_WITHIN_2S=no
NEXT_AFTER_CANCEL_WITHIN_2S=no
TITLE_ONLY next-request gap=30.002s

CASE title-exit exit=0 elapsed=30.708s
EXIT_WITHIN_2S=no
EXIT_AFTER_SECOND_CTRL_C_WITHIN_2S=no
```

NEXT 测了发送下一条后按 ESC、Ctrl+C 仍不能加速；EXIT 测了空闲 Ctrl+C、等待 2 秒再按一次也未退出。30.002 s 是 mock 记录的两次主请求时间差；30.708 s 是含启动的完整退出场景时长，不冒充按键到退出的精确延迟。最终都在原有标题 deadline 后恢复。

### 6.5 R-TOOL：已完成工具轮之后的停滞

临时 agent 启用 shell，`sandbox: off, auto_run: true`，唯一工具命令为无文件副作用的 `printf REVIEW_TOOL_EFFECT`。Mock 第一轮给出完整 shell 调用并正常结束；第二轮确认收到 tool 结果，输出 `REVIEW_AFTER_TOOL_PARTIAL` 后不再发字节；标题正常完成。进程 idle=1 秒。超时后用户发送 `inspect-history`。

```text
[shell printf REVIEW_TOOL_EFFECT]
  REVIEW_TOOL_EFFECT
REVIEW_AFTER_TOOL_PARTIAL
✗ Response stalled

STREAM_REQUEST 1 roles=['system', 'user'] tool_calls=0 tool_effect_present=False
STREAM_REQUEST 2 roles=['system', 'user', 'assistant', 'tool'] tool_calls=1 tool_effect_present=True
STREAM_REQUEST 3 roles=['system', 'user'] tool_calls=0 tool_effect_present=False
```

第二次请求证明工具已执行且结果已进入对话，第三次证明超时后记录被删；不是根据 UI 缺了一行猜测。此 mock 的第三次请求是用户主动发送，非自动重试。本次没有制造真实写文件/付费等副作用，重复真实副作用的风险在 §3 明确标为推测。

## 7. 是否闭合用户体感、合并条件

**部分闭合，尚未完整闭合。**

- 已闭合：成功响应头之后、普通 SSE body 无字节时不再无限等；ESC 取消后不会继续等该轮标题，紧接着下一条与 Ctrl+C 退出两条原复现都通过。头后新文案能帮助区分阶段。
- 仍未闭合：主流成功但标题挂住的 30 秒盲区（本次实测）；已结束协议仍等待 HTTP EOF；自动停滞失败把已完成工作从会话删除。
- 既有且仍存在的范围限制：`send_payload` 等头仍可经过 120 秒 × 3 次客户端尝试及 turn 重试，勘察估算的约 67 分钟上限并未被本次消除（`src/llm/client.rs:27`、`:336`；`src/repl/turn/retry.rs:14`）。`read_error_body` 在错误头之后仍只有 cancel、没有 body timeout（`src/llm/client.rs:481`），所以服务返回 503 头后黑洞仍可无限等待。`do_json` 的成功 body 也无界（`:247`），图像 SSE 刻意不设 idle（`src/llm/images.rs:378`）。这些是代码确认的遗留/显式范围，未在本次逐个实测，不应写成新引入的缺陷；图像耗时长也不宜机械套用相同 timer。

合并前的必要工作是保留超时前已完成工作、按终止事件结束流、补齐成功主请求后的标题盲区，并对附加的 HTTP/2 PING 选择完成非重试边界验证或暂缓启用。补充测试应围绕这些具体失败场景，不要求扩大为全仓库重构或把竞品调研中的全部建议一次搬入。

这次可以保留的核心方案已经足够小：字节级 SSE 空闲限制、单独错误类型、可关闭的一个覆盖参数、取消不依赖标题结果、明确的阶段文案；当前阻塞点在这些方案和原有完成/失败生命周期的衔接。
