# Composer 换行 —— 实现复核（第一步）

> 2026-10-02，分支 `composer-newline` @ `a69d64b`。对象：`git diff 296df94..HEAD` 里的实现提交 `a69d64b`（绑定、两处收尾、测试、文档）；
> `7ba3741` 是上一轮评审本身，不在复核范围内。上一轮：`composer-newline-review.md`（下称「评审 1」），勘察：`composer-newline-recon.md`。
> 只读复核：没有改仓库里的代码或测试，没有提交。变异测试在 scratchpad 里的 `git archive` 副本上做，详见附录。
> 标「推测」的是没核对过的判断，其余结论都附了 `文件:行号`、测试名或实测输出。

## 结论（TL;DR）

**可以合并。** 实现做的就是评审 1 §6 要的东西，没有多做。clippy 干净，`cargo test` 全绿（lib 1017 个），tmux 场景 28 52/52 通过，完整 tmux 套件 27/27 个场景全部通过（附录 A.4）。
变异测试表明三处关键改动里有两处被测试真正钉住：队列行的 ` ⏎ `，以及按折行算高度。

剩下的都不阻塞合并：

1. **文档说法不准（最值得改，改动小）**：三处文档都说 Ctrl+Enter「仍提交」，并把 Shift+Enter 写成「在别处是普通 Enter、照常提交」。
   可在 **Ghostty 默认配置**下（首要目标终端），这两个键都会被 crossterm 整条丢掉，**既不换行也不提交**（评审 1 附录 A.1 实测）。
   X-65 自己在后半句提到了 Ghostty 会被丢，可同一句开头又写「every terminal sends it as a bare CR」，前后矛盾。见 §4.1。
2. **`end_history_nav()` 那一行没有被钉住**：删掉它，测试照样全绿。测试名 `…_ends_the_cycle_and_the_walk` 里「walk」这半句是假绿。
   不过这一行本身**观察不到效果**（§1.3），所以只需改测试注释，不用加测试。
3. **tmux 场景只证明「一条消息里有两行的内容」，没证明「换行符到了 provider」**：假如提交时把换行压成了空格，场景 28 也会全绿（§3.3）。
   这一点有单测 `got.text == "a\nb"` 兜底，所以不算漏洞。想补的话加一行行尾锚定的断言就够了。
4. **Ghostty 配方只验证了一半**：`keybind = shift+enter=text:\n` 能通过 `ghostty +validate-config`，`text:\n` 这个 action 实测发出 `b'\n'`（附录 A.3）。
   还没验证的只有「物理 Shift+Enter 能触发这条 keybind」。写进用户文档之前，人工按一次就行（30 秒）。
5. 合并顺序：`req-cancel`（X-64）、`bot-mode-v1`（X-58…X-63）、本分支（X-65）**编号不撞**，但三者都是在 X-57 那一行后面追加，合并时
   `DIVERGENCES.md` 会有一处纯文本冲突，按编号排好即可。

---

## 1. 三处绑定

### 1.1 Ctrl+J / Alt+Enter / Shift+Enter 换行，Ctrl+Enter 提交

**结论：成立。**

- 代码：`src/ui/input/keys.rs:98-107`。判断条件是 `(ctrl && Char('j')) || (Enter && modifiers ∩ {ALT, SHIFT})`，命中后执行 `insert_str("\n")`、`end_history_nav()`、`return`。
  Row 7（`:111-114`）照旧把剩下的 Enter 全部提交，Ctrl+Enter 也在其中。
- 单测：
  - `keys::tests::row_7a_newline_keys_insert_and_never_submit`（`keys/tests.rs:489`）：三个键各跑一遍，验证不提交、不入队、composer 两行，再按 Enter 后 reader 收到 `"a\nb"`；
  - `row_7_enter_submits_trimmed_or_ignores_blank`（`:448`）：Ctrl+Enter 提交并入队。
- 实测：tmux 场景 28 的四组（`C-j`、`-H 0a`、`M-Enter`、`-H 1b 5b 31 33 3b 32 75`）全部 PASS（附录 A.2）。

**边界（没写进文档，但不算缺陷）**：因为用的是 `intersects`，**Ctrl+Shift+Enter、Ctrl+Alt+Enter 也会换行**。这两个组合只有在 CSI-u 终端，
或者 Windows 的 AltGr（= CTRL|ALT，推测）下才会出现，换行比提交安全。建议在 X-65 的「bare Enter and Ctrl+Enter submit」后面补半句
「any Enter carrying ALT or SHIFT inserts」。改不改都行。

### 1.2 分支在 composer 键梯里，不在 `Editor::on_key`

**结论：成立。** `editor.rs` 不在 diff 里。`row_7a_never_reaches_a_surface_field`（`keys/tests.rs:607`）钉了两层：
surface 打开时按 Ctrl+J，字段值不变、surface 不关（这一层走 Row 1）；直接对 `Field::handle_key` 喂三个键，值仍是 `"ab"`。
如果有人以后把换行挪进 `Editor::on_key`，第二层会失败（推测；按 `Field` 走 `Editor::on_key` 的结构推断，没有做这一项的变异）。

### 1.3 插入后清掉历史导航

**结论：调用加上了（`keys.rs:105`），但测试没钉住它，而且它本来就观察不到。**

- 变异 M1：删掉 `:105`，`ui::input` 全部 47 个测试照样通过。为了确认跑的确实是变异后的二进制，同一次构建里另加了一个金丝雀变异（去掉 SHIFT），
  结果只有 `row_7a_newline_keys_insert_and_never_submit` 失败（附录 A.1）。
- 为什么观察不到：Ctrl+J 之后草稿至少两行，`history_navigable` 为假（`composer.rs:218-220`），↑↓ 落到编辑集，而编辑集在 `keys.rs:118`
  本来就会调 `end_history_nav()`。要让草稿回到单行，也必须经过编辑集，或者经过 submit，而 submit 里的 `push_history` 同样会重置。
  所以 `hist_idx` 残留的那段窗口里，没有任何键能读到它。
- 失败场景：没有用户可见的失败。问题在于 `row_7a_is_not_eaten_and_ends_the_cycle_and_the_walk`（`:562`）的注释和名字说它钉住了「walk」，
  其实它断言的「↑ 是行内移动」不管有没有这一行都成立。
- 建议：保留这一行（跟 Go「任何编辑都结束导航」是同一个意思，零成本）。把测试注释改成如实描述：「the walk cannot resume: a two-row draft keeps ↑ for the cursor」，
  并去掉「ends the walk」这个说法。**不要为了钉它造一个人为场景**。这条建议是评审 1 §2.6 提的，当时高估了它的作用。

### 1.4 键梯表注释

**结论：已同步。** `keys.rs:1-9` 的模块注释把 newline 写进了顺序，也写了「为什么不进 Editor」「为什么 Ctrl+Enter 不绑」；`keys/tests.rs:440-447` 的旧注释（「modifiers are ignored」）已经改写。
措辞问题见 §4.1。

## 2. 两处收尾

### 2.1 删掉显式 `height`

**结论：判断成立，没有任何边界会变矮。只有「多行草稿里有一行折行」这一种情况会变高，而这正是要修的缺口。**

依据：`wrap_spans`（`composer.rs:45-69`）每遇到一个 `'\n'` 就切出一段，结尾再补一段。所以段数 = 逻辑行数 + 软折行数 ≥ 逻辑行数，
新旧两种算法用的又是同一个上限（`MAX_COMPOSER_ROWS` = 5，`event_loop.rs:98`）。

| 边界 | 旧高度 | 新高度 | 依据 |
|---|---|---|---|
| 空草稿 | 1 | 1（`wrap_spans("")` 只有一段） | `composer.rs:67` |
| 单行不折 | 1 | 1 | 旧代码在单逻辑行时本来就走折行 |
| 单行折行 | 折行数 | 折行数 | 同上，行为不变 |
| 只有 `"\n"` / `"a\n"` | 2 | 2 | `row_7a_newline_keys_insert_and_never_submit` 断言 `rows(80).len() == 2` |
| 多行且含长行 | 逻辑行数（长行要滚着看） | 折行数，上限 5 | `a_multi_line_draft_grows_by_its_wrapped_rows`（`composer.rs:653`），覆盖键入、`set_value`、fold-back 三条入口，外加上限 |
| ↑ 召回多行历史 | `set_value` 时按逻辑行数 | 按折行数 | `history_up` → `set_value`（`composer.rs:236`） |
| 大段粘贴 | — | — | 多行粘贴一律折叠成单行的 `[#N …]` 标签（`paste.rs:31-34`），单行粘贴原样插入后按折行算，新旧一致 |
| 窄化 resize | 多行草稿高度固定 | 跟着重新折行变高（≤5） | 单行长草稿一直就这么变，没有新的类别（推测：resize 路径对帧高变化是通用处理） |

- 变异 M3：只在多行时恢复按逻辑行数算高度，`a_multi_line_draft_grows_by_its_wrapped_rows` 在 `composer.rs:661` 失败（附录 A.1）。钉住了。
- 顺带：现在 `history_navigable` 里的 `self.line_count() <= 1`（`composer.rs:219`）已经被后半句的 `wrap_spans(..).len() <= 1` 蕴含，是冗余的。
  `line_count` 也只剩这一处在用。**不建议为此改动**，留着可读性更好。

### 2.2 队列行：「不是撑开帧，是 `» ab`」

**结论：对。评审 1 §2.5(b) 的推测（`\n` 下移一行、盖住分隔线）是错的，实现方的实测纠正了它。修法也够用。**

- 变异 M2：去掉 `frame.rs:189` 的 `replace`，`vt100_tests::a_queued_multi_line_draft_keeps_one_frame_row`（`vt100_tests.rs:2771`）失败，
  网格里那一行正是 `» ab · ↑ edit`（附录 A.1）。这个测试走的是 `start_loop` 的真实渲染路径，所以「ratatui buffer 丢掉控制字符」是生产路径上的行为，不是测试替身造成的。
- tmux 场景 28：`» q one ⏎ q two` 只占一行，`check_frame_intact` 的 4 项全部 PASS，按 ESC 后折回两行草稿（附录 A.2）。
- 修法只动显示：`queue_rows()` 和折回草稿时用的都是原文（`event_loop.rs:641-643`、`:682-689`）。宽度预算按替换后的字符串算（`⏎` 前后各一个空格，共 3 列），截断是对的。
- 残留（不建议现在改）：只替换了 `\n`。排队项里如果还有 `\t` 之类的控制字符，照样会被 ratatui 丢掉（推测）。不过打字时 Tab 是补全键，单行粘贴的内容又由 `paste::normalize` 处理，
  要构造出这种排队项得绕很远，跟本次改动无关。

## 3. 测试是不是假绿

### 3.1 单测覆盖

逐条对照评审 1 §5.1：

| §5.1 | 对应测试 | 是否真钉住 |
|---|---|---|
| 1 交出去的是 `"a\nb"` | `row_7a_newline_keys_insert_and_never_submit`：reader 收到的 `got.text` | ✓（走过 `submit` 的 trim 和 `make_input`） |
| 2 插在光标处 | `row_7a_inserts_at_the_cursor_and_submit_trims_the_edges` | ✓ |
| 3 不提交 | 同 1：reader 为空、队列为空 | ✓（金丝雀变异证明了） |
| 4 首尾 trim / 只有换行就不提交 | 同 2 | ✓ |
| 5 与粘贴标签共存 | `row_7a_newline_after_a_paste_tag_submits_both` | ✓ |
| 6 surface 里不插入 | `row_7a_never_reaches_a_surface_field` | ✓ |
| 7 三个键 + Ctrl+Enter；改写旧循环 | 同 1 + `row_7_enter_…`（`:448`） | ✓。旧的三修饰键循环改成了「换行组 + 提交组」两处显式断言，没有直接删数组 |
| 8 结束补全 / 历史导航 | `row_7a_is_not_eaten_…` | 补全 ✓；历史 ✗（§1.3，钉不住，也不需要钉） |
| 9 高度 | `a_multi_line_draft_grows_by_its_wrapped_rows` | ✓（M3） |
| 10 队列行的 vt100 网格 | `a_queued_multi_line_draft_keeps_one_frame_row` | ✓（M2） |

**Shift+Enter 在单测里是怎么构造的**：`mods(KeyCode::Enter, KeyModifiers::SHIFT)`，也就是直接造一个 `KeyEvent`。它证明的只是**路由**。
解码那一段由 tmux 场景用 `-H` 注入 `ESC[13;2u` 来证明。「真终端会不会发这串字节」，两者都证明不了。测试注释（`keys/tests.rs:483-487`）和场景头注释
（`28-composer-newline.sh:6-9`）都把这一点写明了，**没有冒充覆盖**。

漏掉的（都不值得补）：Ctrl+Shift+Enter / Ctrl+Alt+Enter（§1.1）；Ghostty 默认的 `ESC[27;2;13~` 被丢，这是 crossterm 的行为，不是 iota 的。

### 3.2 场景 28 断言了什么

每个键跑一轮 `newline_turn`（`28-composer-newline.sh:22-50`）：

- 发送之前：
  - 模型没有回显（`echo: $one` 计数为 0）；
  - `❯ $one` 只出现一次，就是草稿本身；
  - composer 正好两行，第一行带 `❯`，第二行是续行前缀，整块里只有一个 `❯`。
- 按 Enter 之后：
  - `❯ $one` 只出现一次；
  - 不存在 `❯ $two`，也就是第二行没有变成一条新消息；
  - 不存在 `echo: $two`，也就是第二行没有单独发一次；
  - `$two` 一共出现 2 次：用户块里一次，模型回显里一次；
  - composer 收回到一行。

队列那一段：`» q one ⏎ q two` 只占一行；`check_frame_intact` 通过；按 ESC 后两行折回草稿。

### 3.3 「送出去的文本确实是两行」是不是端到端

**结论：一半是。** mock 是真的 HTTP provider（`tests/ui_tmux/mock.rs`），回显的内容取自请求体里最后一个 `content`，JSON 转义会被还原（`mock.rs:497-522`）。
所以「模型看到了第二行」确实穿过了 REPL、会话和 provider 请求，**不只是停在按键处理层**。

但这些断言**分不出「换行」和「空格」**：假如提交路径把 `\n` 换成了空格，回显就会变成 `echo: lf line one lf line two` 一行，
`echo: $one` 是子串匹配，`count_all "$two"` 仍然是 2（用户块一行，回显一行），全部都会 PASS。实际防住这个回归的是单测 `got.text == "a\nb"`（只覆盖到 reader 为止）。

- 建议（可选，一行）：在 `newline_turn` 里加一条行尾锚定的断言，例如
  `check "$label: line one ends the echo's first row" "$(capall | grep -cE "echo: ${one}\$")" 1`。
  `capture-pane` 会去掉行尾空格，所以 `$` 能锚住。这样「到了 provider 的是两行」就是真正端到端的了。

### 3.4 不用 `hist_size`、改用两条判断

**结论：这个取舍对，比评审 1 §5.2 的写法更好。** 评审 1 建议用 `hist_size` 不变来证明「没有提交」。可在 inline 视口里，一旦屏幕满了，composer 长出第二行就会把一行转录顶进
scrollback，`history_size` 在没有提交的情况下也会 +1（`28-composer-newline.sh:31-33` 的注释；机制和 X-54 的 inline 视口一致，「误报过一次」这件事本文没有复现）。
现在换成了三条互相独立的判断：没有 `echo: $one`、`❯ $one` 只出现一次、composer 有两行。每一条单独就能拦住「Ctrl+J 被当成了提交」，
三条合起来，在 mock 回复慢、composer 很高这两种情况下也都不会误报。

## 4. 文档

### 4.1 X-65

- **编号**：不撞。各分支的 DIVERGENCES 尾号：`main` 到 X-57；`req-cancel` 有 X-64；`bot-mode-v1` 有 X-58…X-63；本分支 X-65（附录 A.5）。
  三者都插在 X-57 那一行后面，合并时会有一处文本冲突，按编号排好即可。
- **与代码一致的部分**：行号（row 7a）、三个键、`end_history_nav`、不放进 `Editor::on_key` 的理由、删掉 `height`/`set_height`、` ⏎ `、
  不做 Kitty 协议的理由（SIGKILL 残留、`panic = "abort"`），以及 Pinned-by 列出的 7 个测试名和场景，全部能在代码里对上。
  其中「reads `» ab` without the fix」由 M2 证实。
- **不准的地方**（建议改，几个词的事）：
  1. 「**Why not Ctrl+Enter:** without a keyboard protocol every terminal sends it as a bare CR, and Ghostty's default … is dropped」：
     Ghostty 不发 CR，它发的是 `ESC[27;5;13~`，所以「every terminal」不成立。建议改成「a legacy terminal sends it as a bare CR (it submits);
     Ghostty's default encoding of it is dropped inside crossterm (nothing happens)」。
  2. 同一行的「bare Enter and Ctrl+Enter submit」、`keys.rs:8`「a terminal sends it as a bare Enter」、`keys/tests.rs:445`
     「legacy encoding sends it as a bare CR」、`ui-architecture.md`「Ctrl+Enter … IS a bare Enter」「Shift+Enter … is a plain submitting Enter elsewhere」：
     在 Ghostty 默认配置下，这两个键是**死键**，不是提交。失败场景：维护者读了文档，以为 Ghostty 用户按 Shift+Enter 会误发消息，或者以为 Ctrl+Enter 在 Ghostty 里能提交。
     后果只是理解错，没有行为错误。
  3. Alt+Enter 那半句没提 Windows Terminal：它默认把 Alt+Enter 截去切全屏（评审 1 §2.2 表），建议补一句。
- **Go 列**：「Enter submits, whatever its modifiers (model.go updateKey); no key inserts a newline」——本机没有 Go 原版源码，本文**没有核对**，
  沿用的是旧测试注释的说法。另外「the port sized such a draft by its logical lines」说的是移植版，不是 Go，放在 Go 列里有点错位，可以挪到 iota 列的「Two follow-ons」里。
- **Ghostty 配方**：写进 X-65 之前只验证了一半（§结论 4、附录 A.3）。它现在只在 DIVERGENCES 里，用户看不到。

### 4.2 ui-architecture.md

**结论：够用。** 这条说明了键、位置、不进 Editor 的理由、Ctrl+Enter、不入栈，以及高度规则；队列那条也补了 ` ⏎ `。措辞问题同 §4.1 第 2 点。
另外它没写评审 1 §2.6 记的已知局限（↑ 召回多行历史后就困在行间移动，没法继续往前翻）。补一句「a recalled multi-line entry keeps ↑ for row movement」即可，不补也行。

### 4.3 CHANGELOG 和用户文档

diff 里都没有。项目的惯例是发版时单独写 changelog（`git log -- CHANGELOG.md`：`changelog: 0.5.1 — …`、`release: v0.5.2`），所以**不阻塞合并**。
发版时要做：`### Added` 写一条（Ctrl+J / Alt+Enter 换行；Ghostty 的一行配置），官网文档补一小节。官网目前没有讲按键的页面（`iota-website/docs/` 里查不到 Ctrl+J 或换行）。

## 5. 有没有引入新问题

**没有发现回归。** 全量 `cargo test`、clippy 和完整 tmux 套件（27 个场景）都是绿的。逐项看：

- **Ctrl+J 会不会被前面的行吃掉**：Row 2 只认 `c`/`d`（`keys.rs:40`），Row 3 只认 Esc，Row 4 只认 Tab，Row 5、6 只认 ↑↓。不会。
  `row_7a_is_not_eaten_…` 也钉了「运行中的 turn 按 Ctrl+J 不会中断」。
- **Ctrl+Enter 会不会误伤**：条件里没有 CONTROL（`keys.rs:102`），Ctrl+Enter 落到 Row 7 提交。CSI-u 下的 Ctrl+Shift+Enter 会换行（§1.1），属于安全的一侧。
- **Esc、Enter 粘连**：两个字节落进同一次 read 时会解成 `Enter + ALT`。以前的结果是「Esc 丢了 + 提交」，现在是「Esc 丢了 + 换行」，不会误发（评审 1 §2.2）。
- **非 bracketed 粘贴里的 LF 字节**：以前 Ctrl+J 是空操作，LF 被吞掉，两行会粘在一起；现在它是换行。这是改进。终端把粘贴里的换行发成 CR 的情况，跟以前一样（逐行提交）。
- **补全**：`match_suggestions` 遇到含 `'\n'` 的输入直接返回空（`suggest.rs:28`），多行草稿不会弹出候选，Tab 也不会把补全写进多行草稿。
- **斜杠命令加换行**：`/model⏎foo` 走 `match_cmd`（`commands/mod.rs:246-253`），它只认「完全相等」或「后面跟空格」，所以这一条**会作为普通消息发给模型**，
  跟 Go「未知的 `/xyz` 当普通消息」的规则一致。`/export path⏎x` 这种会把换行带进参数（推测：得到一个带换行的路径）。这种草稿以前通过 fold-back 就能构造出来，
  只是现在更容易打出来了。**不建议现在处理**，等有人反馈再说。
- **↑ 弹出队列**：草稿只有 `"\n"` 时算空白（`is_blank`），↑ 会弹出最新的排队项，把这个换行替换掉。丢掉的只是一个空白换行，无害。
- **历史**：召回一条多行历史以后，↑ 就困在行间移动（已知局限，§4.2），没有变得更糟。

## 6. 能不能合并

**能。** 剩下的事按优先级排：

| # | 事项 | 阻塞合并？ | 成本 |
|---|---|---|---|
| 1 | §4.1 第 1、2 点的措辞：Ghostty 下 Ctrl+Enter/Shift+Enter 是死键，不是提交（X-65、`keys.rs:8`、`keys/tests.rs:445`、ui-architecture） | 否，建议合并前顺手改 | 几行文档 |
| 2 | `row_7a_is_not_eaten_…` 的注释：不再声称钉住了「ends the walk」 | 否 | 一行注释 |
| 3 | Ghostty keybind 人工验证：在真 Ghostty 配置里加那一行，reload，在 iota 里按 Shift+Enter，看到换行 | 否；写进用户文档或 CHANGELOG **之前**必须做 | 30 秒 |
| 4 | 场景 28 加一条行尾锚定的断言（§3.3） | 否，可选 | 一行 |
| 5 | 发版时：CHANGELOG `### Added` + 官网一小节 | 否（项目惯例是发版时写） | — |
| 6 | 合并时 DIVERGENCES 的冲突，按 X-64 / X-65 排序 | 合并时处理 | — |

---

## 附录 A：实测

环境：macOS Darwin 25.6.0，tmux 3.7c，Ghostty 1.3.1，crossterm `=0.29.0`。

### A.1 变异测试

方法：用 `git archive HEAD` 导出到 scratchpad，改副本里的源码，`CARGO_TARGET_DIR` 指向工作树的 `target`。
注意：同名的 path 包共用一个 target 时，cargo 会误把对方的产物当成最新的。第一次全量 `cargo test` 因此跑到了变异版本，出现了一个假红。
已经用 `cargo clean -p iota` 清理并重建，下文 A.4 和全量结果都是重建之后的。M1 是在金丝雀变异的保护下重做的。

| 变异 | 结果 |
|---|---|
| M1 删 `keys.rs:105` 的 `end_history_nav()`，并加金丝雀（`intersects(ALT)`，去掉 SHIFT） | `ui::input::keys`：28 passed，1 failed，失败的只有 `row_7a_newline_keys_insert_and_never_submit`（金丝雀）。M1 存活 |
| M2 `frame.rs:189` 不替换 `\n` | `a_queued_multi_line_draft_keeps_one_frame_row` 在 `vt100_tests.rs:2785` 失败，网格那一行是 `» ab · ↑ edit` |
| M3 多行时按逻辑行数算高度 | `a_multi_line_draft_grows_by_its_wrapped_rows` 在 `composer.rs:661` 失败（`ui::` 下 274 passed，1 failed） |

### A.2 tmux 场景 28（`IOTA_TMUX=1 IOTA_TMUX_REQUIRED=1 cargo test --test ui_tmux tmux_composer_newline`）

```
PASS: Ctrl+J: nothing reached the model (0)
PASS: Ctrl+J: two composer rows (2)
PASS: Ctrl+J: echo row 2 is a continuation, not a second prompt (0)
PASS: Ctrl+J: the model saw line two (2)
…（LF byte / Alt+Enter / Shift+Enter (CSI u) 各 11 项，同上全部 PASS）
PASS: the queued item is ONE row (1)
PASS: with a two-line item queued: top separator spans the terminal (80)
PASS: with a two-line item queued: bottom separator spans the terminal (80)
PASS: with a two-line item queued: exactly one composer row between them (1)
PASS: with a two-line item queued: the bottom zone is occupied (row 24)
PASS: the queue row is gone (folded) (0)
PASS: fold row 1 (1)
PASS: fold row 2 (1)
---- 28-composer-newline.sh: PASS=52 FAIL=0 WARTS=0
```

### A.3 Ghostty 配方

```
$ ghostty +validate-config --config-file=ghostty.conf     # keybind = shift+enter=text:\n
exit=0
$ ghostty +validate-config --config-file=bad.conf         # keybind = shift+enter=texxt:\n（对照）
bad.conf:1:keybind: unknown error error.InvalidAction
```

用 AppleScript 开一个新窗口跑原始字节记录器（`tty.setraw` + `os.read`），依次发送：`perform action "text:\n"`、`input text "x"`、
`send key "enter" modifiers "shift"`（这个窗口没有配 keybind，作对照）、`input text "q"`：

```
start
b'\n'                 ← text:\n 这个 action 发的就是 LF，iota 读到的是 Ctrl+J
b'x'
b'\x1b[27;2;13~'      ← 没配 keybind 时的默认 Shift+Enter（与评审 1 A.1 一致）
b'q'
```

没验证的环节：Ghostty 把物理按键 Shift+Enter 匹配到 `keybind = shift+enter=…` 这一步。AppleScript 的 surface configuration 不能带 keybind，
而为了测试去改用户真实的 Ghostty 配置，会影响正在运行的会话，所以没做。

### A.4 全量

- `cargo clippy --all-targets -- -D warnings`：干净。
- `cargo test`：lib 1017 passed；各集成测试二进制都是 0 failed。
- 完整 tmux 套件（`IOTA_TMUX=1 IOTA_TMUX_REQUIRED=1 cargo test --test ui_tmux`，重建之后跑的）：27 passed，0 failed，用时 234 s。
  跟本次改动相关的几个：`07-paste` 15/15、`04-esc-midstream` 18/18、`06-resize` 100/100、`26-resize-residuals` 65/65、`10-edge-pins` 46/46、`28-composer-newline` 52/52。

### A.5 DIVERGENCES 编号

```
main:        … X-55 X-56 X-57
req-cancel:  … X-56 X-57 X-64
bot-mode-v1: … X-61 X-62 X-63      (X-58 起)
HEAD:        … X-56 X-57 X-65
```
