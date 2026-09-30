# bot 模式修复验收

日期：2026-10-01。验收提交：`051227c6a4377edfc43a89fe3f9fce0377d23705`，范围仅为 `db11a05..HEAD` 的这一个提交，对照 `docs/design/bot-mode-review-codex-2.md` 的合并门槛。环境：Darwin 25.6.0 arm64。下文源码行号均指该 HEAD。

**现在不批准合并：R1、N1、N2 及 R7 脚本已通过，但上一轮第 4 条门槛中的两条长跑断言仍有实测漏检，必须修正用户轮完成归属和 refresh 豁免范围。** 这是原验收项的剩余部分，不要求重开整轮评审或运行真模型实验。

| 验收项 | 结论 | 本轮直接证据 |
|---|---|---|
| R1 回滚失败 | **已修** | 同样的附件路径文件 + macOS `uappnd`；四种日志写入受阻；解除故障后同一 writer 重试得到 `c1_calls=1; c1_results=1` |
| N1 无锁降级 | **已修** | 重跑原 `without_locks` 注入：写式 resume、delete 拒绝，load 成功；旧降级实现、调用点及依赖测试已移除 |
| N2 meta 同值重试 | **已修** | 原 `meta.json.tmp` 目录阻塞；重试后磁盘标题为 `changed`；成功后同值调用不改文件和时间 |
| R7 保留率脚本 | **已修（脚本范围）** | `bash -n`、源码核对和无模型干跑均通过；窗口 32000，失败/无效结果退出非零，答案绑定问卷轮 |
| 长跑两条断言 | **部分修，仍阻断** | 主线磁盘检查及旧消息比较确实增强；但 flush 回复仍能冒充用户最终回复，summary 请求和无来源的 `memory:` notice 仍可被豁免 |

## 1. R1：原故障复现与恢复

使用当前重新构建的库的公开 `SessionWriter` API，在 `/private/tmp` 编译独立探针；没有改仓库代码或测试，也没有使用产品里的 `CUTS_TO_FAIL` 来替代本次 macOS 故障。

复现步骤与上一轮相同：先保存 `seed`，批次为 `user(q) → assistant(c1=read_file) → tool(c1)`，最后一条带四字节附件；将空 `attachments/` 移走并在原路径放普通文件；对 `messages.jsonl` 执行 `chflags uappnd`。第三条的附件保存失败，前两条已追加，截回被文件标志拒绝。

本次输出：

```text
first_error=writing the session log failed (Not a directory (os error 20)), and the part already written could not be cut back (Operation not permitted (os error 1)); nothing more is written to it until it is
records_after_failure=3; writer_count=1
```

随后只恢复附件目录，保持 `uappnd`，依次尝试同批重试、新消息、compaction marker、`clear_system()`；四次均返回 `SessionError::LogNotCutBack`，错误文本相同：

```text
the session log still holds part of a batch that failed, and it could not be cut back (Operation not permitted (os error 1)); nothing is written until it is
blocked_log_unchanged=true; writer_count=1
```

逐字节比较日志，确认受阻阶段没有新增记录。执行 `chflags nouappnd`，**不更换 writer**，重试原批次：

```text
after_identical_retry: log_records=4; writer_count=4; c1_calls=1; c1_results=1
tail_repairs=0; roles=[User, User, Assistant, Tool]
post_recovery_marker_clear_append_ok=true
```

最后一行表示恢复后又成功写入 marker、system 清除记录及新消息。旧的六条记录、两个 `c1` 调用而只有一个结果的中段孤儿没有再出现。

另跑不设置 `uappnd` 的正常回滚路径：

```text
first_error=Not a directory (os error 20)
records_after_failure=1; writer_count=1
after_identical_retry: log_records=4; writer_count=4; c1_calls=1; c1_results=1
tail_repairs=0; roles=[User, User, Assistant, Tool]
post_recovery_marker_clear_append_ok=true
```

源码对应：`src/session/writer.rs:385` 在截回失败时保存 `uncut`；`:368` 在下一次写入前重试截回，仅成功才清状态；消息、清 system、marker 分别经 `:229`、`:264`、`:307` 进入该门。`:407` 使用独立写句柄执行 `set_len + sync_all`，避免依赖 append 句柄的截断权限。首次错误同时报告写入及回滚失败，见 `src/session/error.rs:108`。

这里“挡住所有写入”准确指**所有会追加日志的入口**；空批次仍是 no-op，`update_meta` 没有被这个日志门禁冻结，但它不向日志追加、不会把残批顶成中段孤儿。

仓库回归 `session::writer::tests::a_failed_cut_holds_every_write_until_the_log_is_cut_back` 也通过。原正常路径相关的 `a_failed_batch_is_all_or_nothing_and_a_failed_meta_is_not_a_failed_batch`、`a_backlog_the_log_refused_is_saved_before_the_compaction_and_survives_a_restart`、`a_failed_meta_rewrite_does_not_double_the_turn` 均通过。

## 2. N1：无锁写入和删除已失败关闭

重跑上一轮使用的同一注入接口：`src/session/lock.rs:165` 的 `without_locks` 设置线程局部标志，使 `:87` 的 `try_lock` 返回 `ENOTSUP`。不是拿普通本地文件锁成功来代替“不支持 flock”的测试。

```text
test session::lock::tests::an_unsupported_lock_fails_closed ... ok
test session::lock::tests::a_resume_without_locks_is_refused ... ok
```

第一项直接断言 bundle 锁及 bot 锁返回 `LockUnsupported`，并核对完整错误文本（路径为测试临时目录）：

```text
file locking is not supported under <临时目录>; iota cannot open or delete a session there
```

第二项在同一注入下实测 `resume` 失败、`delete` 失败、原日志字节不变、只读 `load` 成功且仍加载一条消息（`src/session/lock.rs:220`、`:225`、`:230`、`:231`）。撤销注入后真实锁可重新获取；`EOPNOTSUPP` 也被分类为不支持，普通权限错误没有被误归类（`:196`）。

实现核对：`HeldLock` 现在直接持有 `File`，不再有空 guard；`:78` 明确返回 `LockUnsupported`。新建 session 的 writer 是延迟落盘对象，首次实写仍先获取 bundle 锁（`src/session/writer.rs:350`），不能绕过此检查。

清理核对：在 `src/`、`tests/` 搜索 `lock_cautions`、`opened without a lock`、`opens_unlocked`、`without_locks_still`、`with_a_caution`、`HeldLock(Err`，无旧降级实现或依赖测试残留。原 CLI 三处提示循环、bot 的“无锁也能 resume”测试也在本提交中删除。通用 `Streams::caution` 仍有其它有效调用，不能把它误算成降级死代码。历史评审文档描述旧行为不属于运行代码残留。

未使用真实 NFS/SMB；这里确认的是与上一轮一致的“不支持锁”错误注入结果，没有声称进行了远程文件系统并发实验。

## 3. N2：同值重试落盘，无变化优化保留

用独立公开 API 探针重跑原步骤：保存 `seed`，创建目录 `meta.json.tmp` 阻塞重写；将标题设为 `changed`；移除目录；在同一 writer 上再次设为 `changed`；不追加消息，直接读取 `SessionMeta` 并重载 session。

```text
N2 first_error=Is a directory (os error 21)
retry_ok=true; memory_title="changed"; disk_title="changed"
```

成功后保存 meta 原始字节、mtime 和 `updated_at`，等待 1.2 秒跨过秒级时间精度，再次用目录阻塞 `meta.json.tmp`，重复相同标题：

```text
unchanged_after_1.2s_with_tmp_blocked: ok=true; bytes_unchanged=true; mtime_unchanged=true; updated_at_before="2026-10-01T02:57:01+08:00"; updated_at_after="2026-10-01T02:57:01+08:00"
```

既没有尝试被阻塞的文件重写，也没有刷新时间。关闭 writer 后 `load` 的标题仍为 `changed`。

对应 `src/session/writer.rs:328` 的 `meta_dirty || before != self.meta` 以及 `:339` 根据实际写入结果维护 dirty；日志之后的 meta 写入也统一经过此处（`:416`）。仓库测试 `a_meta_change_that_failed_is_written_by_the_same_change_again` 同时通过。

## 4. R7：脚本静态检查和无模型干跑

静态核对：默认窗口和脚本门槛均为 32000（`scripts/bot-retention.sh:68`、`:78`），启动前拒绝过小窗口（`:95`），与 `check_bot_window` 及已通过的 `a_bot_needs_a_32k_window` 一致。压缩不足返回 3 并写 `INVALID`（`:327`、`:342`）；外层把无效或失败组汇总为退出 1（`:391`）。问卷从最后一个匹配的 user 消息开始，遇到下一条 user（包括 flush notice）停止（`:191`），不再取整个日志最后一个 assistant。

干跑直接执行**未经修改的仓库脚本**，通过临时 `PATH` 替身模拟 tmux 的输入、日志和空闲提示，`IOTA_BIN=/usr/bin/true`，无模型进程、无 API 请求；只把实验规模缩成 `LEAD=0, COMPACTIONS=1, COMPACT_EVERY=2, MAX_TURNS=2, TIMEOUT=2`。窗口不设置，以测试默认值。模拟的正常问卷后追加 flush 回复和 compaction，专门检查答案不会串轮。

```text
bash -n exit=0
default_window: exit=0
generated_window=context_window: 32000
quiz_answers='1: QX-\n2: bramblewood'
under_window: exit=2
bot-retention: RETENTION_WINDOW 16000 is under a bot's minimum of 32000
short: exit=1
flush  INVALID: reached 0 of 1 compactions
failed: exit=1
flush  FAILED
mixed: exit=1
noflush  1  0  0  2/10  0/10  2/20
flush  FAILED
noanswer: exit=1
flush  INVALID: no answer to the quiz in the log
contaminated: exit=1
noflush  INVALID: 1 flush notices in the noflush group
bad_group: exit=2
```

`short` 将最大填充轮数改为 1；`failed` 模拟组内没有回答；`mixed` 一组成功、一组失败；`noanswer` 只有后续 flush 回答，没有问卷回答。正常场景日志最后的 `FLUSH_REPLY_MUST_NOT_BE_GRADED` 未进入 `answers.txt`。上面的 `2/20` 完全来自模拟答案，只证明脚本流程，不是事实保留率结果。

真模型实验未跑；召回率及临时常量的定稿继续属于 L1 完成验收，不作为此次新增要求。

## 5. 长跑：实际通过，但两条断言仍不满足门槛

执行 `cargo test --test repl bot_longrun -- --nocapture`，退出 0：两个 32k 场景及新 refresh 单测通过，8k 反例保持 ignored，本轮没有重跑它。

| 场景 | 新断言打印的已保存完成轮 | markers | 超窗拒绝 | 最大 input+output | 重载视图 | refresh 豁免 |
|---|---:|---:|---:|---:|---:|---:|
| 32k，短记忆 | 2000/2000，重复 0 | 60 | 0 | 17297 | 24/24 相同 | 0 |
| 32k，记忆停在软阈值附近 | 2000/2000，重复 0 | 80 | 0 | 23004 | 24/24 相同 | 1 |

两组分别约 24.71 秒、26.23 秒。软阈值场景仍是 #1683 的 `remember remove` 参数 `fact-u307` / `fact-u332` 差异。正常运行没有被本轮发现为漏答；下面实测的是**新判定函数对坏输入的漏检**，不是声称这两次长跑真的丢了用户回复。

### 必须修 A：flush 回复仍会被计为用户轮完成

增强确实存在：`tests/repl/bot_longrun.rs:727` 从主线最终磁盘记录计算完成集合，不再拿含参考分支的 provider 调用数作完成证据；两组正常场景均要求 invariant 0（`:1097`、`:1117`）。

但 `saved_turns` 在 `:682` 跳过 `notice=true` 的 user 记录，却没有结束前一用户轮；`:688` 随后把任意无 tool calls、非 interrupted 的 assistant 都算给仍保存的 `current`。因此用户轮没有最终回复、只有随后 flush 成功，仍然计完成。

我从 HEAD **原样提取** `saved_turns` 到临时 Rust 探针，输入每轮如下序列，轮号遍历 `1..=2000`：

```text
user #n → assistant(tool call) → tool(result)
→ user(notice=true, flush) → assistant("Saved the memory.")
```

没有任何用户最终回复。按仓库 `:729`、`:733` 的原判定计算：

```text
no_user_final_answers: asked=2000; counted_answered=2000; invariant0_accepts=true
```

**修复要求：** 把最终回复绑定到对应普通用户轮，在 flush 开始时结束该轮的归属；使用 fake 的轮号/回复类型进一步核对亦可。增加“普通轮无最终回复、仅 flush 有回复”必须失败的负例。仍应保持主线轮号集合恰为 `1..=2000`、每轮只出现一次。

### 必须修 B：refresh 豁免还包含未经证明的进程差异

增强也确实存在：`refresh_explains` 比较过滤后的旧历史和调用类型，新增测试 `only_the_models_answer_to_the_reread_block_is_excused` 已通过；它能拒绝测试中的旧普通消息删除及调用类型变化，优于原来仅按时间位置豁免。

但它把**所有 Summary 请求的历史直接替换为空向量**（`tests/repl/bot_longrun.rs:373`），只剩 kind 比较。前一调用历史相等，不能证明“渲染/选取给 summary 的历史”也相等。此外 `:360` 会抹掉任何此前未出现、以 `memory: ` 开头的 notice，未核对它是否来自模型的新工具调用。

从 HEAD 原样提取 `view_of`、`up_to_turn`、`call_ids`、`compare`、`memory_differs_at`、`refresh_explains`，使用当前库的 `GrowingCall` / `Message` 类型构造负例：

- 两侧先有相同旧历史，仅 system 里的 memory block 不同；下一次同为 Summary，一侧请求有旧问答，另一侧请求改成 `CORRUPTED: all old history lost`；再给相同的后续普通轮历史。
- 另一例不产生新工具调用，只在一侧的后续历史插入 `memory: invented unrelated process notice`。
- 对照例删除普通历史消息，应当被拒绝。

输出：

```text
summary_request_corrupted: compare_detects_diff=true; memory_at=Some(0); refresh_explains=Ok(())
unrelated_memory_notice_without_tool_write: refresh_explains=Ok(())
ordinary_history_cut_rejected=true
```

第一例会走真实的豁免条件：已有 diff、已有 memory_at，`refresh_explains` 返回成功，`:310` 即认定可豁免。这里不是模型回答不同，summary 输入的进程差异本身被擦掉了。负例是判定器级实测；产品真的发生此类 summary 截断仍是**推测**，本轮没有复现产品截断。

**修复要求：** 对 summary 请求保留旧历史内容/顺序的校验，或增加固定模型回复的严格续跑对照；`memory:` notice 的豁免应能对应实际的新工具写入。补上这两类坏输入必须失败的负例，不能只验证旧普通消息删除。

## 6. 新问题与回滚状态的恢复边界

本次变更范围内，未发现新的运行时合并阻断；仍阻断的是上一节的两条验收断言。特别是 `uncut` 不是不可解除的 poison：每次实写都同步重试截回，失败立即返回，成功清状态；本次四种入口连续失败后，同 writer 恢复的实测已经覆盖，不存在本次故障条件下的自锁或永久卡死。永久 I/O 故障会继续拒绝写入，这是所需失败关闭。

也跑了关闭 writer 后恢复的变体：仍有三条残留记录时解除文件标志、drop writer，再 resume，输出 `reopen_tail_repairs=1; reopen_append_ok=true; records=6`。新进程没有持久化的 `uncut`，走的是已有尾部孤儿补结果逻辑（`src/session/store.rs:350`），不是撤销旧批次后自动重放；不能把它描述成跨进程事务回滚。

这条变体还观察到既有计数边界：resume 补一条结果后磁盘四条记录，`message_count=2`。原因是 `SessionWriter::resumed` 仍直接沿用旧 meta 的计数（`src/session/writer.rs:124`），未据日志重算；该逻辑在 `db11a05` 已相同，不是本修复新增，也不是此次同 writer 重试重新出现中段孤儿。建议合并后跟进崩溃/未确认批次之后的 meta 计数校准；本轮不把跨进程事务恢复扩成新的合并门槛。

其它有改动的相邻路径已核对：显式 `clear_system`、resume 的 `MetaNotSaved` 处理、compaction marker 成功后再记 usage、bot Context 下限及文案；相应 session、bot flush、bot 启动和窗口测试通过。独立写句柄针对 Windows 的权限修复本次只作源码核对，未在 Windows 实跑；未注入真实 `sync_all` 硬件故障，不把这些未跑条件宣称为已验证。

## 验证记录与交付

```text
cargo test --lib session:: -- --nocapture                         35 passed
cargo test --test session -- --nocapture                         88 passed
cargo test --test repl bot_flush -- --nocapture                  19 passed
cargo test --test repl bot_longrun -- --nocapture                 3 passed, 1 ignored
cargo test --lib cmd::interactive::bot:: -- --nocapture           19 passed
cargo test --lib a_bot_is_not_offered_a_window_below_its_minimum -- --nocapture
                                                                 1 passed
bash -n scripts/bot-retention.sh                                 exit 0
```

临时探针及原始输出留在本机 `/private/tmp/bot-verify-20261001/`：`probe.rs` / `probe.log` 为 R1、N2；`assertions.rs` / `assertions.log` 为原样提取函数的负例；`dry.py`、`shims/`、`dry.log` 为脚本干跑。仓库测试原始输出为 `/private/tmp/bot-verify-{session-lib,session,flush,longrun}.log`。它们是复现工作文件，不是额外交付文件。

仓库仅新增本报告，未修改代码、测试或脚本，未 commit。**剩余必须修仅两项：A 用户完成断言不能借用 flush 回复；B refresh 豁免不能无条件放过 summary 输入和无来源的 memory notice。**
