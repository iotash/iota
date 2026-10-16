# `/tools` 的重构：一张来源列表，点进去是该来源的页

Status: **已实现（2026-10-15，分支 `mcp-child-death`，已合并 `8535fd5`）；§3 的 2、4 两条 2026-10-16 修掉（分支 `tools-fixes`，见 §5）** · 日期：2026-10-15 · 基线：分支上 `e55bab8`（X-78）之后 · DIVERGENCES X-79

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
| 正在重连（之前有工具） | `N tools`（工具留着） | `reconnecting…`（灰） |
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
- **执行**：`Manager::reconnect(name)` 在后台任务里跑：先关掉旧 session（同一时刻只有一份服务器进程），再在同样的 30 秒截止时间下重新连接。页面立刻重新打开，靠刷新显示 `Reconnecting…` 和状态的变化。同一个服务器的重连正在进行时，再按 Enter 不会重复启动。
- **终态**（2026-10-16 起，§5）：

  | 重连前 | 进行中 | 成功 | 失败 |
  |---|---|---|---|
  | 有工具（connected 或 down） | `reconnecting…`，工具留着、照常提供给模型；这时的调用答 `it is being reconnected — try again in a moment` | `connected`：旧工具、索引、segment、前缀先拿掉，再按新列表合并（工具名还是 `mcp__<同一 segment>__…`，排到末尾） | `down: <这次失败的概要>`：工具、wire 名、前缀、工具数组**原样不动**，旧 session 已关，调用答 X-78 那句；`why` 是这次失败的全文 |
  | 没有工具（failed、needs login） | `connecting…` | 同上 | `failed`（与启动时连不上同一个状态） |
- **结果去向**（就算用户已经离开 `/tools`）：
  - 页面提示行：`Reconnected · N tools` 或 `Reconnect failed: <概要>`。
  - 转录一行：成功是 `MCP <name> reconnected · N tools`；失败与启动提示同一套格式：`⚠ MCP <name>: <概要> · details in /tools`。
  - 一级：状态变成 `connected`（工具数更新）；失败时，之前有工具的是 `down: <概要>`（工具数不变），之前没有的是 `failed: <概要>`。
- **措辞**：X-78 里那句「restarting iota …」改为指向这里——`its tools will fail until it is reconnected from /tools`；模型拿到的是 `its tools won't work until it is reconnected, which the user can do from /tools`。

真机：

```
（以下是 2026-10-15 的抓屏，那时失败的重连会把 flaky 变成没有工具的 failed；现在的样子见 §5）
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
2. ~~**重连失败会拿掉工具**~~（2026-10-16 已修，§5）：原来 `reconnect` 先 detach，所以 down（工具留着）之后重连失败，就变成 failed（工具没了），工具数组因此变了一次，与 X-78「不摘工具」拧着。现在失败回到 down、工具原样。成功的重连仍会改变工具数组（新工具排到末尾）——那是服务器真的换了一份工具。
3. **快照探测的副作用**：`servers()` 是个读取接口，现在会改状态、发 notice。这样做的好处是任何视图（`/tools`、`/status`）都不会显示一个已经死掉的服务器是 connected。
4. ~~**通知通道的一个边角**~~（2026-10-16 已修，§5）：启动时一个服务器都没连上，通知用的发送端就会放掉。之后即使从 `/tools` 重连成功、再死掉，也不会有 down 的 notice。现在发送端跟 manager 同寿。
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

## 5. 2026-10-16：重连失败的终态、通知通道

分支 `tools-fixes`，基于 `a9d7857`。修的是 §3 的 2 与 4。

### 5.1 重连失败回到 down，不再变成没有工具的 failed

- **根因**：`reconnect` 一上来就 `detach`——session、工具、索引、segment、前缀一起拿掉，再去连；连不上就只剩 `merge_result(Failed)`，工具已经没了。
- **修法**：`detach` 拆成两步。先只取出并关闭旧 session；工具、索引、segment、前缀留到结果出来再说。成功才做剩下的 detach 再合并；失败而且之前有工具，就把状态写回 `Down { 旧 segment, reason: 这次失败的全文 }`，工具原样。进行中的状态是新增的 `ServerState::Reconnecting { segment }`（`segment()`/`wire_names()` 照常可读，所以 `/tools` 还把工具算在这个服务器下）。`call_tool` 先看状态再看 session：down 答 X-78 那句，重连中答 `it is being reconnected — try again in a moment`，不会掉成 `unknown tool`。
- **为什么是「回到 down」而不是「failed 但留着工具」**：down 的定义本来就是「连过、有工具、现在不能用、重连才能离开」，正好是这个情形；failed 在所有地方（一级、`segment()` 为空、调用答 `unknown tool`）都意味着「没有工具」，让它带着工具会出两种 failed。概要换成这次失败的原因（不是当初死掉的那句），因为用户接下来要修的是它；页面的提示行另有 `Reconnect failed: …`。
- **为什么先关旧 session 再连**（而不是连上新的再换掉旧的）：有的服务器独占锁文件或端口，两份同时起，新的那份会失败；对已经 down 的服务器旧 session 本来就死了，先关没有代价。

### 5.2 通知通道不再因为「一开始没人连上」而关掉

- **根因**：`connect_background` 的最后一个连接任务结束时，若没有一个服务器连上，就 `notices.take()`，理由是「没有东西会 down，别让报告任务空等一整个聊天」。但 X-79 加了重连之后，这个前提不成立了。
- **修法**：发送端跟 manager 同寿，不再提前放掉。代价是报告任务在这种情形下多停在一个空 channel 上——它本来就同时等着界面的 `done`，聊天结束就退出。

### 5.3 真机（`target/debug/iota`，私有 tmux，隔离 HOME，sh mock 服务器 + 本地假模型）

**重连失败**（`flaky` 被杀之后，下一次启动会故意失败；`ok` 是对照）：

```
（二级，按 Enter 之后）
 Reconnect failed: "sh" exited during startup: flaky: license server unreachable · Enter: try again · Esc: back
status        down: "sh" exited during startup: flaky: license server unreachable
…
tool names    mcp__flaky__<tool>
why
  connection closed: initialize response
  stderr:
    flaky: started pid=79844
    flaky: license server unreachable
tools (2)
  echo   says pong
  sleep  sleeps

（一级）
 2 MCP servers · 6 tools · Enter: open · Esc: close
  built in  2 tools  available
▸ flaky     2 tools  down: "sh" exited during startup: flaky: license server unreachable
  ok        2 tools  connected

（转录）
⚠ MCP flaky: exited (its last stderr line: "flaky: fatal: out of memory") — its tools will fail until it is reconnected from /tools · details in /tools
⚠ MCP flaky: "sh" exited during startup: flaky: license server unreachable · details in /tools
```

下一轮假模型收到的 MCP 工具：`["mcp__ok__echo", "mcp__ok__sleep", "mcp__flaky__echo", "mcp__flaky__sleep"]`。`a9d7857` 的二进制同样的步骤：一级 `flaky  —  failed: …`，页面没有 `tool names`，模型只收到 `ok` 的两个。之后再重连成功：`Reconnected · 2 tools`、`tool names mcp__flaky__<tool>`（segment 没变成 `flaky_2`）、转录 `MCP flaky reconnected · 2 tools`。

**启动全失败，重连成功后再死**（配置里只有 `flaky`，启动时故意失败）：

```
（启动）
⚠ MCP flaky: "sh" exited during startup: flaky: license server unreachable · details in /tools

（一级）
▸ built in  2 tools  available
  flaky     —        failed: "sh" exited during startup: flaky: license server unreachable

（二级，Enter 重连成功，然后杀掉）
 Reconnected · 2 tools · Enter: reconnect · Esc: back
status        down: exited (its last stderr line: "flaky: fatal: out of memory") — its tools will fail until it is reconnected from /tools
…
why
  stderr:
    flaky: started pid=86854
    flaky: fatal: out of memory

（一级）
▸ flaky     2 tools  down: exited (its last stderr line: "flaky: fatal: out of memory") — its tools will fail until it is reconnected f…

（转录）
⚠ MCP flaky: "sh" exited during startup: flaky: license server unreachable · details in /tools
MCP flaky reconnected · 2 tools
⚠ MCP flaky: exited (its last stderr line: "flaky: fatal: out of memory") — its tools will fail until it is reconnected from /tools · details in /tools
```

`a9d7857` 的二进制：页面与一级完全相同，转录里**没有**最后那行 `⚠`。

### 5.4 测试

- `tests/mcp/manager.rs`：`a_reconnect_that_fails_leaves_the_server_down_with_its_tools`（down → 拒绝 → down 带工具 → 成功、同一 segment、调用到新进程 → connected 时重连被拒 → down 带工具；从没连上的那个仍是 failed）、`a_server_that_dies_after_a_startup_with_none_connected_is_announced`、`a_server_reconnecting_keeps_its_tools_and_says_so`。服务器由测试用文件操控（`refuse`/`slow`/`die`）。
- `mcp::manager::tests::connect_background_forwards_every_status_and_stays_open_while_the_manager_lives`（原来断言「全失败后 channel 关闭」，那正是缺口本身；改为「manager 在就开着，manager 没了就关」）。
- `repl::commands::tools::tests::a_server_reconnecting_keeps_its_count_and_says_so`；`tests/repl/commands.rs::tools_reconnects_a_server_from_its_page` 补断言：被拒的 `flaky` 仍有 `mcp__flaky__echo`。
- 红绿：把 `src/mcp/{manager,explain}.rs`、`src/repl/commands/tools.rs` 换回 `a9d7857` 跑上面的集成测试，四条全红（`Failed` 且没有工具；channel 收到 `None`；没有 `reconnecting` 状态；`flaky` 的 wire 名为空）。

### 5.5 没改的

- 二级的提示行记的是「这次打开 `/tools` 以来最后一次重连的结果」：重连成功后服务器又死了，提示行仍是 `Reconnected · 2 tools`，状态行已经是 `down`（上面第二段抓屏）。两行不矛盾但读着别扭；要改就让提示行在状态变了以后回到 `Enter: reconnect`。
