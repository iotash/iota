# 请求挂住 / 取消修复 —— 第二轮评审（只评 `aa989fb`）

> 2026-10-01 · 分支 `req-cancel` · 评审对象 `git diff 02bb3bb..HEAD`（一个提交 `aa989fb`）· 上一轮：`request-cancellation-review-opus.md`（下称「上轮」）、`request-cancellation-review-codex.md`
> 评审只读：没有改代码和测试。变异实验和探针都在 scratchpad 里的 `git archive HEAD` 副本上做（`target` 用 APFS clone），本树源码没有动过。
> 标「实测」的附命令或输出；标「查证」的来自读代码，附行号；标「推测」的没有验证。

## 结论

**能合并。** 四条问题都修好了：上轮我提的三条，加上 codex 提的两条。修复没有引入必须在合并前处理的问题。测试是真的：13 个变异（单测层 9 个、tmux 层 3 个、外加 1 个忠实版 `[DONE]` 回退）每个都能让至少一个测试变红。`cargo clippy --all-targets` 退出码 0；全量 `cargo test` 在强制重建后，除两类与本提交无关的环境性失败外全绿（§4.3）。

本轮的新发现（都不阻塞合并）：

1. **在 HTTP/1.1 上，`[DONE]` 处提前结束会丢掉连接复用**（实测，§3.1）。如果服务器把 chunked 的结束块 `0\r\n\r\n` 和 `[DONE]` 分两次写出，客户端没读到结束块就丢弃 body，hyper 就不会把这条连接放回池子：3 次请求出现 3 次 accept，修复前是 1 次。HTTP/2（官方端点）推测不受影响；受影响的是走 h1 的中转和本地 server，代价是每个工具轮次多一次握手。这是性能问题，不是正确性问题，可以以后再做。
2. **本树 `target` 里有陈旧的构建产物**（实测，§4.3）。在本树直接跑 `cargo test`，`llm::client::tests::stream_idle_timeout_reads_the_override` 会红（`left: 180s, right: 300s`）。原因是 lib 单测二进制在 18:22 构建，用的是一份 180 s 的源码，而 `src/llm/client.rs` 的 mtime 被还原回 18:04，cargo 因此判定为 Fresh。代码本身没问题，但**在本树跑出的测试结论不可信**，合并前的 `ci.sh` 应先 `touch src/llm/client.rs`，或者 `cargo clean -p iota`。推测是有人在本树做变异实验时用 `mv` 还原文件，把旧 mtime 一起带了回来。
3. `/` 命令放弃标题请求这个改法，副作用是**这一会话永久停在占位名**（查证，§3.3）。触发窗口很窄。把 `/session` 的 `adopt()` 前移一行，就能去掉这个放弃动作。值得做，但不急。
4. 文档小问题：X-64 和两处注释里「重放会让工具执行两次」的理由还没改（上轮 §3.3）；X-64 写的是「a command that may swap or mint the writer」，而代码的实际行为是「除只读查看器之外的**所有** `/` 命令」。

---

## 1. 上轮我提的三条

| # | 问题 | 现状 | 依据 |
|---|---|---|---|
| ① | `[DONE]` 之后连接不关，被报成 `Response stalled`，整轮回滚 | **已修** | `src/llm/sse.rs:100-107`：看到 `[DONE]` 就 `return Ok(None)`；`:84-86` 让结束状态保持（sticky）。另外两种方言也同样处理了：`src/llm/anthropic.rs:638-641`（`message_stop`），`src/llm/responses.rs:603`（`response.completed`）。三个 wire 用例 `*_although_the_body_stays_open` 用 2 s 阈值、5 s 外层超时，断言 1 s 内结束、usage 还在。tmux 场景 27 第 6 段做了端到端验证。变异 R1b、R2、R3、T1 都会变红（§4.1） |
| ② | `interrupt_turn` 无条件 abort 标题，保留部分输出后会话永远拿不到模型标题 | **已修** | `interrupt_turn`（`src/repl/run.rs:907`）开头的 abort 删掉了。现在只在 `unseed` 返回 `true` 时 abort（`:938`；错误回滚路径是 `:761`）。`unseed` 改为返回 `bool`（`src/repl/title.rs`）。这样一来，后面某一轮被丢弃时，不会误伤第一轮已经在命名的标题请求，比我上轮建议的「移进 else 分支」更精确。用例 `stall::an_interrupt_that_keeps_a_partial_keeps_the_title_pass` 在变异 R5（把无条件 abort 加回去）下变红 |
| ③ | 默认阈值 180 s → 300 s | **已修** | `src/llm/client.rs:41`；单测里的断言改到 `:686`；X-64 的文案同步改了 |

## 2. codex 的两条

| 问题 | 现状 | 依据 |
|---|---|---|
| 超时走普通失败回滚，会删掉已经完成的工具轮次 | **已修** | `TurnReport::is_stalled`（`src/repl/turn/mod.rs`）+ `stream_round` 在停滞时带上 partial（同文件 `:476` 附近）+ `keep_stalled_turn`（`src/repl/run.rs:951`）复用 ESC 的三态表。有内容可留就保留并持久化，没有就照旧回滚（`:752-766`）。停滞仍然是错误：红块、`State::Error`、不重试，不会显示 `Interrupted.`。用例 `stall::a_stall_after_a_tool_round_keeps_the_call_and_its_result` 断言**下一次请求**里带着调用、结果和 partial，在变异 R4 下变红；`a_stall_with_nothing_to_keep_rolls_the_turn_back` 覆盖了另一个分支 |
| 标题请求单独挂住时，仍有 30 s 的按键盲区 | **已修**（换了一种修法） | `join_title` 整个删掉了，现在没有任何地方等标题请求。普通消息让它继续跑（`run.rs:578` 只对 `/` 命令处理）；`/` 命令（只读查看器除外）和退出都会直接放弃（`:583`、`:561`）。用例 `a_hung_title_pass_never_holds_the_{next_message,exit}` 和场景 27 第 4、5 段覆盖了这条；变异 R7、R8、T2、T3 都会变红 |

## 3. 修复本身有没有引入新问题

### 3.1 终止事件结束流之后，usage 还能记到吗

| 方言 | usage 在哪 | 结论 |
|---|---|---|
| chat completions | `include_usage` 的那个 chunk 在 `[DONE]` **之前**（`src/provider/openai.rs:341-346`，按「最后一个 chunk 为准」处理） | 记得到。用例 `chat_completions_ends_at_done_although_the_body_stays_open` 断言 `(10, 5)`。如果某个兼容实现把 usage 放在 `[DONE]` 之后，就会丢失（推测：没见过这种实现，也不符合 OpenAI 规范） |
| Anthropic | `message_start` 带 input，`message_delta` 带 output，都在 `message_stop` 之前 | 记得到，用例断言 `(12, 7)`。下游 `provider/anthropic.rs:291` 本来就不处理 `message_stop`，提前结束不会丢信息（查证） |
| Responses | usage 就在 `response.completed` 里面，先把这个事件交出去，下一次调用才返回 `None` | 记得到，用例断言 `(3, 4)`。`failed`、`incomplete` 走 `Err`，本来就会结束 |
| Gemini | 没有终止事件，仍然读到 EOF 才结束 | 行为不变。如果中转不关 body，完整的回答仍会在 300 s 后被报成停滞；不过现在会**保留 partial**（§2），不会再整轮回滚，代价小了很多。可以拿最后一个 chunk 的 `finishReason` 当终止信号，但不值得为这种推测中的罕见情况再加一个状态。X-64 已经写明「Gemini still ends at EOF」 |

**新问题：HTTP/1.1 连接复用丢失（实测）。** 我在副本的 `tests/provider/wire.rs` 里临时加了一个探针：一个 keep-alive 的 h1 服务器，每次请求回 `data:{"x":1}`、`data: [DONE]`，隔 20 ms 再发 `0\r\n\r\n`；用同一个 `reqwest::Client` 连续发 3 次请求，每次都读到 `None` 后丢掉 `Sse`。

```
HEAD（[DONE] 处结束）                         PROBE accepts = 3
回退为读到 EOF（修复前）                       PROBE accepts = 1
HEAD，但结束块与 [DONE] 在同一次 write 里      PROBE same-write accepts = 1
```

原因：body 没读完就被丢弃，hyper 只能关掉这条 h1 连接。影响范围：走 HTTP/1.1 的中转和本地 server（ollama、vLLM、llama.cpp 这类）；`message_stop` 和 `response.completed` 同样受影响。代价是每个轮次（工具循环里每一轮）多一次 TCP 握手；如果是 TLS，还要加一次 TLS 握手，推测每次 100–300 ms。HTTP/2 只会 `RST_STREAM` 这一条流，连接本身保留（推测，没有实测）。

**建议（可选，不阻塞）：** 看到终止事件后，用一个很短的上界把 body 读到 EOF，比如 `timeout(Duration::from_millis(200), drain)`，超时就放弃。规范的 server 马上就会关，连接能回池；不关 body 的中转最多多等 200 ms。改动只在 `Sse` 里，几行。不做也可以：这是性能退化，不影响正确性，而且只在 h1 上出现。

### 3.2 停滞时 partial 带 `interrupted: true`：恢复和导出时显示 `(interrupted)`，当场显示的是错误

**可以接受，不建议改。**

- 这个标记的真实含义是「这条回复被截断了」，消费方只有三处：`render/replay.rs:216`、`commands/export.rs:451,582`，以及 usage 记账 `run.rs:980`。发给模型的时候不看这个标记（查证：`provider/` 下没有读取它的地方）。所以这个不一致只影响外观，不影响模型看到的内容。
- 当场显示的是红块，外加 notice「What arrived before the stall is kept — the reply may be incomplete.」；事后显示 `(interrupted)`。两者说的都是「不完整」，只是来源不同。如果要区分，就得给 session 记录加一个字段，再加读写和兼容，代价大于收益。
- 唯一值得补的是一句文档：在 X-64 里写明「stall 保留的消息在 resume/export 中同样显示为 `(interrupted)`」。另外 `src/repl/turn/mod.rs` 的注释引用的是 `stalled_turn`，实际函数名是 `keep_stalled_turn`，属于笔误。

### 3.3 输入和退出时放弃未完成的标题请求，代价能接受吗

先说清楚修了之后的真实行为（查证）：

- **普通消息**：不等待，也不放弃，标题请求继续跑（`run.rs:578`）。最常见的路径没有代价。
- **退出**：直接放弃（`:561`）。如果标题还没回来，这个会话的名字就永久是占位名：恢复时 `adopt()` 会把它定为最终名字。只有「第一轮答完就立刻退出，而且标题比回答还慢」时才会触发，窗口很窄。要避免它，只能在退出时有界地等一下，而这正是这次要消灭的盲区。**接受。**
- **除只读查看器外的所有 `/` 命令**（`/help`、`/model`、`/compact` 等）：也直接放弃（`:583`）。`abort_title` 不会 `unseed`，`seeded` 一直为真，`seed()` 每次都在 `title.rs:96` 提前返回。所以在这个会话里**再也不会重新发起**标题请求，名字永久停在前 40 个字的占位名上。窗口同样很窄：第一轮答完、标题还没回来时，就去敲一个 `/` 命令。标题请求用的是会话模型（`run.rs:888` `set_model(model)`），推理模型生成标题可能要好几秒。

**`/session` 那条更彻底的改法值不值得做：值得，但不急。** 我核对了所有会换 writer 的地方，只有两处：

- `/save`（`src/repl/commands/save.rs:47-66`）：不需要放弃标题。标题在换 writer 之前落地，会被 `reapply` 补写；在换之后落地，直接写进新的 writer。带名字的 `/save` 走 `adopt_name`，把 `titled` 置位，迟到的 `land` 会被丢弃。三种时序都正确。
- `/session`（`src/repl/commands/session.rs:179-192`）：先 `*slot = Some(writer)`（`:181`），过了十来行才 `adopt()`（`:192`）。运行时是 multi-thread 的（`src/main.rs:32`），标题任务可以在另一个 worker 上**正好落在这个窗口里**，把旧会话的标题写进刚恢复的 bundle。现在的「`/` 命令先放弃」恰好挡住了这个竞态，所以当前代码是**正确**的。

改法：把 `repl.session.titler.adopt()` 挪到 `:179` 的 slot 替换之前。`land` 和 `adopt` 拿的是同一把 titler 锁，`land` 在持锁时写 writer。`adopt` 先拿到锁，迟到的 `land` 就会因为 `titled` 被丢弃；如果 `land` 先拿到锁，写进去的是旧 writer。两种顺序都安全。挪完之后，`run.rs:578-584` 这整段放弃逻辑都可以删掉，`/` 命令和普通消息一样让标题请求继续跑，占位名的代价只剩退出这一种。规模是挪 1 行、删 7 行，再加一个用例：在标题请求挂住时执行 `/model`，之后标题仍能落地。

为什么不急：触发窗口窄，后果只是外观（名字不好看），没有数据错误。

### 3.4 阈值 300 s 和「0 表示关闭」

- 300 s 是业界区间的上沿，理由上轮已经论证过，X-64 里也写进去了。ESC 一直有效，空闲超时只是没人值守时的兜底，所以没问题。
- `0` 表示关闭：`stream_idle_timeout(Some("0")) == None`，单测 `:674` 覆盖了；连接层面由 `a_zero_idle_bound_never_times_a_stream_out` 覆盖。语义没有变。上轮指出的「`0` 只管响应头之后」（本地 server 要等第一个 token 才发头的情况）仍然成立，这是本次修复之前就有的行为，X-64 的「Not done」已经覆盖。
- 停滞现在会保留 partial，所以就算被误杀（比如代理把整个 body 缓冲起来），损失也从「整轮回滚」降到了「保留已完成的部分、提示不完整」。这一点让 300 s 的取舍更稳了。

## 4. 测试是不是假绿

### 4.1 变异实验（副本，本树未改）

| # | 变异 | 结果 |
|---|---|---|
| R1b | `sse.rs` 看到 `[DONE]` 后改回「`done = true` 然后 `continue`」，一直读到 EOF（忠实还原修复前的行为） | `wire::sse_frames_parse_across_chunk_boundaries`、`wire::chat_completions_ends_at_done_although_the_body_stays_open` **FAILED** |
| R2 | `anthropic.rs` 的 `message_stop` 改成 `continue` | `wire::anthropic_ends_at_message_stop_although_the_body_stays_open` **FAILED** |
| R3 | `responses.rs` 中 `completed` 恒为 false | `wire::responses_ends_at_completed_although_the_body_stays_open` **FAILED** |
| R4 | `run.rs:723` 中 `stalled` 恒为 false（停滞走普通回滚） | `stall::a_stall_after_a_tool_round_keeps_the_call_and_its_result` **FAILED** |
| R5 | `interrupt_turn` 开头加回无条件 `abort_title()` | `stall::an_interrupt_that_keeps_a_partial_keeps_the_title_pass` **FAILED** |
| R6 | 普通消息也放弃标题请求（去掉 `line.starts_with('/') &&`） | `stall::an_interrupt_that_keeps_a_partial_keeps_the_title_pass` **FAILED** |
| R7 | 退出时改回 await 标题任务 | `stall::a_hung_title_pass_never_holds_the_exit`、`…_next_message` **FAILED** |
| R8 | 非查看器的输入前改回 await 标题任务 | `stall::a_hung_title_pass_never_holds_the_next_message` **FAILED** |
| T1 | 同 R1b，跑 tmux 场景 27 | `FAIL: a complete answer failed as a stall because its body stayed open`（PASS=10 FAIL=1） |
| T2 | 同 R8，跑 tmux 场景 27 | `FAIL: the next message waited on the title pass`（第 4 段），34.8 s |
| T3 | 同 R7，跑 tmux 场景 27 | `FAIL: the exit waited on the title pass`（第 5 段），35.3 s |

另外，R1 用的是一个不忠实的版本（顺带把 `saw_event` 置位），结果与 R1b 相同。R5 到 R8 的输出里还会出现 `commands::banner_order_and_completion_row` 失败，那是副本路径过长导致的环境性失败，见 §4.3。

### 4.2 逐条看

- **三个 open-body 用例**：同时断言了 5 s 内结束、`elapsed < 1 s`、正文正确、usage 精确，阈值是 2 s。也就是说，它们证明的是「在终止事件处结束」，而不是「等到超时后侥幸成功」。是硬断言。
- **`stall.rs`**：读的是**下一次请求的 history**（`log.send(2)` 的最后 5 条），而不是 UI 上的文字，所以证明的是模型真的能看到保留下来的内容。第二个用例覆盖了回滚分支。
  - `an_interrupt_that_keeps_a_partial_keeps_the_title_pass` 的写法是：第 2 次调用在 `on_call` 里**忙等**最多 5 s，直到窗口标题出现；而标题请求固定延迟 300 ms。在 multi_thread 2 worker 下是稳定的；但如果 CI 极慢、超过 5 s，就会因为 `landed` 为假而红，属于偏「误红」的方向，不会假绿。可以接受。
  - 两个 `hung_title` 用例用 `< 5 s` 对比 30 s 的 `TITLE_TIMEOUT`，余量很大。
- **tmux 场景 27（11 条断言）**：基线 `PASS=11 FAIL=0`，12.6 s（第一次跑 56.8 s，其中包含构建 dev 二进制的时间）。
  - 第 4、5 段（标题请求单独挂住）用的是正向断言：8 s 内出现 `echo: fourth` 或 `pane_dead`。T2、T3 下都会变红。**够硬。**
  - 第 6 段是**反向**断言：3 s 内**没有**出现 `Response stalled`，阈值是 2 s。它的计时起点是看到 `echo:` 之后，而停滞计时从最后一个字节开始，所以至少有约 1 s 的余量。T1 下会变红，说明它能区分出这个修复。小瑕疵：它没有正向断言「这一轮成功了」，比如状态行回到 idle。如果以后变成别的错误（不叫 `Response stalled`），它不会察觉。可以加一条 `check "no error block" "$(count_all 'Error')" 0` 之类的断言，不急。
  - 第 6 段的 `check "the answer is not retried"` 在修复前也是 0：drain 停滞同样不会重试。所以这条断言区分不出修复前后，是陪跑的。不算假绿，因为同一段里的反向断言已经把关。

### 4.3 全量测试与 clippy

- **本树** `cargo clippy --all-targets`：退出码 0。
- **本树** `cargo test`：`1012 passed; 1 failed`，失败的是 `llm::client::tests::stream_idle_timeout_reads_the_override`（`left: 180s right: 300s`）。原因是陈旧产物：`cargo test --lib -v` 显示 `Fresh iota`，二进制 `target/debug/deps/iota-a6dd05e1edffc127` 的 mtime 是 18:22:16，而 `src/llm/client.rs` 的 mtime 是 18:04:36，`HEAD` 里的内容是 300。**这不是代码问题**，但在本树重跑 `ci.sh` 之前，必须先让 cargo 重新编译。
- **副本** `touch` 全部 `.rs` 文件后强制重建，`cargo test --no-fail-fast`：lib `1013 passed`，`provider` 133、`repl` 121 中只有 `commands::banner_order_and_completion_row` 失败。原因是 banner 里的工作目录被截断，副本路径太长，属于环境问题；本树单跑是绿的。`tool/shell.rs` 的两个 sandbox 用例失败，原因是副本在 `/private/tmp` 下；本树单跑 `cargo test --test tool the_sandbox` 结果是 `2 passed`。其余测试目标全绿。

## 5. 能不能合并

**能。** 没有必须修的。

| 优先级 | 改动 | 位置 | 规模 |
|---|---|---|---|
| 合并前，必须（流程） | 让 cargo 重新编译陈旧的 lib 产物后再跑 `ci.sh`（`touch src/llm/client.rs`，或 `cargo clean -p iota`） | 本树 `target/` | 一条命令 |
| 合并前，建议（文档） | X-64 和 `client.rs:275`、`retry.rs:83` 中不重试的理由，从「工具会执行两次」改成「重试会乘以静默时长」（上轮 §3.3）；X-64 的「a command that may swap or mint the writer」改成「every command but the read-only viewers」；`turn/mod.rs` 注释里的 `stalled_turn` 改成 `keep_stalled_turn` | 文案 | 几处措辞 |
| 可选 | 把 `/session` 的 `adopt()` 挪到 slot 替换之前，然后删掉 `/` 命令前的放弃逻辑（§3.3） | `commands/session.rs:179-192`、`run.rs:578-584` | 挪 1 行、删 7 行，加 1 个用例 |
| 可选 | 终止事件之后，用 ~200 ms 的上界把 body 读到 EOF，恢复 h1 连接复用（§3.1） | `src/llm/sse.rs`，或三种方言共用的位置 | 几行 + 复用上面的探针当用例 |
| 可选 | 场景 27 第 6 段补一条正向断言「这一轮成功了」 | `27-hung-provider.sh` | 1 行 |
