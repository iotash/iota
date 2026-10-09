# bot 记忆作用域：项目键 = `project_slug`

Status: **已定（2026-10-09），未落地** · 基线：main `dc1f49d` · 取代：[`bot-memory-scope-project-superseded.md`](../history/bot-mode/bot-memory-scope-project-superseded.md)（下称**方案 A**，按根提交）与 [`bot-memory-scope-two-layer-superseded.md`](../history/bot-mode/bot-memory-scope-two-layer-superseded.md)（下称**方案 B**，按 git common dir）

所有者的决定（原话）：「**我建议全面简化，用 `project_slug` 作为项目名称**」。

本文把这句话落成一份设计。骨架沿用两份被取代的方案（1–10 节一一对应），能复用的直接复用；它们删掉的东西在 §2.4 列清。权威设计 [`bot-mode.md`](bot-mode.md) §3 已按本文同步改写（rev 3）。代码还是旧形状，按 §10 的切片落地之前，`bot-mode.md` §3 描述的是目标而不是现状，那里已写明。

依据：

- 现状代码：`src/session/store.rs:195`（`SessionStore::project_slug`）、`src/session/store.rs:459-464`（`bucket_of`）、`src/agents/mod.rs:31-44`（`project_root`）、`src/cmd/interactive/mod.rs:280`（`scope = agent.root`）、`src/agents/memory.rs`、`src/agents/memory/snapshot.rs`、`src/repl/run.rs:291`（`memory_project`）、`src/tool/builtins/memory.rs`。坐标指 `dc1f49d`。
- 调研 [`bot-memory-scope-research.md`](bot-memory-scope-research.md)（下称 research），节号与 `[FR*]` 引用编号同两份旧方案。

证据标记沿用旧方案：**【先例】**有已实现的产品或本仓库已有的机制，给出处；**【推断】**本文自己的推理；**【无证据】**没有材料支持或反驳，只能靠 §9 回答。

---

## 1. 一句话定位

**每个 bot 对每个 `project_slug` 有一份项目记忆，放在 `~/.iota/bots/<bot>/projects/<project_slug>/MEMORY.md`；键就是会话桶已经在用的那个 `SessionStore::project_slug(project_root)`，一套键两处用。bot 级 `MEMORY.md` 只留 `## User` 与 `## Open threads`。**

改变的：

- **项目的身份**：从「`project_root` 的目录名」（`iota`）改成「`project_root` 的 slug」（`-Users-joyqi-Work-iota`），与 agent 模式的会话桶 `~/.iota/sessions/projects/<slug>/` 逐字节相同（§2.2）。
- **项目记忆的存放**：从 bot 级 `MEMORY.md` 里的 `## Project: <名字>` 小节，移到 `projects/<slug>/MEMORY.md`，每份各算 8 KiB 上限、各有 `.prev`。
- **`remember` 的 `section`**：三个动作都必填，取值 `User` / `Open threads` / `Project`；`Project` 不带名字，指当前项目，由它决定写哪个文件（§4.1）。
- **推翻 `bot-mode.md` 两条已定决定**：§3.1「不做按项目的独立记忆文件」与 §3.2「`## Project: <名字>` 用目录名、不用 `project_slug`，因为记忆是给人看的」。理由见 §6.4。

不改变的：

- bot 全局唯一、项目只是环境（bot-mode §1.3 方案 A）；会话、指针、锁、压缩时序、flush 状态机都不动。bot 自己的会话仍是 flat 的，不进任何桶（`src/session/store.rs:448` 的 `project: false`）——slug 只用在记忆上，不用在 bot 会话上。
- 记忆归 bot：不进仓库、不跨 bot 共享（理由同方案 A §2.1，不重复）。
- 写入可见、可回滚这一套（bot-mode §3.7）：来源标记、人写的行不可改、Expanded、写入 notice、`.prev`、不做内容过滤。
- 快照的四个刷新时刻、前言口径（数据，低于 AGENTS.md 与用户当下的指令）、单行 500 字节、frontmatter 的 `bot:` 归属校验。

---

## 2. 作用域模型

### 2.1 五个维度

维度取自 research §2.1。

| 维度 | bot 级 | 项目级 |
|---|---|---|
| **所有者** | bot | 同一个 bot；两个 bot 在同一项目里各有一份 |
| **适用范围** | 任何项目：用户偏好、身份、未结事项 | 只在这个 `project_slug` 下：工具链、命令、约定、坑 |
| **存储分区** | `~/.iota/bots/<bot>/MEMORY.md` | `~/.iota/bots/<bot>/projects/<slug>/MEMORY.md` |
| **读取视图** | 每次发送全文注入 | 每次发送只注入**当前** slug 那一份；其它项目一字不提（§5） |
| **生命周期** | 跟 bot 走 | 第一次写入时建目录；之后一直留着，直到人 `rm -r`。改名、搬家、删 worktree 后旧目录成为孤儿，不迁、不清（§2.3） |

### 2.2 project key：`SessionStore::project_slug(project_root)`

**定义**：`project_root = agents::project_root(cwd)`（向上找第一个 `.git`，目录或文件都算；找不到就是 cwd 本身），键 = `SessionStore::project_slug(project_root)`：清理后的路径把分隔符换成 `-`（Windows 上 `: * ? " < > |` 也折成 `-`）。

| `project_root` | slug |
|---|---|
| `/Users/joyqi/Work/iota` | `-Users-joyqi-Work-iota` |
| `/Users/joyqi/.herdr/worktrees/iota/mem-simplify`（linked worktree） | `-Users-joyqi-.herdr-worktrees-iota-mem-simplify` |
| `/tmp/scratch`（非 git，回落到 cwd） | `-tmp-scratch` |
| `C:\Users\x\proj` | `C--Users-x-proj` |

**一套键的含义**：agent 模式的会话桶是 `bucket_of(scope)`，`scope = agent.root`（`src/cmd/interactive/mod.rs:280`），记忆用同一个 `agent.root`、同一个函数。两边对同一次启动算出的字符串必然相同，这一点由 §9 Q11 的测试钉住。【先例】本仓库自己：会话桶从 Go 版起就是这个键（`projectSlug`，chat/session.go:169-174），本机 `~/.iota/sessions/projects/` 下已有 7 个这样的桶；Claude Code 的 `~/.claude/projects/<slug>/` 也是同一种编码（`project_slug` 的文档注释原文就写「Claude Code style」）。

**为什么就用它**：所有者的决定。本文不再比较候选；两份旧方案和 research §9 已经比较过，那些比较的结论（每个候选都有代价）不变，变的是取舍的标准——**一致性优先于键的稳定性**。

### 2.3 这个键的代价（明说，不藏）

下面每一条都**与会话桶现在的行为完全一致**：同一个场景里，agent 模式的会话列表也会这样表现。这是选它的理由，也是它的代价。

1. **worktree 碎片化（有意保留）**。`project_root` 在 linked worktree 里停在 worktree 自己的 `.git` 文件上，所以每个 worktree 是一个项目。这个仓库的日常做法是每个分支一个 worktree（`~/.herdr/worktrees/iota/<分支>`）：bot 在某个分支 worktree 里学到的项目事实，主 checkout 和别的 worktree 看不到；worktree 删掉后，那份记忆成了孤儿目录。**相对现状不是退步**：现状用目录名，worktree 的目录名就是分支名，同样碎片化（方案 A §2.3 表格第一行、方案 B §2.2 已指出）。方案 B 想解决的正是这一条，本文有意不解决。
2. **改名、搬家 = 另一个项目**。`mv ~/Work/iota ~/Code/iota` 之后 slug 变了，旧记忆不跟过来。**这一条相对现状是退步**：现状按目录名，只要最后一级没变就找得回来。【先例】这正是 Claude Code #61349 报告的失败（research §7.1 [FR1]）。缓解只有一个：`/status` 的 `Memory` 行显示当前文件路径（§6.6），人发现后把旧目录 `mv` 成新 slug。不做「疑似改名」提示（§2.4）。
3. **slug 不是单射**。`/Users/x/a-b/c` 与 `/Users/x/a/b-c` 都是 `-Users-x-a-b-c`，会被当成同一个项目、共用一份记忆。本机的 `-Users-joyqi-Work-ubuntu-img` 单看 slug 也分不出是 `ubuntu-img` 还是 `ubuntu/img`。会话桶有同样的问题且至今没人报告过【推断：罕见，没有统计】。不加消歧规则。
4. **非 git 目录各算一个项目**。在 `~/Downloads/x` 里启动，项目就是 `-Users-joyqi-Downloads-x`；换到 `~/Downloads/y` 就是另一个。项目文件只在第一次写 `Project` 时建，不写不留痕迹。这不是特例分支，只是同一个函数的结果（§2.4 第 5 条）。
5. **slug 是给机器看的**。`-Users-joyqi-Work-iota` 不如 `iota` 好读，而且 `-` 的歧义让人无法从 slug 精确还原路径。人要看的界面是 `/status`，那里同时显示真实路径与文件路径（§6.6）。
6. **路径很长时目录名可能超过文件系统的单段上限**（多数文件系统是 255 字节）。会话桶同样会在这里失败【推断，没有实测】；不做截断或 hash。

### 2.4 删掉了什么（对照两份旧方案）

「全面简化」的落点：**键由一个已有函数算出，于是所有为「更稳的键」而存在的机制都没有了用处**。逐条：

| # | 删掉的机制 | 出自 | 它原来防什么 | 为什么现在可以删 |
|---|---|---|---|---|
| 1 | 根提交 sha 作键（`git rev-list --first-parent --max-parents=0 HEAD`），起 git 子进程 | A §2.3、§4.1 | 改名、搬家、worktree、多 clone 失联 | 键改成 slug；这些失联被接受为代价（§2.3） |
| 2 | 浅克隆、空仓库、没装 git 的「无项目层」判定与 `/status` 原因文案 | A §2.3、§6 | 根提交拿不到 | slug 对任何路径都算得出 |
| 3 | `.git` 文件 / `gitdir` / `commondir` 解析、`canonicalize` | B §2.2 | 让 worktree 共用一份项目记忆 | worktree 碎片化被接受（§2.3 第 1 条） |
| 4 | frontmatter 的 `root:`（B）与 `project:` / `name:`（A） | A §3、B §3.2 | 键藏在文件里，目录名只给人看 | 目录名**就是**键，路径拼接即查找 |
| 5 | 「没有当前项目」分支（现状 `memory_project` 返回 `None`、A 的 `NoProject`） | 现状、A | 取不到目录名或根提交 | bot 一定有 `project_root`（缺 cwd 在 `cmd` 里已是致命错误，`src/cmd/mod.rs:312-317`），slug 一定算得出 |
| 6 | 目录名 `<basename>-<键前 12 位>` 与「后缀匹配 + 全键核对」查找 | A §3 | 键不可读，又要能在目录里找到 | 目录名就是键 |
| 7 | 撞名规则（`-2`、`-3`）与「复制目录导致重复键」检测 | A §3、B §3.1、§8.2 | 两个项目显示名相同 | slug 由路径决定，两个不同路径不需要命名规则（不单射的情况见 §2.3 第 3 条，按代价接受） |
| 8 | 「疑似改名」notice | B §3.3 | 改名后提示人重新挂上 | 改名 = 另一个项目，与会话桶一致；`/status` 足够让人发现 |
| 9 | 启动时预建项目目录 | A §4.1 | 给跨项目写入留一个可按名字定位的容器 | 跨项目写入删了（第 10 条），目录在第一次写入时懒建 |
| 10 | 跨项目写入 `remember(section: "Project: <名字>")` | A §4.2 | 换项目后 flush 还能把上一个项目的事实写回去 | 模型要写出项目名，而项目名现在是 slug，这正是歧义的来源；见 §8.1 第 2 条的代价 |
| 11 | `Other projects: …` 汇总行（现状就有） | 现状、A §5、B §5.1 | 提示模型还有别的项目记忆 | 它的消费者是跨项目写入与 `recall(project:)`，前者删了、后者没实现；在碎片化的键下它会列出每个死掉的 worktree。L2 做 `recall` 时若需要再加 |
| 12 | 记忆块的 `project=` 属性（现状就有） | 现状 | 告诉模型当前项目名 | harness 每条消息已带 `project_root:`（bot-mode §1.3），属性是重复的 |
| 13 | `User` 只收 `source: user`、跨层精确去重 | B §2.3 | 路由错误 | 与项目键无关的路由防线，【无证据】有效；Q6、Q8 的数据回来前不做 |
| 14 | 前言里「项目行在本项目内优先于 User 行」、`FLUSH_NOTICE` 的路由句 | A §4.3、B §4.2 | 局部反例改写全局偏好 | 路由规则只写在 `remember` 的工具描述里一处（§4.2）；flush 轮同样广告这个工具，描述就在那里 |
| 15 | 4 KiB + 4 KiB 的新上限（B） | B §4.4 | 注入总量不涨 | 沿用现有的一个常量 `MEMORY_CAP` 按文件算，不加常量（代价见 §8.1 第 4 条） |
| 16 | `recall(project: <名字> \| all)` 参数 | A §5、B §5.3 | L2 跨项目检索 | L2 没开工；到时按本文布局再定，不预留 |

### 2.5 保留了什么，以及为什么不能删

| 保留的机制 | 不能删的理由 |
|---|---|
| bot 级文件与项目文件**分开存放** | 见 §6.4。一句话：在碎片化的键下，单文件共享的 8 KiB 会被一个个死 worktree 的小节吃满，而分文件时死掉的项目只是一个不注入、不占预算、可 `rm -r` 的目录 |
| 项目文件的 frontmatter `bot:` / `updated:` | 两种文件用同一个 `Doc` 格式与同一个 `check_owner`；给项目文件单开一种无 frontmatter 的格式反而多一套解析。`bot:` 照旧防「从别的 bot 目录拷来的文件」 |
| 项目文件里的 `## Project` 标题 | 让现有的按小节 `apply` 原样复用；注入时也不用由宿主另外拼标题 |
| bot 级文件里 `## Project…` 标题不注入 + `⚠` | 现状规则是「人手加的小节按全局注入」。不拦，旧的 `## Project: iota` 小节会在**每个**项目里注入，正是作用域要防的串味。迁移完之后它仍然有用（人手写错位置），所以是格式规则，不是兼容层（§7） |
| 两个 mtime 的外部编辑检测 | 人手迁移（§7）靠它在下一条消息前生效；它是现有机制作用在两个文件上 |
| `/status` 的 `Memory` 行 | slug 对人不友好、改名会失联、迁移要知道往哪个文件贴——三件事都需要一个能看见「这个 checkout 被认成了哪个项目」的地方（§6.6） |
| `section` 必填 | 两个文件之后，`section` 决定写哪个文件；现状的缺省 `User` 会把项目事实默认写进全局（【先例】Copilot #201874，research §7.1 [FR5]），改缺省为 `Project` 又会把偏好困在一个 worktree 里。两个缺省都没有证据，删掉缺省最简单【推断】 |

---

## 3. 磁盘布局

```
~/.iota/bots/<bot>/
    bot.json                  # 指针（不变）
    lock                      # bot 锁（不变）：覆盖整个目录，含 projects/
    MEMORY.md                 # bot 级：frontmatter + ## User + ## Open threads（+ 人手加的小节）
    MEMORY.md.prev
    projects/
        -Users-joyqi-Work-iota/
            MEMORY.md         # 项目级：frontmatter + ## Project
            MEMORY.md.prev
        -Users-joyqi-.herdr-worktrees-iota-mem-simplify/
            MEMORY.md
```

`projects/<slug>/` 与会话根下的 `projects/<slug>/` 同名同构（`PROJECTS_DIR_NAME`），只是一个在 `~/.iota/sessions/`、一个在 bot 目录里。

bot 级文件（与现状相比少了 `## Project:` 小节）：

```markdown
---
bot: coder
updated: 2026-10-09
---

# coder memory

## User
- [user] 回复用中文，技术名词保留原文 (2026-09-30)

## Open threads
- [user] 等 bot-retention.sh 的两组试运行看 flush 的方向 (2026-09-30)
```

项目级文件：

```markdown
---
bot: coder
updated: 2026-10-09
---

## Project
- [user] 提交前只跑 clippy + cargo test，完整 ci.sh 每个 PR 收尾跑一次 (2026-09-15)
- [inferred] 发布日期用 date -u +%F (2026-09-28)
```

- **查找**：`bot_dir.join("projects").join(slug).join("MEMORY.md")`。不扫目录，不读 frontmatter 匹配。文件不存在就是空。
- **创建**：第一次 `remember(section: "Project", action: "add")` 时 `create_dir_all`，写 frontmatter 与 `## Project`，与现状 `MEMORY.md` 的懒创建同一套规则。
- **人手加的其它小节**：项目文件里的只在本项目注入；bot 级文件里的全局注入（现状规则），`## Project…` 例外（§2.5）。

---

## 4. 写入路径

### 4.1 `remember` 的参数

| 参数 | 现状 | 本设计 |
|---|---|---|
| `section` | `add` 可选，缺省 `User`；`Project: <名字>` 带名字 | **三个动作都必填**：`User` / `Open threads` → bot 级文件；`Project` → 当前项目文件。带名字的 `Project: x` 报错：`Project is the current project; drop the name` |
| `old` | 在整个文件里恰好命中一行 | 在 `section` 选中的**那个文件**里恰好命中一行（规则、报错、候选列表都不变） |
| `action` / `text` / `source` | — | 不变 |

为什么 `replace` / `remove` 也要 `section`：它让「恰好命中一行」继续是单文件规则，不需要跨文件合并候选、标注每个候选在哪一层（方案 B §4.1 的做法）。模型在记忆块里看得到每一行在哪个标题下【推断】。

模型只能写当前项目。写别的项目，要么到那个项目里去写，要么人直接改文件。

### 4.2 模型怎么知道该写哪

只写在一处：`REMEMBER_DESCRIPTION`。要点（措辞落地时定）：

> section is required. **User**: what the user said holds everywhere. **Project**: facts about the project you are running in — its toolchain, commands, conventions, pitfalls. **Open threads**: pending matters. Put each fact in one place. For replace and remove, section names the file the line is in (User and Open threads share one file).

前言与 `FLUSH_NOTICE` 不加句子（§2.4 第 14 条）。【先例】把路由规则写给模型看：Gemini CLI 写在系统提示里（research §3.4）；写进工具描述而不是系统提示是本文的选择【推断】。

### 4.3 既有机制

| 机制 | 本设计 |
|---|---|
| `.prev` | 写哪个文件就先备份哪个（`BotMemory::write` 的顺序不变） |
| 8 KiB 硬上限 / 6 KiB 软阈值 | **同一个常量按文件算**。超限时的拒绝信息带**被写的那个文件**全文；软阈值提示写明文件名 |
| 单行 500 字节、来源标记、人写的行不可改 | 不变，两个文件都适用 |
| 写入 notice | `memory: MEMORY.md ## User +1 line: …` / `memory: projects/-Users-joyqi-Work-iota/MEMORY.md ## Project +1 line: …` |
| 返回值 | `saved to <文件> <小节> (x / 8 KiB)` + 受影响的整个小节，文件是相对 bot 目录的路径 |

---

## 5. 读取路径

记忆块（overlay 最后一段，位置不变）：

```
<memory bot="coder">
<MEMORY_PREAMBLE，不变>

<bot 级正文：前言段、## User、## Open threads、人手加的小节；## Project… 小节除外>

<项目文件正文：## Project 及人手加的小节>

Notes (read with recall):     ← L2 才出现
…
</memory>
```

- **快照**：`Snapshot` 持有两份副本（bot 级 + 当前项目，后者可能为空），两个 mtime。四个刷新时刻不变；外部编辑检查两个 mtime，任何一个变了就重读两个，notice 带文件名。
- **上限**：每份 ≤ 8 KiB，人手编辑超限时各自按行截断、各自打 `[memory truncated …]`（沿用 `cut`）。整块最坏 16 KiB 加前言。
- **`Snapshot::block`** 不再有 `project` 参数：当前项目在构造时就定了（进程内 `project_root` 不变，bot-mode §1.3）。
- **summarize 的 LONG-TERM MEMORY 段** = 块里的两段正文（`Snapshot::current()` 返回拼好的文本，`BotCompact` 形状不变）。
- **`price_bot_overhead`** 按整块计价，代码不变。
- **L2**（未实现）：`notes/` 跟着文件走（`notes/` 与 `projects/<slug>/notes/`）。`recall` 的参数等 L2 开工再定（§2.4 第 16 条）。档案检索不变，跨项目。

---

## 6. 与既有机制的交互

### 6.1 压缩与 memory flush

时序与状态机不变。flush 轮照旧只广告 memory 工具集，可写 bot 级与当前项目。摘要的 LONG-TERM MEMORY 段见 §5。

### 6.2 会话档案

不变，不按项目切。**分文件不等于隔离**：压缩摘要可能带着上一个项目的事实，`recall(archive)` 也能搜到。research §8 末尾对现状的判断（相关性控制，不是硬隔离）对本设计同样成立。

### 6.3 会话桶

记忆与 agent 模式会话桶共用 `SessionStore::project_slug`，**不复制、不改写**这个函数。`agents::memory` 不能依赖 `session`（分层），所以 slug 在 `cmd::interactive::wire_session` 里算出（那里同时持有 `SessionStore` 与 `agent.root`），作为字符串传给 `BotMemory::new`。bot 自己的会话仍是 flat 的，不进桶（§1）。

### 6.4 为什么推翻 bot-mode §3.1 与 §3.2

**§3.1「不做按项目的独立记忆文件」**。它的理由有两半：「项目知识的正确归宿是 AGENTS.md」——**仍然成立**，AGENTS.md 是人写的项目规则、进 git、高于记忆；项目文件是这个 bot 的经验，归 bot，是数据（【先例】Claude Code 与 Codex 都把人写的 instructions 和模型写的 learnings 分开，research §3.6）。被推翻的是另一半「写进 `## Project:` 小节足够用」：

- 一个文件、一个 8 KiB 预算，所有项目共用。键改成 slug、且有意保留 worktree 碎片化之后，每个分支 worktree 都会长出一个小节，worktree 删掉后小节留下来继续占预算、继续出现在 `Other projects` 里。分文件之后，死掉的项目只是一个不注入的目录，人可以整目录删【推断】。
- 小节标题要写项目名。名字现在是 slug，让模型在标题里写对 `-Users-joyqi-.herdr-worktrees-iota-mem-simplify` 没有意义；分文件后模型只写 `Project`，名字由宿主定。

**考虑过、没采用**：「单文件 + `## Project: <slug>` 小节」，只改 `memory_project()` 一个函数。改动最小，但上面两条它都躲不开。代价是：分文件的改动面更大（§10）。

**§3.2「用目录名、不用 `project_slug`，因为记忆是给人看的」**。所有者选了「一套约定优先」：项目身份只有一套，会话桶怎么认项目，记忆就怎么认。「给人看」这一需求不丢，换了界面：人看 `/status` 的 `Memory` 行（真实路径 + 文件路径），文件正文里不出现 slug。评审 I7 当时建议的就是 slug，现在被采纳。

### 6.5 锁与指针

不变。`projects/` 在 bot 目录里，bot 锁覆盖它。

### 6.6 `/status`

bot 下新增一行（`/status` 按能力决定显示哪些行，`src/repl/commands/status.rs:3`）：

```
Memory   /Users/joyqi/Work/iota → projects/-Users-joyqi-Work-iota/MEMORY.md 2.3/8 KiB · MEMORY.md 0.4/8 KiB
```

当前项目还没有文件时写 `(none yet)`。这是本设计唯一新增的展示面，理由见 §2.5。

### 6.7 其它

- **「Resumed in a different project」notice**（bot-mode §2.5）：不变，按路径比较；它与项目文件的切换恰好同时发生，因为两者都看 `project_root`。
- **`repl::run::memory_project`**：删除。

---

## 7. 迁移

**不写迁移代码**（项目规矩：不留向后兼容）。

- **为什么不能自动迁移**：旧小节名是目录名（`## Project: iota`，worktree 里是分支名），推不出它当时的完整路径，也就推不出 slug。按「当前 `project_root` 的目录名 == 小节名」去猜，在同名目录与 worktree 上都会猜错，把一个项目的事实挂到另一个项目上，比不迁更糟。
- **旧文件会怎样**：bot 级 `MEMORY.md` 里的 `## Project: X` 小节原样留在文件里（不删人的数据），**不注入**；每次快照刷新时 transcript 打一次（沿用 `Snapshot::warning()`）：
  `⚠ MEMORY.md has a "## Project: X" section; project memory lives in projects/<当前 slug>/MEMORY.md now — move the lines that belong to this project there`
  `remember` 也不再往这种小节里写（`Project: X` 会被拒）。`## User`、`## Open threads` 照常。
- **CHANGELOG 手工步骤**（随实现的那个版本写进 Unreleased）：
  1. 在旧小节对应的那个目录里启动 bot（worktree 里的小节，就在那个 worktree 里启动；worktree 已经删了的，选一个你希望继承它的 checkout）。
  2. 看 `/status` 的 `Memory` 行，得到 `projects/<slug>/MEMORY.md`。
  3. 文件不存在就新建，内容为 `---`、`bot: <bot 名>`、`---`、空行、`## Project`；把旧小节的行剪切到 `## Project` 下，删掉旧小节标题。
  4. 下一条消息前外部编辑检测会重读，出现 reload notice 即生效。
- **`notes/`**：L2 没实现，没东西要迁。
- **实际规模**：本机唯一的 bot（`~/.iota/bots/herdr/`）只有 `bot.json` 和 `lock`，没有 `MEMORY.md`。0.5.x / 0.6.x 已发布 `mode: bot`，别的用户可能有，所以 CHANGELOG 仍要写。

---

## 8. 代价与风险

### 8.1 简化带来的代价

1. **worktree 碎片化**（§2.3 第 1 条）。**这是所有者最需要知道的一条**：在这个仓库的工作方式下（一个分支一个 worktree，用完即删），bot 在分支 worktree 里学到的项目事实几乎都会随 worktree 一起失联，项目记忆很难在主 checkout 里积累起来。能跨 worktree 留下来的，只有 bot 级的 `User` 行和人写进 AGENTS.md 的规则。与会话桶一致：agent 模式下，worktree 里的会话在主 checkout 的 picker 里同样看不到。
2. **换项目后的 flush 写不回上一个项目**。会话跨项目延续：昨天在 herdr 里聊、今天在 iota 里重启，压缩前的 flush 轮里还有 herdr 的内容，但只能写 iota 的项目文件或 bot 级文件。结果是 herdr 的事实要么进摘要、要么被误存进 iota（方案 A §4.2 的论证，本文接受这个代价，删掉了跨项目写入）。【无证据】：不知道多常见。
3. **改名、搬家失联**，且相对现状是退步（§2.3 第 2 条）。
4. **注入上限翻倍**：最坏 8 → 16 KiB（约 2–3k → 4–5k token）。冻结的快照不影响缓存命中，但吃窗口、吃 bot reserve 前的余量。bot-mode §6 #13 的「约 2–3k token 的常驻成本」要改口径。实际数据：唯一一次真模型试运行里 `MEMORY.md` 最大 1163 B（bot-mode §5.2）。
5. **人看不到全貌**：一个 bot 的记忆从一个文件变成 1 + N 个，且目录名是机器编码。
6. **改动面**：比「单文件 + slug 小节」大（§6.4），比两份旧方案小（没有 git 解析、没有命名规则、没有跨文件匹配、没有汇总行）。

### 8.2 与旧方案共有、本设计不解决的风险

- 档案与摘要跨项目，不是硬隔离（§6.2）。
- 被污染的「成功经验」被持续召回（MemoryGraft，research §6.2 [PA7]）：分文件只缩小影响范围。
- 路由写错层：缺省删掉了，但模型仍可能把局部的话写进 `User`（§2.4 第 13 条删掉的防线就是防这个的）。

### 8.3 哪些没有证明

| 断言 | 状态 |
|---|---|
| 记忆键与会话桶键对同一次启动相同 | 【先例】同一个函数、同一个输入；§9 Q11 用测试钉住 |
| 分文件在 slug 键下比单文件好（死 worktree 不占预算） | 【推断】；本机零样本 |
| slug 不单射造成的串味罕见 | 【推断】；会话桶无报告，但也没有统计 |
| 模型能按工具描述把事实写进正确的 section | 【无证据】；Q8 |
| 换项目后的 flush 误存（§8.1 第 2 条）的频率 | 【无证据】；Q5 |
| 16 KiB 最坏注入不明显影响任务表现 | 【无证据】 |
| 超长路径的 slug 超过文件名上限时会失败 | 【推断】；会话桶同样，没实测 |

---

## 9. 验证方式

两类，沿用旧方案：**确定性**（`cargo test`，临时 `HOME`、临时目录、`FakeProvider`）与**模型行为**（真模型、opt-in 脚本、scratch 配置 + 独立 `HOME`，做法同 `scripts/bot-retention.sh`，发送前确认状态行；每项至少 3 次）。

确定性测试不需要 `git` 可执行文件：`project_root` 只看 `.git` 是否存在，测试里写一个 `.git` 目录或 `.git` 文件即可造出普通仓库与 linked worktree。

| # | 问题 | 怎么测 | 通过标准 |
|---|---|---|---|
| Q1 | 切到项目 B 后，会不会误用 A 的工具链 | 确定性：A 的项目文件写入「提交前跑 make lint」，在 B 启动，断言块里没有这一行、也没有任何 A 的痕迹（不再有 `Other projects`）。行为：在 B 里让模型「准备提交」，看它跑不跑 `make lint` | 确定性必须过；行为 3 次都不跑 |
| Q2 | 同名项目会不会串味 | 确定性：`x/app`、`y/app` 各放一个 `.git` 目录，分别启动并写 `Project`，断言得到 `projects/<slug(x/app)>/`、`projects/<slug(y/app)>/` 两个目录，互不注入 | 必须过 |
| Q3 | 改名、搬家后能不能找回 | 确定性：写一行 → `rename` 目录 → 再启动，断言**找不回**：新 slug 下没有文件、旧目录原样还在、`/status` 显示新路径。钉住的是「另一个项目」这一预期行为 | 必须按预期失联 |
| Q4 | worktree 删掉后能不能找回；worktree 之间是否隔离 | 确定性：主目录放 `.git` 目录，`wt/` 放 `.git` 文件（`gitdir: …`）；在 `wt` 里写一行，在主目录启动断言看不到；删掉 `wt/`，断言 `projects/<slug(wt)>/` 仍在（孤儿）。钉住碎片化 | 必须按预期隔离 |
| Q5 | 跨多仓库任务召回是否完整；换项目后的 flush 去哪 | L1 确定性：A、B 各有记忆，在 A 启动，断言块里只有 A。行为：先在 herdr 里聊出一条 herdr 事实、不压缩，换到 iota 重启后触发 flush，统计这条事实落在哪（iota 项目文件 / bot 级 / 只进摘要 / 丢失）。**已知缺口**：L1 看不到别的项目，L2 的 `recall` 才可能补上 | 记录分布；无通过线 |
| Q6 | 通用偏好能不能延续；局部反例会不会推翻全局偏好 | 确定性：`User` 写一行，在三个不同 slug 下启动都注入。行为：A 里说「以后都用中文回复」，看是否进 `User`、B 里是否延续；再在 A 里说「这个仓库的提交信息用英文」，看是写进 `Project` 还是改了 `User` | 偏好进 `User`；反例进 `Project`、没改 `User`；记录违反率 |
| Q7 | 一个项目大量写入，会不会挤掉另一个项目 | 确定性：A 的项目文件写到 8 KiB 被拒，断言 B 的项目文件与 bot 级文件字节不变、照常可写可注入 | 必须过 |
| Q8 | 写入路由是否正确 | 行为：20 条混合陈述（通用偏好、项目事实、只在本项目成立的话、模型推断），按写入 notice 统计落层；确定性部分：不给 `section` 的三种动作都报错 | 记录错层率；确定性必须过 |
| Q9 | 分支经验会不会被别的 worktree 误用 | 确定性：由 Q4 覆盖（结构上隔离）。行为不再需要：碎片化让这个风险在本设计里不存在，代价换成了 Q4 的失联 | Q4 通过即可 |
| Q10 | 非 git 目录 | 确定性：在没有 `.git` 的临时目录启动，写 `Project`，断言文件在 `projects/<slug(cwd)>/`，没有任何「无项目」分支 | 必须过 |
| Q11 | **一套键**：记忆键是否就是会话桶键 | 确定性：同一个 `project_root`（普通目录、linked worktree、非 git 各一例），断言 `BotMemory` 的项目目录名 == `SessionStore::project_slug(root)` == agent 模式 `create(project: true)` 落盘的桶名 | 必须过；这是本设计的核心不变量 |
| Q12 | 旧形状会不会泄漏 | 确定性：bot 级文件里有 `## Project: iota`，断言它不进块、transcript 有 `⚠` 且指向当前 slug 的路径；`remember(section: "Project: iota")` 报错 | 必须过 |
| Q13 | slug 不单射 | 确定性：`a-b/c` 与 `a/b-c` 两处启动，断言命中**同一个**目录。钉住已知行为，防止将来有人「顺手修掉」而让会话桶与记忆分叉 | 必须按预期共用 |
| Q14 | 人能不能找到要改的文件 | 确定性：`/status` 的 `Memory` 行包含真实路径与 `projects/<slug>/MEMORY.md`。人工：只凭 `/status` 与写入 notice 找到并改正一条错误记忆 | 确定性必须过；人工定性记录 |

同时记录每轮注入字节数，对照现状（§8.1 第 4 条）。

---

## 10. 最小落地切片

### 10.1 第一步

**L1 分文件 + slug 键，不碰 L2**：

1. `cmd::interactive::wire_session`（与 `cmd::assemble` 的测试构造）：`BotMemory::new(name, dir, SessionStore::project_slug(&agent.root))`。
2. `agents::memory`：`BotMemory` 持有 bot 级与项目级两个路径；`Section::Project` 不带名字；`apply` 作用在 `section` 选中的文件上，上限按文件算；`.prev` 按文件；项目文件懒建。
3. `agents::memory::snapshot`：两份副本、两个 mtime；删掉 `Other projects` 与 `project=`；bot 级文件的 `## Project…` 小节不注入并给 `warning()`；`block()` 去掉参数。
4. `tool::builtins::memory`：`section` 进 `required`，三个动作都用；更新 `REMEMBER_DESCRIPTION`。
5. `repl::run`：删 `memory_project`。
6. `repl::commands::status`：`Memory` 行。
7. 测试：§9 的全部确定性项。
8. CHANGELOG Unreleased：行为变化 + §7 的手工步骤；`bot-mode.md` §3 去掉「未落地」字样。

不动：`session::*`（包括 `project_slug` 本身）、指针、锁、flush 状态机、压缩时序、记录格式、`MEMORY_PREAMBLE`、`FLUSH_NOTICE`。

### 10.2 怎么知道成了

- §9 的确定性测试全部进 `cargo test` 并通过，其中 **Q11** 是不变量；每个提交 clippy + `cargo test`，PR 收尾跑一次完整 `ci.sh`。
- 手工：在 `~/Work/iota` 和一个 herdr worktree 里先后启动同一个 bot，`/status` 的 `Memory` 行指向**两个不同**的文件，且各自等于 `~/.iota/sessions/projects/` 下 agent 模式会用的桶名。
- Q6、Q8 的行为探测至少跑一轮并记下数字；不阻塞合并，但没有它，§8.3 的「无证据」就一直是无证据。

### 10.3 不在任何一步里的

worktree 共享（被有意放弃，§2.3）；改名提示与自动重挂；跨项目写入；`Other projects` 汇总；`recall(project:)`；自动迁移；仓库内共享记忆。任何一项想加回来，都要先拿出它解决的问题在使用中出现过的证据。
