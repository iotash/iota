# 「新会话」命令：别家怎么做，iota 怎么对齐（`/new`）

Status: **Proposal · 待拍板** · 日期：2026-10-13 · 基线：main `287a17a` · 分支 `new-session-command`

所有者的原话：「我们是不是可以增加一个类似 clear 或者 new 的命令，我看其它 coding agent 都有这个命令，查一下它们的行为，然后怎么和我们的当前指令和特点对齐」。

本文做三件事：§1 别家怎么做（七问逐家回答）；§2 iota 的地基（对着代码）；§3 对齐提案与待拍板的点。**不含实现，不改代码。**

## 写作约定

- 证据标记：**【官方】**厂商文档、仓库源码、官方 changelog/release/news；**【本机实测】**本机装的二进制（跑 `--help`、`strings` 抽二进制字符串、`cargo build` 后实跑 iota）；**【推断】**本文自己的推理；**【查不到】**官方资料没说或无法核实，不用常识补。
- 本机装了 **Claude Code 2.1.296**（`~/.local/share/claude/versions/2.1.296`）和 **Codex CLI 0.162.1**（npm `@openai/codex`）；Gemini CLI、Aider、opencode、Crush、Cursor CLI、Amp、Copilot CLI 本机没装，不另行安装，只看文档与源码。
- 链接都在 2026-10-13 打开。源码行号标明 commit；文档页普遍无日期，只有「Requires vX」之类的版本注记，有就照抄。
- iota 代码坐标指 `287a17a`。

每一家回答同样七问：

| 编号 | 问题 |
|---|---|
| ① | 命令名与别名（含快捷键），谁是主名 |
| ② | 清了什么：可见转录 / 模型上下文 / 磁盘会话 |
| ③ | 旧会话变成什么：没了，还是仍可 resume |
| ④ | 跨清除保留了什么：模型、cwd、权限/沙箱、system prompt、MCP、待办 |
| ⑤ | 副作用：计量、标题、自动压缩状态、屏幕 |
| ⑥ | headless / 非交互里的对等物 |
| ⑦ | 用的是什么证据 |

---

## 1. 别家怎么做

### 1.0 一览

| 工具 | ① 主名 / 别名 | ② 清什么 | ③ 旧会话 | ④ 跨清除保留（有据的） | ⑤ 计量归零 · 屏幕 | ⑥ headless |
|---|---|---|---|---|---|---|
| Claude Code 2.1.296 | `/clear`；别名 `/reset` `/new`；Ctrl+L 只重绘 | 模型上下文；开新会话 | 留在磁盘，`/resume`、rewind 菜单可回 | CLAUDE.md/memory、`/rename` 名、后台任务；实测：MCP 连接留、cwd 回启动目录、前台任务杀 | 归零（2.1.211 起）· 经典渲染器是否清屏【查不到】 | SDK 流式可发 `/clear`；one-shot 无意义 |
| Codex 0.162.1 | `/new` 主名；`/clear` = 清终端 + 新会话；无 `/reset` | TUI 转录、模型上下文；rollout 不删 | 留在磁盘，`/resume` | cwd、sandbox/approval（含会话内改动）、profile、AGENTS.md 重发现；**模型/effort 回到配置默认** | 归零 · alt-screen，`/clear` 清屏+scrollback | `codex exec` 每次新 thread；`exec resume` 续 |
| Gemini CLI 0.63 | `/clear`；别名 `/new`（0.40 起，文档未提）；Ctrl+L 只清屏 | 转录、上下文、开新 jsonl | 留在 `chats/`，`/resume`、`--resume` | 模型（清 runtime override）、cwd、approval、GEMINI.md 重载、MCP；`@文件` 丢 | 归零（telemetry.clear）· 非 alt-buffer 时真清屏 | `-p` 会执行但原文仍当 prompt 发；hook 可 clearContext |
| Aider 0.86 | `/clear`（无别名）；`/reset` = drop 文件 + clear；Ctrl-L 只清屏 | 只清内存 `done/cur_messages` | `.aider.chat.history.md` 继续追加；`--restore-chat-history` 连 clear 前一起恢复 | 模型、`/add` 文件、只读文件、repo map、`total_cost` 全留 | 不归零（`/tokens` 少一行，费用累计）· 不清屏 | `--message` 单发即退；无对等物 |
| opencode 2.0 | `/new`（`session.new`，`ctrl+x n`）；文档说 `/clear` 是别名，源码是另一条 `session.clear`（多关 tab） | 只切到 home 路由；新会话到首条消息才建 | 留在 SQLite，`/sessions`、`--continue`、`--session` | 模型、agent 显式带过去；cwd 默认回启动目录（`new_location: launch`） | 无会话即不显示 · alt-screen 全屏 | `opencode run` 每次新建 |
| Crush 0.98 | `ctrl+n`（面板 "New Session"，别名 `clear`）；无斜杠命令 | 清 TUI 内存态、切 landing；新会话懒建 | 留在 `crush.db`，`ctrl+s`、`--continue`、`-s` | 模型、yolo、MCP、cwd 不动；plan mode 退回、**LSP 全停** | 无会话即不显示 · alt-screen | `crush run` 每次新建 |
| Cursor CLI | `/clear`；别名 `/new` `/new-chat` `/newchat`；Ctrl+L 只清屏 | 「Start a new chat session」，细节【查不到】 | `/resume`、`--resume`、`--continue`；clear 后是否在列表【查不到】 | 文档只写全局配置文件，与 `/clear` 的关系【查不到】 | 【查不到】 | `-p`；`create-chat` + `--resume` |
| Amp | `Ctrl+C Ctrl+N`（归档当前 thread 开新）；`/new` 只在 2025-08 的 news 里 | 当前 thread 归档（不删，URL 仍在） | `amp threads continue T-…`，可 unarchive | mode 按 thread 固定；MCP/AGENTS.md 来自设置文件 | 【查不到】 | `amp -x` 每次新 thread |
| Copilot CLI 1.0.96 | `/clear` `/new` `/reset` 同一行；1.0.11 起 `/clear` 弃旧会话、`/new` 留后台；Ctrl+L 只清屏 | 「新对话」；清 timeline、截断状态；旧会话触发 sessionEnd | `--resume`、`--continue`、`/resume` | MCP、权限（yolo/autopilot）、`/add-dir`、agent mode；custom agent 选择重置 | limits 归零、终端标题重置 · 清屏【查不到】 | `-p` 单次；SDK `history.clearContext()` |

### 1.1 Claude Code（本机 2.1.296；文档与 CHANGELOG 2026-10-13）

- ① **主名 `/clear`，别名 `/reset`、`/new`。**【官方】commands 页："`/clear [name]` | Start a new conversation with empty context. … Aliases: `/reset`, `/new`"；v2.1.236 起 "typing `/new` highlights `/clear` through its alias." https://code.claude.com/docs/en/commands 。【本机实测】二进制里的命令定义原文：`{type:"local",name:"clear",description:"Start a new session with empty context; previous session stays on disk (resumable with /resume)",argumentHint:"[name]",aliases:["reset","new"],supportsNonInteractive:!0,…}`。
  - Ctrl+L 只重绘：【官方】keybindings 页 "`chat:clearInput` | Ctrl+L | Force a full screen redraw, preserving input and conversation"；"`chat:clearScreen` | Cmd+K | Same as `chat:clearInput`" https://code.claude.com/docs/en/keybindings 。历史：2.1.83 Ctrl+L 清屏重绘；2.1.126 "now only forces a screen redraw, matching readline behavior"；fullscreen 页 "Before v2.1.238, pressing `Ctrl+L` twice within two seconds ran `/clear`."；2.1.238 "the double-press `/clear` shortcut was removed" https://code.claude.com/docs/en/fullscreen · https://raw.githubusercontent.com/anthropics/claude-code/main/CHANGELOG.md 。
  - 与 `/compact` 的分工：【官方】"To free up context while continuing the same conversation, use `/compact` instead."（commands）；"When you want a fresh start instead of continuity, `/clear` costs nothing"（https://code.claude.com/docs/en/costs ）。
  - `/rewind`（别名 `/checkpoint` `/undo`）与 `/clear` 相关：【官方】"If you ran `/clear` earlier in the same Claude Code process, the rewind menu shows an additional entry at the top … `/resume <session-id> (previous session)`" https://code.claude.com/docs/en/checkpointing （2.1.191）。
  - 【查不到】`/new` `/reset` 别名是哪个版本加的（CHANGELOG 无条目）。
- ② **清模型上下文，开一个空上下文的新会话；磁盘 JSONL 不删。**【官方】"`/clear`: start fresh with an empty context. Claude Code saves the previous session; resume it with `/resume`"；"Sessions are saved continuously to local transcript files as you work, so you can return to one after exiting or running `/clear`."；路径 "`~/.claude/projects/<project>/<session-id>.jsonl`" https://code.claude.com/docs/en/sessions 。glossary："Running `/clear` starts a new session; the previous one stays stored and is available via `/resume`." https://code.claude.com/docs/en/glossary 。
  - 还清/杀什么：【官方】CHANGELOG 2.1.72 "Fixed `/clear` killing background agent/bash tasks — only foreground tasks are now cleared"；2.1.63 "not resetting cached skills"；2.1.3 "plan files persisting across `/clear`"；2.1.239 "Claude in Chrome: `/clear` now closes the session's Chrome tab group"。【本机实测】`clearConversation` 的实现片段：`m((e)=>{…;return []})`（消息清空）、对运行中的任务 `n.shellCommand?.kill()` / `abortController?.abort()`，`fileHistory:{snapshots:[],…}`（文件快照清零），`D.clear()`（已读文件状态），`N?.clear()`（会话环境变量），`resetWebSearchBudget()`。
  - 经典渲染器下**终端屏幕/scrollback 是否清空：【查不到】**（文档只写 fullscreen 的 alt-screen 行为）。
- ③ **旧会话留在磁盘，可 `/resume`、`claude --resume <id|name>`，同进程内可从 rewind 菜单回去。**【官方】interactive-mode："Running `/clear` starts a new session … The previous session's conversation is preserved and can be resumed."；commands："Pass a name to label the previous conversation in the `/resume` picker."；sessions："With no argument, the new session keeps a name you set with `--name` or `/rename`, but not an AI-generated session title."。hooks：SessionEnd reason "`clear` | Session cleared with `/clear` command"，SessionStart source "`clear` | `/clear`" https://code.claude.com/docs/en/hooks 。
  - 「新 session id」的直接表述只来自用户 issue（#31220 "resets the conversation **and** creates a new session"；#26082 "creates a new session (new .jsonl file)"，由 chrislloyd 关闭为 completed，CHANGELOG 2.1.47 引用）；文档只说 "starts a new session"。`/clear` 之后 `--continue` 指向哪个：【查不到】。
- ④ **文档明说保留的：** CLAUDE.md / auto memory（"`/clear` starts fresh on a new task while keeping project memory." commands；"Both are loaded at the start of every conversation." https://code.claude.com/docs/en/memory ）；`/rename` 的名字（2.1.243）；后台任务（2.1.72）；SessionStart hook 重跑（hooks："When you start an interactive session, resume … or run `/clear`, SessionStart hooks run in the background."）。
  - 模型：hooks 页 SessionStart 输入 "`model` | The active model identifier. It can be omitted, for example after `/clear`"；model-config："`/model` saves your choice as the default for new sessions" https://code.claude.com/docs/en/model-config 。会话内用 `s`（仅本会话）选的模型是否跨 `/clear`：【查不到】。
  - cwd：【官方文档查不到】。【本机实测】实现里 `let M=Ee();try{op(M)}catch{t(`/clear: originalCwd "${M}" no longer exists; falling back`)…}` ——**回到进程启动时的目录**（`/cd` 的效果不跨 `/clear`）。
  - MCP：【官方】只有 IDE/SDK 的修复条目（2.1.136 "Fixed MCP servers … silently disappearing after `/clear` in the VS Code extension, JetBrains plugin, and Agent SDK"）；【本机实测】`for(let e of r().mcp.clients) if(e.name==="ide"&&…) clearServerCache(…)` ——**客户端列表保留，只清 ide 的缓存**。
  - 权限模式、system prompt、todo：【查不到】（权限页只写 "File modification … Until session end"，没说 `/clear` 算不算 session end；todo 只写 "Tasks persist across context compactions"）。
- ⑤ **`/cost`（=`/usage`）的 Session 统计归零**（2.1.211 起）：【官方】"These totals reset when `/clear` starts a new session, so the next session's total cost starts at $0. Before v2.1.211, they kept accumulating across `/clear`"（costs）。AI 生成的标题不保留；终端 tab 标题重置（2.1.129）；子代理配额（2.1.212）、`/loop` 计数（2.1.290）重置；SessionEnd(reason=clear) 触发、默认 1.5 s 超时（hooks）；2.1.286 在查看后台 agent 转录时输入 `/compact` `/clear` `/rewind` 会先弹框确认目标；2.1.84 空闲 75 分钟后提示 `/clear`。自动压缩状态：【查不到】。
- ⑥ **Agent SDK 流式模式可以发 `/clear`；one-shot 没意义。**【官方】"The `/clear` command resets the conversation to an empty context … The previous conversation remains on disk."；"For one-shot `query()` calls, each call already starts with empty context, so sending `/clear` has no practical effect. Start a new `query()` instead." https://code.claude.com/docs/en/agent-sdk/slash-commands 。不落盘："`--no-session-persistence`"（sessions）、"`persistSession: false`"（agent-sdk/sessions）；"Claude Code leaves sessions created with `claude -p` or the Agent SDK out of the session picker and out of `claude --continue`."（sessions）。【本机实测】命令定义 `supportsNonInteractive:!0`。
- ⑦ 证据：官方文档页（无日期，带版本注记）、CHANGELOG（`/clear` 最早见 1.0.45）、用户 issue（除 #26082 外无维护者评论）、本机 2.1.296 二进制字符串。注意 `/docs/en/slash-commands` 现在返回 skills 页，命令参考在 `/docs/en/commands`。

### 1.2 OpenAI Codex CLI（本机 0.162.1 = tag `rust-v0.162.1`；源码 main `9167a0b` 2026-10-13，关键函数两版相同）

- ① **主名 `/new`；`/clear` 是「清终端 + 新会话」的另一条命令；无 `/reset`、无别名、无开新会话的快捷键。**【官方】`SlashCommand::New => "start a new chat during a conversation"`、`SlashCommand::Clear => "clear the terminal and start a new chat"` https://github.com/openai/codex/blob/9167a0b2d4a74d44a4d388057d304682e47313c6/codex-rs/tui/src/slash_command.rs#L96 （`#L105`）；文档章节「Start a new chat with `/new`」，"Unlike Ctrl+L, `/clear` starts a new chat." "Ctrl+L only clears the terminal view and keeps the current chat." "To name the new chat as you create it, run `/new bug bash`." https://learn.chatgpt.com/docs/cli/slash-commands 。Ctrl+L 绑 `clear_terminal`（`keymap.rs#L1664`）。【本机实测】`codex` 二进制字符串里同一串描述按枚举顺序出现："rename the current thread" "start a new chat during a conversation" "archive this session" "permanently delete this session" "resume a saved chat" "fork the current chat" … "clear the terminal and start a new chat"；键位动作 `clear_terminalClear the terminal UI.`。
  - 任务进行中两者不可用：`available_during_task` 里 `New … | Clear … => false`（`slash_command.rs#L243-L265`）；文档 "Codex disables both actions while a task is in progress."
- ② **两者都新开一个 app-server 线程（模型上下文全新）、替换 ChatWidget、清空 TUI 转录单元；rollout 不删。**【官方】`app_event.rs#L614-L618`："Clear the terminal UI (screen + scrollback), start a fresh session, and keep the previous chat resumable."；`session_lifecycle.rs#L999-L1001`："Start a fresh in-memory session while preserving resumability via persisted rollout history."；文档 `/clear`："Codex clears the terminal, resets the visible transcript, and starts a fresh chat in the same CLI session."；`/new`："Reset the chat context without leaving the CLI when you want a fresh prompt in the same repo."。区别：`/clear` 在 `thread/start` 之前先 `clear_terminal_ui` 并传 `ThreadStartSource::Clear`（`event_dispatch.rs#L443-L458`）；`/new` 走 `start_fresh_session`（`#L198-L207`），挂接新线程时也经 `reset_for_thread_switch` 清屏。
- ③ **旧线程 unsubscribe，不删不归档；新线程新 id、新 rollout 文件；`/resume` / `codex resume` 找回。**【官方】新 id `generate_thread_id()`（`core/src/session/session.rs#L895-L897`）；rollout 路径 `sessions/<YYYY>/<MM>/<DD>/rollout-<timestamp>-<id>.jsonl`（`rollout/src/recorder.rs#L1745-L1758`）；`InitialHistory::New | Cleared` 不读旧历史。【本机实测】`codex resume --help`："Resume a previous interactive session (picker by default; use --last to continue the most recent)"。`/new <name>` 只给**新**线程命名（`thread_set_name`）。
- ④ **保留 cwd、sandbox/approval（含会话内 picker 改动）、profile、service tier；AGENTS.md 为新线程重新发现；模型与 reasoning effort 不保留运行时值，回到服务端 effective config 默认，除非 `-m` / `-c` / `-p` 显式给出。**【官方】`new_session.rs#L37-L42`：`if harness_overrides.model.is_none() && !has_launch_setting(config, cli_kv_overrides, "model") { config.model = defaults.model.clone(); }`；文件头 "Replacement starts use server defaults and preserve the current task on failure."；测试 `new_sessions_preserve_yolo_launch_and_later_permission_choices`（`app/tests/new_session_tests.rs#L10-L80`）。MCP：`McpManager` 跨线程共享（`thread_manager.rs#L433`）；是否复用已建立的连接【查不到】。
- ⑤ token/context 指示器归零（新 ChatWidget `token_info: None`，`chatwidget/constructor.rs#L149-L150`）；自动 compact 状态随新 session 全新；屏幕：TUI 默认 alt-screen（`tui.rs#L758`；`--no-alt-screen` "Runs the TUI in inline mode, preserving terminal scrollback history."）。**文档与源码不一致**：文档说 "Unlike `/clear`, `/new` doesn't clear the current terminal view first."，源码里 `/new` 在 `reset_for_thread_switch` 阶段也清屏/清 scrollback（`tui.rs` 0.162.1 `#L1170-L1184`）；本次未运行 TUI 实测。
- ⑥ **`codex exec` 无斜杠层；每次 `exec` 新 thread（`thread.started` 事件）；续用 `codex exec resume` / `fork`；`--ephemeral` 不落盘。**【本机实测】`codex exec --help`：`resume  Resume a previous session by id or pick the most recent with --last`、`fork  Fork a previous session by id into a new session`。【官方】"If you need to continue a previous run … use the `resume` subcommand" https://learn.chatgpt.com/docs/non-interactive-mode 。
- ⑦ 证据：源码 main + tag 核对、learn.chatgpt.com 文档（developers.openai.com/codex/* 已 308 重定向）、本机 `--help` 与二进制字符串。仓库 `docs/*.md` 为跳转页，CHANGELOG 指向 releases。

### 1.3 Gemini CLI（源码 main `51617c1` 2026-10-12；stable v0.63.0 2026-10-06）

- ① **主名 `/clear`，唯一别名 `/new`（v0.40.0 起，文档未提）；无 `/reset`；Ctrl+L 只清屏。**【官方】`clearCommand.ts`：`name: 'clear', altNames: ['new'], description: 'Clear the screen and start a new session'` https://github.com/google-gemini/gemini-cli/blob/main/packages/cli/src/ui/commands/clearCommand.ts ；commands.md `/clear` 条："Ctrl+L only clears the terminal display and redraws the UI, preserving the active conversation context; use `/clear` to start a new session" https://github.com/google-gemini/gemini-cli/blob/main/docs/reference/commands.md ；keyboard-shortcuts.md "`app.clearScreen` | Clear the terminal screen and redraw the UI. | `Ctrl+L`"。`/chat` 是 `/resume` 的别名（子命令 list/save/resume/delete/share）；`/compress` 别名 `summarize` `compact`。
- ② **三样都动。**【官方】commands.md："Clear the agent conversation context (active conversation history) and start a new session, in addition to clearing the visible terminal screen, history, and scrollback within the CLI."；源码 `ui.clear()` → `clearItems(); refreshStatic(); setBannerVisible(false)`（`slashCommandProcessor.ts` L221-225）；`geminiClient.resetChat()` 新建 chat，初始历史只含 environment-context（`client.ts` L338、L410）；磁盘开新文件 `session-<timestamp>-<shortId>.jsonl`（`chatRecordingService.ts` L779-823），旧文件不删不改。会话目录 "`~/.gemini/tmp/<project_hash>/chats/`" https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/session-management.md 。
- ③ **新 UUID；旧会话文件留在 `chats/`，`/resume` 的 Session Browser 与 `gemini --resume` 枚举该目录。**【官方】`clearCommand.ts` L40-43：`newSessionId = randomUUID(); config.resetNewSessionState(newSessionId);`（注释 "MUST do this before calling resetChat()"）；hooks：SessionStart `source: "startup" | "resume" | "clear"`，SessionEnd `reason` 含 `"clear"` https://github.com/google-gemini/gemini-cli/blob/main/docs/hooks/reference.md 。实机可见性未验证（本机无 gemini）。
- ④ **保留：模型（但清 fallback/runtime override）、cwd（新文件仍写同一项目目录）、approval mode（未触碰）、GEMINI.md（重新从磁盘加载，`resetChat` → `memoryContextManager.refresh()`）、MCP（toolRegistry 复用）、`/chat save` 的 checkpoints（按 tag 存文件）。丢失：`@文件` 注入的内容（只是 prompt 文本）。**【官方】`config.ts` `setSessionId` 无 `this.model =`，含 `this.fallbackOverrides.clear(); this.modelConfigService.clearRuntimeOverrides();`。沙箱：【查不到】。
- ⑤ **token/context 指示器归零**：`uiTelemetryService.clear(newSessionId)`（`uiTelemetry.ts` L208-216；commit 0d64355be6 2025-07-17 "update context percentage when /clear command is run (#4162)"）；`/stats` 的 `sessionStartTime`、`promptCount` 重置；`hasFailedCompressionAttempt=false`、`compressionTruncationCounter=0`；**屏幕**：`refreshStatic` 在非 alternate-buffer 模式写 `ansiEscapes.clearTerminal` 并 remount（`AppContainer.tsx` L652-657），alt-buffer 模式只 remount；Ctrl+L 路径不动 chat/telemetry。
- ⑥ **文档查不到 headless 对等物；源码 `-p` 会解析斜杠命令并执行 `/clear` 的 action（UI 空操作），但原文随后仍当 prompt 发出去**（`nonInteractiveCli.ts` L264-277）；`--resume` 可与 `-p` 组合；AfterAgent hook 的 `hookSpecificOutput.clearContext: true` "clears conversation history (LLM memory) while preserving UI display."。
- ⑦ 证据：源码（主）+ 同 commit docs + geminicli.com；CHANGELOG 无 `/clear` `/new` Ctrl+L 条目；按 tag 核对别名版本。任务书里的 `docs/cli/commands.md`、`docs/cli/keyboard-shortcuts.md` 已 404，迁到 `docs/reference/`。

### 1.4 Aider（源码 main `5dc9490` 2026-05-22，`0.86.3.dev`；最新 release v0.86.0 2025-08-09）

- ① **主名 `/clear`，无别名表（命令名由 `cmd_*` 方法名派生，前缀匹配）；`/reset` = `/drop`（全部）+ `/clear`；`Ctrl-L` 只清屏。**【官方】`cmd_clear` docstring "Clear the chat history"；`cmd_reset` "Drop all files and clear the chat history" https://raw.githubusercontent.com/Aider-AI/aider/main/aider/commands.py ；commands.html Keybindings "`Ctrl-L` : Clear the screen." https://aider.chat/docs/usage/commands.html 。
- ② **只清两个内存列表**：`_clear_chat_history`："self.coder.done_messages = []\n self.coder.cur_messages = []"；打印 "All chat history cleared."（v0.85.2 加）。不清屏；`.aider.chat.history.md` 与 `.aider.input.history` 都是 append-only，连 `#### /clear` 这一行和提示语都追加进去（`io.py` `append_chat_history` / `user_input` / `tool_output`）。
- ③ **旧会话在内存里没了，history 文件完整保留并继续追加。**`--restore-chat-history`（默认 False，"Restore the previous chat history messages (default: False)" `args.py`）启动时读整个文件回填，解析器不把 `#### /clear` 当边界（`utils.py` 那段被注释掉），所以 `/clear` 之前的内容也会一起恢复。【官方】https://aider.chat/docs/config/options.html 。
- ④ **`/clear` 保留模型、`/add` 文件、`/read` 只读文件、repo map、`total_cost`、`aider_commit_hashes`；`/reset` 额外清文件（只留 `--read` 启动给的）。**HISTORY v0.52.0 "New `/reset` command to `/drop` files and `/clear` chat history."
- ⑤ `/tokens` 的 "chat history" 行在清后消失（不是归零）；无 `/cost` 命令，`total_cost` 不重置；摘要线程不停不重置（`summarize_end` 只在快照等于当前 `done_messages` 时替换——源码推断）；不清屏。文档定位："Use `/clear` to discard the chat history and make a fresh start." https://aider.chat/docs/usage/tips.html 。
- ⑥ `--message` / `--message-file` 单发即退，斜杠命令会被分派（aider 自己用 `args.message = "/paste"`）但 `/clear` 作为单条消息没意义；无「新会话」flag。Python API："You can run in-chat "/" commands too." https://aider.chat/docs/scripting.html 。
- ⑦ 证据：源码 + HISTORY.md（v0.5.0 加 `/clear`；v0.35.0 加 `--restore-chat-history`；v0.52.0 加 `/reset`）+ aider.chat 文档（无版本号/日期）。【查不到】别名；文档对 `/clear` 与 `--restore-chat-history` 关系的说明；清历史的快捷键。

### 1.5 opencode（anomalyco/opencode，`dev` @ `be674e3` 2026-10-13；CHANGELOG 顶部 v2.0.28 2026-10-13，GitHub release v1.19.0 2026-10-11）

- ① **主名 `/new`（命令 ID `session.new`，面板标题 "New session"），keybind `<leader>n`，leader 默认 `ctrl+x`。文档把 `/clear` 写成 `/new` 的别名；源码里 `/clear` 是另一条命令 `session.clear`（"Clear session"），多做一步关闭当前会话 tab。**【官方】tui 页："Start a new session. _Alias_: `/clear`" … "**Keybind:** `ctrl+x n`" https://opencode.ai/docs/tui/ ；源码 `packages/tui/src/app.tsx` L739-744 `name: "session.new", title: "New session", … slash: { name: "new" }`；L767-770 `name: "session.clear", title: "Clear session", … slash: { name: "clear" }` https://github.com/anomalyco/opencode/blob/dev/packages/tui/src/app.tsx ；`config/keybind.ts` L109 `"session.new": keybind("<leader>n", "Create a new session")`，L41 `LeaderDefault = "ctrl+x"`；keybinds 页 "By default, `ctrl+x` is the leader key" https://opencode.ai/docs/keybinds/ 。命令面板 `ctrl+p`。
- ② **`session.new` 不调任何删除 API，只是路由跳到 `home`（Logo + 输入框）；新会话到第一次提交提示词才 `session.create`。**【官方】`app.tsx` L745-764 `route.navigate({ type: "home", … })`；`component/prompt/index.tsx` L1228-1239 "Sends against the new session gate on the request." … `data.session.create({ id: newSessionID, … })`；核心 `packages/core/src/session.ts` L262-300。`session.clear` 多 `sessionTabs.close()`（L777）。磁盘：SQLite `opencode.db`（`packages/cli/src/database-path.ts`），数据目录 "`~/.local/share/opencode/`"（troubleshooting.mdx L27）。
- ③ **旧会话仍在列表**：`/sessions`（别名 `/resume` `/continue`，`<leader>l`）、`ctrl+o` 菜单、`opencode session list`、`--continue`/`--session`（可加 `--fork`）。【官方】`app.tsx` L730-735；cli 页 "`--continue` (`-c`) Continue the last session"、"`--session` (`-s`) Session ID to continue"、"`--fork`" https://opencode.ai/docs/cli/ 。
- ④ **模型与 agent 显式带到新会话**（`app.tsx` L746-747、L762-763 `local.agent.set(agent.id); local.model.set(model)`）；**cwd 由 `session.new_location` 决定，默认 `"launch"`（回 TUI 启动目录），`"inherit"` 才继承当前会话目录**（`config/index.tsx` L173-175、L323；`config/new-session-location.ts`）；已保存权限按 projectID 存（`core/src/permission/saved.ts` L22-26）。MCP、AGENTS.md：处理函数不碰它们，更多【查不到】。
- ⑤ token/cost 指示器按 sessionID 从消息列表算，home 路由没有 sessionID 所以不显示（不是归零）（`feature-plugins/sidebar/context.tsx` L12-37）；标题先回退 "New session"，首条被接纳的用户消息后由内置 `title` agent 生成，≤100 字符（`util/src/session-title-fallback.ts`；`core/src/session/title.ts`）；自动 compaction 按每次请求判定，非跨会话状态；**屏幕：opentui 默认 alternate-screen 全屏**，`/new` 是切路由不是清终端（opentui `renderer.ts` L204 "Defaults to "alternate-screen""）。
- ⑥ **`opencode run` 每次默认新建会话**（`session-target.ts` L63-72 `selected ?? session.create(…)`），续用加 `--continue`/`--session`/`--fork`（`run/run.ts` L72 "--fork requires --continue or --session"）。
- ⑦ 证据：文档站（`/docs/sessions/` 404）、源码 sparse clone、CHANGELOG。注意任务书给的 `packages/opencode/src/…` 路径在当前树里不存在（TUI 在 `packages/tui/src/`）。

### 1.6 Crush（charmbracelet/crush，main `88827f3` 2026-10-13；release v0.98.1 2026-10-09）

- ① **主名 `ctrl+n`（help "new session"）；命令面板（`ctrl+p`）里叫 "New Session"，ID `new_session`，别名 `clear`；没有斜杠命令。**【官方】`internal/ui/model/keys.go` L197-200 `km.Chat.NewSession = key.NewBinding(key.WithKeys("ctrl+n"), key.WithHelp("ctrl+n", "new session"))` https://github.com/charmbracelet/crush/blob/main/internal/ui/model/keys.go ；`internal/ui/dialog/commands.go` L449-453 `NewCommandItem(…, "new_session", "New Session", "ctrl+n", ActionNewSession{}).WithAliases("clear")` https://github.com/charmbracelet/crush/blob/main/internal/ui/dialog/commands.go 。别名来自 commit `cfad8143`（2026-07-31）；其 release note 写 "Typing `clear` in the command will match Summarize"，与 patch（挂在 New Session 上）矛盾，照实记录。切换器 `ctrl+s`。README 无快捷键表。
- ② **`newSession()` 只清 TUI 内存态**：`m.session = nil`、`chat.ClearMessages()`、切到 landing、`agenttools.ResetCache()`、**`LSPStopAll`**、向后端报告当前会话为空；不删数据库记录；新会话到第一次发消息才 `CreateSession(ctx, "New Session")`；agent 忙时拒绝 "Agent is busy, please wait before starting a new session..."。【官方】`internal/ui/model/ui.go` L5906-5939（注释 "The actual session creation happens when the user sends their first message."）、L3293-3303 https://github.com/charmbracelet/crush/blob/main/internal/ui/model/ui.go 。磁盘 `crush.db`（`internal/db/connect.go` L93）。
- ③ **留在 `crush.db`**，`ctrl+s` 对话框按 `updated_at DESC` 列出（`internal/db/sql/sessions.sql` L37-41），可选回、`ctrl+x` 删、`ctrl+r` 改名；CLI `crush --session <id>` / `crush --continue`、`crush session list|show|last|delete|rename`（`internal/cmd/root.go` L63-65；`internal/cmd/session.go` L34-83）。
- ④ 不动模型选择（全局 preferred model）、yolo、MCP、cwd；**plan mode 退回 code mode、停掉所有 LSP**（`ui.go` L904-906 "Plan mode is scoped to the session it was enabled in"）。反向注意：切回旧会话时 `restoreModelFromSession` 会把全局模型改成该会话最后一条 assistant 消息的模型（`ui.go` L2741-2800）。AGENTS.md 由 `context_paths` 配置加载。
- ⑤ 头部百分比和侧栏 token/cost 从 `m.session` 读，置 nil 后不渲染（`header.go` L152；`sidebar.go` L48-53）；标题先 "New Session"，首条真实提示后异步 `GenerateTitle`（≤50 字符）；自动摘要按当前会话 token 判定；**屏幕：`v.AltScreen = true` 全屏**（`ui.go` L3776-3778），新会话只是换到 landing。
- ⑥ **`crush run` 每次默认 `Sessions.Create(ctx, "Untitled Session")`**；续用 `--session/-s`、`--continue/-C`（互斥）（`internal/cmd/run.go`；`internal/app/app.go` L256-281）。
- ⑦ 证据：源码浅克隆、README、releases；仓库没有 CHANGELOG。任务书猜的 `internal/tui/` 不存在，TUI 在 `internal/ui/model/`、`internal/ui/dialog/`。

### 1.7 Cursor CLI（docs 无日期；CLI changelog 最新 v2026.10.08）

- ① **主名 `/clear`，别名 `/new`、`/new-chat`、`/newchat`；无 `/reset`。**【官方】"/clear — Start a new chat session. `/new`, `/new-chat`, and `/newchat` are aliases." https://cursor.com/docs/cli/reference/slash-commands 。Ctrl+L 清屏："Ctrl+L clears the screen like a shell: clears screen and scrollback, keeps your session running."（changelog May 7, 2026）；Ctrl+Y 打开 `/resume`。新会话快捷键：【查不到】。
- ② 只有 "Start a new chat session."；转录/上下文/磁盘各清什么：【查不到】。
- ③ `/resume`、`/fork`（"Fork the current chat into a new session"）、`agent resume`、`--resume [chatId]`、`--continue`（"alias for `--resume=-1`"）、`create-chat`（"Create a new empty chat and return its ID"） https://cursor.com/docs/cli/reference/parameters 。`/clear` 后旧 chat 是否在列表、新 chat 是否新 id：【查不到】。
- ④ 文档只描述全局配置文件（模型在 `~/.cursor/cli-config.json`、权限、`mcp.json`、`AGENTS.md`/`CLAUDE.md`/`.cursor/rules`），没有一句写 `/clear` 对它们的影响：【查不到】。
- ⑤ `/context`、`/rename`、`/summarize`（别名 `/compress`）与自动 summarize（v2026.10.08）存在；`/clear` 对指示器/标题/屏幕的影响：【查不到】。
- ⑥ `-p/--print` 非交互；`create-chat` + `--resume [chatId]`；JSON 输出含 `session_id`。
- ⑦ 证据：官方 docs 页 + CLI changelog。

### 1.8 Amp（Sourcegraph；docs 无版本号；2026-05-06「Amp, Rebuilt」前后不同）

- ① **当前主入口是 `Ctrl+C Ctrl+N`（"archives the current thread and starts a new one."）和命令面板 `Ctrl+O` 的 `thread: new`。**【官方】https://ampcode.com/docs/cli/keybindings ；https://ampcode.com/news/neo 。`/new` 只见于 2025-08-19 的 news（"Use `/new` for a new thread and `/continue` to pick an old thread."）；2025-10-28 起命令面板取代斜杠菜单；2026 文档里 `/new` `/clear` `/reset`：【查不到】。
- ② **当前 thread 被归档，不删除；内容在服务器。**"Archive removes the thread from your active lists. The URL still works and the agent can still reference it." https://ampcode.com/docs/threads 。清屏：【查不到】。
- ③ `amp threads continue T-…`；可 unarchive；streaming JSON 的 init 消息带 `session_id`（T-…）。Fork 2025-10-21 加入、2026-01-13 移除（"We're ripping out the Fork command."）。
- ④ mode 按 thread 固定（"A thread keeps the mode you chose when you sent its first message." https://ampcode.com/docs/the-dial ）；新 thread 是否继承 mode、cwd：【查不到】；默认不审批；MCP、AGENTS.md 来自设置文件/固定路径。
- ⑤ 自动 compaction 90%（2026-05 起；handoff 已移除）；标题 agent 自动起名可 Rename；指示器/清屏：【查不到】。
- ⑥ `amp -x` / `--execute` 每次新 thread；`amp threads continue … --execute` 续。"Execute mode and other noninteractive thread creation default to Standard mode each time." https://ampcode.com/docs/cli/execute-mode 。
- ⑦ 证据：官方 docs + news（2025-07-24 … 2026-05-06）。完整键位列表不在网上（"run `amp config keymap`"）。

### 1.9 GitHub Copilot CLI（changelog 最新 1.0.96 2026-10-12）

- ① **`/clear [PROMPT]`、`/new [PROMPT]`、`/reset [PROMPT]` 同一行，`/clear` 领头；1.0.11 起二者分化。**【官方】"/clear [PROMPT], /new [PROMPT], /reset [PROMPT] — Start a new conversation." https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-command-reference ；changelog 0.0.381 (2026-01-13) "Add `/new` as an alias for `/clear` command"；1.0.11 (2026-03-23) "/clear now abandons the current session entirely, while /new starts a fresh conversation (keeping the old session backgrounded)"；"/new and /clear commands accept an optional prompt to start the new session with a first message"；1.0.33 加 `/reset` 别名 https://raw.githubusercontent.com/github/copilot-cli/main/changelog.md 。`/new worktree` 在新 worktree 开空会话。快捷键：会话侧栏 `n` / `+`；"Ctrl+L — Clear the screen."；Ctrl+X 后 `x` 关会话。
- ② 「新对话」；0.0.333 "properly clear old timeline entries/session information"；0.0.336 "reset the context truncation tracking state"；hooks："`/clear` closes the old session and fires its `sessionEnd` hooks with `reason: \"user_exit\"` while the CLI keeps running." https://docs.github.com/en/copilot/reference/hooks-configuration 。磁盘 `~/.copilot/session-state/<id>/events.jsonl`；`/clear` 是否删磁盘会话：【查不到】（删除另有 `/session delete`）。
- ③ `--resume`、`--continue`、`/resume` / `/continue [SESSION-ID]`；`/new` 把旧会话留后台、`/clear` "abandons"；旧会话是否仍在列表、新 session id 的逐字说明：【查不到】。
- ④ **changelog 明确跨 `/clear` 保留**：1.0.12 "/yolo path permissions persist after using /clear to start a new session"、"/clear preserves MCP servers in the new session"；1.0.44 "Tool permissions granted in autopilot mode are preserved after /clear"；1.0.3 "/add-dir directories persist across session changes like /clear and /resume"；0.0.412 "/clear preserves agent mode (autopilot, plan, or interactive)"；1.0.40 "/clear and /new reset the active custom agent selection"；"starting a new session with `/new` sandboxes commands again."。会话级 `/model`、cwd：【查不到】。
- ⑤ 1.0.5 "Terminal title resets to default after running /clear or /new"；1.0.66 "Session limits now apply across the current conversation, reset on /clear"；1.0.85 "Run sessionEnd hooks when /clear closes an interactive session"；1.0.64 "Remote export keeps running after /clear"；1.0.40 "Switching sessions with /new or /resume no longer carries over pending messages to the new session"；自动 compaction ~80%。清屏：【查不到】。
- ⑥ `-p` 单次即退；SDK compatibility 页 "Clear conversation | `/clear` | TUI-only"，"Context clearing | `session.rpc.history.clearContext()`" https://docs.github.com/en/copilot/how-tos/copilot-sdk/troubleshooting/compatibility 。
- ⑦ 证据：docs.github.com + 官方 changelog.md + Release v1.0.96。

### 1.10 共性与分歧（本文的归纳）

1. **形态**：六家做成斜杠命令（Claude Code、Codex、Gemini、Cursor、Copilot、opencode），两家做成快捷键/命令面板（Crush、Amp），一家的 `/clear` 根本不换会话（Aider：只清内存历史，文件/模型全留）。
2. **名字**：接受 `/new` 的有 Codex（主名）、opencode（主名）、Claude Code、Gemini、Cursor、Copilot（别名）；`/clear` 作主名的是 Claude Code、Gemini、Cursor、Copilot、Aider。**同一个词在不同家意思不同**：Codex 的 `/clear` 额外清终端；opencode 的 `/clear` 额外关 tab；Copilot 1.0.11 起 `/clear` 弃旧会话、`/new` 留后台；Aider 的 `/clear` 不换会话。`/reset` 只有 Claude Code、Copilot（别名）和 Aider（另一个意思）。
3. **Ctrl+L 在每一家都只是清屏/重绘，不换会话**（Claude Code、Codex、Gemini、Aider、Cursor、Copilot 文档都明说；Claude Code 2.1.238 之前双击曾等于 `/clear`，已移除）。
4. **旧会话一律保留、可找回**：Claude Code、Codex、Gemini、Copilot（`/new`）、opencode、Crush 留在磁盘/数据库；Amp 归档；Aider 的文件继续追加。没有一家把「新会话」做成删除。**新会话拿新 id**：Codex、Gemini 源码明确；Claude Code、Copilot 文档说「新会话」「独立生命周期」，id 的逐字表述只在 issue 里。
5. **保留什么**（有据的）：cwd——Codex、Gemini 保留，Claude Code 实测回到启动目录，opencode 默认回启动目录；MCP——Codex（共享）、Gemini、Copilot、Claude Code（实测）保留；权限——Codex、Copilot 连会话内的改动一起保留，Gemini 未触碰；模型——Gemini、opencode、Crush 保留，**Codex 不保留**（回到配置默认），Claude Code/Copilot 查不到；指令文件（AGENTS.md/CLAUDE.md/GEMINI.md）——都重新加载；后台任务——Claude Code 杀前台留后台 agent，Copilot 远程导出继续跑，Crush 停 LSP。
6. **计量归零**：Claude Code（cost，2.1.211）、Gemini、Codex、Copilot（limits）归零；opencode、Crush 是「无会话即不显示」；Aider 不归零。
7. **屏幕**：Codex、opencode、Crush 是 alt-screen 全屏 TUI，换会话就是换画面；Gemini 在非 alt-buffer 模式真写 clear 转义；Aider 不清；Claude Code 经典渲染器、Cursor、Copilot 查不到。**inline 滚动型程序里唯一有定论的是 Aider：不清。**
8. **headless**：没有一家在非交互模式里需要这条命令——一次调用就是一个新会话（Codex exec、opencode run、crush run、amp -x、aider --message、claude -p），要续才给 `resume`/`--continue`。Claude Code SDK 文档直说 one-shot "has no practical effect"。
9. **生命周期事件**：Claude Code、Gemini、Copilot 都把它建模为 SessionEnd(clear) + SessionStart(clear/new)。
10. **参数**：Codex `/new <name>` 给新线程命名；Claude Code `/clear [name]` 给**旧**会话贴标签；Copilot `/new [PROMPT]` 当第一条消息。三家三个意思。

---

## 2. iota 的现状

坐标都指 `287a17a`。「实跑」指本机 `cargo build` 后跑 `./target/debug/iota`。

### 2.1 没有的东西

- **没有 `/new`、`/clear`、`/reset`，也没有 `/quit`、`/exit`。** 命令表是常量数组 `BASE`（`src/repl/commands/mod.rs:61-93`：`/file` `/session` `/model` `/compact` `/export` `/status` `/tools` `/debug`），加条件行 `/skills`（`:97-100`）、`/save`（`:103-106`）、`/jobs`（`:109-112`）、`/edit` `/redo`（`:116-123`）。`docs/history/bot-mode/bot-mode-recon.md:152` 已记录「`/new` 不存在……开新对话 = 退出重开 `iota`」。
- **没有别名机制。** `CmdSpec { value, desc }` 只有名字和描述（`src/repl/commands/mod.rs:53-56`）；匹配是 `input == cmd || input.starts_with(cmd + " ")`（`match_cmd`，`:257-265`）。
- **未知的 `/xyz` 是普通消息**，会原样发给模型（`src/repl/commands/mod.rs:12-13`「unknown `/xyz` text is a normal message」）。bot 里输入 `/session` 就是这个下场：`run.rs:806` 的分发臂挂在 `session_enabled()` 上（`commands/mod.rs:235-238`，bot 为 false），不命中就一路落到消息路径。
- **没有「清空转录区」的原语。** `Ui` 只有追加接口（`src/ui/facade.rs:891`、`:894`）；全仓唯一的 `clear()` 只擦底部 frame 的几行（`src/ui/runtime/inline_term.rs:423-431`，T-32 强制重绘），调用者只有 `src/ui/runtime/event_loop.rs:1029-1031`。`inline_term.rs:1-4`：「nothing here ever clears the screen」。
- **没有 `-S/--session`、`--temp` 这类 flag**（`src/cmd/args.rs:6-7` 列了退役的 flag；临时会话只有 `--no-save` 与 agent 的 `no_save:`）。

### 2.2 会话怎么开始

- 入口链 `main` → `cmd::run` → `run_agent`（`src/main.rs:38-41`；`src/cmd/mod.rs:148-155`）。子命令封闭：`run | list | resume | config | mcp | version`，无参 `iota` 就是 `run`（`src/cmd/args.rs:70-76`）。实跑 `./target/debug/iota --help`：

  ```
  run      Run an agent — interactively, or headlessly with -m
  list     List what the config declares (agents, models, providers) or the saved sessions
  resume   Resume a saved session
  ...
      --no-save    Start ephemeral: nothing persists unless you run /save in the chat
  ```

- `run` 与 `resume` 共用 `RunArgs`（`-m` `-M` `-s` `--mcp` `--no-save` `--max-turns` `--output-format`，`src/cmd/args.rs:316-361`）。`iota run [AGENT]` 的位置参数是 `agents:` 条目；`iota resume [ID]` 无 agent 参数，`of_resume` 固定 `agent: None`（`src/cmd/args.rs:430-441`），于是 resume 的 agent **永远**来自当前配置的 `agents.default`（`src/cmd/resolve.rs:133-137`；`src/session/meta.rs:134-140`：meta 里的 `agent` 「is a RECORD, not a replay instruction: a resume reassembles from the CURRENT config (decision of 2026-09-09)」）。
- 交互分支的前置顺序（`src/cmd/interactive/mod.rs`）：`--no-save`+resume 冲突（`:256-258`，文案 `src/cmd/error.rs:70-71`）→ 非 TTY 拒绝（`:263-265`）→ `ephemeral = no_save || (agent.no_save && resume.is_none())`（`:267`）→ 建 store 与 scope（`:274-275`）→ 选择器行（`:278-290`）→ MCP 后台连接（`:296-307`）→ `open_ui`（`:312-330`）。
- `wire_session` 三分支：bot（`:500`、`:504-551`）、resume（`:552-603`）、新建（`:605-621`）。新建就是一笔 `store.create(NewSession { temperature, provider, cwd: session_cwd, project: mode == Agent, agent: agent_name, ..NewSession::new(kind, model) })`（`:606-619`）；`NewSession` 的字段在 `src/session/store.rs:50-65`。ephemeral 时不建 writer，改存一个延迟工厂 `SessionFactory`（`:622-649`），给 `/save` 用。
- **create 不碰磁盘**：`SessionWriter::pending`（`src/session/store.rs:287-320`）；第一次 append 才 `ensure_created`（建 `attachments/`、取锁、开 log、首写 meta，`src/session/writer.rs:389-404`）。id 是 12 位 Crockford base32（`src/session/id.rs:10-25`），在 flat 与所有 bucket 里查重后才用（`src/session/store.rs:228-245`）。
- loop 起来后，对**本 run 新建**的 bundle 盖一次「现在跑在哪」：`fresh_bundle = !w.on_disk() || bot` → `w.move_to(&moved_now(repl))`（`src/repl/run.rs:478-486`、`:682-687`；`moved_now` 在 `src/repl/liveparams.rs:336-344`，把 provider 名、model、五参数+来源、window、image 设置一次写进 meta）。
- 启动顺序（`src/repl/run.rs`）：history 播种 `:439-448`（resume 用导入的；否则 `[system]` 或空；`persisted = resumed ? len : 0`）→ `ContextBudget::new` `:453` → meter `:461-469` → `CommandTable::new(CmdFlags{..})` `:489-496` → banner `:540-555` → resume 回放 `:556-569` → titler `:577-586` → `report_session()` `:676`。

### 2.3 启动时的选择器

- 只在 `iota resume` 无 id 且列表非空时弹（`src/cmd/interactive/mod.rs:278-290`）：列当前 scope 的 `store.list(scope)`（agent 模式只列项目桶，否则只列 flat），剔除被 bot 指针指向的会话。
- 规格：可搜索 List，标题 "Select a session to resume"，15 行（`src/cmd/interactive/picker.rs:11-14`、`:17-29`）；行文本 `title · model · 时间 · N msgs`（`src/repl/commands/session.rs:30-48`）。实跑 `iota list sessions` 的行形态：`1e8t8hgf5h13  <title> · deepseek-flash · 9m ago · 4590 msgs`。
- **没有「new」入口**（`picker.rs:22` 只有会话行）；空库不弹；cancel 走 `NoSessionToResume`（`src/cmd/interactive/mod.rs:212-217`、`:475-477`；文案 `src/cmd/error.rs:190-192`）。

### 2.4 bundle 里有什么

- 布局（`src/session/mod.rs:7-11`）：`<root>/<id>/`（agent 模式 `<root>/projects/<slug>/<id>/`）下 `meta.json`、`messages.jsonl`、`attachments/<sha256>`、`images/`。root = `~/.iota/sessions`（`src/app/mod.rs:58-61`）。slug = 项目根路径把分隔符换成 `-`（`src/session/store.rs:196-204`）；桶只对 `mode: agent`（`project: settings.mode == AgentMode::Agent`，`src/cmd/interactive/mod.rs:606-619`）。bot 会话永远 flat，由 `~/.iota/bots/<name>/bot.json` 指向（`src/session/store.rs:455-464`）。
- `meta.json`（`SessionMeta`，`src/session/meta.rs:76-157`，schema v2）：`v id created_at updated_at provider model temperature? top_p? max_tokens? context_window effort image aspect_ratio image_size negative_prompt json_edits cwd title agent param_sources? message_count extra`。写法 tmp+rename（`:170-177`）。
- `messages.jsonl` 的记录 role 只有 `system | user | assistant | tool | compaction`（`src/session/record.rs:24-79`，`ROLE_COMPACTION` `:18`）。**没有参数变更记录、没有标题记录**——参数与标题只在 `meta.json`，入口是 `update_meta`（`src/session/writer.rs:352-359`）、`move_to(&Moved)`（`:369-379`）、`clear_system()`（`:292-304`）。
- compaction 标记：追加一条 `role:"compaction"`，`content` = 摘要，`compacted_through = conv_count - retain_tail`；原记录留在 log 里，不增 `message_count`（`src/session/writer.rs:312-338`）。
- 锁：`<bundle>/.lock` flock，拥有 `SessionWriter` 的进程持有（`src/session/lock.rs:55-60`、`:77-93`）；新建在物化时取（`writer.rs:397-399`），resume 在读 meta 之前先取（`src/session/store.rs:335-336`）；`HeldLock` 的 Drop 解锁（`lock.rs:44-51`），`SessionWriter` 没有 `close()`，Drop 即关句柄（`writer.rs:8`）。另一个进程 resume 被占用的会话得到 `Locked`："session <id> is open in another iota process (pid N)"（`src/session/error.rs:44-53`）。

### 2.5 会话怎么结束

- 退出 = 空闲时 Ctrl+C 或 Ctrl+D：UI 把 `read_input` 的等待者失败为 `Interrupted`（`src/ui/input/keys.rs:45-50`），loop 收到 `Err` 就 `abort_title` 并 `break Ok(())`（`src/repl/run.rs:741-746`）。
- **退出时不做最后一次 persist，也不重写 meta**；持久化在每轮之后（`src/repl/run.rs:1009` 成功轮、`:1341` 中断保留、`:1387` 截断保留、`:298` notice），以及 `/save`（`src/repl/commands/save.rs:53`）、`/compact`（`compact.rs:339`）、`/model` 搬家前（`model.rs:244`）。loop 结束只做 `jobs.kill_all()`、`pres.close()`（`src/repl/run.rs:1030-1035`）；writer 随 `Repl` 析构（关句柄、解锁）。
- 二进制侧收尾：`ui.close()` → drop UI → 弹终端标题栈 → 取消 MCP 连接 → `manager.close()`（`src/cmd/interactive/mod.rs:390-394`）。
- 退出不打印告别文本。

### 2.6 `resume`

- `store.resume`（`src/session/store.rs:330-360`）：取锁 → 读 meta → `load_log` → 以 append 打开 log → `repair_tail`（未回答的 tool call 合成 "interrupted: no result was recorded"，`src/session/loader.rs:286-300`）。
- 视图重建 `load_log`（`src/session/loader.rs:302-394`）：**最后一条** compaction 标记生效（`:326-332`）；system 取最后一条 system 记录（`:336-348`）；视图 = `[system?] + conv[through..]`，摘要以 `"[Earlier conversation summary]\n"` 前置到第一条保留消息（`:374-385`；常量 `:25-27`）。`load_full_history`（`:264-275`）按 append 顺序读全部、跳过标记、不织摘要——给 `/export` 用。
- 参数恢复：`resume_endpoint`（`src/cmd/mod.rs:304-332`）：给了 `-M` 就留在 `-M` 的端点，否则搬到 `meta.provider:model`；`replay_session_settings`（`src/session/tuning.rs:51-63`、`:89-112`）整体以 `meta.recorded_on(on)` 为门，provider 不同一个都不回放、只给一条 warning（`:98-103`）；五个分层参数 `layered_params`（`src/cmd/mod.rs:360-382`）同 provider → `declared.resume(meta, true)` 原样恢复值与来源。交互 resume 之后**立刻** `w.move_to(moved)` 写一次 meta（`src/cmd/interactive/mod.rs:563-583`）。
- 进入 loop 后：banner 的 mode 行显示 `resumed <id>`（`src/repl/render/banner.rs:156-160`）；回放最后 3 轮（`RESUME_ECHO_ROUNDS = 3`，`src/repl/render/replay.rs:26`；`last_rounds` `:99-118`；`echo_rounds` `:142-241`，工具活动折成 `◇ ran N tool(s)`）；`ctxm.seed_totals(w.usage())` 播种会话累计 ↑↓（`src/repl/run.rs:480-486`）。
- headless 的 resume（`iota resume <id> -m`）必须给 id（`src/cmd/mod.rs:185-190`），成功后一次性 `move_to + append_messages(delta)`（`:670-679`）；失败不写。非 resume 的 `-m` 不落盘（`:608-611`）。

### 2.7 `/save`、`no_save` 与临时会话

- 三处定义：CLI `--no-save`（`src/cmd/args.rs:342-346`）；agent 的 `no_save:`（`src/config/agent.rs:45-47`「start ephemeral, as `--no-save` does」）；bot 禁止（`src/config/mod.rs:407-412`「no_save contradicts mode: bot」）。
- **ephemeral = 不落盘，不是落到临时位置**：writer 为 `None`（`src/cmd/interactive/mod.rs:606`、`:624`），`persist_turn` 直接返回 `true`（`src/repl/run.rs:253-255`）；banner 显示 `not saved · /save keeps it`（`src/repl/render/banner.rs:40`、`:156-160`）；`/status` 的 Session 行显示 `not saved (ephemeral)`（`src/repl/commands/status.rs:194-201`）。
- `/save` 只在启动即 ephemeral 时注册（`CmdFlags.save = new_session.is_some()`，`src/repl/run.rs:490`；`src/repl/commands/mod.rs:221-223`），flag 之后不翻转；二次 `/save` 靠 `writer.is_some()` 守卫："Session already saving (<id>)."（`src/repl/commands/save.rs:20-25`）。
- `/save [title]`（`src/repl/commands/save.rs:19-67`）：调工厂 `store.create` 铸 writer（此刻才分配 id）→ `w.move_to(&moved_now(repl))` 盖上**当下**的端点/模型/五参数+来源（`:43-49`）→ `persist_turn()` 一次 append 整个 backlog（水位一直是 0，`:50-53`）→ `report_session()` → 标题 `adopt_name` / `reapply`。**这是 REPL 里「晚铸 bundle」的先例。**
- 限制：ephemeral 且模型已回复的 chat 不能 `/model` 迁端点，提示 "Save it with /save, then pick the model again."（`src/repl/commands/model.rs:233-242`）。

### 2.8 bot 模式（永不结束）

- 设计已定：`docs/design/bot-mode.md:13`「bot 的内核是**一条永不结束的会话**——跨天跨周活着，随时接着聊，不需要 `/new` 也不需要 resume」；`:95`「进程退出不等于会话结束……没有「结束会话」这个概念。**不加 `/new`**；想要一段新对话，就用另一个 agent，或者把 bot 改名」；`:667` 的决策表第 9 行「重开对话 … **不提供** … 真要重来：删 `bot.json`（旧 bundle 变回普通会话）或换 bot 名」。
- 代码：一个 bot 固定指向一个会话 id（`src/session/bot.rs:12-19` `BotPointer{session}`），启动总是 resume-or-create（`src/cmd/interactive/bot.rs:132-192`），bundle 启动即落盘（`fresh_bundle = … || bot`，`src/repl/run.rs:484`）。bot 锁 + bundle 锁，第二个进程被拒（`src/session/writer.rs:74-79`）。
- bot 下不存在的东西：`/session`（`src/repl/commands/mod.rs:191-193`）、`/save`（`src/repl/run.rs:490`）、标题 pass（`:584`）、自动压缩的 Confirm（`src/repl/commands/compact.rs:445-447`、`:461-465`，bot 走状态机直接压）、换端点的 `/model`（`src/repl/commands/model.rs:349-358`「this chat keeps the endpoint it started on」；`conv.endpoints` 在 bot 为 `None`，`src/cmd/interactive/mod.rs:360`）；`iota resume <bot 的 id>` 被拒（`src/cmd/mod.rs:548` `check_not_bot_owned`）。

### 2.9 转录区的分块与分界

- **inline 滚动，不是 alt-screen**：`docs/design/ui-architecture.md:7-10`「a pinned bottom frame … with history flowing above it into native scrollback via `Println`. The terminal's own scrolling is never taken over」；`src/ui/render/region.rs:4-5`「older lines overflow into real scrollback」。
- 「块」= `BlockKind { None, User, Activity, Content, Ask, Notice, Error, Echo, Image }`（`src/repl/render/transcript.rs:27-49`）；`Transcript` 是转录区的单写者（`src/repl/state.rs:169`），每块开头付一个空行、块内空行延迟（`transcript.rs:88-97`）。活动组（thinking + tool 调用）收口成一行摘要（`src/repl/render/group.rs:84-101`）。`docs/design/ui-architecture.md:235-236`：「Direct `u.PrintLines` writes from chat code are an architecture violation — everything goes through the transcript」。
- 现成的「分界」只有两样：启动 banner 卡片（`src/repl/render/banner.rs:93-126`；mode 行 `chat · session <id>` / `resumed <id>` / `not saved · /save keeps it`，`:153-173`），打在 Program 接管之前（`banner.rs:12-15`；`src/repl/run.rs:553-555`）；以及 `/session` 换会话时的一条 dim notice "Resumed session <id> (N messages)" + `tr.echo` 回放（`src/repl/commands/session.rs:247-250`、`:257-270`）。`notice()` 连续合并成一块（`transcript.rs:182-184`）。
- `/export` 读的是磁盘全量，不是屏幕也不是内存视图（`src/repl/commands/export.rs:705-715`：on_disk 走 `load_full`，ephemeral 才用内存 history）；文件名 `iota-<slug|id>-<time>.<ext>`（`:4`）。所以**清屏不影响导出，只影响回看**。

### 2.10 token 计量

- 两个量：`settled`（API usage 的 `context_tokens()`，或无 usage 时本地计数）与 `pending`（本地估算）（`src/repl/context/meter.rs:8-15`、`:84-93`）；本地 tokenizer `o200k_base`（`src/repl/context/tokens.rs:80-82`）。
- 常量（`src/repl/context/tokens.rs`）：`DEFAULT_CONTEXT_WINDOW = 128_000`（`:27`）、`COMPACT_THRESHOLD_PERCENT = 80`（`:39`）、`COMPACT_RESERVE_TOKENS = 16_000`（`:43`）、`COMPACT_SNOOZE_PERCENT = 5`（`:67`）；阈值 `max(window×80%, window−16k)`（`meter.rs:103-106`）。
- 归零/重算的时刻：轮开始 `ctxm.reset()`（`src/repl/turn/mod.rs:355-357`）、每 round 结束 `settle`（`src/repl/turn/tools.rs:154-156`）、轮成功 `budget.update`（`src/repl/run.rs:1010-1013`）、压缩成功 `budget.reseed`（`src/repl/commands/compact.rs:410-412`）、`/session` 切换 `budget.reseed` + `ctxm.seed_totals(usage)`（`src/repl/commands/session.rs:231-232`）。会话累计 ↑↓ 只增不减、resume 由日志总和播种（`meter.rs:406-409`、`:488-492`）。
- 状态行：`↑ N (x% cached) ↓ N  {≈}{pct}% / {window}`（`src/ui/render/frame.rs:259-281`），>90% 红、>70% 黄（`:383-391`）。无 usage 的 provider 没有 `/compact`（`CmdFlags.compact = token_aware && ctxm.is_enabled()`，`src/repl/run.rs:489-496`）。

### 2.11 `/compact`

- `compact_now`（`src/repl/commands/compact.rs:324-424`）：先 `persist_turn()`，失败即 "Compaction failed: the conversation is not saved yet"（`:339-344`）→ 转录区 raise 一行 `Compacting context…`（`:347-349`）→ 摘要成功后 `w.append_compaction(...)`（`:385-389`）→ `history = 新视图`、`persisted = history.len()`（`:407-409`）→ `budget.reseed`（`:410-412`）→ `compact_declined = 0`（`:413`）→ notice "Context compacted → {status}"（`:414-417`）。**转录区旧内容不动**；会话 id 不变；摘要留在上下文里。
- 自动触发在发送前（用户消息入 history 之前）：`offer_before_send`（`src/repl/run.rs:877-884`；`compact.rs:448-484`）弹 Confirm "Context {} — compact before sending?" / "Compact now" / "Not now"；拒绝则记 `compact_declined`（`src/repl/state.rs:56-58`）。
- 文档 `docs/design/context-compaction.md` §8（`:140-153`）：Event Store 派生视图，resume 复用摘要不重算。

### 2.12 模型与参数的分层求值（`param_sources`）

- 五个分层参数 `context_window effort temperature top_p max_tokens`，三层：① `agents:` 条目 ② 当前跑的 `models:` 条目 ③ 会话当前值（仅当来源是 `User`/`Builtin`；`Config` 来源的值在换模型时丢弃）（`src/config/params.rs:4-22`）。原文：「Exactly THREE moments evaluate: a new session starting, a `/model` model switch, and a move to another endpoint」；「a resume on the endpoint the bundle records RESTORES the bundle's values and tags rather than re-deriving them」。
- `ParamSource = Builtin | Config | User`（`src/session/params.rs:19-32`）；`ParamSources` 五字段（`:44-66`），缺键的旧 bundle 全按 `User`（`:84-90`）。值不在 REPL 里——`context_window` 住 `ContextBudget`，四个 tunable 住 provider；REPL 只持来源 map `conv.param_sources`（`src/repl/state.rs:46-51`；`src/repl/liveparams.rs:20-26`）。
- 写入来源：`models:`/`agents:` 条目 → `Config`（`params.rs:92-100`）；`/model` 的 Context/Effort/Temperature tab → `by_hand` → `User`（`src/repl/commands/settings.rs:548-559`）；`top_p`/`max_tokens` 没有 tab 也没有 flag（`liveparams.rs:128-139`）。
- 启动求值入口 `cmd::layered_params`（`src/cmd/mod.rs:360-382`）：无 resume → `declared.evaluate(&LayeredParams::default())`；resume 同 provider → `declared.resume(meta, true)`；异 provider → `land(provider, &declared.resume(meta, false))`。
- `/status` **不展示来源**，只列 Temperature/Effort 的值（`src/repl/commands/status.rs:119-131`）；来源只在 meta.json 的 `param_sources` 和切模型时每个 knob 的 dim notice 里可见（`liveparams.rs:183-238`）。
- `ParamLayers`（tier 1/2 的快照）与 `ModelCatalog` 按 run 建（`src/cmd/interactive/mod.rs:730-757` 的 `EndpointFactory`）；REPL 看不到 `Config`/`Env`（`src/cmd/interactive/mod.rs:357-360`「the loop deliberately knows nothing about」），只能通过 `EndpointFactory(provider, id)` 换端点。

### 2.13 `/model` 的三种结果（最近的「换」先例）

- `ModelCatalog::pick` 分 `Here`（同端点）/`Elsewhere`（`src/repl/catalog.rs:193-198`）；`cmd_model` 对 `Here` 走 `commit`（`provider.set_model` + `update_meta`，`src/repl/commands/model.rs:144-149`）再 `switch_model` 重新求值五参数（`src/repl/liveparams.rs:75-80`）；对 `Elsewhere` 走 `move_to`（`model.rs:194-228`）。
- `move_to` 的流程：已回复则先 `saved()`（ephemeral 拒绝）→ `open_endpoint`（用 `EndpointFactory` 建新 provider/layers/catalog）→ `consent`（"Move this chat to {provider}:{id}?" / "Move it" / "Stay on {here}"，`:296-313`）→ `reevaluate` → `land` → `w.move_to(&moved)` 一次 meta 写 → `install`（换 provider/layers/catalog/title provider，并 `abort_title`，`:385-393`）→ `history = 按目标类型重读的 log` + `budget.reseed`。**不换 id、不复制 bundle、不动 dispatcher/MCP/jobs**（`:192-193` 注释）。
- 拒绝的出路 `way_out`（`model.rs:151-174`）：有 on-disk bundle 且非 bot → "This chat continues there in a new run: iota resume <id> -M provider:id"；否则 → "A NEW session starts there with: iota run <agent> -M provider:id"。**代码已经把 `iota run` 当作「新会话」的同义词。** CHANGELOG Unreleased：`/model` 行不再标 `(moves the chat)` / `(needs a new session)`，「The pick reports itself」。

### 2.14 `/session` 的 swap（最近的「换会话」先例）

- 两页 tabbed：Resume（单选）/ Delete（多选）；无重命名、无新建（`src/repl/commands/session.rs:1-13`、`:87-132`）。选中当前会话 → "Already in this session."（`:164-172`）。
- swap 顺序（`:217-233`）：`usage = writer.usage()` → `titler.switch_writer(writer)`（旧 writer drop → 关句柄、解锁；采用新 meta 的标题，`src/repl/title.rs:169-176`）→ `report_session()`（告诉宿主新 id/dir；herdr 收 `pane.report_agent_session`，`src/host/herdr.rs:72-74`、`:124-126`）→ provider 不同则 `install(ep)` → `history = resumed.messages` → `persisted = len` → `budget.reseed` → `ctxm.seed_totals(usage)` → `pending.clear()` → `replay_session_settings`（`:235-246`）→ notice "Resumed session <id> (N messages)"（`:247-250`）→ 回放 3 轮（`:257-270`）→ `push_status()`。
- **它没做的**（新命令要注意）：不更新 `conv.param_sources`（回放了值但来源 map 还是旧会话的——可疑点，见 §3.8）；不清 `compact_declined`；不动 dispatcher/MCP/jobs/`ApprovalGate`/`reqlog`；不 `ctxm.reset()`。

### 2.15 MCP、工具与其他 run 级状态

- MCP 在 picker 之前 `Manager::new` + `connect_background`，loop 退出后 `manager.close()`（`src/cmd/interactive/mod.rs:292-307`、`:390-394`）；生命周期 = 进程。`/session` 与 `/model` 搬家都不碰它。
- 工具集 `build_dispatcher` 在 wiring 时**建一次**（`src/cmd/assemble.rs:157-210`），放进 `Conversation.dispatch`（`src/repl/state.rs:36-37`）；`/tools` 是纯查看器（`src/repl/commands/tools.rs:192-215`）；meta 里没有 tools/mcp 字段。
- defer 模式的「已加载」集合 `LoadState { enabled, pending }` 在 `DeferDispatcher` 内存里（`src/tool/defer/mod.rs:171-201`），**没有 reset API**；`system-tools` 模式把 mount 作为空 system 消息追加进 history（内存，不写 log：`src/repl/turn/tools.rs:172-181`；`src/session/writer.rs:243-247`）。
- 后台 jobs（`Jobs`）按 run 建（`src/cmd/mod.rs:435-441`），loop 退出 `kill_all()`（`src/shell/jobs.rs:560-572`）；完成通知以 `InputKind::Notice` 排队，**作为 notice 消息进 history 并发给模型**（`src/repl/run.rs:692-697`、`:864-866`、`:890-894`）。
- `ApprovalGate` 的 "allow for this session" 一个 run 一个（`src/repl/turn/approval.rs:1-3`；`src/repl/state.rs:179-181`「The conversation's ONE approval gate」）。
- `/skills <name>` 不是「已加载集合」，是一条 user message（`src/repl/commands/skills.rs:207-219`）；skills catalog 与 AGENTS.md overlay 每次发送前 refresh（`src/repl/run.rs:1105-1131`）；harness 换日重算（`:1208-1223`）；三者都不进 history（`src/agents/mod.rs:304-364` `compose_send_history`：发送时才拼 harness + `<instructions>history[0]</instructions>` + overlay 的副本）。
- system prompt 本身只在 `history[0]`（`src/repl/run.rs:439-448`）；`Conversation` 没有 `system` 字段；没有 `/system` 命令，`/model` 的 System tab 只读（`src/repl/systemtab.rs:18-36`）。
- cwd/root 启动算一次，进程从不 chdir（`src/app/mod.rs:47-56`；`src/agents/mod.rs:31-44`；`grep set_current_dir src` 无结果）；shell 工具每次调用新 shell，`cd` 不延续（`src/tool/builtins/shell.rs:199-211`）。

### 2.16 流式输出中的命令

- 循环是串行的，`read_input` 只在轮与轮之间（`src/repl/run.rs:740-741`）；轮内输入由 UI 排队，steering 只注入连续的非 `/` 前缀，遇到斜杠命令即停（`src/ui/runtime/event_loop.rs:334-341` `is_steerable`；注入点 `src/repl/turn/tools.rs:162-170`）。`docs/design/ui-architecture.md:295-299`：「**Mid-turn command dispatch is scoped OUT of v1.** Every real command mutates state the in-flight turn uses (/model ↔ provider, /compact ↔ history, /session ↔ session writer). Commands queue like messages and run between turns.」
- ESC/Ctrl+C 中断走 `finalize_interrupt` 三态（`src/repl/turn/interrupt.rs:33-73`），部分回复以 `interrupted: true` 落盘。

### 2.17 headless

- 唯一入口 `once`（`src/headless/mod.rs:1-8`）：一条消息进、回复或 JSON 出；`-m` 本身就是一次性的新会话（不落盘，`src/cmd/mod.rs:608-611`），`iota resume <id> -m` 才接旧会话。没有 meter、没有压缩（`src/cmd/mod.rs:566-568`）。

---

## 3. 对齐提案

这一节是本文的判断。每条主张后面标依据：**【先例】**别家的做法（§1）或 iota 自己的机制（§2）；**【推断】**本文的推理。要所有者拍板的点集中在 §3.7，这里只给推荐。

### 3.1 名字与别名

**推荐：主名 `/new`；`/clear` 作为不列在补全表里的别名；不加 `/reset`；CLI 侧不加任何东西。**

- 为什么叫 `/new` 不叫 `/clear`：
  - iota 的转录区是 inline 滚动、进真 scrollback、没有清屏原语（§2.1、§2.9）；旧会话也不删（§3.2）。叫 `/clear` 会撒两次谎：既没清屏，也没清掉会话。【推断】
  - 代码已经把「新会话」叫 `iota run`：`/model` 拒绝时的出路文案是 "A NEW session starts there with: iota run <agent> -M …"（`src/repl/commands/model.rs:151-174`）。REPL 里的同一件事叫 `/new`，口径一致。【先例】
  - 别家：Codex、opencode 以 `/new` 为主名；Claude Code、Gemini、Cursor、Copilot 都接受 `/new`；**`/new` 是唯一每一家都认的词**，而 `/clear` 在 Codex（额外清终端）、opencode（额外关 tab）、Copilot（弃会话）、Aider（不换会话）各有一个不同的意思（§1.10 第 2 条）。【先例】
- 为什么还要收 `/clear`：iota 对未知的 `/xyz` 是原样发给模型（`src/repl/commands/mod.rs:12-13`）。Claude Code/Gemini/Cursor/Copilot 的肌肉记忆会敲 `/clear`，让它变成一条发给模型的消息比当成 `/new` 糟。【推断】但不列进补全表——表里只有一个名字，避免用户以为两条命令不一样（Claude Code 的做法：别名只用于匹配，高亮的仍是主名）。【先例】
- 实现上别名只是 `match_cmd` 多匹配一个字符串（`src/repl/commands/mod.rs:257-265`），不需要改 `CmdSpec`。【推断】
- 不加 `/reset`：只有 Claude Code、Copilot 当别名，Aider 的 `/reset` 是另一回事；iota 没有「文件集」这种需要 reset 的东西。【先例】
- 描述文案（补全表里的一行，对齐现有口吻 `src/repl/commands/mod.rs:61-93`）：`/new` — "Start a new session; this one stays on disk"。ephemeral 时同一句不对，见 §3.3。
- CLI 侧：`iota run` 就是新会话，`iota resume` 就是接旧会话（§2.2）。`/new` 与 `iota run` 的差别要在文档里说清：

| | `iota run`（新进程） | `/new`（同一进程） |
|---|---|---|
| 配置 | 重新读 `~/.iota.yaml`：agent、models、tools、MCP 列表、system | **不重读**（REPL 看不到 `Config`，`src/cmd/interactive/mod.rs:357-360`）；用现在跑着的 |
| 模型与五参数 | 从配置重新求值（`src/cmd/mod.rs:360-382`） | 原样继承现在的值与来源（§3.2） |
| MCP | 重新连接 | 连接不断 |
| 后台 jobs | 上一进程退出时已 `kill_all` | 见 §3.7 第 6 点 |
| 工作目录 | 新 cwd | 同一个 cwd/root/bucket |
| 旧会话 | 已经在磁盘上 | 同样在磁盘上，锁立即释放 |

- headless 不需要对等物：`-m` 本来就是一次性新会话（`src/cmd/mod.rs:608-611`）；别家也没有一家在 headless 里要它（§1.10 第 8 条）。【先例】

### 3.2 语义：清什么、留什么、旧会话去哪、新会话继承什么

一句话：**`/new` = 在同一个进程里做一次 `iota run`，但不重读配置、不重连 MCP；旧会话留在磁盘上，随时 `/session` 或 `iota resume` 回来。**「清掉」不等于「丢掉」——这是 iota 相对别家的强项（bundle + resume），要守住。

**旧会话**

| 步骤 | 做法 | 依据 |
|---|---|---|
| 先落盘 | `persist_turn()`；失败就拒绝，文案照 `/compact`："… failed: the conversation is not saved yet"（`src/repl/commands/compact.rs:339-344`）。轮与轮之间水位通常已追平，这一步只是兜底 | 【先例】`/compact`、`/model` 搬家都先 persist |
| 标题 | `abort_title()`；已落地的名字留在旧 meta；没落地的不补 | 【先例】`install` 换端点时就 `abort_title`（`src/repl/commands/model.rs:385-393`） |
| 释放 | 旧 `SessionWriter` drop → 关句柄、解锁（`src/session/writer.rs:8`；`src/session/lock.rs:44-51`） | 【先例】`/session` swap 就是这么换 writer 的（`src/repl/commands/session.rs:221-222`） |
| 不写任何「结束」标记 | 退出本来也不写（§2.5）；jsonl 没有这种记录类型（§2.4） | 【先例】 |
| ephemeral 的旧会话 | 从来不在磁盘上，`/new` 之后就没了——所以已回复时要拦一下，见 §3.5 | 【推断】 |

**新会话**

| 项目 | `/new` 之后 | 依据 |
|---|---|---|
| id / bundle | 新 id、新 bundle，**懒建**（首次 append 才落盘，与启动一样，`src/session/store.rs:287-320`）；同一个桶（agent 模式 `projects/<slug>/`，`src/cmd/interactive/mod.rs:606-619`） | 【先例】启动；`/save` 晚铸 |
| meta | 立刻 `move_to(&moved_now(repl))` 盖上现在的端点/模型/五参数+来源/window/image（`src/repl/liveparams.rs:336-344`） | 【先例】启动 `fresh_bundle`（`src/repl/run.rs:682-687`）与 `/save`（`save.rs:43-49`） |
| history | `[system]`——把当前 `history[0]`（若是 System）复制过来；否则空（`src/repl/run.rs:439-448` 的播种规则） | 【先例】启动播种 |
| `persisted` | 0（system 随首次 append 落盘） | 【先例】`persisted = resumed ? len : 0` |
| 附件 `pending` | 清空；非零时在 notice 里说一句 | 【先例】`/session` 清（`session.rs:233`）；Copilot 1.0.40 不带 pending 过去 |
| 计量 | `ctxm.reset()`、`budget.reseed(&history)`、`ctxm.seed_totals(0)`；状态行回到 0% | 【先例】`/session`（`session.rs:231-232`）；Claude Code/Gemini/Codex/Copilot 都归零 |
| `compact_declined` | 0 | 【推断】它是上一段对话的 snooze 水位（`src/repl/state.rs:56-58`）；`/session` 没清是遗漏 |
| 标题 | 未命名；首条用户消息后照常起标题（`src/repl/run.rs:896-897`）；窗口标题回应用名 | 【先例】启动；Claude Code 不保留 AI 标题，Copilot/Claude Code 重置终端标题 |
| 宿主 | `report_session()` 报新 id/dir（herdr 的 `pane.report_agent_session`，`src/host/herdr.rs:72-74`） | 【先例】`/session`（`session.rs:223`） |
| `param_sources` | 原样带过去，并随 `move_to` 写进新 meta | 【先例】`moved_now` 就是读它 |

**继承（不动）**

| 项目 | 为什么不动 | 依据 |
|---|---|---|
| 端点、模型 | 用户在这个 run 里选的；`/model` 只写 meta 不写配置，换 run 都要重选；别家多数保留（Gemini、opencode、Crush），Codex 回到配置默认是因为它的 `/model` 会写 config.toml（§1.2 ④ 查不到项） | 【先例】 |
| 五参数与来源 | 「when the config speaks the config decides; when the config is silent the hand-set value stands」（`src/config/params.rs:12-15`）；`/new` 不重读配置，所以 Config 来源的值不会变，User 来源的值按 tier-3 规则本来就该留 | 【先例】`switch_model` 的求值规则 |
| system prompt | 只活在 `history[0]`（§2.15）；复制过去就是继承 | 【先例】resume 也是从 log 取最后一条 system |
| harness / AGENTS.md / skills catalog / 记忆块 | 都不在 history 里，每次发送前 refresh（`src/repl/run.rs:1105-1131`） | 【先例】 |
| dispatcher / 工具集 / MCP 连接 | 按进程建（§2.15）；`/session` 与 `/model` 搬家都不碰 | 【先例】Codex/Gemini/Copilot/Claude Code 都留 MCP |
| cwd / root / scope | 进程从不 chdir（§2.15） | 【先例】Codex、Gemini 保留 |
| ephemeral 与否 | 跟当前会话一样：现在 `not saved` 就还是 `not saved`（工厂还在，`/save` 仍可用）；现在在落盘就继续落盘 | 【推断】见 §3.7 第 4 点 |
| `/debug` 录制开关、`RequestLog` | 按进程 | 【先例】 |
| mode / agent 名 | `conv.mode` "fixed for the run"（`src/repl/state.rs:71-72`）；换 agent 只能换 run（resume 都不能指定 agent，§2.2） | 【先例】 |

**要专门处理的两处残留**（实现时要补 API，不是所有者要拍板的事）：

- defer 模式的已加载集合 `LoadState` 在 `DeferDispatcher` 内存里、没有 reset（`src/tool/defer/mod.rs:171-201`）：不清的话，新会话第一轮就带着上一段对话 load 过的工具 schema。要加一个 `reset()`。`system-tools` 模式的 mount 随 history 清空自然消失。【推断】
- `SessionTitle` 没有「回到未命名」的 API（`src/repl/title.rs` 只有 seed/land/unseed/switch_writer/adopt_name/reapply）；`switch_writer` 又只接受非 ephemeral 的 writer（`title.rs:169-176`）。要加一个 reset。【推断】

### 3.3 和 iota 的特点对齐

**bot 模式：不提供，输入了要说明。** `docs/design/bot-mode.md:95`、`:667` 已经决定「不加 `/new`」，理由成立：bot 的内核就是一条永不结束的会话，记忆靠 MEMORY.md、窗口靠自动压缩。本文不翻案。但有一处要比现状好：现状 bot 里输入 `/session` 会被当普通消息发给模型（`src/repl/run.rs:806` 的臂挂在 `session_enabled()` 上，不命中就落到消息路径）。`/new` 应该**注册但拒绝**：不进补全表，输入了打一条 notice，学 `/model` 在 bot 里的口吻（"this chat keeps the endpoint it started on"，`model.rs:349-358`）——例如 "A bot's session never ends; to start over, run another agent or rename the bot (bot-mode.md §6 #9)."。【先例 + 推断】

**`no_save` 与临时会话：跟着走。** ephemeral 是「不落盘」而不是「落到临时位置」（§2.7）。`/new` 之后的新会话延续这个状态——工厂 `SessionFactory` 还在（`src/repl/state.rs:103-104`），`/save` 照旧能把新会话转正；`CmdFlags.save` 本来就不翻转（§2.7）。代价是 ephemeral 的旧会话真的没了，所以已回复时要拦（§3.5）。【推断】

**`/compact` 是「省上下文」，`/new` 是「换一段」。** 把边界写死：

| | `/compact` | `/new` |
|---|---|---|
| 会话 id | 不变 | 新的 |
| 磁盘 | 追加一条 `compaction` 标记，原记录全留（`src/session/writer.rs:312-338`） | 旧 bundle 不动；新 bundle 从 `[system]` 开始 |
| 上下文 | 摘要 + 最后一轮留在窗口里（`compact.rs:103-108`、`:156-193`） | 什么都不带 |
| 计量 | 降到摘要大小 | 归零 |
| 转录区 | 旧内容不动，加一行 "Context compacted → …" | 旧内容不动，加一行分界（§3.4） |
| 何时用 | 同一件事做不完、窗口快满 | 换一件事；或者这段对话污染了后面的判断 |
| 自动触发 | 发送前按阈值弹 Confirm（`src/repl/run.rs:877-884`） | 永不自动 |
| bot | 走状态机自动压 | 不提供 |

两者的关系：`/new` 之后 `budget` 几乎为零，自动压缩的 offer 不会出现；`compact_declined` 归零也是为此。`/new` **不**替旧会话做摘要、也不把摘要带进新会话——那是「handoff」，Amp 做过又拆了（"Handoff is gone … compaction made it obsolete"，§1.8 ⑤），Codex 用 `/recap` 另开一条命令。要不要做，列在 §3.7 第 11 点。【先例】

**转录区是可回看、可导出的滚动记录——清屏会破坏它。** 这是 §3.4 的结论的前提：`/export` 读的是磁盘全量（`src/repl/commands/export.rs:705-715`），清屏不影响导出；但 inline 滚动区是用户回看的唯一现场（没有 Claude Code 的 Ctrl+O 转录视图），清掉就没了。【先例】

### 3.4 屏幕行为：不清屏，打一条分界，继续往下滚

**结论：转录区不清空；`/new` 在转录区打一条 dim notice 作分界，之后新会话的内容接着往下滚。**

理由：

1. **没有原语，也不该造。** `inline_term.rs:1-4`「nothing here ever clears the screen」是 2026-09-24 UI 评估选 inline 方案时定下的；`region.rs:4-5` 把老行溢出到真 scrollback 当作设计目标。造一个清 scrollback 的原语等于推翻这个决定，而且终端对「清 scrollback」的支持本来就不齐（Codex 源码注释："Some terminals (Terminal.app, Warp) do not reliably drop scrollback when purge and clear are emitted as separate backend commands."，§1.2 ⑤）。【先例】
2. **别家清屏的都是 alt-screen 程序。** Codex、opencode、Crush 全屏，换会话就是换画面，不存在 scrollback；Gemini 是 Ink 整页重绘。inline 程序里唯一有定论的 Aider 不清。Claude Code 的经典渲染器查不到，但它的 Ctrl+L/Cmd+K 都只是重绘（§1.1 ①）。【先例】
3. **用户要清屏有终端自己的 Cmd+K / `clear`。** Claude Code 的做法正是让 iTerm2/Terminal.app 自己处理 Cmd+K，然后重绘（fullscreen 页 §Clear the conversation）。iota 什么都不用做。【先例】
4. **分界已有形状。** `/session` 换会话打的就是一条 dim notice "Resumed session <id> (N messages)" 加三轮回放（`src/repl/commands/session.rs:247-250`、`:257-270`）；启动 banner 的 mode 行是 `session <id>` / `not saved · /save keeps it`（`src/repl/render/banner.rs:156-160`）。`/new` 的分界用同一套词：落盘时 "New session <id>"，ephemeral 时 "New session · not saved · /save keeps it"，附件非零时追加 "(N attachments dropped)"。不重打 banner 卡片：banner 是在 Program 接管前直接写 stdout 的（`banner.rs:12-15`），从聊天代码里直接 `print_lines` 是架构违规（`ui-architecture.md:235-236`）。【先例】
5. 除了那一行，可见的变化在底部 frame：状态行回到 0%、窗口标题回应用名、宿主收到新 id。【推断】

### 3.5 边界

| 情形 | 行为 | 依据 |
|---|---|---|
| **正在流式输出时输入 `/new`** | 排队，等这一轮结束再执行；这一轮的输出落在**旧**会话里，然后才换。不打断、不拒绝 | 【先例】v1 的并发契约：命令排队、轮间执行（`ui-architecture.md:295-299`）；steering 遇到 `/` 就停（`event_loop.rs:334-341`）。Codex 是直接禁用，净效果一样：想立刻换就先 ESC |
| **连续两次** | 第二次落在一个还没有用户消息的新会话上 → no-op，notice "Already a new session."；不再铸一个 id | 【先例】`/session` 选当前会话 → "Already in this session."（`session.rs:164-172`） |
| **还没存盘（ephemeral）且已有回复** | 弹 Confirm："Discard this unsaved chat and start a new one?" / "Discard" / "Keep"。没回复过的直接换 | 【先例】自动压缩的 Confirm 形状（`compact.rs:469-483`）；`/model` 搬家对同一情形是拒绝（"Save it with /save, then pick the model again."，`model.rs:233-242`）——两种都成立，见 §3.7 第 3 点 |
| **落盘失败** | 拒绝，文案照 `/compact`（`compact.rs:339-344`） | 【先例】 |
| **带参数 `/new xxx`** | v1 不收参数：notice "Usage: /new"。参数的三种可能意思（给新会话命名 / 给旧会话命名 / 当第一条消息）别家各执一词（§1.10 第 10 条），先不选 | 【推断】见 §3.7 第 9 点 |
| **bot** | 不进补全表；输入了打拒绝 notice（§3.3） | 【先例】bot-mode.md |
| **headless** | 不存在（没有 REPL） | 【先例】 |
| **有后台 job 在跑** | 不杀；完成通知照旧作为 notice 进（新）会话的 history 并发给模型（`src/repl/run.rs:692-697`、`:890-894`）；`/new` 的分界行后面追加一句 "N background jobs keep running" | 【先例】Claude Code 留后台 agent，Copilot 远程导出继续跑；见 §3.7 第 6 点 |
| **标题 pass 还在跑** | `abort_title()`；旧 meta 保留已落地的名字 | 【先例】`install`（`model.rs:385-393`） |
| **"allow for this session" 的审批授权** | 保留（按进程，与 `/session` 一致） | 【先例】`/session` 不重置 gate；Copilot 1.0.12/1.0.44 保留；见 §3.7 第 7 点 |

### 3.6 和 `/session`、`/export`、`/status`、`/save` 的关系

- **`/session`**：旧会话出现在 Resume 列表里的条件是它已物化（至少 append 过一次，§2.2）——一个从没发过消息的会话在 `/new` 之后不会留下任何目录，这和启动后直接退出一样。从新会话 `/session` 回旧会话就是今天的 swap（`session.rs:217-271`），不需要新东西；旧会话的锁已随 writer drop 释放。【先例】
- **`/export`**：导出的是**当前**会话（新的），没内容时 "Nothing to export yet."（`export.rs:717-720`）；要导出旧会话，先 `/session` 回去。不给 `/export` 加「导出上一个」。【推断】
- **`/status`**：Session 行显示新 id（或 `not saved (ephemeral)`），Messages 行从 "1 in context"（有 system）或 0 开始，Title "(untitled)"（`status.rs:179-209`）。【先例】
- **`/save`**：在 ephemeral run 里 `/new` 之后仍可用，保存的是新会话；旧的 ephemeral 会话此时已不可挽回（这就是 §3.5 要弹 Confirm 的原因）。【推断】
- **`/compact`**：`/new` 之后立刻 `/compact` 会得到 "Nothing to compact yet."（`compact.rs:366-372`）。【先例】
- **`/model`**：`/new` 之后换端点的 `move_to` 不受影响——新 bundle 若还没物化，`saved()` 会先 persist 一次（`model.rs:233-255`）。【先例】

### 3.7 要所有者拍板的点

每点给选项与利弊；**推荐**只是本文的意见。

1. **主名。** (A) `/new`——与 Codex/opencode 同名、与 `iota run` 同义、不撒谎；但 Claude Code 用户第一反应是 `/clear`。(B) `/clear` 主名、`/new` 别名——跟 Claude Code/Gemini/Cursor/Copilot 阵营一致；但 iota 既不清屏也不删会话，名字与行为不符。(C) 两条都注册、补全表里都显示——最省解释；但两行一个意思，表里从此有「别名」概念。**推荐 A。**
2. **`/clear` 要不要当隐藏别名。** (A) 要——敲错不会把 `/clear` 发给模型；代价是用户期待清屏却得到一条分界行。(B) 不要——名字只有一个；代价是 `/clear` 变成一条发给模型的消息（现状对所有未知命令都如此）。**推荐 A。**
3. **ephemeral 且已回复时按 `/new`。** (A) Confirm 框 Discard/Keep——一键可走，误触有拦；多一个对话框。(B) 拒绝，提示先 `/save`——与 `/model` 搬家一致（`model.rs:233-242`）；但「我就是不想存」的用户要先存再新建再删。(C) 直接丢——最像 Claude Code（无确认）；但 iota 的 ephemeral 真的没副本。**推荐 A。**
4. **新会话落不落盘。** (A) 跟当前会话一样——所见即所得（banner/status 显示什么就是什么）；(B) 总是落盘，除非 agent 配了 `no_save:`——`--no-save` 变成「只对第一段有效」，不直观；(C) 总是跟启动 flag——`/save` 过的 run 再 `/new` 又变 ephemeral，更不直观。**推荐 A。**
5. **模型与五参数。** (A) 原样继承现在的值与来源——与 `/model` 的 tier-3 规则、与 Gemini/opencode/Crush 一致；(B) 像 `iota run` 一样从配置重新求值（Codex 的做法）——但 REPL 看不到配置，只能用启动时的 `ParamLayers` 快照重算一遍，结果与 (A) 只在「用户手调过的值」上不同：(B) 会把手调的丢掉。**推荐 A。**
6. **后台 jobs。** (A) 继续跑，完成通知进新会话——不毁用户的长任务；但模型会收到一条它没见过上下文的 "job N done"。(B) 全杀，像退出——干净；但 `/new` 变成有破坏性的命令，且与「旧会话随时能回来」的承诺不配。(C) 继续跑但通知丢弃——不打扰；但 `/jobs` 里还看得到它、完成了却没人说。**推荐 A**，分界行里提一句有几个在跑。
7. **"allow for this session" 的审批授权。** (A) 保留——与 `/session` 一致（按进程），Copilot 也保留；(B) 清空——字面上的 "session" 结束了，更保守；但 `/session` 不清，两条命令口径要一起改。**推荐 A**，同时承认 gate 的注释写的是 "this session may edit files"（`approval.rs:1-3`），措辞上 (B) 更站得住。
8. **屏幕。** (A) 一条 dim 分界 notice——与 `/session` 同形；(B) 重打一张 banner 卡片——更醒目；但要从聊天代码里绕过 transcript 直接写，架构违规（`ui-architecture.md:235-236`），或者给 transcript 加一种新块；(C) 清屏——无原语、反 inline 设计、终端支持不齐。**推荐 A。**
9. **参数 `/new <…>`。** (A) v1 不收；(B) 给新会话命名（Codex）；(C) 给旧会话贴标签（Claude Code）——对 iota 有一点用：标题 pass 还没落地时可以手动定名；(D) 当第一条消息（Copilot）。**推荐 A**，以后要加再加。
10. **bot 里输入 `/new`。** (A) 注册但拒绝并说明——比现状好；(B) 不注册——与 `/session` 现状一致，但 `/new` 会作为消息发给 bot。**推荐 A**，并建议顺手把 `/session` 在 bot 里改成同样的拒绝（本文范围外）。
11. **要不要 handoff（用旧会话的摘要开新会话）。** (A) 不做——`/compact` 已覆盖「省上下文」，`/new` 保持纯粹；Amp 做过又拆了。(B) 做成 `/new` 的一个变体——多一条命令面、多一次模型调用、多一种 history 播种方式。**推荐 A。**

### 3.8 不同意或拿不准的地方

- **不同意所有者原话里的「clear」。** 在 iota 里它会误导（§3.1）。如果所有者坚持 `/clear`，本文的其余部分不变，只是名字与行为的落差要写进文档。
- **拿不准 Claude Code 经典渲染器下 `/clear` 是否清 scrollback**（§1.1 ②【查不到】）。本文「不清屏」的结论不依赖它。
- **拿不准审批授权该不该跨 `/new`**（§3.7 第 7 点）：便利与字面安全各有道理。
- **`/session` swap 不更新 `conv.param_sources`**（`session.rs:217-271` 回放了值、没换来源 map）——看起来是遗漏而不是设计；`/new` 不应继承这个疏漏（§3.2 表里已写明随 `move_to` 写新 meta）。是否顺手修 `/session`，本文范围外。
- **Codex 文档与源码对 `/new` 是否清屏说法相反**（§1.2 ⑤），本文照实记录，不替它裁定。
- **opencode 文档说 `/clear` 是 `/new` 的别名，源码是两条命令**；**Crush 的 `clear` 别名 release note 与 patch 矛盾**——同样只记录。
- **实现成本提示**（不是本文范围，但影响「v1 做多少」）：需要三处新 API——`SessionTitle` 的 reset、`DeferDispatcher` 的 reset、Repl 内「铸一个新 bundle」的入口（现状只有 ephemeral 时才有 `SessionFactory`；非 ephemeral 的 run 要么把工厂也带上，要么从 `conv.catalog.session_provider()`/`agent()`、`conv.agent.root/cwd`、`conv.mode` 现拼一个 `NewSession`——`src/session/store.rs:50-65` 要的字段都拿得到）。

---

## 附录：打开过的 URL（按工具）

- Claude Code：https://code.claude.com/docs/en/commands · /interactive-mode · /sessions · /costs · /checkpointing · /hooks · /fullscreen · /keybindings · /glossary · /memory · /model-config · /permissions · /headless · /agent-sdk/sessions · /agent-sdk/slash-commands（均在 https://code.claude.com/docs/en/ 下）· https://raw.githubusercontent.com/anthropics/claude-code/main/CHANGELOG.md · issues #6835 #26082 #31220 #36213 #39658 #51480 #70606 #72158 #80431（https://github.com/anthropics/claude-code/issues/ ）。本机：`~/.local/share/claude/versions/2.1.296`（`strings -n 8`）。
- Codex：https://github.com/openai/codex/blob/9167a0b2d4a74d44a4d388057d304682e47313c6/codex-rs/tui/src/{slash_command.rs, keymap.rs, app_event.rs, chatwidget/slash_dispatch.rs, chatwidget/worktree_picker.rs, chatwidget/constructor.rs, app/event_dispatch.rs, app/new_session.rs, app/session_lifecycle.rs, app/history_ui.rs, app/tests/new_session_tests.rs, app_server_session.rs, managed_new_thread_defaults.rs} · codex-rs/core/src/session/{mod.rs, session.rs} · codex-rs/rollout/src/recorder.rs · codex-rs/app-server/src/request_processors/thread_processor.rs · https://learn.chatgpt.com/docs/cli/slash-commands · https://learn.chatgpt.com/docs/non-interactive-mode · https://github.com/openai/codex/releases/tag/rust-v0.162.1 。本机：`codex --help`、`codex exec --help`、`codex resume --help`、`codex fork --help`，`strings` 于 `@openai/codex-darwin-arm64/vendor/aarch64-apple-darwin/bin/codex`。
- Gemini CLI：https://github.com/google-gemini/gemini-cli/blob/main/ 下 packages/cli/src/ui/commands/{clearCommand.ts, chatCommand.ts, compressCommand.ts, resumeCommand.ts, statsCommand.ts} · packages/cli/src/ui/hooks/{slashCommandProcessor.ts, useHistoryManager.ts, useSessionBrowser.ts, atCommandProcessor.ts} · packages/cli/src/ui/{AppContainer.tsx, key/keyBindings.ts, contexts/SessionContext.tsx} · packages/cli/src/{nonInteractiveCli.ts, nonInteractiveCliCommands.ts} · packages/core/src/{core/client.ts, core/geminiChat.ts, config/config.ts, services/chatRecordingService.ts, telemetry/uiTelemetry.ts, utils/environmentContext.ts, context/memoryContextManager.ts} · docs/reference/{commands.md, keyboard-shortcuts.md} · docs/cli/{session-management.md, headless.md, cli-reference.md} · docs/hooks/reference.md · docs/changelogs/index.md · https://geminicli.com/docs/reference/commands/ 。
- Aider：https://aider.chat/docs/usage/commands.html · /docs/scripting.html · /docs/config/options.html · /docs/usage/tips.html · /docs/troubleshooting/token-limits.html · /docs/faq.html · https://raw.githubusercontent.com/Aider-AI/aider/main/ 下 aider/{commands.py, coders/base_coder.py, io.py, main.py, args.py, utils.py} · HISTORY.md 。
- opencode：https://opencode.ai/docs/tui/ · /docs/keybinds/ · /docs/cli/ · https://github.com/anomalyco/opencode/blob/dev/ 下 packages/tui/src/{app.tsx, config/keybind.ts, config/index.tsx, config/new-session-location.ts, context/session-tabs.tsx, component/prompt/index.tsx, feature-plugins/sidebar/context.tsx, mini/footer.command.tsx, mini/runtime.ts} · packages/core/src/{session.ts, session/title.ts, session/compaction.ts, permission/saved.ts} · packages/cli/src/{run/run.ts, session-target.ts, database-path.ts} · CHANGELOG.md · https://github.com/anomalyco/opentui/blob/main/packages/core/src/renderer.ts 。
- Crush：https://github.com/charmbracelet/crush/blob/main/ 下 README.md · internal/ui/model/{keys.go, ui.go, header.go, sidebar.go, landing.go} · internal/ui/dialog/{commands.go, sessions.go} · internal/agent/{agent.go, templates/title.md, tools/grep.go} · internal/app/app.go · internal/cmd/{root.go, run.go, session.go} · internal/config/config.go · internal/db/{connect.go, sql/sessions.sql} · https://github.com/charmbracelet/crush/commit/cfad8143fc0414d1406a8b846708a43ee511a757 。
- Cursor CLI：https://cursor.com/docs/cli/overview · /reference/slash-commands · /reference/parameters · /using · /headless · /changelog · /reference/configuration · /reference/permissions · /reference/output-format 。
- Amp：https://ampcode.com/docs/cli · /docs/cli/keybindings · /docs/cli/execute-mode · /docs/cli/streaming-json · /docs/cli/settings · /docs/threads · /docs/the-dial · /docs/tools · /docs/customize/agents-md · /news/new-threads-without-leaving-the-cli · /news/amp-x · /news/cli-tab-navigation · /news/handoff · /news/command-palette · /news/stick-a-fork-in-it · /news/neo 。
- Copilot CLI：https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-command-reference · /cli-programmatic-reference · /cli-config-dir-reference · https://docs.github.com/en/copilot/how-tos/use-copilot-agents/use-copilot-cli · /how-tos/copilot-cli/cli-best-practices · /how-tos/copilot-sdk/troubleshooting/compatibility · /concepts/copilot-surfaces/copilot-cli · /concepts/agents/copilot-cli/context-management · /reference/hooks-configuration · https://raw.githubusercontent.com/github/copilot-cli/main/changelog.md 。
