# bot 模式 v1 实现独立评审

评审日期：2026-10-01。评审对象：`bot-mode-v1`，HEAD `345fde71d86d1cd7b8fe567178dd53e2ed73eae5`，相对 `main` `5317eb43fd6b5939a7819f4446ebfe171e52a2df` 的 13 个提交（`999e0e6` 至 `345fde7`）。已核对 `git log --oneline main..HEAD`、`git diff main...HEAD`、实现及相关测试。

以 `docs/design/bot-mode.md` rev 2、尤其 §6 的已定决策为需求基准；recon 用来核对既有行为，critique 用来复查此前的风险。下文源码行号均指本次 HEAD，不沿用设计文档中的旧源码坐标。本文的 R 编号与设计评审的 S 编号相互独立。

**结论：暂不合并。发现两项严重问题：保存失败后自动压缩会丢掉未落盘的保留尾部；中断后的记忆 notice 会使原本可修复的孤儿工具调用连重启也修不到。** 正常路径、模块边界和测试投入总体扎实，但“永不结束的会话”不能建立在这两个持久化缺口上。

## 严重发现

### R1：保存失败后仍提交压缩，未保存的最后一轮被水位线永久跳过

- **结论**：压缩成功被等同于摘要 API 成功，未验证原始 backlog 和压缩标记是否落盘。这会真实丢数据，而不只是重启时摘要不同。底层问题在 `main` 的手动压缩路径已存在；本分支把它接入无人值守的 flush → compact 流程，放大为 bot 的可靠性缺口，不能声称是全部由本分支新引入。
- **依据**：`src/repl/run.rs:228` 的 `persist_turn` 失败只打印 warning、保留旧水位，且不把失败返回给调用者；`src/repl/run.rs:898`、`:912` 在保存失败后仍令本轮 `landed = true`；`:916`、`:917` 继续驱动 bot。`src/repl/commands/compact.rs:367` 先替换内存历史，`:379` 只追加 marker，`:381` 对 marker 保存失败也仅警告，`:387` 无条件推进 `persisted`，`:400` 无条件报告 `Compacted::Done`。`src/session/writer.rs:236` 则按实际已写入的 `conv_count` 计算 marker，不能替内存中未落盘的尾部补原文。
- **具体失败场景（已复现）**：让新 writer 的 `on_created` 回调持续返回写盘错误，跑 `zero`、`one` 两轮，`one` 越阈值后跑 flush；仅在摘要调用时恢复写盘。下一条 `two` 的实时请求仍含 `one`，但磁盘只剩一条“zero 的摘要”marker 和 `two / reply to two`，重启完全没有 `one`，也没有它的 flush 交换。探针经公开的 `iota::repl::run` 驱动，结果是 `live send keeps one=true; reload keeps one=false`。这模拟指针写入暂时失败、摘要期间磁盘恢复；没有修改产品源码或测试。
- **相关写坏日志路径（亦已复现，继承自旧 writer）**：`src/session/writer.rs:195` 逐条写入后，`:206`、`:207` 才 sync 和写 meta。让 meta 写入失败，日志其实已经有新的一问一答；调用方重试同一 backlog，又追加一遍。实测一个 seed 加一次新问答，重试后成为 5 条而非 3 条。新增的超长记录拒绝也仍会在批次中途返回错误（`:197`、`:288`），调用方没有“已提交前缀”的信息。只在压缩前再盲目调用一次 `persist_turn`，不够解决这个问题。
- **建议**：把保存是否成功传回编排；存在未确认落盘的 backlog 时不推进压缩及水位。marker 写入成功后再安装新视图、清 flush 状态、刷新记忆；失败保持原状态并报告失败。同时给 writer 的批次失败定义可重试语义：区分日志已提交与仅 meta 失败，或在持锁状态恢复到批次起点，避免重复追加和计数漂移。不需要引入通用事务框架。补“保存失败 → 恢复 → 自动压缩 → 重启视图相等”和“meta 失败后重试不重复”的行为测试。

### R2：记忆 notice 把中断尾部的孤儿调用变成永久的中段孤儿

- **结论**：一个正常的“remember 后等待另一个工具审批，再中断”流程即可产生无法自动恢复的会话；并非只能靠外部编辑坏日志触发。Anthropic 连续 user 消息合并修复不了缺失的 `tool_result`。
- **依据**：工具批次先整体追加 assistant 调用列表（`src/repl/turn/tools.rs:115`、`:135`），审批取消可以在补齐结果前返回（`:221`、`:227`）。中断历史按原样保留（`src/repl/turn/interrupt.rs:68`，落盘在 `src/repl/run.rs:1198`）；随后无论本轮成功与否，都执行 `record_memory_writes`（`src/repl/run.rs:915`），把 notice 追加到历史及日志（`:251`、`:265`、`:267`）。`repair_tail` 只查看最后一个非 Tool 消息（`src/session/loader.rs:254`、`:258`），此时看到的是 notice。Anthropic 在 `src/provider/anthropic.rs:143` 和 `:156` 分别构造调用与结果，不会补配对。
- **具体失败场景（已通过完整 REPL 复现）**：模型同批返回 `c1=remember`、`c2=shell`；c1 已成功写 MEMORY.md，c2 的审批返回 Interrupted，然后退出。日志是 `user → assistant(c1,c2) → tool(c1) → memory notice`。再次 `store.resume` 的实测结果为 `repaired=0, missing c2=true, trailing notice=true`。继续发送携带这个未配对的工具历史，违反 Anthropic 工具协议；本次没有向真实 API 发送验证请求。即使重启也无法走尾部修复，只有手工处理或绕过这段历史才能继续。
- **与已知限制的区别**：设计 `docs/design/bot-mode.md:670`、`:671` 承认中段孤儿和同进程中断问题，但这里是新增 notice 主动把一个本来可在重启时修复的尾部移到了中段；不能用“重启即可恢复”解释它。它也可发生在有记忆写入的 flush 中断后，随后压缩又会按最后一个真实用户轮保留这段坏历史。
- **建议**：在中断历史第一次落盘、追加任何 notice 之前，为本轮尚未回答的调用补 interrupted 结果；让同进程下一次发送也使用这份修复后的历史。无需实现任意中段日志重写，也无需等待 L2 的失败轮保留。补“remember 成功 + 后续审批取消 + 再发送 + 再次 resume”的测试，并检查调用 ID 恰好配对，不仅检查 notice 存在。

## 重要发现

### R3：`remember` 的 `section` 和归一化顺序绕过了写入约束

- **结论**：密钥拒写、单条一行、来源标记并没有覆盖所有实际写入路径；这是设计 S1 对策的实现漏洞，不是要求增加审批或更复杂的信任系统。
- **依据**：`src/agents/memory.rs:101` 只要求 `Project:` 后非空，未拒绝换行；`:277`、`:287` 把 section 原样作为 Markdown 写入。只有 `text` 经过 `make_line`（`:518`）；而 `make_line` 在归一化前检查密钥（`:404`），然后才折叠换行（`:407`）。工具入口直接采用该 section（`src/tool/builtins/memory.rs:136`、`:138`）。
- **具体失败场景（已复现）**：`action=add, source=inferred, text="safe fact", section="Project: x\n## User\n- Bearer FAKE_TEST_TOKEN"` 成功生成全局 User 小节中的无来源行，绕过密钥规则和单条长度检查；这行以后还会被当成人写的而拒绝模型修改。另一个更普通的输入 `text="Bearer\nFAKE_TEST_TOKEN"` 也成功，最终落盘为本应被拒绝的 `Bearer FAKE_TEST_TOKEN`。以上均使用虚构字符串。
- **建议**：section 限为一个标题行，拒绝换行、控制字符，并对将被写入的标题内容执行同一密钥规则；先归一化 text 再检查最终单行内容。只校验工具新增的内容，不扫描并阻止用户既有文件。补上述两种反例，以及 section 中超长/带秘密的值；不扩大为任意 prompt 内容审查。

### R4：初始化日志失败后，writer 会被自己持有的锁永久挡住

- **结论**：单写者锁的正常竞争处理正确，但失败重试生命周期不完整；一个可恢复的 I/O 错误会变成必须重启才能解除的自锁。
- **依据**：`src/session/writer.rs:272` 把锁放入 `self.lock`，`:273` 打开日志失败时以 `?` 返回，`:274` 尚未设置 `created`。下一次进入 `ensure_created` 又在旧 guard 仍存活时调用 `lock_bundle`。`src/session/lock.rs:63` 的非阻塞排他锁会拒绝第二个句柄，不管它是否来自本进程。
- **具体失败场景（已复现）**：在 pending bundle 下让 `messages.jsonl` 暂时成为目录，第一次 append 报 `Is a directory`；把障碍移走，再次 append 报 `session … is open in another iota process (pid 本进程)`。打开文件遇到临时权限或句柄资源故障也会走同一条失败路径。后续保存无法恢复，只剩内存历史。
- **建议**：初始化失败时统一恢复 guard/句柄状态，或复用已经持有的锁而不再次加锁；保持“拿锁后才写”的顺序。给现有锁测试加“首次 open 失败 → 修复外部条件 → 同一个 writer 再次 append 成功”，无需另建锁抽象。

### R5：从配置删除 system 不生效，旧 system 永久保留且没有提示

- **结论**：这偏离 §2.2 的“配置变更生效”。是实现为规避旧空 ToolsMount 兼容问题而省掉了合法状态，不是实现更正确。
- **依据**：`src/cmd/interactive/bot.rs:187` 明确说明空配置保留旧值，`:194` 直接返回 false；`src/session/loader.rs:302` 忽略所有空 system。设计 `docs/design/bot-mode.md:177` 承诺比较 config 与历史、变化即替换并提示，没有排除清空。
- **具体失败场景**：用户删掉旧角色指令或把 `system_file` 清空，重启 bot 后旧指令仍进入 system 段；没有 `system prompt updated from config`，用户以为已撤销的约束实际上还在。测试 `a_changed_system_prompt_is_taken_from_the_config`（`src/cmd/interactive/bot.rs:589`）只测非空到非空，`:629` 只补了空到非空，恰好漏了反方向。
- **建议**：让 bot 的当前请求确实服从空 config，并明确持久化清空的表示；同时保留旧日志中误落盘 ToolsMount 的兼容性。不要简单恢复“任意空 system 胜出”，否则会重新引入本分支刚修的旧日志问题。补非空 → 空 → 再重启测试，断言发送内容而不只看 meta。

### R6：bot 所有权扫描吞掉读取错误，损坏指针反而解除本体保护

- **结论**：单独启动 bot 对坏指针是失败关闭，但普通 resume/delete 的保护是失败放行，两条入口的安全语义不一致。
- **依据**：`src/session/bot.rs:41` 的 `BotPointer::read` 返回解析/读取错误；`:75` 把目录读取错误视为没有 bot，`:82` 用 `.ok()??` 丢掉坏指针。`src/session/store.rs:143` 据此查询 owner，`:164` 在没有 owner 时放行；`:391` 是 delete 的保护门，普通 resume 使用相同检查（`src/cmd/interactive/mod.rs:515`、`src/cmd/mod.rs:440`）。
- **具体失败场景（已复现）**：bot 离线后，`bot.json` 因一次错误编辑或读取故障不可解析；`iota run coder` 拒绝启动，但普通会话入口把其 bundle 当成无主。探针先验证有效指针时 delete 被拒，再把指针写成无效 JSON，得到 `pointer read failed=true, owner check allows=true, delete succeeds=true`。因此“不读取坏本体、不误删”的保护会在最需要恢复时消失。
- **建议**：区分“指针不存在”和“无法判断所有权”。读取失败必须可见，并阻止依赖这一判断的写式 resume/delete；只读列表可以提示故障。不需要新建索引或往 session meta 重复存所有权。补“坏指针时普通入口不得写/删”的测试。

### R7：承诺的长跑验证与保留率验收尚未交付，现有假 provider 无法替代

- **结论**：实现了记忆机制，但还没有证据支持“跨周多次压缩仍保留事实”的验收结论。这里判定的是缺少交付物与验证，**不是推断真实模型一定丢失某个比例的信息**。
- **依据**：设计 `docs/design/bot-mode.md:597` 要求 8k 窗口、2000 轮、随机重启的 `GrowingProvider`；`:600`、`:620`、`:643` 明确把 `scripts/bot-retention.sh` 和数值定稿列为 L1 验收项。对 `src/testing/`、`tests/`、`scripts/` 的检索未找到该 fake、脚本或对应长跑测试。现有 `tests/repl/bot_flush.rs:191` 使用固定摘要，`:205`、`:210`、`:221` 人工指定 usage；不会根据请求大小拒绝超窗，也不会验证摘要保留了哪些事实。
- **具体失败场景**：实现即使在第十次压缩后视图越窗，或者重启丢掉某段历史，几轮固定用量测试也可能全绿；返回恒定 `SUMMARY` 的 fake 更不可能证明事实召回。`nothing_saved_keeps_durable_facts_in_the_summary`（`src/repl/commands/compact.rs:950`）实际断言提示词，而不是其名字暗示的事实保留效果。
- **建议**：交付设计要求的按实际请求计算用量、重启对照的长跑测试；补 opt-in 保留率脚本及可复核结果，再确定临时上限。v1 可以先跑无 flush / 有 flush 两组，明确第三组依赖 L2 recall，不为跑实验把 L2 功能偷渡进 v1。缓存 A/B、廉价压缩先验也未见本分支附带结果；只能记为“未提供证据”，不能断言作者私下没做过。

## 次要发现

### R8：flush 限制了 dispatcher，但历史里的 frozen tools mount 仍会广告其它工具

- **结论**：普通工具执行的硬过滤有效；“只广告 memory 工具集”的承诺在 `system-tools` defer 方言下不完整。严重程度低于任意工具可以执行。
- **依据**：`src/repl/run.rs:930` 对 flush 使用 `Only`；`src/tool/dispatch.rs:293` 过滤当前 tools，`:307` 拒绝其它执行。但是 `src/repl/turn/tools.rs:66` 仍发送包含旧挂载的历史，`src/agents/mod.rs:354` 保留历史中的后续消息；`src/provider/openai.rs:85`、`:90` 会把 `Message::system_tools` 中的 schema 原样编码进请求。
- **具体失败场景**：前面的普通轮已加载某个 deferred MCP 工具，随后 flush 的顶层 tools 只有 remember，历史 system-tools 消息却仍广告那个 MCP 工具。**推测**：模型可能选中它、收到 UnknownTool 后多跑无意义 round；是否反复发生依赖模型，未实测。已有 flush 测试（`tests/repl/bot_flush.rs:265`）只检查 `seen_tools`，fixture 没有历史 mount。
- **建议**：仅在 flush 的发送副本中过滤历史挂载中的非 memory schema，不改原始历史；增加一个带历史 mount 的发送测试。调用拒绝门应保留。此项可跟进，不作为单独阻断合并的理由。

### R9：无人值守 bot 的推荐配置模板没有交付

- **结论**：属于小的交付缺项，不应通过修改默认审批政策来补偿。
- **依据**：`docs/design/bot-mode.md:533` 承诺 sandbox + `auto_run` + `auto_write` 的 bot 模板；现有 starter（`src/cmd/config_cmd.rs:43` 至 `:53`）仍只有普通 agent 的注释入口，设计 §1.1 的示例也只有空 shell/code 配置。
- **具体失败场景**：用户照示例开 bot 并离开，第一处需要审批的写操作会停在 NeedsInput。这是现有审批机制的正确行为，但用户没有得到文档承诺的配置答案。
- **建议**：补一个独立的 opt-in bot 配置示例，说明沙箱与审批范围；保持现有默认值。非阻断项。

## 与设计的一致性及旧评审复核

| 项目 | 核对结论与依据 |
|---|---|
| mode、CLI 和固定本体 | 基本符合 §6 #3/#15/#17/#22：bot 自动记忆、flat bundle、拒绝 headless/no-save、禁止普通 resume bot、隐藏 `/session`、固定标题。入口见 `src/cmd/resolve.rs:76`、`src/cmd/interactive/mod.rs:459`、`:515`、`:629`、`src/session/store.rs:425`、`:458`。所有权错误分支有 R6。 |
| 两把锁与崩溃恢复 | 正常路径符合设计：bot 锁早于指针读取（`src/session/store.rs:425`），bundle 锁早于加载（`:331`），修复后追加结果（`:337`、`:341`）。`HeldLock` 显式 unlock（`src/session/lock.rs:34`）比只依赖 close 更稳妥，处理 fork 继承句柄的窗口，是实现更周全；初始化重试仍有 R4。 |
| ToolsMount 与日志行上限 | 写侧过滤 mount、读侧兼容旧空 system（`src/session/writer.rs:190`、`src/session/loader.rs:298`）；`fit_line` 先去 raw 再截文本并拒绝仍超限的结构（`src/session/writer.rs:315`）。不是把超长日志永久写坏。清空真实 system 的回归见 R5；批次失败语义见 R1。 |
| 设计评审 S1：记忆写入风险 | 大部分对策已落地：Expanded（`src/tool/builtins/memory.rs:92`）、diff（`:105`）、来源/日期和人写行保护（`src/agents/memory.rs:403`、`:431`）、备份（`:654`）、数据前言和关闭标签转义（`src/agents/memory/snapshot.rs:17`、`:185`）。仍不能算完全解决：R3 绕过写入门，R2 暴露 notice 与恢复的冲突。来源仍由模型声明，这是 §6 #21 接受的方案，不要求另起鉴权系统。 |
| 设计评审 S2：flush 时序 | 三项核心修正基本正确：只有 flush 输入跳过 offer（`src/repl/run.rs:792`）；工具执行被过滤且 steering 关闭（`:930`、`:951`）；保留最后一个非 notice 用户轮（`src/repl/commands/compact.rs:102`）。用户插队、flush 失败后的待办、snooze、过期 notice 均有状态机/集成测试。R8 是广告侧的漏项；R1/R2 是失败边界尚未闭合。 |
| 设计评审 S3：摘要与记忆互不可见 | 提示词机制已补：压缩读当前磁盘记忆（`src/repl/commands/compact.rs:321`），按 flush 写入数改变 addendum（`:77`），剥离旧摘要（`:197`），记录 token 统计（`:371`）。但“效果已验证”未完成，见 R7；不能用固定摘要 fake 替代保留率实验。 |
| 四个刷新时刻 | 启动读取、外部编辑刷新（`src/repl/run.rs:374`、`:974`），压缩后/换日经动作 reload（`:1139`；状态机 `src/repl/bot.rs:198`、`:222`）。发送前压缩后才取 overlay（`src/repl/run.rs:798`）正确，最后一次提交修掉的旧快照问题有 `the_send_after_a_pre_send_compaction_carries_the_refreshed_memory` 覆盖。 |
| §6 #23 的状态机 | `src/repl/bot.rs:85`、`:115` 的 Event/Action 只携带朴素值，没有 Ui、ContextBudget、CtxMeter；I/O 留在 `src/repl/run.rs:1105`。符合约束，没有为了未来提前造新层。 |
| 合理的实现调整 | reserve 封顶半窗（`src/repl/context/meter.rs:109`）和所有模式都用真实用户轮作保留锚点（`src/repl/commands/compact.rs:102`）均有实际正确性收益，且 rev 2 已追认；不是偷懒。Anthropic 连续 user-role 内容合并（`src/provider/anthropic.rs:93`）也有完整 wire-body 测试。 |
| 文档承诺但未完成 | 配置清空 R5、完整记忆输入约束 R3、错误时的本体保护 R6、长跑/保留率验收 R7、推荐模板 R9；flush 广告的特殊方言漏项见 R8。其余机制的异常路径见 R1/R2/R4。 |
| 已明确排除的范围 | recall/notes、档案检索、逐 round 落盘、失败轮保留、loader 惰性物化、inbox/headless、守护进程均明确在 L2 以后（`docs/design/bot-mode.md:564`、`:571`）。这里不把它们列为实现偷懒，也不建议为了合并 v1 增加这些系统。 |

“flush 是 best-effort”允许的是记忆抽取失败，不是把原始日志保存失败也当成成功压缩。相似地，接受“崩溃丢在途一轮”不等于接受已经显示完成、仍在实时历史中的轮被水位线跳过；R1/R2 需要在 v1 内解决。

## 测试质量与实际验证

本次执行：

```text
cargo test --lib --test layering --test session --test provider --test repl --test cmd --quiet
1090 lib + 126 cmd + 4 layering + 129 provider + 131 repl + 85 session = 1565 passed

cargo clippy --all-targets -- -D warnings
通过
```

首次沙箱内运行有两个 mock server 绑定端口被拒；获准在沙箱外重跑后全部通过，不计为分支缺陷。没有跑完整 `ci.sh`、跨平台/真实终端长跑、真实模型保留率或缓存实验，不能据此宣称这些也通过。

另用当前构建的库编译一次性行为探针，源码从 stdin 输入，数据和可执行文件只在临时目录中并已清理；未修改或新增仓库代码/测试。R1、R2 通过完整 REPL 入口复现；R3、R4、R6 和 R1 的重复追加分支通过公开的 memory/session API 复现。

| 测试/组 | 判断、遗漏场景与必要补充 |
|---|---|
| `tests/repl/bot_flush.rs:251`、`:332`、`:387`、`:463` | 有价值的行为测试：检查请求顺序、工具集、marker、重载视图和宿主 ping，不能一概称为 happy-path 假绿。但没有保存失败、flush 已写记忆后中断、实际大小增长和多次重启；R1/R2 可在它们全部通过时发生。补失败组合与 R7 的长跑，不必重写已有测试。 |
| `resume_answers_the_tool_calls_the_log_left_open`（`tests/session/store.rs:593`）、`resume_terminates_a_torn_last_line_before_appending`（`:632`） | 精确证明纯尾部恢复及换行修补；没有覆盖新增 notice 把尾部变中段。补 R2，断言配对和第二次恢复结果。 |
| `persist_warns_and_retries_the_backlog`（`tests/repl/commands.rs:1250`） | 该旧测试仅在建目录前拒绝写入，并在下一轮恢复；没有已写日志后的 meta 失败，也没有 compact 插入其间。不能由它的绿色推出 backlog 重试幂等。补 R1 的提交边界。 |
| 锁冲突/drop、`on_created_runs_once_after_materialisation`（`tests/session/store.rs:421`、`:541`） | 正常锁竞争与回调重试有覆盖；回调失败发生在 `created=true` 之后，所以测不到拿锁后、打开日志前的 R4。只需补这个失败阶段。 |
| `section_names_are_the_three_conventions`、`secrets_are_refused`（`src/agents/memory/tests.rs:156`、`:383`） | 断言本身合理，但输入域太窄：section 只有三个普通反例，secret 只通过 text 传入。补最终落盘内容的检查，覆盖 R3 的标题与归一化。 |
| `a_changed_system_prompt_is_taken_from_the_config`（`src/cmd/interactive/bot.rs:589`） | 非空替换与不变时不追加都测到了；缺反向清空，见 R5。 |
| `pointers_scan_the_bots_directory`（`src/session/bot.rs:149`） | 把“跳过坏指针”钉成了通过条件，却没从保护使用者的角度测试 delete/resume；这是危险的实现细节测试，R6 展示了其假安全感。应让保护门测试决定如何处理无法判断的 owner。 |
| `nothing_saved_keeps_durable_facts_in_the_summary`（`src/repl/commands/compact.rs:950`） | 只证明提示词选择；作为 prompt 单测有用，作为事实保留验收不成立。R7 补实际结果验证，不应靠更强的字符串断言冒充模型评测。 |
| `cli_sighup_exits_130_with_interrupted_json`（`tests/cmd/cli.rs:895`） | 实际驱动 `-m hi --output-format json`（`:909`），证明信号取消与退出码；没有验证交互 bot 已完成工具轮落盘。建议补一个交互轮取消后 resume 的行为检查，尤其覆盖 R2。没有据此断言 SIGHUP 注册本身失效。 |

## 分层、风格与抽象

**未发现需要阻断的分层或 clippy 问题。** `tests/layering.rs:25` 的层序及空的 `KNOWN_UPWARD`（`:43`）未被放宽；四个 gate 全部通过。memory 的文件规则放 `agents`，工具层解码/展示，session 管持久化，cmd 注入 bot 目录与配置；符合既有依赖方向。`Cargo.toml:190` 的 all/pedantic 在 `-D warnings` 下通过。

**没有发现值得删除的“为将来预留而无人使用”的抽象。** `Flush` 有实际编排调用和纯状态测试；`Only` 有本轮执行隔离用途；`HarnessInputs.clock` 有换日行为测试；`OnCreated` 服务指针物化；`WriteLog` 连接工具写入与 REPL notice。拆成纯规则与 I/O 的 memory 代码也已有直接消费者。建议保留这些边界，不为修上述错误另造 memory backend trait、事件总线或守护进程层。

值得改的是错误语义，不是个人风格：R1 把持久化失败吞成状态成功，R4 的 guard 状态无法重试，R6 把“不知道”压成“不属于 bot”。继续增加注释或抽象都不能替代这三个具体边界的修复。

## 合并门槛

我不会合并当前 HEAD。必须先完成：

1. **R1、R2**：先保证保存失败不会推进压缩水位、中断不会被 notice 固化成坏工具历史，并补重启后的行为断言。
2. **R3 至 R6**：封住记忆输入绕过、修复自锁重试、支持空 system 配置、让所有权检查在读取错误时保护数据。这些都应是局部修复。
3. **R7**：补承诺的长跑机制测试和 L1 保留率验收材料；至少清楚区分已验证的机制与仍属临时选择的数值，不能以当前全部测试通过代替验收。

R8/R9 可作为明确的后续小项。无需为本次合并提前实现 L2/L3；也不要求重做已经正确的正常路径、加审批门或重构分层。
