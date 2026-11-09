# herdr 怎么知道 agent 卡在对话框上，iota 能不能照做：调查

Status: **调查（未拍板）** · 日期：2026-11-09 · 基于 `main` = `81ddf24` · herdr 0.9.5（`/usr/local/Cellar/herdr/0.9.5/bin/herdr`，protocol 22）· claude/codex 检测清单 `2026.10.11.1`

本文只做分析，不改代码。所有者的问题：herdr 监管 claude code、codex 等 agent 时，被监管的 agent 一弹出审批框或选择框，控制它的 agent 就能知道。这是怎么实现的？iota 能不能用同样的方式通知？

写作约定：仓库代码写 `path:line`。herdr 的证据写命令输出、帮助文本、清单文件或二进制字符串。**实测**表示本机跑过，**未核实**表示没跑过。没有动别人的 pane：只用了 `list/get/explain/pane read`，`explain --file` 用的是手写快照（放在会话 scratchpad）。

## 0. 结论

1. **herdr 判定 agent 状态有两条来源，自报优先**。
   - 一条是 agent 自报：`pane.report_agent` 带 `source`/`seq`/`state` 走 socket，herdr 每个 pane 只认一个 lifecycle 权威。
   - 另一条是屏幕识别：按 agent 种类加载 TOML 规则清单，规则按优先级匹配 OSC 标题、OSC 9;4 进度和屏幕若干区域。
   - `herdr integration install claude` 装的 hook 就是第一条路，装在 `~/.claude/hooks/herdr-agent-state.sh`。本机全部显示 `not installed`，所以这里的 claude/codex 面板都走第二条。
2. **iota 已经在走第一条路，而且有 `blocked`**。
   - `src/host/herdr.rs:1-8` 通过 socket 上报 `working`/`blocked`/`idle`。`lifecycle()` 把 `State::NeedsInput` 映射成 `blocked`（`src/host/herdr.rs:195-201`）。
   - 工具审批会置 `NeedsInput`（`src/repl/turn/approval.rs:56`），所以 iota 面板等审批时，`agent wait --until blocked` 理应返回。不需要标题约定，也不需要屏幕规则。
3. **缺口是覆盖面**，有三处：
   - 斜杠命令里的选择框和确认框不置 `NeedsInput`（§4）。
   - 端到端的 `blocked` 还没在真 herdr 上实测过（§4，未核实）。
   - 除 herdr、cmux 和 OSC 9;4 之外，没有一个和宿主无关的出口（§5）。

## 1. herdr 的屏幕识别：清单与规则（问题 1）

- 清单是远程拉取的 TOML，缓存在 `~/.local/state/herdr/agent-detection/remote/<agent>.toml`，共 22 个，没有 `iota.toml`。
  - 更新源是 `https://herdr.dev/agent-detection/index.toml`（二进制字符串）。
  - 有"local override"一层（`herdr server reload-agent-manifests`，帮助文本："Reload local agent detection manifest overrides"），但覆盖目录的路径未核实。
  - 二进制里有 `skipping unknown remote manifest agent`，配置里的 agent 名也是一张固定枚举（`herdr --default-config` 第 369-371 行），所以**自定义种类的清单大概率不被加载**（推断）。
- 规则字段：`state`、`priority`（高者胜）、`region`，以及 `contains`/`regex`/`line_regex`/`any`/`all`/`not` 组合。
  - `region` 的取值见 explain 输出：`osc_title`、`osc_progress`、`prompt_box_body`、`after_last_horizontal_rule`、`after_last_prompt_marker`、`bottom_non_empty_lines(N)`、`whole_recent`。
  - `skip_state_update = true` 表示"看不清，就保持原状态"。
- **claude**（`claude.toml`）：
  - `osc_title_working` p1100：标题匹配 `^[\x{2800}-\x{28FF}\x{25D0}-\x{25D3}] `，即盲文 spinner 或 `◐◑◒◓`。
  - `osc_title_idle` p250：标题匹配 `^\x{2733} `，即 `✳`。
  - `blocked` 规则：
    - `bash_permission_prompt` p850（`whole_recent`）：包含 `do you want to proceed?`，再加 `bash command`/`tab to amend`/`ctrl+e to explain` 之一，再加 `❯? 1. yes` 一类的行。
    - `generic_permission_prompt` p840。
    - `live_blocked_form` p980：最后一条横线之后出现 `esc to cancel`。
    - `mcp_elicitation_prompt`、`dynamic_workflow_prompt`。
  - `live_prompt_box` p950 → idle：提示框里有 `^\s*❯`，并且不含 `enter to select`/`esc to cancel`。
  - `model_picker_menu` p900 → `unknown`。
- **codex**（`codex.toml`）：
  - `osc_title_blocked` p1100：标题包含 `Action Required`。
  - `osc_title_working` p1050：标题里有盲文 spinner。
  - `live_strong_blocker` p900（`after_last_prompt_marker`）：`press enter to confirm or esc to cancel`、`allow command?`、`enter to submit answer` 等。
  - `weak_blocker` p600：`[y/n]`、`yes (y)`、`do you want to`+`yes`。
  - `screen_working_fallback` p500：`(12s • esc to interrupt)` 形状的计时行。
  - `osc_title_idle` p100。
- **第三种，letta**：`osc_progress_blocked` p1400 匹配 `^4;3(?:;|$)`，也就是**把 OSC 9;4;3 当 blocked**。claude 的 `osc_progress_idle` 匹配的是 `^4;0`。OSC 9;4 各个值的含义由每份清单自己定，herdr 不统一解释。
- **opencode**：`permission_required` p300，匹配 `△ Permission required`。**pi** 只有 working 规则。

所以屏幕识别是三样东西的组合：OSC 标题、OSC 9;4 和屏幕区域文本，规则按 agent 种类分开。没有统一的"状态转义序列"协议。

## 2. 现场证据（问题 2，实测）

- `herdr agent explain crop-engine --verbose`（claude，w1P:p1）给出 `state: idle`，命中 `rule: live_prompt_box (region=prompt_box_body priority=950)`，`evidence: "❯ 好，worktree 是我删的，可以忽略"`；`osc_title_idle` 也命中，但优先级低。
  - `pane read w1P:p1 --source detection` 的末尾是 `❯ …` 提示框和 `⏵⏵ auto mode on`，和 explain 一致。
- 本 pane（herdr-state）：`state: working`，命中 `rule: osc_title_working`，`evidence: "◑ Herdr agent 状态检测机制调研"`。
- 手写快照（`agent explain --file F --agent L`）：

| 快照 | agent | 结果 | 决定规则 |
|---|---|---|---|
| Bash 审批框（`Do you want to proceed?` / `❯ 1. Yes` / `Tab to amend`） | claude | **blocked** | `bash_permission_prompt` p850 |
| 空 `❯ ` 提示框加横线 | claude | idle | `live_prompt_box` p950 |
| `Would you like to run…` / `Press enter to confirm or esc to cancel` | codex | **blocked** | `live_strong_blocker` p900 |
| `• Working (12s • esc to interrupt)` | codex | working | `screen_working_fallback` p500 |
| 上面那个 codex 审批快照 | iota | unknown | `manifest: none` |

- iota 面板：`agent list` 里显示 `"agent":"iota"`、`agent_status` 为 working/idle，但 `agent explain w16:p5` 报错 `agent_explain_unavailable: … does not have a detected agent label`。
  - 由此可见，这个标签和状态**不是屏幕识别出来的**，而是 iota 自报的（`source=agent=iota`，`src/host/herdr.rs:72`）。
  - 自报的面板也没有 `interactive_ready` 字段，claude 面板有。

## 3. 控制方收到什么（问题 3）

- **事件通道**：`herdr api schema --json` 里有 `events.subscribe`，订阅类型是 `pane.agent_status_changed`，可选按 `agent_status` 过滤。
  - 推送的事件是 `pane_agent_status_changed`，字段有 `pane_id`、`workspace_id`、`agent_status`、`agent`、`display_agent`、`title`、`state_labels`。
  - 另有 `events.wait`（二进制字符串："events.wait currently supports pane agent status matches"）。
- **CLI 封装**：
  - `agent wait <t> --until blocked [--timeout]` 在服务端等这个事件。不带 `--until` 时，等到 idle、done 或 blocked 任一个就返回。
  - `agent prompt --wait` 要求先观察到 working 或 blocked，5 s 内没有就返回 `agent_prompt_stalled`。
  - 目标已经 `blocked` 时，`agent prompt` 不发任何输入，直接返回 `agent_blocked`（SKILL.md:145）。
- **`agent get` 的字段**：
  - `agent_status`。
  - `state_change_seq`：全局单调，每次状态变化都 +1。
  - `completion_seq`：最近一次完成时的序号。`done` 等于 idle 加上"完成后还没被看过"，focus 才会标记为已看过，read 不会（SKILL.md:59）。
- **给人的通知**：herdr 根据语义状态自己弹桌面通知、放声音（配置里有 `request_path` "needs-attention"、`done_path`）。`herdr notification show` 只是让 agent 主动弹一条通知，不属于状态通道。
- 所以监督者（比如 PM 会话）收到的，是**它自己挂着的 `agent wait`/`prompt --wait` 返回**。返回发生在状态变成 blocked 的那一刻：自报来源是在 iota 发出 `report_agent` 的那一刻，屏幕来源是在下一次快照判定之后。

## 4. iota 现在呈现什么（问题 4）

- **标题**：只是会话标题，没有状态前缀（实测：`"terminal_title":"Venice.ai 隔夜会话超时"`）。即使有清单，`osc_title` 规则也无从下手。
- **OSC 9;4**（ANSI 宿主，每个 run 都会追加）：Busy 发 `9;4;3`，NeedsInput 发 `9;4;4;100`，Error 发 `9;4;2;100`，Idle 发 `9;4;0`（`src/ui/runtime/osc.rs:31-40`，`src/host/ansi.rs:56`）。
- **审批框**的底栏是 `… · ↑↓ move · Enter … · Esc cancel`（`src/ui/surface/tabbed.rs:651-659`），标题是 `<tool> wants to modify files — allow?`（`src/repl/turn/approval.rs:65`）。屏幕规则写得出来，但这是第二套真相（§5e）。
- **置 `NeedsInput`（会报 blocked）的地方**：
  - 工具审批：`src/repl/turn/approval.rs:56`。
  - 交互式工具：`src/repl/turn/tools.rs:316`。
  - 压缩提议：`src/repl/commands/compact.rs:483`。
  - 外来消息待放行：`src/repl/run.rs:1251`。
- **不置的地方**（这时 pane 仍是 idle）：
  - `/model` 切换确认：`src/repl/commands/model.rs:305`。
  - `/model` 选择面板：`src/ui/runtime/handle.rs:772,813`。
  - `/new` 丢弃确认：`src/repl/commands/new.rs:51`。
  - `/export` 选择器：`src/repl/commands/export.rs:770`。
  - 这些都由用户的斜杠命令触发，说明人就在键盘前。claude 对同类情况（`model_picker_menu`）的处理是 `unknown`，并且不更新状态。真正的风险是监督者在这个时候 `agent prompt`，文字会被打进选择器（推断）。
- **ESC 中断**之后没有追问对话框。所有 `select`/`confirm` 调用点就是上面这些，找不到中断后的问题框。
- **shell 命令确认**就是走 `requires_approval` 的工具审批（`src/repl/turn/tools.rs:245-247`），已经覆盖。
- **未核实**：没有在真 herdr 上观察到 iota 面板进入 `blocked`。
  - herdr 服务端日志不记 report。
  - 测试只钉住了线上形状和顺序（`src/host/herdr.rs:613` `test_request_shapes`、`:682` `test_host_sequence`）。
  - 另一个已知的失效方式：子进程 hook 抢走 lifecycle 权威，已由扣留 `HERDR_ENV` 修复（`src/host/herdr.rs:20-31`，`docs/design/child-env-identity-research.md`）。

## 5. 选项对比（问题 5）

| 选项 | 是否有文档的扩展点 | 成本与风险 |
|---|---|---|
| (a) 标题约定（`✳ `/spinner/`Action Required`） | 不是。规则在 herdr 的**按种类**清单里，iota 不在 22 个种类之中，自定义清单大概率被跳过（§1） | 要 herdr 上游收一份 `iota.toml`。标题会被状态前缀污染。只对 herdr 有效 |
| (b) OSC 9;4 | 是 `osc_progress` 区域，但**含义按清单各定**（letta 把 4;3 当 blocked，iota 用 4;3 表示 Busy） | iota 已经在发。没有清单就没用，而且语义没有标准 |
| (c) herdr 自报协议（`pane report-agent`/socket `pane.report_agent`） | **是**，有文档（`--help`、API schema）。herdr 自家 integration 的 hook 走的也是它 | **iota 已实现**。剩下的成本只有覆盖面。风险是一 pane 一个权威（已处理） |
| (d) iota 自己的带外信号（状态文件或 `iota status --json`） | iota 自定的协议 | 要新造协议、新造 CLI 动词，任何监督者都得来轮询或订阅。目前除 herdr、cmux 外没有具体的消费者，属于推测性抽象 |
| (e) 屏幕抓取 iota 的 UI | herdr 清单，但不能自定义种类（同 a） | iota 每改一次文案都可能悄悄破坏它。和 (c) 重复 |

"任何监督者，不只 herdr"：真实存在的监督者都是**宿主**（herdr、cmux、普通终端），iota 的 `Host` trait 已经把同一个 `State` 扇出给每一个宿主（`src/host/mod.rs:220`）。一个新监督者应当再加一个 `Host`，而不是让所有人去轮询 iota。(d) 等出现第一个不是宿主的真实消费者时再做。

## Recommendation

走 (c)，并且只补覆盖面，不新增通道：

1. 用户停在 iota 的任何对话框（`select`/`confirm`）上，宿主都应该看到 `blocked`。做法是让这件事在 UI facade 的一个点上成立，而不是在每个调用点上补 `set_state`。斜杠命令的选择器也要包括在内，因为对监督者来说"别往这里打字"才是要点。
2. 不做标题约定、不做自有状态文件、不加配置项。
3. 把真 herdr 上的端到端观察写成一条可复跑的检查。这是本文唯一没有坐实的环节。

## 第一个可验证增量

- 产物：facade 层的"对话框开着就是 NeedsInput"改动，加单测：`ScriptedUi` 打开 `/model` 确认框时，记录到 `NeedsInput`，关掉后恢复原状态。
- 端到端（手动，在所有者自己开的新 pane 里）：
  - 先 `herdr pane split --current` 开一个 pane 并在里面跑 iota，让模型调用一个需要审批的工具。
  - 另一个终端执行 `herdr agent wait <pane> --until blocked --timeout 60000`，应当返回。
  - 接着 `herdr agent prompt <pane> x` 应当返回 `agent_blocked`。
  - 选 Allow 之后，`agent wait --until working` 应当返回。
- 对 `/model` 确认框重复同样的三步。

## 实测（2026-11-09）

环境：herdr 0.9.5；iota = `target/debug/iota`（main @ 38f39aa，自报 v0.7.7），`-M deepseek-flash --no-save`；被测 pane `w16:pA` 由 `herdr pane split --pane w16:p9 --direction down --cwd /tmp/iota-e2e --no-focus` 创建，自带 `E=1 P=w16:pA S=~/.config/herdr/herdr.sock`（`echo $HERDR_ENV $HERDR_PANE_ID $HERDR_SOCKET_PATH`），无需手动注入。启动后 `herdr agent get w16:pA` → `"agent":"iota","agent_status":"idle"`。

**1. 审批 → blocked：成立。**
- 让模型 `write_file /tmp/iota-e2e/hello.txt`，屏幕停在 `write_file hello.txt wants to modify files — allow?`。`herdr agent wait w16:pA --until blocked --timeout 120000` 1.4s 返回 `"agent_status":"blocked"`。
- `herdr agent prompt w16:pA "x"` → `{"code":"agent_blocked","message":"agent w16:pA is blocked and requires interactive input"}`，exit 1，对话框未变、无输入。
- `herdr agent send-keys w16:pA enter` → `{"code":"agent_not_ready","message":"agent w16:pA is not an active named agent"}`，**不能用来批准**；改用 `herdr pane send-keys w16:pA enter`。预先挂起的 `agent wait --until working` 0.54s 返回 `working`，文件写入 `hi`。
- 回合结束的状态是 `done` 再到 `idle`：`--until done --until idle` 返回 `done`，5s 后 `agent get` 是 `idle`。第一次只用 `--until idle` 等了 60s，结果超时（`timed out waiting for agent status`），事后 `agent get` 已经是 `idle`。

**2. `/model` 选择器 → 仍是 idle：成立（§4 的缺口确实存在）。**
- 打开前和打开后（屏幕上是 `Model │ Context │ … ▸ deepseek:deepseek-flash (current)`），`agent get` 都是 `"agent_status":"idle"`。
- 后果：`herdr agent prompt w16:pA "x"` → `agent_not_ready … not an active named agent`，exit 1，选择器中没有出现任何输入。`herdr agent rename w16:pA e2e-iota` 之后再执行 `agent prompt e2e-iota "x"`，结果相同。所以在 herdr 0.9.5 上，`agent prompt`/`agent send-keys` 只要状态不是 blocked，就会拒绝 `iota` 这个种类，不会往选择器里打字。直接的 `pane send-text` 不看状态，不在此列。这个缺口目前只影响监督者看到的信息（它以为 iota 空闲），这条路径上没有误输入。随后用 Esc 取消，模型没有变。
- 未测：`/model` 确认框、`/new` 丢弃、`/export` 选择器（它们和选择器同属 facade 对话框，未逐个观察）。

**3. 退出 → 标签消失：成立。**
- 空闲时按 Ctrl+D（`herdr pane send-keys w16:pA ctrl+d`），回到 zsh。`agent get w16:pA` 返回 `agent_not_found`，`pane get` 返回 `"agent_status":"unknown"`，也就是改由屏幕识别接管，而它识别不到 agent。
- 注意：在 iota 里输入 `/exit` 并不会退出，这行文字被当作消息发给了模型（↑ token 41.2k→54.9k）。退出前一刻的状态是 `done`。
