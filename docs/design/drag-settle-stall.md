# 慢光标回答下的拖拽提前结束：值不值得修（评估）

Status: **Assessment** · 日期：2026-10-18 · 基于 `main` = `ec613dd`（qos-render 合并之后，X-81）· 本机 macOS 26（Darwin 25.6）、tmux 3.8

本文只做评估，不改代码，等所有者拍板。所有者要的是：修复价值、成本、会不会把架构搞丑、是不是为了修而修。

## 写作约定

- 每条结论都指到 `path:line`（`ec613dd`），或给出本机命令与输出。查不到写「查不到」；从代码推出、没有实测支撑的，标「推断」。
- 实测都在 tmux 3.8 上跑 scenario 06（`cargo test --test ui_tmux tmux_resize_mid_stream_and_idle`）。herdr、Ghostty 没测。
- 实验用的二进制是加了埋点、并用环境变量切换候选修法的临时版本，**没有提交**（补丁见 §8）。埋点只往 `IOTA_DRAG_LOG` 写日志，不改行为；不注入时它与未改版一样全绿（§2.3）。

## 0. 结论

**不值得单开一轮修。推荐「接受并记录」，把候选 B 列为「以后修」：下次有人动 `run_loop` 这一段时顺手带上，不为它单开一轮。**

理由：

1. 作者的推测**成立**（§1，日志逐行坐实），但它**只是残留的一半**。另一半是 pass 阻塞期间排队的拖拽步骤被合成一步、宽过 margin，这跟 26 H 和 06 盲拖是同一族残留，任何「settle 时钟」修法都碰不到它。最好的候选（B）在注入条件下把重复从 6/8/12 降到 2/3/1–4，**仍然 3/3 红**。
2. 触发条件是**一次光标回答超过 2 s，而且这 2 s 里手还在拖**。正常条件下一次 pass ≤ 4 ms，离 2 s 差约 500 倍。真机上只在「后台 QoS + 满载」的人为饥饿和一次 CI 虚拟机上见过超时（§2）。真到了那种时候，整个界面在拖拽中卡住 2 s 以上，重复几行历史是其中最轻的症状。
3. 症状**不丢内容**：活的 frame 完整、贴底、全宽。多出来的是 scrollback 里的重复行（frame 里暂存的最后 1–4 行），外加盲拖或慢拖时的 1–2 段分隔线碎片和 1 份旧 frame。它们会**一直留在终端历史里**，下一次重绘不会把它们抹掉（§3）。
4. 冻结协议不允许调常量（§4）。B 不碰任何常量，也不改协议文字，只是让实现兑现协议原文里的「after the last resize」。但它的收益只有半个残留，而且只在一个罕见条件下才有。
5. 「把测试期望改成允许暂存行重复」**没有对象**：06 在不注入时 4/4 全绿（§2.3），红只出现在手工注入 `scripts/dsr-delay.py` 时，CI 里没有一条红测试需要放宽。改期望就是纯放宽断言，不推荐（§5 E）。

## 1. 机制：推测坐实，但只占一半

### 1.1 代码路径

- `handle_event` 收到 `Event::Resize`，只记下来，并设 `drag_until = now + DRAG_SETTLE`（`src/ui/runtime/event_loop.rs:657-662`）。
- 安静满 `RESIZE_QUIET` 后，`apply_resize` **先**设 `drag_until = Instant::now() + DRAG_SETTLE`（`event_loop.rs:826`），**再**调 `term.resize`（`event_loop.rs:839`）。后者阻塞在光标查询上。
- 一次 pass 有两次查询（`src/ui/runtime/inline_term.rs:356` 的 `start` 与 `:380` 的 `resync`），每次都走 `ask`。`ask` 最多熬两次 crossterm 的 2 s 超时（`DSR_WAITS` = 2，`inline_term.rs:755`），所以一次 pass 最坏阻塞约 8 s。
- `apply_resize` 返回后，**同一轮循环**紧接着做 `if !resizing && m.settle_drag() { term.close_band()?; }`（`event_loop.rs:1061-1063`）。这中间**没有再读事件**：事件只在循环顶上读（`event_loop.rs:1001-1022`）。

所以，只要一次 pass 阻塞 ≥ 2 s，pass 一返回 `drag_until` 就已过期：拖拽被判定结束，band 关掉，frame 按全宽重画（`settle_drag`，`event_loop.rs:871-882`）。阻塞期间手上拖出的步骤躺在事件队列里，下一轮才被读到，开出一个**新**拖拽。新拖拽的第一步要把刚按全宽画出的暂存行折回去，这就是 X-52 残留 (1) 的机制，每次多一份。

### 1.2 日志（未改版 + 埋点，`dsr-delay.py 20 2.5`，settled 25 步拖拽）

```
  11.528 event resize 65x20
  11.578 pass start 65x20 frame_w=63
  14.084 pass end 65x20 took=2.505774209s settle_left=Some(0ns)
  14.085 settle (deadline passed) width=65
  14.137 event resize 57x20
  14.187 pass start 57x20 frame_w=49
```

pass 阻塞 2.506 s，返回时 settle 剩 0，**1 ms 后**拖拽结束；52 ms 后才读到排队的 resize。它已经被合并成一次 57x20（tmux 在这 2.5 s 里把 64…57 都发了）。未改版 4 份完整日志里，这种「pass 一结束就 settle」各出现 6–13 次。

### 1.3 另一半：排队的步骤合成一步

修掉提前 settle（候选 A 或 B，§5）之后，日志变成：

```
  11.645 pass start 65x20 frame_w=63
  14.151 pass end 65x20 took=2.505806417s settle_left=Some(0ns)
  14.204 event resize 57x20          ← 没有 settle，拖拽延续
  14.255 pass start 57x20 frame_w=49
```

拖拽不再提前结束，但 frame 是按 63 列给 65 列的终端画的，下一个 pass 看到的是 57 列：一步就窄了 8 列，比当时的 margin（2）宽。上一帧的行照样要折。这跟 TUI-VERIFY §4.5 记录的 26 H（`docs/TUI-VERIFY.md:414-426`，「a pass that sees a step wider than the drag's margin strands the tail of the frame drawn for the last one」）和 06 盲拖（`docs/TUI-VERIFY.md:427-433`）是同一族。**只要 pass 是阻塞的，手比 pass 快，步骤就会被合并。** settle 时钟管不到这一层。

## 2. 真实可达性

### 2.1 触发条件

两个条件同时满足：

1. 一次 pass 阻塞 ≥ `DRAG_SETTLE`（2 s），也就是一次光标回答晚于 2 s。crossterm 的超时本身就是 2 s，**任何一次超时都满足这一条**。
2. 阻塞期间还有新的拖拽步骤到达。只有一次 resize 的情况下，提前 settle 等于按时 settle，没有损失。

### 2.2 证据

| 条件 | 光标回答超 2 s？ | 出处 |
|---|---|---|
| 本机、不注入、空闲（本轮） | 否：267 次 pass，最长 3.7 ms | §2.3 的命令；`drag-nbase.log`/`drag-nfixb.log` |
| 本机、`taskpolicy -b` + 10 个 `yes` 烧核 | 是：25 次有日志的未改版运行里 5 次超时 | `docs/TUI-VERIFY.md:348-350` |
| 同上，X-81 修复后交替 8 对 | 未改版 2/8 超时，修复版 **0/8** 超时 | `docs/TUI-VERIFY.md:363-365` |
| 同上 + 20 个烧核 | 整机卡死、`spindump` 起来，「not a condition to measure a fix under」 | `docs/TUI-VERIFY.md:350-352` |
| CI macos-14（36305402235） | 是：归因于迟到回答，其重跑绿；本地约 75 次带载运行 0 次 | `docs/TUI-VERIFY.md:398-401` |
| SSH / 高延迟链路 | 需要 RTT > 2 s。**查不到**实测，本轮也没测 | — |
| 其他终端（herdr、Ghostty、iTerm2…） | 本地终端回答 DSR 是微秒到毫秒级（推断）。**没测** | — |

修复版那 8 次没遇到超时，只说明超时罕见，不说明修复版更不容易超时：X-81 改的是超时**之后**怎么取回答，不是超时本身的频率。

**另一个事实**：X-81 之前，任何一次超时都让 pass 阻塞 2.04 s（≥ 2 s），所以这个残留**一直都在**，只是被丢帧掩盖了。X-81 没有制造它；`DSR_WAITS` = 2 把最坏阻塞从 2 s 拉到 4 s（每次查询），但触发门槛没变。

**判断**：在一台正常的桌面上交互使用，基本不可达。要可达，iota 进程得被饿到一次调度延迟超过 2 s：后台 QoS 加满载、内存压力下换页、负载很重的 CI 虚拟机。这些条件下用户先感受到的是拖拽时界面冻住 2–8 s。远程链路能不能到，没有数据。

### 2.3 不注入时的对照（本轮）

```
for v in base fixb base fixb; do IOTA_TMUX=1 IOTA_TMUX_BIN=$S/n-$v cargo test --test ui_tmux tmux_resize_mid_stream_and_idle ...
base: ---- 06-resize.sh: PASS=119 FAIL=0 WARTS=0
fixb: ---- 06-resize.sh: PASS=119 FAIL=0 WARTS=0
base: ---- 06-resize.sh: PASS=119 FAIL=0 WARTS=0
fixb: ---- 06-resize.sh: PASS=119 FAIL=0 WARTS=0
```

## 3. 用户看到什么

注入条件下的真机证据（`dsr-delay.py 20 2.5`，未改版 5 次）：

- **活的 frame 没坏**：每个拖拽块后的 `check_frame_intact` 都通过，分隔线全宽、恰好一行 composer、底部区有内容、无丢行（`check_no_loss` 全绿）。
- **scrollback 里多出拖拽开始时 frame 暂存的行**：06 里是流的最后四行 `l#36`–`l#39`，`l#36` 最多。例：`DUPLICATED (whole history): l#36×12 l#37×7 l#38×4 l#39×4`（慢拖 10 步之后，`run-base-1.out`）。每个块只重复暂存行（`dup_outside` 检查全绿）。
- **盲拖与慢拖时**：另有 2 行分隔线碎片进历史，1 份旧 frame 被挤进历史（5 次里 4 次）。
- **待多久**：这些行已经提交进终端自己的 scrollback。iota 从不改写已提交的行（X-52 / ARCHITECTURE §12），**下一次重绘不会自愈**，它们一直留到被新输出推出屏幕、留在历史里，直到终端清屏或 scrollback 截断。屏幕上的位置是 frame 正上方，新输出会把它们往上推。
- **不是一闪而过，也不是持续坏掉**：活界面一 settle 就是对的，坏的是历史里永久多出的几行。

两边（未改版和候选版）都偶尔出现一次 composer 离底 1 行（`after the paced 10-step drag: … got '17', want '18'`，未改版 1/5、A 1/3、B 1/3）。`check_pinned` 是单次抓屏、不轮询（`tests/ui_tmux/scenarios/06-resize.sh:96-98`），很可能落在一次 2.5 s 停顿中间（推断，没查清）。它与 settle 时钟无关。

## 4. 冻结协议：出处与原文

- **提交** `346ce2d`（2026-09-24）：「The protocol is FROZEN here (the owner): native scrollback wins over zero residuals. Loss = 0 is the hard constraint, ≤ 2 blank or duplicated rows per drag the budget; `DRAG_SETTLE` and `DRAG_MARGIN_MAX` are no longer tuned, and a report inside the budget is closed as by design (TUI-VERIFY §4.3).」
- **DIVERGENCES X-52**（`docs/DIVERGENCES.md:329`）：「FROZEN here (the owner, 2026-09-24): native scrollback wins over zero residuals; loss = 0 is the hard constraint, ≤ 2 blank or duplicated rows per drag the budget; `DRAG_SETTLE`/`DRAG_MARGIN_MAX` are no longer tuned, and a report inside the budget is closed as by design」
- **TUI-VERIFY §4.3**（`docs/TUI-VERIFY.md:249-254`）：「**The budget (the owner's rule, 2026-09-24 — the resize protocol is FROZEN):** a LOST row is the one hard failure … per drag, at most 2 blank or duplicated rows. The residuals listed below are BY DESIGN — native scrollback won over zero-residuals (no alt-screen) — and a report inside this budget is closed as such, not a new round.」
- **ARCHITECTURE §12**（`docs/ARCHITECTURE.md:410-412`）：「**The resize protocol — FROZEN** (X-52, TUI-VERIFY §4.3). … the drag ends `DRAG_SETTLE` (2 s) after the last resize or at once on a key, a paste, a turn or a surface」；`:436`：「**Don't.** … Don't tune `DRAG_SETTLE`, `DRAG_MARGIN_MAX` or `RESIZE_QUIET`.」
- **brain** `pages/inline-resize-owned.md:25`：「**这些常量是交互策略，不是正确性前提**：零丢行由相对寻址、光标优先和 `pad` 不变量保证，不靠调常量；它们不再调，也不许靠调它们修 bug。」

**为什么冻结**：常量调过三轮。250 ms 和 750 ms 两个窗口都被证伪：拖拽步间隔 0.8–1 s 时，下一个 turn 里出现 12–20 行空洞（`event_loop.rs:63-70`，`docs/TUI-VERIFY.md:258-259`）。所有者于是认定原生 scrollback 比零残留重要：丢行 = 0 是硬约束，其余是预算。

**修法要不要动它**：

- A、B 都不碰任何常量。
- A 改的是时钟起点：「最后一次 resize 之后 2 s」变成「最后一次 resize 的 pass 结束之后 2 s」。协议文字（ARCHITECTURE:411、X-52）要跟着改，属于**改协议定义**。
- B 不改定义。协议说「after the last resize」，一个已经到达、只是还没读出来的 resize **就是**最后一次 resize。现在的实现在它还在队列里时就判定结束，违背的是协议原文。B 是让实现兑现协议，不是改协议。
- 「调大 `DRAG_SETTLE`」（比如调到 9 s 盖住最坏阻塞）明文禁止，而且也只能盖住一半（§1.3）。

## 5. 候选方案

注入条件下的实测（`dsr-delay.py 20 2.5`，tmux 3.8；未改版 5 次，A 3 次，B 3 次，B 与未改版交替跑）。数字是该拖拽块新增的重复行，括号里是测试的上限：

| 块 | 未改版 | A | B |
|---|---|---|---|
| settled 25 步（1） | 6 6 6 6 6 | 2 2 2 | 2 2 2 |
| 配对 8 步（1） | 3 3 ≤1 3 3 | ≤1 ≤1 ≤1 | ≤1 ≤1 ≤1 |
| 盲拖 8 步（2） | 8 8 3 8 8 | 3 3 ≤2 | 3 3 3 |
| 慢拖 10 步（0） | 12 12 1 12 14 | 4 4 1 | 4 1 4 |
| mid-stream 收缩（2） | 6 ≤2 ≤2 6 ≤2 | ≤2 ×3 | ≤2 ×3 |
| 分隔线碎片 / 旧 frame 进历史 | 4/5 次 | 2/3 次 | 3/3 次（盲拖） |
| 新增的红 | — | 高度拖拽后 frame 68 ≠ 70（1/3） | 无 |
| 06 总 FAIL | 8–10 | 3–10 | 7–9 |

「配对 8 步：pass … was never drawn」三版都有。那是 `pass_landed` 的 1.5 s 上限短于注入的 2.5 s（`tests/ui_tmux/scenarios/06-resize.sh:198-209`），是测试工具的限制。

### A. pass 结束后重起 settle 时钟（≈「等回答期间暂停时钟」）

- **改什么**：`event_loop.rs:826` 这一行挪到 `term.draw_frame` 之后（或在其后再设一次）。拖拽层，一行。
- **影响面**：拖拽协议的**定义**要改（§4），ARCHITECTURE:411 与 X-52 的文字要跟着改。查询层、几何层不动。
- **风险**：拖拽被延长到「阻塞时长 + 2 s」，连续阻塞时可以到 10 s 以上（实测：高度拖拽最后一次 settle 在第一步之后约 10 s）。期间 frame 一直窄 2–8 列。06 的 `check_frame_intact` 只等 6 s（`tests/ui_tmux/lib.sh:618-628`），所以出现了新红。对用户无害，但 frame 窄着的时间变长，而且测试的等待预算要改。
- **收益**：重复约降 2/3，残留另一半不动（§1.3）。
- **为修而修？** 偏是。动了协议定义，换来半个罕见残留。

### B. settle 之前先看事件队列：有排队事件就这一轮不结束拖拽

- **改什么**：`event_loop.rs:1061` 前用已有的 `EventSource::poll(Duration::ZERO)`（`event_loop.rs:115-120`），返回 true 就跳过本轮 `settle_drag`。下一轮循环先读事件，排队的 resize 自然把 `drag_until` 往后推。拖拽层，约 3 行，外加一个 vt100 测试（`LoopHarness` 的事件源在一次慢 pass 后吐出排队的 resize）。
- **影响面**：协议定义不变（§4），常量不变。查询层、几何层不动。排队的是按键时，下一轮读到按键就照常 `end_drag`，也只晚一轮。
- **风险**：低。正常条件下 4/4 全绿，0 次「deferred」（§2.3）；注入下每次运行介入 8–9 次。零超时 poll 走 `HangupWatch` + crossterm，与循环顶上那次同路径（`event_loop.rs:122-131`）。没想到新的时序问题（推断）。
- **收益**：与 A 相同（约降 2/3），没有 A 的副作用。残留另一半不动。
- **为修而修？** 边界情况。它本身干净，甚至算「让实现符合协议」，但修的是罕见条件下的半个外观残留。**值得做，不值得单开一轮。**

### C. 纯事件驱动：pass 不阻塞，光标回答作为事件回到循环

- **改什么**：`InlineTerminal::relayout`/`ask`（`inline_term.rs:344-430`）拆成「发查询」和「收回答」两半，`run_loop` 加一个「等回答」状态。期间继续读事件，回答回来时按最新尺寸重新规划。查询层 + 拖拽层 + 循环状态机。
- **影响面**：X-54「终端是我们的、DSR 不在同步块内」的不变量、X-81 刚做的标记列配对、`Geometry` 测试缝都要重写。06/26 的大部分计数要重测。
- **风险**：高。等回答期间到达的输出写不写？（现在的回答是「什么都不写」，`event_loop.rs:1041-1044`。）一旦写，就重开 P0（2026-09-25 的高度拖拽丢行，X-52 原文）那一类窗口。
- **收益**：理论上能连 §1.3 那一半一起消掉，因为回答回来时看得到最新尺寸。但即便这样，阻塞期间画出的那一帧仍是旧尺寸的（推断），能不能清零没把握。
- **为修而修？** 是。为一个只在饥饿条件下出现的外观残留，重写刚收敛的查询层。

### D. 接受并记录（推荐）

- **改什么**：只改文档。X-52 残留清单加一条「pass 阻塞 ≥ `DRAG_SETTLE` 时（一次光标回答超时）拖拽提前结束，阻塞中拖出的步骤合并成一步；注入下每块 3–14 行暂存行重复、偶有分隔线碎片；不丢行、活 frame 完好」。TUI-VERIFY §4.5 那句「open — the suspicion, unverified」改成本文的结论与出处。`scripts/dsr-delay.py` 的头注释写明：注入下 06 的重复预算不适用，只看丢行、帧完整、贴底。
- **影响面**：零代码。
- **风险**：无。代价是这条残留留着：罕见、不丢内容、历史里多几行。
- **为修而修？** 否。

### E. 把 06 的期望改成「允许暂存行重复」

- **评估**：**这是放宽断言，不推荐。** 06 在 CI（不注入）里绿，本轮 4/4 绿，红只出现在手工注入时（脚本头注明「OPT-IN, MANUAL, NEVER IN CI」，`scripts/dsr-delay.py:8`）。CI 里没有一条红测试需要处理。把预算放宽到盖住 14，会让正常条件下的回归（比如有人把重复从 1 弄到 5）也悄悄通过。如果要表达「注入下不适用」，就写在 D 说的脚本头和 TUI-VERIFY 里，不改断言。

### F.（附）缩短光标查询的等待

- 让 `ask` 不熬第二次超时，或自定一个短于 2 s 的超时。查询层常量，不在冻结清单里，但违背精神。
- 不成立：crossterm 的超时本身就是 2 s = `DRAG_SETTLE`，第一次超时就已经满足触发条件。另外 X-81 已经实测：不熬第二次超时，composer 会离底 2 行（`docs/TUI-VERIFY.md:359-363`）。

## 6. 架构判断

**这类修法该落在拖拽层（`run_loop` / `Model` 的 settle 判定），不该落在查询层或几何层。**

- 缺陷的本质是「判定拖拽结束」用的输入不完整：判定时没看已经到达的事件。这是拖拽层自己的事。B 正好在这一层，用的是这一层已有的接口（`EventSource::poll`），不引入新状态、新常量。设计不会变丑。
- 查询层（`ask`）刚在 X-81 收敛成「回答只属于提问的查询」这一个规则。让它知道拖拽、settle 的存在（比如 A 的变体「等回答时暂停 settle 时钟」，要么由查询层回调拖拽层，要么拖拽层量查询层的耗时）就是层间耦合。A 的一行实现虽然在拖拽层，语义上却是「拿 pass 耗时修正拖拽时钟」，是把查询层的延迟泄漏进协议定义。
- 几何层（`Term::resize` / `relayout`）管的是「从光标数」的不变量，与「拖拽何时结束」无关，不该碰。
- §1.3 那一半只能在 C 的层面解决，也就是把阻塞的 pass 改成异步。那是重新设计查询层，代价与收益不相称。

## 7. 价值—成本

| 方案 | 价值（消掉什么、多常见） | 成本（代码 / 文档 / 测试） | 架构 | 风险 | 判断 |
|---|---|---|---|---|---|
| A 重起时钟 | 注入下重复约 −2/3；真实中罕见 | 1 行 + 改协议文字 + 改 06 等待预算 | 偏丑：查询延迟漏进协议定义 | 拖拽变长到 10 s+；测试新红 | 不做 |
| B 先看队列 | 同 A，无副作用；真实中罕见 | ~3 行 + 1 个 vt100 测试 + X-52 一句 | 干净：兑现协议原文 | 低（正常 4/4 绿） | **以后修**，顺手做 |
| C 异步 pass | 可能连另一半一起消；真实中罕见 | 重写 `relayout`/`ask`/循环状态机，重测 06/26 | 大改刚收敛的查询层 | 高（重开 P0 一类窗口） | 不做 |
| D 接受并记录 | 让下一个人不再当新问题查 | 三处文档 | 无 | 无 | **现在做** |
| E 放宽 06 | 无（CI 本来就绿） | 改断言 | — | 掩盖正常条件下的回归 | 不做 |
| F 缩短等待 | 无（2 s 超时本身已触发） | — | 违背冻结精神 | 重开 X-81 的离底 | 不做 |

## 8. 拿不准的地方

- **真实世界的频率**：只有人为饥饿和一次 CI 的证据。SSH、远程、其他终端都没有数据。
- **样本小**：每个候选 3 次、未改版 5 次，都在 tmux 3.8 上。`dsr-delay.py` 每 20 个回答扣一个，频率远高于真实情况，06 几乎每个拖拽块都会踩到一次。
- **B 的零超时 poll 能否看见 crossterm `position()` 超时期间排进队列的 resize**：从效果上间接证实了（每次运行「deferred」8–9 次，重复数与 A 相同），没有读 crossterm 源码确认。
- **composer 离底 1 行（17 vs 18）**：三版都有，推断是单次抓屏落在停顿中，没查清。
- **高度拖拽块的重复计数出现 `-4`**（未改版第 3 次）：历史行数在高度拉回时被 tmux 收回，计数器的基线因此失真（推断，没查清）。`check_no_loss` 是绿的。
- **C 能否真的清零 §1.3**：只是推断。

## 材料

实验补丁（**未提交**，用后已 `git checkout` 还原）：在 `event_loop.rs` 里加 `dlog()`（写 `IOTA_DRAG_LOG`），记录 resize 事件、pass 起止与耗时、settle。`IOTA_FIX_A` 在 `term.draw_frame` 之后重设 `drag_until`；`IOTA_FIX_B` 在 `settle_drag` 之前做 `events.poll(Duration::ZERO)`，有事件就跳过。包装脚本：

```sh
# 注入：每第 20 个光标回答扣 2.5 s
export IOTA_FIX_B=1  # 或 IOTA_FIX_A=1，未改版两者都不设
export IOTA_DRAG_LOG=$S/drag-fixb.log
exec python3 -I scripts/dsr-delay.py 20 2.5 $S/held-fixb.log -- $S/iota-instr "$@"
```

```sh
IOTA_TMUX=1 IOTA_TMUX_BIN=$S/w-fixb cargo test --test ui_tmux tmux_resize_mid_stream_and_idle -- --nocapture
```

`$S` 是本次会话的 scratchpad；运行输出 `run-{base,fix,fixb}-N.out`、日志 `drag-*-N.log` 都在那里，不随分支提交。
