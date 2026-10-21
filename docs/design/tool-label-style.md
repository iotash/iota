# 工具调用标签 `[name …]` 的分段上色：清单与方案

Status: **Proposal**（未实现，等所有者评估）· 日期：2026-10-21 · 基于 `main` = `1a1ff9d`（v0.7.4）· 分支 `tool-label-style` · 同日修订一次：「单句话」并入 key-value，成为无键参数（§2.1–§2.5、§3）

所有者的原话：「我想优化一个显示方面的，也就是一系列工具调用 widget（可能不仅限于，需要你找出来），也就是用中括号 `[xxx xxx xxx...]` 渲染的这批，我希望显示的时候能有个高亮效果。首先括号的颜色和里面内容的颜色不一样，然后根据不同的渲染类型（比如：单句话，key value，多参数）适配不同的显示效果。我希望能把这个显示的函数进行统一，先列出所有需要改的地方，给我一个方案，然后我来评估后看能不能做。」

所有者对初稿的修正：「`[shell ls -la]`、`[shell …]` 这样的不能识别为单句话，因为 `shell` 是名字，后面是参数，这应该是 key-value 的一种特殊形式。」于是初稿里的 `LabelBody::Phrase` 取消了，`header_summary` 的输出改成**一个无键（positional）参数**。本文以下各节都按修订后的写。

## 写作约定

- 代码坐标都指 `1a1ff9d`。「chat 侧」「frame 侧」的分法沿用 DIVERGENCES X-27 / X-28。
- 「可见文本」指去掉 SGR 之后的字符（`text::ansi::strip_sgr`），也就是 tmux `capture-pane -p` 读到的、`NO_COLOR` 下打出来的东西。

## 0. 结论

- 工具调用标签**只在 2 个地方拼字符串、3 个地方上色**：`tool::fmt::tool_call_header`（及其 `tool_call_detail`）拼，`repl/turn/tools.rs` 的两处 `cyan(…)` 和 `repl/render/group.rs` 的 `composing_label` 上色。下游 8 个消费点（widget 标签行、`✓` 事件行、`✗` 失败行、单调用经典块、showcase 头……）全都把标签当**不透明字符串**原样拼接，不用改。
- 能统一：把「拼」改成产出结构化的 `ToolLabel`（名字 + 一列参数，参数的键可以为空；外加「生成中」这一种状态），把「上色」收成渲染层的一个 `paint_label`。**可见文本逐字节不变**，所以按纯文本钉的测试（约 35 处，含 7 个 tmux 场景）全部不动，真正要改的旧断言只有 1 条。
- 值得做，但要小做：改动约 5 个源文件 + 1 条测试 + 新增测试 + DIVERGENCES 一行（X-83）+ CHANGELOG。其余带方括号的地方（skills 来源标签、`/tools` 的 defer 状态、picker 行、后台任务通知、粘贴占位）**不建议**并进这次，理由见 §1.3。

## 1. 清单

### 1.1 工具调用标签本身（这次的范围）

| # | 位置 | 拼在哪 | 上色在哪 | 形状 | 宽度 / 截断 |
|---|---|---|---|---|---|
| 1 | `src/tool/fmt.rs:23` `tool_call_header` | 这里：`format!("[{name}]")` / `format!("[{name} {detail}]")` | 不上色（纯文本，`tests/repl/toolfmt.rs`、`tests/headless/approval.rs` 钉它） | 只有名字 / 无键参数 / 有键参数 / 多参数 | 名字经 `display_tool_name`（`:61`，`mcp__srv__tool` → `srv:tool`）不截；见 #2 |
| 2 | `src/tool/fmt.rs:38` `tool_call_detail` | 这里：有 `header_summary` 就整段接管（`Some("")` → 空 → 裸名）；否则键排序、每值 `value_one_line` + `truncate_runes(15)`、最多 3 个、超出 `"… +N args"`，用空格 `join` | 不上色 | 无键参数（summary）/ 有键参数 / 多参数 | **按 rune**，本模块不引宽度 crate（模块头那条已记录的分歧）。另一个调用方：审批的 `detail`（`repl/turn/tools.rs:246` → `approval.rs:49`），要保持纯文本 |
| 2a | `header_summary` 的提供者：`src/tool/builtins/shell.rs:289`，`src/tool/builtins/code/tools.rs:177,225,283,332`（`list_dir`/`read_file`/`edit_file`/`write_file`） | 各工具自己：shell = `header_command`（64 rune，首行 + ` …`）+ 可选 `cd <dir> && ` + 前缀 `(background) ` + 后缀 `(outside the sandbox)`；文件工具 = `header_path`（48 rune，从**头**省略成 `.../`） | 不上色 | 无键参数（一个） | 各自已截好；返回的是 `String`，结构在这一层就丢了 |
| 3 | `src/repl/turn/tools.rs:224` 串行调用 | 调 #1 | **整段 `cyan()`**（`styles.rs:33`，SGR 36） | 同 #1 | — |
| 4 | `src/repl/turn/tools.rs:355` 并行批 | 调 #1 | **整段 `cyan()`** | 同 #1 | — |
| 5 | `src/repl/render/group.rs:552` `composing_label`（`turn/mod.rs:948` 调，并拿它去重） | 这里：`format!("[{n} …]")`，无名时 `"tool"` | **整段 `cyan()`** | 只有名字 + 生成中 `…` | 不截。注释要求它「和最终落地的头长得一样」 |

下游消费点（**全部把 #3–#5 的结果当不透明字符串**，统一后自动跟着变，不用改代码）：

| 位置 | 渲染成什么 | 截断 |
|---|---|---|
| `src/ui/render/frame.rs:175` widget 标签行 | `{CYAN}⠋{RESET} {label}{RESET}` | **不截**（只 `region.rs:550` `one_row` 去换行） |
| `src/ui/render/frame.rs:186` widget 正文行（事件行滚过） | `{FAINT}  {line}{RESET}` | `truncate_ansi(line, width-4)`：按显示宽度、**跳过转义**、截点之后的转义照抄 |
| `src/repl/render/group.rs:231` `event_line` | `✓ [shell ls] · hi`（`✓` dim，snippet dim、48 rune） | 只截 snippet |
| `src/repl/render/group.rs:245` `fail_line` | `✗ [shell ls] · err`（红，64 rune） | 只截 snippet |
| `src/repl/render/group.rs:132` 单调用经典块 | 头 + dim ` · note` + `⎿` 结果行 | 结果行 120 rune |
| `src/repl/render/group.rs:409,421` showcase（文件改动） | 头 + dim `  +a -d` + diff | — |
| `src/repl/render/transcript.rs`（`pending_call` / `raise_pending`） | 推迟的标签原样升起 | — |

不带括号、但所有者点名要查的：

- **`image` 生成 widget**：`src/repl/render/transcript.rs:25` `IMAGE_WIDGET_LABEL = "image"`，`:359` 升起。**没有括号，也不上色**（Go transcript.go:373 的原样）。见 §3「拿不准」。
- **`/jobs`**：`src/repl/commands/jobs.rs:59` 行是 `b3  1m 12s  <command>`，无括号；状态行的任务段 `src/ui/render/frame.rs:390` 把命令画成 **cyan**（注释：「in the header's cyan (2026-09-23)」）。不改，但它是「命令这个值用 cyan」的现成先例。
- **headless**：没有任何 `[…]` 输出。`src/headless/run.rs:415` 只把 `header_summary` 当审批 `detail` 交给 approver。不改。
- **`/tools`**：工具行本身无括号（`src/repl/commands/tools.rs:101,131` 用 `display_tool_name`），带括号的是 defer 状态，见 §1.2。
- 审批弹窗标题 `src/repl/turn/approval.rs:49`：`{name} {detail} wants to modify files — allow?`，无括号、frame 侧 surface。不改。
- 压缩 widget（`transcript.rs:379`，`dim("Compacting context…")`）、暂停标签（`group.rs:496`，`⏸ waiting for approval`）、`Working…`：无括号。不改。
- `/compact` 的摘要输入（`commands/compact.rs:259`）、`/export` HTML（`commands/export.rs:544` `⚙ name`）：给模型 / 给 HTML，无括号。不改。

### 1.2 其他带方括号的地方（不是工具调用）

| 位置 | 内容 | 上色 | 建议 |
|---|---|---|---|
| `src/repl/commands/skills.rs:49` | `name  [project]` 来源标签 | 整段 `yellow()` | 可选第二期：复用括号画法（括号 dim、内容 yellow） |
| `src/repl/commands/tools.rs:267` | `  [deferred]` / `[loaded]` defer 状态 | `yellow()`；隐藏行整行 `dim()` | 同上，可选第二期 |
| `src/repl/catalog.rs:260` | `/model` picker 行 `label [a, b]` | 无（frame 侧 picker 项） | **不改**：picker 按文本过滤、搜索高亮在 frame 侧另上 SGR（`ui/surface/search`），chat 侧再塞转义会和它打架 |
| `src/repl/commands/session.rs:46` | 会话 picker 行 `… [project]` | 无 | **不改**，同上 |
| `src/shell/jobs.rs:661` `notice_headline` | `[background job b3 finished: exit 0] cmd` | 显示处 `tr.notice`（`repl/run.rs:872`）整行 dim | **不改**：这串同时是给模型的、注释写明「fixed shape the model can pattern-match」，`jobs.rs:704-778` 钉了 8 条 |
| `src/ui/input/paste.rs:42` `paste_tag` | `[#1 first… 12 lines]` | 无 | **不属于这次**：它是 composer **可编辑缓冲区里的文本**，光标、宽度、展开都按原文算；上色要动 composer 渲染，另一件事 |
| markdown 任务列表 `[ ]`/`[x]`、`ui/surface/panels.rs:133` 多选框 | 复选框 | green/faint | 不相干 |

## 2. 方案

### 2.1 统一成什么

**拼（数据，放 `src/tool/fmt.rs`）**——保持「不引宽度 crate、按 rune 截断」：

```rust
/// 一个调用标签的结构：名字 + 参数。所有截断都在这里做完，渲染层只上色、不再截。
pub struct ToolLabel {
    pub name: String,          // display_tool_name 之后
    pub body: LabelBody,
}

pub enum LabelBody {
    /// [name …]：参数还在流式生成，内容未知。
    Composing,
    /// [name a b k:v …]：键为 None 的是无键（positional）参数，只显示值；
    /// pairs 为空且 more == 0 就是裸 [name]。
    Args {
        pairs: Vec<(Option<String>, String)>,
        more: usize,           // > 0 → 尾巴 "… +N args"
    },
}

pub fn tool_label(dispatch: &dyn Dispatcher, tc: &ToolCall) -> ToolLabel;
pub fn composing_label(name: &str) -> ToolLabel;      // "" → "tool"
impl std::fmt::Display for ToolLabel { … }             // 逐字节等于今天的纯文本
```

`tool_label` 怎么填 `pairs`，就是今天 `tool_call_detail` 的分支，原样搬过来：

| 来源 | `pairs` | `more` | 值的截断 |
|---|---|---|---|
| `header_summary` 返回 `Some("")` | 空 | 0 | — |
| `header_summary` 返回 `Some(s)`，`s` 非空 | `[(None, s)]`，一个无键参数 | 0 | **不再截**：`s` 已按工具自己的规则截过（`header_command` 64 rune、`header_path` 48 rune）。**不能**套 15 rune，否则字形会变 |
| `header_summary` 返回 `None`，没有参数 | 空 | 0 | — |
| `header_summary` 返回 `None`，有参数 | 键排序后的前 3 个，每个是 `(Some(k), v)` | 剩下的个数 | `value_one_line` + `truncate_runes(15)`，同今天 |

`Display` 的规则：先 `[` 和名字；每个 pair 前加一个空格，有键写 `k:v`，无键只写 `v`；`more > 0` 时加 ` … +N args`；最后 `]`。`Composing` 写 `[name …]`。

- `tool_call_header` 变成 `tool_label(..).to_string()`，`tool_call_detail` 变成「body 的纯文本」（同一套规则，只是不带名字和括号）——**两者签名不变、输出不变**，钉它们的 10 条测试和审批 `detail` 都不动。
- 今天不会出现「无键和有键混在一个标签里」：summary 只给一个无键参数，摘要只给有键参数。模型允许混排，`Display` 和 `paint_label` 也按 pair 逐个处理，将来真有工具这么用也不用改。
- 无键参数只能来自数据本身，**绝不从字符串里反解析**：`[shell echo a:b]` 的纯文本和「键 `echo a`、值 `b`」长得一样，只有结构知道它是一个无键参数。

**`header_summary` 那条边界的结论**：`Dispatcher::header_summary` **不改签名**（仍返回 `String`），整串就是那个无键参数。shell 的 `(background) ` 前缀和 `(outside the sandbox)` 后缀**不单独成键**，留在值里、和命令同色。同意所有者的倾向，理由有四条：

1. 它们不是参数的键。对应的参数是 `background: true`，要成键就得显示成 `background:true` 这类字形，可见文本会变，违反「只改颜色」。
2. 要拆就只能把 summary 字符串拆回去。可前缀是插在命令前面的，命令本身也可能以同样的文字开头，拆不可靠。要做对，得让 trait 返回结构：涉及 trait、5 个实现、`testing/dispatch.rs` 的两个替身和 headless。
3. 同一串 summary 也是审批弹窗的 `detail`，shell.rs 的注释写明「the mark is written once」。它本来就是一句完整的话，不是几个字段。
4. 这两个标记出现得很少，为它们改 trait 不划算。

**上色（渲染，放 `src/repl/render/styles.rs`，或新开 `src/repl/render/label.rs`）**：

```rust
pub(crate) fn paint_label(l: &ToolLabel) -> String;
```

调用方的改法：

| 现在 | 改成 |
|---|---|
| `turn/tools.rs:224` `cyan(&tool_call_header(&*dispatch, tc))` | `paint_label(&tool_label(&*dispatch, tc))` |
| `turn/tools.rs:355` 同上（`map` 里） | 同上 |
| `render/group.rs:552` `composing_label(name) -> String` 里的 `cyan(format!(…))` | `paint_label(&fmt::composing_label(name))`（函数名留着，`turn/mod.rs:948` 不动） |

上色放渲染层而不放 `tool::fmt`，因为 `tool` 在 `repl` 之下（`src/tool` 不依赖 `crate::repl`），`styles` 又是 X-27 开关所在；`fmt` 继续只管「是什么」，和今天「fmt 出纯文本、调用方包 `cyan`」的分工一致。

### 2.2 配色（修订后重列）

现成的只有 16 色 SGR（`styles.rs`：dim 2、cyan 36、bold 1、yellow 33…），**不新增颜色**。

| 部件 | 样式 | 理由 |
|---|---|---|
| `[` `]` | **dim**（2） | 括号是框，不是内容，退到背景 |
| 名字（含 MCP 的 `srv:tool`，那个 `:` 是名字的一部分） | **bold cyan**（`1;36`，`styles` 里加一个 `sgr("1;36", …)` 的包装） | 保住今天「工具头是 cyan」的识别度；加粗把名字和后面的参数分开 |
| 无键参数的值 | **cyan**（36） | 它就是一个值，和有键参数的值同色。和状态行任务段的命令同色（`frame.rs:390`），同一条命令在哪儿都是一个颜色 |
| 键 + `:` | **dim** | 键是说明，值才是信息 |
| 有键参数的值 | **cyan**（36） | 值里带空格（`query:rust tui`）时，颜色把边界画了出来，这是这种形状最大的收益 |
| `… +N args` | **dim** | 省略信息 |
| 生成中的 `…` | **dim** | 同上 |
| 值内的省略记号：`truncate_runes` 的 `…`、`header_command` 多行时的 ` …`、`header_path` 的 `.../` | 跟值同色 | 都已经是值字符串的一部分（截断在上色前做完，见 §2.4） |
| 名字与参数、参数与参数之间的空格 | 不上色 | — |

「无键值跟值同色（cyan）」在修订后**仍然成立**，而且更顺了：初稿里它是单独的一类、只是碰巧也用 cyan；现在它本来就是值，规则只剩一条「值都是 cyan」。代价是无键参数前面没有 dim 的键，它和名字之间只靠 bold 和一个空格区分。见 §3。

`NO_COLOR`（及 `TERM=dumb`、stdout 非终端）：

- 所有片段都走 `styles` 的包装，它们在 `color::enabled()` 为假时**原样交回文本、属性也不加**（X-27 的规矩）。所以 `NO_COLOR` 下的输出**逐字节等于今天**——括号与内容的区分在无色模式下消失，和 chat 侧其它一切一样。
- 标签还会进 frame（widget 标签行、正文行），frame 侧的 X-28 是「丢颜色、留属性」；但 chat 侧已经什么转义都没发，frame 那道关没东西可剥，两条规矩不打架。
- **不建议**为了无色模式下也能区分括号而让 bold/dim 绕过开关：那会破 X-27（`tests/nocolor/main.rs` 逐字节扫 `\x1b[`）。

### 2.3 形状与具体样子（修订后重排）

记号：`‹d›` = dim，`‹c›` = cyan，`‹C›` = bold cyan，每段各自 `…\x1b[0m` 收尾；空格不上色。

| 形状 | 数据 | 例子（可见文本，**与今天相同**） | 分段 |
|---|---|---|---|
| 只有名字 | `Args { pairs: [], more: 0 }` | `[edit_file]`、`[ping]` | `‹d›[` `‹C›edit_file` `‹d›]` |
| 生成中 | `Composing` | `[shell …]` | `‹d›[` `‹C›shell` ␠ `‹d›…` `‹d›]` |
| 无键参数 | `[(None, v)]` | `[shell ls -la]`、`[read_file src/ui/model.rs]`、`[shell (background) make test]`、`[shell echo a:b]` | `‹d›[` `‹C›shell` ␠ `‹c›ls -la` `‹d›]` |
| 有键参数（≤3 个） | `[(Some(k), v), …]`，`more: 0` | `[glob pattern:**/*.rs]`、`[srv:search limit:10 query:rust tui]` | `‹d›[` `‹C›srv:search` ␠ `‹d›limit:` `‹c›10` ␠ `‹d›query:` `‹c›rust tui` `‹d›]` |
| 多参数（>3 个） | 有键 ×3，`more: N` | `[x a:1 b:2 c:3 … +2 args]` | 同上，尾巴 ␠ `‹d›… +2 args` |

- 「多参数」不是另一种 pair，只是有键参数加一条尾巴。所有者原话里的「多参数」例子 `[shell command:… +3 args]` 就是这种形状。
- `[shell echo a:b]` 这一行说明了为什么数据里一定要分有键和无键：纯文本看不出区别，颜色能看出来——`a:b` 整段是 cyan，没有 dim 的键。
- 截断规则**一条都不改**：有键参数的值 15 rune、最多 3 个；无键参数用 summary 自己的 64 / 48 rune。

### 2.4 截断与上色的顺序

1. **先截断、后上色**。`ToolLabel` 里存的全是截好的纯文本片段；`paint_label` 只做「片段 → `sgr(params, 片段)` → 拼接」，自己不量宽度、不截。所以 rune 截断**永远看不到转义**，不存在切断转义的可能。
2. 每个片段自带 `\x1b[0m` 收尾（`sgr` 现成就是这样），不依赖外层状态。现有包裹它的地方——`event_line` 的 dim `✓` / dim snippet、frame 正文行外层的 `{FAINT}…{RESET}`——今天就是「外层 FAINT 被标签末尾的 reset 关掉，snippet 再自己开 dim」，分段后行为相同（标签内部多几次 reset，但外层本来就在第一个 reset 处结束了）。
3. 上色之后**仍会被截**的只有 frame 正文行（`frame.rs:186` 的 `truncate_ansi`）：它按显示宽度走、遇转义原样照抄、截点之后的转义也照抄，所以切在标签中间也不会留下没关的颜色。widget 标签行（`frame.rs:175`）今天就不截，不变。
4. 不变式（建议写成单元测试）：`strip_sgr(&paint_label(&l)) == l.to_string()`，对 §2.3 的五种形状、CJK 值、空 summary、含 `:` 的无键值各跑一遍；以及 `color` 关时 `paint_label(&l) == l.to_string()`。

### 2.5 成本与风险

**屏幕上变不变**：**可见文本不变**。改成「无键参数」只是数据模型换了个说法，`Display` 规则下 `(None, "ls -la")` 输出的还是 ` ls -la`，`[shell ls -la]`、`[read_file src/ui/model.rs]`、`[shell (background) make test]`、`[shell …]`、`[edit_file]` 都逐字节和今天一样；审批 `detail` 也一样。变的只有颜色。所以这次仍然只是「颜色 + 模型」，不改字形。唯一会让字形变的坑是给无键值也套 15 rune 截断，§2.1 的表里已经排除了。

**要改的文件**（4 个源文件 + 2 份文档，修订没有增加）：`src/tool/fmt.rs`（`ToolLabel` + `Display` + 两个旧函数改成薄包装）、`src/repl/render/styles.rs`（`paint_label` + 一个 bold-cyan 包装 + `sgr_pins` 加一条）、`src/repl/turn/tools.rs`（2 处）、`src/repl/render/group.rs`（`composing_label`）；`docs/DIVERGENCES.md`（新 X-83：Go 的工具头是整段 `CodeStyle`，这里分段）、`CHANGELOG.md` Unreleased。修订后 `LabelBody` 从四个变体减到两个，`paint_label` 少一个分支，实现反而略小。

**会被影响的测试**（可见文本不变，所以分类改了也不影响测试改动数；绝大多数不动）：

| 测试 | 钉的字符串 | 是否要改 |
|---|---|---|
| `tests/repl/toolfmt.rs` | 6 条 `tool_call_header` 纯文本（其中 `[edit_file src/ui/model.rs]` 走无键参数） | 不改（函数输出不变） |
| `tests/headless/approval.rs:233-259` | 4 条（`[write_file a/b.rs]` 走无键参数） | 不改 |
| `src/repl/turn/tools/tests.rs:646,1032` | 2 条，经 `strip_sgr` | 不改 |
| `src/repl/turn/tools/tests.rs:562` | `raised[0].contains("[write_file …]")`，**没剥 SGR** | **要改 1 条**（剥 SGR 再比，或直接比 `paint_label` 的期望） |
| `src/repl/render/transcript/tests.rs`（85 行）、`src/ui/render/region/tests.rs`（15）、`frame/tests.rs`（4）、`testing/scripted.rs`（2） | 测试自己喂进去的字面标签（`"[shell]"` 等），不经过构造函数 | 不改 |
| tmux 场景 19、24、25、26、31、33、34（约 16 处，几乎全是 `[shell <命令>]`，即无键参数） | `capture-pane -p`（无 `-e`）读纯文本 | 不改；建议 L4 加一处 `-e` 断言括号与名字的 SGR 不同 |
| `tests/nocolor/main.rs` | 全程无转义 + 针 `"edit"` | 不改（无色下输出不变），它正好守住 §2.2 的 NO_COLOR 结论 |

合计：钉住工具标签可见文本的断言约 35 处，**改 1 处**，修订前后相同；新增约 4 条（五种形状分段、strip 不变式、无色恒等、含 `:` 的无键值不出 dim 键），比初稿多 1 条。

**性能**：每个调用多建一个小 `Vec` 和几次 `format!`，一回合几十次，可忽略。`composing_label` 去重比的是上色后的字符串，结果仍确定。

**兼容/视觉风险**：

- dim 在少数终端（部分 Windows conhost、某些主题）不生效或很淡：括号退化成正常色，最坏等于今天少了 cyan，不会更难读。
- 字节变长：每段多约 8 字节转义，标签进 scrollback 的字节数涨几倍，但 `chunk_overflow`、`ansi_width` 都按可见宽度算，无影响。
- Go 对齐：这是有意的分歧，需要 X-83 一行；`TUI_CONTRACTS §9`（`styles.rs` 头注释里引的「CodeStyle=36 (tool headers)」）的注释要跟着改一句。

**值不值得**：值得，前提是按本方案「只改颜色、不改字形」做——一次小 PR，真正的风险面只有 3 个上色点。收益主要落在有键参数（MCP 工具、`glob`/`grep`/`load_skill`/`remember` 走的都是这一形状）：值里有空格时，现在整段 cyan 糊成一片，分段后一眼能看出几个参数。无键参数（shell、文件工具，最常见的那批）收益小：只是括号变淡、名字加粗。

## 3. 不同意或拿不准的地方

- **所有者的修正我同意**：`shell` 是名字、后面是参数，把 summary 当成一个无键参数比初稿的「单句话」更准确，数据模型也更小。这次修订没有我不同意的地方。
- **「按类型适配」我理解为只换颜色，不换字形**。如果所有者想要的是字形也变（比如单个有键参数时省掉键名，写成 `[glob **/*.rs]`，也就是把有键改成无键），那是另一件事：会改可见文本，上面「约 35 处不改」就变成大面积改，tmux 场景也要跟着动，而且审批 `detail` 共用这串、要单独评估。建议先只做颜色。
- **无键参数和名字挨得太近**：`[shell ls -la]` 里 `shell` 是 bold cyan，`ls -la` 是 cyan，只差一个加粗。bold 在有些终端或主题上不明显，那样名字和命令就连成一片了——而这正是最常见的那批标签。备选：无键值（以至所有值）改用默认前景色，名字留 cyan。代价是和状态行任务段的 cyan 命令不再一致，有键参数里「dim 键 + 默认色值」的对比也弱一些。我仍倾向 bold cyan，但这一条要真机看一眼再定。
- **`image` widget 要不要变成 `[image]`**：它今天不带括号、不上色，是 Go 的原样。套进统一函数（空参数的 `Args`）会让它和工具调用长得一样，但它其实不是工具调用（专用图像 provider 的回合没有 tool call），而且改的是可见文本。建议不动，除非所有者觉得应该一致。
- **skills 来源标签与 `/tools` defer 状态**：形状是「只有一个标签」，能复用括号画法（括号 dim、内容保持 yellow），成本各 1 行。放不放这次都行；我建议放第二期，这次 PR 只管工具调用，免得 X-83 的范围说不清。
- **后台任务通知** `[background job … finished: …]`：所有者说的「可能不仅限于」最像指它，但这串是给模型 pattern-match 的固定格式，显示处又是整行 dim 的「次要信息」；分段上色只能在显示处（`tr.notice`）做，得先认出这个形状——我不建议。
