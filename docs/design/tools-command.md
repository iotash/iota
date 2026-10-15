# `/tools` 的重构：一张来源列表，点进去是该来源的页

Status: **已实现（2026-10-15，分支 `mcp-child-death`，未合并）** · 日期：2026-10-15 · 基线：分支上 `e55bab8`（X-78）之后 · DIVERGENCES X-79

所有者的原话：「第三个我想趁机对 tools 命令做一个重构，现在有两个 tab，我希望整合成一个。显示的是 mcp 列表菜单，包括 mcp 名称，状态，方法数（内置的都收到 built in 里）。点击相应 mcp 可以进入对应的方法列表二级页面，在这个页面还可以显示 mcp 的更详细信息，以及加上你刚刚说得重连功能。」

本文记的是设计取舍与实现后的形状。它是 docs/design/mcp-child-death-research.md（诊断）、X-77（措辞）、X-78（down 与调用截止时间）之后的第三件。

## 写作约定

- 代码坐标指本分支的提交。真机抓屏来自 `target/debug/iota`，运行在私有 tmux 里；用的是本地 mock MCP 服务器和本地假模型，不联网。抓屏里 scratch 目录的长路径写作 `<scratch>`。
- 「一级」指列表，「二级」指某个来源的页。

## 1. 改之前

- `cmd_tools` 建一个 `TabbedSpec`，里面是两个只读 `View` 面板，刷新周期 500 ms，Enter 的提交结果被丢弃。
  - **Tools** 页：一行一个工具，标 `[built-in]` 或 `[mcp: <server>]`。
  - **MCP** 页：一个服务器几行，依次是 `[connected]`/`[disconnected]`、`endpoint:`、`tools (N): …`、`error: <第一行>`，X-77 之后还有细节行。
- 面板机制里没有「点进去」：`PanelResult.path` 是给目录浏览器用的，`TabbedSpec.enter_advances` 是给向导用的。
- 先例：`/jobs`（`src/repl/commands/jobs.rs`）已经是「列表 → `View` 详情页 → Esc 回列表」的循环，而且 2026-09-23 定过一条：同一个界面上一个键不许有两种含义（所以 `/jobs` 的杀进程放在单独的 Kill tab，而不放在页面的按键上）。
- 已核实：**所有面板按 Enter 都会提交，`View` 也不例外**（`src/ui/surface/mod.rs` 的 `enter_commit`）；Esc 和 `q` 都是取消。所以「Enter 进入 / Esc 返回」用现有机制就能做，**不需要改 `src/ui/`**。

## 2. 设计

### 2.1 导航：一个循环，不新增机制

```
/tools ──► 一级 List 面板「Tools」 ──Esc──► 关闭
              │ Enter（光标所在行）
              ▼
           二级 View 面板（标题 = 来源名） ──Esc──► 回到一级，光标停在原来那一行
              │ Enter
              ▼
           服务器页：开始重连（后台），页面重新打开、继续刷新
           built in 页：等同 Esc
```

- 两级都只有一个面板，所以没有 tab 栏。
- 两级都保留 500 ms 刷新：一级里后台连接完成、服务器 down、重连结果都会实时出现；二级里重连的进度也是靠刷新显示出来的。
- 一级每行带一个稳定的 key（`built in`、`server:<i>`），刷新时光标跟着行走，而不是跟着行号走。从二级回来时，用 `with_cursor` 把光标放回进去之前那一行。
- 二级的面板开了折行（`with_wrap`），长 endpoint、stderr 不会被截断。
- 面板底部的通用按键说明（`q/Esc close`）是 `src/ui/` 的固定文案，这次不动。二级的提示行写的是 `Esc: back`。

### 2.2 一级：一行一个来源

格式是 `<名字>  <工具数>  <状态>`，三列对齐。工具数放在状态前面，这样长状态被截断时只截掉末尾。

| 情形 | 工具数一列 | 状态一列（颜色） |
|---|---|---|
| `built in`（固定第一行） | `N tools` | `available`（绿） |
| 已连接 | `N tools`，有延迟加载组时加 `· deferred (k/m loaded)` | `connected`（绿） |
| 正在连接 | `—` | `connecting…`（灰） |
| down（X-78） | `N tools`（工具留着） | `down`（红）+ `: <X-77 概要>` |
| 等待登录 | `—` | `needs login`（黄）+ `: run iota mcp login <name>` |
| 失败 | `—` | `failed`（红）+ `: <X-77 概要>` |

提示行（面板上方）：`<n> MCP server(s) · <m> tool(s) · Enter: open · Esc: close`。

真机（`dead` 是在**没有任何调用**的情况下被 `kill` 掉的；`flaky` 同样被杀）：

```
 4 MCP servers · 10 tools · Enter: open · Esc: close
▸ built in  2 tools  available
  dead      3 tools  down: exited (its last stderr line: "mock_mcp: <- tools/list") — its tools will fail until it …
  flaky     3 tools  down: exited (its last stderr line: "mock_mcp: <- tools/list") — its tools will fail until it …
  ok        2 tools  connected
  refused   —        failed: can't reach http://127.0.0.1:1/mcp: connection refused — check the url, and that the s…
```

### 2.3 二级：一个来源的页

**服务器页**，从上到下：

| 行 | 内容 | 来源 |
|---|---|---|
| `status` | 与一级同一句（状态词 + X-77 概要） | `ServerStatus` |
| `endpoint` | URL，或完整命令行 | `ServerStatus::endpoint` |
| `transport` | `stdio` / `http` | `Manager::detail` |
| `auth` | stdio 一律 `none`（与 `iota mcp list` 一致）；HTTP 是 `auto`/`oauth`/`none`，后面跟登录状态（`logged in`、`expired`、`not logged in`） | `ServerStatus::auth/login` |
| `tool names` | `mcp__<segment>__<tool>`（只在已连接或 down 时显示） | `ServerStatus::wire_prefix` |
| `call timeout` | 人话时长，如 `30 minutes`、`90 seconds`（X-78 的截止时间，`tool_timeout_sec` 覆盖默认值） | `Manager::detail` |
| `why` | 失败或 down 时：**完整的**原因文本（细节行，含 stderr 尾部，最多 40 行） | X-77 的细节行 |
| `stderr (last N lines)` | 已连接的 stdio 服务器：最近写到 stderr 的 10 行 | 会话的 stderr 捕获 |
| `tools (N)` | 每个工具一行：工具的**原名**（服务器自己的叫法）、描述的前 100 个字符、延迟加载状态（未加载的整行置灰） | dispatcher + 状态 |

二级提示行（也随刷新变化）：

| 情形 | 提示行 |
|---|---|
| 没有 manager（没配 MCP 的构建、多数单测） | `Esc: back` |
| 可以重连 | `Enter: reconnect · Esc: back` |
| 重连中 | `Reconnecting… · Esc: back` |
| 重连成功 | `Reconnected · N tools · Enter: reconnect · Esc: back` |
| 重连失败 | `Reconnect failed: <X-77 概要> · Enter: try again · Esc: back` |

**`built in` 页**：内置工具没有服务器，也就没有 endpoint、认证、stderr，没有可以重连的东西。所以这一页只有三样：`status available`；`source the tools iota ships with — no server behind them, so nothing to reconnect`；工具列表。提示行是 `Esc: back`，按 Enter 等同 Esc。

真机（`flaky` 被杀之后、重连之前）：

```
 Enter: reconnect · Esc: back
status        down: exited (its last stderr line: "mock_mcp: <- tools/list") — its tools will fail until it is reconnect
ed from /tools
endpoint      <scratch>/bin/flaky.sh
transport     stdio
auth          none
tool names    mcp__flaky__<tool>
call timeout  30 minutes
why
  stderr:
    mock_mcp: started pid=14986
    mock_mcp: <- initialize
    mock_mcp: <- notifications/initialized
    mock_mcp: <- tools/list
```

### 2.4 重连

- **在哪触发**：只在二级的服务器页，按 Enter。一级的 Enter 已经用来「进入」了；如果在一级也能重连，就得新增按键，而新增按键要动 `src/ui/`。
- **形式**：按键触发，不做成「一行可选」。`View` 面板的行不可选，若改用 `List` 面板放一行「Reconnect」，详情和工具就都成了可选行，Enter 在它们上面没有意义。
- **执行**：`Manager::reconnect(name)` 在后台任务里跑（行为不变：先 detach，再在同样的 30 秒截止时间下重新连接、合并）。页面立刻重新打开，靠刷新显示 `Reconnecting…` 和状态的变化。同一个服务器的重连正在进行时，再按 Enter 不会重复启动。
- **结果去向**（就算用户已经离开 `/tools`）：
  - 页面提示行：`Reconnected · N tools` 或 `Reconnect failed: <概要>`。
  - 转录一行：成功是 `MCP <name> reconnected · N tools`；失败与启动提示同一套格式：`⚠ MCP <name>: <概要> · details in /tools`。
  - 一级：状态变成 `connected`（工具数更新），或 `failed: <概要>`。
- **措辞**：X-78 里那句「restarting iota …」改为指向这里——`its tools will fail until it is reconnected from /tools`；模型拿到的是 `its tools won't work until it is reconnected, which the user can do from /tools`。

真机：

```
（flaky 页，按 Enter 之后）
 Reconnecting… · Esc: back
（3 秒后，flaky 的下一次启动会故意失败）
 Reconnect failed: "flaky.sh" exited during startup: flaky: license server unreachable · Enter: try again · Esc: back
status        failed: "flaky.sh" exited during startup: flaky: license server unreachable
…
why
  connection closed: initialize response
  stderr:
    flaky: license server unreachable

（回到一级：dead 重连成功，flaky 重连失败）
 4 MCP servers · 7 tools · Enter: open · Esc: close
  built in  2 tools  available
▸ dead      3 tools  connected
  flaky     —        failed: "flaky.sh" exited during startup: flaky: license server unreachable
  ok        2 tools  connected
  refused   —        failed: can't reach http://127.0.0.1:1/mcp: connection refused — check the url, and that the s…

（转录）
⚠ MCP flaky: "flaky.sh" exited during startup: flaky: license server unreachable · details in /tools
MCP dead reconnected · 3 tools
```

tmux 场景 23 用真实的 OAuth 走了一遍：`iota mcp login nb` 之后，**在同一个聊天里**从 nb 的页面重连，就用上了新 token，页面显示 `connected`、工具 `echo`，转录记下 `MCP nb reconnected · 1 tool`。原来做不到这一点（X-42：「a chat already running does not pick a new token up」）。

### 2.5 两件顺带的事

- **打开 `/tools` 时要能看到「空闲时被杀」**：`Manager::servers()` 每次取快照时，先找出「状态是 connected、但 `Session::is_closed()`（rmcp `Peer::is_transport_closed`）为真」的服务器，按 X-78 的同一条路径标成 down（同步版 `mark_down_now`，用 `stderr_snapshot` 取当前已捕获的 stderr，不等待），notice 照常发一次。Codex 在查询状态时也是这样查的。一级每 500 ms 刷新一次，也就是每 500 ms 查一次。
- **时长的人话格式**：新增 `text::human_duration`，按「能整除的最大单位」输出：`30 minutes`、`1 hour`、`90 seconds`、`300 ms`。连接超时、调用超时的措辞和页面上的 `call timeout` 都改用它，`go_duration` 留给别处原有的用法。

另有一处措辞调整：X-77 里 stdio 的主语从「完整命令」改成「命令的文件名」（`"/…/flaky.sh"` → `"flaky.sh"`）。完整路径会把失败原因挤到一行之外，而完整命令行本来就在 endpoint 那一行。

## 3. 取舍与拿不准的地方

1. **Enter 在二级是「重连」**。页面上 Enter 只有这一个含义，不违背 `/jobs` 那条「一个键不许有两种含义」。这是动作，不是导航，对一个已连接的服务器也会重启它（无害，但会中断它正在做的事）。好处是不新增按键、不动 `src/ui/`，并且在提示行写明了。如果你更想要「一个明确的确认」或者「一行可选的 Reconnect」，那需要 `View` 面板支持可选行，或者支持自定义按键——那就要动 `src/ui/`了。
2. **重连失败会拿掉工具**：`reconnect` 先 detach，所以 down（工具留着）之后重连失败，就变成 failed（工具没了）。这与 X-78「down 不摘工具」并不冲突，因为 failed 与启动时连不上是同一个状态。但对模型来说，工具数组因此变了一次。成功的重连也会改变工具数组（工具重新排到末尾）。按要求没有改 `reconnect` 的行为。
3. **快照探测的副作用**：`servers()` 是个读取接口，现在会改状态、发 notice。这样做的好处是任何视图（`/tools`、`/status`）都不会显示一个已经死掉的服务器是 connected。
4. **通知通道的一个边角**：启动时一个服务器都没连上，通知用的发送端就会放掉（X-78）。之后即使从 `/tools` 重连成功、再死掉，也不会有 down 的 notice；状态和调用错误照样正确。
5. **一级里 down 的概要写着「until it is reconnected from /tools」**，人已经在 `/tools` 里了，读起来有点多余。但它和启动 notice、模型拿到的文字是同一句，我没有为一级单独改写。

## 4. 实现与验证

- `src/repl/commands/tools.rs`：整体重写。纯渲染函数是 `overview`、`server_page`、`built_in_page`、`page_prompt`、`status_word`；`cmd_tools` 是两级循环；`start_reconnect` 是后台重连。
- `src/mcp/manager.rs`：`servers()` 做快照探测，新增 `mark_down_now`、`Manager::detail` / `ServerDetail`。
- `src/mcp/transport.rs`：`Session::is_closed`、`Session::stderr_snapshot`。
- `src/mcp/explain.rs`：措辞改为指向 `/tools`，stdio 主语用文件名，超时时长用人话。
- `src/text/mod.rs`：`human_duration`。
- 测试：
  - `repl::commands::tools::tests`：一级状态映射（六种情形）、服务器页、stdio stderr 尾部、`built in` 页、提示行、描述截断。
  - `tests/repl/commands.rs`：
    - `tools_is_one_list_and_a_page_per_source`：一级 → `built in` 页 → 一级，光标位置、刷新、单 tab。
    - `tools_reconnects_a_server_from_its_page`：真实 sh 服务器，重连成功与失败、转录行、光标回位。
  - `tests/mcp/manager.rs::a_snapshot_finds_a_server_that_died_idle`：空闲时死掉，快照即显示 down。
  - `text::tests::human_duration_names_the_largest_whole_unit`。
  - tmux 场景 23 改为新导航，并加入「同一个聊天里重连」。
