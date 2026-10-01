# 请求挂住时 ESC / Ctrl+C 无法取消 —— 勘察

> 2026-10-01 · 分支 `req-cancel`（起点 `5317eb4`）· 只读勘察，未改代码
> 被查的报告：远程 provider 的请求一直不返回时，面板停在 `Waiting for the model`，按 ESC 和 Ctrl+C 都取消不了。

## 结论

**按原样描述（停在 `Waiting for the model` 时 ESC/Ctrl+C 不起作用）：不存在。** 实测「只 accept 不回包」和「回了 200 头后不发 body」两种黑洞服务器，两种按键都在约 8 ms 内显示 `Interrupted.`，服务器同时看到对端关闭 TCP 连接。

**但有两个相邻缺陷确认存在，很可能就是用户实际撞到的：**

- **A. 「取消不了」的真实窗口（确认存在）：** 标题生成请求不挂在 turn 的取消 scope 下，只靠 30 s 超时兜底（`src/repl/run.rs:872-881`）。用户按 ESC 取消之后紧接着发下一条消息，或连按 Ctrl+C 退出，主循环会先 `join_title().await`（`src/repl/run.rs:580`、`:560`）。这段时间 UI 的 cancel 栈是空的，ESC/Ctrl+C 不起任何作用，消息不回显、也没有 busy 行。实测：下一条消息干等 30.0 s 才发出；退出要等 25.5 s。
- **B. 「一直不返回」本身（确认存在）：** 没有空闲/停滞超时。响应头只有 120 s 的超时，而且客户端层重试 2 次，turn 层又重试 10 次，理论上要约 67 分钟才放弃（见 §4）。响应头到达之后，body 读取没有任何超时，连接活着但一个字节都不来时会永远等下去（`src/llm/client.rs:523-532`、`src/llm/sse.rs:160-164`）。这两种情况下用户都只看到 `Waiting for the model`。如果不按 ESC，状态就会一直停在那里。

一句话根因：**取消链路本身是通的。问题出在 (1) 标题请求游离在取消 scope 之外，主循环在下一次输入或退出前会无条件地 await 它；(2) 传输层没有停滞超时，状态行永远显示 `Waiting for the model`。** 两者叠加后，用户的感受就是「挂住了、取消不了」。

---

## 1. 请求路径

| 环节 | 坐标 | 说明 |
|---|---|---|
| reqwest 依赖 | `Cargo.toml:65` | `default-features=false`，features `json, stream, http2, system-proxy, rustls`。HTTPS 端点会经 ALPN 协商到 h2 |
| 构造 client | `src/llm/client.rs:523-532` `default_http_client()` | `reqwest::Client::builder().build()`，不设任何选项。注释写明 *NO whole-request timeout, NO read timeout* |
| reqwest 默认值（查证） | `reqwest-0.13.4/src/async_impl/client.rs:299,313-314`、文档 `:1436-1470` | `connect_timeout/read_timeout/timeout` 都是 `None`（"Default is no timeout"）。`tcp_keepalive` 为 15 s × 3 次（`:303-306`），只能探测对端是否失联，探测不到「连接活着但不发数据」 |
| 唯一的超时 | `src/llm/client.rs:29` `HEADER_TIMEOUT = 120s`；`:300` `tokio::time::timeout(header_timeout, http.execute(req))` | 只覆盖到**响应头到达**为止，覆盖 DNS、连接、TLS、上传和等头 |
| 流式入口 | `src/llm/client.rs:234-244` `stream()` → `Sse::new(resp.bytes_stream(), cancel)` | 所有对话 provider 走流式：openai `src/provider/openai.rs:311-331` → `src/llm/chatcomp.rs:348-361,377-378`。anthropic、responses、google 同构（`src/llm/anthropic.rs:575,613`、`responses.rs:550,572`、`google.rs:80,485`） |
| 读流 | `src/llm/sse.rs:148-174` `read_line()` | 每个 chunk：`select!{ biased; cancel.cancelled() => Err(Cancelled), body.next() }`。**body 读取没有超时** |
| 非流式 | `src/llm/client.rs:207-222` `do_json` | 头部同上；`resp.bytes()` 与 cancel 一起放在 select 里，同样没有 body 超时。标题生成（`provider.chat`）、无工具的 image provider（`src/repl/turn/mod.rs:388-408`）走这条 |

判断：
- 连接超时：**不存在**（被 120 s 头超时间接兜住）
- 整体超时：**不存在**（`client.rs:28` 写明是设计决定，POLICY I-02）
- 空闲/停滞超时：**不存在**。头到达后 body 停滞会**无限等待**（查证：代码中没有，reqwest 默认也没有）

## 2. 取消链路

| 环节 | 坐标 |
|---|---|
| turn token 生成，交给 UI | `src/repl/turn/mod.rs:260-261`：`cancel = parent_cancel.child_token()`；`ui.start_stream(cancel.clone())` |
| 压入 UI 的 cancel 栈 | `src/ui/runtime/handle.rs:316-318` 发 `UiMsg::ScopePush`；`src/ui/runtime/event_loop.rs:479-482` `self.cancels.push(token)` |
| Ctrl+C / Ctrl+D | `src/ui/input/keys.rs:35-42`：栈非空 → `fire_cancel(0)`（turn）；栈空 → `fail_waiter_interrupted()`（只有主循环正停在 `read_input` 时才有 waiter） |
| ESC | `src/ui/input/keys.rs:46-49`：栈非空 → `fire_cancel(top)`；栈空 → 落到编辑键集合，**什么也不做** |
| 触发 | `src/ui/runtime/event_loop.rs:665-672` `fire_cancel`：对 `i..` 的每个 token 调用 `.cancel()` |
| UI 线程 | `src/ui/runtime/handle.rs:138-140`：事件循环跑在**独立的 OS 线程**上，不受 tokio 侧任何 `.await` 阻塞，键始终能读到 |
| 等头阶段 | `src/llm/client.rs:287-305`：`select!{ biased; cancel.cancelled() => return Err(Cancelled), timeout(execute) }` |
| 读 body 阶段 | `src/llm/sse.rs:160-164`（流式）、`client.rs:216-220`（非流式）、`client.rs:448-452`（错误体） |
| 重试退避阶段 | `client.rs:334-338`、`src/repl/turn/retry.rs:52-61`、`src/repl/turn/mod.rs:369-373`：都是 cancel 优先的 select |
| SIGINT（headless） | `src/cmd/signals.rs:17-24` 把 SIGINT/SIGTERM 接到 root token；交互模式下 `:28-33` 关掉 SIGINT 半边，Ctrl+C 交给 raw mode 的键路由 |

**`select!` 里被取消的 future 被 drop 后，TCP 会不会真的断开？（查证，不是推测）**

- HTTP/1.1，等头阶段：drop `execute` 的 future 会 drop hyper 的回调接收端。`hyper-1.11.1/src/proto/h1/dispatch.rs:751-758` `poll_ready` 发现 `cb.poll_canceled` 就绪（"callback receiver has dropped"）后返回 `Err`，dispatcher 结束，连接关闭。
- HTTP/1.1，读 body 阶段：drop `bytes_stream` 会 drop body 接收端。`dispatch.rs:239-243` 走 "body receiver dropped before eof, draining or closing" → `poll_drain_or_close_read`。服务器不发数据时不可能 drain 完，所以连接会被关闭。
- HTTP/2（HTTPS 端点的常态）：drop 流后，`h2-0.4.19/src/proto/streams/streams.rs:1686-1704` `maybe_cancel` 会调度 `RST_STREAM(CANCEL)`。**单个流被取消，TCP 连接留在池里**（`pool_idle_timeout` 90 s）。对服务端来说这次请求已经取消，对 iota 来说 future 已经结束。这一点是读代码得出的，没有对 h2 实测。
- **实测（HTTP/1.1 明文）：** ESC/Ctrl+C 后 8–9 ms，服务器 `recv()` 返回 0，即收到 FIN（§5 日志）。

判断：**取消链路完整，被 drop 的 future 会真的结束请求。** 「按键读不到」和「select 里漏了读流的 future」都不存在。

## 3. 谁在等：`Waiting for the model` 的设与清

| 动作 | 坐标 |
|---|---|
| 文案 | `src/repl/turn/phases.rs:14` `PHASE_WAITING` |
| 设置 | `phases.rs:88-107` `watch_phases`：开始时设一次（`:105`）。`on_sent` 在**响应头到达或本次尝试失败时**再设回 `Waiting`（`:103`，上传超过 128 KiB 时中间会切到 `Sending request`） |
| 清除 | 收到第一段正文、思考内容或工具调用时：`src/repl/turn/mod.rs:834,849,900`；轮结束时：`:792-794`、`:409`；`(ESC to cancel)` 后缀由 UI 自己追加（`src/ui/facade.rs:899-901`） |
| 是否阻塞输入 | 否。主循环 task 在 `await` provider，但键由独立 UI 线程读取，`fire_cancel` 直接调用 `token.cancel()` |

推论：**头到达后 body 停滞时，状态行仍然是 `Waiting for the model`**（`on_sent` 把它设回来，正文没来就没人清）。所以对用户来说，「连不上」「等头」「头来了但 body 不来」三种情况看起来完全一样。

**空 cancel 栈的盲区（缺陷 A）：**

| 环节 | 坐标 |
|---|---|
| 标题任务 | `src/repl/run.rs:863-885` `title_now`：`tokio::spawn`，token 是 `handles.cancel.child_token()`（root 的子 token，**不在 UI cancel 栈上**），外面包了 `TITLE_TIMEOUT = 30s`（`src/repl/title.rs:32`） |
| 下一条输入前 join | `src/repl/run.rs:577-581`：除了 `/debug /status /tools /skills` 这几个只读查看命令，**任何输入都会先 `join_title().await`**（`src/repl/state.rs:119-123`，没有 select，也没有 cancel） |
| 退出前 join | `src/repl/run.rs:557-561`：`read_input` 返回 Err（空闲 Ctrl+C/Ctrl+D）后 `join_title().await` 再 break |
| 每轮重新发起 | 被中断的 turn 会「把会话名交回去」（`run.rs:888-893` 注释），下一条消息又会 spawn 一个新的标题请求（§5 实测第二轮又出现一对连接） |

在这段 join 期间：没有 busy 行、刚输入的消息还没回显、cancel 栈为空，ESC 落到编辑键集合，Ctrl+C 找不到 waiter，**两个键都没有任何效果**，最长持续 30 s。

## 4. 已有防护

| 机制 | 坐标 | 对「挂住」的效果 |
|---|---|---|
| 响应头超时 120 s | `client.rs:29,300-303` | 只防「头不来」 |
| 客户端重试 | `client.rs:27` `DEFAULT_RETRIES = 2`；`:327-339`；`should_retry` `:466-482` 把 `Transport` 和 `HeaderTimeout` 都算作可重试 | 头超时会再试 2 次，退避 0.5 s、1 s（减抖动） |
| turn 层重试 | `src/repl/turn/retry.rs:14` `MAX_RETRIES = 10`，线性退避 1..10 s（`:18,60`）；`is_retryable` `:79-92` 中 `HeaderTimeout` 落到 `_ => true`；调用点 `src/repl/turn/tools.rs:69` | 每轮再套一次上面的 3 次尝试 |
| 整轮重放 | `src/repl/turn/mod.rs:311-376` | 有工具的 turn（`used_tools`）不重放（`:348-351`）。无工具时仍按 `MAX_RETRIES` 重放 |
| `/debug` 请求日志 | `src/llm/client.rs:277-281,295-297,311`；`src/llm/reqlog.rs:86` | 开启后每次尝试都有一行，挂住的请求显示为 pending（`…`），取消后记为 cancelled，**可以用来诊断**，但默认关闭（`reqlog.rs:151`） |
| 心跳 / 空闲超时 | — | **不存在** |

「连上了但一直不返回」时的耗时（推算）：
- 头不来：单轮 = 3 × 120 s + 约 1.3 s 退避 ≈ 361 s；turn 层 11 轮 + Σ1..10 s = 55 s → **约 4030 s ≈ 67 分钟**才报错。期间状态行大部分时间是 `Waiting for the model`，只在每轮之间短暂显示 `Network error — retrying (attempt n/10)`。客户端层的 3 × 120 s 由 §5 第 6 项实测确认（361 s 报错）。交互模式下 turn 层的 10 轮是**读代码推算**的，没有实测满 67 分钟。
- 头来了 body 不来：**永远**，任何重试都不会触发。

## 5. 复现（不联网）

黑洞服务器 `blackhole.py PORT MODE`：`silent` 只 accept、读完请求后不回包；`headers` 回 `200 text/event-stream` 头后不再发送。日志记录 accept 时刻和对端关闭的时刻：

```python
import socket, sys, threading, time
port=int(sys.argv[1]); mode=sys.argv[2]
s=socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR,1); s.bind(("127.0.0.1",port)); s.listen(16)
def h(c,a):
    t0=time.time(); buf=b""; print(f"{t0:.3f} accept {a}",flush=True)
    while b"\r\n\r\n" not in buf:
        d=c.recv(65536)
        if not d: break
        buf+=d
    if mode=="headers":
        c.sendall(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n")
    while c.recv(65536): pass
    print(f"{time.time():.3f} closed-by-peer {a} after {time.time()-t0:.1f}s",flush=True)
while True:
    c,a=s.accept(); threading.Thread(target=h,args=(c,a),daemon=True).start()
```

scratch 配置 `silent.yml`（`headers.yml` 只是把端口换掉）：

```yaml
providers:
  hole: {type: openai, key: x, url: "http://127.0.0.1:18781/v1"}
models:
  m: hole:test-model
agents:
  default: {model: m}
```

启动方式（隔离 HOME，去掉 `HERDR_*` 以免给宿主 pane 推状态）：

```sh
python3 blackhole.py 18781 silent > silent.log &
tmux new-session -d -s rc -x 120 -y 30 \
  "env -u HERDR_ENV -u HERDR_PANE_ID -u HERDR_SOCKET_PATH -u HERDR_TAB_ID -u HERDR_WORKSPACE_ID -u HERDR_BIN_PATH \
   HOME=$PWD/home target/debug/iota -c silent.yml --no-save"
tmux send-keys -t rc hello Enter
```

### 实测结果（debug 构建，v0.5.2 @ `5317eb4`）

| # | 场景 | 操作 | 现象 | 判断 |
|---|---|---|---|---|
| 1 | silent | `hello` → 约 10 s 后按 ESC | 状态行 `⠙ Waiting for the model  4s (ESC to cancel)`；按 ESC 后显示 `Interrupted.`。服务器：按键 `…764.304` → `closed-by-peer … after 9.6s` @ `…764.312`（**8 ms**）。另一条连接（标题请求）在 `after 30.0s` 才关闭 | ESC 有效 |
| 2 | silent | 第二轮 `second` 在 `Waiting` 时按 Ctrl+C | 按键 `…855.250` → `closed-by-peer … after 18.6s` @ `…855.258`（**8 ms**），显示 `Interrupted.` | Ctrl+C 有效 |
| 3 | headers | `hello` → 6 s 后按 ESC | 头已到，状态行**仍是** `Waiting for the model  5s`；按 ESC 后 `…875.010` → `closed-by-peer … after 6.0s` @ `…875.019`（**9 ms**） | 头后停滞时 ESC 也有效；状态行无法区分是否已收到头 |
| 4 | silent | `hello` → ESC → 1 s 后输入 `second` 回车 → 再按 ESC、Ctrl+C | 3 s 后：`second` **没有回显**，没有 busy 行，看起来是空闲的输入框；ESC/Ctrl+C 没有反应。直到标题请求 `after 30.0s` 超时（`…836.686`），`second` 才回显、发出新请求（`…836.687/688` 两次 accept），进入 `Waiting for the model` | **缺陷 A 确认** |
| 5 | silent | `hello` → ESC → 连按两次 Ctrl+C 退出 | 按键 `…957` → 进程退出 `EXITED-AT-…983`（**约 25.5 s**），即标题请求发出后正好 30 s | **缺陷 A 确认**（退出路径） |
| 6 | silent，headless `iota -c silent2.yml -m hello`（无工具，走 unary） | 不按键 | 3 次连接：`…328.768` 连上 → `after 120.0s` 关闭 → 0.41 s 后重连 → `after 120.0s` → 0.82 s 后重连 → `after 120.0s`；在约 361 s 时报错 `Error: chat error: response headers not received within 2m0s`，exit 1。之后没有更多连接 | 客户端层 120 s × 3 次已实测确认；headless 不经过 turn 层的 `retry_round` |

注：headless 在 `--no-save` 时直接拒绝（`flag --no-save is not supported in headless mode`），所以第 6 项不带这个参数，并用隔离的 HOME 运行。

### 最小复现

- **缺陷 A**（取消不了）：起 `silent` 黑洞 → iota 指向它 → 输入 `hello` → ESC（立即生效）→ **马上再输入任意一句回车** → 观察：约 30 s 内既没有回显也没有 busy 行，ESC/Ctrl+C 都不起作用。也可以在 ESC 后连按 Ctrl+C 两次：要过约 30 s 进程才退出。
- **缺陷 B**（永不返回）：起 `headers` 黑洞 → 输入 `hello` → 不按键 → `Waiting for the model` 的计时会一直走下去，不会出错也不会重试（`silent` 黑洞则在 120 s、241 s……处重连，约 67 分钟后才报错）。

## 汇总

| # | 问题 | 判定 | 依据 |
|---|---|---|---|
| 1 | 有没有超时 | 连接/整体/空闲：**不存在**；响应头：120 s | `client.rs:29,523-532`；reqwest `client.rs:299,313-314` |
| 2 | 取消链路 / drop 是否断连 | **链路完整**；h1 断开 TCP（查证 + 实测），h2 发 RST_STREAM（查证，未实测） | `keys.rs:35-49`、`event_loop.rs:665-672`、`client.rs:287-305`、`sse.rs:160-164`；hyper `dispatch.rs:239-243,751-758`；h2 `streams.rs:1686-1704` |
| 3 | `Waiting for the model` 时 ESC 能否读到 | **能**（UI 是独立线程）；但空 cancel 栈期间 ESC/Ctrl+C 无效 | `handle.rs:138`；`run.rs:557-561,577-581` |
| 4 | 对挂住的防护 | 头超时 + 两层重试 ≈ 67 分钟；body 停滞**无防护** | `client.rs:327-339,466-482`；`retry.rs:14,79-92` |
| 原报告 | 「停在 Waiting 时 ESC/Ctrl+C 无法取消」 | **按字面不存在**；**相邻缺陷 A（30 s 无法取消的盲区）、B（无停滞超时、无限 Waiting）确认存在** | §5 实测 1–5 |

修复方向（仅供参考，不在本次范围内）：给 `join_title` 加 select（root/turn cancel，或不等它直接 abort），或把标题任务的取消挂到 turn scope 上；给流式 body 加空闲超时（例如每个 chunk 用 `tokio::time::timeout` 包一层，或者用 reqwest 的 `read_timeout`），超时后归入 `Transport` 交给已有的重试处理；状态行在 `on_sent`（头已到）后改用另一个文案，比如 `Waiting for the first token`，以便区分卡在哪个阶段。
