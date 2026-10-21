# 工具调用标签 `[name …]` 的分段上色：清单与方案

Status: **Proposal**（未实现，等所有者评估）· 日期：2026-10-21 · 基于 `main` = `1a1ff9d`（v0.7.4）· 分支 `tool-label-style`

所有者的原话：「我想优化一个显示方面的，也就是一系列工具调用 widget（可能不仅限于，需要你找出来），也就是用中括号 `[xxx xxx xxx...]` 渲染的这批，我希望显示的时候能有个高亮效果。首先括号的颜色和里面内容的颜色不一样，然后根据不同的渲染类型（比如：单句话，key value，多参数）适配不同的显示效果。我希望能把这个显示的函数进行统一，先列出所有需要改的地方，给我一个方案，然后我来评估后看能不能做。」

## 写作约定

- 代码坐标都指 `1a1ff9d`。「chat 侧」「frame 侧」的分法沿用 DIVERGENCES X-27 / X-28。
- 「可见文本」指去掉 SGR 之后的字符（`text::ansi::strip_sgr`），也就是 tmux `capture-pane -p` 读到的、`NO_COLOR` 下打出来的东西。

## 0. 结论

- 工具调用标签**只在 2 个地方拼字符串、3 个地方上色**：`tool::fmt::tool_call_header`（及其 `tool_call_detail`）拼，`repl/turn/tools.rs` 的两处 `cyan(…)` 和 `repl/render/group.rs` 的 `composing_label` 上色。下游 8 个消费点（widget 标签行、`✓` 事件行、`✗` 失败行、单调用经典块、showcase 头……）全都把标签当**不透明字符串**原样拼接，不用改。
- 能统一：把「拼」改成产出结构化的 `ToolLabel`（名字 + 三种形状之一），把「上色」收成渲染层的一个 `paint_label`。**可见文本逐字节不变**，所以按纯文本钉的测试（约 35 处，含 7 个 tmux 场景）全部不动，真正要改的旧断言只有 1 条。
- 值得做，但要小做：改动约 5 个源文件 + 1 条测试 + 新增测试 + DIVERGENCES 一行（X-83）+ CHANGELOG。其余带方括号的地方（skills 来源标签、`/tools` 的 defer 状态、picker 行、后台任务通知、粘贴占位）**不建议**并进这次，理由见 §1.3。

## 1. 清单

### 1.1 工具调用标签本身（这次的范围）

| # | 位置 | 拼在哪 | 上色在哪 | 形状 | 宽度 / 截断 |
|---|---|---|---|---|---|
| 1 | `src/tool/fmt.rs:23` `tool_call_header` | 这里：`format!("[{name}]")` / `format!("[{name} {detail}]")` | 不上色（纯文本，`tests/repl/toolfmt.rs`、`tests/headless/approval.rs` 钉它） | 只有名字 / 单句话 / key-value / 多参数 | 名字经 `display_tool_name`（`:61`，`mcp__srv__tool` → `srv:tool`）不截；见 #2 |
| 2 | `src/tool/fmt.rs:38` `tool_call_detail` | 这里：有 `header_summary` 就整段接管（`Some("")` → 空 → 裸名）；否则键排序、每值 `value_one_line` + `truncate_runes(15)`、最多 3 个、超出 `"… +N args"`，用空格 `join` | 不上色 | 单句话（summary）/ key-value / 多参数 | **按 rune**，本模块不引宽度 crate（模块头那条已记录的分歧）。另一个调用方：审批的 `detail`（`repl/turn/tools.rs:246` → `approval.rs:49`），要保持纯文本 |
| 2a | `header_summary` 的提供者：`src/tool/builtins/shell.rs:289`，`src/tool/builtins/code/tools.rs:177,225,283,332`（`list_dir`/`read_file`/`edit_file`/`write_file`） | 各工具自己：shell = `header_command`（64 rune，首行 + ` …`）+ 可选 `cd <dir> && ` + 前缀 `(background) ` + 后缀 `(outside the sandbox)`；文件工具 = `header_path`（48 rune，从**头**省略成 `.../`） | 不上色 | 单句话 | 各自已截好；返回的是 `String`，结构在这一层就丢了 |
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
- **`/jobs`**：`src/repl/commands/jobs.rs:59` 行是 `b3  1m 12s  <command>`，无括号；状态行的任务段 `src/ui/render/frame.rs:390` 把命令画成 **cyan**（注释：「in the header's cyan (2026-09-23)」）。不改，但它是「单句话值用 cyan」的现成先例。
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
/// 一个调用标签的结构：名字 + 形状。所有截断都在这里做完，渲染层只上色、不再截。
pub struct ToolLabel {
    pub name: String,          // display_tool_name 之后
    pub body: LabelBody,
}

pub enum LabelBody {
    Bare,                                   // [name]
    Composing,                              // [name …]
    Phrase(String),                         // [name <header_summary>]
    Args { pairs: Vec<(String, String)>,    // [name k:v k:v]，值已一行化 + 15 rune
           more: usize },                   // >0 → 尾巴 "… +N args"
}

pub fn tool_label(dispatch: &dyn Dispatcher, tc: &ToolCall) -> ToolLabel;
pub fn composing_label(name: &str) -> ToolLabel;      // "" → "tool"
impl std::fmt::Display for ToolLabel { … }             // 逐字节等于今天的纯文本
```

- `tool_call_header` 变成 `tool_label(..).to_string()`，`tool_call_detail` 变成 body 的纯文本——**两者签名不变、输出不变**，钉它们的 10 条测试和审批 `detail` 都不动。
- `Dispatcher::header_summary` **不改签名**（仍返回 `String`），所以 shell 的 `(background) ` / `(outside the sandbox)` 在第一期里是 Phrase 的一部分、和命令同色。要单独弱化它们就得让 summary 返回结构——涉及 trait、5 个实现、`testing/dispatch.rs` 两个替身和 headless，**不建议**为两个小标记做。

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

### 2.2 配色

现成的只有 16 色 SGR（`styles.rs`：dim 2、cyan 36、bold 1、yellow 33…），**不新增颜色**。建议：

| 部件 | 样式 | 理由 |
|---|---|---|
| `[` `]` | **dim**（2） | 括号是框，不是内容，退到背景 |
| 名字 | **bold cyan**（`1;36`，`styles` 里加一个 `sgr("1;36", …)` 的包装） | 保住今天「工具头是 cyan」的识别度，加粗把名字从参数里拎出来 |
| 单句话（Phrase） | **cyan**（36） | 与状态行任务段的命令同色（`frame.rs:390` 的先例），「你跑的那条命令」处处一个颜色 |
| 键 + `:` | **dim** | 键是说明，值才是信息 |
| 值 | **cyan** | 同单句话；值里带空格（`command:git status`）时，颜色本身就把边界画出来了——这是 key-value 形状最大的收益 |
| `… +N args` | **dim** | 省略信息 |
| 生成中的 `…` | **dim** | 同上 |
| 值内截断的 `…` | 跟值同色 | 它是值的一部分（截断在上色前做完，见 §2.4） |

`NO_COLOR`（及 `TERM=dumb`、stdout 非终端）：

- 所有片段都走 `styles` 的包装，它们在 `color::enabled()` 为假时**原样交回文本、属性也不加**（X-27 的规矩）。所以 `NO_COLOR` 下的输出**逐字节等于今天**——括号与内容的区分在无色模式下消失，和 chat 侧其它一切一样。
- 标签还会进 frame（widget 标签行、正文行），frame 侧的 X-28 是「丢颜色、留属性」；但 chat 侧已经什么转义都没发，frame 那道关没东西可剥，两条规矩不打架。
- **不建议**为了无色模式下也能区分括号而让 bold/dim 绕过开关：那会破 X-27（`tests/nocolor/main.rs` 逐字节扫 `\x1b[`）。

### 2.3 三种形状的具体样子

记号：`‹d›` = dim，`‹c›` = cyan，`‹C›` = bold cyan，每段各自 `…\x1b[0m` 收尾。

| 形状 | 例子（可见文本） | 分段 |
|---|---|---|
| 只有名字 | `[edit_file]` | `‹d›[` `‹C›edit_file` `‹d›]` |
| 生成中 | `[shell …]` | `‹d›[` `‹C›shell` ` ` `‹d›…` `‹d›]` |
| 单句话 | `[shell ls -la]`、`[read_file src/ui/model.rs]`、`[shell (background) make test]` | `‹d›[` `‹C›shell` ` ` `‹c›ls -la` `‹d›]` |
| key-value（≤3 个） | `[glob pattern:**/*.rs]`、`[srv:search query:rust tui limit:10]` | `‹d›[` `‹C›srv:search` ` ` `‹d›query:` `‹c›rust tui` ` ` `‹d›limit:` `‹c›10` `‹d›]` |
| 多参数（>3 个） | `[x a:1 b:2 c:3 … +2 args]` | 同上，尾巴 ` ` `‹d›… +2 args` |

- MCP 名字 `srv:tool` 里的 `:` 跟名字同色（它是名字的一部分，不是键值分隔），不拆。
- 截断规则**一条都不改**：值 15 rune、最多 3 个、summary 自己的 64 / 48 rune。

### 2.4 截断与上色的顺序

1. **先截断、后上色**。`ToolLabel` 里存的全是截好的纯文本片段；`paint_label` 只做「片段 → `sgr(params, 片段)` → 拼接」，自己不量宽度、不截。所以 rune 截断**永远看不到转义**，不存在切断转义的可能。
2. 每个片段自带 `\x1b[0m` 收尾（`sgr` 现成就是这样），不依赖外层状态。现有包裹它的地方——`event_line` 的 dim `✓` / dim snippet、frame 正文行外层的 `{FAINT}…{RESET}`——今天就是「外层 FAINT 被标签末尾的 reset 关掉，snippet 再自己开 dim」，分段后行为相同（标签内部多几次 reset，但外层本来就在第一个 reset 处结束了）。
3. 上色之后**仍会被截**的只有 frame 正文行（`frame.rs:186` 的 `truncate_ansi`）：它按显示宽度走、遇转义原样照抄、截点之后的转义也照抄，所以切在标签中间也不会留下没关的颜色。widget 标签行（`frame.rs:175`）今天就不截，不变。
4. 不变式（建议写成单元测试）：`strip_sgr(&paint_label(&l)) == l.to_string()`，对四种形状、CJK 值、空 summary 各跑一遍；以及 `color` 关时 `paint_label(&l) == l.to_string()`。

### 2.5 成本与风险

**要改的源文件**（5 个）：`src/tool/fmt.rs`（`ToolLabel` + `Display` + 两个旧函数改成薄包装）、`src/repl/render/styles.rs`（`paint_label` + 一个 bold-cyan 包装 + `sgr_pins` 加一条）、`src/repl/turn/tools.rs`（2 处）、`src/repl/render/group.rs`（`composing_label`）、`docs/DIVERGENCES.md`（新 X-83：Go 的工具头是整段 `CodeStyle`，这里分段）+ `CHANGELOG.md` Unreleased。

**会被影响的测试**（因为可见文本不变，绝大多数不动）：

| 测试 | 钉的字符串 | 是否要改 |
|---|---|---|
| `tests/repl/toolfmt.rs` | 6 条 `tool_call_header` 纯文本 | 不改（函数输出不变） |
| `tests/headless/approval.rs:233-259` | 4 条 | 不改 |
| `src/repl/turn/tools/tests.rs:646,1032` | 2 条，经 `strip_sgr` | 不改 |
| `src/repl/turn/tools/tests.rs:562` | `raised[0].contains("[write_file …]")`，**没剥 SGR** | **要改 1 条**（剥 SGR 再比，或直接比 `paint_label` 的期望） |
| `src/repl/render/transcript/tests.rs`（85 行）、`src/ui/render/region/tests.rs`（15）、`frame/tests.rs`（4）、`testing/scripted.rs`（2） | 测试自己喂进去的字面标签（`"[shell]"` 等），不经过构造函数 | 不改 |
| tmux 场景 19、24、25、26、31、33、34（约 16 处） | `capture-pane -p`（无 `-e`）读纯文本 | 不改；建议 L4 加一处 `-e` 断言括号与名字的 SGR 不同 |
| `tests/nocolor/main.rs` | 全程无转义 + 针 `"edit"` | 不改（无色下输出不变），它正好守住 §2.2 的 NO_COLOR 结论 |

合计：钉住工具标签可见文本的断言约 35 处，**改 1 处**；新增约 3 条（四形状分段、strip 不变式、无色恒等）。

**性能**：每个调用多建一个小 `Vec` 和几次 `format!`，一回合几十次，可忽略。`composing_label` 去重比的是上色后的字符串，结果仍确定。

**兼容/视觉风险**：

- dim 在少数终端（部分 Windows conhost、某些主题）不生效或很淡：括号退化成正常色，最坏等于今天少了 cyan，不会更难读。
- 字节变长：每段多约 8 字节转义，标签进 scrollback 的字节数涨几倍，但 `chunk_overflow`、`ansi_width` 都按可见宽度算，无影响。
- Go 对齐：这是有意的分歧，需要 X-83 一行；`TUI_CONTRACTS §9`（`styles.rs` 头注释里引的「CodeStyle=36 (tool headers)」）的注释要跟着改一句。

**值不值得**：值得，前提是按本方案「只改颜色、不改字形」做——一次小 PR，真正的风险面只有 3 个上色点。收益主要落在 key-value 形状（MCP 工具、`glob`/`grep`/`load_skill`/`remember` 走的都是这一形状），值里有空格时现在整段 cyan 糊成一片，分段后一眼能看出几个参数。单句话（shell、文件工具，最常见的那批）收益小：只是括号变淡、名字加粗。

## 3. 不同意或拿不准的地方

- **「按类型适配」我理解为只换颜色，不换字形**。如果所有者想要的是字形也变（比如单参数时省掉键名、`[glob **/*.rs]`），那是另一件事：会改可见文本，上面「约 35 处不改」就变成大面积改，tmux 场景也要跟着动，而且审批 `detail` 共用这串、要单独评估。建议先只做颜色。
- **名字 bold cyan 还是 cyan**：bold 有时在浅色主题上显得重；拿不准就退成「名字 cyan、值默认前景色」，但那样就和状态行任务段的 cyan 命令对不上了。我倾向 bold cyan，需要真机看一眼再定。
- **`image` widget 要不要变成 `[image]`**：它今天不带括号、不上色，是 Go 的原样。套进统一函数（`LabelBody::Bare`）会让它和工具调用长得一样，但它其实不是工具调用（专用图像 provider 的回合没有 tool call），而且改的是可见文本。建议不动，除非所有者觉得应该一致。
- **skills 来源标签与 `/tools` defer 状态**：形状是「只有一个标签」，能复用括号画法（括号 dim、内容保持 yellow），成本各 1 行。放不放这次都行；我建议放第二期，这次 PR 只管工具调用，免得 X-83 的范围说不清。
- **后台任务通知** `[background job … finished: …]`：所有者说的「可能不仅限于」最像指它，但这串是给模型 pattern-match 的固定格式，显示处又是整行 dim 的「次要信息」；分段上色只能在显示处（`tr.notice`）做，得先认出这个形状——我不建议。
