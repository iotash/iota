# bot-mode-v1：简化后的最终复核

日期：2026-10-01。范围：`git diff 2b07294..HEAD`，HEAD 为 `38f6539d0046c0ff1cd5b57be3f57a4f70ad8334`，包含 `5f97c77`、`11082d0`、`a42c026`、`38f6539` 四个提交。依据：[上一轮仲裁](bot-mode-overdesign-codex.md) §2(a)–(d)。以下文件坐标均指本次 HEAD；历史实测另行标明。

**总判断：批准合并。** 上轮为删除懒物化和缩减长跑设置的必要条件均满足；没有发现这四批改动新增的合并阻断。覆盖确实缩减了：24 个随机重启点的发送差分网变成两个固定回复的代表性对照，不能称为等价替代。启动即留下空会话、锁报错改措辞、长标题放宽，都已在现行设计中如实体现。保留率试运行仍未完成，批准代码合并不等于完成真实模型的 L1 事实保留验收。

本轮只新增本文件，没有修改代码、测试、脚本或其它文档，没有 commit。先逐项读 diff、调用路径和判定逻辑，再运行现有测试；没有为取得绿色而修改测试。未运行真实模型、Windows 或完整 `ci.sh`。静态结论与实测分列，未复现实验不冒充本轮实测。

## 1. (a) 机械七项

按本次任务列出的七项核对；上轮另列的文档归档在本节末补核。

| 项目 | 判断 | 依据与行为核对 | 本轮验证方式 |
|---|---|---|---|
| `BotPointer.v` / `BOT_POINTER_VERSION` | **满足** | `src/session/bot.rs:15` 只剩必需的 `session`；`:31` 仍将坏 JSON / 缺字段作为错误。`pointer_round_trips` 断言完整 JSON 为 `{"session":"01KABC"}\n`，并保留覆盖写入、读回、临时文件消失的断言；`a_corrupt_pointer_is_an_error` 还新增了合法 JSON 缺 `session` 的负例。 | diff + `rg` 检查声明/导出/使用点；上述测试随 lib 实跑通过。没有把格式测试改成仅检查“文件存在”。 |
| `BotRunning` → `Locked` | **满足** | `src/session/lock.rs:57` 仍走同一个 `try_lock_file`，只改成 `Locked { what: "bot <name>", pid }`；`src/session/error.rs:31` 保留 pid 后缀。`bot_lock_names_the_bot` 仍验证第二次获取被拒、pid、完整措辞及 drop 后重入；`a_running_bot_is_locked` 仍验证启动层拒绝。 | 静态比较锁路径；lib 中锁测试、Display 测试、启动层测试均通过。 |
| `middle_tokens` / `summary_tokens` | **满足** | 计算、`Compaction::Done` 传递、`CompactionStats`、序列化链已全部删除。`src/session/record.rs:67` 保留 `compacted_through` / `usage` / `flush_skipped`；`src/repl/commands/compact.rs:364` 仍把摘要账单与 flush 状态交给 writer。删去的测试仅钉尺寸计数；`tests/repl/bot_flush.rs:329` 仍钉压缩边界和未跳过 flush。 | 全仓源码/测试/脚本无旧符号；lib、REPL、session 实跑。`session_usage_round_trip`、`compaction_marker_bumps_no_counter`、正常 flush / skipped / 手动压缩测试均通过。 |
| `MEMORY_SECTION_CAP` | **满足** | `src/agents/memory.rs:89` 仍先拒绝换行/控制字符，再检查三种小节形态和非空项目名；`:545` 的 8 KiB 正文增长限制未改。`a_section_is_one_heading_line`（`src/agents/memory/tests.rs:175`）保留五个结构攻击输入，只删 100 B 长度断言。 | diff + lib 实跑：该结构测试、`section_names_are_the_three_conventions`、`past_the_hard_cap_only_shrinking_edits_go_through` 均通过。超过 100 B 的合法标题现在可接受是静态路径结论；当前没有新增该正例测试，列为可选补钉。 |
| `AgentMode::Bot` 过期注释 | **满足** | `src/config/agent.rs:78` 删除 “Before T4 none of that exists”，仍说明 bot 包含 agent 能力、session 采用 flat 布局；枚举和能力判断未变。 | 对比 diff；lib 配置测试、cmd 的模式请求回归通过。 |
| fixture 改名 | **满足** | `tests/cmd/session.rs:713` 的注释、`mode_agent_requests_are_pinned`、读取路径一起改名。fresh/resume 的完整请求比较，以及 chat/缺键、bot/agent 的比较均未削弱。 | 两个路径的 Git blob 都是 `57bc613a879907772311203371fb39f505565e31`，内容逐字未变；cmd 测试通过。`config::strict::tests::workspace_is_an_unknown_agent_key` 也通过。 |
| `append_compaction` 合并 | **满足** | `src/session/writer.rs:270` 只留一个入口，显式接收 `flush_skipped: bool`。`:281` 仍按完整 conversation count 减保留尾部，`:289` 仍走失败批截回，成功后才累计 usage、写 meta。旧调用者加 `false`；产品调用传真实 flush 状态，未改成默认跳过或默认不跳过。 | `rg` 无 `_with` / `CompactionStats`；所有调用点对比；session 的 counter、usage、负边界、loader、roundtrip、golden 测试及 REPL 的压缩/导出测试实跑通过。 |

**可见变化是否如实记录：满足。** 旧句子 `bot coder is already running (pid …)` 变为 `bot coder is open in another iota process (pid …)`，现行设计 `docs/design/bot-mode.md:202` 与测试采用新句子。`:510` 明写小节名仍有结构检查，长度只受整份 8 KiB 限制；因此合法长标题可能消耗更多记忆额度，也不再收到原 100 B 报错。`:496` 记录两个统计键被删除。不能把这一批概括为“完全没有行为变化”；它也额外收紧了外来记忆的读取，见 §5。

补核文档归档：**满足迁移要求，清理完整度部分满足。** 九份既有过程文档以零内容 diff 搬到 `docs/history/bot-mode/`，现行设计入口（`docs/design/bot-mode.md:3`、`:631`）和目录说明（`docs/history/bot-mode/README.md:3`）能找到历史；research 留在设计目录。小残留是现行设计 `:587`、`:658` 仍说“标记三个键”，与 `:496` 删除两个统计键后的描述冲突，建议改成明确键名；这不是代码或覆盖缺失，也不阻断合并。

## 2. (b) 删除懒物化的边界

| 条件 | 判断 | 依据 | 本轮验证方式 |
|---|---|---|---|
| ① 物化失败不得发布指针；建好目录不算成功 | **满足** | `src/session/store.rs:452` 起依次设置标题、`writer.materialize()?`、写指针。`src/session/writer.rs:330` 的物化依次创建 attachments、取 bundle 锁、开日志、写 meta；任何一步 `?` 失败都回到 `open_bot` 的错误出口，指针调用不可达。尤其 `:338` 虽先设 `created = true`，`:339` 的 meta 错误仍向外传播；这里失败的 writer 被丢弃，不会在同一次启动中再次调用物化然后误发指针。 | 逐句检查控制流；`a_bundle_that_cannot_be_created_publishes_no_pointer` 和 `first_launch_then_resume` 实跑通过。前者只注入 sessions 根不是目录的早期失败，**没有实测目录建好后的 meta 写失败**；该更晚边界本轮依据控制流判断，不虚报故障覆盖。 |
| 指针发布失败允许孤儿空 bundle，下次另建 | **满足** | `src/session/store.rs:454` 物化后才原子写指针；`src/session/bot.rs:46` 复用 `write_atomic`。`an_unpublished_pointer_leaves_an_ordinary_empty_session`（`src/cmd/interactive/bot.rs:553`）用不可写 bot 目录阻止发布，验证无指针、一个 message_count 为 0 且无 owner 的会话，再启动使用新 ID。 | Unix 权限故障测试在本机通过；检查错误传播与局部锁的 RAII 释放。未做进程强杀/断电实验。 |
| ② bot 锁与 bundle 锁都留 | **满足** | `src/session/store.rs:431` 在读指针前取 bot 锁；fresh 在物化时取 bundle 锁（`src/session/writer.rs:335`），resume 在读 meta/log 前取 bundle 锁（`src/session/store.rs:336`）。两路返回前都把 bot guard 交给 writer；writer 仍持有两个独立字段。 | 实跑 `a_running_bot_is_locked`、`bot_lock_names_the_bot`、`resume_is_refused_while_another_store_holds_the_bundle`、`a_new_bundle_is_locked_from_its_first_append`、`delete_is_refused_while_the_bundle_is_held`；不支持锁时失败关闭的 lib 测试也通过。 |
| ③ 故障测试按行为迁移 | **满足** | `tests/repl/bot_flush.rs:762` 改用 `AtomicBool` 控制的局部写盘注入；`src/session/writer.rs:405` 在真实 `write_all` 前返回错误，外层仍走原批次截回。没有用“把打开的日志路径改成目录”冒充写失败。测试仍断言 backlog 未保存时压缩失败、总共只发生一次摘要调用、恢复写盘后的发送与 `load_log` 视图一致，且摘要保留 `one`。 | 该测试实跑通过；核对 `:779`、`:787`、`:799`、`:803` 的行为断言未删。这里名称中的 restart 由重新 `load_log` 验证，测试自身并未再启动整个 REPL；真实重新打开会话的接线由恢复测试和 §3 的 paired run 补充。`a_failed_meta_rewrite_does_not_double_the_turn`、writer 的 failed-cut 测试也通过。 |
| 不留旧指针状态兼容分支 | **满足** | `BotPointer` 只剩 `session`；`src/session/store.rs:432` 有指针就 resume，`NotFound` 一律 `BotMissing`，没有 false 状态回空、stale 补正。`NewSession.id`、`OnCreated` / `on_created`、`never_saved` / `NEVER_SAVED` 整链消失；物化入口是 `pub(super)`（`src/session/writer.rs:134`）。 | `rg` 扫 `src tests scripts` 无上述遗留符号；`a_missing_bundle_is_a_hard_error` 与损坏本体拒绝测试通过。Serde 默认忽略额外 JSON 字段仍在，这不是按旧状态恢复的兼容分支：旧文件若带 `materialized:false` 且本体不存在，也走硬错误（静态结论）。 |
| 空启动留下空会话的产品变化如实记录 | **满足** | `docs/design/bot-mode.md:160`、`:163`、`:696` 分别记录部分初始化失败、首次不说话也落盘、列表/普通 picker 的可见性及孤儿空对象，不声称零成本。被指针指向的 bundle 后续复用；没有指针的孤儿下次不复用。 | 文档与 store 路径核对；`first_launch_then_resume` 验证首个 append 之前已 on-disk、meta 标题正确；`a_running_bot_is_locked` 包含首轮未说话即 drop 后重开；孤儿测试实跑验证独立新 ID。 |

启动顺序的外层也没有倒退：`src/cmd/interactive/mod.rs:475` 在调用 `open_bot_session` 前检查 provider、解析参数、检查最小窗口。元信息已落盘与“用户轮已保存”仍有区别；`tests/repl/commands.rs:1397` 特意新增 `meta.message_count > 0`，避免 eager 创建让原来仅能读 meta 的断言变成假阳性。

## 3. (c) 长跑缩减

### 3.1 必要条件逐条核对

以下实测都来自本轮 `cargo test --lib --test repl --test session --test cmd --test layering -- --nocapture`，不是沿用旧 HEAD 的绿色。

| 条件 | 判断 | 判定位置与本轮结果 | 验证方式 |
|---|---|---|---|
| 不变量 0：每个用户轮号一次且有自己的最终回复 | **满足** | `tests/repl/bot_longrun.rs:483` 在 notice 处清空归属；`:508` 要求完整轮号集合、无重复、完整已答集合。实测 2000/2000，日志 2000 个用户轮，0 重复。 | 比较原判定与当前判定，核心不变；实跑长跑及下述负例。 |
| 不变量 1：日志只增不改 | **满足** | `:136` 检查前缀字节，`:564` 要求无 violations。实测 2148 次增长，无字节被重写。 | diff + 实跑；快照恢复时重置观察基线的既有做法未变。 |
| 不变量 2：请求不超窗 | **满足** | `:574` 同时要求无 refused 和最大 answered input+output ≤ window。实测 0/2372 被拒，最大 23004 < 32000。 | 检查 fake 的计量/拒绝路径未被该批放宽，实跑。这里只是 fake 的 `bytes / 4` 度量。 |
| 不变量 4：每个 marker 前恰一个 flush 或 skipped | **满足** | `:460` 顺序扫主线日志；`:671` 只接受 `(1,false)` / `(0,true)`。80 个 marker：66 次 flush，14 次 skipped。 | diff + 实跑，不用总和恰好相等替代逐区间检查。 |
| 不变量 5：记忆 ≤ 8 KiB | **满足** | `:153` 观察正文峰值，`:689` 检查正文上限。实测正文 6259 B，整文件 6298 B。 | diff + 实跑；正文/含 frontmatter 整文件的口径没有互换。 |
| 不变量 6a：重启加载视图 | **满足，覆盖门槛略加强** | `:298` 现在直接读 `open_bot` 返回的 loaded view，与未重启侧第一请求反推的旧 view 比较；`:713` 保留 `compared * 2 >= DROPS`，并增加 `run.drops.len() == DROPS`。实测 24/24 可比，0 差异，其中 1 处欠 flush。 | 精读 diff + 实跑；不再因重启侧先摘要而跳过。未重启侧首调用是 Summary/Followup 时仍可能 `None`（`:287`），且 system/overlay 不在比较内；没有宣称全状态相等或消除了所有盲点。 |
| 不变量 7：加载时间 | **满足** | `:727` 保留每次启动的 turn / 日志体积 / 耗时曲线及 `< 2s` 门槛。实测最慢 53.85425 ms，最终日志 4642 KiB。 | diff + 实跑；不是大到 256 MiB 的性能证据。 |
| 进展与压缩观测（原不变量 3） | **满足** | `:597` 保留增长 / 保留占用 / 半轮 / flush 的估算和 ±15%；`:748` 仍输出进展。实测 80 marker = 80 summary passes，预期约 80，ratio 1.00，每 25.0 轮一次。`:831` 的 required 列表仍包含 0、1、2、3、4、5、6a、7。 | diff + 实跑，不只是留下打印语句而删除失败断言。 |
| 不变量 0 的负例 | **满足** | `a_flush_reply_does_not_answer_the_users_turn`（`:788`）保留：只有 flush 的回复时失败；补齐用户自身最终回复时通过；删轮/重复轮也失败。 | 实跑通过；只随 6b 删除了三个专属元测试，没有误删这个负例。 |
| 短、固定回复 paired run | **满足** | `tests/repl/bot_flush.rs:1317` 的模型依提示固定动作，不依记忆块；`:1370` 在同一进程 idle 处复制磁盘、继续运行，随后恢复该副本再开一轮。`:1420` 先断言两侧完整种类数组，再逐请求比较 `Message` 内容/顺序；summary 比较整个请求，普通调用排除 system 后比较旧历史。 | 两个新增测试实跑通过；检查了快照时点、恢复路径、固定日期和断言本身，详见下文。 |
| `flush_accounting` 实际范围写清 | **满足** | `tests/repl/bot_longrun.rs:456` 明写只检查主线日志内部自洽，无法对照另一侧的轮次、调用顺序、发送内容，引用短 paired run。 | 注释逐句与函数输入/输出核对；没有把它包装成 restart/no-restart 对照。 |
| 8k 依据有落处 | **满足** | `src/repl/context/tokens.rs:57` 记两次失败数字；[8k 归档](bot-mode-8k-evidence.md):15、:24、:56 记提交、命令、退出状态、输出与 fake 局限。明确 8k 失败、32k 通过不证明 31999 必败，更改窗口/记忆/reserve 要重新测。 | 核对历史提交中确有被删场景及归档指向；本轮只实跑保留的 32k 与启动拒绝小窗口测试，**未重新检出运行 8k**。历史输出不能算本轮实测。 |

两个短对照具体钉住了什么：

- `a_restart_sends_what_running_on_would_have_with_a_flush_queued`（`tests/repl/bot_flush.rs:1441`）：两侧必须是 `[flush, followup, summary, turn]`；完整 summary 请求相等，且显式含 `User: zero`、`prefers tabs`、`deploys on fridays`；flush 首请求的 memory 块分别断言旧副本/重读副本，压缩后 system 块相等并含新写事实。
- `a_restart_sends_what_running_on_would_have_when_the_next_message_compacts`（`:1473`）：两侧必须是 `[summary, turn]`；完整 summary 请求相等且含旧历史和记忆；后续用户请求的历史相等，刷新后的 system 块也相等。

这确实检查了送出的旧历史及摘要输入，不是只比较加载 view。它主要抓两侧差异；两侧同时发生同一种错误时，仍靠显式内容断言及原有定点测试发现，不能当作所有历史内容的独立黄金标准。已有 `a_restart_near_the_threshold_still_compacts_before_the_next_message`、`without_the_measurement_the_restart_would_not_compact`、`a_flush_queued_when_the_process_went_down_runs_after_the_restart` 也都保留并通过。

### 3.2 真正失去的覆盖：上轮五类逐类对账

| 上轮列出的覆盖 | 当前保留/替代 | 真正放弃的部分 |
|---|---|---|
| 每个随机 drop 后的调用种类与顺序 | 两个短 paired run 严格钉两个代表序列；恢复计量和欠 flush 的旧 bug 测试保留。 | 不再在长跑 24 个 drop、不同历史长度/状态组合下检查完整发送顺序。6a 不比较 meter、snooze、失败计数等内存状态；不能等价替代。 |
| 发送副本的旧消息内容与顺序，含 summary 旧历史 | `assert_same_sends` 对短对照逐条比较消息及整个摘要请求；摘要结构、previous-summary、保留尾轮的定点测试继续通过。 | 随机长历史上漏消息、重排、摘要输入裁错的端到端差分网已撤掉；新短对照只覆盖其固定历史。 |
| 记忆重读后的 notice / 新工具写入因果与 flush 写入计数 | 固定回复对照比较对应历史/summary；`a_bots_summary_pass_sees_the_memory_and_the_flush_writes`、正常 memory notice 和 flush 测试保留。 | `refresh_explains` 等豁免器及其三个负例删除，不再在长跑动态验证“模型因重读改变动作后，哪些 notice / 计数才合法”。`flush_accounting` 只数 flush notice 与 skip，无法补回。 |
| tidy 32k 的长期小记忆策略 | 唯一长跑是软阈值策略，开头经历小记忆；小文件/记忆工具定点测试仍在。 | 独立的 2000 轮 `.keeping(12)` 策略及 fake 的 keep_lines 分支已删。软阈值负载不构成其严格超集；不再验证“未到软阈值就主动删旧条目”的长期路径。 |
| 8k 可执行反例 | 历史提交、命令和输出归档；`a_bot_needs_a_32k_window` 钉启动政策。 | 当前树中没有 `--ignored` 即跑的 8k 反例。启动拒绝测试不能证明 32k 数值最优或 8k 负载为何失败；需到历史提交重测，未来改政策应建立新证据。 |

**这五项都是明确接受的取舍，不是零覆盖损失。** 当前 oracle 没有因删除 6b 而顺便放宽其余不变量；6a 计数要求还加强了一点。也不把本轮 14.43 s 与上轮两长跑并行的 25.44 s 当作受控 CI 性能对照。

## 4. (d) 保留率脚本

| 条件 | 判断 | 依据 | 本轮验证方式 |
|---|---|---|---|
| recall 组及旋钮删干净 | **满足** | `scripts/bot-retention.sh:96` 默认只有 `noflush flush`，`:99` 只接受这两个值，`:202` 不再处理 recall/skipped，`:220` 配置不再插入未来工具集。`RETENTION_RECALL_SET` 没有定义/消费/说明残留。评分说明中的英语 recall 只是“回忆率”，不是隐藏组。 | diff + `rg`；`bash -n` 退出 0；无模型拒绝路径实跑见 §6：传 `recall` 退出 2，明确列出两个合法组。 |
| 收回“脚本定稿 §6 #13”承诺 | **满足，改得正确** | `docs/design/bot-mode.md:624`、`:651`、`:664` 改为“两组试运行只给方向证据；常量由产品依据证据选择”，说明不调常量/无候选对照、组间未配平、关键词评分的局限。`:674` 保留旧决策原文但紧接带日期的明确修订，不是仍有效的旧承诺。脚本开头 `:2` 同步收回 finalise。 | 文档/脚本逐句与实际流程核对。没有运行真实模型，不对事实保留率下结论。 |
| 没把 #13 与 reserve/最小窗口混成一项 | **满足** | `docs/design/bot-mode.md:651` 的 #13 仍是 MEMORY 8/6 KiB、单条 500 B、摘要 1500 词、note 32 KiB × 200；`:624` 显式把 reserve / 最小窗口归到 §3.6.1 / §4.1。脚本 `:92` 仍拒绝低于最小窗口，没有冒充扫描边界；noflush 仍按手动间隔，flush 按阈值（`:311`、`:319`）。 | 静态核对参数与分组执行路径，确认没有删除当前重复压缩/保留率实验来冒充完成验收。 |
| 合并状态与实验验收状态分开 | **满足** | `docs/design/bot-mode.md:629` 说脚本不阻塞合并，`:630` 仍列明真实模型保留率验收未完成；结果不确定时照实写方向未定，不把一次 20/20 当常量验证。 | 阅读现行验收状态；本轮只做语法和非法组拒绝验证，未花 API token。 |

“检验方向”仍应解释为**这两种运行策略在所选模型/负载下的方向性样本**。两组都能主动 remember、输入长度和压缩间隔未配平，不能声称严格隔离了 flush 的因果增益。现行文档已写这些限制，表项区分也正确。

## 5. 新问题与剩余小项

**没有发现新增的阻断问题。** 重点检查的两类风险没有成立：启动路径不是“目录存在就发布”；测试也不是删掉严格比较后只剩总量相等。还有一项不应漏报的范围外延：`5f97c77` 同时实现了上一轮 §2(e) 提到的记忆读取归属闭环，它是可见行为收紧，不是机械重命名。

该闭环的检查结果：**满足预期。** `src/agents/memory.rs:218` 提取原 write-side owner 比较，`:697` 的 `read_owned` 复用它；snapshot reload（`src/agents/memory/snapshot.rs:66`）和 summary 的 fresh read（`:101`）都不再读入外来正文。`src/repl/commands/compact.rs:313` 把 fresh-read 失败 warning 显示在 transcript，摘要拿到空 memory；`docs/design/bot-mode.md:306` 同步说明拒读/拒写。`another_bots_file_is_not_injected` 实跑检查不泄漏正文、空 current、warning、不重复刷新，以及外部改正 owner 后恢复；`the_frontmatter_names_the_bot` 继续钉拒写和无 owner 的人写文件可采用。没有增加新所有权状态或兼容迁移。

建议合并后处理的小项（均不是本轮发现的功能故障）：

1. **补一个晚期物化失败的回归钉子**：当前“不发布指针”测试只挡在建目录之前，建议再覆盖目录/日志已建而首次 meta 写失败的情况。代码的 `?` 已满足规则，所以这是测试补强，不是要求重构启动路径。也可顺带补合法 >100 B 标题的正例及总 cap 拒写，钉住刚放宽的用户可见行为。
2. **清掉文档的“标记三个键”残留**：`docs/design/bot-mode.md:587`、`:658`，应与 `:496` 的现行字段一致。`5f97c77` 提交说明的 “reviews converged” / “read by nothing” 也仍不够精确：上轮已说明评审有分歧、测试曾读统计键；本报告不据此声称全员一致或无人读取，也不要求重写提交历史。
3. **核对 Windows CI 那一腿**：`.github/workflows/ci.yml:116` 明确跑 fmt/clippy/docs/tests，但本轮仅在 Darwin 执行。新增发布失败权限测试有 `#[cfg(unix)]`，Windows 不覆盖这一故障注入；锁、打开句柄后的截回以及 paired run 目录恢复在 Windows 的结果仍应以该平台 CI 为准，本轮不推测它已通过。
4. **由用户选模型与预算后，考虑跑一次两组保留率试运行**：保存参数、回答、逐事实评分和适用范围，更新验收状态；不需要为“定稿常量”搭参数搜索平台。不跑就继续保持“未验收”，不要因为本报告批准合并而划掉它。

## 6. 本轮实测记录

环境：本机 Darwin，当前 HEAD。最初沙箱内运行 lib 与集成测试时，lib 有 2 项、cmd 有 27 项因绑定本地 mock 端口/socket 被拒而失败（`PermissionDenied: Operation not permitted`）；集成命令在 cmd 失败后尚未执行其余目标。随后获准在沙箱外重跑下列完整命令，退出 **0**，全部通过。这些环境失败没有被忽略计入绿色。

```sh
cargo test --lib --test repl --test session --test cmd --test layering -- --nocapture
```

| 目标 | 结果 |
|---|---|
| lib | 1109 passed，0 failed，0 ignored，9.59 s |
| cmd | 127 passed，0 failed，0 ignored，2.92 s |
| layering | 4 passed，0 failed，0 ignored，0.07 s |
| repl | 147 passed，0 failed，0 ignored，14.92 s；含全部 21 个 bot_flush 测试和 2 个 bot_longrun 测试 |
| session | 86 passed，0 failed，0 ignored，0.51 s |

共 **1473 项通过**。本轮日志位于 `/private/tmp/bot-final-verified.log`（临时诊断记录，不作为需保存的交付件）；关键输出摘录（缩短标签和换行，数值原样）如下，避免只依赖临时文件：

```text
memory at the soft threshold, 32k: 2000 of 2000 turns answered,
2372 calls (28 in no-restart references), 24 drops, log 4642 KiB, 14.433359667s
[ok] 0: 2000 of 2000 turns saved with a final reply, 2000 user turns in the log, 0 twice
[ok] 1: 2148 growths seen, none rewrote a byte
[ok] 2: 0 of 2372 calls refused as over 32000 tokens; widest answered call 23004
[ok] 3: 80 markers (80 summary passes), expected ≈ 80,
        growth 1092395 / (16000 − 4294 kept + 244 half a turn + 1749 flush), ratio 1.00
[ok] 4: 66 with a flush, 14 flush_skipped
[ok] 5: body up to 6259 bytes, whole file up to 6298 bytes
[ok] 6a: 24 of 24 drops compared (1 with a flush queued), 0 differ
[ok] 7: slowest 53.85425ms (bound 2s)
```

其它只读检查：

| 命令/核对 | 结果 |
|---|---|
| `cargo fmt --check` | 退出 0 |
| `cargo clippy --all-targets -- -D warnings` | 退出 0 |
| `git diff --check 2b07294..HEAD` | 退出 0 |
| `bash -n scripts/bot-retention.sh` | 退出 0 |
| `env RETENTION_MODEL=review-placeholder RETENTION_OUT=/private/tmp/bot-final-retention-unused bash scripts/bot-retention.sh recall` | 预期退出 2：`bot-retention: unknown group recall (noflush, flush)`；参数校验即结束，没有启动 tmux/模型 |
| `rg` 扫 `src tests scripts` 的旧符号 | `BOT_POINTER_VERSION`、`BotRunning`、两种 token 尺寸字段、`MEMORY_SECTION_CAP`、`OnCreated` / `on_created`、`never_saved` / `NEVER_SAVED`、`materialized`、`append_compaction_with` / `CompactionStats`、`RETENTION_RECALL_SET`、`workspace-true` 均无匹配 |

未声称完成：完整 `ci.sh`（含发布大小/终端等其它检查）、Windows/Linux 实机、真实模型保留率/缓存/压缩先验、8k 历史反例重跑、掉电持久性实验。上述限制与本次“批准这四批简化合并”的结论并存；没有还必须修复的合并阻断项。
