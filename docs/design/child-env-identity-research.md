# 子进程继承宿主身份：别家怎么做（调研）

Status: **Research** · 日期：2026-10-17 · 基于 `busy-host-state` = `a9d7857`（v0.7.2 之后），本机 herdr 0.9.5、Claude Code 2.1.301、codex-cli 0.162.1、gemini-cli 0.46.0

本文只做调研，不含设计决定，不改代码。所有者的原话：「我想看看其它的 coding agent 会怎么处理这个问题」。「这个问题」是 busy-host-state 分支查清的那一类：**agent 把自己的宿主/会话身份环境变量交给子进程（shell 命令、MCP stdio 服务器），子进程（尤其是嵌套的另一个 agent）顶着父 agent 的身份跟宿主说话，把父 agent 的身份抢掉。**

## 写作约定

- 每条结论都附出处：URL 加原文，`path:line@SHA`，或本机命令加输出。查不到写「查不到」；从代码推出、没有原文或实测支撑的，标「推断」。
- 第 1 节、第 2 节的本机实测，第 3.1 节 Crush 的 `run.go`，第 4.2 节 herdr 的 `pane.rs`/`state.rs`/hook 文件，以及 herdr issue #2824、#1789、#4631 的原文，由撰写者本人对着原文或输出复核过。其余出处来自五个调研 agent（笔记见文末「材料」），没有逐条复核。
- 源码都按固定提交读：openai/codex `rust-v0.162.1` = `092d3acd`；google-gemini/gemini-cli `v0.46.0` = `85b0c55c`，另读本机 Homebrew 装的 0.46.0 发行 bundle（`/opt/homebrew/Cellar/gemini-cli/0.46.0/libexec/lib/node_modules/@google/gemini-cli/bundle/chunk-RCJSF5RP.js`，下称 G-BUNDLE）；charmbracelet/crush `v0.98.1` = `8b182428`；anomalyco/opencode（原 sst/opencode）`v1.19.3` = `e3c4eb24`；Aider-AI/aider `v0.86.0` = `a4be6ccd`；herdrdev/herdr `v0.9.5` = `2ecf19a9`；tmux 3.8 = `7f2a35ad`；kitty v0.49.2 = `b2cb8712`；WezTerm `20240203-110809-5046fc22`；Zellij v0.45.1 = `efd8fd5a`；GNU screen v.5.0.2 = `e9206eff`；manaflow-ai/cmux v0.65.2 = `9d01f956`。
- 「宿主身份类」= 多路复用器/终端注入、子进程可以拿来对宿主说「我是这个 pane」的变量（`TMUX`、`TMUX_PANE`、`KITTY_*`、`WEZTERM_*`、`ZELLIJ*`、`STY`、`CMUX_*`、`HERDR_*`、`TERM_PROGRAM`）。「agent 标记类」= agent 自己给子进程打的标记（`CLAUDECODE`、`CODEX_THREAD_ID`……）。「凭证类」= 名字或值像密钥的变量。

## 0. 结论

**通行做法是透传：宿主身份类变量没有一家 agent 默认剥掉——只有 Crush 一家，在 bash 工具这一条路径上，剥掉了 herdr 的三个变量（`HERDR_ENV`、`HERDR_SOCKET_PATH`、`HERDR_PANE_ID`），理由正是我们遇到的这件事。** 别家主动剥的只有两类：凭证（Gemini 对 MCP 强制、Codex 对 MCP 用白名单、Claude Code 可选开启），和 agent 自己的会话身份（每一层都**改写成自己的值**，而不是删掉）。Codex 的 MCP 白名单顺带剥掉了一切宿主变量，但那是为了凭证，用户还在 issue 里要求把 `TMUX`/`TMUX_PANE` 加回去。

宿主一侧没有一家说「子进程不要继承」；真正区分父子 agent 的只有 herdr 和 cmux，两家都是宿主自己在**开 pane 的边界**剥掉外层身份，并在服务端过滤。herdr 的集成 hook 判断「我是不是被嵌套启动的」，靠的是**父 agent 打的标记**（`OMPCODE`、`CODEX_THREAD_ID`），只防同一种 agent 套自己。herdr 维护者对「外来 source 的报告被丢弃」的立场是**按设计如此**（#2824：「Herdr intentionally has one lifecycle authority per pane」）；嵌套 agent 卡住父 pane 的几个 issue（#2668、#4919、#4814、#2012）至今开着。

**对我们的含义**（第 5 节展开）：iota 和 Crush 处境相同——自己就是向 herdr 上报的那一方。这件事上唯一的先例是「在 iota 自己的 shell 子进程边界上扣掉 herdr 变量」；「什么都不动」等于把修复交给 herdr，而 herdr 已经表明不会改。扣哪几个，Crush 扣了全部三个，代价是它的模型无法再用 `herdr --current`。按 herdr 自己的约定（集成「Only report when `HERDR_ENV=1`」），**只扣 `HERDR_ENV`** 就够挡住 herdr 发行的全部 34 个 hook 文件，而 herdr CLI 照常可用。再扣 `HERDR_PANE_ID` 没有先例证明更安全，反而实测会让 `--current` 悄悄落到焦点 pane。

## 1. 起因（busy-host-state 已查清的部分）

- iota 的 shell 工具（`src/shell/exec.rs:145-200`，不调 `env_clear`/`env_remove`）和 MCP stdio（`src/mcp/transport.rs:475-476`，`envs(&server_cfg.env)` 叠在父环境上）都把 iota 的完整环境交给子进程，`HERDR_*`、`CMUX_*` 都在里面。
- 私有 herdr 0.9.5 上的端到端复现：模型在 iota 的 shell 里跑 `claude -p` 之后，它的 SessionStart hook（`~/.claude/hooks/herdr-agent-state.sh`）以 `source=herdr:claude` 往 iota 的 pane 报了 `report_agent_session`。此后 iota 的每一条 `report_agent` 都回 `ok`、都不生效，pane 永久停在 `working`；iota 释放后重报、在同一 pane 重启 iota 都救不回来。
- herdr 侧的机制（本人复核）：`src/terminal/state.rs:736-745`@`2ecf19a9`——`current_session_owner_conflicts`（`:1330-1336`）比较报告方的 `(source, agent)` 与 pane 当前 session 的所有者，不同且前台进程检测不确认接管时，`return None`，报告被静默丢弃。
- 所以「iota 的界面不忙、宿主却报 working」不是 iota presenter 的错：iota 一直在发对的状态，herdr 不采纳。

## 2. 本机实测：三家真实交给子进程的环境

### 2.1 方法

一组**假的**宿主身份、凭证与普通变量，随 peer 一起启动（`HERDR_SOCKET_PATH` 指向不存在的路径，保证不会真的打到所有者的 herdr）：

```sh
export TMUX=/tmp/probe-fake-tmux,99999,0 TMUX_PANE=%99
export KITTY_WINDOW_ID=99 KITTY_LISTEN_ON=unix:/tmp/probe-fake-kitty KITTY_PID=99999
export WEZTERM_PANE=99 WEZTERM_UNIX_SOCKET=/tmp/probe-fake-wezterm
export ZELLIJ=0 ZELLIJ_SESSION_NAME=probe-fake STY=99999.probe-fake
export CMUX_SURFACE_ID=probe-fake-surface CMUX_WORKSPACE_ID=probe-fake-ws
export HERDR_ENV=1 HERDR_PANE_ID=probe:p1 HERDR_TAB_ID=probe:t1 HERDR_WORKSPACE_ID=probe \
       HERDR_SOCKET_PATH=/nonexistent/probe-herdr.sock HERDR_BIN_PATH=/nonexistent/herdr
export TERM_PROGRAM=ProbeTerm
export PROBE_PLAIN=plain PROBE_SECRET=s3cr3t PROBE_TOKEN=t0ken PROBE_API_KEY=k3y PROBE_PASSWORD=pw \
       AWS_SECRET_ACCESS_KEY=fakeaws GITHUB_TOKEN=fakegh
```

shell 子进程：让模型只跑 `env | sort > <文件>`。MCP 子进程：一个最小的 stdio MCP 服务器（Python，响应 `initialize`/`tools/list`），一启动就把自己的 `os.environ` 写进文件。然后逐变量对比传入的环境与子进程看到的环境。

```sh
claude -p --model haiku --strict-mcp-config --mcp-config '{"mcpServers":{"probe":{"command":"python3","args":["-I","…/mcp_env.py","claude"]}}}' \
  --allowedTools=Bash "Run exactly this bash command and nothing else, then reply OK: env | sort > …/shell-claude.env"
codex exec --skip-git-repo-check --ephemeral -s workspace-write -C … \
  -c 'mcp_servers.probe.command="python3"' -c 'mcp_servers.probe.args=["-I","…/mcp_env.py","codex"]' \
  "Run exactly this shell command and nothing else, then reply OK: env | sort > out/shell-codex.env" </dev/null
```

（`CPATH`、`LIBRARY_PATH`、`SDKROOT` 出现在每个 MCP 环境里，是 macOS `/usr/bin/python3` 垫片自己加的，不是 peer 的行为。）

### 2.2 结果

| | 宿主身份类（20 个探针） | 凭证类（6 个探针） | peer 自己加的 |
|---|---|---|---|
| Claude Code 2.1.301，Bash 工具 | **全部原样** | **全部原样** | `CLAUDECODE=1` `CLAUDE_CODE_CHILD_SESSION=1` `CLAUDE_CODE_ENTRYPOINT=sdk-cli` `CLAUDE_CODE_SESSION_ID` `CLAUDE_CODE_MESSAGING_SOCKET` `CLAUDE_CODE_MESSAGING_TOKEN` `CLAUDE_PID` `CLAUDE_EFFORT` `CLAUDE_CODE_EXECPATH` `CLAUDE_CODE_SESSION_ATTENDED=0` `AI_AGENT=claude-code_2-1-301_agent` `COREPACK_ENABLE_AUTO_PIN=0` `GIT_EDITOR=true` |
| Claude Code 2.1.301，MCP stdio | **全部原样** | **全部原样** | `CLAUDECODE=1` `CLAUDE_CODE_SESSION_ID` `CLAUDE_CODE_MESSAGING_SOCKET/TOKEN` `CLAUDE_PROJECT_DIR` `AI_AGENT=claude-code_2-1-301_harness`（没有 `CLAUDE_CODE_CHILD_SESSION`） |
| Codex 0.162.1，shell | **全部原样** | **全部原样**（默认 `ignore_default_excludes=true`） | `CODEX_THREAD_ID` `CODEX_SESSION_ID` `CODEX_VERSION` `CODEX_SANDBOX=seatbelt` `CODEX_SANDBOX_NETWORK_DISABLED=1` `CODEX_CI=1` `CODEX_MANAGED_BY_NPM=1` `CODEX_APPLY_PATCH_PRESERVE_LINE_ENDINGS=1` `NO_COLOR=1` `PAGER/GIT_PAGER/GH_PAGER=cat` `LC_CTYPE=C.UTF-8` |
| Codex 0.162.1，MCP stdio | **全部剥掉** | **全部剥掉** | 无。子进程只剩 `HOME LANG LC_ALL LOGNAME PATH SHELL TERM TMPDIR USER __CF_USER_TEXT_ENCODING`（白名单），连 `SSH_AUTH_SOCK`、`CLAUDECODE` 也没了 |
| Gemini CLI 0.46.0，shell（默认配置） | **全部原样** | **全部原样** | `GEMINI_CLI=1` `TERM=xterm-256color` `PAGER/GIT_PAGER=cat`，非交互时再加 `GIT_TERMINAL_PROMPT=0` 等（G-BUNDLE:319557-319580） |
| Gemini CLI 0.46.0，MCP stdio | **全部原样** | **全部剥掉**（连同 `SSH_AUTH_SOCK`、`CLAUDE_CODE_MESSAGING_TOKEN`，名字命中 `/AUTH/`、`/TOKEN/`） | `GEMINI_CLI=1` |

Gemini 的两行不是真跑出来的：本机账号被服务端停用（`Error authenticating: … This service has been disabled in this account for violation of Terms of Service`），模型调不起来，MCP 也在鉴权之后才启动。所以从 G-BUNDLE 原样抠出 `sanitizeEnvironment` 及其常量（G-BUNDLE:252005-252169），对同一组探针变量各跑一次：shell 一侧传 `enableEnvironmentVariableRedaction: false`（发行默认值，G-BUNDLE:252167、373982），MCP 一侧传 `true`（MCP stdio 路径强制打开，G-BUNDLE:365352-365355）。

### 2.3 嵌套：本机实测的 Claude 套 Claude

撰写者本身就跑在 Claude Code 2.1.301 里，带着 `CLAUDECODE=1`。从这个 shell 里直接 `claude -p …`：**没有被拒绝**，正常执行。内层给子进程的变量里，**自己的身份被改写成内层的值**，宿主身份原样不动：

```
CLAUDE_CODE_SESSION_ID:       edb97d78-… -> 909d38f2-…   （外层 -> 内层）
CLAUDE_PID:                   39957 -> 47386
CLAUDE_CODE_MESSAGING_SOCKET: /tmp/cc-socks/39957.sock -> /tmp/cc-socks/47386.sock
CLAUDE_CODE_ENTRYPOINT:       cli -> sdk-cli
HERDR_*、TMUX* 等：           原样
```

## 3. 逐家对照

### 3.1 一览

| | shell 子进程 | MCP stdio 子进程 | 宿主身份类 | 凭证类 | agent 自己的身份 |
|---|---|---|---|---|---|
| **Claude Code** | 继承全部 + 标记 | 推断：继承全部 + `env` + 标记 | 透传（实测） | 透传；`CLAUDE_CODE_SUBPROCESS_ENV_SCRUB=1` 可选剥（默认关）；`OTEL_*` 无条件剥 | 每层改写成自己的（实测） |
| **Codex CLI** | `inherit=all`，默认不排除 | **11 项白名单** | shell 透传；MCP 剥（白名单的副作用） | shell 透传；MCP 剥；6 个内部变量无条件剥 | 每层改写成自己的 |
| **Gemini CLI** | 继承；过滤默认关 | 继承，**强制过滤凭证** | 透传 | shell 默认透传；MCP 剥 | 加 `GEMINI_CLI=1` |
| **Aider** | 继承（不传 `env`） | 不支持 MCP | 透传 | 透传，还会把 key 写进进程 env | 无标记 |
| **opencode** | 继承 + 插件 env | 继承 + `environment` | 透传 | 透传，还会写入存储的凭证 | v1 加 `OPENCODE=1`/`OPENCODE_PID`；v2 shell 改写 `OPENCODE_SESSION_ID` |
| **Crush** | `os.Environ()`，**剥 `HERDR_ENV`/`HERDR_SOCKET_PATH`/`HERDR_PANE_ID`** | `os.Environ()` + 配置 env（不剥 herdr） | bash 工具剥 herdr 三个，其余透传 | 透传 | 加 `CRUSH=1`/`AGENT=crush`/`AI_AGENT=crush` |
| **Copilot CLI** | 继承 + 标记 | 文档：只给 `PATH` + 配置项；issue 说是「继承 + 过滤表」（矛盾） | 查不到 | `--secret-env-vars` 可选 | `COPILOT_CLI=1`、`COPILOT_AGENT_SESSION_ID` |
| **Cursor CLI** | 文档：「with the CLI's … environment」 | 继承与否查不到 | 查不到 | 查不到 | `CURSOR_AGENT`，沙箱内 `CURSOR_SANDBOX` 等 |

### 3.2 Claude Code（闭源；本机实测 + 文档 + CHANGELOG）

- shell：sandboxing 文档表格 「Environment variables | Inherited from Claude Code, including any secrets in its environment」（https://code.claude.com/docs/en/sandboxing）。实测见 2.2。
- 标记：`CLAUDECODE=1` 给 Bash、hook、status line、stdio MCP；`CLAUDE_CODE_CHILD_SESSION=1` 「Not set for stdio MCP server subprocesses」，「only set by Claude Code itself … so it reliably distinguishes a nested session」（https://code.claude.com/docs/en/env-vars）。`AI_AGENT` 见 CHANGELOG 2.1.120：「Set `AI_AGENT` environment variable for subprocesses so `gh` can attribute traffic to Claude Code」。
- 剥的：CHANGELOG 2.1.128「Subprocesses (Bash, hooks, MCP, LSP) no longer inherit `OTEL_*` environment variables」；2.1.83 引入可选的 `CLAUDE_CODE_SUBPROCESS_ENV_SCRUB=1`「to strip Anthropic and cloud provider credentials from subprocess environments (Bash tool, hooks, MCP stdio servers)」，默认关；env-vars 文档说它保留 `GITHUB_TOKEN`/`GH_TOKEN`。issue #101890（open）报告 `CLAUDE_CODE_OAUTH_TOKEN` 不开 scrub 也被剥，无文档。
- 宿主身份类：文档里查不到任何剥离；实测全部透传。
- MCP：文档只说 `env` 是「environment variables passed to the server」（https://code.claude.com/docs/en/mcp），没有明说继承；实测继承全部。

### 3.3 Codex CLI（`rust-v0.162.1` = `092d3acd`，路径相对 `codex-rs/`）

- shell 默认 `inherit=All`、`ignore_default_excludes=true`（`config/src/shell_environment_policy.rs:135-136`，`protocol/src/config_types.rs:213`、`:244-245`、`:261-271`）。组装顺序在 `protocol/src/shell_environment.rs:91-161`，最后无条件剥掉 `NON_INHERITABLE_ENV_VARS`（`:13-21`，6 个 Codex 自己的认证/联邦变量，注释「prevents accidental propagation of Codex launch context」）。spawn 先 `env_clear()` 再 `envs(env)`（`core/src/spawn.rs:88-89`）。
- 为什么默认不排除凭证：最初的排除来自 PR #1061（「the sensitive `OPENAI_API_KEY` … was made available to everything run by `shell` … it does not seem like a good default」），之后 PR #1904 改回 `inherit="all"`（「Trying to use `core` as the default has been "too clever."」），PR #8281 把 `ignore_default_excludes` 默认改成 true（PR 描述为空，理由查不到）。
- MCP 白名单：`rmcp-client/src/utils.rs:162-175`（Unix 11 项），组装在 `:16-59`，`env_clear()` 在 `utils/pty/src/child_command.rs:94-103`。理由出自 PR #831：「takes a cue from Claude Code where the MCP server is launched with a restricted `env` … as it is common for developers to have sensitive API keys present in their environment」——**为凭证，不是为身份**。
- 宿主身份类：`core`、`protocol`、`rmcp-client` 里对 `TMUX|TMUX_PANE|TERM_PROGRAM|KITTY_|WEZTERM_|ZELLIJ` grep 零命中，没有专门处理。issue #37209（open）「Codex stdio MCP subprocess omits TMUX/TMUX_PANE, breaking tmux-aware tools」，绕过办法 `env_vars = ["TMUX","TMUX_PANE"]`——**用户要的是把宿主身份交给 MCP**。issue #48500（open）：app-server daemon「inherits the environment of whichever TUI first spawned it, so every hook for every later TUI sees that first client's `TMUX` / `TMUX_PANE`」，建议在跑 hook 前剥掉终端身份变量。
- 自己的身份：`CODEX_THREAD_ID` 无条件改写成当前线程（`protocol/src/shell_environment.rs:151-154`），起因是 issue #15527「Nested codex exec commands inherit parent CODEX_THREAD_ID」。

### 3.4 Gemini CLI（0.46.0 发行 bundle；源码 `85b0c55c`）

- 源码对应：过滤函数 `packages/core/src/services/environmentSanitization.ts:17-22`（关时原样返回）、`:143-195`（判定）；shell `packages/core/src/services/shellExecutionService.ts:482-521`；MCP stdio `packages/core/src/tools/mcp-client.ts:2332-2362`；开关 `security.environmentVariableRedaction.enabled` 默认 false（`packages/cli/src/config/settingsSchema.ts:1956-1965`），文档原话「Environment redaction is currently **OFF by default**」（`docs/hooks/best-practices.md:457`）。MCP 强制脱敏的理由：防止把宿主机密钥泄给第三方 MCP server，server 配置里显式写的 env「constitutes informed consent」（`docs/tools/mcp-server.md:217-245`，PR #17311）。PR #15740 想把 shell 默认改成开启，未合并。以下行号按 G-BUNDLE。
- 过滤函数 `sanitizeEnvironment`（G-BUNDLE:252005-252169，源文件注释 `packages/core/dist/src/services/environmentSanitization.js`）：`enableEnvironmentVariableRedaction` 关时直接 `{ ...processEnv }`；开时按名字模式 `/TOKEN/ /SECRET/ /PASSWORD/ /PASSWD/ /KEY/ /AUTH/ /CREDENTIAL/ /CREDS/ /PRIVATE/ /CERT/` 与值模式（私钥头、带口令的 URL、`ghp_`、`AIzaSy`、`AKIA`、JWT、Stripe、Slack）剥掉；`GEMINI_CLI_*` 永远保留。检测到 `GITHUB_SHA` 或 `SURFACE=Github` 时进入严格模式，只留白名单。
- shell：用配置里的 `sanitizationConfig`，默认 `enableEnvironmentVariableRedaction ?? false`（G-BUNDLE:252167、373982），然后加 `GEMINI_CLI=1`、`TERM`、`PAGER`（G-BUNDLE:319557-319563）。
- MCP stdio：先要求 folder 受信任，否则抛错「uses stdio transport but current folder is not trusted」；然后**强制** `enableEnvironmentVariableRedaction: true`（G-BUNDLE:365348-365355），再叠配置的 `env`。
- 名单里没有任何宿主身份类变量；实测（2.2）全部透传。
- 嵌套：查不到检测或拒绝。

### 3.5 Aider、opencode、Crush

- **Aider**（`v0.86.0`）：所有命令走 `aider/run_cmd.py:62-73`（`Popen(shell=True)`，不传 `env`）或 `:116`（`pexpect` 交互 shell）。没有标记，不处理宿主变量，凭证透传，还会把 `.env`（`main.py:381`，`override=True`）和 `--api-key` 等写进进程环境。没有 MCP。嵌套：查不到。
- **opencode**（`v1.19.3`）：bash 工具 `{...process.env, ...插件 env}`（`packages/opencode/src/tool/shell.ts:416-426`），本地 MCP `{...process.env, ...mcp.environment}`（`packages/opencode/src/mcp/index.ts:347-357`）；入口给进程写 `AGENT=1`、`OPENCODE=1`、`OPENCODE_PID`（`packages/opencode/src/index.ts:75-77`）。代码里 `TMUX`/`KITTY`/`WEZTERM`/`ZELLIJ`/`HERDR` 零命中。v2.0.31 的 shell 工具会改写 `OPENCODE_SESSION_ID`、保留外层 `AI_AGENT`（`packages/core/src/tool/plugin/shell.ts:211-216`，PR #51975，「does not change MCP servers」）。相关 issue #51784（open）「run recovered agent B shells with A's identity and workspace variables」。
- **Crush**（`v0.98.1` = `8b182428`，本人复核）：`internal/shell/run.go:257-285`：

  ```go
  // herdrEnvVars are the environment variables herdr injects into panes
  // so agents can report state over its Unix socket API. Subprocesses
  // must not inherit these: a child process that calls herdr.Init()
  // would attach to the parent's pane and, on exit, release its agent
  // authority — making the status vanish. Stripping them here closes
  // that gap for every command the bash tool runs.
  var herdrEnvVars = []string{
  	"HERDR_ENV",
  	"HERDR_SOCKET_PATH",
  	"HERDR_PANE_ID",
  }
  ```

  随 PR #3145「feat(herdr): add herdr socket integration」（2026-06-29 合入）一起进来——**Crush 加 herdr 上报的同一个 PR 就剥了子进程的 herdr 变量**。只在 bash 工具上剥：stdio MCP 是 `append(os.Environ(), cfg env...)`（`internal/agent/tools/mcp/init.go:1211-1212`），hooks、`!` 命令也不剥（调研 agent 读码，推断）。`TMUX`、`HERDR_WORKSPACE_ID`、`HERDR_TAB_ID`、`HERDR_BIN_PATH` 不剥。

## 4. 嵌套 agent 与宿主一侧

### 4.1 agent 怎么知道自己跑在另一个 agent 里

- **Claude Code**：2.1.41 起硬拒绝（CHANGELOG：「Added guard against launching Claude Code inside another Claude Code session」），文案（引自 issue #25803）：「Error: Claude Code cannot be launched inside another Claude Code session. Nested sessions share runtime resources and will crash all active sessions. To bypass this check, unset the CLAUDECODE environment variable.」后来取消硬拒绝，改成用 `CLAUDE_CODE_CHILD_SESSION` 判断嵌套、嵌套的交互会话不进 `--resume` 列表（env-vars 文档，「Requires Claude Code v2.1.172 or later」）；当前文档已 grep 不到拒绝文案，本机 2.1.301 实测嵌套 `claude -p` 正常运行（2.3）。具体哪个版本移除了硬拒绝，CHANGELOG 查不到。
- **Codex**：生产代码里查不到读 `CODEX_THREAD_ID`/`CODEX_SESSION_ID` 做嵌套检测的地方；处理方式是改写（3.3）。
- **Gemini、Aider、opencode、Copilot、Amp、Cursor**：查不到嵌套检测。Crush 只在遥测里记 `CRUSH=="1"`（`internal/event/event.go:40-44`），不警告不拒绝。
- **跨厂商提案**：agents.md#136 想把 `AI_AGENT` 定为统一标记，草案规定嵌套时「the outermost agent wins」。是第三方提案，不是任何厂商的承诺。
- **明确写「子进程不要继承我的会话身份」的 agent**：只有 Crush 那段注释（针对 herdr）。Codex 最接近的是 PR #52119（不在 0.162.1 里）：「without stale values from inherited environments」。其余查不到。

### 4.2 herdr（本人复核：源码 `2ecf19a9`，文档 https://herdr.dev/llms-full.txt）

- 文档：「Every process in a Herdr pane inherits these environment variables」；「Only report when `HERDR_ENV=1` and the other variables are set. Outside Herdr, your integration should do nothing.」（add-herdr-support 页）
- **开 pane 时剥外层身份**：`src/pane.rs:119-135` 注释「Host handles refer to the outer terminal, never to this pane」，剥 `ITERM_SESSION_ID`、`WEZTERM_PANE`、`KITTY_WINDOW_ID`、`WT_SESSION`、`TMUX`、`TMUX_PANE`、`STY`、`ZELLIJ*`；`src/pane.rs:185-197` 注释「A new pane is not a child agent of the process that started the server」，剥 `CODEX_THREAD_ID`、`OMPCODE`、`CLAUDECODE`、`CLAUDE_CODE_CHILD_SESSION`、`CLAUDE_CODE_SESSION_ID`、`CLAUDE_CODE_MESSAGING_TOKEN`。也就是说：**herdr 自己在「所有权换手」的边界上剥掉上一任的身份**。
- **hook 的嵌套守卫**用父 agent 打的标记：OMP `src/integration/assets/omp/herdr-agent-state.ts:17-23`（「OMP marks every shell it spawns with OMPCODE=1. A nested `omp` launched from a parent session's shell inherits it, so that process is not the pane's root agent and must not report its short-lived session over the parent's.」）；Codex `src/integration/assets/codex/herdr-agent-state.sh:62-64`（`CODEX_THREAD_ID` 与本会话不同就退出，来自 issue #1789「Nested Codex process replaces the pane's saved session ID」）。Claude 的 v11 hook（`src/integration/assets/claude/herdr-agent-state.sh`）只排除 subagent（`agent_id`），**不看** `CLAUDE_CODE_CHILD_SESSION`。这些守卫只防「X 里套 X」，iota 里套 claude 防不住。
- **每个发行 hook 都引用 `HERDR_ENV`**：`src/integration/assets/*/` 下 34 个 hook 文件 grep 计数均 ≥1（`hermes/plugin.yaml`、`opencode/tui.js` 两个非 hook 文件为 0）；撰写者抽查的 claude、codex、omp 三个都是 `HERDR_ENV == "1"` 的守卫。
- **服务端**：一 pane 一个 lifecycle 权威，所有者冲突即丢弃（`src/terminal/state.rs:736-745`，见第 1 节）。issue #2824「`pane report-agent` silently drops every state report from a foreign source once an integration has anchored the pane」，维护者以 feature request 关闭：「Herdr intentionally has one lifecycle authority per pane」。
- **同类 issue**：#511、#672、#4919（open，嵌套 `claude -p` 覆盖 pane 的 session id；#4919 在 0.9.5 仍复现）；#2668（open，「Nested interactive Pi can leave the parent pane stuck In progress」）；#4697（以 #2668 的重复关闭，pi 子 worker「inherits `HERDR_ENV` / `HERDR_PANE_ID` / `HERDR_SOCKET_PATH`」后父 pane 卡在 `working`）；#4814（open，Codex 向继承来的 pane 报 session）；#2012（open，「pane current trusts an inherited $HERDR_PANE_ID」）；#4631（open，herdr server 从 Claude Code 里启动，把 `AI_AGENT=claude-code_…` 带给了每一个 pane——方向相反的同类泄漏）。
- 文档 L861 说「`--current` requires a calling pane and errors when `HERDR_PANE_ID` is unavailable」。本机实测（私有 session）不符：`env -u HERDR_PANE_ID HERDR_ENV=1 herdr pane current --current` 退出码 0，返回**焦点 pane** `w1:p1`。
- 唯一一处明确就「继承」给出警告的：`HERDR_AGENT`「Avoid exporting it globally unless every inherited foreground process should be treated as that agent」。

### 4.3 其他宿主

没有一家说「子进程应 unset」或「身份变量只给最外层」；文档用词都是「passed to the child process」，服务端都不核实调用者确实在那个 pane 里（调研 agent 读码）。

- **tmux 3.8**：man「The pane ID is passed to the child process of the pane in the TMUX_PANE environment variable」（tmux.1:933-935），`TMUX` 是「to allow commands to be executed from inside」（:7877-7879）。嵌套拒绝「sessions should be nested with care, unset $TMUX to force」（`cmd-new-session.c:196-197`、`cmd-attach-session.c:75-76`），判定要 `$TMUX` 非空**且**客户端 tty 是某个 pane 的 tty（`server-client.c:211-226`）。当前 pane 先按 tty 找，找不到才用 `TMUX_PANE`（`cmd-find.c:80-100`）。
- **kitty v0.49.2**：`kitten @` 不给 `--to` 时用 `KITTY_LISTEN_ON`，每条请求带环境里的 `KITTY_WINDOW_ID`（`tools/cmd/at/main.go:272-275`），服务端直接当 self_window（`boss.py:809-819`）。唯一承认继承出错的一处：clone-in-kitty 不克隆 `KITTY_WINDOW_ID`（`launch.py:1229-1230`，changelog「Fix … being cloned and thus having incorrect value」）。
- **WezTerm**：「If the `$WEZTERM_PANE` environment variable is set, it will be used」（docs/cli/cli/index.markdown）。
- **Zellij v0.45.1**：只拒绝 attach 自己所在的 session（「Zellij does not support nesting a session in itself.」，`commands.rs:863-874`），靠 DCS 探测，不靠 env。
- **GNU screen v.5.0.2**：开窗口时写新 `STY`、剔除继承来的 `STY`/`WINDOW`（`screen.c:1502-1514`）；man 警告改名后「The $STY environment variables will still reflect the old name in pre-existing shells. This may result in confusion.」
- **cmux v0.65.2**：有意让后代继承 `CMUX_SURFACE_ID` 当绑定键（`docs/agent-session-tracking-spec.md:45-51`）；`automation.suppressSubagentNotifications` 默认 true，「Suppress visible completion notifications and status mutations from nested Codex or Claude child agents」；它的 claude wrapper 主动 unset `CLAUDECODE`、`CLAUDE_CODE_CHILD_SESSION` 等，但保留 `CMUX_SURFACE_ID`。

**归纳**：宿主一侧的约定是「**在所有权换手的 spawn 边界上，由开新进程的那一方剥掉上一任的身份**」——tmux/screen 开 pane、herdr 开 pane（剥外层终端和外层 agent）、cmux 的 wrapper（剥 Claude 的嵌套标记）都是这个形状。没有一家要求「子进程自觉 unset」。

## 5. 对我们的含义

iota 的 shell 子进程边界，就是一次所有权**不**换手的 spawn：pane 的主人仍是 iota，子进程只是 iota 的工具。按第 4.3 节的形状，该剥的是「会让子进程以为自己是 pane 主人」的那部分。

| 选项 | 与别家的对照 | 效果 | 代价 |
|---|---|---|---|
| **扣 `HERDR_ENV`**（shell 与 MCP stdio 子进程） | 没有一家正好这样做；最接近 Crush（它扣了这个，外加两个）。与 herdr 自己的约定对齐：集成「Only report when `HERDR_ENV=1`」，34 个发行 hook 都引用它 | herdr 发行的全部集成（claude、codex、pi、omp…）在 iota 的子进程里都会跳过上报；`herdr` CLI 照常可用（`--current`、命名 session 都在） | herdr skill 第一步 `test "${HERDR_ENV:-}" = 1` 在 iota 的 shell 里会失败；第三方自带上报、但不按 herdr 约定检查 `HERDR_ENV` 的 agent 挡不住（查不到有没有这种） |
| **扣 `HERDR_ENV` + `HERDR_PANE_ID`**（Crush 还扣了 `HERDR_SOCKET_PATH`） | **Crush 的做法**，且是唯一先例 | 更彻底：连「只看 `HERDR_PANE_ID`」的第三方上报也挡住 | 模型的 `herdr --current` 实测会**悄悄落到焦点 pane**（4.2），文档说会报错，实际不报；`herdr pane split` 不带目标时同样落到焦点 pane（文档 L861）。再扣 `HERDR_SOCKET_PATH` 则命名 session 下 CLI 连错 server |
| **什么都不动** | 7 家里 6 家对宿主变量的做法；herdr 的 #2668/#4919/#4814 都卡在这里 | 无 | 本 bug 原样保留，且 herdr 已表明「一 pane 一权威」不会改（#2824）；卡住后协议内无解，只能关 pane |

另外三点：

1. **「每层改写成自己的值」是 agent 自身标记的通行做法**（Claude Code 实测、Codex `CODEX_THREAD_ID`、opencode v2 `OPENCODE_SESSION_ID`），但它只管 agent 自己的变量，解决不了宿主变量。iota 目前没有自己的标记；即使加一个（例如 `AI_AGENT=iota`），herdr 的 hook 也不看它（4.2），**对这个 bug 无效**。可以作为向 herdr 上游提的方案：hook 用通用的父 agent 标记（agents.md#136 的 `AI_AGENT`，「the outermost agent wins」）判断嵌套。
2. **MCP stdio 也是同一条边界**。Crush 只剥了 bash 工具、漏了 MCP；iota 若要剥，应当 shell 与 MCP stdio 一起剥。反方向的证据：Codex #37209 的用户需要 MCP 拿到 `TMUX`/`TMUX_PANE`——剥宿主变量会打坏「依赖宿主身份的 MCP 工具」，但那个 issue 讲的是 tmux，不是上报方。
3. **cmux 有同样的形状**：iota 的 cmux 宿主靠 `CMUX_SURFACE_ID`（`src/host/cmux.rs:156`），子进程同样继承。cmux 的立场是有意让后代继承、靠服务端压掉嵌套子 agent 的状态变更（4.3），所以 cmux 下暂时没有对应的 bug 报告；本文没有在 cmux 上复现。

## 6. 查不到 / 拿不准

- **Claude Code 的 MCP 继承**只有间接证据和本机实测，文档没明说。硬拒绝在哪个版本移除：查不到。
- **Copilot CLI 的 MCP 环境**：官方文档（「The `PATH` variable is automatically inherited … All other environment variables must be configured here.」）与 issue #2107（「existing environment inheritance」+「filter list」）矛盾，本机没装，未实测。Amp、Cursor CLI 的 MCP 继承与嵌套处理：查不到。
- **Gemini CLI 没有真跑**（账号被停用），2.2 的两行是对发行 bundle 函数的离线执行，不是端到端结果。
- **「扣 `HERDR_ENV` 就够」的覆盖面**只针对 herdr 发行的 hook。自己实现 herdr 上报的 agent（Crush、iota 本身、还有 herdr 文档「Agents that support Herdr themselves」一节列的那些）是否检查 `HERDR_ENV`：没逐个查。Crush 的 `herdr.Init()` 检查哪些变量：没查。
- herdr 文档 L861 与实测不符（`--current` 不报错、落到焦点 pane），只在私有 session 里测了一次，没有查源码确认是有意还是 bug。
- herdr skill 的守卫失败之后模型会怎样（停下、还是照 `<environment>` 里的 `host: herdr` 继续）：没测。

## 材料

调研 agent 的完整笔记与本机实测的原始输出，在本次会话的 scratchpad 里（不入库）：`research-codex.md`、`research-gemini.md`、`research-claude-copilot-amp.md`、`research-aider-opencode-crush.md`、`research-hosts.md`、`peer-probe/out/*.env`、`peer-probe/compare.py`、`peer-probe/gemini-sanitize.mjs`、`herdr-issues/*.json`。
