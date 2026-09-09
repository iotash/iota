# iota-rs 架构评审：Rust 代码是否过度迁就 Go 的结构？

日期：2026-09-04。性质：**独立、只读**的架构评审 —— 亲自通读 `src/provider`、`src/chat`、`src/repl`，并对
`src/ui`、`src/tool`+`src/mcp`+`src/shell`、`src/llm`、`src/session`+`src/config.rs`+`src/cmd`+`src/testing`
做了逐文件审读；下文每一处 `file:line` 都在写入前用 `grep -n` / `sed -n` 核实过（行号以 2026-09-04 的
工作树为准）。未修改任何源码或测试。

说明：`// Go: file:line` 锚点注释是刻意的可追溯性设计，本评审**不**把锚点本身算作问题；session
bundle 的磁盘格式必须与 Go 二进制互通，wire 形状的 struct 在这一层是合理的（§4 明确列出）。

---

## 1. 总体判断

**直觉部分成立，但成立的方式和所有者预期的不完全一样。**

成立的部分（真正的「Go 形状」渗透进了 Rust 类型）：

- **数据形状**是 Go 的：`Message` 是把四种角色的互斥字段摊平的大一统 struct（`provider/model.rs:137-167`，
  与 `provider.go:37-59` 逐字段同构）；`ui::facade::Panel` 是 8 种面板类型字段的并集（23 个 `pub` 字段）；
  `ProviderConfig` 21 个字段里 9 个用 `String` 的 `""` 表示「未设置」；计数用 `i64`（`max_turns`、
  `context_window`、`compacted_through`），配套一堆 `try_from(..).unwrap_or(..)`。
- **协议语法**是 stringly-typed 的：anthropic / openresponses 两个 dialect 的 event、delta、block kind
  全部是 `String` 字段 + 字面量 `match`（`provider/anthropic.rs:224-293, 475-532`；`provider/openresponses.rs:331-410`），
  `BlockAcc.kind: String` 的文档就是 `"text" | "thinking" | "tool_use" | a server block type`。
- **错误处理**在 provider 边界把 `LlmError` 擦成 `BoxError`（`provider/error.rs:19-42`），下游只好用三轮
  `source()` 链遍历 + `downcast_ref` 还原类型（`repl/errors.rs:38-79`、`repl/retry.rs:93-114`）—— 这是
  Go `errors.As` 的直译，还附带了一个 Go 时代的 `\b4\d{2}\b` 字符串扫描兜底。
- **上下文包 + `&mut` 尾巴**模拟 Go 的 receiver 方法：`Repl`(18 字段) 之外再造 `TurnCtx`(9)、`Turn`，
  `run_turn(cx, cancel, provider, history, ctxm, steer)` 这类签名一路传到 `walk(t, calls, history, ctxm, side_fx)`；
  `cmd` 层的 `Startup<'a>`(16 字段) 被 `&mut` 传进 helper 并 `mem::take` 字段；`ui` 层 9 个函数都以
  `(st: &mut SurfaceState, panels: &[Panel])` 开头。
- **Go 多返回值**变成元组或「`Ok`/`Err` 两侧都带同样的簿记字段」：`stream_round -> (Result<..>, String, String)`，
  `TurnOutput` 与 `TurnError` 各自携带 `used_tools`/`side_fx`。

**不成立或需要修正的部分**：

- **模块边界基本没问题**。`ui` 只依赖 `text` 和 `BoxFuture`，`repl` 只通过 `ui::facade` 触达终端，`session`
  不读环境变量 —— 依赖方向是清晰的，`pub(crate)` 泛滥主要在**字段**级（`ui` 111 个 `pub(crate)` 字段），不在模块级。
- **错误类型的骨架是对的**：`ProviderError`/`ChatError`/`LlmError`/`SessionError`/`CliError`/`UiError`/`McpError`
  全是 `thiserror` 枚举，Display 逐字节对齐 Go 并有测试钉住。问题只在「擦除 + downcast」和「用 `Io` 变体当字符串载体」。
- **「巨型文件」是假象**：`repl/toolloop.rs` 2163 行里实现只有 407 行，其余 1755 行是 2026-09-02 合并时搬进来的
  `#[cfg(test)]`；`ui/event_loop.rs` 710/1100、`ui/surface/mod.rs` 509/1144、`ui/frame.rs` 337/815。全 crate 约
  23.6k 行 in-file 测试。实现层面没有需要拆的巨型函数（`repl::run` 600 行是最长的，且结构就是 Go `Run` 的顺序）。
- **最伤可读性的东西并不是 Go 化，而是两样别的**：
  1. **十 crate 时代的边界产物在合并成单 package 后原样留下**：`PreviewHandle` 在 `markdown::sink` 和 `ui::facade`
     各定义一次再用 `PreviewBridge` 桥接；`TranscriptSurface` 把 `Ui` 的 11 个方法再声明一遍并写 45 行转发；
     `McpServerView` 逐字段镜像 `ServerStatus`；`llm` 反向 `use crate::provider::model::Raw`；注释里 33 处
     `iota-repl`/`iota-tui`/`iota-core` 仍被当作「为什么这样写」的理由。
  2. **注释把读者绑在仓库里不存在的过程文档上**：`CONTRACTS` ×85、`TUI_CONTRACTS` ×72、`TUI_DESIGN` ×33、
     `POLICY` ×26、`DEVIATIONS`/`DEVIATIONS3` ×49、`WPnn` ×169、`T-nn` ×137 —— `docs/` 里只有 7 个文件，没有一个
     叫这些名字。14 个文件的 `#![allow(dead_code)]` 理由（「`src/run.rs` 还是脚手架」「WP45 落地后失效」）早已过期，
     现在实际上在遮蔽真正的死代码。

**受影响最重的层**，按顺序：`provider` 的两个 dialect（stringly 协议 + 错误擦除）、`repl`（上下文包、元组返回、
过期注释、downcast）、`cmd`（`Startup` 包、resume 回放三份拷贝、Go 缩写命名）、`ui`（union struct、`Cell`
绕借用、全局主题标志）、`llm`（`String` 当 `Option`、四份孪生 `next()`）。`session`、`config` 的磁盘/YAML
层和 `tool`/`mcp` 的 trait 骨架总体健康。

---

## 2. 主要发现（按严重程度排序）

### F1. 错误类型在 provider 边界被擦除，下游用 `downcast` 链遍历还原（Go `errors.As` 直译）

**位置**：`src/provider/error.rs:19-42`，`src/provider/common.rs:104-109`，`src/repl/errors.rs:38-79`，
`src/repl/retry.rs:93-114`，23 处 `map_llm_err(e, ProviderError::Xxx)` 调用点（openai 6 / anthropic 4 / google 4 /
openresponses 4 / images 2 / imagen 2）。

**现状**：
```rust
// provider/error.rs:19-27
pub enum ProviderError {
    #[error("chat error: {0}")]          Chat(#[source] BoxError),
    #[error("stream error: {0}")]        Stream(#[source] BoxError),
    #[error("failed to list models: {0}")] ListModels(#[source] BoxError),
    ...
// provider/common.rs:104-109
pub(crate) fn map_llm_err(e: LlmError, wrap: fn(BoxError) -> ProviderError) -> ProviderError {
    match e { LlmError::Cancelled => ProviderError::Cancelled, other => wrap(Box::new(other)) }
}
// repl/errors.rs:40-45（同样的循环在 48-58、60-73 再写两遍）
let mut node: Option<&(dyn std::error::Error + 'static)> = Some(e);
while let Some(n) = node {
    if let Some(LlmError::Status(se)) = n.downcast_ref::<LlmError>() { return describe_status(se); }
    node = n.source();
}
// repl/retry.rs:111-113
// Historical fallback: any standalone `4\d{2}` other than 429 in the message means a client error
!has_non_429_4xx(&err.to_string())
```

**为什么伤害**：`Chat`/`Stream`/`ListModels` 三个变体存在的唯一理由是复刻 Go `fmt.Errorf("chat error: %w")`
的前缀；擦成 `BoxError` 之后，原本静态可知的 `LlmError` 只能靠运行时 downcast 找回，且找回逻辑要在
`describe_error` 和 `is_retryable` 各写一份。`retry.rs:111-114` 的字符串扫描注释说「SDK-shaped errors until every
provider is ported」—— 所有 provider 都是手写 wire 层，这个兜底早已无事可做，却仍是控制流的一部分。

**更 Rust 的写法**（Display 文本不变）：
```rust
pub enum ProviderError {
    #[error("{op} error: {source}")]
    Wire { op: WireOp /* Chat|Stream|ListModels */, #[source] source: LlmError },
    #[error("no response choices")] NoChoices,
    #[error(transparent)] Permanent(PermanentError),
    #[error("interrupted")] Cancelled,
    #[error("{0}")] Other(#[source] BoxError),
}
// repl/errors.rs 变成一个 match：
match e { ChatError::Provider(ProviderError::Wire { source: LlmError::Status(se), .. }) => describe_status(se), ... }
```
`is_retryable` 同理直接 `match`，删除 `has_non_429_4xx`。

**范围与风险**：跨模块（provider + repl + 6 个 dialect 文件），用户可见文本不变；`display_texts_match_go` 类测试全部照旧。

---

### F2. 协议事件语法 stringly-typed：wire 层给出「扁平化」的 `String` 字段，dialect 层用字面量 `match`

**位置**：`src/llm/anthropic.rs:278-299`（`Delta` 6 个 `String`），`src/llm/responses.rs:311-343`（`RespEvent`
「every field any event type carries」），`src/provider/anthropic.rs:224-293`（外层 4 臂 + 内层 4 臂）、
`:270`、`:313`、`:400-401`、`:427-435`、`:475-532`（`assemble` 5 臂），`src/provider/openresponses.rs:252-263`、
`:331-410`（9 臂）、`:519-560`。

**现状**：
```rust
// provider/anthropic.rs:398-401
struct BlockAcc {
    /// `"text"` | `"thinking"` | `"tool_use"` | a server block type.
    kind: String,
// provider/anthropic.rs:250-281
match delta.r#type.as_str() {
    "thinking_delta" => { gate.reasoning(&delta.thinking); block(&mut blocks, evt.index, "thinking")... }
    "text_delta"     => { split.write(&delta.text, &mut gate); block(..., "text")... }
    "input_json_delta" => { ...; if acc.kind == "tool_use" { ... } }
    "signature_delta" => { block(..., "thinking").sig.push_str(&delta.signature); }
    _ => {}
}
// provider/anthropic.rs:313
let tool_calls = if stop_reason == "tool_use" && ...
```

**为什么伤害**：一个拼错的字面量能编译通过；`BlockAcc` 的 `kind` 写入点（`:253,259,265,276`）和读取点
（`:475-532`）分散在 300 行之外，读者无法从类型上知道哪些字段会与哪个 kind 同时出现；`llm::anthropic::Delta`
把 6 种 delta 的载荷摊平成 6 个 `String`（Go `omitempty` 的形状）。`openresponses` 同样：`TypePeek`（`:97-100`）、
`Probe`（anthropic `:429-432`）、`ContentBlockStart`（llm `:264-274`）三个「偷看 `type`」的小 struct 其实已经是
手写的两段解析。google dialect 是反例：按字段存在性建模（`llm/google.rs:239-288`），provider 侧零字符串匹配。

**更 Rust 的写法**：注意 serde_json 的 `RawValue` 不能出现在 `#[serde(tag)]`/`untagged` 枚举里，所以诚实的做法
是**两段解析放进 wire 层**，dialect 拿到枚举：
```rust
// llm/anthropic.rs
pub(crate) enum AnthropicEvent {
    MessageStart { usage: Option<AnthropicUsage> },
    BlockStart   { index: u32, kind: BlockKind, id: String, name: String, raw: Raw },
    Delta        { index: u32, delta: DeltaKind },     // Text(String) | Thinking(String) | InputJson(String) | Signature(String)
    MessageDelta { stop_reason: StopReason, output_tokens: Option<u64> },
    Other,
}
pub(crate) enum BlockKind { Text, Thinking, RedactedThinking, ToolUse, ServerToolUse, ToolSearchResult, Other(String) }
```
`next()` 里 `match evt.r#type.as_str()` 只写一次（Go 锚点也在这里），provider 侧变成穷尽的 `match`；
`BlockAcc.kind: BlockKind`，`stop_reason: StopReason`。

**范围与风险**：跨模块（每个 dialect 的 llm + provider 两个文件）。会话文件无影响 —— 回放 blob 仍是 `Raw`。

---

### F3. Go 的「大一统 struct」直译：互斥字段摊平，读者靠注释知道哪些字段何时有效

**位置**：`src/provider/model.rs:137-167`（`Message` 12 字段），`src/ui/facade.rs:108-156`（`Panel` 23 字段）、
`:171-187`（`PanelResult`），`src/ui/surface/tabbed.rs:31-91`（`PanelState` 28 个 `pub(crate)` 字段，12 个
`Cell`/`RefCell`），`src/config.rs:20-74`（`ProviderConfig`）。

**现状**：
```rust
// provider/model.rs:142-167（节选）
pub struct Message {
    pub role: Role,
    pub content: String,
    pub reasoning: String,
    pub attachments: Vec<Attachment>,
    pub tool_calls: Vec<ToolCall>,        // 只有 Assistant 用
    pub tool_call_id: String,             // 只有 Tool 用
    pub tool_call_name: String,           // 只有 Tool 用
    pub is_error: bool,                   // 只有 Tool 用
    pub interrupted: bool,                // 只有 Assistant 用
    pub raw_content: Option<RawContent>,  // 只有 Assistant 用
    pub tools: Vec<ToolDef>,              // 「Only meaningful on a System message with empty content」
    pub usage: Option<Usage>,             // 只有 Assistant 用
}
// ui/facade.rs:108-156
pub struct Panel {
    pub title: String, pub kind: PanelKind, pub prompt: String,
    pub items: Vec<String>, pub cursor: usize, pub checked: Vec<usize>, pub custom: bool, // List/Multi
    pub min: f64, pub max: f64, pub step: f64, pub value: Option<f64>,                   // Slider
    pub on: bool,                                                                          // Switch
    pub lines: Vec<String>, pub wrap: bool,                                               // View
    pub dir: PathBuf,                                                                      // Browser
    pub text: String, pub placeholder: String, pub input_width: usize,                    // Input
    pub details: Vec<String>, pub refresh: Option<RefreshFn>, pub preview: Option<PreviewFn>,
    ...
}
```

**为什么伤害**：`Message::is_tools_mount()` 这种「靠两个字段的组合判断这是哪种消息」的谓词（`model.rs:237-239`）
就是 sum type 缺席的症状；`Role::System` + 非空 `tools` + 空 `content` 是一个用注释约束的隐式变体。`Panel`
的每个渲染/按键函数都先 `match p.kind` 再读对该 kind 无意义的字段（`slider_step` 读 `min/max/step`，
`render_input` 靠 `input_width == 0 → 40`）；`Panel` 还因为携带 `FnMut` 闭包不能 `Clone`，测试只好写
`clone_shape`（`ui/surface/mod.rs:1640-1652`），打开 surface 时要把闭包「搬」出去（`tabbed.rs:76-80` 三行
道歉注释在三处重复）。`PanelState` 的 12 个 `Cell`/`RefCell` 存在的原因是 `render` 拿 `&self` 却要发布
光标目标、终端高度和 picker 缓存（`event_loop.rs:565-568`）—— 这不是 Go 镜像，是**用内部可变性绕借用**，
比 Go 原版更难读。

**更 Rust 的写法**：
```rust
// 内存模型（磁盘上的 SessionRecord 保持 Go 形状不动，loader/writer 做转换 —— 转换本来就存在）
pub struct Message { pub role_data: RoleData, pub attachments: Vec<Attachment> }
pub enum RoleData {
    System { content: String },
    SystemTools { tools: Vec<ToolDef> },
    User { content: String },
    Assistant { content: String, reasoning: String, tool_calls: Vec<ToolCall>,
                raw_content: Option<RawContent>, usage: Option<Usage>, interrupted: bool },
    Tool { call_id: String, call_name: String, content: String, is_error: bool },
}
// ui
pub struct Panel { pub title: String, pub prompt: String, pub search: bool, pub height: usize, pub body: PanelBody }
pub enum PanelBody { List { items, cursor, custom }, Multi { .. }, Slider { min, max, step, value },
                     Switch { on }, Input { text, placeholder, width: Option<NonZeroUsize> },
                     View { lines, wrap, refresh }, Browser { dir }, Picker { items, details, preview } }
// PanelState：render(&mut self, ..) -> Rendered { rows, cursor }，picker 的 5 个 prev_* + prev_valid 收成 Option<PickerCache>
```

**范围与风险**：`Message` 改 enum 影响 13 个文件（4 个 dialect 的 `build_request`、session loader/writer、
repl 的 replay/export/compact/toolloop）；会话文件字节不变（`SessionRecord` 不动）。`Panel` 影响 repl 约 10 个
构造点（`systemtab`、`editpicker`、`settings`、`model`、`session`）。`PanelState` 局部于 `ui/surface`。
这是本评审里**收益最大但也最需要设计**的一项。

---

### F4. 「上下文包 + 一串 `&mut` 参数」模拟 Go 的 receiver 方法

**位置**：
- repl：`src/repl/run.rs:178-218`（`Repl` 18 字段全 `pub(crate)`）、`:494-514`（构造）、`:568-579` 与 `:711-719`
  （`ensure_model(&repl.ui, &repl.tr, &mut *repl.provider, &repl.writer, &root_cancel)` 拆 5 个字段）、
  `:782-792`（每条消息重建 `TurnCtx`）、`:811-819`；`src/repl/turn.rs:76-107`（`TurnCtx` + `Turn<'a>`）、
  `:208-215`（`run_turn` 6 参）；`src/repl/toolloop.rs:53-60`（`tool_loop` 6 参）、`:172-178`（`walk` 5 参）、
  `:290-296`（`surface_call` 5 参）；`src/repl/commands/model.rs:133-139`。
- cmd：`src/cmd/interactive.rs:66-100`（`Startup<'a>` 16 字段）、`:314`（`std::mem::take(&mut s.mcp_configs)`）、
  `:422-428`（`wire_session(s: &mut Startup, ..)` 读其中 9 个字段）、`:181-194`（`Wiring`）、`:406-415`（`WireInput`）。
- ui：`src/ui/surface/mod.rs:76,215,249,272,310,382,413,428,456`（9 个函数以 `(st: &mut SurfaceState, panels: &[Panel])` 开头，
  `st.ps[focus]` 在实现区出现 19 次）；`src/ui/keys.rs:19` + `src/ui/event_loop.rs:466-468`
  （`Model::handle_key` 只是跳到 `keys::update_key(m: &mut Model, ..)`，后者直接戳 21 个 `m.<field>`）。
- provider：`src/provider/openresponses.rs:387-397, 593-601`（`ItemSinks` 打包 6 个 `&mut`）。

**现状**：
```rust
// repl/run.rs:811-819
let res = run_turn(&cx, &root_cancel, &*repl.provider, &mut repl.history, &mut repl.ctxm, &mut steer).await;
// repl/run.rs:510 —— 而 root_cancel 早就被存进了 repl.cancel：
cancel: root_cancel.clone(),
// 4 个命令用 repl.cancel（compact.rs:183, session.rs:115, status.rs:269, tools.rs:219），
// 另 4 个从参数收 cancel: &CancellationToken（file.rs:180, model.rs:186, debug.rs:227, export.rs:762）—— 同一个 token 两条路。
// cmd/interactive.rs:314
std::mem::take(&mut s.mcp_configs),
// ui/surface/tabbed.rs:76-80
/// Picker: the preview renderer, MOVED here out of [`Panel::preview`] when the surface
/// opens (tabbed.go's `p.Preview` stays on the panel; Rust's render path holds the panels
/// by shared reference, and an `FnMut` needs `&mut` — so the closure lives beside the cache...
pub(crate) preview: RefCell<Option<PreviewFn>>,
// provider/openresponses.rs:593-601
struct ItemSinks<'a> {
    raw_items: &'a mut Vec<Raw>, images: &'a mut Vec<Attachment>, leg_searches: &'a mut Vec<SearchCall>,
    pending_args: &'a HashMap<String, String>, fn_calls: &'a mut HashMap<String, FnCallAcc>, fn_call_order: &'a mut Vec<String>,
}
```

**为什么伤害**：状态被劈成两半 —— `Repl` 里放了 18 个字段，可 `overlay`、`gate`、`images_dir`、`title_task`、
`title_provider`、`compact_declined`、`dark`、`image_provider`、`token_aware`、`agent` 仍是 `run()` 的栈变量，
`TurnCtx` 每条消息从两边各抓一点拼一次。读者无法回答「一个 turn 到底依赖什么」。`Startup` 的 `mem::take`
是「这是一个临时草稿袋而不是值」的标志。`SurfaceState` + `&[Panel]` 是 Rust 引入的拆分（Go 的 `surfaceState`
把 `spec` 和 `ps` 放在一个 struct 上），`ItemSinks` 只是把 6 个 `&mut` 参数换了个名字。

**更 Rust 的写法**：
```rust
// repl：把 turn 相关状态收进一个类型，方法取代自由函数
impl Repl {
    async fn run_message(&mut self, content: String) -> Result<(), ReplError> { ... }
}
struct TurnEngine { ui, tr, dispatch, gate, overlay, images_dir, can_retry, code_theme, pres,
                    ctxm: CtxMeter, steer: Steerer }          // 一次构造，跨 turn 复用
impl TurnEngine { async fn run_turn(&mut self, cancel: &CancellationToken, provider: &dyn Provider,
                                    history: &mut Vec<Message>) -> Result<TurnOutput, TurnError> }
// 命令统一 fn cmd_x(repl: &mut Repl, arg: &str)，cancel 从 repl.cancel 取
// cmd：不可变共享部分 RunContext { dirs, http, transport, reqlog, resolver, cancel }（Clone），
//      wire_session 只收它真正改的东西：(provider: &mut dyn Provider, settings: &RunSettings, overrides: &ResumeOverrides)
// ui：SurfaceState { focus, enter_advances, slots: Vec<PanelSlot> }，PanelSlot { spec: Panel, state: PanelState }，
//     fn focused_mut(&mut self) -> &mut PanelSlot；render/tick/paste/key 变成 SurfaceState 的方法
// openresponses：struct LegAcc { raw_items, images, searches, fn_calls, order } 作为拥有者，on_item_done 是它的方法
```

**范围与风险**：repl 是跨文件重排（run.rs + turn.rs + toolloop.rs + commands/*），行为不变但改动面大；
cmd/ui/openresponses 各自局部。无 Go 对齐或会话文件风险。

---

### F5. Go 多返回值 → 元组，或 `Ok`/`Err` 两侧携带同样的簿记字段

**位置**：`src/repl/turn.rs:312-317`（`stream_round -> (Result<RoundResult, ChatError>, String, String)`）、
`:112-141` 与 `:163-175`（`TurnOutput`/`TurnError` 各有 `used_tools`、`side_fx`）、`src/repl/interrupt.rs:23-28`
（`-> (Vec<Message>, bool, Vec<Attachment>)`）、`src/chat/run.rs:147-183`（5 元组解构后再装回
`Message`，而同文件 `:208-220` 已经有 `LoopOutcome` 可用）、`src/cmd/mod.rs:364-374`
（`-> (Arc<Manager>, Option<(Arc<dyn Dispatcher>, PrefixOf)>)`）、`src/repl/commands/settings.rs:79,108,132`
（三个 `-> (Vec<_>, Vec<String>, usize)`）。

**现状**：
```rust
// repl/turn.rs:312-317
pub(crate) async fn stream_round(t: &Turn<'_>, tp: &dyn ToolProvider, send: &[Message], tools: &[ToolDef])
    -> (Result<RoundResult, ChatError>, String, String) {
// repl/toolloop.rs:81-86 —— 调用方只能靠闭包捕获把两个 String 搬出去：
async || { let (res, p, pr) = stream_round(t, tp, &send, &tools).await; partial = p; partial_reasoning = pr; res }
// repl/run.rs:237-246 —— run_turn 结尾对两侧各写一次同样的赋值：
match res { Ok(mut out) => { out.used_tools = used_tools; Ok(out) }
            Err(mut e)  => { e.used_tools = used_tools; Err(e) } }
```

**为什么伤害**：`(Result, String, String)` 没有名字，调用方要靠位置记住哪个是 partial、哪个是 reasoning；
「错误里带数据」迫使 `TurnError` 复制 `TurnOutput` 的两个字段，并在 `run_turn` 末尾对 `Ok`/`Err` 各写一遍。
`chat/run.rs:147` 的五元组把 `LoopOutcome` 拆开又在 `:187-195` 逐字段装回 `Message`。

**更 Rust 的写法**：
```rust
struct RoundOutcome { result: Result<RoundResult, ChatError>, partial: String, partial_reasoning: String }
struct TurnReport { used_tools: bool, side_fx: u32, partial: String, partial_reasoning: String,
                    outcome: Result<TurnOutput, TurnFailure> }
struct InterruptDecision { history: Vec<Message>, persist: bool, dropped_attachments: Vec<Attachment> }
// chat/run.rs：两个分支都产出 LoopOutcome（unary 分支填 reasoning: String::new()），后面只写一次
```

**范围与风险**：局部到跨文件（repl 三个文件 + chat/run.rs），无对齐风险。

---

### F6. 十 crate 时代的边界产物在合并成单 package 后原样残留

**位置与现状**：
- `src/markdown/sink.rs:1-16` 与 `src/ui/facade.rs:290-297`：**同名同签名的 `PreviewHandle` 定义两次**，
  `src/repl/uisink.rs:117-125` 用 `PreviewBridge` newtype 把一个转成另一个。注释理由：「this crate stays iota-core-free」。
- `src/repl/transcript.rs:26-98`：`TranscriptSurface` 重新声明 `Ui` 的 11 个方法，再用 45 行
  `impl TranscriptSurface for Arc<dyn Ui>` 逐个转发（`crate::ui::facade::Ui::print_lines(self.as_ref(), lines)` ×11）。
  这是 Go 结构化接口「消费者自定义窄接口」的免费习惯，在 Rust 里每次都要付适配器的钱。
- `src/mcp/status.rs:8-25` 与 `src/repl/run.rs:88-107`：`McpServerView` 逐字段镜像 `ServerStatus`（+ `wire_names`），
  `src/cmd/interactive.rs:612-640` 手工搬 8 个字段；理由是「iota-repl never imports iota-mcp」（`run.rs:70-71, 83-84`）。
- `src/cmd/mod.rs:364-374`、`src/cmd/assemble.rs:1-3, 78`：`(Arc<dyn Dispatcher>, PrefixOf)` 元组穿 4 层，只为让
  `assemble.rs` 不认识 `Manager`。
- `src/llm/{anthropic:3,chatcomp:4,client:10,images:5,google:6,responses:3}.rs`：wire 层 `use crate::provider::model::{Raw, JsonObject}`
  —— 「llm 在 provider 之下」只是 Go 包名的约定，不是依赖方向；`src/llm/reqlog.rs:212-277` 的 `last_user_text`
  在传输层重新解析 5 种 dialect 的请求 JSON，注释承认是为绕 `llm → repl` 的方向问题。
- `src/vars.rs:11-18` 与 `:67-78`：`VarResolver { env_var, cwd, home }` 和 `EnvSource { var }` 两个环境 seam，
  `src/cmd/mod.rs:340-356` 写 `EnvResolver` 适配器；`testing` 里两套 fake，`vars.rs:106-126` 测试里再手写第三份。
- 注释：33 处 `iota-repl`/`iota-tui`/`iota-core`/`iota-mcp`（`ui/facade.rs:1-3`「Implemented by `iota-tui`; consumed by
  `iota-repl`」、`tool/mod.rs:236-238`「Lives in iota-core」、`mcp/config.rs:1-3`、`mcp/manager.rs:290`、
  `text/ansi.rs:2`、`cmd/resolve.rs:252`）仍作为设计理由存在；`mcp/mod.rs:3-5` 引用的路径
  `crate::mcp::tool::Dispatcher`、`crate::mcp::cmd::assemble` 根本不存在；`shell/mod.rs:3` 的 `crate::tool::tool::shell` 同样。

**为什么伤害**：每一个都是「为了一个已经不存在的约束而付出的间接层」，读者要先重建十 crate 的历史才能理解
为什么有两个 `PreviewHandle`。`ARCHITECTURE.md §1.3` 宣称「the crate-boundary hacks died with the boundaries」，
代码里这些没死。

**更 Rust 的写法**：删除 `markdown::sink::PreviewHandle`，`markdown` 直接依赖 `ui::facade::PreviewHandle`（或反过来，
二选一）；`Transcript` 直接持有 `Arc<dyn Ui>`，测试用 `ScriptedUi`（已存在）；`McpServerView` 换成
`ServerStatus` + `wire_names`（或给 `ServerStatus` 加 `wire_names()` 方法）；`assemble::build_dispatcher` 直接收
`Option<&Arc<Manager>>`；`Raw`/`JsonObject` 移到 `llm::json` 由 `provider::model` re-export；`Env` 一个 trait
（`var`/`cwd`/`home`）、一个 `ProcessEnv`、一个 fake。

**范围与风险**：每项局部或双模块；零行为变化；修完顺手把 33 处过期 crate 名注释改掉。

---

### F7. 注释把读者绑在仓库外的过程文档上；过期的「计划态」注释在遮蔽死代码

**位置**（全 crate grep 计数）：`CONTRACTS` 85、`TUI_CONTRACTS` 72、`TUI_DESIGN` 33、`POLICY` 26、
`DEVIATIONS`+`DEVIATIONS3` 49、`\bWP[0-9]+\b` 169、`\bT-[0-9]+\b` 137、`\bD-[0-9]+\b` 88（D-nn 在 `DIVERGENCES.md`
里能查到，其余都查不到）。`#![allow(dead_code)]`（模块级）：`src/repl/{turn.rs:32, toolloop.rs:20, steer.rs:9,
approval.rs:10}`，`src/ui/{event_loop.rs:26, frame.rs:16, region.rs:33, msgs.rs:12, osc.rs:13, sink.rs:13,
spans.rs:14, term.rs:33, theme.rs:16, debug.rs:6}`；条目级：`src/repl/uisink.rs:19,31,35`、`src/paths.rs:112`。

**现状**：
```rust
// repl/turn.rs:29-32（toolloop.rs:17-20、steer.rs:6-9、approval.rs:7-10 逐字相同）
// Not a stub: the whole turn engine is complete and driven by tests/toolloop.rs, but its
// only production consumer is WP50's run loop (`src/run.rs` is still a scaffold), so the
// lib build sees every item as dead. The attribute goes inert when WP50 lands.
#![allow(dead_code)]
// —— 而 src/repl/run.rs 已是 1021 行的完整实现并在 :62 导入 run_turn。
// repl/mod.rs:54-58
/// Hand-implemented `Display`/`Error` (transparent to the source) rather than a
/// `thiserror` derive: the frozen §1.4 manifest carries no `thiserror` dependency
// —— Cargo.toml:39 有 thiserror = "2"，chat/error.rs 等 7 个错误类型都在用它。
// provider/sink.rs:13
/// ... No dialect emits it until WP55 (T-11).
// —— openai.rs:397、anthropic.rs:272、openresponses.rs:342/367/375 都在 emit；repl/turn.rs:24-27 同样过期。
// cmd/mod.rs:38-53 —— 17 行 doc comment 完全用 root.go 行号叙述顺序；
// repl/turn.rs:126-135 —— 一个字段的 doc 里出现「resolves the `NEEDS: [WP49]` row of DEVIATIONS3; inert until WP53's meter reads it」。
```

**为什么伤害**：读者必须同时懂 Go、懂一套不在仓库里的工作包编号体系，才能判断一段注释是「现在的真相」
还是「某个阶段的计划」。模块级 `#![allow(dead_code)]` 现在正在压制真实的死代码（`paths::within` 明说
「no production caller」，`SessionWriter::images_dir` 在 `src` 中无调用者，`ui/composer.rs` 的 `set_draft`、
`field.rs:136` 的 `input_cursor_cols` 等都躲在后面）。`ReplError` 的手写实现和 `thiserror` 并存于同一 crate 是
最直白的一致性缺口。

**更 Rust 的写法**：删掉全部模块级 `#![allow(dead_code)]`，让 rustc 报一遍，真死的删、测试用的加 `#[cfg(test)]`；
`ReplError` 改 `thiserror`（`#[error(transparent)]` ×3）；把 `CONTRACTS §x`/`WPnn` 引用要么落成 `docs/` 里的文件，
要么改写成一句 Rust 不变量（例：「每个 client 都有 2 分钟响应头超时，永不设整请求超时」）；`// Go: file:line`
锚点保留。

**范围与风险**：仅注释与属性；零行为变化；是所有发现里性价比最高的一项。

---

### F8. Go 零值语义渗透：`""`/`0` 当 `None`，`i64` 当计数

**位置**：`""` = unset 的文档化约定 19 处（`config.rs:21,34`、`cmd/resolve.rs:27,31`、`chat/run.rs:66`、
`chat/once.rs:20`、`chat/delegator.rs:30`、`repl/errors.rs:18`、`ui/region.rs:91,117`、`ui/surface/search.rs:42`、
`ui/surface/tabbed.rs:50`、`ui/composer.rs:37`…）；`src/llm` 响应 struct 里 `String` 91 处 vs `Option<String>` 9 处，
`skip_serializing_if = "String::is_empty"` 约 22 处，`llm/mod.rs:26-31 is_zero`（6 处 `u32` 上）；
`src/provider/mod.rs:156-166`（`Effort::parse("") → Ok(None)`）、`:279-296`（`ImageGenParams` 三个 `String`「Empty
values mean omit」）；`src/tool/mod.rs:138-141`（`header_summary -> Option<String>`：「`None` = no headliner capability;
`Some("")` = capability present, bare header」）；`src/mcp/status.rs:23-24` 与 `src/repl/run.rs:105-106`
（`err: String`「empty on success」—— 同文件 `:75-76` 的 `McpEvent.error` 却用 `Option<String>`）；
`i64`：`src/chat/run.rs:122,237`、`chat/once.rs:25`、`cmd/cli.rs:108`、`repl/run.rs:157-158`、`repl/meter.rs:52-58`
（`window: i64` 与 `settled/pending: u64` 同 struct）、`:97-100`、`session/meta.rs:61,91`、`session/record.rs:60`、
`session/loader.rs:249,255`、`cmd/interactive.rs:576`；`src/tool/args.rs:11-28`（缺参 → `""`/`0`）。

**现状**：
```rust
// repl/meter.rs:97-100
fn threshold_of(window: i64) -> u64 {
    let pct = window * COMPACT_THRESHOLD_PERCENT / 100;
    u64::try_from((window - COMPACT_RESERVE_TOKENS).max(pct)).unwrap_or(0)
}
// cmd/interactive.rs:576 —— parse_window_size 刚返回 u64，立刻转回 i64：
i64::try_from(n).unwrap_or(i64::MAX)
// tool/mod.rs:138-141
/// `None` = no headliner capability; `Some("")` = capability present, bare header.
fn header_summary(&self, _args: &JsonObject) -> Option<String> { None }
// provider/openresponses.rs:540-544
let call_id = if peeked.call_id.is_empty() { peeked.id.clone() } else { peeked.call_id.clone() };
```

**为什么伤害**：类型不再表达「可能缺席」，每个读取点都要记得 `is_empty()`/`<= 0` 的约定；`i64` 计数使
每一次与 `usize`/`u64` 相遇都长出 `try_from(..).unwrap_or(..)`；`Option<String>` 的三态（`None`/`Some("")`/`Some(s)`）
是 Go `(string, bool)` 的直译。`ARCHITECTURE.md G26` 明确把 `--max-turns: i64` 当成对齐决定 —— 对齐的是 CLI
**行为**（负数 = 无限），不必是内部类型。

**更 Rust 的写法**：`ProviderConfig` 的 serde struct 可以继续用 `String`（YAML 兼容 + Go 的报错时机），但暴露
类型化 accessor `fn effort(&self) -> Result<Option<Effort>, InvalidEffort>`，删掉 4 处手写 `is_empty() + parse`；
wire 响应 struct 用 `Option<String>` + `#[serde(default)]`，`peeked.call_id.as_deref().unwrap_or(&peeked.id)`；
`n: Option<u32>` + `Option::is_none` 删掉 `is_zero`；`header_summary -> Option<Cow<str>>` 改为
`enum Header { None, Bare, Detail(String) }` 或干脆两个方法；`max_turns: Option<NonZeroU32>`、`context_window: u64`、
`compacted_through`/`conv_count: usize`（CLI 解析时把负数折成 `None`，行为不变）；`ServerStatus.err: Option<String>`。

**范围与风险**：多为局部；`i64 → usize` 改动会碰 session 语义的边角（手改成负数的 `compacted_through` 从
「夹到 0」变成「跳过该行」，Go 也是夹到 0，可见结果相同）。会话文件字节不变。

---

### F9. 能力发现：8 个 `as_*` 访问器本身可接受，但 `Dispatcher` 用返回值 `Option` 编码「有无能力」是真问题

**位置**：`src/provider/mod.rs:220-262`（`as_tool_provider`、`as_tunable`、`as_top_p_tunable`、`as_image_tunable`、
`as_image_gen_tunable`、`as_image_edit_json_tunable`、`as_tool_search_host`、`as_image_partial_provider` + `reports_usage`）；
`src/tool/mod.rs:171-177`（`owns -> Option<bool>`「None = no Owner capability」、`search_tools -> Option<Vec<ToolDef>>`
「None = no ToolSearcher capability」）、`:189-234`（`impl Dispatcher for Arc<T>` 45 行逐方法转发）、
`src/chat/delegator.rs:109-173`（`ApprovingDispatch` 再转发 10 个方法）。

**评价**：`Option<&dyn Cap>` 的默认方法是 Rust 里替代 Go 接口断言的**合理**写法（对象安全、不用 `Any` downcast），
调用方 `provider.as_tool_provider()` 也不冗长；`&mut self` 版本在需要同时拿两个能力时会打架，但目前调用点都是
串行的。真正别扭的是 `Dispatcher`：`owns` 的三态 `Option<bool>`、`search_tools` 的 `None` 与「有能力但零命中」
的 `Some(vec![])` 靠注释区分；`impl Dispatcher for Arc<T>` 是为了同一处既用 `Arc<dyn Dispatcher>` 又用 `&dyn Dispatcher`
（`&*dispatch` 11 处），代码里没有任何 `T: Dispatcher` 泛型调用方需要它。

**更 Rust 的写法**：`fn as_owner(&self) -> Option<&dyn Owner>`、`fn as_tool_searcher(&self) -> Option<&dyn ToolSearcher>`
—— 与 `Provider` 保持同一种能力发现风格；删除 `impl Dispatcher for Arc<T>`，统一以 `&dyn Dispatcher` 传参。

**范围与风险**：局部（tool/mod.rs + merge.rs + 3 个 Dispatcher impl）。

---

### F10. 「Go 有这个函数所以 Rust 也有」的 pass-through 与无意义分层

**位置与现状**：
- `src/chat/batch.rs:57-60`：`pub fn batch_message(tc, o) -> Message { Message::tool_result(tc, o.text.clone(), o.is_error) }`
  （注释：`parallel.go:132-140`）—— 还多付一次 `clone`。
- `src/llm/chatcomp.rs:333-335` 与 `responses.rs:457-459`：`models()` 三行转发到 `models::openai_model_ids`；
  `llm/models.rs` 存在的理由（文件头）是「两个 dialect 包不共享文件」。
- `src/paths.rs:109-123`：`absolute` 一行包 `std::path::absolute`；`within` 无生产调用者、`#[allow(dead_code)]` 保留。
- `src/ui/clipboard.rs`：整个模块只有一行 `pub(crate) use crate::ui::surface::copy_to_clipboard;` + `#![allow(unused_imports)]`。
- `src/ui/mod.rs:41-56` → `handle.rs:49` → `handle.rs:192`：`Tui { inner: TuiInner }`、`TuiInner(Arc<TuiHandle>)`、
  `fn facade(&TuiInner)` 三层只为把一个 `Arc` 转成 `Arc<dyn Ui>`；`ui/composer.rs:105-109` `set_draft` = `set_value`
  （「the Go setDraft verb」）。
- `outcome_of`（`repl/toolloop.rs:395-400`）、`chat/run.rs:338-342`、`chat/batch.rs:44-47` 三处把
  `Result<ToolOutput, ToolError>` 压成 `(String, bool)` 的同一段代码。
- `src/chat/turns.rs:90-106`：`BudgetExt for Option<Arc<TurnBudget>>`（「Nil-budget rule」）—— Go nil receiver 方法的直译；
  可接受，但一个 `TurnBudget::unlimited()` 常量或在 `RunCtx` 上放方法更直接。

**更 Rust 的写法**：删除 pass-through，调用点直接用目标函数；`ToolResult` 上加一个 `fn into_model_text(self) -> (String, bool)`
（或 `ToolOutput::from_result`），三处共用。

**范围与风险**：全部局部、零行为变化。

---

### F11. 重复代码：孪生 `next()`、双份 `query_escape`、双份 usage、三份 resume 回放、七份测试 harness

**位置**：
- `src/llm/chatcomp.rs:345-360` ≈ `google.rs:488-502` ≈ `responses.rs:472-491` ≈ `anthropic.rs:411-440`
  （同样的「无事件则 `NoEvents`，解析，检查 in-band error」）；四个 provider 的流循环开头（`openai.rs:334-343`、
  `anthropic.rs:215-223`、`google.rs:472-480`、`openresponses.rs:323-329`）再各写一遍 `match stream.next().await`。
- `src/llm/anthropic.rs:381-401` 与 `google.rs:196-213`：两份 `query_escape`（一个 `[char;16]` 一个 `[u8;16]`），
  两套测试（`:448-455`、`:606-612`）。
- `src/llm/chatcomp.rs:177-199` vs `responses.rs:204-226`（`ChatUsage`/`RespUsage` 仅字段名不同），
  `src/provider/usage_conv.rs:9-23` ≡ `:27-41`（函数体逐字相同）。
- `src/cmd/mod.rs:231-257`、`src/cmd/interactive.rs:443-463`、`src/repl/commands/session.rs:195-206`：resume 时
  「按条件 `set_model` + `apply_session_tuning(meta, provider, kind, bool, bool, warn)`」三份拷贝，两个裸 `bool` 位置参数。
- `PoisonError::into_inner` 97 处 + 三个本地 `lock` helper（`ui/handle.rs:81`、`ui/sink.rs:31`、`ui/event_loop.rs:78`）。
- 测试：`struct Surf` ×7（`ui/surface/mod.rs:549`、`search.rs:508,1086`、`tabbed.rs:656,899,1076,1338`），
  `test_model()` ×4，`SharedBuf`+`ChannelEvents` ×2，`render_plain` ×2。

**更 Rust 的写法**：`llm` 里一个 `JsonEvents<T>`（`futures::stream::try_unfold` 约 15 行）实现 `Stream<Item = Result<T, LlmError>>`，
`NoEvents` 规则只写一次，provider 侧 `while let Some(ev) = stream.try_next().await?`；一个 `query_escape`；
`OpenAiUsage` + `#[serde(alias)]`；`SessionStore::resume_into(id, provider, Overrides { model, temperature, window }, warn)`；
`#[cfg(test)] pub(crate) mod testutil` 放一份 `Surf`/`LoopHarness`。

**范围与风险**：局部到双模块；wire 字节与 golden 测试不变。

---

### F12. 「巨型文件」其实是测试：把 in-file 测试拆到 `tests.rs` 子文件

**位置**：`src/repl/toolloop.rs`（实现 1-407 / 测试 408-2163）、`src/ui/event_loop.rs`（1-710 / 711-1810）、
`src/ui/surface/mod.rs`（509 / 1144）、`src/ui/surface/tabbed.rs`（621 / 984）、`src/ui/surface/search.rs`（474 / 924）、
`src/ui/region.rs`（552 / 912）、`src/ui/frame.rs`（337 / 815）、`src/repl/commands/export.rs`（821 / 484）。
全 crate `#[cfg(test)]` 之后的行数合计 23,645；`tests/` 另有 30,944 行。

**为什么伤害**：编辑器打开 `toolloop.rs` 看到 2163 行，实际逻辑不到五分之一；测试内的 `FakeStream`、
`OrderUi`（26 个方法、119 行）等 fake 与 `src/testing/` 的 `ScriptedUi` 功能重叠。

**更 Rust 的写法**：`#[cfg(test)] mod tests;` + `src/repl/toolloop/tests.rs`（或 `toolloop_tests.rs` 通过 `#[path]`），
实现文件保持原样；`OrderUi` 若只比 `ScriptedUi` 多记录顺序，合并进 `testing::scripted`。

**范围与风险**：零风险，纯文件搬移。

---

### F13. 全局可变状态镜像 Go 包变量

**位置**：`src/ui/theme.rs:47-52`（`static DARK_BACKGROUND: AtomicBool`），`src/repl/styles.rs` 的 `set_dark_background`
（`repl/run.rs:371,779` 调用），`src/ui/event_loop.rs:326-330`（消息里已经带着 `dark`，却只是写入全局），
`src/ui/surface/mod.rs:643-649`（测试为此加 `static BG: Mutex<()>` 串行化）；`src/llm/progress.rs:63-71`
（`tokio::task_local! TURN_PROGRESS`，`client.rs:274` 读取）—— 同一个调用里 `cancel` 是显式参数、进度是环境值。

**更 Rust 的写法**：`dark: bool` 放到 `Model`/`FrameInput`/`render_surface(.., theme: &Theme)`；`Client::with_progress(tp)`
（`Client` 已经 `Clone`）或一个 `RequestScope { cancel, progress }` 参数。字节输出不变。

**范围与风险**：`ui` 内局部；`progress` 涉及 `repl/turn.rs:42`、`repl/phases.rs:74-90`。

---

### F14. 错误里携带展示逻辑，或用 `Io`/`String` 当文本载体

**位置**：`src/session/store.rs:248-254`（`SessionError::Io(io::Error::new(InvalidInput, format!("invalid session id {id:?}")))`，
`:246` 注释明说是为了「without widening the error enum」）、`src/session/writer.rs:212`、`src/cmd/interactive.rs:61-63`
（`fn refuse(msg) -> CliError { CliError::Io(io::Error::other(msg)) }` 用于 `UiError` 和 join 错误）、
`src/cmd/resolve.rs:207-212`（`UnknownProvider { name, hint }`，`hint` 是在 `:170-174` 预渲染好的
`"\n  configured aliases: a, b"`）、`:281-282`（`ContextWindow(String)`，`"<label>: <err>"` 在调用点拼）；
`Result<_, String>` 20 处（`repl/commands/compact.rs:83`、`chat/images.rs:57`、`shell/exec.rs:255`、三个 `sandbox_*`、
`cmd/window.rs:6`、`tool/yaml11.rs:63` …）；`src/ui/surface/mod.rs:474-508`（剪贴板 `io::Error::other(..)`，唯一调用方
`:325` 只看 `.is_ok()`）。

**更 Rust 的写法**（Display 字节不变）：`#[error("invalid session id {0:?}")] InvalidId(String)`；
`UnknownProvider { name, aliases: Vec<String> }` 在 `Display` 里渲染 hint；
`#[error("{label}: {source}")] ContextWindow { label: &'static str, #[source] source: WindowSizeError }`；
`#[error(transparent)] Ui(UiError)`；`compact_history -> Result<Compaction, CompactError>`。`CliError` 已有 30 个变体，
「不扩枚举」不是有效理由。

**范围与风险**：局部。

---

### F15. Go 缩写命名

**位置**：`src/cmd`、`src/config.rs`、`src/session` 中 `pc` 119 次、`cfg` 55、`mgr` 21、`sc` 15、`tun` 14、`sess` 13、
`reg` 11、`ptype` 6（`cmd/delegate.rs:76-83` 的 `struct Resolved { ptype, pc, tools }` 就是 Go `type resolved struct` 原样）；
`src/ui` 中 `ptail`、`hoff`、`in_off`、`errmsg`、`sug_base`/`sug_idx`、`surf`/`surf_gen`、`vh`、`crow`/`ccol`、`boxp`。
好的一面：没有 `Get`/`Set` 前缀，没有 `Err` 后缀。

**更 Rust 的写法**：`provider_cfg`、`manager`、`session`、`provider_type`、`preview_body`、`pan_offset`、`completion`。纯改名。

---

## 3. 模式级问题

| 模式 | 出现次数 / 典型位置 | 与 Go 的关系 |
|---|---|---|
| **上下文包 + `&mut` 尾巴** | `Repl`/`TurnCtx`/`Turn`（repl 4 个函数各带 4-6 参）、`Startup`/`Wiring`/`WireInput`（cmd）、`(st, panels)` ×9（ui/surface）、`ItemSinks`（openresponses）、`RunCtx`（chat/turns.rs:13-26，Go `context.Value` 的显式化：三个 `Option<Arc<_>>`） | 一半是 Go receiver 直译，一半（`SurfaceState`+`&[Panel]`、`PanelState` 的 12 个 `Cell`）是 Rust 借用规则的绕行，比 Go 更糟 |
| **stringly-typed kind** | anthropic 18 处读 + 4 处写，openresponses 15 处，`SessionRecord.role: String` + `loader.rs:150-156` 四臂 match，`config.rs` 的 `kind/effort/defer_mode/context_window: String` | wire 层 `#[serde(default)] String` 是 Go `omitempty` 的形状；google dialect 证明可以不这样做 |
| **`""`/`0`/`i64` 零值当 `None`** | 文档化 19 处；llm 91 `String` vs 9 `Option`；`i64` 计数 18 处声明 + 10 余处 `try_from` 噪音；`Effort::parse("")`；`header_summary` 三态 | 直接来自 Go 零值语义与 `int` |
| **大一统 struct** | `Message` 12、`Panel` 23、`PanelState` 28、`Model` 25、`ProviderConfig` 21、`SessionMeta` 19、`RunParams` 16、`Startup` 16 | `Message`/`Panel`/`PanelState`/`ProviderConfig` 与 Go 逐字段同构；`RunParams` 反而是把 Go 15 参数的 `Run` 签名收成 struct，是改进 |
| **错误擦除 + downcast** | `ProviderError::{Chat,Stream,ListModels}(BoxError)`；`repl/errors.rs` 三轮链遍历；`repl/retry.rs` 链遍历 + 4xx 字符串扫描 | Go `fmt.Errorf("%w")` + `errors.As`/`errors.Is` 直译 |
| **多返回值 → 元组 / 双侧簿记** | 3+ 元组返回 10 处（其中 6 处在 repl）；`TurnOutput`/`TurnError` 重复字段；`connect_mcp` 嵌套元组 | Go `(a, b, err)` |
| **同名 pass-through** | `batch_message`、`models()` ×2、`paths::absolute`/`within`、`clipboard.rs`、`Tui/TuiInner/facade`、`set_draft`、`outcome_of` ×3 | 「Go 有这个函数」 |
| **crate 边界残留** | `PreviewHandle` ×2 + 桥、`TranscriptSurface` 转发、`McpServerView`、`(Dispatcher, PrefixOf)` 元组、`llm → provider::model` 反向依赖、`EnvSource`/`VarResolver` 双 seam、33 处过期 crate 名注释 | 不是 Go 化，是合并前架构的化石 |
| **计划态注释与过期 allow** | 8 种仓库外文档 ~300 处引用；`WPnn` 169、`T-nn` 137；模块级 `#![allow(dead_code)]` 14 文件；`ReplError` 的错误理由；`sink.rs:13`/`turn.rs:25` 失实 | 过程债，不是 Go 化 |
| **全局状态镜像包变量** | `DARK_BACKGROUND` 静态 + 测试互斥锁；`TURN_PROGRESS` task-local | Go 包级变量 / `context.Value` |
| **Go 缩写命名** | `pc` 119、`cfg` 55、`mgr` 21 …；ui 的 `ptail`/`hoff`/`sug_*` | 直接沿用 |
| **`warn: &mut dyn FnMut(String)` 回调** | `cmd/mod.rs` 5 处 `\|w\| io.warning(&w)` 闭包，7 个签名 | Go `warnf func(...)` 参数 |
| **`Result<_, String>`** | 20 处 | Go `errors.New`/`fmt.Errorf` |

---

## 4. 做得好的 / 不该改的

**明确不要动的（对齐或互通所需）**：

- **`session/record.rs`、`session/meta.rs` 的磁盘 struct**：`SessionRecord`/`SessionMeta`/`SessionUsage`/`SessionAttachment`/
  `SessionRaw` 按 Go 字段顺序 + `omitempty` 矩阵（`is_zero_i64`/`is_false`）+ `#[serde(flatten)] extra` 保未知键，
  是双二进制共享 bundle 的必要形状。`role: String` 也是磁盘形状（`"compaction"` 不是 `Role`）—— 只建议在
  **内存侧**引入 `RecordRole` 枚举（F3/模式表），字节不变。
- **所有 `Display` 文本逐字节对齐 Go 并有测试**（`chat/error.rs:44-67`、`provider/mod.rs` tests、`session/error.rs:52-87`、
  `ui/facade.rs:555-560`）：错误**文本**必须保持，本评审所有关于错误的建议都是「改结构不改字节」。
- **`text::go_quote`/`go_float`/`go_duration`**：只用于产生 Go 格式的用户可见文本（`#[error]` 字符串、`/status`、`"10m0s"`），
  没有泄漏到不打印 Go 文本的地方。
- **wire 层的 serde struct 镜像 API JSON**（`llm/*.rs` 的请求/响应 struct、`r#type: &'static str` 常量标签、
  `#[serde(untagged)]` 的 verbatim 回放包装）：请求字节由 golden 测试钉住，`ChatToolCall` 的键序更是被
  `provider/openai.rs:549` 钉住 —— 改成 `#[serde(tag)]` 前要先确认键序。
- **`multipart.rs` 手写**（reqwest 的 multipart 不能给出可克隆的 `Bytes` 体供重试和 `/debug` 抓包；三条转义规则有 golden）、
  **`sse.rs` 手写**（`BytesMut` + 偏移扫描 + `[DONE]` 规则）、**`reqlog.rs` 环形记录**：都是有据的工程决定。
- **`ui/theme.rs`、`ui/osc.rs`、`ui/surface/tabbed.rs:534-564` 的 SGR/OSC 字节表**：终端输出必须逐字节等于 Go，测试钉住。
- **`--max-turns` 负数 = 无限、`--resume` 的 bare/valued 双形态、`-m ""` 报错等 CLI 行为**：行为保持，只建议改内部类型
  （`Option<Option<String>>` 代替 `" "` 哨兵，`cli.rs:85-94`）。

**看似 Go 化但其实合理的**：

- **手写 `BoxFuture`**（56 处 `Box::pin(async`）：`dyn Provider`/`dyn Dispatcher`/`dyn Ui`/`dyn Tool` 都需要对象安全，
  原生 `async fn in trait` 在 rustc 1.98 仍不 dyn-compatible，所以要么 `async-trait`（同样装箱）要么手写。手写的代价
  是每个方法多 3 行和「trait 方法只是 `Box::pin(self.stream_internal(..))`」的双层（`anthropic.rs:620-628`、
  `openresponses.rs`），可以接受；换 `async-trait` 只省样板不改形状，**不建议为此动手**。
- **`Option<&dyn Cap>` 能力访问器**（`provider/mod.rs:220-262`）：比 `Any` downcast 或泛型约束更适合运行时决定的
  provider；调用方一行即可。真正需要修的只是 `Dispatcher` 的返回值三态（F9）。
- **`RunCtx` 显式 struct 代替 Go `context.Value`**、**`CancellationToken` 显式传参**：正确方向。
- **`CtxMeter::disabled()` 的空对象**（`repl/meter.rs:1-6`）：注释直说是复刻 Go nil receiver，但 Null Object 本身是
  合理模式，调用点无条件调用比到处 `if let Some` 干净。
- **`Repl` 的 `pub(crate)` 字段**：让命令处理器分文件是一个合理取舍；问题在 `run()` 栈上还剩十个状态没收进去（F4）。

**做得好的（Rust 优于 Go 的地方）**：

- `ReasoningGate` 用 `Drop` 实现 Go 的 `defer closeReasoning()`（`provider/sink.rs`）；`BusyGuard`/`ScopeGuard`/`PreviewWriter: Drop`/
  `TermGuard`/`RawGuard`（`ui`）—— RAII 全面替代 Go 的 stop 函数。
- `ChatResult`/`RoundResult` 随返回值携带 usage，消掉了 Go `LastUsage()` 的时序竞争（G1）；`Occupancy.last_usage` 用
  「消费即清空」表达 Go 的 per-call reset（`repl/meter.rs:75-87`）。
- `SessionWriter::update_meta(FnOnce(&mut SessionMeta))` 把 Go 的 8 个 `Set*` 合成一个闭包 API；`Drop` 替代 `Close()`。
- `HostDirs`/`EnvSource` 注入 + `ci.sh` grep 守卫「`session` 不读环境」；`TerminalSeam`/`open_ui`（`cmd/interactive.rs:102-241`）
  把终端所有权顺序做成可单测的 seam。
- `Payload` 枚举（`llm/client.rs:83-114`）统一 JSON 与 multipart 的发送路径；`should_retry`/`retry_delay` 纯函数 + 可注入
  `Jitter`；`Option<Raw>` 把 `"error": null` 建模为缺席（F-05）。
- `Raw` newtype 给 `RawValue` 加上 `PartialEq`，让 `Message`/`RoundResult` 可以 `==` 比较；`RawContent` 按 dialect 分变体，
  「只信任自己的变体」的回放规则由类型表达。
- google dialect 按字段存在性建模 `GPart`，零字符串匹配；`sanitize_content` 用 `Cow` 避免无谓拷贝。
- `ui` 的枚举（`PanelKind` `#[non_exhaustive]`、`ProgressState`、`SurfaceEffect`、`BlockKind`）、`keys.rs` 的编号优先级表、
  `build_frame(&FrameInput) -> FrameView` 的纯函数渲染核。
- `scan_records` 在分配前实现 32 MiB 上限；`credential_header` 标记 sensitive 并有 `Debug` 泄漏测试。
- `src/testing/` 作为 feature 门控的共享 fake 是正确选择（unit test 与 integration test 共用，无生产代码依赖）；`ScriptedUi`
  的「一个事件日志 + 一个脚本队列 + 三个形状不匹配 panic」可读。

---

## 5. 建议的重构顺序

### 5.1 可立即做、低风险（纯清理，零行为变化，每项 ≤ 半天）

1. **删除 14 个模块级 `#![allow(dead_code)]` 与过期理由注释**（F7）：让 rustc 报一遍，删真死代码（`paths::within`、
   `images_dir`、`set_draft`、`input_cursor_cols` …），测试用的加 `#[cfg(test)]`。顺手修 `ReplError` → `thiserror`、
   `provider/sink.rs:13` / `repl/turn.rs:24-27,494` 的失实注释、`mcp/mod.rs:3-5` / `shell/mod.rs:3` 的错误路径。
2. **把 in-file 测试拆到 `tests.rs` 子文件**（F12）：toolloop / event_loop / surface/* / region / frame / export 八个文件。
3. **合并十 crate 化石**（F6）：删 `markdown::sink::PreviewHandle` + `PreviewBridge`；`Transcript` 直接持 `Arc<dyn Ui>`；
   `McpServerView` → `ServerStatus`；`Raw`/`JsonObject` 移到 `llm`；`(Dispatcher, PrefixOf)` → `Option<&Arc<Manager>>`；
   改掉 33 处过期 crate 名注释。
4. **删除 pass-through**（F10）：`batch_message`、`models()` ×2、`clipboard.rs`、`Tui/TuiInner`、`set_draft`、`paths::absolute`；
   `outcome_of` 三处合一。
5. **去重**（F11 的小项）：一个 `query_escape`、一个 `OpenAiUsage` + `From`、一个 `lock` helper、`resume_into` 收拢三份回放、
   ui 测试 `testutil`。
6. **改名**（F15）：`pc`/`cfg`/`mgr`/`ptype` 等，IDE 重命名即可。
7. **错误变体做载体的地方改成真变体**（F14）：`InvalidId`、`UnknownProvider { aliases }`、`ContextWindow { label, source }`、
   `Ui(UiError)`；`Result<_, String>` 20 处逐个换成小枚举。

### 5.2 值得做、需要设计（行为不变，但改动面跨模块，需要先写 RFC 式的小设计）

1. **`ProviderError` 停止擦除 `LlmError`**（F1）：`Wire { op, source: LlmError }`，`describe_error`/`is_retryable` 改成 `match`，
   删除 4xx 字符串扫描。这是收益/风险比最好的一项跨模块改动：文本不变、测试现成。
2. **wire 层产出类型化事件枚举**（F2 + F11 的 `JsonEvents<T>`）：先做 anthropic（最集中，两个文件），再 openresponses；
   `BlockAcc.kind`/`stop_reason` 跟着变枚举。会话 blob 不受影响。
3. **repl 的 turn 状态收进一个类型**（F4 + F5）：`TurnEngine` 跨 turn 复用，`run_turn`/`tool_loop`/`walk` 变方法，
   `stream_round` 返回具名 struct，`TurnReport` 合并 `TurnOutput`/`TurnError` 的簿记；命令处理器统一 `(repl, arg)` 签名，
   `cancel` 只从 `repl.cancel` 取。建议在 (1) 之后做，因为 `describe_error` 简化后 `run.rs:826-860` 的重试分支会短很多。
4. **`Message` 改 sum type**（F3）：先加 `RoleData` 枚举并提供与旧字段等价的 accessor（`content()`, `tool_calls()`），
   dialect 逐个迁移，最后删旧字段。13 个文件；`SessionRecord` 不动，golden/差分测试可以逐字节验证。
5. **`ui::facade::Panel`/`PanelResult` 改 enum body，`PanelState` 去 `Cell`**（F3 + F4 的 ui 部分）：
   `render(&mut self) -> Rendered`，`SurfaceState` 拥有 `Vec<PanelSlot>`；repl 约 10 个构造点跟着改。
6. **零值语义清理**（F8）：`i64 → usize/u64/Option<NonZeroU32>`，llm 响应 `Option<String>`，`ProviderConfig` 类型化 accessor，
   `Dispatcher` 能力发现改 `as_*`（F9）。逐 struct 推进即可，不必一次做完。
7. **`cmd` 的 `Startup` 拆成 `RunContext` + 显式参数，`run()` 提取具名阶段**（F4 cmd 部分、`cmd/mod.rs:54-326`）。
8. **主题标志与进度 task-local 显式化**（F13）。

### 5.3 不建议做

- **用 `async-trait`/`trait_variant` 替换手写 `BoxFuture`**：形状不变，只省样板，还引入 proc-macro；不值得。
- **把 `Option<&dyn Cap>` 能力访问器换成泛型约束或 enum dispatch**：provider 类型在运行时由配置决定，`dyn` 是对的。
- **动 `session/record.rs`、`session/meta.rs` 的磁盘 struct 形状、`text::go_*` 格式器、任何 `Display` 文本、
  SGR/OSC 字节表、请求 JSON 的键序**：这些是互通契约。
- **为 `Ui` 的 29 个方法做大规模瘦身**：把 7 个 call-widget 动词收成 `Box<dyn CallWidget>` 是个好想法，但要动
  `Transcript`、`ScriptedUi`、`OrderUi` 三处，收益主要在测试替身的体积；排在 5.2 全部完成之后再考虑。
- **重写 `sse.rs`/`multipart.rs`/`reqlog.rs` 换第三方 crate**：现有实现有 golden 与明确的工程理由。
- **删除 `// Go: file:line` 锚点**：它们是双维护期间的导航；要删的是「计划态」叙述，不是锚点。

---

### 附：本评审用到的计数（2026-09-04 工作树，`grep -rn` 于 `src/`）

| 指标 | 数值 |
|---|---|
| `src` 总行数 / 其中 `#[cfg(test)]` 之后 | 69,762 / ~23,645 |
| `tests/` 行数 | 30,944 |
| `pub(crate)` 出现次数 / `pub (fn\|struct\|enum\|trait\|type\|const\|static)` | 1,508 / 704 |
| `BoxFuture` 引用 / `Box::pin(async` | 134 / 56 |
| `Option<&(mut )?dyn` | 48 |
| `PoisonError::into_inner` | 97 |
| `.clone()` | 586 |
| ` as (usize\|i64\|u64\|u16\|u32\|i32\|f32\|f64)` / `usize::from` | 33 / 39 |
| `// Go:` 锚点 | 444 |
| `#![allow(dead_code)]`（模块级） | 14 文件 |
| `Result<_, String>` | 20 |
| 3+ 元组返回 | 10 |
| `CONTRACTS` / `TUI_CONTRACTS` / `TUI_DESIGN` / `POLICY` / `DEVIATIONS(3)` 引用 | 85 / 72 / 33 / 26 / 49 |
| `WPnn` / `T-nn` / `D-nn` 引用 | 169 / 137 / 88 |
| 过期 crate 名（`iota-repl` 等）注释 | 33 |

---

## 6. 落地记录（2026-09-04，5.1 档）

5.1 的七项已全部落地，零行为变化：`cargo clippy --all-targets -D warnings` 零告警，1391 个测试通过（改前 1392：两份 `query_escape` 测试合为一份），`ci.sh` 全绿。相对改前快照：`src` 修改 81 文件、删除 1（`ui/clipboard.rs`）、新增 10（`llm/json.rs`、`sync.rs` 与 8 个拆出的测试文件）；`src` 总行数 69,762 → 69,274。

与本报告建议**有意偏离**的三处：

- **5.1-3 `(Dispatcher, PrefixOf)`**：没有改成 `Option<&Arc<Manager>>`，而是具名的 `assemble::McpPart { dispatch, prefix_of }` 加 `McpPart::of(&manager)`。理由：`build_dispatcher` 的测试用一个假 `Dispatcher` 驱动，换成真 `Manager` 会把测试绑到连接逻辑上。匿名元组穿四层的问题已消除。
- **5.1-4 `models()` 转发**：`chatcomp`/`responses` 的两个三行转发保留。它们让四个 dialect 的模型列表接口形状一致（anthropic/google 各有真实实现），删掉会让 openai 侧改调自由函数而其他侧调方法。只改掉了 `models.rs` 文件头里 crate 时代的理由。
- **5.1-5 resume 回放**：`cmd/mod.rs` 保留内联的 model 分支，只把两个裸 `bool` 换成 `session::Overrides`；`interactive.rs` 与 `/resume` 命令用新的 `replay_session_settings`。理由：headless 路径的 `ModelRequired` 检查必须夹在「回放 model」与「回放 tuning」之间，合成一个调用会把 tuning 的警告排到错误之前。

**未做、留待 5.2 或另起**：`Result<_, String>` 的其余 19 处；约 300 处 `CONTRACTS`/`WPnn`/`T-nn` 注释引用（F7 的大头）；`ui` 测试里 `Surf` ×7、`test_model()` ×4 等桩的去重（F11）；97 处内联的 `unwrap_or_else(PoisonError::into_inner)`（四个具名 `lock` 助手已收成 `crate::sync::lock`）。

改名（5.1-6）的取舍：表示根 `Config` 的 `cfg` 保留——它是 Rust 通行缩写；表示 `ServerConfig`/`ShellConfig` 的改为 `server_cfg`/`shell_cfg`。`sess` 改为 `resumed` 而非 `session`，因为 `cmd/mod.rs` 的同一函数里已有一个 `session` 局部变量。

### 5.2 进度

- **5.2-1 `ProviderError` 停止擦除 `LlmError`**：已落地（2026-09-04），设计与行为边界见 `docs/refactor/5.2-1-provider-error.md`。`Wire { op: WireOp, source: Box<LlmError> }` 替代三个 `BoxError` 变体（装箱是为了不让 `ChatError` 撞上 `result_large_err`），`describe_error(&ChatError)` 与 `is_retryable` 变成纯 `match`，`map_llm_err`、`has_non_429_4xx` 与 `src/` 里最后的 `downcast_ref` 一并消失。用户可见文本不变；测试桩改用 `ProviderError::other("boom")`，对应六处 `"stream error: boom"` 断言改为 `"boom"`。
- **5.2-2 wire 层产出类型化事件**：已落地（2026-09-04），设计见 `docs/refactor/5.2-2-typed-events.md`。anthropic：`AnthropicEvent`/`BlockKind`/`DeltaKind`/`StopReason`，`RespBlock` 改 `#[serde(tag = "type")]` 枚举；openresponses：`RespEvent` 十个变体加 `ItemKind`，`TypePeek` 消失。两个 dialect 的 provider 文件里不再有 `r#type.as_str()` 匹配。集成测试是 SSE 文本驱动，一行未改；只有 `llm/responses.rs` 的内联测试改为反序列化私有的 `RawRespEvent`。

- **5.2-4 `Message` 改 sum type**：已落地（2026-09-04），设计见 `docs/refactor/5.2-4-message-body.md`。`content`/`attachments` 留作共享字段，九个角色专属字段收进 `body: Body`（`System` / `ToolsMount` / `User` / `Assistant(AssistantBody)` / `Tool(ToolBody)`），`role()` 由 `body` 推导，`is_tools_mount()` 这类靠字段组合猜角色的谓词变成对变体的匹配。`SessionRecord` 不动，golden 与跨二进制往返测试验证会话字节不变。迁移由编译器错误驱动的脚本完成：79 处字段读取改 accessor、约 70 处字面量改构造器或 builder。
- **5.2-3 repl 的 turn 状态与返回值**：已落地 A、B 两片（2026-09-04），设计与暂缓理由见 `docs/refactor/5.2-3-turn-state.md`。A：`RoundOutcome`、`TurnReport`（`TurnError` 消失，`used_tools`/`side_fx` 只在 report 上，`run_turn` 末尾只赋一次）、`InterruptDecision`、`settings::Rows<T>`，`chat/run.rs` 两个分支都产出 `LoopOutcome`。B：`run()` 的十个栈变量收进 `Repl`，`TurnCtx` 由 `Repl::turn_ctx` 构造，overlay 刷新与标题任务变成 `Repl` 方法，六个命令改读 `repl.cancel`，`compact_now`/`offer_before_send` 不再传 `&mut u64`。C（`TurnEngine` 方法化）暂缓。
- **5.2-5 `ui::Panel` / `PanelState`**：A 片已落地（2026-09-04），设计见 `docs/refactor/5.2-5-panel.md`。`PanelState` 的 12 个 `Cell`/`RefCell` 全部变普通字段，picker 的五个缓存字段收成 `Option<PreviewCache>`；`render_surface` 拿 `&mut SurfaceState` 并返回 `Rendered { rows, cursor }`，`cursor_target` 与 `SurfaceState::cur` 消失，`frame_view` 改 `&mut self`。B 片（`Panel` enum body）与 C 片（`PanelSlot`）待做。
- **5.2-5 B 片**（`Panel` enum body）已落地：`Panel { title, prompt, search, height, refresh, body: PanelBody }`，八个 kind 各自的字段收进 `PanelBody` 变体；75 处字面量改构造器 + builder，读取点改 accessor；`PanelKind` 保留为 `kind()` 推导的标签供分发与 hint 表匹配；`PanelResult` 保持扁平（一次性回传，enum 化不挡任何错误）。1404 测试 + 全量 CI 通过。详见 `docs/refactor/5.2-5-panel.md`。C 片（`PanelSlot`）待做。
- **5.2-5 C 片**（`SurfaceState` 拥有面板）已落地：`SurfaceState { focus, enter_advances, slots: Vec<PanelSlot { spec, state }>, term_h }`，`new` 拿走 `Vec<Panel>` 所有权，`focused()` 拆借出 `(&Panel, &mut PanelState)`；`surface_key/render_surface/tick/paste` 变成 `key/render/tick/paste` 方法，键梯私有函数只收 `st`；`event_loop::SurfaceOpen` 与 `oneshot` 不再各存一份 `panels`，两处 `NEEDS [WP64]` 注释随之消失。1404 测试 + 全量 CI 通过。5.2-5 至此全部完成。
- **5.2-6 A/B 片**（零值语义：计数类型、Dispatcher 能力发现）已落地：`--max-turns` 在 CLI/YAML 边界折成 `Option<NonZeroU32>`（`turn_cap`），`TurnBudget`/`LocalCap` 随之无符号化；`context_window` 全链路 `u64`，只在 `SessionMeta::set_context_window` 处写回磁盘的 `i64`；会话计数 `usize`。`Dispatcher::as_owner / as_tool_searcher` 取代 `owns -> Option<bool>` 与 `search_tools -> Option<Vec>` 的三态，`impl Dispatcher for Arc<T>` 删除；`ServerStatus.err`、`ResponseHalf.err` 改 `Option<String>`。`header_summary` 保留 `Option<String>`（两态，注释改写）。1404 测试 + 全量 CI 通过。详见 `docs/refactor/5.2-6-zero-values.md`。C/D 片待做。
- **5.2-6 C/D 片**（`ProviderConfig` accessor、llm wire `Option`）已落地：`Effort::parse` 严格 + `Effort::optional` 承接四个「空 = 未设」边界，`ProviderConfig::effort()/image_gen_params()` 与 `SessionMeta` 同名 accessor 是唯一读原始字符串的地方，`ImageGenParams` 改 `Option<String>`；`llm::none_if_empty` 取代 `is_zero`，响应侧 `RespOutputItem`/`GFunctionCall`/`GFunctionResp`/`ModelsEntry` 与请求侧 `reasoning_effort`/`tool_call_id`/`thinking_level`/`GImageParams`/images `size` 及四个计数改 `Option`；请求 JSON 与 multipart golden 未改一字节。1404 测试 + 全量 CI 通过。5.2-6 完成；设计与偏离见 `docs/refactor/5.2-6-zero-values.md`。
- **5.2-7 `Startup` 拆分**已落地：`cmd::RunContext { dirs, http, transport, reqlog, resolver, cancel }`（Clone）与 `ToolAssembly { mcp_configs, mcp_defers, tool_env, interactor, delegator, agent }` 取代 16 字段的 `Startup`；`run()` 提成 `RunContext::new` → `open_provider` → `assemble_tools` → 输出格式 → `run_headless` / `interactive::run_interactive(Interactive { cli, settings, kind, provider, ctx, tools })` 六个阶段，错误顺序不变；`wire_session` / `resolve_context_window` 改显式依赖，两处 `mem::take` 消失。设计与记录见 `docs/refactor/5.2-7-startup.md`。
- **5.2-8 主题标志 / 进度 task-local**：A 片已落地——`ui::theme` 与 `repl::styles` 的两个 `AtomicBool` 删除，`input_bg(dark)` / `diff_shades(dark)` / `diff_code_theme(dark)`；`SurfaceState.dark` 与 `Model.dark` 由 `UiMsg::DarkBackground` 驱动（`Tui::start` 把探测值作为第一条消息发出），`Transcript::set_dark` 承接运行循环两处刷新时机，`oneshot::run_surface(spec, dark)` 让 picker 按真实探测值渲染。B 片（`TURN_PROGRESS`）决定不做：显式化要动 11 个文件 31 个 `cancel` 签名与全部 provider 测试，而作用域与读取各只有一处，理由记在 `docs/refactor/5.2-8-globals.md`。5.2-7 + 5.2-8A 合跑全量 CI 通过（1404 测试，跨二进制往返 `raw_restored=1`）。**§5.2 八项至此全部处理完毕**（3C `TurnEngine`、8B 进度 task-local 两处记录为不做）。
