# Composer 换行 —— 方案评审

> 2026-10-02，分支 `composer-newline` @ `296df94`。评审对象：PM 的「两步走」方案（第一步 `Ctrl+J`(+`Alt+Enter`) 加两处收尾；
> 第二步 Kitty `DISAMBIGUATE_ESCAPE_CODES`）。材料：`composer-newline-research.md`（下称「调研」）、`composer-newline-recon.md`（下称「勘察」）。
> 只读评审：没有改代码、没有提交。行号以 `296df94` 为准（与勘察的 `5317eb4` 相同，中间只有文档提交）。
> 标「推测」的是没核对过的判断；其余结论都附了 `文件:行号`、报告章节或本文附录里的实测原文。

## 结论（TL;DR）

1. **诊断成立，已在真 Ghostty 1.3.1 里实测**：Shift+Enter 发的是 `ESC[27;2;13~`，crossterm 0.29 整条丢掉，iota 今天**既不换行也不提交**；
   Ctrl+Enter 同样被吞。`keys/tests.rs:442` 的注释（「modifiers are ignored」）在 Unix 上描述的是一个真终端不会产生的事件。（附录 A.1、A.2）
2. **第一步方向对，但范围该小改两处**：
   - **`Enter + SHIFT` 也在第一步就绑成换行**。这一条跟协议是两回事：绑定只是 `keys.rs` 里的一个条件，**不入栈就不会多出任何风险**。只要终端自己上报
     SHIFT 就能用：tmux 设了 `extended-keys always` + `csi-u`（调研 A.3 实测）、用户自己把 Shift+Enter 映射成 CSI u、Windows 控制台 API（推测）。
     终端不上报的地方，这个分支永远不会触发，行为和今天一样。
   - **「提示文案自适应」先不做**：今天的帧里没有空闲时的提示位（只有忙碌时的 `ESC to cancel`，见 `frame.rs:156,289`，以及队列的 `↑ edit`，见 `frame.rs:49`）。
     不做第二步，就没有东西需要「自适应」。第一步的可发现性靠 CHANGELOG 和文档。
3. **第二步现在不该做**。收益只落在 Ghostty 一个目标终端上：tmux、Terminal.app、Windows 都拿不到（调研 §3.2）。而且 Ghostty 用户今天在终端配置里加一行，
   配合第一步就能用 Shift+Enter（§4.2）。风险已经实测出来了：**进程被 SIGKILL 之后，Kitty 标志会留在终端里，后面的 shell 收到的 Esc 是 `ESC[27u`、
   Shift+Enter 是 `ESC[13;2u`**（附录 A.3）。iota 是 `panic = "abort"`（`Cargo.toml:170`），panic 时 `Drop` 根本不跑；项目文档写的恢复办法 `stty sane`
   （`handle.rs:52-54`）**清不掉**这个状态。PM 方案里「panic 时出栈」靠现有的 `TermGuard` 做不到。至于「Ctrl+Z 挂起恢复」，iota 根本没有这条路径。
4. **最严重的发现**：第二步的「退出必须出栈」在现有架构下兑现不了（abort 不跑 Drop、SIGKILL 无解、文档里的恢复命令无效）。所以第二步要等有人报问题再做，
   做的时候先补 panic hook 和恢复文档。

---

## 1. 诊断对不对

**结论：成立。** 今天在 Ghostty 1.3.1（默认配置）里按 Shift+Enter，iota 收不到任何事件。Ctrl+Enter 也一样。

**依据（实测，附录 A.1/A.2）**：

- 用 Ghostty 1.3 自带的 AppleScript `send key … modifiers "shift" to <terminal>` 给 crossterm 0.29 探针发键。探针跑在 raw mode，**不请求任何协议**：
  `shift+enter` 和 `ctrl+enter` 之后只有哨兵 `x`，没有按键事件；`enter` → `Enter`，`opt+enter` → `Enter + ALT`，`ctrl+j` → `Char('j') + CONTROL`。
- 同一窗口换成原始字节记录器：Shift+Enter 是 `b'\x1b[27;2;13~'`，Ctrl+J 是 `b'\n'`。这和调研 §3.2 引的 Ghostty 源码一致。
- 在 tmux 里用 `send-keys -H 1b 5b 32 37 3b 32 3b 31 33 7e` 直接给探针喂这串字节：同样只有哨兵，没有事件（附录 A.4，第一组）。

**失败场景**：Ghostty 用户按 Shift+Enter 想换行，结果什么也没发生，看起来像「按了没反应」。这不会误发消息，所以眼下是**无害的死键**，不是事故。

**对第二步收益的含义**：第二步能把首要目标终端里的一个死键变成换行。这是真收益，但不紧急：今天这个键不会造成误提交，第一步之后 Ctrl+J 和 Alt+Enter 在 Ghostty 里都能用
（`opt+enter` 默认就是 `Enter + ALT`，附录 A.1）。

**建议**：第一步就把 `keys/tests.rs:440-443` 的文档注释改写成事实：Unix 上 Enter 带 SHIFT/CONTROL 只会从 CSI u 来；Ghostty 默认的 Shift+Enter 在
crossterm 里就被丢了，根本到不了这里。

**实测的局限**：AppleScript 的 `send key` 走的是 Ghostty 自己的键编码器，但不经过 macOS 的 NSEvent 和输入法层。所以 Option 的平台翻译、输入法这两块不在这次实测的覆盖范围里。

## 2. 第一步够不够

### 2.1 `Ctrl+J` 是唯一在四个目标终端默认都成立的组合吗

**结论：是**（这里只算组合键，不算行尾 `\`）。依据是调研 §4 的表和 §4.1；Ghostty 部分本文实测过（附录 A.1），tmux 部分勘察 §2 实测过；
Terminal.app 和 Windows Terminal 部分来自官方表述和社区报告，**没有实测**。

**失败场景**：

- tmux 用户装了 vim-tmux-navigator，`C-j` 被 tmux 的 root 表截走，iota 收不到（调研 §4.1）。这正是 Alt+Enter 要补的洞。
- VS Code 集成终端在 Win/Linux 上 Ctrl+J 默认是切换面板（调研 §2.2 引的 codex#2358，社区报告）。它不在目标终端里，写进文档就够了。

### 2.2 `Alt+Enter` 顺手接受有没有坑

**结论：值得接受，坑都小。改测试是必须的。**

| 情形 | 结果 | 依据 |
|---|---|---|
| Ghostty 默认 | `Enter + ALT` ✓ | 附录 A.1 实测（`opt+enter`） |
| Terminal.app 默认 | Option+Enter 就是 Enter，**照常提交**，跟今天一样，不算回归 | 调研 §4.2（官方） |
| Terminal.app 开了 Option-as-Meta | ✓ | 同上 |
| tmux 默认 | 透传 `ESC \r` → `Enter + ALT` ✓ | 勘察 §2 实测 |
| Windows Terminal | 被全屏快捷键截走，到不了 iota | 调研 §3.2（官方 defaults.json） |
| Kitty 协议下（如果以后做第二步） | `CSI 13;3u` → `Enter + ALT` ✓ | 附录 A.2 实测 |

- **ESC + CR 粘连**：先按 Esc、紧接着按 Enter，如果两个字节落在同一次 read 里，crossterm 会解成 `Enter + ALT`（附录 A.4 的 `ESC CR` 一组）。
  今天这种情况是「Esc 丢了 + 提交」，接受 Alt+Enter 以后变成「Esc 丢了 + 换行」。后者更安全（不会误发），不是新问题。慢速 SSH 上更容易粘连（推测）。
- **测试必须改**：`keys/tests.rs:462-474` 的循环断言 SHIFT、ALT、CONTROL 三种都「must still submit」（`:472`）。按本文建议，SHIFT 和 ALT 改成「插入换行、不提交」，
  CONTROL 保持提交。**不要只是把 ALT 从数组里删掉**，那样等于把覆盖一起删了。应该改成两条显式断言：换行组断言 value 含 `\n` 并且队列为空；提交组保持原样。

### 2.3 补一条：`Enter + SHIFT` 也在第一步绑上

**结论：建议加，零风险。** PM 方案把「Shift+Enter 绑定」和「Kitty 入栈」放在一起，其实可以拆开：

- 绑定只是 `keys.rs` 换行分支里的一个条件，跟入栈无关。终端不上报 SHIFT（Terminal.app，以及 tmux 的默认配置）时，Shift+Enter 到达时就是一个普通的 `Enter`，
  这个分支不会触发，**行为和今天完全一样**。
- 终端**自己**上报 SHIFT 的场景，第一步就能用上：
  - tmux 设 `extended-keys always` + `extended-keys-format csi-u`：不需要应用请求，Shift+Enter 就到达为 `Enter + SHIFT`（调研附录 A.3 实测最后一行）；
  - 用户在终端里把 Shift+Enter 映射成 `ESC[13;2u`（opencode 文档给 Windows Terminal 写的就是这一招，见调研 §2.3）；
  - Windows 控制台 API 路径，crossterm 从 `control_key_state` 里取到 SHIFT（调研 §3.2，**推测**，没有实测）。
- 把绑定和入栈拆开以后，第二步就**只剩入栈**这一件事，以后要做的话评审面也更小。

### 2.4 `Ctrl+J` 的可发现性

**结论：写 CHANGELOG（`### Added`）和用户文档。第一步不在帧里加提示位。**

- readline 里 Ctrl+J 是 accept-line。但 iota 里它今天是空操作：`Editor::on_key` 遇到 `(true, Char('j'))` 会落到 `_ => return false`（`editor.rs:120`）。
  所以不存在「iota 用户习惯 Ctrl+J 提交」，只有 shell 里的肌肉记忆。误按的后果是多出一个看得见、可以退格删掉的换行，不会造成损失。
  CHANGELOG 写成 Added 就行，不必写成行为变更。
- **提示放在哪**：帧里今天没有空闲时的提示位，只有忙碌时的 `ESC to cancel`（`frame.rs:156`、`:289`）和队列的 `↑ edit`（`frame.rs:49`）。
  新加一个常驻提示位是一个 UI 决策，不是换行功能附带的东西。PM 的「提示自适应」正好建立在这个不存在的位置上。建议第一步只做 CHANGELOG 和文档
  （文档按调研 §6.5 的写法：「Shift+Enter submits / does nothing」开头，三行讲清 Ctrl+J、Alt+Enter，以及终端侧的映射配方，见 §4.2）。
  要不要加常驻提示，由 owner 单独拍板。

### 2.5 两处收尾

**(a) 多行草稿长行折行后，输入框高度不跟着长**（勘察 §4 的「已知缺口」）

- 原因：`effective_height` 在多逻辑行时用 `self.height`（`composer.rs:137-143`）。`height` 在每次编辑后被设成逻辑行数（`:291`、`:99`、`:117`），
  fold-back 时由 `event_loop.rs:691` 设置。
- **建议的修法是删代码，不是加代码**：让 `effective_height` 一律返回 `total_rows.clamp(1, MAX_ROWS)`。按折行切出来的行数一定不少于逻辑行数，
  显式的 `height` 也就再没有存在的理由，`height` 字段、`set_height` 和 `event_loop.rs:690-691` 那两行都可以删掉。
  删之前要确认 `composer.rs` 的 WP46 测试没有钉住「多行时取逻辑行数」。这是相对 Go 的一处偏离（推测：Go 用的 textarea 高度是 LineCount），
  按项目惯例应该在 `docs/DIVERGENCES.md` 加一行。
- 失败场景（现状）：输入 `aaa…(200 列)`，按 Ctrl+J，再输入 `b` → 框只有 2 行高，第一行的折行部分要靠滚动才能看到。

**(b) 队列行被 `\n` 撑开**

- `frame.rs:172-190` 把排队项原样交给 `truncate_ansi`，后者不处理 `'\n'`（`ansi.rs:122` 起）。这条是推测，勘察里也标的是推测，本文没有在终端上复现。
  raw mode 关掉了 OPOST，`\n` 只下移一行、不回车，会把后半截画到下一行的同一列上，盖住下面的分隔线或 composer。**测试要证明它，而不只是修它**（见 §5）。
- 建议：显示时把 `'\n'` 替换成 `" ⏎ "`（或者只显示首行，后面加 `…`），只在 `frame.rs` 渲染这一处改。`queue_rows()`（`event_loop.rs` 里的 `Queued::row`）
  照样返回原文：↑ 弹回和 fold-back 用的都是原文。

### 2.6 第一步还要注意的两处

- **分支的位置和收尾**：放在 Row 7（`keys.rs:89-93`）前面是对的。它在 Row 4 之后，所以会先结束补全循环（`keys.rs:58-59`）；
  Row 5、6 只认 ↑↓，Ctrl+J 不会被它们截走。但 `Composer::insert_str`（`composer.rs:115-118`）**不会**结束历史导航，分支里要显式调用一次
  `end_history_nav()`（勘察 §7 写了，提醒别漏）。
- **多行历史条目会把 ↑ 困住**：↑ 召回一条多行历史后，再按 ↑ 只是在行间移动，不会继续往前翻（`history_navigable` 要求单行，`composer.rs:235-237`）。
  这是现有行为：折行的长单行今天就这样（`keys/tests.rs` 里「a wrapping line keeps ↑ for row movement」那条就钉的是它），和 Go 一致。
  有了换行以后这种情况会更常见。**第一步不改**，记作已知局限即可。

## 3. 第二步值不值

**结论：现在不做。等到有用户明确要求 Ghostty 原生 Shift+Enter，而且「终端配置一行」的方案不被接受时，再按下面的边界做。**

### 3.1 为什么现在不做（按项目规则：最简实现，不拿能用的产品换未完成的复杂度）

| | 收益 | 依据 |
|---|---|---|
| Ghostty | 死键变成换行 | §1 |
| Terminal.app | 无（没有协议） | 调研 §3.2 |
| tmux | **无**（tmux 不认 Kitty 入栈，`extended-keys on` 也没用） | 调研 §3.3、附录 A.3 |
| Windows | 无（`supports_keyboard_enhancement()` 在 Windows 上恒为 `Ok(false)`） | 调研 §3.2 |

成本（以下三条都是本文实测或代码核对）：

1. **异常退出会留下标志**。附录 A.3：探针入栈后被 `kill -9`，同一个窗口里接着跑的程序收到的 Esc 是 `b'\x1b[27u'`、Shift+Enter 是 `b'\x1b[13;2u'`。
   - iota 是 `panic = "abort"`（`Cargo.toml:170`），panic 不展开，`TermGuard::drop`（`handle.rs:64-75`）不会执行；
   - 项目写下的恢复办法是 `stty sane`（`handle.rs:52-54`）。它只重置 tty 驱动，**清不掉终端里的键盘协议栈**（推测：要 `printf '\e[<u'` 或 `reset`）；
   - 用户的 shell 如果不认 CSI u（bash/readline、zsh，推测），Esc 和 Ctrl+字母就会变成乱码。Ctrl+C 能不能发出 SIGINT，取决于终端在 Kitty 模式下
     怎么编码 Ctrl+C（规范里是 `CSI 99;5u`，不再是 `0x03`）。这一条推测**没有在真按键下证实**（见 3.3）。
2. **所有 Ctrl/Alt+字母和 Esc 的编码都会变**。crossterm 的解码大体等价（附录 A.4：`CSI 99;5u` → `Char('c')+CONTROL`、`CSI 27u` → `Esc`、
   `CSI 98;3u` → `Char('b')+ALT`、`CSI 106;5u` → `Char('j')+CONTROL`、`CSI 57414u`（小键盘 Enter）→ `Enter`）。
   但这只证明解码器没问题，证明不了真终端在 Kitty 模式下对每个键发的是什么。附录 A.2 里，AppleScript 合成的 Ctrl/Alt+字母在 Kitty 模式下**一个字节都没发出来**
   （推测是合成事件缺少键盘布局信息，属于测试工具的局限）。所以这部分只能靠人工在 TUI-VERIFY 里按一遍。
3. **测试钉的形状和 Kitty 下的真实形状对不上**：`keys/tests.rs:218-233` 用 `Char('C') + CONTROL|SHIFT` 证明 Ctrl+Shift+C 不中断。
   但 crossterm 解 `CSI 99;6u` 得到的是**小写** `Char('c') + SHIFT|CONTROL`（附录 A.4），Row 2 的 `ctrl && Char('c'|'d')`（`keys.rs:35`）会匹配，于是中断。
   这不算回归：Unix 上的传统编码里 Ctrl+Shift+C 本来就是 `0x03`，今天也会中断。但那条测试给人的安全感是假的。做第二步时要么改测试的形状，要么接受并写明。
4. **对手方案已经够用**：§4.2 的终端侧映射，加上第一步，Ghostty 用户写一行配置就有 Shift+Enter，而且在 tmux 里也成立（LF 能穿过 tmux）。

### 3.2 如果做，最小安全边界

- **探测**：`crossterm::terminal::supports_keyboard_enhancement()`，放在 `handle.rs:168` 的 `cursor::position()` 旁边，也就是 loop 线程起来之前
  （W8 只允许一个 crossterm 读者），`Err` 一律当 `false`。注意它最坏要等 2 秒（crossterm `terminal/sys/unix.rs` 的 `query_keyboard_enhancement_flags_raw`
  用的是 `Duration::from_millis(2000)`），只有不回 DA1 的终端才会碰到。`osc.rs:89` 起有一个 100ms、用 DSR 收尾的查询可以借用，但那要自己写解析，
  默认不建议走这条路。**不要不探测就入栈**：在 Windows 上 `PushKeyboardEnhancementFlags` 走 winapi 路径会返回 `Err(Unsupported)`（crossterm `event.rs:500-507`），
  如果照抄 `EnableBracketedPaste` 那行的 `?`（`handle.rs:165`），Windows 上就**启动失败**。oneshot（`oneshot.rs:26-39`）没有 composer，不入栈。
- **标志**：只开 `DISAMBIGUATE_ESCAPE_CODES`。不开 `REPORT_EVENT_TYPES`（键梯本来就只认 Press，见 `keys.rs:20-22`；Codex 的注释说明了它在 Ghostty、iTerm2 上会泄漏释放事件，见调研 §2.2）。
- **出栈**：在 `TermGuard` 里记一个「入栈了没有」的 bool，`Drop` 时**先** `PopKeyboardEnhancementFlags`，再关 bracketed paste、关 raw mode（`handle.rs:66-74`）。
  SIGINT、SIGTERM 走 `signals::install`（`main.rs:39`）优雅退出，会经过 Drop。
- **panic**：`abort` 下只有 panic hook 还会执行。加一个最小的 hook，只写 `ESC[<u`。或者明确接受这个风险，并把 `handle.rs:52-54` 的恢复说明改成 `reset`。二选一，写进 DIVERGENCES。
- **SIGKILL 和 OOM**：无解，只能写进文档。
- **Ctrl+Z**：iota 没有挂起路径。raw mode 关了 ISIG，`ctrl('z')` 被测试钉成空操作（`keys/tests.rs:233`），源码里也没有 SIGTSTP/SIGCONT 处理（已 grep）。
  **不要为了这一步新加挂起支持**。外部发来的 `kill -TSTP` 今天就会让 raw mode 留在开启状态，这个问题跟本方案无关。
- **逃生开关**：一个环境变量，关掉入栈（参照 Codex 的 `CODEX_TUI_DISABLE_KEYBOARD_ENHANCEMENT`，调研 §2.2）。只是一行判断，出问题时用户能自救。
- **TUI-VERIFY 新增一节**（Ghostty 必做，iTerm2 可选）：Shift+Enter 换行、Ctrl+J 换行、Alt+Enter 换行、Ctrl+Enter 提交；流式输出中按 Esc 取消；
  Ctrl+C 中断、连按两次退出；Ctrl+A/E/K/U/W/B/F；Alt+b；Shift+字母大写（Claude Code 2.1.98 修过的回归）；Ctrl+Shift+C；小键盘 Enter；
  **§1 的中文输入法组字**（入栈状态下重做一遍）；正常 `/exit` 和连按两次 Ctrl+C 退出之后，在 shell 里跑 `cat -v` 按 Esc，应该看到 `^[`，而不是 `^[[27u`；
  `kill -9` 之后用文档里的恢复命令能恢复。
- **tmux 端到端**：用 `pipe_raw` 抓字节（照 `14-osc-signals.sh` 的做法），断言在 tmux 里 iota **不会**写出 `ESC[>1u`（tmux 不回探测），启动也没有变慢。
  用 `key -H 1b 5b 31 33 3b 32 75` 注入，证明解码和路由。这条测不到入栈、出栈本身，那部分只能靠 Ghostty 人工验证，
  或者用 Ghostty 的 AppleScript `send key` 半自动验证（附录 A 的做法，但它覆盖不到 Ctrl/Alt+字母）。

## 4. 有没有更好的方案

### 4.1 PM 列的候选

| 候选 | 结论 | 理由 |
|---|---|---|
| 行尾 `\` + Enter | **现在不做**，留作有人反馈时再加 | 实现只要几行，但会**悄悄改掉用户的内容**：想提交一条以 `\` 结尾的消息（Windows 路径、shell 续行）就会被拦成换行（调研 §4.4）。它唯一不可替代的场景，是「Ctrl+J 被截走 + 没开 Option-as-Meta 的 Terminal.app，或者 Windows Terminal」这个交集。第一步加上 Alt+Enter 和 §4.2 以后，这个交集已经很窄。 |
| `Ctrl+V Ctrl+J` | 不做 | raw mode 下 Ctrl+V 只是 `0x16`，Ctrl+J 本身已经能区分，前面加 Ctrl+V 不增加任何信息（调研 §4.5）。 |
| 原生修饰键检测（Claude Code 在 Apple Terminal 上的做法） | 不做 | 要引入平台原生依赖，SSH 下必然失效，可靠性连 Claude Code 自己的用户都有异议（调研 §2.1、claude-code#62322）。 |

### 4.2 第五种：终端侧映射，iota 只写文档，一行代码不加

**结论：这是 Ghostty 等终端用户拿到 Shift+Enter 最便宜的路，应该进第一步的文档。**

- 原理：让终端在 Shift+Enter 时直接发 `\n`（等于 Ctrl+J）或 `ESC[13;2u`。前者配合第一步的 Ctrl+J 分支，后者配合 §2.3 的 SHIFT 分支。iota 不入栈、不探测。
- 发 `\n` 的版本**能穿过 tmux**（tmux 十种配置里有八种原样透传 `\n`，见调研 §3.3），这是第二步在 tmux 里做不到的。
- 配方（推测，具体语法以各终端文档为准，写进文档前每条要实测一次）：Ghostty `keybind = shift+enter=text:\n`；iTerm2 的 Profiles › Keys 里加映射，Send Hex Codes `0x0a`；
  WezTerm 用 `SendString "\n"`；Windows Terminal 用 `sendInput` 动作。
- 局限：要用户自己动手配，Terminal.app 做不到（调研 §3.2）。

### 4.3 有意不提的

用 `$EDITOR` 编辑长消息（aider 的 `/editor`）是更大的功能，跟「插入换行」不是一回事，不在本次范围里。

## 5. 验证方案

### 5.1 单测（`src/ui/input/keys/tests.rs`）

每条都要断言「最后交出去的是什么」，而不只是 composer 的状态：

1. `type a` → Ctrl+J → `type b` → Enter，此时有一个已挂起的 reader：断言 `reader.try_recv()` 拿到 `got.text == "a\nb"`。这一步走过了 `submit` 的 trim 和
   `paste::make_input`（`event_loop.rs:625-638`），**这才是「提交出去的文本确实带两行」**。
2. 换行插在光标处，不是插在末尾：`abc`，←←，Ctrl+J → `"a\nbc"`。
3. Ctrl+J 不提交：队列为空、reader 没收到东西，`composer.rows(80).len() == 2`。
4. 首尾的换行会被 trim：Ctrl+J、`hi`、Ctrl+J、Enter → `"hi"`；只有换行的草稿什么也不提交（`is_blank`，见 `composer.rs:121`）。
5. 换行和粘贴标签共存：先粘贴一个多行块拿到 `[#1 …]` 标签，Ctrl+J，`x`，Enter → `text` 是展开后的块加上 `\nx`。
6. **surface 打开时 Ctrl+J 不插入**：`Field` 的值不变。这一条把「不放进 `Editor::on_key`」这个决定钉住。
7. Alt+Enter、Shift+Enter（如果按 §2.3 绑上）各跑一遍 1 和 3；Ctrl+Enter 仍然提交；改写 `:462-474`（见 §2.2）。
8. Ctrl+J 结束历史导航和补全循环：先 ↑ 召回一条，Ctrl+J，再 ↑，这时是行间移动而不是翻到更早的条目。
9. 高度（`composer.rs` 的测试）：一行 200 列加一个 `\n` 再加 `b`，宽度 80 → `rows(80).len() == 4`（3 行折行 + 1 行）。
10. 队列行：用 vt100 测试（`vt100_tests.rs`，`:2079` 一带有现成的写法）入队一个 `"a\nb"`，断言渲染后的网格里帧的行数等于预期、分隔线完整。
    **只断言 `queue_rows()` 是假绿**：它按设计就是返回原文。

### 5.2 tmux 端到端（新建一个场景，或者在 `07-paste.sh` 里加一段）

```sh
type_ 'line one'; key C-j; type_ 'line two'
# 没提交：历史没长；composer 两行，第二行是续行前缀，不是新的 ❯
check "Ctrl+J did not submit" "$(hist_size)" "$before_hist"
check "two composer rows" "$(composer_block | wc -l | tr -d ' ')" 2
check "continuation row" "$(count_composer '  line two')" 1
key Enter
wait_all 'echo: line one' || bad "never reached the model"
check_once "echo row 1" '❯ line one'
check_once "echo row 2 is a continuation" '  line two'      # 不是 '❯ line two'
check "the model saw line two" "$(count_all 'line two')" 2  # 用户回显 + mock 回显，跟 07 号的「model saw line 3」同一个手法
```

再用 `key M-Enter`（等价于 `key -H 1b 0d`）把同一段跑一遍。`key -H 0a` 应该和 `key C-j` 结果相同。

**`send-keys -H` 的用法**：每个参数是一个十六进制字节，空格分开，不带 `0x`。`lib.sh:51` 的 `key()` 原样透传参数，
所以写 `key -H 1b 5b 31 33 3b 32 75` 就是注入 `ESC[13;2u`。`14-osc-signals.sh` 已经在用 `key -H 1b 5b 4f`。

队列路径也要跑一遍：在一个慢的 mock turn 进行中输入两行再按 Enter，用 `check_frame_intact`（`lib.sh:437`）断言帧没被撑坏。

### 5.3 假绿清单

| 假绿 | 为什么假 | 怎么改 |
|---|---|---|
| 只测 `keys::update_key` 之后 `composer.value()` 里有 `\n` | 没走 `submit` 的 trim 和 `make_input` | 用挂起的 reader 断言 `got.text`（§5.1 第 1 条） |
| 单测构造 `KeyEvent(Char('j'), CONTROL)` | 没覆盖「raw mode 下 `0x0A` 才会解成 Ctrl+J」（勘察 §2：非 raw mode 下它是 Enter） | tmux 里的 `key C-j` 或 `key -H 0a` |
| tmux 里只断言回显出现了 `line two` | 如果 Ctrl+J 被当成提交，`line one` 和 `line two` 会作为两条消息出现，同样能匹配 | 断言 `hist_size` 没变，并且第二行用续行前缀 `'  line two'` |
| `count_all 'line two'` 在 Enter 之前取 | composer 自己那一行也会匹配 | 在 `wait_all 'echo: …'` 之后再数 |
| tmux 里 `key S-Enter` | 默认配置下它就是 Enter（勘察 §2），测的是「提交」，不是换行 | 第二步的路由只能用 `-H` 注入 CSI u 来测，并且写明测不到入栈 |
| 用 `-H` 注入 CSI u 证明「Shift+Enter 能用」 | 只证明了解码和路由，证明不了真终端会发这串字节，也证明不了入栈、出栈 | 加 Ghostty 的人工 TUI-VERIFY 项 |
| `queue_rows()` 等于 `["a\nb"]` | 它按设计就返回原文 | 用 vt100 网格断言帧的形状 |
| 把 `:462-474` 数组里的 ALT 删掉 | 测试照样绿，覆盖却没了 | 改成换行组和提交组两条显式断言 |

## 6. 总判断

**按两步走，但重新切分：第一步稍微扩大（加 SHIFT 绑定和终端配方文档），第二步无限期推迟，等有人反馈再做。**

**第一步的确切范围（一个 PR）**：

1. `keys.rs`：Row 7 前面加一个分支。`Char('j') + CONTROL`、`Enter + ALT`、`Enter + SHIFT` 都执行 `insert_str("\n")` 加 `end_history_nav()`，然后 return。
   `Enter + CONTROL` 和裸 Enter 仍然提交。更新模块注释里的键梯表（`keys.rs:1-4`）。
2. `composer.rs`：高度一律按折行后的行数算，删掉 `height` 和 `set_height`（以及 `event_loop.rs:690-691`），在 DIVERGENCES 记一行。
3. `frame.rs`：队列行显示前把 `\n` 替换掉。
4. 测试：§5.1 的 1–10，改写 `keys/tests.rs:440-474`（注释和断言），tmux 场景按 §5.2。
5. CHANGELOG `### Added` 一条；用户文档一小节：Ctrl+J 到处可用，Alt+Enter 视终端而定，Shift+Enter 靠终端侧映射（§4.2 的配方，要实测过）。
6. **不做**：帧内提示和「自适应」、行尾 `\`、任何协议入栈。

**第二步**：现在不做。理由见 §3.1：收益只覆盖一个终端，§4.2 已经给了零代码的替代；成本是已经实测到的退出残留，加上 abort 下兑现不了的出栈承诺。
触发条件：有 Ghostty 或 kitty 用户明确拒绝配置方案。届时按 §3.2 的边界做，先补 panic hook（或者改恢复文档），再补 TUI-VERIFY 那一节，然后才入栈。

---

## 附录 A：实测方法与原始输出

环境：macOS Darwin 25.6.0，Ghostty 1.3.1（用户配置文件为空），tmux 3.7c，rustc 1.98.0，crossterm `=0.29.0`。探针在 scratchpad 里，不在仓库里：
`enable_raw_mode` → 可选的 `PushKeyboardEnhancementFlags(DISAMBIGUATE_ESCAPE_CODES)` → 循环 `event::read()`，把每个 `KeyEvent` 写进日志，收到 `q` 就退出（入栈的话先出栈）。
字节记录器是一个 Python `tty.setraw` 脚本，把 `os.read` 的结果 `repr` 后写进日志。

Ghostty 的键是用它 1.3 版的 AppleScript 字典发的（`/Applications/Ghostty.app/Contents/Resources/Ghostty.sdef`）：
`new window with configuration`（command = 探针），`send key "<k>" modifiers "<m>" to <terminal>`，再加一个 release。
哨兵 `x` 用 `input text "x"`，因为 `send key "x"` 合成的事件不带文本，什么字节也不产生。这种方式不抢焦点，键只会送到指定的 terminal。

### A.1 Ghostty，不请求任何协议（探针）

```
start kitty=false supports=Ok(true) TERM_PROGRAM=Ok("ghostty")
-- enter
Enter KeyModifiers(0x0) Press
Char('x') KeyModifiers(0x0) Press
-- shift+enter
Char('x') KeyModifiers(0x0) Press          ← 没有按键事件
-- ctrl+enter
Char('x') KeyModifiers(0x0) Press          ← 没有按键事件
-- opt+enter
Enter KeyModifiers(ALT) Press
Char('x') KeyModifiers(0x0) Press
-- ctrl+j
Char('j') KeyModifiers(CONTROL) Press
Char('x') KeyModifiers(0x0) Press
```

同一个窗口里，探针按 `q` 出栈退出以后，再跑字节记录器（这就是「不请求协议时」的原始字节）：

```
-- AFTER-POP shift+enter
b'\x1b[27;2;13~'
-- AFTER-POP ctrl+j
b'\n'
-- AFTER-POP ctrl+c
b'\x03'
```

### A.2 Ghostty，入栈 `DISAMBIGUATE_ESCAPE_CODES`

探针：

```
start kitty=true supports=Ok(true) TERM_PROGRAM=Ok("ghostty")
-- K enter          → Enter KeyModifiers(0x0)
-- K shift+enter    → Enter KeyModifiers(SHIFT)
-- K ctrl+enter     → Enter KeyModifiers(CONTROL)
-- K opt+enter      → Enter KeyModifiers(ALT)
-- K esc            → Esc KeyModifiers(0x0)
-- K ctrl+j         → （只有哨兵）
-- K ctrl+c         → （只有哨兵）
-- K ctrl+shift+c   → （只有哨兵）
-- K ctrl+z         → （只有哨兵）
```

用 `printf '\e[>1u'` 入栈后跑字节记录器：

```
-- raw-kitty shift+enter   b'\x1b[13;2u'
-- raw-kitty esc           b'\x1b[27u'
-- raw-kitty ctrl+j        （无字节）
-- raw-kitty ctrl+c        （无字节）
-- raw-kitty alt+b         （无字节）
```

**解读**：Kitty 模式下，合成的 Ctrl/Alt+字母**终端那一侧就没发出字节**，并不是 crossterm 丢了。推测原因是 AppleScript 合成的事件不带键盘布局或码点信息，
而 Kitty 编码需要这些信息。Ghostty 里的 Claude Code、Codex 都开着 Kitty 协议，Ctrl+C 照常能用，这是旁证。所以这一栏不能当作证据，必须真人按键（§3.2 的 TUI-VERIFY）。

### A.3 入栈后被 SIGKILL：标志留在终端里

窗口里跑 `sh -c '<probe> <log> kitty; echo PROBE-EXITED >> <log>; python3 bytes.py <log>'`，从外面 `kill -9` 探针本身（不杀外层的 sh），然后往同一个 terminal 发键：

```
start kitty=true supports=Ok(true) TERM_PROGRAM=Ok("ghostty")
PROBE-EXITED
bytes-logger start
-- after-kill shift+enter
b'\x1b[13;2u'          ← 正常应为 b'\x1b[27;2;13~'（对照 A.1）
-- after-kill esc
b'\x1b[27u'            ← 正常应为 b'\x1b'
```

### A.4 tmux 3.7c 里给探针喂原始字节（`send-keys -H`，每组后跟哨兵 `x`）

```
start kitty=false supports=Ok(false) TERM_PROGRAM=Ok("tmux")
-- CSI 27;2;13~ shift+enter ghostty-legacy   → （只有哨兵）
-- CSI 13;2u                                  → Enter KeyModifiers(SHIFT)
-- LF                                         → Char('j') KeyModifiers(CONTROL)
-- ESC CR                                     → Enter KeyModifiers(ALT)
-- CSI 99;5u ctrl+c kitty                     → Char('c') KeyModifiers(CONTROL)
-- CSI 99;6u ctrl+shift+c kitty               → Char('c') KeyModifiers(SHIFT | CONTROL)   ← 小写 c
-- CSI 100;5u ctrl+d kitty                    → Char('d') KeyModifiers(CONTROL)
-- CSI 27u esc kitty                          → Esc KeyModifiers(0x0)
-- CSI 106;5u ctrl+j kitty                    → Char('j') KeyModifiers(CONTROL)
-- CSI 57414u KP_Enter kitty                  → Enter KeyModifiers(0x0)
-- CSI 98;3u alt+b kitty                      → Char('b') KeyModifiers(ALT)
-- CSI 122;5u ctrl+z kitty                    → Char('z') KeyModifiers(CONTROL)
-- 0x1a ctrl+z legacy                         → Char('z') KeyModifiers(CONTROL)
```

### A.5 基线

`cargo test --lib ui::input`：41 passed，0 failed（`296df94`，评审时跑的）。
