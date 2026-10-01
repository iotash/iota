# 请求取消修复第二轮评审（Codex）

评审日期：2026-10-01。范围仅为 `git diff 02bb3bb..HEAD`；HEAD 为 `aa989fbf3f8444771bf0de32295cac41516466d2`，功能修复是 `aa989fb`，区间中的 `d32b017` 只增加两份首轮报告。对照 `docs/history/request-cancellation-review-codex.md` 与 `request-cancellation-review-opus.md`，没有重新评审整个分支。下文源码行号均指本次 HEAD。

**结论：暂不合并。首轮两条 P1、一条 P2，以及 Opus 的中断丢标题问题均已修复；但把标题 join 换成仅 `abort()` 后，成功回答后的 `/session` 切换新增了旧标题写入新会话的竞态（P2，见下文）。这是本轮唯一必须修的事项。** 没有发现这次终止事件改动丢失正常协议 usage，也没有发现停滞保留历史的新阻塞问题。

本评审在仓库内只新增本报告，不修改代码或测试，不 commit。反向验证在 `/private/tmp` 的副本中恢复生产实现的旧行为，测试文件保持 HEAD 原样；另用标准输入编译一个临时调度探针。测试只使用 fake/provider 本机 mock，没有调用真实模型。需绑定端口和运行 tmux 的测试已申请沙箱外权限。

## 1. 首轮问题逐条结案

| 首轮问题 | 本轮状态 | 可核对的依据 |
|---|---|---|
| **P1 R-TOOL：新超时普通回滚，删除已完成工具调用和结果** | **已修** | `src/provider/error.rs:86` 保留 `StreamIdle` 分类；`src/repl/turn/mod.rs:476` 将停滞前正文、reasoning 带回；`src/repl/run.rs:756` 进入 `keep_stalled_turn`，`:959` 复用保留表，`:972` 持久化。`tests/repl/stall.rs:119` 的 `a_stall_after_a_tool_round_keeps_the_call_and_its_result` 实测通过：第三次请求仍有原用户消息、`c1` 调用、关联结果、partial 和下一条输入；总请求数为 3，无重试，显示错误和不完整提示。恢复旧实现后该测试红。 |
| **P1 R-DONE：收到 `[DONE]` 仍等 EOF，完整回答被判失败** | **已修** | `src/llm/sse.rs:100` 当即结束，`:84` 保证此后仍为结束状态；Anthropic 在 `src/llm/anthropic.rs:638` 处理 `message_stop`；Responses 在 `src/llm/responses.rs:603` 交出最后事件后结束。三个 `*_although_the_body_stays_open` 用例均通过，并断言正文、usage 和小于 1 秒完成（idle 是 2 秒）。恢复旧行为后三个都红。 |
| **P2 R-TITLE-NEXT / R-TITLE-EXIT：主回答成功，单独挂住的标题阻塞下一输入/退出约 30 秒** | **已修；此修法另引入下述 P2 竞态** | 普通消息不再 join，`src/repl/run.rs:578` 只对非只读的 slash 输入取消标题；退出在 `:561` 取消。`a_hung_title_pass_never_holds_the_next_message`、`a_hung_title_pass_never_holds_the_exit` 均通过；原实现两项均撞 10 秒 watchdog。tmux 第 4、5 子场景也由红转绿。 |
| **Opus：`interrupt_turn` 无条件 abort，保留 partial 的会话永远没有模型标题** | **已修** | `src/repl/run.rs:929` 保留 partial 时保留标题 pass；只有 `unseed` 确实撤回 seed 才在 `:937` 取消。`src/repl/title.rs:134` 还保护先前仍存活的首条消息：后续轮次失败不会取消首条消息的标题。`an_interrupt_that_keeps_a_partial_keeps_the_title_pass` 实测收到 `Model Name`，恢复旧实现后断言失败。 |

R-TOOL 保留的是已经执行完的工具轮次和已流出的正文，不会把当前轮尚未完成的工具参数误记为已执行动作。无正文、无工具轮次时仍回滚，`a_stall_with_nothing_to_keep_rolls_the_turn_back` 在新旧实现均通过，属于边界对照。仅有 reasoning 的 partial 仍按原中断表的“无正文”处理（`src/repl/turn/interrupt.rs:32`），不是本轮新加的保留承诺。

## 2. 本轮新问题：P2，标题取消不能隔离随后交换的 writer

**触发条件：** 主回答已经成功；旧会话 A 的标题恰好完成网络请求、正在另一 runtime 线程执行同步 `land()`；用户此时用 `/session` 切换到 B。多线程 runtime 是实际入口配置（`src/main.rs:32`），不是假设产品只有单线程之外的特殊模式。

代码有一条允许写错会话的执行顺序：

1. 标题任务在 `src/repl/run.rs:896` 进入 `land`，取得 title state 锁并通过 generation/titled 检查（`src/repl/title.rs:114`）。它随后准备取得 writer 锁；这中间没有 async yield，但线程可以被抢占。
2. 主循环在 `src/repl/run.rs:583` 调 `abort_title`。`src/repl/state.rs:125` 只是 `take()` 后 `h.abort()`，没有等待正在执行的同步段结束，也没有使 generation 失效。Tokio 的 abort 不能打断当前 poll 内的同步代码。
3. `/session` 把共享 writer 换成 B（`src/repl/commands/session.rs:179`）。标题任务恢复执行，`src/repl/title.rs:179` 从共享 slot 解析的是**此刻的 writer B**，把 A 的模型标题写进 B 的 meta，并更新窗口标题。
4. `cmd_session` 到 `src/repl/commands/session.rs:192` 才 `adopt()`；此时旧标题可能已写入，adopt 不会恢复 B 的原名。

旧的成功路径会在命令分发前 join 完标题任务，因而不会允许上述交错；这里评的是本次删除那道同步之后的新增路径，不是重新追究旧的网络等待问题。后果是另一会话的**持久化标题被覆盖**，不是仅仅使用占位名，也不涉及消息正文丢失。

**受控调度实测：** 独立 `rustc` stdin 探针通过 `#[path]` 直接加载 HEAD 原样的 `src/repl/title.rs`，使用真实 `SessionStore`/`SessionWriter`、Tokio 双线程和 `JoinHandle::abort`。只在探针的 `sync::lock` 包装层设置检查点：标题线程已通过 `land` 检查，在取得 `WriterSlot` 锁之前暂停，模拟这一行前的线程抢占；主线程按新命令顺序执行 `abort → writer 换 B → adopt`。核心调度如下：

```rust
// title worker: SessionTitle::land(generation, "A model title")
// checkpoint: title state 锁已持有，WriterSlot 锁尚未取得
reached_rx.recv_timeout(Duration::from_secs(5)).unwrap();
handle.abort();
*slot.lock().unwrap() = Some(writer_b); // 原标题为 "B original title"
resume_tx.send(()).unwrap();
titler.adopt();
let joined = runtime.block_on(handle);
```

输出（`/private/tmp/iota-review2-title-race.log`）：

```text
checkpoint=before WriterSlot lock; abort_join=Ok(()); expected=B original title; actual=A model title
```

这是实际标题模块在指定合法线程交错下的复现，**不是完整 REPL `/session` 端到端复现**。自然发生频率没有测量；**推测**窗口很窄，但没有同步保证，不能用 `abort` 等价于“任务已停止”来排除。

**合并前最小要求：** 在交换 writer **之前**，用 `SessionTitle` 与 `land()` 共用的锁使旧 pass 失效；可利用现有 `adopt`/generation 机制，不必增加取消状态机或恢复 30 秒标题等待。补一个取消与 `land` 并发、随后交换 writer 不会改写 B 标题的回归用例。现有挂住标题测试始终停在网络 await，覆盖不到这个同步收尾窗口。

## 3. 修复的其它影响

### 终止事件与 usage

| 方言 | 结束位置及 usage 去向 | 实测 |
|---|---|---|
| OpenAI Chat Completions | 仍读过 `finish_reason`，保留后续 usage chunk，直到 `[DONE]` 才停。`src/provider/openai.rs:343` 读 usage，`src/llm/sse.rs:105` 结束。没有把 `finish_reason` 当结束条件。 | `chat_completions_ends_at_done_although_the_body_stays_open`：正文 `all of it`，input/output **10/5**，一次连接（`tests/provider/wire.rs:754`）。 |
| Anthropic | `message_start` 提供初始 usage，`message_delta` 累积覆盖（`src/provider/anthropic.rs:228`、`:281`）；无 usage 的 `message_stop` 才结束。 | `anthropic_ends_at_message_stop_although_the_body_stays_open`：正文完整，usage **12/7**（`tests/provider/wire.rs:781`）。 |
| Responses | `response.completed` **本次仍作为事件交给 provider**，下一次 `next()` 才返回 None；provider 在 `src/provider/openresponses.rs:380` 提取该事件的 usage。 | `responses_ends_at_completed_although_the_body_stays_open`：正文完整，usage **3/4**（`tests/provider/wire.rs:821`）。 |
| Gemini | 未新增按内容或 finish reason 提前结束的逻辑，仍从 SSE 读到 EOF；`src/llm/google.rs:484`、`src/provider/google.rs:480`。有 usage 的最后一块仍覆盖先前值（`:490`）。 | 现有 `a_google_stream_assembles_text_thoughts_and_function_calls` 没有终止事件，EOF 后得到 usage **7/3/10**（`tests/provider/google.rs:529`、`:568`）；Google 15 项测试通过。 |

这些测试测到 `RoundResult.usage`，未单独新增端到端账单测试；正常成功分支仍在 `src/repl/run.rs:775` 将 usage 放入 assistant 消息，`:783` 记账，`:799` 持久化，接线未变。没有发现终止事件导致正常 usage 丢失。协议结束后再发送 usage 的非规范中转不会被继续读取；本轮没有实测这类服务，也没有理由为它恢复 drain-to-EOF。

这里不承诺“任意被截断的请求都能得到准确 usage”：partial 的保留不等于能够补造 provider 没交出的用量。Gemini 无终止事件，若最后内容后连接不关，仍受 idle 约束；本次也没有把它误当成三种已有终止事件的协议。

### partial 的 `interrupted: true`

**可接受，不阻塞。** `finalize_interrupt` 在 `src/repl/turn/interrupt.rs:44` 用现有字段标记“被截短的回答”；停滞当场仍走 `State::Error`、Failed 通知和 `Response stalled` 红块（`src/repl/run.rs:744`），并提示 `the reply may be incomplete`（`:971`），没有改成用户取消，也没有自动重试。

恢复会显示 `(interrupted)`（`src/repl/render/replay.rs:216`），Markdown/HTML 导出亦如此（`src/repl/commands/export.rs:451`、`:582`）。这会丢掉“用户 ESC 还是线路停滞”的原因区分，但保留了最重要的“不完整”语义，与当场显示具体错误并不矛盾。本修复不必为此扩展会话 schema 或新增停止原因枚举；若以后改统一文案，可用更中性的 incomplete，但不是本轮必须项。

### 标题被放弃的代价

**普通下一条输入现在不会放弃标题**；它让 pass 在后台继续。会放弃的是退出，以及以 `/` 开头且不是四个只读 viewer 的输入（`src/repl/run.rs:578`）。这个范围也包括不交换 writer 的命令、甚至未识别的 slash 文本，属于较保守的处理。

被放弃时若模型标题尚未落地，保留前 40 个字符的占位名；`seeded` 仍为真，下一条普通消息不会重新发起标题请求（`src/repl/title.rs:95`）。因此占位名可能成为该会话最终名称。**这个取舍可以接受**：标题是辅助信息，不能阻塞输入/退出，不要求额外重试系统。它也不等于 Opus 原问题仍未修——单纯 ESC 保留 partial、继续聊天或空闲等待时，标题任务现在仍能完成。唯一不可接受的是上一节所述“写入另一个会话”的竞态。

### 300 秒与 `0`

`src/llm/client.rs:41` 已改为 **300 秒**；`:570` 仍是 trim 后解析无符号整数秒，`0 → None`，非法值回到现在的 300 秒。`stream_idle_timeout_reads_the_override` 通过，单独把常量改回 180 后断言 `180s != 300s` 失败；并未真的等待五分钟来测试默认值。

`a_zero_idle_bound_never_times_a_stream_out` 实测关闭 idle 后 1200 ms 内不自行结束，随后取消仍有效；心跳续期和静默失败测试也通过。300 秒只是更保守的静默容忍度，不保证任意慢推理都不会超时；超过它仍可调大或关闭。`0` 只关闭 SSE 空闲限制，不关闭 connect/header/HTTP2 等其它等待边界。现有 `src/llm/error.rs:55` 的 `0 to never time out` 表述仍偏宽，这是首轮已有的非阻塞文案问题，本轮不提升为新的合并要求。

## 4. 测试是否假绿：亲自做了旧行为对照

临时副本：`/private/tmp/iota-review2-mut-4aus2xw8`，由 `git archive HEAD` 生成。`src/repl/run.rs`、`state.rs`、`turn/mod.rs` 用 `git show 02bb3bb:<path>` 恢复；SSE 恢复 `[DONE]` 后 drain，Anthropic 移除 stop 截止，Responses 移除 completed 后的提前结束，默认常量单独改回 180。保留当前测试、测试 fake、当前默认值断言和三个协议的 fixture；用逐文件字节比较确认 `tests/` 与工作树一致。是生产行为变异，不是改坏断言来造红。

| 同一测试/测试组 | HEAD | 恢复旧行为 | 失败证据 |
|---|---:|---:|---|
| `cargo test --test repl stall:: -- --nocapture` | **5 通过** | **1 通过 / 4 失败** | R-TOOL 在 `stall.rs:145` 因第三次请求缺少历史、长度不足 5 而下溢；两个标题等待用例撞 `stall.rs:74` 的 10 秒 watchdog；Opus 用例在 `:269` 报模型标题未落地。 |
| `cargo test --test provider wire:: -- --nocapture` | **16 通过** | **12 通过 / 4 失败** | `[DONE]` 后多余事件的解析用例失败；OpenAI/Anthropic 开口 body 用例得到 `StreamIdle(2s)`；Responses 用例因 **2.004069042 s** 超过小于 1 秒的截止断言而失败。 |
| `cargo test --lib stream_idle_timeout_reads_the_override -- --nocapture` | **1 通过** | **1 失败** | `left: 180s; right: 300s`。 |
| `IOTA_TMUX_REQUIRED=1 cargo test --test ui_tmux tmux_hung_provider -- --nocapture` | **PASS=11 FAIL=0** | **PASS=8 FAIL=3** | `the next message waited on the title pass`；`the exit waited on the title pass`；`a complete answer failed as a stall because its body stayed open`。 |

Responses 的反向结果特别有价值：旧行为在这个 fixture 下未必直接返回错误，但耗时断言仍抓住等到 idle 才结束的行为。不能只以“不出现红块”认定它结束正确。

变异副本复用了编译缓存。切回工作树后一次复查误用了变异产物，仍报 4 个失败；没有将此记为 HEAD 回归。随后执行 `cargo clean -p iota`，从 HEAD 重新编译并再次验证：REPL stall **5/0**、wire **16/0**、Google **15/0**、title 状态单测 **20/0**、默认值单测 **1/0**。正常标题落盘的 `commands::title_pass_names_the_session_on_the_second_provider` 在初次 HEAD 基线也通过。最终 tmux 亦重新构建核验，**PASS=11 FAIL=0 WARTS=0**，包含构建共 36.93 秒；结束后已断开临时副本的共享 target 链接。未运行全分支测试或 clippy，不把局部通过表述为全仓库验证。

日志：`/private/tmp/iota-review2-{stall,wire,google,default,title-state}-rebuilt.log`、`/private/tmp/iota-review2-tmux-rebuilt.log`；反向结果为 `/private/tmp/iota-review2-mut-{stall,wire,default,tmux}.log`。首次 wire 在沙箱内因端口绑定 `PermissionDenied` 失败，申请权限后的上述结果才是有效测试结果。

### 场景 27 的 11 项断言够不够硬

准确说是 **6 个子场景、11 项 PASS 断言**，不是 11 个独立子场景（脚本 `tests/ui_tmux/scenarios/27-hung-provider.sh:6`）。

- 第 4、5 子场景够硬：`:92` 等的是模型回复 `echo: fourth`，不是用户输入回显；`:108` 等的是 `pane_dead`。`FAST=80`（`:24`）配合 100 ms poll，名义约 8 秒，明显小于旧的 30 秒；实际恢复旧实现两项都红。它们证明没有旧的长等待，不是严格毫秒级性能测试。
- 第 6 子场景较弱：`:121` 看到正文后，`:123` 只观察超过 2 秒 idle 的约 3 秒内没有 `Response stalled`，`:128` 查没有重试文案。**推测：** 一个“收到 DONE 后停止 idle 计时、但继续永久等 EOF”的错误实现仍会让这段绿；正文到达不证明 turn 已成功返回或已落盘。
- 配合 wire 的 `Result::Ok`、小于 1 秒完成和 usage 数值断言，当前 R-DONE 证据足够，不是整组假绿。值得的小补强是第 6 段再发送一条消息并限时断言下一次模型回复；不要求扩展成大套 UI 基建。第 3 段无重试仍只是数 UI 文案，实际不重放还由 wire 的 accept 计数、R-TOOL 的请求计数和错误分类共同支持。
- R-TOOL 用例确实抓住历史删除，但目前只核对结果的 role/call id，未断言工具结果正文；变红时也是切片下溢而非友好提示。加长度前置断言和结果正文断言会更清楚，均属非阻塞补强，不改变已经证实的修复结论。

## 5. 合并门槛

**必须修一项：切换 writer 前使旧标题 pass 在同步意义上失效，并用并发交错用例钉住它。** 现有标题盲区、终止事件、停滞保留、默认值修复及其变红对照均成立，无需重做。

`interrupted` 的通用标记、放弃未完成标题留下占位名、300 秒及 `0` 的语义均可接受；tmux 第 6 段的正向结束断言值得补，但已有 wire 覆盖，不单独阻止合并。首轮涉及且本次未改的首部等待、HTTP/2 策略等不在本轮重开评审。
