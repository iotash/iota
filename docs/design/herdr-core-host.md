# herdr-core host：iota 在 hc 终端里报告状态

Status: **Shipped（未发版）** · 日期：2026-11-14 · hc 0.1.21、herdr 0.9.5、Claude Code 2.1.325 · 调研：`~/.iota/reports/iota/herdr-core-host.md`

herdr-core（`hc`）是 herdr 桌面 app 背后的终端核心，是新产品，不是 herdr 多路复用器的新版本（`hc --skill`：*"It is not `herdr`, the older terminal multiplexer."*）。两者都在用，iota 两个都支持：`src/host/hc.rs` 的 `HcHost`（`name() = "herdr-core"`）与 `src/host/herdr.rs` 的 `HerdrHost` 并存。

## 1. 协议（按实现）

hc 没有上报状态的 socket 方法（`hc schema` 里只有 `terminal.* / machine.* / file.* / doc.* / events.subscribe / server.*`）。它从终端输出里读程序自己的 OSC 7501（"Program Status Protocol"，rev 0.4）。iota 写的只有这两种：

| iota | 字节 |
|---|---|
| `Busy` | `ESC ] 7501 ; state=working:app=iota ESC \` |
| `NeedsInput` | `ESC ] 7501 ; state=blocked:app=iota ESC \` |
| `Error` | `ESC ] 7501 ; state=error:app=iota ESC \` |
| `Idle` | `ESC ] 7501 ; state=idle:app=iota ESC \` |
| 退出（`Closer`） | `ESC ] 7501 ; state=clear ESC \` |

- 每次上报都替换整条记录，所以每次都带 `app=iota`。
- `blocked` 不带 `kind`：`State::NeedsInput` 不知道原因。
- `Idle` 报 `idle`，不报 `done`：`done` 的意思是"完成了、用户还没看"，桌面 app 怎么渲染它还不知道。
- 不设 `title`/`msg`：`StateReporter` 只拿到一个 `State`。要设它们，就得给 Presenter 加新的输入。
- 字节由 `host::hc::tests::each_state_is_one_exact_report_and_close_clears` 钉住。

`blocked` 期间 `hc send` 会被拒（`kind: blocked`，exit 9），监督者没法往审批框里打字。`hc keys` 和 `send --force` 不检查状态。

## 2. 检测与排序（问终端，2026-11-14 第二轮；调研：`~/.iota/reports/iota/hc-detector.md`）

`detect_hc`：

1. `HC_TERMINAL_ID` 为空 → 不是 hc，**一个字节也不写**。
2. 否则问终端（`Probe::program_status`，真实实现是 `host::hc::probe_tty`）：向 `/dev/tty` 写 `ESC]7501;?ESC\` 紧跟 `ESC[6n`，读到 DSR 的 `R` 为止，上限 100 ms。机制就是 OSC 11 那次往返（`ui::runtime::osc::round_trip`），两者在 UI 循环接管输入之前先后串行。
   - 应答里有 `ESC]7501;?` → `Some(true)`，是 hc。hc 0.1.21 实测应答：`ESC]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth ESC\`，0.1–0.3 ms。
   - 只有 DSR → `Some(false)`，不是 hc，交给后面的 detector。
   - 打不开 tty、超时、或**本进程不在终端的前台进程组** → `None`，退回环境规则：`TERM_PROGRAM != "herdr"`。没有 tty 或超时，host 也写不出东西，这条规则只挡住泄漏的 `HERDR_*`；后台进程却写得出（没有 `TOSTOP`），所以 iota 自己的子进程要靠扣下 id 来挡（§4）。
3. `DETECTORS = [hc, herdr, cmux]`，只取第一个命中（innermost-exclusive 不变）。

为什么不看环境变量：变量只能说明祖先里有谁，说明不了 tty 对面是谁。hc 的 `TERM_PROGRAM=ghostty` 是 0.1.21 的硬编码，不是契约——桌面 app 已经通过 `restart_env` 请求 `TERM_PROGRAM=herdr`。

前台检查是实测出来的：raw mode 是一次 `tcsetattr`，后台进程组调用它会被 `SIGTTOU` 停住。shell 工具的命令有自己的进程组，所以一个会探测的嵌套 `iota -m` 会停在 `T` 状态，永远不返回。

| 场景 | 探测 | host |
|---|---|---|
| 纯 hc | 应答 | herdr-core |
| hc 嵌在 herdr 里（core 从 herdr pane 启动，`HERDR_*` 泄漏） | 应答 | herdr-core（hc 在 herdr 前，挡住错的 pane） |
| herdr ≥0.9.2 嵌在 hc 里 | 只有 DSR（**herdr 是否透传 7501：未知**，未实测；0.9.5 二进制里搜不到 `7501`） | herdr |
| 老 herdr（不设 `TERM_PROGRAM`）嵌在 hc 里 | 同上，**未知** | herdr |
| tmux 嵌在 hc 里（`TERM_PROGRAM=tmux`，`HC_TERMINAL_ID` 被继承） | 只有 DSR（实测 tmux 3.8） | 无，ANSI 照常发 OSC 9;4 |
| hc 嵌在 tmux 里（在 tmux pane 里 `hc attach`） | 应答（iota 的 tty 是 hc 的终端） | herdr-core |
| 没有 hc | 不问 | 不变 |

矩阵由 `host::hc::tests::the_detection_matrix_yields_one_host_each` 跑 `detect()`，探测结果按表注入，所有错行一次报出。端到端：

- `tests/host/hc.rs::inside_hc_started_from_herdr_only_hc_hears`：core 带着指向 mock 的 `HERDR_*` 启动，hc 拿到状态，mock 一个请求都没收到。
- `…::inside_tmux_inside_hc_the_terminal_keeps_the_progress`：hc 终端里的 tmux 中跑 iota，`hc get` 是 `reported:false`，pane 收到 OSC 9;4，没有 7501。

残余风险：tmux 嵌在"从 herdr pane 启动的 hc"里 → 探测为否 → `detect_herdr` 命中泄漏的 pane。需要三层同时成立，只记在这里。

`tests/ui_tmux/lib.sh` 不再 unset `HC_TERMINAL_ID`：场景里的 iota 在 tmux 里，探测为否，碰不到开发者的终端。

## 3. 写入路径（风险 1：无头没有 facade）

`HcHost` 是唯一的 `InBand` host（`host::InBand`）：

- **交互**：`Presenter::show_turn_on(ui)` 把 facade 交给它（`write_through`）。之后每次上报都走 `Ui::program_status` → `UiMsg::ProgramStatus` → UI 循环在两帧之间按顺序原样写出，不去重（去重在 Presenter 里）。`pres.close()` 在 `ui.close()` 之前，所以 `state=clear` 会在 `Quit` 之前进邮箱。
- **无头（`-m`）**：`Presenter::over(host, None, false)` 拿不到 facade（探测照样在 `host::detect` 跑一次）。host 在第一次上报时打开 `/dev/tty` 直接写，打不开就什么也不做（只记一条 `debug!`）。stdout 可能是管道，但记录属于终端，所以写 `/dev/tty`。`tests/host/hc.rs::a_headless_run_reports_through_the_tty`：`-m` 的输出重定向到文件，`hc get` 看到 `working`；回复完成后记录被清掉，终端里的 shell 还活着。

Presenter 的降级不变：`HcHost` 是第一个 `StateReporter`，ANSI 的 OSC 9;4 就不再写了（和在 herdr 里一样）；OSC 9 ping 仍由 ANSI host 发。jobs→Busy 的重写、去重、对话框→`NeedsInput` 都在 Presenter 里，没有改。

## 4. 风险 2：子 agent 往同一个 tty 写 7501（实测）

Claude Code（≥2.1.295，本机 2.1.325）写的是**根记录**：`state=…:app=claude-code`，不带 `id`（`id` 只用于它的子 agent）。它只在 `ESC]7501;?` 探测收到应答后才开始写；应答从 stdin 读，而 iota 的 shell 工具给子进程的 stdin 是 `/dev/null`。但任何直接写 `/dev/tty` 的程序都能写进来：工具子进程与 iota 同一个 session，只是进程组不同（`process_group(0)`），控制终端还是那个。

在隔离 core 里测到：

1. 子进程写的根记录**会**替换 iota 的记录。换成独立进程组、raw 模式，或在 iota 重绘的同时写，结果都一样。
2. 子进程退出后**不会**恢复：记录一直是 `claude-code idle`，直到 iota 的下一次状态变化。
3. 它把 `turn` 推进了一格（状态离开了 `working`），所以监督者的 `hc wait` 会以为 iota 的这一轮已经结束。
4. 带 `id=…` 的记录不碰根记录。
5. 时序：如果子进程在审批被应答的同一刻写，它的字节会先于 iota 的 `working` 落地（UI 循环要等下一轮才写），随即被 iota 覆盖。只有晚一点写，才会盖住 iota 的记录。

在 iota 里，这个窗口从子进程写入开始，到 iota 的下一次状态变化结束，通常就是这一轮结束时报 `Idle`。`tests/host/hc.rs::a_child_writing_7501_replaces_the_record_until_the_next_state` 把这个行为钉住了：子进程的记录出现，`turn` 前进，轮结束后记录回到 `iota idle`。

代价有两项：

- 监督者的 `hc wait` 可能提前返回。
- 一个**后台** job 里的子 agent，如果在 iota 的审批框打开时写了根记录（比如 `idle`），记录就不再是 `blocked`。`hc send` 因此不再拒绝，监督者的输入会打进审批框。

**iota 自己作为子进程，已经修掉。** `HcHost` 实现 `ChildEnvironment`，扣下 `HC_TERMINAL_ID`，做法和 herdr 扣下 `HERDR_ENV` 一样。没有 id 的嵌套 `iota -m` 既不探测，也不上报。不扣的话（变异实测）：

- 加前台检查之前，子 iota 在探测时被 `SIGTTOU` 停住。
- 加了之后，它退回环境规则，写出 `working` 和 `clear`，父 iota 的记录变成 null。

`<environment>` 多一行 `hc note`：id 是有意去掉的，`hc` 命令要显式带 terminal id，`hc run` 要带 `--parent <id>`。实测去掉 id 后，`hc get t1` 和 `hc label t1` 都能用；`hc run --bg` 起的终端 `parent = None`。`tests/host/hc.rs::a_nested_headless_iota_leaves_the_chat_s_record` 钉住了这一点：子 iota 跑完后，父记录仍是 `iota working`。

**v1 接受这个行为，不做补救。** 可选的做法交给 owner：

- (a) 接受（现状）。代价是工具调用期间监督者可能看错状态，`hc wait` 可能提前返回。
- (b) 每次工具调用结束后重发一次当前状态。这要绕开 Presenter 的去重，或者给 host 加一个"工具结束"信号；窗口缩短到单次调用的时长。
- (c) 不让子进程拿到控制终端（`setsid`）。这会改 shell 工具和沙箱，而且会让需要 tty 的命令失效。
- (d) 用子记录：iota 写自己的根记录，期望子 agent 都用 `id`。这一点 iota 管不了，Claude Code 写的就是根记录。

## 5. 不做的，和告诉不了桌面 app 的

- `Notifier`：ANSI host 保留 OSC 9（hc 不会因 OSC 9 产生事件；app 显不显示，不知道）。
- `BackgroundReporter`：hc 自己应答 OSC 11。
- `SessionReporter`：协议里没有 session 键。`hc label` 能用，但没有证据表明 app 会显示它。
- `--on <machine>`：协议是带内的，远程终端不需要 iota 做任何事。
- app 无法得知：`blocked` 的原因（`kind`）、"完成但未读"（`done`）、任务标题和消息（`title`/`msg`）、session id。前三项要等 app 装好、能看到它怎么渲染再定；session id 在协议里根本没有对应的键。
- 扣下 `HC_TERMINAL_ID` 挡不住自己直接写 `/dev/tty` 的子进程（§4）。
