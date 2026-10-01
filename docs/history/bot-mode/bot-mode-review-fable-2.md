# bot 模式 v1 实现评审（第二轮：修复核对）

Status: **Review**（第二轮，2026-10-01）· 对象：第一轮评审之后的六个 commit，`3b7342b`（drop the secret filter）到 `db11a05`（HEAD），即 `git diff 345fde7..HEAD`。任务书写的 `aa46e17..HEAD` 只含两个 commit；按「六个 commit」取 `345fde7`（两份第一轮报告的 HEAD）为基。· 依据：`bot-mode-review-fable.md`（下称我的第一轮）、`bot-mode-review-codex.md`（下称 codex）、`docs/design/bot-mode.md`（rev 2，随修复更新）。

坐标约定：`src/...:行号` 指 `db11a05`。标「推测」的条目只有代码推理链，没有运行复现。本评审**不改任何代码或测试**。

本地核对（`db11a05`）：

```text
cargo test --lib --test session --test repl --test cmd --test tool --test layering
1106 lib + 127 cmd + 4 layering + 145 repl (1 ignored) + 88 session + 101 tool，全绿
cargo clippy --all-targets -- -D warnings          通过
cargo test --test repl -- bot_longrun --nocapture   两个 32k 场景各约 24–25 s，七项不变量全 [ok]
cargo test --test repl -- --ignored a_bot_whose_memory_sits_at_the_soft_threshold_outgrows_an_8k_window
                                                   按声明失败于 2、3、6b（1245/2668 次调用超窗被拒）
```

分支仍未推送，Windows 一腿没有跑过；涉及 Windows 的判断标「推测」。

---

## 0. 总体判断

**没有严重发现，也没有需要先修再合并的重要发现。**

- 我第一轮的两条必修：R1（密钥正则）被**整个移除**——这个决定成立，留下两处小洞（flush 提示词没提密钥；文档没说「删掉不等于抹掉」），都是次要。R2（同进程孤儿）修好了，修在比我建议的更早的位置（落盘前、notice 前），换成 SIGTERM、审批门、并行批三条路径都成立。
- codex 的六条：R1–R6 都修好了，我没有找到换个路径还能复现的。修复引入的新问题有五条，全是次要（§3）。
- 长跑测试不是假绿：超窗真的报错（8k 复现证明这条断言能红）、重启对照按 `Message` 相等（含 usage）逐条比、种子固定可复现。它自己的软点是 6b 那条「记忆块刷新之后的差异豁免」比需要的宽，但两轮 32k 只豁免了 1 例，我核对过那一例确是模型对新块的选择（§4.2）。两个发现的处置合理（§4.3）。
- **能合并。**

---

## 1. 我第一轮两条必修的处置

### R1 密钥拒写 → 整个移除（`3b7342b`）

- **结论**：决定成立。理由链站得住：(1) 误报是真实的且落在 flush 轮的合法写入上；(2) 那张表的召回本来就低——裸密码、base64、任何不带已知前缀的 token 都过；(3) 真正的防线是可见性（Expanded 展示、写入 notice、`.prev`、明文可 diff），这些都在；(4) 模型侧的提示留在工具描述里，是它该在的地方。移除的同时把 codex R3 的结构性检查留下并加强了（§2 R3）。
- **依据**：`src/agents/memory.rs:91-115`（`Section::parse` 先拒换行/控制字符/超 100 字节再 trim）、`:364-367`（`breaks_line`）、`:370-390`（`normalize` 走 `str::lines`，`\r\n` 也折叠）、`:407-425`（`make_line` 对归一化后的文本判空、控制字符、500 字节）；`src/tool/builtins/memory.rs:26` 描述末句 `Do not store secrets or tokens here: the file is plain text and may be committed to git`。设计同步：§3.7 第 4 条、§6 #21、§7「同步盘」「不透明的服务端记忆」、L1 表——都如实降级，没有残留「密钥拒写」的承诺。全仓 grep：用户文档从未提过密钥门（bot 模式本身也还没有用户文档），CHANGELOG 没有 bot 条目，所以没有陈旧描述要改。
- **留下的洞（都次要）**：
  1. **flush 提示词没提密钥**。`FLUSH_NOTICE`（`src/repl/bot.rs:32`）让模型把「read in tool output」的内容也存成 `[inferred]`，这恰是把 `.env` / token 写进记忆最可能的路径；工具描述那句在 flush 轮也被广告（模型看得到），但 flush 轮是被明确要求「现在保存」的那一轮，提示词自己该说一句。建议：`FLUSH_NOTICE` 加半句 `never a secret or a token`。一行。
  2. **文档没说「删掉不等于抹掉」**。一行密钥进了 `MEMORY.md` 之后，`remember` 的参数在 `messages.jsonl`、Expanded 展示在 transcript、旧版在 `MEMORY.md.prev`；用户 `remove` 或手删只清了常驻层。§3.7 现在写「风险由明文、可 diff 与可回滚承担」，应补一句抹掉的做法（删 `.prev`；日志里那条只能手工处理）。
  3. 工具描述提到 git；`bots/<name>/` 默认不在任何仓库里，进 git 是用户自己的选择。措辞可以接受——与 §7「`bots/<name>/` 可以放同步盘；不要把密钥写进记忆」并读，明文 / 同步盘 / 可能进 git 三件事文档里都有了。

### R2 同进程内的孤儿 `tool_use`（`47a3217`）

- **结论**：修好，且路径比我建议的完整——不只是「再发消息不 400」，也堵住了 codex R2「notice 把尾部顶成中段」。
- **依据**：`src/repl/run.rs:1249-1251`（`interrupt_turn` 在 `persist` 分支、`persist_turn` 之前跑 `repair_tail`）、`:1270`（随后落盘）、`:964`（`record_memory_writes` 在这之后才追加 notice）。三处 `interrupted()`（审批门 `turn/tools.rs:227`、并行批 `:193`、顺序结果 `:279`）都经 `TurnReport::failed` 回来，它不带 partial（`turn/mod.rs:240-248`），所以 history 尾是 `assistant(tool_calls)` + 已答的 tool 结果，`repair_tail` 只看最后一条非 tool 消息（`session/loader.rs:257-271`）正好命中；流式中断带 partial 时 push 的是没有 tool_calls 的 assistant，而它前面的 round 都已配对。测试 `an_interrupted_batch_is_answered_before_the_memory_notice_follows_it`（`tests/repl/bot_flush.rs:896`）断言下一次 send 逐调用配对（`assert_paired`）、c2 的结果是 `INTERRUPTED_RESULT`、记忆 notice 紧跟在 c2 结果之后、resume 时 `repaired == 0` 且视图一致。
- **换路径复现**：试了三条推理，都不成立。(1) 非中断的失败分支整轮回滚到 `hist0 - 1`（`run.rs:909`），没有孤儿；(2) SIGTERM / SIGHUP 取消根 token，走同一条中断路径；(3) 失败分支之后 `record_memory_writes` 追加的 notice 落在上一轮末尾，也没有孤儿。
- 设计 §7.1 那条「同进程内被中断的轮可能当场留下孤儿」已删，§2.7 补了说明（`56c80ba`）。一致。

---

## 2. codex 六条修复核对

### R1 保存失败后压缩丢数据（`47a3217`）

- **结论**：修好，分两层。编排层：`persist_turn` 返回是否落盘；`compact_now` 先落 backlog，落不了就不压缩；marker 落盘之后才装新视图、跳水位。writer 层：一批要么整批进日志，要么截回批起点；只有 meta 失败是 `MetaNotSaved`，调用方不重追加。
- **依据**：`src/repl/run.rs:231-249`；`src/repl/commands/compact.rs:328-333`（gate，`Compaction failed: the conversation is not saved yet`）、`:383-406`（marker 落盘后才 `history = ...`、`persisted = len`）；`src/session/writer.rs:217-244`（`log_len` → 批写 → `settle_batch` → 计数 → `write_meta_after_log`）、`:338-349`（`set_len(start)` + `sync_all`；bundle 锁在手上，没人插队）、`:353-357`。`on_created` 钩子在 `ensure_created` 里、批写之前跑（`:323-326`），所以 codex 复现用的「钩子持续失败」不会留下半批。测试：`a_backlog_the_log_refused_is_saved_before_the_compaction_and_survives_a_restart`（`bot_flush.rs:760`：钩子失败到打出「Compaction failed」为止；断言期间零次摘要调用、重载视图 = 最后一次 send + 回复、首条是 `summary_preamble + one`）；`a_failed_meta_rewrite_does_not_double_the_turn`（`:828`：`meta.json.tmp` 变目录；断言六条不重复、`message_count == 6`）；`a_failed_batch_is_all_or_nothing_and_a_failed_meta_is_not_a_failed_batch`（`tests/session/store.rs:489`：第二条记录超 32 MiB 拒写 → 日志截回 1 行、计数不动；meta 失败 → 3 行、`MetaNotSaved`；继续写 → 4 条，resume 一致）。
- **换路径**：(1) marker 落盘失败 → 早退，`repl.conv.history` 未替换、水位未动，状态机记一次失败，下次 `BeforeSend` 重试——对。(2) `Ok(out)` 分支仍忽略 `persist_turn` 的返回值、`landed = true`（`run.rs:946`、`:960`）——codex 指的 `:898/:912` 那处没动，但压缩前的 gate 让它无害：未保存的轮只会让 flush 提前排队，压缩不会跳过它。(3) 手动 `/compact` 与普通会话同样受 gate 保护——行为变化：日志不可写时 `/compact` 报错而不是静默丢中段，更对。
- **新问题**：§3 N1、N2、N3。

### R2 notice 把尾部孤儿顶成中段（`47a3217`）

同 §1 R2。codex 的具体场景（`remember` 成功 + `read_file` 审批取消 + 再发送 + 再 resume）就是那条测试。

### R3 `section` / 归一化顺序绕过（`3b7342b`）

- **结论**：修好。`section` 一个标题行、100 字节；`text` 先归一化再判空、控制字符、500 字节。
- **依据**：同 §1 R1。补充核对：`normalize` 折叠 `\r\n`，孤立 `\r` 留下被拒——不会误伤 Windows 风格换行；`Project: x ## User` 仍是一个标题行（写成 `## Project: x ## User`），注入不到行级。测试 `a_section_is_one_heading_line`（五个反例 + 上限两侧）、`a_text_is_one_line`（断言 `body.lines().count()` 只多 1、`## User` 仍只有 2 处、没有独立的 `- planted` 行；`\r`、U+2029、ESC 序列在 `add` 与 `replace` 都拒）、`bad_arguments_are_refused` 里 codex 的 section 注入样例（`src/tool/builtins/memory/tests.rs`）。

### R4 初始化失败自锁（`47a3217`）

- **结论**：修好。锁先取到局部变量，日志句柄打开成功后才存进 `self.lock`；打开失败时局部 `HeldLock` drop → `unlock`。
- **依据**：`src/session/writer.rs:312-328`；`src/session/lock.rs:49-56`（Drop 只对真持有的锁 unlock）。测试 `a_failed_first_open_leaves_the_lock_to_the_retry`（`tests/session/store.rs:467`：`messages.jsonl` 变目录 → 失败 → 移走 → 同一 writer 成功 → 第二个 store 被 `Locked` 拒，证明锁此后是真持有的）。
- **换路径**：`created = true` 之后 `meta.write` 失败：锁与句柄都留着，下次直接写——对。

### R5 删 `system:` 不生效（`56c80ba`）

- **结论**：修好。空 config 追加一条带 `system_cleared: true` 的空 system 记录；loader 让它胜出、视图无 system；旧版误落盘的空 mount 没有这个键，仍被忽略。
- **依据**：`src/cmd/interactive/bot.rs:214-238`（`adopt_system` 三种情形）；`src/session/writer.rs:461`（`Body::System && content.is_empty()` → 标记）；`src/session/loader.rs:311-315`、`:323`（空且无标记的记录不参与；有标记的胜出但不进视图）；`src/session/record.rs` 新键 `skip_serializing_if`，老日志逐字不变。`run()` 对 resumed 历史不再插 system（`run.rs:398-405`），所以清空后 `params.system == ""` 也不会被塞回去。测试 `a_removed_system_prompt_is_cleared_not_kept`（`bot.rs:805`：断言三次 open **发出去的内容**，不是 meta）、`a_legacy_empty_mount_does_not_clear_the_prompt`（`:833`：手工追加 `{"role":"system"}`，断言 `keep me` 仍胜出，清空后最后一行是 `{"role":"system","system_cleared":true}`、视图无 system）。
- Go 互通：Go 读到这条记录按「最后一条 system 胜出」得到空 system，语义一致。
- **新问题**：§3 N4。

### R6 坏指针解除本体保护（`47a3217` + `56c80ba`）

- **结论**：修好，失败关闭。`pointers` 对读不出的指针 / bot 目录 / bots 根返回 `(path, err)`；`bot_owner` 包成 `BotOwnerUnknown`（带路径与做法）；`check_not_bot_owned` 传播；`delete`、`iota resume`、headless resume、`/session` 切换四个入口都过这道门；只有列表（`bot_sessions`）跳过坏指针。
- **依据**：`src/session/bot.rs:76-100`；`src/session/store.rs:144-160`、`:164-176`、`:181`、`:407`；`src/cmd/interactive/mod.rs:533`、`src/cmd/mod.rs:440`、`src/repl/commands/session.rs:176`；`src/session/error.rs:86-95`、`:126-134`（`repair or delete <bots>/<name>/bot.json (without it the bot starts a new session; memory is kept)`）。测试 `an_unreadable_pointer_blocks_the_gate_and_delete`（`tests/session/store.rs:603`：坏指针期间 bot 的 id 与无关 id 都拒、`find_dir` 仍在、列表跳过、修好后无关 id 可删）；`resume_refuses_while_a_bot_pointer_is_unreadable`（`tests/cmd/session.rs:1058`：撕裂 JSON → 退出码 1、文案含路径、日志字节不变、零次请求）。`pointers_scan_the_bots_directory` 从「跳过坏指针是通过条件」改成「坏指针是错误并报路径」——codex 指出的那条危险测试已经反过来了。
- **代价**（设计 §2.7 写明）：一个坏指针挡住所有普通会话的 resume / delete，直到修好或删掉。文案可直接照做。接受。

---

## 3. 修复引入的新问题

都是次要，没有一条改坏正常路径。

### N1 marker 落盘失败时摘要调用已 `book_call`，`last_usage` 残留

- **结论**：`compact_now` 在 marker 写入前 `book_call(usage)`（`src/repl/commands/compact.rs:377`），失败早退时不 `reseed`，`Occupancy.last_usage` 留着摘要调用的 usage（`src/repl/context/meter.rs:464-475`），下一次 `settle_with`（`:84-93`）会把它当占用消费。以前压缩是无条件完成的，`reseed` 总会清掉它，这是新路径。
- **失败场景**：marker 写失败 → 用户下一句 → 重试压缩又失败两次（Alarm）→ 消息照发 → 这一轮网络错误回滚（`ctxm.reset` 不清 `last_usage`）→ 再下一轮结束 `update` 把摘要调用的 `input + output` 当成占用。一轮内的错估，下一轮 round 结算就纠正。
- **建议**：把 `book_call` 挪到 marker 落盘成功之后（`booked` 只是要写进 marker 的值，可以先算后记），或失败分支清 `last_usage`。三行。

### N2 `MetaNotSaved` 在 `store.resume` 与 `adopt_system` 里是硬错误

- **结论**：`resume` 追加修复结果（`src/session/store.rs:357`）、`adopt_system` 追加 system 记录（`src/cmd/interactive/bot.rs:229`）都用 `?`，meta 单独失败会让这次打开整个失败，虽然日志已经写好。下次打开会成功（修复结果已配对、system 已相同），所以只是一次多余的失败启动。
- **建议**：两处对 `MetaNotSaved` 只 warning。不改也可以。

### N3 Windows 上 `settle_batch` 的 `set_len` 对只 append 的句柄（推测）

- **结论**：非 unix 的 `open_append_0644` 只 `.append(true)`（`src/session/writer.rs:441-446`）；std 在 Windows 上给这种句柄的是 `FILE_GENERIC_WRITE & !FILE_WRITE_DATA`，`set_len`（`SetFileInformationByHandle` / `FileEndOfFileInfo`）是否需要 `FILE_WRITE_DATA` 我没有把握。若需要，Windows 上截回是空操作，批中途失败后重试会重追加——codex R1 的第二半在 Windows 回来；而 `settle_batch` 用 `let _ =` 吞掉了截回失败，看不见。
- **建议**：推送后看 Windows 腿的 `a_failed_batch_is_all_or_nothing_and_a_failed_meta_is_not_a_failed_batch`——它断言截回后只剩 1 行，会直接暴露。若红，非 unix 分支加 `.write(true)`。

### N4 `system_cleared` 靠「空的 `Body::System`」推断

- **结论**：`to_record` 用 `matches!(body, Body::System) && content.is_empty()`（`src/session/writer.rs:461`）；`Message::system_tools(vec![])` 构造的正是这个形状（`src/provider/model.rs:297-305`）。今天两个 mount 调用点（`src/repl/turn/tools.rs:158-163`、`src/headless/run.rs:315-318`）和 flush 轮的收窄（`src/repl/turn/mod.rs:141`）都守着非空，所以不是 bug。隐患：将来任何路径 push 一条空 system 进历史，落盘就是「清空 prompt」，下次加载视图无 system，而且不会有人注意到。
- **建议**：给 `Message` 一个显式的清空构造（或让 `system_tools` 对空 defs 返回 `None` / 断言），`to_record` 认构造而不是认形状。十行。

### N5 M4 只修了一半：resume notice 本身仍推动 `updated_at`

- **结论**：`update_meta` 比较前后值（`src/session/writer.rs:300-307`）解决了参数回写与 `stamp_bundle`；但 gap ≥ 1 小时时 `resume_notices` 生成的「Resumed after …」经 `record_notices` → `persist_turn` 落盘（`src/cmd/interactive/mod.rs:515-516`、`src/repl/run.rs:597`、`:266-275`），`updated_at` 随之更新。测试 `opening_and_closing_does_not_move_the_last_written_time`（`bot.rs:870`）传的是空 `recorded_notices`，绕开了这条路径。
- **失败场景**：我第一轮的原场景不变：09:00 开一眼关掉（写一条 notice），20:00 再开：「Resumed after 11 hours (last message 09:00)」，而最后一条真消息是三天前；天天开一次，日志天天多一条。
- **建议**：措辞改 `last activity`（最省，且与设计 §2.5 现在的「`updated_at` 代表最后一次真正的写入」一致——notice 确实是写入，是文案在跟它打架）；要精确到「最后一条消息」得等 L2 的 `SessionRecord.at`。

---

## 4. 长跑测试（`tests/repl/bot_longrun.rs` + `src/testing/growing.rs`）

### 4.1 是不是假绿——不是

- **超窗真报错**：`GrowingProvider::answer`（`src/testing/growing.rs:194-240`）按「发送字节 / 4」算 input（消息文本、tool call 名与参数、工具定义的名/描述/schema，`:164-191`），`input > window` 就返回 openai 形状的 400 `context_length_exceeded`（`:449-462`），不回答。不变量 2 要求 `refused.is_empty() && widest <= window`（`bot_longrun.rs:608-617`）。8k 复现里 1245/2668 次被拒、不变量 2 红——这条断言能红。
- **重启对照真逐字节**：6a 比 `view_at_drop`（`:306-316`）两边的 `&[Message]`，`Message` 派生 `PartialEq`（`src/provider/model.rs:93`），含 body（usage、raw_content、tool_calls）与 attachments；两轮 32k 都是 24/24 可比、0 差异，并要求至少一半 drop 可比才算过（`:735`），不会因为「两边都先压缩了」而空转。6b 比 call kind 与 `view_of`（去掉 system 段的历史），逐调用直到下一个用户轮（`:354-400`）。
- **随机性可复现**：`SEED` 固定；`drop_points` 是 splitmix64 流（`:69-86`）；回复长度、摘要长度用 `mix(seed ^ turn)`；harness 时钟固定为 `TODAY`（`db11a05` 补的，避免跨午夜一侧刷新一侧不刷新）。我跑了两次（全套一次、`--nocapture` 一次），七项 verdict 的数字完全一致。
- **日志只增**：每次 provider 调用读全文比前缀（`Watch::look`）；2114 / 2148 次增长、0 次改写。
- **压缩次数**：期望值由测得的 growth / room / 半轮 / flush 交换算出，比值 1.00（接受 0.85–1.15）。这是自洽检查，不是外部断言，但足够挡住退化——8k 是 0.75，红。
- **不变量 4**：按日志逐 marker 数 flush notice（`flush_accounting`，`:540`），要求恰好 `(1, false)` 或 `(0, true)`。32k：54 + 6、66 + 14；8k：123 + 85。

### 4.2 断言够不够硬——两处软点

- **6b 的豁免比需要的宽**。`follows_the_refresh`（`:299-301`）把「记忆块从第 m 次调用起不同、差异出现在 m 之后」全部列出不算失败。理由成立（§3.4：启动是刷新时刻，运行中的进程保留旧副本到下次压缩），但被豁免的不只是模型对新块的回答——m 之后的 call kind 差异（比如 snooze 水位丢失导致多或少一次压缩）同样会被豁免。实际结果：tidy 12/24、soft 10/24 的 drop 有块差异，最终豁免了 1 例；我核对那一例（drop before #1683，`Followup` 的 `remove old` 是 `fact-u332;` vs `fact-u307;`，两侧 call kind 序列相同）确是模型对新块的选择。**建议**：把豁免收窄到「kind 序列相同，只有 tool_calls 参数或 content 不同」，让进程决策（kind）永远受检。十几行。
- **「drop 时 flush 已排队」的覆盖薄**。24 个 drop 里这一状态出现 0 次（tidy）和 1 次（soft），长跑对 `Event::Resumed { over: true }` 基本没测到；靠定向测试 `a_flush_queued_when_the_process_went_down_runs_after_the_restart`（`bot_flush.rs:1288`：重启后第一条 prompt 是 flush、marker 无 `flush_skipped`）钉住。够，但这个数长跑报告已经打印，读的人要看。

### 4.3 两个发现的处置

- **丢 flush / 低估用量 → meter 播种（`db11a05`）**。`LoadedLog.measured` 只认最后一个 marker 之后的 usage（`src/session/loader.rs:295-296`、`:317-319`；测试 `the_measurement_is_the_last_answer_since_the_last_compaction`，`:374`），`seed_resumed` 以它为 settled、其后消息本地估算（`src/repl/context/meter.rs:276-292`；测试 `a_resumed_budget_settles_on_the_last_measurement`，`:591`），没有测量时本地计数 + overhead（记忆块 + 工具定义，`src/repl/run.rs:1005-1013`、`src/repl/context/tokens.rs:135`）；`Event::Resumed { over }` 在 Idle 时重新排 flush（`src/repl/bot.rs:208-211`）。合理，且是「重启 = 不重启」这条设计原则的直接推论。反向测试 `without_the_measurement_the_restart_would_not_compact`（`bot_flush.rs:1260`：剥掉日志里的 usage 就不压缩）证明起作用的是播种那一半，不是巧合。仍只在内存里的 snooze 水位、失败计数，§4.1 写明，32k 未观察到差异——接受。
  一处边界：`seed_resumed` 用 `m.usage() == Some(u)` 找位置；测量记录在最后一个 marker 之后，必在保留部分，找不到时退回 `reseed`。可以。
- **8k 不相容 → `BOT_MIN_WINDOW = 32k`（`db11a05`）**。`src/repl/context/tokens.rs:56`；`check_bot_window` 在取锁、建任何东西之前（`src/cmd/interactive/mod.rs:477`）；测试 `a_bot_needs_a_32k_window`（`bot.rs:273`，含 31,999 / 32,000 / 0 三个边界）。合理：根因是 8 KiB 记忆上限与 32k reserve 都是 flat 的，8k 复现证明它们把阈值吃光；与其让两个常量随窗口缩放（引入新的比例参数，而 §6 #13 的数值还没定稿），不如给窗口设下限。文案带了当前值与配置键。两点提醒：(a) 32k 是「能跑」的下限，不是推荐值——32k 场景最宽的调用 23k / 32k，真实模型一次 20k token 的工具结果就超窗，按 §4.1 超窗规则由人处理；§4.1 说了前提，可以再直白一句。(b) 运行中 `/model` 切到更小窗口不在校验内，已写明。

---

## 5. 其余修复简评

- **R8（flush 轮不广告历史 mount 的工具）**：`TurnCtx::send_history` 只改发送副本（`src/repl/turn/mod.rs:118-150`），历史与日志不动；测试 `the_flush_turn_does_not_advertise_a_mounted_tool` 断言普通轮 mount 完整、flush 轮只剩 `remember`、空掉的 mount 被丢不发。对。
- **M2（中断但保留的轮算 landed）**：`landed = saved && !flush`（`src/repl/run.rs:885`）；`update_kept` 用该轮最后一条带 usage 的消息结算、之后的合成结果本地估算（`meter.rs:242-259`；测试 `a_kept_turn_keeps_its_measured_usage`，`:697`）——否则本地计数几百 token 永远过不了阈值；`an_interrupted_turn_that_was_kept_still_queues_the_flush` 把参数从 120k 字符改回 `"x"` 后仍过，就是证据。对。
- **M5（无锁文件系统降级）**：`HeldLock(Result<File, PathBuf>)`；`cannot_lock` 认 `ErrorKind::Unsupported` 与 ENOTSUP / EOPNOTSUPP（macOS 45 / 102，Linux 95）（`src/session/lock.rs:82-98`、`:121-125`）；caution 从 writer 冒到三个入口。`delete` 在这种 FS 上无锁删除——与「单写者只能靠人」一致。测试用线程局部开关注入 ENOTSUP，两个入口各一条。对。
- **验收材料（`aa46e17`）**：flush 失败 / 中断、手动 `/compact`、`Unchanged` 退避、`Only` 拒绝名单外工具、`Wiring → SessionCtx` 逐字段——我第一轮 §3.2 列的六条都有了。`scripts/bot-retention.sh` 写了没跑（脚本头自己声明），§6 #13 仍是临时值——这是 L1 验收项，不阻塞合并，但「L1 完成」要等它跑过。`cargo doc` 进了门。
- **文档**：设计随修复同步得很实（§2.2 清空、§2.7 失败关闭与同进程修复、§4.1 两行新规则、§5.2 长跑改 32k 并写明 6b 的豁免规则、§7.1 删同进程孤儿）。用户文档仍只有 starter 里的注释示例；CHANGELOG 没有 bot 条目——合并前该补一条 Unreleased。

---

## 6. 能不能合并

**能。必须先修：无。**

建议随合并或紧接着做（都小，按值得的顺序）：

1. N4：`system_cleared` 用显式构造，不认形状。
2. R1 洞 1：`FLUSH_NOTICE` 加半句不要存密钥。
3. N1：`book_call` 挪到 marker 落盘之后。
4. N5：resume notice 措辞 `last activity`；R1 洞 2：§3.7 补「抹掉」的做法。
5. CHANGELOG 的 Unreleased 条目。

推送后看 Windows 腿：N3（`set_len` 对 append 句柄）与第一轮 M6（`delete` 持锁 `remove_dir_all`），对应测试会直接暴露。

L1 收尾（不阻塞）：跑 `scripts/bot-retention.sh` 定稿 §6 #13；6b 的豁免收窄。
