# flush 与摘要的交接缺口：对修法方案的评审（fable）

2026-10-01，分支 `bot-mode-v1` 的 `75e6c27`。评审对象是 PM 提出的四种修法与它的推荐（先做 1 + 2，4 留作后手），
不是整条分支。我没有改代码、测试或别的文档，只写了这一份。写完即冻结。

材料：`bot-mode-retention-2026-10-01.md` §4.2、设计文档 §3.6.1 / §3.6.2 / §7.1、相关源码，以及那次试运行的原始日志
（`<retention>/flush/messages.jsonl`，256 行，今天还在上次会话的 scratchpad 里）。下文的「行 N」是这个文件从 1 起的行号，
等于保留率报告里的序号加 1（报告的 #95 是行 96）。摘要调用的 prompt 没有落盘，凡是说「摘要看到了什么」的地方，
都是**按代码推定**，并注明了钉住它的测试。

同目录下有一份未跟踪的 `bot-mode-handoff-opus.md`（同一题目的另一份评审）。我没有打开它；一次 grep 的输出带出了它的
几行，其中也提到「saved 1 line」。本报告在那之前已经从 `run.rs:255-262` 与日志行 92–96 得出了同一点。

## 0. 结论

- **诊断部分成立**。事实链是对的（flush 删了一行，摘要 #1 一条状态没写，S9 丢了），「摘要看到的是删除之后的记忆」
  也是对的。但「摘要拿到快照而不是 diff」**不是 S9 丢失的原因**：S9 从没进过记忆，也不在被删的那一行里，
  给摘要一份 diff 救不了它。
- 真正起作用的是两件事：摘要调用**以对话里的旧写入记录为准**，没有去看给它的记忆段；iota 自己又把这次删除
  **报成了「saved 1 line」**，并因此选了「do not repeat」那一版措辞。后者是确定性的代码缺陷，读代码就能证实。
- **PM 的 1 + 2 不建议做**：1 单独做是安慰剂，2 的前半句已是现状、后半句同 1。**4 不建议做**：成本估低了，
  保证估高了，而且它要算的 diff 已经存在。
- **我的选择**：现在只修计数这一处缺陷（remove 不算「saved」），提示词一个字不改；把 §7.1 那一条的机制描述改对；
  其余先记录。以后真要结构性手段，用「把保留区里的 `memory:` notice 附给摘要」（§2 的 5b），不用方案 4。

## 1. 诊断对不对

**结论**：事实对，归因要改。

### 1.1 代码里的事实

| # | 事实 | 依据 |
|---|---|---|
| F1 | 摘要调用拿到的记忆是压缩那一刻磁盘上的文件，也就是 **flush 之后**的 | `compact.rs:312` 调 `memory.current()`；`snapshot.rs:97-114` 说明它绕过冻结副本重新读盘；集成测试 `the_flush_turn_saves_memory_then_the_users_last_turn_survives_the_compaction`（`tests/repl/bot_flush.rs:302-315`）断言记忆段里有 flush 刚写的那行 |
| F2 | flush 轮整个落在**保留区**，同一次压缩的摘要看不到它 | `retain_tail_count`（`compact.rs:102-107`）锚在最后一条非 notice 的 user 消息；flush 轮不接受 steering（`run.rs:1005`），里面不会有这样的消息；`a_bot_keeps_the_users_last_turn_not_the_flush`（`compact.rs:775`） |
| F3 | 删除的 notice 带**整行原文**，不截断，进历史并落盘 | `memory.rs:533-538`（`-1 line`，`shown = was`）、`:579`、`:670`；`run.rs:255-276`。replace 的 notice 同样带 `(was: 旧行)`（`memory.rs:531`） |
| F4 | flush 的「写入数」是 notice 的条数，**remove 也算一条** | `run.rs:259-262`（`n = written.len()`，注释写着「each write is one line」）；remove 成功后照样 push（`memory.rs:670`；`src/tool/builtins/memory/tests.rs:120-122` 里 add、replace、remove 共 3 条） |
| F5 | 写入数 ≥ 1 时，addendum 用「do not repeat」那一版，再加一句「saved N line(s)」；为 0 时用「Nothing was saved … keep durable facts in the summary」 | `compact.rs:77-87` |

F1 说明 PM 任务书第 2 点（摘要看到的是删除**之后**的记忆）是对的。保留率报告 §4.2 第 2 点和设计文档 §7.1 最后一条写的是
「删除**之前**的记忆块」，与代码不符。

### 1.2 日志里实际发生了什么

1. 行 6：用户给出 20 条事实。行 7–16：模型调了 9 次 `remember add`，状态类并成一行 Live state（没有 S9）。
   行 17：助手说「I've saved these」。行 18–26：9 条 `+1 line` notice。「Kestrel」在整个日志里只出现在行 6。
2. 行 91–95：第 1 次 flush 轮。行 92 的推理原文：「instructions say don't save transient state since the summary keeps
   it … Let me remove the Open threads line to comply」。它调了一次 `remember remove`。行 95 是 `-1 line` notice，带整行原文。
3. 行 96：摘要 #1（`compacted_through: 87`，保留区从行 89 起，行 91–95 全在保留区）。按 F1、F4、F5 推定，它的 prompt 是：
   - addendum =「…do not repeat them. … **The memory flush just before this compaction saved 1 line.**」；
   - 记忆段里 `## Open threads` **是空的**；
   - 对话中段（行 2–88）里有 add 调用、列出 Live state 行的工具结果、`+1 line` notice、助手的「I've saved these」；
   - 删除本身（行 92–95）不在它的输入里。

   摘要 #1 的原文：「The assistant saved these into MEMORY.md via the remember tool (User, Project: work, and Open threads
   sections) … The live-state details live in memory and need not be restated」。它说的「在记忆里」来自**对话记录**，
   与给它的记忆段相矛盾。
4. 行 171：摘要 #2 看到了行 95 的 notice（这时它在中段），把 9 条状态抄了回来，并写明「removed from MEMORY.md」。
5. 行 243–254，**同一次运行里的第二次同类误判**：行 243 在普通轮里 add 了一行 Filler 记录（行 246 是 notice，都在中段）；
   行 248 在下一轮里把它删了（行 251 是 notice，在保留区）；行 252–253 的 flush 没有写入。摘要 #3（行 254）写的是
   「the filler exercise is now recorded in long-term memory … Open threads: the filler-exercise line described above
   (newly added)」。按 F1，给它的记忆段里没有这一行。这一次没有 flush 删除，addendum 是 `NOTHING_SAVED` 那一版。

### 1.3 归因

- **S9 的丢失不能用「删除让内容从记忆里消失」解释。** S9 不在被删的那行里，也从没进过记忆。它在摘要 #1 的输入里
  只有一处来源（行 6 的用户原话），摘要 #1 把整类状态让给了「记忆」，于是它丢了。
- **删除对 S9 的因果通路只有计数这一条**（推测，依据如下）：删除 → 写入数 = 1 → addendum 走「do not repeat」+
  「saved 1 line」。如果删除不计数，写入数是 0，prompt 就是 `NOTHING_SAVED` 那一版，也就是 noflush 组摘要 #46 拿到的
  那一版。同一个模型在那一版下保住了 10 条状态里的 9 条，包括 S9，尽管其中 5 条当时也在记忆里
  （保留率报告 §2、§3）。这是单次对照，只能算旁证。
- **更一般的问题是「摘要以对话记录为准，不以记忆段为准」**，加上保留区里的记忆改动它看不见（F2）。第 5 步说明它不需要
  flush 删除，也不需要「do not repeat」那一版 addendum 就会发生。所以「任何一次 flush 移除一行都会打开这个口子」范围划小了：
  保留区里的任何记忆改动都会，普通轮里的也算。
- **「其余 9 条靠侥幸」只对一半。** `-1 line` notice 带整行原文（F3）并落在保留区（F2），这两点是结构决定的，
  不是碰巧；从压缩 N 到压缩 N+1，模型一直能在上下文里看到被删的原文，摘要 N+1 的中段里也一定有它。
  不由机制保证的只有一步：摘要 N+1 是否把它写进去。这与摘要对任何事实的取舍是同一种不确定性。

别的解释，逐条核对：

| 解释 | 判断 |
|---|---|
| 模型漏存 | **成立，是 S9 的第一个原因**：行 7 的 Live state 行没有 Kestrel。它本来就是状态，不进记忆并不算错，所以要靠摘要 |
| `remember` 的 remove 语义 | 语义本身没错。两处措辞把删除说成了保存：计数（F4）是缺陷；工具结果对 remove 也写「saved to MEMORY.md …」（`memory.rs:562-566`，行 93），摘要 #2 因此写了「The tool result confirmed it was saved」。后者只是别扭，不必改 |
| 保留区的选择 | 没错，而且正是它让被删的原文活到下一次压缩（F2 + F3）。副作用是摘要 N 看不见 flush N |
| `-1 line` notice 的形态 | 够用：整行原文、落盘、进历史。它被渲染成 `User: memory: …` 行（`compact.rs:229-233`），摘要 #1、#2 都把它读成「the user echoed back …」。归属读错了，内容没丢 |
| flush 提示诱发了删除 | **成立**，行 92 的推理原文可证（`FLUSH_NOTICE`，`bot.rs:32`）。但按 §3.6.2 的分工，状态不留在记忆里并不算错；错在接收的那一侧 |

**失败场景**（按这个归因，下面这些都会重现，与模型是否自觉无关的只有第一条）：

- 一次只删不存的 flush：摘要被告知「saved N lines」「do not repeat」，而记忆里并没有新东西。
- 被删的行是**更早的窗口**里存的（长寿 bot 的常态）：摘要 N 的中段里没有它，记忆段里也没有，唯一的原文在保留区的 notice 里；
  摘要 N+1 不抄，它就没了。
- 普通轮里的改动落在保留区（行 248 那种）：摘要对记忆内容的陈述是错的。这次无害，因为那行的内容摘要自己又写了一遍。

**建议**：把 §7.1 那一条的机制改成上面的版本（摘要看到的是删除之后；误判来自对话里的旧记录和「saved 1 line」；
被删原文进保留区是结构性的）。保留率报告已冻结，不改，在 §7.1 里指明它 §4.2 第 2 点的出入即可。

## 2. 四种修法逐条评，以及第五种

| 方案 | 能否闭合 | PM 估的成本与保证 | 判断 |
|---|---|---|---|
| 1 把差分交给摘要（一句提示词） | **单独做不能**。摘要调用看不到 flush 删了什么：flush 轮在保留区（F2），记忆段是删后的快照（F1）。只有那行恰好是在本窗口中段里 add 的，模型才可能拿对话旧记录反推；更早存下的行，它没有任何来源 | 成本对（一句话，可钉住）。「依赖模型自觉」说轻了：它连材料都没有 | 安慰剂。要生效必须同时把数据给它，那就是 4 或 5b |
| 2 弱化「别重复」 | **基本不能**。前半句是现状：`compact.rs:65` 写的就是「already in the LONG-TERM MEMORY section below」。后半句「被移除的必须补回」与 1 一样没有材料。这次的失败不是模型不知道「段里没有的要写」，而是它没去看段 | 成本对。「侵蚀让摘要短」的担心反而偏大，影响很小 | 不做。真要动这句，有效的方向是「记忆以这一段为准，对话里的 remember 记录可能已被改掉」，见 §3 |
| 3 让 flush 别 remove | 能挡住这次的诱因，挡不住这一类：行 248 的删除发生在普通轮；软阈值的合并要求明确叫模型 remove（`memory.rs:488`），两句话会打架 | 评价基本对 | 不做。按 §3.6.2，flush 把状态行清出记忆并不算错 |
| 4 iota 算差分（读两次文件） | 能把 diff **送进 prompt**，不能保证它**进摘要**，写不写仍是模型的事。要真保证，只能由 iota 逐字拼进摘要，那会把有意删掉的行（过时的、写错的、合并掉的）复活 | **成本低估**：flush 前的记忆要在 notice 到达时读出并存在进程里，压缩失败重试或重启后就没了（`bot.rs:23-25`：notice 只活在那个进程里）；`MEMORY.md.prev` 是上一次写入的前像，不是 flush 的前像。**保证高估**：见左栏。**范围偏窄**：只管 flush 轮，管不到行 248 | 不做。它要算的东西已经存在（F3 的 notice） |

第五种，按我认为的优先顺序：

- **5a 只修计数**（推荐，现在做）：remove 不算「saved」。只删不存的 flush 于是走 `NOTHING_SAVED`，iota 不再对摘要说假话。
  它不闭合整类问题（§1.2 第 5 步就是在 `NOTHING_SAVED` 下发生的），但它是这次丢失里唯一确定、可测、与模型无关的一环。
  代价：`NOTHING_SAVED` 那一版会让摘要把记忆里的长期事实再列一遍（摘要 #2、#3 各有一段「Durable facts already in
  MEMORY.md」）。这是 0 写入 flush 的现有行为，不是新增的。
- **5b 把保留区里的 `memory:` notice 附给摘要**（真要结构性手段时用它，现在不做）：`compact_history` 手里就有保留区
  （`compact.rs:150-175`），把其中的写入 notice 原样列进 prompt 的一个小段，说明「这些改动已经体现在上面的记忆段里」。
  不读文件，不加状态，不动状态机，重启后照样成立（notice 在日志里），而且覆盖普通轮的改动。保证的强度与 4 相同
  （送到，不保证写进去），成本更低，范围更全。
- **5c 结构化墓碑**：不做。墓碑已经有了，就是 `-1 line` notice。缺的不是结构，是时机（摘要 N 看不到，N+1 才看到），
  换一种格式不改变谁在什么时候看到它。
- **5d 给摘要两份记忆让它自己 diff**：不做。多至 8 KiB 输入，状态成本同 4，仍然依赖模型。
- **5e 完全不修、只写文档**：对提示词层面的改动，这是对的（见 §3）。对计数缺陷不对，它不是概率问题。

## 3. 证据够不够支撑动手

**结论**：分两类看。计数缺陷现在修；提示词措辞先记录。

- **计数缺陷不靠这次运行的统计**。它违背的是设计文档自己的话：§3.6.2 第 3 步写「本次 flush **写了** N 行」
  「**什么都没写**（`flush_writes == 0`）」；`FlushReport.writes` 的注释是「Lines the flush turn wrote to `MEMORY.md`」
  （`bot.rs:149`）。一次只删一行的 flush 被报成「saved 1 line」，读代码即可证实（F4、F5），一个测试就能钉住。
  这符合「最简实现」：不加机制，只让已有的一句话说真话。
- **提示词措辞（方案 1、2、3，以及「以段为准」）是行为类改动**。证据是一次运行、一个模型、一个窗口里的一条丢失和
  两次误判；被丢的是一条合成的、本来就短命的状态；19/20 对 20/20 在单次波动之内。`cargo test` 只能钉住文字，
  证明不了效果。现在改，等于拿一句没验证过的话换一句没出过别的问题的话。**先记录**。
- **方案 4 与 5b 是为将来预留的机制**。按项目规则不做，等这类丢失带着实际损失再出现一次。

**失败场景**（只记录不修计数的话）：下一次只删不存的 flush 照样告诉摘要「saved 1 line」，这句话会一直留在
设计文档与测试都以为正确的路径上。

## 4. 如果修：最小正确改动与验证

**改动**（提示词常量一个字不改，所以 `compact.rs` 与 `tests/repl/bot_flush.rs` 里钉住的断言全部不用动）：

1. flush 的写入数只数 add 与 replace。知道 edit 种类的地方是 `BotMemory::write`（`memory.rs:650-672`），
   计数在 `record_memory_writes`（`run.rs:255-263`），中间是 `WriteLog`（`memory.rs:585-598`）。做法由作者定，
   例如让 `WriteLog` 的每条记录带上它是不是 remove。**不要去嗅 notice 文本里的「-1 line」**：小节名和行文本都是
   模型写的，可能含这一串。notice 照旧每次写入一条，transcript 与日志不变。
2. 同步三处注释：`run.rs:252-254`（「each write is one line」）、`bot.rs:149`、`compact.rs:94`。
3. 文档：§3.6.2 第 3 步写明「N 只数 add / replace，只删不存按什么都没写处理」；§7.1 那一条按 §1 改正。

一次既存又删的 flush（add 2、remove 1）仍然报「saved 2 lines」。这是真话，被删的那行不在记忆段里，
「do not repeat」对它不适用，原文在保留区的 notice 里。不为它加东西。

**能进 `cargo test` 的断言**（`tests/repl/bot_flush.rs`，仿 `:278` 那个用例；初始记忆带一行 `old line`，
flush 轮只调一次 `remember remove`）：

- 摘要 prompt 含「Nothing was saved to long-term memory this time」，不含「saved 1 line」；
- 记忆段里没有 `old line`（再钉一次 F1）；
- 重新加载的视图里，保留区最后一条 notice 含 `-1 line: [user] old line`。这一条把「被删的原文留在保留区」
  从侥幸钉成不变量（F2 + F3），以后有人动保留规则或 notice 形态时会红；
- 另一个用例 add 一行再 remove 另一行：prompt 含「saved 1 line」。

**需要真模型的实验**（只在想动提示词措辞之前做；下面的步骤我没有跑过）：不要重跑整套 `bot-retention.sh`
（17 分钟、135 万 token，而且两组都触顶），只**重放摘要 #1 这一次调用**。把 `flush/messages.jsonl` 的行 2–88 按
`summarize` 的规则渲染成 prompt（用一个不提交的测试加记录型 provider 倒出来最省事），记忆段用 flush 之后的
`MEMORY.md`（就是收尾时那份，`## Open threads` 为空），只换 addendum：A = 现状（「saved 1 line」那一版），
B = `NOTHING_SAVED`（修计数后的结果），C = B 或 A 加一句「以记忆段为准」。每臂对 deepseek-flash 打 5 次以上
（每次约 14k 输入、2k 输出），数摘要里出现 Kestrel 和另外 9 个状态关键词的次数。B 明显好于 A，说明修计数就够了；
C 还明显好于 B，才值得加那一句。原始日志在临时目录里，会被清掉；要做这个实验，先把 `flush/messages.jsonl` 与
`MEMORY.md` 另存。

**如果以后还是决定改提示词**，被逐字钉住的位置在这里，要一并处理：

| 文字 | 常量 | 字面量断言 | 设计文档 |
|---|---|---|---|
| 「already in the LONG-TERM MEMORY section below … do not repeat them」 | `compact.rs:65` | `compact.rs:956`（反向断言） | `bot-mode.md:491` |
| 「saved N line(s)」 | `compact.rs:81`、`:84` | `compact.rs:915`、`:963`；`tests/repl/bot_flush.rs:306` | `bot-mode.md:491` |
| 「Nothing was saved to long-term memory this time …」 | `compact.rs:72` | `compact.rs:959`；`tests/repl/bot_flush.rs:378`、`:454`、`:1068` | `bot-mode.md:492` |
| 记忆段标题「already saved separately; do not repeat these」 | `compact.rs:276` | `compact.rs:920`、`:952`；`tests/repl/bot_flush.rs:310` | `bot-mode.md:481` |
| `FLUSH_NOTICE` | `bot.rs:32` | `tests/repl/bot_flush.rs:481`（整句副本） | `bot-mode.md:458`（已经落后一版，没有「or a secret or token」） |
