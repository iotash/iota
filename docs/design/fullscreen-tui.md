# 全屏（alternate-screen）TUI

Status: **已实现，唯一的终端（S1–S5，分支 `fullscreen`；S5 删除了 inline 终端）** · 计划：`~/.iota/reports/iota/tui-plan.md` · 决策记录：DIVERGENCES X-97–X-101（X-52、X-54、X-81 被取代，作为历史保留）· 契约摘要：ARCHITECTURE §12 · 人工检查：TUI-VERIFY §1–§4b

## 终端拥有屏幕

启动后第一帧进入 alt 屏（`ESC[?1049h`），退出时离开（`?1049l`）。之后所有字节都可以用绝对寻址：alt 屏没有历史、没有 reflow。inline 时代自研终端（X-54）的唯一理由——主屏上的绝对行号会在 resize 竞态中失效——在这里不存在，所以直接用 ratatui 自带的 `Terminal`（`Viewport::Fullscreen`）+ `CrosstermBackend`（`src/ui/runtime/screen.rs`）。backend 外面套一层 `Out`：尺寸取自最近一次 resize 事件，光标位置取自最近一次设置——**循环里零次光标查询**：启动、resize、退出都不发 DSR，对 DSR 不应答的终端也能运行。唯一的例外在循环之外：两个启动探测（OSC 11 查背景色，`osc.rs`；OSC 7501 查 herdr-core，`host/hc.rs`）各跟一个 `ESC[6n` 当读取终止符——在两者都不应答的终端上，第一个按键会被吞掉。

**布局**：自上而下 `[对话视口][帧]`。帧是 `FrameView` 的行（预览、分隔线、composer、状态行、surface；`render/frame.rs` 未改），固定在屏幕底部；视口占其余全部，对话贴着帧（内容少时空行在顶部）。

**每帧路径**：一次循环迭代 = 布局 → `Buffer::diff` → 一次写。X-55 的规则不变，只是从 `InlineTerminal::commit` 移到了 backend 之下的 `SyncOut::commit`：一次写超过 `SYNC_MIN`（1024 B）就包成一个 DEC 2026 同步块，小的裸发。循环外的一次绘制（会话选择器）也是一次写。

**resize = 整屏重画**：SIGWINCH → 记下尺寸 → 安静 `RESIZE_QUIET`（50 ms，纯节流）后交给屏幕 → 下一帧 `autoresize` 清屏并重画每个格子。没有拖动协议、没有 band、没有少几列的帧。

**流式输出**：跟随尾部、几何不变、新增 `0 < n < 视口高` 行时，用一次 `DECSTBM 1;h` + `SU n` + `ESC[r` 滚动视口，只写新行（`Screen::scroll_region`；先把上一帧缓冲上移 n 行、在 backend 静音时用 `current_buffer_mut()` + `apply_buffer()` 交给 ratatui 当作"上一帧"）。M2（tmux 3.8，120×40）：打 11 个字符 11 B；`stream 100 20` 5 558 B（inline 8 982 B）；30 步拖动 21 032 B（每步整屏重画，按设计）。

## 对话缓冲（`screen/conversation.rs`）

- 行 = 已渲染的 ANSI 字符串（T-05）+ 提交时的列宽；按宽度折行并缓存（`wrap_ansi`），只有可见行经过 `ansi_to_spans`。**不存 cell**。
- 上限 20 万行；超出丢最旧的，并在顶部放一行 `earlier output trimmed — /export has the full text`。M1：10 万行 RSS +30.7 MB，一帧约 1 ms。
- 视图要么跟随尾部，要么锚在某一行（`Pos{row, line}`）上；resize 后停在同一行。上滚后新输出不拖动视图，视口最后一行显示 `↓ N new lines · Ctrl+End`。
- 提交的行不经过暂存：实时 region 是 `Region::unstaged()`，提交即入缓冲；预览仍在帧里。（region 的暂存模式与 `TAIL_KEEP` 只剩它自己的测试在跑，S7 随暂存窗口一起删除。）
- resume（`/resume`、`-c`、`iota resume`、bot）只回放最后 3 轮（`RESUME_ECHO_ROUNDS`），缓冲从这 3 轮开始，上面什么都没有。**owner 已定（2026-11-15，"回放限定最大轮数，和 session 回放一致"）**：全量回放的代价随会话线性增长（S5 评审实测：300 轮 23 ms / 5 700 行，3 000 轮 230 ms / 57 000 行），而且回放的行退出时也会写出，一个反复重启的长寿会话（bot）每次都往 scrollback 里再加一份完整历史。有了上限，退出只写出这 3 轮和新的轮次（X-98/X-99）。

## 键（tui-plan D5 与 owner 的搜索键决定）

| 键 | 作用 |
|---|---|
| `PgUp` / `PgDn` | 半屏（Mac：Fn+↑/↓）；`PgDn` 到底恢复跟随 |
| `Ctrl+Home` / `Ctrl+End` | 第一行 / 尾部并恢复跟随 |
| `Ctrl+R`；`/`（已上滚且 composer 为空） | 对话搜索：View 面板的 JUMP 模式（`ascii_fold`、高亮、计数、`n`/`p`）；Esc 回到原位置，Enter 停在命中处 |
| `Ctrl+F` | **不是搜索**：保持编辑器的 forward-char（有测试钉住） |
| `Ctrl+O` | 写出（见下文），下一个键回来 |
| `Ctrl+Z` | 挂起（见下文"每一种退出方式"） |

不做 Claude Code 那种独立的 transcript 模式（`j/k/g/G`）：多一个模态，收益只是 less 的键位。

## 因 inline 而妥协的清单，现在的状态

| # | inline 时 | 现在 |
|---|---|---|
| C1 | resize 残差 R1–R8（X-52 的冻结预算） | 不存在：整屏重画（TUI-VERIFY §4.1 改成"resize 后无残留"） |
| C2 | 拖动期间帧短 2–8 列、2 s 后回满宽 | 任何时刻满宽；`DRAG_SETTLE`/`DRAG_MARGIN_MAX` 已删 |
| C3 | 启动与每次 resize 依赖 DSR；迟到应答（X-81） | 循环里零次光标查询；`InlineTerminal::ask` 已删（启动探测的终止符 DSR 除外，见上） |
| C4 | surface 关闭后 composer 暂时浮在底部之上；每次 insert 的光标闪烁窗口 | 帧永远贴底；没有 insert |
| C5 | 退出时要重画帧防重复（X-49）、`park_cursor` | 退出 = 离开 alt 屏 + 写出 |
| C7 | 对话内无搜索 | `Ctrl+R` / `/`（S3） |
| C12 | resume 只回放 3 轮 | ~~全量~~ 仍是 3 轮（owner 2026-11-15，X-98） |
| C14 | tmux `scroll-on-clear` 特判；`oneshot.rs` 自建 inline 终端 | 特判消失；会话选择器用同一个全屏终端（对话为空，退出时什么都不写） |
| C15 | 被杀后对话留在屏上（inline 的优点） | panic hook 与信号路径先恢复终端；SIGKILL 无解（见下表） |
| C6、C13 | 没有鼠标；surface 挤在帧行里 | **S6**：mouse capture、滚轮、应用内选择、浮层 surface |
| C8–C11 | 提交行是当时宽度的硬换行；暂存窗口；缓冲块攒完才出现；折叠不可逆 | **S7**：从源重渲染、删暂存窗口、实时块；C11 之后单独定 |

## 不归点：第一帧

全屏在**第一帧画出时**才进入 alt 屏（`Screen` 的 `Place::Main` → `Alt`）。在此之前终端上什么都不写，所以第一帧之前就结束的运行——`--help`、解析不了的配置、不存在的会话、`Tui::start` 之后第一帧之前的任何错误——都不会离开 shell 的屏幕，报错直接出现在 shell 里（L4 43）。没画过帧就退出时，缓冲里已有的行照常写进主屏，并且不会发出任何 alt 屏序列。会话选择器（`iota resume` 不带 id）也在全屏上运行；它的对话为空，离开时什么都不写，连分隔行也没有。

## 每一种退出方式

| 退出方式 | 终端 | 对话 |
|---|---|---|
| Ctrl+D、`/exit`、空闲时 Ctrl+C | 离开 alt 屏，关闭 raw/粘贴/焦点 | 写出（见下文） |
| SIGTERM 一次、SIGHUP（终端还在） | 同上，走正常收尾，退出码 130 | 写出 |
| 第二次 SIGTERM（收尾被卡住时，例如某个 MCP 服务器不肯退出） | `restore_terminal`：如果还在 alt 屏就离开，然后关闭 raw/粘贴，`exit(130)` | 如果 UI 已经关闭则已写出；否则**不写** |
| panic（release 是 `panic = "abort"`） | panic hook **先**调用 `restore_terminal`，再报告，所以报告出现在 shell 的屏幕上 | **不写**（缓冲在 UI 循环线程上，hook 拿不到） |
| UI 循环出错返回 | `TermGuard` 恢复终端 | 不写 |
| 终端挂断（pty 关闭、`tmux kill-server`） | 已经没有终端可以恢复；`HangupWatch` 让进程在 2 秒内退出（L4 29、42 B） | 无处可写 |
| Ctrl+Z 或 SIGTSTP，然后 `fg` | 先把终端交还（主屏、cooked、关闭粘贴和焦点），再 `SIGSTOP`；`fg` 之后重新接管，下一帧进入 alt 屏并**整屏重画**当时的尺寸（L4 44） | 不写；`fg` 后对话原样还在 |
| SIGKILL | **无解**：不运行任何代码，终端停在 alt 屏和 raw 模式。用 `reset`（或 `tput rmcup; stty sane`）恢复 | 屏幕上没有，但会话日志里有 |

凡是"不写"的情况，对话都不会丢：会话日志（`messages.jsonl`）每追加一批就同步一次，`iota resume` 会把整段对话载回缓冲，`/export` 能导出全文。

## 写出策略（Ctrl+O 与退出）

- **每行只写一次。** Ctrl+O 写出此前没写过的行，退出时写剩下的；第一段（第一次有行可写时）前面有一行 `┈┈┈┈ iota transcript ┈┈┈┈`。原生 scrollback 里对话正好一份、顺序正确，Cmd+F 和 tmux copy-mode 不会重复命中。计划 D2 原本建议每次全量重写并接受重复；没有采纳，因为重复本身没有好处。
- **宽度**：每一段按写出那一刻的终端宽度折行。之后的 resize 不会回头改已经写出的行，那些行归终端所有，由终端自己 reflow。退出时写的那段用退出时的宽度。
- **写出期间**：不画任何东西，新输出照样进缓冲；下一个键（不管是什么）回到全屏并整屏重画，这个键本身被吞掉。如果写出期间退出，就不再发第二次 `?1049l`。
- **与搜索叠加**：搜索中按 Ctrl+O，写出的是**原样的行**（不带高亮）；回来后搜索仍在原处。搜索中退出，同样写原样的行。
- **上限**：写出之前就被 20 万行上限裁掉的行，用"更早内容已裁剪"那一行代替。
- **挂起**（Ctrl+Z/SIGTSTP）不写出：`fg` 会把全屏原样带回来。

## 事件源（为什么用 `use-dev-tty`）

crossterm 0.29 默认的 unix 事件源（mio、边沿触发）有两处会让终端失控：
1. 同一次 poll 里 resize 和按键一起就绪时，它先返回 resize，丢掉 tty 的就绪状态。边沿触发的注册在**新**输入到来之前不会再报告，所以那个键要等到下一次按键才被读到（L4 42：40 轮里有 6–8 个键卡住；场景 39 的空闲 Ctrl+D 就是这样"丢"的）。
2. 读取循环只在 `WouldBlock` 或解析出事件时才退出。如果 tty 在 `HangupWatch` 判定"可读"之后才挂断，它会对着 EOF 一直空读，于是进程成了孤儿（PPID 1、`R` 状态、100% CPU）。L4 42 B 在旧事件源上 120 轮复现了 5 次，换成新事件源后 160 轮 0 次。

`use-dev-tty` 换成 crossterm 另一个电平触发、先读 tty 再读 SIGWINCH 管道的事件源，两处都消失了。它在零超时下什么都不读，所以 `HangupWatch` 等待之后，用 1 ms（`CROSSTERM_SLICE`）去问 crossterm。（S4 时 inline 也换了；S5 起只有一个世界。）

## S6、S7 从这里继承什么

- **S6（鼠标）**：现在不开 mouse capture，终端原生选择只覆盖当前一屏（TUI-VERIFY §4b.4）。视口的滚动 API（`ViewOp::HalfUp`…、`Conversation::scroll_up/down`、锚点）就是滚轮要接的地方；搜索的高亮（`Highlight`）可以借给选择用。帧是最后画的，浮层 surface 需要视口让出矩形——`draw_frame` 里 `conv`/`frame` 两个 `Rect` 是唯一的布局点。
- **S7（从源重渲染）**：缓冲的行仍是提交时宽度的字符串（变窄折行、变宽不重排）；`Row` 是将来"块"的位置。`Region` 的暂存模式与 `TAIL_KEEP`、`chunk_overflow`（T-06）只剩 region 自己的测试，S7 一起删掉。写出策略不变：每行只写一次，按写出时的宽度。
