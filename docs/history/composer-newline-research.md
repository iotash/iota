# 输入框换行键调研：主流 coding agent 用什么键换行，终端怎样才能区分它

Status: **Research** · 日期：2026-10-01 · 基于 `5317eb4` · 用途：为 iota 的 composer 增加「插入换行而不提交」的按键提供依据

本文只做调研，不含实现。结论在 §6。

## 写作约定

- 每条结论后附来源。标记含义：
  - 【官方】厂商文档、CHANGELOG、仓库源码（源码链接钉在具体提交或标签上）。
  - 【实测】撰写者在本机跑出来的结果：macOS（Darwin 25.6.0）、tmux 3.7c、crossterm 0.29.0（iota 钉的版本）、Ghostty 1.3.1 已安装但**没有在真实窗口里按键**。方法与原始输出见附录 A。
  - 【本机核对】从本机安装的 Claude Code 2.1.286 二进制里读出的字符串，不是公开链接，可复现方法见附录 A.4。
  - 【推断】由两条已证实的事实推出、但没有直接验证的结论。
  - 【社区】issue 里用户的陈述、第三方文章。
- 查不到或来源不足的写「未证实」。
- 链接在 2026-10-01 打开核对过。

---

## 0. 结论先行

1. **行业惯例是 `Shift+Enter` 主、`Ctrl+J` 兜底。** 调研的 7 个产品里，6 个绑了 `Shift+Enter`，同样这 6 个绑了 `Ctrl+J`；其中 4 个还提供行尾 `\` + Enter。只有 aider 走另一条路（`Meta+Enter`）。见 §2.8。
2. **`Shift+Enter` 永远需要终端配合，`Ctrl+J` 永远不需要。** 传统编码下 Shift+Enter 与 Enter 是同一个字节 `0x0D`；`Ctrl+J` 是 `0x0A`，天生不同。见 §1。
3. **iota 的四个目标终端里，协议只在 Ghostty 上「开了就有」。** Terminal.app 两个协议都没有；tmux 不认 Kitty 协议，只认 modifyOtherKeys 且默认关；Windows Terminal 的 Kitty 协议还在 Preview 通道（稳定版仍是 1.24）。见 §3.2。
4. **一个此前没人写下来的事实：Ghostty 在应用什么都不请求时，Shift+Enter 发的是 `ESC[27;2;13~`，而 crossterm 0.29 会把这个序列整个丢掉。** 所以 iota 今天在 Ghostty 里按 Shift+Enter 很可能是「什么都不发生」，而不是测试注释里假设的「照常提交」。见 §3.4 与 §6.1。
5. **没有任何产品能检测「用户按了 Shift+Enter 但终端没告诉我」**——按定义不可能。成熟产品的做法是：兜底键无条件可用，提示文案只展示当前终端里确实能用的那个键。见 §5。

---

## 1. 问题的两面：字节层事实

终端把按键编码成字节流交给程序。传统（legacy）编码里，Enter 的各种修饰组合是这样的：

| 按键 | 字节 | 能否与 Enter 区分 |
|---|---|---|
| Enter | `0x0D`（CR） | — |
| Shift+Enter | `0x0D` | 不能 |
| Ctrl+Enter | `0x0D` | 不能 |
| Ctrl+Shift+Enter | `0x0D` | 不能 |
| Alt+Enter | `0x1B 0x0D`（ESC CR） | **能** |
| Ctrl+J | `0x0A`（LF） | **能** |
| Ctrl+M | `0x0D` | 不能（就是 Enter） |

来源：Kitty 协议规范的 legacy 编码表，Enter 一行七列依次是 `0xd, 0xd, 0x1b 0xd, 0xd, 0xd, 0x1b 0xd, 0x1b 0xd`【官方：[keyboard-protocol.rst L527-L535](https://github.com/kovidgoyal/kitty/blob/62c73ac9885b9b5e7889db7a764c815c3aee3f82/docs/keyboard-protocol.rst#L527-L535)】；规范开头也点名了这类歧义（`ctrl+i` 即 Tab、`ctrl+m` 即 Enter）【官方：[同文件 L580-L585](https://github.com/kovidgoyal/kitty/blob/62c73ac9885b9b5e7889db7a764c815c3aee3f82/docs/keyboard-protocol.rst#L580-L585)】。

要让 Shift+Enter 可区分，只有三条路：

- **Kitty keyboard protocol**：应用发 `CSI > flags u` 请求，终端改用 `CSI 13;2u` 上报 Shift+Enter。
- **xterm modifyOtherKeys**：应用发 `CSI > 4 ; N m` 请求，终端改用 `CSI 27;2;13~` 上报。
- **终端侧私有映射**：用户（或产品的 setup 命令）在终端配置里把 Shift+Enter 映射成某个固定序列。

「选哪个组合键」与「怎么让它可区分」是同一个问题。

---

## 2. 各产品

### 2.1 Claude Code

**键位。** Enter 提交。换行有四种【官方：[interactive-mode § Multiline input](https://code.claude.com/docs/en/interactive-mode#multiline-input)】：

| 方式 | 键 | 官方说明 |
|---|---|---|
| Quick escape | `\` + Enter | Works in all terminals |
| Option key | Option+Enter | 需先在 macOS 上把 Option 设为 Meta |
| Shift+Enter | Shift+Enter | iTerm2、WezTerm、Ghostty、Kitty、Warp、Apple Terminal、Windows Terminal 原生可用，其余见 terminal-config |
| Control sequence | Ctrl+J | Works in any terminal without configuration |

键位表里 `chat:newline` 的**默认绑定是 `Ctrl+J`**，`chat:submit` 是 Enter【官方：[keybindings § Chat actions](https://code.claude.com/docs/en/keybindings)】。Shift+Enter 与 Meta+Enter 不在键位表里，是 Enter 处理函数里的分支：前一个字符是 `\` → 删掉反斜杠并换行；带 `meta` 或 `shift` → 换行；否则提交【本机核对：附录 A.4】。

**是否要求 Kitty 协议：不要求，但能用就用。** 文档把终端分成四档【官方：[terminal-config § Enter multiline prompts](https://code.claude.com/docs/en/terminal-config#enter-multiline-prompts)】：

| 终端 | Shift+Enter |
|---|---|
| Ghostty、Kitty、iTerm2、WezTerm、Warp、Apple Terminal、Windows Terminal | 无需设置 |
| 其他支持 Kitty 协议的终端（foot、Alacritty ≥ 0.16） | 无需设置，需 Claude Code ≥ 2.1.269 |
| VS Code、Cursor、Devin Desktop、Alacritty < 0.16、Zed | 运行一次 `/terminal-setup`（往终端配置文件里写一条 Shift+Enter 键位） |
| gnome-terminal、JetBrains IDE | 不可用，用 Ctrl+J 或 `\` + Enter |

历史脉络【官方：[CHANGELOG](https://github.com/anthropics/claude-code/blob/main/CHANGELOG.md)】：

- 2.1.0：「Changed Shift+Enter to work out of the box in iTerm2, WezTerm, Ghostty, and Kitty without modifying terminal configs」——此前靠 `/terminal-setup` 改终端配置；之后改为运行时请求键盘协议【推断：同版本还有一条「Fixed terminal keyboard mode not being reset on exit in Ghostty, iTerm2, Kitty, and WezTerm」】。
- 2.1.269：「terminals that answer the kitty keyboard query (such as foot and Alacritty 0.16+) now get Shift+Enter」——从「按终端名单启用」改为「探测到就启用」。
- 2.1.47：新增 `chat:newline` 可配置动作。

**Apple Terminal 是特例，且有争议。**

- 官方文档把它列在「无需设置」一档【官方：同上 terminal-config】。
- 实现方式不是协议，而是**原生修饰键检测**：二进制里有 `function(){return terminal==="Apple_Terminal" && isModifierPressed("shift")}`，Enter 处理函数在 `meta||shift` 之后再问一次这个函数【本机核对：附录 A.4】。也就是 Enter 到达时去问操作系统「Shift 此刻是否按着」。
- 但有用户报告在 macOS 26.5 / Terminal 2.15 / Claude Code 2.1.150 上 Shift+Enter 仍然提交，只有 Ctrl+J 与 `\` + Enter 有效，并要求改文档【社区：[claude-code#62322](https://github.com/anthropics/claude-code/issues/62322)】。该 issue 次日关闭，无官方回复，文档至今未改。**哪边为准：未证实。**
- `/terminal-setup` 在 Apple Terminal 里做的事是**打开「Use Option as Meta Key」并关掉响铃**，提示文案是「…like Option + Enter for new line」而不是 Shift+Enter【官方：[terminal-config § Enable Option key shortcuts](https://code.claude.com/docs/en/terminal-config#enable-option-key-shortcuts-on-macos)；本机核对：附录 A.4】。

**tmux。** 「When Claude Code runs inside tmux, by default Shift+Enter submits instead of inserting a newline」，需要用户在 `~/.tmux.conf` 加【官方：[terminal-config § Configure tmux](https://code.claude.com/docs/en/terminal-config#configure-tmux)】：

```
set -g allow-passthrough on
set -s extended-keys on
set -as terminal-features 'xterm*:extkeys'
```

并且「even when the outer terminal supports it」也要配。

**SSH。** 终端身份环境变量不随 SSH 传递，所以早期按名单启用的做法在 SSH 下失效：2.1.69 修过「Shift+Enter printing `[27;2;13~` instead of inserting a newline in Ghostty over SSH」，2.1.269 起改为探测【官方：CHANGELOG】。Apple Terminal 的原生修饰键检测在 SSH 下不可能生效（进程在远端，读不到本机键盘状态）【推断】。

### 2.2 OpenAI Codex CLI（Rust / ratatui / crossterm 分支）

**键位。** 编辑器层 `insert_newline` 的默认绑定是 `ctrl-j`、`ctrl-m`、`enter`、`shift-enter`、`alt-enter`；composer 层 `submit` 是 plain `enter`【官方：[keymap.rs L1678-L1694](https://github.com/openai/codex/blob/57ac6f51639f7cad705a48ac4bd8073034a2f150/codex-rs/tui/src/keymap.rs#L1678-L1694)】。用户可在配置里改（`tui.keymap`）。

**是否依赖协议：启用但不依赖。** 启动时【官方：[keyboard_modes.rs L210-L259](https://github.com/openai/codex/blob/57ac6f51639f7cad705a48ac4bd8073034a2f150/codex-rs/tui/src/tui/keyboard_modes.rs#L210-L259)】：

- 推 Kitty 标志 `DISAMBIGUATE_ESCAPE_CODES | REPORT_ALTERNATE_KEYS`；
- 只在**不是** Ghostty / iTerm2 / tmux（非 csi-u 格式）时再加 `REPORT_EVENT_TYPES`，注释写明原因：「iTerm and Ghostty can leak shortcut release events… tmux's xterm key format also loses Shift-Enter when event types are reported」；
- 环境变量 `CODEX_TUI_DISABLE_KEYBOARD_ENHANCEMENT` 可整体关闭；WSL 下探测到 VS Code（或探测不出结果）时默认关闭，原因是死键组合会坏【官方：[同文件 L20-L57](https://github.com/openai/codex/blob/57ac6f51639f7cad705a48ac4bd8073034a2f150/codex-rs/tui/src/tui/keyboard_modes.rs#L20-L57)】。

**tmux。** Codex 会主动问 tmux 的 `extended-keys-format`（`tmux display-message -p '#{extended-keys-format}…'`，失败再 `show-options -gqv`）【官方：[tmux.rs L47-L64](https://github.com/openai/codex/blob/57ac6f51639f7cad705a48ac4bd8073034a2f150/codex-rs/tui/src/tui/tmux.rs#L47-L64)】，**只有确认是 `csi-u` 才发 `CSI > 4 ; 2 m`**，注释：「Older tmux versions… may emit xterm-style sequences, which crossterm does not parse consistently for modified keys」【官方：[keyboard_modes.rs L272-L280](https://github.com/openai/codex/blob/57ac6f51639f7cad705a48ac4bd8073034a2f150/codex-rs/tui/src/tui/keyboard_modes.rs#L272-L280)】。这与 §3.4 的实测一致。

**提示自适应。** 底栏的换行提示：探测到键盘增强且绑定里有 Shift+Enter 就显示 `shift+enter`，否则显示第一个不是 plain Enter 的绑定，默认即 `ctrl+j`【官方：[chat_composer.rs L4446-L4462](https://github.com/openai/codex/blob/57ac6f51639f7cad705a48ac4bd8073034a2f150/codex-rs/tui/src/bottom_pane/chat_composer.rs#L4446-L4462)】。

**Terminal.app / SSH。** 没有专门的官方说明：未证实。按上述机制，探测不到增强就落到 `ctrl+j` 提示【推断】。

**已知痛点**【社区】：

- [codex#2358](https://github.com/openai/codex/issues/2358)：Rust 版一度只有 Ctrl+J，用户反馈 Ctrl+J 在 VS Code 里已被占用、在 tmux 里被用作窗格导航或 prefix（「inside a tmux session, only ctrl+j is shown, but I'm already using ctrl+j for moving between panes」）。
- [codex#48680](https://github.com/openai/codex/issues/48680)（2026-09-27，open）：Windows Terminal 1.24 上，用户按 opencode 文档把 Shift+Enter 映射成 `ESC[13;2u`，Codex 原生 Windows 版把 `[13;2u` 当字面文本插进输入框；同一环境 Ctrl+J 正常。
- [codex#21562](https://github.com/openai/codex/issues/21562)：0.128.0 升级后 Konsole 里 Shift+Enter 与 Alt+Enter 双双失效。

### 2.3 opencode

- 默认 `input_newline` = `shift+return,ctrl+return,alt+return,ctrl+j`，`input_submit` = `return`【官方：[keybinds.mdx L104](https://github.com/anomalyco/opencode/blob/0112a92c416f5ad833d96e7a8308441f0a875d94/packages/web/src/content/docs/keybinds.mdx#L104)，渲染页 [opencode.ai/docs/keybinds](https://opencode.ai/docs/keybinds/)】。
- TUI 启动时带 `useKittyKeyboard: {}`，即请求 Kitty 协议【官方：[app.tsx L199](https://github.com/anomalyco/opencode/blob/0112a92c416f5ad833d96e7a8308441f0a875d94/packages/tui/src/app.tsx#L199)】。
- 文档单列一节「Shift+Enter」：「Some terminals don't send modifier keys with Enter by default. You may need to configure your terminal to send `Shift+Enter` as an escape sequence」，并给出 Windows Terminal 的手工配置——在 `settings.json` 里加一条 `sendInput` 动作发 `\u001b[13;2u`【官方：[同文件 L262-L296](https://github.com/anomalyco/opencode/blob/0112a92c416f5ad833d96e7a8308441f0a875d94/packages/web/src/content/docs/keybinds.mdx#L262-L296)】。
- tmux / Terminal.app 下的行为：未证实。

### 2.4 crush（Charm）

- 换行绑定 `shift+enter` 与 `ctrl+j`；帮助文案默认写 `ctrl+j`，源码注释：「If the terminal supports "shift+enter", we substitute the help text to reflect that」【官方：[keys.go L139-L145](https://github.com/charmbracelet/crush/blob/76cc5c574e15072b15aaed0f4f843a5711fae0d9/internal/ui/model/keys.go#L139-L145)】。
- 收到 `KeyboardEnhancementsMsg` 且 `SupportsKeyDisambiguation()` 为真时，把帮助改成 `shift+enter`【官方：[ui.go L1147-L1152](https://github.com/charmbracelet/crush/blob/76cc5c574e15072b15aaed0f4f843a5711fae0d9/internal/ui/model/ui.go#L1147-L1152)】。
- 行尾反斜杠 + Enter 也换行：「If the last character is a backslash, remove it and add a newline」【官方：[ui.go L3165-L3176](https://github.com/charmbracelet/crush/blob/76cc5c574e15072b15aaed0f4f843a5711fae0d9/internal/ui/model/ui.go#L3165-L3176)】。

### 2.5 Gemini CLI

- `input.newline` 默认绑定：`Ctrl+Enter`、`Cmd/Win+Enter`、`Alt+Enter`、`Shift+Enter`、`Ctrl+J`；`input.submit` 是 Enter【官方：[keyboard-shortcuts.md L91-L93](https://github.com/google-gemini/gemini-cli/blob/c6bccb7ecbf6d8368d995455dd725ed34466faad/docs/reference/keyboard-shortcuts.md#L91-L93)】。
- 行尾 `\` + Enter 换行【官方：[同文件 L236-L237](https://github.com/google-gemini/gemini-cli/blob/c6bccb7ecbf6d8368d995455dd725ed34466faad/docs/reference/keyboard-shortcuts.md#L236-L237)】。
- 协议：启动时同时探测 Kitty（`CSI ? u`）与 modifyOtherKeys（`CSI > 4 ; ? m`），**Kitty 优先，没有再退到 modifyOtherKeys**【官方：[terminalCapabilityManager.ts L44-L48](https://github.com/google-gemini/gemini-cli/blob/c6bccb7ecbf6d8368d995455dd725ed34466faad/packages/cli/src/ui/utils/terminalCapabilityManager.ts#L44-L48)、[L265-L275](https://github.com/google-gemini/gemini-cli/blob/c6bccb7ecbf6d8368d995455dd725ed34466faad/packages/cli/src/ui/utils/terminalCapabilityManager.ts#L265-L275)】。
- 文档「Limitations」一节直说【官方：[同文件 L352-L360](https://github.com/google-gemini/gemini-cli/blob/c6bccb7ecbf6d8368d995455dd725ed34466faad/docs/reference/keyboard-shortcuts.md#L352-L360)】：
  - Windows Terminal：`shift+enter` is only supported in version 1.25 and higher；
  - macOS Terminal：`shift+enter` is not supported。
- `/terminal-setup`：「Configure terminal keybindings for multiline input (VS Code, Cursor, Windsurf)」【官方：[commands.md L452-L455](https://github.com/google-gemini/gemini-cli/blob/c6bccb7ecbf6d8368d995455dd725ed34466faad/docs/reference/commands.md#L452-L455)】。

### 2.6 aider

不走 Shift+Enter。基于 prompt-toolkit，换行方式【官方：[Entering multi-line chat messages](https://aider.chat/docs/usage/commands.html#entering-multi-line-chat-messages)】：

- `Meta-ENTER`（「Esc+ENTER in some environments」）——即 `ESC CR`，不依赖任何协议；
- 单独一行 `{` 开始、`}` 结束（或 `{tag` … `tag}`）；
- `/multiline-mode` 把 Enter 与 Meta+Enter 的职责对调；
- `/editor`（或 Ctrl-X Ctrl-E）、`/paste`、直接粘贴多行。

### 2.7 Cursor CLI

官方有专页【官方：[cursor.com/docs/cli/reference/terminal-setup](https://cursor.com/docs/cli/reference/terminal-setup)】：

- 开箱即用 Shift+Enter：iTerm2、Ghostty、Kitty、Warp、Zed；
- 需 `/setup-terminal`：Apple Terminal、Alacritty、VS Code——该命令「detects your terminal and provides instructions for configuring Option+Enter」；
- 通用方式：`Ctrl+J`（「Standard control character for newline」）、`\` + Enter；
- 对多路复用器的态度是直接放弃：「tmux and screen intercept Shift+Enter before it reaches applications. Use the universal options instead」。

### 2.8 横向对比

| 产品 | Shift+Enter | Ctrl+J | Alt/Option+Enter | Ctrl+Enter | `\` + Enter | 用到的协议 | 终端不支持时 |
|---|---|---|---|---|---|---|---|
| Claude Code | ✓ | ✓（`chat:newline` 默认） | ✓ | ✗（是 send-now） | ✓ | Kitty；VS Code 系靠改终端配置；Apple Terminal 靠原生修饰键检测 | 文档 + `/terminal-setup` + 启动提示；否则静默提交 |
| Codex CLI | ✓ | ✓ | ✓ | ✗ | ✗（未见） | Kitty；tmux 里确认 csi-u 后加 modifyOtherKeys 2 | 底栏提示自动换成 `ctrl+j`；否则静默提交 |
| opencode | ✓ | ✓ | ✓ | ✓ | ✗（未见） | Kitty | 文档给手工终端配置 |
| crush | ✓ | ✓ | ✗ | ✗ | ✓ | Kitty（Bubble Tea 上报） | 帮助默认写 `ctrl+j`，支持时才换成 `shift+enter` |
| Gemini CLI | ✓ | ✓ | ✓ | ✓ | ✓ | Kitty 优先，退 modifyOtherKeys | 文档 Limitations + `/terminal-setup`（仅 VS Code 系） |
| aider | ✗ | ✗（未见） | ✓（主） | ✗ | ✗（用 `{` `}`） | 无 | 不存在失败路径 |
| Cursor CLI | ✓ | ✓ | ✓（setup 后） | 未证实 | ✓ | 未证实（闭源） | `/setup-terminal` + 文档 |

来源即 §2.1–§2.7 各节。「✗（未见）」指在所引文档 / 源码里没找到，不等于确认不存在。

---

## 3. 协议与终端支持矩阵

### 3.1 两个协议各能区分什么

**Kitty keyboard protocol**

- **应用主动开启**：`CSI > flags u` 入栈，`CSI < u` 出栈；规范原话是「allows applications to opt-in」【官方：[keyboard-protocol.rst L20-L25](https://github.com/kovidgoyal/kitty/blob/62c73ac9885b9b5e7889db7a764c815c3aee3f82/docs/keyboard-protocol.rst#L20-L25)】。终端不会自己开。
- **探测**：发 `CSI ? u` 紧跟 DA1（`CSI c`）；只收到 DA1 应答就说明不支持【官方：[同文件 L438-L449](https://github.com/kovidgoyal/kitty/blob/62c73ac9885b9b5e7889db7a764c815c3aee3f82/docs/keyboard-protocol.rst#L438-L449)】。
- **最低一档标志 `0b1`（Disambiguate escape codes）就够区分 Enter 的修饰组合**。规范文字说 Enter / Tab / Backspace「still generate the same bytes as in legacy mode」【官方：[同文件 L341-L372](https://github.com/kovidgoyal/kitty/blob/62c73ac9885b9b5e7889db7a764c815c3aee3f82/docs/keyboard-protocol.rst#L341-L372)】，这句话只对**不带修饰键**的情形成立：kitty 的实现里，`mods == 0` 才走 `\r`，带修饰键就落到 `CSI 13;mods u`【官方：[key_encoding.c L160-L230](https://github.com/kovidgoyal/kitty/blob/62c73ac9885b9b5e7889db7a764c815c3aee3f82/kitty/key_encoding.c#L160-L230)】；Ghostty 的单元测试直接叫「kitty: shift+enter emits CSI u」，期望值 `\x1b[13;2u`【官方：[key_encode.zig@v1.3.1 L1336](https://github.com/ghostty-org/ghostty/blob/v1.3.1/src/input/key_encode.zig#L1336)】。
- 修饰值 = 1 + 位域（shift 1、alt 2、ctrl 4）。于是：

  | 按键 | 标志 `0b1` 下的编码 |
  |---|---|
  | Enter | `\r`（不变） |
  | Shift+Enter | `CSI 13;2u` |
  | Alt+Enter | `CSI 13;3u` |
  | Ctrl+Enter | `CSI 13;5u` |
  | Ctrl+J | `CSI 106;5u`（不再是 `\n`） |

  **Shift+Enter 能区分，Ctrl+Enter 也能。** 注意 Ctrl+J 的字节也变了，解析器必须认 CSI u 形式（crossterm 认，见 §3.4）。

**xterm modifyOtherKeys**

- **应用主动开启**：`CSI > Pp ; Pv m`，`Pp = 4 ⇒ modifyOtherKeys`【官方：[xterm ctlseqs，XTMODKEYS](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html)】。资源默认值「The default is "0": 0 disables this feature」；1 对「those with well-known behavior」之外的键生效，2 连这些例外也改写【官方：[xterm 手册 modifyOtherKeys](https://invisible-island.net/xterm/manpage/xterm.html#VT100-Widget-Resources:modifyOtherKeys)】。
- 上报格式 `CSI 27 ; 修饰 ; 码点 ~`；`formatOtherKeys` 资源设为 1 时换成 `CSI 码点 ; 修饰 u`【官方：[xterm modified-keys](https://invisible-island.net/xterm/modified-keys.html)】。
- xterm 自己的对照表里，`XK_Return` 一行【官方：[modified-keys-us-pc105](https://invisible-island.net/xterm/modified-keys-us-pc105.html)，表「Other modified-key escapes」，撰写者解析 HTML 取出】：

  | 修饰 | Mode 0 | Mode 1 | Mode 2 |
  |---|---|---|---|
  | 无 | `\r` | `\r` | `\r` |
  | Shift | `\r` | `\E[27;2;13~` | `\E[27;2;13~` |
  | Alt | `\r` | `\E[27;3;13~` | `\E[27;3;13~` |
  | Ctrl | `\r` | `\E[27;5;13~` | `\E[27;5;13~` |

  **mode 1 就足以区分 Shift+Enter 与 Ctrl+Enter。** mode 2 的代价是连 Ctrl+J 这类本来有 C0 字节的键也被改写（§3.3 实测：`\n` 变成 `CSI 27;5;106~`）。

### 3.2 矩阵

列含义：**Kitty** / **modifyOtherKeys** = 应用请求后终端是否照办；**不请求时 Shift+Enter** = 应用什么都不发时终端送来的字节；**用户要不要配** = 为了让「应用请求即生效」，用户是否得先改终端配置。

iota 的四个目标终端：

| 终端 | Kitty | modifyOtherKeys | 不请求时 Shift+Enter | 用户要不要配 |
|---|---|---|---|---|
| **Ghostty** | ✓ | ✓（mode 2） | **`ESC[27;2;13~`**（不是 `\r`） | 不用 |
| **Terminal.app** | ✗ | ✗ | `\r` | 配了也没有；只能开 Option-as-Meta 换来 Option+Enter |
| **tmux**（3.7c） | ✗（不应答探测，入栈被忽略） | ✓，但 `extended-keys` 默认 `off` | `\r` | **要**：`extended-keys on`，且外层终端需有 `extkeys` 特性 |
| **Windows Terminal** | ✓ 仅 1.25+（截至 2026-07-16 仍是 Preview；稳定版 1.24） | 未证实 | 未证实（VT 输入路径）；原生控制台 API 路径见下 | 读 VT 输入的应用：稳定版要么装 Preview，要么手工映射；读控制台 API 的应用可能不用（见下） |

逐格来源：

- **Ghostty · Kitty**：列在规范的实现者名单里【官方：[keyboard-protocol.rst L34-L47](https://github.com/kovidgoyal/kitty/blob/62c73ac9885b9b5e7889db7a764c815c3aee3f82/docs/keyboard-protocol.rst#L34-L47)】；测试见 §3.1。
- **Ghostty · modifyOtherKeys**：源码注释「We only change behavior for "set_other" which is ESC [ > 4; 2 m」【官方：[function_keys.zig L15-L27](https://github.com/ghostty-org/ghostty/blob/d5427745de0b9339736627b170ddf38dd2a35241/src/input/function_keys.zig#L15-L27)】。
- **Ghostty · 不请求时**：Enter 的编码表第一条就是 `shift → "\x1b[27;2;13~"`，没有 `modify_other_keys` 条件，即任何模式都生效；同表 `ctrl → "\x1b[27;5;13~"`，`alt → "\x1b\r"`（mode 2 下改为 `\x1b[27;3;13~`）【官方：[function_keys.zig@v1.3.1 L195-L219](https://github.com/ghostty-org/ghostty/blob/v1.3.1/src/input/function_keys.zig#L195-L219)】。Claude Code 2.1.69 修的「Shift+Enter printing `[27;2;13~`… in Ghostty over SSH」是同一事实的旁证【官方：CHANGELOG】。**未在真实 Ghostty 窗口里按键验证。**
- **Terminal.app**：Gemini CLI 文档「`shift+enter` is not supported」【官方：§2.5】；「Apple Terminal emits the same byte, carriage return (`0x0d`), for both Enter and Shift+Enter… There is no first-party Apple Terminal setting that produces a distinct Shift+Enter」【社区：[claude-code#62322](https://github.com/anthropics/claude-code/issues/62322)】；第三方文章为此动用了 Karabiner-Elements 在系统层把 Shift+Return 改写成 Ctrl+J【社区：[classmethod](https://dev.classmethod.jp/en/articles/mapping-shift-enter-key-to-line-feed-for-macos-terminal-claude-code/)】。Apple 官方未见任何协议支持声明。
- **tmux · Kitty**：【实测：附录 A.1】对 `CSI ? u` 无应答（只回 DA1 `ESC[?1;2;4c`），`CSI > 1 u` 之后 Shift+Enter 仍是 `\r`。tmux 的手册与 CHANGES 里没有出现 kitty 一词【官方：[tmux.1](https://github.com/tmux/tmux/blob/5a820e63b72f05c121441149c72327aeeb16dfa4/tmux.1)、[CHANGES](https://github.com/tmux/tmux/blob/5a820e63b72f05c121441149c72327aeeb16dfa4/CHANGES)，撰写者 grep】。
- **tmux · modifyOtherKeys**：`extended-keys [on | off | always]`，「This is the equivalent of the modifyOtherKeys xterm(1) resource」；`on` 时由窗格内程序请求 mode 1 或 2，`always` 时强制 mode 1，`off` 时「only standard keys are reported」【官方：[tmux.1 L4913-L4946](https://github.com/tmux/tmux/blob/5a820e63b72f05c121441149c72327aeeb16dfa4/tmux.1#L4913-L4946)】。默认值 `off`、格式默认 `xterm`【实测：`tmux -f /dev/null` 下 `show -sv`】。版本史：3.2 引入、3.2a 加 `always`、3.5 重写并加 `extended-keys-format`【官方：[CHANGES L847-L851](https://github.com/tmux/tmux/blob/5a820e63b72f05c121441149c72327aeeb16dfa4/CHANGES#L847-L851)、[L1227-L1228](https://github.com/tmux/tmux/blob/5a820e63b72f05c121441149c72327aeeb16dfa4/CHANGES#L1227-L1228)、[L1371-L1375](https://github.com/tmux/tmux/blob/5a820e63b72f05c121441149c72327aeeb16dfa4/CHANGES#L1371-L1375)】。
- **tmux · 外层那一段**：tmux「will always request extended keys itself if the terminal supports them」，是否支持看 `terminal-features` 的 `extkeys`【官方：同上 tmux.1】。**外层终端 → tmux 这一段撰写者没有实测**（`send-keys` 只能模拟 tmux → 窗格这一段）。
- **Windows Terminal · Kitty**：PR「Implement the Kitty Keyboard Protocol」2026-02-17 合并【官方：[terminal#19817](https://github.com/microsoft/terminal/pull/19817)】，随 Preview 1.25 于 2026-03-05 发布【官方：[Windows Terminal Preview 1.25 Release](https://devblogs.microsoft.com/commandline/windows-terminal-preview-1-25-release/)】。发布列表里最新一对是 2026-07-16 的 `v1.25.1912.0`（Preview）与 `v1.24.11911.0`（稳定）【官方：[releases](https://github.com/microsoft/terminal/releases)】。
- **Windows Terminal · 另两个事实**：`alt+enter` 默认绑定到 `toggleFullscreen`，不会到达应用【官方：[defaults.json L725](https://github.com/microsoft/terminal/blob/2b5336c1fca1e53ceeaac09710a13c478938cc8d/src/cascadia/TerminalSettingsModel/defaults.json#L725)】；1.25 之前 Ctrl+Enter 对 VT 输入的应用表现为换行符【官方：Claude Code CHANGELOG 2.1.282「terminals that send it as a newline (Windows Terminal before 1.25)」】。
- **Windows · 原生控制台路径**：crossterm 在 Windows 上读的是控制台输入记录，`VK_RETURN → KeyCode::Enter`，修饰键取自记录里的 `control_key_state`（含 `SHIFT_PRESSED`）【官方：[windows/parse.rs](https://github.com/crossterm-rs/crossterm/blob/0.29/src/event/sys/windows/parse.rs#L204-L246)】。所以 iota 在 Windows 上**可能**不靠任何 VT 协议就拿得到 Shift+Enter【推断，未实测；`docs/TUI-VERIFY.md` §9 整节尚未跑过】。反过来，crossterm 的 Kitty 入栈在 Windows 上是不可用的：`supports_keyboard_enhancement()`「always returns `Ok(false)` on Windows」【官方：[terminal/sys/windows.rs L73-L77](https://github.com/crossterm-rs/crossterm/blob/0.29/src/terminal/sys/windows.rs#L73-L77)】。

顺带查到的其他终端（非 iota 目标，供对照）：

| 终端 | Kitty | modifyOtherKeys | 不请求时 Shift+Enter | 用户要不要配 |
|---|---|---|---|---|
| kitty | ✓（协议出处） | 未证实 | `\r` | 不用 |
| WezTerm | ✓，但 `enable_kitty_keyboard` **默认 `false`** | ✓（收到 `CSI >4;Nm` 即切换） | 未证实 | Kitty 要配；modifyOtherKeys 不用 |
| iTerm2 | ✓ | ✓ | 未证实 | 有开关「Apps can change how keys are reported」，默认值文档未写明：未证实 |
| Alacritty | ✓ | 未证实 | 未证实 | 未证实 |
| VS Code 集成终端 | 未证实 | 未证实 | `\r` | 要（各产品都靠 `/terminal-setup` 写键位） |

来源：kitty legacy 行为见 §1 的编码表；WezTerm【官方：[key-encoding](https://wezterm.org/config/key-encoding.html)、[enable_kitty_keyboard](https://wezterm.org/config/lua/config/enable_kitty_keyboard.html)】；iTerm2【官方：[Profiles › Keys](https://iterm2.com/documentation-preferences-profiles-keys.html)，其中「Report modifiers using CSI u」一项注明应用应改用 Kitty 协议；tmux wiki 把 xterm、mintty、iTerm2 列为支持 extended keys 的终端：[Modifier-Keys](https://github.com/tmux/tmux/wiki/Modifier-Keys)】；Alacritty 与 iTerm2 在 Kitty 规范实现者名单里【官方：同 Ghostty · Kitty】；VS Code 一行来自 Claude Code 与 Gemini CLI 都需要 `/terminal-setup` 这一事实【官方：§2.1、§2.5】。

### 3.3 tmux 3.7c 实测：窗格里的程序到底收到什么

方法见附录 A.1：隔离的 tmux 服务器（`-f /dev/null`），窗格里跑一个 raw 模式的字节记录器，用 `tmux send-keys` 送键。

| tmux 配置 | 程序的请求 | Enter | Shift+Enter | Ctrl+Enter | Alt+Enter | Ctrl+J |
|---|---|---|---|---|---|---|
| 默认（`off`） | 无 | `\r` | `\r` | `\r` | `ESC \r` | `\n` |
| 默认（`off`） | `CSI >4;2m` | `\r` | `\r` | `\r` | `ESC \r` | `\n` |
| 默认（`off`） | `CSI >1u`（Kitty） | `\r` | `\r` | `\r` | `ESC \r` | `\n` |
| `on` | 无 | `\r` | `\r` | `\r` | `ESC \r` | `\n` |
| `on` | `CSI >1u`（Kitty） | `\r` | `\r` | `\r` | `ESC \r` | `\n` |
| `on` | `CSI >4;1m` | `\r` | `ESC[27;2;13~` | `ESC[27;5;13~` | `ESC \r` | `\n` |
| `on` | `CSI >4;2m` | `\r` | `ESC[27;2;13~` | `ESC[27;5;13~` | `ESC[27;3;13~` | `ESC[27;5;106~` |
| `on` + `format csi-u` | `CSI >4;1m` | `\r` | `ESC[13;2u` | `ESC[13;5u` | `ESC \r` | `\n` |
| `on` + `format csi-u` | `CSI >4;2m` | `\r` | `ESC[13;2u` | `ESC[13;5u` | `ESC[13;3u` | `ESC[106;5u` |
| `always` | 无 | `\r` | `ESC[27;2;13~` | `ESC[27;5;13~` | `ESC \r` | `\n` |

读法：

- **tmux 会不会吃掉序列：默认会。** `extended-keys off` 时无论程序请求什么，Shift+Enter 都是 `\r`。
- **只推 Kitty 标志在 tmux 里完全无效**，哪怕 `extended-keys on`。要在 tmux 里拿到 Shift+Enter，程序必须发 modifyOtherKeys 请求。
- mode 1 比 mode 2 干净：Shift+Enter 可区分，而 Alt+Enter 与 Ctrl+J 保持传统字节。
- 默认格式是 xterm 风格（`CSI 27;…~`），要 CSI u 风格得用户额外设 `extended-keys-format csi-u`。

### 3.4 crossterm 0.29 能解析什么（实测）

方法见附录 A.2 / A.3。同分支的 `docs/history/composer-newline-recon.md`（另一份并行产出的代码勘察）用自己的探针独立测到了同样的结果。

| 送进来的字节 | 谁会发 | crossterm 0.29 报的事件 |
|---|---|---|
| `\r` | 所有终端的 Enter | `Enter` |
| `\n` | Ctrl+J（传统） | `Char('j') + CONTROL` |
| `ESC \r` | Alt+Enter（传统） | `Enter + ALT` |
| `CSI 13;2u` | Kitty / tmux csi-u 的 Shift+Enter | `Enter + SHIFT` |
| `CSI 13;5u` | 同上的 Ctrl+Enter | `Enter + CONTROL` |
| `CSI 13;3u` | 同上的 Alt+Enter | `Enter + ALT` |
| `CSI 106;5u` | Kitty / mode 2 csi-u 的 Ctrl+J | `Char('j') + CONTROL` |
| **`CSI 27;2;13~`** | **Ghostty 默认、tmux xterm 格式、xterm 的 Shift+Enter** | **没有事件，整个序列被丢弃** |
| `CSI 27;5;13~` | 同上的 Ctrl+Enter | 没有事件 |
| `CSI 27;5;106~` | tmux mode 2 xterm 格式的 Ctrl+J | 没有事件 |

原因在源码里：`\n` 在 raw 模式下落到 Ctrl+字母分支，注释写着「it's better to use Ctrl+J」【官方：[unix/parse.rs L92-L111](https://github.com/crossterm-rs/crossterm/blob/0.29/src/event/sys/unix/parse.rs#L92-L111)】；`CSI … ~` 的第一个参数只认功能键编号，27 不在表里，返回解析错误【官方：[同文件 L619-L654](https://github.com/crossterm-rs/crossterm/blob/0.29/src/event/sys/unix/parse.rs#L619-L654)】。

把 §3.3 与本节接起来，端到端的结果是（crossterm 探针跑在 tmux 3.7c 里）：

| tmux 配置 | 应用的请求 | Shift+Enter 在应用里是 |
|---|---|---|
| 默认 | 任意 | `Enter`（与 Enter 无异） |
| `on` | 仅 Kitty 入栈 | `Enter`（与 Enter 无异） |
| `on`（格式 xterm） | `CSI >4;1m` | **无事件（被吞）** |
| `on` + `format csi-u` | `CSI >4;1m` 或 `2m` | `Enter + SHIFT` ✓ |
| `always`（格式 xterm） | 无 | **无事件（被吞）** |
| `always` + `format csi-u` | 无 | `Enter + SHIFT` ✓ |

Ctrl+J 在上述每一种配置里都是 `Char('j') + CONTROL`。

**对 crossterm 应用而言，xterm 格式的 modifyOtherKeys 比不开更糟**：不开只是「Shift+Enter 等于 Enter」，开了是「Shift+Enter 没反应」。Codex 只在 tmux 确认 csi-u 时才发请求，原因正是这个（§2.2）。

---

## 4. 不依赖协议的候选逐个评

| 候选 | Ghostty | Terminal.app | tmux（默认配置） | Windows Terminal |
|---|---|---|---|---|
| `Ctrl+J` | ✓ | ✓ | ✓ | ✓ |
| `Alt/Option+Enter` | ✓ | 仅开 Option-as-Meta 后 | ✓（透传 `ESC \r`） | ✗（被全屏快捷键截走） |
| `Ctrl+Enter` | 字节可区分但 crossterm 丢弃 | ✗ | ✗ | ✗ |
| 行尾 `\` + Enter | ✓ | ✓ | ✓ | ✓ |
| `Ctrl+V Ctrl+J` | 不成立 | 不成立 | 不成立 | 不成立 |

### 4.1 `Ctrl+J`

- **机制**：`0x0A`，与 Enter 的 `0x0D` 不同（§1）。raw 模式下 crossterm 报 `Char('j') + CONTROL`【实测】。
- **哪里能用**：Claude Code 文档「Works in any terminal without configuration」【官方：§2.1】；Cursor 文档「If you're in tmux or having trouble with other keybindings, Ctrl+J is the most reliable option」【官方：§2.7】；tmux 3.7c 十种配置里八种原样透传 `\n`，例外是程序自己请求 mode 2 的两种（变成 `ESC[27;5;106~` 或 `ESC[106;5u`）【实测：§3.3】；Windows Terminal 1.24 上 Codex 用户确认可用【社区：[codex#48680](https://github.com/openai/codex/issues/48680)】。Ghostty 与 Terminal.app 未在真实窗口实测，依据是传统编码与上述官方表述。
- **开了 Kitty 协议以后**字节变成 `CSI 106;5u`，crossterm 仍报同一个事件【实测：§3.4】。Claude Code 为此修过一次回归（2.1.212「Ctrl+J not inserting a newline… on terminals with extended key reporting」）【官方：CHANGELOG】——说明这条路径必须有测试。
- **弱点一：会被别的东西先截走。** vim-tmux-navigator 把 `C-j` 绑在 tmux 的 root 表上做窗格切换【官方：[README L100](https://github.com/christoomey/vim-tmux-navigator/blob/e41c431a0c7b7388ae7ba341f01a0d217eb3a432/README.md?plain=1#L100)】；用户反馈 VS Code 里 Ctrl+J 已有用途、有人拿它当 tmux prefix【社区：[codex#2358](https://github.com/openai/codex/issues/2358)】。
- **弱点二：没人猜得到。** 必须靠提示文案告诉用户。
- **结论**：唯一一个在四个目标终端的默认配置下都成立的按键组合。

### 4.2 `Alt/Option+Enter`

- **机制**：传统编码就是 `ESC \r`（§1），crossterm 报 `Enter + ALT`【实测】。
- **Ghostty**：编码表 `alt → "\x1b\r"`【官方：§3.2 引的 function_keys.zig】。macOS 上 Option 是否算 Alt 受 `macos-option-as-alt` 影响，但文档注明「if an Option-sequence doesn't produce a printable character, it will be treated as Alt regardless of this setting」【官方：[Config.zig L3442-L3474](https://github.com/ghostty-org/ghostty/blob/d5427745de0b9339736627b170ddf38dd2a35241/src/config/Config.zig#L3442-L3474)】，Enter 不产生可打印字符，所以默认可用【推断】。
- **Terminal.app**：默认 Option 不作 Meta，Option+Enter 就是 Enter；要在 Settings → Profiles → Keyboard 勾「Use Option as Meta Key」【官方：[terminal-config § Enable Option key shortcuts](https://code.claude.com/docs/en/terminal-config#enable-option-key-shortcuts-on-macos)】。代价是 Option 不能再输入特殊字符。Claude Code 与 Cursor 的 setup 命令在 Apple Terminal 上做的就是这件事（§2.1、§2.7）。
- **tmux**：默认透传 `ESC \r`【实测：§3.3】。
- **Windows Terminal**：`alt+enter` 是切全屏的默认键位，到不了应用【官方：§3.2】。
- **弱点**：`ESC \r` 与「先按 Esc 再按 Enter」在字节上无法区分，只能靠时序；aider 的文档干脆把后者写成等价用法（「Esc+ENTER in some environments」）【官方：§2.6】。对 Esc 有独立含义的 TUI 是个隐患。
- **结论**：免费的别名（crossterm 已经解出来了），但不能当主力——Terminal.app 默认不通，Windows Terminal 完全不通。

### 4.3 `Ctrl+Enter`

- 传统编码与 Enter 同码（§1）。Ghostty 默认发 `ESC[27;5;13~`，可区分但 crossterm 丢弃（§3.2、§3.4）。Windows Terminal 1.25 之前对 VT 应用表现为换行符（§3.2）。Terminal.app、tmux 默认都是 `\r`。
- 语义也不统一：opencode 与 Gemini CLI 把它当换行，Claude Code 把它当「立即发送」（`chat:sendNow`，文档注明「Terminals that don't report extended keys deliver `Ctrl+Enter` as plain `Enter`」）【官方：[keybindings](https://code.claude.com/docs/en/keybindings)】。
- **结论**：与 Shift+Enter 同样依赖协议，却没有 Shift+Enter 的惯例地位。不选。

### 4.4 行尾 `\` + Enter

- **机制**：纯应用层——Enter 到达时看光标前一个字符是不是反斜杠，是就删掉它并插入换行。不依赖终端任何能力。
- **谁在用**：Claude Code（「Works in all terminals」）、Gemini CLI、Cursor CLI、crush（§2）。
- **代价**：想提交一条以 `\` 结尾的消息（例如 Windows 路径 `C:\dir\`）会被拦成换行；每行多敲一个字符；对新用户同样需要提示。
- **结论**：唯一一个连 tmux 键位冲突都绕得开的方案——当 Ctrl+J 被 vim-tmux-navigator 截走、Shift+Enter 又过不了 tmux、终端还是没开 Option-as-Meta 的 Terminal.app 时，只剩它。

### 4.5 `Ctrl+V Ctrl+J`

- 这是 tty 行规程 / readline 的「quoted insert」：Ctrl+V（`lnext`）让下一个控制字符按字面进入输入行。它在 **cooked 模式或 readline 里**才有意义。
- raw 模式的 TUI 里，Ctrl+V 只是字节 `0x16`，除非应用自己实现引用插入；而 Ctrl+J 本身已经是可区分的 `0x0A`（§4.1），前面加 Ctrl+V 不增加任何信息【推断】。有 Codex 用户提到自家软件「use ctr-j as an alias to ctr-v ctr-j」【社区：[codex#2358](https://github.com/openai/codex/issues/2358)】，正说明二者在 TUI 里是一回事。
- Claude Code 里 Ctrl+V 是粘贴图片（`chat:imagePaste`）【官方：[keybindings](https://code.claude.com/docs/en/keybindings)】，没有引用插入。调研的产品里没有一个实现它。
- **结论**：不是独立候选。

---

## 5. 失败时的 UX：成熟产品怎么办

| 手段 | 谁在用 | 来源 |
|---|---|---|
| **兜底键无条件可用**（Ctrl+J 始终绑定，不看终端能力） | Claude Code、Codex、crush、Gemini CLI、opencode、Cursor | §2 |
| **提示文案自适应**：只在确认终端支持时才写 `shift+enter`，否则写 `ctrl+j` | Codex（底栏）、crush（帮助） | §2.2、§2.4 |
| **文档专页**，以症状开头（「Shift+Enter submits instead of inserting a newline」） | Claude Code、Cursor；Gemini CLI 有 Limitations 一节 | §2.1、§2.7、§2.5 |
| **setup 命令**替用户改终端配置或给出步骤 | Claude Code `/terminal-setup`、Gemini CLI `/terminal-setup`、Cursor `/setup-terminal` | §2.1、§2.5、§2.7 |
| **启动提示** | Claude Code：「Run /terminal-setup to enable convenient terminal integration like Shift + Enter for new line and more」（Apple Terminal 上换成 Option + Enter） | 【本机核对：附录 A.4】 |
| **逃生开关**：环境变量整体关掉键盘增强 | Codex `CODEX_TUI_DISABLE_KEYBOARD_ENHANCEMENT` | §2.2 |
| **对 tmux 直接劝退** | Cursor：「Use the universal options instead」 | §2.7 |

三点观察：

1. **终端不支持时，所有产品的 Shift+Enter 都是静默提交。** 没有人弹错误——应用收到的就是一个普通的 Enter，没有可供判断的信号。能做的只有事前告知。
2. **setup 命令是维护负担。** Claude Code 的 CHANGELOG 里光 `/terminal-setup` 的修复就有：覆盖了用户整个 Zed `keymap.json`（2.1.247）、在 Windows Terminal 里报自相矛盾的错（2.1.132）、在 Warp 里误提示（2.1.47）、往 VS Code 的 Shift+Enter 里多写了反斜杠（2.0.28）【官方：CHANGELOG】。
3. **开启键盘协议本身会引入一类新 bug。** 同一份 CHANGELOG：退出后终端停留在增强模式（2.1.0、2.1.85）、Kitty 协议下大写字母变小写（2.1.98）、Shift+非 ASCII 字符丢失（2.1.166）、非拉丁键盘布局下 Ctrl 快捷键失效（2.1.247）、Ctrl+Z 挂起失效（2.1.9）、小键盘输出转义序列（2.1.6）【官方：CHANGELOG】。Codex 则因为释放事件泄漏而对 Ghostty / iTerm2 少开一个标志、因为死键问题在 WSL + VS Code 下整体关闭（§2.2）。

---

## 6. 对 iota 的建议

### 6.1 先认清现状

- composer 已经能承载多行草稿（`src/ui/input/composer.rs`、`editor.rs` 按逻辑行工作），缺的只是「插入换行」这个按键。
- `src/ui/input/keys.rs:89-93`：`KeyCode::Enter` 不看修饰键，一律提交。`src/ui/input/keys/tests.rs:440-470` 把「带 SHIFT / ALT / CONTROL 的 Enter 仍然提交」钉成了测试，注释是「no terminal's Shift+Enter inserts a newline here」。
- iota 目前只开了 bracketed paste（`src/ui/runtime/handle.rs:165`），没有请求任何键盘协议。
- **因此今天在 Ghostty 里按 Shift+Enter，iota 收到的是 `ESC[27;2;13~`，crossterm 丢弃它，结果是既不换行也不提交**【推断：§3.2 的 Ghostty 源码 + §3.4 的 crossterm 实测；未在真实窗口验证，一次按键即可确认】。上面那条测试覆盖的 `Enter + SHIFT` 事件，在 Unix 上只有终端发 CSI u 时才会出现。

### 6.2 推荐

- **主组合：`Shift+Enter`。** 理由：7 个产品里 6 个用它，也是聊天类界面的通行习惯；在首要目标 Ghostty 上开启 Kitty 协议后即可用。
- **兜底：`Ctrl+J`。** 理由：唯一一个在 Ghostty、Terminal.app、tmux、Windows Terminal 的默认配置下都成立的组合（§4）；同样是那 6 个产品的共同选择；crossterm 现在就能解出来，零协议依赖。**无条件绑定，不看终端能力。**
- **顺手接受 `Alt+Enter`**，不宣传。crossterm 已经报 `Enter + ALT`，成本为零；它恰好补上「Ghostty + tmux + Ctrl+J 被窗格导航占用」这个洞（tmux 默认透传 `ESC \r`）。
- **`Ctrl+Enter` 不绑换行**，保持提交。它在没有协议的终端里本来就等于 Enter，绑了只会让行为因终端而异。
- **行尾 `\` + Enter：建议列为第二兜底，可以晚于前两项。** 它是 Terminal.app + tmux + 键位冲突这种最坏组合下唯一的出路，但要处理「消息确实以反斜杠结尾」的边界，是否值得做留给设计阶段。
- **不采纳**：`Ctrl+V Ctrl+J`（§4.5）；Claude Code 式的原生修饰键检测（要引入平台原生依赖、SSH 下必然失效、其可靠性连 Claude Code 自己的用户都有异议，§2.1）；`/terminal-setup` 式的改写终端配置（§5 观察 2）。

### 6.3 要不要依赖协议：启用，但不依赖

- **启用 Kitty 协议，只开 `DISAMBIGUATE_ESCAPE_CODES` 一个标志。** 这一档已足够让 Shift+Enter 变成 `CSI 13;2u`（§3.1），crossterm 0.29 能解析（§3.4）。不要开 `REPORT_EVENT_TYPES`：Codex 的注释记录了它在 Ghostty / iTerm2 上泄漏释放事件、在 tmux 上弄丢 Shift+Enter（§2.2）。
- **先探测再入栈**：crossterm 的 `supports_keyboard_enhancement()` 走的就是规范的 `CSI ? u` + DA1 探测。探测不到（Terminal.app、tmux、Windows）就不入栈，行为与今天完全一致。
- **退出路径必须出栈**，包括 panic、Ctrl+Z 挂起与恢复。别人踩过的坑见 §5 观察 3。
- **功能不依赖它**：协议缺席时 Ctrl+J 照常工作，Shift+Enter 退化成提交。
- **这是有风险的改动，不只影响 Enter。** 开启后 Esc、Ctrl+字母、Alt+字母的字节全部变成 CSI u 形式。§5 观察 3 列的那些回归（大写、非 ASCII、非拉丁布局、挂起）需要在 `docs/TUI-VERIFY.md` 里各有一项，输入法（§1 的 preedit）要重点看。
- **modifyOtherKeys 暂不请求。** 它只对 tmux 有意义，而 tmux 的默认格式会让 crossterm 把 Shift+Enter 吞掉（§3.4）。如果以后要做，照 Codex 的办法：先问 `tmux display-message -p '#{extended-keys-format}'`，确认是 `csi-u` 才发 `CSI > 4 ; 1 m`（mode 1 够用，且不改写 Ctrl+J）。

### 6.4 四个目标终端分别会怎样

| 终端 | Shift+Enter | Ctrl+J | 备注 |
|---|---|---|---|
| Ghostty | ✓（开 Kitty 标志后） | ✓ | 不开协议时 Shift+Enter 被吞（§6.1） |
| Terminal.app | ✗，等于提交 | ✓ | 开 Option-as-Meta 后 Option+Enter 可用 |
| tmux | 默认 ✗，等于提交 | ✓ | 用户设了 `extended-keys always` 且格式为 xterm 时 Shift+Enter 被吞，iota 这边无解 |
| Windows Terminal | 可能 ✓（控制台 API 自带修饰键） | ✓ | 未实测；Alt+Enter 被全屏键位占用 |

### 6.5 失败时怎么办

- **提示文案自适应**（Codex / crush 的做法）：探测到键盘增强才显示 `shift+enter`，否则显示 `ctrl+j`。永远不向用户展示一个在他的终端里不工作的键。
- **文档写一小节**，以症状开头，三行讲清：Ctrl+J 到处可用；Shift+Enter 需要 Ghostty / kitty / iTerm2 这类终端；tmux 用户想要 Shift+Enter 得自己加 `extended-keys on` + `extended-keys-format csi-u`（并说明 iota 目前不请求，或届时已请求）。
- **不做运行时报错**：终端不支持时应用收不到任何信号，无从报起（§5 观察 1）。
- **被吞的 Shift+Enter 是已知局限**：Ghostty 不开协议、tmux `always` + xterm 格式这两种情形，按键在到达 iota 之前就被 crossterm 丢了。前者靠开协议消除，后者只能写进文档。
- **留一个逃生开关**（环境变量或配置项）关掉键盘增强，像 Codex 那样。出了 §5 观察 3 那类问题时用户能自救。

### 6.6 落地前需要补的验证

- 在真实 Ghostty 1.3.1 窗口里确认 §6.1 的「被吞」现象，以及开启 Kitty 标志后 Shift+Enter 报 `Enter + SHIFT`。
- 在真实 Terminal.app 里确认 Ctrl+J 插入换行、Shift+Enter 提交。
- Windows Terminal 稳定版（1.24）上 crossterm 是否真的报 `Enter + SHIFT`（§3.2 的推断）。
- 外层终端 → tmux 这一段（§3.2 注明未测）。
- `src/ui/input/keys/tests.rs:440-470` 那条测试的断言要随决定改写：SHIFT 与 ALT 变成换行，CONTROL 维持提交。

---

## 附录 A：实测方法与原始输出

环境：macOS Darwin 25.6.0，tmux 3.7c，rustc 1.98.0，crossterm 0.29.0，Claude Code 2.1.286。每组实验用独立的 tmux 服务器（`tmux -L <随机名> -f /dev/null`），不读用户配置，跑完即 `kill-server`。

### A.1 tmux 向窗格内程序转发的字节

窗格里跑一个 Python 脚本：`tty.setraw` → 向 stdout 写一段可选的前导序列（即「程序的请求」）→ 把读到的字节 `repr` 后写日志。外面用 `tmux send-keys Enter | S-Enter | C-Enter | M-Enter | C-j` 逐个送键。结果即 §3.3 的表。

对 Kitty 探测的应答：前导序列为 `ESC[?u ESC[c`、`extended-keys on` 时，日志只有

```
b'\x1b[?1;2;4c'
```

没有 `CSI ? flags u` 应答。

默认值：`tmux -f /dev/null` 下 `show -sv extended-keys` → `off`，`show -sv extended-keys-format` → `xterm`。

局限：`send-keys` 模拟的是「tmux 已经认出了这个键」之后的转发，不覆盖外层终端 → tmux 那一段。

### A.2 crossterm 0.29 对各字节序列的解析

一个 20 行的 Rust 探针（依赖 `crossterm = "=0.29.0"`）：`enable_raw_mode` → 循环 `event::read()` → 把 `KeyEvent` 的 `code` 与 `modifiers` 写日志。在 tmux 窗格里运行，用 `tmux send-keys -H <hex…>` 注入原始字节，每组后面跟一个 `x` 作哨兵。原始输出：

```
-- inject: CR (Enter everywhere)
Enter KeyModifiers(0x0)
Char('x') KeyModifiers(0x0)
-- inject: LF (Ctrl+J)
Char('j') KeyModifiers(CONTROL)
Char('x') KeyModifiers(0x0)
-- inject: ESC CR (Alt+Enter legacy)
Enter KeyModifiers(ALT)
Char('x') KeyModifiers(0x0)
-- inject: CSI 13;2u (Shift+Enter, kitty/csi-u)
Enter KeyModifiers(SHIFT)
Char('x') KeyModifiers(0x0)
-- inject: CSI 13;5u (Ctrl+Enter, kitty/csi-u)
Enter KeyModifiers(CONTROL)
Char('x') KeyModifiers(0x0)
-- inject: CSI 13;3u (Alt+Enter, kitty/csi-u)
Enter KeyModifiers(ALT)
Char('x') KeyModifiers(0x0)
-- inject: CSI 27;2;13~ (Shift+Enter, xterm/Ghostty legacy)
Char('x') KeyModifiers(0x0)
-- inject: CSI 27;5;13~ (Ctrl+Enter, xterm/Ghostty legacy)
Char('x') KeyModifiers(0x0)
-- inject: CSI 106;5u (Ctrl+J under mok2 csi-u)
Char('j') KeyModifiers(CONTROL)
Char('x') KeyModifiers(0x0)
-- inject: CSI 27;5;106~ (Ctrl+J under mok2 xterm)
Char('x') KeyModifiers(0x0)
```

`CSI 27;…~` 三组只有哨兵 `x`，没有按键事件，也没有残留字符。

### A.3 端到端：crossterm 探针 + tmux 键名

同一探针加一个启动参数决定请求什么（`kitty` = `PushKeyboardEnhancementFlags(DISAMBIGUATE_ESCAPE_CODES)`，`mok1` / `mok2` = 写 `ESC[>4;1m` / `ESC[>4;2m`），用 `tmux send-keys S-Enter` 等键名送键。原始输出（空白表示该键没有产生事件）：

```
=== [app=      extended-keys=off    format=xterm]  S-Enter → Enter KeyModifiers(0x0)
=== [app=kitty extended-keys=off    format=xterm]  S-Enter → Enter KeyModifiers(0x0)
=== [app=kitty extended-keys=on     format=xterm]  S-Enter → Enter KeyModifiers(0x0)
=== [app=mok1  extended-keys=on     format=xterm]  S-Enter →            C-Enter →
=== [app=mok1  extended-keys=on     format=csi-u]  S-Enter → Enter KeyModifiers(SHIFT)   C-Enter → Enter KeyModifiers(CONTROL)
=== [app=mok2  extended-keys=on     format=csi-u]  S-Enter → Enter KeyModifiers(SHIFT)   C-Enter → Enter KeyModifiers(CONTROL)
=== [app=      extended-keys=always format=xterm]  S-Enter →            C-Enter →
=== [app=      extended-keys=always format=csi-u]  S-Enter → Enter KeyModifiers(SHIFT)   C-Enter → Enter KeyModifiers(CONTROL)
```

八组里 `Enter` 都是 `Enter KeyModifiers(0x0)`，`M-Enter` 都是 `Enter KeyModifiers(ALT)`，`C-j` 都是 `Char('j') KeyModifiers(CONTROL)`。

### A.4 Claude Code 2.1.286 二进制里的字符串

对 `~/.local/share/claude/versions/2.1.286` 做字节搜索（Python `mmap` + 正则，取匹配点前后各约 300 字节）。标识符是压缩后的名字，下面保留原样：

- 原生模块导出表里有 `getModifiers`、`isModifierPressed`。
- `function Uir(){return a.terminal==="Apple_Terminal"&&vt("shift")}`
- Enter 处理函数：`function Yt({meta:u,shift:E}){if(U&&!te&&b.offset>0&&b.text[b.offset-1]==="\\")return …b.backspace().insert(…"\n"…);if(u||E)return b.insert(…"\n"…);if(Uir())return b.insert(…"\n"…);if(n)n(…)…}`——依次是：反斜杠续行、`meta||shift`、Apple Terminal 原生 Shift 检测、提交。
- `/terminal-setup` 的描述：`if(a.terminal==="Apple_Terminal")return"Enable Option+Enter key binding for newlines and disable the audible bell (skipped in screen-reader mode)"`；对 Ghostty / Kitty / Warp / WezTerm / Windows Terminal 则是 `Check terminal setup (Shift+Enter is natively supported in …)`。
- 启动提示：`a.terminal==="Apple_Terminal"?"Run /terminal-setup to enable convenient terminal integration like Option + Enter for new line and more":"Run /terminal-setup to enable convenient terminal integration like Shift + Enter for new line and more"`。

这是对已发布二进制的静态阅读，不是官方说明；版本一变名字和逻辑都可能变。
