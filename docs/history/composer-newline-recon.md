# Composer 换行 —— 勘察（只读）

> 2026-10-01，分支 `composer-newline`（起点 `main` @ `5317eb4`）。只读勘察，没有改代码。
> 行号以 `5317eb4` 为准。标「推测」的是没有核对过的判断；其余结论都附了 `文件:行号` 或实测输出。
> 实测用的是 scratchpad 里的一个 crossterm `=0.29.0` 探针（开 raw mode，逐条打印 `Event::Key`），
> 在 tmux 3.7c 私有 server 里用 `send-keys` 喂键。探针不在仓库里。

## 结论（TL;DR）

- **今天就能用的组合键是 `Ctrl+J`**：raw mode 下它解码为 `KeyCode::Char('j') + CONTROL`，跟 Enter 不同；
  它眼下是空操作，从来不提交，也不插入任何字符。这条不依赖任何协议：只要终端发 `0x0A` 就行，
  所有 VT 类终端都发。`Alt+Enter` 也能区分（`Enter + ALT`），但 macOS 终端要开「Option 当 Meta」才会发 ESC 前缀（推测），
  而且现在有一条测试明确钉死「Enter 带任何修饰键都提交」。
- **多行能力大部分已经在了**：`Editor` 的缓冲本来就允许 `'\n'`，编辑键按逻辑行生效；composer 的换行、光标、
  多行高度、↑/↓ 行间移动都已实现（目前只有中断时 fold-back 会产生多行草稿）；提交路径和 provider 也已经在送带
  `\n` 的文本（多行粘贴展开后就是这样）。
- **最小改动面（方案 a）**：`src/ui/input/keys.rs` 在 Row 7 前面加一个 `Ctrl+J → composer.insert_str("\n")`
  分支，`src/ui/input/composer.rs` 的高度计算顺手改成按折行后的总行数算，再补测试
  （`src/ui/input/keys/tests.rs` 加单测，tmux 场景加端到端）。**不要**把换行放进共享的 `Editor::on_key`，
  否则 surface 里的单行 `Field` 也会跟着能插入换行。
- **Shift+Enter（方案 b）** 只能走 Kitty 键盘协议：crossterm 0.29 能解析 CSI‑u（`ESC[13;2u` → `Enter + SHIFT`，
  实测），但**解析不了** xterm modifyOtherKeys 的 `ESC[27;2;13~`（实测：这个序列被整条丢弃，不产生任何事件）。

---

## 1. 输入框是谁、键怎么路由、Enter 怎么变成提交

| 角色 | 位置 |
|---|---|
| 共享编辑核心 `Editor`（`value: String` + 字节偏移光标） | `src/ui/input/editor.rs:16-20` |
| 唯一的按键处理 `Editor::on_key`（emacs 编辑子集 + 字符插入） | `src/ui/input/editor.rs:82-121` |
| composer（包一个 `Editor`，外加历史、高度、补全状态） | `src/ui/input/composer.rs:24-41`，`editor: Editor` 在 `:26` |
| surface 的单行 `Field`（`/model` 手输框、Ask「Other…」编辑器、搜索框） | `src/ui/surface/field.rs:13-15`，`handle_key` → `editor.on_key` 在 `:71-73` |
| composer 的键梯（9 行优先级表） | `src/ui/input/keys.rs:19-98` |
| 事件入口 | `src/ui/runtime/event_loop.rs:561-570`（`Event::Key` → `handle_key` → `keys::update_key`，`:580-582`） |

- **共用 `Editor`**：是的，composer 和 surface 的 `Field` 用的是同一个 `Editor`（`composer.rs:15,80`，`field.rs:8,21`）。
  审批门、Ask 工具和 `/model`、`/file` 选择器都是 `tabbed` surface（例如 `src/repl/turn/interact.rs:86`、
  `src/repl/commands/model.rs:192`、`src/repl/commands/file.rs:233`）。surface 打开时，Row 1 把**所有**键交给它
  （`keys.rs:26-29` → `event_loop.rs:597-611`），composer 根本收不到。surface 自己的 Enter 分支在
  `src/ui/surface/mod.rs:104,135,179,224,258`。
- **`/file`、`/` 命令补全**不是独立输入框，而是 composer 的 Tab 循环（`keys.rs:53-56` → `suggest::tab_complete`）。
- **Enter → 提交**：`keys.rs:90-93`，`if key.code == KeyCode::Enter { m.submit(); }` **只比较 `code`，不看修饰键**，
  所以 Shift/Alt/Ctrl+Enter 今天都会提交。`Model::submit` 在 `event_loop.rs:625-638`：先 `trim`，空的就丢弃；
  有挂起的 waiter 就直接交给它（`paste::make_input`），否则入队。这个「修饰键被忽略」的行为由测试明确钉死：
  `src/ui/input/keys/tests.rs:440-475`（`row_7_enter_submits_trimmed_or_ignores_blank`，SHIFT/ALT/CONTROL 三种都断言「仍然提交」）。

## 2. 现在能区分哪些键（crossterm 0.29，实测）

解码规则见 `~/.cargo/registry/src/*/crossterm-0.29.0/src/event/sys/unix/parse.rs`：

- `0x0D` → `KeyCode::Enter`（`:92-94`）
- `0x0A` 只有在**非** raw mode 时才是 Enter（`:99-101`）；raw mode 下落进 `0x01..=0x1A` 分支，变成
  `Char('j') + CONTROL`（`:106-109`）。iota 用 crossterm 自己的 `enable_raw_mode` 开 raw mode
  （`src/ui/runtime/handle.rs:162`；oneshot 在 `src/ui/runtime/oneshot.rs:27`），所以 crossterm 内部的
  `is_raw_mode_enabled` 标志是 true（探针输出 `raw=true`）。
- `ESC` + 键 → 同一个键再加上 `ALT`（`:77-88`）
- `ESC [ … u` → `parse_csi_u_encoded_key_code`（`:203`，`:497` 起）——**不需要事先 push 增强标志**，收到就会解析。
- `ESC [ … ~` → `parse_csi_special_key_code`：第一个参数只认 1–34 范围内的 Home/End/F 键，`27` 直接 `Err`，
  也就是说 modifyOtherKeys 的格式不被支持。

**实测**（tmux 3.7c，`send-keys`；四种配置结果完全一样：默认、`extended-keys always`+`csi-u`、
`extended-keys always`（xterm 格式）、探针 push `DISAMBIGUATE_ESCAPE_CODES`+`extended-keys on`+`csi-u`；
`set -g` 与 `set -s` 各跑一遍）：

```
send Enter              → Enter KeyModifiers(0x0) Press
send C-j                → Char('j') KeyModifiers(CONTROL) Press
send M-Enter            → Enter KeyModifiers(ALT) Press
send S-Enter            → Enter KeyModifiers(0x0) Press      ← 与 Enter 无法区分
send C-Enter            → Enter KeyModifiers(0x0) Press      ← 与 Enter 无法区分
send C-m                → Enter KeyModifiers(0x0) Press
send -H 1b5b31333b3275  → Enter KeyModifiers(SHIFT) Press    ← 原始 CSI-u（ESC[13;2u）
send -H 1b5b32373b323b31337e → （没有任何事件，被丢弃）       ← modifyOtherKeys（ESC[27;2;13~）
supports_keyboard_enhancement() = Ok(false)   （tmux 里）
```

**Shift+Enter / Ctrl+Enter 今天能不能和 Enter 区分？不能。** 原因不在代码：传统编码下终端对这两个组合发的就是
`0x0D`，跟 Enter 一模一样。只有终端发 CSI‑u 时 crossterm 才能解出 `SHIFT`/`CONTROL`，而终端通常要等应用
push Kitty 标志才会发 CSI‑u（推测：少数终端，比如被 `/terminal-setup` 一类工具改过键位的 VS Code 终端，
会无条件为 Shift+Enter 发自定义序列）。tmux 里用 `send-keys S-Enter` 拿不到 CSI‑u，开了 `extended-keys` 也一样
（实测；原因未查明，推测是 `send-keys` 合成的键不走扩展编码，或者需要外层终端也声明支持）。

## 3. 有没有启用键盘增强

`grep -rn "KeyboardEnhancement\|PushKeyboard\|kitty\|Kitty\|modifyOtherKeys" src tests` 只有**一处**命中：
`src/ui/input/keys/tests.rs:31` 的一句注释（「kitty / Windows report releases and repeats」）。**没有任何 push/pop。**
`Cargo.toml:95` 写的是 `crossterm = { version = "=0.29.0" }`。0.29 提供 `PushKeyboardEnhancementFlags`、
`PopKeyboardEnhancementFlags`、`KeyboardEnhancementFlags`（`DISAMBIGUATE_ESCAPE_CODES` 等）和
`terminal::supports_keyboard_enhancement()`（`crossterm-0.29.0/src/terminal.rs:102`，`src/event.rs:493`）。
启动时打开的只有 bracketed paste（`handle.rs:165`、`oneshot.rs:28`）。键梯已经只处理 `KeyEventKind::Press`
（`keys.rs:20-22`），所以将来就算终端上报 release/repeat，也不会重复触发。

## 4. 多行输入今天存在吗

**存在，只是没有键能产生它。**

- 缓冲是单个 `String`（`editor.rs:17`），按设计可以含 `'\n'`：模块注释写明「Line-scoped where it matters — Home/End,
  Ctrl+K/U/W stop at the logical line」（`editor.rs:4-5`），`line_start`/`line_end` 也是按 `'\n'` 找边界的。
- composer 渲染已经支持多逻辑行：
  - `wrap_spans` 遇到 `"\n"` 就断行（`composer.rs:50-73`，`:56`）；`cursor_rowcol` 也一样（`:148-170`，`:156`）。
  - 每行之后接两空格的续行前缀，最多 5 行，超出时滚动让光标保持可见（`composer.rs:184-204`，`MAX_ROWS` 在 `:20`）。
  - 多行草稿里 ↑/↓ 是**行间移动**，不翻历史：`history_navigable` 要求 `line_count() <= 1`（`composer.rs:235-237`），
    `move_cursor_row` 在 `:297-320`。
- **今天唯一会产生多行草稿的路径**是中断时把队列折回 composer：`fire_cancel` 用 `typed.join("\n")`
  拼好再 `set_value`，并设置高度（`event_loop.rs:665-695`）。
- **一处已知缺口**：草稿有多个逻辑行时，高度取「逻辑行数」，不是「折行后的总行数」（`effective_height`
  `composer.rs:137-143`；`handle_edit_key` 每次编辑后执行 `height = line_count()`，`:291`）。所以一行很长、自动折了
  三行时，框不会跟着长高，只能靠视口滚动。功能上不出错，但如果要正式支持换行，应该改成按 `wrap_spans` 的总数取高度
  （clamp 到 `MAX_ROWS`）。
- **bracketed paste**：已启用（`handle.rs:165`）。`Event::Paste` 进来后走 `route_paste`（`event_loop.rs:586-593`）。
  composer 侧先做 W7 规范化（`\r\n`/`\r` → `\n`，去掉末尾换行，`paste.rs:21-24`）；**多行粘贴会折叠成一个
  `[#N 首行… M lines]` 标签**（`paste.rs:29-38`），提交时再展开（`paste.rs:58-64`）。surface 的 `Field` 侧会把换行压成空格
  （`src/ui/surface/mod.rs:513`）。tmux 场景 `tests/ui_tmux/scenarios/07-paste.sh` 端到端验证了这条路径。
- **渲染层**：transcript 的用户回显能画多行（`src/repl/render/replay.rs:249` `print_user_block`；单测在 `:575`；
  `07-paste.sh` 断言回显第 1–3 行）。**队列行不能**：`frame.rs:172-190` 把每个排队项 `truncate_ansi` 之后当成**一行**
  push 进去，`truncate_ansi` 也不处理 `'\n'`（`src/text/ansi.rs:122`）。推测：带 `\n` 的排队项会把一个帧行撑成两个终端行，
  破坏帧高度的计算。这种情况今天其实已经能触发（turn 进行中按 ESC 中断内层 scope → fold-back 出多行草稿 →
  Enter 入队），只是很少见；支持换行以后就是常态了。

## 5. 提交路径

- `submit` 只做整体 `trim()`（`event_loop.rs:626`）：首尾的空白和换行会去掉，**中间的 `\n` 原样保留**；
  历史和队列存的都是这段原文。
- REPL 端：`run.rs:567` 再做一次 `input.text.trim()`，之后 `content` 原样进入 `Message`（`run.rs:693-697`）。
  **发送前没有任何 strip `\n` 的处理。**
- provider 接受带换行的消息：多行粘贴展开后就是这样的文本，`07-paste.sh` 断言 mock provider 收到了全部三行
  （「the model saw line 3」）。JSON 序列化会把 `\n` 转义，各家 API 的 content 字段本来就是自由文本（这一句是推测，
  但已有粘贴路径作为旁证）。
- 斜杠命令：`match_cmd` 要求命令后面紧跟空格或者整行完全相等（`src/repl/commands/mod.rs:246-254`）。所以
  `"/model\n…"` **不会**被识别成命令，会作为普通消息发出去。这是可以接受的语义，但值得写进测试或文档。
- 回显：`run.rs:676-678` → `tr.user(&input.display)`，多行显示正常（见 §4）。

## 6. 测试面

- **单元**：`src/ui/input/keys/tests.rs` 用 `m.handle_key(mods(KeyCode::…, KeyModifiers::…))` 直接构造 `KeyEvent`
  （例如 `:463-468`），任何组合键都能写，包括 `Enter + SHIFT`。`src/ui/runtime/event_loop/vt100_tests.rs`
  可以把渲染结果喂进 vt100 断言网格（`:2079` 就是一个注入多行 `Event::Paste` 的例子）。
- **tmux 端到端**：`tests/ui_tmux/lib.sh:50-51` 定义了 `type_()`（`send-keys -l`，字面文本）和 `key()`
  （`send-keys` 命名键，原样透传参数，**支持 `-H` 十六进制**，`14-osc-signals.sh` 已经在用 `key -H 1b 5b 4f`）。
  - `key C-j` → 实测到达时是 `Char('j')+CONTROL` ✅
  - `key M-Enter` → 实测到达时是 `Enter+ALT` ✅
  - `key S-Enter` / `key C-Enter` → 实测与 Enter 相同 ❌，只能改用 `key -H 1b 5b 31 33 3b 32 75` 直接注入 CSI‑u ✅
    （这样测的是 iota 的解码与路由，测不到「真终端会不会发这个序列」）。
  - 断言可以复用 `composer_block` / `count_composer`（`lib.sh:306,321`）和 07 号场景的 `hist_size` 手法：
    换行不能提交，composer 变成 2 行，Enter 之后 mock 回显出两行。**可行**，建议新增一个场景，或者在 07 号里追加一段。

## 7. 最小改动面与两个方案

### 方案 (a)：`Ctrl+J`，不依赖任何协议（推荐先做）

| 文件 | 改动 |
|---|---|
| `src/ui/input/keys.rs` | 在 Row 7（`:89-93`）**前面**加一个分支：`ctrl && key.code == KeyCode::Char('j')` → `m.composer.insert_str("\n"); m.composer.end_history_nav(); return;`。顺手更新模块注释里的 9 行表（`:1-4`）。 |
| `src/ui/input/composer.rs` | `insert_str`（`:115-118`）已经会更新 `height`，可以直接用。建议把 `effective_height` / `handle_edit_key` / `insert_str` 的高度改成按折行总数取（见 §4 的缺口）。 |
| `src/ui/render/frame.rs` | `:175-190` 队列行：显示前把 `'\n'` 替换成 `⏎` 或空格，或者只显示首行并加 `…`（目前帧行里会混进原始换行，见 §4）。 |
| `src/ui/input/keys/tests.rs` | 新增：Ctrl+J 插入 `\n` 且不提交；Enter 提交出多行文本；多行草稿里 ↑ 是行间移动不是翻历史；Ctrl+J 不会被 Row 2 当成中断。`:440-475` 那条（修饰键都提交）**不用改**。 |
| `tests/ui_tmux/scenarios/` | 新增或扩展一个场景：`type_ a; key C-j; type_ b; key Enter`，断言 mock 回显出两行。 |
| 状态行 / 帮助文案（可选） | 让用户知道有 Ctrl+J。推测：可以放在 composer 为空时的提示里，需要 UI 侧确认。 |

**为什么不放进 `Editor::on_key`**：`Field`（单行）也走 `on_key`（`field.rs:71-73`），而且 surface 的粘贴会特意把换行压平
（`surface/mod.rs:513`），可见单行字段的设计意图就是不允许换行。把换行放在 composer 的键梯里，影响面只有 composer。

**代价 / 注意**：
- `Ctrl+J` 的可发现性差，需要文案提示。终端不会占用它（不像 Ctrl+S/Q 可能被 XON/XOFF 吃掉），tmux 默认也不绑定它
  （默认前缀是 C-b，推测）。
- 也可以同时接受 `Alt+Enter`（`key.code == Enter && modifiers.contains(ALT)`）：Linux 终端和 tmux 默认可用（实测 tmux）；
  macOS 的 Terminal.app / iTerm2 要开 Option→Meta/Esc+（推测）。要这么做就**必须改** `keys/tests.rs:463-474` 里 ALT 那一项。
- 推测：少数用户在 shell 里习惯 Ctrl+J = 提交（readline 里它就是 accept-line）。在这里换成插入换行是有意的语义变化，
  可以写进 CHANGELOG。

### 方案 (b)：`Shift+Enter`，依赖 Kitty 键盘协议

| 文件 | 改动 |
|---|---|
| `src/ui/runtime/handle.rs` | `start`（`:162-165`）里开 bracketed paste 之后再 `execute!(PushKeyboardEnhancementFlags(DISAMBIGUATE_ESCAPE_CODES))`；`TermGuard` 恢复时（`:66-74`）要先 `PopKeyboardEnhancementFlags`。panic 和信号的恢复路径也要覆盖到，否则用户的 shell 会一直收到 CSI‑u。 |
| `src/ui/runtime/oneshot.rs` | `RawGuard`（`:26-39`）同样做 push/pop。或者 oneshot 根本不开，因为它没有 composer。 |
| `src/ui/input/keys.rs` | Row 7 前加 `Enter && modifiers.intersects(SHIFT)` → 插入 `\n`（可以和方案 a 共用一个分支）。 |
| `src/ui/input/keys/tests.rs` | `:463-474` 的 SHIFT 项**必须改**（Shift+Enter 不再提交）。 |
| （可选）探测 | `crossterm::terminal::supports_keyboard_enhancement()` 在启动时发查询（CSI ?u + DA1）并等待回应；可以在 `handle.rs:168` 那里（`cursor::position()` 旁边，同样在 loop 线程起来之前）调用一次。 |

**代价**：
- **终端覆盖**：kitty、WezTerm、Ghostty、foot、Alacritty（0.13+）、iTerm2（新版本，可能要开选项）支持 Kitty 协议；
  macOS **Terminal.app 不支持**；VS Code / xterm.js 支持程度不确定（以上都是推测，没有逐个实测）。
- **tmux**：要外层终端支持，并且用户在 tmux 里配置了 `extended-keys on` + `extended-keys-format csi-u`（tmux 3.5+，推测）。
  在本机 tmux 3.7c 上，`send-keys S-Enter` 无论怎么配置都拿不到区分（§2 实测）。
- **modifyOtherKeys 走不通**：crossterm 0.29 不解析 `ESC[27;…~`（实测被丢弃），tmux 默认的 extended-keys 格式又恰好是这种
  xterm 格式。要支持就得自己写解析器，或者升级/patch crossterm（`Cargo.toml:87-95` 对 crossterm/ratatui 的版本有锁定约束，W8）。
- **副作用**：push `DISAMBIGUATE` 之后，Esc、Alt+字母、Ctrl+字母都会改成以 CSI‑u 形式到达。crossterm 会把它们解码成等价的
  `KeyEvent`，但现在依赖传统编码的细节可能被打破，比如 `keys/tests.rs:12` 提到的 `Char('b')+ALT`、Ctrl+Shift+C 的大小写
  （`keys/tests.rs:215-231`）。需要在支持该协议的真终端上把整个键梯回归一遍（推测有风险，没有实测）。
- **探测的代价**：`supports_keyboard_enhancement` 要一次往返；在不回 DA1 的终端或慢速 SSH 上会拖慢启动，而且和 W8
  「单一 crossterm 读者」的约束有关，只能在 loop 线程起来之前调用。**不探测也可以**：不支持的终端会忽略 push，
  Shift+Enter 退化成普通 Enter（提交），不会出错，只是「不灵」。
- **失败时怎么办**：保留方案 (a) 的 `Ctrl+J` 作为永远可用的兜底；文案可以写成「Shift+Enter（支持的终端）/ Ctrl+J 换行」。

### 建议

先做 (a)：Ctrl+J（可以加上 Alt+Enter），一并修好高度缺口和队列行的 `\n`。这一步是纯 UI 层的改动，有单测和 tmux 端到端
兜底。(b) 作为后续单独的 PR，需要先在 kitty、Ghostty、iTerm2 上把整个键梯回归一遍再决定。

## 附：复现探针

scratchpad 里的 `probe/`：一个 crossterm `=0.29.0` 的 bin 加一个 `run.sh`（`set -euo pipefail`，日志放在 `mktemp -d`
目录里，删除前守卫路径）。每种配置起一个私有 tmux server，`send-keys` 依次发 `Enter C-j M-Enter S-Enter C-Enter C-m`，
再用 `-H` 注入 CSI‑u 和 modifyOtherKeys 两个原始序列。§2 的输出就是它打印的原文。
