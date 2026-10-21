# 工具调用标签 `[name …]` 的分段上色：清单与方案

Status: **已实现**（2026-10-21，分支 `tool-label-style`，按 §2 的 (b)：`header_summary` 不动、`Phrase` 暂无产出者；DIVERGENCES X-83）· 日期：2026-10-21 · 基于 `main` = `1a1ff9d`（v0.7.4）· 分支 `tool-label-style` · 同日修订两次：一修「`header_summary` 的输出是位置参数，不是单句话」，二修「单句话仍是合法的一类，只是今天没有产出者」（§2.1–§2.5、§3 均按二修写）

所有者的原话：「我想优化一个显示方面的，也就是一系列工具调用 widget（可能不仅限于，需要你找出来），也就是用中括号 `[xxx xxx xxx...]` 渲染的这批，我希望显示的时候能有个高亮效果。首先括号的颜色和里面内容的颜色不一样，然后根据不同的渲染类型（比如：单句话，key value，多参数）适配不同的显示效果。我希望能把这个显示的函数进行统一，先列出所有需要改的地方，给我一个方案，然后我来评估后看能不能做。」

所有者对初稿的修正：「`[shell ls -la]`、`[shell …]` 这样的不能识别为单句话，因为 `shell` 是名字，后面是参数，这应该是 key-value 的一种特殊形式。」

所有者的第二次修正：「单句话不能说没有，只是我举的例子里没有——如果中括号里 `[...]` 就是没有特殊的、值得标记结构的一句话时，就适用单句话。」

两次合起来：`header_summary` 今天的输出是**位置参数**（key-value 的无键特例）；**单句话**（`Phrase`）作为一类保留，判据是「内容是一句话、没有值得标记的结构」。今天没有任何工具产出它（§2.1 核过）。

## 写作约定

- 代码坐标都指 `1a1ff9d`。「chat 侧」「frame 侧」的分法沿用 DIVERGENCES X-27 / X-28。
- 「可见文本」指去掉 SGR 之后的字符（`text::ansi::strip_sgr`），也就是 tmux `capture-pane -p` 读到的、`NO_COLOR` 下打出来的东西。

## 0. 结论

- 工具调用标签**只在 2 个地方拼字符串、3 个地方上色**：`tool::fmt::tool_call_header`（及其 `tool_call_detail`）拼，`repl/turn/tools.rs` 的两处 `cyan(…)` 和 `repl/render/group.rs` 的 `composing_label` 上色。下游 8 个消费点（widget 标签行、`✓` 事件行、`✗` 失败行、单调用经典块、showcase 头……）全都把标签当**不透明字符串**原样拼接，不用改。
- 能统一：把「拼」改成产出结构化的 `ToolLabel`（名字 + 四类形状之一：只有名字 / 位置参数 / 有键参数 / 单句话，外加「生成中」这一种状态），把「上色」收成渲染层的一个 `paint_label`。**可见文本逐字节不变**，所以按纯文本钉的测试（约 35 处，含 7 个 tmux 场景）全部不动，真正要改的旧断言只有 1 条。
- 值得做，但要小做：改动 4 个源文件 + 1 条测试 + 新增测试 + DIVERGENCES 一行（X-83）+ CHANGELOG。其余带方括号的地方（skills 来源标签、`/tools` 的 defer 状态、picker 行、后台任务通知、粘贴占位）**不建议**并进这次，理由见 §1.2。

## 1. 清单

### 1.1 工具调用标签本身（这次的范围）

| # | 位置 | 拼在哪 | 上色在哪 | 形状 | 宽度 / 截断 |
|---|---|---|---|---|---|
| 1 | `src/tool/fmt.rs:23` `tool_call_header` | 这里：`format!("[{name}]")` / `format!("[{name} {detail}]")` | 不上色（纯文本，`tests/repl/toolfmt.rs`、`tests/headless/approval.rs` 钉它） | 只有名字 / 位置参数 / 有键参数（含多参数） | 名字经 `display_tool_name`（`:61`，`mcp__srv__tool` → `srv:tool`）不截；见 #2 |
| 2 | `src/tool/fmt.rs:38` `tool_call_detail` | 这里：有 `header_summary` 就整段接管（`Some("")` → 空 → 裸名）；否则键排序、每值 `value_one_line` + `truncate_runes(15)`、最多 3 个、超出 `"… +N args"`，用空格 `join` | 不上色 | 位置参数（summary）/ 有键参数（含多参数） | **按 rune**，本模块不引宽度 crate（模块头那条已记录的分歧）。另一个调用方：审批的 `detail`（`repl/turn/tools.rs:246` → `approval.rs:49`），要保持纯文本 |
| 2a | `header_summary` 的提供者：`src/tool/builtins/shell.rs:289`，`src/tool/builtins/code/tools.rs:177,225,283,332`（`list_dir`/`read_file`/`edit_file`/`write_file`） | 各工具自己：shell = `header_command`（64 rune，首行 + ` …`）+ 可选 `cd <dir> && ` + 前缀 `(background) ` + 后缀 `(outside the sandbox)`；文件工具 = `header_path`（48 rune，从**头**省略成 `.../`） | 不上色 | 位置参数（一个值 + 可选标记）；今天没有单句话 | 各自已截好；返回的是 `String`，结构在这一层就丢了 |
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

#### 分类（所有者定）

| 形状 | 判据 | 例 | 今天的产出者 |
|---|---|---|---|
| **只有名字** | 没有参数 | `[edit_file]`、`[ping]` | `header_summary` 返回 `Some("")`；或者没有 summary 也没有参数 |
| **位置参数**（key-value 的无键特例） | 内容是这个工具的**一个输入值** | `[shell ls -la]`、`[read_file src/ui/model.rs]` | 5 个 `header_summary` 实现，全部 |
| **有键参数**（含多参数：超过 3 个时带尾巴 `… +N args`） | 有 `k:v` | `[glob pattern:**/*.rs]`、`[srv:search limit:10 query:rust tui]`、`[x a:1 b:2 c:3 … +2 args]` | 通用摘要（没有 summary 的工具：`glob`、`grep`、`load_skill`、`remember`、全部 MCP 工具） |
| **单句话** | 内容是一句话，**没有值得标记的结构** | （今天没有） | **无** |
| *生成中*（状态，不是形状） | 参数还在流式生成 | `[shell …]` | `composing_label` |

「生成中」单列成状态：它说的是参数还不知道，不是参数长什么样，流完以后会变成上面四类之一。

**今天有没有单句话：没有。** 核过 5 个实现，产出的都是一个位置值加可选标记：

- `shell.rs:289`：`header_command(cmd)`，可能写成 `cd <path> && <cmd>` / `cd <path>; <cmd>`；前缀 `(background) `；**后缀** ` (outside the sandbox)`（`shell.rs:304-307` 是 `push`，标记在命令**后面**，不是前面）。
- `code/tools.rs:177/225/283/332`（`list_dir`/`read_file`/`edit_file`/`write_file`）：`header_arg(args)`，就是那个路径。
- 默认实现（`tool/mod.rs:149,192`）返回 `None`；3 个转发（`dispatch.rs:146,245,333`）只转发；`testing/dispatch.rs:140,306` 的两个替身是测试用，返回什么都行，不算产出者。MCP 工具没有 summary。

#### 数据

```rust
/// 一个调用标签的结构：名字 + 形状。所有截断都在这里做完，渲染层只上色、不再截。
pub struct ToolLabel {
    pub name: String,          // display_tool_name 之后
    pub body: LabelBody,
}

pub enum LabelBody {
    /// [name …]：参数还在流式生成。
    Composing,
    /// [name <一句话>]：没有值得标记的结构，整句一个颜色。今天没有产出者。
    Phrase(String),
    /// [name v]、[name k:v k:v]、[name k:v … +N args]：键为 None 是位置参数；
    /// pairs 为空且 more == 0 就是裸 [name]（只有名字）。
    Args {
        pairs: Vec<(Option<String>, String)>,
        more: usize,
    },
}

pub fn tool_label(dispatch: &dyn Dispatcher, tc: &ToolCall) -> ToolLabel;
pub fn composing_label(name: &str) -> ToolLabel;      // "" → "tool"
impl std::fmt::Display for ToolLabel { … }             // 逐字节等于今天的纯文本
```

`tool_label` 怎么填（就是今天 `tool_call_detail` 的分支，原样搬过来）：

| 来源 | 结果 | 值的截断 |
|---|---|---|
| `header_summary` → `Some("")` | `Args { pairs: [], more: 0 }` | — |
| `header_summary` → `Some(s)`，`s` 非空 | `Args { pairs: [(None, s)], more: 0 }`，位置参数（方案 (b)，见下） | **不再截**：`s` 已按工具自己的规则截过（64 / 48 rune）。不能套 15 rune，否则字形变 |
| `header_summary` → `None`，没有参数 | `Args { pairs: [], more: 0 }` | — |
| `header_summary` → `None`，有参数 | 键排序后的前 3 个 `(Some(k), v)`，`more` = 剩下的个数 | `value_one_line` + `truncate_runes(15)`，同今天 |
| （将来）工具声明自己是单句话 | `Phrase(s)` | 工具自己截，同位置参数 |

`Display` 的规则：`[` + 名字；`Phrase(s)` 写 ` s`；`Args` 的每个 pair 写一个空格，再写 `k:v`（有键）或 `v`（无键）；`more > 0` 写 ` … +N args`；`Composing` 写 ` …`；最后 `]`。`tool_call_header` = `tool_label(..).to_string()`，`tool_call_detail` = 同一套规则去掉名字和括号——**两者签名、输出都不变**。

- 位置参数和单句话只从数据里来，**绝不从字符串反解析**：`[shell echo a:b]` 的纯文本和「键 `echo a`、值 `b`」长得一样。
- 标记（`(background) `、` (outside the sandbox)`）留在位置值里，不单独成键。理由同上一版：它们不是参数键，成键就得改字形；拆字符串不可靠；同一串还是审批弹窗的 `detail`（shell.rs 注释：「the mark is written once」）。

#### 单句话将来怎么产出：三条路

- **(a) 改返回类型**：`header_summary` 改成返回 `Option<Summary>`，`Summary::{Arg(String), Phrase(String)}`。每次调用都能各自决定形状。改动面：
  - 源码 12 处：`Tool` 和 `Dispatcher` 的默认实现（`tool/mod.rs:149,192`）、3 个转发（`dispatch.rs:146,245,333`）、5 个实现、`testing/dispatch.rs:140,306` 的两个替身、`headless/run.rs:417`（`.unwrap_or_default()` 改成取文本）、`fmt.rs` 的 `tool_call_detail`。
  - 测试约 20 处：`tests/tool/{framework,code,shell}.rs` 的 13 处 `header_summary(..).as_deref()` 比较，加上 `HeaderDispatch { summary: Some(String) }` 的构造（`tests/repl/toolfmt.rs`、`tests/headless/approval.rs`，约 6 处）。
  - 可见文本不变；**今天的 5 个实现会全部返回 `Arg`**，`Phrase` 仍然没人用。
- **(b) 不动签名（YAGNI）**：今天所有 summary 一律当位置参数。`Phrase` 先放进模型和渲染器（一个变体、`paint_label` 一个分支、一条测试），**等有工具需要时再加信号**。分类表里「单句话」暂时是空集，但它是合法的一类，渲染路径已经备好、测过。额外改动：0 个 trait、0 个实现。
- **(c) 加一个附加的形状声明**：不改 `header_summary`，在 `Tool` 上新增一个带默认值的方法 `fn header_shape(&self) -> HeaderShape { HeaderShape::Arg }`，`Dispatcher` 按名字转发（默认实现 + 3 个转发），`tool_label` 读它：`Phrase` → `LabelBody::Phrase(s)`。改动面：源码约 6 处，**一个已有实现、一条已有测试都不用碰**；新工具只需覆写一行。局限是形状**按工具**定，不能按每次调用定。

**推荐 (b)，并把 (c) 写定为将来的入口。** 理由：

1. 今天没有任何产出者。(a) 要改约 32 处，换来的只是 5 个实现全部改写成 `Arg(…)`，屏幕上一个字节都不变。这是为假设的需求付真实的成本。
2. (b) 不是丢掉这一类：`Phrase` 在数据模型、`Display`、`paint_label` 里都有，单元测试直接构造它来测。缺的只是「谁来产出」这个信号，而这个信号最好等第一个真实的工具出现再定。
3. 等它出现，(c) 是纯增量（约 6 处、不碰现有实现），够用的前提是「一个工具的标签总是同一种形状」。今天 5 个实现都满足（shell 永远是命令，文件工具永远是路径），可以合理预期新工具也满足。只有出现「同一个工具有时是参数、有时是一句话」的情况才需要 (a)，到那时 (a) 的成本和今天一样，没有因为推迟而变贵。

#### 上色

```rust
pub(crate) fn paint_label(l: &ToolLabel) -> String;   // src/repl/render/styles.rs（或新开 label.rs）
```

| 现在 | 改成 |
|---|---|
| `turn/tools.rs:224` `cyan(&tool_call_header(&*dispatch, tc))` | `paint_label(&tool_label(&*dispatch, tc))` |
| `turn/tools.rs:355` 同上（`map` 里） | 同上 |
| `render/group.rs:552` `composing_label` 里的 `cyan(format!(…))` | `paint_label(&fmt::composing_label(name))`（函数名留着，`turn/mod.rs:948` 不动） |

上色放渲染层：`tool` 在 `repl` 之下，`styles` 是 X-27 的开关所在；`fmt` 只管标签「是什么」，和今天「fmt 出纯文本、调用方包 `cyan`」的分工一致。

### 2.2 配色（第二次重列）

只用现成的 16 色 SGR（dim 2、cyan 36、bold 1），**不新增颜色**。

| 部件 | 样式 | 出现在 |
|---|---|---|
| `[` `]` | **dim** | 全部 |
| 名字（含 MCP 的 `srv:tool`，其中 `:` 属于名字） | **bold cyan**（`1;36`，`styles` 加一个包装） | 全部 |
| 位置参数的值 | **cyan** | 位置参数 |
| 单句话 | **整句 cyan，不拆键** | 单句话 |
| 键 + `:` | **dim** | 有键参数 |
| 有键参数的值 | **cyan** | 有键参数 |
| `… +N args` | **dim** | 有键参数（多参数） |
| 生成中的 `…` | **dim** | 生成中 |
| 值或句子里的省略记号（`truncate_runes` 的 `…`、多行命令的 ` …`、路径的 `.../`） | 跟所在的值/句子同色 | 位置参数 / 有键参数 / 单句话 |
| 空格 | 不上色 | — |

和上一版比只多了「单句话」一行，其余没变。注意：**按这张表，单句话和位置参数画出来完全一样**（都是整段 cyan，前面没有键）。见 §3。

`NO_COLOR`（及 `TERM=dumb`、stdout 非终端）：结论不变。所有片段都走 `styles` 的包装，无色时原样交回文本、不加属性（X-27），输出**逐字节等于今天**；frame 侧 X-28 的「丢颜色、留属性」没东西可剥。不让 bold/dim 绕过开关（`tests/nocolor/main.rs` 逐字节扫 `\x1b[`）。

### 2.3 四类形状 + 生成中的具体样子

记号：`‹d›` = dim，`‹c›` = cyan，`‹C›` = bold cyan，每段各自 `\x1b[0m` 收尾；␠ = 不上色的空格。

| 形状 | 数据 | 例子（可见文本，**与今天相同**） | 分段 |
|---|---|---|---|
| 只有名字 | `Args { [], 0 }` | `[edit_file]` | `‹d›[` `‹C›edit_file` `‹d›]` |
| 生成中 | `Composing` | `[shell …]` | `‹d›[` `‹C›shell` ␠ `‹d›…` `‹d›]` |
| 位置参数 | `Args { [(None, v)], 0 }` | `[shell ls -la]`、`[read_file src/ui/model.rs]`、`[shell (background) make test]`、`[shell iota mcp list (outside the sandbox)]`、`[shell echo a:b]` | `‹d›[` `‹C›shell` ␠ `‹c›ls -la` `‹d›]` |
| 有键参数 | `Args { [(Some(k), v), …], more }` | `[srv:search limit:10 query:rust tui]`；多参数 `[x a:1 b:2 c:3 … +2 args]` | `‹d›[` `‹C›srv:search` ␠ `‹d›limit:` `‹c›10` ␠ `‹d›query:` `‹c›rust tui` `‹d›]`；多参数再加 ␠ `‹d›… +2 args` |
| 单句话 | `Phrase(s)` | （今天没有；假想例 `[delegate summarize the failing tests]`） | `‹d›[` `‹C›delegate` ␠ `‹c›summarize the failing tests` `‹d›]` |

截断规则一条不改：有键值 15 rune、最多 3 个；位置值（以及将来的单句话）由工具自己截。

### 2.4 截断与上色的顺序

1. **先截断、后上色**。`ToolLabel` 里存的全是截好的纯文本；`paint_label` 只做「片段 → `sgr(params, 片段)` → 拼接」，不量宽度、不截，所以 rune 截断永远碰不到转义。
2. 每段自带 `\x1b[0m` 收尾，不依赖外层状态。外面包着它的 `event_line` 的 dim `✓`/snippet、frame 正文行的 `{FAINT}…{RESET}`，今天就是「外层 FAINT 被标签末尾的 reset 关掉，snippet 自己再开 dim」，分段后行为相同。
3. 上色后**还会被截**的只有 frame 正文行（`frame.rs:186` 的 `truncate_ansi`）：按显示宽度走、遇到转义原样照抄、截点之后的转义也照抄，切在标签中间不会留下没关的颜色。widget 标签行（`frame.rs:175`）今天就不截。
4. 不变式测试：`strip_sgr(&paint_label(&l)) == l.to_string()`，对 §2.3 的五行（含直接构造的 `Phrase`）、CJK 值、空 summary、含 `:` 的位置值各跑一遍；`color` 关时 `paint_label(&l) == l.to_string()`。

### 2.5 成本与风险

**屏幕上变不变**：**可见文本不变。** 今天只有位置参数（单句话没有产出者），所以位置参数和单句话之间**今天没有任何可见差异**。即使将来有了单句话，按 §2.2 的配色两者颜色也一样，只是数据上分开。全部 `[shell …]`、`[read_file …]`、`[edit_file]`、`[glob pattern:…]` 和审批 `detail` 都逐字节等于今天。这次仍然只是「颜色 + 模型」，不改字形。

**两种做法的改动面**：

| | (b) 推荐 | (a) 改返回类型 | (c) 将来加形状声明（增量，不在这次） |
|---|---|---|---|
| 源文件 | 4 个：`tool/fmt.rs`、`repl/render/styles.rs`、`repl/turn/tools.rs`（2 处）、`repl/render/group.rs` | (b) 的 4 个 + 再 12 处：`tool/mod.rs`（2）、`tool/dispatch.rs`（3）、`builtins/shell.rs`、`builtins/code/tools.rs`（4）、`testing/dispatch.rs`（2）、`headless/run.rs` | `tool/mod.rs`（2）、`tool/dispatch.rs`（3）、`tool/fmt.rs` 读它，约 6 处 |
| 已有测试要改 | **1 条**（`src/repl/turn/tools/tests.rs:562`，没剥 SGR 的 `contains`） | 1 + 约 20（13 处 `header_summary(..).as_deref()`、约 6 处 `HeaderDispatch` 构造） | 0 |
| 新增测试 | 约 5 条（五行分段、strip 不变式、无色恒等、含 `:` 的位置值不出 dim 键、`Phrase` 整句一色） | 同左 | 1 条（覆写后得到 `Phrase`） |
| 文档 | DIVERGENCES X-83、CHANGELOG | 同左 | 改 X-83 一句 |
| 可见文本 | 不变 | 不变 | 不变 |

(b) 下按纯文本钉标签的断言约 35 处，仍然只改 1 处（明细见上一版，分类变化不影响：它们钉的是纯文本）。

**性能**：每个调用多建一个小 `Vec` 和几次 `format!`，可忽略。

**兼容/视觉风险**：dim 在少数终端不生效，括号退化成正常色，不会更难读；每段多约 8 字节转义，`chunk_overflow`、`ansi_width` 都按可见宽度算，不受影响。和 Go 的分歧要记一行 X-83；`styles.rs` 头注释「CodeStyle=36 (tool headers)」要跟着改一句。

**值不值得**：值得，前提是只改颜色、不改字形，并且用 (b)。一次小 PR，风险面只有 3 个上色点。收益主要落在有键参数（MCP 工具，以及 `glob`/`grep` 这类）：值里有空格时，今天整段 cyan 糊成一片。位置参数（shell、文件工具，最常见的那批）收益小：只是括号变淡、名字加粗。

## 3. 不同意或拿不准的地方

- **所有者的核对有一处小出入**：`(outside the sandbox)` 是**后缀**（`<cmd> (outside the sandbox)`，`shell.rs:304-307` 是 `push`），不是前缀；`(background) ` 才是前缀。不影响结论：两个标记都留在那个位置值里。
- **「四类」与表里的五行**：我把「生成中」算作状态，不算形状。它说的是参数还不知道，流完就变成另外四类之一。「多参数」并进了「有键参数」：它只是有键参数加一条尾巴。所以是四类形状 + 一个状态，与所有者的表一致，只是归法写明了。
- **按定下的配色，单句话和位置参数看起来一模一样**：都是名字后面跟一段 cyan，没有键。所以这一类今天在模型里有意义，在屏幕上没有。要让分类在屏幕上可见，得给其中一个换样式，比如单句话用默认前景色、不上 cyan，表示「这是描述，不是输入值」。我没有这么写，因为所有者明确要整句 cyan；建议等第一个真正的单句话出现、在真终端上对比过再定。
- **位置值和名字挨得太近**（上一版提过，仍然成立）：`[shell ls -la]` 里名字和命令只差一个 bold。bold 不明显的终端上会连成一片，而这是最常见的那批标签。备选：值改用默认前景色。我仍倾向 bold cyan，要在真终端上看一眼再定。
- **(c) 的局限**：形状按工具定，不按每次调用定。如果将来有工具需要「这次是参数、下次是一句话」，就得走 (a)。届时 (a) 的成本和今天一样，推迟不会让它变贵。
- **「按类型适配」我理解为只换颜色，不换字形**（所有者尚未否决）。如果要改字形（例如单个有键参数省掉键名），可见文本会变，约 35 处断言和 tmux 场景都要大面积改，审批 `detail` 也要单独评估。
- 其余照旧：`image` 不改成 `[image]`；skills 来源标签和 `/tools` 的 defer 状态放第二期；后台任务完成通知不碰。
