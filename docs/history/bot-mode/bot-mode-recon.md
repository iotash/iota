# bot 模式勘察：chat / agent 模式的实现边界与接缝

Status: **Recon**（只读勘察，2026-09-30，基于 `d46b085`）· 目的：为第三种「bot 模式」（常驻、不换上下文的持续对话）找接缝

坐标约定：`src/...:行号` 指本提交的源码；文档写 `文件 §节`。无法从代码直接确认的标 **待确认**。

---

## 0. 先纠正两个名词

- **代码里没有「chat 模式 / agent 模式」两个独立实现。** 只有两条**循环**：`-m` 的 headless 单轮（`src/headless/`）与无 `-m` 的交互 REPL（`src/repl/`）；「agent 模式」是一个**布尔开关**，叠加在两条循环之上。
- **agent 模式的唯一开关是 `agents.<name>.workspace: true`**（`src/config/agent.rs:40-44`），经 `src/cmd/resolve.rs:137` 变成 `RunSettings.agent_mode`，再在 `src/cmd/mod.rs:308-324` 变成 `AgentOptions { enabled: true, root, .. }`（结构体见 `src/headless/mod.rs:60-69`）。`docs/design/agent-mode.md §Switch` 里写的 `--agent` 旗标 / provider 级 `agent: true` 已过时（`src/config/strict.rs:85` 对旧写法报错）。

agent 开关打开后实际只改变三件事：
1. 每次发送叠加 AGENTS.md + skills 目录的易失覆盖层（§2.2）；
2. 自动挂上 `skills` 工具集（`load_skill`），并出现 `/skills` 命令（`src/repl/run.rs:353`、`:656-664`）；
3. 会话落到 `projects/<slug>/` 桶里，`/session`、`iota resume` 的列表只看本项目（§3）。

---

## 1. 入口与生命周期

### 1.1 命令分发

| 用户输入 | 解析结果 | 坐标 |
|---|---|---|
| `iota` | `Command::Run(RunCmd { agent: None, .. })` | `src/cmd/args.rs:70-76` |
| `iota run <agent>` | `Invocation::of_run` | `src/cmd/args.rs:420-427` |
| `iota resume [<id>]` | `Invocation::of_resume`，空 id = `Resume::Pick` | `src/cmd/args.rs:430-441` |
| 以上任意 + `-m` | 同上，`args.message = Some(..)` | `src/cmd/args.rs:316-323` |

`cmd::run`（`src/cmd/mod.rs:120-151`）把 `Run` / `Resume` 都交给同一个 `run_agent`（`src/cmd/mod.rs:169-249`）。它按固定顺序：`Config::load` → `resolve_run`（`:196`）→ `open_provider`（`:203`, `:252-275`）→ `assemble_tools`（`:205`, `:279-368`，这里决定 `AgentOptions` 与 `ToolEnv`、`Jobs`、harness 输入）→ 在 `:220` 分岔：

- **无 `-m`** → `interactive::run_interactive`（`src/cmd/interactive/mod.rs:205-373`）
- **有 `-m`** → `run_headless`（`src/cmd/mod.rs:392-553`）

`workspace: true` 不产生新的入口，只改变上面每条路径里的 `agent_mode` 分支。

### 1.2 headless（`-m`）：单轮即退出

| 步骤 | 坐标 |
|---|---|
| 只有 `iota resume <id> -m` 才打开会话；普通 `-m` **不建会话、不写盘** | `src/cmd/mod.rs:421-473` |
| 解析 id（agent 模式先查本项目桶）→ `store.resume` 得到 `(SessionWriter, Session)` | `:424-429` |
| Presenter 无 ANSI 回退、不发 ping | `:480` |
| 同步连 MCP、建 dispatcher | `:487-503` |
| `headless::once` → `run_once` | `:526-527`；`src/headless/once.rs:59`；`src/headless/run.rs:126-206` |
| **仅成功时**把本轮 delta 一次性 `append_messages` | `src/cmd/mod.rs:532-536` |
| `pres.close()`、`manager.close()`，进程结束 | `:540-544` |

`run_once` 自己不认识会话存储：它以 `req.history` 长度为水位线，返回水位线之后的 delta（`src/headless/run.rs:138-140`, `:202`），由 `cmd` 写盘（ARCHITECTURE §1.2 末段、§8.1）。

### 1.3 交互（REPL）：一个进程 = 一次对话循环

| 步骤 | 谁做 | 坐标 |
|---|---|---|
| 非 TTY 直接拒绝 | `run_interactive` | `src/cmd/interactive/mod.rs:241-243` |
| `ephemeral = --no-save \|\| (agent.no_save && !resume)` | 同上 | `:245` |
| `SessionStore::from_dirs`；agent 模式 `scope = Some(project root)` | 同上 | `:252-253` |
| MCP 后台连接（与 picker 重叠） | 同上 | `:270-280` |
| **创建 / 恢复会话**：`wire_session` | `wire_session` | `:413-577` |
| ├ resume：`store.resume` + 回放 meta 里的模型/参数 | | `:436-477` |
| ├ 新会话：`store.create(NewSession{..})`——**不碰磁盘**（懒创建） | | `:493-506`；`src/session/store.rs:206-240` |
| └ ephemeral：留一个工厂闭包给 `/save` 晚铸 | | `:509-534` |
| 组装 harness（此时才知道宿主） | | `:327` |
| 进入 `crate::repl::run` | | `:331-357` |
| 退出：`ui.close()` → 标题栈出栈 → `manager.close()` | | `:361-365` |

**谁写盘**：`Repl::persist_turn`（`src/repl/run.rs:207-221`）在**每轮成功结束后**把 `history[persisted..]` 一次性追加（调用点 `:786`；中断时 `:924`）。第一次追加触发 `SessionWriter::ensure_created`（`src/session/writer.rs:184`）真正建目录。另外两条写盘路径：压缩标记 `append_compaction`（`src/session/writer.rs:145-166`）与 meta 改写 `update_meta`（`:174`）。**轮内不落盘**：一轮进行中崩溃，这一轮全部丢失（工具副作用已发生）。

---

## 2. 一次 turn 的完整数据流（交互路径）

```
ui.read_input ─► 分发链(/file /model /session /compact …) ─► 回显 ❯
   run.rs:557        run.rs:589-669                           run.rs:675-679
 ─► refresh_overlay（stat AGENTS.md 链与 skills，变了才重读）─► 自动压缩询问 ─► history.push(user)
      run.rs:686 / 829-855                                   run.rs:691       run.rs:693-697
 ─► title_now（异步起名）─► TurnEngine::run（整轮重试）─► run_turn ─► tool_loop
      run.rs:700              turn/mod.rs:311-376          :252-283     turn/tools.rs:46-165
        每轮 round：compose_send_history（发送时副本）─► stream_round ─► assistant(+tool_calls) 入 history
                     tools.rs:66                        turn/mod.rs:440-455   tools.rs:115-135
                   ─► walk（执行/审批）─► steer.drain（排队消息与 Notice 在轮边界注入）─► 延迟挂载的 schema
                      tools.rs:140        tools.rs:147-152                              tools.rs:158-163
        无 tool_calls → TurnOutput 返回
 ─► 成功：拼 assistant 消息 ─► persist_turn ─► budget.update ─► pres Idle + Done ping
      run.rs:755-797           run.rs:786
    失败：history 截回用户消息之前、撤销标题（run.rs:732-754）；中断：三态表（run.rs:894-933）
```

### 2.1 历史组装与易失覆盖层

`compose_send_history(history, harness, overlay)`（`src/agents/mod.rs:316-363`）每次发送生成一个**副本**：`harness` + `<instructions>` 包裹的用户 system prompt（`history[0]`）+ `overlay`。内存与磁盘里的 history 只含用户自己的 system prompt（`docs/design/agent-mode.md §AGENTS.md`；`src/agents/harness.rs:5-9`）。

- **overlay**：`Overlay::content()` = AGENTS.md 链 + skills 目录（`src/agents/mod.rs:269-278`）。链从项目根走到 cwd，32 KiB 上限（`:24`, `:74`）。交互模式**每条消息**调 `Overlay::refresh` 做 mtime 探测（`:246-266`），同一轮的各 round 共用该快照（`src/repl/run.rs:686` → `TurnCtx.overlay`）。headless 只算一次（`src/headless/run.rs:147-153`）。
- **harness**：仅当 agent 配了 `tools:` 才非空（`src/agents/harness.rs:99-112`），其 `<environment>` 含 `date:`（`:124`，值来自 `src/cmd/assemble.rs:82` 的 `harness::today()`）。**harness 在启动时组装一次**（`src/cmd/interactive/mod.rs:327`，存进 `Conversation.harness`，`src/repl/state.rs:57-59`）——常驻进程跨日后 `date:` 会过期，这是 bot 模式的一个必改点。

### 2.2 持有「当前会话」状态的结构体

| 结构体 | 持有什么 | 坐标 |
|---|---|---|
| `Repl` | 三部分的组合 | `src/repl/run.rs:168-175` |
| `Conversation` | provider、dispatcher、**`history: Vec<Message>`**、待发附件、`ContextBudget`/`CtxMeter`、参数来源、harness、`Option<Overlay>`、`AgentOptions` | `src/repl/state.rs:33-67` |
| `SessionSlot` | `WriterSlot = Arc<Mutex<Option<SessionWriter>>>`、store、scope、`/save` 工厂、**`persisted` 水位线**、标题器 | `src/repl/state.rs:70-92` |
| `UiHandles` | 门面、transcript、Presenter、命令表、审批门、jobs、root cancel | `src/repl/state.rs:127-153` |
| `TurnCtx` / `Turn` | 一条消息范围内不变的句柄 / 一轮的 cancel 与流句柄 | `src/repl/turn/mod.rs:82-115` |
| `Steerer` | 本轮已注入的排队消息（重试时重放） | `src/repl/turn/steer.rs:19-23` |
| `SessionWriter` | 目录、meta、日志句柄、`conv_count`、累计 usage | `src/session/writer.rs:27-39` |

headless 侧没有等价结构：`run_once` 的 `messages` 是局部变量（`src/headless/run.rs:139`）。

---

## 3. 会话存储

### 3.1 目录布局与 project bucket

```
~/.iota/sessions/<ULID>/                         # 普通模式（flat）
~/.iota/sessions/projects/<slug>/<ULID>/         # agent 模式
    meta.json         # 整体改写（tmp + rename）
    messages.jsonl    # 只追加，每批一次 fsync
    attachments/<sha256>
    images/
```

- 根：`SessionStore::from_dirs` = `<app home>/sessions`（`src/session/store.rs:94-98`）。
- 桶：`create` 在 `project && !cwd.is_empty()` 时放 `projects/<slug(cwd)>/`（`:219-224`）；`cwd` 在 agent 模式下是项目根（`src/cmd/interactive/mod.rs:479-492`）。
- slug：清洗后的绝对路径，分隔符换 `-`，Windows 再折叠 `:*?"<>|`（`src/session/store.rs:115-123`）。
- 定位：`find_dir` 先 flat 再扫所有桶，以存在 `meta.json` 为准（`:128-136`）。
- 列表：`list(scope)` **模式隔离**（`:169-177`）；`list_all` 合并两种布局，只用于 id 解析（`:181-188`）；`resolve_id` 先查本模式，仅 `NoMatch` 才放宽（`:194-204`）。
- 文件名常量：`src/session/record.rs:12-16`；meta `src/session/meta.rs:17-19`。

### 3.2 `meta.json` 字段（`src/session/meta.rs:45-120`）

`v`、`id`、`created_at`、`updated_at`、`provider`、`model`（这 6 个与 `message_count` 恒输出）；可省略：`temperature`、`top_p`、`context_window`、`effort`、`image`、`aspect_ratio`、`image_size`、`negative_prompt`、`json_edits`、`base_url`、`cwd`、`title`、`agent`、`param_sources`；`extra`（`#[serde(flatten)]` 保留未知键，注释规定 **Rust 永不往里加自己的键**，`:116-118`）。每次 `write` 重盖 `updated_at`（`:133-140`）。

### 3.3 `messages.jsonl` 一行（`SessionRecord`，`src/session/record.rs:26-68`）

`role`（`system|user|assistant|tool|compaction`）、`content`、`reasoning`、`attachments[{filename,mime,data_ref}]`、`tool_calls[{id,name,arguments}]`、`tool_call_id`、`tool_call_name`、`is_error`、`interrupted`、**`notice`**（user 角色、宿主注入的通知，`:54-58`）、`raw{provider,blob}`、`compacted_through`（仅压缩标记）、`usage`。

### 3.4 resume 路径

| 入口 | 行为 | 坐标 |
|---|---|---|
| `iota resume <id>` | 前缀解析 → `store.resume` → 回放 meta → 历史进 `RunParams.imported_history` | `src/cmd/interactive/mod.rs:436-477`；`src/session/store.rs:245-262` |
| `iota resume`（无 id） | 启动前 picker（`list(scope)`） | `src/cmd/interactive/mod.rs:256-264`, `:177-190` |
| `iota resume <id> -m` | 同上但 headless，成功后追加 delta | `src/cmd/mod.rs:421-473`, `:532-536` |
| `/session` | 聊天中切换：Resume / Delete 两个 tab；换 writer、换 history、`persisted = len` | `src/repl/commands/session.rs:88`, `:167-192` |
| `/save [title]` | **只在 ephemeral 会话存在**：铸 writer，水位线为 0 所以整段一次追加 | `src/repl/commands/save.rs:19-72` |
| `/new` | **不存在**。命令表里没有 `/new`、`/clear`（`src/repl/commands/mod.rs:63-121`）；开新对话 = 退出重开 `iota` | — |
| `iota list sessions` | `list_all` 平铺列出 | `src/cmd/list.rs:212-214` |

加载时 `load_log` 构建**派生视图**：最后一条 system 放首位，最后一个压缩标记之后的尾部 + 摘要前言；usage 对全日志求和（`src/session/loader.rs:229-286`）。

文档漂移：`docs/design/session-format.md §10` 写「`/save` removed」，代码里 `/save` 仍为 ephemeral 会话保留；§4.1 的 meta 示例远少于实际字段。

---

## 4. 上下文压缩今天怎么实现

| 项 | 实现 | 坐标 |
|---|---|---|
| 前提 | 只在 provider `reports_usage()` 时启用（meter、`/compact`、自动询问） | `src/repl/run.rs:293`, `:324-332`, `:352` |
| 触发阈值 | `max(window×80%, window−16k)`；默认窗口 128k | `src/repl/context/meter.rs:99-101`；`src/repl/context/tokens.rs:27,39,43` |
| 自动触发 | **每次发送前**弹 Confirm（`Compact now` / `Not now`）；拒绝后增长 5% 窗口才再问 | `src/repl/commands/compact.rs:253-284`；`meter.rs:258-270`；`tokens.rs:47` |
| 手动 | `/compact [hint]` | `src/repl/run.rs:625-630` |
| 保留规则 | system + **最后一轮**（从最后一条 user 到末尾）原样；中间全部交给模型写一段摘要 | `compact.rs:50-55`, `:88-125` |
| 摘要调用 | 一次无工具 `provider.chat`，指令防注入；工具结果截到 2000 字符 | `compact.rs:43`, `:46`, `:129-181` |
| 产物（内存） | 摘要前言 `[Earlier conversation summary]\n…———` **拼进第一条保留消息的副本** | `compact.rs:107-118`；`src/session/loader.rs:24-38` |
| 产物（磁盘） | 追加一条 `role: compaction` 记录（摘要 + `compacted_through` + usage），原消息不删 | `src/session/writer.rs:145-166` |
| resume | 取**最后一个**标记重建视图，不重新摘要 | `src/session/loader.rs:241-280` |
| headless | **没有压缩**：`run_once` 不检查窗口 | `src/headless/run.rs:126-206` |

**压缩后还能不能恢复细节**：
- 对**模型**：不能。视图里只剩摘要；多次压缩是「旧摘要 + 新内容 → 新摘要」，损失累积（`docs/design/context-compaction.md §8`；实现上 `compact_history` 把旧前言当普通文本再摘要）。模型没有任何工具能回读被压掉的原文。
- 对**人/程序**：能。磁盘是完整事件日志；`load_full_history` 跳过标记读全量（`src/session/loader.rs:210-221`），`/export` 就用它（`src/session/store.rs:283-286`）。

---

## 5. 长期记忆

**没有。** 全仓 `grep -rniw 'memory|remember|recall' src` 只命中 UI 的 ↑ 历史、审批门的「本会话已允许」和读上限注释，无任何跨会话事实留存或检索机制。

今天仅有的跨轮/跨会话「持久输入」：

| 机制 | 性质 | 能否由模型写入 |
|---|---|---|
| 会话日志 + resume | 整段对话，单会话内 | 否（只能被整体恢复） |
| 压缩摘要 | 单会话内的有损蒸馏 | 否（由摘要调用产生） |
| agent 的 `system:` / `system_file:` | 静态配置 | 否 |
| **AGENTS.md 链** | 每条消息 mtime 探测、变更即重读（`src/agents/mod.rs:246-266`） | **间接可以**：开了 `code` 工具集时 `write_file`/`edit_file` 能写项目根内的 AGENTS.md（路径 jail `src/tool/builtins/code/mod.rs:96-112`），默认需审批（`src/tool/builtins/code/tools.rs:271-273`, `:320-322`）。但这是副作用，不是设计过的记忆：无结构、无检索、32 KiB 截断、和项目指令混在一起 |
| skills | 按需加载的静态说明 | 同上，间接 |

所以「AGENTS.md 是唯一手段」基本成立，而且它是**项目级**的——bot 若不绑定项目根，就连这条路也没有。

---

## 6. host 层能提供什么

host 层是**纯出站**的（`src/host/mod.rs:1-41`）：

| 能力 | 实现 | 坐标 |
|---|---|---|
| 状态 Idle / Busy / NeedsInput / Error | Presenter 去重后交给第一个 `StateReporter` | `src/host/mod.rs:59-69`, `:274-320` |
| 后台 job 运行时把 Idle 改写为 Busy | `set_jobs` / `job_ended` / `notice_taken` | `:227-232`, `:282-301` |
| 注意力 ping（Done / Failed / NeedsInput） | 第一个 `Notifier`；受 `notify:` 开关控制 | `:73-89`, `:328-335` |
| 会话身份上报 | herdr `pane.report_agent_session` | `:338-342`；`src/host/herdr.rs:68-81` |
| 给模型的 `<environment>` 事实 | 所有宿主汇总 | `src/host/mod.rs:344-` |
| herdr：working / blocked / idle、退出 release | Unix socket JSON 行，500 ms 超时，失败只 debug | `src/host/herdr.rs:1-25`, `:181-186` |
| cmux：侧栏状态 | CLI | `src/host/cmux.rs` |
| ANSI：OSC 9;4 进度 + OSC 9 通知（终端失焦才发） | 走 ui 门面 | `src/host/ansi.rs:44`；`docs/design/host-integration.md §Hosts` |

**能不能后台跑完再通知？** 部分能，且限于一个活着的交互进程之内：
- shell 工具的 `background: true` / 前台超 20 s 自动转后台（`src/tool/builtins/shell.rs:57`；上限 16 个 `src/shell/jobs.rs:56`）。job 结束时 sink 调 `ui.enqueue(Notice)`（`src/repl/run.rs:508-513`），空闲的循环被唤醒并把结果当一轮喂给模型；结束后发 Done ping（`:792-796`）。
- 但 iota 进程本身不能脱离终端常驻：交互模式要求 TTY（`src/cmd/interactive/mod.rs:241-243`），`-m` 跑完即退；退出时杀掉所有 job（`src/repl/run.rs:802`）。herdr 的 Done 通知由 herdr 按状态自己发（herdr 不实现 `Notifier`，`src/host/herdr.rs:20-25`）。

**能不能被外部事件唤醒？** 不能（iota 侧没有入站通道）：
- 没有 socket/管道/文件监听；SIGTERM 只会取消根 token 让进程退出（`src/cmd/signals.rs`）。
- 进程内唯一的「非键盘输入」是 `Ui::enqueue`（`src/ui/facade.rs:885-888`）+ `InputKind::Notice`（`:21-28`），目前只有 job 注册表调用它。它会把 parked 的 `read_input` 立刻唤醒，或排进 type-ahead 队列在下一个轮边界被 `Steerer::drain` 取走（`src/repl/turn/steer.rs:39-58`），落盘为 `notice: true`。**这就是外部唤醒最现成的注入点。**
- herdr 那边有 `agent prompt --wait`（`src/host/mod.rs:35` 注释提到），相当于从 pane 外面往终端里打字并等待 idle——那是 herdr 的能力，走终端输入，不经 iota 的 API（**待确认** herdr 的具体语义）。

---

## 7. 对 bot 模式的判断

「常驻、不换上下文」拆成四个需求，对照今天：

1. **常驻进程** —— 今天两个循环都不满足：REPL 绑 TTY，headless 一轮退出。
2. **固定的一段会话** —— 存储层能直接复用（懒创建、只追加、崩溃安全、resume 派生视图），缺的是「这个 bot 的会话是哪一个」的稳定寻址，以及多进程写同一 bundle 的锁（`docs/design/session-format.md §12` 明说「未来多进程共享需要文件锁」，至今没有）。
3. **不换上下文而又不爆窗** —— 必须自动压缩且**无人确认**；今天的自动路径硬依赖 `ui.confirm`，且 headless 根本没有压缩。长期运行下「只留最后一轮 + 累积摘要」的损失会持续累积，所以长期记忆（第 5 节的空白）在 bot 模式里是刚需，不是锦上添花。
4. **被事件驱动** —— 只能靠新增入站通道，最省事是接到 `Ui::enqueue`（TUI 形态）或一个新的输入源（无 TUI 形态）。

两种落地形态，改动面差别很大：

- **形态 A：TUI 常驻（在 herdr/tmux pane 里一直开着的 `iota`）** —— 复用整个 `repl::run`，改动最小。本质是「一个固定会话的 REPL + 自动压缩 + 外部消息注入 + 每轮刷新 harness」。
- **形态 B：无终端守护进程** —— `repl` 的 `TurnEngine` 与 `Ui` 门面深度耦合（`TurnCtx.ui`、transcript、审批门都要门面），无法直接复用；只能复用 `headless::run_once` 做单轮，并补齐会话循环、压缩、审批策略。按分层表（ARCHITECTURE §2，`tests/layering.rs`），`headless` 在 `repl` 之下，不能调用 `repl::commands::compact`，压缩逻辑需要下沉。

建议先做 A：它验证 bot 语义（固定会话、自动压缩、事件注入、记忆）而不必同时重写 turn 引擎；B 可以在 A 的语义稳定后，把 A 新增的部分从 `repl` 下沉。

---

## 8. 接缝清单

### 8.1 可直接复用

| 模块 | 复用什么 | 坐标 | 备注 |
|---|---|---|---|
| 会话存储 | `SessionStore::{create,resume,resolve_id,list}`、`SessionWriter::{append_messages,append_compaction,update_meta}`、`load_log` 派生视图 | `src/session/store.rs:194-262`；`src/session/writer.rs:122-175`；`src/session/loader.rs:229-286` | 懒创建、只追加、每批 fsync，天然适合长会话 |
| 全量回读 | `load_full_history` | `src/session/loader.rs:210-221` | 记忆检索 / 重新摘要的原料 |
| Notice 输入 | `Ui::enqueue` + `InputKind::Notice` + `Message::notice` + record `notice` | `src/ui/facade.rs:21-28`, `:885-888`；`src/provider/model.rs:171-175`；`src/session/record.rs:54-58` | 外部事件进对话的现成通道，且轮中安全（轮边界注入） |
| REPL turn 引擎 | `TurnEngine::run` / `run_turn` / `tool_loop`（含重试、steering、中断表） | `src/repl/turn/mod.rs:252-376`；`src/repl/turn/tools.rs:46-165` | 仅形态 A；依赖 `Ui` |
| headless 单轮 | `run_once` / `execute_with_tools`（会话盲、返回 delta） | `src/headless/run.rs:126-206`, `:274-` | 形态 B 的 turn 引擎；审批一律拒绝（`:26-52`） |
| 压缩核心 | `compact_history` / `summarize` / `retain_tail_count`（纯函数，只需 `&dyn Provider`） | `src/repl/commands/compact.rs:50-181` | 形态 A 直接用；形态 B 需下沉出 `repl` |
| 覆盖层 | `Overlay::{new,refresh,content}`、`compose_send_history` | `src/agents/mod.rs:189-301`, `:316-363` | 每消息刷新已实现 |
| 宿主上报 | `Presenter::{set_state,notify,set_session}`、herdr/cmux/ANSI | `src/host/mod.rs:202-342` | 常驻 bot 的「在忙/空闲/完成」信号现成 |
| 后台 job | `Jobs` + sink + watch | `src/repl/run.rs:505-537` | bot 起长任务、完成后被唤醒 |

### 8.2 必须新增或改动

| # | 接缝 | 最小改动面 | 坐标 | 说明 |
|---|---|---|---|---|
| 1 | bot 开关 | `AgentConfig` 加 `bot: bool`；`AGENT_KEYS` 加 `"bot"`（数组长度 14→15）；`RunSettings` 带出 | `src/config/agent.rs:19-67`；`src/config/strict.rs:37-52`；`src/cmd/resolve.rs:13-45`, `:128-146` | 与 `workspace:` 同一层；动词集是封闭的（`src/cmd/args.rs:79-95`），加 `iota bot` 动词是另一种选择，但 X-10/X-11 的 CLI 决策倾向「配置决定，不加旗标」（`src/config/agent.rs:40-42`）——**待决策** |
| 2 | 固定会话寻址（resume-or-create） | 在 `wire_session` 的 `resume_given` 分支前加：bot 时按稳定键找已有 bundle，找到走 `store.resume`，否则 `store.create` | `src/cmd/interactive/mod.rs:436-506` | 稳定键的存放：meta 新增字段（`src/session/meta.rs:45-120`，需放弃「不往 extra 加键」而是加正式字段），或 `~/.iota/bots/<agent>` 指针文件；前者让 `find_dir` 不必改布局。**待决策** |
| 3 | 单写者锁 | `SessionWriter` 打开时加 advisory lock（`resume`/`ensure_created`），第二个进程拒绝或只读 | `src/session/store.rs:245-262`；`src/session/writer.rs:184` | bot 常驻时用户再 `iota resume <id>` 会双写同一 `messages.jsonl` |
| 4 | 无人值守自动压缩 | `offer_before_send` 在 bot 下跳过 `ui.confirm` 直接 `compact_now(repl, "", false)` | `src/repl/commands/compact.rs:253-284` | 一个布尔即可；失败只警告、保留原历史（`:200-204`） |
| 5 | 每轮刷新 harness | `Conversation.harness` 改为每条消息重组（或至少重算 `date:`）；需把 `HarnessInputs` 与 `Presenter` 留在 `Repl` 里 | `src/cmd/interactive/mod.rs:327`；`src/repl/state.rs:57-59`；`src/repl/run.rs:811-824`；`src/cmd/assemble.rs:82`, `:92-110` | 否则跨日常驻的 `<environment> date:` 过期；harness 变动会打破 prompt cache，只在日期变化时重组即可 |
| 6 | 外部事件入站通道 | 新任务监听（Unix socket / FIFO / 目录监视，**待决策**），收到即 `ui.enqueue(Input{kind: Notice, ..})`；在 `repl::run` 里与 jobs sink 并列安装 | 挂点 `src/repl/run.rs:505-513` | 门面与 steering 已保证「空闲唤醒、忙时轮边界注入」；新模块应放在 `cmd` 或 `host` 层以守分层（`host` 在 `repl` 之下，只能被传入） |
| 7 | 长期记忆 | 新模块（建议 `src/memory/` 或 `agents/` 旁）：写入 = 新工具集（如 `remember`/`recall`）或压缩时抽取；读取 = 作为易失覆盖层第四段拼进 `compose_send_history` | `src/agents/mod.rs:316-363`（加段）；`src/tool/sets.rs:15`（`SET_NAMES` 加集合）；`src/repl/commands/compact.rs:129-181`（压缩时顺带抽取） | 覆盖层模式天然合适：不进 history、不落进日志、每轮新鲜；存储位置与作用域（按 bot / 按项目）**待决策** |
| 8 | 退出语义 | bot 下 Ctrl+D / 空闲 Ctrl+C 是否仍退出；`jobs.kill_all` 是否保留 | `src/repl/run.rs:557-562`, `:802` | 形态 A 至少需要确认「误关窗口 = bot 下线」的处理 |
| 9 | 审批策略 | bot 无人值守时审批门会一直阻塞在 `NeedsInput` | `src/repl/turn/approval.rs:18-`；headless 对照 `src/headless/run.rs:26-52` | 选项：沿用人在环（herdr 会显示 blocked）、或 bot 专用的 `auto_write`/`auto_run` 预设——**待决策** |
| 10 | 标题 | 固定会话只在首条消息起名；常驻后标题可能长期不代表内容 | `src/repl/title.rs:94-127`；`src/repl/run.rs:863-885` | 低优先级 |
| 11 | （形态 B）无 TUI 循环 | 新 `cmd` 分支：`loop { input = 源.next(); outcome = run_once(history…); writer.append_messages(delta); history.extend(delta) }` + 下沉的压缩 | 仿 `src/cmd/mod.rs:392-553`；压缩下沉出 `src/repl/commands/compact.rs` | `compact.rs` 依赖 `repl::context::tokens::go_map` 与 `repl::render::styles::truncate_runes`（`:36-37`），一并下沉；分层门 `tests/layering.rs` 会卡住任何上行引用 |

### 8.3 形态 A 的最小改动面（汇总）

1. `src/config/agent.rs` + `src/config/strict.rs` + `src/cmd/resolve.rs`：`bot:` 开关（#1）。
2. `src/cmd/interactive/mod.rs::wire_session`：bot 时 resume-or-create 固定会话（#2），`ephemeral` 对 bot 无意义应拒绝。
3. `src/session/{store,writer}.rs`：单写者锁（#3）。
4. `src/repl/commands/compact.rs::offer_before_send`：bot 跳过确认（#4）。
5. `src/repl/run.rs::run` + `src/repl/state.rs::Conversation`：harness 按日期重组（#5）；安装外部事件监听到 `ui.enqueue`（#6）。
6. `src/agents/mod.rs::compose_send_history` + 新记忆模块 + 新工具集：长期记忆（#7），可作为第二阶段。

---

## 附：本次核对过的文档与漂移

| 文档 | 与代码不一致处 |
|---|---|
| `docs/design/agent-mode.md §Switch` | 开关已改为 `agents.<name>.workspace:`，无 `--agent` 旗标 |
| `docs/design/session-format.md §4.1 / §10` | meta 字段远多于示例；`/save` 仍存在（仅 ephemeral） |
| `docs/design/context-compaction.md §6/§7` | 与实现一致；「post-compaction target 0.5」未实现——保留规则是「只留最后一轮」，不是按比例压到 50%（`src/repl/commands/compact.rs:50-55`） |
| `docs/design/host-integration.md` | 仍以 Go 路径（`chat/run.go`）与 cmux 为主，未记 herdr 宿主与「最内层宿主独占」决策（见 `src/host/mod.rs:19-29`） |
| `docs/ROADMAP.md` | 无 bot / 记忆相关条目 |
