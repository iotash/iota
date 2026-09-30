# bot 模式 v1 实现评审

Status: **Review**（独立评审，2026-09-30）· 对象：分支 `bot-mode-v1` 相对 `main`（`5317eb4`）的 13 个提交，HEAD `345fde7` · 依据：`docs/design/bot-mode.md`（rev 2，下称设计）、`bot-mode-critique.md`（下称评审 C）、`bot-mode-recon.md`。

坐标约定：`src/...:行号` 指 `345fde7` 的源码。标「推测」的条目只有代码推理链，没有运行复现。本评审**不改任何代码或测试**。

本地核对：`cargo clippy --all-targets -- -D warnings` 通过；`cargo test` 17 个套件全绿（lib 1090、repl 131、session 85、cmd 126……，2 个 tmux 场景按环境跳过）。分支没有推到远端，`windows-latest` 这一腿没有跑过，涉及 Windows 的判断都标了「推测」。

---

## 0. 总体判断

**没有严重发现。** 评审 C 的三个严重项（S1 注入通道、S2 时序三洞、S3 摘要与记忆互不可见）在实现里都有对应的代码和测试钉住（见 §2 表）。锁、指针、配对修复、写侧截断这四件 L0 的事做得比设计还稠密（`terminate_last_line`、`HeldLock` 显式 unlock、`materialized` 的补正都是设计没写、实现补上的）。状态机 `repl::bot` 守住了「朴素数据进出」这条硬约束。

**最值得先修的三件事**（都是「重要」，没有一件到「严重」）：

1. **密钥正则按子串匹配，`disk-usage` / `task-based` / `risk-adjusted` 都被当成密钥拒写**（R1）。测试用 `"risk- assessment"`（多了一个空格）绕开了这个误报，等于把 bug 写进了断言。
2. **同进程内被中断的轮留下孤儿 `tool_use`，下一条消息就吃 Anthropic 400，直到重启**（R2）。`repair_tail` 已经写好，却只在 resume 时调用；在审批门上按 Ctrl+C 就能复现，这是 bot 最常见的交互之一。设计 §7.1 把它列为已知限制，但修法现在只有几行。
3. **设计 §5.2 承诺的两项 L1 验证都没有落地**：长跑不变量测试（`GrowingProvider`，"L1 收尾，可行"）和保留率脚本 `scripts/bot-retention.sh`（"L1 验收项"）。现有测试覆盖单次 flush→压缩周期很好，但 `Flushed` 相持续多轮、`Unchanged` 反复退避、重启中途接续这些跨周期行为只有状态机单测，没有端到端。

---

## 1. 正确性

### 1.1 重要

#### R1 密钥正则误报：`sk-` 无词边界

- **结论**：`secret_patterns()` 的 `sk-[A-Za-z0-9]` 没有词边界，任何含 `?sk-x` 的英文都命中。
- **依据**：`src/agents/memory.rs:353-359`（正则表），`:362-364`（`looks_like_secret`），`:403-406`（`make_line` 先查密钥再看内容）。`printf 'disk-usage\ntask-based\nrisk-adjusted\n' | grep -E 'sk-[A-Za-z0-9]'` 三行全中。测试 `src/agents/memory/tests.rs:403-417` 的 near-miss 样例是 `"risk- assessment"`，中间的空格是绕开误报的唯一原因。
- **失败场景**：模型调 `remember(text: "the disk-usage script lives in ~/bin", source: inferred)`，得到 `refusing to store what looks like a secret`。模型可能改写重试，也可能放弃；无论哪种，flush 轮里这类事实丢掉，而 transcript 上的理由是错的。
- **建议**：`\bsk-[A-Za-z0-9]{8,}`（或至少 `(^|[^A-Za-z])sk-`），并把 `disk-usage` 这类样例加进 near-miss 列表。设计 §3.7 的表就写的是 `sk-[A-Za-z0-9]`，所以这是设计与实现共同的疏漏，不是实现偏离。

#### R2 中断留下的孤儿 `tool_use` 只在 resume 时修，同进程内继续发消息会被 API 拒

- **结论**：`repair_tail` 只在 `SessionStore::resume` 调用；一轮被中断后，视图末尾可能是带 `tool_calls`、缺结果的 assistant，`interrupt_turn` 照原样落盘并留在内存里，下一条消息（或紧随其后的记忆写入 notice）发出去就是 Anthropic 400，不重试，整轮回滚。重启才会好。
- **依据**：assistant 带 `tool_calls` 先入 history（`src/repl/turn/tools.rs:135`），随后四处 `return Err(interrupted())` 都在结果补齐之前（`:190-193` 并行批后、`:205-207` surface 调用后、`:227` 审批门被 Ctrl+C、`:278-280` 每个顺序结果后）；`finalize_interrupt` 对「history 长于 watermark+1」的情况原样 `persist: true`（`src/repl/turn/interrupt.rs:55-72`）；`interrupt_turn` 不调 `repair_tail`（`src/repl/run.rs:1168-1207`）；修复只在 `store.rs:337-341`。之后 `record_memory_writes` 还会把一条 user-role notice 追加到孤儿后面（`run.rs:915-917`）。设计 §7.1 自己列了这条：「同进程内被中断的轮可能当场留下孤儿」。
- **失败场景**：bot 请求执行一条 shell 命令，审批门弹出，用户按 Ctrl+C。history 末尾是 `assistant(tool_calls)`。用户接着说「算了，换个思路」→ 400 → 红块 → 回滚；再说什么都是 400；只有 Ctrl+D 重启，`repair_tail` 才把它修好。对普通 chat 这是老问题（D-43 那一族），但 bot 的前提是「随时接着聊，不需要 resume」，这里恰好需要 resume。
- **建议**：`interrupt_turn` 里在 `persist` 分支调用 `repair_tail(&mut repl.conv.history)`，再 `persist_turn()`，并打同一条 `Recovered N tool call(s)…` notice。约 5 行，与 reload 路径的视图完全一致，不影响 L2 的「失败轮保留」。

### 1.2 次要

#### M1 压缩连续失败期间不再排新的 flush，最终成功那次的「saved N lines」是陈旧的

- **结论**：进入 `Phase::Flushed` 后，普通轮结束一律不排 flush（`src/repl/bot.rs:183`），压缩一直失败时这个相可以持续几十轮；最终成功时 `report()` 仍报当初那次 flush 的 `writes`（`bot.rs:226-238`），摘要 addendum 说「The memory flush just before this compaction saved N lines」，而那次 flush 早已不是「just before」。
- **依据**：`bot.rs:170-184`、`:226-238`；`compact.rs:77-87`、`:319-325`。设计 §4.1「已跑过的 flush 不重跑」是有意为之，所以这是设计的边界而非实现偷懒；只是 addendum 措辞在长失败链后不准。
- **失败场景**：摘要模型宕机一小时，bot 照常干活 30 轮；恢复后压缩成功，摘要被告知「flush 刚保存了 2 行、记忆里已有的别重复」，而这 30 轮的新事实没经过任何 flush。
- **建议**：`Flushed` 相持续超过若干轮（或用量再涨 5%）后允许再排一次 flush；至少让 addendum 在 `failures > 0` 时不说「just before」。不改也能接受，写进 §7.1。

#### M2 被中断但已落盘的轮不算 `landed`，越过阈值也不排 flush

- **结论**：`landed` 只在 `Ok(out)` 分支置真（`src/repl/run.rs:826`、`:912`），中断走 `interrupt_turn` 后 `landed=false`，状态机 `Phase::Idle if landed && over` 不排 flush（`bot.rs:179`）。下一条消息走 `BeforeSend{over:true}` → 直接压缩，`flush_skipped: true`，打 `⚠ Compacted without a memory flush`。
- **依据**：同上；设计 §3.6.1 写的是「第 N 轮成功结束」，实现忠实于字面。
- **失败场景**：一个 20 分钟的 `auto_run` 迁移轮读了大量输出，用户 Ctrl+C 打断（设计 I4 说这是最常见的中断），部分历史已落盘且越过阈值；用户下一句话触发无 flush 的压缩，这一轮学到的东西只剩摘要。
- **建议**：`interrupt_turn` 走 `persist` 分支时把 `landed` 视为真（把 `InterruptDecision.persist` 传回主循环即可）。判断题，不是 bug。

#### M3 以「最后一条非 notice 的 user 消息」为锚，无人值守的 notice 轮可以让保留尾部无限增长（推测）

- **结论**：X-63 把锚点改成 `role == User && !is_notice()`（`src/repl/commands/compact.rs:102-107`），所有模式生效。后台 job 完成的 notice 各自开一轮，用户不说话时这些轮全部落在保留尾部里，压缩永远压不到它们。
- **依据**：`compact.rs:102-107`、`:153-157`（中段为空 → `Unchanged`）；`bot.rs:205-209`（`Unchanged` → 退避 5%）。
- **失败场景**：夜里 30 个 `background: true` 的 job 陆续完成，每个 notice 轮模型都读一段日志；尾部超过阈值后每次 `BeforeSend` 都 `Unchanged`，用量涨到窗口上限，之后的 notice 轮全部 400 回滚，直到用户回来说一句话（它成为新锚点）才恢复。没有数据损坏，但无人值守期间停摆。
- **建议**：保留尾部单独超过半个窗口时退回 Go 的锚点（任意 user-role 消息），或对连续 notice 轮设上限。标推测：需要真实 job 密度才知道多久会撞到。

#### M4 「Resumed after … (last message …)」用的是 `meta.updated_at`，它被每次 meta 写入刷新

- **结论**：`resume_notices` 的时间来自 `Session::last_written = meta.updated_at`（`src/cmd/interactive/bot.rs:72-84`），而 `SessionMeta::write` 每次都刷 `updated_at`（`src/session/meta.rs:133-134`）；bot 每次启动都 `stamp_bundle`（`run.rs:421`、`:589-592`），`open_bot_session` 也写一次 meta（`bot.rs:155-160`），启动时的 `record_notices` 再追加记录。
- **失败场景**：用户 09:00 打开 bot 看一眼没说话就关掉，20:00 再开：notice 说「Resumed after 11 hours (last message 09:00)」，而最后一条消息是三天前。天天开一次不说话，日志每天多一条 notice 记录。
- **建议**：措辞改成 `last activity`；或在 `record_notices` 之前读 `last_written`（已经是了），但 `stamp_bundle` 只在值真的变化时写 meta。等 L2 的 `SessionRecord.at` 之后可以精确到「最后一条消息」。

#### M5 不支持 `flock` 的文件系统上 resume 直接失败（推测）

- **结论**：`try_lock_file` 把 `TryLockError::Error(e)` 原样上抛（`src/session/lock.rs:61-72`），`resume` 在读 meta 之前就返回 `Io`。以前能用的 NFS/SMB 上的会话目录，升级后一条也打不开。
- **建议**：`ENOTSUP`/`EOPNOTSUPP` 时降级为「无锁 + 一条 caution」，其余错误照旧。设计 §7.1 只提了同步盘，没提这一类。

#### M6 Windows 上 `delete` 持有 `.lock` 句柄期间 `remove_dir_all`（推测）

- **结论**：`SessionStore::delete` 先 `lock_bundle` 再 `remove_dir_all(dir)`（`src/session/store.rs:387-395`），删除时锁文件仍打开。Rust std 用 `FILE_SHARE_DELETE` 打开、`remove_dir_all` 用 POSIX 删除语义，Win10+ 的 NTFS 上应该能过，但分支没推送，`windows-latest` 这一腿没跑过，`tests/session/store.rs::delete_is_refused_while_the_bundle_is_held` 是唯一会暴露它的测试。
- **建议**：推送后看 Windows CI；若红，删除前 drop 锁（拿到锁只是为了确认没人持有）。

#### M7 `fit_line` 拒写会变成每轮重复写入的永久失败（推测，几乎不可达）

- **结论**：`fit_line` 对「工具参数本身超 32 MiB」返回 `Err`（`src/session/writer.rs:315-341`），`append_messages` 在批中途失败（`:194-208`），`persist_turn` 不推进水位（`run.rs:228-242`）；下一轮重放整个 backlog，前面的行再写一遍，再在同一条上失败。
- **建议**：`fit_line` 的兜底改成「把这条记录替换为一条带标记的空内容记录」而不是 `Err`；或至少让 `append_messages` 在中途失败时推进水位到已写的那一行。现实中要模型吐 32 MiB 参数才会触发，记着就行。

### 1.3 核对过、没有问题的路径

- **锁**：两把锁职责清楚；`HeldLock` 的显式 `unlock` 处理了 fork 窗口（`lock.rs:21-39`）；`open_bot` 先拿 bot 锁再读指针（`store.rs:425-426`）；`delete` 先查归属再锁（`:391-393`）；`/session` Delete 对 `Locked` 跳过不算失败（`commands/session.rs:148-150`）。
- **指针与本体**：`materialized` 在首写后置真（`writer.rs:277-280`）、resume 时补正陈旧的 false（`store.rs:436-442`）、`NotFound` 按 `materialized` 分流（`:446-455`）、损坏的指针是错误不是「没有指针」（`bot.rs:41-53`）。
- **配对修复与截断**：`repair_tail` 只看尾部、按调用顺序合成、追加落盘、`last_written` 取修复前的值（`store.rs:337-349`）；`terminate_last_line` 让撕裂行不吞掉修复记录（`:495-510`）；`fit_line` 先丢 `raw` 再切 `content`/`reasoning`（`writer.rs:315-341`）。
- **压缩与记忆**：flush 轮只见 `remember`（`run.rs:932-936`、`dispatch.rs:271-329`）、不 drain（`steer.rs:50-52`）、不发 Done（`run.rs:906-911`）、失败不进 Error（`:850-856`）；摘要看到当前文件（`compact.rs:321`）和 flush 行数（`:77-87`）；`compacted_through` 按对话消息数（`writer.rs:236`）；overlay 在 offer 之后读，压缩后的刷新只花一次 cache miss（`run.rs:795-798`）。
- **并发 `remember`**：`Tool::supports_parallel` 默认 false（`tool/mod.rs:144-146`），`Remember` 没有覆盖，同一 round 的多个 `remember` 顺序执行，读-改-写没有竞争。
- **方言**：Anthropic 的连续 user-role 合并（`anthropic.rs:87-115`）让 assistant → notice → user 成为一条 user 消息，`tool_result` 块仍在文本块之前；Google 本来就按角色折叠；OpenAI 两个方言接受连续 user。
- **换日**：`roll_day` 在每次发送前比较（`run.rs:1074-1091`），同日零成本，跨日一次 miss 合并记忆刷新。

---

## 2. 与设计的一致性

### 2.1 评审 C 的三个严重发现是否被实现解决

| 评审 C | 设计的对策 | 实现 | 钉住它的测试 |
|---|---|---|---|
| S1 注入通道 | Expanded 展示、写入 notice、`.prev`、来源标记、人写行不可改、密钥拒写、前言口径 | 全部在：`tool/builtins/memory.rs:91-94`（Expanded）、`run.rs:247-269`（notice 进 history 与日志）、`agents/memory.rs:654-661`（`.prev`）、`:423-463`（`find_one` 拒改无标记行）、`:353-364`（密钥表，见 R1）、`snapshot.rs:17-25`（前言） | `a_memory_write_is_recorded_once_after_its_turn`、`old_must_name_exactly_one_tagged_line`、`secrets_are_refused`、`the_disk_write_is_lazy_backed_up_and_announced`、`the_block_is_the_preamble_and_the_file` |
| S2 时序三洞 | 只有 notice 跳过检查；flush 轮只带 memory 集、不 drain；锚点跳过 notice；bot reserve | `run.rs:664-667`、`:791-794`（只有 flush 跳过 offer）；`turn_ctx(…, flush)`；`Steerer::new(…, open)`；`compact.rs:102-107`；`meter.rs:105-114` | `the_flush_turn_saves_memory_then_the_users_last_turn_survives_the_compaction`、`a_message_ahead_of_the_notice_compacts_without_a_flush_and_the_notice_is_dropped`、`a_flush_notice_taken_by_steering_is_put_back_not_injected`、`a_bot_keeps_the_users_last_turn_not_the_flush`、`a_bots_threshold_keeps_the_larger_reserve` |
| S3 两层互不可见 | summarize 附 MEMORY.md 与 flush 行数；标记三个键；`flush_skipped` 可见 | `compact.rs:283-292`、`:371-375`；`record.rs` 三个 optional 键 | `a_bots_summary_pass_sees_the_memory_and_the_flush_writes`、`nothing_saved_keeps_durable_facts_in_the_summary`、`two_failed_compactions_in_a_row_tell_the_host` |

结论：三项都解决了，且解决方式与设计 rev 2 的文字一致。S1 的密钥门有 R1 的误报，但门本身在。

### 2.2 实现比设计更对的地方

- **`materialized` 陈旧值的补正**（`store.rs:433-442`）：设计只说首写置真；实现在 resume 成功时顺手把 false 改成 true，覆盖了「bundle 已建、指针没改」的崩溃窗口。测试 `a_resume_fixes_a_stale_materialized_flag`。
- **`terminate_last_line`**（`store.rs:495-510`）：设计没提撕裂行；没有它，`repair_tail` 的追加会粘到半行上、下次加载被一起跳过。测试 `resume_terminates_a_torn_last_line_before_appending`。
- **`HeldLock` 的显式 `unlock`**（`lock.rs:21-39`）：设计说「drop 时释放」，实现发现 fork 出的子进程会共享文件描述，改为主动 unlock。
- **ToolsMount 两侧都修**（`writer.rs:189-194`、`loader.rs:297-306`）：设计要求「先复现」；实现复现了（`a_frozen_tools_mount_is_skipped_on_write_and_the_watermark_passes_it`），写侧跳过、读侧不让空 system 胜出，老日志也能加载。DIVERGENCES 相应改写。
- **被 steering 取走的 flush notice 放回队列**（`steer.rs:53-56`、`bot.rs:177`）：设计没考虑这条竞争；实现给了 `Requeue`。
- **空的 config system 不覆盖**（`cmd/interactive/bot.rs:189-196`）：设计没写；实现指出日志无法表达「无 system」，保留旧的。合理。
- **`Compacted::Unchanged` 明确退避**（`bot.rs:205-209`）：设计 M12 提了，实现给了独立分支和测试。

### 2.3 与设计不同、但可接受的地方

- **§2.2 `iota resume <普通会话 id>` 而 `agents.default` 是 bot**：设计说走普通 resume；实现 `bot = is_bot && !resume_given`（`interactive/mod.rs:459`），`enabled_toolsets` 同条件（`cmd/mod.rs:357-362`）。一致。
- **§3.6.1 「flush 轮不算最后一轮」改成全模式的锚点**：设计 rev 2 已同步（X-63）。M3 是它的代价。
- **§2.5 `meta.cwd` 记上一次运行的目录**：设计 rev 2 已同步。

### 2.4 设计承诺、代码没做的

| 设计条目 | 状态 | 级别 |
|---|---|---|
| §5.2 长跑实验：`testing::GrowingProvider`，8k 窗口驱动 2000 轮，中途 drop 再 resume，断言日志只增、视图 ≤ 窗口、每次压缩前恰好一个 flush 或 `flush_skipped`、重启后视图相同（"L1 收尾，可行"） | 不存在（`grep GrowingProvider` 无结果） | 重要（见 §3） |
| §5.2 保留率实验 `scripts/bot-retention.sh`（"L1 验收项，不是可选项"；§6 #13 的数值由它定稿） | 不存在（`scripts/` 只有 check-*.sh、size.sh） | 重要（验收项，不阻塞合并） |
| §4.2 M8「文档里给出一份 bot 推荐配置模板（sandbox + auto_run + auto_write）」；§2.2「`/model` 只对本次进程有效，文档写明」 | 除 `docs/DIVERGENCES.md` X-58 与设计文档外，没有任何用户文档提到 `mode: bot`；starter 配置只有 `# mode: agent`（`config_cmd.rs:53`） | 次要 |
| §7.1「同项目多 bot 至少 warning」 | 没有实现 | 次要 |
| §5.2 缓存实测（手动） | 手动项，未见记录 | 不评 |

没有发现「实现偷懒」型的偏离：v1 范围（L0 四项 + L1 八项）里每一项都能在代码里指到，且大多带测试。

---

## 3. 测试

### 3.1 质量

新增测试整体质量高：断言到字节（`the_block_is_the_preamble_and_the_file`、`a_bots_summary_pass_sees_the_memory_and_the_flush_writes` 把整段 prompt 钉死）、覆盖失败路径（`two_failed_compactions_in_a_row_tell_the_host` 数到第三次失败与 5% 退避）、有反向断言（`a_frozen_tools_mount_is_skipped_on_write…` 先断言 mount 确实进了 history，防止测试空转）。`mode_agent_is_what_workspace_true_was` 用旧二进制抓的 fixture 钉住「一个字节都没变」，是这类重构该有的样子。状态机 `repl::bot::tests` 七个用例覆盖了全部转移。

**假绿或偏弱的地方**：

- `secrets_are_refused` 的 near-miss 列表用 `"risk- assessment"` 绕开了 R1 的误报（`memory/tests.rs:404-409`）——这条断言在保护一个 bug。
- `a_resumed_bot_is_stamped_with_the_running_parameters`（`tests/repl/commands.rs`）对 bot 分支断言的是 `meta.effort == ""`，因为 fake 没有 tuning 能力；它证明「被覆盖了」，没证明「覆盖成了 config 的值」。`cmd/interactive/bot.rs::the_config_wins_over_the_session_meta` 补上了 model 那一半，effort/temperature/top_p 的回写没有一处断言到具体值。

### 3.2 该测而没测的路径

- **flush 轮失败或被中断**：设计 §4.1/§4.3 说「照常压缩、`flush_skipped: true`、不进 Error、无 ping」。状态机单测 `a_finished_flush_compacts` 覆盖了 `failed: true` 的转移，端到端一条都没有（`bot_flush.rs` 的 flush 轮都是成功的）。`run.rs:850-856` 那个 `if !flush` 没有测试保护。
- **bot 下手动 `/compact`**：设计 §4.3 说写 `flush_skipped: true` 但不打 `⚠` notice（`compact.rs:396-399`）。没有测试。
- **`Compacted::Unchanged` 端到端**：只有状态机单测。
- **R2 的场景**：中断后同进程继续发送。没有测试，也正是因为没有测试，§7.1 才把它写成限制。
- **`Only::call_tool` 对不在名单里的工具**：返回 `UnknownTool`（`dispatch.rs:301-313`），没有直接测试；`bot_flush.rs` 里 fake 从不越权调用。
- **`wire_session` 的 bot 分支到 `run()` 的接线**：`cmd/interactive/bot.rs` 的单测止于 `BotSession`，`bot_flush.rs` 手工构造 `SessionCtx`；`recorded_notices`/`memory`/`notices` 三个字段从 `Wiring` 到 `SessionCtx` 的搬运没有测试。风险低，但正是这种搬运容易漏一个字段。
- **§5.2 长跑不变量**（见 §2.4）。M1、M3 两个多周期问题只有这类测试能暴露。

### 3.3 建议

1. 补 `bot_flush.rs`：一个 flush 轮 400 的用例（断言 `flush_skipped`、无 `Failed` ping、host 不进 `Error`、随后的压缩仍跑）；一个手动 `/compact` 的用例。
2. 把 R1 的 `disk-usage` 样例放进 near-miss 列表（修了正则之后）。
3. 长跑测试按设计的形状写（`GrowingProvider`、8k 窗口、随机 drop 再 resume），几秒能跑完，放进 `cargo test`。

---

## 4. 分层与风格

- **分层**：`tests/layering.rs` 通过。新边全部向下：`tool/builtins/memory.rs → agents::memory`、`agents/memory.rs → sync/text/app`、`session/bot.rs → app::fs`、`repl/commands/compact.rs → session::CompactionStats`。`repl::bot` 的 `Event`/`Action` 只有 `u32`/`bool`/枚举，`held: Vec<Input>` 留在主循环里没有进状态机（`run.rs:1105-1110`），§6 #23 的硬约束成立。`HarnessInputs` 从 `cmd::assemble` 下移到 `agents::harness` 并带上 `clock`，是合理的位置。
- **clippy pedantic**：`--all-targets -D warnings` 干净。`let _ = self.0.unlock()`、`let _ = write_pid(..)` 都有注释说明为什么可以忽略。
- **错误处理**：`SessionError` 新增四个变体文案都有测试；`BotPointer::read` 把解析失败包成 `Io(InvalidData)` 并带路径；`remember` 的每条拒绝都是 model-facing 文本。一致。
- **过度设计**：没有发现为将来预留而没人用的抽象。`OnCreated`（boxed `FnMut`）、`Only`、`WriteLog`、`Seen.edited` 各自只有一个使用者，但每个都对应一个真实的竞争或时序问题。`SessionCtx` 新增四个字段（`bot`、`notices`、`recorded_notices`、`memory`）略显散，`notices` 与 `recorded_notices` 的区别靠文档注释撑着——可以接受，不建议现在动。
- **与既有代码的一致性**：`open_append_0644`/`open_lock_file` 的 unix/non-unix 双实现与既有 `write_0644` 同款；`bot_owner` 的目录扫描与 `list_bucket` 同风格；`SUMMARY_PREFIX`/`SUMMARY_SEPARATOR` 没动，Go 互通不受影响。

---

## 5. 会不会合并

**会，先修两处。**

必须先修（都小）：

1. **R1** 密钥正则加词边界，near-miss 样例改成会暴露误报的那种。
2. **R2** `interrupt_turn` 的 `persist` 分支调 `repair_tail` 再落盘。bot 的前提是「随时接着聊，不需要 resume」，而现在在审批门上按一次 Ctrl+C 就得重启。

合并后、宣布 L1 完成之前必须补的（设计自己的验收清单）：

3. §5.2 的长跑不变量测试；flush 轮失败/中断与手动 `/compact` 的端到端用例。
4. `scripts/bot-retention.sh` 与 §6 #13 数值的定稿；用户文档（`mode: bot`、推荐配置模板、`/model` 语义）。

可以记进 §7.1 或 backlog 的：M1、M2、M3、M4、M5、M6、M7。

推送后看一眼 Windows 那一腿（M6）。
