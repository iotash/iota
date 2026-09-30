# bot 模式第二轮评审：第一轮之后的修复

评审日期：2026-10-01。HEAD：`db11a05b905c72b110c3c6c039ed8b24a5ad0c49`。先读了 `bot-mode-review-codex.md`，再核对 fable 的第一轮报告与修复；本轮没有重评整个分支，没有修改代码、测试或提交 commit。

**范围校正**：当前仓库的 `aa46e17..HEAD` 实际只有 `68e1875`、`db11a05` 两个提交。第一轮报告评到 `345fde7`；它之后的六个提交是 `3b7342b`、`47a3217`、`56c80ba`、`aa46e17`、`68e1875`、`db11a05`。R1–R6 的主要修复在前四个里。因此本轮检查 `345fde7..HEAD` 的六个提交，重点检查指定的最后两个提交；`main...HEAD` 的其余实现仅作为调用上下文。下文行号均指本次 HEAD。

**结论：仍暂缓合并。** 原来的 R1 丢尾轮复现、R2 孤儿调用复现已通过回归验证，但 R1 的“批次可重试”保证仍不成立：回滚本身失败后继续追加会写出永久的中段孤儿，已实测。另有无锁降级与截断回滚组合的数据丢失风险（条件性，推测）、meta 同值重试假成功（已实测），以及新窗口门槛使保留率脚本默认无法启动的问题。

## R1–R9 总表

分级表示本轮仍需处理的问题；“原严重”等说明已关闭项的第一轮级别。

| 项目 | 本轮结论 | 级别 | 核对入口 |
|---|---|---|---|
| R1 保存失败后提交压缩、批次重复追加 | **部分修**：原丢尾轮与 meta 重复追加已修，回滚失败的重试仍会写坏历史 | 严重 | `src/repl/commands/compact.rs:328`、`:388`；`src/session/writer.rs:338`；下文实测 |
| R2 notice 把尾部孤儿顶到中段 | **已修** | 原严重，关闭 | `src/repl/run.rs:1245`；`an_interrupted_batch_is_answered_before_the_memory_notice_follows_it` |
| R3 section 注入、归一化顺序 | **已修（按用户的新决定）**：取消密钥拒写，结构约束保留 | 原重要，关闭 | `src/agents/memory.rs:90`、`:407`；`a_section_is_one_heading_line`、`a_text_is_one_line` |
| R4 初始化失败后的自锁 | **已修** | 原重要，关闭 | `src/session/writer.rs:312`；`a_failed_first_open_leaves_the_lock_to_the_retry` |
| R5 删除 system 不生效 | **已修** | 原重要，关闭 | `src/cmd/interactive/bot.rs:214`；`a_removed_system_prompt_is_cleared_not_kept` |
| R6 坏指针解除本体保护 | **已修**；全局失败关闭的代价可接受，错误可操作 | 原重要，关闭 | `src/session/bot.rs:76`；`an_unreadable_pointer_blocks_the_gate_and_delete` |
| R7 长跑与保留率验收缺失 | **部分修，并有新回归**：长跑已交付；断言有宽松处；保留率脚本默认窗口被新校验拒绝，未提供真模型结果 | 重要 | `tests/repl/bot_longrun.rs:854`、`:873`；`scripts/bot-retention.sh:64` |
| R8 flush 仍广告历史挂载工具 | **已修** | 原次要，关闭 | `src/repl/turn/mod.rs:118`；`the_flush_turn_does_not_advertise_a_mounted_tool` |
| R9 无人值守推荐模板缺失 | **已修** | 原次要，关闭 | `src/cmd/config_cmd.rs:54`；`the_starter_bot_entry_uncomments_to_an_unattended_bot` |

## 严重：R1 的剩余缺口与新增风险

### R1：回滚失败被吞掉，下一次追加仍把它当作“未提交批次”

**结论：部分修。** 保存 backlog 成功以后才请求摘要、marker 确认提交以后才安装新视图，这两处顺序是正确的；把 `MetaNotSaved` 与日志失败分开也解决了原来的重复追加。但是“写失败 → 回滚 → 原批次可重试”只在回滚成功时成立，代码却没有检查这个前提。

**依据与原复现验证：**

- `persist_turn` 只在 `Ok` 或 `MetaNotSaved` 后推进水位（`src/repl/run.rs:231`）；`compact_now` 先检查保存结果（`src/repl/commands/compact.rs:328`），marker 的普通错误直接返回，安装历史与 `reseed` 在其后（`:388`、`:404`）。失败不会发送 `Compacted::Done`，因而不会按成功清 flush 状态或刷新记忆。
- 已执行 `a_backlog_the_log_refused_is_saved_before_the_compaction_and_survives_a_restart`（`tests/repl/bot_flush.rs:760`）：重跑 `zero → one → flush → 恢复写盘 → two → reload`，重载视图等于实时发送历史加最终回复，`one` 连同保留尾部没有消失。这个回归用例将恢复点设在第一次 `Compaction failed` 后；第一轮探针“摘要回调里恢复磁盘”的回调现在被保存门挡住，不能再充当恢复信号。验证的是原触发序列及其修复后的恢复路径，没有把未发生的摘要调用说成成功。
- 已执行 `a_failed_meta_rewrite_does_not_double_the_turn`（同文件 `:828`）：meta 临时路径被目录阻塞，恢复后六条 `zero/re zero/one/re one/two/re two` 恰好各一条，`message_count=6`。
- 已执行 `a_failed_batch_is_all_or_nothing_and_a_failed_meta_is_not_a_failed_batch`（`tests/session/store.rs:489`）：覆盖批中途失败且截断成功，以及日志已提交、仅 meta 失败。**没有覆盖截断自身失败**。marker 提交顺序本轮做了源码核对，没有额外注入 marker 的 `sync_all` 故障。

**剩余失败场景（已实测，不是推测）：** `src/session/writer.rs:343`–`:348` 用 `let _ = file.set_len(start).and_then(...)` 丢弃回滚错误，仍只返回原写入错误；不记住未完成的回滚，也不禁止后续追加。`conv_count` 和 `message_count` 则停在批次前（`:239`）。

本轮用公开 `SessionWriter` API，在临时目录运行探针，未改产品代码或测试：

1. 落盘一条 `seed`；构造批次 `user(q) → assistant(c1=read_file) → tool(c1)`，最后一条带 4 字节附件。
2. 将空的 `attachments/` 临时替换为普通文件，让第三条记录的附件写入报 `Not a directory`；给 `messages.jsonl` 设置 macOS `uappnd` 标志，允许追加但禁止截断，确定性模拟回滚失败。
3. 首次追加报错时，磁盘已有 3 条记录，writer 仍只计 1 条。解除两个障碍，**用同一个 writer 重试完全相同的批次**。
4. 结果如下；重载后的角色是 `[User, User, Assistant, User, Assistant, Tool]`。

```text
first_error=Not a directory (os error 20)
records_after_failure=3; writer_count=1
after_identical_retry: log_records=6; writer_count=4
c1_calls=2; c1_results=1; tail_repairs=0
```

第一个 `assistant(c1)` 已成中段孤儿，R2 的尾部修复救不了；内存计数也与磁盘不符。这里的文件标志是故障注入方式，不声称普通磁盘默认如此；真实 I/O 错误导致截断失败或无法确认回滚落盘时，同样必须处理这个分支。特别是 marker 已写入、sync 失败、回滚也失败的情形，不能继续假定磁盘没有 marker（此 marker 情形为**推测**，未注入实测）。

**建议：** 保存批次起点及回滚未确认状态；在确认恢复到起点以前拒绝新追加与压缩，明确报告“写入及回滚均失败”。允许之后重试回滚，或要求关闭并恢复会话；不能把普通可重试错误继续交给上层。补一个回滚失败的行为测试即可，不需要通用事务框架。原 R1 的正常恢复路径应保留现有两条 REPL 回归。

### N1：无锁降级与新的截断回滚不兼容

**结论：修复引入的新问题，严重但条件性；数据丢失过程为推测。** fable M5 的“不支持锁时继续打开”建议不适合与当前回滚方案同时合并。这里不只是少一道竞争提示，而是破坏了 R1 实现依赖的排他性。

**依据：** `src/session/lock.rs:91` 将 `ENOTSUP/EOPNOTSUPP` 变成成功的空 guard，bundle 锁和 bot 锁都使用它（`:60`、`:68`）。`src/session/writer.rs:335` 却明确依赖“持有 bundle 锁、期间无人追加”来截断。`SessionStore::delete` 同样把空 guard 当作删除许可（`src/session/store.rs:409`）。已运行的 `an_unsupported_lock_opens_unlocked`、`a_resume_without_locks_opens_with_a_caution` 证明降级是当前设计行为；本轮没有真实 NFS/SMB 环境，没有声称实测过并发丢数据。

**失败场景（推测）：** 不支持锁的文件系统上，A、B 都成功 resume 同一 bundle。A 记下批次起点 L，B 在其后追加并成功 sync；A 的批次遇到错误，`set_len(L)` 会把 **B 已确认保存的数据** 一并截掉。即使所有截断都成功，也无法保证批次可重试。警告用户不要开第二个进程不能建立这条正确性前提。

**建议：** 对需要独占的写式打开和删除恢复失败关闭，错误说明目录不支持文件锁；已有只读加载不需要这个降级。不要为兼容少数文件系统新增锁实现。若未来确实要支持无锁写入，应另定清楚的能力边界，不能让当前可截断 writer 把空 guard 当作锁。

## 重要：其它新问题与验收缺口

### N2：meta 写失败后，同值重试返回成功但不落盘

**结论：修复引入的新问题，已实测。** 为避免每次启动都刷新 `updated_at` 而增加的“值未变化就不写”优化，比较的是已被上次失败调用修改的内存值，不能代表磁盘已经保存。

**依据：** `src/session/writer.rs:300` 先克隆内存 meta、执行修改，然后只在前后不同的情况下写文件；写失败不恢复内存、不记 dirty。`SessionMeta::write` 也会在实际写盘前更新内存时间（`src/session/meta.rs:133`）。

**失败场景与验证：** 临时目录中的 writer 已有 `seed`；用目录阻塞 `meta.json.tmp`，调用 `update_meta(|m| m.title="changed")` 得到 I/O 错误；移走目录，用同一个 writer 重试相同修改。不追加其它消息，直接读磁盘：

```text
first_error=Is a directory (os error 21)
retry_ok=true; memory_title="changed"; disk_title=""
```

进程此时退出，标题/配置更新丢失；下一次同值重试的 `Ok` 是错误的持久化承诺。后续恰好追加聊天会写 meta，并不能证明这条重试语义正确。

**建议：** 写失败后保留 dirty 标记，使同值重试仍写盘；或仅在成功时提交 meta 的内存变更。保留“已成功保存且无变化时不刷新时间”的优化，补“失败 → 外部条件恢复 → 同值重试 → 不发消息直接重载”用例。

### R7：长跑已交付，保留率验收还不能关闭

**结论：部分修。** 两个长跑测试是真实运行 REPL、保存日志和重启的机制验证，有实质价值；它们没有验证真实模型的事实保留率。新增脚本是交付进展，但最后一个提交把其默认运行配置变成了非法配置。

**依据与失败场景：**

- `scripts/bot-retention.sh:42`、`:64` 仍使用默认窗口 **16000**，并在 `:197` 写进 scratch 配置；`src/cmd/interactive/mod.rs:477` 调用的 `check_bot_window` 拒绝 `<32000`（`src/cmd/interactive/bot.rs:81`）。照脚本示例执行，bot 在 TUI 出现前被拒；脚本 `:270`–`:273` 只会等不到提示符。此冲突由代码常量与已通过的 `a_bot_needs_a_32k_window` 边界测试确认，未花费真实 API token 跑脚本。
- 脚本达到 `RETENTION_MAX_TURNS` 仍不足 N 次压缩时，只 `break`，随后照常问卷与评分（`:290`–`:304`），没有把该组标成验收失败；所有组失败也只打印错误，最后通常仍退出 0（`:342`–`:345`）。这会给未达到实验条件的结果留下“正常结果表”的外观。
- 若问卷回复触发自动 flush，`wait_idle` 会等 flush 结束，但评分取的是整个日志的最后一个 assistant（`:153`、`:160`、`:310`），可能取到 flush 回复而非问卷。此具体时序为**推测**，未向真实模型复现。
- 本轮未发现附带的真模型 noflush/flush 召回结果；设计仍明确将 8 KiB / 500 B / 1500 词标为临时值（`docs/design/bot-mode.md:615`、`:635`）。不把“有脚本”说成“已验收”，也不推断作者私下没有做过实验。

**验证方式：** `bash -n scripts/bot-retention.sh` 通过；两个 32k 长跑及显式启用的 8k 反例结果见下一节。没有执行收费的真实模型实验。

**建议：** 脚本默认窗口与支持下限对齐，启动前验证参数；不足 N 次压缩与组执行失败应给非零退出或明确无效状态；按问卷所在用户轮定位答案。真模型结果继续作为 L1 验收项，数值继续标临时，不需要为此提前实现 L2 recall。

## 长跑测试、32k 与 resume meter 的判断

### 实测结果

运行 `cargo test --test repl bot_longrun -- --include-ignored --nocapture`，两个 32k 场景通过，8k 反例按预期失败，整条命令退出 101。

| 场景 | 打印的已回答轮数 | 压缩 marker | 超窗拒绝 | 最大 input+output | 重启对照 | 最大记忆正文 |
|---|---:|---:|---:|---:|---|---:|
| 32k，保持 12 行记忆 | 2000/2000 | 60 | 0 | 17140 | 24/24 视图相同；无后续差异 | 930 B |
| 32k，记忆停在软阈值附近 | 2000/2000 | 80 | 0 | 22994 | 24/24 视图相同；1 次后续差异被 refresh 规则豁免 | 6259 B |
| 8192，软阈值记忆，ignored 反例 | 842/2000 | 208 | 1245 次调用 | 9051（已回答调用） | 24/24 加载视图相同；18 次后续调用不同 | 6214 B |

两个 32k 场景分别约 29.7 s、31.2 s；最终日志约 3764 KiB、4644 KiB，最慢 open 约 19.5 ms、18.5 ms。8k 失败的是不变量 2、3、6b。当前 8k 结果是约每 9.6 个尝试轮一条 marker；测试注释 `tests/repl/bot_longrun.rs:889` 的“每 2.2 轮”不是本轮 HEAD 的实测数，不能原样当新结果引用。

### 长跑是不是假绿、断言够不够硬

**结论：本次两组正常结果不是单纯“把失败吞掉”的绿，但存在值得收紧的假绿通道，R7 不能无条件关闭。**

**成立的部分：**

- fake 根据请求消息、工具参数与定义计算 `bytes/4`，输入超窗会返回真实形状的 400（`src/testing/growing.rs:164`、`:194`）；测试又检查所有调用都未拒绝且 `input+output <= window`（`tests/repl/bot_longrun.rs:597`）。因此即便 REPL 吃掉 provider 错误，也不会靠忽略该错误使这项变绿。fake 只拒绝输入超窗，输出使总量越界仍由测试断言捕获。
- 日志按字节检查前缀、marker 按日志核对 flush 数、记忆按正文 cap 核对（同文件 `:131`、`:540`、`:688`、`:711`）。重启不是两次读同一个磁盘文件来互证，而是保留磁盘副本，让原进程继续跑作对照，再恢复副本重启（`:463`–`:519`）。
- 当前 32k 软阈值场景被豁免的差异可以解释：#1683 的 flush 在两份不同快照里分别删除 `fact-u307` / `fact-u332`；日志打印了具体位置和调用参数，不是完全隐去差异。

**仍偏弱的地方（重要，失败场景为可静态证明的漏检条件，未修改测试注入回归）：**

1. `answered.len()` 只打印，不断言等于 2000；还包含后来被丢弃的“不重启参考分支”，并且只代表首 round 获得回答，不代表工具轮最终成功并落盘（`:815`–`:833`）。某些轮不完成或仅参考分支完成，不必然触发其它七项。建议直接断言主线完成且已持久化的用户轮号集合等于 `1..=2000`；不要只断言日志行数。
2. `follows_the_refresh` 只看“历史差异发生得比 memory 首次变化更晚”（`:299`），然后整次差异不判失败（`:759`–`:773`）。先出现合法 memory 刷新、后出现真正的历史截断/重排，也会被豁免。当前这一例可人工解释，不代表这个规则足以判断因果。建议比较始终应保持的旧历史前缀，只允许 fake 实际生成的新后缀不同，或固定回复做一组严格续跑对照。
3. 随机重启都是 idle 边界，且使用一个固定 seed、只启用 memory 工具、关闭 agent overlay（`:55`、`:215`、`:248`、`:477`）。这验证不了写入中断、锁失效、任意工具输出、AGENTS.md/skills 体积与真实协议 tokenization。不要拿它替代 R1/R2 的故障测试，也不要把约 4.6 MiB 的加载曲线外推到 256 MiB。

**建议：** 前两项只需局部强化断言；其余明确测试覆盖边界即可，不要求随机 fuzz 框架或把长跑扩成整套真实 API 测试。

### 32k 下限

**结论：作为“当前测试支持的保守启动下限”成立；不是推导出的精确最小值，也不是所有 32k bot 都不会超窗的保证。** 8k 反例会真实拒绝请求，忽略它有明确产品范围依据；不是在仍承诺支持 8k 的同时隐藏失败。

**依据：** `src/repl/context/tokens.rs:56` 的 `BOT_MIN_WINDOW=32000`、启动前校验、两组 32k 测试与 8k 反例。8k 不够、32k 在两种负载下足够，为选择常见的 32k 档提供依据；没有 16k/24k 的边界搜索，因此不能证明 31999 必定失败。无需为了选择支持档位再做精确最小化。

**失败场景与限制：** `bytes/4` 是 fake 的合成计量，不是真实 provider 的 token 数；大 system/AGENTS.md、大工具 schema、多次大工具返回都可能吃掉余量。并且 `/model` 的 Context 仍提供 8000，`set_window` 不设 bot 下限（`src/repl/commands/settings.rs:32`、`:368`）。设计 `docs/design/bot-mode.md:521` 已主动承认运行中换小窗口的例外，本轮不把它当成未披露实现遗漏，但用户可把刚通过启动门的 bot 带回已知失败区域。

**建议（次要，非单独阻断）：** 文案明确是启动支持下限与合成负载证据；bot 的 Context 选项过滤低于下限的预设，或至少当场提示。必须修的是 R7 中脚本默认值与这条门槛的矛盾，而非继续承诺 8k。

### resume 播种 meter

**结论：本次修正成立，计量恢复比纯本地重数准确；不是完整恢复所有状态机状态。**

**依据与验证：** loader 遇到 compaction marker 清 `measured`，只采用之后记录的 usage（`src/session/loader.rs:295`、`:317`；`the_measurement_is_the_last_answer_since_the_last_compaction`）；`seed_resumed` 采用最后测量加尾部估算，没有测量才本地重数加 overhead（`src/repl/context/meter.rs:276`）。启动在播种后判断是否补排 flush（`src/repl/run.rs:603`）。已运行 `a_restart_near_the_threshold_still_compacts_before_the_next_message`、反向用例 `without_the_measurement_the_restart_would_not_compact`、`a_flush_queued_when_the_process_went_down_runs_after_the_restart`，均通过。特别是反向用例证明恢复的是持久化测量，不是测试里短历史恰好越阈值。

**失败场景/边界：** 新进程更换模型、system、项目或记忆时，旧 usage 不等于新请求精确成本；`set_overhead` 对已有实测值不重定价（`src/repl/context/meter.rs:297`）。无测量时的 overhead 只补 memory 与启动工具定义，不包括 agent overlay/harness 的全部成本（`src/repl/run.rs:1005`）。失败计数、snooze 与 flush 阶段也未从日志完全恢复，`Event::Resumed { over }` 只是补排规则；8k 反例实际显示过这种分歧，32k 通过不能证明其它负载下永不发生。

**建议：** 接受这次修复，不要求持久化整套状态机；说明“同配置、正常 idle 恢复”的保证，避免称作精确 token 恢复。改变请求构成后的估算失准应与既有超窗处理一起跟进，不把它冒充本轮已复现的数据丢失。

## 已关闭项的逐条复核

### R2：中断与 notice 的顺序

**结论：已修，没有发现这次 R2 修复引入新的配对问题。** `interrupt_turn` 在安装保留历史后、第一次保存前调用 `repair_tail`（`src/repl/run.rs:1245`–`:1270`），随后主循环才 `record_memory_writes`（`:964`）；被修改的是运行中的 `conv.history`，下一次发送也用它。

**验证与原失败场景：** `an_interrupted_batch_is_answered_before_the_memory_notice_follows_it` 重跑 `c1=remember` 成功、`c2=read_file` 审批取消、同进程 `again`、再 resume。`read_file` 与原探针的 shell 使用同一个审批中断出口（`src/repl/turn/tools.rs:221`）。测试按连续工具结果逐 ID 比较，检查恰好两个结果、c2 为 `INTERRUPTED_RESULT`、notice 紧跟 c2，resume 的 `repaired=0` 且视图相同（`tests/repl/bot_flush.rs:877`、`:925`–`:948`）。不是仅断言 notice 存在。真实 Anthropic API 未调用。

**建议：** 保留该回归即可；不要求补任意中段日志修复。R1 的残余会制造另一种中段孤儿，应在 writer 解决，不归罪于已修的 R2。

### R3：删除密钥拒写，保留结构校验

**结论：按用户的新决定已修，决定可接受。** 不再要求“像密钥的字符串必须被拒绝”；正则无法控制所有记忆内容，且原来有误报。`Bearer\nFAKE_TEST_TOKEN` 现在归一化后允许保存是预期变化，不能再列成漏洞。

**依据与验证：** `Section::parse` 在写标题前拒绝换行、CR、其它危险控制字符、Unicode 行/段分隔符并限制长度（`src/agents/memory.rs:90`）；工具入口确实经过它（`src/tool/builtins/memory.rs:136`）。`make_line` 先归一化，再检查最终单行及 500 B 上限（`src/agents/memory.rs:407`）。已运行 `a_section_is_one_heading_line`，包括原 `Project: x\n## User\n- Bearer FAKE` 攻击串；已运行 `a_text_is_one_line`，检查不会新增无来源行、add/replace 都拒绝残留控制字符。允许 tab 不会产生第二个标题行。

**失败场景/剩余注入面：** 本轮未找到工具输入能绕过这些检查生成第二行标题或无来源条目的路径。自然语言指令仍能作为记忆内容出现，模型也仍能自称 `source=user`；这是内容信任边界，不是单行校验能消除的语义注入。现有 `<memory>` 数据前言与结束标签转义仍在（`src/agents/memory/snapshot.rs:177`–`:187`），不能据此宣称彻底免疫 prompt injection。

**建议：** 保持删除密钥过滤的决定和现有结构检查；不重建密钥扫描器，不增加无关审批。

### R4：初始化自锁

**结论：已修。** guard 先放在局部变量，日志打开成功后才存入 writer（`src/session/writer.rs:317`）。打开失败会释放局部 guard，下一次不会被自己的锁挡住。

**验证与原失败场景：** 已运行 `a_failed_first_open_leaves_the_lock_to_the_retry`（`tests/session/store.rs:467`）：先把 `messages.jsonl` 变成目录导致 open 失败，移走障碍，同一个 writer 追加成功且只有一条记录；另一个 writer 仍被锁拒绝。**建议：** 无需再改这项生命周期；N1 的无锁降级是另一个新增问题。

### R5：清空 system

**结论：已修。** 显式空 `Body::System` 写成 `system_cleared:true`（`src/session/writer.rs:460`）；loader 用该标志区分清空与历史遗留的空 ToolsMount（`src/session/loader.rs:303`）。`adopt_system` 先追加标志，再移除当前视图头（`src/cmd/interactive/bot.rs:228`）。

**验证与原失败场景：** 已运行 `a_removed_system_prompt_is_cleared_not_kept`：非空启动 → 空配置发送 → 再次空配置重启发送，断言请求里没有旧 prompt，更新提示只出现一次（同文件 `:805`）。`a_legacy_empty_mount_does_not_clear_the_prompt` 同时保住旧日志兼容（`:833`）。**建议：** 关闭 R5，不需要再改日志表示。

### R6：坏指针的失败关闭与爆炸半径

**结论：已修，当前爆炸半径可接受。** 一个不可解析指针确实阻止所有依赖归属判断的普通 resume/delete；因为不知道它指向谁，无法安全只拦“它自己的”会话。保留正常新建、只读加载和直接修复指针的通路，比另加所有权索引更符合 v1 范围。

**依据、失败场景与验证：** `pointers` 把目录/指针读取失败连路径返回（`src/session/bot.rs:76`），`bot_owner` 映射成 `BotOwnerUnknown`，普通 CLI resume 在打开 writer 前过门（`src/session/store.rs:144`；`src/cmd/interactive/mod.rs:533`），delete 也过门（`src/session/store.rs:407`）。已运行 `an_unreadable_pointer_blocks_the_gate_and_delete`：坏指针时 bot 本体与无主普通会话都拒删，修复指针后恢复正常；还运行了 CLI 用例 `resume_refuses_while_a_bot_pointer_is_unreadable`（`tests/cmd/session.rs:1058`）。

错误包含具体文件路径、原因，以及 `repair or delete ... (without it the bot starts a new session; memory is kept)`（`src/session/error.rs:124`），足够操作；删除指针不会删除旧 bundle，但会解除归属关系，文案说明重开新会话是必要的。**建议：** 接受本轮处置，不为了缩小这个故障范围引入第二份归属真相。列表继续只读跳过坏指针，不应当被当作写操作的授权判断。

### R8：flush 中的历史工具挂载

**结论：已修。** `TurnCtx::send_history` 只对发送副本过滤 schema，空挂载整条移除，原历史不变（`src/repl/turn/mod.rs:118`–`:144`）；dispatcher 的执行过滤仍保留。

**验证与原失败场景：** 已运行 `the_flush_turn_does_not_advertise_a_mounted_tool`（`tests/repl/bot_flush.rs:969`），fixture 包含纯非 memory 挂载及混合挂载；普通轮完整发送，flush 各轮只剩 remember，并断言没有空 system 伪挂载。**建议：** 关闭，无需改 provider 编码层。

### R9：无人值守模板

**结论：已修。** starter 增加 opt-in 的 bot 示例，明确默认等审批，示例启用 `auto_write`、`sandbox:auto`、`auto_run`，并写清无沙箱环境下 `auto_run` 的实际行为（`src/cmd/config_cmd.rs:54`）。

**验证与原失败场景：** 已运行 `the_starter_bot_entry_uncomments_to_an_unattended_bot`：直接取消注释后配置可解析，三项工具设置正确，默认 agent 未改变。原先“按推荐例子配置却在第一次审批停住”的文档缺口已补。**建议：** 关闭，不改默认审批政策。

## 验证记录与合并门槛

已执行：

```text
cargo test --lib --test session --test repl --test cmd --test layering --quiet
1106 lib + 88 session + 145 repl + 127 cmd + 4 layering 通过；1 个 8k 反例 ignored

cargo test --test tool --test provider --quiet
101 tool + 129 provider 通过

cargo test --test repl bot_longrun -- --include-ignored --nocapture
2 个 32k 场景通过；8k 反例按预期失败（退出 101）

bash -n scripts/bot-retention.sh
通过（不代表真实模型实验已执行）
```

另用当前已构建库的公开 API，通过 stdin 编译临时探针，实测 R1 回滚失败和 N2 meta 同值重试；故障目录均为临时目录。没有改仓库代码/测试，没有访问真实会话或调用真实模型；未跑 Windows/NFS/SMB 环境。

**合并前必须修：**

1. **R1：** 回滚失败后禁止盲目重试批次，直到确认恢复或明确中止写入；补回滚失败的断言，保证不会再生成中段孤儿。
2. **N1：** 写式打开与删除不能在锁不支持时假装持有独占锁，尤其不能让新的截断回滚在无锁状态运行。
3. **N2：** meta 失败留下的未保存状态不能被“无变化”优化吞掉；同值重试必须真的写盘。
4. **R7 的确定性回归：** 保留率脚本默认配置应能通过新的窗口校验，并明确拒绝未达到实验条件的结果；问卷取值绑定到对应轮。长跑增加主线完成数断言，收紧 refresh 后的全量豁免，避免把当前覆盖说得比实际更强。

真模型保留率结果与临时常量定稿仍是 **L1 完成前** 的验收事项；若本次只合并机制，可以继续明确标为未验收，不要求提前做 L2。32k 的精确最小化、全量状态机持久化、通用事务系统和任意中段日志修复都不是本轮合并要求。
