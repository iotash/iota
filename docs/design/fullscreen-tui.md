# 全屏（alternate-screen）TUI

Status: **已实现，唯一的终端（S1–S6；S5 删除了 inline 终端，S6 加了鼠标、应用内选择与浮层 surface）** · 计划：`~/.iota/reports/iota/tui-plan.md` · 决策记录：DIVERGENCES X-97–X-102（X-52、X-54、X-81 被取代，作为历史保留）· 契约摘要：ARCHITECTURE §12 · 人工检查：TUI-VERIFY §1–§4b

## 终端拥有屏幕

启动后第一帧进入 alt 屏（`ESC[?1049h`），退出时离开（`?1049l`）。之后所有字节都可以用绝对寻址：alt 屏没有历史、没有 reflow。inline 时代自研终端（X-54）的唯一理由——主屏上的绝对行号会在 resize 竞态中失效——在这里不存在，所以直接用 ratatui 自带的 `Terminal`（`Viewport::Fullscreen`）+ `CrosstermBackend`（`src/ui/runtime/screen.rs`）。backend 外面套一层 `Out`：尺寸取自最近一次 resize 事件，光标位置取自最近一次设置——**循环里零次光标查询**：启动、resize、退出都不发 DSR，对 DSR 不应答的终端也能运行。唯一的例外在循环之外：两个启动探测（OSC 11 查背景色，`osc.rs`；OSC 7501 查 herdr-core，`host/hc.rs`）各跟一个 `ESC[6n` 当读取终止符——在两者都不应答的终端上，第一个按键会被吞掉。

**布局**：自上而下 `[对话视口][帧]`。帧是 `FrameView` 的行（预览、分隔线、composer、状态行、surface；`render/frame.rs` 未改），固定在屏幕底部；视口占其余全部，对话贴着帧（内容少时空行在顶部）。

**每帧路径**：一次循环迭代 = 布局 → `Buffer::diff` → 一次写。X-55 的规则不变，只是从 `InlineTerminal::commit` 移到了 backend 之下的 `SyncOut::commit`：一次写超过 `SYNC_MIN`（1024 B）就包成一个 DEC 2026 同步块，小的裸发。循环外的一次绘制（会话选择器）也是一次写。

**resize = 整屏重画**：SIGWINCH → 记下尺寸 → 安静 `RESIZE_QUIET`（50 ms，纯节流）后交给屏幕 → 下一帧 `autoresize` 清屏并重画每个格子。没有拖动协议、没有 band、没有少几列的帧。

**流式输出**：跟随尾部、几何不变、新增 `0 < n < 视口高` 行时，用一次 `DECSTBM 1;h` + `SU n` + `ESC[r` 滚动视口，只写新行（`Screen::scroll_region`；先把上一帧缓冲上移 n 行、在 backend 静音时用 `current_buffer_mut()` + `apply_buffer()` 交给 ratatui 当作"上一帧"）。M2（tmux 3.8，120×40）：打 11 个字符 11 B；`stream 100 20` 5 558 B（inline 8 982 B）；30 步拖动 21 032 B（每步整屏重画，按设计）。

## 对话缓冲（`screen/conversation.rs`）

- 缓冲存的是**块**（`ui::block::Block`，S7a，见下节"块模型"）：固定的行，或者带着源、按宽度布局的块。每块在某个宽度下的显示行在第一次被画到时算出，连同宽度一起缓存；只有可见行经过 `ansi_to_spans`。**不存 cell**。
- 上限 20 万**块**；超出丢最旧的，每块整个丢（一张表不会剩半张），并在顶部放一行 `earlier output trimmed — /export has the full text`。M1：10 万行 RSS +30.7 MB，一帧约 1 ms（行块；S7a 没有重测）。
- 视图要么跟随尾部，要么锚在某一块（`Pos{row, line}`：块号、块内行）上；resize 后停在同一块（块内行夹到新高度）。上滚后新输出不拖动视图，视口最后一行显示 `↓ N new lines · Ctrl+End`。
- 提交的块不经过暂存：实时 region 是 `Region::unstaged()`，提交即原样入缓冲（不再在提交时按 width−1 预折行）；预览仍在帧里。（region 的暂存模式与 `TAIL_KEEP` 只剩它自己的测试在跑，也是唯一还把块压平成"提交时宽度的行"的地方；S7b 随暂存窗口一起删除。）

## 块模型（S7a，`ui/block.rs`）

宽度变化时，对话从**源**重新布局，不再把别的宽度的行盲折。owner 的原话："拖放窗口时，输出区域元素的重绘：启动的 banner、表格、列表的左部换行对齐缩进、代码块、引用块"。

- **两种块**。`Block::Row(String)`：一条逻辑行，原样提交——段落行、标题、通知、活动行、分隔空行。它不依赖宽度：比视口宽就由屏幕折行（`wrap_ansi`），不宽就整行显示，所以变宽时段落会重新展开。`Block::Laid(Arc<dyn Layout>)`：带着源，`Layout::lines(width)` 在画的宽度下布局。
- **块的种类就是源的类型**（`Layout` 的实现），下游没有任何代码按种类分支，所以不另带标签：助手 markdown 的五种缓冲块是 `markdown::Fragment`（代码、表格、列表、引用、行间公式）；用户块（`runtime/handle.rs` 的 `UserBlock`、回放的 `EchoUser`）；banner（`repl/render/banner.rs` 的 `Banner`）；文件改动的 diff 列表（`repl/render/group.rs` 的 `DiffListing`）；结构化错误（`transcript.rs` 的 `ErrorRows`）。图片仍是行：半块像素画按提交时的宽度缩放，不重排。
- **提交接口**：`Ui::print(Vec<Block>)`（原 `print_lines(Vec<String>)`）。markdown 的 `Writer` 自己不再按宽度布局任何东西：缓冲块关闭时以 `Fragment` 交给 sink（`Sink::write_block`）；文本 sink 按自己的宽度渲染写出。生产环境里唯一的文本 sink 是引用块的子文档（`blocks/quote.rs` 的 `BufSink`），另外就是测试用的 `new_writer_to`。终端的 sink（`repl/render/uisink.rs`）和回放的 sink（`replay.rs`）保留源。
- **缓存与惰性**：缓冲在第一次需要某个宽度时调用 `Layout`，结果连同宽度缓存在块上；宽度一变，下一次画只重新布局它碰到的块（视口里的、滚动经过的），其余的等视图到达它们时再说。
- **搜索**：命中记为（块号, 块内第 k 次出现）。搜的文本与宽度无关：行块用它的原文（折行不会切断匹配）；布局块用它**第一次**布局时、在那个宽度下的纯文本行，以 `\n` 连起来，存下后不再变。所以 resize 既不重扫，也不为搜索布局任何块。跳转只布局落点那一块和它周围一屏，在新宽度下数出第 k 次出现在哪一行。搜索开着时 resize，视图会回到当前命中。代价有两个：布局块多存一份纯文本；表格在新宽度下如果把一个词折断，第 k 次出现可能数不到，这时跳到块的第一行。
- **上限按块计**，不按行或字节：一个块不论多高都算 1。评审 S2 之后，diff 只存预算内的行（`DiffRows`），剩下可能很高的块只有模型输出本身。评审实测：3 万个布局块全部缓存后 RSS +38 MB。不按源行数加权，是因为那需要每种 `Layout` 报告自己的大小，而 20 万块的上限在实际会话里远远够不到。

五个元素在新宽度下做什么：

| 元素 | 新宽度下 |
|---|---|
| banner | **重新布局**，不截断：同一份 facts，宽了卡片回来、窄了三行裸排、目录按新宽度从中间截。截断是不可逆的——窄过一次再拉宽，路径就永远是截过的；重新布局的代价是零（`Banner::lines` 本来就是宽度的纯函数） |
| 表格 | 列宽按新宽度重新水位填充（width−1 律照旧）；宽了长单元格回到一行，窄了在列内折行，框永远完整 |
| 列表 | 条目文本按新宽度在悬挂缩进下折行（`build_list` 的 `room`；嵌套列表在父缩进剩下的宽度里布局，至少 8 列）；宽了每个条目回到一行 |
| 代码块 | 长行在两列缩进（gutter）下折行，绝不从第 0 列开始；高亮只做一次（与宽度无关），只有折行随宽度。markdown 代码块本身没有背景；**带背景的是 diff 列表**：± 行的背景块按新宽度铺满，代码按新宽度截断，行号 gutter 保持 |
| 引用块 | 子文档按 width−2 重新渲染，`│` 画在每一个折出来的行上，行补齐到宽度 |
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

鼠标（`mouse:` 开着时，默认开）另见下文"鼠标"：滚轮滚对话，点击放光标 / 选行，拖选复制。

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
| C6 | 没有鼠标 | 滚轮、点击、拖选 + 双击选词 + 松开复制、跨屏自动滚动（S6，见"鼠标"） |
| C13 | surface 挤在帧行里：View `min(lines,15)`、picker 预览 `[4, h−12]` | surface 占视口：视口把帧之外的所有行让给它（S6，见"surface 占视口"） |
| C8 | 提交行是当时宽度的硬换行；变宽不重排、变窄盲折 | **S7a 完成**：缓冲存块，宽度变化时从源重新布局（见"块模型"） |
| C9–C11 | 暂存窗口；缓冲块攒完才出现；折叠不可逆 | **S7b**：删暂存窗口、实时块；C11 之后单独定 |

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

## 鼠标（S6，tui-plan D1/D3）

**默认开 capture，配置可关。** 终端不再滚动之后，滚轮是唯一自然的滚动手势（Claude Code 默认同样）。进入 alt 屏时请求 `?1000h`（按下/松开）、`?1002h`（按住时的移动，即拖动；不要 1003 的每次悬停）、`?1006h`（SGR 坐标，没有 223 列上限）；**每一条离开 alt 屏的路**——写出（Ctrl+O）、挂起、退出、`restore_terminal`（panic、第二次 SIGTERM）——都先关掉它，所以主屏上终端自己的选择永远可用。

| 位置 | 滚轮 | 左键 按下 / 拖动 / 松开 |
|---|---|---|
| 对话视口 | 一格 3 行；上滚暂停跟随，滚到底恢复跟随 | 选择：拖选、双击选词、三击选整行；松开复制 |
| 视口末行 `↓ … Ctrl+End`（已上滚时） | — | 回到尾部 |
| 搜索行 | — | 已应用的查询重新可编辑（保留查询） |
| surface | 移动列表（View：3 行） | 不在光标上的行：光标移过去；光标所在的行：**选中**（Enter 的路径；多选是 Space 的勾选）；tab 标签：切换；提示行：聚焦搜索框 |
| composer | 滚对话 | 光标放到指针下（宽字形的第二列算这个字形） |

**一次误点不会回答对话框**：surface 的一行要点两次（第一下移光标，第二下选中）——审批对话框里"Always allow"不该被一次手滑选中。

拖动按住在视口**顶行**或**末行及以下**（帧里）时，每 40 ms（`EDGE_STEP`）滚一步，指针不动也继续，所以一次选择能跨很多屏（opencode #61053 的反例）。顶行每步 1 行；视口下方按指针越过末行的距离加速，每步 1、3、5 … 行（帧的行数就是这个距离）。即便如此，**几百屏的大段复制请用 Ctrl+O 加终端自己的选择**：顶行方向最快每秒 25 行，500 屏要几分钟。时钟在循环的 poll deadline 里（W10 照旧有限）。

**松开丢了**（指针在窗口外松开、Cmd+Tab 切走）：任何按键、窗口失去焦点、resize 都结束拖动——按键丢掉选择，失焦和 resize 当作松开。否则边缘滚动会一直滚下去（S6 评审 SF1）。

**按下就固定视图**：在对话上按下开始选择时视图被"按住"（`Conversation::hold`）：跟随中的视图锚在当前顶行，新输出不移动指针下的文字；末行照旧报 `↓ N new lines`。松开时放开：仍显示最后一行就恢复跟随，否则停在原处。

**抖动不算拖动**：同一行上至少跨 2 列（或换了行）才算选择；1002 下点击时抖一格只是点击，不会覆盖系统剪贴板。

## 应用内选择（`render/selection.rs`）

- **点是缓冲里的格，不是屏幕的格**：(行号, 折行内第几行, 显示列)。行号按提交编号，所以 20 万行上限丢旧行不移动它；新输出到来时选择留在原文上。resize 换了折行，选择清掉。
- **画**：只改屏幕 cell 的 `REVERSED`，行本身不动——**写出（Ctrl+O、退出）永远不带选择的样式**（`screen::tests::the_selection_is_drawn_reversed_and_never_written_out`）。
- **复制的内容**：去掉 SGR 的纯文本；**屏幕**把一行折成的几段拼回成那一行（缓冲按 `Block::display` 标出哪些显示行是续段）；其余的行，包括块与块之间、布局块自己的行（表格的、代码块的），之间都是 `\n`；行尾空白去掉。三击选指针下那一行连同它的续段，不是整块（S7a 评审 B1）。双击的"词"＝字母数字加路径/URL/标识符的标点 `_-./~+:@\`，句末的 `.` `,` `:` 不算。
- **按键清掉选择**（也结束一次没收到松开的拖动）；单击（不拖，或只抖一格）什么也不选、不复制。
- **三条复制路径**（`runtime/clipboard.rs`，Claude Code 的做法）：不在 SSH 下用本机工具（`pbcopy`；`wl-copy`/`xclip`/`xsel`；`clip`）；在 tmux 里（`$TMUX`）写 `tmux load-buffer -w -`（`-w` 让 tmux 按 `set-clipboard` 再交给外层终端）；SSH 下且不在 tmux 里写 OSC 52；本机工具一个也没有时也退回 OSC 52。外部工具在自己的线程里跑，卡住也不拖住循环，**跑完把结果报回循环**：只有某个工具成功才说 `copied`；全部失败（没有 `$DISPLAY` 的 `xclip`、出错的 `pbcopy`）就落到 OSC 52。OSC 52 自己报不回结果，所以状态行写明 `via OSC 52`。OSC 52 序列有上限 `OSC52_MAX`（100 000 字节，约 73 KB 文本；hterm、tmux 也在这个量级截断）：超过就不发，状态行写 `not copied: N characters is too large — use Ctrl+O`。
- **提示**：复制后状态行 4 秒内显示 `copied N characters · Shift+drag selects in the terminal`（只走了 OSC 52 时是 `copied N characters via OSC 52 · …`；iTerm2——`TERM_PROGRAM=iTerm.app` 或 `LC_TERMINAL=iTerm2`——显示 `Option`）。一句话，不是段落。

## 关闭开关（`mouse: false`）

`~/.iota.yaml`（或项目的 `.iota.yaml`）顶层 `mouse: false`。之前没有任何 UI 设置项，这是最小的那一个：顶层标量，和 `write_roots`/`outside_writes` 并列，YAML 1.1 布尔，栈里最后一个写它的文件说了算。关了之后**一个鼠标序列都不写**（不请求也不"关闭"），终端自己的选择照旧，跨屏选择先 Ctrl+O 写出。**滚轮归终端，但终端拿它做什么不由 iota 决定**：tmux 3.8 在 alt 屏里什么也不做（L4 47 断言了：不进 copy mode、不变成 ↑↓）；不少终端默认在 alt 屏里把滚轮变成 ↑/↓——iTerm2 的 "scroll wheel sends arrow keys in alternate screen mode"、Terminal.app、WezTerm 的 alternate scroll——而 ↑/↓ 在空 composer 上会翻输入历史（X-89），这正是 X-102 不用 1007 的理由。这些终端里 `mouse: false` 的滚轮会翻历史而不是滚对话；**没验证**（这里没有图形终端），列在 TUI-VERIFY §4c.7。键盘滚动不受影响。会话选择器（`iota resume`）同样遵守。

## surface 占视口（C13）

S5 时 surface 挤在帧行里：帧变高会把对话推走，所以 View 限 `min(lines,15)`、picker 预览限 `[4, h−12]`。现在 surface 画在视口的位置上：`Model::frame_view` 先量出帧自己的行（暂存/预览、间隔、队列、两条分隔线、composer），**剩下的所有行都是 surface 的**（`SurfaceState::set_room`），视口让出它们；关掉 surface，视口拿回来，对话停在原处（跟随时仍是尾部，锚定时仍是那一行）。面板的默认行数就是这块空间，显式 `height` 也以它为上限；picker 预览夹在 `[min(4, room), room]`。room 是 `Option`：只有没被循环布局过的 surface（单测里的）是 `None`，才用 tabbed.go 的 10 / View 15。**room 至少是 `SURFACE_MIN_ROWS`（5 行）**：帧自己的行就把屏幕占满时（8–9 行的窗格里写满 5 行草稿、再有队列或预览），帧比屏幕高，这时屏幕保留帧的**底部**——surface 和它的提示行——让上面的预览和 composer 让出行（surface 开着时 composer 本来就不接键）；点击按被截掉的行数换算（`FrameMap::cut`）。否则审批框会整个落在屏幕外，Enter 回答的是看不见的选项（S6 评审 SF2）。surface 仍在 composer 下面：一个审批对话框要回答的那次工具调用就在它上方的视口里，盖住它反而看不见。

## 兼容性（本机实际试过的）

| 环境 | 滚轮 | 拖选 + 复制 | 点击 | 试过的方式 |
|---|---|---|---|---|
| tmux 3.8，`mouse on` | ✓ | ✓（进 tmux paste buffer） | ✓ | L4 47：外层 tmux 的 pane 挂着内层 tmux，报告写进外层 pane，由内层 tmux 按它的绑定转发 |
| tmux 3.8，`mouse off` | ✓（仍转发） | ✓（按下、拖动、松开都转发；评审实测） | ✓（同左） | L4 47 断言滚轮；`mouse` 只是 tmux **自己**用不用鼠标，pane 请求了上报，tmux 照样替它向终端请求并原样转发。计划里"tmux 无 `mouse on` 吞滚轮"在 3.8 上不成立 |
| tmux，iota `mouse: false` | 归 tmux：alt 屏下 `send -M` 给一个没请求上报的 pane，什么也不发生 | 终端原生（Shift） | — | L4 47 断言：`#{mouse_any_flag}`=0，Ctrl+O 往返后也没有 `?1000h`/`?1006h`，滚轮后视图不动、不进 copy mode、composer 没被历史填上 |
| herdr（本机 pane，SSH 会话里） | ✓ | ✓（走 OSC 52 路径） | — | 在临时 split 里跑 iota，`herdr pane send-text` 写入 SGR 报告、`herdr pane read` 读屏，试完关掉。测的是 herdr 把输入原样交给 iota；herdr 自己把真实鼠标转成上报、OSC 52 是否传到客户端，**没试** |
| Ghostty | 未试 | 未试 | 未试 | 本会话没有图形终端可驱动 |
| iTerm2 | 未试 | 未试 | 未试 | 同上；上报是逐 profile 设置（opencode #49622），关着时什么报告都没有，键盘照旧（与"什么也收不到"同一条路） |
| `tmux -CC` | 未试 | 未试 | 未试 | Claude Code 文档说 alt 屏与上报都不可靠；收不到报告时退回键盘 |

收不到报告的终端（iTerm2 profile 关了上报、`tmux -CC`、IDE 终端）上，鼠标这一层什么都不做，键盘的一切照旧——从不卡住。逐项手验列在 TUI-VERIFY §4c。

## S7b 从这里继承什么

- **暂存窗口**：`Region` 的暂存模式、`TAIL_KEEP`、`tail_keep`、`rebalance`、`retrim`、`fit_width`、残留（residue）与 `chunk_overflow`（T-06）只剩 region 自己的测试在跑。S7a 让删除更容易：实时路径（`commit` 的 unstaged 分支）已经不碰 `tail`，暂存模式是唯一把块压平成字符串的地方——删掉它就是删掉 `commit` 的 staged 分支、`publish` 的 staged 分支和它们用到的字段；`region/tests.rs` 里 staged 的用例随之删掉或改写。
- **实时块（C10）**：缓冲块攒完之前，帧里是一行计量预览（`label · N lines`）。S7b 要让它在缓冲里原地生长：块模型已经给了位置——一个打开的 `Fragment` 以块的身份入缓冲，之后需要"按块号替换"的一条消息（今天的缓冲只追加）。
- 写出策略不变：每块只写一次，按写出时的宽度布局。
- **选择的点**是 (块号, 块内行, 列)；resize 清选择的规则保留。
- **C11（折叠块展开/收起）**若做，点击已经有路由（`input/mouse.rs` 的 `Target`）：视口里的一下按下目前是选择的起点，展开需要一个区分（例如点在折叠摘要行上）。
