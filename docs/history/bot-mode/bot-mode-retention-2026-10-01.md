# bot 保留率试运行：2026-10-01，deepseek-flash，32k

`docs/design/bot-mode.md` §5.2 的 L1 验收项「保留率实验」第一次用真模型跑。脚本 `scripts/bot-retention.sh`，
代码在 `bot-mode-v1` 的 `d86b917`（脚本未改动）。写完即冻结。

结论先写：两组都合格（exit 0）。分数是 noflush **20/20**，flush **19/20**，两组都触到天花板，差距只有 1 条，
落在单次波动范围之内。**方向未定**，这次不能记成「flush 保留更多」。另外日志里看到一处 flush 与摘要之间的交接缺口
（见 §4.2），比分数更值得看。

## 1. 条件

| 项 | 值 |
|---|---|
| provider | `deepseek`（type `openresponses`，`https://api.deepseek.com`），key 从 `~/.iota.yml` 读出后放进 `OPENAI_API_KEY` 环境变量，没有打印，也没有写进任何文件 |
| 模型 | `deepseek-flash`；scratch 配置里只写了 `id` 与 `context_window`，没带 effort、temperature、top_p |
| 窗口 | `RETENTION_WINDOW=32000`（默认）。bot 的阈值是 `max(32000 − max(32k, 25%), 16000)` = **16000** |
| 其他 knob | 全用默认值：`RETENTION_COMPACTIONS=3`、`RETENTION_LEAD=2`、`RETENTION_COMPACT_EVERY=4`、`RETENTION_MAX_TURNS=120`、`RETENTION_TIMEOUT=300` |
| 隔离 | 脚本自带的 scratch HOME（`$RETENTION_OUT/<group>/home`），真实的 `~/.iota` 没有读写 |
| 构建 | `cargo build`（debug），`IOTA_BIN` 用默认的 `target/debug/iota` |
| 耗时 | 两组顺序跑，`real 1050.56` 秒（约 17.5 分钟） |

选模型的理由：用户配置里便宜的候选有 deepseek-flash 与 glm-5.3。开跑前在 scratch HOME 里各探测了一轮，让模型用
shell 工具执行 `echo hi`：两者都正确调用了工具，也都报了 usage。但 glm 走 anthropic 端点时 `total_tokens` 报的是 0
（in/out 正常，是已知现象），所以选了 deepseek-flash。探测时我自己的配置写错过一次（`sandbox: on` 不是合法值，
整个 toolset 被忽略），那一次模型把工具调用当正文吐了出来；改成 `sandbox: auto` 后正常。这次失败是我配置写错导致的，
与 provider 无关。

复现命令（key 的取法省略）：

```sh
export OPENAI_API_KEY=...            # ~/.iota.yml 里 providers.deepseek.key
RETENTION_OUT=<scratch>/retention RETENTION_TYPE=openresponses RETENTION_URL=https://api.deepseek.com \
  RETENTION_MODEL=deepseek-flash scripts/bot-retention.sh
```

`RETENTION_OUT` 在本次会话的 scratchpad
`/private/tmp/claude-501/-Users-joyqi--herdr-worktrees-iota-bot-mode-v1/63919487-0da6-4645-ab72-bc22e38a6a59/scratchpad/retention`
（临时目录，会被清掉；下面把关键内容抄了进来）。

## 2. 原始结果

`results.tsv`：

```
group    compactions  flush notices  MEMORY.md bytes  memory kind  state kind  total  note
noflush  3            0              1163             10/10        10/10       20/20
flush    3            3              745              10/10        9/10        19/20
```

退出码 0，两组都没有 INVALID、CONTAMINATED 或 FAILED。noflush 的 3 个压缩标记都带 `flush_skipped: true`
（来自手打的 `/compact`），日志里没有 flush notice。

`flush/grades.tsv`（noflush 的 20 条全部 ok，答案依次是 QX- / bramblewood / SQLite / Orzo / 2 pm / Periwinkle /
British English / 4417 requests per hour / Friday / Okafor / step 3 of the 7-step migration checklist /
test_ledger_rollover / fix/overflow-guard / 20931 / src/quartz.rs / 812 ms / 55-A / RETRY_JITTER=0 / room Kestrel / #1288）：

```
M1   memory ok    QX-
M2   memory ok    bramblewood
M3   memory ok    SQLite — chosen over Postgres because the cache must work without a network
M4   memory ok    Orzo
M5   memory ok    2 pm — mornings only
M6   memory ok    Periwinkle
M7   memory ok    British English
M8   memory ok    4417 requests per hour
M9   memory ok    Friday
M10  memory ok    Okafor (Tamsin Okafor)
S1   state  ok    Step 3 of 7 — from the earlier conversation summary, not long-term memory
S2   state  ok    test_ledger_rollover — summary only
S3   state  ok    fix/overflow-guard — summary only
S4   state  ok    20931 — summary only
S5   state  ok    src/quartz.rs — summary only
S6   state  ok    812 ms — summary only
S7   state  ok    55-A — summary only
S8   state  ok    RETRY_JITTER=0 — summary only
S9   state  miss  unknown
S10  state  ok    1288 — summary only
```

flush 组的 quiz 回复末尾，模型自己附了一句：「11–18 and 20 are transient work state that I deliberately did not save
to MEMORY.md … 19 I've never seen.」

收尾时的 `MEMORY.md`：

- **noflush**（1163 B）：`## User` 4 行、`## Project: work` 7 行、`## Open threads` 4 行。10 条记忆类全在，状态类有 S1、
  S2、S3、S7、S8、S10（S8 的 workaround 被放进了 Project 小节）。没进记忆的只有 S4、S5、S6、S9。
- **flush**（745 B）：`## User` 5 行、`## Project: work` 3 行（其中一行合并了 M2/M10/M6），`## Open threads` **为空**。
  10 条记忆类全在，状态类一条也没有。

## 3. 两组实际经历了什么（从 `messages.jsonl` 读出）

| | noflush | flush |
|---|---|---|
| filler 轮数（事实之后） | 12（加 LEAD 2，日志里共 14 条 Filler） | 105（日志里共 107 条 Filler，离 120 的上限还剩 15） |
| 压缩点 | 每 4 个 filler 手打一次 `/compact`，压缩时 usage 约 8–9k / 32k | 自动触发，压缩前一轮 input 约 15.3k–16.0k |
| 记录过的最大单次 input | 8,691 | 15,995 |
| `remember` 调用 | 15 次，**全在事实那一轮**（模型自发调用） | 事实那一轮 9 次，加 flush 及其后 3 次（见下） |
| 摘要字数（3 次） | 255 / 375 / 591 | 526 / 664 / 888 |

flush 组 `remember` 的时间线：

1. 事实那一轮（log #6）：自发写入 9 行。记忆类全部写入；状态类被并成一行 `## Open threads`：
   `[user] Live state: on step 3 of 7 …; branch fix/overflow-guard; draft PR 1288; failing test test_ledger_rollover;
   workaround RETRY_JITTER=0; build 20931; last benchmark 812 ms; waiting on vendor re ticket 55-A; src/quartz.rs open.`
   这一行**没有 S9（Kestrel）**。
2. 第 1 次 flush 轮（#90–94）：模型调用 `remember remove`，**删掉了**这行 Live state，回复是「I removed the transient
   status line」。flush 提示里有「Do not save transient state (the summary keeps it)」。
3. 第 2、3 次 flush 轮：都回答「Nothing new to save」，没有写入。
4. 中间一次 filler 轮（#242–248）：模型先自发 add 了一行 `[inferred]`「Running "Filler N" essay exercise」，两轮后又自己删掉了。

按关键词检查每次摘要漏了哪些事实：

| 组 | 摘要（log 序号，compacted_through） | 漏掉的事实 |
|---|---|---|
| noflush | #46（43） | 全部 10 条记忆类，以及 S8；状态类除 S8 外都在 |
| noflush | #55（51）、#64（59） | 无 |
| flush | #95（87） | **19 条**（只有 M7 British English 作为写作约定出现）。原文：「The live-state details live in memory and need not be restated」 |
| flush | #170（164） | 只漏 S9 |
| flush | #253（243） | 只漏 S9 |

## 4. 解读

### 4.1 这次能说明什么

- **管道是通的**：在一个真实的 openresponses 模型上，32k 窗口里，bot 的阈值压缩、flush notice → flush 轮 → 压缩这条链路
  跑了 3 次，没有卡住，也没有超时。noflush 里手打 `/compact` 记成 `flush_skipped`，日志也没有被 flush 污染。
  这些与 L1 集成测试的断言一致。
- **在这个模型上，「先 flush 再压缩」相对「只压缩」没有显示出优势**。20/20 对 19/20，差 1 条，又是单次运行，
  算不上方向证据。按 §5.2 的规则，结论应记为**方向未定**。
- **天花板效应的来源很清楚**：两组的模型都在**收到事实的那一轮就主动调用了 `remember`**（noflush 15 次，flush 9 次）。
  脚本头部注释已经说明这样计入，所以两组比的其实是「自发 remember + 压缩」与「自发 remember + flush + 压缩」。
  对 deepseek-flash 来说，flush 要补的那块（模型没主动存下的事实）几乎是空的，所以测不出 flush 的增益。
  这本身就是一条数据：对于一个会主动记录的模型，flush 的边际价值在这个设置下接近 0。

### 4.2 一个比分数更值得看的观察：flush 与摘要之间的交接缺口

flush 组唯一丢掉的 S9，以及其余 9 条状态事实差点一起丢掉，原因不在压缩次数，而在 flush 提示与摘要的分工：

1. 第 1 次 flush 轮里，模型按「不要存 transient state，摘要会保留」的提示，把记忆里那行 Live state **删了**；
2. 同一次压缩的摘要（#95）是根据删除之前的视图写的：当时记忆块里还有这行，于是它判断「live-state details live in
   memory and need not be restated」，**一条状态事实也没写**（20 条里只出现了 M7）；
3. 结果是状态事实同时不在记忆里、也不在摘要里。其余 9 条之所以能救回来，是因为删除时产生的 `-1 line` notice
   （#94）带着整行原文，又正好落在保留区（compacted_through 87 之后），第 2 次摘要（#170）把它抄成了「Transient
   work state (removed from MEMORY.md; retained here only for continuity)」。这属于**侥幸**，不是机制保证的。
4. S9 从来没进过记忆（模型并行时漏掉了它），第 1 次摘要又把它让给了「记忆」，所以在第 1 次压缩时就丢了。

也就是说，§3.6.2「摘要装对话状态、事实归记忆」的分工在这里出现了一个**双向推诿**：flush 把状态推给摘要，摘要把状态推给
记忆，而两者看到的记忆版本不同（摘要看到的是 flush 之前的）。这只是单次观察，我没有改代码，也没有改设计文档。
它是否值得在 §3.6.1 / §3.6.2 里处理（例如摘要提示不要因为「已在记忆里」而省略状态，或者 flush 提示不鼓励 remove），
由产品判断。noflush 组没有 flush 轮，也就没有这种推诿：它的第 1 次摘要（#46）同样省掉了已在记忆里的记忆类事实，
但状态类保留了下来。

### 4.3 这次不能说明什么

- **不定稿 §6 #13 的任何常量**：脚本不改常量，也不设候选值对照。本次 MEMORY.md 最大只有 1163 B，离 6 KiB 的软阈值、
  8 KiB 的上限都很远；摘要最长 888 词，也没碰到 1500 词。所以这些上限根本没有受到考验。
- **两组没有配平**：noflush 只跑了 12 个 filler，压缩点在 usage 约 28% 时；flush 跑了 105 个 filler，每次压缩都发生在
  接近 16k 的阈值处。flush 组的事实要穿过的对话长了大约 9 倍，摘要也更长。两组的差距（如果有的话）混进了这个因素。
- **评分只是关键词匹配**：例如 M10 只查 `okafor`，S1 只查 `3`；flush 组状态类的答案带着「summary only / treat them as
  unverified」这类自我保留，仍判为 ok。它不评估否定、决策理由与表述的准确性。
- **单次运行、单个模型、单个窗口**：没有重复，没有方差，也不能推广到别的模型。尤其是一个不主动调用 `remember` 的模型，
  结果可能完全不同。
- **测不出 reserve 与最小窗口**：32k 就是最小窗口，本次也没扫窗口边界。这两项归 §3.6.1、§4.1 各自的表项。

### 4.4 实验条件与脚本本身

两组都满足了实验条件（各 3 次压缩，noflush 0 个 flush notice），脚本也跑通了，**没有发现脚本 bug**。
有两点记下来，供以后再跑时参考（不是 bug，我没有改脚本）：

- flush 组用了 120 个 filler 上限里的 105 个。一个回复更短的模型，每轮 token 增长更慢，可能撞上上限而得到 INVALID。
- 脚本把模型自发的 `remember` 计入两组（注释已说明，属于设计如此）。但对会主动记录的模型来说，这使得实验区分不出
  flush 的作用。如果想检验 flush 本身的作用，需要一个让模型在事实轮不主动记录的设置；这是实验设计问题，不是实现问题。

## 5. token 用量

按 `messages.jsonl` 里每条记录的 `usage` 求和（包含 assistant 的每次调用和压缩摘要调用；deepseek 报的 `in` 已经包含
`cache_read`）：

| 组 | 调用次数 | in | 其中 cache_read | out | total |
|---|---|---|---|---|---|
| noflush | 20 | 90,455 | 66,048 | 12,917 | 103,372 |
| flush | 119 | 1,199,796 | 1,077,760 | 47,067 | 1,246,863 |
| **合计** | 139 | 1,290,251 | 1,143,808 | 59,984 | **1,350,235** |

另有开跑前的探测：deepseek-flash 两次，共 3,313 token；glm-5.3 一次，in 1,226 / out 15（它自报的 total 为 0）。
总计约 **135.5 万 token，其中约 114 万是缓存命中**，未命中的输入约 14.6 万，输出约 6 万。没有拿到账单金额。

## 6. 记到验收状态里的建议措辞（供参考，文档由用户更新）

> 2026-10-01，deepseek-flash（openresponses），窗口 32k，3 次压缩，按关键词判分：noflush 20/20（记忆 10 / 状态 10），
> flush 19/20（10 / 9）。两组都触顶：模型在事实轮就自发 `remember`，flush 没有可补的余量。**方向未定**。
> 另外观察到 flush 删掉状态行、摘要又把状态让给记忆的交接缺口（`docs/history/bot-mode/bot-mode-retention-2026-10-01.md` §4.2）。
> §6 #13 的常量保持现值。
