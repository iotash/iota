# bot 记忆作用域方案：按项目

Status: **Superseded**（2026-10-09，被 [`bot-memory-scope.md`](../../design/bot-memory-scope.md) 取代：所有者决定项目键用 `project_slug`；本文冻结，不再更新）· 原状态：Proposal（供选择，未拍板）· 日期：2026-10-07 · 基线：main `9022f72`（0.6.1）

这是两份对照方案之一，讲的是「按项目」这条路：让项目成为 bot 记忆的主要容器。另一份方案用同一个骨架，两份可以逐节对照。

依据：

- 现状设计 [`bot-mode.md`](../../design/bot-mode.md) §1.3、§2、§3（下称 bot-mode），现状代码 `src/agents/memory.rs`、`src/agents/memory/snapshot.rs`、`src/repl/run.rs`、`src/agents/mod.rs`。
- 本次调研 [`bot-memory-scope-research.md`](../../design/bot-memory-scope-research.md)（下称 research）。它由两份报告合并而成，下文写「research §N」指合并后文档的节号，方括号里的 `[FR2]` 之类是它附 B 的引用编号。

证据标记：**【先例】**表示 research 里有已实现的产品或公开记录，后面给出处；**【推断】**表示本文自己的工程推理；**【无证据】**表示没有找到任何支持或反驳的材料，只能靠 §9 的验证来回答。

---

## 1. 一句话定位

**每个 bot 对每个 git 仓库有一份自己的项目记忆，放在 `~/.iota/bots/<bot>/projects/<名字>-<键前缀>/`，键取仓库沿第一父提交走到底的根提交。bot 级的 `MEMORY.md` 缩小到只放身份、跨项目的用户偏好和未结事项。**

改变的：

- **项目知识的存放位置**：从 `MEMORY.md` 里的 `## Project: <目录名>` 小节，移到按项目分开的文件。每个项目单独计 8 KiB 上限，单独有 `.prev`，单独可以删除。
- **项目的身份**：从「项目根的目录名」改为「仓库的根提交」。改名、移动、worktree、另一个 clone 都能认成同一个项目；两个同名但不相关的目录不会再被认成一个项目。
- **`remember` 的写入目标**：缺省写当前项目（今天缺省写 `## User`）。当前项目由工具自己解析，模型不用写项目名。
- **bot 级 `MEMORY.md` 的格式**：不再允许出现 `## Project:` 小节。

不改变的：

- **所有者仍是 bot**。项目记忆是「这个 bot 在这个项目里学到的东西」，住在 bot 目录里，受同一把 bot 锁保护，不进仓库，也不和别的 bot 共享（理由见 §2.1）。
- **会话仍是一条**：bot 全局唯一、项目只是环境（bot-mode §1.3 方案 A），指针、锁、会话布局、压缩时序、flush 状态机都不动。
- **三层结构不动**：L0 档案、L1 常驻、L2 检索（bot-mode §3.1）。只是 L1/L2 里多了一个按项目的分区。
- **AGENTS.md 的地位不动**：项目规则仍由人写、可 review、进 git，优先级高于记忆。
- **写入可见、可回滚这套机制不动**（bot-mode §3.7）：来源标记、人写的行不可改、Expanded 展示、写入 notice、`.prev`、不做内容过滤。

---

## 2. 作用域模型

### 2.1 先选所有权：bot 私有，不放进仓库共享

任务书给了两种形态：bot 目录里的 `projects/<project-id>/`，或者把项目知识放在仓库里、让多个 bot 共享。两者的所有者不同：前者归 bot，后者归项目或团队。**本方案选前者：bot 私有。** 理由按分量排列：

1. **仓库里本来就有一个归项目所有的层，就是 AGENTS.md。**再在仓库里放一份「模型写的项目记忆」，等于同一个所有者有两份文件：一份人维护、要 review，一份模型免审批写。bot-mode §3.1 不做按项目文件的理由正是这一条。bot 私有的项目记忆和 AGENTS.md 所有者不同（bot 对项目），不和它竞争。
2. **放进仓库，就要和 git 打交道，而两种选择都有问题。**如果文件被跟踪：切分支会换掉记忆内容；合并会冲突；模型免审批写下的行会出现在 diff 和 PR 里；bot-mode §3.7 第 4 条明确不做内容过滤，密钥可能被提交。如果文件不跟踪（gitignore）：每个 worktree、每个 clone 各有一份，正好重现 Claude Code #28037 的问题，worktree 删掉，经验跟着丢（【先例】research §7.1 [FR2]），「共享」也就落空了。
3. **写入 jail 和审批会变。**现在 memory 工具的写入被 jail 在 bot 目录里，所以免审批（bot-mode §3.3）。往仓库里写，属于 code 工具集那条要审批的路径（`src/tool/builtins/code/tools.rs:271-273`）。无人值守的 flush 轮要么要人审批，要么就得为它开一个口子。
4. **并发要另外加锁。**bot 锁只覆盖 `~/.iota/bots/<name>/`。两个 bot 同时在一个仓库里写同一个文件，需要一把跨 bot 的新锁。
5. **信任边界会变。**记忆块的前言说这是「你（模型）自己早先写下的数据」（`snapshot.rs` 的 `MEMORY_PREAMBLE`）。如果是仓库共享的，别的 bot、别的人写的内容也会以这个口吻进来，前言就不成立了。

仓库共享形态有先例：Qwen Code 的 `<repo>/.qwen/team-memory/` 是 opt-in，写入默认询问并做秘密扫描（【先例】research §3.4、§3.6）。也就是说，它必须带上 iota 已经明确拒绝的那两样东西：审批和内容扫描。「bot 私有的项目层」也有先例：Claude Code 子代理的 `memory: project` 是「某个 agent 名 × 某个项目」（【先例】research §3.3 [CC2]）。不同的是，它把目录放在仓库里的 `.claude/agent-memory/`，本方案放在 bot 目录里。

### 2.2 五个维度

| 维度 | 本方案 |
|---|---|
| **所有者** | bot。bot 级 `MEMORY.md` 和所有 `projects/*/` 都属于 `~/.iota/bots/<bot>/`，由 bot 锁串行化。没有跨 bot 共享，也没有仓库内文件 |
| **适用范围** | 三种，各有固定落点：**User**（这个用户在哪都希望怎么做）→ bot 级 `## User`；**Open threads**（会话层面的未结事项，会话本来就是跨项目的）→ bot 级 `## Open threads`；**Project**（这个仓库的工具链、布局、命令、坑）→ `projects/<该项目>/MEMORY.md`。一条事实只放一处，不在两层镜像 |
| **存储分区** | 每个项目一个目录，目录名 `<名字>-<键前缀 12 位>`；身份以文件 frontmatter 里完整的 40 位 `project:` 为准 |
| **读取视图** | 每次发送注入：bot 级文件全文 + 当前项目文件全文 + 一行「其它项目」清单（只列名字和行数，不给正文）。其它项目的正文 L1 看不到，到了 L2 可以用 `recall` 显式取 |
| **生命周期** | 项目目录在 bot 第一次在该仓库里启动时建立（只写 frontmatter），之后一直留着，直到人删掉它。不做 GC，不按时间过期。仓库键变了（见 §2.3 的代价），旧目录不会自动迁过去，靠人手动合并 |

### 2.3 project key：仓库的第一父根提交

**选定**：键 = `git -C <project_root> rev-list --first-parent --max-parents=0 HEAD` 的输出，一个 40 位 sha。做法是从 HEAD 沿第一父提交一直走到没有父提交的那个提交。

不能用作项目键的情形，一律视为「没有当前项目」：只剩 bot 级记忆，`/status` 写明原因。

- 不在 git 仓库里（`project_root` 底下没有 `.git`）；
- 仓库还没有提交（HEAD 无效）；
- 浅克隆（`git rev-parse --is-shallow-repository` 为 `true`，这时根提交只是截断边界，和完整 clone 的根提交对不上）；
- 找不到 `git` 可执行文件，或者命令失败。

选它的理由，逐个对比候选：

| 候选 | 为什么不选 | 本方案的键在这里的表现 |
|---|---|---|
| 当前 basename（现状） | 不相关的同名目录会碰撞；改名后找不回旧小节。还有一条两份原始调研都漏了（合并后的 research 已在开头「iota 现状」与 §9 补上）：iota 的 `project_root` 遇到 `.git` **文件**就停（`src/agents/mod.rs:31-44`），所以 linked worktree 的项目根是 worktree 目录本身，basename 取的是 worktree 名。例如本工作树叫 `mem-design-a`，现状下它看不到 `## Project: iota`（`src/repl/run.rs:291-297`）。换句话说，现状对 worktree 已经是碎片化的 | 同名目录不碰撞；改名、worktree 都认作同一个项目 |
| 绝对路径 / 路径 hash | 移动、改名、另一个 clone、临时 worktree 都会碎片化。有公开记录：Claude Code #61349 改名后旧记忆不再加载（【先例】research §7.1 [FR1]） | 和路径无关 |
| git common root | 所有 worktree 能共用，但主工作树的路径一移动就失联；而且依然是路径 | 同上 |
| 远程 URL | fork、迁仓、多个 remote、SSH 和 HTTPS 别名都要定规则；没有 remote 的本地仓库（个人项目里很常见）得另找退路。OpenCode 首选它，没有 remote 时就退到根提交（【先例】research §4.6.6） | 迁仓、换 remote 不影响；没有 remote 的仓库照样有键 |
| 显式稳定 ID + 别名 | 表达力最强，但要有人建 ID、维护别名、处理复制冲突。ID 放仓库里就是往用户仓库写文件，放 bot 目录里就退化成「路径 → ID」的别名表，改名后仍要人补别名 | 不需要人建，也没有别名表 |

为什么用 **`--first-parent`**：仓库合并进一段不相关的历史（subtree merge）时，那段历史的根是作为第二父进来的，沿第一父走下去仍然是项目自己原来的根，键不变。不加 `--first-parent` 会得到多个根，取哪个都会在某次合并后变。**【推断】**

2026-10-07 在临时仓库里手工核对过（git 2.50）：用 `--allow-unrelated-histories` 合并一段孤儿历史之后，`--max-parents=0 HEAD` 返回两个根，加上 `--first-parent` 只返回原来的根；linked worktree 返回同一个根；`--depth 1` 的 clone 上 `--is-shallow-repository` 为 `true`；没有提交的仓库上 `rev-list` 以 128 退出。这只证明命令的行为符合设计，不证明真实仓库里键的稳定性（§8.3）。

有先例的部分：OpenCode 没有 remote 时退到根提交 sha，旧版本直接用 `git rev-list --max-parents=0 --all` 当项目 ID（【先例】research §4.6.6）。它用的是 `--all`，不是第一父，而且会把 ID 缓存在 `<git common dir>/opencode` 里。本方案不写用户的 `.git`，所以不缓存。

**代价**（选它就要付的）：

1. **硬依赖 git。**非 git 目录、刚 `git init` 还没提交的仓库、浅克隆、机器上没装 git，这几种情况都没有项目层。现状的 basename 在任何目录都能用。这是本方案相对现状的一个真实退步。
2. **从同一个模板 fork 出来、保留了历史的两个不相关项目，根提交相同，会被认成一个项目。**用 `degit`、删掉 `.git` 重新 init、GitHub「use this template」得到的是新历史，不受影响。**【推断】**，没有统计过它有多常见。
3. **upstream 和自己的 fork 会被认成同一个项目。**多数情况下这正是想要的，但不能把两者分开。
4. **切到孤儿分支（如 `gh-pages`）后键会变**：重启后那个分支被当作另一个项目。这在语义上说得通（它确实是另一段历史），但可能出乎意料。
5. **启动时多两次子进程**：`rev-parse` 和 `rev-list`。`rev-list --first-parent` 要走完整条第一父链，在几十万提交的仓库上可能要秒级。**【无证据】**，没有实测，见 §9 Q10。
6. **键对人不可读。**所以目录名前面加上建目录时的 basename，`/status` 显示完整路径（§6）。

---

## 3. 磁盘布局

```
~/.iota/bots/<bot>/
    bot.json                   # 指针（不变）
    lock / lock.pid            # bot 锁（不变）：串行化整个目录，包括 projects/
    MEMORY.md                  # bot 级常驻层：frontmatter + ## User + ## Open threads（+ 人手加的小节）
    MEMORY.md.prev             # 上一版，工具每次写 bot 级文件前保存
    notes/                     # L2：bot 级笔记（跨项目的 Runbook、偏好细节）
        <topic>.md
    projects/
        iota-1a2b3c4d5e6f/     # <建目录时的 basename>-<键前 12 位>
            MEMORY.md          # 项目常驻层
            MEMORY.md.prev
            notes/             # L2：该项目的笔记
                <topic>.md
        herdr-9f8e7d6c5b4a/
            MEMORY.md
```

**bot 级 `MEMORY.md`**：格式和现在一样（bot-mode §3.2），区别只有一条：不再有 `## Project:` 小节。

```markdown
---
bot: coder
updated: 2026-10-07
---

# coder memory

## User
- [user] 回复用中文，技术名词保留原文 (2026-09-30)

## Open threads
- [user] 下次在 herdr 里：确认 pane 关闭时的 SIGHUP 行为 (2026-10-06)
```

**项目 `MEMORY.md`**：

```markdown
---
bot: coder
project: 1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b
name: iota
updated: 2026-10-07
---

- [inferred] 提交门：每个提交只跑 clippy + cargo test，ci.sh 在 PR 收尾跑 (2026-10-07)
- [user] 发布日期用 date -u +%F (2026-10-02)
```

- **frontmatter 四个键**：`bot` 和 `project` 是所有权校验，用法和现有的 `bot:` 校验一样（`Doc::check_owner`）：任何一个对不上，整个文件拒绝，不注入、不给 flush 和摘要看、`remember` 也拒绝写，transcript 打 `⚠`。`name` 是显示名，取建目录时的 basename，人可以改，改了不影响身份。`updated` 由工具写入时刷新。
- **正文不要求小节**：一行一条，格式和来源标记同 bot-mode §3.2。人手加的 `##` 小节原样保留。注入时由宿主统一加上 `## Project: <name>` 标题（§5），文件里不用写。
- **谁写**：
  - 目录和只含 frontmatter 的文件：bot 第一次在该仓库启动时由宿主建立（§4.1）；
  - 带标记的行：`remember` 写；
  - 无标记的行和 `name`：人写。
- **查找规则**：算出 40 位键之后，在 `projects/` 里找名字以 `-<前 12 位>` 结尾的目录，再用文件里的 `project:` 全文核对。找到多个（例如人复制了目录）就报错，不猜。

---

## 4. 写入路径

### 4.1 启动时解析项目

`wire_session` 走 bot 分支、拿到 bot 锁之后：

1. 用本次的 `project_root` 算键（§2.3）。结果是 `Project { key, name, dir }`，或者 `NoProject(原因)`。
2. 有键但还没有目录：建 `projects/<name>-<key12>/` 和一个只含 frontmatter 的 `MEMORY.md`。之所以提前建（而不是等第一次写入时再建），是为了让 bot 去过的项目都留下一个可以按名字定位的容器，§4.2 的跨项目写入要靠它。
3. 结果存进 `BotState`，本进程内不再变化。一个进程只有一个项目根，iota 没有进程内切目录的命令。

新代码放在新增的 `agents::memory::project`，用 `std::process::Command` 调 `git`。这是「用已有的东西」：问用户自己的 git，而不是引入 `gix` 或者自己解析 `.git`。

### 4.2 `remember` 的 `section` 参数

参数形状不变，取值的含义变了：

| `section` | 写到哪 | 备注 |
|---|---|---|
| `Project`（**有当前项目时的缺省**） | 当前项目的 `MEMORY.md` | 模型不写项目名，工具按 §4.1 的结果解析，写不到别的项目 |
| `Project: <名字>` | **已存在的**另一个项目 | `<名字>` 先按 `name` 匹配，唯一就用；不唯一时必须写完整的目录名（`herdr-9f8e7d6c5b4a`），否则报错并列出候选。**从不新建项目** |
| `User` | bot 级 `## User` | 没有当前项目时，缺省是它 |
| `Open threads` | bot 级 `## Open threads` | |

- **为什么缺省写 Project**：「把项目事实误存成个人偏好，然后带到无关仓库」是有公开记录的失败（Copilot Discussion #201874，【先例】research §6.2、§7.1 [FR5]）。缺省写窄的那一层，是把错误推向「偏好只留在一个项目里」这一侧。两种错误哪个代价更大，**【无证据】**，见 §8 和 §9 Q6。
- **为什么还要保留 `Project: <名字>`**：会话跨项目延续。bot 昨天在 herdr 里工作，今天在 iota 里重启时，上次压缩之后的那段对话里还有 herdr 的内容。如果此时的 flush 只能写当前项目，herdr 的经验要么丢进摘要，要么被误存进 iota，而后者正是这条路承诺要避免的串味。多仓库任务（在 iota 里改完要去 website 仓库发文章）同理。只能写已存在的项目，避免了模型凭一个名字凭空造出一个新项目。
- **没选的方案**：「换项目时先强制做一次 flush 加压缩」也能防止串味，但每次换项目都要等一次摘要调用（几十秒），而且对同一轮里跨多个仓库的任务没用。它比一个按名字定位的参数复杂，作用又更窄。
- **`replace` / `remove` 的 `old` 匹配**：在 bot 级文件和当前项目文件（如果给了 `Project: <名字>`，就是那个项目的文件）里一起找，必须恰好命中一行，否则报错并列出候选，规则同 bot-mode §3.3。
- **返回值**：沿用 `saved to <文件> <小节> (x / 8 KiB)` 加受影响的整个小节；项目文件没有小节时就是它的整个正文。项目文件会写成相对 bot 目录的路径，例如 `projects/iota-1a2b3c4d5e6f/MEMORY.md`。
- **写入 notice**：`memory: projects/iota-1a2b3c4d5e6f/MEMORY.md +1 line: [inferred] …`。格式不变，只是文件名变了。

### 4.3 模型怎么知道该写哪

三个地方都写同一条路由规则，口径一致：

1. **`remember` 的工具描述**：「`Project` 写这个仓库的事实：工具链、布局、命令、坑，以及只在这里成立的做法；`User` 只写用户明说在任何地方都适用的偏好；`Open threads` 写还没结束的事。拿不准的写 `Project`。同一条事实只写一处。」
2. **记忆块前言**（§5）：加上优先级。「`## Project` 里的行只在这个项目里成立，在这个项目里优先于 `## User`；不要因为某个项目的例外去改 `## User` 的行。」这是防止「局部反例推翻全局偏好」的主要手段。**【推断】**，效果见 §9 Q6。
3. **`FLUSH_NOTICE`**：加一句「facts about this repository go to section Project; facts about another project you worked on earlier in this conversation go to `Project: <name>` (see Other projects)」。

有先例：Gemini CLI 把路由规则写进系统提示，并规定「一条事实只能落一层、不许跨层镜像」（【先例】research §3.4）。Qwen 按 `type` 路由（同上）。Copilot 存储时向用户显示 scope（同上）。

### 4.4 人怎么改

- 直接编辑 `~/.iota/bots/<bot>/MEMORY.md` 或 `projects/*/MEMORY.md`。外部编辑检测对两个已加载的文件（bot 级和当前项目）都生效：mtime 变了，下一条消息前重读，打 `MEMORY.md reloaded` 或 `project memory reloaded`。
- 删除一个项目的全部记忆：`rm -r projects/<dir>`。这是本方案独有的生命周期操作，现状要人手删一个小节。
- 合并两个项目（例如 §2.3 代价 2、4 造成的分裂）：人把行从一个文件挪到另一个文件，删掉空目录。不提供命令（动词集封闭，bot-mode §6 #1）。
- 找文件：`/status` 显示当前项目文件的完整路径（§6）。

### 4.5 `.prev`、上限、软阈值

| 机制 | 本方案 |
|---|---|
| `.prev` | 每个文件各自一份：写哪个文件就先备份哪个 |
| 8 KiB 硬上限（`MEMORY_CAP`） | **每个文件各自**计算。bot 级文件一个，每个项目文件一个 |
| 6 KiB 软阈值（`MEMORY_SOFT_CAP`） | 每个文件各自计算。写入结果里的提示写明是哪个文件超了；flush 提示词对超过软阈值的文件逐个点名 |
| 单行 500 字节 | 不变 |
| 人手编辑超限 | 按文件截断注入，`[memory truncated …]` 写在对应文件的那一段末尾（§5） |

**不新增常量**：一个上限、一套规则，作用在每个文件上。代价是注入上限从 8 KiB 变成 16 KiB（bot 级 + 当前项目各 8 KiB）。**【推断】**bot 级文件不再装项目内容后，实际会远小于 8 KiB。同行的常驻上限在 6,000 字符到 25 KB 之间（【先例】research §3.4「常驻索引上限对照」），16 KiB 落在区间内。

---

## 5. 读取路径

**注入位置不变**：overlay 的最后一段，`join_overlay(agent overlay, memory block)`（`src/repl/run.rs:1054`）。

**块的形态**：

```
<memory bot="coder" project="iota" key="1a2b3c4d5e6f">
<前言：现有 MEMORY_PREAMBLE + §4.3 第 2 条的优先级说明>

<bot 级 MEMORY.md 正文：前言段、## User、## Open threads、人手加的小节>

## Project: iota
<当前项目 MEMORY.md 正文>

Other projects: herdr (3 lines), web-9f8e (1 line)

Notes (read with recall):          ← L2 才出现
- notes/release [Runbook] — …
- project notes/lock-choice [Decision] — …
</memory>
```

- **裁剪**：现在由代码在一个文件里按小节标题挑选；改成两个文件整份注入，其它项目只出现在 `Other projects` 一行里。显示的是 `name`，有重名时显示目录名，行数不计空行。只列至少有一行的项目，空项目不占位置。
- **没有当前项目时**：块上不写 `project=` 和 `key=`，只注入 bot 级正文，所有项目都进 `Other projects`，其规则和今天 `project_root` 取不出目录名时一样（`Snapshot::block(None)`）。
- **上限**：每个文件 ≤ 8 KiB（超出按整行截断），整块 ≤ 16 KiB 加前言和清单。
- **快照刷新**：仍是那四个时刻（bot-mode §3.4）：启动、压缩成功后、换日、外部编辑。两个文件一起冻结、一起刷新，`price_bot_overhead` 按整块计价。模型自己的写入，包括写到别的项目，都不刷新快照。
- **`Snapshot` 的变化**：从「一个文件 + 按标题挑选」改成「bot 级文件 + 可选的当前项目文件 + 其它项目的行数清单」。现在按 `## Project:` 挑选的逻辑（`snapshot.rs` 的 `block`）删掉。bot 级文件里如果出现 `## Project:` 标题（旧文件或人手写的），那个小节**不注入**，transcript 打 `⚠ MEMORY.md has a "## Project: X" section; project memory now lives in projects/ — move it (see /status)`。不注入是为了不让它按「人手小节 = 全局」的规则泄漏到每个项目。

**`notes` 与 `recall`（L2，现在还没实现，按这个布局设计）**：

- `remember(file: "notes/<topic>", section: …)`：`section` 决定写进哪个容器的 `notes/`（`Project` 写进项目目录，`User` 写进 bot 级）。
- 目录段列出 bot 级 notes 和当前项目的 notes，项目的条目加 `project` 前缀；总长仍 ≤ 2 KiB。
- `recall(query, source: memory)`：缺省搜索 bot 级文件、bot 级 notes、当前项目的文件和 notes。新增参数 `project`：`<名字>` 搜那一个项目，`all` 搜所有 `projects/*/`。结果带上 `projects/<dir>/…:行号`，这是跨多仓库任务找齐记忆的途径。
- `recall(source: archive)`：不变。会话是一条，档案不分项目，检索会跨项目。**本方案不对档案做隔离**，这一点和现状一样。

---

## 6. 与既有机制的交互

| 机制 | 怎么变 | 为什么必须变 / 为什么不变 |
|---|---|---|
| **压缩与 memory flush**（bot-mode §3.6） | 时序和状态机（`repl::bot`）**不变**。只改两段文字：`FLUSH_NOTICE` 加上 §4.3 的路由句；`summarize` 的 LONG-TERM MEMORY 段改成 bot 级正文加当前项目正文（和注入块的正文相同） | 摘要需要知道哪些事实已经存过，现在存过的事实分在两个文件里 |
| **会话档案**（永不结束的那条） | 不变。不分段，不按项目打标签 | 会话是 bot 的，项目只是环境（bot-mode §1.3） |
| **`## Project:` 小节** | 从 bot 级文件里**删除**这个概念：`Section::parse` 不再接受 `Project: <名字>` 作为 bot 级小节，改为 §4.2 的路由含义；`Snapshot` 不再按它挑选；遇到旧小节就警告且不注入 | 不删的话，同一件事有两个家 |
| **与 AGENTS.md 的分工** | 不变：AGENTS.md 是人写的项目**规则**，归仓库，高于记忆；项目记忆是这个 bot 的项目**经验**，归 bot，是数据 | 两者所有者不同（§2.1）。风险是项目记忆看上去越来越像一份私有的 AGENTS.md；前言里「低于 AGENTS.md」那句不动 |
| **bot 的锁与指针** | 不变。bot 锁本来就锁整个 bot 目录；`bot.json` 不加字段 | 项目目录在 bot 目录内，选 bot 私有的原因之一就是不用另加锁（§2.1 第 4 条） |
| **`/status`** | 新增一行 `Memory`：`projects/iota-1a2b3c4d5e6f/MEMORY.md (2.3 / 8 KiB)`；没有项目时写 `bot only — not a git repository` / `no commits yet` / `shallow clone` / `git not found` | 键不可读，人需要知道当前用的是哪个文件、为什么没有项目层。这是唯一新增的展示面 |
| **`/session`** | 不变：bot 下不注册（bot-mode §2.2） | |
| **「Resumed in a different project」notice**（bot-mode §2.5） | 不变，仍按路径比较 | 它告诉模型「文件可能不在了」，说的是路径；worktree 和主仓库路径不同，但项目记忆相同，这两件事不冲突 |
| **`memory_project()`**（`src/repl/run.rs:291`） | 删除，改读 `BotState` 里 §4.1 的解析结果 | 用目录名当项目这件事本身被取代了 |

需要动的代码，以及为什么非动不可：

- `agents::memory`：`Doc` 要能处理带 `project:` 的项目文件；`BotMemory` 从「一个 bot 一个文件」变成「一个文件一个实例」。新增 `agents::memory::project`（键解析、目录查找和建立）。这些是这条路本身。
- `agents::memory::snapshot`：从一个文件变成两个文件加清单（§5）。
- `tool::builtins::memory`：`section` 的路由（§4.2）。
- `cmd::interactive::wire_session`：启动时解析一次（§4.1）。
- `repl::run`：去掉 `memory_project`；外部编辑检测覆盖两个文件。
- `repl::commands::status`：一行。
- `repl::commands::compact`、`repl::bot`：两段文字。
- 不动的：`session::*`、指针、锁、flush 状态机、压缩时序、记录格式。

落地时，`bot-mode.md` §3.1 里「不做按项目的独立记忆文件」、§3.2 里「`## Project: <名字>` 用目录名」、§3.4 里「按作用域裁剪」这三处要按本方案改写。本文不动它们。

---

## 7. 迁移

**结论：不写迁移代码。用户手动迁移，发布说明写明步骤；新版本把旧形状识别成错误（警告且不注入），而不是兼容它。**

- **为什么不能自动迁移**：旧小节只记了一个 basename（`## Project: iota`），新键要在那个仓库里跑 git 才能算出来，而 basename 推不出仓库路径。这不是图省事，是信息不够。自动猜（比如扫描 `~/Work/*`）就是投机性的机制。
- **为什么不保留旧行为**：项目规矩是不留向后兼容。保留「bot 级 `## Project:` 小节照旧按目录名裁剪」，等于同时维护两套项目作用域。
- **旧文件会怎样**：bot 级 `MEMORY.md` 里的 `## Project: X` 小节保留在文件里，不删人的数据；但不注入，transcript 每次快照刷新时打一条 `⚠`（§5）。`## User`、`## Open threads` 照常工作。`remember` 不会再往这种小节里写。
- **手动步骤**（写进发布说明）：在项目 X 的仓库里启动 bot → `/status` 看 `Memory` 行给出的文件路径 → 把旧小节的行剪切进去 → 下一条消息前自动重读（外部编辑检测）。
- **`notes/`**：L2 还没实现，没有现成数据要迁。本方案落地时，`notes/` 直接按 §3 的两级布局实现。
- **实际规模**：本机唯一的 bot（`~/.iota/bots/herdr/`）只有 `bot.json` 和 `lock`，没有 `MEMORY.md`。bot 模式在 0.6.x 已经发布，别的用户可能有文件，所以发布说明仍然要写。

---

## 8. 代价与风险

### 8.1 这条路比另一条（按 bot 一份、小节裁剪）差在哪

1. **写入要多做一次路由判断，而且错误的方向反过来了。**现状缺省写 `## User`，容易把项目事实泛化到所有项目（Copilot 式失败）。本方案缺省写 `Project`，容易把真正的个人偏好困在一个项目里，换个项目就要再纠正一次。哪种错误更伤，**【无证据】**。research 找不到比较「身份全局 / 按项目 / 混合」的对照研究（research §6.4 检索结论）。
2. **硬依赖 git**：非 git 目录、空仓库、浅克隆、没装 git 的机器都没有项目层。另一条路用目录名，在哪都能用（§2.3 代价 1）。
3. **人看不到全貌**：现在一个文件就是 bot 的全部记忆；本方案要看 N+1 个文件，而且要手动迁移。
4. **L1 的隔离收益几乎为零。**这是最需要老实说的一点：现状注入时已经把其它项目的小节正文裁掉了（`Snapshot::block`），所以在「切到 B 之后会不会在常驻层读到 A 的工具链」这个问题上，本方案和现状都是「看不到」。本方案独有的收益只有三条：
   - **按项目的独立预算**：一个项目写满不挤别的项目；
   - **按项目的生命周期**：整个目录删掉、备份、查看；
   - **写入目标由工具解析**：模型写不错项目名，也凭空造不出项目。

   **稳定的项目键和存储布局是两个独立的决定**（research §9）：根提交这个键也可以装到另一条路的单文件小节上。如果所有者最在意的是改名、同名、worktree 这三个问题，另一条路换个键就能解决，不必拆文件。
5. **代码和概念更多**：一个子进程解析、一个多文件快照、一个跨项目定位参数、`/status` 一行、一条旧形状警告。另一条路改键只需要换掉 `memory_project()`。
6. **注入上限翻倍**（8 → 16 KiB），每次发送的固定开销随之上限翻倍。被冻结的快照不影响缓存命中，但会影响窗口占用和 bot reserve 前的余量。
7. **worktree 共享带来的 checkout 专属状态混入。**同一仓库的所有 worktree 共用一份项目记忆，某个分支上的临时命令、没合并的实现细节可能被别的 worktree 误用。Claude Code 改成按 repo 共享之后，就有人报告过路径混淆（【先例】research §7.1 [FR3]）。本方案没有结构性的缓解，只靠行文里写清分支。另一条路如果也换成仓库级的键，同样有这个问题；保留目录名作键，反而碰巧按 worktree 隔离了（§2.3 表格第一行）。

### 8.2 两条路共有、本方案并不解决的风险

- 档案和压缩摘要仍然跨项目（§5 末尾），本方案**不是硬隔离**，不能承诺租户或客户级别的隔离。research §8 末尾对现状的判断同样适用于本方案。
- 被污染的「成功经验」被持续召回（MemoryGraft，【先例】research §6.2 [PA7]）：按项目分文件只是缩小了影响范围，错误照样可能写进同一个项目或者 bot 级。

### 8.3 哪些是没有证明的

| 断言 | 状态 |
|---|---|
| 按项目的独立预算在实际使用中有必要（会有项目写满 8 KiB） | **【无证据】**：本机唯一的 bot 还没有 `MEMORY.md` |
| 模型能按 §4.3 的规则把事实路由到正确的一层 | **【无证据】**：有路由规则的先例（Gemini、Qwen），没有准确率数据 |
| 「项目行在本项目内优先于 User 行」这句前言能阻止局部反例改写全局偏好 | **【推断】** |
| 第一父根提交在真实用户仓库里稳定（不会因为模板、孤儿分支、重写历史而频繁变化） | **【推断】**；OpenCode 用根提交有先例，但用的是 `--all`，没有它稳定性的数据 |
| 大仓库里 `rev-list --first-parent` 的启动开销可以接受 | **【无证据】** |
| 注入上限 16 KiB 不明显降低任务表现 | **【无证据】**；AGENTS.md 的实验显示多塞约束可能增加成本（research §6.2 [PA6]），但那不是记忆实验 |

---

## 9. 验证方式

分两类：**确定性**的（单元或集成测试，用临时 git 仓库和 `FakeProvider`，进 CI）和**模型行为**的（真模型、opt-in 脚本、scratch 配置加临时 HOME，不进 CI，做法同 `scripts/bot-retention.sh`）。行为类脚本每项至少跑 3 次，报告错域应用次数、该召回而没召回的次数、人工重复纠正次数、注入字节数。

| # | 问题 | 怎么测 | 通过的标准 |
|---|---|---|---|
| Q1 | 切到项目 B 之后，会不会误用 A 的工具链 | 确定性：A 的项目文件写入「提交前跑 make lint」，在 B 的仓库里启动，断言注入块里没有这一行，只有 `Other projects: A (1 line)`。行为：在 B 里让模型「准备提交」，看它会不会跑 `make lint` | 块里不含 A 的正文；行为类 3 次都不跑 |
| Q2 | 同名项目会不会串味 | 确定性：`a/iota` 和 `b/iota` 是两个独立 `git init` 的仓库，各提交一次，分别启动，断言得到两个目录（`iota-<k1>`、`iota-<k2>`），写入互不可见；`remember(section: "Project: iota")` 在第三处报「名字不唯一」并列出两个目录 | 两个目录、互不注入、歧义时报错 |
| Q3 | 改名、移动之后能不能找回 | 确定性：在仓库里写一行 → `mv repo repo-renamed` → 再启动，断言命中同一个目录，那一行还在 | 同一个目录 |
| Q4 | worktree 删掉之后能不能找回 | 确定性：`git worktree add ../wt`，在 wt 里写一行 → `git worktree remove ../wt` → 在主仓库启动，断言那一行还在；再在一个独立的 clone 里启动，断言也命中 | 主仓库和 clone 都命中 |
| Q5 | 跨多个仓库的任务能不能召回完整 | 确定性（L1）：A、B、C 三个项目各有记忆，在 A 里启动，断言 `Other projects` 列出 B、C；在 A 里 `remember(section: "Project: B")` 写入 B 的文件。确定性（L2）：`recall(project: all)` 返回 B、C 的命中，并带路径。行为：三个仓库的联动任务，统计漏掉的依赖事实数 | L1 清单完整，跨项目写入成功；L2 全部命中 |
| Q6 | 通用偏好能不能延续；局部反例会不会推翻全局偏好 | 行为：在 A 里说「以后都用中文回复」，看是否写进 `User`；在 B 里启动看是否延续。再在 A 里说「这个仓库的提交信息用英文」，看是写成 Project 行，还是改了 `User` 里的中文偏好；换到 B 看中文偏好是否还在。确定性部分：前言和工具描述里有路由句，且两处措辞一致 | 偏好进 `User`；反例进 `Project`，没改 `User` |
| Q7 | 一个项目大量写入，会不会挤掉另一个项目的关键记忆 | 确定性：把 A 的文件写到 8 KiB 上限，下一次写 A 被拒；B 照常可写，B 的注入不受影响；bot 级文件也不受影响 | 只有 A 被拒 |
| Q8 | 旧分支的经验会不会被别的 worktree 误用 | 行为：在 worktree X（分支 feat）里让模型记一条只对 feat 成立的命令，在主仓库（main）里看它会不会照用。确定性部分：无。这是 §8.1 第 7 条的已知风险，**只测量，不设通过线** | 记录误用率，作为是否需要分支维度的依据 |
| Q9 | 拿不到键时会不会退化正确 | 确定性：非 git 目录、空仓库、`git clone --depth 1`、`PATH` 里没有 git，四种情况都启动成功，没有项目层，`/status` 写明原因，`remember(section: "Project")` 报错，`User` 照常可写 | 四种都满足 |
| Q10 | 大仓库的启动开销 | 手动：在一个 10 万提交量级的仓库里计时键解析 | 记录数值；超过 1 s 时，在复议里讨论是否缓存 |
| Q11 | 键的稳定性 | 确定性：subtree merge 一段不相关历史之后键不变；切到孤儿分支之后键变（预期行为，测试钉住） | 两条都符合预期 |
| Q12 | 旧形状会不会泄漏 | 确定性：bot 级文件里有 `## Project: iota`，断言它不进块，transcript 有 `⚠`；`remember` 不往里写 | 不注入，有警告 |

Q1、Q8 的行为部分，和另一条路用同一份脚本、同一组事实跑，结果才能对照。research §10.5 提出的维度（错域应用、未召回、重复纠正、注入量、迁移维护成本）就是这份脚本的计分项。

---

## 10. 最小落地切片

**第一步：项目键 + 项目常驻文件 + 路由 + `/status`，不碰 L2。**

范围：

1. `agents::memory::project`：键解析（§2.3）、目录查找和建立（§3、§4.1）；
2. 项目 `MEMORY.md` 的读写，包括所有权校验、`.prev`、每个文件独立的上限；
3. `remember` 的 `section` 路由（§4.2），以及工具描述、`FLUSH_NOTICE`、前言里的路由句（§4.3）；
4. `Snapshot` 变成两个文件加清单（§5），bot 级 `## Project:` 警告且不注入，删除 `memory_project()`；
5. summarize 的 LONG-TERM MEMORY 段；
6. `/status` 的 `Memory` 行；
7. 发布说明里的手动迁移步骤（§7）。

**它能独立交付**：做完之后 bot 就有按项目的常驻记忆，端到端可用。L2（notes、recall）仍按 bot-mode §5 的排期，到时直接按 §3 的两级布局实现，不需要回头改第一步。

**怎么知道第一步成了**：

- Q1（确定性部分）、Q2、Q3、Q4、Q5（L1 部分）、Q7、Q9、Q11、Q12 的测试全部进 CI 并通过；
- 手动：在 `~/Work/iota` 和它的一个 herdr worktree 里先后启动同一个 bot，`/status` 的 `Memory` 行指向同一个文件。现状下这两个地方会被当成两个项目（`iota` 和 worktree 名）；
- Q6 的行为脚本跑一轮，记下路由准确率。它不阻塞合并，但没有这个数据，§8.3 第二行就一直是「无证据」，所有者复议时应当看到它。

**第二步**（L2 排期时）：notes 按两级布局；`recall` 加 `project` 参数（§5）；Q5 的 L2 部分。

**不在任何一步里的**：仓库内共享记忆（§2.1 已否决）；按分支的作用域（等 Q8 的数据）；键缓存（等 Q10 的数据）；自动迁移（§7，信息不够，做不了）。
