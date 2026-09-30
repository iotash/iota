# bot 模式分支的过度设计评审（Opus）

对象：`bot-mode-v1`，`git diff main...HEAD`（23 个 commit，至 `05ff608`）。评审人：Claude Opus 5.5，2026-10-01。只读评审：没改代码，没跑测试，没 commit。

判据：项目自己的七条规则（不留兼容、最简实现、分层生长、模块化、复用依赖、不做权宜方案、先看成熟产品）。已拍板的不再讨论：`mode` 枚举、OKF 核心、删掉密钥拒写、形态 A。

分类用三档：**过度设计**（为以后而写，或者花的成本明显超过换来的保证）、**必要复杂度**（安全、正确性、数据完整性需要，而且已经接近最简）、**存疑**（有更简单的写法，但代价要实测或要用户拍板）。标「推测」的是我没验证过的。

---

## 0. 总判断

**有过度设计，但不在大家最先怀疑的地方。** 产品代码的主体（状态机、SessionError 的多数变体、截回、`repair_tail`、写侧截断、`seed_resumed`）大多是必要复杂度：它们几乎每一条都对应一个评审或长跑真正复现出来的故障，而且写得比较克制。过度设计集中在三处：

1. **长跑测试的 6b「重启等价」判定**：约 400 行判定逻辑，外加 3 个专门测这个判定的元测试。它测的是「测试怎么给模型的不同回答找理由」，换来的保证 6a 基本已经给了。
2. **`bot.json` 的 `materialized` 标志和它牵出的一整条链**：为了支持「bundle 懒创建」这一件事，加了一个状态位、一个 writer 回调、一种 `Fresh { never_saved }` 分支、一条提示文案，以及修正过期标志的逻辑。bot 的 bundle 直接在第一次启动时创建，这一整类状态就都不存在了。
3. **`scripts/bot-retention.sh`**：409 行，没跑过，里面还有一个依赖 L2 `recall` 的实验组，而 `recall` 不存在。它就是「为以后预留」的样板，同时又是 #13 那些「临时值」定稿的唯一依据，但一直没兑现。

文档负担是第四个问题：bot 模式相关的 `docs/design` 共 348 KB，其中 5 份评审/验收报告 127 KB。它不影响代码，但会让设计文档（110 KB）很难当现状规格来读（§3）。

分支新增约 6.1k 行产品代码、6.7k 行测试和脚本、1.2k 行文档（`git diff --numstat` 粗分）。测试和代码接近 1:1，这个比例对「本体永不重来」的会话来说不算离谱。多出来的部分主要是长跑判定和重复覆盖。

---

## 1. 代码层面

### 1.1 `repl::bot` 纯状态机（`src/repl/bot.rs`，475 行，其中约 220 行是单测）

- **结论：留。必要复杂度。** 但它存在的理由应该改写：不是「为 L3 预留」，而是「这些状态本来就要有」。
- **依据**：`Flush` 只有两个字段，`phase`（Idle/Queued/Flushed）和 `failures`（`src/repl/bot.rs:157-161`）。`step` 是一个 70 行的 match（`:165-239`）。执行在 `bot_perform` 里（`src/repl/run.rs:1159-1215`），约 60 行。
- **它换来了什么**：flush 欠账、压缩连续失败计数、「用户抢在 notice 前面发话」这三种状态本来就要存在。设计 #20 的时序（评审 S2）无论写在哪里都要有这些状态。写成纯函数以后，8 个单测不用终端就能覆盖所有转移（`:288-474`）。
- **更简单的替代**：设计 #23 的选项 c，直接写在 `repl::run` 里。
- **替代的代价**：状态量一样多，只是散到 `Repl` 的字段和 `if` 里，转移只能用端到端测试覆盖（`tests/repl/bot_flush.rs` 那一类，每个都要 50–80 行脚本）。状态机方案并不比它复杂。所以「为 L3 预留」这个动机就算去掉，结论也一样。
- **值得简化的一点（存疑）**：状态机里最繁琐的是 notice 进出输入队列这一套：`QueueFlush`、`Requeue`、`DropNotice`、`NoticeArrived`、`is_flush_notice` 的前缀识别（`:52-55`）、`held` 回放（`run.rs:1183-1187`），加上 `TurnCtx.steering: false`。这些都是「flush 走 UI 输入队列」带来的。另一种写法是：一轮结束后如果超过阈值，由 loop **直接**跑 flush 轮；此时输入队列非空（用户已经打字抢先）就改成不 flush 直接压缩。这样可以去掉 Requeue、DropNotice、NoticeArrived 三个转移和前缀识别，`Resumed` 也可以变成「启动时超阈值就直接 flush」。代价：需要一个「输入队列是否有待发内容」的探针。Ui facade 有没有现成的，我没核实（推测要加一个方法）。另外 flush 轮本来就关了 steering，所以「flush 期间用户打字」在两种写法下的行为一样。**这条不进前三**：现在的写法是对的，也有测试，重写会冒回归风险。

### 1.2 `SessionError` 的新变体（`src/session/error.rs:31-126`）

逐个判：

| 变体 | 判 | 依据与理由 |
|---|---|---|
| `Locked` | **留**（必要） | X-60：两个进程写同一个 bundle，今天就会交错写坏日志。一个变体，一句话。 |
| `BotRunning` | **简化**（轻度过度） | `Locked` 已经带了 `what: String`（`:35-40`）。`lock_bot` 传 `what = "bot coder"` 就够了（`src/session/lock.rs:56-62`）。现在的注释（`:42-43`）说单开一个变体是为了措辞不同：「is already running」对比「is open in another iota process」。换来的只是一句更顺的文案。代价：删掉一个变体和一条 Display 测试，文案变成 `bot coder is open in another iota process (pid N)`，意思照样清楚。 |
| `BotOwned` | **留**（必要） | 本体保护（§2.7，评审 I1）：普通 `resume` 或 picker 删掉 bot 的本体是不可逆的。 |
| `BotMissing` | **留**（必要） | 本体物化以后又不见了，是硬错误，不能静默重来（评审 I1）。文案给了两条出路，有用。 |
| `BotOwnerUnknown` | **存疑** | 只要有一个 `bot.json` 读不出来，**所有**会话的 resume 和删除都会被拒绝（`:81-94`，`src/session/store.rs:144-190`）。换来的是「判断不了归属时绝不误删本体」。更简单的写法是：把读不出的 pointer 当成「那个 bot 坏了」，只让 `iota run <那个 bot>` 报错（`BotPointer::read` 本来就会报，`src/session/bot.rs:41-53`），其他会话照常。代价：在「pointer 损坏」与「用户在 picker 里恰好删掉那个 bot 的本体」同时发生时会丢数据。这个窗口很窄，但结果不可逆。评审 R6 专门要求了 fail-closed，所以我不判它过度，只指出它的影响面（一个坏文件锁死全部会话）比它防的事故大。可以折中成：只拒绝删除，不拒绝 resume。 |
| `MetaNotSaved` | **留**（必要） | `persist_turn` 会重试 backlog（`run.rs` 的 `persist_turn`）。没有这个区分，「日志写成功、meta 写失败」就会把整批重复追加一次。这是数据完整性。 |
| `CutFailed` + `LogNotCutBack` | **留，可合并成一个**（必要，轻度冗余） | 见 1.3。两个变体只差「本次」和「上次」。合成一个 `LogNotCutBack { write: Option<…>, cut }`，或者只留后者，调用方的处理完全一样（都是不再写）。 |
| `LockUnsupported` | **留**（必要） | 截回依赖独占写（设计 §7.1、评审 codex N1）。无锁时降级，会截掉另一个进程已经确认的数据。一个变体，fail-closed，不另做锁实现，已经是最简。 |
| `LogNotOpen` | **留** | 表示 bug 的哨兵，零成本。 |

### 1.3 批次全有或全无、写侧截回、「截回失败挡住所有写入」（`src/session/writer.rs:217-414`）

- **结论：留。必要复杂度。**
- **依据**：`append_messages` 失败时 `settle_batch` 截回到批次开头（`:380-396`）。截回失败时记下 `uncut`，之后每次写之前先重试截回（`:368-377`）。
- **它换来了什么**：`persist_turn` 的语义是「失败不推进水位，下次把 backlog 一起写」。没有截回的话，一个写了一半的批次在重试时会留在日志中间，loader 读到的就是重复消息，或者孤儿 `tool_calls`，本体从此坏掉（X-61）。`uncut` 也只多了一个 `Option<u64>` 和 4 行重试。
- **更简单的替代**：截回失败时直接把 writer 置为「永久停写」，不再重试。
- **替代的代价**：代码量几乎一样（一个 bool 对一个 `Option<u64>`），而且瞬时 I/O 错误之后没法自愈。现在的写法已经是最简。唯一可删的是 1.2 说的第二个错误变体。

### 1.4 `repair_tail`（`src/session/loader.rs:257-271`）与写侧截断 `fit_line`（`src/session/writer.rs:443-489`）

- **`repair_tail`：留。必要复杂度。** 15 行。永不结束的会话一次断电留下孤儿 `tool_use`，就会被 API 永久拒绝（X-61），而且 bot 没有「另开一个会话」的退路。它只修尾部，不修中段，这个限制已经如实写进 §7.1。已经最简。
- **`fit_line` 截断：存疑，偏留。** 它防的是「一条记录 ≥ 32 MiB（`MAX_LOG_LINE`，`loader.rs:30`），reader 读不回来，bundle 永久不可读」。内置工具输出都封顶在 64 KiB（`CODE_MAX_OUTPUT`，`src/tool/builtins/code/mod.rs:28`），附件单独存放。能碰到 32 MiB 的只剩 MCP 工具输出、`raw` 回放载荷和超大粘贴。这是低概率事件，但后果不可逆，而且 bot 会一直活下去，概率会一直累积。更简单的替代是 reader 不设上限（`read_until`）；代价是 D-56 专门防的「恶意长行吃光内存」问题会回来，也破坏和 Go 对齐的上限语义。另一个替代是只保留「超长就拒写」的分支（`:476-481`）；代价是 `persist_turn` 会每轮重试这条写不进去的 backlog，会话从此存不了盘，比截断更糟。所以 40 行换一条不可逆故障的防线，**留**。

### 1.5 `materialized` 标志（`src/session/bot.rs:15-27`，`src/session/store.rs:436-505`）

- **结论：删（连同整条链）。过度设计。前三之一。**
- **依据**：pointer 先写、bundle 懒创建，所以需要区分「还没创建」和「创建过但丢了」。为了这个区分，出现了下面这些东西：
  - `BotPointer.materialized` 字段；
  - writer 的 `on_created` 回调和 `OnCreated` 类型（`writer.rs:39,146-151`），加上 `ensure_created` 里「失败重试回调」的逻辑（`:361-364`）；
  - `BotOpen::Fresh { never_saved }`，还有 `NEVER_SAVED` 提示（`src/cmd/interactive/bot.rs:35`）；
  - resume 时修正过期标志（`store.rs` 的 `if !ptr.materialized { … write }`）；
  - 对应的测试：`an_unsaved_pointer_starts_over_under_the_same_id`、`a_resume_fixes_a_stale_materialized_flag`，以及 `tests/session/store.rs`、`tests/repl/commands.rs` 里的相关断言。`grep` 在 8 个文件里命中 45 处。
- **它换来了什么**：「启动 bot 但一句话没说就退出」时，磁盘上不会多出一个空 bundle。
- **更简单的替代**：`open_bot` 在第一次启动时**先**物化 bundle（写 `meta.json` 和空日志，拿 bundle 锁），**然后**写 pointer。pointer 存在就意味着 bundle 存在，`NotFound` 永远是 `BotMissing`。writer 需要一个 5 行左右的 `materialize()` 公开方法（现在的 `ensure_created` 是私有的）。
- **替代的代价**：
  - 首次启动就会留下一个空 bundle。对 bot 来说这就是它永久的本体，迟早会有内容，不算垃圾。
  - 如果在「bundle 已建、pointer 未写」之间崩溃，会留下一个孤儿空 bundle。它是普通会话，可以在 picker 里删掉，无害。
  - 普通会话的懒创建语义不受影响。
  - 总体上删掉一个状态位、一个回调机制、一个分支、一条提示，以及至少 2 个测试。bot 锁仍然需要（两个进程同时首次启动的竞争）。
- 顺带一提：`BotPointer.v` 和 `BOT_POINTER_VERSION`（`bot.rs:15-22`）从来没有被读取或校验过。按「不留兼容」的规则，版本号字段就是为以后迁移预留的，**删**。

### 1.6 meter 的 `seed_resumed`（`src/repl/context/meter.rs:276-293`）

- **结论：留，但和 `update_kept`（`:242-259`）合并。必要复杂度，有重复。**
- **依据**：长跑的第一个发现（`db11a05`）：恢复后的 bot 只用本地 tokenizer 计数，漏掉了记忆块和工具定义，应该压缩的那一轮没有压。`seed_resumed` 修的是一个真实复现过的 bug。
- **它换来了什么**：重启前后的压缩时机一致。长跑的 6a、6b 在 32k 下能过，靠的就是它。
- **更简单的替代**：没有更简单的正确做法。但两个函数的后半段逐字相同，都是「找到最后一条带 usage 的消息，settled = 它，pending = 其后本地计数」。抽一个 `settle_on(at, usage, history)`，两个调用方各自只负责找到位置，省掉 15 行重复。另外 `seed_resumed` 只在 bot 下用（commit 说明写着 "normal sessions keep Go's count"）。普通会话恢复时同样会低估，只是后果轻一些；统一使用就能少一个按模式分支的地方（存疑：这会改变普通会话的行为，要记一条 X）。

### 1.7 `BotState.name` 的 `#[allow(dead_code)]`

- **结论：已经解决，无需处理。** 这个 allow 在 `cc889fe` 加入、`51cb10d` 移除（`git log -S`）。现在 `name` 在告警文案里被读到（`src/repl/run.rs:1207`）。HEAD 的 `src/` 里，本分支改过的文件没有新增 `dead_code`。

### 1.8 其他小项

- **`AgentMode::Bot` 的文档注释过期**（`src/config/agent.rs:78-80`）：还写着「Before T4 none of that exists」。T4 早就落地了。这是开发过程中的中间态残留，删掉那半句。（不算设计问题，顺手记录。）
- **`MEMORY.md` frontmatter 的 `bot:` 归属校验**（`src/agents/memory.rs` 的 `apply` 开头）：文件就在 `bots/<name>/` 下，归属已经由路径决定。这个校验防的是「人把另一个 bot 的记忆文件拷过来」。它换来一次 fail-closed，代价是多一条拒绝路径和一个测试。**存疑，偏删**。frontmatter 的 `bot` 键本身是已定的（#19），但「校验它」不在决定里。

---

## 2. 测试与脚本

### 2.1 `tests/repl/bot_longrun.rs`（1450 行）+ `src/testing/growing.rs`（571 行）

**判断：保留长跑本身；砍掉 6b 判定和第二个 32k 场景。前三之一。**

- **值得留的部分**：`GrowingProvider` 按请求计算用量，并且拒绝超窗请求（`growing.rs:1-17`）。这让「请求不超窗」成为一个真能失败的断言。长跑也确实抓到了两个真 bug（`68e1875` 的提交说明：重启丢失已排队的 flush、恢复后低估用量），`db11a05` 就是修它们的。不变量 0–5、6a、7 各自只有几十行，对应的是「一周对话」的真实风险，**留**。
- **6b「重启后与不重启发出同样的请求」是过度设计**：
  - 依据：它要在两次运行里逐个请求比对，但重启时会按设计重读记忆块（§3.4），fake 模型看到不同的块就可能给出不同回答，所以需要 `refresh_explains`（`bot_longrun.rs:440-546`）及其辅助函数（`:274-660`，约 380 行），逐类「豁免」这些差异：新写入的 notice、摘要请求里的记忆节、「saved N lines」计数……然后又需要 3 个元测试来证明这个豁免器不会放过真的错误（`:1167-1392` 的 `only_the_models_answer_to_the_reread_block_is_excused`、`a_memory_notice_no_new_write_made_is_not_excused`、`a_summary_request_that_lost_its_history_is_not_excused`）。`d0b87d9` 整个 commit 都在修这个判定器自己的漏检（验收报告 `bot-mode-verify-codex.md` §5 与「复核」节）。
  - 它换来了什么：重启不改变 loop 的后续行为。
  - 更简单的替代：只保留 6a（重启加载的视图和运行中进程持有的视图逐字节相同，`compare_loaded`，`:559-580`），加上「压缩标记数和 flush 记账在两边一致」。loop 是确定性的：视图相同、记忆文件相同（磁盘是同一份拷贝）、配置相同，后续行为就相同。剩下唯一的差异来源就是记忆块重读，而那是设计本身。如果还想要端到端的保证，可以让测试在重启点**不**触发重读，比如让记忆在两个分支里都保持 mtime 不变；或者只比到下一次重读之前的第一个请求。
  - 替代的代价：会失去对「重启后 loop 内存状态（snooze 水位、压缩欠账）丢失」的直接观测。但这两项已经作为已知残留写进 §4.1，6b 在 32k 下也观测不到它们（`bot_longrun.rs:1428-1440` 的注释说它们只在 8k 下出现）。换句话说，6b 现在守的主要是它自己的豁免逻辑。
- **两个 32k 场景重复**（`:1398` tidy、`:1417` 软阈值）：两者驱动同一台机器、同一套不变量，区别只在 fake 模型的记忆策略。软阈值那个场景覆盖得更多（记忆一直在 6 KiB 附近），留它就够了。每个场景约 25 秒（`bot-mode-verify-codex.md:143,307`），合并后 CI 省下一半时间。
- **`#[ignore]` 的 8k 场景**（`:1441-1449`）：它是 `BOT_MIN_WINDOW` 的「证据」。证据写在注释和设计文档里就够了；一个永远被忽略、永远会失败的测试函数没有人会去跑。**删**，在 `BOT_MIN_WINDOW` 的文档注释（`src/repl/context/tokens.rs:52-56`）里保留一句推导即可。代价：以后想调这个下限时，要把 8k 场景临时写回去（改一个常量就行）。

### 2.2 `tests/repl/bot_flush.rs`（1305 行，19 个用例）与 `src/repl/bot.rs` 单测（8 个）

- **结论：大体留，少量重复。必要复杂度。**
- 状态机单测覆盖转移，bot_flush 覆盖接线，两者分工正确。重复的是那些在集成层重新断言一遍转移的用例，比如 `a_message_ahead_of_the_notice_compacts_without_a_flush_and_the_notice_is_dropped`（`:355`）对应单测 `a_message_ahead_of_the_notice_compacts_without_a_flush`（`bot.rs:362`），`an_unchanged_compaction_snoozes_the_next_flush`（`:1159`）对应 `an_unchanged_compaction_snoozes`（`bot.rs:453`）。集成层只需要证明「动作被执行了」，每类动作一个用例就够。估计可以合并掉 3–5 个（推测，没有逐个比对断言）。
- `a_backlog_the_log_refused_is_saved_before_the_compaction_and_survives_a_restart`（`:760`）、`a_failed_meta_rewrite_does_not_double_the_turn`（`:828`）、`an_interrupted_batch_is_answered_before_the_memory_notice_follows_it`（`:896`）是数据完整性用例，**留**。

### 2.3 `tests/fixtures/mode/workspace-true.txt`（178 行）与 `mode_agent_is_what_workspace_true_was`（`tests/cmd/session.rs:713-740`）

- **结论：改名或删。轻度过度设计。**
- 它证明的是「`mode: agent` 和旧的 `workspace: true` 逐字节等价」，是一次性的迁移证明。迁移已经完成，`workspace:` 也已经是未知键（`src/config/strict.rs:325-330`）。以后它只是一份 Gemini 完整请求的 golden，任何 harness 或 prompt 措辞的改动都会让它失败，而它的名字却指向一个已经不存在的键。按「不留兼容」的规则：把它改成 agent 模式请求形状的 golden（改名，去掉 workspace 字样）；或者删掉，只保留 `mode: chat == 缺省` 那一半断言。

### 2.4 `scripts/bot-retention.sh`（409 行，没跑过）

- **结论：简化或删。过度设计。前三之一。**
- **依据**：
  - 三个组里的 `recall` 组「DEPENDS ON L2, which is not in v1」（`scripts/bot-retention.sh:19-20`），全文有 15 处 recall 相关代码。这是为不存在的功能写的实验，正好违反「不为未来预留」。
  - 脚本有十来个环境变量旋钮（`:37-50`）、污染检测，以及对 tmux 的完整驱动。
  - 它**从来没跑过**，却是 #13 所有「临时值」定稿的唯一依据（设计 §6 #13、七问第 7 条）。
  - 在评审轮里它已经改了一次，+96 行（`051227c`）。也就是说，一段从没执行过的代码在持续消耗维护成本。
- **它换来了什么**：一个「以后可以定稿常量」的承诺。
- **更简单的替代**（二选一）：
  - (a) 删掉 `recall` 组和它的旋钮，只留 `noflush` 对 `flush`，**合并前跑一次**，把结果写进 §6 #13，从此常量不再叫「临时值」；
  - (b) 删掉脚本，按七问第 7 条 (c) 的「廉价先验」做一次手工 `/compact` 实验，结论写进设计文档。以后要重新调常量时再写脚本。
- **替代的代价**：(a) 要花一次真模型的 token；(b) 失去可重复性。两者都比「一个永远不跑、却被当作定稿依据的 409 行脚本」诚实。

### 2.5 其余端到端用例（`tests/session/store.rs` +393 行 26 个、`tests/cmd/session.rs` +304 行 17 个、`tests/repl/commands.rs` +612 行）

- **结论：留。** 抽查下来，大多对应锁、本体保护、system 采纳、X-59、X-62、X-63 这些 DIVERGENCES 条目，是行为契约。跟着 1.5 删掉 `materialized` 链以后，相关用例会一起消失。

---

## 3. 文档与流程

| 文件 | 体量 | 判断 |
|---|---|---|
| `docs/DIVERGENCES.md` X-58…X-63 | 6 行，单行很长 | **资产**。这是项目既有的、和 Go 版对齐的机制。每条对应一个真实的行为差异，并且点名了钉住它的测试。单行 500+ 字符很难读，但这是整份文件的格式，不是本分支的问题。 |
| `bot-mode.md` | 110 KB，「已定」出现 39 次，「评审」70 次，另有 rev 历史、七问、§6 的 24 行决策大表 | **一半资产一半负担**。决策表的「选项/理由」对以后翻案有用，是资产。但正文里交织着「rev 1 曾经这样写、评审 X 指出、已定（日期）」这类来源标注，读者要做过滤才能拿到**现状**。分支合并以后，它应该收成一份现状规格：正文只写现在的行为，决策表留着，来源与修订史挪到文末一节，或者交给 git log。 |
| 5 份评审/验收报告（`bot-mode-review-*.md` ×4、`bot-mode-verify-codex.md`） | 127 KB | **合并后是负担**。它们的作用（驱动修复、留下验收证据）在合并那一刻就完成了。结论已经被吸收进设计文档（§7.1 已知限制）和 commit message。继续放在 `docs/design/` 里，会被 grep、被 agent 读、被误当成现行规格。建议合并前删掉，或者移到 `docs/archive/`。git 历史里永远能找回来。 |
| `bot-mode-recon.md` / `-research.md` / `-critique.md` | 111 KB | research 里「成熟产品怎么做」（Hermes、Claude Code、OpenClaw……）正是规则 7 要的输入，**留** research。recon 和 critique 已经被设计 rev 2 吸收，同上，归档。 |

对一个 0.x、单一操作者的项目，「多模型交叉评审、每轮留报告」这套流程本身是有价值的：本分支里至少 R1（截回失败）、N1（无锁降级）、长跑判定的两个漏检，都是评审抓出来的，而且都是数据完整性问题。**问题不在流程，在产物的归宿**：过程文档不该和规格文档放在同一个目录、同一种地位。

---

## 4. 配置面

所有数值都是**代码里的 `const`**，没有一个暴露成配置键（`src/agents/memory.rs:33-39`、`src/repl/context/tokens.rs:47-60`、`src/repl/bot.rs:41`、`src/cmd/interactive/bot.rs:23`）。所以「把『以后可能要调』变成了『现在就配』」这个担心**不成立**：用户侧的配置面只多了 `mode: bot` 一个枚举值。逐个看：

| 常量 | 判 | 理由 |
|---|---|---|
| `MEMORY_CAP` 8 KiB | 必要护栏 | 记忆块每次请求都要带，没有上限就是无界的 system 段。量级和 Hermes、Claude Code 对齐（§6 #13）。 |
| `MEMORY_SOFT_CAP` 6 KiB | 必要 | 触发「请整理」提示，否则只有撞硬上限才会整理。 |
| `MEMORY_LINE_CAP` 500 B | 必要，偏弱 | 防止一行塞进一篇文章。和 8 KiB 放在一起，拒写路径算是冗余，但它给模型的拒绝文案更具体，成本只有一个 `if`。 |
| `MEMORY_SECTION_CAP` 100 B | **轻度过度** | 小节名只有三种形态，其中 `Project: <目录名>` 的目录名受文件系统限制；控制字符已经单独拒绝（`memory.rs:91-96`）。这个上限防的是模型传一个超长 section 名，但那样的写入本来就会被 8 KiB 上限兜住。删掉它，一个测试随之消失，没有安全损失。（不值得单独开 commit，顺手删即可。） |
| `BOT_MIN_WINDOW` 32k | 必要 | 长跑证明了 8k 下会不停压缩、flush 轮超窗（`bot_longrun.rs:1428-1440`）。在启动时拒绝，比运行中的神秘失败好。 |
| `max(32k, 25%)` 封顶 50% 的 reserve | 必要，公式可以再简化（存疑） | flush 轮加上一次抢先发送的用户轮，都要落在阈值和窗口之间。「封顶一半」只在 32k–64k 的窗口上起作用（`meter.rs:112-118`，测试表 `a_bots_threshold_keeps_the_larger_reserve`）。既然已经有 `BOT_MIN_WINDOW`，可以直接用 `max(32k, 25%)` 并把下限提到 64k。代价是 32k–64k 的模型不能跑 bot。这是产品取舍，不是代码问题，保持现状即可。 |
| `RESUME_GAP_NOTICE_SECS` 1 h | 必要 | 否则每次重启都会往日志和上下文里加一行噪声。 |
| `FAILURES_BEFORE_ALARM` 2 | 必要 | 一次失败会在下次发送时重试，不打扰人。 |

---

## 5. 如果只能删或简化三样

1. **删掉长跑的 6b 判定和它的豁免器**（`bot_longrun.rs:274-660` 的大部分、`:1167-1392` 的 3 个元测试），同时把两个 32k 场景合成一个，删掉 `#[ignore]` 的 8k 场景。约 700 行测试代码。保证几乎不损失：6a 加上确定性的 loop 已经覆盖了「重启不改变行为」；6b 能额外观测的 loop 内存状态丢失，在 32k 下它自己也看不到，而且已经作为残留写进文档。
2. **删掉 `materialized` 一整条链，改成首次启动就物化 bundle**（`BotPointer.materialized` 和 `.v`、`SessionWriter::on_created`/`OnCreated`、`BotOpen::Fresh{never_saved}`、`NEVER_SAVED`、过期标志修正，以及对应测试）。去掉一种持久化状态和一个回调机制；代价只是一个空 bundle。
3. **砍掉 `bot-retention.sh` 的 `recall` 组，合并前跑一次**，然后把 #13 从「临时值」改成定稿；做不到就整个删掉，改用一次手工先验实验。不要让一个没跑过的脚本同时充当「为 L2 预留的代码」和「定稿依据」。

次一级的（不进前三，但顺手就能做）：

- 5 份评审/验收报告和 recon、critique 移出 `docs/design/`，设计文档收成现状规格；
- `BotRunning` 并进 `Locked`，`CutFailed`/`LogNotCutBack` 合并成一个；
- `seed_resumed` 和 `update_kept` 抽出共同的部分；
- `workspace-true.txt` 改名或删掉；
- `MEMORY_SECTION_CAP` 和 frontmatter `bot:` 校验删掉；
- `AgentMode::Bot` 的过期注释删掉。

## 6. 明确判为「必要复杂度」、不该动的

`Flush` 状态机本身；`Locked`/`BotOwned`/`BotMissing`/`MetaNotSaved`/`LockUnsupported`；批次截回与 `uncut`；`repair_tail`；`fit_line`；`seed_resumed`；两把锁（bundle 锁防双写，bot 锁防首次启动时铸出两个 pointer）；`BOT_MIN_WINDOW` 与 bot reserve；记忆的各个上限（`MEMORY_SECTION_CAP` 除外）。它们都有对应的真实故障（X-60/X-61、评审 R1/N1、长跑发现），而且实现已经接近最简。「为 L3 预留」只是状态机的**附带**理由，去掉这个理由，结论也不变。

## 7. 本评审没做的

- 没跑任何测试。长跑耗时引自 `bot-mode-verify-codex.md:143,296,307`。
- 1.1 里「Ui facade 有没有输入队列探针」没有核实（推测需要新增）。
- 2.2 的重复用例是按名字和结构判断的，没有逐条比对断言。
- `src/cmd/interactive/mod.rs`（+227 行）、`src/repl/commands/compact.rs`（+603 行）只看了和上面各项有交集的部分，没有全面评审。
