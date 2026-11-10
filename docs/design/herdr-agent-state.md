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
- **审批框**的底栏是 `… · ↑↓ move · Enter … · Esc cancel`（`src/ui/surface/tabbed.rs:651-659`），标题是 `<tool> — allow?`（`src/repl/turn/approval.rs:65`）。屏幕规则写得出来，但这是第二套真相（§5e）。
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
- 让模型 `write_file /tmp/iota-e2e/hello.txt`，屏幕停在 `write_file hello.txt — allow?`。`herdr agent wait w16:pA --until blocked --timeout 120000` 1.4s 返回 `"agent_status":"blocked"`。
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

## 合并后复验（2026-11-09）

环境：herdr 0.9.5；`target/debug/iota` 由 main @ ef4d73c 构建（自报 v0.7.7），模型用配置默认的 glm-5.3，全程没有发出聊天消息。被测 pane `w16:pC` 由 `herdr pane split --pane w16:pB --direction down --cwd /tmp/iota-verify --no-focus` 创建，`echo "ENV=$HERDR_ENV PANE=$HERDR_PANE_ID"` → `ENV=1 PANE=w16:pC`。启动后 `agent get` → `"agent":"iota","agent_status":"idle"`。

**1. `/exit` 退出：成立。**
- `pane send-text w16:pC '/exit'` 后再 `pane send-keys w16:pC Enter`：屏幕回到 `joyqi@… iota-verify %`，底栏退出前保持 `glm-5.3 · ≈0% / 1m`，没有出现回合。
- `herdr agent get w16:pC` → `{"code":"agent_not_found",…}`。
- 会话 `jewdqe1wsnmn` 在 `~/.iota/sessions` 下没有任何目录，说明没有消息落盘。第二次会话 `3smwc93f9gp4` 同样用 `/exit` 退出，结果相同。

**2. `/model` 选择器 → blocked：成立。**
- 打开前 `agent get` → `idle`。先在后台挂起 `herdr agent wait w16:pC --until blocked --timeout 120000`，再发送 `/model` 和 Enter。wait 返回 `"agent_status":"blocked"`，`state_change_seq` 为 1244，exit 0。
- `herdr agent prompt w16:pC "x"` → `{"code":"agent_blocked","message":"agent w16:pC is blocked and requires interactive input"}`，exit 1，选择器没有收到任何输入。昨天这里返回的是 `agent_not_ready`。
- 按 Esc（`pane send-keys w16:pC Escape`）后，`agent get` → `"agent_status":"done","completion_seq":1245,"focused":false`，25s 后仍是 `done`。这里**不是**字面上的 `idle`：iota 只会上报 working/blocked/idle（`src/host/herdr.rs:197-199`），`done` 是 herdr 在上报的 idle 之上加的"完成未看过"标记，只有 focus 才会清除（见上文 `completion_seq`）。由于 `--no-focus`，没有再确认它会不会落回 `idle`。可以确定的是状态没有停在 blocked。
- 副作用：从 blocked 回到 idle 也会被 herdr 记成一次"完成"，未聚焦的 pane 会因此显示 done。

**3. 审批路径只上报一次：跳过。** 没有不调用模型就能触发审批的办法，这一项依赖该分支的单元测试。

清理：`/exit` 退出 iota → `herdr pane close w16:pC`（返回 ok，之后 `pane get` → `pane_not_found`）→ `rm -rf /tmp/iota-verify`。

## 其它 agent 怎么对待对话框（2026-11-09）

环境：herdr 0.9.5；Claude Code 2.1.319；codex-cli 0.165.0；gemini 0.39.1（只读了 manifest）。manifest 取自 `strings $(which herdr)`，也就是内置的 claude `2026.10.11.1`、codex `2026.10.11.1`、gemini `2026.06.10.1`；`agent explain` 显示实际生效的是 `~/.local/state/herdr/agent-detection/remote/{claude,codex}.toml`，版本相同。所有测量都在自建的 `w16:pE` 上进行（`pane split --pane w16:pD --direction down --cwd /tmp/peers-probe --no-focus`），测完 `pane close` → `pane_not_found`，`/tmp/peers-probe` 已删。

**1. 完成规则：进入 idle 时，前一个状态是 working 或 blocked，才算一次完成。** skill 只写了"seen 状态区分 idle/done，focus 才标记 seen"；schema 对 `completion_seq` 的描述是 "The current idle transition completed work"；`pane report-agent --state` 只接受 `idle|working|blocked|unknown`，没有"不算完成"之类的参数。规则是实测出来的：在 pane 里跑 `sleep 3000` 当前台进程，用假 source `--source probe --agent probe` 逐个上报，每步之后执行 `agent get`：
- working→idle → `done, completion_seq=1273`；blocked→idle → `done, completion_seq=1275`（间隔 3s 重测一次，结果 `1285`，相同）
- idle→unknown→idle → `idle, completion_seq=None`；blocked→unknown→idle → `idle, None`；**working→unknown→idle 也是 `idle, None`**（真实完成的标记被吞掉了）
- idle→idle → `state_change_seq` 不变（1280）。前台进程是 shell 时，上报 idle 会直接得到 `agent_not_found`。
- 所以 source 能避开这个标记：先经过 unknown 再进 idle。herdr 文档里没写这条规则，它是 0.9.5 上的实测行为。

**2. claude：只有审批和确认类表单才是 blocked，选择器保持原来的状态。** 在已信任的 iota 目录里启动 `claude`（没有发出任何消息）：
- 启动后 → `idle`，`state_change_seq=1290`。`/model` 打开时屏幕显示 `Select model … Enter to set as default · s to use this session only · Esc to cancel`，`agent get` 仍是 `idle` / 1290。`agent explain --verbose` 给出 `state: unknown`、`rule: model_picker_menu`，也就是该规则 `state = "unknown"` + `skip_state_update = true`：判定是 unknown，但不写入，保留原状态。Esc 之后 `idle`，`completion_seq=None`，seq 仍是 1290，**没有 done**。
- `/resume` 选择器 → `idle`，命中的是 `osc_title_idle` 兜底（没有任何对话框规则命中）。Esc 之后同样没有 done。
- 信任目录对话框（`/tmp/peers-probe`，`Enter to confirm · Esc to cancel`）→ `blocked`，`rule: live_blocked_form`。这个 manifest 把带 "esc to cancel" 加 "enter to confirm/select" 页脚的表单都判成 blocked。为了不写 `~/.claude.json` 没有点信任，用 Esc 退出了，所以 claude 的 blocked→idle 有没有 done **未核实**（按第 1 条推断会有）。
- 副作用：claude 留下了一个 session 记录（`e26921b3-…`），配置没有改动。

**3. codex：只测到一个启动通知，选择器和审批 未核实。** 在 `/tmp/peers-probe` 里启动 `codex`，屏幕是 `GPT-5.4 is no longer available … enter/esc continue · ⌃c quit`，`agent get` → `idle`，explain 给出 `rule: none`。按 Enter/Esc 会切换模型，可能改写 `~/.codex/config.toml`，所以改用 Ctrl+C 退出（config 的 mtime 仍是 Oct 28）。以下只来自 manifest：标题含 "Action Required" 时 `osc_title_blocked`（priority 1100），屏幕出现 "press enter to confirm or esc to cancel"/"allow command?"/"enter to submit answer" 时 `live_strong_blocker` 判 blocked，信任目录 `trust_directory` 和更新提示 `startup_update` 也判 blocked；codex 只有 `transcript_viewer` 一条规则用 `skip_state_update`（unknown，保留原状态）。gemini 的 manifest（未核实）：只有 apply/allow 判 blocked；任何带 "esc to cancel" 的画面都判成 **working**（`esc_cancel_working`），关闭这样的画面就是 working→idle，按规则会显示 done。

**4. 对比**

| | 对话框打开时上报 | `agent prompt` 受保护？ | 关闭后有 done？ |
|---|---|---|---|
| claude | 审批/确认表单 blocked；`/model` 判 unknown 但保留原状态（实测 idle）；其它选择器 idle | 表单：是（`agent_blocked`）；选择器：否，状态是 idle，prompt 会照常发出（按 skill §prompt 推断，未实测，因为 Enter 会改写默认模型） | 选择器：否（实测）；表单：未核实，推断有 |
| codex | 审批/确认 blocked（manifest）；启动通知 idle（实测） | blocked 时是；通知：否 | 审批后直接进入 working，一般看不到；通知：否 |
| iota | 任何 facade 对话框都是 blocked（ef4d73c） | 是（`agent_blocked`，上一节实测） | 是：Esc 之后 `done, completion_seq=1245`（上一节实测） |

**结论：保留 blocked，不要学 claude。** claude 让选择器保持 idle，代价是监督者看到的是一个可以输入的 agent，`agent prompt` 会把文字打进选择器里。claude 也只把会拦住回合的表单报成 blocked。iota 如果改报 idle 或 unknown，会失去两样东西：`agent_blocked` 拒绝（以及 `wait --until blocked` 这个信号），还有"有人在等用户"这个侧栏状态。done 这个副作用可以在不放弃 blocked 的前提下去掉：对话框关闭、而回合本身是 idle 时，先报 `unknown` 再报 `idle`（实测 blocked→unknown→idle 没有 done）。这一跳**只能**用在对话框关闭上，回合结束时不能用，因为 working→unknown→idle 会吞掉真实的完成。代价是 sidebar 上会闪一下 unknown，而且依赖的是 herdr 未写入文档的行为，需要用测试固定下来。是否这样做由 owner 决定，本节不改源码。
