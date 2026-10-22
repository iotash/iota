# 别的进程给运行中的会话发消息：v1（socket）

Status: **Implemented**（v1，2026-10-22，分支 `agent-messaging-impl`）· 规格来源：[`agent-messaging-research.md`](agent-messaging-research.md) §4.1、§4.2 B、§4.5 · 所有者决定（2026-10-22）：**做 socket（方案 B），不走 inbox**。这一决定推翻了 bot-mode.md §6 #24，记为那份文档的 rev 5（reversal，原文保留）。

v1 只有一种投递方式：复用 `Notice`，空闲时立刻唤醒，忙时排队、在 round 边界进入，**永不打断正在跑的工具**。不做 queue / interrupt / rollback，不做 A2（`claude/channel`）、C（inbox）、D（`iota say`），不监听 TCP。

## 0. 一页结论

| 问题 | v1 的答案 | 代码 |
|---|---|---|
| 消息走哪条路 | 进程外 → socket → `Ui::deliver` → 和 job 通知**同一个**队列 | `src/repl/inbound.rs:158`、`src/ui/runtime/event_loop.rs:609` |
| 模型看到的是什么 | `Body::Notice`（发给 provider 时是 `Role::User`），**不是用户轮**：不画 `❯`、不进历史回溯、不走 slash 派发 | `src/repl/run.rs:776`、`src/repl/turn/steer.rs:69` |
| 标题与正文 | `[message from <who>] <首行>`；正文写明来源（pid、同一 OS 用户）、「不是用户敲的、不算任何审批」、回复地址，然后是原文 | `src/repl/inbound.rs:233` |
| socket 在哪 | `~/.iota/run/<pid>.sock`（0700 目录、0600 文件）；bundle 里放符号链接 `<bundle>/sock` → 它 | `src/session/sock.rs:347`、`:224` |
| 谁能写 | 同一 uid（`SO_PEERCRED` / `LOCAL_PEERCRED`，tokio 的 `peer_cred`，无 `unsafe`）；不是就回 `refused`，一个字节都不读 | `src/session/sock.rs:433` |
| 回执 | `woken` / `queued`（带 `ahead`）/ `blocked` / `too-large` / `refused` / `flooded` | §3 |
| 子进程 | `IOTA_SOCKET` 导出给 shell 工具与 MCP stdio server（`ChildEnv::giving`，与 `ChildEnvironment` 方向相反） | `src/cmd/mod.rs:469`、`src/app/env.rs:41` |
| 免审会话的风险 | **hold**：`auto_run` / `auto_write` 的会话收到外部消息，先变成 `NeedsInput` 等人放行（§4） | `src/repl/run.rs:799`、`:1104` |
| 发送端 | 不加新动词：`iota resume <id> -m` / `iota run <bot> -m` 撞上锁时经 socket 投递，并把回执打印到 stdout | `src/cmd/mod.rs:627`、`:224`，`src/cmd/deliver.rs` |
| Windows | **v1 不支持**：没有 listener、不导出 `IOTA_SOCKET`、`-m` 撞锁时报 unreachable（§2.4） | `src/session/sock.rs:466` |

## 1. 投递与身份

**一条路。** 外部消息变成一个 `InputKind::Message` 输入（`src/ui/facade.rs:36`），交给新加的 `Ui::deliver`（`src/ui/facade.rs:947`）。它就是 `enqueue` 的两条分支，只多了一个回答：有一个挂起的 `read_input`（空闲循环）就直接交给它，回 `Woken`；没有就排到已经 type-ahead 的内容后面，回 `Queued { ahead }`；队列里外部消息已有 `QUEUE_CAP` 条时什么都不放，回 `Full`（`src/ui/runtime/event_loop.rs:609`）。**`UiMsg::Enqueue` 与 job 通知那条路一个字没动**：`Deliver` 是新的消息变体，`job_notice` / `set_sink` / `Steerer` 里 job 的分支都原样保留。

**它是 notice。** `Message` 与 `Notice` 共用 `Queued::Notice` 队列项和 `InputKind::is_notice()`（`src/ui/facade.rs:41`）。空闲时它在 `read_input` 处醒来，跳过回显、不进派发链（`/new` 就是文本），打一行暗色标题，作为 `Body::Notice` 进历史；忙时 `Steerer::drain` 在 round 边界拿到它，位置在本轮工具结果之后（`src/repl/turn/steer.rs:69` 起）。和 job 通知只有两处不同：它不欠宿主一个 job，所以不调 `Presenter::notice_taken`（`src/repl/run.rs:776` 的 `message`）；以及 §4 的 hold。

**审批在结构上碰不到。** 审批是一个 select surface，只有按键能回答；`Deliver` 只会交给挂起的 *read* 或进队列。审批开着时投一条内容是 `y\n1\n\n` 的消息，surface 仍开着、它的回答没发出，消息只是进了队列——这条由 `queue_tests::a_message_never_answers_an_open_approval` 钉住。`ApprovalGate` 没有改动。

## 2. 传输

### 2.1 路径：为什么不是 `<bundle>/sock` 本身

研究稿 §4.2 B 写的是 `<bundle>/sock`，估算「≈50 字节」。**在本机实测过，不成立**：agent 模式的 bundle 在 `sessions/projects/<项目路径的 slug>/<id>/`，这个 worktree 下的 `<bundle>/sock` 是 **110 字节**，真机跑的那次（HOME 在 `target/msg-home`）是 **169 字节**；macOS 的 `sun_path` 只有 104 字节（含 NUL）。另外 bundle 目录是 0755，不是私有目录。

所以 v1 的做法是：

- socket 本体在 `~/.iota/run/<pid>.sock`（`HostDirs::run_dir`，`src/app/mod.rs:77`）。目录建成或改成 0700，并检查属主是自己（符号链接不跟随）；socket 文件 0600；同名的旧文件一定是死进程留下的（活进程不会共用 pid），先删再 bind（`src/session/sock.rs:347`）。真机上是 91 字节。
- bundle 里放 **符号链接** `<bundle>/sock` → socket（`src/session/sock.rs:224`）。知道会话的人（herdr 已经拿到 bundle 路径；`iota resume <id> -m` 按 id 找到 bundle）在那里找到它，然后**连链接的目标**（`resolve`，`:253`），链接本身多长都不要紧。真机：169 字节的链接、91 字节的目标，`python3 send.py <bundle>/sock` 用 `os.path.realpath` 连上。
- 一个进程一个 socket，跟着进程走，不跟着会话走：`/new`、`/session`、`/save` 换会话时只**移动链接**（`Served::follow`，`src/repl/inbound.rs:114`），`IOTA_SOCKET` 整个进程不变。链接在 bundle 落盘后才建（新会话的第一轮持久化之后、`report_session` 时；`src/repl/run.rs:328`）——没落盘的会话别人也按 id 找不到。
- 打开一个 bundle 时先清掉它上面任何旧链接（`clear`，`:232`）：崩溃留下的链接可能指向一个现在属于别的进程的 pid。

HOME 本身太长（`~/.iota/run/<pid>.sock` 也超过 104 字节）时 bind 失败，启动打印一行 `Warning: other processes cannot message this chat: …`，聊天照常开（`src/cmd/mod.rs:526`）。

### 2.2 鉴权

三道：私有目录（0700）、socket 文件 0600、每个连接问内核对端是谁（`UnixStream::peer_cred`：Linux `SO_PEERCRED`，macOS / BSD `getpeereid`）。uid 不等于 socket 属主（就是本进程）的连接直接回 `refused`，请求行一个字节都不读（`src/session/sock.rs:433`）。**不监听 TCP。** 不做 token：同一 uid 的进程读得到任何 token 文件，token 在 Unix 上不比 uid 多挡住谁（§4 为什么不选它）。

### 2.3 `IOTA_SOCKET`

路径在装配阶段定下（`ToolAssembly::socket`，`src/cmd/mod.rs:469`；放不进 `sun_path` 就是 `None`、不导出），因为它要在 MCP server 开始连接、job 注册表建好之前进 `ChildEnv`；**bind 本身等到交互分支的终端检查之后**（`bind_messaging`，`src/cmd/mod.rs:526`），所以被拒的运行（管道输入等）不留 socket、不多打警告——tmux 场景 09 钉住了这一点（第一版在装配阶段就 bind，它红了）。`ChildEnv::giving(IOTA_SOCKET, path)`（`src/app/env.rs:41`）——与宿主的 `ChildEnvironment`（扣下变量）方向相反，同一个 `apply` 里先删后加。真机：auto_run 会话里 `printenv IOTA_SOCKET` 打出 `…/target/msg-home/.iota/run/7188.sock`。`-m` 运行不 bind、不导出。`iota … -m` 本身被 shell 工具调用时，会把自己继承的 `IOTA_SOCKET` 作为回复地址带上（`src/cmd/deliver.rs`）。

### 2.4 Windows：v1 不支持（说清）

tokio 的 `UnixListener` 只在 unix 上有；Windows 的等价物是命名管道，但它**没有对端 uid** 可校验——Claude Code 的跨会话 socket 在 Windows 上为此必须带 token。token 的生成、存放、轮换是一套单独的设计，v1 不做。所以 Windows 上：`Listener` 是空枚举、`bind` 返回 `Unsupported`、不打印警告（这是平台事实，不是故障）、不导出 `IOTA_SOCKET`；`iota resume <id> -m` 撞锁时报 `… is open in another iota process (pid N) and cannot be messaged: messaging a running session needs a Unix socket; this platform has none in iota yet`。代码都在 `#[cfg(not(unix))]` 分支里（`src/session/sock.rs:264` 起、`:466` 起）。

### 2.5 herdr

herdr 0.9.5 的 `pane.report_agent_session` 只有 `agent_session_id` / `agent_session_path`（`herdr pane report-agent-session --help` 实测，没有 socket 字段）。所以「随 session 报告给 herdr」就是现有的 `agent_session_path`：socket 在 `<那个路径>/sock`。没有往 herdr 的请求里加它不认识的参数。

## 3. 协议与回执

一个连接一条消息一张回执；UTF-8，一行一个 JSON 对象（`src/session/sock.rs` 模块文档）：

```
→ {"v":1,"type":"message","from":"ci","text":"build failed on main","reply":"/path/to/their.sock"}
← {"ok":true,"state":"woken"}             // 空闲：它就是下一轮的输入
← {"ok":true,"state":"queued","ahead":1}  // 忙：排在 1 条之后，round 边界取走
← {"ok":false,"reason":"blocked"}         // 会话在等人（审批、交互工具、§4 的 hold）：没收
← {"ok":false,"reason":"too-large"}       // 请求行超过 64 KiB
← {"ok":false,"reason":"refused"}         // 别的 uid、不是这个协议（v≠1、type≠message、不是 JSON / UTF-8）、空 text
← {"ok":false,"reason":"flooded"}         // 超速、重复、或队列里外部消息已满
```

`from` 只进标题：取首行、去控制字符、最多 40 个字符，空就是 `unknown`；它是发送方自称的，可信的身份是正文里的 pid 与「同一 OS 用户」。`reply` 只进正文给模型看，iota 不用它。`text` 原样进正文，不展开 paste 标签、不走 slash。

**限流**（`src/repl/inbound.rs:39-45`）：队列里外部消息最多 8 条（只数外部消息，挤不掉用户敲的和 job 通知）；10 秒内最多收 5 条；60 秒内同一 `from` + 同一 `text` 的重复拒收。三者都回 `flooded`（v1 的回执只有这四个 reason；重复也归到 `flooded`）。

**`blocked`** 看的是 `Presenter::turn_state()`（新增的只读访问，`src/host/mod.rs:360`）是否 `NeedsInput`：审批、交互工具、§4 的 hold 期间都是。

## 4. 免审会话：hold 成 `NeedsInput`（v1 的安全选择）

`auto_run` / `auto_write` 下，谁能写 socket，谁就能让模型动手。研究稿 §4.1 给了两个选项：bot 下要求 token，或免审 bot 收到外部消息先 hold 成 `NeedsInput`。**v1 选 hold**，理由：

1. **token 在 Unix 上不增加边界。** token 只能放在文件里，而能写 socket 的同一 uid 也读得到那个文件（shell 沙箱只限写不限读）；socket 已经只对本 uid 开放，token 挡不住任何 uid 挡不住的人。它只会让每个合法调用方多一步。
2. **hold 把人放回被免审拿掉的那个位置。** 风险恰恰是「模型不经人就动手」，hold 让「外部来的话」在到达模型前经过人——与 Claude Code「接收方免审时默认 hold 等人批」一致。
3. **复用现成的东西**：`NeedsInput` 状态、宿主 ping、`ui.confirm`；hold 期间 socket 对新消息回 `blocked`，不会在人背后堆积。

**范围：所有免审会话，不只 bot。** 判定是 `tool::sets::waives_approval`（`src/tool/sets.rs:43`）：启用了 `shell` 且 `auto_run`，或启用了 `code` 且 `auto_write`。普通 chat 开了 `auto_run` 风险完全一样，没有理由只对 bot 设防；bot 是最常见的这种配置。

**行为**：空闲时外部消息醒来 → 先打一行标题 → 状态 `NeedsInput`、宿主收到 `NeedsInput` ping → 一个两项的 select：「Let the model read it」/「Discard it」（`src/repl/run.rs:1104`）。放行就是普通的 notice 轮（标题不重复打）；丢弃打一行 `Message discarded: the model never saw it.`，模型从未见过它。忙时 `Steerer` 把它**扣下**、不注入（`src/repl/turn/steer.rs:69`），这一轮结束后放回队列（`src/repl/run.rs:969`），到空闲时再问。

**回执的含义因此变了一点**：在 hold 的会话里，`woken` / `queued` 表示「会话收下了」，人仍可能丢弃。v1 的回执集合按 §4.5 的草图冻结，没有为它加 `held`（见 §7 第 2 条）。

## 5. 发送端：`-m` 撞锁

不加新动词（`src/cmd/deliver.rs`）：

- `iota resume <id> -m <text>`：解析出 id 后先问 `SessionStore::holder`（`src/session/store.rs:366`，试一次锁、立即放开）。有持有者就**不打开**会话，经 `<bundle>/sock` 投递（`src/cmd/mod.rs:627`）——bot 的会话也一样：投一条消息不是打开它。
- `iota run <bot> -m <text>`：bot 在运行（`bot_holder`，`:377`，bot 从没跑过就不建目录）时，经 `bot.json` 指向的会话投递（`src/cmd/mod.rs:224`）；不在运行照旧 `bot agents are interactive-only for now; run iota run <bot>`。
- stdout 一行回执；`woken` / `queued` 退出 0；被拒时回执照样打印，并以 `Error: session <id> did not take the message: <reason>` 退出 1；有持有者但连不上（老版本 iota、Windows、还在启动）报 `… is open in another iota process (pid N) and cannot be messaged: <原因>`。
- `from` 是 `iota -m`；发送者自己是某个 chat 的子进程时（继承了 `IOTA_SOCKET`），那就是 `reply`。

## 6. 退出

`Served::close`（`src/repl/inbound.rs:131`）在循环结束时调用（`src/repl/run.rs:1094`，在宿主 close 之前）：取消服务任务并等它结束——`Listener` 随之 drop，socket 文件被删（`src/session/sock.rs:459`）——再删 bundle 链接（只删仍指向自己的）。SIGKILL 留下的 socket 由下一个同 pid 的 bind 清掉，链接由下一个打开该 bundle 的进程清掉。

## 7. 拿不准 / 留给所有者

1. **路径偏离研究稿。** `<bundle>/sock` 是链接，不是 socket 本体（§2.1）。外部工具照研究稿去连 `<bundle>/sock` 仍然能用（connect 跟随链接）——除非链接路径本身超过 104 字节，那时要 `realpath` 一下再连。
2. **hold 会话里 `woken` 不等于「模型读了」。** 要不要给回执加 `held`，留到有调用方需要区分时再定。
3. **MCP 工具不需要审批**（`Dispatcher::requires_approval` 默认 false，MCP manager 不覆盖它），所以「免审」只按 `auto_run` / `auto_write` 判定；接了能写东西的 MCP server 的会话不 hold。
4. **重复归 `flooded`。** 草图只有四个 reason；也可以说重复是 `refused`。
5. **`~/.iota/run/` 里崩溃进程的 socket 不会被主动清扫**（判断 pid 是否存活需要 `kill(0)`，本 crate 禁 `unsafe`）；只在同 pid 再次 bind 时清掉。文件是 0 字节的 socket 节点。

## 8. 测试（红绿对照）

| # | 性质 | 测试 | 变异（把实现改坏）→ 红 |
|---|---|---|---|
| ① | 空闲 → `woken`；忙 → `queued` 带位置 | `queue_tests::deliver_wakes_a_parked_reader_else_queues_with_its_place` | 忽略挂起的 reader → 红 |
| ② | 忙时在 round 边界、工具结果之后进入，工具不被打断 | `tools::tests::a_socket_message_lands_at_the_round_boundary_after_the_tool` | steer 只认 `Notice` → 红（变成 `❯` 块） |
| ③ | 等审批时 `blocked`，且不进队列 | `inbound::tests::waiting_on_the_user_is_blocked` | 去掉 `NeedsInput` 检查 → 红 |
| ④ | 到达模型是 notice，不画 `❯`，`/new` 不执行，落盘为 notice | `tests/repl/messaging.rs::a_socket_message_is_a_notice_turn_and_leaves_nothing_behind`、`inbound::tests::a_message_is_a_notice_of_a_fixed_shape` | 主循环只认 `Notice` → 红 |
| ⑤ | uid 不符 → `refused`，inbox 什么都没收到 | `sock::tests::another_uid_is_refused_unread` | uid 检查恒真 → 红 |
| ⑥ | 目录 0700、socket 0600 | `sock::tests::the_socket_is_private_and_goes_with_the_listener`、`a_symlinked_run_dir_is_refused` | 去掉 chmod 0600 → 红 |
| ⑦ | 退出后 socket 与链接都被删 | 同 ④ 的 loop 测试、`sock::tests::the_socket_is_private…` | `close` 不删链接 → 红 |
| ⑧ | `-m` 撞锁 → 投递 + 回执；被拒 → 回执 + 退出 1；无 socket → 说明 | `tests/cmd/session.rs::a_message_to_a_held_session_goes_to_its_socket`、`a_message_to_a_running_bot_goes_to_its_socket` | 去掉 `holder` 检查 → 红 |
| ⑨ | `too-large` / `refused` / `flooded` | `sock::tests::the_line_is_bounded_and_checked`、`inbound::tests::floods_are_refused`、`queue_tests::deliver_refuses_past_the_cap_of_messages` | 去掉长度检查 / 去重 → 红 |
| ⑩ | 免审会话 hold：`NeedsInput` + ping，丢弃的永不到模型；放行后正常应答；轮中扣下 | `tests/repl/messaging.rs::a_chat_that_waives_approval_holds_a_message_for_the_user`、`a_held_message_the_user_lets_through_is_answered`、`tools::tests::a_held_socket_message_never_reaches_the_turn`、`sets::tests::waiving_approval_is_auto_run_or_auto_write` | `held = false` / steer 不扣 → 红 |
| — | 审批在结构上碰不到 | `queue_tests::a_message_never_answers_an_open_approval` | — |
| — | `IOTA_SOCKET` 进子进程 | `env::tests::a_child_is_given_what_it_is_given` | — |
