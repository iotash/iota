# 合盖过夜后请求卡在 "Waiting for the model"：调查

Status: **调查（未拍板）** · 日期：2026-11-08 · 基于 `main` = `1d8f252`（iota 0.7.7）· 本机 macOS（Darwin 24.6）· 依赖：reqwest 0.13.4、hyper 1.11.1、hyper-util 0.1.20、h2 0.4.19、tokio 1.54.1（`Cargo.lock:1034,1147,1184,2556,3386`）

本文只做分析，不改代码。所有者的报告：用 venice.ai provider，干完活合上盖子，没退出会话；第二天打开继续同一会话，之后每个请求都挂住，busy 行一直停在 "Waiting for the model"，始终不完成。退出程序再 resume，对话就恢复正常。

## 写作约定

- 本仓库的代码写 `path:line`（`1d8f252`）。依赖源码写 `<crate>-<ver>/path:line`，根目录是 `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/`。std 源码取自本机 stable 工具链的 rust-src（项目钉的 1.98 没装 rust-src，见 §8）。
- **实测**：在本机跑过（§1.3）。**推断**：从代码推出来、没有实测支撑。**未核实**：两者都没有。
- 外部资料（Go、curl、各家 CLI）由一个子代理读源码和文档后汇报，我没有逐条复读。凡是写了文件行号的，都是它读过的原文。

## 0. 结论

1. **假设不成立**：「死连接一直留在池里，120 s 超时加重试梯子永远出不来」。源码和实测都说明，一条睡死的池化连接**最多拖垮一次 wire 尝试**，下一次尝试就走新连接（§1）。
   - HTTP/1.1：响应头超时一丢弃 future，dispatcher 就关掉这条连接，`Pooled` 不会把它放回池里。
   - HTTP/2：请求在等响应头时，PING keepalive 会生效，最多 50 s 就把连接关掉。
2. **`pool_idle_timeout` 保护不了**：它用 `Instant` 计时。`Instant` 在 macOS 上是 `CLOCK_UPTIME_RAW`，睡眠期间不走。所以睡了 8 小时的连接醒来后照样被复用（§2）。
3. **真实存在的缺陷**：醒来后的第一个请求，会先在尸体连接上白等 20–50 s（h2）或 120 s（h1），busy 行一声不吱。
   - Go 版 iota 没有这个问题：Go 的 `Transport` 取闲置连接时按挂钟判断闲置时间，专门照顾「笔记本从挂起中醒来」（golang/go#29308，`net/http/transport.go:1273-1276`）。
   - 所以这是 Rust 移植丢掉的一项保护（§5.1）。
4. **「一直挂、重启就好」用池子解释不了**。剩下几种解释（§4.3）：
   - 醒来后**新连接**也拿不到响应头。本机 HTTPS 一律走 Surge 系统代理 `127.0.0.1:6152`，venice 走的是 Surge 的上游策略。
   - 所有者等的时间短于最坏的 9–67 分钟。
   - 一种还没找到的进程内状态。

   哪一种都没坐实。§8 给出复现时要取的证据。
5. **推荐**：只做 (d)。共享 HTTP 客户端改成可替换的；发请求前若挂钟显示进程睡过（挂钟闲置超过 90 s，而 `Instant` 闲置没超过），就换一个新客户端，即新池。这等于把 Go 的规则搬过来。超时和重试契约不动（I-02、X-84）。

## 1. 死连接遇上一个请求：逐层追踪（问题 1）

### 1.1 iota 这一层

- 所有 provider 共用一个 `reqwest::Client`（`src/llm/client.rs:621-632`、`src/provider/common.rs:98-113`）。它在 reqwest 默认值之上只加了三项：`connect_timeout` 15 s、`http2_keep_alive_interval` 30 s、`http2_keep_alive_timeout` 20 s。
- 每次尝试是 `tokio::time::timeout(HEADER_TIMEOUT=120s, self.http.execute(req))`（`src/llm/client.rs:29,359-363`）。超时就**丢弃** `execute` 的 future，记为 `LlmError::HeaderTimeout`。
- `should_retry` 对 `Transport`、`HeaderTimeout` 一律返回 true（`src/llm/client.rs:547`），wire 层最多 3 次尝试（`DEFAULT_RETRIES = 2`，`:27`），退避是 0.5 s、1 s 减抖动（`:557-568`）。
- 外面再套 `retry_round` 的 11 次调用（`src/repl/turn/retry.rs:16,96-162`），`is_retryable` 对这两类错误也返回 true（`:188-203`）。

### 1.2 hyper / hyper-util：丢弃 future 之后，连接还会被复用吗

**HTTP/1.1：不会。**
- 请求一发出，dispatcher 就在 `poll_read_head` 里先问回调还在不在。回调接收端被丢弃时，它执行 `self.close()`（`hyper-1.11.1/src/proto/h1/dispatch.rs:301-307`，回调检查在 `:751-760`）。
- 池这一侧，`Pooled::drop` 只把 `is_open()` 为真的连接放回去（`hyper-util-0.1.20/src/client/legacy/pool.rs:560-571`）。
- `is_open = !poisoned && is_ready()`（`hyper-util-0.1.20/src/client/legacy/client.rs:854-856`）。h1 的 `is_ready` 是 `giver.is_wanting()`（`hyper-1.11.1/src/client/dispatch.rs:83-85`）。请求在途时 dispatcher 不处于「想要」状态，所以这条连接不会回池。

**HTTP/2：取决于 PING，不取决于超时。**
- h2 连接是共享的：每次 checkout 都把它重新插回闲置列表，而且 `idle_at: now`（`pool.rs:319-325`）。
- 它的 `is_ready` 只看连接任务是否已经结束（`hyper-1.11.1/src/client/dispatch.rs:141-143`）。所以 HeaderTimeout 丢弃一个流，**不会**把连接逐出池。
- 能逐出它的是 keepalive PING：
  - 等响应头的 `ResponseFutMap` 持有一份 ping `Recorder`（`hyper-1.11.1/src/proto/h2/client.rs:527,566-571`），`Arc` 计数因此 > 2，连接被视为非闲置（`ping.rs:326-328`）。
  - 于是 `maybe_schedule` 在 `last_read_at + interval` 安排 PING（`ping.rs:432-455`），发出后再等 `timeout`（`:457-481`）。超时返回 `KeepAliveTimedOut`（`:483-492`），连接 future 结束（`client.rs:244-247`）。
  - 在途请求随之以 `KeepAliveTimedOut` 失败（`client.rs:658-659`），这条连接也不再 `is_open`。

**hyper-util 自带的重试帮不上忙**：`retry_canceled_requests` 只在请求还没开始写、连接又是复用的时候才重试（`client.rs:241-262`，可重试分支在 `:323-334`，只在 `take_message()` 拿回了请求时成立）。POST 一旦写进死连接，就不在这里重试，由 iota 的 wire 循环接手。

### 1.3 实测：可冻结的中继

我在 scratchpad 里写了一个临时 crate，没有提交。版本钉成与本仓库相同（拷了本仓库的 `Cargo.lock`）。

- 结构：hyper 源站（h1 + h2c）前面放一个 TCP 中继，客户端是 reqwest 0.13.4，builder 与 `http_client_builder` 相同（只缩短了 interval 和超时）。
- 「合盖」的模拟：中继对**已有**连接的字节双向吞掉，但不关 socket。这正是「对端没了、没有 FIN 也没有 RST」。新连接正常放行。

| 场景 | 第 2 个请求（命中尸体） | 第 3 个请求起 | 中继累计接受的连接 |
|---|---|---|---|
| h1，头超时 8 s | 8.0 s `HEADER TIMEOUT` | 0.0 s 200 | 2 |
| h2，PING 2 s + 2 s，头超时 8 s | 4.0 s `Transport: SendRequest, Http2, KeepAliveTimedOut` | 0.0 s 200 | 2 |
| h2，PING 30 s，头超时 3 s | 3 s 超时 | **第 3–6 个请求全超时**，都在同一条连接上 | 1 |

最后一行说明：h2 下 HeaderTimeout 本身不逐出连接，只有 PING 能逐出。生产里 30 + 20 = 50 s 小于 120 s，所以 PING 先到。这个不等式目前没有测试钉住（§9 第一条）。

### 1.4 经过 Surge 时

- 本机 `scutil --proxy` 显示 HTTP 和 HTTPS 代理都是 `127.0.0.1:6152`（Surge 正在运行）。reqwest 的 `system-proxy` 特性在 `build()` 时读一次 macOS 的 SCDynamicStore：`reqwest-0.13.4/src/async_impl/client.rs:414-416` → `proxy.rs:515-517` → `hyper-util-0.1.20/src/client/proxy/matcher.rs:238-243,576-589`。所以 iota 发往 venice 的 TCP 对端是**本机回环上的 Surge**，不是 venice。
- 推论 1（推断）：TCP keepalive（reqwest 默认 15 s × 3，`reqwest-0.13.4/src/async_impl/client.rs:303-305`）探测的是回环，永远有 ACK，**看不见上游死了**。剩下能看见的只有端到端的 h2 PING，以及 HeaderTimeout。
- 推论 2：`connect_timeout` 包住了「CONNECT 隧道 + TLS」整个建连过程（`reqwest-0.13.4/src/connect.rs:943-946`）。所以新连接若在上游 TLS 握手处卡住，15 s 就失败。

## 2. `pool_idle_timeout` 是否有保护（问题 2）

- 池按 `idle_at` 判断是否过期：`now.saturating_duration_since(instant) > timeout`（`pool.rs:772-776`）。`now` 取自 `pool_timer`（`pool.rs:342-346`），reqwest 设置的是 `TokioTimer`（`reqwest-0.13.4/src/async_impl/client.rs:976-978`），其 `now()` 就是 `tokio::time::Instant::now()`（`hyper-util-0.1.20/src/rt/tokio.rs:299-301`）。tokio 的 `Instant` 包的是 `std::time::Instant`。
- std 在 Apple 平台用 `CLOCK_UPTIME_RAW`。注释原文：「does not increment while the system is asleep」（`library/std/src/sys/time/unix.rs:55-67`）。
- **结论：没有保护。** 合盖前闲置 10 s 的连接，睡 8 小时醒来，池看到的闲置时间仍约 10 s 加上醒来后的秒数，照样复用。h2 连接更糟：每次 checkout 都把 `idle_at` 刷成 now（§1.2），只要最后一轮在合盖前 90 s 内结束，它就一定被复用。
- 上游已知此事：hyperium/hyper#3810「pool won't clean expired connections after system sleep」仍是 OPEN，维护者认为该在 libstd 里解决；rust-lang/rust#79462 也是 OPEN（子代理读过 issue）。

## 3. h2 PING 能不能救（问题 3）

- **闲置时不发**：`while_idle` 默认 false（`reqwest-0.13.4/src/async_impl/client.rs:353,971-973`），iota 也没打开它。在 `maybe_schedule` 和 `maybe_ping` 里，闲置时直接返回（`ping.rs:435-437,469-471`）。所以睡眠中和刚醒时，池里的 h2 连接没有任何探测。
- **等响应头时会发**（§1.2）。醒来后，`schedule` 的截止时刻是 `last_read_at + 30 s`，用的也是 `CLOCK_UPTIME_RAW`。只要最后一次读发生在合盖前 30 s 以上，这个时刻已经过去，流一打开 PING 就发出，20 s 后超时。
- 结果：第一个请求白等约 20 s，最多 50 s，然后以 `Transport`（`KeepAliveTimedOut`）失败，wire 层在 0.5 s 内换新连接重试。这对应实测表的第 2 行。

## 4. 所有者实际看到什么，看多久（问题 4）

### 4.1 屏幕上有什么

- 一轮之内，失败的 wire 尝试**不换标签、不重置计时**：仍是 "Waiting for the model"（`src/repl/turn/phases.rs:97-128`，契约见同文件 `a_success_head_moves_the_row_to_the_first_token_phase` 的注释）。
- wire 层的重试不出任何提示。
- 梯子的标签 `Network error — retrying (attempt n/10)` 只在第 n 级的退避期间出现 n 秒（`retry.rs:140-151`）。下一次 `round()` 一开始，busy 行就回到 "Waiting for the model"。
- 只有整条梯子用完，才落一个 `Network error` 错误块（`src/repl/errors.rs:120-124`）。

### 4.2 时间线

- **已核实的情形：只有池里的尸体。** 第一次尝试白等约 20 s（h2，最多 50 s）或 120 s（h1），之后正常出结果。没有任何提示，也不会有恢复通知，因为 wire 层的重试对梯子不可见。这只是「慢一下」，不是「一直挂」。
- **若每次尝试都失败**（推断：醒来后新连接也拿不到响应头）：共 11 次调用 × 3 次尝试 = 33 次。wire 退避每次调用最多 1.5 s（共约 16.5 s），梯子退避 1 + … + 10 = 55 s。

| 每次尝试卡在哪 | 单次上限 | 到错误块为止 |
|---|---|---|
| 建连（隧道或 TLS 握手不通） | 15 s（`CONNECT_TIMEOUT`） | 约 9.4 分钟 |
| h2 已连上、没有响应头 | 约 50 s（PING） | 约 28.7 分钟 |
| h1 已连上、没有响应头 | 120 s（`HEADER_TIMEOUT`） | 约 67.2 分钟 |

这段时间里，每 45 s 到 6 分钟才有一个持续 n 秒的 retrying 标签，其余时间都是同一个计时器在走的 "Waiting for the model"。人看上去就是「一直挂」。

### 4.3 「一直挂、重启就好」的候选解释

| # | 假设 | 状态 |
|---|---|---|
| H1 | 尸体连接永远留在池里 | **否定**（§1.2 源码，§1.3 实测） |
| H2 | 醒来后一段时间内，新连接经 Surge 走 venice 的上游也拿不到响应头；所有者退出重开时，路径恰好已经恢复 | 未核实。与「只有 venice」相容：Surge 配置里没有针对 venice 的规则，它落到 FINAL 策略（`~/Library/Application Support/Surge/Profiles/*.conf` 里 grep `venice` 为空）。FINAL 是什么、其他 provider 是否 DIRECT，都没读 |
| H3 | 所有者等的时间短于 §4.2 的窗口 | 未核实。报告里没有等待时长 |
| H4 | 进程内另有状态，重启才清掉。例如 reqwest 在 `build()` 时取了系统代理的快照，若醒来后代理设置变了，旧客户端还拿着旧地址（§1.4） | 未核实。本机此刻的代理是 Surge 的固定端口，没有找到它会变的证据 |

## 5. 成熟客户端的做法（问题 5，子代理汇报）

### 5.1 Go net/http：iota 的原版

- `DefaultTransport` 的 `IdleConnTimeout` 是 90 s，拨号 `KeepAlive` 是 30 s（`net/http/transport.go:47-58`）。
- 取闲置连接时**只按挂钟**判断（`transport.go:1273-1276`）：「considering only the wall time (the Round(0)), in case this is a laptop or VM coming out of suspend with previously cached idle connections」。出处是 golang/go#29308。
- Go 版 iota 克隆了 `DefaultTransport`（brain `internal-llm-client` 载重契约 ④），所以**原版不会复用睡过 90 s 以上的连接**。Rust 版丢了这项保护。这是一条没人选择过的偏离，属于 brain `port-regressions` 那一类。
- Go 的单调钟在 darwin 上也是 `mach_absolute_time`，睡眠时不走（`time/time.go:54`），所以它才用挂钟兜底。
- Go 只在「连接是复用的、且什么都还没写」时重试（`transport.go:844 shouldRetryRequest`），已写出的 POST 不重试。

### 5.2 curl

- 命令行默认开 TCP keepalive 60 s；libcurl 默认关。
- 闲置连接最长 118 s（`CURLOPT_MAXAGE_CONN`），复用前还做一次存活检查（`lib/url.c url_match_conn` → `Curl_cpool_conn_seems_healthy`）。
- 它在 Apple 上用 `CLOCK_MONOTONIC_RAW`，按 man page 的说法睡眠时也走（推断）。

### 5.3 Node / undici

- 闲置 socket 4 s 就关（`keepAliveTimeout`）。
- libuv 在 darwin 上用 `mach_continuous_time`，睡眠计入（`src/unix/darwin.c:60-62`）。所以 Node 系的 CLI（Claude Code、Gemini CLI）天然不会复用睡过的连接。

### 5.4 各家 CLI

- **Claude Code**：文档写明有睡眠处理，重试标签之一是「Connection lost while your computer was asleep」，「Once your computer wakes, Claude Code treats the connection as broken」。另有首字节、字节、事件三层看门狗。
- **Codex**：用 reqwest 默认值，加 300 s 流空闲超时和重试。没找到断线后重建客户端的代码（未核实），它甚至可以选择关掉 `pool_idle_timeout`。
- **Gemini CLI**：undici，`headersTimeout` 60 s，`bodyTimeout` 300 s。
- **opencode**：未核实。

### 5.5 reqwest 本身

- 没有清空连接池的公开接口。seanmonstar/reqwest#2956（OPEN）里维护者说 poisoning 没暴露出来，**唯一的办法是新建一个 `Client`**。
- 默认的 `tcp_user_timeout` 只在 Linux 上设置。

## 6. 候选修法（问题 6）

| | 修法 | 修得了醒来的情形吗 | 代价和副作用 | CI 能测吗 |
|---|---|---|---|---|
| a | wire 层遇到 HeaderTimeout 或 Transport 时，重试前换一个新的共享客户端 | 对尸体**没有增益**，hyper 已经自己逐出了（§1.3）。只对 H4 那种未知进程内状态有用，等于「局部重启」 | 共享客户端得变成可替换的（ArcSwap 或 Mutex），所有 provider 共用。并发的其他请求不受影响（旧 `Client` 由克隆保活）。每次网络错误都要重新握手（经代理多一次 CONNECT 加 TLS） | 能：用冻结中继，断言换了客户端 |
| b | 显式设置 `tcp_keepalive` 等参数 | **不能**。默认已经是 15 s × 3。经过 Surge 时对端是回环，永远有 ACK。macOS 没有 `TCP_USER_TIMEOUT` | 无 | 半开连接在 CI 里很难造 |
| c | 缩短 `pool_idle_timeout` | **不能**。它在 `CLOCK_UPTIME_RAW` 上计时，睡眠不计入。只有设成 0（不池化）才算修好 | 不池化的话，工具环每轮都要重新 CONNECT 加 TLS，延迟增加约 100–300 ms（推断） | 能，但等于测「不池化」 |
| d | 两次请求之间检测挂钟断层：挂钟闲置超过 90 s 而 `Instant` 闲置没超过，就换新客户端（新池） | **能**，而且是在第一个请求**之前**，白等的 20–120 s 归零。这就是 Go 的规则 | 每次发送多读一次 `SystemTime` 和 `Instant`。客户端可替换（同 a）。手动把系统时间往前拨也会多换一次池，无害 | 能：时钟可注入，加冻结中继，结果确定 |
| e | 整请求超时 | **不能**，而且违反 I-02 / `HEADER_TIMEOUT` 的注释（「never a whole-request timeout」，`src/llm/client.rs:28`）。每次尝试本来就有上限，问题不在这 | 会杀掉长流 | — |
| f | `http2_keep_alive_while_idle(true)` | 部分能：醒来后的定时器约 20 s 内处理掉 h2 尸体。h1 不管 | 进程活多久，就每 30 s 给每个 provider 主机发一次 PING。有的服务端会对没有流的 PING 回 `ENHANCE_YOUR_CALM` 或 GOAWAY（推断） | 能 |

几项都只修「白等一次」那部分。§4.3 的 H2 是环境问题，任何客户端改动都修不了，只能让它**更快、更清楚地报出来**。这一点属于 X-84 / busy 行的范围，不在本文。

## 7. 架构判断

- 换客户端的入口只该有一个：持有共享 `reqwest::Client` 的那一处（现在是 `default_http_client()` 的调用方和 `HttpTransport`，`src/provider/common.rs:98-113`）。不要每个 provider 各自判断。
- 挂钟检测放在 `Client::send_payload` 的发送前一步（`src/llm/client.rs:332` 的 `loop` 之前），记录「上次活动」的 `(Instant, SystemTime)` 对。
- 不加用户配置项：这是网络策略，与 `STREAM_IDLE_TIMEOUT` 不设为用户配置的理由相同（`src/llm/client.rs:38-40`）。
- 90 s 应当直接复用池的闲置上限：reqwest 默认值，在本仓库里没有常量，需要新建一个，并与 builder 的 `pool_idle_timeout` 共用。
- 重启之所以能修好，正是因为它换了一个新客户端。(d) 只在确实睡过时才这样做。

## 8. 拿不准的地方 / 下一次复现要取的证据

- H2、H3、H4 都没坐实。请所有者在下一次复现时先打开 `/debug`：每次 wire 尝试各占一行（`src/llm/client.rs:320-322`）。
  - 每行耗时约 20 s 或 120 s、之后就成功：只是尸体。
  - 每行都是 15 s 的连接错误，或 50/120 s 的超时，并且持续好几分钟：新连接也不通，属于 H2，这时再看 Surge 的请求日志。
  - 还要记下到底等了多久（H3）。
- std 源码读自 stable 工具链（1.99.1），项目钉的是 1.98，那个工具链没有 rust-src。Apple 上用 `CLOCK_UPTIME_RAW` 早于这两个版本（子代理没能确认是哪个 PR 引入的），但 1.98 的源码我没有亲眼看过。
- 经过 Surge 时，venice 是否真协商出了 h2（ALPN 在隧道里端到端协商，除非 Surge 对该主机做 MITM）：未核实。如果是 h1，尸体的代价是 120 s，而不是 20–50 s。
- macOS 内核里，已写出但未确认的数据要重传多久才放弃：未核实。经过 Surge 时这个问题不成立（对端是回环）。

## Recommendation

只做 (d)，把 Go 的 #29308 规则搬回来：
- 共享 HTTP 客户端改成一个可替换的句柄。`Client::send_payload` 每次发送前比较上次活动以来的挂钟时间和 `Instant` 时间。若挂钟超过池的闲置上限（90 s，和 builder 共用一个新常量），而 `Instant` 没超过，说明进程睡过：先换一个新客户端（新池，并重读系统代理），再发请求。
- 超时、重试、梯子都不动（I-02、X-84）。不加配置项。
- 在 DIVERGENCES 的移植回归里记一笔：「恢复 Go 的挂钟闲置规则」。
- (a)、(b)、(c)、(e)、(f) 都不做。所有者提交一次带 `/debug` 的复现后，如果坐实是 H4，再单独评估 (a)。

## Tests

全部放在 `tests/provider/wire.rs`。死连接用确定的方式造：一个进程内 TCP 中继，`freeze()` 之后吞掉**已有**连接的双向字节、不关 socket，新连接照常放行，并对外报告累计接受的连接数（§1.3 的实验就是这个夹具的原型）。源站用 hyper 的 h1 / h2c 服务。头超时通过 `with_header_timeout` 缩到亚秒级。

- `a_dead_pooled_h1_connection_costs_one_attempt`：钉住现状。冻结后第一次尝试是 `HeaderTimeout`，第二次走新连接成功，中继累计 2 条连接。
- `a_dead_pooled_h2_connection_is_closed_by_its_ping`：钉住现状（h2c prior knowledge，PING 缩短）。以 `Transport`（`KeepAliveTimedOut`）失败，随后走新连接。
- `the_h2_ping_gives_up_before_the_header_timeout`：常量断言 `HTTP2_KEEP_ALIVE_INTERVAL + HTTP2_KEEP_ALIVE_TIMEOUT < HEADER_TIMEOUT`。§1.3 第 3 行证明，反过来就会在尸体上一直超时。
- `a_suspend_gap_sends_on_a_fresh_pool`：新行为。注入的时钟让挂钟跳过 8 小时而 `Instant` 不动，冻结中继后发请求。断言零超时、立即 200、中继累计 2 条。旧代码上它会变红（等满头超时）。
- `no_gap_keeps_the_pool`：挂钟和 `Instant` 同步前进 89 s，请求复用原连接，中继累计 1 条，防止换池换得过勤。
- `a_wall_clock_set_forward_only_costs_a_new_pool`：挂钟往前拨，请求照常成功，只是多了一条连接。
