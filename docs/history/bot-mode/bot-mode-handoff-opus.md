# flush 与摘要交接缺口：四种修法的评审（Opus）

评审对象：PM 针对保留率试运行 §4.2（`bot-mode-retention-2026-10-01.md`）提出的四种修法及其推荐「先做 1 + 2，4 留作保证」。
代码基于 `bot-mode-v1` 的 `75e6c27`。本文只评审，不改代码。标「推测」的地方没有日志或代码可以核对。

## 结论先写

- **诊断部分成立**。「摘要拿到的是记忆的快照而不是 diff」这件事在代码里属实，但它不是唯一的原因，也解释不了 S9。
  代码里还有一处**与设计不符的缺陷**：flush 删除一行，也被当成「saved 1 line」告诉了摘要。这次运行里，第 1 次摘要的
  addendum 说的正是这句假话。
- **四种修法里，1、2、3 基本是安慰剂**：摘要根本看不到那次删除（删除发生在保留区里），提示词再怎么要求「把被移除的
  补回来」，它也没有可以照做的材料。4 是唯一给摘要提供材料的修法，但 PM 说它是「结构性保证」，这个说法过头了：
  材料到了，抄不抄仍然由模型决定。而且 4 救不回 S9。
- **建议**：现在只修那处缺陷，也就是删除不算「saved」。这是一行计数的修正，提示词一个字都不改，可以进 `cargo test`，
  而且它针对的正是这次运行里真实出现的错误前提。另外把剩余的风险写进设计文档。1、2、3 不做；4 等到第二次观察到
  同样的缺口再说。

## 1. 诊断对不对

### 1.1 已经核对属实的部分

| 事实 | 依据 |
|---|---|
| 摘要拿到的记忆是 flush **之后**、从磁盘现读的版本，不是冻结的快照 | `compact.rs:312` 调 `memory.current()`；`snapshot.rs:97-113` 的 `current()` 每次 `read_owned()` 现读文件，文档注释里写明「the flush turn's own writes are on disk but not in the copy」 |
| LONG-TERM MEMORY 段原样放这份正文，标题是「already saved separately; do not repeat these」 | `compact.rs:274-281` |
| flush 轮整个落在**保留区**，被摘要的中间段里没有它 | `retain_tail_count`（`compact.rs:102`）以最后一条**非 notice** 的用户消息为锚点；flush 轮在那条消息之后。报告 §3 的数字也对得上：compacted_through 87，flush 轮是 #90–94 |
| 删除产生的 notice 带着整行原文，是结构化的 | `memory.rs:533-537`（`"-1 line"`，`was` 是被删的整行）、`memory.rs:579`（`memory: {place} {verb}: {shown}`），测试 `memory/tests.rs:264` 钉住了这个形态 |

所以摘要 #95 实际看到的是：**删除之后的记忆**（里面没有 Live state 那一行），加上**删除之前的对话**（#6 里那次
`remember add` 的完整参数，`compact.rs:239-245` 把工具调用渲染成 `Assistant called tool …(…)`），而看不到删除本身。
「快照不是 diff」说的就是这个结构，这一点成立。

### 1.2 报告 §4.2 第 2 点与代码矛盾

报告写的是摘要「根据删除之前的视图写的：当时记忆块里还有这行」。按上面的代码，摘要拿到的是删除之后的版本，PM 背景里
的说法是对的，报告错了。推测报告混淆了两样东西：flush 轮里模型看到的 `<memory>` 覆盖层是冻结快照，确实还有那一行
（`snapshot.rs:1-7`，只在压缩成功后 reload）；摘要读的却是 `current()`。报告已经声明「写完即冻结」，建议在设计文档或
验收状态里引用本条更正，不去改原报告。

这个更正会改变解读：摘要并不是被一份过期的记忆骗了。它手上的材料其实足以发现状态不在记忆里（对话里有 add，记忆里
没有这一行），但它仍然写下了「The live-state details live in memory and need not be restated」。这是模型判断出了错，
而且有人推了它一把，见 1.3。

### 1.3 漏掉的原因：删除被当成「saved」

- 每一次 `remember` 写入，包括 remove，都会把一条 notice 推进 `WriteLog`（`memory.rs:670`）；
- `record_memory_writes` 返回的是 notice 的**条数**（`run.rs:255-262`，注释写的是「each write is one line」）；
- 这个数作为 `flush_writes` 传给 `bot_summary_addendum`（`compact.rs:76-86`）：只要大于 0，就选「已有记忆，不要重复」
  那一版，并加上 **「The memory flush just before this compaction saved 1 line.」**

第 1 次 flush 只做了一次 remove（报告 §3 的时间线第 2 点），所以摘要 #95 收到的 addendum 告诉它「flush 刚**存了**一行」，
再加上「do not repeat」，等于暗示「状态已经存好了」。设计 §3.6.2 第 3 点的原意是「本次 flush **写了** N 行」，
`flush_writes == 0` 时应该走 `BOT_SUMMARY_NOTHING_SAVED`（「keep durable facts in the summary」）。如果删除不计数，这次的
addendum 本应是后者。这是**代码与设计不符**，不是推测；它在这次运行里实际生效了，这一点也有日志可查（报告 §3）。

### 1.4 S9 不是这个缺口造成的

S9（Kestrel）**从来没进过记忆**（报告 §3 第 1 点：Live state 那一行里没有它）。即使摘要拿到了完美的 diff（修法 4），
diff 里也只有 Live state 那一行，S9 依然不在其中。S9 之所以丢，是因为摘要 #95 把**整个「状态」类别**都推给了记忆，
不是因为漏掉了哪一行。能救 S9 的只有一件事：摘要不再因为「记忆里有」而整类省略状态。1.3 的错误前提是推动这个整类判断
的因素之一（推测：只是之一，因为 `do not repeat these` 这个段标题本身也在推）。

### 1.5 「其余 9 条靠侥幸」说过头了

flush 轮总是压缩前的最后一轮，所以它，连同删除的 notice，**必然**落在本次压缩的保留区（1.1 第 3 行），也**必然**进入
下一次压缩的中间段。所以这 9 条能留下来，「被抄进下一次摘要的输入」这一步是结构保证的；只有「摘要决定抄」这一步取决于
模型，而任何摘要内容都有同样的依赖。真正暴露的窗口是：**从第 N 次压缩到第 N+1 次之间，状态只存在于保留区里，第 N 次
摘要里没有**。这段时间模型仍然看得到保留区，所以对话本身不受影响。风险在第 N+1 次摘要：它如果也省略，就真的丢了。

### 1.6 其他候选解释

| 候选 | 判断 |
|---|---|
| 模型漏存 | 只能解释 S9 不在记忆里（1.4），解释不了另外 9 条为什么在 #95 里缺席 |
| `remember` 的 remove 语义 | 没有问题：删一整行、留备份 `.prev`、notice 里带原文（`memory.rs:533-537`、`:656-663`）。问题只在于它被计成了「saved」（1.3） |
| 保留区的选择 | 不是原因，反而是它救回了那 9 条（1.5） |
| `-1 line` notice 的形态 | 已经是结构化的墓碑（区、动作、原文）。缺的不是形态，是它到得晚了一次压缩 |
| flush 提示鼓励删除 | 推测是诱因：`FLUSH_NOTICE`（`bot.rs:32`）说的是「Do not **save** transient state」，模型把它扩大成了「删掉已经存了的状态」。这只有一次观察 |

## 2. 四种修法逐条评

| # | 能否闭合缺口 | PM 估的成本与保证对不对 | 失败场景 |
|---|---|---|---|
| 1 把差分交给摘要 | **不能**。被移除的行不在摘要的输入里：删除在保留区，记忆是删除后的版本（1.1）。要求摘要「补回被移除的行」，它没有材料可以照做，只能自己从「对话里有 add、记忆里没有」去推，而这次它恰恰没推出来 | 成本对，保证比 PM 估的还弱：不是「依赖自觉」，是「没有材料」 | 和这次一模一样：摘要仍然认为状态在记忆里 |
| 2 弱化「别重复」 | **基本不能**。现在的措辞已经是「already in the LONG-TERM MEMORY section below」，意思本来就是「仍在」。加上「被移除的必须补回」，问题和 1 一样，没有材料 | 成本对；对「让摘要短」的侵蚀其实很小，真正的问题是无效 | 同 1 |
| 3 让 flush 别 remove | 能减少触发，但**有害**：同一条 flush notice 在超过软阈值时还会附上合并要求（`bot.rs:45-50`，§3.5），合并本来就需要删除和改写。两句话互相矛盾 | 成本对，PM 对它的判断也对 | 记忆接近 6 KiB 时，flush 轮被要求合并、又被要求别删，结果不可预期；记忆持续膨胀 |
| 4 由 iota 算差分 | **部分能**：把被移除的行交到摘要手里，补上的正是 1、2 缺的材料。但它不是「结构性保证」：抄不抄仍然由模型决定。而且**救不回 S9**（1.4） | 成本比 PM 估的还低：不需要读两次文件，flush 轮的删除 notice 已经带着整行原文（`memory.rs:537`），`record_memory_writes` 本来就拿得到 | 模型拿到「已移除：…」后，仍然判断它是「transient」而不写；S9 这类从没存过的状态仍然被整类省略 |

### 第五种：让 addendum 说真话（推荐，现在就做）

remove 不计入 `flush_writes`，只有 add 和 replace 才算「saved」。

- **它修的是一个已经核对过的缺陷**（1.3），不是猜测出来的风险，而且它在这次运行里实际生效了。
- **效果**：一次只删不写的 flush（这次就是）会走 `BOT_SUMMARY_NOTHING_SAVED`，也就是「Nothing was saved … keep durable
  facts in the summary」。这句话推动的方向，正好与这次的错误判断相反。连带着，它也是唯一可能救下 S9 的修法（推测：
  救不救得下，取决于模型是否照着保留整类状态）。
- **提示词不改**，所以 `compact.rs` 里逐字钉住提示词的断言一条都不用动。
- **不能覆盖的情况**：一次既存又删的 flush（例如 add 2 行、remove 1 行）仍然会说「saved 2 lines」，被删的那一行照样
  看不见。这就是剩下的风险，交给文档（第 3 节），等再次观察到时再考虑修法 4。

其他几种，我**不建议**：

- 把 notice 改成更结构化的墓碑：现有形态已经足够（1.6）；
- 让摘要自己 diff 两份记忆：就是修法 4 换了个承担者，成本更高，可靠性更低。

## 3. 证据够不够

- 支撑「修法 1–4」的证据**不够**：只有一次运行、一个模型、一个窗口、一条观察。而且第 1 节表明，这条观察的主因并不是
  「缺 diff」，四种修法里有三种针对的机制在日志里并不存在（摘要从来没有收到过被删的行）。
- 支撑「第五种」的证据**够了**：它不需要真模型的证据。代码与设计 §3.6.2 第 3 点不一致，单元测试就能证明这个缺陷；这次
  运行只是说明它不是纸面上的问题。按项目规则，它是对现有行为的修正，不是为未来预留的复杂度。
- **先记录的部分**：§3.6.2 写上剩余的风险（1.5 的窗口、既存又删的情况、S9 这类整类省略），注明一次观察、来源和本文。
  第二次观察到「flush 删除的行在下一次摘要里也丢了」时，再上修法 4。

## 4. 最小正确改动与验证

### 4.1 改动（建议，未实施）

1. **计数只算 add 与 replace**。最小的做法是让 `WriteLog` 记下每条 notice 是否属于删除，例如 `BotMemory::write`
   （`memory.rs:670`）推入 `(notice, saved: bool)`，`saved = !matches!(edit, Edit::Remove { .. })`；
   `record_memory_writes`（`run.rs:255-262`）照旧把全部 notice 记进历史，但返回值只数 `saved`。不要靠解析 notice 里的
   `"-1 line"` 字符串去判断。`run.rs:252-254` 的文档注释（「each write is one line」）一并改正。
   - 推测需要核对：`after_bot_turn`（`run.rs:973`）和 flush 状态机（`bot.rs:63-70`、`:150`）里的 `writes`，除了
     addendum 之外还有没有别的读者（例如 compaction 标记、transcript 文案）。如果有，就看那里要的是「改动数」还是
     「存了几行」，分开处理，不要一刀切。
2. **设计文档 §3.6.2 第 3 点**：「本次 flush 写了 N 行」改为「新增或改写了 N 行（删除不计：被删的行不在记忆里，不能
   作为省略的理由）」，并补一段「已知缺口」（第 3 节的内容），同时引用 1.2 对保留率报告 §4.2 第 2 点的更正。
3. `FLUSH_NOTICE`、`BOT_SUMMARY_ADDENDUM`、`BOT_SUMMARY_NOTHING_SAVED` **都不改**。

### 4.2 验证

- **能进 `cargo test` 的**：
  - 在 `memory/tests.rs` 里，对同一个 `BotMemory` 依次 add、replace、remove，断言 `take()` 出来的是三条 notice，其中
    `saved` 为 true 的是两条。
  - 在 `compact.rs` 的测试里沿用 `nothing_saved_keeps_durable_facts_in_the_summary` 的写法：`flush_writes` 由一次
    只删除的 flush 得出时为 0，prompt 以 `BOT_SUMMARY_NOTHING_SAVED` 开头。如果计数在 `run.rs` 里，不方便直接构造，
    就把「notice 列表 → saved 数」抽成一个纯函数来测。
  - 现有的 `a_bots_summary_pass_sees_the_memory_and_the_flush_writes`（`compact.rs:900`）和
    `nothing_saved_keeps_durable_facts_in_the_summary`（`:934`）应该原样通过，提示词断言不需要改。
- **需要真模型的**：用同样的条件重跑 `scripts/bot-retention.sh` 的 flush 组（deepseek-flash，32k），看第 1 次摘要里
  有没有状态类事实。这次的触发条件（事实轮自发存了 Live state、flush 轮把它删掉）未必会复现，所以要么跑 ≥ 3 次看
  有没有出现「只删不写」的 flush，要么在 grades 之外直接读 `messages.jsonl` 里每次 flush 的 remember 动作和随后那次
  摘要的内容。单次运行的分数（19/20 对 20/20）**不能**拿来判定这项修正是否有效。
