# Bot 记忆作用域调研：按项目、按身份，还是分层

Status: **Research** · 日期：2026-10-07 · 用途：为 iota `mode: bot` 的长期记忆该按什么键分作用域（项目、bot 身份，还是两层）提供同行做法、记忆系统模式与证据

本文只做调研，不含设计决定。第 10 节列出三个选项及其代价，但不做推荐。

**iota 现状**（[IO1] §3.1–3.6，实现见 [IO2]）：一个 bot = 一个身份 + 一条永不结束的会话；长期记忆在 `~/.iota/bots/<bot 名>/`，**按 bot 名存，不按项目目录存**。目前实现的只有 `MEMORY.md` 常驻层（8 KiB）；设计里的 `notes/*.md` 检索层和 `recall` 工具属于 L2，**代码里还没有**（[IO1] §3.3；仓库中 `notes/` 只以记忆正文里的字符串出现）。项目维度是 `MEMORY.md` 里的 `## Project: <目录名>` 小节，注入时按当前项目裁剪；`User`、`Open threads`、前言和未知小节对所有项目可见。

「当前项目」取的是项目根的目录名（`src/repl/run.rs:291` `memory_project`），项目根由 `src/agents/mod.rs:31-44` `project_root` 从 cwd 向上找、**遇到 `.git` 就停，目录和文件都算**。所以在 linked worktree 里（它的 `.git` 是文件），`## Project:` 拿到的是 **worktree 自己的目录名**，不是主仓库名：例如本文写作所在的 worktree `~/.herdr/worktrees/iota/mem-archive` 会被认成项目 `mem-archive`，而不是 `iota`。同一仓库的每个 worktree 因此各占一个 `Project:` 小节（下文称「worktree 碎片化」）。两份原始调研都没有提到这一点，是合并时补充的；行为与 Qwen Code、Aider 取 worktree 自己的 git root 相同（§2.2）。设计上「不做按项目的独立记忆文件」，理由是项目知识应进 AGENTS.md（人维护、可 review、进 git）。

本文合并了两份独立调研：一份是**产品横向调研**（19 个 coding agent 各自怎么做），一份是**系统与模式调研**（专用记忆系统、官方 API 做法、学术与工程证据、公开失败记录）。合并时按「作用域键」重新组织，不按原报告分章；两份在同一产品上的结论已去重，说法不一致的地方单列在第 11 节。

## 写作约定

**来源标记**（两份原稿的标记已统一成下面一套）：

| 标记 | 含义 |
|---|---|
| 【官方】 | 厂商文档、官方博客、帮助中心、changelog、官方 PR，或厂商员工在官方论坛的回复。产品调研原稿把读到的官方仓库源码也标为【官方】，写成「【官方】`文件:行号`」；本文保留这种写法 |
| 【源码】 | 本次读取的实现代码，与「【官方】`文件:行号`」同义。行号以「版本基线」里的 commit 为准 |
| 【论文】 | 研究论文的结果 |
| 【报告】 | 公开的使用者记录（issue、discussion、论坛帖），不是厂商表态 |
| 【媒体】 | 第三方报道 |
| 【非官方】 | 逆向分析或社区总结 |
| 【本机】 | 在本机 `~/.claude`、`~/.codex`、`~/.copilot` 下的只读观察（原稿有的写作「本机验证」，已统一） |
| 【推断】 | 调研者自己的架构分析，不是任何来源的原话 |

**查不到的怎么写**：查了但没有找到的写「**未找到**」；有线索但来源不足以支撑的写「**未证实**」。不猜。两类都汇总在附 A。

**证据等级**：失败记录与先例核查另外标注证据强度（例如「【报告】closed/not_planned，未复现」「【文档明确】」「最接近的已实现先例，未找到效果实验」），见第 7、8 节；第 6.4 节专门说明哪些材料能证明什么、不能证明什么。

**引用编号**：全文只有一套编号，形如 `[CC1]`，前缀表示来源所属的产品或系统（`CC` Claude Code、`CX` Codex、`GH` GitHub Copilot、`PA` 论文、`FR` 失败记录……），完整列表与每条的来源类型在附 B。两份原稿里指向同一页面的链接已合并成一个编号（例如 Claude Code memory 文档、Codex Memories 文档、Copilot Memory 概念页），附 B 的「出处」列注明「两份共用」。产品详情各节开头有一行「本节简称」，把正文里的「memory 页」「concept 页」之类简称对应到编号。源码引用写成 `文件:行号`，不另编号，仓库与 commit 见版本基线。

**术语**：

- 「规则文件」= 人写的指令文件（CLAUDE.md / AGENTS.md / rules / steering）；「记忆」= 模型在使用中自动积累的内容。两者分开说，因为作用域取舍对两者并不相同（见 §3.6）。
- 「项目」一词在各家的含义不同：启动目录的绝对路径、git root、远程 URL、GitHub 仓库身份都有。各处都写明是哪一种（汇总见 §2.2）。
- 「默认」区分产品默认、SDK 参数默认和教程示例，不混为一谈。

**核对情况**：产品调研的链接在 2026-10-06～07 由五个并行调研 agent 打开并核对，开源项目以源码为准；系统与模式调研在 2026-10-06～07 读取，研究开始与核对时 iota 工作树干净、HEAD 为 `9022f72`，读过 `BRAIN.md`、brain 的 `bot-mode` 页面、`docs/design/bot-mode.md` 与相关源码。本文合并时（2026-10-07）对附 B 的全部外链重新做了一次可达性探测，结果写在附 B 开头。developers.openai.com/codex/* 已 308 重定向到 learn.chatgpt.com/docs/*，下文用重定向后的地址；help.openai.com 对直接抓取返回 403，是经浏览器读取的原文。

**每个 coding agent 回答同样六个问题**（第 4 节各产品的小标题即这六个）：

| 编号 | 问题 |
|---|---|
| ① | 有没有持久记忆，形态是什么 |
| ② | 存在哪（本地路径 / 仓库内 / 云端） |
| ③ | **作用域键**：项目目录、用户全局、agent 实例、会话，还是分层 |
| ④ | 谁写：模型自动 / 用户手写；有无审批与可见性 |
| ⑤ | 怎么进上下文：常驻注入还是按需检索；上限与裁剪 |
| ⑥ | 同一个 agent 换项目会怎样；官方有没有讨论跨项目（worktree / monorepo / 多 clone） |

## 版本基线

### Coding agent

| 产品 | 版本 / 日期 | 核对方式 |
|---|---|---|
| Claude Code | 2.1.291（2026-10-06） | 闭源：code.claude.com 文档、仓库 CHANGELOG、本机 `~/.claude/projects/` 只读观察 |
| OpenAI Codex CLI | 0.160.1；源码 openai/codex `57d57df`（2026-10-06）；归并模板 v1 另读 `551bd409` | 完整浅克隆读源码；learn.chatgpt.com 文档；ChatGPT 帮助中心（≈2026-09-19 更新，经浏览器读取） |
| GitHub Copilot CLI | 1.0.92（2026-10-05） | 闭源：docs.github.com、GitHub Changelog、工程博客 2026-01-15；仓库只含 changelog |
| Cursor | 文档无版本，changelog 至 2026-09-23 | 闭源：现行文档 + Wayback 2025-07/08 快照 + 论坛 staff 回复 |
| Windsurf → Devin Desktop | docs.devin.ai 2026-10-06；Wayback 2025-05/09/12、2026-02 | 闭源：文档对照快照 |
| Trae / TraeCode | docs.trae.ai、docs.trae.cn 2026-10-06（页面无日期） | 闭源：文档 + 官方论坛 |
| Kiro / Amazon Q Developer | Kiro 文档 2026-08～10；Q CLI 源码 aws/amazon-q-developer-cli `15cc8f3`（2026-04-23，已停止维护） | 源码只能证实 Q CLI 时代路径；Kiro CLI 闭源靠文档 |
| Cline | 4.1.22；cline/cline `cd80a20e`（2026-10-06） | 完整浅克隆；`@cline/shared` 0.0.90 读 npm dist |
| Roo Code | 3.53.0（main 2026-10-07） | raw 文件 + `gh api` 目录列表/代码搜索（未完整克隆） |
| Aider | 0.86.3.dev（main 2026-10-07）；release v0.86.0（2025-08-09） | 同上 |
| Continue | 1.3.40（main 2026-10-07）；release v2.0.0-vscode（2026-06-19） | 同上 |
| Gemini CLI | main `fb972b2f`（2026-10-02）；release v0.62.0（2026-09-29）；对照 tag v0.39.0 | 完整浅克隆 |
| Qwen Code | 0.25.0；`28e0f3a8`（2026-10-06） | 完整浅克隆 |
| OpenCode（sst） | 1.18.34；`3f393d78`（2026-10-06）；Go 版 opencode-ai/opencode 已归档 | 完整浅克隆 |
| pi | 1.0.4（2026-10-05）；`311f0e02`；仓库已迁 earendil-works/pi | 完整浅克隆 |
| Devin | docs.devin.ai 2026-10-06；Release Notes 至 2026-10-05 | 闭源：文档 |
| OpenHands | software-agent-sdk 1.53.0 `aae9c437`（2026-10-06）；V0 以 tag 0.62.0 为准；enterprise 仓 2026-10-06 | 完整浅克隆 |
| Goose | `540df77c`（2026-10-06）；release v1.53.0（2026-10-02） | 读源码 |
| Amp | docs 2026-10-06；news 至 2026-09-29 | 闭源：全部 docs + 153 条 news |

### 记忆系统、官方 API 与论文

系统与模式调研原稿没有单列版本表；下表的 commit 取自其引用的源码链接，文档均在 2026-10-06～07 读取。

| 对象 | 版本 / 日期 | 核对方式 |
|---|---|---|
| MemGPT | arXiv 2310.08560（2023） | 论文 [PA1] |
| Letta v1 SDK | 文档现标 legacy | 文档 [LE1][LE2][LE3] |
| Letta Harness / MemFS、Letta Code | 当前文档；letta-code `4b028fab` | 文档 [LE4][LE5][LE6]；提示模板源码 [LE7]；工程博客 [LE8] |
| Mem0 Platform / OSS | 当前文档；mem0 `c93420c4`；论文 arXiv 2504.19413v1（2025） | 文档 [ME1][ME2][ME4]；源码 [ME3]；论文 [PA2] |
| Zep / Graphiti | 当前文档；graphiti `689de295`；论文 arXiv 2501.13956v1 | 文档 [ZE1][ZE2][ZE3]；源码 [GR1][GR2]；论文 [PA4] |
| LangMem / LangGraph Store | 当前文档 | 文档 [LM1]–[LM4] |
| Cognee | cognee `b32d8afc` | SDK 源码 [CG1]；MCP README [CG2] |
| A-MEM | A-mem `ceffb860`；论文 arXiv 2502.12110v1 | 源码 [AMM1]；论文 [PA3] |
| Anthropic Memory Tool | 当前 API 文档 | [AN1]；工程博客 [AN2] |
| OpenAI Agents SDK / Cookbook | 当前文档 | [OA1][OA2] |
| AGENTS.md 约定 | agents.md 官网（AAIF / Linux Foundation 托管） | [AG1] |
| 其他论文 | LongMemEval 2410.10813v1；Evaluating AGENTS.md 2602.11988v1；MemoryGraft 2512.16962v1 | [PA5][PA6][PA7] |
| 公开失败记录 | 各 issue / 帖子的发布日期与报告版本见 §7 | [FR1]–[FR7] |

---

## 1. 总表与关键发现

### 1.1 Coding agent 总表

按第 3、4 节的分组排列。「作用域键」一列是核心；「项目」后面括号里写的是该产品对「项目」的具体定义。每行的来源在「详见」那一节。

| 分组 | 产品 | 记忆形态 | 存在哪 | **作用域键** | 谁写 | 注入还是检索 | 大小上限 | 详见 |
|---|---|---|---|---|---|---|---|---|
| 单层项目 | Claude Code 2.1.291 | 自动记忆：`MEMORY.md` 索引 + 主题 md（frontmatter `type`）；规则：CLAUDE.md 家族 / `.claude/rules` / AGENTS.md 兜底 | `~/.claude/projects/<绝对路径转义>/memory/`，本机、不同步 | **项目（git 仓库）**：worktree/子目录共用；非 git 用项目根；**无用户全局自动记忆**；子代理另按 agent 名 + `memory:` scope | Claude 写，无审批；`/memory` 看/改/删/关 | 索引常驻；主题文件按需 Read | 索引前 200 行或 25 KB | §4.1.1 |
| 项目＋用户 | Qwen Code 0.25.0 | 自动（默认开）：project / user / team 三层 Markdown（`MEMORY.md` 索引 + user/feedback/project/reference 主题文件 + `pinned/`），Dream 每日合并；可选结构化召回 + `search_memory`；外部 Mem0；规则 `QWEN.md` / AGENTS.md / `.qwen/rules` | `~/.qwen/projects/<sanitized git root>/memory/`、`~/.qwen/memories/`、`<repo>/.qwen/team-memory/` | **项目（git root 绝对路径，linked worktree 各自一份，可切 cwd）+ 用户全局 + team（仓库内，进 git）**；会话却按 cwd | 模型每轮后台自动写，不审批；team 层默认询问 + 秘密扫描；`/remember` `/forget` `/dream` | 索引常驻（system 尾部）+ 每轮异步相关召回（≤ 5 篇，等 100 ms）+ 可选 `search_memory` | 索引 200 行 / 25,000 字符 / 每行 150；召回正文 1,200 字符 | §4.2.1 |
| 项目＋用户 | Gemini CLI 0.62（≥0.40 四层记忆） | 记忆：私有项目 `MEMORY.md` + 同目录 md、全局 `~/.gemini/GEMINI.md`（兼作全局规则）；实验性 Auto Memory 补丁收件箱（默认关）；规则 `GEMINI.md` 层级 | `~/.gemini/tmp/<slug>/memory/{MEMORY.md,.inbox/,skills/}`；`~/.gemini/GEMINI.md` | **项目（启动目录绝对路径 → basename slug，旧版 sha256(path)）** + 用户全局；无 agent 键 | 记忆：模型按系统提示里的路由规则用 edit/write 写（走工具审批）；Auto Memory 只产补丁，用户在 `/memory inbox` 逐条批准 | 常驻三级：system（全局 + MEMORY.md）/ 首条 user 消息（项目 GEMINI.md）/ 工具输出 JIT（子目录） | 未找到（import 深度 5） | §4.2.2 |
| 项目＋用户 | OpenHands SDK 1.53.0 | 自动（opt-in，默认关）：两层 `MEMORY.md` + 每日日志；Cloud：用户表一列 `memory_context`；规则 AGENTS.md / repo.md / SKILL.md | `~/.openhands/memory/MEMORY.md` + `<workspace>/.openhands/memory/MEMORY.md` | **OS 用户 + 项目（workspace 绝对路径）** 两层；Cloud = 用户账号一份、不分仓库 | agent 自写，无审批，包在 UNTRUSTED_CONTENT 里；文件可人改/可 commit | 两层索引常驻注入 `<MEMORY_CONTEXT>`；日志按需读 | 两层合计 6,000 字符，顶部整行截断 | §4.2.3 |
| 项目＋用户 | Goose 1.53.0 | 自动：Memory extension（非默认开）`<category>.txt`；规则 `.goosehints` / AGENTS.md | 全局 `~/.config/goose/memory/`、本地 `<cwd>/.goose/memory/` | `is_global` 二选一：**用户 config dir vs 项目（工作目录绝对路径）**；无 agent 维度 | 模型用工具写（提示要求先确认，非硬审批）；明文可改 | 全局记忆全量进 system prompt；本地只能工具检索 | 无 | §4.2.4 |
| 项目＋用户 | Trae / TraeCode | 自动记忆：`user_profile.md` + `project_memory.md`；规则 `~/.trae/user_rules` + `.trae/rules/*.md` | `~/.trae/memory/user_profile.md`、`~/.trae/memory/projects/{project_path}/project_memory.md`（国内版 `~/.trae-cn/`），本地、不跨设备 | **用户全局 + 项目（`{project_path}`，编码未证实）** 两层 | AI 自动建/改/删 + 用户直接编辑；无审批 | 未证实 | 未找到 | §4.2.5 |
| 仓库＋账号 | Copilot CLI 1.0.92 / Copilot Memory | `{subject, fact, citations, reason}` 条目；规则：`.github/copilot-instructions.md`、`*.instructions.md`、AGENTS.md 等 | GitHub 服务端（Repo Settings、`github.com/settings/copilot/memory`） | **GitHub 仓库（owner/repo）+ GitHub 账号** 两层；本地路径无关 | agent 调 `store_memory`，CLI 每次弹确认并标 scope；28 天未用过期 | 会话开始注入，读时按 citation 校验，30 分钟刷新 | 未找到 | §4.2.6 |
| 用户全局 | Codex CLI 0.160.1 | 自动记忆（默认关）：SQLite + `memory_summary.md` / `MEMORY.md` / `rollout_summaries/`；规则：AGENTS.md 链 | `$CODEX_HOME/memories/`（默认 `~/.codex/memories/`） | **用户全局**；项目只是摘要里的 `### <project scope>` 分组，注入时不按 cwd 裁剪；线程级可关 | 后台两阶段流水线写；用户只能让模型追加 ad_hoc note | 摘要作为 developer 片段每线程常驻；其余「quick memory pass」或工具检索 | 摘要 ≤ 10,000 字节（8,900 字节分片）；AGENTS.md 32 KiB | §4.3.1 |
| 账号 | ChatGPT 记忆 | saved memories + chat history 派生 + memory summary | OpenAI 云端 | **账号**；Projects 可 project-only，共享项目强制 project-only | 模型自动 + 用户显式；设置页可看/删 | 按相关性取用 | 未找到 | §4.3.1 |
| 用户×组织 | Devin（docs 至 2026-10-05） | 自动：Memory drive（Git 仓里的 Markdown，`MEMORY.md` + 主题文件，Dreaming 每日整理，2026-10-05 新上）；半自动 Knowledge（已 deprecated → Skills plugin）；规则 AGENTS.md / SKILL.md | 全在 Cognition 云端，app 内只读 | Memory = **用户 × 组织**，仓库只是 agent 自己分的文件夹；Knowledge/Skills = org/enterprise/personal + 可 pin 到仓库；blueprint knowledge = 仓库/子目录 | Memory：Devin 自动写、不可手改、可关；Knowledge：人 + Devin 建议需确认 | `MEMORY.md` 常驻，其余 note 按需；Knowledge 按 trigger 检索 | Memory 未找到；AGENTS.md 16 KiB；企业 Knowledge 300 条 | §4.3.2 |
| agent 实例（知识库） | Kiro IDE/CLI（Q CLI 源码 2026-04） | **无自动记忆**；steering / rules 人写；agent JSON `resources`；`/knowledge` 显式知识库 | `~/.kiro/`、`.kiro/`（旧 `~/.aws/amazonq`、`.amazonq`）；知识库 `~/Library/Application Support/kiro-cli/knowledge_bases/<agent>` | 用户全局 / **cwd** 工作区 / **agent 实例**（知识库键 `{agent 名}_{配置路径哈希}`）/ 会话 | 人写；模型可经 `knowledge` 工具增删 | resources 每轮拼进伪 user 消息常驻；knowledge 工具检索 | 上下文文件 ≤ 75% 窗口，超限整文件丢弃 | §4.4.1 |
| 用户 | Kiro Web / Kiro Crew（附） | Web：从 PR 反馈自动学习；Crew：六层记忆 | Web 云端；Crew `~/.kiro/crew/workspace/memory/` + SQLite | Web **按用户、跨所有仓库**；Crew 按用户 | 自动；Web 只能删 | Web 未说明；Crew 开局注入 + 逐条检索 episodic | Crew 各层字符上限 | §4.4.1 |
| 已下线 | Cursor（changelog 至 2026-09-23） | 自动 Memories 2025-06～2025-11 存在，**2.1 起删除**；规则：Team / `.cursor/rules/*.mdc` + AGENTS.md / User / `~/.cursor/rules` | 规则在仓库/设置/dashboard；旧 Memories 位置未证实（Privacy Mode 下禁用） | 规则：组织 / 工作区目录 / 用户；旧 Memories：**项目 × 用户** | 规则人写；旧 Memories sidecar 自动 + 保存前审批 | 规则「start of the model context」；Memories 未证实 | 无硬限，建议 < 500 行 | §4.5.1 |
| 已下线 | Windsurf → Devin Desktop（2026-06） | legacy Cascade 自动 memories；新默认 agent Devin Local **不持久化记忆**；规则 `global_rules.md` / `.devin|.windsurf/rules` / AGENTS.md | `~/.codeium/windsurf/memories/`，本地、不进仓库 | memories 按 **workspace**（键形式未证实）；规则：用户 / 目录（向上到 git root）/ 机器 | Cascade 自动或按要求写，可编辑；无审批 | memories 按需检索；规则按 trigger | global 6,000 字符；workspace 每文件 12,000（旧「总 12,000」已取消） | §4.5.2 |
| 无自动记忆 | Cline 4.1.22 | **无自动记忆**；Memory Bank 只是 docs 里的 prompt 模式（仓库内 `memory-bank/*.md`）；规则 `.clinerules` / `.cline/rules` / AGENTS.md + 全局 Rules | 工作区目录；`~/Documents/Cline/Rules`、`~/.cline/rules`；`~/.cline/data/workspaces/<hash(路径)>/` | 工作区**绝对路径** + 用户 home + orgId（远程规则）；无身份层 | 规则人写（模型可当普通文件写）；历史自动 | 规则每请求常驻 | 无 | §4.6.1 |
| 无自动记忆 | Roo Code 3.53.0 | **无自动记忆**；Memory Bank 为社区方案；Qdrant 代码索引（非记忆）；规则 `.roo/rules*` + `~/.roo/rules*` + AGENTS.md | `cwd/.roo`、`~/.roo`；索引 collection `ws-<sha256(路径)[:16]>` | 用户 home × cwd 绝对路径 × **mode slug**（`rules-{mode}` 按 agent 身份挂规则） | 规则/modes 人写；索引自动 | 规则每请求常驻（固定 9 段）；`codebase_search` 按需 | 无 | §4.6.2 |
| 无自动记忆 | Aider 0.86.x | **无**；`.aider.chat.history.md` 默认不回读；规则 = `--read CONVENTIONS.md` | git root（无 git 则 cwd）；`~/.aider.conf.yml` | **git root** + 用户 home；无身份层 | 转录自动追加；规则人写 | 只读文件 + repo map 每请求常驻；回读的历史立即摘要 | 历史 token 软上限 1024～8192 | §4.6.3 |
| 无自动记忆 | Continue 1.3.40 | **无自动记忆**；模型可用 `create_rule_block` 写 `.continue/rules/*.md`（最接近模型写记忆）；sqlite + lancedb 代码索引 | 工作区 `.continue/`；`~/.continue/{rules/,sessions/,index/}`；Hub 云端 | 工作区目录 + 用户 home + Hub 账号/组织 + 文件 glob；索引 = (目录, git 分支) | 规则人写或模型写；会话/索引自动 | 规则拼进 system message；`alwaysApply:false` 的由 `request_rule` 按需 | 无 | §4.6.4 |
| 无自动记忆 | Amp（news 至 2026-09-29） | **无自动记忆**；云端 thread 可搜索/`@T-…` 引用/Handoff；规则 AGENTS.md 多层 + skills | thread 在 ampcode.com；AGENTS.md 在仓库/`~/.config/amp`/系统/云端 Global | AGENTS.md = 目录层级 + 用户 + workspace；thread = Project（仓库 URL）× Owner | AGENTS.md 人写（Amp 只在被要求时写）；thread 自动记录 | AGENTS.md 常驻；thread 仅在引用/搜索时读 | 未找到 | §4.6.5 |
| 无自动记忆 | OpenCode 1.18.34 | **无内置记忆**（第三方 supermemory 插件）；规则 AGENTS.md / CLAUDE.md + `instructions` | `~/.config/opencode/AGENTS.md`、仓库内；会话 `~/.local/share/opencode/opencode.db` | 项目 = **git 远程 URL 的 hash**（无远程→根提交 sha；非 git→global）；规则 = cwd 向上到 git root；用户 = `~/.config/opencode` | 人写（`/init` 一次性生成） | 常驻 system prompt；子目录 AGENTS.md 随 read JIT | 未找到 | §4.6.6 |
| 无自动记忆 | pi 1.0.4 | **无内置记忆**（会话 JSONL + compaction）；规则 AGENTS.md / CLAUDE.md / SYSTEM.md / APPEND_SYSTEM.md | `~/.pi/agent/`；会话 `~/.pi/agent/sessions/--<cwd 编码>--/` | 规则 = agent dir + cwd 全部祖先（无 git 边界，worktree 去重）；会话 = 精确 cwd | 人写 | 常驻 system prompt | 未找到 | §4.6.7 |

### 1.2 记忆系统与官方 API 总表

Coding agent 之外的专用记忆系统和官方 API。Codex、Claude Code、Copilot 也出现在系统与模式调研的总表里，与 1.1 重复的行已并入 1.1 与第 4 节，这里不再列。

| 系统 | 作用域键与默认 | 是否分层 | 写入策略 | 检索／注入策略 | 来源 |
|---|---|---|---|---|---|
| MemGPT（2023 论文） | 单个有状态 agent 的记忆；没有规定统一的项目键或多租户 API | 主上下文／外部 recall、archival；用户与 persona 位于工作上下文 | 模型调用函数改工作记忆、写档案；运行时自动记消息，容量告警促使整理 | 常驻工作记忆；模型显式检索外部资料 | 【论文】[PA1] |
| Letta v1 SDK（现标 legacy） | `agent_id`、独立 `block_id`／label；通常给 agent 配 human、persona，**是推荐而非强制模式** | 常驻 blocks／archival／消息历史；block 可多 agent 共享 | 模型 memory tools 或开发者 API；block 默认可写，可设只读 | 已附加 block 常驻；archival 语义查询，可按 tags 限定 | 【官方】[LE1][LE2][LE3] |
| 当前 Letta Harness / MemFS | `agent_id` 拥有一个记忆 Git 仓库；`conversation_id` 分消息线程，线程共享身份与记忆 | 常驻文件／索引／按需文件；可附加共享记忆仓库 | agent 文件操作＋提交；可配置 dreaming 后台整理 | 常驻核心＋目录索引；按需文件搜索；默认没有向量索引 | 【官方】[LE4][LE5][LE6] |
| Mem0 Platform | `user_id`、`agent_id`、`app_id`、`run_id`；外层服务 `org_id/project_id`；未传实体字段为 null | 多维标签与过滤组合；不自动继承父层 | 应用调 `add`，服务抽取；当前默认抽取按发言者区分 user／agent 归属；可直接导入 | 先指定实体／metadata filters，再相关性排序；应用决定注入 | 【官方】[ME1][ME2] |
| Mem0 OSS | `user_id`／`agent_id`／`run_id` 至少一个；另有 collection、metadata；没有默认「当前代码项目」 | 可组合键／过滤；不是现成项目层级 | 应用触发、LLM 抽取；`infer=False` 原文入库 | 向量后端、关键词能力及可选 reranker；实体条件由调用者给出 | 【源码】[ME3]、【官方】[ME2] |
| Zep 服务 | `user_id` 对应 User Graph；`thread_id` 是输入会话；另有 `graph_id` 表示共享／项目等 Context Graph | 用户摘要、事实、实体、episodes 等；用户图＋独立领域图 | 应用提交消息／数据，服务抽取实体、关系、时间及摘要 | 用户线程默认从整个用户图找相关内容；共享图明确指定 `graph_id` | 【官方】[ZE1][ZE2][ZE3] |
| Graphiti OSS | `group_id` 图分区；检索 `group_ids`；默认 group 通常空串，FalkorDB 为 `_` | 图结构与时间分层；多个分区可组合查询；不自带 user→project 继承 | 应用 `add_episode`；LLM 抽取、实体消歧、关系时效处理 | 混合搜索及重排；应用传分区，不自动从 cwd 推导 | 【源码】[GR1][GR2]、【论文】[PA4] |
| LangMem / LangGraph Store | namespace tuple＋条目 key；`thread_id` 管短期会话；namespace 由开发者配置，可含 user、agent、org、project | 可多级 namespace；semantic／episodic／procedural；profile／collection | agent 热路径工具，或后台抽取、合并；应用选择触发方式 | namespace 内搜索／按键读取；模型工具或应用预取 | 【官方】[LM1][LM2][LM3] |
| Cognee | 核心 SDK：user＋dataset，默认用户和 `main_dataset`；MCP 当前默认按客户端分 dataset；可加 session | dataset 隔离、session cache／长期图；权限可分配 | 应用 add/cognify 或 agent `remember`；抽取图结构，可后台处理 | dataset 内图／向量检索；MCP 可先 session 再长期图 | 【源码】[CG1]、【官方】[CG2] |
| A-MEM | 研究算法没有 user／agent／project namespace 契约；公开库默认 `memories` collection＋内存字典 | 链接笔记、语义网络；不是权限／租户层级 | 调用者 `add_note`，模型生成描述、关键词、关联及演化 | 向量相似＋关联笔记；未找到内建项目隔离 | 【论文】[PA3]、【源码】[AMM1] |
| Anthropic Memory Tool | 客户端映射的 `/memories`；真实 user／agent／project 键由应用决定 | 文件目录可分层；不规定统一 schema | Claude 发起 view/create/edit/delete 等，应用执行 | 开始任务检查目录，按需读取文件；不是默认全文注入 | 【官方】[AN1] |
| OpenAI Agents SDK / Cookbook | Session 与跨运行 memory 分开；示例为用户 state；sandbox 默认 workspace 内 memory layout，应用可隔离布局 | profile＋session/global notes，或 summary／handbook／rollout | 工具采集候选、运行后归并；sandbox 关闭后提取与归并 | 状态注入或 summary 常驻＋手册按需；复用需保留存储 | 【官方】[OA1][OA2] |
| AGENTS.md / 同类规则 | 仓库根、子目录；用户级路径由各产品定义，**没有统一全局路径** | 通常 user→repo→directory；加载／覆盖语义各异 | 主要由人管理，也可让模型起草；无统一自动学习协议 | 启动加载或触碰目录时加载；不是语义记忆搜索 | 【官方】[AG1][CX3][CC1][CU2] |

### 1.3 关键发现

每条后面是证据所在的小节；【推断】是调研者的分析，不是来源原话。

1. **没有一个统一的「正确」作用域键。** 成熟系统至少有五个独立维度（所有者、适用范围、存储分区、读取视图、生命周期），各家做了不同的已实现选择：Letta 把所有者设为 agent，Zep 把用户长期知识与 thread 分开，Copilot 同时设用户偏好和仓库事实，LangMem 把 namespace 交给应用（§2.1）。【推断】仅把文件移到 `projects/` 下不足以证明隔离更强；仅按 bot 保存也不意味着必须全文全局注入。
2. **有自动记忆的 coding agent 里，「项目 + 用户」两层最常见**：Qwen Code、Gemini CLI、OpenHands SDK、Goose、Trae、Copilot 六家（§3.4）。单层只按项目的只有 Claude Code（git 仓库，所有 worktree 共用，§4.1.1）；单层只按用户的只有 Codex（`$CODEX_HOME`，§4.3.1）。
3. **「项目」至少有五种定义，各家不同**：git 仓库（Claude Code，worktree 共享）、git root 绝对路径（Qwen，linked worktree 各自一份）、启动目录绝对路径（Gemini、OpenHands、Goose、Cline、Roo、Continue、pi）、git 远程 URL 的 hash（OpenCode）、GitHub 仓库身份 owner/repo（Copilot）（§2.2）。路径键的失效已有公开报告：改名目录后旧记忆不再加载、worktree 下学到的东西回不到主仓库（§7）。
4. **主会话记忆按「命名 agent 实例」存的，在产品调研覆盖的 19 个 coding agent 里没有。** 按实例存的只有 Kiro CLI 的 `/knowledge` 知识库（显式添加，不是自动记忆）和 Claude Code 子代理记忆（§3.3）。但系统与模式调研找到了该清单之外的先例：**Letta（含 Letta Code）把全部记忆归 `agent_id` 所有**，项目只是挂载环境与内容里的引用（§5.1）。两份结论的边界不同，见 §11 分歧 3。
5. **iota 的「一份文件、按 `Project:` 小节、注入时裁剪」没有找到同款。** 最接近的是 Codex：用户全局一份记忆，内部按项目／任务分组并写明适用范围，但注入时不按 cwd 裁剪，由模型自判相关性（§4.3.1、§8）。广义的相邻先例充分：身份所有权下的一份长期库（Letta）、文件内标注适用范围（Codex）、从共享全集按当前上下文取视图（Zep）、可组合 scope 的检索（Mem0、LangMem）（§8）。**相似实现不等于 iota 方案已被证明有效。**
6. **两家主流 IDE 下线了自动记忆，同时另有几家在 2026 年新上或重做**：Cursor 2.1（2025-11）删除 Memories，让用户导出成 Rules；Windsurf 改名 Devin Desktop 后默认 agent 不再持久化记忆，文档写「prefer Rules or AGENTS.md」（§4.5）。反方向：Devin Memory（2026-10-05，用户×组织）、Gemini CLI v0.40.0（2026-04-28，四层）、Qwen Code（2026-04-16，project/user/team 三层 + Dream）、OpenHands SDK（2026-07-22，opt-in 两层）（§4.2、§4.3）。
7. **「`MEMORY.md` 索引常驻 + 主题文件按需」已成事实标准**：Claude Code、Qwen、Gemini（≥ 0.40）、OpenHands、Devin、Codex 六家都是这个形态，Devin 把它写成了开放规范 Agent Memory Repo [DV11]。常驻索引的上限在 6,000 字符（OpenHands 两层合计）到 25 KB（Claude Code、Qwen）之间，iota 的 8 KiB 落在这个区间（§3.4）。
8. **双层不是免疫方案，失败集中在「写入归属」和「读取越界」**：Copilot 有用户报告某客户的 Terraform 习惯被当作个人偏好带进无关仓库 [FR5]；Cursor 多根 workspace 的根 AGENTS.md 被全局加载 [FR4]；Cognee 文档明说关闭后端隔离后图遍历会跨 dataset [CG2]（§7）。【推断】失败分四类——身份解析、写入归属、读取越界、内容陈旧／污染——独立文件主要帮助组织和独立生命周期，不自动消除后三类。
9. **没有找到直接对照实验。** 本次未找到控制其他变量、比较「同一个 coding agent 按身份全局保存／按项目独立保存／混合保存」的成熟实证研究（§6.4）。已有论文（MemGPT、Mem0、A-MEM、Zep、LongMemEval）比较的是上下文驻留、记忆粒度、检索方式，不是项目作用域；AGENTS.md 实验显示多塞规则会增加成本、效果依内容而变 [PA6]。
10. **把项目知识交给规则文件有多家先例**：OpenHands 未开记忆时让模型把仓库根 AGENTS.md 当记忆；Continue 的 `create_rule_block` 把学到的东西写成 `.continue/rules/*.md`；Devin 建议 Skill 并给「Create PR」；Gemini 的路由规则把团队约定写进仓库 `./GEMINI.md`；Amp 只在用户要求时更新 AGENTS.md；Codex 文档要求必需的团队指南放 AGENTS.md，记忆只当 recall 层（§3.6）。
11. **写入审批少见**：每次写都弹确认并显示 scope 的只有 Copilot CLI；Gemini Auto Memory 只产补丁、用户在 inbox 逐条批准；Cursor 旧 Memories 保存前需审批；Qwen team 层默认询问。Claude Code、Qwen project/user 层、OpenHands、Trae、Devin、Codex 都是模型直接写（§3.4）。
12. **「换项目会看到别的项目的记忆」在若干产品里是设计使然**：Codex 把所有项目分组一起注入；Kiro CLI 全局 agent 的知识库跨项目共用；Kiro Web 记忆「across all your repositories」；OpenHands Cloud 一个用户一份 `memory_context` 写进任意仓库的项目层路径（代码事实）；Goose 全局记忆无条件进所有会话；Devin Memory 跨仓库、换仓库时的过滤规则未证实（§7.3）。

---

## 2. 先把「作用域键」拆开

### 2.1 五个独立维度

**【推断】没有证据表明成熟系统统一选择 project 或 agent 作为唯一正确键。** 实际至少有五个独立维度：记忆所有者、适用范围、存储位置、当前召回条件、生命周期。Letta 把所有者设为 agent，Zep 把用户长期知识与 thread 分开，Copilot 同时设用户偏好和仓库事实，LangMem 将 namespace 交给应用——这些都是已实现的不同选择 [LE4][ZE1][GH1][LM2]。

| 维度 | 问题 | 对 iota 的对应 |
|---|---|---|
| 所有者／身份 | 这是哪个人、bot、组织拥有的长期状态？ | `<bot 名>`，不等于项目目录 |
| 适用范围 | 某条事实在哪些项目、工作流、分支成立？ | `User`、`Project: <名字>`、`Open threads` |
| 存储分区 | 一个文件、一组文件、数据库行还是独立数据库？ | 一个 `MEMORY.md`（＋设计中尚未实现的 L2 `notes/`） |
| 读取视图 | 这一轮先给模型看哪些内容？ | 项目小节裁剪；notes 索引与显式 `recall` 属于尚未实现的 L2 |
| 生命周期／来源 | 何时失效、如何覆盖、能否回到原始证据？ | 时间与来源标记、`.prev`、会话档案 |

此表是对产品模式的抽象，不是现成行业标准。**一个文件可以有多个逻辑 scope；多个文件也可以被无条件一起注入**（Codex 一份摘要内按项目分组 [CX2]；Goose 的全局记忆按 category 分文件但全量注入，§4.2.4）。因此，仅把文件移到 `projects/` 下不足以证明隔离更强；仅按 bot 保存也不意味着必须全文全局注入 [ME1][LM2][CX2][FR4]。

### 2.2 「项目」在各家的定义

| 「项目」的定义 | 自动记忆按它存的 | 只有规则 / 会话 / 索引按它存的 | worktree 与多 clone 的结果 |
|---|---|---|---|
| **git 仓库**（由仓库推导，worktree 与子目录共用；非 git 用项目根） | Claude Code（§4.1.1） | — | worktree 共享；多 clone 是否共享**未找到**（路径派生命名推测为两份） |
| **git root 绝对路径**（`.git` 文件也算根） | Qwen Code（§4.2.1）；iota 现状也属于这一类，只是取其 basename（见开头「iota 现状」） | Aider 的历史与缓存（§4.6.3） | linked worktree 各自一份（Qwen 官方明说，#6449）；多 clone 各自一份 |
| **启动目录 / workspace 绝对路径**（或其 hash、basename slug） | Gemini CLI（basename slug，旧版 sha256）、OpenHands SDK、Goose、Trae（`{project_path}`，编码未证实）、Windsurf legacy Cascade（键形式未证实） | Cline（hash）、Roo（sha256 前 16 位）、Continue（目录 + git 分支）、pi（精确 cwd）、Kiro 工作区 | 换路径即另一个项目；子目录启动在 Gemini 是另一个项目；同路径换了仓库内容会共享旧记忆（Gemini） |
| **git 远程 URL 的 hash**（无远程 → 根提交 sha；非 git → global） | — | OpenCode 的 project 记录与会话列表（§4.6.6）；Amp thread 的 Project（仓库 URL，§4.6.5） | 同一远程的所有 clone / worktree 共享 |
| **GitHub 仓库身份 owner/repo**（服务端，需写权限才能创建） | Copilot Memory 的 repository facts（§4.2.6） | — | 本地路径、clone、worktree 完全无关；fork 是否共享**未找到** |
| **仓库 × 用户**（物理键未证实） | Cursor 旧 Memories（2.1 起删除，§4.5.1） | — | 未找到 |
| **用户 × 组织**，项目只是 agent 自己分的文件夹 | Devin Memory（§4.3.2） | Devin blueprint knowledge 按仓库 / 子目录 | 未找到 |

官方专门讨论过 worktree 对记忆影响的只有三家：Claude Code 按 git 仓库共享（CHANGELOG 2.1.63）；Qwen 按 worktree 隔离，并明说「repository-wide conventions you want in every worktree belong in team memory」；OpenCode 的会话按远程 URL 共享。同一仓库多个 clone，没有任何一家讨论（§4.1.1、§4.2.1、§4.6.6）。

### 2.3 同名概念不要混用

Mem0 的服务 `project_id`、Zep 的服务 project 与 **iota 的本地代码项目**不是同一契约；Graphiti 的 `group_id` 也不天然表示人或项目；A-MEM 的 note UUID 是记录身份，不是访问作用域；LangMem 文档里的 namespace 也不自动等于安全权限 [ME1][ZE1][GR1][AMM1][LM2]。Cognee MCP 的「agent-scoping」指 MCP 客户端来源（Claude Code、Cursor 各一个 dataset），不应直接理解为 iota 的命名 bot 身份 [CG2]。Claude Code auto memory 的 frontmatter `type: user` 是分类，不是作用域（§4.1.1）。

---

## 3. 按作用域键看各家（横向）

本节只汇总第 4、5 节已有来源的事实，不另引新来源；括号里是详细出处所在的小节。

### 3.1 以项目为键

有模型自动写入的记忆、并以「项目」为键的，共 9 家（含两家已下线）：

| 产品 | 「项目」的定义 | 另有用户全局记忆层？ | 备注 |
|---|---|---|---|
| Claude Code | **git 仓库**：所有 worktree 与子目录共用一个 `~/.claude/projects/<repo 路径转义>/memory/`；非 git 用项目根 | **无**。`type: user` 的偏好也存在仓库目录里，不跟到别的仓库；跨项目只能靠人写 `~/.claude/CLAUDE.md` 或子代理 `memory: user` | 官方唯一明确「worktree 共享记忆」的；共享前后都有公开报告（§4.1.1、§7） |
| Gemini CLI ≥ 0.40 | **启动目录的绝对路径** → `projects.json` 里的 basename slug（旧版 sha256(path)）；不是 git root | 有：`~/.gemini/GEMINI.md`，同一文件兼作全局规则与个人记忆 | 子目录启动、另一 clone、`--worktree` 都是另一个项目（§4.2.2） |
| Qwen Code | **git root 的绝对路径**；`.git` 文件也算根，所以 linked worktree 各自一份；`QWEN_CODE_MEMORY_PROJECT_SCOPE=workspace` 可改成 cwd | 有：`~/.qwen/memories/`；另有 team 层 `<repo>/.qwen/team-memory/` 进 git | 会话却按 cwd 键，两者不一致（§4.2.1） |
| OpenHands SDK | **workspace 工作目录的绝对路径** | 有：`~/.openhands/memory/MEMORY.md` | Cloud 版退化为用户账号一份、注入到任意仓库（§4.2.3） |
| Goose | **会话工作目录的绝对路径**（`.goose/memory/`，不向上找 git root） | 有：`~/.config/goose/memory/`，写入时用 `is_global` 二选一 | 本地记忆不预载，只能工具检索（§4.2.4） |
| Trae | `~/.trae/memory/projects/{project_path}/`（编码未证实） | 有：`user_profile.md` | 注入方式未证实（§4.2.5） |
| Windsurf legacy Cascade | workspace（键形式未证实，【非官方】说按磁盘路径） | 无自动记忆的全局层（只有人写的 `global_rules.md`） | 2026-06 后默认 agent 不再持久化记忆（§4.5.2） |
| Cursor 1.0～2.0 | 「项目 × 用户」（物理键未证实） | 无 | 2.1 起删除，让用户导出进 Rules（§4.5.1） |
| Copilot Memory | **GitHub 仓库身份 owner/repo**（远程身份，不是本地目录；需写权限才能创建） | 有：GitHub 账号级 preferences | 本地 clone / worktree / 路径完全无关（§4.2.6） |

没有记忆、只有会话或规则，但「项目键」的定义值得参考的：OpenCode = **git 远程 URL 的 hash**（无远程 → 根提交 sha），同一远程的所有 clone / worktree 共享一个 project 记录（§4.6.6）；Amp thread = 仓库 URL × Owner（§4.6.5）；Aider = git root（§4.6.3）；Cline、Roo、Continue、pi = 路径字符串（§4.6.1、§4.6.2、§4.6.4、§4.6.7）。

记忆系统里，「项目」一般不是内建概念：Mem0 没有自动识别代码目录的默认行为，代码项目可由应用映射为 metadata 或某个业务维度 [ME1][ME2]；Zep 可以另建 `graph_id` 表示项目、组织、产品等 Context Graph，与用户图分开查询 [ZE2]；Graphiti 的 `group_id` 可以赋予项目含义，但写入、实体合并、搜索走对 scope 要调用者自己保证 [GR1]（§5.2、§5.3）。

### 3.2 以用户 / 账号为键

不按项目分目录、只有一个用户（或账号）级根的：Codex CLI（`$CODEX_HOME`，即按 OS 用户，§4.3.1）、ChatGPT（账号，Projects 可切 project-only，§4.3.1）、Devin Memory（用户 × 组织，§4.3.2）、Kiro Web（用户，跨所有仓库，§4.4.1）、OpenHands Cloud（用户表一列 `memory_context`，§4.2.3）。

这些产品的「身份」都是账号或 OS 用户，不是可以起名、可以并存多个的 agent 实例。项目信息只作为内容存在：Codex 在 `memory_summary.md` 里按 `### <project scope>` 分组、`raw_memories.md` 每线程记一行 `cwd:`；Devin 让 agent 自己按 `my-app/` 分文件夹，文档没有说明换仓库时是否只加载相关文件夹（未证实）。

记忆系统里同样常见：Zep 的默认用户记忆是**跨线程整合**，`thread.get_user_context` 返回整个用户图中与最近线程消息相关的部分，不限于该 thread 写入的内容 [ZE1]；OpenAI Cookbook 的个性化示例用应用自己的用户 state，冲突按「当前用户输入 → session overrides → global defaults」[OA1]（§5.3、§5.8）。

### 3.3 以 agent 身份为键

**Coding agent 里**：

- **Kiro CLI（含 Q CLI 源码）是唯一把「agent 实例」当一等作用域键的**：每个 agent JSON 自带 `resources`（常驻上下文），`/knowledge` 知识库目录键 = `{agent 名}_{配置文件路径哈希}`，文档写明「Each agent maintains its own isolated knowledge base… No Cross-Agent Access」。但它是**显式添加的知识库，不是自动记忆**；用全局 agent 时知识库跨项目共用，没有按项目隔离的选项（§4.4.1）。
- **Claude Code 子代理**：frontmatter `memory: user | project | local` 三选一，目录再按 agent 名（`~/.claude/agent-memory/<agent>/` 等）；`user` 作用域下同名子代理跨项目共享，`project`/`local` 是某项目内该 agent 的经验；官方建议 `project`，未配置时不凭空创建这一层，三种 scope 也不自动叠加。主会话本身没有身份键（§4.1.1）[CC2]。
- 只在**规则**层面按身份挂、不带记忆的：Roo Code `rules-{mode}`（全局与项目两处按 mode slug 叠加，§4.6.2）、Continue 的 assistant/profile（§4.6.4）、OpenCode `~/.config/opencode/agents/*.md`（§4.6.6）、Kiro agents（§4.4.1）。
- 产品调研覆盖的 19 个 coding agent 里，**没有一家把主会话的自动记忆按「命名 bot / persona」存**。前次调研 [IO3] 里的 Grok Bot 按 Bot 存记忆、OpenClaw / Hermes 按 workspace 存，它们不在本次清单里。

**记忆系统与清单之外的 coding agent 里**：

- **Letta**：当前 Letta 明确把记忆归属与会话分开，一名 agent 可拥有多个无限长 conversations，共用身份和 MemFS；CLI 按当前项目恢复最近 conversation，只是会话选择行为，不会把长期记忆改成项目所有；MemFS 由 agent 拥有、投射到当前机器，即「机器／cwd 只是记忆的挂载环境」[LE4][LE5]。**Letta Code** 的公开提示模板把所有记忆归于 `agent_id`，同时区分 `conversation_id`，并用 `projects/...` 等引用帮助找相关记忆 [LE7]（§5.1）。这是「身份所有权下组织多项目知识」的直接例子，也是产品调研的「没有一家」结论之外的反例，见 §11 分歧 3。
- **Mem0** 的 `agent_id` 是四个业务维度之一，用于 agent 行为／persona [ME1]；Letta v1 的 human / persona block 是推荐用法而非强制，同一 `block_id` 可挂到多个 agent [LE1][LE3]。
- **Hermes**（不在产品清单内）有运营者报告单 profile 全局记忆在 DM／群聊等场景之间共享，提出 tenant routing 方案 [FR6]：「agent 身份相同」不代表所有受众应共享记忆（§7）。

### 3.4 两层都有：怎么分、怎么注入

| 产品 / 系统 | 层 | 分层依据（谁决定一条记忆落哪层） | 注入时怎么处理 |
|---|---|---|---|
| Qwen Code | user / project(git root) / team(仓库内) | 记忆 `type` 路由：`user` 类型「always user (cross-project)」；team 层只收显式写入且默认询问 + 秘密扫描 | 索引常驻（200 行 / 25k 字符预算）+ 每轮异步召回 ≤ 5 篇（§4.2.1） |
| Gemini CLI | 全局 `~/.gemini/GEMINI.md` / 私有项目 `MEMORY.md` / 项目 `GEMINI.md` / 子目录 | 系统提示里的**路由规则**：团队约定 → `./GEMINI.md`；「我机器上、别提交」→ 私有项目记忆；「我一向喜欢 X、所有项目」→ 全局；两可就问用户；**一条事实只能落一层、不许跨层镜像** | 全局 + 私有项目索引进 system instruction，项目 GEMINI.md 进首条 user 消息，子目录 JIT（§4.2.2） |
| OpenHands SDK | user / project(workspace 路径) | 系统提示让模型「fold only durable, broadly useful facts into MEMORY.md」；哪层由模型选 | 两层一起注入，**共用 6,000 字符预算**，按层公平分，用户层在前、项目层在后（§4.2.3） |
| Goose | global / local(cwd) | 写入时的布尔 `is_global`，提示要求先向用户确认存哪层 | 全局全量进 system prompt；本地不预载、只能工具检索（§4.2.4） |
| Trae | `user_profile.md` / `project_memory.md` | 未证实 | 未证实（§4.2.5） |
| Copilot Memory | repo facts / user preferences | 条目自带 scope；repo facts 协作者共享、preferences 仅本人；CLI 存储时显示 `owner/repo` 或 `user scope` | 会话开始注入该仓库 + 该用户的记忆；仓库事实用前按当前分支校验（§4.2.6） |
| Devin | Memory（用户 × 组织）+ Knowledge/Skills（可 pin 到仓库）+ blueprint knowledge（按仓库） | 自动记忆**不按仓库分**（agent 自己按 `my-app/` 分文件夹，是约定不是键）；项目维度交给人审的 Knowledge/Skills 与 blueprint | `MEMORY.md` 常驻、其余 note 按需；换仓库时对主题文件的过滤规则未证实（§4.3.2） |
| Codex CLI | **单层**用户全局 | 项目只是 `memory_summary.md` 里的 `### <project scope>` 标题分组，由合并模板生成 | **整份摘要注入每个新线程，不按 cwd 裁剪**；模板让模型按「query 提到的 workspace/repo/path」自判相关性（§4.3.1） |
| Claude Code | **单层**项目（git 仓库） | 没有用户层可选；frontmatter `type: user` 只是分类 | 索引前 200 行 / 25 KB 常驻（§4.1.1） |
| Kiro CLI | 全局 steering / cwd steering / agent 知识库 | 无自动记忆；人写 steering 分两层，知识库按 agent | resources 常驻（≤ 75% 窗口），知识库工具检索（§4.4.1） |
| Letta Draft 案例 | agent-owned skills / project-owned skills | 程序性经验跟身份走，项目技能跟仓库走 | 未展开（§5.1）[LE8] |
| Mem0 / LangMem / Zep | 实体标签 / namespace / 用户图 + 领域图 | 应用决定写入标签与查询组合；Mem0 Platform 当前默认按发言者拆 user／agent 归属 | 应用显式组合查询；**不自动继承父层**（§5.2–5.4） |

事实小结：

- 「项目 + 用户」双层是 coding agent 里最常见的形态（Qwen、Gemini、OpenHands、Goose、Trae、Copilot 六家）；单层项目的只有 Claude Code，单层用户的只有 Codex。
- 把「项目」做成**同一份文件里的小节**而不是独立文件的，只有 Codex（`### <project scope>`）；它注入时不裁剪。iota 的 `## Project:` 小节 + 按当前项目裁剪，在 19 个 coding agent 里没有现成同款（另见 §8）。
- 双层产品里，决定「落哪层」的机制有三种：模型按系统提示里的路由规则（Gemini、OpenHands、Qwen 按 type）、写入参数（Goose `is_global`）、条目自带 scope 且用户可见（Copilot）。
- 有「团队 / 进 git」第三层的只有 Qwen（`team-memory/`，opt-in）；其他家的「可提交」只是文件碰巧放在仓库里（OpenHands `.openhands/memory/`、Goose `.goose/memory/`、Claude Code 子代理 `project` scope）。
- 记忆系统的分层是**查询时的组合**，不是自动继承：Mem0 只过滤 `user_id=alice` 会检索该用户下带不同 app/run 的记录，增加 AND 条件会缩小结果，不会自动把 user-only 的通用偏好补回来 [ME1]；Zep 一次 search 选 user_id 或 graph_id，合成个人与项目上下文由应用安排 [ZE2][ZE3]。

**常驻索引上限对照**：Claude Code 200 行 / 25 KB；Qwen 200 行 / 25,000 字符 / 每行 150；Codex 摘要 10,000 字节（8,900 字节分片）；OpenHands 两层合计 6,000 字符；Windsurf 全局规则 6,000 字符；Devin AGENTS.md 16 KiB；Kiro 上下文文件 ≤ 75% 窗口；Goose 无上限；Gemini 未找到。iota 的 8 KiB 落在这个区间内（第 4 节各产品⑤）。

**写入审批对照**：每次写都弹确认的只有 Copilot CLI（并显示 scope）；Gemini Auto Memory 只产补丁、用户在 inbox 逐条批准；Cursor 旧 Memories 保存前需审批；Qwen team 层默认询问。Claude Code、Qwen project/user 层、OpenHands、Trae、Devin、Codex 都是模型直接写、不审批；Goose 是提示级「先确认」（第 4 节各产品④）。

**后台整理任务**：Devin Dreaming（每日）、Qwen Dream（24 小时或脏改 10 次）、Codex Phase 2 consolidation（启动时、空闲 ≥ 6h 的线程）、Gemini Auto Memory（空闲 ≥ 3h、≥ 10 条用户消息）、Mem0 Platform Dream（Supersede/Merge 自动，Synthesis 需开启）、Letta dreaming（可配置）。Claude Code CHANGELOG 多处提到 memory extraction / recall，机制未证实（§4.1.1、§4.2.1、§4.2.2、§4.3.1、§4.3.2、§5.1、§5.2）。

### 3.5 规则文件这一支：AGENTS.md

**【官方】AGENTS.md 官网将其定义为普通 Markdown，建议仓库根和子项目目录放置，近处指令优先，并由 Linux Foundation 下 AAIF 托管** [AG1]。官网列出的生态包括 Codex、Amp、Jules、Cursor、Factory、GitHub Copilot coding agent、Gemini CLI、Aider、goose、opencode、Zed、Warp、Windsurf 等；但「支持」包括配置接入，例如官网给 Aider 配 `read: AGENTS.md`、给 Gemini CLI 改 context filename 的方法，不能一概写为原生默认开启 [AG1]。产品调研的源码核对与此一致：Aider 的规则是 `--read CONVENTIONS.md`（§4.6.3），Gemini CLI 默认读 `GEMINI.md`、文件名可配置为数组（§4.2.2）。

它是**开放文件约定，不是统一的记忆服务协议**：各家的用户层路径、加载链、覆盖语义都不同。Codex、Claude Code、Cursor、Amp 的「个人指令＋项目指令」是成熟的两层模式 [CX3][CC1][CU2][AP3]，但这是**规则层**证据，不应被当成自动记忆双层效果的实验。

| 产品 | 用户／身份层 | 仓库、目录层与加载 | 限制与差异 | 来源 |
|---|---|---|---|---|
| Codex | `$CODEX_HOME/AGENTS.override.md` 或 `AGENTS.md`（取第一个非空） | 项目根（默认 `.git` 标记）到 cwd，每目录最多一文件；override 优先；合并默认上限 32 KiB；不越过项目根 | git worktree 有自己的 `.git` 文件，故自成一根 | [CX3]，§4.3.1 |
| Claude Code | `~/.claude/CLAUDE.md`＋用户 rules，另有 managed policy | CLAUDE.md 从 cwd 向上**每一级**（不止到 git root）＋子目录按需；v2.1.277 起无 CLAUDE.md 时直接读 AGENTS.md | 默认有项目 CLAUDE.md/CLAUDE.local.md 时不读 AGENTS.md，可配置两者一起；不读 `AGENTS.override.md` | [CC1]，§4.1.1 |
| Cursor | User Rules（全项目）；Team Rules（组织）；`~/.cursor/rules` | repo/subdirectory AGENTS.md；子目录规则与祖先组合，具体者优先；`.cursor/rules/*.mdc` 可用 globs | 旧文档曾说 nested「planned」，现行文档明确已支持；多根 workspace 有串味报告 | [CU1][CU2][FR4]，§4.5.1 |
| Amp | `~/.config/amp/AGENTS.md`、`~/.config/AGENTS.md`；系统层；web 端个人 / 工作区 Global AGENTS.md | cwd、父目录（到 `$HOME`）常驻；读子树文件时加载该子树规则；`globs:` 引用文件按需 | 用户层位置与 Codex 不同，**没有跨产品统一的 `$HOME/AGENTS.md`** | [AP3]，§4.6.5 |
| Windsurf / Devin Desktop | `global_rules.md`（6,000 字符） | 根 AGENTS.md 常驻；子目录按自动 glob 激活；规则向上发现到 git root | Memories 只适用于 legacy Cascade | [WS1][WS2]，§4.5.2 |
| GitHub Copilot / VS Code | 用户级 `~/.copilot/copilot-instructions.md`；个人与组织 instructions | 仓库根 → cwd → 嵌套目录（CLI 1.0.11 起）；也读 CLAUDE.md、GEMINI.md、`.claude/rules` | VS Code Local 的 nested AGENTS 仍为实验功能，`chat.useNestedAgentsMdFiles` 默认关闭 | [AG1][GH3][GH6]，§4.2.6 |
| Gemini CLI | `~/.gemini/GEMINI.md`（兼作全局个人记忆） | git root → cwd；子目录 JIT；`--include-directories` 多根各自向上扫 | 未受信文件夹不加载项目 GEMINI.md | §4.2.2 |
| OpenCode | `~/.config/opencode/AGENTS.md`（回退 `~/.claude/CLAUDE.md`） | cwd 向上到 git root；子目录 AGENTS.md 随 read JIT | 不自动解析 `@file` | §4.6.6 |
| pi | `~/.pi/agent/AGENTS.md` | cwd 的全部祖先（无 git 天花板），worktree 去重 | 上下文文件不受项目信任门控 | §4.6.7 |
| Devin | `~/.config/devin/AGENTS.md` | 仓库内；开头 16 KiB 自动注入 | 文档要求「as small as possible」，推荐改用 Skills | [DV1][DV2]，§4.3.2 |

### 3.6 规则与记忆的分界

**【官方】Claude Code 明确区分人写 instructions 与模型写 learnings；Codex 明确要求必需团队指南放 AGENTS.md／版本化文档，自动 Memories 只是 recall 层** [CC1][CX1]。模型可以帮人起草 AGENTS.md，人也可以修改 MEMORY.md，所以物理作者不是唯一分界。更稳妥的区分是：**规范性指令**规定「应该怎么做」，**经验性记忆**记录「此前观察到什么、用户曾如何纠正、何时可复用」。

**【推断】把模型推测自动提升成始终生效的项目规则，会扩大错误影响。** 记忆因此需要适用范围、来源、时效和纠正路径；规则需要维护者审阅。Copilot 在使用仓库记忆前重新核验证据，是把经验与当前事实分开的实际设计；并不要求所有记忆都改写 AGENTS.md [GH1]。

把项目知识写进规则文件的已有做法（第 4 节各产品）：

- OpenHands 未开记忆时，系统提示让模型「Use `AGENTS.md` under the repository root as your persistent memory」；V0/Cloud 的 `/remember` 写 `.openhands/microagents/repo.md`，必须先列清单让用户确认（§4.2.3）。
- Continue 的 `create_rule_block` 工具把模型学到的东西写成 `.continue/rules/*.md`，文件可见、可 git 管理（§4.6.4）。
- Devin 会建议创建 / 更新 Skill，并给「Create PR」按钮提交到仓库（§4.3.2）。
- Gemini 的路由规则把「团队约定」直接路由到仓库内 `./GEMINI.md`，Auto Memory 被禁止给项目根 GEMINI.md 出补丁（§4.2.2）。
- Amp 只在用户要求时更新 AGENTS.md；其 Agentic Review 的开放问题把长期记忆的落点设想为 AGENTS.md（§4.6.5）。
- Cursor 下线 Memories 时让用户导出成 `.mdc` 放进 Rules；Windsurf 文档写「for durable knowledge, prefer Rules or AGENTS.md」（§4.5）。
- 项目记忆进 git 的做法：Qwen team 层（opt-in，写入默认询问 + 秘密扫描，可选会话开始 ff-pull + commit + push，§4.2.1）；OpenHands 文档说团队可以 commit `.openhands/memory/`（§4.2.3）；Claude Code 子代理 `memory: project` 写到 `.claude/agent-memory/<agent>/` 可入库（§4.1.1）。
- 一个文件两用的先例：Gemini 的 `~/.gemini/GEMINI.md` 既是全局规则又是全局个人记忆（§4.2.2）。

---

## 4. 各产品详情（按主作用域键分组）

19 个 coding agent 按主作用域键分成六组，每个产品回答「写作约定」里的六个问题。开源项目以源码为准，行号以各节「版本基线」里的 commit 为准。Claude Code、Copilot、Codex、Cursor、Windsurf、Amp 六节末尾带有「补充：模式调研里的相关证据」小节，是系统与模式调研对同一产品的补充事实；与产品调研重复的结论不再重复列出，只在正文里并用编号。

### 4.1 单层项目键：只有项目层

只有 Claude Code 一家：主会话的自动记忆只有「项目（git 仓库）」一层，没有用户全局的自动记忆层；跨项目的个人偏好只能靠人写的 `~/.claude/CLAUDE.md` 或子代理 `memory: user`。

#### 4.1.1 Claude Code（Anthropic）
版本基线：本机 `claude --version` = **2.1.291**（GitHub 最新 release v2.1.291，2026-10-06）。核对页面（均 2026-10-06 抓取，文中引用的版本号最高到 v2.1.285）：
- 【官方】[CC1]（CLAUDE.md 层级、AGENTS.md、.claude/rules、auto memory）
- 【官方】[CC3]（`<project>` 目录命名规则、`CLAUDE_CODE_PROJECT_DIR_NAME`）
- 【官方】[CC2]（子代理 `memory:` 字段）
- 【官方】[CC4]、/env-vars、/worktrees、/context-window、/claude-projects
- 【官方】[CC9]（顶部 2.1.291）
- 【本机】`~/.claude/projects/` 目录命名与 `memory/` 子目录分布（只读）。

本节简称：「memory 页」= [CC1]；「sub-agents 页」= [CC2]；「sessions 页」= [CC3]；claude-directory = [CC4]；「env-vars 页」= [CC5]；worktrees = [CC6]；「context-window 页」= [CC7]；「claude-projects 页」= [CC8]；「CHANGELOG」= [CC9]。

##### ① 持久记忆
两套机制，官方明确分开（memory 页「CLAUDE.md vs auto memory」表）：
- **规则文件（人写）**：CLAUDE.md 家族。层级按加载顺序：① 托管策略 `/Library/Application Support/ClaudeCode/CLAUDE.md`（macOS）/ `/etc/claude-code/CLAUDE.md`（Linux）/ `C:\Program Files\ClaudeCode\CLAUDE.md`，或 managed-settings.json 的 `claudeMd` 键；② 用户 `~/.claude/CLAUDE.md` 与 `~/.claude/rules/*.md`；③ 项目 `./CLAUDE.md` 或 `./.claude/CLAUDE.md`，从 cwd 向上**每一级目录**都加载（不止到 git root）；④ `./CLAUDE.local.md`（个人、建议 gitignore）；⑤ 子目录 CLAUDE.md / CLAUDE.local.md 按需加载（Read/Write/Edit 触碰该子目录文件时）；⑥ `.claude/rules/*.md`（递归发现；无 `paths` frontmatter 的随启动加载、与 `.claude/CLAUDE.md` 同级；有 `paths` 的只在操作匹配文件时加载；2.0.64 引入）；⑦ `AGENTS.md`：v2.1.277 起在 cwd 及以上**没有任何 CLAUDE.md/CLAUDE.local.md** 时直接读 AGENTS.md（不读 `AGENTS.local.md`、`AGENTS.override.md`、`.agents/`），可用 `/config` → Project instructions 改为 `claude-md-and-agents-md` / `claude-md` / `managed-only`。`@path` 导入：相对于所在文件解析，最多 4 跳递归；项目级文件导入工作目录之外的路径会弹一次审批。【官方】memory 页。
- **自动记忆（模型写）**：auto memory，Claude 自己写的 markdown 笔记。2.1.32「Claude now automatically records and recalls memories as it works」、2.1.59「automatically saves useful context to auto-memory. Manage with /memory」【官方 CHANGELOG】。四类，记在文件 frontmatter `type`：`user`（角色/偏好）、`feedback`（纠正与确认）、`project`（进行中的工作、决策）、`reference`（外部信息位置）。会跳过能从代码推导的内容和 CLAUDE.md 已写的内容。【官方】memory 页「Auto memory」。
- 另有两个独立的「记忆」：子代理记忆（agent frontmatter `memory: user|project|local`，各自一个 MEMORY.md）；云端 Claude Projects 的「project memory」（Project settings > Memory，同样用 MEMORY.md 索引，但官方说明「separate from the auto memory Claude Code keeps on your machine」）【官方】sub-agents、claude-projects 页。
- CHANGELOG 2.1.172 提到「mounted team memory stores (`CLAUDE_MEMORY_STORES`) in remote sessions」，env-vars 文档页未收录该变量 → **未证实**。

##### ② 存在哪
- 主会话 auto memory：`~/.claude/projects/<project>/memory/`，内含 `MEMORY.md`（索引，一行一条）+ 若干主题文件（如 `user_role.md`、`feedback_testing.md`）。【官方】memory 页「Storage location」。
- `<project>` 名的规则（sessions 页「Where transcripts are stored」）：工作目录绝对路径，**非字母数字字符全部替换为 `-`**；转换后超过 200 字符则截到 200 并附全路径 hash。【本机】`/Users/<user>/.herdr/worktrees/iota/bot-mode` → `-Users-<user>--herdr-worktrees-iota-bot-mode`（`.` 也变成 `-`，于是出现双横线）；`/Users/<user>/Work/brain.md` → `-Users-<user>-Work-brain-md`。
- 但 **memory 子目录的 `<project>` 不按 cwd，而按 git 仓库推导**（见本节③）。【本机】本机 `~/.claude/projects/` 下有 35 个 `-Users-<user>--herdr-worktrees-iota-*` 目录（都只有会话 `.jsonl`），**没有一个含 `memory/`**；`memory/` 只存在于主仓库目录 `-Users-<user>-Work-iota/memory/`（15 个文件，MEMORY.md 2080 字节）。
- 可改位置：`autoMemoryDirectory`（2.1.74 加入，任意 settings 层级，须绝对路径或 `~/` 开头）；`CLAUDE_CONFIG_DIR` + `CLAUDE_CODE_PROJECT_DIR_NAME`（2.1.234+，把 transcripts 和 auto memory 一起放到 `<config dir>/projects/<name>/`，「whatever the working directory is」）。【官方】memory、sessions、env-vars 页。
- 子代理记忆：`~/.claude/agent-memory/<agent>/`（user）、`.claude/agent-memory/<agent>/`（project，可入库）、`.claude/agent-memory-local/<agent>/`（local）。【官方】sub-agents 页。
- 会话 transcript：`~/.claude/projects/<project>/<session-id>.jsonl`（按 cwd 键）；`cleanupPeriodDays`（默认 30 天）清理 transcript 但**不清 memory 目录**（2.1.228 修过误删）。本机另有 `~/.claude/history.jsonl`（每行带 `"project": "/abs/path"`）。
- 云端 Claude Projects：memory 文件存在 Anthropic 云端项目里，Project settings > Memory 可读/改/删。【官方】claude-projects 页。
- 全部 machine-local：「Files are not shared across machines or cloud environments」。【官方】memory 页。

##### ③ 作用域键
分层：
| 层 | 键 | 存什么 |
|---|---|---|
| 机器/组织 | 托管策略文件路径（全机器、全仓库） | 规则（managed CLAUDE.md / `claudeMd`） |
| 用户全局 | `~/.claude/` | 规则（`~/.claude/CLAUDE.md`、`~/.claude/rules/`）；**没有**用户全局的 auto memory |
| 项目（规则） | 文件系统祖先链（cwd → `/`，不止到 git root；monorepo 用 `claudeMdExcludes` 排除） | CLAUDE.md / CLAUDE.local.md / rules / AGENTS.md |
| 项目（auto memory） | **git 仓库**：「The `<project>` path is derived from the git repository, so all worktrees and subdirectories within the same repo share one auto memory directory. Outside a git repo, the project root is used instead.」CHANGELOG 2.1.63「Project configs & auto memory now shared across git worktrees of the same repository」；2.1.283 修复「started in a subdirectory of a git repository」时 memory 写入被拦 | MEMORY.md + 主题文件 |
| agent 实例 | 子代理 `name` + `memory:` scope（user → 跨项目同名共享；project/local → 仓库内） | 子代理自己的 MEMORY.md |
| 会话 | session-id（按 cwd 键） | transcript（不是记忆；fork 子代理继承父会话含已加载 memory） |
| 云端项目 | Claude Projects 的 project | project memory（与本机 auto memory 无关） |

- git 键到底是什么（common dir？主工作树路径？）闭源无法核对 → **未证实**。但【本机】观察：用 `git worktree add` 建在主仓库之外的 `~/.herdr/worktrees/iota/*` 仍与 `/Users/<user>/Work/iota` 共用 memory 目录（worktree 目录无 memory/），与「derived from the git repository」一致，说明键落在主仓库而非 worktree 自身的 top-level。
- 不是远程 URL 键：目录名由路径派生，同一仓库的两个独立 clone 应是两套 memory（官方未明说 → **未证实**，但「machine-local」+ 路径派生命名可推）。

##### ④ 谁写
- CLAUDE.md：人写；`/init` 生成/建议；`/doctor prompt-audit`（2.1.283+）给修改建议但不自动改；可以让 Claude「add this to CLAUDE.md」。
- auto memory：**Claude 写**，无审批弹窗（写入就是普通文件写；2.1.283 修复的是它被当成敏感文件写入拦下）；界面出现「Saved N memories」「Recalled N memories」提示，文件名可点开（2.1.86）；用户用 `/memory` 浏览、切换开关、打开 memory 文件夹；VS Code 有 Memory 对话框可查看/编辑/删除（2.1.274/2.1.275）；文件是纯 markdown 随时可手改/删。写入 `MEMORY.md` 后客户端度量是否接近/超过 200 行 / 25KB：接近时提醒精简（2.1.186），超限时写入成功但返回错误要求重写索引（2.1.210）。写 memory 文件时客户端自动在 frontmatter 加 ISO `modified` 时间戳（2.1.214+）。加载时对 MEMORY.md 和被 recall 的笔记做不可见字符/仿冒标记的中和（2.1.284）。
- 开关：本地会话默认开；`/memory` 切换写入 `~/.claude/settings.json` 的 `autoMemoryEnabled`；项目 settings 设 `"autoMemoryEnabled": false` 只关一个项目；`CLAUDE_CODE_DISABLE_AUTO_MEMORY=1`（`=0` 强制开）；`--bare` 与 `--safe-mode` 关闭；自托管/远程环境默认关；后台会话或由 Claude 自己启动的子 `claude` 进程只能关不能开（2.1.285）；`CLAUDE_CODE_DISABLE_CLAUDE_MDS=1` 连同所有 CLAUDE.md 一起不加载。【官方】memory、env-vars 页。
- 子代理记忆：子代理自己写（自动开启 Read/Write/Edit）；主会话 auto memory 关掉则 `memory:` 字段失效。

##### ⑤ 进上下文
- 常驻注入：每次会话启动把 `MEMORY.md` 的**前 200 行或前 25KB（先到为准）**注入（2.1.83「truncates at 25KB as well as 200 lines」），超出部分不加载；context-window 页的加载顺序是：system prompt → Auto memory (MEMORY.md) → environment info → MCP/skills 索引 → CLAUDE.md 等。CLAUDE.md 以 system prompt 之后的 user message 形式投递，单文件最大 4 MiB（更大跳过），建议 <200 行，超长会在启动和 `/status` 报警。
- 按需：主题文件不随启动加载，Claude 用 Read 工具按需读；路径型 rules 与子目录 CLAUDE.md 在触碰匹配文件时加载。
- 压缩：`/compact` 后根 CLAUDE.md 从磁盘重读再注入；嵌套 CLAUDE.md 与 path rules 重新按需加载。
- 「Recalled N memories」/「memory recall」/「memory extraction」在 CHANGELOG 多处出现（2.1.77「race between memory-extraction writes and the main transcript」、2.1.273「loaded into the prompt, recalled, indexed, or used by memory extraction」、2.1.288「memory recall … structured outputs」、2.1.286 Claude Tag「memory recall … default Sonnet model」），说明存在后台抽取 + 检索式召回路径（依赖结构化输出/小模型），但文档只写「Claude reads and writes memory files during your session」→ 召回的具体机制（索引、向量/关键词）**未证实**。
- 子代理不继承主会话 auto memory（fork 例外）；有 `memory:` 的子代理注入自己 MEMORY.md 前 200 行/25KB。

##### ⑥ 换项目
- 身份/偏好：auto memory 按仓库存，`type: user` 的偏好**不会**自动跟到别的仓库；跨项目只能靠人写的 `~/.claude/CLAUDE.md`、`~/.claude/rules/`，或子代理 `memory: user`。
- 项目记忆不会串到别的仓库（目录隔离）。
- 官方专门讨论过：worktree（memory 共享；`CLAUDE.local.md` 只在创建它的 worktree 存在，建议 `@~/.claude/xxx.md` 导入；`.claude/worktrees/` 下的子代理 worktree 不读其根 CLAUDE.md/rules 而沿用主会话的）、monorepo（`claudeMdExcludes`、large-codebases 页、子目录启动共享 memory）、多租户宿主（`CLAUDE_CODE_PROJECT_DIR_NAME`）、Cowork（跳过用户级文件的外部 import）、云端（不共享本机 memory；Claude Projects 另有 project memory）。同仓库多 clone 未讨论（**未找到**）。

##### 一句话结论
作用域键 = auto memory 按 **git 仓库**（所有 worktree/子目录共用一个 `~/.claude/projects/<repo-path-sanitized>/memory/`，非 git 用项目根；machine-local），无用户全局自动记忆；规则文件 = 机器(托管)/用户(`~/.claude`)/项目(目录祖先链)/子目录与 rules(按需)/AGENTS.md 兜底 的分层拼接；记忆 vs 规则文件分别 = Claude 写的 MEMORY.md+主题文件（前 200 行/25KB 常驻、其余按需）vs 人写的 CLAUDE.md 家族（全量常驻，≤4 MiB）。

##### 补充：模式调研里的相关证据

- **项目键随路径变化的公开报告**（证据等级见 §7）：#61349（2026-05-22，2.1.148）改名目录后新建了 memory 目录，旧的 27 条不再加载，磁盘内容未删 [FR1]；#28037（2026-02-24，2.1.49）worktree 下学到的知识只存在 worktree 键下，删工作树后主仓库看不到 [FR2]——这是 2.1.63「shared across git worktrees」之前的历史问题，与本节③的现行文档一致；#31008（2026-03-05）共享 repo 身份之后，又有用户报告被带到其他 worktree 的路径、混淆当前执行位置，并要求按 worktree 隔离，诉求与 #28037 相反 [FR3]。
- **子代理记忆三选一，不叠加**：`memory: user/project/local` 只选一种归属，目录再按 agent 名区分；官方建议用 `project`，但这是建议，不是三种 scope 同时加载 [CC2]。
- **规则与记忆的官方分界**：文档明确把人写的 instructions（CLAUDE.md）和模型写的 learnings（auto memory）分开 [CC1]；主会话 auto memory 即使写了 `type: user` 的条目，默认仍落在该仓库的 memory 目录，不等于跨项目的用户记忆 [CC1][CC2]。

##### 未证实 / 未找到
- auto memory 的 git 键具体取什么（common dir / 主工作树路径 / `.git` 位置）：**未证实**（闭源）。
- 同一仓库多个 clone 是否共享：**未找到**。
- 「memory recall / extraction」的实现（后台抽取时机、召回算法）：**未证实**。
- `CLAUDE_MEMORY_STORES` 团队记忆存储：仅见 CHANGELOG 2.1.172，文档无 → **未证实**。

### 4.2 项目层＋用户层两层（含仓库身份＋账号）

最常见的形态。区别在于：「项目」怎么定义（git root、启动目录、workspace 路径、GitHub 仓库身份），以及一条记忆由谁决定落哪层（见 §3.4）。Copilot 的「项目」是远程仓库身份而不是本地目录，放在本组末尾。

#### 4.2.1 Qwen Code（QwenLM/qwen-code）
版本基线：main @ `28e0f3a8` (2026-10-06)，`packages/cli/package.json` 0.25.0，最新 tag **v0.25.0**；自动记忆系统来自 PR #3087「feat(memory): managed auto-memory and auto-dream system」（merged 2026-04-16）。核对文档：`docs/users/features/memory.md`、`rules.md`、`mem0.md`、`worktree.md`、`commands.md`、`docs/users/configuration/settings.md`、`docs/design/2026-05-15-async-memory-recall-design.md`。核对源码：`packages/core/src/memory/{paths,scopes,types,memoryDiscovery,manager,extract,dream,user-dream,recall,prompt,index-budget,team-memory-sync,refresh,writeContextFile,store}.ts`、`tools/{manage-memory,search-memory,memory-config,tool-names}.ts`、`utils/{memory-constants,paths}.ts`、`config/storage.ts`、`core/client.ts`、`core/prompts.ts`。

**与 Gemini CLI 的关系**：Qwen Code 2025 年中从 Gemini CLI 分叉，分叉时 Gemini 只有 `save_memory`/`## Gemini Added Memories`。Gemini 的 Auto Memory（2026-04-22/28）**没有被 Qwen 吸收**——Qwen 源码里没有 `.inbox`、`experimental.autoMemory`、`startMemoryService`（grep 零命中），Qwen 自己的 managed auto-memory 比 Gemini 的早一周合入（#3087，2026-04-16），是**独立设计**（有自己的 extract / dream / recall / team memory）。Gemini 时代的痕迹只剩：`MEMORY_SECTION_HEADER = '## Qwen Added Memories'`（`utils/memory-constants.ts:32`）仅被 `memory/writeContextFile.ts:310-313` 用于 serve/workspace 路径（`packages/cli/src/serve/workspace-memory.ts`）；`ToolNames.MEMORY='save_memory'`（`tools/tool-names.ts:30`）只出现在 deny 列表里，`tools/` 下已无 `memoryTool.ts`；`~/.qwen/memory.md`（`storage.ts:225-226 getGlobalMemoryFilePath()`）仍有定义。

本节源码与文档均为仓库内文件（`docs/users/...`、`packages/core/...`），以版本基线 commit `28e0f3a8` 为准；外部 Mem0 见 §5.2。

##### ① 持久记忆
有，两套分开：
- **规则文件**：`QWEN.md`，同时也读 `AGENTS.md`（默认文件名数组 `['QWEN.md','AGENTS.md']`，`memory-constants.ts:33-36`；可用 `context.fileName` 改）；三处：`~/.qwen/QWEN.md`（个人全局）、项目根 `QWEN.md`（团队）、`.qwen/QWEN.local.md`（个人、项目内、需自己 gitignore；`LOCAL_CONTEXT_FILENAME` :31，只从项目根单一位置加载，项目根 = 最近含 `.git` 目录或文件的祖先）；另有 `.qwen/rules/*.md`（`~/.qwen/rules/` 恒加载；项目 rules 需信任；带 `paths:` 的是条件规则，触碰到匹配文件时注入一次）；扩展的 context 文件（`docs/users/features/rules.md`）。
- **记忆（模型自动写）**：「Auto-memory」，默认**开**（`memory.enableManagedAutoMemory` 默认 `true`，`settings.md:427`）。三层：
  - project：`~/.qwen/projects/<key>/memory/`：`MEMORY.md` 索引 + 主题文件（`AUTO_MEMORY_TYPES = user|feedback|project|reference` → `<type>.md`，`types.ts:7-14`；也可任意主题文件）+ `pinned/`（手工文档，自动维护不得动）。
  - user：`~/.qwen/memories/`（跨项目，`USER_AUTO_MEMORY_DIRNAME='memories'`，`paths.ts:34`）。
  - team：`<gitRoot>/.qwen/team-memory/`（opt-in `memory.enableTeamMemory` 默认 false；进 git；写入过秘密扫描；可选 `enableTeamMemorySync` 在会话开始 ff-pull + commit + 只推该提交）。
  - 维护：Dream（去重/清理）默认开，一天一次（`user-dream.ts:39-42` `DEFAULT_USER_DREAM_MIN_HOURS=24`、脏改 10 次触发）。
  - 结构化召回（opt-in `memory.enableStructuredRecall`）：给每个文件加 frontmatter（name/description/type/category/keywords/usage_scenarios，20 个固定 category，`types.ts:19-41`），注入「聚焦子树」并提供 `search_memory` 工具（`tools/search-memory.ts`）；`manage_memory(remember|forget)` 工具也只在此模式可用（`tools/manage-memory.ts:56-60`）。
- **外部记忆**：Mem0（`docs/users/features/mem0.md`）——内置 MCP `external-context` 的 `context_search`，默认只读，不自动召回。
- 会话：`~/.qwen/projects/<sanitizeCwd(cwd)>/chats/`（`storage.ts:617-621, 730-740`）；临时 `~/.qwen/tmp/<sha256(cwd)>/`（:624-629）。

##### ② 存在哪
全部本地 Markdown/JSON（`packages/core/src/memory/paths.ts`，HEAD 28e0f3a）【官方】：
- 基目录 `getMemoryBaseDir()` :70-75 → `Storage.getRuntimeBaseDir()` → 默认 `~/.qwen`（`QWEN_HOME` / `QWEN_RUNTIME_DIR` 可改，`storage.ts:172-203`；测试可用 `QWEN_CODE_MEMORY_BASE_DIR`）。
- 项目记忆根 `getAutoMemoryRoot(projectRoot)` :111-144 → `<base>/projects/<sanitizeCwd(projectKey)>/memory`；`QWEN_CODE_MEMORY_LOCAL=1` 时改为 `<projectRoot>/.qwen/memory`。`sanitizeCwd` = 非字母数字全部替换为 `-`（Windows 先小写）（`utils/paths.ts:388-392`）。
- 项目状态文件（`meta.json`、`extract-cursor.json`、`consolidation.lock`）放在记忆根的**父目录**，让 `memory/` 只有 md（:20-26, :190-196）。
- 用户层 `<base>/memories/`（:300-303）+ `<base>/user-memory-meta.json`、`user-memory-consolidation.lock`。
- 团队层 `getTeamAutoMemoryRoot()` :343-349 → `findGitRoot(projectRoot)/.qwen/team-memory/`。
- 索引预算 `index-budget.ts:7-10`：`MAX_INDEX_LINE_CHARS=150`、`MAX_INDEX_LINES=200`、`MAX_INDEX_CHARS=25_000`（文档 memory.md「200 lines and 25,000 UTF-16 code units」）。

##### ③ 作用域键
三层 + 会话 + 规则，键各不相同：
- **project 记忆：git root 的绝对路径**（默认 `git-root` scope）：`findGitRoot()` 从 cwd 向上找第一个 `.git`（目录**或文件**，所以 linked worktree 以自己为根，注释明说「WITHOUT resolving linked worktrees back to their canonical repository root … each worktree gets its own memory … See #6449」，`paths.ts:122-133`）；不是远程 URL。`QWEN_CODE_MEMORY_PROJECT_SCOPE=workspace` 改为精确 cwd（嵌套 workspace 各自一份，:84-108）。分支共享（docs：「All branches of the same checkout share the same memory folder」）。
- **user 记忆：用户全局**（不按项目）。
- **team 记忆：仓库（git root）**，通过 git 共享给所有协作者。
- **会话/chats/tmp：cwd**（`getProjectDir()` 用 `sanitizeCwd(getProjectRoot())`，`getProjectTempDir()` 用 `sha256(projectRoot)`，`storage.ts:617-629`；`docs/users/features/worktree.md:335`「sessions and worktrees are tightly bound by `projectHash(cwd)`」）。注意：记忆按 git root、会话按 cwd——在子目录启动时会话目录和记忆目录不是同一个 `<key>`。
- **规则文件 QWEN.md**：全局 `~/.qwen/QWEN.md` + 从 cwd 向上到项目根（`findProjectRoot`，`memoryDiscovery.ts:160-199`，未受信工作区不扫）+ `.qwen/QWEN.local.md` 固定槽位。
- **agent/身份**：没有按 agent 分的记忆（未找到）；`memory-scoped-agent-config.ts` 只是给后台记忆 agent 限制可写路径；subagent 的 `capability.ts:43` 把 `save_memory` 设为 deny。Mem0 的 scope 是 user/repository，可配 `scope.userId/appId/agentId` 跨 worktree 复用（mem0.md「Scope and writes」）。

##### ④ 谁写
- QWEN.md / rules：人写（`/init` 让模型生成一份起始 QWEN.md）。
- 自动记忆：**模型在后台写，不审批**（project/user 层）。每个 UserQuery 轮次结束时 `scheduleExtract` + `scheduleDream`（`core/client.ts:3156-3185`），由 fork 出的抽取 agent 执行，只允许写 `isAnyAutoMemPath`（project+user 根）内的文件（`paths.ts:395-404`，注释强调 team 层**故意排除**）；有内存压力门槛、并发锁、`extract-cursor.json` 游标。team 层写入在默认审批模式下**每次询问**，YOLO/AUTO_EDIT 不问但出现在 git diff；写入前过 `secret-scanner.ts`。`pinned/` 下文件抽取与 Dream 都不得改。
- 显式：`/remember <text>`、`/forget <text>`、`/dream`；`manage_memory` 工具描述「Use only when the user asks to remember … Never save information merely learned while doing another task」（`manage-memory.ts:124`）。
- 开关：`memory.enableManagedAutoMemory`、`enableManagedAutoDream`（均默认 true）、`enableAutoSkill`（true，自动生成 skill 需确认 `autoSkillConfirm`）、`enableTeamMemory`/`enableTeamMemorySync`/`enableStructuredRecall`（默认 false）；`/memory` 面板顶部可切换、可打开记忆文件夹（`memory.md`「Commands」）。
- 可见性：纯 Markdown，可随意编辑删除；团队层在 PR diff 里可审。

##### ⑤ 进上下文
混合「常驻 + 按需召回」：
- QWEN.md / baseline rules：每次请求 system prompt 全文；条件 rules 命中一次注入一次；扩展 context 文件常驻（`rules.md` 对比表）。
- 自动记忆（默认 legacy 模式）：`MEMORY.md` 索引作为 system prompt 尾部（`client.ts:2018 autoMemory: this.config.getAutoMemoryPrompt()`，`prompts.ts:881-891 buildSystemPromptSuffix`），受 200 行 / 25,000 字符预算裁剪，超限整条跳过并加截断提示；详情在主题文件里由模型按链接 `read_file`。另外每个 UserQuery **异步相关性召回**：模型挑最多 5 篇相关文档（`recall.ts:32-37` `MAX_RELEVANT_DOCS=5`、正文截 1,200 字符、候选 ≤200），主请求最多等 100 ms，等不到就先用启发式快速结果，其余结果在下一个 ToolResult 后以 `system-reminder` 追加（设计文档 2026-05-15 + 2026-08-08 更新）。
- 结构化召回模式：注入「聚焦子树」+ `search_memory` 工具按需取全文（RAG 风格，无向量库，关键词/元数据）；后台迁移每轮最多 10 文件补 frontmatter。
- Mem0：只有工具调用，不自动注入。
- 压缩/合并：Dream 负责去重、合并、过期清理；索引重建 `indexer.ts`。

##### ⑥ 换项目
- 跟着走：`~/.qwen/QWEN.md`、`~/.qwen/rules/`、user 层 `~/.qwen/memories/`（类型 `user` 的记忆「always user (cross-project)」，`prompt.ts:58-62`）。
- 不串：project 层按 git root 绝对路径；team 层在仓库里。官方明确讨论了 worktree：「Each linked git worktree gets its own memory folder, matching the per-worktree isolation of chats and other session state — repository-wide conventions you want in every worktree belong in team memory」（`memory.md`「Where it's stored」）；Mem0 文档：「Moving the repository or using another checkout changes it, including temporary `--worktree` and agent-isolation worktrees. To reuse a known scope across worktrees, set `scope.userId`…」；`worktree.md:335` 说会话与 worktree 由 `projectHash(cwd)` 绑定，并预告「anchoring storage at the repo root instead of cwd」的未来改动。
- monorepo：`QWEN_CODE_MEMORY_PROJECT_SCOPE=workspace` 让嵌套 workspace 各自一份（`paths.ts:84-96`）；QWEN.md 向上到项目根。

##### 一句话结论
作用域键 = **project 记忆：git root 绝对路径（worktree 各自一份，可切成 cwd）；user 记忆：用户全局；team 记忆：仓库内目录；会话：cwd**。记忆 vs 规则文件 = `~/.qwen/projects/<key>/memory/MEMORY.md` + 主题文件（模型自动写、默认开、不审批）vs `QWEN.md`/`AGENTS.md`/`.qwen/rules`（人写）。注入 = 索引常驻（200 行/25k 字符预算）+ 每轮异步相关性召回 + 可选 `search_memory` 检索。Gemini 的 Auto Memory 没被继承。

##### 未证实 / 未找到
- 分叉时点、以及 Qwen 从 Gemini 移除 `save_memory` 工具的具体版本：未找到（只确认现版本无该工具文件）。
- `~/.qwen/memory.md`（`getGlobalMemoryFilePath`）现在是否还被任何读路径加载：未找到调用（仅定义）。
- `/memory` 面板的「Open the project QWEN.md」具体打开哪一级：文档只说 project QWEN.md，未核对源码。
- 向量检索：未找到（关键词 + 模型选择）。

#### 4.2.2 Gemini CLI（google-gemini/gemini-cli）
版本基线：main @ `fb972b2f` (2026-10-02)，`package.json` 0.64.0-nightly.20260929；GitHub 最新 release **v0.62.0**（2026-09-29，`gh api releases/latest`）；`docs/changelogs/latest.md` 写的是 v0.61.0 (2026-09-23)。核对的文档：`docs/cli/gemini-md.md`、`docs/cli/auto-memory.md`、`docs/cli/tutorials/memory-management.md`、`docs/tools/memory.md`、`docs/cli/session-management.md`、`docs/cli/checkpointing.md`、`docs/cli/trusted-folders.md`、`docs/cli/git-worktrees.md`、`docs/cli/settings.md`、`docs/reference/commands.md`、`docs/changelogs/index.md`。核对的源码：`packages/core/src/utils/paths.ts`、`config/storage.ts`、`config/projectRegistry.ts`、`config/storageMigration.ts`、`tools/memoryTool.ts`、`utils/memoryDiscovery.ts`、`context/memoryContextManager.ts`、`config/memory.ts`、`services/memoryService.ts`、`services/memoryPatchUtils.ts`、`agents/skill-extraction-agent.ts`、`commands/memory.ts`、`prompts/snippets.ts`、`utils/environmentContext.ts`、`tools/jit-context.ts`、`core/client.ts`、`services/chatRecordingService.ts`、`packages/cli/src/ui/commands/memoryCommand.ts`、`packages/cli/src/utils/autoMemory.ts`；另用 `gh api` 取了 tag v0.39.0 的 `memoryTool.ts` / `memoryCommand.ts` 对照旧行为。

**先说一个重要变化（按 `save_memory` 理解 Gemini 记忆已经过时）**：`save_memory` 工具、`/memory add`、以及写到 `~/.gemini/GEMINI.md` 的 `## Gemini Added Memories` 小节，在 **v0.39.0 仍存在**（tag v0.39.0 `packages/core/src/tools/memoryTool.ts:33` `MEMORY_SECTION_HEADER = '## Gemini Added Memories'`，`memoryCommand.ts:49` `name: 'add'`【官方】），但 **PR #25716「refactor(memory): replace MemoryManagerAgent with prompt-driven memory editing across four tiers」（merged 2026-04-22，进 v0.40.0 2026-04-28）之后被整体替换**：当前 main 的 `memoryTool.ts` 只剩文件名常量（`DEFAULT_CONTEXT_FILENAME='GEMINI.md'` :11、`PROJECT_MEMORY_INDEX_FILENAME='MEMORY.md'` :12），系统提示里明确写「There is no `save_memory` tool」（`prompts/snippets.ts:872`），`/memory` 只剩 `show | reload(refresh) | list | inbox`（`packages/cli/src/ui/commands/memoryCommand.ts`）。全仓库 grep `Gemini Added Memories` 零命中。来源：[GM1] 、[GM2] （v0.40.0 条目）【官方】。

本节外链：PR #25716 = [GM1]；changelog 索引 = [GM2]；其余文档为仓库内 `docs/` 文件，以 commit `fb972b2f` 为准。

##### ① 持久记忆
有，而且分得很清楚，官方叫「四层（four-tier）记忆」（PR #25716 正文表格）【官方】：

| 层 | 文件 | 性质 |
|---|---|---|
| Project Instructions | `./GEMINI.md`（git 根到 cwd 的各级） | **规则文件**，人写、提交到仓库 |
| Subdirectory Instructions | `./<subdir>/GEMINI.md` | **规则文件**，JIT 按需加载 |
| Private Project Memory | `~/.gemini/tmp/<project-id>/memory/MEMORY.md`（索引）+ 同目录其它 `*.md` | **记忆**：模型按用户要求写、Auto Memory 产出补丁；私有、不入库 |
| Global Personal Memory | `~/.gemini/GEMINI.md` | **记忆**（跨项目个人偏好），但它同时也是「全局规则文件」——一个文件两用 |

此外：
- 扩展（extension）的 context 文件也是规则文件一层（`config/memory.ts` 的 `HierarchicalMemory { global, extension, project, userProjectMemory }`）。
- **Auto Memory（实验性，默认关）**：后台从历史会话抽取「记忆补丁」和 Skill 草稿，放进收件箱等用户批准（`docs/cli/auto-memory.md`，v0.39.0 加 inbox、v0.42.0 定「canonical-patch」合同，#24544/#25148/#26338）【官方】。
- 会话转录（jsonl）、检查点、计划/任务目录——都是会话态，不是记忆。
- 没有向量库、没有数据库；全部是 Markdown + JSON 文件。

##### ② 存在哪
全部本地文件。路径定义（`packages/core/src/config/storage.ts`，HEAD fb972b2）【官方】：
- 全局目录 `Storage.getGlobalGeminiDir()` :55-61 → `~/.gemini`（`GEMINI_CLI_HOME` 可改，`utils/paths.ts:22-28`）。沙箱 `SANDBOX=sandbox-exec` 时运行态改到 `~/.cache/.gemini`（`getGlobalRuntimeDir()` :95-110）。
- 全局个人记忆 / 全局规则文件：`tools/memoryTool.ts:90-92 getGlobalMemoryFilePath()` → `~/.gemini/<contextFileName>`，默认 `~/.gemini/GEMINI.md`。
- 项目临时目录 `getProjectTempDir()` :230-234 → `~/.gemini/tmp/<project-id>/`；其下：
  - `memory/`（`getProjectMemoryTempDir()` :336-338）= 私有项目记忆目录，索引 `MEMORY.md`（`memoryTool.ts:94-99`），遗留私有 `GEMINI.md` 仍兼容读取（`memoryDiscovery.ts getUserProjectMemoryPaths`）。
  - `memory/skills/`（:340-342）= Auto Memory 抽出的 Skill 草稿与 `.patch`。
  - `memory/.inbox/<kind>/extraction.patch`，kind ∈ {`private`,`global`}（`services/memoryPatchUtils.ts:232`；允许根 :278-280）；状态文件 `memory/.extraction.lock`、`memory/.extraction-state.json`（`memoryService.ts` 顶部常量）。
  - `chats/session-<ts>-<shortid>.jsonl`（`chatRecordingService.ts:757-799`，前缀 `SESSION_FILE_PREFIX='session-'` `chatRecordingTypes.ts:12`）。
  - `checkpoints/`、`logs/`、`<sessionId>/plans|tracker|tasks`（:365-411）。
- Git 快照（checkpointing）在 `~/.gemini/history/<project-id>/`（`getHistoryDir()` :326-330；`docs/cli/checkpointing.md:15`）。
- 项目注册表 `~/.gemini/projects.json`（:289-291），每个项目目录下有 `.project_root` 归属标记（`projectRegistry.ts:23`）。
- 用户级 skills `~/.gemini/skills/`，项目级 `.gemini/skills/`（inbox 里的 skill 可「promote」到这两处）。

##### ③ 作用域键
分层混合：
- **用户全局**：`~/.gemini/GEMINI.md`（既是全局规则又是全局个人记忆）。
- **项目**：键 = **启动 CLI 的目录的绝对路径**（`Config.targetDir`，`storage.getProjectRoot()` :265 直接返回 `this.targetDir`），**不是 git root、不是远程 URL**。项目标识符的算法：
  - 现行：`ProjectRegistry.getShortId(normalizedPath)`（`projectRegistry.ts:163-240`）——`normalizePath` 只做 `path.resolve`（Windows 才小写，:95-101）；slug = `slugify(basename(path))`，冲突则 `-1`、`-2` 后缀（`claimNewSlug` :304-309，`slugify` :414-421），映射存 `projects.json`，并在 `tmp/<slug>/.project_root` 写回绝对路径做归属校验。
  - 遗留：`getProjectHash(projectRoot) = sha256(projectRoot).hex`（`utils/paths.ts:318-320`），`storageMigration.ts` 启动时把 `tmp/<hash>` 复制到 `tmp/<slug>`。官方文档 `session-management.md`、`checkpointing.md` 仍写 `<project_hash>`——文档落后于代码。
  - 推论：同一仓库两个 clone（路径不同）→ 两套私有记忆/会话；在子目录里启动 → 以子目录为项目（slug 是子目录名）；`--worktree` 建在 `.gemini/worktrees/<name>`（`docs/cli/git-worktrees.md`）→ 路径不同 → 独立。
- **规则文件（GEMINI.md）发现**：全局 `~/.gemini/GEMINI.md` + 每个受信工作区目录从自身向上到 git root（`findProjectRoot` 以 `.git` 目录或文件为界，`memoryDiscovery.ts:152-205`；`getEnvironmentMemoryPaths` :360-385；多工作区 `--include-directories` 各自向上扫）+ 子目录 JIT（`loadJitSubdirectoryMemory` :437+，工具访问文件时向上到 git root/受信根）。文件名可配置为数组 `context.fileName`（`docs/cli/gemini-md.md`「Customize the context file name」）。
- **会话**：plans/tracker/tasks 按 `sessionId` 分目录；会话文件本身在项目的 `chats/`。
- **agent/身份**：未找到任何按 agent 身份划分的记忆；Auto Memory 跳过 `kind==='subagent'` 的会话（`memoryService.ts shouldProcessConversation`）。

##### ④ 谁写
- 规则文件：人写。模型也能改——系统提示（`prompts/snippets.ts:872-890`）给了「路由规则」：团队约定 → `./GEMINI.md`；「我机器上/别提交」→ 私有项目记忆目录；「我一向喜欢 X / 所有项目」→ `~/.gemini/GEMINI.md`；两可就**问用户**；一条事实只能落一层、不许跨层镜像；不要存会话态/改动摘要。写入走普通 `edit`/`write_file` 工具的审批流程（`docs/tools/memory.md`）。
- Auto Memory：后台子 agent（预览版 Flash，maxTurns 30、30 分钟上限，`memoryService.ts`）只能写 `memory/` 工作目录内的 `.inbox/<kind>/extraction.patch` 与 `skills/`，**不能直接改** MEMORY.md / GEMINI.md / 设置 / 凭据，且被禁止给项目根下的 GEMINI.md 出补丁（`skill-extraction-agent.ts` 系统提示；`memoryPatchUtils.ts` 校验目标必须落在允许根内）。用户在 `/memory inbox` 对话框里逐条 Apply / Dismiss（`commands/memory.ts applyInboxMemoryPatch` :558、`dismissInboxMemoryPatch` :948）；有新候选时 UI 弹一行「… Use /memory inbox to review.」（`memoryService.ts coreEvents.emitFeedback`）。
- 触发条件：会话空闲 ≥3h、≥10 条用户消息、非子 agent；每次最多处理 10 个新会话、索引 50 个；两次抽取间隔 ≥30 min；跨实例用 `.extraction.lock` 互斥（`memoryService.ts` 顶部常量）。开关 `experimental.autoMemory`（默认 `false`，`docs/cli/settings.md:181`），改后需重启。
- 可见性：全是 Markdown，可直接编辑删除；`/memory list` 列出在用文件，`/memory show` 打印拼接后的全文。

##### ⑤ 进上下文
常驻注入，分三级（源码注释引 issue #11488，`utils/environmentContext.ts:62-66`）【官方】：
- Tier 1：全局个人记忆 + 私有项目 `MEMORY.md` → **system instruction**（`config.ts:2593-2607 getSystemInstructionMemory()`；`client.ts:389/409`）。MEMORY.md 是索引，同目录其它 `*.md` 只有被 MEMORY.md 用**绝对路径**指到时才由模型 `read_file` 按需读（`skill-extraction-agent.ts`「MEMORY.md IS THE INDEX」、`snippets.ts`）。
- Tier 2：扩展 context + 项目 GEMINI.md + MCP instructions → **第一条 user message** 的 `<session_context>`（`environmentContext.ts:66-76`）。
- Tier 3：子目录 GEMINI.md → 高意图工具（read_file/ls/write/replace/read_many_files）输出末尾追加 `--- Newly Discovered Project Context ---`（`tools/jit-context.ts:47-59`），同一文件只加载一次（按 dev:inode 去重）。
- 每次请求都带；`/memory reload` 重扫。应用 inbox 补丁后自动 `refreshMemory`。
- 大小上限：**未找到**字符/行数上限。相关限制：`@import` 深度上限 5（`memoryImportProcessor.ts:196`）；向下发现目录数 `context.discoveryMaxDirs` 默认 200（`settings.md:122`）。压缩（history compression）不触及这些文件。
- 信任：未受信文件夹不加载环境/项目 GEMINI.md，JIT 也返回空（`memoryContextManager.ts:55-59,131-133,143`；`docs/cli/trusted-folders.md`「Automatic memory loading is disabled」）；全局与私有项目记忆仍加载。

##### ⑥ 换项目
- 跟着走的：`~/.gemini/GEMINI.md`（全局规则+个人偏好）、`~/.gemini/skills/`。
- 不串的：私有项目记忆、inbox、skills 草稿、会话、检查点都按「启动目录绝对路径」隔离。官方原话：「Inbox items are stored per project. Skills extracted in one workspace are not visible from another until you promote them to the user-scope skills directory」（`docs/cli/auto-memory.md` Limitations）；「Sessions are project-specific. Switching directories to a different project switches to that project's session history」（`docs/cli/session-management.md`）。
- worktree / 多 clone：`docs/cli/git-worktrees.md` 只讲 `--worktree` 建目录和用 `cd <worktree> && gemini --resume <id>` 恢复，**没有讨论记忆是否共享**；按实现，worktree 是另一个路径 = 另一个项目（规则文件 GEMINI.md 在 worktree 里有同一份，私有记忆不共享）。同名目录的两个不同项目得到 `name` 和 `name-1` 两个 slug，不会误共享（`.project_root` 校验）。同一路径换了仓库内容 → 共享旧记忆（纯路径键）。
- monorepo：子目录 GEMINI.md 走 JIT；`--include-directories` 多根各自向上扫。

##### 一句话结论
作用域键 = **项目：启动目录绝对路径 → projects.json 注册的 basename slug（旧版 sha256(path)）；用户：`~/.gemini`**。记忆 vs 规则文件 = 私有项目记忆 `~/.gemini/tmp/<slug>/memory/MEMORY.md`（+Auto Memory inbox 补丁）vs 仓库内 `GEMINI.md` 层级；`~/.gemini/GEMINI.md` 一个文件同时扮演两种角色。注入为常驻三级（system / 首条 user 消息 / 工具输出 JIT），无字数上限。

##### 未证实 / 未找到
- 记忆文件字节/行数上限：未找到。
- 官方对 worktree / 多 clone 的记忆共享策略：未找到（文档只讲会话恢复）。
- 文档中的 `<project_hash>` 与代码的 slug 不一致，以代码为准；slug 机制进入哪个版本未在 changelog 中找到关键词（未证实）。
- `## Gemini Added Memories` 的旧文件是否会被自动迁移到新四层：未找到迁移代码（全局 GEMINI.md 本身仍整体加载，所以旧内容不会丢）。

#### 4.2.3 OpenHands（开源）
版本基线：
- **V1 Python agent 已搬到 `OpenHands/software-agent-sdk`**（main `aae9c437`，2026-10-06；`openhands-sdk` 1.53.0，release v1.53.0 2026-10-05）。`OpenHands/OpenHands` 主仓现在是 TypeScript 的 "Agent Canvas"（`@openhands/agent-canvas` 1.25.0，main `b0a1a2d1`，2026-10-06），不含 agent 代码。
- V0 Python 代码：`OpenHands/OpenHands` tag `0.62.0`（2025-11-11，最后一个 0.x tag）；另有 `OpenHands/legacy` 仓（"Legacy code… that used to live in OpenHands/OpenHands"，最后提交 2026-07-25）。
- 云端：`OpenHands/enterprise`（public，main 2026-10-06）。
- 读过的源码：sdk `openhands/sdk/context/memory.py`、`context/agent_context.py`、`context/prompts/sections/static.py`、`context/condenser/*`、`skills/skill.py`、`conversation/impl/local_conversation.py`；agent-server `skills_router.py`、`skills_service.py`；enterprise `app_conversation_service_base.py`、`event_callback/memory_change_callback_processor.py`、`storage/user.py`、`migrations/versions/167_add_user_memory_context.py`、`skills/agent_memory.md`；V0 `openhands/memory/memory.py`、`openhands/runtime/base.py`。文档 docs.openhands.dev：`sdk/guides/persistent-memory`、`overview/skills*`、`openhands/usage/customization/repository`、`sdk/guides/context-condenser`。

本节简称：skills 文档 = [OH1]；repository 定制 = [OH2]；组织 skills = [OH3]；persistent-memory = [OH4]；context-condenser = [OH5]。

##### ① 持久记忆
**规则文件（人写；V0 叫 microagents，V1 叫 skills，两套路径都兼容）**
- 始终注入：`AGENTS.md`（也认 `agent.md`、`claude.md`、`gemini.md`、`.cursorrules`，`skill.py:346-352` `PATH_TO_THIRD_PARTY_SKILL_NAME`）；旧式无 trigger 的 `.openhands/microagents/repo.md`（"Without triggers (None): Full content in <REPO_CONTEXT>, always active"，`skill.py:190`）。嵌套 `server/AGENTS.md` 作为目录范围的 path rule（`skill.py:1131-1144`）。
- 按需：`SKILL.md`（名字+描述常驻，调用时才注入）；`triggers:` 关键词出现在用户消息时注入（`KeywordTrigger`）；`paths:` 碰到匹配文件时注入一次（`PathTrigger`）。【官方】[OH1]
- `.openhands/setup.sh`（每次开始处理仓库时跑）、`.openhands/hooks.json`（Stop hook 做质量门）；`.openhands/pre-commit.sh` 为旧机制，文档建议迁到 Stop hook。【官方】[OH2] ；V0 代码 `runtime/base.py:501-502`、`533-534`（0.62.0）
- 组织级：在 SCM 组织下建 `.agents` 仓（GitLab 用 `openhands-config`），放 `skills/`，对该组织所有仓库生效。【官方】[OH3] ；V0 代码读的是 `<org>/.openhands` 仓（`runtime/base.py:703-767`）。
- 公共：github.com/OpenHands/extensions（`skill.py:1166`），默认 marketplace 过滤。

**自动记忆（模型写）**
- **SDK Persistent Memory**（opt-in，默认关）：`AgentContext(load_memory=True)`。两层 `MEMORY.md` 索引 + 每日日志 `YYYY-MM-DD.md`。首次提交 `ca3361b6` 2026-07-22 "feat: add opt-in persistent memory across sessions (#4178)"，落在 v1.37.0（2026-07-23）。【官方】[OH4] ；`openhands-sdk/openhands/sdk/context/memory.py`
- 未开启时，系统提示默认指引是："Use `AGENTS.md` under the repository root as your persistent memory for repository-specific knowledge and context."（`static.py:124`）——即把 AGENTS.md 当记忆文件让模型追加。
- V0/Cloud 的 `/remember` skill（`enterprise/skills/agent_memory.md`，trigger `/remember`）：让 agent 把仓库结构、常用命令、风格偏好写进 `.openhands/microagents/repo.md`，**必须先列出条目让用户确认**。
- **OpenHands Cloud/Enterprise 的按用户持久记忆**：user 表新增 `enable_memory_context`（默认 false）和 `memory_context`（Text）（migration 167，Create Date 2026-06-05）；`MemoryChangeCallbackProcessor`（首次提交 `0853ff4a` 2026-09-24）监听 `file_editor` 对 `MEMORY.md` 的编辑，把**新内容整体**写进用户记录；会话开始时 `maybe_inject_memory_context` 把它写到沙箱 `<project_dir>/.openhands/memory/MEMORY.md`。前端设置项文案 "Enable Persistent Memory — When enabled, the agent maintains a MEMORY.md file that survives across conversations"。【官方】enterprise `storage/user.py:65-74`、`app_conversation_service_base.py:852-900`、`memory_change_callback_processor.py:1-16, 40-48`、`frontend/src/i18n/declaration.ts` 7380-7399

**会话记忆（摘要）**
- Condenser：`LLMSummarizingCondenser`（`max_size` 默认 240 个事件，`keep_first` 默认 2，`max_tokens` 可选）；到阈值时把前一半事件替换成一条摘要事件（Condensation 事件追加到 append-only 日志，View 应用它）；上下文溢出时做 hard reset 全量摘要。【官方】`openhands-sdk/openhands/sdk/context/condenser/README.md`、`llm_summarizing_condenser.py:54-69`；[OH5]
- Cloud 有 "Enable memory condensation" 设置与 `/settings/condenser` 页。

**向量库长期记忆：已删除。** V0 曾有 `openhands/memory/long_term_memory.py`（chromadb + llama_index，0.28.0 树中存在），`5128377b` 2025-03-11 "remove llamaindex (#7151)" 删除；0.35.0 起 `openhands/memory/` 只剩 `condenser/`、`conversation_memory.py`、`memory.py`（microagent 召回）、`view.py`。V1 SDK 没有任何向量库/embedding 记忆。

##### ② 存在哪
- 用户层：`~/.openhands/memory/MEMORY.md`（`memory.py:70` `get_user_persistence_dir() / "memory" / "MEMORY.md"`；可用 `OH_PERSISTENCE_DIR` 改根）。
- 项目层：`<workspace>/.openhands/memory/MEMORY.md`（`memory.py:23` `MEMORY_INDEX_RELPATH = ".openhands/memory/MEMORY.md"`）；日志同目录。
- Skills：用户 `USER_SKILLS_DIRS`（`skill.py:949-953`）= `~/.agents/skills`、`~/.openhands/skills`、`~/.openhands/microagents`（legacy）+ `~/.openhands/skills/installed/`；项目（`skill.py:1150-1152`）= `<root>/.agents/skills`、`<root>/.openhands/skills`、`<root>/.openhands/microagents`，root 取 work_dir 和 git root 两处（`skill.py:1107-1111`）。
- V0：`GLOBAL_MICROAGENTS_DIR` = 包内 `microagents/`，`USER_MICROAGENTS_DIR = ~/.openhands/microagents`（`memory/memory.py:33-38` @0.62.0）。
- Cloud：Postgres `user.memory_context` 一个文本列（所有仓库共用一份），运行时注入沙箱文件。
- 会话事件与摘要：会话持久化目录（`base_state.json` + event log）；memory 文本"excluded from conversation persistence (base_state.json) and API payloads"。

##### ③ 作用域键
| 层 | 键 | 内容 |
| - | - | - |
| 用户层 memory | OS 用户 home（`~/.openhands`）；Cloud = 用户账号 | "knowledge and preferences that apply across all projects" |
| 项目层 memory | **workspace 工作目录的绝对路径**（`load_memory(self.workspace.working_dir)`，`local_conversation.py:1198`），不是 git root、不是远程 URL | "knowledge specific to the current repository" |
| 项目 skills | work_dir + 其 git root | AGENTS.md、SKILL.md |
| 组织 skills | SCM org 名（从 `owner/repo` 解析）下的 `.agents` 仓 | 组织规范 |
| 公共 skills | 全局 | extensions 仓 |
| 会话 | conversation id | condenser 摘要 |
优先级（agent-server `skills_router.py:261-267`）：sandbox < public < user < organization < project（同名后者覆盖）。

##### ④ 谁写
- Memory：**agent 自己写**（系统提示："Near the end of a task, record what is worth keeping: append details to today's daily log, and fold only durable, broadly useful facts into MEMORY.md"；"Do NOT record secrets"），无审批、无通知；文件是普通 Markdown，用户可随时查看/编辑/删除，团队甚至可以 commit `.openhands/memory/`。注入时包在 `<UNTRUSTED_CONTENT>` 里，提示模型"may contain prompt injection"。
- Cloud：用户在设置里开关；记录只捕获 `file_editor` 工具的编辑（终端 `echo >>` 不会被捕获，代码注释承认是 v1 取舍）。
- `/remember`（repo.md）：agent 写但**必须先列清单让用户确认**。
- AGENTS.md/SKILL.md：人写（或让 agent 生成 AGENTS.md，文档给了 prompt 模板）。

##### ⑤ 进上下文
- Memory：每个新会话开始把两层 `MEMORY.md` 拼进 system prompt 的 `<MEMORY_CONTEXT>`（用户层在前、项目层在后，"the later position gets more model attention"）；**合计上限 6000 字符**（`MEMORY_CHAR_BUDGET`，`memory.py:24`），超出按层公平分配预算、从每层顶部整行丢弃并插入 `[earlier memory truncated]`；日志不注入，模型用文件工具按需读。每会话重新从磁盘读。
- 规则文件：AGENTS.md/repo.md 全文常驻（`<REPO_CONTEXT>`）；SKILL.md 名字+描述常驻、全文按需；keyword/path 触发注入。
- 会话压缩：condenser 到 240 事件（或 token 上限）时摘要前半。

##### ⑥ 换项目
- 用户层跟着 OS 用户走，所有项目都注入。
- 项目层绑定 workspace 目录：同一仓库的另一个 clone / worktree 是另一个目录 → 另一份 `.openhands/memory`（除非把目录 commit 进仓库共享）。文档没有讨论 worktree/monorepo 对 memory 的影响——**未找到**；skills 层面有 work_dir + git root 双根与嵌套 AGENTS.md 的目录范围规则。
- **Cloud：一个用户只有一份 `memory_context`，不分仓库，且被写到每个会话的"项目层"路径** → 仓库 A 学到的项目知识会出现在仓库 B 的会话里（代码事实；官方文档**未找到**说明）。
- 组织 skills 按 SCM 组织过滤；仓库 skills 只在该仓库。

##### 一句话结论
作用域键 = 两层：OS 用户 home（`~/.openhands/memory`）+ workspace 绝对路径（`.openhands/memory`），Cloud 退化为"用户账号一份、注入到任意仓库"；记忆 = opt-in 的 agent 自维护 `MEMORY.md`（6000 字符常驻 + 日志按需）与 condenser 会话摘要，规则文件 = AGENTS.md/repo.md（常驻）+ SKILL.md（关键词/路径/调用触发），向量库已于 2025-03 删除。

##### 未证实 / 未找到
- OpenHands Cloud 文档层面对 per-user memory 的说明（只有代码与设置文案）；是否/何时在 app.all-hands.dev 对所有用户开放。
- Cloud 记忆是否区分仓库（代码显示不区分）。
- `OpenHands/legacy` 仓 `openhands/memory` 路径 API 返回 404，V0 结论以 `OpenHands/OpenHands@0.62.0` 为准。

#### 4.2.4 Goose（Block，开源，Rust）
版本基线：`block/goose` main `540df77c`（2026-10-06），最新 release v1.53.0（2026-10-02）。源码：`crates/goose-mcp/src/memory/mod.rs`（最近改动 `a5a297fc` 2026-09-28）、`crates/goose-mcp/src/lib.rs`、`crates/goose/src/hints/load_hints.rs`、`crates/goose/src/config/paths.rs`、`crates/goose/src/context_mgmt/mod.rs`、`crates/goose/src/session/session_manager.rs`、`crates/goose/src/prompts/system.md`、`crates/goose/src/agents/prompt_manager.rs`。文档（已迁到 goose-docs.ai）：`docs/mcp/memory-mcp`、`docs/guides/context-engineering/using-goosehints`（仓库 `documentation/docs/guides/context-engineering/using-goosehints.md`）、`docs/guides/sessions/smart-context-management`、`docs/guides/sessions/session-management`。

本节简称：memory-mcp 文档 = [GS1]；using-goosehints 等其余文档以仓库内 `documentation/` 文件为准。

##### ① 持久记忆
**规则文件（人写）**：`.goosehints` 与 `AGENTS.md`（`GOOSE_HINTS_FILENAME`/`AGENTS_MD_FILENAME`，`load_hints.rs:10-11`；可用 `CONTEXT_FILE_NAMES` 改）。全局 + 本地，支持 `@file` 导入、嵌套目录。【官方】using-goosehints.md；`load_hints.rs:232-312`
**记忆（模型写）**：内置 **Memory extension**（MCP server，`goose-mcp` feature `memory-server`，`lib.rs:31-45`；**不是默认开启**，`DEFAULT_EXTENSION = "developer"`（`config/extensions.rs:9`），需用户在 Extensions 里 toggle）。四个工具：`remember_memory(category, data, tags, is_global)`、`retrieve_memories(category|"*", is_global)`、`remove_memory_category`、`remove_specific_memory`（`mod.rs:58-100`）。形态：**纯文本文件，每个 category 一个 `<category>.txt`**，每条带 `# tags` 行。无向量库。【官方】`mod.rs`；[GS1]
**会话**：会话存 SQLite `~/.local/share/goose/sessions/sessions.db`（`SESSIONS_FOLDER="sessions"`、`DB_NAME="sessions.db"`，`session_manager.rs:29-30`，根为 `Paths::data_dir()`），表有 `working_dir` 列；自动压缩（默认 80%，`GOOSE_AUTO_COMPACT_THRESHOLD`，`context_mgmt/mod.rs:224-271`，模板 `goose-context-management/src/prompts/compaction.md`）、`/compact` `/summarize`、工具调用输出后台摘要（`GOOSE_TOOL_CALL_CUTOFF`）、`GOOSE_CONTEXT_STRATEGY=summarize|truncate|clear`。压缩只影响会话上下文，不写入记忆文件。【官方】smart-context-management 文档；源码同上

##### ② 存在哪
- 全局：`~/.config/goose/memory/<category>.txt`（`mod.rs:133-135`：`choose_app_strategy(APP_STRATEGY).in_config_dir("memory")`，APP_STRATEGY = Block/goose，`lib.rs:14-18`；文档明示 `~/.config/goose/memory/`）。
- 本地：`<working_dir>/.goose/memory/<category>.txt`（`mod.rs:205-214`）；`working_dir` 来自 MCP 请求 `_meta` 的 `agent-working-dir` 头（`WORKING_DIR_HEADER`，`mod.rs:21,42-48`），缺省退到进程 `current_dir()`。
- goosehints 全局：`~/.config/goose/.goosehints`、`~/.config/goose/AGENTS.md`，另加 `Paths::in_agents_home_dir("AGENTS.md")`（`~/.agents/AGENTS.md`，`load_hints.rs:240-248`）；本地：从 git root 到 cwd 的每一级目录（`get_local_directories`，`load_hints.rs:187-209`；无 git 时只看 cwd），嵌套目录在访问文件时再加载（`load_new_hints`）。

##### ③ 作用域键
- 记忆：**一个布尔 `is_global`** → 用户全局（config dir）或 **会话工作目录的绝对路径**（`.goose/memory`，不找 git root、不认远程 URL、不向上查父目录）。category 只是文件名（必须是单一路径分量，`mod.rs:190-203`）。没有 agent/profile/bot 维度；不区分模型或 provider。
- 规则文件：用户全局 + git root→cwd 目录链（monorepo 文档专门讨论了分层 `.goosehints`）。
- 会话：按 session id，记录 `working_dir`。

##### ④ 谁写
- 记忆由模型通过工具写；server 指令要求"Save proactively when users share preferences… **Always confirm with the user before saving**… clarify storage scope (local vs global)"（`mod.rs:119-131`）——是提示级约束，无硬审批。文件是明文 txt，用户可直接看/改/删，也可让 goose "forget"。文档说 goose 可能"automatically suggest saving them to memory"。
- goosehints：人写（Desktop 有编辑 UI，改后需重启会话）。

##### ⑤ 进上下文
- **全局记忆：常驻**。Memory server 启动（`MemoryServer::new()`）时 `retrieve_all(true, None)` 读完所有全局 category，拼进 server 的 `instructions`（"Global Memories:\nCategory: …\n- …"，`mod.rs:143-170`），goose 把每个扩展的 `instructions` 渲染进 system prompt（`prompts/system.md:28-29` `{{extension.instructions}}`）→ 每次请求都带。**没有任何大小上限或裁剪**。
- **本地记忆：不预载**。`new()` 没有工作目录，只有工具调用时才带 `agent-working-dir`；所以 `.goose/memory` 的内容只能靠模型调用 `retrieve_memories` 拉取（文档"goose loads all saved memories at the start of a session and includes them in every prompt"与代码只对全局成立——本地部分**未证实**）。
- goosehints：会话开始加载，"adds hints to the system prompt for every request"；分 "### Global Hints" / "### Project Hints" 两段。
- 会话压缩：80% 阈值自动摘要。

##### ⑥ 换项目
- 全局记忆跟用户走且无条件注入到任何目录的会话（没有"按项目过滤"）；本地记忆严格绑定目录：在子目录开会话看不到父目录的 `.goose/memory`，同仓库另一个 clone/worktree 是另一份（除非把 `.goose/memory` 提交进仓库）。
- 官方文档对 worktree/多 clone 下记忆的讨论：**未找到**；对 monorepo 只讨论了 `.goosehints` 分层。
- 2025-06-05 官方博客 "What's in my goosehints file" 有 goosehints-vs-memory 对比图（`documentation/blog/2025-06-05-…`）。

##### 一句话结论
作用域键 = `is_global` 二选一：用户 config dir vs 会话工作目录绝对路径；记忆 = 模型用工具写的 `<category>.txt`（全局全量塞进 system prompt、本地只能工具检索、无上限），规则文件 = `.goosehints`/`AGENTS.md`（全局 + git root→cwd 分层，常驻）。

##### 未证实 / 未找到
- 本地记忆是否在某处被预载进提示（代码未见）。
- 记忆大小上限 / 过期 / 合并机制（无）。
- macOS 上实际路径是否为 `~/.config/goose`（etcetera 策略；文档如此写，未在 mac 上实测）。

#### 4.2.5 Trae（字节跳动；国际版产品名已改为 TraeCode，国内版文档域名 docs.trae.cn）
版本基线：[TR1] 、[TR2] （英文，内容是前端嵌入 JSON，用 curl 抓取解析，2026-10-06）；[TR3] 、[TR4] （中文）。页面无日期/版本号；页面内嵌的翻译条目时间戳 2025-11-20、2025-12-31，文档对象 ID 对应 2025-12 创建。官方中文社区 2026-03-22 帖子称记忆当时「仅国际版支持，国内版暂未上线」（[TR5] ），现在中文文档已有该页。

本节简称：国际版 memories / rules = [TR1] / [TR2]；国内版 记忆 / 规则 = [TR3] / [TR4]；社区帖 2777 = [TR5]。

##### ① 持久记忆
- **记忆（模型自动）**：**有**，Markdown 文件形态，两类：
  - 全局记忆 `user_profile.md`：「Applies to all local projects for the current user」。
  - 项目记忆 `project_memory.md`：「Applies only to the current local project for the current user」【官方】[TR1]。
  - 不会自动保存的内容：一次性/临时指令、模糊偏好、敏感信息（密码/隐私）【官方】。
- **规则文件（人写）**：全局规则 `~/.trae/user_rules`；项目规则 `.trae/rules/` 目录（Markdown + frontmatter `alwaysApply / description / globs`，另有 `scene: git_message`），子文件夹最多 3 层嵌套，且「supports reading the `.trae/rules/` folder in any subdirectory of the project」（子目录规则在提及/读取该目录文件时自动生效）；设置里可开启「Include AGENTS.md in the context」「Include CLAUDE.md in context」读取根目录 AGENTS.md / CLAUDE.md / CLAUDE.local.md【官方】[TR2]。

##### ② 存在哪
- 国际版：`~/.trae/memory/user_profile.md`；`~/.trae/memory/projects/{project_path}/project_memory.md`（Windows `%userprofile%/.trae/memory/...`）【官方】。
- 国内版：`~/.trae-cn/memory/user_profile.md`；`~/.trae-cn/memory/projects/{project_path}/project_memory.md`【官方】[TR3]。
- 「Memory data is stored locally and cannot be shared across devices」【官方】。
- 规则：`~/.trae/user_rules`（国内 `~/.trae-cn/user_rules`）；项目内 `.trae/rules/`【官方】。

##### ③ 作用域键
- 用户全局 + **项目路径**两层。项目键 = `{project_path}`，文档只给占位符，编码形式（绝对路径转义/哈希）**未证实**；论坛与掘金文章也没有给出实际目录样例。
- 没有 agent/profile 维度；没有会话级记忆文件。

##### ④ 谁写
- 都有：「Automatically create memories: AI automatically identifies preferences or rules that are valuable... Automatically update memories: When AI identifies changes...」；也可按要求 create/update/delete；用户可在 Settings > Rules & Memories > Memory 直接打开 `user_profile.md` / `project_memory.md` 编辑并保存【官方】。
- **无审批/通知机制的描述**；只有一个总开关「Toggle the Memory switch on」【官方】。

##### ⑤ 进上下文
- 记忆如何注入（常驻 vs 检索）、大小/条数上限、裁剪规则：官方文档**均未说明**（未证实）。按形态（两个 md 文件）推测是整文件注入，但无证据。
- 规则：四种生效方式（always apply / 文件 glob / 智能 description / 手动 `#Rule`）；字符上限未找到【官方】[TR2]。

##### ⑥ 换项目
- 全局记忆/全局规则跟用户走；项目记忆按 `{project_path}` 隔离，换目录即另一份。
- worktree / monorepo / 多 clone 的官方讨论：**未找到**（查了 [TR1]、/ide/rules、[TR3]、/ide_rules、forum.trae.cn 2777 与 45869）。

##### 一句话结论
作用域键 = 用户全局（`~/.trae/memory/user_profile.md`）+ 项目路径（`~/.trae/memory/projects/{project_path}/project_memory.md`）；记忆 = AI 自动维护的两个本地 Markdown 文件（无审批、不跨设备）；规则文件 = `~/.trae/user_rules` + `.trae/rules/*.md`（+ 可选 AGENTS.md/CLAUDE.md）。

##### 未证实 / 未找到
- `{project_path}` 的实际编码形式：未证实。
- 记忆的注入方式、上限、裁剪：未找到。
- 功能上线版本号/日期：未找到（文档无日期；社区帖 2026-03 称仅国际版有）。
- TraeCode CLI 是否有同样的记忆：未找到。

#### 4.2.6 GitHub Copilot CLI + Copilot Memory
版本基线：Copilot CLI 最新 **v1.0.92（2026-10-05）**；源码仓 github/copilot-cli 只含 `README.md`/`changelog.md`/`install.sh`（二进制闭源），clone HEAD `6783c47`（2026-10-05）。本机未安装 `copilot`，`~/.copilot/` 仅 `config.json`（`firstLaunchAt 2026-07-29`，注释「User settings belong in settings.json. This file is managed automatically.」）、`ide/`、`logs/`。核对页面（2026-10-06 抓取）：
- 【官方】[GH1]（public preview）
- 【官方】[GH2]
- 【官方】[GH3]
- 【官方】[GH4]
- 【官方】GitHub Changelog：2025-12-19「Copilot memory early access for Pro and Pro+」、2026-01-15「Agentic memory for GitHub Copilot is in public preview」、2026-03-04「on by default for Pro and Pro+」（仅搜索摘要）、2026-05-26「more controls for deletion, scope, and the Copilot CLI」、2026-09-25「Agentic autofix now uses Copilot Memory」（仅搜索摘要）
- 【官方】工程博客 2026-01-15 [GH5]
- 【官方】github/copilot-cli-for-beginners `04-agents-custom-instructions/README.md`

（注：常被说成「2025-11 前后公告」，实际首个 changelog 是 **2025-12-19**（Pro/Pro+ early access），2025-11-12 的公告是「agent-specific instructions」而非 memory。）

本节简称：「concept 页」= [GH1]；cli-config-dir-reference = [GH2]；add-custom-instructions = [GH3]；response-customization = [GH4]；「工程博客」= [GH5]。GitHub Changelog 各条目只以日期与标题引用，未单独编号。

##### ① 持久记忆
- **规则文件**（CLI 读的，add-custom-instructions 页）：用户级 `$HOME/.copilot/copilot-instructions.md`、`$HOME/.copilot/instructions/**/*.instructions.md`（0.0.412，2026-02-19 加入）；仓库级 `.github/copilot-instructions.md`、`.github/instructions/**/*.instructions.md`（`applyTo` glob）；agent 文件 `AGENTS.md`、`CLAUDE.md`、`GEMINI.md`；`COPILOT_CUSTOM_INSTRUCTIONS_DIRS` 指定的目录；1.0.89（2026-09-28）起还读 Claude Code 的 `.claude/rules`。`@` 导入在 copilot-instructions.md / AGENTS.md / CLAUDE.md 展开（GEMINI.md 与 `*.instructions.md` 不展开）。GitHub.com 侧还有个人 custom instructions（账号级，存在 Copilot Chat 设置）和组织级 instructions（Business/Enterprise）。
- **记忆**：Copilot Memory（public preview）。结构化条目：`subject` / `fact` / `citations`（指向代码位置或用户原话）/ `reason`（工程博客）。两种：**repository-level facts**（约定、架构决策、构建命令…，对该仓库所有有权限者共享）与 **user-level preferences**（个人偏好，跨仓库、仅本人）。用于 Copilot cloud/coding agent、code review、CLI、agentic autofix；「code review uses facts only」。
- CLI 另有「cross-session memory (experimental)」（0.0.412：「ask about past work, files, and PRs across sessions」）与 `session-store.db`（「SQLite database used by the CLI for cross-session data」）——两者是否同一物**未证实**。

##### ② 存在哪
- Copilot Memory 在 **GitHub 服务端**，不在仓库文件也不在本地：仓库事实在 Repository Settings > Copilot > Memory 查看/删除；个人偏好在 `github.com/settings/copilot/memory`；Business/Enterprise 下「Preferences are owned by the billing entity」，管理员可批量/按人导出与删除。【官方】concept 页、2026-05-26 changelog。
- CLI 本地 `~/.copilot/`（`COPILOT_HOME` 或已弃用的 `--config-dir` 可改）：`config.json`（内部状态：认证、插件元数据）、`settings.json`（用户设置，1.0.35 起分离）、`copilot-instructions.md`、`instructions/`、`agents/`、`skills/`、`hooks/`、`extensions/`、`mcp-config.json`、`lsp-config.json`、`permissions-config.json`、`providers.json`（BYOK）、`session-state/`（会话历史，供 `--resume/--continue`）、`history-session-state/`（旧格式，0.0.342）、`command-history-state/`、`session-store.db`、`logs/`、`ide/`、`installed-plugins/`、`plugin-data/`、`mcp-oauth-config/`、`mcp-secrets/`、`pkg/`（自更新）。【官方】cli-config-dir-reference、changelog。

##### ③ 作用域键
| 层 | 键 | 内容 |
|---|---|---|
| 仓库 | GitHub **repository（owner/repo）**：「can only be created in response to actions taken within that repository by contributors with write permissions, and can only be used in tasks on that same repository initiated by users with read permissions」 | repository-level facts |
| 用户 | GitHub **账号**（跨仓库；企业下归计费实体所有） | user-level preferences |
| 无仓库上下文 | 可用 scope 受限（CLI 1.0.49「limits available scopes when no repository context is present」） | — |
| 会话 | `session-state/`（本地） | 会话记录，非记忆 |
| 规则文件 | 仓库根 → cwd 中间目录 → 正在处理的文件所在嵌套目录（1.0.11 起「every directory level from the working directory up to the git root, enabling full monorepo support」）；`.github/instructions/**` 不看中间目录 | instructions |

- 键是 GitHub 仓库身份（而非本地路径）：CLI 1.0.49 的权限提示直接显示 `owner/repo`；1.0.5「Memory storage errors now indicate when repository doesn't exist or you lack write access」。
- 没有 agent 实例/profile 维度的记忆（custom agents 只是指令文件）。

##### ④ 谁写
- **agent 写**：各 agent 在用户发起的活动中调用存储工具（CLI 0.0.384「add memory storage tool」、0.0.385「`store_memory` tool is only included when memory is enabled」）；也会从已关闭的 PR 自动捕获（concept 页「Automatic capture from closed pull requests」）。「Memories … created only in response to Copilot activity initiated by users」且需写权限。
- **审批与可见性**：CLI 每次存储弹确认，提示显示 scope（1.0.41），并注明「user scope」或具体 `owner/repo`、时间线标 `(for user)` / `(shared with repository collaborators)`（1.0.49）；时间线显示 subject/fact/citations（0.0.411）。`vote_memory` 工具（1.0.55 限流）用于给记忆投票，语义**未证实**。
- **查看/删除/关闭**：个人偏好在个人设置查看删除；仓库事实由仓库所有者在 Repository Settings 查看删除；仓库管理员可在 Copilot feature controls 关闭本仓库 memory（「Repository-level facts will no longer be stored or read」）；组织策略开启、用户可 opt-out；个人计划默认开（2026-03-04 起 Pro/Pro+ 默认开）；CLI `/memory on|off|show`（1.0.49，持久）。让 Copilot「forget」时它只会「point you to the right place to remove the memory」。
- 过期：28 天未使用自动删除，成功验证并使用时计时器重置。
- 1.0.85：「Add session and memory import commands for the semantic JSONL interchange format」（可导入记忆）。

##### ⑤ 进上下文
- **常驻注入**：会话开始时检索该仓库（+该用户）的记忆并「included in the prompt」（工程博客；CLI 0.0.384「Inject repo memories in the prompt」）；长会话每 30 分钟刷新记忆上下文（1.0.71）；SDK 可在 `session.create/resume` 配置（1.0.62）。
- **读时校验**：「just-in-time verification」——使用前按 citations 核对当前分支代码是否仍成立（concept 页「Validated against current branch before use」）。
- 搜索/加权检索列为未来工作（博客）。大小上限：**未找到**。
- 规则文件：全量进 system prompt；相同内容的 copilot-instructions.md 与 CLAUDE.md 去重（1.0.26）；`.github/instructions/*.instructions.md` 不再整体常驻（1.0.35）；1.0.5 有实验性的按轮嵌入检索（针对 MCP/skill 指令）。

##### ⑥ 换项目
- 用户偏好跟账号走，跨仓库可用；仓库事实严格隔离在同一 GitHub 仓库（fork 是否共享**未找到**）；同一仓库内跨功能共享（coding agent 发现的事实 code review 也能用）。
- 本地路径/worktree/monorepo 对记忆无影响（键是 GitHub 仓库），官方未就 worktree/多 clone 专门讨论 → **未找到**；规则文件层面的 monorepo 由目录遍历支持。
- 无仓库上下文（不在 git 仓库里跑 CLI）时只剩 user scope。

##### 一句话结论
作用域键 = **GitHub 仓库（owner/repo）+ GitHub 账号** 两层、存在 GitHub 服务端、28 天未用即过期；记忆 vs 规则文件分别 = agent 调工具写的 {subject, fact, citations, reason} 条目（会话开始注入、读时按引用校验）vs 人写的 `.github/copilot-instructions.md` / `.github/instructions/*.instructions.md` / AGENTS.md / CLAUDE.md / GEMINI.md / `~/.copilot/copilot-instructions.md` 等（本地目录遍历、常驻）。

##### 补充：模式调研里的相关证据

- **两类作用域与受众不同**：repository-level facts＋user-level preferences 是明确的两层；CLI 可以同时用两者，code review 只用仓库事实；仓库事实只用于同一仓库，并在使用前按当前分支核对引用 [GH1]。企业用户的偏好另受 billing entity 约束 [GH1]。这是「人的习惯」与「仓库事实」拥有不同生命周期和受众的直接先例；这里的身份是 GitHub 用户，不是 agent persona。
- **双层也会在「写入归属」处失败**：Discussion #201874（2026-07-14，open）报告某客户的 Terraform 习惯、内部工单联系人被当作用户偏好带进无关的 Obsidian 仓库 [FR5]（证据等级见 §7）。
- **规则层**：VS Code 本地模式下的嵌套 AGENTS.md 仍是实验功能，`chat.useNestedAgentsMdFiles` 默认关闭，不能把 Copilot 各入口都概括成「自动分层加载」[GH6]。
- 两份调研对「用户偏好怎么进上下文」的表述不完全一致，见 §11 分歧 2。

##### 未证实 / 未找到
- 注入记忆的数量/大小上限：**未找到**。
- CLI「cross-session memory (experimental)」与 `session-store.db` 的关系及其是否本地：**未证实**。
- `vote_memory` 的语义：**未证实**。
- fork 与原仓库是否共享 repository facts：**未找到**。
- 2026-03-04、2026-09-25 两篇 changelog 只读到搜索摘要，未逐字核对。

### 4.3 用户／账号全局：项目只是内容里的分组

记忆只有一个用户（或账号、用户×组织）级的根；项目只体现在记忆内容里（Codex 的 `### <project scope>` 标题、Devin 的 `my-app/` 文件夹），不是存储键。

#### 4.3.1 OpenAI Codex（Codex CLI / 桌面 / 云）+ ChatGPT 记忆
版本基线：本机 `codex --version` = **codex-cli 0.160.1**（GitHub 最新 release `rust-v0.160.1`，2026-10-05）。源码 `git clone --depth 1` openai/codex 到 `$SCRATCH/src-codex/`，HEAD = `57d57df` (2026-10-06 15:33 UTC)。文档（developers.openai.com/codex/* 现 308 跳到 learn.chatgpt.com，2026-10-06 抓取）：
- 【官方】[CX1]（含 .md 原文）
- 【官方】[CX4]
- 【官方】[CX3]
- 【官方】[CX5]
- 【官方】ChatGPT：[CH1]、[CH2]（直抓 403，经浏览器读取；页面标「Updated: 17 天前」≈ 2026-09-19）
- 【本机】`~/.codex/`（只读）：有 `memories/`（空，2026-03-30 建）、`memories_1.sqlite`、`sessions/`、`archived_sessions/`、`session_index.jsonl`、`history.jsonl`、`AGENTS.md`、`config.toml`（无 memories 配置，即本机未开）。

本节简称：memories 文档 = [CX1]；agents-md = [CX3]；config-reference = [CX4]；cloud-environment = [CX5]；ChatGPT 帮助中心 memory / projects = [CH1] / [CH2]。源码引用以 openai/codex `57d57df` 为准。

##### ① 持久记忆
- **规则文件**：`AGENTS.md`。全局：`~/.codex/AGENTS.override.md` 优先，否则 `~/.codex/AGENTS.md`，只取第一个非空（`codex-rs/codex-home/src/instructions/mod.rs:12-13,43`）。项目：从项目根到 cwd 每层目录取**一个**文件，候选顺序 `AGENTS.override.md` → `AGENTS.md` → `project_doc_fallback_filenames`（`codex-rs/core/src/agents_md.rs:43-45,272-290`）；项目根由 `project_root_markers`（默认 `.git`）向上查找，**不越过项目根**，无标记则只看 cwd（`agents_md.rs:9-17,189-245`）。合并上限 `project_doc_max_bytes` 默认 32 KiB（`core/src/config/mod.rs:255`）。
- **自动记忆（Memories）**：存在，默认关。`[features] memories = true` 开启；feature 定义 `codex-rs/features/src/lib.rs:1231-1235`（key `memories`，`Stage::Stable`，`default_enabled: false`；旧名 `memory_tool`，`features/src/legacy.rs`）。形态 = **SQLite + 一组 markdown 文件 + 一个内部 git 基线**：两阶段后台流水线（`codex-rs/memories/README.md`）：Phase 1 按线程把 rollout 交给模型抽成 `raw_memory` + `rollout_summary`（存 DB `stage1_outputs`）；Phase 2 全局合并成文件并跑一个「consolidation 子代理」产出 `memory_summary.md`（≤10,000 UTF-8 字节，`write/templates/memories/consolidation_v2.md`）、`MEMORY.md`、`skills/`。另有 `[memories] version = "v2"` 变体（`memories_v2_1.sqlite`、`*_v2.md` 模板）。
- 还有「外部 agent 记忆导入」：feature `external_agent_memory_import`（`Stage::UnderDevelopment`，`features/src/lib.rs:1237-1239`）与 `codex-rs/external-agent-migration/src/memory_import.rs` → 细节**未证实**。
- **ChatGPT 记忆**（与 Codex 本地记忆是两套）：文档原话「ChatGPT web uses ChatGPT memory, while local Codex clients use a separate local memory store and controls」「ChatGPT Work uses the memory settings available to your account and workspace; it doesn't use a local Codex memory store」。ChatGPT 侧形态：saved memories（显式条目）+ Reference chat history（从历史聊天派生、会变）+ 「improved memory」的 memory summary；Projects 有 default / project-only memory。

##### ② 存在哪
- 记忆根 = `$CODEX_HOME/memories/`（默认 `~/.codex/memories/`）：`codex-rs/memories/read/src/lib.rs:13-15`、`memories/write/src/lib.rs:119`（`codex_home.join("memories")`）。文件名常量：`write/src/lib.rs:38-40`（`extensions/`、`rollout_summaries/`、`raw_memories.md`）、`:114`（`phase2_workspace_diff.md`）、`write/src/workspace.rs:87`（`MEMORY.md`）、`:101`（`memory_summary.md`）；用户补充笔记放 `extensions/ad_hoc/notes/<timestamp>-<slug>.md`（`ext/memories/templates/memories/read_path.md`）；根目录本身用 `~/.codex/memories/.git` 做基线（README「Phase 2」）。
- DB：`~/.codex/memories_1.sqlite`（`codex-rs/state/src/sqlite.rs:36`），v2 为 `memories_v2_1.sqlite`（`:109`）；schema `state/memory_migrations/0001_memories.sql`：`stage1_outputs(thread_id PK, raw_memory, rollout_summary, rollout_slug, generated_at, usage_count, last_usage, selected_for_phase2…)` + `jobs`。
- 会话：`~/.codex/sessions/YYYY/MM/DD/rollout-<YYYY-MM-DDTHH-MM-SS>-<uuid>.jsonl`（`codex-rs/rollout/src/lib.rs:86-87` `SESSIONS_SUBDIR`/`ARCHIVED_SESSIONS_SUBDIR`；`rollout/src/rollout_file_name.rs:51`）；`session_index.jsonl`（id/thread_name/updated_at）；`history.jsonl`（`history.persistence = save-all|none`、`history.max_bytes`）。恢复：`codex resume [SESSION_ID|name] [--last] [--all]`（本机 `codex resume --help`）。
- 文档立场：「Treat these files as generated state… don't rely on editing them by hand as your primary control surface.」
- 云：cloud environment 文档只讲容器缓存（「caches container state for up to 12 hours」）、setup/maintenance 脚本、env/secrets（secrets 在 agent 阶段前移除）、读 AGENTS.md；**没有任何记忆持久化**的描述 → **未找到**。
- ChatGPT 记忆：OpenAI 云端账号数据（Settings > Personalization > Memory）。

##### ③ 作用域键
- **用户全局（按 CODEX_HOME，即按 OS 用户）**，不按项目分目录：记忆根只有一个；Phase 1 的输入筛选不看 cwd；读取工具 `list/read/search` 的 `resolve_scoped_path` 只是把相对路径限制在根目录内（`ext/memories/src/local.rs:32-44`）。项目信息只作为**内容元数据**：`raw_memories.md` 每个线程写一行 `cwd: …`（`write/src/storage.rs:66`）；Phase 1 提示词给 `rollout_primary_cwd_hint` / `git_branch_hint` 并声明「A single session may involve multiple working directories」（`write/templates/memories/stage_one_input_v2.md`）；合并提示词要求 `memory_summary.md` 内按 `### <project scope>` / `#### <YYYY-MM-DD>` 分组，并且「`memory_summary.md` will be injected at the beginning of every new session for the same user」（`consolidation_v2.md`）。
- 线程级：每个线程在 state DB 有 `memory_mode`（`generate_memories=false` → `"disabled"`；`disable_on_external_context=true` 且用了 MCP/web search/tool search → `"polluted"`）（`codex-rs/config/src/types.rs:324-330`）。`/memories` 命令（TUI `slash_command.rs:151`「configure memory use and generation」）按当前聊天切换「用现有记忆」与「本聊天可作为未来记忆输入」。
- 配置层：`~/.codex/config.toml` 全局 + 受信项目的 `.codex/config.toml`；`projects."<abs path>".trust_level` 按**绝对路径**键（「Mark a project or worktree as trusted or untrusted」）。
- 宿主：IDE 扩展用所连接 Codex 宿主的本地库；ChatGPT 桌面 app 与 CLI 同一本地库；ChatGPT Work/网页用账号/工作区的 ChatGPT 记忆。
- 规则文件 AGENTS.md：全局（CODEX_HOME）+ 项目根→cwd 链（项目根 = 最近的 `.git` 等标记；git worktree 有自己的 `.git` 文件故自成一根）。
- **ChatGPT 记忆**：键 = **账号**（Plus/Pro/Free；Enterprise 则账号 + 工作区策略）。Projects：`Default memory`（项目聊天可引用 saved memories 与同项目其他聊天；Enterprise/Edu 下项目内外互不引用；其他计划可引用项目外对话）或 `Project-only memory`（不引用已保存记忆；只在项目内互相引用；项目外也引用不到项目内）；**共享项目强制 project-only 且不能改回**；没有「全局把所有项目设为 project-only」的开关；project memory 不提供记忆列表（「Project memory does not show a list of memories like personal memory」）。Temporary chat 不产生/更新记忆。

##### ④ 谁写
- 模型/后台写：根会话启动时触发（非 ephemeral、功能开启、非子代理、state DB 可用），异步跑 Phase 1/2；门槛：线程空闲 ≥ `min_rollout_idle_hours`（默认 6）、年龄 ≤ `max_rollout_age_days`（30，夹到 0–90）、每次启动 ≤ `max_rollouts_per_startup`（16，上限 128）、剩余额度 ≥ `min_rate_limit_remaining_percent`（25%）；合并保留 `max_raw_memories_for_consolidation`（256，上限 4096）、`max_unused_days`（30）；可指定 `extract_model` / `consolidation_model`。生成字段会脱敏（「redacts secrets」）。合并子代理「runs with no approvals, no network, local write access only」（README）。
- 用户：不建议手改生成文件；显式「记住/忘掉/纠正」时，模型**只能**往 `extensions/ad_hoc/notes/` 追加小文件，由下次 consolidation 应用（`read_path.md`：「You can update the memories **only** when explicitly asked by the user」）。开关：Settings > Personalization 或 `config.toml`：`[features] memories`、`memories.generate_memories`、`memories.use_memories`、`memories.disable_on_external_context`、`memories.dedicated_tools`；`/memories` 按聊天切换。重置：TUI 有「memories reset」确认流程（`tui/src/chatwidget/snapshots/*memories_reset_confirmation.snap`）、app-server `memory_reset`（`app-server/tests/suite/v2/memory_reset.rs`）、CLI `codex debug clear-memories`（`cli/tests/debug_clear_memories.rs`）→ 具体 CLI 用法**未证实**。可见性：文件可直接打开；回答末尾附 `<oai-mem-citation>` 引用块标明用了哪些记忆文件/rollout id。
- ChatGPT：模型自动写（saved memories、chat history 派生、memory summary）+ 用户显式「记住…」；可在 Settings > Personalization > Memory 查看/纠正/删除/「Don't mention this again」/「Delete and turn off memory」；删除后日志最多保留 30 天；关闭 Reference chat history 后派生信息 30 天内删除；回答下方可显示 Sources。

##### ⑤ 进上下文
- 线程开始时，作为 **developer 角色** 片段注入（`ext/memories/src/extension.rs:56-84`，content kind `memories.instructions`）：`read_path.md` 指令 + **整份 `memory_summary.md`**（模板尾部 `MEMORY_SUMMARY BEGINS/ENDS`）。v2 按 8,900 字节切片（`core/src/context/memory.rs:37` `TruncationPolicy::Bytes(8_900)`，注释「capped below 10k tokens」）；`memory_summary.md` 自身被要求 ≤10,000 字节。
- 其余按需检索：模板让模型「quick memory pass」——先看摘要抽关键词 → 搜 `MEMORY.md` → 只开 1–2 个 `rollout_summaries/*.md` 或 `skills/` → 必要时搜原始 rollout；预算「<= 4-6 search steps」。有 `dedicated_tools` 时走 `list/read/search/ad_hoc_note` 工具（`ext/memories/src/tools/`），否则记忆根被加入沙箱可读根（`core/src/config/mod.rs:4188`）。
- 合并/裁剪：Phase 2 只取 top-N stage1 输出，按 `usage_count` 再按 `last_usage/generated_at` 排序，超过 `max_unused_days` 的剔除；`rollout_summaries/` 与选集同步并剪枝；用 git diff 决定是否需要跑合并代理。
- AGENTS.md：全部拼接常驻（根→cwd，后者在后），总量 32 KiB 截断。
- ChatGPT：按需（「ChatGPT looks for relevant context when it is likely to improve a response」「No separate storage limit for what ChatGPT can reference through chat history」）。

##### ⑥ 换项目
- 本地 Codex：记忆**全局**，用户偏好跟着走（`## User preferences`）；项目知识也跟着走——换到另一个仓库时，摘要里其他项目的 `### <project scope>` 段照样注入（设计如此，靠模板「the query mentions workspace/repo/module/path/files in MEMORY_SUMMARY」让模型自行判断相关性）。文档未讨论 worktree / monorepo / 多 clone 对记忆的影响 → **未找到**；只在信任层面提到「project or worktree」按绝对路径。
- 规则文件：AGENTS.md 链以 `.git` 标记为根，worktree 各自为根，monorepo 用子目录 AGENTS.md 叠加。
- 云：environment 按仓库配置、容器缓存 12 小时，**没有**跨任务记忆 → **未找到**。
- ChatGPT：账号级记忆跨所有聊天；Projects 可用 project-only 隔离；Enterprise/Edu 项目天然隔离。

##### 一句话结论
作用域键 = **用户全局（`$CODEX_HOME/memories/`，一份 `memory_summary.md` 注入该用户所有新会话）**，项目只是摘要里的分组标签；ChatGPT 记忆键 = 账号（Projects 可切 project-only）。记忆 vs 规则文件分别 = 后台两阶段流水线生成的 SQLite + markdown（摘要 ≤10KB 常驻，其余按需检索，默认关）vs 人写的 `~/.codex/AGENTS(.override).md` + 项目根→cwd 的 AGENTS.md 链（32 KiB 常驻）。

##### 补充：模式调研里的相关证据

- **官方对规则与记忆的分工**：文档要求必需的团队指南放进 AGENTS.md 或版本化文档，自动 Memories 只当 recall 层 [CX1]。
- **归并模板 v1 的组织方式**：patterns 调研读的是 commit `551bd409` 的 `consolidation.md`（v1），它把一个 `MEMORY.md` 组织成 Task Groups，并要求写明 scope、cwd／复用边界，以及 checkout 或时间相关的限制；细节靠关键词和 rollout 证据找回 [CX2]。这和本节③引用的 v2 模板（`57d57df`，`memory_summary.md` 按 `### <project scope>` 分组）是同一机制的两个版本、两个文件，见 §11 分歧 1。
- 两份调研都没有找到文档宣称「宿主每轮按当前项目对正文做确定性裁剪」[CX1][CX2]；这一点是「一份记忆＋按项目裁剪」先例核查（§8）的关键。

##### 未证实 / 未找到
- `codex debug clear-memories` 等重置命令的确切用法：**未证实**（仅见测试文件名）。
- 项目级 `.codex/config.toml` 能否覆盖 `[features] memories`：**未证实**。
- Codex cloud / environment 的跨任务记忆：**未找到**。
- `external_agent_memory_import`（从其他 agent 导入记忆）的行为：**未证实**（UnderDevelopment）。
- ChatGPT 记忆的具体注入/大小上限：**未找到**（官方只说按相关性取用、无单独上限）。

#### 4.3.2 Devin（Cognition，闭源，云端）
版本基线：docs.devin.ai 2026-10-06 抓取；Release Notes 最新条目 October 5, 2026。核对页面：`product-guides/knowledge`、`product-guides/memory`（Memory and Dreaming）、`product-guides/skills`、`product-guides/plugins`、`onboard-devin/knowledge-onboarding`、`onboard-devin/agents-md`、`onboard-devin/environment`、`onboard-devin/environment/blueprints`、`onboard-devin/environment/workspaces`、`product-guides/creating-playbooks`、`product-guides/using-playbooks`、`cli/extensibility/rules`、`release-notes/2026`、`release-notes/2025`。重要变化：**Knowledge 已标记 deprecated，正在迁移为 Skills（Plugins）**（Release Notes "Knowledge Is Moving to Skills"，September 18, 2026）；**Memory + Dreaming 是 2026-10-05 前后新上线的自动记忆**。

本节简称：AGENTS.md = [DV1]；CLI rules = [DV2]；skills = [DV3]；playbooks = [DV4] / [DV5]；blueprints = [DV6]；「Knowledge 页」= [DV7]；knowledge-onboarding = [DV8]；Release Notes 2026 = [DV9]；Memory = [DV10]；Agent Memory Repo 规范 = [DV11]；environment / workspaces = [DV12] / [DV13]。

##### ① 持久记忆
有，且分四层，规则文件与自动记忆是分开的产品：

**规则文件（人写）**
- `AGENTS.md`：仓库内，Devin 自动注入**每个文件开头最多 16 KiB（16,384 bytes）**，超出会提示截断、可按需读全文。【官方】[DV1]（无日期）
- Devin CLI 的 Rules：`AGENTS.md` / `AGENTS.local.md`（个人，建议 gitignore）/ `.devin/rules/*.md`（支持 `trigger` frontmatter：`always_on`、`manual`、`model_decision`、`agent`、`glob`）/ `.devin/global_rules.md`；也读 `.cursor/rules`、`.windsurfrules`、`CLAUDE.md`。全局规则：`~/.config/devin/AGENTS.md`（Windows `%APPDATA%\devin\AGENTS.md`），并读 `~/.claude/CLAUDE.md`。文档明确："Rules and AGENTS should be kept as small as possible"，推荐改用 Skills。【官方】[DV2]
- Skills：`SKILL.md`（Agent Skills 标准），仓库内 6 个路径都扫：`.agents/skills/<name>/SKILL.md`（推荐）、`.devin/skills/`、`.github/skills/`、`.claude/skills/`、`.cognition/skills/`、`.windsurf/skills/`；也可放在 Devin 托管的 Plugin 里（personal / organization / enterprise 三个作用域）。【官方】[DV3]
- Playbooks：组织/企业/System 级的可复用 prompt，用 `!macro` 挂到会话，不是记忆。【官方】[DV4] 、/using-playbooks
- Blueprint 里的 `knowledge` 段：按仓库写的 lint/test/build 命令参考，"Not executed. Loaded into Devin's context at session start"。【官方】[DV6]

**半自动：Knowledge（deprecated）**
- 形态：条目 = Trigger Description + Content（"a handful of sentences"）+ 可选 `!macro`；可建文件夹树、可按用户启停。
- 谁写：人在 Settings → Resources → Knowledge 建；**Devin 也会"automatically suggest Knowledge to remember based on your feedback in chat"**，用户可编辑后保存、驳回、或让它重新生成；Devin 还能建议更新已有条目。Onboarding 时 Devin 会"automatically generate repo knowledge based on the existing READMEs, file structure and contents of the connected repositories"，文档要求人工 review 完整性与准确性。【官方】[DV7] 、[DV8]
- 迁移：每个作用域（organization / enterprise / personal）生成一个 `knowledge` plugin，trigger → skill `description`，content → skill body 原样复制，pinned repository 和 folder 保留在 `metadata.devin` 里；"Nothing is committed to your repos"。【官方】同上 Knowledge 页 "Knowledge to Skills migration FAQ"；Release Notes September 18, 2026 [DV9]

**全自动：Memory（+ Dreaming）**
- 形态："a persistent Git repository of Markdown notes"（memory drive）：`MEMORY.md`（通用偏好 + 其他文件的索引）+ 主题文件（示例 `my-app/testing.md`、`tools/datadog.md`）。每条是一行 bullet，带 `[source: <session url>; added: YYYY-MM-DD]`。
- 记什么/不记什么（官方表格）：记偏好、纠正（下次要遵守的规则）、决策及理由、仓库与环境的 gotchas；**不记会话摘要、任务状态（PR 号）、容易重新发现的东西、密钥**。
- Dreaming：约每天一次的后台会话，合并重复、删过期条目、补遗漏、重组 `MEMORY.md` 索引；"Your first dream seeds memory from your recent sessions"。
- 基于开放规范 Agent Memory Repo（`MEMORY.md` 入口、单行条目、`[[path]]` 链接）。【官方】[DV10] ；规范 [DV11]
- 上线时间：文档示例写 `added: 2026-10-05`；【媒体】daily.dev / alphasignal / kucoin 报道为 2026-10-05 发布（Cognition X 帖 status/2107165034463867001，本次未能直接核对）。cognition.com/blog 列表里**未找到**对应博文。

##### ② 存在哪
- 全部在 Cognition 云端（Devin-managed）；Memory drive 是 Devin 侧的 Git 仓库，每个会话"gets its own checkout of the drive"；在 app 的 Customize → Memory 可浏览（只读）。
- Knowledge/迁移后的 skills：Devin Cloud，按作用域（org/enterprise/personal）存，API `/v3beta1/organizations/{org_id}/managed-plugins/bundles/knowledge/skills`。
- 仓库 Skills / AGENTS.md：在仓库里；Devin 后端会索引所有已连接仓库的 `SKILL.md`（"Indexed repos"），克隆后再按磁盘扫描覆盖。
- 环境：每个 organization 每个平台（Linux/Windows/macOS）**一个 active snapshot**，"Session changes don't persist back to the snapshot"。Blueprint 可按仓库写 `initialize/maintenance/knowledge`，monorepo 可按子目录建 workspace blueprint。【官方】[DV12] 、/environment/workspaces

##### ③ 作用域键
分层混合：
| 层 | 键 |
| - | - |
| Memory | **用户 × 组织**："Memory is **personal to you** within each organization. It is not shared with your teammates or your organization." 文件内部按主题/项目分文件夹（`my-app/…`）是 agent 自己的约定，不是硬键。自动化（automations）启动的会话不读不写 memory。 |
| Knowledge（→ knowledge plugin） | organization（默认，2025-10-10 起默认组织共享）/ enterprise / personal；外加 **pin to repo**：no repo / a specific repo / all repos（仓库身份 = 已连接的 SCM 仓库）。可按用户 enable/disable。 |
| Skills | 仓库（在该仓库 `SKILL.md`）/ personal / organization / enterprise（plugin 安装作用域）。 |
| Blueprint `knowledge` | 按仓库；monorepo 按子目录 workspace："If you have 5 repositories configured, Devin only sees the knowledge entries for the one it's working on." |
| Snapshot | 组织 × 平台。 |
| CLI rules | 项目目录（workspace root 到 cwd 之间每一级）+ 用户全局 `~/.config/devin/`。 |
| AGENTS.md（云端） | 仓库内路径。 |

##### ④ 谁写
- Memory：**Devin 自动写**（"When you correct Devin, explain a preference, or it works out something reusable, Devin edits the relevant note"），提交并合并并行会话的改动（stale write 重试、冲突合并）；会话里出现 **Updated memory** 卡片显示改了什么；"New memories apply to future sessions, not the one that wrote them"。用户在 app 里**只读**，改/删要在会话里让 Devin 做（"forget that I prefer npm"）；用户可关闭个人 memory；组织/企业管理员可开关（Default on / Always on / Always off）。无审批流。
- Knowledge：人写 + Devin 建议（需人确认保存）+ onboarding 自动生成（需人 review）；Release Notes 2025-11-07 "Agentic Knowledge Management… allowing Devin to contribute knowledge base entries within the folder hierarchy during sessions"。
- Skills：人写；Devin 会在测试完应用或学到新东西后建议创建/更新 skill，给 **"Create PR"** 按钮提交到仓库。
- AGENTS.md：人写（文档未提 Devin 自动写）。

##### ⑤ 进上下文
- Memory：`MEMORY.md` **每个会话常驻注入**；其他 note "Devin searches and reads the other notes only when the task needs them"（按需检索）。大小上限：**未找到**（Dreaming 负责瘦身）。
- Knowledge："Devin retrieves Knowledge when relevant, not all at once or all at the beginning"，按 trigger description 匹配；pin 到某仓库则在该仓库里"always used"；"Devin will read the entire Knowledge contents"。企业级 Knowledge 条数上限 300（Release Notes June 17, 2026）。
- Skills：会话开始只看到 name + description 列表；被触发（自动或 `@skills:name`）时整篇 `SKILL.md` 注入为 system-level instruction；**同一时间只能有一个 skill 激活**。
- AGENTS.md：开头 16 KiB 自动注入。Blueprint knowledge：会话开始注入、只注入当前仓库的。
- 压缩：Memory 靠 Dreaming 合并/清理；会话内上下文管理方式**未找到**公开说明。

##### ⑥ 换项目
- Memory 按 (用户, 组织) 存，**跟着用户跨仓库走**；"Gotchas about your repos" 以仓库为文件夹分文件，但文档**没有**说明换仓库时如何过滤（是否只加载相关文件夹）——**未证实**。
- Knowledge/skills：靠 pin-to-repo 或 skill 的仓库归属过滤；未 pin 的 Knowledge 在任何仓库都可能被 trigger 命中（设计如此，不算串）。"Repo skills are scoped to a repo — Devin picks up the right skills based on which repos are relevant to the task."
- Blueprint knowledge 严格按仓库/子目录隔离。
- worktree / 同仓库多个 clone：**未找到**。monorepo：有专页（native workspaces，按子目录 blueprint + knowledge）。

##### 一句话结论
作用域键 = 用户×组织（Memory）+ 组织/企业/个人 + 可选 pin 到仓库（Knowledge→Skills plugin）+ 仓库路径（SKILL.md/AGENTS.md）+ 组织×平台（snapshot）；记忆 = Devin 自动写的个人 Git-markdown drive（MEMORY.md 常驻、其余按需），规则文件 = AGENTS.md（16 KiB）/ SKILL.md / Playbook / blueprint knowledge（人写，按仓库或作用域）。

##### 未证实 / 未找到
- Memory 发布的官方博文/日期（仅媒体 + 文档示例日期 2026-10-05）。
- Memory 文件大小上限、每会话注入量、换仓库时对主题文件的过滤规则。
- Memory 是否在 Devin CLI / Desktop 生效（文档只写 app 的 Customize → Memory；llms.txt 中 CLI 板块无 memory 页）。
- Knowledge 云端存储实现、会话内上下文压缩机制。

### 4.4 agent 实例作为一级键

只有 Kiro（含 Amazon Q Developer CLI 源码）一家把 agent 实例做成一级作用域键；它按 agent 隔离的是显式添加的 `/knowledge` 知识库，不是自动记忆。Claude Code 子代理的 `memory:` 是另一个按 agent 名分目录的例子，写在 §4.1.1。

#### 4.4.1 Kiro（AWS）与 Amazon Q Developer
版本基线：
- Kiro 文档：[KR1]（updated 2026-10-06）、/docs/custom-agents/ 与 /docs/custom-agents/configuration-reference/（2026-10-02）、/docs/configuration/（2026-10-02）、/docs/cli/chat/context/（2026-08-04）、/docs/cli/experimental/knowledge-management/（2026-08-04）、/docs/reference/slash-commands/（2026-10-05）、/docs/cli/v3/（2026-10-01）、/docs/upgrade-guides/migrating-from-q/、/docs/web/memory/、/docs/crew/features/memory/（2026-08-04）。
- 源码：github.com/aws/amazon-q-developer-cli main @ `15cc8f3`（2026-04-23）。README 顶部：「This open source project is no longer being actively maintained... Amazon Q Developer CLI is now available as Kiro CLI, a closed-source product」【官方】。所以**源码只能证实 Q CLI 时代的路径，Kiro CLI 的新路径只能靠文档**。没有 `aws/kiro-cli` 仓库（404）；issue 跟踪在 kirodotdev/Kiro。
- 改名：Kiro CLI 2025-11-17 可用，2025-11-24 自动更新；`q`/`q chat` 继续可用；默认 agent 改名 `kiro_default`【官方】[KR8] ；源码 `crates/chat-cli/src/constants.rs:223-229` 的升级公告文案与之一致。
- Amazon Q Developer IDE 插件 2027-04-30 终止支持【官方】[KR11] 。

本节简称：steering = [KR1]；custom-agents = [KR2]；configuration-reference = [KR3]；configuration = [KR4]；cli/chat/context = [KR5]；knowledge-management = [KR6]；slash-commands = [KR7]；migrating-from-q = [KR8]；web/memory = [KR9]；crew/features/memory = [KR10]；Q Developer 终止支持 / 项目 rules = [KR11] / [KR12]。Q CLI 源码以 `15cc8f3` 为准。

##### ① 持久记忆
- **规则文件（人写）**：
  - Kiro IDE/CLI steering：`.kiro/steering/*.md`，frontmatter `inclusion: always | fileMatch (+fileMatchPattern) | manual (#steering-file-name) | auto (+name/description)`；基础三件 `product.md / tech.md / structure.md` 由 Kiro 按用户点击生成（「Kiro generates three core steering files」），「included in every interaction by default」；AGENTS.md 也支持（无 inclusion 模式、恒常包含）【官方】[KR1]。
  - 用户全局 steering：「Global steering files reside in your home directory under `~/.kiro/steering/`, and apply to all workspaces」，冲突时「Kiro will prioritize the workspace steering instructions」【官方】同页。
  - Q Developer IDE 插件：`{{project-root}}/.amazonq/rules/*.md`，「Amazon Q will automatically use them as context whenever a developer chats... within your project」，聊天框 Rules 按钮可按会话勾选开关【官方】[KR12] 。
  - Q CLI / Kiro CLI 的 **agent 级上下文**：agent JSON 的 `resources: ["file://..."]`，这是「按 agent 实例配置上下文」的例子（见本节③）。
- **记忆（模型自动）**：
  - Kiro IDE、Kiro CLI：**未找到任何自动记忆功能**（steering、custom-agents、context、knowledge 四页均无）。
  - Kiro CLI `/knowledge`：**用户/模型显式添加的持久知识库**（语义或 BM25 索引），「persistent knowledge base functionality... persists across chat sessions」，不是自动记忆【官方】[KR6]。
  - Kiro Web（云端 agent）：有自动 Memory——「As you work with Kiro Web and give it feedback, it picks up your preferences and applies them to future work」，主要来自 PR 评论，「applies those patterns to future work across all your repositories」【官方】[KR9] 、steering 页。
  - Kiro Crew（独立的常驻个人 agent 产品）：六层记忆（preferences / projects / history / semantic(SQLite+向量) / episodic / lessons）【官方】[KR10] 。与 IDE/CLI 不是一个东西，下面只简述。

##### ② 存在哪
- Q CLI 源码（`crates/chat-cli/src/util/paths.rs`）：
  - 工作区：`.amazonq/cli-agents`（L46）、`.amazonq/prompts`（L47）、`.amazonq/mcp.json`（L48）、`.amazonq/rules/**/*.md`（L51）、默认资源 `file://AmazonQ.md`、`file://AGENTS.md`、`file://README.md`（L54）。
  - 全局：`~/.aws/amazonq/cli-agents`（L59）、`~/.aws/amazonq/global_context.json`（L64，已废弃）、`~/.aws/amazonq/profiles`（L65，已迁移到 agents）、`~/.aws/amazonq/knowledge_bases`（L66）。
  - 另一份定义 `crates/agent/src/agent/util/directories.rs:61-71`：`local_agents_path()` = `current_dir()/.amazonq/cli-agents`，`global_agents_path()` = `~/.aws/amazonq/cli-agents`。
  - 默认 agent 的资源（`crates/agent/src/agent/agent_config/definitions.rs:177-182`）：`file://AmazonQ.md`、`file://AGENTS.md`、`file://README.md`、`file://.amazonq/rules/**/*.md`。
  - 知识库目录（`crates/chat-cli/src/util/knowledge_store.rs:42-49`）：`~/.aws/amazonq/knowledge_bases/<agent_unique_id>/`，每个库下 `contexts.json` + `<context-id>/data.json`（+ `bm25_data.json`）【官方】`docs/knowledge-management.md`。
- Kiro CLI（文档）：
  - agents：`~/.kiro/agents/[name].json|.md`（全局）、`.kiro/agents/[name].json|.md`（工作区），支持子目录命名 `team/planner`【官方】[KR2]。
  - steering：`~/.kiro/steering/`、`.kiro/steering/`；skills `~/.kiro/skills/`、`.kiro/skills/`；MCP `~/.kiro/settings/mcp.json`、`.kiro/settings/mcp.json`；prompts `~/.kiro/prompts`【官方】[KR4] 、migrating-from-q。
  - 迁移映射：`~/.aws/amazonq` → `~/.kiro`；`~/.aws/amazonq/cli-agents` → `~/.kiro/agents`；`~/.aws/amazonq/rules` → `~/.kiro/steering`；项目级 `.amazonq` → `.kiro`（原目录保留不动）【官方】migrating-from-q。注意源码里并没有全局 `~/.aws/amazonq/rules` 的读取逻辑（只有工作区 `.amazonq/rules/**/*.md`），这条映射只见于 Kiro 文档。
  - 知识库：macOS `~/Library/Application Support/kiro-cli/knowledge_bases/`、Linux `~/.local/share/kiro-cli/knowledge_bases/`、Windows `%LOCALAPPDATA%\kiro-cli\knowledge_bases\`【官方】knowledge-management 页（与 Q CLI 源码的 `~/.aws/amazonq/knowledge_bases` 不同，说明 Kiro CLI 改了路径）。
- Kiro Web Memory：云端，在「Memory section of your Kiro Web Settings」查看/删除【官方】。
- Kiro Crew：`~/.kiro/crew/workspace/memory/{preferences.md, projects.md, history/}` + SQLite（`semantic_memory`、`episodic_memories` 表，可选 FAISS）【官方】crew/features/memory。

##### ③ 作用域键
Kiro/Q CLI 是**四层混合**，且有一个少见的「agent 实例」维度：
- **用户全局**：`~/.kiro/`（steering、agents、skills、mcp、prompts）。
- **工作区**：键是**当前工作目录**（`env::current_dir()`，`directories.rs:62-65`、`paths.rs:232-233`），不是 git root、不是 remote；「only available when running Q CLI from that directory or its subdirectories」【官方】`docs/agent-file-locations.md`。同名时「local agent takes precedence」并警告「Agent conflict for my-agent. Using workspace version」。
- **agent 实例**：每个 agent JSON 自带 `resources`（file:// 文件/glob、skill://、`{"type":"knowledgeBase","source":"file://./docs"}`）、`prompt`、`hooks`、`mcpServers`；「By default, custom agents inherit default resources (steering files, skills, and AGENTS.md) alongside their own configured resources」【官方】custom-agents/configuration-reference。
  - `/knowledge` 知识库**按 agent 隔离**：「Each agent maintains its own isolated knowledge base... No Cross-Agent Access」；目录键 = `agent.name` 或 `{agent.name}_{hash(agent 配置文件路径):x}`（`knowledge_store.rs:23-39`，`generate_agent_unique_id`），默认 agent 用 `q_cli_default`（Kiro 里 `kiro_default`）。即**同名 agent 放在不同路径就是两份知识库，同一个全局 agent 跨项目共用一份知识库**。
- **会话**：`/context add|remove|show|clear` 只影响当前会话，「Context changes are NOT preserved between chat sessions」「To make context changes permanent, add the files to your agent's resources field instead」【官方】slash-commands、cli/chat/context。源码里会话路径与 agent 路径分别标记为 `ContextFilePath::Session` / `ContextFilePath::Agent`（`context.rs:85-116`）。
- Q Developer IDE 插件：只有工作区键 `{{project-root}}/.amazonq/rules`，无全局层【官方】。
- Kiro Web Memory：按**用户**，跨「all your repositories」；「Other reviewers' comments don't affect it」【官方】。
- Kiro Crew：按用户、跨 workspace 内所有频道。

##### ④ 谁写
- steering / rules / agent 配置：人写；Kiro 可按用户点击生成 product/tech/structure 三件；`/agent generate` 可让模型生成 agent 配置【官方】。
- `/knowledge`：用户用斜杠命令添加；模型也有 `knowledge` 工具（`tool_index.json:250`：「Store and retrieve information in knowledge base across chat sessions」），命令含 add/remove/clear/search/update（`tools/knowledge.rs:34-80`），受 `allowedTools` 权限控制；无自动写入。
- Kiro Web Memory：agent 自动学习，「nothing to turn on and nothing to add manually」；用户只能查看和删除（「you can delete any memory you don't want the agent to keep」），不能编辑【官方】。
- Crew：consolidator 每 30 条消息自动重写 preferences/projects，LLM 写语义记忆要求置信度 ≥ 0.8，用户显式「remember X」直接入 lessons；CLI `kirocrew learn add/list/remove` 与 dashboard 可增删【官方】。

##### ⑤ 进上下文
- agent `resources`（file://）**常驻**：「loaded directly into context at startup」「consume tokens from your context window on every request, whether referenced or not」【官方】cli/chat/context。源码：每轮把 steering/resources 文件拼成一条伪 user 消息（`--- CONTEXT ENTRY BEGIN ---` … `[filename]\n content` … `--- CONTEXT ENTRY END ---`）加上一条固定的伪 assistant 回复，再接 agent `prompt`（`conversation.rs:804-860`）。
- **上限与裁剪**：「Context files are limited to 75% of your model's context window. Files exceeding this limit are automatically dropped」【官方】；源码 `context.rs:264-267` `calc_max_context_files_size` = 上下文窗口 × 3/4；`util/mod.rs:168-181` `drop_matched_context_files` 先按 token 数降序排，累加超限的文件整份丢弃（大文件优先被丢）。
- skill:// 只载元数据、按需读全文；steering `fileMatch`/`auto`/`manual` 条件注入（Kiro CLI V3 支持全部四种，V1/V2 只自动加载 `always`）【官方】steering 页。
- `/knowledge`：**工具调用检索**（`knowledge` 工具 `search`），Fast=BM25、Best=all-MiniLM-L6-v2 语义；设置 `knowledge.maxFiles`、`chunkSize`、`chunkOverlap`、`indexType`、默认 include/exclude；V1/V2 需 `chat.enableKnowledge true`，「V3 enables knowledge by default」【官方】。
- 会话压缩：`/compact` 或溢出时自动，摘要作为 CONTEXT ENTRY 注入（`conversation.rs:811-818`）；Kiro CLI 自动保存每轮会话，可 `/chat resume`【官方】slash-commands。
- Kiro Web Memory 注入方式未说明；Crew 在会话开始注入 preferences(4,250 字符)/projects(6,400)/history(26,600)/lessons(37,250)/semantic(12,000)，每条消息再检索 episodic top-8（3,000 字符）【官方】。

##### ⑥ 换项目
- 全局 agent、`~/.kiro/steering`、全局 skills 跟用户走；工作区 agent/steering 只在该 cwd 及其子目录可见。
- **跨项目串味的点**：`/knowledge` 按 agent 不按项目——用全局 agent 在项目 A 里 `/knowledge add` 的内容，在项目 B 里用同一 agent 时仍可搜到；文档把它当作设计（「knowledge contexts are scoped to the specific agent you're working with」），没有按项目隔离的选项。工作区 agent 因路径哈希不同而天然隔离。
- Kiro Web Memory 明确跨仓库生效（「across all your repositories」）。
- worktree / monorepo / 多 clone：官方未专门讨论（**未找到**）；工作区键是 cwd，子目录可见父目录的 `.kiro`，不同 clone 互不可见。

##### 一句话结论
作用域键 = 用户全局 `~/.kiro`（旧 `~/.aws/amazonq`）/ 工作区 = 当前工作目录 `.kiro`（旧 `.amazonq`）/ **agent 实例**（配置自带 `resources`，知识库按 `{agent 名}_{配置路径哈希}` 隔离）/ 会话（`/context` 不持久）；记忆 vs 规则 = IDE/CLI **没有自动记忆**，只有 steering/rules（人写、常驻、75% 上下文封顶）和 `/knowledge`（显式添加、按 agent、工具检索）；自动记忆只存在于云端 Kiro Web（按用户、跨仓库、只可删）和独立产品 Kiro Crew。Q Developer IDE 插件只有 `.amazonq/rules`，2027-04-30 终止支持。

##### 未证实 / 未找到
- Kiro CLI（闭源）当前 `~/.kiro/agents` 的知识库目录命名是否仍是 `{name}_{hash}`：源码只能证实到 2026-04 的 Q CLI；Kiro 文档只给 `my-custom-agent_<alphanumeric-code>`。
- Kiro CLI V3 的 GA 版本号/日期：文档只说「early release」（2026-10-01）；「v2.8.0 / 6 月 17 日」见于第三方教程，未证实。
- Kiro IDE 是否有任何自动记忆：未找到。
- Kiro Web Memory 的存储位置（云端具体）、注入方式、上限：未找到。
- worktree / monorepo 的官方讨论：未找到。

### 4.5 自动记忆已下线：回到人写规则

两家主流 IDE 在 2025-11～2026-06 之间下线了自动记忆，把用户导向人写规则文件。

#### 4.5.1 Cursor
版本基线：[CU1]（页面无日期，2026-10-06 查看）；cursor.com/changelog 最新条目 2026-09-23（changelog 按日期不按版本号；docs/agent/projects 提到「Enterprise teams need Cursor 3.21.9 or later」）；Memories 旧文档用 Wayback 快照 docs.cursor.com/context/memories @2025-07-03、docs.cursor.com/en/context/memories @2025-08-14；论坛 staff 回复 2025-05-30、2025-11-25。

本节简称：「rules 页」= [CU1]（patterns 调研引用的是 [CU2]，内容同为现行 rules 文档）。旧 Memories 文档只有 Wayback 快照（docs.cursor.com/context/memories @2025-07-03、docs.cursor.com/en/context/memories @2025-08-14），未单独编号。

##### ① 持久记忆
- **规则文件（人写）**：四类，均【官方】[CU1]：
  - Project Rules：`.cursor/rules/*.mdc`，frontmatter `description / alwaysApply / globs`；同目录下的 `.md` 文件「Ignored (wrong extension)」。
  - `AGENTS.md`：项目根或任意子目录，嵌套合并，「more specific instructions taking precedence」。
  - User Rules：在设置 Customize → Rules，「Global to your Cursor environment. Used by Agent (Chat)」，不作用于 Inline Edit。
  - Team Rules：Team/Enterprise 从 dashboard 下发，「included in the model context for Agent (Chat) across all repositories」。
  - 另有家目录规则：2.1 changelog（2025-11-21）「Rules in home folder (`~/.cursor/rules`) will be included in context」【官方】[CU3] 。当前 rules 文档页未提这一项。
- **记忆（模型自动）——已下线**：
  - 2025-06-04 1.0 引入 Memories（beta）：「With Memories, Cursor can remember facts from conversations and reference them in the future. Memories are stored per project on an individual level」【官方】[CU4] 。
  - 旧文档定义：「Memories are automatically generated rules based on your conversations in Chat. These memories are scoped to your project and maintain context across sessions」；生成方式两种——Sidecar observation（「another model observes your conversations and automatically extracts relevant memories... Background-generated memories require user approval before being saved」）和 Tool calls（agent 被要求记住时直接写）【官方】Wayback docs.cursor.com/en/context/memories @2025-08-14。
  - **2.1（2025-11）起移除**：staff deanrie 2025-11-25「The Memories feature was intentionally removed starting from version 2.1.x」，建议 `Cmd+Shift+P` → "Export memories" 导出为 `.mdc` 再放进 User/Project Rules【官方】[CU5] 。2.1 changelog 本身只写了「Custom modes have been removed」，未提 Memories【官方】[CU3] 。
  - **2026-10 时点**：cursor.com/docs 已无 memories 页（/docs/context/memories 回落到 rules 页）；changelog 2025-11 之后没有任何 memory 条目；没有改名或并回 rules 的官方说法——结论是「删掉了，让用户手工搬到 Rules」。
  - 新的自动累积机制只在 **Projects**（2026-09-10）里：「Each Project maintains a set of files that sync across every cloud and local machine its agents use. Agents add research and artifacts, along with what they learn about the codebase and how you prefer work to be done」【官方】[CU6]（2026-09-10 条目）、[CU7] 。这是 coordinator+subagents 的共享文件，不是通用 chat memory。

##### ② 存在哪
- Project Rules / AGENTS.md：仓库内。User Rules：Cursor 设置（本地还是账号同步，文档未说，**未证实**）。Team Rules：dashboard（云端）。`~/.cursor/rules`：家目录。
- 旧 Memories：文档只说「per project on an individual level」、在 Settings → Rules 管理；本地/云端未明说（**未证实**）。间接证据偏向服务端：Privacy Mode 下不可用——staff condor「Memories are currently unavailable for users with Privacy Mode enabled」【官方】[CU8] ；staff danperks 2025-05-30「knowledge about your codebase can end up in these memories, and we want to ensure that there is no possible vector for information about sensitive codebases to be stored in the training data of any LLM provider」【官方】[CU9] 。
- Projects 共享文件：云端为主（「A Project runs on its own computer in the cloud」），同步到本地机器【官方】docs/agent/projects。

##### ③ 作用域键
- 规则：三层 + 家目录。优先级「Team Rules → Project Rules → User Rules」【官方】rules 页。Project 键 = 工作区目录（`.cursor/rules` 所在目录；AGENTS.md 按目录嵌套）；Team 键 = 组织；User 键 = 用户环境。
- 旧 Memories：**项目 × 用户**（「scoped to your project」「per project on an individual level」）。项目键的具体形式（路径 / git root / remote URL）**未证实**。
- Projects：按 Project 实体（云端对象），跨机器同步。

##### ④ 谁写
- 规则：用户手写（Team Rules 由管理员）。
- 旧 Memories：sidecar 模型自动提取，**保存前需用户审批**（2025-08 文档）；agent 也可在用户要求时用 tool call 直接写；Settings → Rules 可查看/编辑/删除（社区回复 condor）。2.1 后只剩导出命令。
- Projects 文件：agent 自动写入（「Agents add research and artifacts...」）；用户是否可直接编辑，文档未说（**未证实**）。

##### ⑤ 进上下文
- 规则：「When applied, rule contents are included at the start of the model context」；四种应用方式 Always / Apply Intelligently（按 description 由模型决定）/ 文件 glob / Manual（@ 提及）。无硬上限，建议「Keep rules under 500 lines」【官方】rules 页。
- 旧 Memories：注入方式（常驻还是检索）官方文档未说明（**未证实**）。
- Projects 共享文件：同步到每台 agent 机器，注入细节未说明。

##### ⑥ 换项目
- User Rules、Team Rules、`~/.cursor/rules` 跟用户/组织走；Project Rules、AGENTS.md、旧 Memories 不跨项目（Memories 明确 per project）。
- 官方文档没有讨论 worktree / monorepo / 多 clone 的键问题（**未找到**）；monorepo 的唯一机制是子目录 AGENTS.md。

##### 一句话结论
作用域键 = 规则三层（组织 / 工作区目录 / 用户）+ 家目录 `~/.cursor/rules`；记忆 = 2025-06 ~ 2025-11 存在过的「项目 × 用户」自动记忆（需审批、Privacy Mode 下禁用），2.1 起删除并要求用户导出进 Rules；2026-09 起只有 Projects 里的 agent 共享文件是自动累积的。

##### 补充：模式调研里的相关证据

- **嵌套 AGENTS.md**：旧文档曾写 nested「planned」，现行文档明确已支持；子目录规则与祖先组合，具体者优先；`.cursor/rules/*.mdc` 可用 globs [CU2]。
- **多根 workspace 串味报告**（2026-03-21）：一个仓库的根 AGENTS.md 被用到另一个仓库，论坛回复确认根规则被全局加载、嵌套规则行为不同；帖子后来自动关闭，未见修复验证 [FR4]（证据等级见 §7）。说明「目录上分开存」仍可能因 loader 全局拼接而串。

##### 未证实 / 未找到
- 旧 Memories 的物理存储位置（本地/云端）与项目键形式：未证实。
- User Rules 是否账号同步：未证实。
- Projects 共享文件的文件名、格式、是否可人工编辑：未找到。
- worktree / monorepo / 多 clone 的官方讨论：未找到。

#### 4.5.2 Windsurf（Codeium → Cognition，2026 起更名 Devin Desktop）
版本基线：docs.windsurf.com/windsurf/cascade/memories 现在 307 跳转到 [WS1]（2026-10-06 查看，Mintlify 页，无日期）；对照 Wayback 快照 @2025-05-21、@2025-09-04、@2025-12-15、@2026-02-26。更名时间：2026-06-02 以 OTA 更新把 Windsurf 变成 Devin Desktop，Cascade 计划 2026-07-01 前退役、由 Rust 重写的 Devin Local 取代【媒体】[WS4] 、[WS5] 。官方文档只写「legacy Cascade agent」「Devin Local agent — the default agent for new tabs」，没给日期。

本节简称：「docs.devin.ai memories 页」= [WS1]；devin-local = [WS3]；AGENTS.md 页 = [WS2]（patterns 调研引用）。旧 Windsurf 文档只有 Wayback 快照（@2025-05-21、@2025-09-04、@2025-12-15、@2026-02-26），未单独编号。

##### ① 持久记忆
- **记忆（模型自动）**：有，但**只对 legacy Cascade**。「During conversation, Cascade can automatically generate and store memories if it encounters context that it believes is useful to remember」，也可「create a memory of …」手动触发。**新默认 agent 不再有记忆**：「Memories apply to the legacy Cascade agent only. The Devin Local agent — the default agent for new tabs — does not persist memories. Migrate the ones you rely on to skills with the Devin: Open Cascade Migration Wizard command」【官方】[WS1] ；devin-local 页：「The Devin Local agent does not persist memories between sessions」【官方】[WS3] 。官方建议：「for durable knowledge, prefer Rules or AGENTS.md」。
- **规则文件（人写）**：global_rules.md（用户全局）、`.devin/rules/*.md`（首选）/ `.windsurf/rules/*.md`（兼容）、根目录 `.windsurfrules`（仍读）、AGENTS.md（根=always on，子目录=auto glob）、企业 System rules（OS 目录，只读）【官方】同页。

##### ② 存在哪
- 自动记忆：「stored locally in `~/.codeium/windsurf/memories/`」，「not committed to your repository」，「Auto-generated memories live only on your machine」【官方】。
- 全局规则：`~/.codeium/windsurf/memories/global_rules.md`（单文件）【官方】。
- 工作区规则：仓库内 `.devin/rules` 或 `.windsurf/rules`，发现范围「current workspace and sub-directories」+「parent directories up to the git root」；新建规则存到当前工作区的 `.devin/rules`，「not necessarily at the git root」【官方】。
- 系统规则：macOS `/Library/Application Support/Devin/rules/*.md`（legacy `Windsurf/`）、Linux `/etc/devin/rules/*.md`、Windows `C:\ProgramData\Devin\rules\*.md`【官方】。

##### ③ 作用域键
- 自动记忆键 = **workspace**：「associated with the workspace they were created in... Memories generated in one workspace are not available in another」【官方】。workspace 的具体键（路径哈希？）官方未写，**未证实**；【非官方】分析认为按仓库在磁盘上的位置键存，换 clone 路径就是另一份记忆（[WS6] ，2026-10-04，作者承认未直接查看目录）。
- 规则三层：global（用户）/ workspace（目录，向上到 git root，多根工作区去重）/ system（机器）。

##### ④ 谁写
- 记忆：Cascade 自动写或按要求写，无审批步骤的说明；可在 Customizations 面板「click into it and then click the Edit button」编辑【官方】。
- 规则：用户写（也可让 Cascade 写进 `.devin/rules/` 或 AGENTS.md）；系统规则由 IT 下发，「cannot be deleted by end users」【官方】。

##### ⑤ 进上下文
- 记忆：**按需检索**——「Cascade retrieves them when it believes they're relevant」；「do NOT consume credits」【官方】。
- 规则按 `trigger:`：`always_on`「included in the system prompt on every message」；`model_decision`「Only the description is shown in the system prompt. Cascade reads the full rule file when it decides the description is relevant」；`glob` 读/改匹配文件时；`manual` 只在 @rule-name 时。global_rules.md 与根 AGENTS.md 无 frontmatter、恒常 on【官方】。
- **上限变迁**（常被引用的 6000/12000 是否仍有效）：
  - @2025-05-21 快照：「Rules files are limited to 6000 characters each. Any content above 6000 characters will be truncated」+「If the total of your global rules and local rules exceed 12,000 characters, priority will be given to the global rules, followed by the workspace rules. Any rules beyond 12,000 characters will be truncated」【官方】Wayback。
  - @2025-09-04 起到 @2026-02-26：只剩「Rules files are limited to 12000 characters each」，总量 12000 的说法消失【官方】Wayback。
  - 现行（docs.devin.ai）：「The global rules file is limited to 6,000 characters」「Workspace rule files are limited to 12,000 characters each」；**没有总量上限**【官方】。即「每文件 6000 + 总 12000」已失效，改为 global 6000 / workspace 每文件 12000。

##### ⑥ 换项目
- 自动记忆不跨 workspace；global_rules.md 跟用户走；system rules 跟机器走。
- 同一仓库第二个 clone / 另一台机器：官方只说按 workspace 隔离、不进仓库；【非官方】推断是另一份空记忆。worktree/monorepo 无专门讨论（**未找到**）；相关机制只有「向上搜到 git root」+ 子目录 `.devin/rules` + 多根工作区去重。

##### 一句话结论
作用域键 = 记忆按 workspace（本地、不进仓库、键形式未证实）；规则 = 用户全局单文件（6000 字符）/ 工作区目录（每文件 12000 字符，发现到 git root）/ 机器级 system；2026-06 产品更名 Devin Desktop 后默认 agent（Devin Local）**不再有自动记忆**，官方把持久知识导向 Rules / AGENTS.md / Skills。

##### 补充：模式调研里的相关证据

- 当前文档：根 AGENTS.md 常驻，子目录 AGENTS.md 按自动 glob 激活 [WS2]；Memories 只适用于 legacy Cascade，新默认 Devin Local 不持久化该功能 [WS1]——两份调研结论一致。

##### 未证实 / 未找到
- `~/.codeium/windsurf/memories/` 下 workspace 子目录的命名/键形式：未证实（闭源，官方未写）。
- 记忆文件格式、条数上限、检索算法：未找到。
- 更名与 Cascade 退役的官方日期：官方文档未写，仅媒体。

### 4.6 没有自动记忆：只有规则文件与会话

这七家没有模型自动积累的记忆。它们值得看，是因为各自对「项目」「用户」「agent 身份」的键定义可以直接对照（见 §3.1、§9）。

#### 4.6.1 Cline

版本基线：cline/cline main @ `cd80a20e`（2026-10-06），`apps/vscode/package.json` version **4.1.22**（仓库已改为 monorepo，扩展源码在 `apps/vscode/`，CLI 在 `apps/cli/`）；路径解析逻辑在 npm 包 `@cline/shared` **0.0.90**（仓库里以 `workspace:*` 引用但源码不在本仓库，我下载了 npm tarball 读 `dist/storage/index.js`，已 minify）。核对的文件：`apps/vscode/src/core/storage/disk.ts`、`apps/vscode/src/shared/storage/storage-context.ts`、`apps/vscode/src/core/context/instructions/user-instructions/{cline-rules.ts,rule-helpers.ts}`、`apps/vscode/src/core/prompts/responses.ts`、`apps/vscode/src/sdk/{SdkController.ts,sdk-task-history.ts,legacy-state-reader.ts}`、`apps/vscode/src/shared/HistoryItem.ts`、`apps/vscode/src/core/storage/state-migrations.ts`、`CHANGELOG.md`、`apps/cli/CHANGELOG.md`。文档（页面无日期）：[CL1]、/prompting/cline-memory-bank、/core-workflows/task-management、/features/multiroot-workspace。

本节简称：「docs cline-rules」= [CL1]；cline-memory-bank = [CL2]；task-management = [CL3]；multiroot-workspace = [CL4]。

##### ① 持久记忆

- **没有内置的「模型自动积累」记忆。**【官方】源码 `apps/vscode/src/services/` 子目录为 `account auth banner browser error feature-flags glob logging mcp search telemetry temp uri`，`apps/vscode/src/core/` 为 `api context controller hooks ignore locks mentions prompts storage task webview workspace`，没有 memory 模块；全仓库 grep `memory` 只命中 in-memory cache、`standalone/memory-monitor.ts`（进程内存监控）、CLI connectors 的 `InMemoryStateAdapter`（`apps/cli/src/connectors/stores/memory-state.ts`，Map 实现的临时状态，不落盘）。
- **「Memory Bank」只是 docs 里的 prompting 模式，不是内置功能。**【官方】[CL2]：把一段自定义指令放进 Cline Rules，让模型在仓库里维护 `memory-bank/` 目录的六个 markdown（`projectbrief.md`、`productContext.md`、`activeContext.md`、`systemPatterns.md`、`techContext.md`、`progress.md`），靠用户说 "follow your custom instructions" / "update memory bank" 来读写。源码中无 `memory-bank`/`memorybank` 字样（grep 为空）。
- **规则文件（人写）**：工作区 `.clinerules`（单文件或目录）、`.cline/rules/`、`AGENTS.md`（递归加载子目录的 AGENTS.md）、`.cursorrules`、`.cursor/rules/`、`.windsurfrules`；全局 `~/Documents/Cline/Rules`（Rules 面板创建处）、`~/.cline/rules`、`~/Cline/Rules`、OneDrive 重定向的 `Documents/Cline/Rules`、`~/.agents/AGENTS.md`。workflows 同理（`.clinerules/workflows`、`.cline/workflows`、`~/Documents/Cline/Workflows`、`~/.cline/workflows`）。旧版 settings 里的 "custom instructions" 文本已被迁移成全局规则文件 `custom_instructions.md`（`state-migrations.ts:74-110`）。
- **远程规则（组织层）**：企业 remote config 带 `GlobalInstructionsFile[]`，按 `remote_config_<orgId>.json` 缓存（`disk.ts:35`，`rule-helpers.ts:262-298` `getRemoteRulesTotalContentWithMetadata`）。
- **任务历史 / 会话记录**：每个 task 的 `api_conversation_history.json`、`ui_messages.json`、`context_history.json`（截断记录）、`task_metadata.json`、`settings.json`（`disk.ts:17-36` `GlobalFileNames`）。这是会话转录，不会自动喂给新任务。
- **压缩**：`/compact`（别名 `/smol`、`/newtask`）与自动 compaction 只作用于当前任务（`core/controller/slash/condense.ts:1-16`；CHANGELOG 4.1.x 多条）。

##### ② 存在哪

- `apps/vscode/src/core/storage/disk.ts:17-36`：`GlobalFileNames`（上述文件名常量）；`:47-49` `getClineHomePath()` = `~/.cline`；`:51-53` `ensureTaskDirectoryExists(taskId)` = `<globalStorage>/tasks/<taskId>`；`:55-64` `ensureRulesDirectoryExists()` = `<Documents>/Cline/Rules`（失败回退 `~/Documents/Cline/Rules`）；`:66-75` Workflows；`:88-97` Hooks；`:190-194` `getGlobalStorageDir()` 以 `HostProvider.globalStorageFsPath` 为根。Documents 路径按 OS 解析（`documents-path.ts:6-35`：Windows 用 PowerShell `MyDocuments`，Linux 用 `xdg-user-dir DOCUMENTS`，否则 `~/Documents`）。
- `apps/vscode/src/shared/storage/storage-context.ts:108-112`（注释）文件布局：`~/.cline/data/globalState.json`、`~/.cline/data/secrets.json`（0600）、`~/.cline/data/workspaces/<hash>/workspaceState.json`；`:116-125` `<hash>` = `hashString(workspacePath)`（`:72-80`，JS 字符串 hash 取 8 位 hex，输入是**工作区绝对路径**）；`:84` 优先级 `CLINE_DATA_DIR` > `CLINE_DIR/data` > `~/.cline/data`。
- `@cline/shared` 0.0.90 `dist/storage/index.js`（minified，函数名已混淆，按导出表对应）：
  - `resolveGlobalRulesConfigPaths()` = `[~/.cline/rules, ~/Cline/Rules, ~/Documents/Cline/Rules, $OneDrive*/Documents/Cline/Rules]`；
  - `resolveWorkspaceRulesConfigPaths(ws)` = `[ws/.clinerules, ws/.cline/rules]`；
  - `resolveRulesConfigSearchPaths(ws)` = `[ws/AGENTS.md, ws/.clinerules, ws/.cline/rules, ~/.agents/AGENTS.md, ...global]`；
  - `resolveSessionDataDir()` = `$CLINE_SESSION_DATA_DIR || ~/.cline/data/sessions`（SDK 会话目录，`sdk-task-history.ts:499` `sessionDir = resolveSessionDataDir()/<taskId>`）；
  - 无工作区时的「聊天工作区」= `~/.cline/data/workspaces/chat`（`SdkController.ts:1163-1168` 注释，seeded 一个 AGENTS.md）。
- 旧版（SDK 之前）任务历史：`<globalStorage>/state/taskHistory.json` + `tasks/<id>/`（`apps/vscode/src/sdk/legacy-state-reader.ts:42-44`；`hosts/vscode/vscode-to-file-migration.ts:26-31` 把 VS Code 存储迁到 `~/.cline/data/` 以便 CLI/JetBrains 共享）。
- 规则注入模板：`apps/vscode/src/core/prompts/responses.ts:317-337`（global `.clinerules/`、local `.clinerules/`、`.clinerules` 单文件、`.windsurfrules`、`.cursorrules`、`.cursor/rules`、`AGENTS.md` 七种标题）。
- 规则开关：`cline-rules.ts:44-63`：全局 toggles 存 globalState `globalClineRulesToggles`，工作区 toggles 存 workspaceState `localClineRulesToggles`。
- 【官方】[CL1]："Cline also searches `~/.cline/rules` and `~/Cline/Rules` for global rules"；"Workspace rules go in `.clinerules/` or `.cline/rules/` at your project root"；"Every rule has a toggle to enable or disable it"。

##### ③ 作用域键

- **规则**：两层。工作区层键 = **当前工作区根目录的绝对路径**（`cwd`，即 VS Code 打开的文件夹；多根工作区只取第一个文件夹，见本节⑥），不是 git root，也不是远程 URL；全局层键 = 用户 home。AGENTS.md 另按目录递归（嵌套 AGENTS.md 合并，提示模型只应用与当前操作目录相关的部分，`responses.ts:336`）。规则文件可带 YAML frontmatter 做条件启用（`rule-helpers.ts:205-249` `evaluateRuleConditionals`，如 `paths:`），这是**文件级**子作用域。
- **组织层**：remote config 按 `orgId`。
- **任务历史**：单一全局列表（`~/.cline/data/sessions/` 或旧 `taskHistory.json`），每条带 `cwdOnTaskInitialization`（`HistoryItem.ts:13`，任务开始时的 cwd 绝对路径）。History 面板 "current workspace only" 用 `arePathsEqual(item.cwd ?? item.workspaceRoot, workspacePath)` 过滤（`SdkController.ts:2082-2110`）。
- **工作区状态**（规则开关等）：`~/.cline/data/workspaces/<hash(绝对路径)>/workspaceState.json`。
- **agent 身份层：没有。** Cline 没有 profile/persona/mode 之类的身份概念，规则不按身份挂。
- **会话**：taskId（ULID），每任务一个目录。

##### ④ 谁写

- 规则：用户手写（Rules 面板可新建文件、开关；`rule-helpers.ts:300-390` 还会把旧的 `.clinerules` 单文件自动转成目录）。模型可以用 `write_to_file` 写 `.clinerules` 或 `memory-bank/*.md`，但那是普通文件写入，走正常审批/auto-approve，没有「记忆写入」专用通道或通知。
- Memory Bank：模型在用户说 "update memory bank" 时更新（docs），用户可见可编辑（就是仓库里的 md）。
- 任务历史：自动写；UI 可删除、收藏（收藏项受保护不被批量删除，docs task-management）。
- 远程规则：组织管理员写，用户只能开关（`alwaysEnabled` 的不能关，`rule-helpers.ts:270`）。

##### ⑤ 进上下文

- 规则：**每次请求常驻 system prompt**（`addUserInstructions` 路径，`responses.ts` 模板），开关关闭或 frontmatter 条件不满足则不注入。没有字节/token 上限（docs 只提醒 "Rules consume context tokens. Avoid lengthy explanations or pasting entire style guides"）。
- Memory Bank：靠 prompt 约定让模型在任务开始 `read_file` 全部文件，非自动检索。
- 任务内压缩：自动 compaction 触发阈值基于模型上下文窗口，4.1.x 起同时用 provider 报告的真实 token 数（CHANGELOG `## [4.1.x]` "Long tasks now compact when they actually need to…"）；`/compact` 手动；`context_history.json` 记录截断范围。压缩结果不跨任务。
- 任务历史：不注入新任务；用户可从 History 面板恢复旧任务继续（整段转录回到上下文）。

##### ⑥ 换项目

- 全局规则（`~/Documents/Cline/Rules` 等）、`~/.agents/AGENTS.md` 跟着用户走；工作区规则、Memory Bank 留在仓库，不会串到别的项目。
- 任务历史是全局的，但带 cwd 可过滤；切到别的目录默认也能看到所有任务。
- 同一仓库多个 clone / git worktree = 不同绝对路径 → `workspaces/<hash>` 不同、history 过滤结果不同、各自读各自目录下的 `.clinerules`（因为键是路径不是 git root/远程 URL）。
- 【官方】[CL4] 只讨论了多根工作区："Cline rules (`.clinerules/` directory) only work in the primary workspace (the first folder in your workspace)"，"Checkpoints are disabled in multi-root workspace mode"；建议把共享规则放主文件夹或用全局规则。**未找到**官方对 worktree / 同仓库多 clone 的专门讨论。

##### 一句话结论

作用域键 = 工作区绝对路径（规则、workspaceState hash、history 过滤）+ 用户 home（全局规则 / `~/.cline/data`）+ 组织 orgId（远程规则）；无 agent 身份层。记忆 = **无内置自动记忆**（Memory Bank 是 prompt 模式，产物是仓库里的 md）；规则文件 = `.clinerules`/`.cline/rules`/AGENTS.md（工作区）+ `~/Documents/Cline/Rules` 等（全局），每次请求常驻注入。

##### 未证实 / 未找到

- `@cline/shared` 的源码仓库位置：npm 元数据指向 cline/cline，但当前仓库里没有 `packages/`，只能读 minified dist（函数体已确认，行号不可给）。
- 自动 compaction 的具体百分比阈值：源码 grep `autoCondenseThreshold`/`autoCompact` 在 `apps/vscode/src` 无命中（逻辑在 SDK 包内），**未证实**具体数值。
- docs 页面没有更新日期。

#### 4.6.2 Roo Code

版本基线：RooCodeInc/Roo-Code main，`src/package.json` version **3.53.0**（raw 拉取于 2026-10-07）。核对的源码：`src/services/roo-config/index.ts`、`src/core/prompts/sections/custom-instructions.ts`、`src/services/code-index/{vector-store/qdrant-client.ts,cache-manager.ts,manager.ts}`、`src/core/config/CustomModesManager.ts`、`src/core/task-persistence/TaskHistoryStore.ts`、`src/utils/storage.ts`、`src/shared/globalFileNames.ts`、`packages/types/src/history.ts`、`src/core/webview/ClineProvider.ts`；`gh api` 列出 `src/services/`、`src/core/tools/`、`src/core/task-persistence/`。文档：docs.roocode.com 现 301 到 roocodeinc.github.io/Roo-Code：/features/custom-instructions（页面标注 2026-05-15）、/features/custom-modes、/features/codebase-indexing、/features/intelligent-context-condensing。

本节简称：「docs custom-instructions」= [RC1]；custom-modes = [RC2]；codebase-indexing = [RC3]；intelligent-context-condensing = [RC4]（docs.roocode.com 现跳转到 roocodeinc.github.io/Roo-Code）。

##### ① 持久记忆

- **没有内置自动记忆。**【官方】`src/services/` = `checkpoints code-index command glob mcp ripgrep roo-config search skills tree-sitter`；`src/core/tools/` 的工具为 ApplyDiff/ApplyPatch/AskFollowupQuestion/AttemptCompletion/CodebaseSearch/EditFile/ExecuteCommand/GenerateImage/ListFiles/NewTask/ReadCommandOutput/ReadFile/RunSlashCommand/SearchAndReplace/SearchFiles/Skill/SwitchMode/UpdateTodoList/UseMcpTool/WriteToFile/accessMcpResource，无 memory 工具；`gh search code "memory" repo:RooCodeInc/Roo-Code path:src` 命中 0。
- **Memory Bank 是社区方案**【非官方】：GreatScottyMac/roo-code-memory-bank、Shivabots/roo-memory-bank、shipdocs/roocode-memorybank-optimized 等都是「自定义 mode + rules 文件 + 仓库内 `memory-bank/` md」。官方文档库 RooCodeInc/Roo-Code-Docs 搜 "memory bank" 只命中 `docs/update-notes/v3.3.6.md`（"Added a `new_task` tool … enabling workflows like context continuation or memory bank updates"），即官方只把它当用户侧 workflow。
- **规则文件（人写）**：全局 `~/.roo/rules/`、`~/.roo/rules-{modeSlug}/`；项目 `.roo/rules/`、`.roo/rules-{modeSlug}/`；单文件回退 `.roorules`、`.roorules-{mode}`、`.clinerules`、`.clinerules-{mode}`；`AGENTS.md`/`AGENT.md`/`AGENTS.local.md`（`custom-instructions.ts:288-335`）；可选子目录 `.roo/`（monorepo，`enableSubfolderRules`，默认 false，`:396`）。另有 Prompts 面板的 "Global Instructions" 与每个 mode 的 `customInstructions`（存 globalState / modes 配置）。
- **自定义 mode（agent 身份）**：全局 `<globalStorage>/settings/custom_modes.yaml`，项目 `.roomodes`（`CustomModesManager.ts:19,249-258`）。
- **Codebase indexing = 代码检索，不是记忆**：代码块 embedding 存 Qdrant，文件 hash 缓存存本地 JSON；`codebase_search` 工具按需查询。
- **任务历史**：`<globalStorage>/tasks/<taskId>/{api_conversation_history.json, ui_messages.json, task_metadata.json, history_item.json}` + `tasks/_index.json`（`globalFileNames.ts`、`TaskHistoryStore.ts:20-32`）；`customStoragePath` 设置可改根目录（`storage.ts:13-47`）。
- **Intelligent Context Condensing**：任务内 LLM 摘要，不跨任务。
- **Sticky model / profile per mode**：`history.ts:22` `apiConfigName`，docs "Sticky Models"——这是 mode 级偏好，不是记忆。

##### ② 存在哪

- `src/services/roo-config/index.ts:26-29` `getGlobalRooDirectory()` = `~/.roo`；`:104-106` `getProjectRooDirectoryForCwd(cwd)` = `cwd/.roo`；`:155-165` `getRooDirectoriesForCwd` 顺序 `[global, project]`；`:186-200` `getAllRooDirectoriesForCwd` 再追加子目录 `.roo`（ripgrep 扫 `**/.roo/**`，`:73-110`）；`:53-56` `~/.agents`（skills 共享目录）。
- `src/core/prompts/sections/custom-instructions.ts:206-239` `loadRuleFiles`（目录优先，否则 `.roorules`/`.clinerules`）；`:402-439` mode 规则；`:449-492` 拼装顺序；`:46` 符号链接深度 5；`:168-176` 文件按文件名（不区分大小写）字母序；`:513-548` 排除 `.DS_Store/*.bak/*.log/…`；`:497-506` 最终包成 `USER'S CUSTOM INSTRUCTIONS` 段。
- `src/services/code-index/vector-store/qdrant-client.ts:81-83`：collection 名 = `ws-` + `sha256(workspacePath)` 前 16 位；`cache-manager.ts:24-27`：`<globalStorage>/roo-index-cache-<sha256(workspacePath)>.json`；`manager.ts:19,32-62`：`CodeIndexManager.instances` 是按 workspace 路径的 Map（多根工作区每个 folder 一个实例）。
- `src/core/config/CustomModesManager.ts:19` `.roomodes`；`:249-258` `settings/custom_modes.yaml`；`:297-301` `.roomodes` 优先并合并。
- `src/utils/storage.ts:52-58` `getTaskDirectoryPath` = `<basePath>/tasks/<taskId>`；`TaskHistoryStore.ts:552-555` 同。
- `packages/types/src/history.ts:20` `workspace: z.string().optional()`；`ClineProvider.ts:2491-2499` 最近任务过滤 `item.workspace !== this.cwd` 则跳过；`TaskHistoryStore.ts:148-150` `getByWorkspace` 严格相等。
- 【官方】docs custom-instructions（2026-05-15）："Global Rules first, then workspace rules. If there's a conflict, workspace rules take precedence"；"System loads rules from ALL applicable directories (both global `~/.roo/` and workspace `.roo/`)"；"Files are read and appended to the system prompt in alphabetical order based on filename"。

##### ③ 作用域键

- **规则 = 三维叠加**：用户 home（`~/.roo`）× **工作区 cwd 绝对路径**（`cwd/.roo`，可选子目录 `.roo`）× **mode slug**（`rules-{mode}`）。Roo 的 mode 就是「按 agent 身份挂规则」的实现：同一 slug 在全局和项目两处都可有 `rules-{slug}/`，全局先、项目后叠加（不是替换）；mode 定义本身则是项目 `.roomodes` **整体覆盖**同 slug 的全局定义（docs："the `.roomodes` version completely overrides the global one. This applies to ALL properties"）。
- **AGENTS.md**：按目录（根目录，开启子目录规则后还包括含 `.roo` 的子目录）。
- **代码索引**：`sha256(workspace 绝对路径)`，同一 Qdrant 实例内按 collection 隔离。
- **任务历史**：全局存储，每条带 `workspace`（cwd 字符串），UI 用严格字符串相等过滤。
- **会话**：taskId；子任务树（`rootTaskId`/`parentTaskId`/`childIds`，`history.ts:9-11,24-27`）。
- 键都是路径字符串，没有 git root / 远程 URL 概念。

##### ④ 谁写

- 规则文件：用户手写；模型可用 `write_to_file` 写（普通文件写入，走审批）。Prompts 面板的 Global/Mode instructions 由用户在 UI 填写。
- modes：UI、YAML 手写、或模型通过工具写 `.roomodes`；支持导出/导入单个 YAML（docs）。
- 索引：自动（需用户配置 embedder + Qdrant URL），可暂停/清除。
- 任务历史：自动；可删除、导出。
- condensing：自动（阈值）或手动按钮；可自定义摘要 prompt。

##### ⑤ 进上下文

- 规则：**每次请求常驻 system prompt**。顺序（`custom-instructions.ts:441-492`，docs 同）：Language Preference → Global Instructions → Mode-specific Instructions → `rules-{mode}` 目录 → `.roorules-{mode}` → `.rooignore` 说明 → AGENTS.md → `rules/` 目录 → `.roorules`。目录递归读取、字母序、去掉缓存类文件；**无大小上限**。
- 代码索引：`codebase_search` 工具按需 RAG，返回片段+路径+相似度，不常驻。
- Condensing：默认阈值 100%（docs），到阈值用 LLM 摘要早期消息，摘要替换原消息用于后续调用，checkpoint 回滚可恢复原文；只在任务内。

##### ⑥ 换项目

- `~/.roo/rules`、`~/.roo/rules-{mode}`、全局 modes、sticky model 跟用户走；`.roo/`、`.roomodes` 留在仓库。
- 同一仓库多 clone / worktree：路径不同 → 各自一套 `.roo`（内容相同）、各自一个 Qdrant collection（重复索引）、history 的 `workspace` 不同。
- 【官方】docs codebase-indexing："In multi-folder workspaces, each folder maintains its own indexing status and configuration"；docs custom-instructions 有 monorepo 子目录 `.roo` 的 `enableSubfolderRules` 设置。**未找到**对 worktree / 多 clone 的专门讨论。
- 身份层：mode 的全局规则跨项目一致，项目级 `rules-{mode}` 叠加，这是 Cline、Roo、Aider、Continue 这一批里唯一把「身份」做成一级作用域键的。

##### 一句话结论

作用域键 = 用户 home × 工作区 cwd 绝对路径 × mode slug（规则）；索引 = sha256(工作区路径)；历史 = 全局 + `workspace` 字段过滤。记忆 = **无内置自动记忆**（Memory Bank 为社区 rules/mode 方案；codebase indexing 是向量代码检索）；规则文件 = `.roo/rules*`、`.roorules*`、`.clinerules*`、AGENTS.md（项目）+ `~/.roo/rules*`（全局），每次请求常驻。

##### 未证实 / 未找到

- 搜索结果提到的 "Implement memory bank directly in Roo Code" Discussion #3319：`gh api graphql` 查 discussion 与 `repos/.../issues/3319` 都返回 NOT_FOUND，**未证实**其状态（可能已删除或编号有误）。
- `autoCondenseContextPercent` 默认值：`packages/types/src/global-settings.ts:118-119` 只有字段定义，默认值在别处，文档说 100%，源码**未证实**。
- 未能完整 clone 仓库做全文 grep，"无 memory 模块" 的结论基于目录列表 + GitHub code search。

#### 4.6.3 Aider

版本基线：Aider-AI/aider main，`aider/__init__.py` `__version__ = "0.86.3.dev"`（raw 拉取于 2026-10-07）；GitHub 最新 release **v0.86.0**（2025-08-09）。核对的源码：`aider/main.py`、`aider/args.py`、`aider/repomap.py`、`aider/history.py`、`aider/coders/base_coder.py`、`aider/models.py`、`aider/io.py`。文档（无日期）：aider.chat/docs/usage/conventions.html、/docs/config/options.html、/docs/config/aider_conf.html、/docs/repomap.html、/docs/faq.html、/docs/usage/commands.html、/docs/usage/tips.html。

本节简称：「docs conventions」= [AI1]；options = [AI2]；aider_conf = [AI3]；repomap = [AI4]；FAQ = [AI5]；commands = [AI6]。

##### ① 持久记忆

- **没有持久记忆（证实）。**【官方】`gh search code "memory" repo:Aider-AI/aider path:aider` 只命中 `aider/io.py`、`aider/repomap.py`（均为 "falling back to memory cache" 之类的内存缓存语义）和一篇博客；无 memory 模块、无自动写入的记忆文件。
- 有的只是四类文件：
  1. **聊天转录** `.aider.chat.history.md`：纯日志（markdown append），默认**不**回读；`--restore-chat-history` 时作为 `done_messages` 读回（`base_coder.py:519-522`），并立刻走摘要器（`summarize_start`）。
  2. **输入历史** `.aider.input.history`（prompt_toolkit 的命令行历史）；可选 `.aider.llm.history`（`--llm-history-file`，原始 LLM 日志）。
  3. **规则文件（人写）**：任意文件（惯例 `CONVENTIONS.md`）用 `--read` / `/read-only` / `.aider.conf.yml` 的 `read:` 加为只读文件；docs："It's best to load the conventions file with `/read CONVENTIONS.md` or `aider --read CONVENTIONS.md`. This way it is marked as read-only, and cached if prompt caching is enabled."
  4. **repo map 的 tags 缓存** `.aider.tags.cache.v4/`（diskcache SQLite，`repomap.py:35-43,217-222`）：只是 tree-sitter 解析结果缓存，不是记忆；坏了就回退内存 dict（`:177-215`）。
- `/save` 把「当前会话加了哪些文件」写成命令脚本，`/load` 重放（docs commands）；`/clear` 清历史，`/reset` 清文件+历史。
- `~/.aider/` 只放 OAuth key（`main.py:370` `Path.home()/".aider"/"oauth-keys.env"`）等杂项，没有记忆。

##### ② 存在哪

- `aider/args.py:271-277`：`default_input_history_file = git_root/.aider.input.history if git_root else ".aider.input.history"`；`default_chat_history_file` 同理 → **有 git 仓库时放在 git root，否则放 cwd**。
- `aider/main.py:464-477`：`.aider.conf.yml` 查找顺序 `[CWD, git root, ~]`，`:494` 反转后交给 configargparse（后者覆盖前者，即 cwd > git root > home）；docs aider_conf："Your home directory. The root of your git repo. The current directory."，"`--config <filename>` … will only load the one config file"。
- `aider/main.py:155-200` `check_gitignore`：启动时询问把 `.aider*`（和 `.env`）加进 `.gitignore`（`--no-gitignore` 可跳过）。
- `aider/repomap.py:43` `TAGS_CACHE_DIR = f".aider.tags.cache.v{CACHE_VERSION}"`（v4）；`:218` 放在 `Path(self.root)`；`base_coder.py:447` `self.root = self.repo.root`（git root），无 git 时 `:476` 取已加文件的公共根目录。
- `aider/models.py:339,356-358`：`max_chat_history_tokens` 默认 1024，按模型 `min(max(max_input_tokens/16, 1024), 8192)`。
- `aider/history.py:7-18` `ChatSummary(max_tokens)`，`too_big()` 判定。

##### ③ 作用域键

- **git root**（`main.py:60-66` `get_git_root()` = `git.Repo(search_parent_directories=True).working_tree_dir`；解析完参数后 `:69-85` `guessed_wrong_repo` 再用 `GitRepo(...).root` 校正）：chat/input 历史、tags cache、`.env`、`.aider.conf.yml` 的仓库层、`.aiderignore` 都挂在 git root；无 git 时退化为 cwd。
- **用户 home**：只有 `~/.aider.conf.yml`（可在里面写 `read:` 指向全局 conventions 文件）和 `~/.aider/oauth-keys.env`。
- **agent 身份层：没有。** 会话 = 进程生命周期；多次会话向同一个 `.aider.chat.history.md` 追加。
- `--subtree-only` 把 repo map / 文件范围限制在启动子目录（FAQ："This will tell aider to ignore the repo outside of the directory you start in"）。
- FAQ："Currently aider can only work with one repo at a time."

##### ④ 谁写

- 转录/输入历史：程序自动追加；用户可直接编辑/删除文件；无审批概念。
- 规则文件：用户手写；模型不会主动写（除非用户要求它编辑那个文件——此时与改任何源码文件一样走 SEARCH/REPLACE + git 自动提交）。
- `/save` 脚本：用户触发。

##### ⑤ 进上下文

- **常驻注入**：每次请求都把 read-only 文件全文（`format_chat_chunks`，`base_coder.py:1226+`）、repo map（`--map-tokens` 默认 1k，会动态放大；docs repomap："Aider sends a repo map to the LLM along with each change request"）、以及 `done_messages`（`:1279` `chunks.done = self.done_messages`）放进去。
- **历史裁剪**：`done_messages` 超过 `max_chat_history_tokens` 就后台摘要（`base_coder.py:1003-1015` `summarize_start` → `ChatSummary.summarize`，`history.py:27-100` 保留尾部、摘要头部），恢复的历史也先摘要。
- 没有检索：无向量库、无按需工具；repo map 的「相关性」靠 PageRank 对当前聊天文件/提及符号加权（docs repomap）。

##### ⑥ 换项目

- 换到另一个 git 仓库 = 另一套 `.aider.*` 文件，互不串；身份/偏好只有 `~/.aider.conf.yml`（模型、flags、全局 `read:` 文件）跟着走。
- 同一仓库多 clone：各自 git root → 各自历史与缓存。git worktree：`get_git_root()` 取 `git.Repo(search_parent_directories=True).working_tree_dir`（`main.py:60-66`），对 worktree 而言就是 worktree 目录本身——所以也是分开的（从 GitPython 语义推断，**未证实**有专门测试）。
- 官方讨论：FAQ 有 "Can I use aider in a large (mono) repo?"（`--subtree-only`、`.aiderignore`）和 "multiple git repos at once"（不支持，用 `/read` 拉别的仓库文件）；**未找到** worktree 讨论。

##### 一句话结论

作用域键 = git root（无 git 则 cwd）+ 用户 home（仅 `~/.aider.conf.yml`）；无身份层、无跨会话记忆。记忆 = **无**（只有可选回读并立即摘要的 `.aider.chat.history.md`）；规则文件 = 任意只读文件（`--read CONVENTIONS.md` / conf `read:`），每次请求全文常驻。

##### 未证实 / 未找到

- git worktree 场景下 `git_root` 的实际值未做实验验证。
- 文档页面均无日期；源码 main 分支相对 v0.86.0 已有改动但相关逻辑（args/history/repomap）未见变化。

#### 4.6.4 Continue

版本基线：continuedev/continue main（raw 拉取于 2026-10-07），`extensions/vscode/package.json` version **1.3.40**；GitHub 最新 release 标签 **v2.0.0-vscode**（2026-06-19）。核对的源码：`core/util/paths.ts`、`core/config/markdown/{loadMarkdownRules.ts,loadCodebaseRules.ts}`、`core/config/getWorkspaceContinueRuleDotFiles.ts`、`core/config/loadLocalAssistants.ts`、`core/config/profile/doLoadConfig.ts`、`core/config/yaml/loadYaml.ts`、`core/llm/rules/{getSystemMessageWithRules.ts,constants.ts}`、`core/indexing/{CodebaseIndexer.ts,utils.ts,refreshIndex.ts,LanceDbIndex.ts}`、`core/util/{history.ts,GlobalContext.ts}`、`core/tools/definitions/{requestRule.ts,createRuleBlock.ts}`、`core/tools/implementations/createRuleBlock.ts`、`extensions/cli/src/session.ts`；`gh api` 列出 `core/context/providers/`、`core/tools/definitions/`。文档（无日期）：docs.continue.dev/customize/deep-dives/rules、/customize/rules、/reference、/reference/deprecated-codebase、/faqs、/guides/codebase-documentation-awareness。

本节简称：「docs deep-dive」= [CN1]；reference = [CN2]；deprecated-codebase = [CN3]；faqs = [CN4]。

##### ① 持久记忆

- **没有内置自动记忆。**【官方】`gh search code "memory" repo:continuedev/continue path:core` 9 个命中全是无关项（`AutocompleteLruCache`、Ollama 参数、vendored transformers.js、测试）；`path:extensions/cli` 10 个命中是 `ResourceMonitoringService`、`TextBuffer` 等。`core/tools/definitions/` 无 memory 工具，`core/context/providers/` 无 memory provider。docs 全站搜 "memory" 只有 Ollama `keepAlive` 和第三方 "Memory MCP server"。
- **最接近「模型写记忆」的是 `create_rule_block` 工具**：描述为 "Creates a 'rule' that can be referenced in future conversations… or when you want to avoid making a mistake again"（`createRuleBlock.ts:28-30`），实现写到**第一个工作区目录**的 `.continue/rules/<name>.md`（`implementations/createRuleBlock.ts:33-36`）。它仍是规则文件（用户可见、git 可管）而非自动记忆，且由模型按需调用。
- **规则文件（人写或模型写）**：工作区 `.continue/rules/*.md`、`.continue/prompts/*.md`、`.continuerules`、`AGENTS.md`/`AGENT.md`/`CLAUDE.md`（根目录，只取第一个命中，`alwaysApply: true`）、任意子目录的 `rules.md`（colocated）；全局 `~/.continue/rules/*.md`、`~/.continue/prompts`；`config.yaml` 的 `rules:` 块（本地或 `uses: <hub-slug>` 引用 Hub 上的规则）；Hub assistants（云端，按账号/组织）。
- **会话记录**：`~/.continue/sessions/<sessionId>.json` + `sessions.json` 索引（带 `workspaceDirectory`）。
- **代码索引（不是记忆）**：`~/.continue/index/index.sqlite`（chunk/FTS/tag_catalog）、`~/.continue/index/lancedb/`（向量）、`docs.sqlite`、`autocompleteCache.sqlite`。`@Codebase` 已标 deprecated（docs reference/deprecated-codebase），Agent 模式改用 grep/glob/read 工具。
- `~/.continue/` 还有：`config.yaml`、`.continuerc.json`、`sharedConfig.json`、`index/globalContext.json`（UI/偏好状态）、`dev_data/`、`logs/`、`.utils/repo_map.txt`、`.continueignore`（全局忽略）。

##### ② 存在哪

- `core/util/paths.ts:30-36` `CONTINUE_GLOBAL_DIR`（env 或 `~/.continue`）；`:69-76` `getContinueGlobalPath`；`:78-84` `sessions/`；`:86-92` `index/`；`:94-96` `index/globalContext.json`；`:102-112` `sessions/<id>.json`、`sessions/sessions.json`；`:119` `config.yaml`；`:210-212` `.continuerc.json`；`:326-340` `index.sqlite`、`lancedb`、`autocompleteCache.sqlite`、`docs.sqlite`；`:405` `prompts/`；`:433` `.utils/repo_map.txt`。
- `core/config/loadLocalAssistants.ts:104-124` `getDotContinueSubDirs`：`<每个 workspaceDir>/.continue/<sub>` + `~/.continue/<sub>`（`includeGlobal`）。
- `core/config/markdown/loadMarkdownRules.ts:10` `SUPPORTED_AGENT_FILES = ["AGENTS.md","AGENT.md","CLAUDE.md"]`；`:22-55` 只取第一个工作区的第一个命中，`alwaysApply: true`；`:57-102` `.continue/rules` 与 `.continue/prompts`（global+workspace）。
- `core/config/getWorkspaceContinueRuleDotFiles.ts:4` `.continuerules`；`core/llm/rules/constants.ts` `rules.md`；`loadCodebaseRules.ts:65-85` 遍历工作区找 `rules.md`。
- `core/config/profile/doLoadConfig.ts:40-61` `loadRules` 合并顺序：`.continuerules` → markdown rules → colocated。
- `core/tools/implementations/createRuleBlock.ts:33-36`：`[localContinueDir] = ide.getWorkspaceDirs()` → 写 `.continue/rules/<slug>.md`。
- `core/indexing/utils.ts:18,29-53` `tagToString` = `"{directory}::{branch}::{artifactId}"`（过长时目录取 hash 前缀+截尾）；`CodebaseIndexer.ts:254-263,405-412` `branch = ide.getBranch(directory)`；`refreshIndex.ts:28-31,80-86,116-117` `tag_catalog(dir, branch, artifactId, path, cacheKey)` 唯一索引。
- `core/util/history.ts:41-48`：会话列表按 `workspaceDirectory.toLowerCase() === target` 过滤；`extensions/cli/src/session.ts:50-67,103,344`：CLI 会话同样存 `~/.continue/sessions/<id>.json`，`workspaceDirectory: process.cwd()`。
- `core/util/GlobalContext.ts:24-29`：`lastSelectedProfileForWorkspace[workspaceIdentifier]`、`lastSelectedOrgIdForWorkspace[workspaceIdentifier]`；`:30-32` `selectedModelsByProfileId`。
- 【官方】docs faqs："Continue stores its data in the `~/.continue` directory"；deprecated-codebase："all embeddings are calculated locally using `transformers.js` and stored locally in `~/.continue/index`"，"metadata is stored in `~/.continue/index/index.sqlite`"。

##### ③ 作用域键

- **规则 = 工作区目录（每个 workspace folder 的 `.continue/rules`，多根都加载）+ 用户 home（`~/.continue/rules`、`config.yaml`）+ Hub 账号/组织（assistant 配置云端）+ 文件级（`globs`/`regex` frontmatter；colocated `rules.md` 按所在目录）**。AGENTS.md/CLAUDE.md 例外：只取第一个工作区的第一个文件。
- **代码索引 = (目录绝对路径, git 分支, artifactId)**：分支是键的一部分，同目录切分支会各建一份 tag（cache 按 `cacheKey` 共享内容，`global_cache` 表）。
- **会话 = `workspaceDirectory` 字符串**（小写比较），全局目录统一存放。
- **agent 身份层**：有「profile / assistant」概念（Hub assistant = 一整套 config：models+rules+tools），`lastSelectedProfileForWorkspace` 按 workspace 记住选的 profile。规则挂在 assistant 配置上，可视为按 agent 配置挂规则，但不是「记忆」。
- 无 git root / 远程 URL 键（`getRepoName` 仅用于显示）。

##### ④ 谁写

- 规则：用户手写（UI "Add Rules"、Hub 编辑）、或模型调用 `create_rule_block`（docs："you can say 'Create a rule for this', and a rule will be created for you in `.continue/rules`"）——文件可见、可编辑、可 git 管理；是否走审批取决于工具权限设置。
- 会话：自动保存；可删。
- 索引：自动（可 pause、`Continue: Rebuild codebase index`）；`.continueignore` 控制范围。

##### ⑤ 进上下文

- 规则**拼进 system message 每次请求**（docs deep-dive："To form the system message, rules are joined with new lines, in the order they appear in the toolbar"；reference："Rules are concatenated into the system message for all Agent, Chat, and Edit requests"）。取舍逻辑 `getSystemMessageWithRules.ts:141-160,205-260`：`alwaysApply: true` 或根级无 globs → 常驻；有 `globs`/`regex` → 仅当上下文里的文件匹配；`alwaysApply: false` + `description` → 不常驻，模型通过 `request_rule` 工具按需拉取（`requestRule.ts:11,27-34`）。"Rules are not included in autocomplete or apply"。**无大小上限**。
- 索引：`@Codebase`/`@Folder` 检索（embedding + 全文 + rerank），已 deprecated；Agent 模式靠工具实时探索。
- 会话：不自动带入新会话。

##### ⑥ 换项目

- `~/.continue/rules`、`config.yaml`、Hub assistants（登录后云同步）跟用户走；`.continue/rules`、AGENTS.md 留仓库。
- 同一仓库多 clone / worktree：目录不同 → 索引 tag 不同（各自重建）、会话 `workspaceDirectory` 不同、`.continue/rules` 各自读；同目录切分支也会按分支分别索引。
- 【官方】docs 只提到全局 `~/.continue/.continueignore` 和多根工作区各自加载；**未找到**对 worktree / monorepo / 多 clone 的专门讨论。

##### 一句话结论

作用域键 = 工作区目录（规则、会话）+ 用户 home + Hub 账号/组织（assistant 配置）+ 文件 glob；索引键 = (目录, git 分支)。记忆 = **无内置自动记忆**（最接近的是模型可用 `create_rule_block` 写规则文件）；规则文件 = `.continue/rules/*.md`、AGENTS.md/CLAUDE.md、`.continuerules`、colocated `rules.md`（工作区）+ `~/.continue/rules`、`config.yaml`、Hub（全局/云端），按 `alwaysApply`/globs 决定常驻或按需。

##### 未证实 / 未找到

- Hub assistants 的云端存储细节（按用户还是按组织、是否同步到本地文件）：docs hub 页面抓取 404，**未证实**，只从 `config.yaml` `uses:` 与 `GlobalContext.lastSelectedOrgIdForWorkspace` 推断。
- `v2.0.0-vscode` release 与 main 上 `1.3.40` 的关系（可能是不同发布通道），**未证实**。
- 未能完整 clone 仓库，"无 memory 模块" 基于 GitHub code search 与目录列表。

#### 4.6.5 Amp（Sourcegraph，闭源）
版本基线：ampcode.com/docs 2026-10-06 抓取（旧的 /manual 已改为 /docs），/news 最新 September 29, 2026。核对页面：`docs/customize/agents-md`、`docs/threads`、`docs/projects`、`docs/collaborate/workspaces`、`docs/customize/skills`、`docs/customize/global-plugins-and-skills`、`docs/cli/settings`、`docs/enterprise/workspace-thread-visibility-controls`、`docs/enterprise/minimal-data-retention`、`/security#thread-data`、`/guides/context-management`、news：`handoff`（Oct 23 2025）、`ask-to-handoff`（Jan 13 2026）、`read-threads`（Oct 29 2025）、`find-threads`（Dec 8 2025）、`agentic-code-review`（Dec 18 2025）、`end-of-public-threads`（Jun 2 2026）、`global-plugins-and-skills`（Aug 11 2026）、`multi-repo-projects`（Aug 27 2026）、`one-runner-many-worktrees`（Sep 22 2026）。

本节简称：agents-md = [AP3]；plugins = [AP1]；skills = [AP4]；Handoff = [AP5]；context-management = [AP6]；security = [AP7]；Agentic Review = [AP2]。其余 news 条目只以日期与标题引用，未单独编号。

##### ① 持久记忆
- **自动记忆：未找到。** 全部 153 条 news 标题和所有 docs 页面 grep `memor|remember`，只有两处：(a) Plugin API 示例命令 "Thread note"，提示语 "What should Amp remember in this thread?"，把一条笔记追加到当前 thread（不是跨 thread 记忆）；(b) Agentic Review（Dec 18 2025）列的开放问题："How do we incorporate review feedback into long-term memory? When you accept or reject review comments, should Amp learn from that? Should it incorporate feedback into AGENTS.md?"——即官方当时**没有**长期记忆机制，设想的落点是 AGENTS.md。【官方】[AP1] 、[AP2]
- **规则文件 AGENTS.md**（人写，Amp 可代写）：`AGENTS.md` in cwd、parent dirs（到 `$HOME`）、subtrees；`$HOME/.config/amp/AGENTS.md` 与 `$HOME/.config/AGENTS.md`（个人）；`/etc/ampcode/AGENTS.md`、`/Library/Application Support/ampcode/AGENTS.md`、`%ProgramData%\ampcode\AGENTS.md`（系统/组织）；web 端 Settings → Advanced → Global AGENTS.md（个人）与 Workspace Settings 的 Global AGENTS.md（工作区，先于个人与仓库注入，不作用于 subagent/Puck）。无 `AGENTS.md` 时回退 `AGENT.md` / `CLAUDE.md`。支持 `@path`、`@~/`、glob 引用，被引用文件可用 YAML `globs:` 限定只在读到匹配文件时注入。`AMP_IGNORE_GUIDANCE_FILES` 可屏蔽。【官方】[AP3] ；news AGENT.md（May 7 2025）、Multiple AGENT.md Files（Jul 7 2025）、AGENTS.md（Aug 20 2025）、Globs（Sep 23 2025）
- Skills / Plugins：项目 `.agents/skills/`（及 `.claude/skills/`）、机器级 `~/.config/agents/skills/`、`~/.agents/skills/`、`~/.config/amp/skills/`、`~/.claude/skills/`、`amp.skills.path`；个人与工作区 skills/plugins 各存在 Amp 托管的 Git 仓（`amp clone workspace-skills`）。【官方】[AP4] 、/global-plugins-and-skills
- **Thread 即长期记录**：thread 含"your prompts, the agent's replies, every tool call it made, and the files it changed"，存云端、跨设备、可分享、可搜索（关键词 / 触及的文件 / 作者 / label / 日期）、可 `@T-…` 引用让 agent 从中抽取、可 fork、可 Handoff。
- 会话压缩：**2025-10-23 起移除 compaction，改为 Handoff**（"Instead of summarizing a thread, you're extracting from it what matters for your next task"）；另一个模型分析旧 thread，生成带相关文件的新 prompt 草稿供编辑。【官方】[AP5] 、[AP6]

##### ② 存在哪
- Thread：ampcode.com 服务器（URL `https://ampcode.com/threads/T-…`，web/CLI/iOS/macOS 同一份）；删除后 30 天内清除；企业工作区的 thread 归企业所有。【官方】[AP7]
- AGENTS.md：仓库 / 用户 config / 系统目录 / web 端 Global AGENTS.md（云端）。
- Skills/plugins：个人与工作区各一个 Amp 托管 Git 仓；项目内 `.agents/skills`；本机 `~/.config/agents/skills`。
- 设置：`~/.config/amp/settings.json`、项目 `.amp/settings.json`。
- Orb 项目设置脚本、Secrets & Env Vars：ampcode.com 项目设置（"Amp stores them outside the repository"）。

##### ③ 作用域键
| 对象 | 键 |
| - | - |
| Thread | 归属 **Project**（= 仓库 URL/GitHub 仓 + 可选附加仓库；CLI `amp -ox` 按当前目录 git remotes 匹配项目，可配 Git Remote Aliases）+ **Owner**（个人 / Workspace）；visibility private / workspace / group / unlisted；`amp.defaultVisibility` 按**仓库 origin**（`{"github.com/org/repo": "workspace"}`）设默认。commit 带 `Amp-Thread-ID` trailer。 |
| AGENTS.md | 文件系统路径层级（cwd→$HOME）+ 用户 config + 系统 + workspace（云端） |
| Skills/Plugins | 个人（用户账号，"available everywhere you use Amp"）/ Workspace / 项目目录 / 本机目录；同名优先级：本地 > 个人 > 工作区 > 官方 |
| 环境 | Orb snapshot 按项目 × orb size |

##### ④ 谁写
- AGENTS.md：人写；"Amp offers to generate an AGENTS.md file for you if none exists"，也可要求 "Update AGENTS.md based on what I told you in this thread"——**只在用户要求时写**，没有自动写入。
- Thread：系统自动记录，用户可改可见性、归档、删除、导出 `.md`。
- Skills：人写或让 Amp 建（Amp 在 checkout 里改、commit，"asks before pushing"）。

##### ⑤ 进上下文
- AGENTS.md：cwd/父目录/用户/系统的**常驻注入**；子树 AGENTS.md 在读到子树文件时加载；`globs:` 文件只在读到匹配文件后加载。大小上限：**未找到**（只建议顶层保持通用、子项目各自一份）。
- Thread：**不自动注入**；用户 `@T-…` 或粘贴 URL 时 agent 读取并"pulls out what matters"（Jul 2 2026 起可读任意长度）；agent 也可主动搜索 thread。
- Handoff：按需、生成式抽取；上下文窗口上限随模型（Sonnet 4 时 1M tokens，Aug 27 2025）；官方倡导短 thread（"200k Tokens Is Plenty"，Dec 10 2025）。
- Skills：SKILL 进度披露（`reload_skills` 工具重扫）。

##### ⑥ 换项目
- 没有自动记忆，所以不存在串项目问题；个人层 AGENTS.md / 个人 skills 跟用户走到任何项目；仓库层随目录。
- Thread 按 Project 分组；在 CLI 换目录即按 git remote 切换项目。多仓库项目（Aug 27 2026）把多个仓库放进一个项目。
- Worktree：runner 可从 thread composer 直接建 worktree（`~/code/amp` → 兄弟目录 `~/code/amp-<branch>`），thread 在新目录启动，结束时 "Archive and Remove Worktree"；AGENTS.md 按新目录层级重新解析（同一仓库的 worktree 自然共享仓库内 AGENTS.md）。多根 workspace：Multi-Root Workspaces（Jun 2 2025）。
- 跨项目的 thread 引用/搜索是显式、人工的（工作区默认可见彼此 thread）。

##### 一句话结论
作用域键 = 目录层级（AGENTS.md）+ 用户账号/Workspace（个人与工作区 AGENTS.md、skills、plugins）+ Project（仓库 URL）×Owner（thread）；记忆 = **没有自动记忆**，只有云端 thread（按需引用/搜索/Handoff 抽取），规则文件 = AGENTS.md（多层常驻，Amp 可代写但不自动写）。

##### 补充：模式调研里的相关证据

- 用户层位置是 `~/.config/amp/AGENTS.md`、`~/.config/AGENTS.md`，另有系统层；与 Codex 的 `$CODEX_HOME/AGENTS.md` 不同，**没有跨产品统一的 `$HOME/AGENTS.md`** [AP3][CX3]。

##### 未证实 / 未找到
- 任何自动/隐式记忆、偏好学习机制（docs/news 均无）。
- AGENTS.md 注入的大小上限。
- CLI 本地是否缓存 thread 副本（docs 未提；官方描述为服务器存储）。
- 旧 ampcode.com/manual 页面内容（已重定向到 /docs 结构）。

#### 4.6.6 OpenCode（sst/opencode，opencode.ai）
版本基线：main @ `3f393d78` (2026-10-06)，`packages/opencode/package.json` 1.18.34，最新 release **v1.18.34**（2026-09-30）。核对文档（仓库内即官网源）：`packages/web/src/content/docs/rules.mdx`、`agents.mdx`、`ecosystem.mdx`。核对源码：`packages/core/src/global.ts`、`packages/core/src/project.ts`、`packages/core/src/project/sql.ts`、`packages/core/src/session/sql.ts`、`packages/core/src/database/database.ts`、`packages/core/src/fs-util.ts`、`packages/opencode/src/session/instruction.ts`、`packages/opencode/src/session/system.ts`、`packages/opencode/src/storage/storage.ts`、`packages/opencode/src/config/agent.ts`、`packages/opencode/src/project/project.ts`。
同名项目一句话：`opencode-ai/opencode`（Go 版，Kujtim Hoxha）已于 2025-09 归档（`gh api`: archived=true, pushed_at 2025-09-18），其后继是 `charmbracelet/crush`；本节只讲 sst 版。

本节源码与文档均为仓库内文件（`packages/web/src/content/docs/*.mdx` 即官网源），以 commit `3f393d78` 为准。

##### ① 持久记忆
- **规则文件**：有。`AGENTS.md`（兼容 `CLAUDE.md`，`CONTEXT.md` 已弃用，`instruction.ts:64-68`）；全局 `~/.config/opencode/AGENTS.md`（回退 `~/.claude/CLAUDE.md`，:60-63）；`opencode.json` 的 `instructions: [...]` 可列本地 glob 或 http(s) URL（5 秒超时）；`/init` 生成/改进 AGENTS.md（`rules.mdx`）。
- **记忆（模型自动积累）**：**未找到**。`packages/opencode/src` 非测试文件 grep -i `memory` 零命中。官方生态页列了第三方插件 `opencode-supermemory`「Persistent memory across sessions using Supermemory」（`ecosystem.mdx:42`）——非官方功能。
- 会话：SQLite（`~/.local/share/opencode/opencode.db`），会话摘要（summary/compaction）是会话态。
- agents：`~/.config/opencode/{agent,agents}/**/*.md` 与 `.opencode/{agent,agents}/`（`config/agent.ts:13,22`；文档写 `agents/`，代码两种都扫）——只是 prompt/模型/权限 profile，**不带记忆**。

##### ② 存在哪
`packages/core/src/global.ts:10-16`【官方】：data = `$XDG_DATA_HOME/opencode`（默认 `~/.local/share/opencode`），config = `~/.config/opencode`（`OPENCODE_CONFIG_DIR` 覆盖，:60），state = `~/.local/state/opencode`，cache = `~/.cache/opencode`。
- 数据库：`database.ts:53` → `<data>/opencode.db`（SQLite via drizzle；bun/node 两种驱动）。表：`project`（id, worktree, vcs, sandboxes…，`project/sql.ts:6-18`）、`project_directory`（project_id, directory, type ∈ main|root|git_worktree，:21-35）、`session`（id, project_id, workspace_id, parent_id, **directory**, title, agent, model, revert, permission…，`session/sql.ts:22-62`）、`message`、`part`。
- 遗留 JSON：`<data>/storage/{project,session,message,part,session_diff}/…json`（`storage/storage.ts:224` 及 migration.1/2），启动时迁入 SQLite。
- 项目 ID 缓存：`<git common dir>/opencode` 文件（`core/src/project.ts:65-71 cached()`、:123-125 `commit()`）。
- 全局规则 `~/.config/opencode/AGENTS.md`；agents md 见上。

##### ③ 作用域键
- **项目 = 远程 URL（首选）**：`Project.resolve()`（`core/src/project.ts:110-121`）：`id = remote(repo) ?? cached ?? root(repo)`，其中 `remote` = `Hash.fast("git-remote:" + host小写 + "/" + 去掉 .git 的路径)`（:73-100，`file:` 协议不算，支持 scp 形式）；无远程则用 `.git/opencode` 缓存的旧 id，再退到**首个 root commit sha**（:104-108）；不在 git 仓库 → `global`。`directory = repo.worktree`。旧版（storage 迁移代码）用的是 `git rev-list --max-parents=0 --all` 的根提交。
  - 推论：同一远程的所有 clone / worktree **共享一个 project 记录和会话列表**；`project_directory` 表按目录记录 main/root/git_worktree，`session.directory` 记录每个会话实际目录。
- **用户全局**：`~/.config/opencode/`。
- **规则文件发现**（`instruction.ts:110-140 systemPaths`）：全局文件取第一个存在的；项目级 `fs.findUp(file, ctx.directory, ctx.worktree)`——从 cwd 向上**到 worktree（git root）为止**收集所有同名文件（`fs-util.ts:154-166`），按 `AGENTS.md`→`CLAUDE.md`→`CONTEXT.md` 顺序，第一个有命中的**文件名**胜出（:122 注释）；`instructions` 里的相对 glob 用 `globUp` 从 directory 到 worktree；`OPENCODE_DISABLE_PROJECT_CONFIG` 时只看全局目录。子目录 AGENTS.md 在 `read` 工具读到该子树文件时**按消息**追加一次（:179-200 `resolve`，「Walk upward from the file being read and attach nearby instruction files once per message」）。
- **会话**：SQLite 行，键 session id；按 project_id 与 directory 过滤。
- **agent**：agent 名只是 prompt 配置键；`session.agent` 列记录会话用的 agent。无 agent 级记忆。

##### ④ 谁写
人写 AGENTS.md；`/init` 由模型一次性生成（「If you already have an AGENTS.md, /init will improve it in place」）。没有模型自动写记忆的机制（未找到）。

##### ⑤ 进上下文
每次请求把 `Instructions from: <path>\n<content>` 逐个拼进 system prompt（`instruction.ts:155-176 system()`，远程 URL 每次 fetch、失败置空）；子目录 AGENTS.md 随 read 工具 JIT 附加；`rules.mdx` 说明 OpenCode **不**自动解析 `@file` 引用，建议用 `instructions` glob 或在 AGENTS.md 里教模型按需 read。大小上限：未找到。

##### ⑥ 换项目
- 全局 `~/.config/opencode/AGENTS.md` 跟着用户；项目 AGENTS.md 在仓库里随 clone 走。
- 因项目键是远程 URL，换 clone/worktree 仍是同一 project（会话列表共享，directory 列区分）；无自动记忆所以不存在串味问题。
- 官方对 monorepo 的讨论：`instructions: ["packages/*/AGENTS.md"]`（`rules.mdx`「For monorepos … using opencode.json with glob patterns is more maintainable」）；worktree：`project_directory.type='git_worktree'`、`project.sandboxes`（代码层面），文档未专门讨论（未找到）。

##### 一句话结论
作用域键 = **项目：git 远程 URL 的 hash（无远程则根提交 sha；非 git = global）；规则文件：cwd 向上到 git root；用户：`~/.config/opencode`**。记忆 vs 规则文件 = **无内置记忆**（只有第三方 supermemory 插件）vs `AGENTS.md`/`CLAUDE.md` + `instructions` 配置；会话在 `~/.local/share/opencode/opencode.db`（SQLite）。

##### 未证实 / 未找到
- 内置自动记忆：未找到。
- AGENTS.md 大小上限：未找到。
- 官方关于 worktree/多 clone 与会话共享的文档说明：未找到（仅代码可推）。
- `agent/` 与 `agents/` 两种目录名的文档一致性：文档只写 `agents/`，代码两者都扫。

#### 4.6.7 pi（badlogic/pi-mono → earendil-works/pi，`@earendil-works/pi-coding-agent`）
版本基线：main @ `311f0e02` (2026-10-06)，`packages/coding-agent/package.json` **1.0.4**（CHANGELOG「[1.0.4] - 2026-10-05」）；仓库 README 已指向 `earendil-works/pi`。核对文档：`packages/coding-agent/docs/{configuration,sessions,how-pi-works,security,extensions,settings,cli,session-format}.md`、`README.md`、`CHANGELOG.md`。核对源码：`src/config.ts`、`src/core/resource-loader.ts`、`src/core/session-manager.ts`、`src/core/system-prompt.ts`、`src/core/agent-session.ts`、`src/experimental/durable/README.md`；`examples/extensions/` 全目录列表。

本节源码与文档均为仓库内文件，以 commit `311f0e02` 为准。

##### ① 持久记忆
- **规则文件**：有。候选名 `AGENTS.override.md, AGENTS.md, AGENTS.MD, CLAUDE.md, CLAUDE.MD`（`resource-loader.ts:185`），同目录取第一个命中（`AGENTS.override.md` 只替换同目录的 AGENTS/CLAUDE）；`<agent-dir>/SYSTEM.md`（替换默认系统提示）、`APPEND_SYSTEM.md`（追加）；项目 `.pi/SYSTEM.md`、`.pi/APPEND_SYSTEM.md`（需项目信任，`resource-loader.ts:1209-1233`；同名时项目优先、不合并）。
- **记忆（模型自动积累）**：**未找到**。docs/src 里 `memory` 只命中 in-memory/heap；`examples/extensions/` 没有 memory/remember 类示例；`docs/extensions.md:220-232` 的「State」表把「Data outside one session」归为「External storage」，并提供 `pi.appendEntry()` 存「Durable data excluded from model context」（仅会话内）。README 明说 pi「skips features like sub-agents and plan mode. Ask Pi to build what you want, or install a package」——记忆也属于留给扩展的范畴。
- 会话：JSONL 树（`~/.pi/agent/sessions/…`），支持分支 `/tree` `/fork` `/clone`，离开分支可生成分支摘要；上下文接近上限时自动 compaction（插入摘要条目，不删原条目）（`docs/sessions.md`）。实验性 durable harness 用 SQLite（`experimental/durable/README.md`）。

##### ② 存在哪
`src/config.ts`【官方】：`getAgentDir()` :605-611 → `~/.pi/agent`（`PI_CODING_AGENT_DIR` 覆盖；`CONFIG_DIR_NAME='.pi'` :581）；`getSessionsDir()` :649-651 → `~/.pi/agent/sessions`；`prompts/`、`skills/`、`extensions/`、`themes/`、`settings.json`、`auth.json`、`models.json`、`mcp.json`、`AGENTS.md`、`SYSTEM.md`、`APPEND_SYSTEM.md` 都在 `<agent-dir>` 下（`docs/configuration.md` 表）。
- 会话目录：`~/.pi/agent/sessions/--<cwd 去掉首个分隔符、/ \ : 换成 ->--/`（`session-manager.ts:589-594 getDefaultSessionDirPath`），文件 `<ISO 时间戳去掉:.>_<sessionId>.jsonl`（:1079-1080）；可用 `--session-dir` / `PI_CODING_AGENT_SESSION_DIR` / 设置 `sessionDir` 改（CLI 优先；`sessionDir` 在信任判定**之前**读取，`docs/security.md:31`）。
- durable 实验：`~/.pi/agent/experimental/durable-sessions/<cwd-hash>/<session>/session.sqlite`。

##### ③ 作用域键
- **规则文件：目录祖先链**（`resource-loader.ts:232-266 loadProjectContextFiles`）：先 `<agent-dir>` 的上下文文件（用户全局），再从 **cwd 一路向上到文件系统根**（没有 git root 天花板），祖先在前、cwd 在后；`findShadowedContextFile` :214-227 处理「嵌套 linked worktree」：worktree 自己的 AGENTS.md 会遮蔽主仓库同名文件，避免同一仓库上下文加载两次。文档：「A context file applies whenever Pi runs in its directory or anywhere below it」（`configuration.md`「Context files」）。
- **会话：精确 cwd**（`resolvePath` 后的绝对路径），会话头记录 `cwd`（:1068）；`--continue` 取该 cwd 最近一个（:749-751）。
- **用户全局**：`~/.pi/agent`。
- **agent/身份**：pi 是单 agent，`agentDir` 是安装级目录；`APP_NAME`/`piConfig.configDir` 允许白牌成别的名字（:579-586），那是另一套目录而非「同一 agent 多身份」。未找到按身份分的记忆。

##### ④ 谁写
人写。`/init` 不存在于本包（未找到）；模型若修改 AGENTS.md 只是普通文件编辑。

##### ⑤ 进上下文
每次请求：上下文文件渲染成 system prompt 的一段「Project-specific instructions and guidelines:」+ 每个文件一个 `<project_instructions path="…">…</project_instructions>`（`system-prompt.ts:79-86`；`agent-session.ts:1711` 把 `getAgentsFiles()` 喂进去）。`--no-context-files` 关闭（`docs/cli.md:215`）。**上下文文件不受项目信任门控**（`security.md:57`「Context files … load regardless of project trust」）。大小上限：未找到。历史压缩靠 compaction（`docs/compaction.md`）。

##### ⑥ 换项目
- 跟着走：`~/.pi/agent/AGENTS.md`（或 CLAUDE.md）、`SYSTEM.md`/`APPEND_SYSTEM.md`、skills/prompts/extensions。
- 项目规则按目录祖先：父目录里放一个 AGENTS.md 会作用到所有子项目（没有 git 边界）；worktree 场景有专门的遮蔽逻辑（源码注释，docs 未见专门章节——未找到）。
- 会话按 cwd：换 worktree/clone 即换会话列表；无自动记忆，不存在串味。

##### 一句话结论
作用域键 = **规则文件：`~/.pi/agent` + cwd 的全部祖先目录（无 git 天花板，worktree 去重）；会话：精确 cwd 编码成目录名**。记忆 vs 规则文件 = **无内置记忆**（只有会话 JSONL + compaction 摘要，扩展可用 `pi.appendEntry()`/外部存储自建）vs `AGENTS.md`/`SYSTEM.md`/`APPEND_SYSTEM.md`。

##### 未证实 / 未找到
- 官方 memory 扩展/示例：未找到（`examples/extensions/` 无；WebSearch 也未见官方 memory 包）。
- 上下文文件大小上限：未找到。
- 官方文档对 worktree/monorepo 的专门讨论：未找到（只有源码 `findShadowedContextFile` 注释）。

---

## 5. 专用记忆系统与官方做法

### 5.1 MemGPT 与 Letta：连续身份是有力先例，但要区分代际

**【论文】MemGPT 的核心是管理有限上下文，而不是规定「按什么项目分租户」。** 工作上下文包含用户事实与 agent persona，recall 保存消息，archival 保存可检索外部资料；模型通过函数自行搬运、编辑、检索，运行时负责消息日志、告警和队列压缩。其评估是长文档和多会话对话，不能推出项目目录隔离的优劣 [PA1]。

**【官方】Letta v1 的基本访问对象是 agent 与 block。** 一个 agent 可以有零到多个 blocks；human/persona 是推荐用法和常见标签，任意 label 可表达其他用途。相同 `block_id` 能挂到多个 agent，也能卸载；标签本身不是 `project_id`。这使「个体 persona＋共享用户资料／领域知识」的组合成为直接支持的机制 [LE1][LE3]。

**【官方】当前 Letta 明确把记忆归属与会话分开。** 一名 agent 可拥有多个无限长 conversations，共用身份和 MemFS；CLI 按当前项目恢复最近 conversation，只是会话选择行为，不会把长期记忆改成项目所有。MemFS 由 agent 拥有，投射到当前机器，故「机器／cwd 只是记忆的挂载环境」是该产品明确采用的模式 [LE4][LE5]。

**【官方／版本限制】不要把旧 blocks 方案当成当前新项目推荐。** v1 shared-memory 页面明确标为 legacy，并建议新工作迁移到共享记忆 repositories。当前文档内部还存在路径表述差异：MemFS 概念页说根部文件常驻、旧 agent 用 `system/`；SDK memory 页仍说 `system/` 常驻。可确定的是「常驻核心＋按需文件」和「agent 所有」；本文不把某个具体目录布局说成所有版本统一默认 [LE3][LE4][LE6]。

**【源码】Letta Code 的公开提示模板把所有记忆归于 `agent_id`，同时区分 `conversation_id`，并用 `projects/...` 等引用帮助找相关记忆。** 这是身份所有权下组织多项目知识的直接例子；未找到根据当前项目名称自动屏蔽其他项目文件／小节的明确契约。其普通文件搜索默认也不依赖向量库 [LE7][LE4]。

**【官方工程经验】Letta 的 Draft 案例同时使用 agent-owned 与 project-owned skills**，agent 的程序性经验跟身份走，项目技能跟仓库走。这是明确两层所有权的实用案例；适用范围是 procedural memory／skills，不等于其所有事实记忆都按这两层保存 [LE8]。

### 5.2 Mem0：实体标签和检索过滤，不是固定树形目录

**【官方】Platform 给出四个业务维度：user、agent、app、run。** 持久用户信息通常用 `user_id`，agent 行为／persona 用 `agent_id`，部署／应用用 `app_id`，临时工单或会话用 `run_id`。没有自动识别代码目录的默认行为；代码项目可由应用映射为 metadata 或一个合适的业务维度 [ME1][ME2]。

**【官方】检索遗漏的字段意味着不约束，不意味着必须为空。** 只过滤 `user_id=alice` 会检索该用户下带有不同 app/run 的记录。反过来，增加 AND 条件会缩小结果集，不能指望它自动把 user-only 的通用偏好补回来；要读取「个人层＋项目层」，应用应明确组合查询／OR／归并。这是逻辑分层与自动继承的区别 [ME1]。

**【官方，当前版本关键变化】Platform 默认抽取路径会按发言者归属拆分。** 同一次 `add(user_id, agent_id)` 中，用户说的事实可只有 user_id，assistant 说的事实可只有 agent_id；app/run 则随记录保留。因而机械地在读取时 AND user＋agent 可能得到空集。`infer=False` 直接导入才可能保留两者同时赋值。不能把旧文章中「给两个 ID 就得到共同作用域」的解释直接用于当前 Platform [ME1]。

**【源码】OSS `Memory.add` 仍要求 user/agent/run 至少一个，其 metadata／过滤构造和 Platform 的发言者拆分不是同一个合同。** 所读 `main.py` 展示显式校验、过滤器构造、`infer` 参数及 search；因此 OSS 与 Platform 分列，避免把托管 API 特性套给本地库 [ME3]。

**【官方】写入流程由应用决定什么时候 `add`，LLM 决定抽取什么；调用者也能直接导入。** 检索后是否放入 prompt 仍是应用责任。当前文档描述 additive extraction、去重、实体与时间信号；纠正／删除有显式操作，Platform 另有 Dream：Supersede/Merge 自动运行，Synthesis 需开启且只处理纯 user scope；旧事实标为 superseded 后默认仍可能被读到，需 `latest_only=true` 才只取当前事实。**【论文】2025 Mem0 论文描述的却是 ADD/UPDATE/DELETE/NOOP 更新循环。** 两者应按版本分别引用，不能拿论文当当前 SDK 精确行为说明 [ME2][ME4][PA2]。

**【论文】Mem0 在 LoCoMo 上比较抽取记忆、图记忆、RAG 和完整历史**；作者报告相对其 OpenAI baseline 的 judge 指标提升，以及相对 full-context 的时延／token 节省。这支持「精选持久记忆可优于反复灌全文」的研究方向，**没有比较 user/agent/project 三种隔离键，也没有比较一个 Markdown 与多个项目文件**；其 OpenAI baseline 还是特定年代、特定灌入方法，不代表当前 OpenAI 全产品（[PA2] §3–4）。

Qwen Code 内置的外部记忆就是 Mem0：MCP `external-context` 的 `context_search`，默认只读、不自动召回，scope 是 user/repository，可配 `scope.userId/appId/agentId` 跨 worktree 复用（§4.2.1）。

### 5.3 Zep / Graphiti：用户长期图与线程上下文分离

**【官方】Zep 的默认用户记忆是跨线程整合。** UserID 可用应用用户 ID；多个 threads 的消息都进入同一用户图。`thread.get_user_context` 返回整个用户图中与最近线程消息相关的部分，不限定为该 thread 曾写入的内容。用户摘要是稳定底座，线程是当前相关性的线索 [ZE1]。

**【官方】领域／项目知识还可以用独立 `graph_id`。** 当前 Zep Context Graph 可表示客户、项目、组织或产品；search 一次选 user_id 或 graph_id，不能把两个随意同时塞入同一调用。合成个人与项目上下文由应用安排。检索结合语义、BM25、可选图邻域及重排，且搜索前应完成授权 [ZE2][ZE3]。

**【源码】Graphiti 的隔离原语是 `group_id`，检索可传多个 `group_ids`。** 默认分区在多数后端是空字符串，FalkorDB 为 `_`；这不是自动 per-user。调用者可以给 group 赋用户／项目含义，但仍要自己保证写入、实体合并、搜索走对 scope。代码的多 group 参数也不是自动父子继承或完整 ACL [GR1][GR2]。

**【论文】Zep 的「层级」还有另一种含义**：episodes→实体／关系→communities，以及事实的有效／失效时间。它解决从原文到知识、随时间更新和检索效率，不能等同于 user→project 权限层级。论文的 DMR、LongMemEval 测试也没有直接检验代码项目作用域 [PA4]。

### 5.4 LangMem：namespace 是应用设计空间

**【官方】LangMem 提供动态 namespace tuple**，例如 `("memories", "{user_id}")`，运行时从配置补值，agent 工具无需自己猜用户 ID。同样可以按组织、agent、业务领域组合。教程出现全局 `("memories",)` 与 user-specific 两种写法，均是示例配置；未找到「库默认自动按当前项目隔离」的行为 [LM2][LM3]。

**【官方】短期 thread/checkpointer 与长期 Store 不同。** 前者留住该会话状态，后者可跨 thread。长期内容既可是一份 profile，也可是一组独立 facts／episodes，另有 procedural prompt learning；工具能在对话热路径写入，后台 manager 能在交互之后抽取与归并 [LM1][LM3][LM4]。

**【推断】这最容易表达「身份层＋项目层＋任务层」，但系统并未替产品决定如何合并。** namespace 选型还必须配套：查几个 scope、父层是否总读、冲突谁优先、项目经验何时提升到全局。只把 namespace 写成多段 tuple 不会自动解决这些问题 [LM1][LM2]。

### 5.5 Cognee：dataset 与真实存储隔离要分别看

**【源码】核心 SDK 的 `add` 默认为 `main_dataset`；不传 user 时使用默认用户。** 显式 user＋dataset 可以表达个人、团队或领域知识，但没有强制的「项目」定义 [CG1]。

**【官方仓库文档】当前 MCP 又有不同默认：根据连接客户端生成 dataset**，例如 Claude Code、Cursor 分开，避免客户端间无意共享。可以显式给 dataset_name 覆盖，也可关闭该 agent-scoping，让客户端都默认用 `main_dataset`。这里的「agent」是 MCP 客户端来源，不应当直接理解为 iota 的命名 bot 身份 [CG2]。

**【官方仓库文档】`ENABLE_BACKEND_ACCESS_CONTROL=true` 当前默认使 user＋dataset 拥有独立图／向量后端。** 关闭后各 dataset 共享后端，即使顶层点做了过滤，`GRAPH_COMPLETION` 的关联遍历仍可能取到其他 dataset 节点。该文档还明确说明切换模式不迁移旧数据，旧内容会因为存储路径不同而不可见。这是「有 dataset 字段 ≠ 所有检索阶段都隔离」的直接工程证据 [CG2]。

**【官方】MCP `remember` 无 session_id 时写长期图，有 session_id 时走快速 session cache；`recall` 可先找 cache 再找长期图。** 这是短长期分层。应用／agent 触发 ingest，Cognee 做结构化处理；是否长久保存不能仅看模型有没有记忆工具，还要看 session 模式与后台桥接设置 [CG2]。

### 5.6 A-MEM：研究的是组织与演化，不是多租户作用域

**【论文】A-MEM 借鉴 Zettelkasten**，将记忆形成带描述、关键词、标签、时间、链接的笔记，新记忆能更新旧笔记的上下文和关系。检索相似笔记并利用链接，重点在自组织和长期问答 [PA3]。

**【源码】公开 `AgenticMemorySystem` 的构造器没有 user_id／agent_id／project_id／namespace 参数**，默认用名为 `memories` 的 Chroma collection 和 `self.memories` 字典；初始化甚至尝试 reset collection。因此不能把每个对象天然当成可靠隔离且持久的生产 namespace。其标签可以分类，但未找到标签到访问边界的实现契约；生产多租户或项目隔离需要外层设计。这里是对所读库的判断，不是说 A-MEM 算法不能扩展 [AMM1]。

### 5.7 Anthropic：Memory Tool 不替应用选择所有者

**【官方】Memory Tool 的 `/memories` 是虚拟前缀，后端由应用映射到用户目录、数据库等存储。** Claude 发起文件操作，应用执行；以后会话接上同一个 store 才会延续。目录可以分层，但没有自动 user、agent、project 默认键 [AN1]。

**【官方】它采用先看目录、按需读写的策略；工程博客也把 structured note-taking 与 compaction 分开。** 笔记让 agent 跨上下文恢复重要状态，压缩让当前对话继续。官方例子包含长期软件项目与游戏，但没有证明某个本地项目键更优，也没有规定所有信息进入一个全局文件 [AN1][AN2]。

Claude Code（同属 Anthropic）在产品层做了明确选择：主会话按 git 仓库、子代理按 agent 名 × 所选 scope（§4.1.1）。

### 5.8 OpenAI：会话状态、应用记忆和 Codex 本地记忆是三件事

**【官方 Cookbook】个性化示例用应用自己的用户 state，内含 profile、global notes、session notes。** 模型工具记录候选，运行后去重归并；冲突按当前用户输入 → session overrides → global defaults。这里的 global 是该状态对象的长期默认，不代表所有用户共享。它说明「身份连续＋临时上下文覆盖」的模式，但没有给通用项目 ID 或生产多租户存储合同 [OA1]。

**【官方】当前 Sandbox Agents 的 memory 与 Session 明确分开。** 默认 artifacts 在 workspace 的 `memories/`，summary 引导读取 `MEMORY.md` 和 rollout summaries；session 关闭后提取、归并。可配置不同 agent 的隔离 layout，复用需保留目录／snapshot／持久挂载。运行记录分组依次看显式 conversation ID、SDK session ID、run-group ID、生成的 run ID；live sandbox ID 不是记忆 conversation ID。故不要把 session_id 当成所有长期事实的唯一键 [OA2]。

**【官方＋源码】Codex 本地 Memories 是另一个实际产品模式**：在 Codex home 下维护共享记忆，默认关闭，后台处理合格的空闲 chats。归并模板把一个 `MEMORY.md` 组织为 Task Groups，并要求 scope、cwd/reuse 边界；细节通过关键词和 rollout evidence 找回。它是「共享手册＋内部适用范围」的强先例，但文档未宣称每轮由宿主按项目做确定性正文裁剪，也不是每个命名 bot 一个独立身份库 [CX1][CX2]。完整机制（两阶段流水线、`memory_summary.md` 注入、线程级 `memory_mode`）见 §4.3.1。ChatGPT 记忆是与 Codex 本地记忆分开的另一套（账号键，§4.3.1）。

---

## 6. 证据有多强：学术与工程取舍

### 6.1 三种「分层」应分别讨论

| 分层轴 | 常见切法 | 解决的问题 | 不自动解决的问题 | 证据 |
|---|---|---|---|---|
| 上下文驻留 | 核心常驻／按需检索／完整档案 | 有限 token、连续性、找回细节 | 谁可以看哪个项目 | MemGPT、Letta、Codex、Anthropic [PA1][LE4][CX2][AN2] |
| 知识形态 | semantic facts/profile、episodic experiences、procedural skills | 当前事实、经历与做法的组织方式不同 | 用户／项目隔离 | LangMem、A-MEM、Zep [LM1][PA3][PA4] |
| 业务与权限 | tenant/user/agent/project/thread | 归属、共享、适用范围和生命周期 | 内容正确性、召回充分性 | Mem0、Zep、Cognee、Copilot [ME1][ZE3][CG2][GH1] |

**【推断】iota 的 L0/L1/L2 在设计上按第一条轴分层（目前实现了 L0 会话档案与 L1 `MEMORY.md`，L2 `notes/` 与 `recall` 尚未实现），Project 小节在第三条轴上分层。** 是否拆项目文件不会取代核心／档案／检索分层，也不要求把永续 bot 会话拆成项目会话 [IO1]。

### 6.2 作用域过粗的代价

- **错误泛化。** 把「这个仓库用 Terraform」「本次工单联系某人」写成该用户的普遍习惯，会在无关项目出现。Copilot 用户已有这样的具体报告；这说明写入分类也是边界的一部分，不能只靠读取过滤补救 [FR5]。
- **上下文竞争。** 无关内容会消耗注意力和预算；Anthropic 的工程文强调精简高信号上下文，AGENTS.md 实验也显示多塞约束可能增加成本、降低完成率。后者并不是自动记忆项目键实验 [AN2][PA6]。
- **更大的污染传播范围。** MemoryGraft 在 MetaGPT DataInterpreter/GPT-4o 上展示被污染的「成功经验」跨后续任务持续被召回。它支持持久经验可能带来持久错误，但没有证明按项目分文件就能防住：错误仍可能位于同一项目或被提升到身份层 [PA7]。
- **分类过滤可能被绕过。** Cognee 明说共享图后端的遍历可能跨 dataset；Cursor 多根 workspace 曾把每个根规则都全局加载。物理分文件、metadata 字段、UI 上分项目均不是充分证据，应看整条数据流 [CG2][FR4]。
- **设计上就跨项目可见的产品**：Codex、Kiro CLI 全局 agent 知识库、Kiro Web、OpenHands Cloud、Goose 全局记忆、Devin Memory（§7.3）。这些是已上线的选择，产品文档没有把它们当缺陷。

### 6.3 作用域过细的代价

- **地址变动造成可见性丢失。** 按绝对路径识别时，改名／移动目录会留下旧记忆；worktree 独立键会让临时工作树的经验不回到主仓库。已有具体公开报告，见 §7 [FR1][FR2]。产品调研的源码核对显示，Gemini、OpenHands、Goose、Cline、Roo、Continue、pi 都按路径键（§2.2）。
- **共享语境碎片化。** 用户偏好若在每项目重复，会产生不一致、重复纠正与更新成本。Zep 刻意跨 thread 合并，Letta 刻意让多个 conversation 共享 agent memory，Copilot 把用户偏好提到跨仓库层；这些是产品选择的依据，尚不是跨项目身份保真度的统一定量实验 [ZE1][LE5][GH1]。Claude Code 只有项目层，`type: user` 的偏好不跟到别的仓库（§4.1.1）。
- **跨项目工作流难找齐。** 一项任务若涉及前端、后端、部署三个仓库，只查当前 cwd 项目会漏掉依赖。LangMem 可组合 namespace、Graphiti 可搜多个 groups、Zep 可另外查询领域图，说明其 API 为显式组合留了空间；具体漏召回率，本次未找到直接的多仓库记忆隔离对照实验 [LM2][GR1][ZE3]。产品侧，Amp 有多仓库 Project（2026-08-27），Aider FAQ 明说一次只能处理一个仓库（§4.6.5、§4.6.3）。
- **分支经验与仓库经验并不等价。** 共享所有 worktrees 可保留通用知识，但临时路径、未合并实现和某分支命令可能误用；完全隔离又失去共享。Copilot 用当前分支校验证据、Codex 模板要求注明 checkout/time-specific 边界，是比单一 repo 键更细的处理方式 [GH1][CX2][FR3]。Continue 的代码索引把 git 分支直接做进键（§4.6.4）。

### 6.4 实证证据的边界

| 材料 | 真正比较／验证了什么 | 不能用于证明什么 |
|---|---|---|
| MemGPT | 有限窗口下层级管理、多会话 persona 与事实保留 | bot-key 比 project-key 更好 [PA1] |
| Mem0 | LoCoMo 上抽取／图记忆与其选定基线的问答、成本、时延 | 当前产品通用最优；项目目录隔离收益 [PA2] |
| A-MEM | 自组织笔记、链接与演化对长期对话问答的作用 | 内建多租户隔离、生产存储稳定性 [PA3][AMM1] |
| Zep | 时间图在 DMR、LongMemEval 的表现 | 独立项目文件优于一个用户图 [PA4] |
| LongMemEval | 500 个问题，信息抽取、跨会话推理、时间、更新、拒答；索引／检索／读取设计 | 其「key」是检索索引表征，不是本题租户／项目作用域键 [PA5] |
| Evaluating AGENTS.md v1 | SWE-bench 与开发者维护规则的仓库任务；增加规则会增加探索／成本，效果依内容而变 | AGENTS.md 无价值，或项目记忆隔离无效 [PA6] |
| MemoryGraft | 被污染的经验跨任务持续召回 | 按项目分文件能防住污染 [PA7] |
| 产品文档、源码与 issues（第 4、7 节） | 已部署机制、具体失效现象与用户期望 | 大样本故障率、因果收益和当前版本必然仍有缺陷 |

**【论文】LongMemEval 尤其提醒不要把「细粒度」一概当好事**：其 round 级存储优于整 session，进一步压成事实会损失信息，虽有利于部分跨会话问题；事实扩展索引、时间范围限制又能提升检索。这里讨论的是**记忆条目粒度与召回方式**，不是项目 scope，不能偷换（[PA5] §5）。

**【检索结论】本次未找到直接控制其他变量、比较「同一个 coding agent 按身份全局保存／按项目独立保存／混合保存」的成熟实证研究，也未找到对 iota 精确文件裁剪方案的长期对照评估。** 已有材料足以说明失效机制和设计空间，尚不足以替 iota 选一个普遍最优解。

---

## 7. 公开失败记录与设计使然的跨项目可见

### 7.1 公开失败记录

| 记录 | 现象与边界 | 证据等级与状态 | 对作用域设计的启示 |
|---|---|---|---|
| Claude Code #61349，2026-05-22，报告版本 2.1.148 | 用户改名目录后创建新 memory 目录，旧 27 条记录未再加载；不是磁盘内容被删除 | 【报告】调研时 API 状态 closed/not_planned；不等于缺陷已修复，也没有本机复现 [FR1] | filesystem address 与稳定 project identity 不等价 |
| Claude Code #28037，2026-02-24，2.1.49 | worktree 下学到的知识只存 worktree 键；删除工作树后主 repo 不可见 | 【报告】closed/completed；当前官方文档已规定同 repo worktree 共享（CHANGELOG 2.1.63），故按历史问题引用 [FR2][CC1][CC9] | 临时执行目录通常不适合直接作为长期唯一键 |
| Claude Code #31008，2026-03-05 | 共享 repo identity 后，用户报告跳到其他 worktree 路径，混淆当前执行位置 | 【报告】closed/duplicate；报告者要求 worktree 隔离，与上一条诉求相反 [FR3] | 通用 repo 知识与 checkout-specific 状态应能区分 |
| Cursor 多根 workspace，2026-03-21 | 一个仓库的根 AGENTS.md 被用于另一个仓库；论坛回复确认根规则被全局加载，nested 行为不同 | 【报告＋维护方回复】后来自动关闭；未找到该帖提供当前修复验证 [FR4] | 目录上分开存，仍会因 loader 全局拼接而串味 |
| Copilot Discussion #201874，2026-07-14 | 某客户 Terraform 习惯、内部工单联系人信息被作为用户记忆带到无关 Obsidian repo | 【报告】open；未独立复现，不等于官方承认每个偏好都无条件注入 [FR5] | 双层架构仍可能在「项目事实误归个人偏好」处失败 |
| Hermes #34352，2026-05-29 | 运营者称单 profile 全局记忆在 DM／群聊等多场景之间共享，提出 tenant routing/隔离方案 | 【报告】（生产使用者）open；含方案推广，未作为普遍故障率或现行全部版本事实 [FR6] | agent 身份相同不代表所有受众应共享记忆；iota 单用户本地现状与该多租户风险不同 |
| Mem0 #3773，2025-11-19，SDK 0.1.98 | 明确给 agent_id 后查询仍无结果，报告称索引字段与 metadata 不一致 | 【报告】closed/completed；是旧版本过滤实现问题，不把它解释为当前文档默认归属拆分 [FR7] | 作用域键必须可用于真实检索，不能仅存进 JSON |
| Cognee 当前官方 MCP README | 关闭 backend access control 后图遍历能跨 dataset；切换存储模式后旧数据不自动迁移 | 【文档明确】不是推测 [CG2] | 检索闭包、后端隔离和迁移与「给 scope 起名字」同样重要 |

### 7.2 失败的四类

**【推断】失败可以分成四类，解决方法不相同**：①身份解析失败（改名／worktree）；②写入归属错误（把局部经验升成个人事实）；③读取越界（loader、图遍历、过滤遗漏）；④内容陈旧／污染（旧路径、错误经验）。独立文件主要帮助组织和独立生命周期，不自动消除后面三类 [FR1][FR2][FR3][FR4][FR5][FR6][FR7][CG2][PA7]。

### 7.3 设计使然的跨项目可见（产品调研的源码与文档事实）

以下不是 bug 报告，而是产品自己的设计；列在这里，是因为它们在效果上与 7.1 的「读取越界」相同：

- Codex 把所有项目的 `### <project scope>` 分组一起注入每个新线程（§4.3.1）。
- Kiro CLI 全局 agent 的 `/knowledge` 知识库跨项目共用，文档把它当作设计，没有按项目隔离的选项（§4.4.1）。
- Kiro Web 记忆明确「across all your repositories」（§4.4.1）。
- OpenHands Cloud 一个用户只有一份 `memory_context`，被写到每个会话的「项目层」路径，仓库 A 学到的项目知识会出现在仓库 B 的会话里（代码事实；官方文档未找到说明，§4.2.3）。
- Goose 全局记忆无条件进所有目录的 system prompt（§4.2.4）。
- Devin Memory 跨仓库，换仓库时对主题文件的过滤规则未证实（§4.3.2）。
- Qwen Code 的记忆按 git root、会话按 cwd，在子目录启动时两者不是同一个 `<key>`（§4.2.1）。

---

## 8. 「一份记忆＋按项目裁剪」有没有先例？

先明确「一份」的含义：**一个身份拥有的逻辑记忆库**与**一个 Markdown 文件**不同；「裁剪」又可能指确定性 scope 过滤、语义相关性召回、或模型自己看索引选文件。下面不把它们混为一个成功案例。

| 先例 | 与 iota 相同的部分 | 不同之处 | 证据强度 |
|---|---|---|---|
| Codex 本地 Memories | 一个共享手册内按 task/project/cwd 组织，记录适用范围；有常驻摘要和按需证据 | 默认所有者是本地 Codex profile（OS 用户）；模型检索与 scope 文本，不是宿主按项目标题硬裁剪；整份摘要注入每个新线程 | **最接近单文件组织形式的已实现先例**；源码＋文档，未找到精确裁剪效果实验 [CX1][CX2]，§4.3.1 |
| Letta MemFS / Letta Code | 一个 agent 持续拥有其身份与多项目经验，工作位置可变；核心＋按需文件 | 多文件 Git 仓库；常驻与按需按记忆结构区分，未找到当前项目自动屏蔽规则 | **最接近身份所有权的产品先例** [LE4][LE5][LE7] |
| Zep User Graph | 一份用户长期知识横跨会话，每次只返回与当前线程相关的内容 | 图数据库，按语义／时间等选取；thread 相关性不是项目精确隔离 | **最清楚的共享全集→当前上下文视图先例** [ZE1][ZE3] |
| Mem0 filters / LangMem namespaces | 一个服务／存储系统可含不同 scope，读取时指定范围；可以构造 identity＋current-project | 应用仍需制定写入标签和联合读取；不要求只存一个文件，也不默认项目裁剪 | **通用机制先例**，不能冒充与 iota 相同的开箱产品 [ME1][LM2] |
| Devin Memory | 一份用户（×组织）级记忆，`MEMORY.md` 常驻，内部按项目分文件夹 | 项目文件夹是 agent 自己的约定，换仓库时是否只加载相关文件夹**未证实** | 已上线产品，过滤规则未公开 [DV10]，§4.3.2 |
| Claude Code auto memory | `MEMORY.md` 索引＋主题按需披露 | 默认是每 repository 自己一份；「一份索引」不是跨项目全集 | **只支持渐进读取，不支持声称与 iota 同构** [CC1]，§4.1.1 |

**精确答案：未找到。** 在两份调研核查的成熟系统与 19 个 coding agent 中，没有找到明确采用「一个命名 bot 的单一 Markdown，保存多个 `Project` 小节，宿主每轮按当前项目精确选择正文、其他项目只列标题」，并公开验证其长期效果的案例。**广义答案：有充分的相邻机制先例**——身份所有权下的一份长期库、文件内标注适用范围、按当前上下文形成较小视图，都已存在。可将它们作为模式依据，不能将「有相似实现」写成「iota 方案已被证明成功」[LE4][CX2][ZE1][ME1][LM2]。

**【推断】因此，与现状最相关的问题未必是「一个文件合不合理」，而是这份文件的 scope 是否被所有访问路径一致解释。** 当前 iota 的裁剪针对 L1 注入；`User`、`Open threads`、前言和未知小节全局可见；archive 仍是同一个永续会话，压缩摘要还可能保留之前的项目事实。按设计，L2 的 `recall` 会搜索 MEMORY.md 与 `notes/`、`recall(source: "archive")` 会回读档案——**这两条路径目前尚未实现**，它们绕过按项目裁剪的风险是未来的、不是已存在的。它是**默认上下文的相关性控制**，不能描述成跨项目不可访问的硬隔离。此外，项目键本身在 worktree 下就已碎片化：同一仓库的不同 worktree 落到不同的 `Project:` 小节，彼此的项目事实默认互不注入（见开头「iota 现状」）。这既与「bot 连续身份」目标相容，也意味着只拆 L1 文件不足以实现全面隔离（[IO1] §3.4–3.6）[IO2]。

---

## 9. project key 候选

**存储布局与 project key 应独立决定。** 同一个 Markdown 也能用稳定 ID，项目分目录也可能继续用不稳定路径。下表左三列来自系统与模式调研的工程推理，右列「已有产品」来自产品调研的源码与文档核对 [FR1][FR2][FR3][LM2]：

| project key 候选 | 好处 | 代价／风险 | 已有产品（出处） |
|---|---|---|---|
| 当前 basename（iota 现状） | 人可读；父目录迁移不改 basename 时仍能匹配 | 两个无关同名目录碰撞；项目改名后旧节不自动匹配；不能表达多仓库共同项目；**worktree 碎片化**：iota 的项目根遇到 `.git` 文件就停，linked worktree 取的是 worktree 自己的目录名（常是分支名），同一仓库的每个 worktree 各成一个项目（`src/agents/mod.rs:31-44`、`src/repl/run.rs:291`） | Gemini CLI 用 basename slug，但用 `projects.json` 注册绝对路径、冲突时加 `-1` 后缀并以 `.project_root` 校验归属，不会误共享（§4.2.2） |
| 规范化绝对路径／路径 hash | 实现直观，区分同名目录 | 移动、改名、不同机器、独立 clone、临时 worktree 会碎片化；hash 只隐藏路径，不使身份稳定 | OpenHands SDK、Goose、Trae（编码未证实）、Cline、Roo、Continue、pi、Kiro 工作区；Claude Code 的目录名也由路径派生（§2.2）；改名失联见 [FR1] |
| Git common root／repo identity | 可让同 repo worktree 共享 | 本地 root 仍会移动；repo 并不等于业务项目；分支状态会混入共用经验 | Claude Code（「derived from the git repository」，具体取 common dir 还是主工作树路径未证实，§4.1.1）；对照：Qwen、Aider 取 worktree 自己的 git root，worktree 各自一份（§4.2.1、§4.6.3） |
| remote URL 或其规范化值 | 跨 checkout／机器较容易归一 | fork、迁仓、多个 remote、SSH/HTTPS 别名、无 remote 项目都需规则 | OpenCode：`Hash("git-remote:" + host 小写 + 去 .git 的路径)`，无远程退到缓存 id、再退到根提交 sha，非 git 为 `global`（§4.6.6）；Amp thread 按仓库 URL，可配 Git Remote Aliases（§4.6.5）；Copilot 用服务端 owner/repo，fork 是否共享未找到（§4.2.6） |
| 沿第一父提交走到的根提交（root commit sha） | 由仓库历史决定，不依赖路径与 remote：改名、搬家、worktree、另一个 clone 都认作同一项目；无需网络与配置 | 非 git 目录没有；新建仓库首个提交之前没有；从同一模板/历史 fork 出的不同项目会共享同一根提交；改写历史（换根、filter-repo）后变化；多根历史要靠「第一父」规则选定唯一一个 | OpenCode 在没有远程时退到首个根提交 sha，旧版用 `git rev-list --max-parents=0 --all` 的根提交（§4.6.6）；本文未找到以它为首选键的产品 |
| 显式稳定 project ID＋路径／repo aliases | 能表示改名、多个 checkout、多仓库同项目；显示名可单独改变 | 需建立、迁移、别名与冲突管理；ID 文件被复制时仍须定义是否代表同项目 | 部分接近：OpenCode 把项目 id 缓存进 `<git common dir>/opencode`；Amp 的多仓库 Project（2026-08-27）；Gemini 的 `projects.json` 注册表（§4.6.6、§4.6.5、§4.2.2） |

这张键表是工程推理，**没有论文证明某一行普遍最优**。其中 basename 的碰撞／改名行为可从 iota 当前匹配契约直接推得，worktree 碎片化可从项目根查找代码直接推得（合并时补充，两份原稿都没有写）；路径键风险则已有真实报告（[IO1] §3.2）[FR1][FR2]。

---

## 10. 三个选项与代价（不作推荐）

以下均是依据前述产品与失败记录列出的**设计选项**，本次没有实施，也不在三者之间做推荐。

### 10.1 三个选项共同需要分开的决定

- **存储布局**（一个文件 / 按项目分目录 / 身份层＋项目层）与 **project key**（§9）是两件事。
- **所有权**：「某 bot 对项目的经验」与「团队的项目记忆」不同（后者更像 Qwen 的 team 层或 AGENTS.md）。
- **写入路由**：一条记忆由谁决定落哪层（模型按规则 / 写入参数 / 条目自带 scope，§3.4）。
- **读取视图**：哪些层每轮常驻、哪些按需、跨项目 recall 是否允许（§2.1 读取视图）。
- **会话**：拆存储不必拆永续 bot 会话；但旧会话档案与压缩摘要里的跨项目信息不会因拆文件而消失（§8）。

### 10.2 ① 保持现状：按 bot 一份，项目小节决定默认注入

**保留内容**：bot 继续拥有唯一长期记忆与永续会话；身份、通用偏好和项目事实共存，L1 按当前项目裁剪，L2 显式检索。无迁移成本，人能一次浏览、纠正所有记忆 [IO1]。

**代价／风险**：全部项目共用 8 KiB L1 配额，一个项目的增长会挤压其他项目的常驻摘要；basename 同名／改名问题仍在，linked worktree 还会被认成独立项目（worktree 碎片化）；模型若漏写 section，默认 User 会扩大适用范围；Open threads 及压缩摘要仍可能带入其他项目，L2 的 `recall`／`notes/` 落地后检索路径也会（目前尚未实现）。它不适合被承诺为客户／租户安全隔离（[IO1] §3.2–3.6）[IO2]。

**他人经验**：Letta 支持 agent 所有权，Codex 支持统一手册内部标 scope，Zep 支持全集按上下文取视图，所以方向并不孤立；但 Copilot 的误归类报告说明「把项目事实写到全局层」是关键风险，Cognee／Cursor 说明过滤必须覆盖实际读路径 [LE4][CX2][ZE1][FR5][CG2][FR4]。产品侧，19 个 coding agent 里没有同款（§3.4、§8）。相邻先例支持该选项的合理性，不能证明当前大小、键或裁剪方式已足够。

**可单独考虑、不要求换布局的增强**：项目 ID 与显示名分离；记忆条目／小节明确适用边界；默认写入 scope 更显式；（L2 落地后）跨项目 recall 返回来源；把项目临时事项与真正跨项目事项区分。它们增加格式与工具复杂度，是否需要应由实际失效案例决定 [ME1][CX2][FR1]。

### 10.3 ② 改成按项目分目录：让项目成为主要知识容器

**可选形态**：例如 bot 目录内的 `projects/<project-id>/MEMORY.md` 与 notes；也可让项目知识住在 repo 内、被多个 bot 共享。**这两者所有权不同**，应先确定是「某 bot 对项目的经验」还是「团队的项目记忆」。会话仍可保持 bot 永续，不必因为拆文件就拆 session [LM2][CC2][GH1]。

**收益**：项目可独立预算、归档、备份、清理；项目知识更容易审阅与共享；读取白名单可以按容器构造。Claude Code 主 agent auto memory 与 Cascade workspace memory 表明这种默认容易对应编码工作环境 [CC1][WS1]；产品调研里有自动记忆的 coding agent 多数以项目为键（§3.1）。

**代价／风险**：现有小节迁移和同名冲突需要处理；如果仍用路径，Claude Code 式改名／worktree 失联风险会被引入或保留；如果把全部记忆都放进项目目录，身份偏好会复制且漂移；多仓库任务需显式联合读取。如果保留全局身份文件来解决这个问题，架构就已靠近③。独立目录也不能消除旧会话／摘要中的跨项目信息 [FR1][FR2][ZE1][LE5][IO1]。

**他人经验**：Claude Code 曾从 worktree 碎片化走向 repo 共享，但又有共享后路径混淆的报告，说明「目录分得更细」不是单调改进；Qwen 选了相反方向（worktree 各自一份，仓库级约定放 team 层）。Copilot 校验当前分支事实的做法提示：项目容器之外还需识别事实是否只适用于某次 checkout [FR2][FR3][CC1][GH1]，§4.2.1。

### 10.4 ③ 混合：身份层＋项目层分开存，运行时组合

**可选形态**：bot identity／稳定偏好独立保存，项目事实放 `projects/<project-id>/`；每轮读取「该 bot 的核心＋当前涉及项目的内容＋必要的会话状态」。项目层可以是 bot 私有，也可团队共享；若以后需要跨 bot 的真实用户偏好，user 与 bot persona 还可再拆，但不必预设都要建 [GH1][LE3][LE5][LM2]。

**收益**：身份不因切项目丢失；项目知识有独立预算、生命周期和共享边界；多仓库任务可以选择多个 project scope。Copilot Memory 是真实的个人偏好＋仓库事实双层先例，Letta 的 agent-owned/project-owned skills 是另一个程序性知识先例 [GH1][LE8]；Qwen、Gemini、OpenHands、Goose、Trae 是「项目＋用户」双层的 coding agent 先例，但它们的上层是用户，不是命名 bot（§3.4）。

**代价／风险**：写入路由、经验升级、冲突优先级、跨层去重和删除语义更复杂。模型可能把客户局部偏好错误提升为 bot/user 通用事实，造成与 Copilot 报告相同的污染。只分目录却所有层全文加载，也可能比当前裁剪更吵；用户更正一条错误时还需知道修哪一层 [FR5][ME1][AN2]。

**他人经验**：双层不是免疫方案；要明确定义什么能提升到身份层、项目范围如何标识、检索如何组合、旧事实如何验证。OpenAI Cookbook 的 session-over-global 优先级可作生命周期参考，Copilot 的证据校验可作项目事实参考，LangMem 的 namespace 可作存储原语参考，Gemini 的「一条事实只能落一层、两可就问用户」可作写入路由参考；它们并未替 iota 决定具体合并规则 [OA1][GH1][LM2]，§4.2.2。

### 10.5 对三个选项可提出的同一组验证问题

**【建议的评估维度，尚未执行】** 可用相同 bot 和相同任务轨迹检查：切到项目 B 后是否误用 A 的工具链；A/B 同名是否串味；改名、移动、worktree 删除后是否找回；跨三个仓库任务是否召回完整；用户通用偏好能否延续；局部反例是否错误推翻全局偏好；旧分支经验是否重新验证；一个项目大量写入是否挤掉另一个项目的关键记忆。分别统计错域应用、该记忆未召回、人工重复纠正、注入量与迁移／维护成本 [FR1][FR2][FR3][FR4][FR5][GH1][PA5]。

这些问题允许评估①②③，而无需先宣布哪个架构正确。**当前证据足以支持三个选项各自的合理性与已知代价；尚不足以替所有者拍板。**

---

## 11. 两份调研的分歧与各自边界

两份调研在同一产品上的结论高度一致：Claude Code 按 git 仓库（worktree 共享、无用户全局自动记忆）、Codex 用户全局且默认关、Copilot 仓库事实＋用户偏好两层、Cursor 自动记忆已下线、Windsurf 记忆只剩 legacy Cascade、Claude Code 子代理 `user/project/local` 三选一、AGENTS.md 没有统一的用户层路径。以下是说法不一致或边界不同、合并时**没有抹平**的地方。

**分歧 1：Codex 归并模板，两份读的不是同一个版本、描述的不是同一个文件。**
- 产品调研：读 openai/codex `57d57df` 的 `consolidation_v2.md`，描述 **`memory_summary.md`** 按 `### <project scope>` / `#### <YYYY-MM-DD>` 分组、「injected at the beginning of every new session for the same user」（§4.3.1）。
- 模式调研：读 `551bd409` 的 `consolidation.md`（v1），描述一个 **`MEMORY.md`** 手册按 Task Groups 组织、要求写 scope、cwd/reuse 与 checkout/time-specific 边界 [CX2]（§5.8）。
- 两者不矛盾到互斥（v2 同样产出 `MEMORY.md`，模板要求也都含项目范围），但「项目分组在哪个文件、以什么标题」随版本不同。引用 Codex 做先例时应说明版本；`[memories] version = "v2"` 是当前可选变体（§4.3.1）。

**分歧 2：Copilot 的用户偏好是「会话开始注入」还是「按相关性使用」。**
- 产品调研：会话开始时检索该仓库（+该用户）的记忆并「included in the prompt」，长会话每 30 分钟刷新；**搜索/加权检索列为未来工作**（引工程博客与 CLI changelog，§4.2.6）。
- 模式调研：用户偏好「跨仓库**按相关性**使用」（原稿总表，引概念页 [GH1]）。
- 两者引用的来源不同（工程博客 2026-01-15 [GH5] 对概念页 [GH1]），可能反映的是不同时间点或不同入口（CLI 对 cloud agent）的行为。本文没有第三方来源判定哪个是现行行为，**未证实**。

**分歧 3：「有没有按 agent 身份存记忆的先例」——结论范围不同。**
- 产品调研：「20 家里没有一家把主会话的自动记忆按『命名 bot / persona』存」（§3.3）。
- 模式调研：Letta（含 Letta Code，也是 coding agent）把全部记忆归 `agent_id`，是「最接近身份所有权的产品先例」（§5.1、§8）。
- 产品调研的清单不含 Letta Code，所以它的结论只对清单内成立；合并后的表述是「产品调研覆盖的 19 个 coding agent 里没有，清单之外的 Letta Code 有」。

**分歧 4：Cursor rules 文档的地址不同。** 产品调研引用 `cursor.com/docs/context/rules` [CU1]，模式调研引用 `cursor.com/docs/rules` [CU2]，两者都能打开，内容是现行 rules 文档；两份对规则层级的描述一致，只是模式调研多了「nested AGENTS.md 旧文档写 planned、现已支持」这一条（§4.5.1）。

**产品调研内部的计数不一致（不是两份之间的分歧）**：正文写「以下 19 节」，专题与取舍两节写「20 家」。按产品小节数是 19（Codex 节含 ChatGPT，Kiro 节含 Kiro Web 与 Crew），本文统一写 19；原稿的「20 家」可能把 ChatGPT 单独计入，未核实。另外原稿取舍一节说「项目 + 用户」双层有五家（不含 Copilot），专题一节说六家（含 Copilot）；本文按六家写，并注明 Copilot 的「项目」是 GitHub 仓库身份（§3.4）。

**各自的盲区**：产品调研不看专用记忆系统、论文与公开 issue；模式调研对 Gemini CLI、Qwen Code、OpenHands、Goose、Trae、Devin、Kiro 等产品没有单独核查。上面「项目＋用户双层最常见」「iota 的方案没有同款」等结论，只在各自覆盖的范围内成立。

**两份都漏掉、合并时补上的 iota 现状事实**：① linked worktree 下的 worktree 碎片化（项目键取 worktree 自己的目录名，见开头「iota 现状」与 §9）；② 两份都把 `notes/` 与 `recall` 当成现有机制推理，实际它们是 [IO1] 设计中尚未实现的 L2，相关推理处已逐一标注。

---

## 附 A：未证实 / 未找到汇总

**产品（详见第 4 节各产品「未证实 / 未找到」）**：

- Claude Code auto memory 的 git 键具体取什么（common dir / 主工作树路径）、同仓库多 clone 是否共享、memory extraction/recall 的机制、`CLAUDE_MEMORY_STORES`：闭源，未证实（§4.1.1）。
- Gemini CLI 文档仍写 `<project_hash>`，代码已改为 basename slug（`projectRegistry.ts`），以代码为准；slug 机制进入的版本号未找到（§4.2.2）。
- Goose 文档说「loads all saved memories at the start of a session」，代码只对全局记忆成立，本地记忆不预载（§4.2.4）。
- Windsurf 规则上限：2025-05 文档「每文件 6,000 + 总 12,000」，2025-09 起只剩「每文件 12,000」，现行文档「全局 6,000 / 工作区每文件 12,000、无总量上限」（§4.5.2）。
- Copilot Memory 首次公告是 2025-12-19，不是常说的 2025-11；注入数量/大小上限未找到；CLI cross-session memory 与 `session-store.db` 的关系、`vote_memory` 语义未证实；fork 是否共享 repository facts 未找到；用户偏好的注入方式见 §11 分歧 2（§4.2.6）。
- Devin Memory 的发布只有文档示例日期（2026-10-05）与媒体报道，cognition.com/blog 未找到博文；换仓库时对主题文件的过滤规则、大小上限、CLI 是否生效：未找到（§4.3.2）。
- OpenHands Cloud 的 per-user memory 只有代码与设置文案，文档未说明；跨仓库行为是代码推断（§4.2.3）。
- Cursor 旧 Memories 的物理存储位置（本地/云端）、User Rules 是否账号同步：未证实（§4.5.1）。
- Trae `{project_path}` 的编码、记忆注入方式与上限、上线版本：未证实 / 未找到（§4.2.5）。
- Kiro CLI 闭源：知识库目录键 `{name}_{hash}` 只在 2026-04 的 Q CLI 源码里证实；Kiro 文档只给 `my-custom-agent_<alphanumeric-code>`（§4.4.1）。
- Roo / Aider / Continue 未完整克隆，「无 memory 模块」的结论基于 GitHub code search 与目录列表（§4.6.2–§4.6.4）。
- Codex `codex debug clear-memories` 的用法、项目级 `.codex/config.toml` 能否覆盖 `[features] memories`、`external_agent_memory_import`：未证实；Codex cloud 的跨任务记忆、ChatGPT 记忆的注入/大小上限：未找到（§4.3.1）。
- 同一仓库多个 clone：没有任何一家官方讨论（§2.2）。

**系统与模式**：

- Letta 当前文档内部对常驻目录（根部文件 vs `system/`）的表述不一致；未找到 Letta Code 按当前项目自动屏蔽其他项目文件的契约（§5.1）。
- LangMem：未找到「库默认自动按当前项目隔离」的行为（§5.4）。
- A-MEM：未找到内建项目隔离，也未找到标签到访问边界的实现契约（§5.6）。
- Cursor 多根 workspace 串味：帖子自动关闭，未找到修复验证 [FR4]；Copilot #201874、Hermes #34352 仍 open、未独立复现 [FR5][FR6]（§7.1）。
- 直接比较「按身份全局 / 按项目独立 / 混合」的对照实验：未找到；iota 精确裁剪方案的长期评估：未找到（§6.4、§8）。
- 跨三个仓库任务的漏召回率：未找到直接的多仓库记忆隔离对照实验（§6.3）。

## 附 B：来源列表

共 146 条编号来源：外链 143 条，本仓库文件 3 条。另有大量 `文件:行号` 形式的源码引用，按产品写在第 4 节正文里，仓库与 commit 见版本基线，不单独编号。

合并时（2026-10-07）对 143 条外链逐一做了 HTTP 探测（跟随重定向）：141 条返回 200；[CH1]、[CH2]（help.openai.com）对直接抓取返回 403，与原稿「直抓 403，经浏览器读取」的说明一致。探测只说明链接可达，不代表本次重新核对了页面内容。

「出处」列：`landscape` = 产品横向调研原稿，`patterns` = 系统与模式调研原稿（后附其原编号），「两份共用」= 两份原稿都引用了同一页面，已合并为一个编号。


**iota**

| 编号 | 类型 | 内容 | 出处 |
|---|---|---|---|
| [IO1] | 【本仓库】 | iota bot 模式设计文档（现状依据） | patterns I1 |
| [IO2] | 【本仓库】 | iota 记忆快照实现 | patterns I2 |
| [IO3] | 【本仓库】 | 前次调研：Bot 模式调研 | landscape |

**论文**

| 编号 | 类型 | 内容 | 出处 |
|---|---|---|---|
| [PA1] | 【论文】 | MemGPT | patterns P1 |
| [PA2] | 【论文】 | Mem0（2025） | patterns P2 |
| [PA3] | 【论文】 | A-MEM | patterns P3 |
| [PA4] | 【论文】 | Zep 时间知识图 | patterns P4 |
| [PA5] | 【论文】 | LongMemEval | patterns P5 |
| [PA6] | 【论文】 | Evaluating AGENTS.md v1 | patterns P6 |
| [PA7] | 【论文】 | MemoryGraft | patterns P7 |

**Letta**

| 编号 | 类型 | 内容 | 出处 |
|---|---|---|---|
| [LE1] | 【官方】 | Letta v1 memory blocks | patterns L1 |
| [LE2] | 【官方】 | Letta v1 archival memory | patterns L2 |
| [LE3] | 【官方】 | Letta v1 shared memory（标 legacy） | patterns L3 |
| [LE4] | 【官方】 | Letta MemFS 概念页 | patterns L4 |
| [LE5] | 【官方】 | Letta conversations | patterns L5 |
| [LE6] | 【官方】 | Letta agent SDK memory | patterns L6 |
| [LE7] | 【源码】 | Letta Code 提示模板（commit 4b028fab） | patterns L7 |
| [LE8] | 【官方】 | Letta 工程博客：Draft 案例 | patterns L8 |

**Mem0**

| 编号 | 类型 | 内容 | 出处 |
|---|---|---|---|
| [ME1] | 【官方】 | Mem0 实体作用域 | patterns M1 |
| [ME2] | 【官方】 | Mem0 工作流程 | patterns M2 |
| [ME3] | 【源码】 | Mem0 OSS `main.py`（commit c93420c4） | patterns M3 |
| [ME4] | 【官方】 | Mem0 Platform Dream | patterns M4 |

**Zep**

| 编号 | 类型 | 内容 | 出处 |
|---|---|---|---|
| [ZE1] | 【官方】 | Zep 用户与用户图 | patterns Z1 |
| [ZE2] | 【官方】 | Zep 图概览 | patterns Z2 |
| [ZE3] | 【官方】 | Zep 图搜索 | patterns Z3 |

**Graphiti**

| 编号 | 类型 | 内容 | 出处 |
|---|---|---|---|
| [GR1] | 【源码】 | Graphiti `graphiti.py`（commit 689de295） | patterns G1 |
| [GR2] | 【源码】 | Graphiti `helpers.py`（默认值） | patterns G2 |

**LangMem**

| 编号 | 类型 | 内容 | 出处 |
|---|---|---|---|
| [LM1] | 【官方】 | LangMem 概念指南 | patterns N1 |
| [LM2] | 【官方】 | LangMem 动态 namespace | patterns N2 |
| [LM3] | 【官方】 | LangMem 后台抽取示例 | patterns N3 |
| [LM4] | 【官方】 | LangMem memory tools | patterns N4 |

**Cognee**

| 编号 | 类型 | 内容 | 出处 |
|---|---|---|---|
| [CG1] | 【源码】 | Cognee SDK `add.py`（commit b32d8afc） | patterns C1 |
| [CG2] | 【官方】 | Cognee MCP README（官方仓库文档） | patterns C2 |

**A-MEM**

| 编号 | 类型 | 内容 | 出处 |
|---|---|---|---|
| [AMM1] | 【源码】 | A-MEM `memory_system.py`（commit ceffb860） | patterns A1 |

**Anthropic**

| 编号 | 类型 | 内容 | 出处 |
|---|---|---|---|
| [AN1] | 【官方】 | Anthropic Memory Tool API 文档 | patterns H1 |
| [AN2] | 【官方】 | Anthropic 工程博客：context engineering | patterns H4 |

**Claude Code**

| 编号 | 类型 | 内容 | 出处 |
|---|---|---|---|
| [CC1] | 【官方】 | Claude Code memory（CLAUDE.md 层级、AGENTS.md、rules、auto memory） | 两份共用（patterns H2） |
| [CC2] | 【官方】 | Claude Code 子代理 `memory:` 字段 | 两份共用（patterns H3） |
| [CC3] | 【官方】 | Claude Code sessions（`<project>` 目录命名、`CLAUDE_CODE_PROJECT_DIR_NAME`） | landscape |
| [CC4] | 【官方】 | Claude Code `.claude` 目录 | landscape |
| [CC5] | 【官方】 | Claude Code 环境变量 | landscape |
| [CC6] | 【官方】 | Claude Code worktrees | landscape |
| [CC7] | 【官方】 | Claude Code context window（加载顺序） | landscape |
| [CC8] | 【官方】 | Claude Projects（云端 project memory） | landscape |
| [CC9] | 【官方】 | Claude Code CHANGELOG（顶部 2.1.291） | landscape |

**OpenAI**

| 编号 | 类型 | 内容 | 出处 |
|---|---|---|---|
| [OA1] | 【官方】 | OpenAI Cookbook：个性化 | patterns O1 |
| [OA2] | 【官方】 | OpenAI Sandbox Agents memory | patterns O2 |

**Codex**

| 编号 | 类型 | 内容 | 出处 |
|---|---|---|---|
| [CX1] | 【官方】 | Codex Memories 文档 | 两份共用（patterns O3） |
| [CX2] | 【源码】 | Codex 归并模板 v1（commit 551bd409） | patterns O4 |
| [CX3] | 【官方】 | Codex AGENTS.md | 两份共用（patterns R2） |
| [CX4] | 【官方】 | Codex 配置参考 | landscape |
| [CX5] | 【官方】 | Codex cloud environment | landscape |

**ChatGPT**

| 编号 | 类型 | 内容 | 出处 |
|---|---|---|---|
| [CH1] | 【官方】 | ChatGPT Memory 帮助页 | landscape |
| [CH2] | 【官方】 | ChatGPT Projects 帮助页 | landscape |

**Cursor**

| 编号 | 类型 | 内容 | 出处 |
|---|---|---|---|
| [CU1] | 【官方】 | Cursor rules（landscape 引用的地址） | landscape |
| [CU2] | 【官方】 | Cursor rules（patterns 引用的地址） | patterns R3 |
| [CU3] | 【官方】 | Cursor 2.1 changelog（2025-11-21） | landscape |
| [CU4] | 【官方】 | Cursor 1.0 changelog（2025-06-04） | landscape |
| [CU5] | 【官方】 | Cursor 论坛 staff 回复：Memories 已移除 | landscape |
| [CU6] | 【官方】 | Cursor changelog（2026-09-10 Projects 条目） | landscape |
| [CU7] | 【官方】 | Cursor Projects | landscape |
| [CU8] | 【官方】 | Cursor 论坛 staff：Privacy Mode 下不可用 | landscape |
| [CU9] | 【官方】 | Cursor 论坛 staff：Memories 与训练数据 | landscape |

**Windsurf**

| 编号 | 类型 | 内容 | 出处 |
|---|---|---|---|
| [WS1] | 【官方】 | Devin Desktop（原 Windsurf）Cascade memories | 两份共用（patterns R5） |
| [WS2] | 【官方】 | Devin Desktop Cascade AGENTS.md | patterns R6 |
| [WS3] | 【官方】 | Devin Local agent | landscape |
| [WS4] | 【媒体】 | Windsurf 更名 Devin Desktop 报道 | landscape |
| [WS5] | 【媒体】 | Windsurf 更名 Devin Desktop 报道 | landscape |
| [WS6] | 【非官方】 | Windsurf 记忆键分析（作者未直接查看目录） | landscape |

**Cline**

| 编号 | 类型 | 内容 | 出处 |
|---|---|---|---|
| [CL1] | 【官方】 | Cline rules | landscape |
| [CL2] | 【官方】 | Cline Memory Bank（prompting 模式） | landscape |
| [CL3] | 【官方】 | Cline task management | landscape |
| [CL4] | 【官方】 | Cline 多根工作区 | landscape |

**Roo Code**

| 编号 | 类型 | 内容 | 出处 |
|---|---|---|---|
| [RC1] | 【官方】 | Roo custom instructions（页面标注 2026-05-15） | landscape |
| [RC2] | 【官方】 | Roo custom modes | landscape |
| [RC3] | 【官方】 | Roo codebase indexing | landscape |
| [RC4] | 【官方】 | Roo context condensing | landscape |

**Aider**

| 编号 | 类型 | 内容 | 出处 |
|---|---|---|---|
| [AI1] | 【官方】 | Aider conventions | landscape |
| [AI2] | 【官方】 | Aider options | landscape |
| [AI3] | 【官方】 | Aider `.aider.conf.yml` | landscape |
| [AI4] | 【官方】 | Aider repo map | landscape |
| [AI5] | 【官方】 | Aider FAQ | landscape |
| [AI6] | 【官方】 | Aider commands | landscape |

**Continue**

| 编号 | 类型 | 内容 | 出处 |
|---|---|---|---|
| [CN1] | 【官方】 | Continue rules deep dive | landscape |
| [CN2] | 【官方】 | Continue config reference | landscape |
| [CN3] | 【官方】 | Continue `@Codebase`（deprecated） | landscape |
| [CN4] | 【官方】 | Continue FAQ | landscape |

**Gemini CLI**

| 编号 | 类型 | 内容 | 出处 |
|---|---|---|---|
| [GM1] | 【官方】 | Gemini CLI PR #25716（四层记忆） | landscape |
| [GM2] | 【官方】 | Gemini CLI changelog 索引 | landscape |

**Copilot**

| 编号 | 类型 | 内容 | 出处 |
|---|---|---|---|
| [GH1] | 【官方】 | Copilot Memory 概念页（public preview） | 两份共用（patterns K1） |
| [GH2] | 【官方】 | Copilot CLI 配置目录参考 | landscape |
| [GH3] | 【官方】 | Copilot CLI custom instructions | landscape |
| [GH4] | 【官方】 | Copilot response customization | landscape |
| [GH5] | 【官方】 | GitHub 工程博客 2026-01-15：agentic memory | landscape |
| [GH6] | 【官方】 | VS Code custom instructions（nested AGENTS.md） | patterns R7 |

**Kiro**

| 编号 | 类型 | 内容 | 出处 |
|---|---|---|---|
| [KR1] | 【官方】 | Kiro steering（2026-10-06） | landscape |
| [KR2] | 【官方】 | Kiro custom agents | landscape |
| [KR3] | 【官方】 | Kiro agent 配置参考 | landscape |
| [KR4] | 【官方】 | Kiro configuration | landscape |
| [KR5] | 【官方】 | Kiro CLI context | landscape |
| [KR6] | 【官方】 | Kiro CLI `/knowledge` | landscape |
| [KR7] | 【官方】 | Kiro slash commands | landscape |
| [KR8] | 【官方】 | 从 Q Developer 迁移 | landscape |
| [KR9] | 【官方】 | Kiro Web Memory | landscape |
| [KR10] | 【官方】 | Kiro Crew memory | landscape |
| [KR11] | 【官方】 | Q Developer IDE 插件终止支持 | landscape |
| [KR12] | 【官方】 | Q Developer 项目 rules | landscape |

**Devin**

| 编号 | 类型 | 内容 | 出处 |
|---|---|---|---|
| [DV1] | 【官方】 | Devin AGENTS.md | landscape |
| [DV2] | 【官方】 | Devin CLI rules | landscape |
| [DV3] | 【官方】 | Devin skills | landscape |
| [DV4] | 【官方】 | Devin playbooks（创建） | landscape |
| [DV5] | 【官方】 | Devin playbooks（使用） | landscape |
| [DV6] | 【官方】 | Devin blueprints | landscape |
| [DV7] | 【官方】 | Devin Knowledge（deprecated） | landscape |
| [DV8] | 【官方】 | Devin knowledge onboarding | landscape |
| [DV9] | 【官方】 | Devin Release Notes 2026 | landscape |
| [DV10] | 【官方】 | Devin Memory and Dreaming | landscape |
| [DV11] | 【官方】 | Agent Memory Repo 开放规范 | landscape |
| [DV12] | 【官方】 | Devin environment | landscape |
| [DV13] | 【官方】 | Devin workspaces（monorepo） | landscape |

**OpenHands**

| 编号 | 类型 | 内容 | 出处 |
|---|---|---|---|
| [OH1] | 【官方】 | OpenHands skills | landscape |
| [OH2] | 【官方】 | OpenHands 仓库定制 | landscape |
| [OH3] | 【官方】 | OpenHands 组织 skills | landscape |
| [OH4] | 【官方】 | OpenHands SDK persistent memory | landscape |
| [OH5] | 【官方】 | OpenHands context condenser | landscape |

**Goose**

| 编号 | 类型 | 内容 | 出处 |
|---|---|---|---|
| [GS1] | 【官方】 | Goose Memory extension | landscape |

**Amp**

| 编号 | 类型 | 内容 | 出处 |
|---|---|---|---|
| [AP1] | 【官方】 | Amp plugins | landscape |
| [AP2] | 【官方】 | Amp news：Agentic Review（2025-12-18） | landscape |
| [AP3] | 【官方】 | Amp AGENTS.md | 两份共用（patterns R4） |
| [AP4] | 【官方】 | Amp skills | landscape |
| [AP5] | 【官方】 | Amp news：Handoff（2025-10-23） | landscape |
| [AP6] | 【官方】 | Amp context management 指南 | landscape |
| [AP7] | 【官方】 | Amp thread 数据安全 | landscape |

**Trae**

| 编号 | 类型 | 内容 | 出处 |
|---|---|---|---|
| [TR1] | 【官方】 | Trae memories（国际版） | landscape |
| [TR2] | 【官方】 | Trae rules（国际版） | landscape |
| [TR3] | 【官方】 | Trae 记忆（国内版） | landscape |
| [TR4] | 【官方】 | Trae 规则（国内版） | landscape |
| [TR5] | 【官方】 | Trae 中文社区 2026-03-22 帖 | landscape |

**AGENTS.md**

| 编号 | 类型 | 内容 | 出处 |
|---|---|---|---|
| [AG1] | 【官方】 | AGENTS.md 约定官网 | patterns R1 |

**失败记录**

| 编号 | 类型 | 内容 | 出处 |
|---|---|---|---|
| [FR1] | 【报告】 | Claude Code #61349：改名目录后旧记忆不再加载 | patterns F1 |
| [FR2] | 【报告】 | Claude Code #28037：worktree 键导致知识丢失 | patterns F2 |
| [FR3] | 【报告】 | Claude Code #31008：共享 repo 身份后路径混淆 | patterns F3 |
| [FR4] | 【报告】 | Cursor 多根 workspace AGENTS.md 串味 | patterns F4 |
| [FR5] | 【报告】 | Copilot Discussion #201874：项目事实误入用户记忆 | patterns F5 |
| [FR6] | 【报告】 | Hermes #34352：单 profile 记忆跨场景共享 | patterns F6 |
| [FR7] | 【报告】 | Mem0 #3773：agent_id 过滤查不到 | patterns F7 |

[IO1]: bot-mode.md
[IO2]: ../../src/agents/memory/snapshot.rs
[IO3]: bot-mode-research.md
[PA1]: https://arxiv.org/html/2310.08560
[PA2]: https://arxiv.org/html/2504.19413v1
[PA3]: https://arxiv.org/html/2502.12110v1
[PA4]: https://arxiv.org/html/2501.13956v1
[PA5]: https://arxiv.org/html/2410.10813v1
[PA6]: https://arxiv.org/html/2602.11988v1
[PA7]: https://arxiv.org/html/2512.16962v1
[LE1]: https://docs.letta.com/v1-sdk/memory/memory-blocks
[LE2]: https://docs.letta.com/v1-sdk/memory/archival-memory
[LE3]: https://docs.letta.com/v1-sdk/memory/shared-memory
[LE4]: https://docs.letta.com/concepts/memfs/index.md
[LE5]: https://docs.letta.com/concepts/conversations/index.md
[LE6]: https://docs.letta.com/agent-sdk/memory/index.md
[LE7]: https://github.com/letta-ai/letta-code/blob/4b028fab07c69edaac2ddb4f7b9a43573ff20d81/src/agent/prompts/letta.md
[LE8]: https://www.letta.com/blog/building-draft/
[ME1]: https://docs.mem0.ai/platform/features/entity-scoped-memory
[ME2]: https://docs.mem0.ai/core-concepts/how-it-works
[ME3]: https://github.com/mem0ai/mem0/blob/c93420c49a6b14c3d446bdb156d96811908fd90a/mem0/memory/main.py
[ME4]: https://docs.mem0.ai/platform/features/dream
[ZE1]: https://help.getzep.com/users-and-user-graphs
[ZE2]: https://help.getzep.com/graph-overview
[ZE3]: https://help.getzep.com/searching-the-graph
[GR1]: https://github.com/getzep/graphiti/blob/689de295c209631405c00e19e0af4f9735142f13/graphiti_core/graphiti.py
[GR2]: https://github.com/getzep/graphiti/blob/689de295c209631405c00e19e0af4f9735142f13/graphiti_core/helpers.py
[LM1]: https://langchain-ai.github.io/langmem/concepts/conceptual_guide/
[LM2]: https://langchain-ai.github.io/langmem/guides/dynamically_configure_namespaces/
[LM3]: https://langchain-ai.github.io/langmem/background_quickstart/
[LM4]: https://langchain-ai.github.io/langmem/guides/memory_tools/
[CG1]: https://github.com/topoteretes/cognee/blob/b32d8afc59e1064d9291b9828a8a147be9cc8bab/cognee/api/v1/add/add.py
[CG2]: https://github.com/topoteretes/cognee/blob/b32d8afc59e1064d9291b9828a8a147be9cc8bab/cognee-mcp/README.md
[AMM1]: https://github.com/agiresearch/A-mem/blob/ceffb860f0712bbae97b184d440df62bc910ca8d/agentic_memory/memory_system.py
[AN1]: https://platform.claude.com/docs/en/agents-and-tools/tool-use/memory-tool
[AN2]: https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents
[CC1]: https://code.claude.com/docs/en/memory
[CC2]: https://code.claude.com/docs/en/sub-agents#enable-persistent-memory
[CC3]: https://code.claude.com/docs/en/sessions
[CC4]: https://code.claude.com/docs/en/claude-directory
[CC5]: https://code.claude.com/docs/en/env-vars
[CC6]: https://code.claude.com/docs/en/worktrees
[CC7]: https://code.claude.com/docs/en/context-window
[CC8]: https://code.claude.com/docs/en/claude-projects
[CC9]: https://github.com/anthropics/claude-code/blob/main/CHANGELOG.md
[OA1]: https://developers.openai.com/cookbook/examples/agents_sdk/context_personalization
[OA2]: https://developers.openai.com/api/docs/guides/agents/sandboxes#persist-memory-across-runs
[CX1]: https://learn.chatgpt.com/docs/customization/memories
[CX2]: https://github.com/openai/codex/blob/551bd409ebf03fc6ea0dcad0915368d8a493f012/codex-rs/memories/write/templates/memories/consolidation.md
[CX3]: https://learn.chatgpt.com/docs/agent-configuration/agents-md
[CX4]: https://learn.chatgpt.com/docs/config-file/config-reference
[CX5]: https://learn.chatgpt.com/docs/environments/cloud-environment
[CH1]: https://help.openai.com/en/articles/8590148-memory-in-chatgpt
[CH2]: https://help.openai.com/en/articles/10169521-projects-in-chatgpt
[CU1]: https://cursor.com/docs/context/rules
[CU2]: https://cursor.com/docs/rules
[CU3]: https://cursor.com/changelog/2-1
[CU4]: https://cursor.com/changelog/1-0
[CU5]: https://forum.cursor.com/t/are-my-memories-gone/144057
[CU6]: https://cursor.com/changelog
[CU7]: https://cursor.com/docs/agent/projects
[CU8]: https://forum.cursor.com/t/cant-turn-on-memories-disabled/100701
[CU9]: https://forum.cursor.com/t/0-51-memories-feature/98509
[WS1]: https://docs.devin.ai/desktop/cascade/memories
[WS2]: https://docs.devin.ai/desktop/cascade/agents-md
[WS3]: https://docs.devin.ai/desktop/devin-local
[WS4]: https://apidog.com/blog/whats-new-in-devin-2026/
[WS5]: https://www.digitalapplied.com/blog/windsurf-becomes-devin-desktop-ide-migration-2026
[WS6]: https://baalda.com/blog/windsurf-second-brain
[CL1]: https://docs.cline.bot/features/cline-rules
[CL2]: https://docs.cline.bot/prompting/cline-memory-bank
[CL3]: https://docs.cline.bot/core-workflows/task-management
[CL4]: https://docs.cline.bot/features/multiroot-workspace
[RC1]: https://roocodeinc.github.io/Roo-Code/features/custom-instructions/
[RC2]: https://roocodeinc.github.io/Roo-Code/features/custom-modes/
[RC3]: https://roocodeinc.github.io/Roo-Code/features/codebase-indexing/
[RC4]: https://roocodeinc.github.io/Roo-Code/features/intelligent-context-condensing/
[AI1]: https://aider.chat/docs/usage/conventions.html
[AI2]: https://aider.chat/docs/config/options.html
[AI3]: https://aider.chat/docs/config/aider_conf.html
[AI4]: https://aider.chat/docs/repomap.html
[AI5]: https://aider.chat/docs/faq.html
[AI6]: https://aider.chat/docs/usage/commands.html
[CN1]: https://docs.continue.dev/customize/deep-dives/rules
[CN2]: https://docs.continue.dev/reference
[CN3]: https://docs.continue.dev/reference/deprecated-codebase
[CN4]: https://docs.continue.dev/faqs
[GM1]: https://github.com/google-gemini/gemini-cli/pull/25716
[GM2]: https://github.com/google-gemini/gemini-cli/blob/main/docs/changelogs/index.md
[GH1]: https://docs.github.com/en/copilot/concepts/agents/copilot-memory
[GH2]: https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-config-dir-reference
[GH3]: https://docs.github.com/en/copilot/how-tos/copilot-cli/customize-copilot/add-custom-instructions
[GH4]: https://docs.github.com/en/copilot/concepts/prompting/response-customization
[GH5]: https://github.blog/ai-and-ml/github-copilot/building-an-agentic-memory-system-for-github-copilot/
[GH6]: https://code.visualstudio.com/docs/agent-customization/custom-instructions
[KR1]: https://kiro.dev/docs/steering/
[KR2]: https://kiro.dev/docs/custom-agents/
[KR3]: https://kiro.dev/docs/custom-agents/configuration-reference/
[KR4]: https://kiro.dev/docs/configuration/
[KR5]: https://kiro.dev/docs/cli/chat/context/
[KR6]: https://kiro.dev/docs/cli/experimental/knowledge-management/
[KR7]: https://kiro.dev/docs/reference/slash-commands/
[KR8]: https://kiro.dev/docs/upgrade-guides/migrating-from-q/
[KR9]: https://kiro.dev/docs/web/memory/
[KR10]: https://kiro.dev/docs/crew/features/memory/
[KR11]: https://docs.aws.amazon.com/amazonq/latest/qdeveloper-ug/q-developer-ide-end-of-support.html
[KR12]: https://docs.aws.amazon.com/amazonq/latest/qdeveloper-ug/context-project-rules.html
[DV1]: https://docs.devin.ai/onboard-devin/agents-md
[DV2]: https://docs.devin.ai/cli/extensibility/rules
[DV3]: https://docs.devin.ai/product-guides/skills
[DV4]: https://docs.devin.ai/product-guides/creating-playbooks
[DV5]: https://docs.devin.ai/product-guides/using-playbooks
[DV6]: https://docs.devin.ai/onboard-devin/environment/blueprints
[DV7]: https://docs.devin.ai/product-guides/knowledge
[DV8]: https://docs.devin.ai/onboard-devin/knowledge-onboarding
[DV9]: https://docs.devin.ai/release-notes/2026
[DV10]: https://docs.devin.ai/product-guides/memory
[DV11]: https://github.com/AgentMemoryRepo/agentmemoryrepo
[DV12]: https://docs.devin.ai/onboard-devin/environment
[DV13]: https://docs.devin.ai/onboard-devin/environment/workspaces
[OH1]: https://docs.openhands.dev/overview/skills
[OH2]: https://docs.openhands.dev/openhands/usage/customization/repository
[OH3]: https://docs.openhands.dev/overview/skills/org
[OH4]: https://docs.openhands.dev/sdk/guides/persistent-memory
[OH5]: https://docs.openhands.dev/sdk/guides/context-condenser
[GS1]: https://goose-docs.ai/docs/mcp/memory-mcp
[AP1]: https://ampcode.com/docs/customize/plugins
[AP2]: https://ampcode.com/news/agentic-code-review
[AP3]: https://ampcode.com/docs/customize/agents-md
[AP4]: https://ampcode.com/docs/customize/skills
[AP5]: https://ampcode.com/news/handoff
[AP6]: https://ampcode.com/guides/context-management
[AP7]: https://ampcode.com/security#thread-data
[TR1]: https://docs.trae.ai/ide/memories
[TR2]: https://docs.trae.ai/ide/rules
[TR3]: https://docs.trae.cn/ide_memories
[TR4]: https://docs.trae.cn/ide_rules
[TR5]: https://forum.trae.cn/t/topic/2777
[AG1]: https://agents.md/
[FR1]: https://github.com/anthropics/claude-code/issues/61349
[FR2]: https://github.com/anthropics/claude-code/issues/28037
[FR3]: https://github.com/anthropics/claude-code/issues/31008
[FR4]: https://forum.cursor.com/t/agents-md-leaks-into-other-repositories-in-multi-repo-workspace/155477
[FR5]: https://github.com/orgs/community/discussions/201874
[FR6]: https://github.com/NousResearch/hermes-agent/issues/34352
[FR7]: https://github.com/mem0ai/mem0/issues/3773
