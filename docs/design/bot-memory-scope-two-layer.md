# bot 记忆作用域：身份层 + 项目层双层分存（方案说明）

Status: **Proposal**（供选择，未拍板）· 日期：2026-10-07 · 分支 `mem-design-b`（自 main `9022f72`）

依据：

- 现状设计 [`docs/design/bot-mode.md`](bot-mode.md) §3（下称 **bot-mode**）与源码 `src/agents/memory.rs`、`src/agents/memory/snapshot.rs`、`src/agents/mod.rs`、`src/repl/run.rs`、`src/tool/builtins/memory.rs`（坐标指 `9022f72`）。
- 本次调研 [`docs/design/bot-memory-scope-research.md`](bot-memory-scope-research.md)（下称 **research**）。它由两份报告（19 个 coding agent 的横向调研，与专用记忆系统、证据等级、失败记录的模式调研）合并而成。本文写「research §N」指合并后文档的节号，例如「research §4.2.6」是 Copilot 一节，「research §9」是 project key 候选表；方括号里的 `[FR2]` 之类是它附 B 的引用编号。

标注约定：**【先例】**有产品或系统已经这样做，并给出 research 中的出处；**【推断】**本文自己的设计推理，没有外部证据；**【无证据】**目前没有任何数据支持或反驳，只能靠第 9 节的验证来回答。

本文与另一份方案说明使用同一个骨架（1–10 节），可以逐节对照。

---

## 1. 一句话定位

**把 bot 的长期记忆拆成两个独立文件：身份层 `~/.iota/bots/<bot>/MEMORY.md` 只放跨项目成立的东西（用户偏好、身份、未结事项）；项目层 `~/.iota/bots/<bot>/projects/<名字>/MEMORY.md` 一个项目一份，项目由宿主按 git 公共目录（common dir）识别。每轮注入「身份层 + 当前项目层」。**

**改变的：**

- 记忆从「一个文件 + `## Project: <名字>` 小节」改成「一个身份文件 + N 个项目文件」，每个文件有自己的上限和 `.prev`。
- 项目怎么识别：从「`project_root` 的目录名」改成「git common dir 的规范化路径」。这样同一仓库的所有 worktree 共用一份项目记忆，同名的不同仓库不会串。
- `remember` 的 `section` 改为**必填**，`Project` 不再带名字（名字由宿主定，模型不用也不能写）；`User` 只接受 `source: user`。
- bot-mode §3.1「**不做按项目的独立记忆文件**」这条决定被推翻（第 6.4 节说明为什么推翻，以及 AGENTS.md 的分工为什么仍然成立）。

**不改变的：**

- 一个 bot = 一个身份 + 一条永不结束的会话（bot-mode §1.3 方案 A）。会话不按项目切，bot 目录、指针、锁都不变。
- 记忆只属于 bot：不做跨 bot 的共享记忆，也不做进 git 的团队记忆层。项目层是「**这个 bot** 对这个项目的经验」，不是「这个项目的团队记忆」。
- 写入机制：显式 `remember` + 压缩前的 flush 轮、来源标记、人写的行不可改、`.prev`、写入 notice、不做内容审查（bot-mode §3.3、§3.7）。
- 快照的四个刷新时刻（bot-mode §3.4）、记忆块的前言口径（数据，低于 AGENTS.md 和用户当下的指令）。
- 单行 500 字节、来源标记格式、文件级 frontmatter 的 `bot:` 归属校验。

---

## 2. 作用域模型

### 2.1 五个维度

维度划分取自 research §2.1。

| 维度 | 身份层 | 项目层 |
|---|---|---|
| **所有者** | bot（`agents.<name>`，`mode: bot`） | 同一个 bot。项目层挂在 bot 目录下，两个 bot 在同一项目里各有各的项目层 |
| **适用范围** | 在任何项目都成立：用户明说的偏好、身份设定、跨项目的未结事项 | 只在这一个项目（同一 git 仓库的任意 checkout/worktree）成立：工具链、命令、约定、踩过的坑 |
| **存储分区** | `~/.iota/bots/<bot>/MEMORY.md`（一个文件） | `~/.iota/bots/<bot>/projects/<目录名>/MEMORY.md`（一个项目一个文件；目录名只是显示用，匹配看文件里的 `root:`） |
| **读取视图** | 每轮全文注入 | 每轮只注入**当前项目**那一份的全文；其它项目只列名字和行数，一行 |
| **生命周期** | 跟 bot 走：删 bot 目录才消失。模型能改/删带标记的行，人能改任何行 | 跟项目走：项目文件可以整目录删除、归档，不影响身份层和其它项目。项目改名/搬走后，人改一行 `root:` 就能重新挂上（§3.3） |

**冲突优先级**（从高到低）：用户当下的请求 > AGENTS.md > **项目层** > 身份层。项目层排在身份层前面，理由是「局部覆盖一般」：近处优先。【先例】AGENTS.md 嵌套目录近处优先（research §3.5）；OpenAI Cookbook 的 session overrides 先于 global defaults（research §5.8）。项目层**只在本项目内**覆盖身份层，不改写身份层：项目里的反例写进项目层，身份层那一行原样不动（§4.3）。

### 2.2 project key：选 git common dir 的规范化路径

**定义**：从 `agents::project_root(cwd)`（`src/agents/mod.rs:31`，向上找第一个 `.git`）得到的根出发：

1. `<root>/.git` 是目录 → common dir = `<root>/.git`。
2. `<root>/.git` 是文件（linked worktree 或 submodule）→ 读 `gitdir: <路径>`。如果该 gitdir 下有 `commondir` 文件，common dir = gitdir 拼上 `commondir` 的内容（worktree 的情况，内容通常是 `../..`）；没有 `commondir` 就取 gitdir 本身（submodule 的情况）。
3. 找不到 `.git`（`project_root` 回落到 cwd）→ key 用 cwd 本身。
4. 最后一律 `std::fs::canonicalize`（解开符号链接）。

**key 就是这条规范化路径**，写在项目文件 frontmatter 的 `root:` 里。显示名用来给目录起名：common dir 的 basename 是 `.git` 时取它的父目录名，否则取 basename 去掉 `.git` 后缀（bare 仓库 `repo.git` 得到 `repo`，submodule 得到模块名）；没有 git 时取 cwd 的目录名。

实测本工作树：`.git` 是文件，内容为 `gitdir: /Users/joyqi/Work/iota/.git/worktrees/mem-design-b`；那个目录下的 `commondir` 是 `../..`，所以 common dir 是 `/Users/joyqi/Work/iota/.git`，显示名是 `iota`。**按现状的规则，这个工作树的项目名却是 `mem-design-b`**，见下面「为什么不选当前 basename」。

**为什么选它，不选其它四个候选**（候选和各自的代价见 research §9）：

| 候选 | 不选的理由 |
|---|---|
| 当前 basename（现状） | `project_root` 在 linked worktree 里停在 worktree 自己的 `.git` **文件**上，所以 basename 是**worktree 的目录名**。这个仓库的日常工作方式恰恰是每个分支一个 worktree（`~/.herdr/worktrees/iota/<分支>`）：现状下每个分支都会长出一个 `## Project: <分支名>` 小节，worktree 删掉后那一节就成了孤儿。再加上两个无关仓库同名时会串（research §9）。合并前的模式调研原稿只记了碰撞和改名两条，漏掉了 worktree 这一条（合并后的 research §9 已补上），而它是本项目最常碰到的 |
| 规范化绝对路径（`project_root` 本身） | worktree 各算一个项目，问题同上；Claude Code #28037 正是这个失败（research §7.1 [FR2]） |
| 远程 URL | 没有 remote 的本地仓库、fork、多个 remote、SSH/HTTPS 两种写法都要额外定规则（research §9）；读 remote 要解析 `.git/config`，它比 `commondir` 复杂得多 |
| 显式稳定 ID + 别名 | 要么把 ID 写进用户仓库（写到 bot 目录之外，越出记忆工具的 jail），要么存在 bot 目录里、用路径当别名，可改名后照样找不回来，还多一层别名管理。对单用户、本机这个场景，收益撑不起它的复杂度【推断】 |
| （补充）根提交 sha | OpenCode 在没有 remote 时用它（research §4.6.6）。改名、搬家、换机器都不会变。但要么起 `git` 子进程（iota 现在一处也没有），要么自己解析 packfile；空仓库和非 git 目录还得另配一套回落；从模板 clone 来的仓库会共用同一个根提交。不选 |

**选它的理由**：

- **worktree 共享**，这是这个仓库的主要工作方式。【先例】Claude Code 让同一仓库的 worktree 共用记忆，这是官方明确表态的唯一一家（research §2.2、§3.1）。
- **同名不串**：路径本身唯一。
- **零新依赖**：只读两个小文本文件（`.git`、`commondir`），不起 git 进程。`project_root` 已经在做「找 `.git`」这件事，这里只是往下多走一步。
- **非 git 目录不用另写分支**：key 直接是 cwd，同一套代码。

**代价**（都写进第 8 节）：

- **改名/搬家会断**：路径变了就匹配不上。缓解是§3.3 的「疑似改名」提示，人改一行 `root:` 就能重新挂上；不做自动重挂，因为路径相同不代表是同一个项目，反过来也一样。
- **同一仓库的两个独立 clone 算两个项目**（不同的 common dir）。research 提到的产品里没有一家讨论过多 clone（research §2.2）。
- **分支状态混进共用经验**：所有 worktree 读同一份项目层，某个分支特有的命令可能在别的分支被误用。【先例】Copilot 用当前分支核对 citation（research §4.2.6）；本方案不做核对，只在工具描述里要求「只对某个分支成立的事不要写进项目层」，这条约束能不能起作用【无证据】。
- 在 `~`、`/tmp` 这种非 git 目录里启动，也会各算一个「项目」。项目文件在第一次写入时才创建，所以不写就不留痕迹。

### 2.3 写入路由：选「模型显式选 + 宿主硬约束」，不选其它四家的做法

双层产品决定「这一条落哪层」的机制，research 归纳为三种（§3.4 事实小结）：模型按系统提示里的路由规则（Gemini、OpenHands，Qwen 按 type 路由），写入参数（Goose 的 `is_global`），条目自带 scope 且用户可见（Copilot）。本方案的选择：

- **写入参数，必填**：`remember(section: "User" | "Open threads" | "Project")`，没有缺省值。【先例】Goose `is_global`（research §4.2.4）。现状缺省是 `User`；research §10.2 指出「模型漏写 section 时缺省成 User 会扩大适用范围」，Copilot #201874 就是这种失败（[FR5]）。改成必填，模型就必须当场判断。
- **工具描述里写路由规则**：【先例】Gemini 系统提示里的路由规则，包括「一条事实只能落一层、不许跨层镜像」（research §4.2.2）。规则原文见 §4.2。
- **宿主的两条硬约束**【推断，无先例】：
  1. **`User` 只收 `source: user`**。身份层只放用户明说的话（以及人手写的行）。模型自己推断出来的东西只能进 `Project` 或 `Open threads`。这条直接针对 research §7.2 的第 ② 类失败「写入归属错误」：把局部经验升成个人事实（[FR5]）。
  2. **跨层精确去重**：`add` 归一化后的文本如果和另一层的某一行完全相同，就拒绝写入，并提示「已经在 X 层；要挪层先 remove」。只做精确匹配，语义上的重复抓不到。
- **条目可见**：每次写入在 transcript 里展开，notice 带层名（§4.4）。【先例】Copilot CLI 每次存储都显示 scope（research §4.2.6）；iota 不加审批门，这一点沿用 bot-mode §3.7。

**没选的做法**：

| 做法 | 不选的理由 |
|---|---|
| Copilot：每次写入都弹确认 | bot 要能无人值守地跑（bot-mode §4），flush 轮里不能停下来等人确认。bot-mode §3.7 已经定了「可见 + 可回滚」替代审批 |
| Qwen：按 type 路由（`user` 类型永远进用户层）+ 后台 Dream 合并 + team 层 | type 词表是 notes（L2）的事，L1 一行一条、不加逐条 schema（bot-mode §3.2）。后台整理 bot-mode §3.3 明确不做。team 层要进 git，越出 bot 目录的 jail，还要做秘密扫描，和「不做内容审查」冲突（bot-mode §3.7 第 4 条）（research §4.2.1） |
| Gemini：拿不准时问用户 | 「问用户」放进工具描述，作为模型可以做的事，但不当成宿主机制；flush 轮里没人可问 |
| OpenHands：两层共用一个预算、按层公平分（合计 6,000 字符） | 共用预算正好保留了「一个项目写多了挤掉别的」这个问题，只是从「挤掉别的项目」变成「挤掉身份层」。本方案每个文件各有固定上限，现有代码本来就是按文件算上限（`MEMORY_CAP`），只需要把它变成参数（research §4.2.3） |
| Goose：本地层不预载、只能检索 | 项目事实（工具链、命令）恰恰是每轮都需要的；不预载就会出现「切到项目后忘了用 pnpm」（research §4.2.4） |

---

## 3. 磁盘布局

### 3.1 目录

```
~/.iota/bots/<bot>/
    bot.json              # 指针（不变；SessionStore 写）
    lock                  # 单写者锁（不变）
    MEMORY.md             # 身份层：frontmatter + ## User + ## Open threads；remember 工具与人写
    MEMORY.md.prev        # 身份层上一版；工具每次写身份层前保存
    notes/                # L2（未实现）：身份层笔记
    projects/
        iota/             # 目录名 = 显示名；与另一个项目撞名时依次加 -2、-3
            MEMORY.md     # 项目层：frontmatter（含 root:）+ ## Project；remember 工具与人写
            MEMORY.md.prev
            notes/        # L2（未实现）：项目层笔记
        herdr/
            MEMORY.md
```

- `projects/<目录名>/` 在**第一次往这个项目写入时**才创建，和现在 `MEMORY.md` 懒创建是同一套规则。只读的项目不留目录。
- **匹配只看 `root:`，不看目录名**。目录名只给人看，创建之后就不变（项目改名后目录名仍是旧名，人可以手动改名，不影响匹配）。
- 都在 bot 目录内：记忆工具的 jail 和 bot 锁的覆盖范围不用变。

### 3.2 文件格式

身份层（和现在相比只是少了 `## Project:` 小节）：

```markdown
---
bot: coder
updated: 2026-10-07
---

# coder memory

## User
- [user] 回复用中文，技术名词保留原文 (2026-09-30)
- 不要用 rebase

## Open threads
- [user] 等 bot-retention.sh 的两组试运行看 flush 的方向 (2026-09-30)
```

项目层：

```markdown
---
bot: coder
root: /Users/joyqi/Work/iota/.git
updated: 2026-10-07
---

# coder memory · iota

## Project
- [user] 提交前只跑 clippy + cargo test，完整 ci.sh 每个 PR 收尾跑一次 (2026-09-15)
- [inferred] 发布日期用 date -u +%F (2026-09-28)
```

- 项目文件的 frontmatter 比身份文件多一个键 `root:`，即 §2.2 的 key。读写时都校验 `bot:`（沿用 `Doc::check_owner`）。`root:` 用来**查找**；找到之后，读和写都只针对这个文件。
- 身份文件里的**保留标题**：`## Project`、`## Project: …` 不再是身份层的小节。身份文件里出现这种标题时，这一节**不注入**，transcript 打 `⚠ MEMORY.md has a "## Project: iota" section; project memory lives in projects/ now — move its lines to projects/iota/MEMORY.md`。这是格式规则，不是兼容层：它防的是「人手写了一个项目小节，结果在所有项目里全局生效」这种误解。迁移期之后它照样有用（§7）。
- 项目文件里人手加的其它小节：和现在一样保留原样，只在本项目注入。

### 3.3 谁写什么

| 文件 / 字段 | 谁写 | 何时 |
|---|---|---|
| 身份 `MEMORY.md` 正文 | `remember`（`section` 为 `User` / `Open threads`）、人 | 工具调用、flush 轮、手动编辑 |
| 项目 `MEMORY.md` 正文 | `remember`（`section: Project`）、人 | 同上，只针对当前项目 |
| 项目 frontmatter 的 `root:` | 工具创建文件时写一次；之后**只有人改** | 首次写入；改名/搬家后由人重新挂上 |
| `updated:` | 工具每次写入时刷新 | 同现状 |
| `*.prev` | 工具，每个文件各一份 | 写该文件之前 |

**疑似改名提示**（只提示，不自动改）：启动时如果当前 key 没有匹配的项目文件，而 `projects/` 下有某个文件的 `root:` 路径**已经不存在**、且它的目录名等于当前的显示名，就打一条暗色 notice：`projects/iota/ was for /old/path/.git, which no longer exists; if this is the same project, set its root: to /new/path/.git`。【推断】不自动重挂，因为「旧路径没了、名字相同」不能证明是同一个项目。

---

## 4. 写入路径

### 4.1 `remember` 的参数变化

| 参数 | 现状 | 本方案 |
|---|---|---|
| `section` | 可选，缺省 `User`；`Project: <名字>` 带名字 | **必填**：`User` / `Open threads` / `Project`。`Project` 不带名字，指当前项目；带了名字（`Project: x`）就报错，提示「project is the current one; drop the name」 |
| `source` | `user` / `inferred` | 不变；`section: User` 时只能是 `user`，`inferred` 报错：「User holds only what the user said; file your own conclusion under Project or Open threads」 |
| `old` | 在整个文件里恰好命中一行 | 在**身份层 + 当前项目层**里恰好命中一行。命中 0 行或多行都报错并列出候选，每行前面标层（`MEMORY.md ## User:` / `projects/iota:`） |
| `action` / `text` | 不变 | 不变 |

- **当前项目总是存在**：`project_root` 至少回落到 cwd，key 总能算出来，所以 `Project` 永远可用。这比现状简单：现状里「取不到目录名、没有当前项目」是一个单独的分支（`memory_project` 返回 `None`）。
- **不能写别的项目**。模型只能写当前项目的项目层。跨项目写入的需求罕见，风险却高：模型要自己说出项目名，而这正是 basename 歧义的源头。要改别的项目，切到那个项目里去改，或者人直接改文件【推断】。

### 4.2 模型怎么知道该写哪

工具描述（`REMEMBER_DESCRIPTION`）里关于路由的那句，改写为（这是要点，措辞落地时再定）：

> section is required. **User**: only what the user said holds everywhere — their preferences, how to address them, who you are (source must be user). **Project**: facts about the current project — its toolchain, commands, conventions, pitfalls — and anything the user said holds only here. **Open threads**: pending matters to pick up later. Put each fact in one place only. When the user makes an exception for this project, add it under Project and leave the User line as it is. Do not save what holds only on one branch or checkout. If you cannot tell whether something holds everywhere, ask the user, or file it under Project.

- 记忆块前言（`MEMORY_PREAMBLE`）加一句：`Project lines apply to this project only and take precedence over User lines here.`
- `FLUSH_NOTICE` 加一句：`File each line under User only when the user said it holds everywhere; otherwise under Project.`

### 4.3 局部反例与升级

- **局部反例**：用户在项目 B 里说「这里用 tab 缩进」，身份层有「缩进用空格」。正确做法是在 B 的项目层 `add` 一行，身份层不动。前言里的优先级（项目层 > 身份层）让模型在 B 里按 tab 做，到了别的项目按空格做。宿主没法判断模型 `replace` 身份层的那一行是不是因为这个反例，所以这条只能靠工具描述约束（「leave the User line as it is」），由第 9 节的 Q6 来验证【无证据】。
- **升级**（项目 → 身份）：只能在用户明说「所有项目都这样」时做，写法是先 `remove` 项目层那一行，再 `add` 到 `User`（`source: user`）。跨层去重会拒绝「不 remove 就 add」。
- **降级**（身份 → 项目）：同理，先 `remove` 再 `add`。

### 4.4 既有机制

- **`.prev`**：每个文件各一份，写哪个文件就先备份哪个文件（沿用 `BotMemory::write` 的顺序：读旧文件 → 写 `.prev` → 原子写新文件）。
- **上限**：每个文件独立计算，**身份层 4 KiB、每个项目层 4 KiB**，软阈值都是 75%（3 KiB）。每轮注入的上限 = 4 + 4 = 8 KiB，和现在单文件 8 KiB 的最坏情况一样，bot 的 reserve 计算（bot-mode §3.6.1）不用改。超过硬上限时的拒绝信息只带**被写的那个文件**的全文（现在带整个文件；项目层满了的时候，不该把身份层也灌给模型）。「缩小体积的 replace/remove 永远放行」这条不变。
  - 为什么是 4 + 4 而不是 8 + 8：注入量不涨，压缩 reserve 和缓存成本都不变；身份层只收用户明说的话，4 KiB 大约是 40 行 100 字节的偏好【推断，尺寸本身无证据】。
- **单行 500 字节、来源标记、人写的行不可改**：不变，两层都适用。
- **写入 notice**：`memory: MEMORY.md ## User +1 line: …` / `memory: projects/iota/MEMORY.md +1 line: …`。`WriteLog` 的 `saved` 计数不分层，flush 摘要那边不受影响。
- **人怎么改**：直接编辑两个文件之一。外部编辑检测（mtime）对身份文件和当前项目文件**各自**生效，哪个变了都会触发重读和 `MEMORY.md reloaded` 那条 notice（notice 带上文件名）。

---

## 5. 读取路径

### 5.1 每轮注入什么

`Snapshot` 改为持有两个文件的副本：身份层和当前项目层（如果当前项目有文件）。记忆块：

```
<memory bot="coder" project="iota">
<前言（加了项目层优先那一句）>

## User
- …
## Open threads
- …

## Project
- …

Other projects: herdr (3 lines), web (1 line)
</memory>
```

- **顺序**：身份层在前、项目层在后。【先例】OpenHands 也是用户层在前、项目层在后，理由是后出现的内容模型更关注（research §4.2.3）；这也和「项目层优先」的口径一致。
- `project=` 属性取显示名（目录名），不再是 basename。
- **其它项目一行**：保留现在的 `Other projects:` 汇总，数据来源从「同一文件里的其它小节」变成「`projects/*/MEMORY.md` 的行数」。它提示模型还有别的项目记忆，是 L2 跨项目 recall 的入口（§5.3）。
- **上限**：身份层 ≤ 4 KiB + 项目层 ≤ 4 KiB + 汇总行。人手编辑超出上限时，各自按行截断，各自打 `[memory truncated …]`（沿用 `cut`）。
- **刷新时刻**：四个时刻不变。「外部编辑」检查身份文件和当前项目文件两个 mtime。`Other projects` 的行数只在这四个时刻重算，不因为别的项目文件变了就刷新（别的项目文件只有人会改，而且不影响本项目的正文）。

### 5.2 当前项目在什么时候确定

进程启动时确定一次（`project_root` 在进程生命周期内不变，jail 也是这样，bot-mode §1.3）。`repl::run::memory_project`（`src/repl/run.rs:291`，现在返回 basename）改为启动时算出 `ProjectKey { root, name }`，存进 `BotState`。

### 5.3 notes 与 recall（L2，未实现）

**现状核对**：bot-mode §3.1 把 `notes/*.md` 和 `recall` 列为 L2，代码里都还没有（`src/` 下没有 `recall` 工具，也没有 notes 的读写代码）。所以本节是给 L2 的约束，不在最小切片里。

- notes 跟着层走：`notes/` 放身份层笔记，`projects/<p>/notes/` 放项目层笔记。`remember(file: "notes/<topic>")` 按 `section` 决定落在哪层的 `notes/` 下（`User` / `Open threads` → 身份层，`Project` → 当前项目）。
- 记忆块里的笔记目录：身份层的 notes + 当前项目的 notes。
- `recall(source: "memory")` 默认搜身份层 + 当前项目层（`MEMORY.md` 和 notes）；加 `projects: "all"` 时搜所有项目，每条结果前面标项目显示名。这是跨多仓库任务召回别的项目经验的唯一途径。【先例】Graphiti 的多个 `group_ids`、LangMem 组合 namespace，都是显式组合查询（research §6.3）。
- `recall(source: "archive")` 不变：会话只有一条，不分项目。

---

## 6. 与既有机制的交互

### 6.1 压缩与 memory flush

- flush 轮照旧：只广告 memory 工具集，可以写身份层，也可以写当前项目层。
- 摘要调用的 `LONG-TERM MEMORY` 段 = 身份层正文 + 当前项目层正文（合计 ≤ 8 KiB，和现在一样），中间用 `## Project` 标题自然分开。`Snapshot::current()` 返回拼好的正文，`BotCompact { memory, flush_writes }` 的形状不变。
- 软阈值提示：哪个文件超过 75% 就附上哪个文件的那一句（`soft_warning` 带上文件名），两个都超就两句都附。
- 其它项目层**不进**摘要调用：摘要只管当前对话状态。

### 6.2 会话档案（永不结束的那一条）

不变。会话不按项目切，`messages.jsonl` 里有所有项目的原文。**这意味着项目层分开存不等于项目之间互相看不见**：压缩摘要可能带着前一个项目的事实，`recall(archive)` 也能搜到。research §8 末尾对现状说的「这是默认上下文的相关性控制，不是硬隔离」，对本方案同样成立。本方案缩小的是**默认注入**的范围，不是可访问的范围。

### 6.3 `## Project:` 小节

从身份文件格式里删掉。`Section::Project(String)` 改为不带名字的 `Section::Project`，`Section::parse` 不再接受 `Project: <名字>`。身份文件里残留的 `## Project…` 标题按 §3.2 的保留标题规则处理：不注入，并打警告。

### 6.4 与 AGENTS.md 的分工（以及为什么推翻 bot-mode §3.1）

bot-mode §3.1 不做项目文件的理由是「项目知识的正确归宿是 AGENTS.md」。这个分工**仍然成立**：AGENTS.md 放规范性指令（这个项目应该怎么做），由人维护、进 git、团队共享；项目层放的是**这个 bot 的经验性记忆**（此前观察到什么、用户怎么纠正过），私有、不进 git。【先例】research §3.6：Claude Code 和 Codex 都把人写的 instructions 和模型写的 learnings 分开。

推翻的是另一半：「写进 `MEMORY.md` 的 `## Project:` 小节，足够用」。双层的理由在第 1 节和 §2.1，但要老实说清楚：**目前没有观察到单文件不够用的实际案例**。本机唯一的 bot（`herdr`）至今还没有 `MEMORY.md`。是否值得推翻，取决于所有者怎么看第 8 节的代价。

优先级不变：记忆（两层）都排在 AGENTS.md 之下。

### 6.5 bot 的锁与指针

不变。`projects/` 在 bot 目录内，bot 锁（`lock`）已经保证同一时刻只有一个写者，项目文件不需要单独加锁。两个**不同的** bot 在同一个项目里，各写各的 `projects/`，互不干扰。

### 6.6 `/status`

**新增一行 `Memory`**（只在 bot 下出现，符合 `/status` 按能力决定显示哪些行的规矩，见 `src/repl/commands/status.rs:3`）：

```
Memory   MEMORY.md 2.1/4 KiB · projects/iota 3.0/4 KiB (/Users/joyqi/Work/iota/.git)
```

当前项目还没有文件时写 `projects/iota (new, /Users/…/.git)`。**这一行是必须的**：project key 是本方案新引入的、看不见的状态，人得能看到「这个 checkout 被认成了哪个项目」，才能发现认错和改名断开。

### 6.7 `/session`

不变。bot 的会话在普通模式的 picker 里本来就看不到（bot-mode §2.7），和记忆分几层无关。

---

## 7. 迁移

**本机的实际情况**：唯一的 bot `~/.iota/bots/herdr/` 只有 `bot.json` 和 `lock`，**还没有 `MEMORY.md`**，所以本机没有任何东西要迁。外部用户（0.5.x/0.6.x 已经发布了 `mode: bot`）可能有带 `## Project:` 小节的文件。

**做法：一次性的手工迁移，二进制里不放迁移代码**（遵守「不留向后兼容」）：

1. 新版本读到身份文件里的 `## Project: <名字>` 小节，按 §3.2 的保留标题规则：**不注入**，打 `⚠` 警告并指出目标路径。数据原样留在文件里，不丢，也不会悄悄变成全局生效。
2. CHANGELOG 写明手工步骤：在项目里启动一次 bot，用 `/status` 的 `Memory` 行看它被认成哪个 `projects/<名字>/`；把旧小节的行剪切到那个文件的 `## Project` 下（文件不存在就新建，frontmatter 写 `bot:` 和 `/status` 显示的 `root:`）。
3. `notes/`：L2 没实现，没有东西要迁。

**为什么不在二进制里自动迁移**：自动迁移得把旧小节名（basename，而且可能是 worktree 名）映射到新 key（common dir 路径）。这个映射没法从名字推出来：`## Project: mem-design-b` 应该归到哪个仓库，只有人知道。猜错了会把一个项目的事实挂到另一个项目上，比不迁更糟。而且迁移代码一旦写了就得一直留着。

**保留标题规则为什么不算兼容层**：迁移完之后它照样有用，防的是人手在身份文件里写项目小节、以为它只在那个项目生效。它和 `bot:` 归属校验是同一类规则：格式规则，不认旧版本。

---

## 8. 代价与风险

### 8.1 这条路比另一条（一份记忆 + 按项目裁剪）差在哪

1. **写错层的后果更隐蔽**。单文件里，事实放错小节，人打开一个文件就能看到；双层下，一条本该是通用偏好的话落进了项目 A，在项目 B 里就**完全不存在**，人得先想到去 `projects/a/` 里找。「人能一次浏览、纠正所有记忆」（research §10.2 列为现状的优点）在这条路上没有了。
2. **改动面更大**。新增 project key 解析，`BotMemory` 从单文件拆成「身份文件 + 当前项目文件」，`Snapshot` 要组合两个副本，`apply` 的上限和可用小节变成参数，`old` 要跨文件匹配，`/status` 加一行，工具描述、前言、flush 提示都要改，bot-mode §3 要重写（清单见 §10.3）。另一条路如果只修 project key，改动小得多。
3. **身份层容量减半**：从 8 KiB（和项目小节共用）变成 4 KiB 专用，加上 `User` 只收 `source: user`，模型推断出来的跨项目知识（例如「这个用户主要写 Rust」）身份层不再收，只能散落在各个项目层里重复写。
4. **推翻了一条有书面理由的决定**（bot-mode §3.1），用的却是没有使用数据支撑的理由（§6.4）。
5. **项目识别引入新的失败模式**：改名/搬家断开、两个 clone 算两个项目（§2.2）。另一条路如果继续用 basename，改名照样断开，而且 worktree 碎片化更严重；但它不需要人去改 `root:` 这种机器路径。
6. **不提供硬隔离，却容易让人以为提供了**：分开存放给人「项目之间互不可见」的印象，可摘要和档案仍然跨项目（§6.2）。另一条路的单文件形态不会让人产生这种误解。

### 8.2 风险

| 风险 | 缓解 | 证据 |
|---|---|---|
| 模型把项目事实写成 `User` | `User` 只收 `source: user`；`section` 必填 | 【推断】宿主约束能挡住 `inferred`，挡不住模型把用户一句局部的话标成 `user` 写进 `User` |
| 模型因为局部反例去改身份层 | 工具描述 + 前言优先级 | 【无证据】 |
| 分支特有的事实在别的 worktree 被误用 | 工具描述禁止写分支特有的事 | 【无证据】；Copilot 用 citation 核对，本方案不做 |
| 4 KiB 不够 | 软阈值促使模型合并；上限是常量，有数据再调 | 【无证据】本机零样本 |
| 改名后找不回 | 疑似改名 notice + `/status` 显示 key + 人改 `root:` | 【先例】Claude Code #61349 是改名后失联的真实报告（research §7.1 [FR1]）；本方案不自动修，只是让人看得见 |
| 人复制项目目录导致两个文件 `root:` 相同 | 查找时发现重复就拒绝加载项目层，⚠ 列出两个文件（和 `bot:` 不符时的处理同类） | 【推断】 |

### 8.3 哪些未经证明

- **双层在长期使用中是否比单文件裁剪更好**：research §6.4 明确说，没有找到对比「身份全局 / 按项目 / 混合」的实证研究。Copilot 是双层的真实先例，但它的身份是 GitHub 用户、存在服务端、写入要确认，和 iota 的条件不同（research §4.2.6）。
- **「一个项目写多了挤掉另一个项目」这个问题现在存不存在**：没有任何使用数据。按项目分预算解决的是一个**预期中**的问题。
- 4 KiB + 4 KiB 的尺寸、`source` 门槛的效果、必填 `section` 能不能提高路由正确率：都【无证据】。

---

## 9. 验证方式

前提：用同一个 bot、同一组脚本化轨迹，分别跑现状和本方案，这样能和另一条路对照（research §10.5）。分两类：**确定性测试**（单元测试或带 fake provider 的集成测试，进 `cargo test`）和**模型行为探测**（真模型，脚本驱动，人工判读，不进 CI）。真模型探测按 provider 实测的老办法做：scratch 配置 + 独立的 `HOME`，发送前先确认状态行。

| # | 问题 | 怎么测 | 通过标准 |
|---|---|---|---|
| Q1 | 切到项目 B 后，会不会误用 A 的工具链 | 确定性：项目 A 的项目层写入 `用 pnpm`，在 B 启动，断言记忆块里没有这一行、只有 `Other projects: a (1 line)`。模型探测：A 里教会「用 pnpm」，B（npm 项目）里让它装依赖，看它跑的是什么命令 | 确定性必须过；探测中 B 不出现 pnpm |
| Q2 | 同名项目会不会串味 | 确定性：两个临时目录 `x/app`、`y/app`（各自 `git init`），分别写项目层；断言生成 `projects/app/` 和 `projects/app-2/`、`root:` 不同、各自注入各自的 | 必须过 |
| Q3 | 改名、搬家、删 worktree 之后能不能找回 | 确定性：① 主仓库加一个 linked worktree（手工造 `.git` 文件 + `commondir`，或者测试里调 `git worktree add`），在 worktree 里写入，删掉 worktree，回到主仓库，断言能看到那一行；② 把仓库 `mv` 到新路径，断言出现疑似改名 notice，按提示改 `root:` 后能重新注入 | ① 必须过；② notice 必须出现，改 `root:` 之后必须能注入 |
| Q4 | 跨多仓库任务，召回是否完整 | L2 之后：三个仓库各写一条相关事实，在第四个目录里让模型做一个涉及三者的任务，看它有没有用 `recall(projects: "all")`，召回了几条。**最小切片里这一项明确失败**：和现状一样，只能看到 `Other projects` 的名字 | 记作已知缺口，L2 验收 |
| Q5 | 通用偏好能不能延续 | 确定性：`User` 写一行，在三个不同项目里启动，断言都注入。模型探测：A 里说「以后回复都用中文」，B 里用英文提问，看它用什么语言回答 | 必须过 |
| Q6 | 局部反例会不会错误推翻全局偏好 | 模型探测：身份层有「缩进用空格」；在 B 里说「这个项目用 tab」；然后检查①B 的项目层多了一行 ②身份层那一行没被 replace/remove ③切到 C 后按空格做 | 三条都满足；记录违反率 |
| Q7 | 一个项目大量写入，会不会挤掉另一个项目的关键记忆 | 确定性：A 的项目层写到 4 KiB 硬上限被拒，断言 B 的项目层和身份层字节不变、注入照常；对照现状同一轨迹，看 B 的小节在 A 涨满后能不能再写 | 必须过（这是本方案相对现状最直接可测的收益） |
| Q8 | 写入路由是否正确 | 模型探测：20 条混合陈述（通用偏好、项目事实、用户只在某项目说的话、模型自己的推断），脚本读写入 notice 统计每条落在哪层；对照现状统计 `section` 缺省成 `User` 的比例 | 记录错层率；`inferred` 写进 `User` 必须为 0（宿主拒绝） |
| Q9 | 分支经验会不会被误用 | 模型探测：在 worktree `feat-x` 里告诉它「这个分支用 `make dev2`」，在主 worktree 里让它启动开发服务器 | 记录它是否把这条写进了项目层；【无证据】项，只观察 |
| Q10 | 人能不能找到并改正一条错误记忆 | 人工：给出一条错误的行为，让人只凭 `/status` 和 transcript 的 notice 找到要改的文件 | 定性记录 |

同时记录每一轮的注入字节数（`/status` 的 Context 行或 `Usage`），对照现状，确认注入量没有涨。

---

## 10. 最小落地切片

### 10.1 第一步做什么

**L1 双层，不碰 L2**：

1. 新增 `agents::memory::project`：`ProjectKey::resolve(project_root) -> ProjectKey { root: PathBuf, name: String }`，按 §2.2 读 `.git` / `commondir`，加上单元测试（普通仓库、linked worktree、submodule、bare 仓库的 worktree、非 git 目录、符号链接）。
2. `agents::memory`：把单文件的 I/O 抽成「一个记忆文件」（路径、上限、mtime 记录、`.prev`），`BotMemory` 持有身份文件和当前项目文件（项目文件按 `root:` 扫 `projects/*/MEMORY.md` 找，没有就等第一次写入时创建 `projects/<名字>/`，撞名加 `-2`）。`apply` 的上限和可用小节由调用方传入；`Section::Project` 不带名字；`old` 跨两个文件匹配；`User` 拒收 `inferred`；跨层精确去重。
3. `Snapshot`：组合两个副本 + `Other projects` 行；两个 mtime；身份文件的保留标题规则。
4. `remember`：schema 里 `section` 改为必填，更新描述；`MEMORY_PREAMBLE`、`FLUSH_NOTICE` 各加一句。
5. `repl::run::memory_project` → 启动时算出 `ProjectKey`，存进 `BotState`。
6. `/status` 的 `Memory` 行；疑似改名 notice。
7. 更新 bot-mode §3.1–3.5、§3.7（推翻「不做按项目文件」，写入新布局），CHANGELOG 写手工迁移步骤。

### 10.2 能独立交付什么

一个完整可用的双层 L1：身份不会因为切项目而丢；worktree 共用项目记忆；项目各有 4 KiB，互不挤占；写错层能在 `/status` 和 notice 里看出来。不依赖 L2，也不挡 L2（notes 和 recall 按 §5.3 的约束加上去就行）。

### 10.3 必须动的地方及原因

| 位置 | 为什么必须动 |
|---|---|
| `src/agents/memory.rs` | 文件从一个变成两个；上限从常量变成每个文件一个；`Section` 的语义变了 |
| `src/agents/memory/snapshot.rs` | 记忆块由两份副本组合而成；外部编辑要查两个 mtime |
| 新增 `src/agents/memory/project.rs` | project key 是新概念，现有的 `project_root` 只找到 worktree 根 |
| `src/tool/builtins/memory.rs` | `section` 必填、路由规则（模型只能从描述里知道该写哪） |
| `src/repl/run.rs`（`memory_project`） | 项目从 basename 改成 `ProjectKey` |
| `src/repl/bot.rs`（`FLUSH_NOTICE`） | flush 轮写入最多，路由要求必须出现在这里 |
| `src/repl/commands/status.rs` | key 是看不见的状态（§6.6） |
| `src/cmd/interactive/mod.rs`、`src/cmd/assemble.rs`（`BotMemory::new` 的两处调用） | 构造参数多了 `ProjectKey` |
| `docs/design/bot-mode.md` §3 | 推翻 §3.1 的决定，格式和布局都变了 |

**不动的**：`compact.rs` 的 `BotCompact` 形状（`current()` 返回拼好的正文）、`WriteLog`、会话层、锁、指针、`HostDirs`。

### 10.4 怎么知道第一步成了

- 第 9 节的确定性测试 Q1、Q2、Q3①、Q5、Q7 进 `cargo test` 并通过；`clippy` 无告警。PR 收尾时跑一次完整 `ci.sh`。
- 手工验收：在 `~/Work/iota` 和 `~/.herdr/worktrees/iota/<任意分支>` 两处分别启动同一个 bot，`/status` 的 `Memory` 行显示**同一个** `projects/iota (/Users/joyqi/Work/iota/.git)`；在其中一处 `remember(section: "Project")`，重启后在另一处能看到。
- 注入字节数和现状相比不涨（身份 + 项目 ≤ 8 KiB）。
- 模型行为探测 Q6、Q8 至少跑一轮并记录数字。它们不是通过门槛，是所有者在两条路之间做选择时的数据。
