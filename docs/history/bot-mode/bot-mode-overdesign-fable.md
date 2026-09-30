# bot 模式 v1：过度设计评审

Status: **Review**（独立评审，2026-10-01）· 对象：分支 `bot-mode-v1` 相对 `main` 的 23 个提交，HEAD `05ff608`（源码与 `d0b87d9` 相同）· 依据：`docs/design/bot-mode.md`（rev 2，下称设计）、四份评审与一份验收报告、`git diff main...HEAD`。

坐标约定：`src/...:行号` 指 HEAD。标「推测」的条目只有推理，没有运行复现。本评审**不改任何代码或测试**，只写本文件。

判据是任务书列出的项目规则，逐条拿来量：不留兼容层；满足当前需求的最简实现；分层生长、不拿能用的产品换未完成的复杂度；模块化；用已有依赖；为长期做决策、不接受权宜方案；先看成熟产品。已拍板的四件（`mode` 枚举、OKF 核心、删密钥拒写、形态 A）不翻。

分支规模（`git diff --numstat`，估算）：产品代码约 4.5k 行，测试约 8k 行（`tests/` 4.7k + `src/**/tests.rs` 1.1k + `src/testing/growing.rs` 0.6k + 内联 `#[cfg(test)]` 约 1.7k），文档与脚本约 1.7k 行。测试与产品代码之比接近 2 : 1。

---

## 0. 总判断

**产品代码没有结构性的过度设计；分支的多余重量在测试基建和文档，不在运行时。** 逐条量下来：

- 「为以后而存在」的运行时构造只找到两个小件：`BotPointer.v`（写了从不读）和懒物化簇（`materialized` + `OnCreated` 回调 + `never_saved`，为 bot 保留一个 bot 用不上的性质）。前者删，后者简化。
- 设计 §7「不做为未来预留的抽象」在代码里是守住的：`grep` 整个 `src/` diff 找不到 L2/L3/L4 的桩、trait、TODO；唯一指向未来的是 `repl::bot` 模块注释里的「L3 下移」（`src/repl/bot.rs:8`），而那个状态机本身有当下的用处（9 个不依赖终端的转移测试）。
- `SessionError` 的新变体、`repair_tail`、写侧截断、批次回滚、`seed_resumed`、四个刷新时刻——每一个都对应一条**已复现**的数据损坏或一条设计写明的正确性要求（评审报告里的 R1/R2/R4/R6/N1/N2 都有复现记录）。这些是必要复杂度，不因为「是为正确性写的」而免检，但检下来都站得住；只有「截回失败后重试」和「中断轮算 landed」两处在边界上。
- 真正不成比例的是 **`tests/repl/bot_longrun.rs` 的 6b 判定机**（约 400 行，需要 4 个单测来测试测试、修了两轮才不漏判）和 **文档形态**（设计文档 699 行仍标 `Proposal`，五份评审报告入库，而 CHANGELOG 与用户文档一字没有）。
- 「分层生长」这一条有一处偏离：设计自己建议「按 L0 → L1 顺序发两个 PR」（`docs/design/bot-mode.md:590`），实际是一个 14k 行的分支。L0 四项（锁、`repair_tail`、摘要剥离、SIGHUP）和 `workspace → mode` 重命名都是自足的、所有模式受益的改动，本可以早两周合进 `main`。

如果只能删/简化三样：**长跑 6b 的豁免判定机、懒物化簇、设计文档瘦身与评审报告归档**。详见 §5。

---

## 1. 代码层面

### 1.1 一览

| 构造 | 判定 | 分类 |
|---|---|---|
| `BotPointer.v` / `BOT_POINTER_VERSION` | **删** | 过度设计 |
| 懒物化簇：`materialized`、`OnCreated`、`on_created`、`never_saved`/`NEVER_SAVED`、stale 修正 | **简化**：bot 会话在 `open_bot` 里立即落盘 | 过度设计（轻） |
| `SessionError::BotRunning`（与 `Locked` 只差措辞） | **简化**：并入 `Locked` | 过度设计（微） |
| `Action::Requeue` + `Steerer.held`/`take_held` | **简化**：steering 直接丢弃 flush notice | 存疑 |
| `TurnCtx.mounts_only` + `send_history` 过滤历史 mount（codex R8） | 留，但属「顺手多做」 | 存疑 |
| `uncut` / `CutFailed` / `LogNotCutBack`：截回失败后挡住写入并重试 | 留 | 存疑（边界） |
| `update_kept` + `landed = saved && !flush`（fable M2） | 留 | 存疑（边界） |
| `CompactionStats.middle_tokens` / `summary_tokens` | 留 | 存疑：写了没人读 |
| `repl::bot` 纯状态机（`Event`/`Action`/`Flush`） | **留** | 必要复杂度 |
| `SessionError` 其余变体（`BotMissing`/`BotOwned`/`BotOwnerUnknown`/`Locked`/`MetaNotSaved`/`LockUnsupported`/`LogNotOpen`） | **留** | 必要复杂度 |
| `repair_tail` + `terminate_last_line` + `fit_line` | **留** | 必要复杂度 |
| 批次 all-or-nothing（`settle_batch`/`cut_to`）+ `MetaNotSaved` + `meta_dirty` | **留** | 必要复杂度 |
| `seed_resumed` + `LoadedLog.measured` + `set_overhead`/`tool_tokens`/`price_bot_overhead` | **留** | 必要复杂度 |
| `system_cleared` 记录键 + loader 三分法 + `clear_system` | **留** | 必要复杂度 |
| `Only` 收窄的 dispatcher | **留** | 必要复杂度 |
| `Snapshot`（四时刻刷新、`Seen.edited`、按项目裁剪、超限截断、`escape_close`） | **留** | 必要复杂度 |
| `HarnessInputs.clock` + `roll_day` | **留** | 必要复杂度 |
| `BotState.name` 的 `#[allow(dead_code)]` | 已不存在（`51cb10d` 删），字段现被 `Alarm` 读 | 无事 |

### 1.2 过度设计

**`BotPointer.v` 与 `BOT_POINTER_VERSION`——删。**
- 依据：`src/session/bot.rs:15-16`（常量）、`:19-27`（字段）、`:41-53`（`read` 反序列化后不校验 `v`）。全仓 `grep '\.v\b\|BOT_POINTER_VERSION'` 只有 `src/session/mod.rs:33` 的 re-export。
- 现在换来什么：一个写进 `bot.json` 的 `"v":1`，没有读者、没有校验、没有分支。
- 更简的替代：删掉字段和常量。将来格式真要变，按项目规则是改代码不留兼容，当时再加。
- 替代的代价：`bot.json` 少一个键；`pointer_round_trips` 测试的期望字串改一行。零。
- 为什么算过度设计：这正是「为未来预留」的最小形态——版本号存在的唯一理由是将来做迁移，而项目规则说不做迁移。

**懒物化簇——简化为「bot 会话在 `open_bot` 里立即落盘」。**
- 依据：`src/session/bot.rs:25-26`（`materialized`）；`src/session/store.rs:87-99`（`BotOpen::Fresh { never_saved }`）、`:447-452`（无指针：先写 `materialized: false`）、`:457-463`（resume 成功时补正陈旧的 `false`）、`:467`（`NotFound && !materialized` → 同 id 重来）、`:485-491`（`on_created` 闭包在首写后改写指针）；`src/session/writer.rs:38-39`（`OnCreated = Box<dyn FnMut …>`）、`:63-64`、`:143-150`、`:359-362`（钩子在 `ensure_created` 里跑、失败重试）；`src/cmd/interactive/bot.rs:35-36`（`NEVER_SAVED`）、`:145-161`（`never_saved` → notice）。测试：`an_unsaved_pointer_starts_over_under_the_same_id`、`a_resume_fixes_a_stale_materialized_flag`（`src/cmd/interactive/bot.rs:553`、`:622`）、`on_created_runs_once_after_materialisation`（`tests/session/store.rs`）。
- 现在换来什么：bot 的 bundle 沿用普通会话的懒创建——「一个从未说话的会话不留目录」。这条性质对普通 chat 有用（picker 不被空会话塞满），对 bot 没有：一个 bot 只有一个 bundle，且 picker 根本不显示它（`src/cmd/interactive/mod.rs` 的 `bot_sessions` 过滤）。为了保住这条无用的性质，多了一个指针字段、一个 boxed 回调类型、一条「指针先写、bundle 后建」的两阶段协议、一个「陈旧 false」的补正分支、一条 `NEVER_SAVED` notice、三个测试。
- 更简的替代：`open_bot` 在无指针时 `create` 后立刻物化 bundle（写 `meta.json`，一个 `pub fn materialize(&mut self)` 包住 `ensure_created`，约 5 行），再写指针。此后 `NotFound` 一律是 `BotMissing`；`materialized`、`OnCreated`、`on_created`、`never_saved`、`NEVER_SAVED`、补正分支全删，`BotOpen::Fresh` 退回单元组。净减约 100 行产品代码与 3 个测试。
- 替代的代价：(1) 启动后一句话没说就关掉的 bot 留下一个只有 `meta.json` 的空 bundle——对 bot 无害，`iota list sessions` 里多一行标题为 bot 名的空会话；(2) 建 bundle 与写指针之间崩溃会留一个无主的空 bundle，变回普通会话，可在 picker 里删——比现在「指针悬空」的对称情形还好处理；(3) `tests/repl/bot_flush.rs:54-58`、`:97-98` 用 `on_created` 钩子做故障注入（`a_backlog_the_log_refused_is_saved_before_the_compaction_and_survives_a_restart`，`:760`），要换成同文件其它测试已在用的「把 `messages.jsonl` 变成目录」手法。
- 为什么算过度设计：一个间接层（回调）加一个状态位，服务的是 bot 模式里没有需求的性质。评审 fable 第一轮把「陈旧 `materialized` 的补正」记作「实现比设计更对」——它确实对，但它修的是懒物化自己制造的窗口。

**`SessionError::BotRunning`——并入 `Locked`。**
- 依据：`src/session/error.rs:31-40`（`Locked { what, pid }`）、`:41-50`（`BotRunning { bot, pid }`，doc 自陈「a variant of its own rather than a second spelling of Locked … the sentence says so」）。全仓没有 `match … BotRunning` 的分支。
- 现在换来什么：文案 `bot coder is already running (pid N)` 而不是 `bot coder is open in another iota process (pid N)`。
- 更简的替代：`lock_bot` 返回 `Locked { what: format!("bot {bot}") }`；`what` 已经是自由文本。
- 替代的代价：一句英文稍不自然；一个变体、两处 Display 测试少掉。微。

### 1.3 存疑

**`Action::Requeue` + `Steerer.held`——可简化为丢弃。**
- 依据：`src/repl/bot.rs:128-131`、`:187`；`src/repl/turn/steer.rs:28-29`、`:50-56`（flush notice 被 drain 到时暂存不注入）、`:83-85`；`src/repl/turn/mod.rs:347-351`（`take_held`）；`src/repl/run.rs:1105-1130`（`held` 穿过 `after_bot_turn` 到 `bot_perform`）。测试 `a_flush_notice_taken_by_steering_is_put_back_not_injected`（`tests/repl/bot_flush.rs:486`）。
- 现在换来什么：用户 typed-ahead 排在 flush notice 前、且那一轮有工具 round 时，steering 会把 notice 从队列拿走；`held` 把它放回去。放回去之后它的命运：压缩已成功 → `DropNotice`；压缩失败（第一次）→ 仍作为 flush 轮运行。也就是说只有「typed-ahead + 发送前压缩失败 + 该轮有工具 round」这三重叠加时，`Requeue` 才比「直接丢弃」多保住一次 flush。
- 更简的替代：`Steerer::drain` 遇到 flush notice 直接丢弃（不注入、不保留），删 `held`/`take_held`/`Requeue` 与 `bot_perform` 的 `held` 参数，约 30 行。
- 替代的代价：上述三重叠加时，这一周期的压缩会走 `flush_skipped` 兜底并打 `⚠ Compacted without a memory flush`——这正是设计 §4.1「flush 是 best-effort」已经接受的降级。评审 fable 第一轮把这个 `Requeue` 记为「设计没考虑的竞争，实现给了」，我同意它是对的，只是不值 30 行加一个端到端测试。

**`TurnCtx.mounts_only` + `send_history`（codex R8）——留，但记着它是顺手多做的。**
- 依据：`src/repl/turn/mod.rs:107-145`；测试 `the_flush_turn_does_not_advertise_a_mounted_tool`（`tests/repl/bot_flush.rs:969`）。
- 现在换来什么：defer 方言下，历史里冻结的 `system_tools` mount 在 flush 轮的发送副本里被裁到只剩 `remember`，模型不会看到一个 dispatcher 会拒绝的工具。
- 更简的替代：不裁。`Only`（`src/tool/dispatch.rs:276-329`）已经拒绝执行，模型若真选了旧 mount 里的工具，得到 `UnknownTool`，多跑一个无意义 round。codex 自己标了「推测：模型可能选中它」。
- 替代的代价：那一个可能的空 round。约 45 行加一个测试的代价换一个推测场景的整洁——按「最简实现」它偏多，但既已写好且不引入间接层，删它的收益也小。留。

**`uncut` / `CutFailed` / `LogNotCutBack`——留，在必要复杂度的边界上。**
- 依据：`src/session/writer.rs:65-68`（字段）、`:366-374`（`log_start` 先重试截回）、`:376-396`（`settle_batch`）、`:398-412`（`cut_to` 用独立写句柄，含 `CUTS_TO_FAIL` 注入）；`src/session/error.rs:106-123`。测试 `a_failed_cut_holds_every_write_until_the_log_is_cut_back`（`src/session/writer.rs:588`）。
- 现在换来什么：写失败**且**截回也失败时，不会在残批之上重追加、制造 `repair_tail` 救不了的中段孤儿（codex 第二轮用 `chflags uappnd` 实测过重试会写坏；验收报告 §1 实测修复后 `c1_calls=1`）。
- 更简的替代：截回失败 → writer 标记为坏、此后一律拒写、错误文案说「重启 iota」；重启后 `repair_tail` 处理尾部孤儿（验收报告 §6 实测过这条路：`reopen_tail_repairs=1`）。省掉 `log_start` 里的重试与 `LogNotCutBack` 一个变体，约 25 行。
- 替代的代价：一个能自愈的磁盘故障（截回一时失败、稍后成功）要靠重启来恢复。对「跨周活着」的 bot 这是一次不必要的重启；但触发条件是两次独立的 I/O 失败接连发生，复现只能靠文件标志。我把它放在存疑而不是过度设计：它不是间接层，是同一个函数里多一个分支；删与不删差 25 行。作者选了不需要人介入的那一边，与「为长期做决策」一致。

**`update_kept` + `landed = saved && !flush`（fable M2）——留，同样在边界上。**
- 依据：`src/repl/context/meter.rs:242-259`；`src/repl/run.rs:878-885`、`:1276-1279`。测试 `a_kept_turn_keeps_its_measured_usage`、`an_interrupted_turn_that_was_kept_still_queues_the_flush`（`tests/repl/bot_flush.rs:1021`）。
- 现在换来什么：Ctrl+C 打断一个已越过阈值的长轮后，flush 仍会排队，而不是下一条消息走「无 flush 直接压缩」的兜底。
- 更简的替代：不做，中断轮不算 landed，走兜底。评审 fable 第一轮自己写的是「判断题，不是 bug」。
- 替代的代价：那一轮学到的东西只剩摘要；transcript 有 `⚠` 提示。约 40 行换「最常见的中断（评审 I4）不丢一次 flush」——值，但这是打磨，不是必需。

**`CompactionStats.middle_tokens` / `summary_tokens`——写了没人读。**
- 依据：`src/session/writer.rs:26-36`；`src/session/record.rs` 两个 optional 键；消费者只有 `tests/repl/bot_flush.rs:328-329` 断言非零。`scripts/bot-retention.sh` 也不读它们。
- 现在换来什么：设计 §3.6.2 第 4 条说它们是「将来量化衰减的唯一数据源」。可是那个「将来」（保留率实验）的脚本没有用它们。
- 更简的替代：删两个键，需要时再加。
- 替代的代价：历史数据补不回来；两个 optional 键、Go 忽略、不进任何分支，代价近零。留——但设计文档里「唯一数据源」的说法应降为「顺手记录」，或者让保留率脚本把它们读出来，否则这就是纯预留。

### 1.4 必要复杂度（检过，站得住）

**`repl::bot` 纯状态机。** `src/repl/bot.rs:57-72`（三个相）、`:85-120`（7 个事件）、`:122-144`（7 个动作）、`:163-239`（`step`，约 75 行逻辑）；解释器 `bot_perform` 与三处胶水在 `src/repl/run.rs:1105-1226`。设计 #23 给的理由是 L3 下移不重写；这条理由是推测性的，但状态机还有一个当下的理由：flush/压缩/退避/告警这四件事的交织（Queued → Flushed → 失败计数 → Snooze）在 `src/repl/bot.rs:288-462` 用 9 个几毫秒的单测钉住了每条转移，其中 `an_unchanged_compaction_snoozes`、`a_queued_notice_outlives_a_failed_compaction` 对应的正是评审 M1/M12 那类多周期问题。它没有 trait、没有泛型、没有插件点，输入输出是 `Copy` 的枚举——这是让编排可测的最朴素写法，不是抽象。代价是约 100 行的 enum → match 间接和一次「这件事在哪发生」的查找。判：留。顺手一提，`FlushReport`（`:146-153`）只有一个读者（`compact.rs:320`），可以是元组，不值一改。

**`SessionError` 其余变体。** `MetaNotSaved`（`error.rs:77-80`）被 `persist_turn`、`resume`、`adopt_system`、`compact_now` 四处 `match`，语义是「批已落、勿重追加」，删了就回到 codex R1 的重复追加（已复现）。`Locked`（`:31-40`）被 `/session` Delete tab 匹配为「跳过」（`src/repl/commands/session.rs`）。`BotMissing`（`:60-76`）、`BotOwned`（`:51-59`）、`BotOwnerUnknown`（`:81-94`）、`LockUnsupported`（`:95-105`）都只为文案存在，没有分支匹配它们；但项目的既有风格就是「每种拒绝一个带 Display 测试的变体」（`display_texts_match_go`），换成一个 `Refused(String)` 省不了多少，还打破了 Go-parity 文案测试的形式。`BotOwnerUnknown` 的失败关闭是 codex R6 复现过的真实漏洞（坏指针解除本体保护）。判：留（`BotRunning` 除外，见 §1.2）。

**`repair_tail`、`terminate_last_line`、`fit_line`。** `src/session/loader.rs:257-272`；`src/session/store.rs:518-532`；`src/session/writer.rs:443-478`。三个都对应「一次故障 → 永久不能用」的形状：孤儿 `tool_use` 让每次请求 400；撕裂行让修复记录粘在半行上；一条 32 MiB 记录让 bundle 永久 `ReadLog`。第一个在 X-61 之前就是所有模式的老问题（D-43 一族）。`fit_line` 的三段收缩（`raw` → `content` → `reasoning`）可以只保留一段，但每段各 3 行。判：留。

**批次 all-or-nothing + `MetaNotSaved` + `meta_dirty`。** `src/session/writer.rs:223-250`、`:328-343`。codex R1 复现了「保存失败 → 压缩 → 重启丢一轮」和「meta 失败重试 → 5 条而非 3 条」；codex N2 复现了「同值重试假成功」。三个修复都是最小的：一个 `set_len`、一个变体、一个 bool。判：留。

**`seed_resumed` 与 overhead 定价。** `src/repl/context/meter.rs:270-292`（播种）、`:294-303`（`set_overhead`）、`:60-66`（两个字段）；`src/session/loader.rs:87`（`measured` 只认最后一个标记之后的 usage，`:295-299`、`:317-319`）；`src/repl/run.rs:596-620`、`:1005-1017`；`BotState.tool_tokens`（`src/repl/state.rs:79-91`）。它来自长跑的一个真实发现：重启后本地计数漏掉 system 段、记忆块与工具定义，「不重启时该压缩的那一轮」重启后原样发出。没有它的后果是重启后的第一条消息可能超窗，或那个周期丢一次 flush；反向测试 `without_the_measurement_the_restart_would_not_compact` 证明起作用的是播种。它把 `measured` 从 loader 穿到 writer 再到 run，约 150 行，是这一簇里最贵的——但它兑现的是设计 §1.2 第 2 条「进程重启 = resume」，不是为测试而写。判：留。

**`system_cleared` 三分法。** `src/session/loader.rs:308-315`（空且无标记 = 旧 mount，忽略；有标记 = 清空，胜出且不进视图）；`src/session/writer.rs:257-269`（`clear_system`）；`src/cmd/interactive/bot.rs:214-247`。三条规则是被两个真实约束逼出来的：Go 与旧 Rust 已把 mount 落成空 system 记录（DIVERGENCES 那段），而 codex R5 又要求「删掉 `system:` 要生效」。忽略无标记空记录不是兼容层，是数据完整性——否则老 bundle 一加载就丢 prompt。判：留。

**`Only`。** `src/tool/dispatch.rs:276-329`，一个 50 行的过滤视图，让 flush 轮只能执行 `remember`（设计 S2b）。替代是「只靠提示词」，代价是一个被告知「一行回复」的轮带着全部工具替用户干活。判：留。

**`Snapshot`。** `src/agents/memory/snapshot.rs`：四时刻刷新（§3.4，已定）、`Seen.edited`（`src/agents/memory.rs:606-614`，修的是「人改了文件、工具随后写入、mtime 变成工具的」这个真实竞争，一个 bool）、按项目裁剪（评审 I7，已定）、超限截断与警告（`:184-198`，否则 8 KiB 上限只对工具生效、对人不生效）、`escape_close`（`:202-215`，注入）。每一件都指向一条设计决定或一个复现过的边界。判：留。

**`HarnessInputs.clock` + `roll_day`。** `src/agents/harness.rs`（`clock: Arc<dyn Fn() -> String>`）、`src/repl/run.rs:1132-1149`。跨周会话的 `date:` 必须换日；闭包是测试注入点，替代是 `#[cfg(test)]` 的全局覆盖，更差。判：留。

**`BotState.name` 的 `#[allow(dead_code)]`。** 在 `cc889fe` 加入（注释「T7 是它的第一个读者」），`51cb10d` 删除；现在 `src/repl/run.rs:1207` 的 `Alarm` 读它。这是「按计划提前一个提交加字段」的痕迹，持续了一个提交，无事。

---

## 2. 测试与脚本

### 2.1 `tests/repl/bot_longrun.rs` + `src/testing/growing.rs`（2021 行）

**结论：长跑的七条不变量里六条是廉价而有效的；6b 的「重启后发送逐字节相同、只豁免模型对新记忆块的反应」这一条是整个分支上唯一需要「测试测试」的代码，换来的保证与它的维护成本不成比例。**

- 依据：`tests/repl/bot_longrun.rs:1-38`（模块注释）、`:440-542`（`refresh_explains`，约 100 行）、`:326-368`（`Write`/`new_writes`：把 `memory:` notice 与它对应的工具写入配对）、`:370-438`（`without_memory_section`/`flush_writes`/`summary_request`：把 summary 请求里允许变化的两段文本正规化）、`:582-593`、`:595-645`（`compare`）、`:1167-1395`（4 个判定单测）。这部分一共约 400 行，经历了 fable 第二轮 §4.2（豁免过宽）、codex 第二轮（两条漏检）、验收 A/B（原样提取函数做负例、再修）三轮才收口。
- 它换来什么：重启后进程**发出的调用**与不重启逐条相同（call kind 序列 + 消息级），豁免模型看到新记忆块后的不同选择。两个 32k 场景里 6b 实际抓到 0 处进程差异，1 处被豁免的模型差异（`#1683` 的 `fact-u307` vs `fact-u332`）。真正由长跑发现的两个问题——重启后 meter 漏计、8k 窗口不相容——一个由不变量 2/3 暴露，一个由 6a 与 2 暴露；6b 在 8k 里红是**后果**（注释 `:1428-1436` 自述），不是发现者。
- 维护成本：`GrowingProvider::round` 用字串匹配产品文案决定行为——`SUMMARY_INSTRUCTION`（`src/testing/growing.rs:245`）、`"cap; nothing was written"`（`:268`）、`"consolidate soon"`（`:275`）、`FLUSH_MARK`（`tests/repl/bot_longrun.rs:70`）；`summary_request` 正规化依赖 `--- LONG-TERM MEMORY` 与 `--- NEW CONVERSATION START ---` 的精确边界（`:370-376`）、`saved N line(s)` 的措辞（`:409`）。改任何一句提示词都要同时改判定机。两个场景本机实测 15 s 与约 25 s（评审记录 24–30 s），每次 `cargo test --test repl` 多 40–50 s；按项目「每个提交跑 cargo test」的门，这是每个提交的固定开销。
- 更简的替代：6b 只比 **call kind 序列**（`kinds(&pa) != kinds(&pb)`，`:518-524` 已有），删掉消息级比较与全部豁免机（`refresh_explains` 大半、`Write`/`new_writes`、`summary_request`、`memory_differs_at`、`follows_the_refresh`、4 个判定单测）。消息级的保证交给已有的东西：6a 逐字节比对加载视图（`:559-580`）、不变量 1 日志只增（`Watch::look`）、定向测试 `the_send_after_a_pre_send_compaction_carries_the_refreshed_memory` 与 `the_first_send_of_a_new_day_recomposes_the_harness_and_refreshes_the_memory_once`（`tests/repl/bot_flush.rs:700`、`:568`）。
- 替代的代价：丢掉「重启后进程在**发送副本**里改了旧消息、截了历史、重排了顺序」这一类检测——而这类差异的来源（overlay 拼接、summary 段序、mount 裁剪）每一个都有定向测试。丢掉的是一张网，留下的是几根钉；对一个机制测试来说够。
- 另一处可减：两个 32k 场景（`:1398`、`:1417`）跑的是同一机制的两种模型形态（tidy / soft-threshold）。软阈值那一个更严（触发合并、`remember` 回显更大、压缩更多：80 vs 60 次），单独保留它、把 tidy 场景标 `#[ignore]` 或删掉，损失的是「记忆短时的路径」——那条路径在 bot_flush 里全部有定向覆盖。省一半时间。
- 不建议删的：不变量 0（每轮恰好一次并有最终回复，`:812-860`）、1、2（超窗真拒绝，`src/testing/growing.rs:194-240`）、4（flush 记账）、5、6a、7。`GrowingProvider` 本身（按请求算 usage、超窗 400）是让「视图 ≤ 窗口」能红的唯一办法，值得留。8k 反例保留为 `#[ignore]`（`:1441`）是对的：它是 `BOT_MIN_WINDOW` 的证据。

### 2.2 `tests/repl/bot_flush.rs`（1305 行，19 个用例）

**结论：成比例。** 每个用例对应一条设计条款或一条已复现的缺陷（S2a/b/c、R1/R2、M2、M12、R8、播种三件），夹具 `Fixture`（`:44-206`）是它们共用的 160 行。可合并的只有形式上的：`a_failed_flush_turn_…` 与 `an_interrupted_flush_turn_…` 已共用 `a_flush_that_did_not_finish`（`:1063`）。`without_the_measurement_the_restart_would_not_compact`（`:1260`）是「证明另一条测试能红」的反向测试，30 行，留。

### 2.3 其它端到端用例

- `tests/cmd/session.rs::mode_agent_is_what_workspace_true_was`（`:720`）+ `tests/fixtures/mode/workspace-true.txt`（178 行，旧二进制抓的请求体）：对一次重命名做逐字节回归。**合并前有用，合并后是负担**——它把 agent 模式的请求体冻在一个固定 fixture 上，此后任何合法改动（harness 措辞、overlay 段序）都要重抓 fixture。建议合并后删 fixture，或改成结构断言（有 overlay、有 skills 段、桶路径正确）。分类：简化（合并后）。
- `tests/repl/commands.rs` 新增 7 个 bot 用例、`tests/session/store.rs` 新增 19 个、`tests/cmd/session.rs` 其余 5 个：每个对应一条 §2.7/§2.2 的行为。成比例。
- `the_wiring_hands_every_session_field_to_the_loop`（`src/cmd/interactive/mod.rs`）：为「`SessionCtx` 多了四个字段、搬运容易漏」写的测试。它是 `SessionCtx` 长胖的症状，不是病；`notices` 与 `recorded_notices` 两个 `Vec<String>`（`src/repl/run.rs:125-129`）只差「是否进 history」，可以是一个 `Vec<(String, bool)>` 或干脆都记录（`SYSTEM_UPDATED` 让模型知道 prompt 变了并无坏处）。微。

### 2.4 `scripts/bot-retention.sh`（409 行，已写未跑）

**结论：实验必要，脚本偏重；最该做的不是再改它，是跑一次。**
- 依据：`scripts/bot-retention.sh:1-58`（头注）、`:150-200`（事实/问卷/日志查询）、`:205-`（`run_group`，tmux 驱动 TUI、spinner 正则、空闲探测）。设计 `docs/design/bot-mode.md:620`、`:625` 把它定为 L1 验收项，§6 #13 说数值「由它定稿」。
- 它换来什么：还没换来任何东西。它经过 codex 第二轮（默认窗口 16000 被新门槛拒、退出码、问卷取值串轮）与验收（无模型干跑）两轮修正，**在从未用真模型跑过的情况下被审到了 CI 标准**。
- 更简的替代：先跑一次（三组里两组，`RETENTION_RECALL_SET` 为空时第三组本来就跳过），拿到数字再决定脚本值不值得维护；或者把 409 行缩成一页操作清单加 100 行驱动。
- 替代的代价：可复现性。定稿常量要能重跑，脚本比清单可靠。所以留，但停止在它跑之前继续打磨。
- 另一条要说清：这个实验测的是「20 个事实经 3 次压缩后的召回」，它能回答「flush 有没有用」，回答不了 8 KiB 该是 8 还是 12、500 B 该是 500 还是 800——设计把「定稿 #13」压在它身上（`:646`、`:669`），是对一个手动实验的过度承诺。见 §4。

---

## 3. 文档与流程开销

**结论：评审流程是资产（十几条真缺陷在合并前被抓住），文件形态是负担；用户可见的文档反而缺席。**

- **`DIVERGENCES.md` X-58…X-63**（`docs/DIVERGENCES.md:327-332`）：六行都是「Rust 与 Go 行为不同、所有模式生效」的记录（mode 枚举、anthropic 连续 user 合并、单写者锁、尾部修复与写侧截断、摘要剥离、保留锚点）。这是项目既有的 Go-parity 账本，条目形式与 X-55/X-56 同款。**资产**——一个以「Go 移植」为身份的项目必须记这些；冗长是既有的房风，不是本分支新增的负担。
- **设计文档**（`docs/design/bot-mode.md`，699 行、112 KB）：`:3` 仍标 `Status: Proposal`，`:5` 说已实现，`:14` 的坐标约定指向 `d46b085`（早于分支），§6 是 24 行的决策表带「已定/推荐/备选」三态，§6 末尾还保留「七个待确认问题」的问答（`:661-669`）。这份文档是用户与多个 agent 异步拍板的**机制**——「已定（2026-09-30）」标注就是决定被记录的地方，它起了作用。但合并之后它应该变成 as-built：删「推荐/备选/rev 1 曾…」的谈判文字、把坐标改到 HEAD、Status 改 Implemented、§6 只留决定与理由。估计能减到 300 行以内。**资产带负担尾巴。**
- **五份评审/验收报告**（1051 行）：fable ×2、codex ×2、verify ×1。它们的产出：密钥正则误报（导致删除整个过滤器）、同进程孤儿、保存失败丢轮、notice 顶成中段孤儿、section 注入、自锁、清空 system 不生效、坏指针解除保护、无锁降级不兼容截回、meta 同值假成功、长跑两处漏判——十二条，全部复现或钉住。**流程是分支上回报率最高的开销。** 文件本身的问题是：五份报告引用的行号分别对应三个已不存在的 HEAD，读者要先确定哪份是最新；`docs/design/` 里 bot-mode 相关文件已有 10 个（recon、research、critique、design、5 份评审、本文）。建议合并时归档到 `docs/design/reviews/bot-mode/` 或压成设计文档的一个「发现 → 修复」附录，并且**这条分支不再新写报告**。
- **缺席的**：CHANGELOG 没有 bot 条目（`grep -n bot CHANGELOG.md` 无命中）；`mode: bot` 在 `README.md` 与 `docs/*.md` 中只出现在 DIVERGENCES 和 starter 注释里。相对设计文档的 699 行，这是明显的失衡：写给自己看的多，写给用户看的没有。
- **「分层生长」的偏离**：设计 `:590` 建议 L0 与 L1 分两个 PR，`:582` 列的 L0 四项都与 bot 无关、所有模式受益。实际提交序列里 `6bd31c1`（锁 + 修复 + 截断，+943）、`8f3c875`（摘要剥离）、`d6c63f2`（`mode` 枚举）都是自足的，可以在 9 月 30 日就进 `main`。把它们攒进一个 14k 行的分支，评审面翻倍，产品晚拿到四个正确性修复。这不是过度设计，是过度捆绑；下次照设计自己说的做。

---

## 4. 配置面（常量）

**结论：没有一个是配置。全是代码常量，用户改不了，这与项目「不为将来预留配置」一致。它们是护栏；问题不在数量，在两处措辞。**

| 常量 | 位置 | 判定 |
|---|---|---|
| `MEMORY_CAP` 8 KiB | `src/agents/memory.rs:33` | 必要：没有它记忆块是无上限的注入通道 |
| `MEMORY_SOFT_CAP` 6 KiB | `:35`；`soft_warning` `:480-489` | 必要：模型自己合并的唯一信号，写入结果与 flush 提示词共用同一句 |
| `MEMORY_LINE_CAP` 500 B | `:37`；`make_line` `:407-428` | 必要（轻）：保住「一行一条」的格式，8 KiB 总上限挡不住一条 7 KiB 的行 |
| `MEMORY_SECTION_CAP` 100 B | `:39`；`Section::parse` `:91-104` | 必要（轻）：codex R3 的修复，标题行也是一行；一处检查一个测试 |
| `BOT_RESERVE_TOKENS` 32k、`BOT_RESERVE_PERCENT` 25、`min(…, window/2)` | `src/repl/context/tokens.rs:45-50`；`bot_threshold_of` `meter.rs:112-117` | 必要，但三项定一个数：`/2` 只在 [32k, 64k) 起作用（`BOT_MIN_WINDOW` 以下不存在）。可写成一个更直白的分段，但改了也是三个数 |
| `BOT_MIN_WINDOW` 32k | `tokens.rs:56`；`check_bot_window` `src/cmd/interactive/bot.rs:71-93` | 必要：8k 反例是硬证据。它顺带长出 `context_window_rows(current, floor)`（`src/repl/commands/settings.rs:90`）这类 UI 细节，属于会累积的那种；留 |
| `FAILURES_BEFORE_ALARM` 2、`RESUME_GAP_NOTICE_SECS` 3600 | `src/repl/bot.rs:41`、`src/cmd/interactive/bot.rs:23` | 给魔数起名，无争议 |

两处要改的是**说法**，不是值：

1. 设计 `:646`、`:669` 说这些数值「由 `scripts/bot-retention.sh` 定稿」。那个实验测的是召回率（§2.4），与 reserve、最小窗口、行上限无关；8 KiB 与 6 KiB 的取舍它也只能给方向。把「定稿」改成「校验方向」，否则这是把一个手动实验当成了配置面的最终权威。
2. codex 第二轮已指出 `BOT_MIN_WINDOW` 的证据是 fake 的 `bytes / 4`，不是真 tokenizer。设计 §4.1 的表已写「32k 是能跑的下限，不是推荐值」，够了。

没有发现「把以后可能要调变成现在就配」：`memory` 集不进 `SET_NAMES`（`src/tool/sets.rs`，`strict` 测试拒绝 `tools: {memory: {}}`），没有 `memory_cap:`、`reserve:`、`min_window:` 这类键。这一条守住了。

---

## 5. 如果只能删/简化三样

1. **长跑 6b 的豁免判定机**（`tests/repl/bot_longrun.rs:326-438`、`:440-542`、`:582-593`、`:1167-1395`，约 400 行）→ 6b 只比 call kind 序列；消息级保证交给 6a、不变量 1 与两条定向测试。顺手把 tidy 场景标 `#[ignore]`，`cargo test` 每次省约 15–25 s。这是分支上唯一「测试需要测试、三轮才收口」的代码，守的东西已有更便宜的守法。
2. **懒物化簇**（`materialized`、`OnCreated`、`on_created`、`never_saved`、`NEVER_SAVED`、stale 修正）→ bot 会话在 `open_bot` 里立即落盘，`NotFound` 一律 `BotMissing`。净减约 100 行产品代码与 3 个测试；代价是一个从未说话的 bot 留一个空 bundle，对 bot 无害。这是分支上唯一为「bot 用不上的性质」保留的间接层。
3. **设计文档瘦身与报告归档**：`bot-mode.md` 改 as-built（Status、坐标、删谈判文字，目标 ≤ 300 行）；五份评审报告归到子目录或压成一个「发现 → 修复」附录；补 CHANGELOG Unreleased 与一段用户文档。这不减代码，减的是下一个读者的时间——对单一操作者的 0.x 项目，这份成本每次打开 `docs/design/` 都要付。

顺手可做、不进前三：删 `BotPointer.v`；`BotRunning` 并入 `Locked`；`Steerer` 对 flush notice 改为丢弃（删 `Requeue`/`held`）；合并后删 `workspace-true.txt` fixture 或改结构断言。

**不要动的**（有人会想动，但它们各自守着一条复现过的损坏）：`repair_tail` 与写侧截断、批次 all-or-nothing 与 `MetaNotSaved`、`seed_resumed` 一簇、`system_cleared` 三分法、`Only`、`BotOwnerUnknown` 的失败关闭、`LockUnsupported` 的失败关闭、`repl::bot` 状态机。

---

## 附：核对方法

- 读了 `docs/design/bot-mode.md` 全文、四份评审与验收报告全文、`docs/DIVERGENCES.md` 与 `agent-mode.md` 的 diff。
- 读了 `src/repl/bot.rs`、`src/session/{error,writer,lock,bot}.rs` 全文，`src/session/{store,loader,tuning,record}.rs`、`src/repl/{run,state}.rs`、`src/repl/turn/{mod,steer}.rs`、`src/repl/context/{meter,tokens}.rs`、`src/repl/commands/{compact,settings,session,mod}.rs`、`src/cmd/interactive/{bot,mod}.rs`、`src/cmd/{assemble,mod,error,resolve,signals,config_cmd}.rs`、`src/config/{agent,strict,mod}.rs`、`src/agents/{memory,harness}.rs`、`src/agents/memory/snapshot.rs`、`src/tool/{dispatch,builtins/memory}.rs` 的相关段或 diff。
- 读了 `tests/repl/bot_longrun.rs`、`src/testing/growing.rs` 的判定与驱动部分，`tests/repl/bot_flush.rs` 的夹具与用例名，`scripts/bot-retention.sh` 头部与结构；列了各测试文件新增用例名。
- 实测：`cargo test --test repl a_bot_runs_two_thousand_turns_in_a_32k_window` 在已构建的 target 上 15.09 s。没有跑完整 `ci.sh`，没有改代码、测试或脚本，没有 commit。
- 「`BotState.name` 的 `#[allow(dead_code)]`」用 `git log -S'allow(dead_code)' main...HEAD -- src/repl/state.rs` 定位到 `cc889fe` 加、`51cb10d` 删。
- 「产品代码里没有 L2/L3/L4 桩」用 `git diff main...HEAD -- src/ | grep '^+' | grep -n 'L2\b\|L3\b\|L4\b\|TODO\|todo!\|unimplemented'` 核对，命中只有 `src/repl/bot.rs` 的模块注释与文案里的普通英文。
