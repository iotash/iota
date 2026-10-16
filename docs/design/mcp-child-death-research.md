# stdio MCP 子进程的管理与死亡：诊断

Status: **Research**（决定与实现见 §6） · 日期：2026-10-15 · 基于 `main` = `5fde2bf`（v0.7.2），rmcp 3.1.4，process-wrap 9.1.0

本文只做诊断，不含设计决定，不改代码。所有者的原话：「command 类型的 mcp，是如何管理进程的，如果这个进程被我杀掉了会发生什么」。

## 0. 结论

**子进程被杀之后，iota 不知道它死了。** rmcp 的读任务在管道 EOF 时静默退出、把子进程回收掉（不留僵尸），此后对它的每一次调用都**立即**返回 `Error calling tool: Transport closed`——不挂、不超时、不重连；但管理器状态一直是 `Connected`，`/tools` 的 MCP 页照样显示 `[connected]` 和原来的工具数，**模型每一轮仍然被提供这些已不可能工作的工具**，用户看不到任何 notice，整个会话内没有任何办法把它连回来（`Manager::reconnect` 在生产代码里没有调用者）。

有一种情形会**永远挂住**：被杀的进程不是管道的唯一持有者（例如 `sh -c` 包装、启动器的子进程），读端拿不到 EOF，调用没有超时，只能 ESC。

另有两处进程清理缺口：iota 关闭时只杀**直接子进程**，孙进程会成孤儿；iota 被 SIGKILL 时，不理会 stdin EOF 的服务器会成孤儿。

## 1. 进程管理地图

行号对 `5fde2bf`；rmcp 路径相对 `~/.cargo/registry/src/index.crates.io-*/rmcp-3.1.4/`。

### 1.1 怎么起

| 事项 | 实际行为 | 位置 |
|---|---|---|
| 命令 | `tokio::process::Command::new(command).args(args).envs(env)`；`env` 是**追加**到 iota 自己的环境上（继承全部父环境），值先经 `expand_server_config` 展开 | `src/mcp/transport.rs:370-371`、`:168` |
| cwd | 不设，继承 iota 的 cwd | `src/mcp/transport.rs:370-371`（无 `current_dir`） |
| stdin/stdout | rmcp builder 默认 `piped` | rmcp `src/transport/child_process.rs:124-131` |
| stderr | iota 改成 `piped`（rmcp 默认 `inherit`） | `src/mcp/transport.rs:372-375` |
| stderr 捕获 | 一个 tokio 任务读到 EOF 为止，存进 64 KiB 上限的缓冲 | `src/mcp/transport.rs:376-391`、`src/mcp/manager.rs:37` |
| stderr 何时被读 | **只在握手失败时**拼进错误；运行期间从不展示 | `src/mcp/transport.rs:199-210` |
| 进程组 / kill_on_drop | **都没有**：`CommandWrap::from(Command)` 不带任何 wrapper，子进程与 iota 同一进程组 | process-wrap `src/generic_wrap.rs:169-176`；`src/mcp` 中 grep `process_group`/`kill_on_drop` 零命中 |
| 对照 | shell 工具用 `process_group(0)` + `killpg(SIGKILL)` 杀整棵树 | `src/shell/exec.rs:274-280` |
| 起的时机 | 交互：`connect_background`，每个服务器一个任务，立即返回；headless：`connect_all` 等全部结束；每个都套 30 s `timeout` 与 run 的取消令牌 | `src/mcp/manager.rs:398-415`、`:347-387`、`:424-430`、`:34` |

### 1.2 握手（initialize）失败

- `client_info(opts).serve(transport)` 失败 → 先等 stderr 管道到 EOF（≤ 2 s，`STDERR_DRAIN_GRACE`），再报 `McpError::Connect(e + "\n  subprocess stderr:\n…")`：`src/mcp/transport.rs:197-210`、`:29`、`:141-146`。
- `tools/list` 失败 → `running.cancel()`（≤ 10 s）后报 `McpError::ListTools`：`src/mcp/transport.rs:334-343`。
- 超时 → `McpError::Timeout`；取消 → `Connect("interrupted")`：`src/mcp/manager.rs:424-446`。
- 结果经 `merge_result` 写成 `ServerState::Failed(text)`，不进工具表：`src/mcp/manager.rs:687-690`；交互模式下经 `map_events` 变成转录里的 `⚠ MCP <name> failed: …`：`src/cmd/interactive/mod.rs:829-842`。
- 握手期间读到 EOF 是 `ClientInitializeError::ConnectionClosed("initialize response")`（rmcp `src/service/client.rs:158`），即 §2.4 看到的 `connection closed: initialize response`。
- 握手失败时子进程：`serve_client_with_ct_inner`（rmcp `src/service/client.rs:749` 起）以 `Err` 提前返回，局部持有的 `TokioChildProcess` 被丢弃，`ChildWithCleanup::drop` 起一个任务 `kill()` 它：rmcp `src/transport/child_process.rs:45-57`。实测进程表里没有残留（§2.4、§2.5）。

### 1.3 谁持有子进程句柄，有没有人 `wait()`

- 句柄在 `TokioChildProcess.child: ChildWithCleanup` 里（rmcp `child_process.rs:36-43`），整个 transport 被**移进 rmcp 的服务循环任务**。iota 手里只有 `RunningService`（锁在 `RmcpSession.running` 里）和一个克隆的 `Peer`：`src/mcp/transport.rs:52-67`。
- **iota 没有任何代码监控子进程或连接的存活**：`src` 中 grep `is_transport_closed`、`is_closed()`、`.waiting()`、`QuitReason` 在 MCP 路径上零命中。
- `child.wait()` 只出现在 rmcp 的 `graceful_shutdown`（`child_process.rs:81-107`），而它只在服务循环结束时由 `transport.close()` 调到（rmcp `src/service.rs:1759`）。所以子进程退出后的回收是**间接**发生的：EOF → 服务循环退出 → `close()` → `wait()`。没有任何一步把消息传回 iota 的管理器。
- `Manager::reconnect`（`src/mcp/manager.rs:515-529`）唯一的调用者是 `Manager::login`（`:609`），而 `Manager::login`/`logout` 在生产代码里**没有调用者**——`/mcp` 在 X-42 被移除，`McpHooks::manager` 只是留给未来的 config toolset（`src/repl/run.rs:96-100`；`docs/DIVERGENCES.md` X-42）。

### 1.4 读端在子进程死时收到什么

1. 子进程死 → stdout 管道写端全部关闭 → `AsyncRwTransport::receive` 的 `read_until` 返回 `Ok(0)` → `return None`：rmcp `src/transport/async_rw.rs:125-140`。
2. 服务循环 `transport.receive()` 得到 `None` → `break QuitReason::Closed`（日志 `input stream terminated`，tracing 级别，发布版看不到）：rmcp `src/service.rs:1395-1402`。
3. 排空在途响应 ≤ 5 s，然后 `transport.close()` → `graceful_shutdown`：关 stdin、`wait()`（子进程已死，立即返回，进程被回收）：`service.rs:1727-1762`、`child_process.rs:81-107`。
4. 循环任务结束，`local_responder_pool`（`service.rs:1345`）被丢弃 → 在途请求的 oneshot 收到关闭 → `ServiceError::TransportClosed`（`service.rs:555`、`:566`）；之后的新请求在 `self.tx.send(...)` 处失败 → `TransportClosed`（`service.rs:905-921`）。其 Display 是 `Transport closed`（`service.rs:84-85`）。
5. iota 侧：`McpError::Call(e.to_string())`（`src/mcp/transport.rs:108`）→ `ToolError::Transport`（`src/mcp/manager.rs:780`）→ 加前缀 `Error calling tool: `（`src/tool/mod.rs:63`），作为 `is_error` 的工具结果交给模型。**状态不动**：`call_tool` 的错误路径不碰 `state`（`src/mcp/manager.rs:754-782`）。
6. 调用本身**没有超时**：`call_tool_once` 走 `send_request` → `PeerRequestOptions::no_options()`（`timeout: None`），只与取消令牌 `select!`：rmcp `src/service/client.rs:1346-1356`、`src/service.rs:771-773`、`:835-840`；`src/mcp/transport.rs:94-97`。读端拿不到 EOF 时就一直等。

### 1.5 iota 退出时子进程怎么办

所有退出路径都 `manager.close()`：`src/cmd/interactive/mod.rs:336`、`:399`；`src/cmd/mod.rs:687`。`Manager::close` 取出所有 session，各自 `timeout(10 s, session.close())`（`src/mcp/manager.rs:662-675`）→ `RmcpSession::close` → `running.cancel()`（`src/mcp/transport.rs:115-122`）→ 服务循环 `QuitReason::Cancelled` → 排空 ≤ 2 s → `graceful_shutdown`：关 stdin，等 3 s，到点 `kill()` **直接子进程**（`child_process.rs:10`、`:81-107`）。

| 退出方式 | 机制 | 实测（§2.6） |
|---|---|---|
| Ctrl+D / Ctrl+C | 正常收尾（raw 模式下 Ctrl+C 是按键，`src/cmd/signals.rs:4-6`） | 健康服务器 ~1 s 退出；不理 EOF 的服务器 ~4 s（3 s 后被 SIGKILL），都不留 |
| SIGTERM | 取消根令牌，走同一收尾（`src/cmd/signals.rs:1-7`），退出码 130 | 同上 |
| SIGKILL | 没有收尾；内核关闭 iota 的 fd，子进程 stdin 得 EOF | 健康服务器自己退出；**不理 EOF 的服务器成孤儿（ppid 1）** |
| 任何收尾方式 + 孙进程 | `kill()` 只发给直接子进程，无进程组 | **孙进程成孤儿（ppid 1）** |
| 终端/tmux 关闭 | 子进程与 iota 同一前台进程组，pty 挂断的 SIGHUP 一并送达 | 首次实测时 tmux 会话结束把 `sh` 与 `sleep` 一起带走（§2.6 的注） |

## 2. 外部杀掉子进程之后：真机复现

### 2.0 环境

- 二进制：本 worktree `cargo build` 的 `target/debug/iota`（v0.7.2）。
- 私有 tmux 服务器 `tmux -L mcpdeath`，`HOME` 指向 scratch 目录（配置、会话都不碰真实 HOME），模型是本地假 provider，不联网。
- 交互会话跑在 `bash -c 'iota; echo IOTA-EXIT=$?; sleep 99999'` 里，让 pty 在 iota 退出后仍然活着（否则 tmux 结束的 SIGHUP 会混淆孤儿测试）。

mock MCP 服务器 `mock_mcp.py`（行分隔 JSON-RPC；`echo` 立即返回，`sleep` 睡 N 秒；环境变量控制在 `initialize`/`tools/list` 处卡住、写 pid 文件、忽略 stdin EOF）：

```python
#!/usr/bin/env python3
import json, os, sys, time

pidfile = os.environ.get("MOCK_PIDFILE")
if pidfile:
    with open(pidfile, "w") as f:
        f.write(str(os.getpid()))
sys.stderr.write(f"mock_mcp: started pid={os.getpid()}\n"); sys.stderr.flush()

TOOLS = [
    {"name": "echo", "description": "echo back text",
     "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}},
    {"name": "sleep", "description": "sleep N seconds then answer",
     "inputSchema": {"type": "object", "properties": {"seconds": {"type": "number"}}}},
]

def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n"); sys.stdout.flush()

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    mid, method = msg.get("id"), msg.get("method")
    sys.stderr.write(f"mock_mcp: <- {method}\n"); sys.stderr.flush()
    if mid is None:
        continue  # notification
    if method == "initialize":
        time.sleep(float(os.environ.get("MOCK_SLOW_INIT", "0")))
        send({"jsonrpc": "2.0", "id": mid, "result": {
            "protocolVersion": msg["params"].get("protocolVersion", "2025-06-18"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "mock", "version": "1.0.0"}}})
    elif method == "tools/list":
        time.sleep(float(os.environ.get("MOCK_SLOW_LIST", "0")))
        send({"jsonrpc": "2.0", "id": mid, "result": {"tools": TOOLS}})
    elif method == "tools/call":
        name = msg["params"]["name"]; args = msg["params"].get("arguments") or {}
        if name == "sleep":
            time.sleep(float(args.get("seconds", 30)))
            text = f"slept {args.get('seconds', 30)}"
        else:
            text = f"echo:{args.get('text', '')} pid={os.getpid()}"
        send({"jsonrpc": "2.0", "id": mid, "result": {"content": [{"type": "text", "text": text}]}})
    elif method == "ping":
        send({"jsonrpc": "2.0", "id": mid, "result": {}})
    else:
        send({"jsonrpc": "2.0", "id": mid, "error": {"code": -32601, "message": "no such method"}})

if os.environ.get("MOCK_IGNORE_EOF"):  # silent: a write to a dead stderr pipe would SIGPIPE us
    while True:
        time.sleep(3600)
sys.stderr.write("mock_mcp: stdin EOF\n"); sys.stderr.flush()
```

假模型 `mock_llm.py`：OpenAI chat-completions 流式接口；用户消息 `call:<wire 工具名> <json>` → 发一个该工具的调用；最后一条是工具结果 → 回 `ran: <结果>`；**每个请求里提供给模型的 `mcp__*` 工具名记进日志**——这是判断「模型是否还被提供死工具」的证据。

配置（`$HOME/.iota.yaml`，`$S` 为 scratch 目录）：

```yaml
providers:
  mock: {type: openai, key: test, url: "http://127.0.0.1:18931"}
models:
  m: mock:fake
agents:
  default: {model: m}
mcp_servers:
  dead:
    command: python3
    args: ["$S/bin/mock_mcp.py"]
    env: {MOCK_PIDFILE: "$S/dead.pid"}
```

杀进程一律是 `kill $(cat $S/dead.pid)`（SIGTERM；python 默认动作即退出）。

### 2.1 基线（活着时）

```
❯ call:mcp__dead__echo {"text":"alive"}
[dead:echo text:alive]
  ⎿ echo:alive pid=78634
```

`/tools` → MCP：`1 server(s) · 2 tool(s)` / `dead  [connected]` / `tools (2): echo, sleep`。假模型日志：`"mcp_tools": ["mcp__dead__echo", "mcp__dead__sleep"]`。

### 2.2 空闲时杀掉 → 第一次调用 → 再调几次

```
$ kill 78634
--- ps after 1s        # 只剩 tmux 与 iota；ps -o pid,stat -p 78634 为空：已回收，无僵尸
```

杀后 7 s 转录里**没有任何 notice**。`/tools` → MCP 仍是：

```
1 server(s) · 2 tool(s)
dead  [connected]
  tools (2): echo, sleep
```

第一次调用（11:49:32 发出，同一秒内返回）：

```
❯ call:mcp__dead__echo {"text":"after-kill-1"}
[dead:echo text:after-kill-1]
  ⎿ Error calling tool: Transport closed
```

再调两次（`sleep`，各 1 s 内返回），结果相同：`⎿ Error calling tool: Transport closed`。假模型日志，每一轮都还在提供死工具：

```
{"t": "11:49:40", "last_role": "user", "last": "call:mcp__dead__sleep {\"seconds\":1}", "mcp_tools": ["mcp__dead__echo", "mcp__dead__sleep"], "n_tools": 4}
{"t": "11:49:41", "last_role": "tool", "last": "Error calling tool: Transport closed", "mcp_tools": ["mcp__dead__echo", "mcp__dead__sleep"], "n_tools": 4}
{"t": "11:49:43", ...同上}
{"t": "11:49:44", ...同上}
```

进程表里 `mock_mcp` 计数为 0；没有任何重新拉起。

### 2.3 调用进行中杀掉

新起 iota（新子进程 79214），发 `call:mcp__dead__sleep {"seconds":60}`，2 s 后（状态行 `⠦ Running tools (ESC to cancel)`）`kill 79214`，时间 11:50:00.28：

```
t+1s errors_on_screen=2
❯ call:mcp__dead__sleep {"seconds":60}
[dead:sleep seconds:60]
  ⎿ Error calling tool: Transport closed
```

假模型日志里工具结果的请求时间是 `11:50:00`——**杀后 1 s 内**返回错误，没有挂。状态与 §2.2 相同（仍 `connected`，仍提供工具）。

### 2.4 启动握手期间被杀

`MOCK_SLOW_INIT: "20"`，启动 3 s 后杀：

```
⚠ MCP dead failed: connect failed: connection closed: initialize response
```

`/tools`：`1 server(s) · 0 tool(s)` / `dead  [disconnected]` / `error: connect failed: connection closed: initialize response`。随后调用 → `Error calling tool: unknown tool: mcp__dead__echo`；假模型日志 `"mcp_tools": [], "n_tools": 2`——**没有**再提供死工具。进程表无残留。

### 2.5 `tools/list` 期间被杀

`MOCK_SLOW_LIST: "20"`，启动 3 s 后杀：

```
⚠ MCP dead failed: list tools: Transport closed
```

`/tools`：`dead  [disconnected]` / `tools: (none)` / `error: list tools: Transport closed`。进程表无残留。

### 2.6 读端拿不到 EOF：永远挂住

服务器经 `sh -c` 包装（`command: sh`，`args: ["-c", "python3 …/mock_mcp.py; sleep 600"]`），只杀 python（79968），`sh` 与随后的 `sleep 600` 仍持有管道：

```
79967 79954 S+   sh -c python3 …/mock_mcp.py; sleep 600
79968 79967 S+   …/Python …/mock_mcp.py
kill python 79968 (sh wrapper survives) at 11:51:11
```

11:51:12 发出 `call:mcp__dead__echo {"text":"wrapped"}`，30 s 后与 2 分 10 s 后：

```
  fake · ≈0% / 128k · ⠇ Running tools  29s (ESC to cancel)
  fake · ≈0% / 128k · ⠸ Running tools  2m 10s (ESC to cancel)
```

ESC 后 `⎿ Error calling tool: interrupted`；下一次调用同样挂住（10 s 时 `Running tools  9s`）。没有任何超时。

### 2.7 退出矩阵

脚本：起 iota → 用 Ctrl+D / Ctrl+C（tmux send-keys）/ `kill -TERM` / `kill -KILL` 结束 iota → 1 s 后查服务器 pid。

```
== healthy: how=ctrl-d     iota gone after ~1s   IOTA-EXIT=0    server exited
== healthy: how=ctrl-c     iota gone after ~1s   IOTA-EXIT=0    server exited
== healthy: how=sigterm    iota gone after ~1s   IOTA-EXIT=130  server exited
== healthy: how=sigkill    iota gone after ~1s   IOTA-EXIT=137  server exited
== ignore-eof: how=ctrl-d  iota gone after ~4s   IOTA-EXIT=0    server exited
== ignore-eof: how=ctrl-c  iota gone after ~4s   IOTA-EXIT=0    server exited
== ignore-eof: how=sigterm iota gone after ~3s   IOTA-EXIT=130  server exited
== ignore-eof: how=sigkill iota gone after ~1s   IOTA-EXIT=137  SERVER STILL ALIVE: 82725  1  S+  …/Python
```

孙进程（§2.6 的配置，python 已被杀，`sleep 600` 是 `sh` 的子进程），Ctrl+D：

```
81451 81439 81437 S+  sh -c python3 …          # 退出前：iota 81439 → sh 81451 → sleep 81504
81504 81451 81437 S+  sleep 600
iota exited after ~3s
--- after exit
81504     1 81437 S+  sleep 600                 # 孤儿，ppid 1
```

注：
- 第一轮 ignore-eof + SIGKILL 显示 `server exited`，是 mock 的假象：它在 EOF 后往已无读端的 stderr 管道写了一行，被 SIGPIPE 杀掉。改成静默后（上面的源码）复测才是 `STILL ALIVE`。
- 第一次测孙进程时 pane 直接跑 iota，iota 退出即 tmux 会话结束，pty 挂断把整组进程（同一进程组）带走，看不到孤儿；之后改为 `bash -c` 套一层。

### 2.8 没测的

- headless（`iota run -m …`）与 bot 模式下的同样情形：没跑。代码路径相同（同一个 `Manager::call_tool`，`src/mcp/manager.rs:754-782`），但没有真机证据；headless 没有 ESC。
- Windows：没测。
- `kill -9` 杀服务器：没单独跑；读端看到的同是管道 EOF（§1.4 第 1 步不区分信号）。

## 3. 别家怎么做

源码快照：openai/codex `bb64b89`、google-gemini/gemini-cli `06d3298`（都是 2026-10-14 的提交）。Claude Code 闭源，出处只有官方文档、CHANGELOG 和 issue。下面几条由撰写者对着原文复核过：CC 文档里「不自动重连 stdio」那句、Codex 的 `is_closed` 与两个默认超时常量、Gemini 的 `onerror` 处理与 10 分钟默认超时。其余出处来自调研 agent，没有逐条复核。

| | 检测死亡 | 死工具是否还给模型 | 自动重连 | 手动重连 | 调用超时（默认） | 退出清理 |
|---|---|---|---|---|---|---|
| **Claude Code** | 有，状态变 failed（issue #41367：「Tools gone, status green. Eventually updates to ✗ failed」；文档没写 stdio 的检测机制） | **撤下**，并告诉模型「their MCP server disconnected」（#41367；CHANGELOG 2.1.267 修过「a tool that disappears mid-conversation」） | **stdio 不重连**。文档原句：「Stdio servers are local processes, and Claude Code doesn't reconnect them automatically」（code.claude.com/docs/en/mcp#automatic-reconnection）；远程服务器重连 5 次，指数退避 | `/mcp` 里的 Reconnect、`/mcp reconnect all` | `MCP_TOOL_TIMEOUT` 100000000 ms（约 28 h）；`MCP_TIMEOUT` 管启动，30 s；每服务器可配 `timeout`（code.claude.com/docs/en/env-vars） | macOS/Linux：SIGINT → 100 ms 后 SIGTERM → 400 ms 后 SIGKILL（mcp 文档） |
| **Codex CLI** | 不主动监控；查询时才看 `service.is_closed() \|\| peer().is_transport_closed()`（`codex-rs/rmcp-client/src/rmcp_client.rs:1050`），已关闭报 `Failed`（`codex-mcp/src/rmcp_client/status.rs:32-35`） | **照旧提供**（`codex-mcp/src/rmcp_client.rs:616-628` 直接返回缓存的工具目录）；调用时报 「MCP server '…' is not connected」（`connection_manager.rs:1031-1032`） | stdio 不重连（只有内置的 codex_apps 服务器有 1 s–30 s 退避重连，`rmcp_client.rs:107-108`、`:515`） | `Op::RefreshMcpServers` → `reconnect_on_next_refresh()`（`core/src/session/handlers.rs:238-240`）；app-server 的 `config/mcpServer/reload` | 源码 `DEFAULT_TOOL_TIMEOUT` 300 s、`DEFAULT_STARTUP_TIMEOUT` 30 s（`codex-mcp/src/rmcp_client.rs:104-105`）；文档写的是 60 s / 10 s，与源码不一致 | **进程组** SIGTERM → 2 s 后 SIGKILL（`rmcp-client/src/stdio_server_launcher.rs:218`、`:420-436`） |
| **Gemini CLI** | 部分：`client.onerror` 把状态设为 `DISCONNECTED`（`packages/core/src/tools/mcp-client.ts:208-221`）；`onclose` 不改状态（`:1898-1904`） | **照旧提供**：只有主动 `disconnect()` 才 `removeMcpToolsByServer`（`mcp-client.ts:288-296`） | 不重连 | `/mcp reload`（别名 `refresh`），重启全部服务器（`cli/src/ui/commands/mcpCommand.ts:348-386`）；另有 `restartServer(name)` | 每服务器 `timeout`，默认 10 min（`mcp-client.ts:96`） | 交给 `@modelcontextprotocol/sdk` 的 `transport.close()`；没有进程组代码 |

**判断**：「stdio 死了不自动重启」三家一致，iota 也一样，这一条不算独一份。「死工具照旧给模型」Codex 和 Gemini 也这样，只有 Claude Code 撤下。iota 独有的缺口有两个：
- **没有任何手动重连入口**。三家都有。
- **调用没有超时**。三家都有，最长是 Claude Code 的约 28 h，但总归有一个。

另外，iota 的状态即使在查询时也不会更新。Codex 至少在查询时会看一眼 `is_transport_closed`，而 iota 用的就是同一个 rmcp 原语，却从来没调用过。

拿不准：Claude Code 靠什么检测 stdio 死亡（进程退出还是 transport onclose），查不到。Gemini 的状态取决于 MCP TS SDK 的 stdio transport 在子进程被杀时触发 `onerror` 还是 `onclose`：没核对 SDK 源码，也没实测；如果只触发 `onclose`，它的状态会一直停在 `CONNECTED`，和 iota 一样。

## 4. 结论与提案

### 4.1 是不是 bug

是。错误本身（立即报 `Transport closed`）不算坏，坏在三件事：**状态在说谎**（`[connected]`、工具数照旧）、**模型被持续提供死工具**（每次都要白跑一轮并收到一句看不懂的 `Transport closed`）、**会话内无路可回**（没有 notice、没有重连入口、没有自动重启——只能退出 iota 重开）。§2.6 的永久挂起与 §2.7 的孤儿是另外两个独立缺陷。

### 4.2 按优先级

| # | 问题 | 选项 | 代价 |
|---|---|---|---|
| 1 | 死了不知道 | **a.** 调用返回 `TransportClosed` 时把该服务器记为 `Failed("server exited")`，并在转录里发一条 `⚠ MCP <name> exited`（附 stderr 尾部——缓冲已经在捕获，`transport.rs:376-391`，只是从未展示）。**b.** 主动监视：每个 session 起一个任务等服务循环结束（rmcp `RunningService` 的 `waiting`/`is_closed`，或 `Peer::is_transport_closed`，`service.rs:1042`、`:1079`），死了立即标记并通知。 | a 只在下一次调用时才发现；b 要把 `RunningService` 的所有权从 `close` 的 `Mutex<Option<…>>` 里理顺（`waiting(self)` 消费所有权，`transport.rs:52-57`），多一个常驻任务。 |
| 2 | 模型还在被提供死工具 | **a.** 标记死亡时一并 `detach`（`manager.rs:533-563` 已有现成逻辑），工具从 `tools()` 消失。**b.** 工具留着，但调用直接返回一句可操作的错误（「MCP 服务器 dead 已退出，本会话不可用」）。 | a 改变工具数组 → 下一轮的提示词缓存前缀失效（工具定义在缓存前缀里），并让模型对着历史里调用过、现在不存在的工具；b 不破缓存，但模型仍会尝试调用。 |
| 3 | 会话内无法恢复 | **a.** 下一次调用时惰性重连一次：`TransportClosed` 发生在**发送**阶段（`service.rs:905-921`，请求根本没发出）才自动重连后重试；在途请求（`:555`/`:566`）不重试（可能已产生副作用）。**b.** 自动重启带退避与上限。**c.** 交给 config toolset / 一个手动入口调用现成的 `Manager::reconnect`。 | a 实现量小但要能区分两种 `TransportClosed`（rmcp 两处是同一个变体，只能靠 iota 自己先查 `is_transport_closed`）；b 对崩溃循环的服务器要防刷屏，重启后工具集可能变（又回到 2a 的缓存问题）；c 与 X-42「REPL 不再加管理类斜杠命令」冲突，只能走 config toolset，而它还不存在。 |
| 4 | 读端无 EOF 时永久挂住 | **a.** 每服务器可配 `tool_timeout`，默认给一个大值。**b.** 调用期间同时检查子进程是否还活着（需要拿到 pid：`TokioChildProcess::id()`，`child_process.rs:72-74`，要在 `serve` 之前取）。 | a 合法的长任务会被误杀，默认值难定；b 只覆盖「直接子进程死了」这一种，`sh` 包装里 python 死、`sh` 活的 §2.6 情形仍然覆盖不到。交互模式有 ESC 兜底；headless / bot 没有。 |
| 5 | 孤儿 | **a.** `process_group(0)` + 关闭时 `killpg`（照抄 `src/shell/exec.rs:274-280`）——解决孙进程孤儿。**b.** iota 被 SIGKILL 的孤儿：macOS 没有 `PR_SET_PDEATHSIG`，只能靠服务器自己处理 stdin EOF。 | a 的副作用：子进程离开前台进程组后，终端挂断的 SIGHUP 不再送到它们——§2.7 注里 tmux 结束时「整组被带走」的兜底会消失，iota 被 SIGKILL 时孤儿可能**更多**（启动器的孙进程不再因 SIGHUP 死）。需要实测权衡。 |

我的排序：1a + 2b 最便宜、收益最大（状态说真话、给用户 notice、错误可操作、不破缓存）；3a 其次（三家都至少有手动重连，iota 没有）；4a 给一个很大的默认超时（对照 §3：Claude Code 约 28 h、Gemini 10 min、Codex 300 s）成本低，可以和 1a 一起做；5 是边缘情形，先记下。

## 5. 拿不准 / 不同意的地方

- **2a vs 2b**：把死工具从列表里摘掉看起来更「正确」，但对提示词缓存和历史一致性都有代价；我倾向 2b，但这是产品取舍，需要所有者定。
- **「最该修」的判断依赖真实服务器的行为**：常见的 `npx`/`uvx` 启动器在子进程死后是否也退出、从而给出 EOF，我没有测（不能联网）。如果它们会挂着不退，§2.6 的永久挂起就不是边缘情形而是常态，优先级要上调。
- §2.3 在途调用 1 s 内返回，依赖 rmcp 服务循环 `Closed` 后的排空（`service.rs:1727-1757`，上限 5 s）恰好没有可等的东西；一个在途 `notifications/progress` 之类的场景下是否会拖满 5 s，没有测。

## 6. 之后

本文以上是诊断，原样保留。所有者 2026-10-15 的决定与实现记在 DIVERGENCES X-78（down、通知、调用截止时间）与 X-79（`/tools` 与重连，docs/design/tools-command.md）：§4.2 取 #1a（标记 + 通知）、#2b（**工具留着不摘**，调用返回明确错误）、#4a（大的默认截止时间），#3 以 `/tools` 里的手动重连实现。

2026-10-16 补了两处与这些决定拧着的地方（tools-command.md §5）：

- **重连失败不摘工具**。重连原来先 detach 再连，所以一个 down（工具留着）的服务器重连失败后会变成没有工具的 failed，工具数组变了一次——正是 #2b 要避免的。现在先只关旧 session，失败且之前有工具就回到 down（概要换成这次失败的原因），工具原样；进行中是 `reconnecting…`，工具也留着。只有从没连上过的服务器失败后是 failed。
- **通知通道跟 manager 同寿**。启动时一个都没连上，down 通知的发送端原来会被放掉，之后从 `/tools` 重连上的服务器再死就没有 `⚠ MCP <name>: …` 那一行。
