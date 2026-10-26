# 别的进程给运行中的会话发消息：v1（socket）

Status: **Implemented**（v1，2026-10-22，分支 `agent-messaging-impl`；2026-10-24 按两份独立评审修订，见 §9）· 规格来源：[`agent-messaging-research.md`](agent-messaging-research.md) §4.1、§4.2 B、§4.5 · 所有者决定（2026-10-22）：**做 socket（方案 B），不走 inbox**。这一决定推翻了 bot-mode.md §6 #24，记为那份文档的 rev 5（reversal，原文保留）。

v1 只有一种投递方式：复用 `Notice`，空闲时立刻唤醒，忙时排队、在 round 边界进入，**永不打断正在跑的工具**。不做 queue / interrupt / rollback，不做 A2（`claude/channel`）、C（inbox）、D（`iota say`），不监听 TCP。

## 0. 一页结论

| 问题 | v1 的答案 | 代码 |
|---|---|---|
| 消息走哪条路 | 进程外 → socket → `Ui::deliver` → 和 job 通知**同一个**队列 | `src/repl/inbound.rs:165`、`src/ui/runtime/event_loop.rs:609` |
| 模型看到的是什么 | `Body::Notice`（发给 provider 时是 `Role::User`），**不是用户轮**：不画 `❯`、不进历史回溯、不走 slash 派发 | `src/repl/run.rs:786`、`src/repl/turn/steer.rs:63` |
| 标题与正文 | `[message from <who>] <首行>`；正文写明来源（pid、同一 OS 用户）、「不是用户敲的、不算任何审批」、回复地址，然后是原文 | `src/repl/inbound.rs:239` |
| socket 在哪 | `~/.iota/run/<pid>.sock`（0700 目录、0600 文件）；bundle 里放符号链接 `<bundle>/sock` → 它 | `src/session/sock.rs:322`、`:197` |
| 谁能写 | 同一 uid（`SO_PEERCRED` / `LOCAL_PEERCRED`，tokio 的 `peer_cred`，无 `unsafe`）；不是就回 `refused`，一个字节都不读 | `src/session/sock.rs:385` |
| 回执 | `woken` / `queued`（带 `ahead`）/ `held` / `blocked` / `too-large` / `refused` / `flooded`；会话已关闭时**不回执、直接断开** | §3 |
| 子进程 | `IOTA_SOCKET`（bind 成功才导出）给 shell 工具与 MCP stdio server（`ChildEnv::giving`，与 `ChildEnvironment` 方向相反） | `src/cmd/mod.rs:467`、`src/app/env.rs:41` |
| 免审会话的风险 | **hold**：工具不经询问就动手的会话（`Dispatcher::waives_approval`）收到外部消息，回执 `held`，先变成 `NeedsInput` 等人放行（§4） | `src/repl/run.rs:674`、`:809`、`:1117` |
| 发送端 | 不加新动词：`iota resume <id> -m` / `iota run <bot> -m` 撞上锁时经 socket 投递，把回执**原样**打印到 stdout，只读 `ok` | `src/cmd/mod.rs:631`、`:225`，`src/cmd/deliver.rs` |
| Windows | **v1 不支持**：没有 listener、不导出 `IOTA_SOCKET`、`-m` 撞锁时报 unreachable（§2.4） | `src/session/sock.rs:316` |

## 1. 投递与身份

**一条路。** 外部消息变成一个 `InputKind::Message` 输入（`src/ui/facade.rs:36`），交给新加的 `Ui::deliver`（`src/ui/facade.rs:947`）。它就是 `enqueue` 的两条分支，只多了一个回答：有一个挂起的 `read_input`（空闲循环）就直接交给它，回 `Woken`；没有就排到已经 type-ahead 的内容后面，回 `Queued { ahead }`；队列里外部消息已有 `QUEUE_CAP` 条时什么都不放，回 `Full`（`src/ui/runtime/event_loop.rs:609`）。**`UiMsg::Enqueue` 与 job 通知那条路一个字没动**：`Deliver` 是新的消息变体，`job_notice` / `set_sink` 原样保留。

**它是 notice。** `Message` 与 `Notice` 共用 `Queued::Notice` 队列项和 `InputKind::is_notice()`（`src/ui/facade.rs:41`）。空闲时它在 `read_input` 处醒来，跳过回显、不进派发链（`/new` 就是文本），打一行暗色标题，作为 `Body::Notice` 进历史；忙时 `Steerer::drain` 在 round 边界拿到它，位置在本轮工具结果之后（`src/repl/turn/steer.rs:63` 起）。和 job 通知只有三处不同：它不欠宿主一个 job，所以不调 `Presenter::notice_taken`（`src/repl/run.rs` 的 `message`）；notice gate（X-84）不暂扣它（§10）；以及 §4 的 hold。

**审批在结构上碰不到。** 审批是一个 select surface，只有按键能回答；`Deliver` 只会交给挂起的 *read* 或进队列。审批开着时投一条内容是 `y\n1\n\n` 的消息，surface 仍开着、它的回答没发出，消息只是进了队列——这条由 `queue_tests::a_message_never_answers_an_open_approval` 钉住。`ApprovalGate` 没有改动。

## 2. 传输

### 2.1 路径：为什么不是 `<bundle>/sock` 本身

研究稿 §4.2 B 写的是 `<bundle>/sock`，估算「≈50 字节」。**在本机实测过，不成立**：agent 模式的 bundle 在 `sessions/projects/<项目路径的 slug>/<id>/`，这个 worktree 下的 `<bundle>/sock` 是 **110 字节**，真机跑的那次（HOME 在 `target/msg-home`）是 **169 字节**；macOS 的 `sun_path` 只有 104 字节（含 NUL）。另外 bundle 目录是 0755，不是私有目录。

所以 v1 的做法是：

- socket 本体在 `~/.iota/run/<pid>.sock`（`HostDirs::run_dir`，`src/app/mod.rs:77`）。目录建成（或改成）0700；socket 文件 0600；同名的旧文件一定是死进程留下的（活进程不会共用 pid），先删再 bind（`src/session/sock.rs:322`）。真机上是 91 字节。目录是符号链接时照样用（评审后删掉了「拒符号链接」与「bind 后查属主」两道防御：目录在用户自己的 HOME 里，0600 与对端 uid 校验才是边界）。
- bundle 里放 **符号链接** `<bundle>/sock` → socket（`src/session/sock.rs:197`），目标永远是绝对路径。知道会话的人（herdr 已经拿到 bundle 路径；`iota resume <id> -m` 按 id 找到 bundle）在那里找到它，然后**连链接的目标**（`resolve`，`:228`），链接本身多长都不要紧。
- 一个进程一个 socket，跟着进程走，不跟着会话走：`/new`、`/session`、`/save` 换会话时只**移动链接**（`Served::follow`，`src/repl/inbound.rs:121`），`IOTA_SOCKET` 整个进程不变。链接在 bundle 落盘后才建（新会话的第一轮持久化之后、`report_session` 时；`src/repl/run.rs:328`）——没落盘的会话别人也按 id 找不到。链接建不起来时聊天里打一行 `⚠ Other processes cannot message this session: <原因>`（只打一次）。
- **链接只能指向持锁者。** 取得 bundle 锁的那一刻就删掉它上面的任何旧链接（`src/session/lock.rs:65` 调 `sock::clear`，`clear` 只对 `session` 模块可见）：此后持锁者才是链接唯一能指向的进程。这样崩溃留下的链接、或者一个**没有 socket** 的会话（Windows、bind 失败）打开 bundle 时，旧链接都不会把 `-m` 投进一个复用了旧 pid 的无关会话。

### 2.2 鉴权

三道：私有目录（0700）、socket 文件 0600、每个连接问内核对端是谁（`UnixStream::peer_cred`：Linux `SO_PEERCRED`，macOS / BSD `getpeereid`）。uid 不等于 socket 属主（就是本进程）的连接直接回 `refused`，请求行一个字节都不读（`src/session/sock.rs:385`）。**不监听 TCP。** 不做 token：同一 uid 的进程读得到任何 token 文件，token 在 Unix 上不比 uid 多挡住谁（§4 为什么不选它）。

### 2.3 bind 的时机与 `IOTA_SOCKET`

交互模式在装配阶段 bind（`ToolAssembly::messaging`），因为 socket 路径要在 MCP server 开始连接、job 注册表建好之前进 `ChildEnv`；**只有 bind 成功才导出** `IOTA_SOCKET`（`src/cmd/mod.rs:467`）。bind 的结果原样留着，**失败的警告等到交互分支的终端检查之后才打**（`messaging_or_warn`，`src/cmd/mod.rs:548`，调用在 `src/cmd/interactive/mod.rs:271`），并写出真实原因（例如 `path must be shorter than SUN_LEN`）。被拒的运行（管道输入等）什么都不多打；它的 `Listener` 随运行一起 drop，socket 文件随之删除——tmux 场景 09 钉住「被拒运行不留 socket、不打警告」。`ChildEnv::giving`（`src/app/env.rs:41`）只装一个变量，`apply` 里先删扣留的、再设这一个。真机：auto_run 会话里 `printenv IOTA_SOCKET` 打出 `…/target/msg-home/.iota/run/7188.sock`。`-m` 运行不 bind、不导出。`iota … -m` 本身被 shell 工具调用时，会把自己继承的 `IOTA_SOCKET` 作为回复地址带上（`src/cmd/deliver.rs`）。

### 2.4 Windows：v1 不支持（说清）

tokio 的 `UnixListener` 只在 unix 上有；Windows 的等价物是命名管道，但它**没有对端 uid** 可校验——Claude Code 的跨会话 socket 在 Windows 上为此必须带 token。token 的生成、存放、轮换是一套单独的设计，v1 不做。所以 Windows 上：不 bind（`bind_messaging` 在非 unix 上返回 `None`）、不打印警告（这是平台事实，不是故障）、不导出 `IOTA_SOCKET`；`Listener` 是空枚举；`iota resume <id> -m` 撞锁时报 `… is open in another iota process (pid N) and cannot be messaged: messaging a running session needs a Unix socket; this platform has none in iota yet`。

### 2.5 herdr

herdr 0.9.5 的 `pane.report_agent_session` 只有 `agent_session_id` / `agent_session_path`（`herdr pane report-agent-session --help` 实测，没有 socket 字段）。所以「随 session 报告给 herdr」就是现有的 `agent_session_path`：socket 在 `<那个路径>/sock`。没有往 herdr 的请求里加它不认识的参数。

## 3. 协议与回执

一个连接一条消息一张回执；UTF-8，一行一个 JSON 对象（`src/session/sock.rs` 模块文档）：

```
→ {"v":1,"type":"message","from":"ci","text":"build failed on main","reply":"/path/to/their.sock"}
← {"ok":true,"state":"woken"}             // 空闲：它就是下一轮的输入
← {"ok":true,"state":"queued","ahead":1}  // 忙：排在 1 条之后，round 边界取走
← {"ok":true,"state":"held"}              // 收下了，但会话的工具不经询问就动手：由人决定模型读不读（§4）
← {"ok":false,"reason":"blocked"}         // 会话在等人（审批、交互工具、§4 的 hold）：没收
← {"ok":false,"reason":"too-large"}       // 请求行超过 64 KiB
← {"ok":false,"reason":"refused"}         // 别的 uid、不是这个协议（v≠1、type≠message、不是 JSON / UTF-8）、空 text
← {"ok":false,"reason":"flooded"}         // 超速、重复、或队列里外部消息已满
（无回执，连接关闭）                         // 会话已经收不了任何东西（界面已关闭）：发送方得到 I/O 错误，不是一个归咎于它写法的 refused
```

**发送方只读 `ok`。** `sock::send` 返回 `Answer { line, ok }`（`src/session/sock.rs:188`）：回执行原样保留，只解析 `ok`；`-m` 把这一行原样打印。不维护封闭的回执枚举的解析——更新的 iota 加了新的 state 或 reason，老的发送方照样能用。

`from` 只进标题：取首行、去控制字符、最多 40 个字符，空就是 `unknown`；它是发送方自称的，可信的身份是正文里的 pid 与「同一 OS 用户」。`reply` 只进正文给模型看，iota 不用它。`text` 原样进正文，不展开 paste 标签、不走 slash。

**限流**（`src/repl/inbound.rs:41-47`）：队列里外部消息最多 8 条（只数外部消息，挤不掉用户敲的和 job 通知）；10 秒内最多收 5 条；60 秒内同一 `from` + 同一 `text` 的重复拒收。三者都回 `flooded`。

**`blocked`** 看的是 `Presenter::turn_state()`（新增的只读访问，`src/host/mod.rs:360`）是否 `NeedsInput`：审批、交互工具、§4 的 hold 期间都是。

## 4. 免审会话：hold 成 `NeedsInput`（v1 的安全选择）

工具不经询问就动手时，谁能写 socket，谁就能让模型动手。研究稿 §4.1 给了两个选项：bot 下要求 token，或免审 bot 收到外部消息先 hold 成 `NeedsInput`。**v1 选 hold**，理由：

1. **token 在 Unix 上不增加边界。** token 只能放在文件里，而能写 socket 的同一 uid 也读得到那个文件（shell 沙箱只限写不限读）；socket 已经只对本 uid 开放，token 挡不住任何 uid 挡不住的人。它只会让每个合法调用方多一步。
2. **hold 把人放回被免审拿掉的那个位置。** 风险恰恰是「模型不经人就动手」，hold 让「外部来的话」在到达模型前经过人——与 Claude Code「接收方免审时默认 hold 等人批」一致。
3. **复用现成的东西**：`NeedsInput` 状态、宿主 ping、`ui.confirm`；hold 期间 socket 对新消息回 `blocked`，不会在人背后堆积。

**触发器：已建好的 dispatcher 说了算（一处真相）。** `Dispatcher::waives_approval`（`src/tool/mod.rs`，Registry 与 Merged 汇总各工具的 `Tool::waives_approval`，`src/tool/dispatch.rs:135`）；每个工具读的是它的 `requires_approval` 读的**同一个**配置字段：`shell` 的 `auto_run`（`src/tool/builtins/shell.rs:271`）、`edit_file` / `write_file` 的 `auto_write`。循环启动时问一次（`src/repl/run.rs:674`）。评审前它是 `tool::sets` 里对 YAML 的第二次解码，已删除。**范围：所有这样配置的会话，不只 bot**——普通 chat 开了 `auto_run` 风险完全一样；bot 是最常见的这种配置。

**政策维持现状（未决，见 §7 第 1 条）：沙箱不算免审。** `shell: {sandbox: auto}` 不开 `auto_run` 时，沙箱内的命令不问就跑，但 `shell` 的 `waives_approval` 是 `auto_run`，所以这样的会话**不 hold**。`sets::tests::the_dispatcher_says_whether_approval_is_waived` 把这一点钉成了今天的预期。

**行为**：回执是 `held`（不是 `woken` / `queued`：那两个承诺模型会读）。空闲时外部消息醒来 → 先打一行标题 → 状态 `NeedsInput`、宿主收到 `NeedsInput` ping → 一个两项的 select：「Let the model read it」/「Discard it」（`src/repl/run.rs:1117`）。放行就是普通的 notice 轮（标题不重复打）；丢弃打一行 `Message discarded: the model never saw it.`，模型从未见过它。忙时 `Steerer` 把它**扣下**、不注入——和 bot 的 flush notice 进同一个 `held` 列表（`src/repl/turn/steer.rs:63`）；这一轮结束后循环把其中的外部消息放回队列（`src/repl/run.rs:981`），到空闲时再问。

## 5. 发送端：`-m` 撞锁

不加新动词（`src/cmd/deliver.rs`）：

- `iota resume <id> -m <text>`：解析出 id 后先问 `SessionStore::holder`（`src/session/store.rs:366`，试一次锁、立即放开）。有持有者就**不打开**会话，经 `<bundle>/sock` 投递（`src/cmd/mod.rs:631`）——bot 的会话也一样：投一条消息不是打开它。
- `iota run <bot> -m <text>`：bot 在运行（`bot_holder`，`:377`，bot 从没跑过就不建目录）时，经 `bot.json` 指向的会话投递（`src/cmd/mod.rs:225`）；不在运行照旧 `bot agents are interactive-only for now; run iota run <bot>`。
- stdout 一行回执，原样；`ok: true` 退出 0；否则回执照样打印，并以 `Error: session <id> did not take the message: <回执行>` 退出 1；有持有者但连不上（老版本 iota、Windows、还在启动、会话没有 socket）报 `… is open in another iota process (pid N) and cannot be messaged: <原因>`。
- `from` 是 `iota -m`；发送者自己是某个 chat 的子进程时（继承了 `IOTA_SOCKET`），那就是 `reply`。

## 6. 退出

`Served::close`（`src/repl/inbound.rs:135`）在循环结束时调用（`src/repl/run.rs:1107`，在宿主 close 之前）：取消服务任务并等它结束——`Listener` 随之 drop，socket 文件被删（`src/session/sock.rs:412`）——再删 bundle 链接（只删仍指向自己的）。SIGKILL 留下的 socket 由下一个同 pid 的 bind 清掉，链接由下一个取得该 bundle 锁的进程清掉（§2.1）。

## 7. 拿不准 / 留给所有者

1. **沙箱算不算免审（未决）。** `shell: {sandbox: auto}` 不开 `auto_run` 时，沙箱内命令不问就跑，而沙箱允许写项目根；今天的政策是**不 hold**。两种选项：(a) 维持——hold 只对用户明确放弃审批（`auto_run` / `auto_write`）的会话，沙箱被视为替代审批的边界；(b) 改成 hold——「不问就能写项目根」就算免审，`shell` 的 `waives_approval` 改为 `sandboxed || auto_run`，代价是默认配置（`sandbox: auto`）的会话都会 hold。
2. **路径偏离研究稿。** `<bundle>/sock` 是链接，不是 socket 本体（§2.1）。外部工具照研究稿去连 `<bundle>/sock` 仍然能用（connect 跟随链接）——除非链接路径本身超过 104 字节，那时要 `realpath` 一下再连。
3. **MCP 工具不需要审批**（`Dispatcher::requires_approval` 默认 false，MCP manager 不覆盖它），也不 `waives_approval`，所以接了能写东西的 MCP server 的会话不 hold。
4. **重复归 `flooded`。** 也可以说重复是 `refused`。
5. **`~/.iota/run/` 里崩溃进程的 socket 不会被主动清扫**（判断 pid 是否存活需要 `kill(0)`，本 crate 禁 `unsafe`）；只在同 pid 再次 bind 时清掉。文件是 0 字节的 socket 节点。

## 8. 测试（红绿对照）

每一行的「变异」都实际做过：把实现改坏、对应测试变红、再还原。

| # | 性质 | 测试 | 变异 → 红 |
|---|---|---|---|
| ① | 空闲 → `woken`；忙 → `queued` 带位置 | `queue_tests::deliver_wakes_a_parked_reader_else_queues_with_its_place` | 忽略挂起的 reader |
| ② | 忙时在 round 边界、工具结果之后进入，工具不被打断 | `tools::tests::a_socket_message_lands_at_the_round_boundary_after_the_tool` | steer 只认 `Notice`（变成 `❯` 块） |
| ③ | 等审批时 `blocked`，且不进队列 | `inbound::tests::waiting_on_the_user_is_blocked` | 去掉 `NeedsInput` 检查 |
| ④ | 到达模型是 notice，不画 `❯`，`/new` 不执行，落盘为 notice | `tests/repl/messaging.rs::a_socket_message_is_a_notice_turn_and_leaves_nothing_behind`、`inbound::tests::a_message_is_a_notice_of_a_fixed_shape` | 主循环只认 `Notice` |
| ⑤ | uid 不符 → `refused`，inbox 什么都没收到 | `sock::tests::another_uid_is_refused_unread` | uid 检查恒真 |
| ⑥ | 目录 0700、socket 0600；链接目录照样可用 | `sock::tests::the_socket_is_private_and_goes_with_the_listener`、`a_linked_run_dir_works` | 去掉 chmod 0600 |
| ⑦ | 退出后 socket 与链接都被删 | 同 ④ 的 loop 测试 | `close` 不删链接 |
| ⑧ | `-m` 撞锁 → 投递 + 原样回执；被拒 → 回执 + 退出 1；无 socket → 说明 | `tests/cmd/session.rs::a_message_to_a_held_session_goes_to_its_socket`、`a_message_to_a_running_bot_goes_to_its_socket`；只读 `ok`：`sock::tests::an_unknown_receipt_passes_through` | 去掉 `holder` 检查 |
| ⑨ | `too-large` / `refused` / `flooded` | `sock::tests::the_line_is_bounded_and_checked`、`inbound::tests::floods_are_refused`、`queue_tests::deliver_refuses_past_the_cap_of_messages` | 去掉长度检查 / 去重 |
| ⑩ | 免审会话 hold：回执 `held`、`NeedsInput` + ping，丢弃的永不到模型；放行后正常应答；轮中扣下 | `tests/repl/messaging.rs::a_chat_that_waives_approval_holds_a_message_for_the_user`、`a_held_message_the_user_lets_through_is_answered`、`inbound::tests::a_holding_chat_answers_held`、`tools::tests::a_held_socket_message_never_reaches_the_turn` | 去掉 `held` 分支 / steer 不扣 |
| B8 | 旧链接不会把 `-m` 投进无关会话 | `tests/cmd/session.rs::a_stale_link_never_reaches_another_chat` | 取锁时不清链接 |
| B9 | 会话已关闭：不回执、断开 | `inbound::tests::a_closed_chat_answers_nothing`、`sock::tests::a_chat_that_cannot_take_closes_without_a_receipt` | 映射回 `refused` |
| B10 | 链接建不起来：聊天里说一次 | `tests/repl/messaging.rs::a_link_that_cannot_be_made_is_said_once` | 吞掉 `follow` 的错误 |
| C13 | hold 的触发器是 dispatcher 的回答；沙箱不算免审 | `sets::tests::the_dispatcher_says_whether_approval_is_waived` | `shell` 的 `waives_approval` 恒 false |
| D15 | 链接跟着 `/new`、`/session`、`/save` 搬 | `tests/repl/messaging.rs::the_link_follows_new_and_session`、`the_link_follows_save` | `follow` 不删旧链接 |
| D16 | 忙时扣下的消息在轮末放回队列、空闲时被问到并应答 | `tests/repl/messaging.rs::a_message_held_mid_turn_is_asked_about_after_the_turn` | 删掉轮末放回的那几行（脚本里轮后的读等不到它，8 秒后红） |
| D17 | bind 失败：一行写出真实原因的警告 | `cmd::tests::a_failed_bind_warns_with_its_cause` | — |
| — | 审批在结构上碰不到 | `queue_tests::a_message_never_answers_an_open_approval` | — |
| — | `IOTA_SOCKET` 进子进程 | `env::tests::a_child_is_given_what_it_is_given` | — |

## 9. 评审后的修订（2026-10-24）

fable 与 opus 各做了一次独立评审，两份都判「偏大」；所有者挑了要修的。删掉的：`ChildEnv::giving` 的去重与泛化、运行目录的两道防御、回执的解析再序列化、`Peer.uid` / `Listener::uid()`、`resolve` 的相对路径分支、`clear` 的公开可见性、非 unix 的 bind 桩、`fits` / `SUN_PATH`、`tool::sets::waives_approval`、`Steerer` / `TurnEngine` 上的 hold builder 与第二个扣留列表。修掉的：旧链接投错会话（取锁即清）、界面关闭时回执撒谎（改为断开）、链接失败无声（打一行）、hold 会话的回执（加 `held`）。补的测试见 §8 的 B8–D17。

## 10. 与 notice gate（X-84）：放行，不暂扣（rebase 到 main 后，2026-10-26）

main 上的 rate-limit 那条线引入了 `repl::noticegate::NoticeGate`：一轮被中断、或以 429 结束后，**job 的 notice** 不再自己开一轮，而是被放到一边，等用户说话（中断）或一次有节奏的探测（限流）。外部消息走的正是 notice 那条路（`InputKind::is_notice()`），所以 rebase 时必须决定它遇到 gate 怎么办。

**决定：放行，与用户输入同一条路径。**（rebase 时按派单方的判断做的，所有者可以推翻。）gate 压的是**自动工作**——没人要求的 job 完成通知；外部消息是另一个进程明确发来的一句话。更要紧的是，这个功能的目标用例是 bot，**bot 没有人会「说话」把它放出来**：被暂扣，它要等探测的那一分钟、乃至用量上限的几个小时，功能在目标场景里等于失效。

**怎么做的**（`src/repl/run.rs`）：

- 主循环里 `job_notice = notice && !flush && !message`：外部消息不是 job 的 notice，不进 `holds()` 的暂扣分支、不 `notice_taken`。之后它和打字一样：带上 gate 里压着的 notice（排在它前面）一起发出；答上了，`settle(Answered)` 解除 hold。
- 被 429 拒了：`Outcome::RateLimited { typed: !job_notice }`——它不是探测，和打字一样**不推迟**探测的时间表（只有更晚的 `retry-after` 能推后）；它不是「已经在 hold 下又被拒一次的自动轮」，所以照常打红色错误块与 ping，而不是一行暗色的 `⏸ Still …`。回滚交还的只是它带着的 job notice，消息本身随回滚丢掉，与打字的一行一样。
- 轮中被 steer 进来（`Steerer::drain`）：只有 `InputKind::Notice` 记进 `injected`。后面某一 round 的 429 回滚这一轮时，交还 gate 的只有 job 的 notice；外部消息与 steer 进来的打字一样随回滚丢掉——交给 gate 就等于让下一次探测来带它，也就是暂扣。
- 与 §4 的 hold（免审会话先问人）无关、互不影响：那条在 gate 之后照旧判断。

**测试**（`tests/repl/rate_limit.rs`，暂停时钟下的脚本 facade；每条都做过变异 → 红 → 还原）：

| 性质 | 测试 | 变异 → 红 |
|---|---|---|
| 限流 hold 期间投递 → 立即发出，带着压着的 notice，不等那一分钟 | `a_message_from_another_process_passes_a_rate_limit_hold` | `job_notice` 不排除 `message` |
| 中断 hold 期间投递 → 立即发出，带着压着的 notice | `a_message_from_another_process_passes_an_interrupt_hold` | 同上 |
| 在 hold 下被拒 → 探测仍在第一次拒绝后一分钟，不是两分钟 | `a_refused_message_leaves_the_probe_schedule_alone` | `typed: !notice` |
| 轮中 steer 进来又被 429 回滚 → 探测只带 job 的 notice | `a_message_steered_in_before_a_429_is_not_put_aside` | `injected` 不按 kind 过滤 |

**拿不准的一处**：中断 hold 是「用户按了 Esc，自动工作停到他说话为止」。外部消息走打字那条路，就会把 Esc 之后压着的 job notice 一起带出去，并且答上之后解除 hold——等于把一个外部进程当成了「用户说话」。另一种做法是让消息单独发出、不带压着的 notice、也不改变 hold，但那就不再是「与用户输入同一条路径」，且限流 hold 下一次成功的应答本来就说明限额已经恢复。v1 取前者，留给所有者。

