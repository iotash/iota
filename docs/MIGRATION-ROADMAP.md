# iota：整体迁移到 Rust 的最终路线图

**日期**：2026-09-08 ｜ **状态**：已评审，可执行。
**产出方式**：由一次 74 个 agent 的审计产出并复核——12 名代码阅读者、54 名对抗性验证者、3 份独立提案、3 名评审、1 次合成、1 次完整性批判；批判提出的 33 条修正已逐条就地应用（凡与仓库事实冲突的断言均在 2026-09-08 的工作树上重新核实后再采信）。
**位置**：本文入库为 `rust/docs/MIGRATION-ROADMAP.md`；Phase 3 的文档处置把它搬到 `docs/history/MIGRATION-ROADMAP.md`（连同其余移植期文档一起冻结）。

> 合成自获胜提案 B（RISK），嫁接三位评审推荐的 A/C 要点，并逐条回应评审的共同异议。所有数字与坐标均在 2026-09-08 的工作树上重新核实；凡是本次核实与提案/评审不一致的地方，以本文为准。工作量单位为「一名工程师驱动 AI agent 的工程日」；只有你本人能做的终端人工验证另计（§4 总计）。

## 0 执行记录（2026-09-08，一步切换）

用户决定跳过分阶段试用，直接把仓库切到 Rust 原生状态。实际执行的提交序列（本地，未推送；远端 `origin` 仍重定向到 chatchain，推送前必须先建 `joyqi/iota` 并核对 `gh api`）：

| 提交 | 内容 |
|---|---|
| `786f894` go: land the last fixes | 17 个未提交的 Go 修复入库 |
| `7ceceea` rust: import the port | `rust/` 372 个文件原样入库；**标签 `go-final` 打在这里**（同时含 Go 树与 `rust/tests-go/gofix`） |
| `7aaef57` repo: retire the Go implementation | 删 Go 模块、goreleaser、Go workflows、根 `spikes/`、`rust/tests-go`、往返脚本、`rust/spikes`、`docs/SIZE.md` |
| `90378c2` repo: rust/ becomes the root | 纯改名：Cargo/src/tests/scripts/examples/docs 上移，站点 `docs/`→`site/`，设计文档→`docs/design/`，CI workflow 到 `.github/workflows/ci.yml` |
| （下一提交） | `.gitignore` 改 Rust 模板（brain 文件含 AGENTS.md 一并忽略）；`ci.sh` 去往返步骤、体积表改写到 `target/size.md`；`scripts/size.sh` 去 Go 对比；`scripts/check-stubs.sh` 改零容忍；`.github/workflows/ci.yml` 去路径过滤与 `working-directory`、装 tmux、`LANG=C.UTF-8`、apparmor sysctl；`pages.yml` 指向 `site/`；`Cargo.toml` 元数据（description/homepage/readme/publish=false）；`lib.rs` crate 文档；夹具 manifest 与 `tests/cmd/session.rs` 的来源说明；站点安装块；`docs/history/` 归档 EVALUATION/MERGE-PLAN/ARCH-REVIEW/refactor + ROUNDTRIP-FINAL；两份 README 合一 |

保留但未做的：Phase 1 的功能缺口修复（§3）、Phase 3 的发布管线、Phase 4–6 的测试树/结构/残留工作——路线图其余部分照旧适用，只是 Phase 0/2 已经在一次切换里完成。`docs/REPORT-PHASE-1.md` 原地未动。

## 1 现状判断

仓库 `/Users/joyqi/Work/iota` 的 `main` 是一个孤儿提交 `b67722d`（Go 模块），`rust/`（src 70k + tests 31k LOC，1 392 个 `#[test]` 加 14 个 tmux 场景，`ci.sh` 全绿）**整体未入库**（`git ls-files rust` = 0），另有 17 个 Go 文件未提交（Anthropic thinking 回放、`attachRawContent`、列表内公式、去 legacy 迁移；其中 8 个新增 Go 测试，6 个已有 Rust 同名测试，`TestAttachRawContent` 与 `TestFindConfigFile` 没有）；`origin` 指向 `git@github.com:joyqi/iota.git`，而 `gh api repos/joyqi/iota` 仍解析为 `joyqi/chatchain`（GitHub 重定向），并且 `joyqi/chatchain` 本身**尚未归档**（实测 `gh api repos/joyqi/chatchain --jq '{archived,visibility}'` = `{"archived":false,"visibility":"public"}`），本地还留着 `refs/remotes/origin/main` 与 `refs/remotes/origin/image-gen` 两个指向它的跟踪引用；Homebrew tap 只有 `Formula/chatchain.rb`。本地 44 个 chatchain 标签不是 `HEAD` 的祖先（`git describe` 直接失败），唯一风险是 `git push --tags`。功能对等在 macOS 上基本完成，审计确认的真实缺口只有 7 项（§3），其中 `edit_file` 对非 UTF-8 文件的静默改写是数据损坏级别。**但「Linux 上也基本完成」是推断而非事实：Rust 树很可能从未在 Linux 上编译或运行过**——`rust/.github/workflows/ci.yml` 位于 GitHub 不读取的路径、从未被 GitHub 执行过，开发机是 macOS，全部文档里找不到任何 Linux 运行记录（只有 `rust/docs/ARCHITECTURE.md:491` 一句「Ubuntu runner installs bubblewrap」的计划）。Rust TUI 从未通过自己的人工门槛（`rust/docs/TUI-VERIFY.md` 结果表全部为空），tmux L4 层从未在 GitHub 上执行（`tests/ui_tmux/main.rs:169-200` 的 `gate()` 只认 `IOTA_TMUX`，缺 tmux 时打印 `SKIP` 并通过）。代码「像移植品」的证据是可计量的：src 2 203 行 `*.go:NNN` 坐标、tests 607 行；`// Go:` 锚点 442 + 411；`WPnn` 149、`T-nn` 155、`D-nn` 137、`CONTRACTS` 99、`TUI_DESIGN` 30 处指向仓库外文档；559 个 `fn test_*`；8 个 `#[doc(hidden)]` 测试缝（`src/repl/mod.rs:37-51`）；20 个 `Result<_, String>`；18 个 `go_*` 命名；模块依赖存在 tool ⇄ chat::turns ⇄ mcp 与 markdown ⇄ mathtext 两个环，markdown 通过 `markdown/sink.rs:7` 依赖 `ui::facade`。Rust 侧真正依赖 Go 工具链的只有两处：`scripts/go-session-roundtrip.sh` + `tests-go/gofix`（其 `replace github.com/joyqi/iota => ../../..` 在 Go 树删除后**会失败而不是 SKIP**）与 `scripts/size.sh` 的 Go 重建分支；`examples/mkbundle.rs` 是**纯 Rust**（`use iota::session::SessionStore`、`use iota::provider::…`，不碰 Go 工具链），只是被往返脚本第 5 步调用，因此它的去留与 Go 退役无关（§2.1 保留它，理由见 §2.5）。结论：先消除「一份工作副本」的丢失风险，再修数据安全缺口、在真实 Linux 上跑通一次、并在用户自己的终端上试用，然后切换仓库、退役 Go、尽早发 1.0，最后做用户不可见的结构与注释工作——每个阶段结束时产品都可发布。

## 2 目标终态

### 2.1 仓库布局

```
iota/                          # git 根 = cargo package；Go 树不存在
├── Cargo.toml · Cargo.lock · rust-toolchain.toml（钉 1.98.0）· deny.toml（仅 advisories+licenses）· ci.sh
├── dist-workspace.toml        # cargo-dist；或手写 release.yml（见 2.6 备选）
├── src/                       # 见 2.2
├── tests/                     # 九个领域二进制 + ui_tmux + fixtures + 精简后的 common/{wire,session}.rs + layering.rs
├── examples/mkbundle.rs       # 保留：唯一的 Rust 侧 bundle 生成器（见 2.5「格式冒烟样本」）
├── scripts/
│   ├── check-deps.sh + direct-deps.allow       # 保留（记录在案的决策）
│   ├── check-residue.sh + residue-baseline.txt # 新：残留计数棘轮门（含 check-stubs.sh 唯一还有用的规则：no todo!()）
│   ├── code-tokens.py                          # 新：剥注释后的 token 流 diff，纯注释 PR 的机械门（词法要求见 2.4.2）
│   ├── export-go-map.sh                        # 新：一次性导出 // Go: 锚点 → docs/history/go-test-map.tsv
│   └── size.sh                                 # 仅 Rust 一行 + 新增 --budget 旗标；Go 分支删除；输出到 target/ 不再提交
├── docs/                      # 工程文档：ARCHITECTURE.md · COMPAT.md · TESTING.md · TUI-VERIFY.md · design/*.md · history/
├── site/                      # iota.sh 落地页（index.html, og.png, favicon.svg）；pages.yml path: site
├── .github/workflows/{ci.yml, release.yml, pages.yml}
├── README.md · CHANGELOG.md · LICENSE · AGENTS.md（入库与否见 §6.17）
├── CLAUDE.md · BRAIN.md · .mindmux/ · .claude/   # 仍按 .gitignore:33-38 忽略
└── .gitignore                 # Rust 模板：/target、.iota.*、上面的 brain 行；Go 模板行删除
```

删除（只存在于标签 `go-final`）：`main.go`、`go.mod`/`go.sum`、`cmd/ chat/ config/ provider/ tool/ mcp/ internal/`、根 `spikes/`、`.goreleaser.yml`、`.github/workflows/{test,release}.yml`（Go 版）；`rust/tests-go/gofix`、`rust/scripts/go-session-roundtrip.sh`、`rust/spikes/ratatui-inline`、`rust/.github/`、`rust/docs/SIZE.md`。**不删** `rust/examples/mkbundle.rs`：它是纯 Rust，且是「Go 读 Rust 新建的 bundle」这条手工验证路径唯一的 bundle 生成器——headless `-m` 不创建会话（`rust/README.md:180`：「headless runs resume sessions but do not create them」），删掉它以后第 5 步只能靠人工开 TUI 造一个会话。它改以「会话格式冒烟样本」的身份留下（模块文档重写，不再自称往返脚本的输入），并在 Phase 4 与 `tests/session/roundtrip.rs` 共用同一份记录形状清单。顺手修正：`tests/fixtures/sessions/manifest.json:3` 的 `../../crates/iota/...` 路径与 `go run . gen` 再生说明、14 处仓库内 `rust/` 路径引用、`rust/README.md` 的 `cd rust`。`docs/index.html` 不链接 `docs/design/*.md`，所以设计文档离开公开站点没有损失。

### 2.2 src 模块树

图例：KEEP 不动；MOVE `git mv` + 路径修正；SPLIT/MERGE/RENAME 以编译器为清单的机械重构。

```
src/
├── main.rs · lib.rs（重写 crate 文档：分层与公开面；删除 "a Rust port … mirroring the Go module's layout"）· sync.rs（唯一锁习语；130 处内联 PoisonError 收敛）
├── app/         MERGE app.rs+vars.rs+paths.rs → mod.rs · env.rs（唯一环境缝 `trait Env`，取代 VarResolver/EnvSource/cmd::EnvResolver）· vars.rs · paths.rs
├── config/      SPLIT config.rs → mod.rs（`Config::provider(name) -> Result<ResolvedProvider, ConfigError>` 取代 get+check_provider_name 元组两步）· provider.rs（12 个 "" 字段→Option；唯一的 api_key 优先级，今天四份）· mcp.rs · yaml11.rs（MOVE 自 tool/）
├── text/        width.rs KEEP · ansi.rs（成为唯一 CSI/OSC 扫描器，markdown/ui/tests 三份并入）· fmt.rs（go_quote/go_float/go_duration → quote_debug/float_short/duration_short，字节不变）
├── llm/         KEEP 文件；models.rs MERGE 进 client.rs；新 stream.rs：一个泛型 `DialectStream<E>` 取代四个孪生 next()（ARCH-REVIEW F11）；响应字段 ""→Option（google.rs 五处文档/类型不符修正）
├── provider/    mod.rs SPLIT（kind.rs 分出 ProviderKind/Effort；测试移到 tests.rs）· core.rs（←common.rs）· usage.rs（合并 usage_conv）· images.rs（合并 image_util）· Role 枚举（20 处字面量）· 图片 provider 错误类型化
├── tool/        mod.rs（契约）· context.rs（MOVE 自 chat/turns.rs：RunCtx/TurnBudget/DelegationLedger/ArtifactSlot——打破 tool⇄chat⇄mcp 环，13 个导入点）· approval.rs（`Approval::{Allow, Deny(String)}` 取代两处 (bool,String)）· dispatch.rs（←registry+merge；PrefixOf 返回 Option）· args.rs（读取器返回 Option，无零值默认）· display.rs（←fmt.rs）· defer/{mod,modes}.rs · builtins/{mod(←sets.rs),shell,agent,delegate,ask,code/{mod,tools,walk,udiff}}.rs
├── shell/       exec.rs（`enum Outcome { Exited, Killed, TimedOut, SpawnFailed }`）· sandbox/{mod,darwin,linux}.rs
├── agents/      KEEP
├── mcp/         manager.rs 吸收 naming.rs+status.rs；`ServerState::{Pending, Connected{segment,tools}, Failed{error}}`；重复 wire-name 告警走 Streams::warning；clientInfo 版本来自 CARGO_PKG_VERSION
├── engine/      RENAME chat/ → engine/（两个前端共用的回合引擎）：mod.rs run.rs batch.rs delegate.rs report.rs images.rs error.rs；turns.rs→tool/context.rs；once.rs→cli/headless.rs；execute_with_tools 8 参数→`TurnParams` 结构
├── session/     KEEP 全部文件；`SessionStore::create(NewSession{..})` 取代位置参数 `"", "", false`（13 处）；rawcodec.rs→raw.rs；fs.rs 收拢 0644 助手与 serde 谓词
├── markdown/    mod.rs（Writer over `enum Block { None, Fence, Table, List, Quote, Math }`，取代 5 组 in_* 标志/缓冲/预览句柄）· blocks/{code,table,list,quote,math}.rs（解析+渲染同处）· preview.rs（**在此定义** PreviewHandle，ui 实现）· inline.rs style.rs link.rs highlight.rs（删 PlainIndent 与单实现 trait）· sink.rs · export.rs（←html.rs）
├── mathtext/    成为叶子：删 MathRenderer trait + Mathtext ZST，markdown 直接调用；删 rune 索引孪生扫描器与死的公开 delimiter API；AST ""→Option；文件拆分保留但模块文档改为流水线描述
├── imgterm.rs   KEEP（ImgtermError 保留 source）
├── host/        KEEP；background.rs 用 tokio timeout 取代手写 wait_timeout；Env 闭包包→app::env
├── ui/          facade.rs（唯一 pub；PanelResult 按 body 枚举、`TabbedOutcome::{Cancelled, Committed}`、label/desc/height/width/refresh/model → Option）· runtime/{handle,msgs,event_loop,term,osc,oneshot}.rs · render/{region(Preview/CallClock 具名),frame,spans,theme(←src/ui/theme.rs，见 §3 #2 的 ColorMode 归属),sink,debug}.rs · input/{editor(新：Composer 与 Field 共用的一个行编辑器),composer,keys(`impl Model { fn on_key }`),paste,suggest,state}.rs · surface/{state(PanelState{.., kind: KindState}),panels,search,field,clipboard}.rs · testutil.rs（七份 Surf 收敛）
├── repl/        mod.rs · state.rs（`Repl` 拆成 Conversation/SessionSlot/UiHandles，今天 28 字段）· turn/{mod,tools(←toolloop),retry,phases,steer,interrupt,approval,interact}.rs · render/{transcript(+group),sink,styles(接收颜色标志),diff,banner,replay,mcpreport}.rs · context/{meter,tokens}.rs · commands/{mod(表驱动派发，替代 run.rs:604-680 的 if 阶梯),file,session,model/{mod,settings,system},compact,export,status,tools,debug,edit/{mod,picker},save,skills}.rs · title.rs errors.rs
├── cli/         RENAME cmd/ → cli/：mod.rs（RunContext→open_provider→assemble_tools→headless|interactive）· args.rs（←cli.rs；`#[command(version)]`）· error.rs（ArgsError/SetupError/RunError 取代 33 变体 CliError）· resolve.rs（RunSettings Option 字段）· headless.rs（←chat/once.rs）· interactive/{mod,picker,title}.rs（拆 927 行）· list.rs tuning.rs（合并 window.rs）assemble.rs delegate.rs signals.rs · io.rs（`Streams::warning` 加 "Warning: " 前缀，唯一告警出口；穿过 9 个函数的闭包删除）
└── testing/     feature `testing`，零额外依赖：mod.rs（StaticDispatcher/FakeToolProvider/MapEnv）· providers.rs（FakeProvider builder 取代 18 个手写 impl Provider）· dispatchers.rs · ui.rs（ScriptedUi + printed/surfaces/busy_labels 访问器）· repl.rs（ReplFixture 取代 21 处 RunParams 字面量）· session.rs
```

依赖方向（无环）：`app ← config ← text ← llm ← provider ← tool ← {shell, agents, mcp} ← engine`；`session ← {app, provider}`；`markdown ← {text, mathtext}`；`ui ← {text, markdown::preview}`；`host ← ui`；`repl ← {engine, tool, mcp, session, markdown, mathtext, imgterm, host, ui::facade}`；`cli ← 一切`。`tests/layering.rs` 用 `CARGO_MANIFEST_DIR` 走读 `src/`，把 ci.sh 的三条 grep（ratatui/crossterm 只在 ui、session 不读环境、image 只在 imgterm）加上新规则（tool/mcp 不引用 engine；markdown/mathtext 不引用 ui；每个 `COMPAT X-nn` 引用在 docs/COMPAT.md 中存在；每个 `Pinned by <file>::<fn>` 在 `tests/` 里能找到同名 `fn`）变成 `cargo test` 可见的断言。

关于 PreviewHandle 方向：brain 页 `rust-arch-cleanup`（2026-09-04）记录的是「渲染器依赖 UI 契约，ui 不依赖 markdown」。本路线图**推翻**它：消费者定义自己需要的 trait 是 Rust 的常规做法，markdown 处在 ui 之下的层，而 `ui::facade` 已经在实现 sink；落地时必须在该页追加 `kind: reversal` 时间线并写明理由，否则不改。

### 2.3 crate 形状

一个 package `iota`，lib + 薄 bin，edition 2024，无 workspace（2026-09-02 的合并决策成立：一个产物、无下游 crate 消费者、分层用模块图测试而非 manifest 表达）。公开面策略：默认 `pub(crate)`；`pub` 只留给 lib.rs 文档列出的层——`cli::run`、`provider`、`llm`、`tool`、`session`、`engine`、`markdown`、`mathtext`、`config`、`app`、`testing`——注明「对本二进制及其测试稳定，无 semver 承诺」。8 个 `#[doc(hidden)]` 缝通过把测试移入 crate 内（`src/repl/*/tests.rs`）删除；22 个只为测试而 pub 的 session 项、google.rs 全 pub 的 wire 结构、`SET_NAMES`/`merge_result` 降为 `pub(crate)`。`testing` feature 保留（自 dev-dependency 只在 `cargo test` 打开），但**不**挂任何 optional 依赖：wiremock/tempfile 助手留在 `tests/common/{wire,session}.rs` 作普通 dev-dependency——这样 `publish = false` 是当下的选择而不是被迫的；若日后要上 crates.io（名字 `iota` 已被 dtolnay 的 0.2.3 占用），改 `[package] name = "iota-cli"` + `[[bin]] name = "iota"` + `[lib] name = "iota"` 即可，不需要重构 fakes。

manifest：description 去掉 "cross-platform"（Windows 决策前）与 "(Rust port)"（`Cargo.toml:3`），加 `readme`/`keywords`/`categories`/`exclude`，删未用的 `anyhow`（`Cargo.toml:40`），并**必须加 `homepage = "https://iota.sh"`**——cargo-dist 生成的 Homebrew formula 直接从 Cargo.toml 取 `homepage`/`description`（今天 `.goreleaser.yml:38-39` 是手写的 `homepage: https://iota.sh` 与 `description: A lightweight cross-platform AI chat CLI`，正是 formula 里的那两行），Cargo.toml 今天没有 `homepage` 字段，不补就会产出一个没有主页的 `iota.rb`。`rust-version = "1.98"` 保留，README 里那句 MSRV 说明是**硬需求而不是可选的润色**：即便不上 crates.io，`cargo install --git` 也**不读取**源码树的 `rust-toolchain.toml`，用户机器上的 stable 只要低于 1.98 就会直接编译失败——README 必须写明「MSRV = 钉住的工具链；`cargo install --git` 需要 rustc ≥ 1.98；与 rust-toolchain.toml 同步升级」。lints 不变（forbid unsafe；deny unwrap/expect/panic；clippy::pedantic；`missing_docs = "warn"`，见 `Cargo.toml:125`）。Windows 不是本 crate 的构建目标（`nix` 无条件依赖 `Cargo.toml:59`；`shell/exec.rs` 用 process_group/nix pipe/killpg/unix pipe），除非用户决定投入（§6.3）。

### 2.4 约定

1. **文档注释**：每个条目的 doc 用现在时、自包含地说明它保证什么、服务哪个外部契约（API、文件格式、终端协议、产品规则）。改写分三类：(a) 只含坐标/票号/章节号的句子 → 删除；(b)「Go 做 X，这里做 Y 因为 Z」→「Y 因为 Z」，**禁止删掉带 because 的内容**；(c) 标记字节/格式钉子的坐标 → 换成钉住它的 Rust 测试名（`// Pinned by tests/session/golden.rs::meta_json_layout`）。

   **(a) 类的补充规则（否则 CI 必红）**：`Cargo.toml:125` 是 `missing_docs = "warn"`，而 `ci.sh:18` 的 `cargo clippy --all-targets -- -D warnings` 会把它升级成错误。审计发现约 60 个条目的**全部**文档就是一句坐标（例如 `src/tool/code/tools.rs:87` 的 `/// code.go:249-300.`），按 (a) 直接删就是编译失败。因此：**删除坐标后条目若无文档，必须补写一句现在时的行为句**——来源是读函数体加 `git show go-final:<path>` 看原实现，不是猜。删除后剩下名词短语（不成句）的，同样要补成完整的现在时句子。三个来自真实文件的对照样例：

   ```rust
   // 例 1 — src/tool/code/tools.rs:87（`impl Tool for Glob` 的 `call`）
   // 之前：
   /// code.go:249-300.
   // 之后：
   /// Runs the glob match on a blocking thread and returns its tool result; the walk
   /// touches the filesystem, so it never occupies an async worker.

   // 例 2 — src/mathtext/symbols.rs:200（`symbol_rune`）
   // 之前：
   /// Greek first, then operators (symbols.go:176 `symbolRune`).
   // 之后：
   /// Resolves a macro name to a single rune, trying the Greek table before the operator
   /// table; returns `None` when neither table knows the name.

   // 例 3 — src/ui/region.rs:248（`screen_height`）
   // 之前：
   /// Screen height with the 24-row fallback (region.go:151-159).
   // 之后：
   /// Reports the terminal height the staging window sizes itself against, falling back
   /// to 24 rows while no height has been reported yet.
   ```

   注意例 3：把坐标删掉后剩下的 “Screen height with the 24-row fallback” 是名词短语而不是句子——这正是本规则要挡住的产物。例 2 的「Greek first, then operators」同理，它描述的是实现顺序而不是这个函数保证什么。

   **(c) 类与测试改名的顺序**：`// Pinned by tests/...::<fn>` 的引用会被 §2.4.3 与 Phase 6 的「测试改名、文件按被测单元改名」悄悄弄失效。规则：**(c) 类改写在 Phase 4 的测试改名之后进行**（Phase 5 的结构 PR 若要顺手写 (c)，只能引用当时已定名的测试）；同时 `check-residue.sh` 增加一条校验——每个 `Pinned by <file>::<fn>` 必须能在 `tests/`（或 `src/**/tests.rs`）中 grep 到对应的 `fn <fn>`，否则失败。两条同时生效，顺序规则防止大批返工，校验规则兜住漏网的。

   每个 `mod.rs` 三段：Responsibility / `# Invariants`（具名，如 FINITE_POLL_DEADLINE、RECREATE_ON_HEIGHT_CHANGE、ONE_ENTRY_ONE_ROW、TAIL_KEEP，取代 spike 的 W1–W10 与 `TUI_CONTRACTS §n`）/ Talks to。历史（"formerly a #[path]-mounted…"、"merged 2026-09-02"）只进 `docs/history/`。代码里唯一允许的外部引用是 `COMPAT X-nn`（ids 冻结，§2.7）。

2. **残留门 `scripts/check-residue.sh`**：只用精确 token，不封普通英语词。禁止：`\.go:[0-9]+`、`// Go:`、`\bWP[0-9]{2}\b`、不带 `COMPAT ` 前缀的 `\b[TDIF]-[0-9]{2}\b`、`CONTRACTS`、`TUI_DESIGN`、`DEVIATIONS`、`TEST_PLAN`、`\bPOLICY\b`、`WORK_PACKAGES`、`SESSIONS_DESIGN`、`\bT3_`、`PROBE-RESULT`、`wart W[0-9]`、`crates/iota`、`iota-rs`、`iota-repl`、Go 库名（fatih、bubbletea、bubbles、lipgloss、cobra、goldmark、chroma、runewidth、uniseg、tiktoken-go、go-udiff、termenv）、`todo!(`。加校验：每个 `Pinned by <file>::<fn>` 在测试树中存在同名 `fn`（见上）。**不**禁止 `\bGo\b`/`parity`/`twin`——它们由 (b) 类人工改写处理，脚本另出一份 `--report` 清单供审阅。

   **`[TDIF]-nn` 规则会误伤，必须配一条改写规则**：`docs/COMPAT.md` 之外，`tests/` 与 `docs/design/` 里有大量**合法**的 `I-07`/`D-37`/`T-16` 引用（例如 `src/ui/region.rs` 模块文档里的 T-01/T-02/T-16、`src/ui/region.rs:50` 的 T-40）。规则是：**凡引用账本 id 一律写成 `COMPAT I-07` 形式**，并且**从 Phase 1 就开始用这个前缀**（那时账本文件还叫 `DIVERGENCES.md`，前缀先行不影响任何东西）。若拖到 Phase 2 才开始，提交 C 建立的初始基线里会混进几百条本应保留的合法引用，棘轮从第一天起就名不副实。

   作用域 `src/ tests/ scripts/ ci.sh Cargo.toml README.md docs/`，排除 `docs/history/**`、`CHANGELOG.md`、`docs/COMPAT.md` 的前言、`tests/fixtures/**`（fixture 是数据：manifest.json 的 `generator`/`go_version` 字段是来源记录，保留；只改 `doc` 字段）。**棘轮**：`scripts/residue-baseline.txt` 记录每文件计数；CI 失败条件 = 总数超过基线，或 PR 触碰的文件（`git diff --name-only base...HEAD`）计数 > 0。

   **`code-tokens.py` 的词法要求**（纯注释 PR 的机械门只有在这些都对时才成立）：Rust 的 `///` 会展开成 `#[doc = "…"]` 属性，普通「剥 `//` 到行尾」的词法器不够；还必须正确处理原始字符串 `r#"…"#`、嵌套块注释 `/* /* */ */`、字符字面量 `'/'`（不能当成注释起始）。二进制哈希对比**不可用**：`panic::Location` 把行号编进 release 二进制，改注释行数二进制就变。因此优先实现为 `examples/` 下的一个小工具，用 `proc_macro2`（已是传递依赖）把两棵树解析成 `TokenStream` 并剥掉 `doc` 属性后比较；若坚持 Python 版，上述四种词法必须逐一覆盖并各有一个自测样例。

3. **测试锚点**：先由 `scripts/export-go-map.sh` 一次性导出为 `docs/history/go-test-map.tsv`（Rust 测试 → Go 测试 @ `go-final`），再逐文件删除；测试名是行为句子，无 `test_` 前缀；文件按被测单元命名；字节钉子分类：格式 golden（session bundle、JSON report、请求体、multipart、SGR/OSC 表、mathtext/export golden）保留并提供 `UPDATE_GOLDENS=1` bless 路径（**由 Phase 4 步骤 1 落地**，覆盖 `tests/fixtures/mathtext/goldens-2d.txt`、`tests/fixtures/mathtext/goldens-inline.txt`、`tests/fixtures/mathtext/glyph-widths.txt`、`tests/fixtures/export/sample.md`、`tests/fixtures/export/sample.html` 五份），「Go 的精确文本」钉子改成同断言的行为规格（诚实命名）。今天 `UPDATE_GOLDENS` 在 src/tests/scripts/ci.sh 中零命中，是纯新增。

4. **类型**：`Option<T>` 表缺席（不再用 ""/0/-1/bool+payload）；互斥结果用枚举（shell::Outcome、mcp::ServerState、ui::PanelResult、TabbedOutcome、tool::Approval、markdown::Block）；跨模块边界的 ≥3 元组改具名结构；计数用 u64/usize。磁盘结构（session record/meta）、wire JSON、SGR/OSC 字节表、Display 文本是互操作契约，不动。

5. **错误**：每模块一个 thiserror 枚举、`#[source]` 链；`Result<_, String>` 归零（20 处）；`BoxError` 只在 delegate 工厂边界；Display 文本由测试钉住，测试不再从私有助手重建期望。

6. **命名**：模块按职责命名，不按来源的 Go 文件；无 `go_*` 助手名。`go_*` 逐个分类：`go_quote/go_float/go_duration` 产生用户可见文本（ARCH-REVIEW §5.3 第 633/720 行视为契约）→ 只改名、字节不变；`go_base/go_ext/go_abs` → std::path + 两个边界测试，字节不变；`go_pad` → 按显示宽度补齐（修 `…` 缺陷，单独 PR）；`go_value/go_map`（`src/repl/tokens.rs:120-137`、`:144`，压缩提示词的参数渲染）→ 改名 + 可能的行为变化，**但幅度比字面看起来小得多**：`go_value` 对数组/对象**今天已经**输出 compact JSON，其 doc 自述「Composite values (arrays, objects) print as COMPACT JSON where Go would print `[1 2]` / `map[k:v]`」，是移植时的有意选择。与 Go 字节不同的只剩顶层 `map[k1:v1 k2:v2]` 外壳（`go_map`）与标量的少数写法（`null` → `<nil>`）。也就是说这里**已是半对等**，全面改 compact JSON 的风险与影响面都比原提案暗示的小，但它仍会改变模型看到的压缩摘要文本，因此仍是单独 PR + CHANGELOG 行 + 用户拍板（§6.8）。

7. **并发**：`crate::sync::lock` 处处使用；`Arc<Mutex>` 只用于真正跨任务共享（repl 两处单一所有者改普通字段）；手写 `BoxFuture` 保留（对象安全 async trait、无 proc-macro）；`TURN_PROGRESS` task-local 保留并在 lib.rs 记为唯一例外。

8. **lints**：测试豁免只在两处——`lib.rs` 的 `cfg_attr(test, allow(...))` 与每个 `tests/<area>/main.rs`；今天 108 个文件级 `#![allow(clippy::unwrap_used…)]` 头与 13 个 `#![allow(dead_code)]` 删除；生产代码的 clippy allow 需一行理由；7 个「为了与 *.go 可 diff」的 `match_same_arms` 删除。

9. **诊断**：`tracing` 不再编译掉（`Cargo.toml:41` 的 `release_max_level_off`）：`IOTA_LOG=<path>` 安装文件 subscriber（INFO）；用户可见告警一律走 `cli::io::Streams::warning`。

10. **版本**：`iota --version` 打印 `CARGO_PKG_VERSION`（删 `cli.rs:17` 的 `disable_version_flag`）；MCP clientInfo（`mcp/manager.rs:39,62` 硬编码 `1.0.0`）来自同一常量；CHANGELOG 同源。

### 2.5 测试

金字塔保留：单元测试随代码（`foo/tests.rs`）；九个集成二进制 `tests/{provider,tool,mcp,session,engine,markdown,mathtext,repl,cli}/main.rs`（chat→engine、cmd→cli 随重命名）；`tests/ui_tmux`（Rust 门 + 14 个 bash 场景 + SSE mock），在 CI 里**真的执行**：runner 安装 tmux，`IOTA_TMUX=1 IOTA_TMUX_REQUIRED=1`，缺 tmux/bash/mock 端口时**失败**而非 SKIP，二进制用 `CARGO_BIN_EXE_iota` 而不是在测试里跑 `cargo build`；`tests/layering.rs`。一层 fakes（§2.3）。

**「缺依赖就失败」必须同样覆盖沙箱测试**，否则 Linux 沙箱在 CI 上等于零覆盖：`tests/tool/shell.rs:344-352` 与 `:484-492` 在 `exec::available()` 为假、或 `sandbox_runs()` 探测失败时打印 `SKIP:` 并**通过**。Ubuntu 24.04 默认 `kernel.apparmor_restrict_unprivileged_userns=1`，GitHub runner 上 `bwrap` 极可能静默跳过——绿勾什么都不证明。加 `IOTA_SANDBOX_REQUIRED=1`：置位时这两处（及同类的 SKIP 分支）改为 `panic!`；workflow 侧要么 `sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0`，要么确认 Ubuntu 自带 bubblewrap 的 AppArmor profile 已生效（二者取其一，实施时以第一次 Linux 运行的实测为准）。

fixture 与来源：`tests/fixtures/sessions/`（Go writer 写出的 5 个 bundle + manifest）永久保留为冻结的 v1 格式语料，`tests/fixtures/README.md` 写明来源与「不可再生；新 fixture 由 Rust writer 另起名」；mathtext（128 2D + 176 inline + 148 宽度，落在 `tests/fixtures/mathtext/{goldens-2d.txt,goldens-inline.txt,glyph-widths.txt}`）与 export golden（`tests/fixtures/export/{sample.md,sample.html}`）保留为本项目的渲染契约，`UPDATE_GOLDENS=1` bless 路径**在 Phase 4 步骤 1 与 fakes 收敛一起落地**（今天该环境变量零命中，是纯新增）；唯一钉 go-runewidth 东亚模糊宽度的测试改写为 unicode-width 行为。

**Go 往返的替代——退役后仍然存在的三条证据链**（写清楚是为了避免「oracle 全部丢失」的误解）：

1. `tests/cmd/session.rs:315-320` 已经钉住「Go 写的 meta.json 里未知键 `future_key` 在 Rust 重写后原样存活」——这正是往返脚本第 4 步 `-expect-meta-keys future_key` 所证明的事，且是在 `cargo test` 里、无 Go 工具链地完成的。
2. `tests/session/golden.rs:16-31` 内嵌 Go writer 产出的 `meta.json` 字节（`json.MarshalIndent`、struct-order keys、无末尾换行）与其 `messages.jsonl` 三行，作为常量断言。
3. `tests/fixtures/sessions/` 的 5 个 Go 写的 bundle + manifest，由 `tests/cmd/session.rs`（tier A：真二进制恢复 Go 写的 bundle、走 wiremock 追加一轮）驱动。

再加新增的 `tests/session/roundtrip.rs`（create → append → compact → resume → load，覆盖 `examples/mkbundle.rs` 列举的每种记录形状：system、带附件的 user、带 tool call 与 raw payload 的 assistant、tool result、带 usage 的 assistant）——图中无 Go 工具链。**真正只由 `gofix` 证明、退役后没有自动化替代的只有一条：「Go 能读 Rust 写的 bundle」**（往返脚本第 5 步）。这条只在 Phase 1 的回退窗口里有意义——那是唯一可能拿 `iota-go` 去读 Rust 写的会话的时期——所以 **Phase 1 全程保持 `go-session-roundtrip.sh` 留在 `ci.sh` 里跑**（现状如此，不动），Phase 2 做一次归档式最终运行后即退役。

**从 `go-final` worktree 手工重跑的正确写法**（`docs/TESTING.md` 必须这样写，否则是假 oracle）：`scripts/go-session-roundtrip.sh:22` 是 `cd "$(dirname "${BASH_SOURCE[0]}")/.."`，其第 3、5 步用的是 `cargo run -q --bin iota` / `cargo run -q --example mkbundle`——在 `go-final` 的 worktree 里整脚本重跑，测的是 **go-final 时代的 Rust**，不是当前 HEAD，证明不了任何事。正确步骤：在 worktree 里**只**做 `go build` 出 `gofix` 并执行它的 `serve`/`verify`；第 3、5 步用**当前树**的产物——归档前给脚本加一个 `IOTA_BIN` 覆盖（缺省 `cargo run -q --bin iota`）与 `MKBUNDLE_BIN` 覆盖，重跑时传 `IOTA_BIN=<当前 target/release/iota>` 与当前树的 bundle 生成器。这一改动属于 Phase 2 步骤 1 的「归档前收尾」，不是可选润色。

缺失的孪生测试补齐：`TestAttachRawContent`、`TestWatchToolComposing`、`TestInputCursorColsCJK`、`TestFindConfigFile`（未提交 diff 新增）、`-m -` 端到端 stdin、NO_COLOR/TERM=dumb 解析器、edit_file Latin-1 字节保真、Responses 空 schema 省略 `parameters`、`--version`、Composer/Field 键表。11 处真实时钟 sleep 改 tokio paused clock。

**UI 触碰规则**（回应评审异议 2）：任何改动 `src/ui/**` 或 `src/repl/render/**` 的 PR 必须在描述里点名钉住该不变量的 tmux 场景（01 启动/状态行、02 流式与暂存区、03 surface 自愈、04 ESC 中断、05 双 Ctrl-C、06 resize、07 粘贴、08 大会话回放、09 非 TTY、10 短窗口钉子、11 export 选择器、12 debug、13 edit 选择器、14 OSC 信号），并在合并前于 Ghostty + Terminal.app 上跑 TUI-VERIFY 对应小节（≤30 分钟）并把结果行写入 PR。**这是一笔只能由你本人支付的成本**：Phase 5 的 5c/5e 会产生十几个触碰 UI 的 PR，逐个验证合计数小时；Phase 1 的 TUI-VERIFY 全表（7 节 × 2 终端）同理。因此 §4 总计单列「用户人工验证时间」，且 **Phase 5 建议把 UI 触碰 PR 按批次合并后一次验证**（同一批内仍然一模块一 PR，只是验证按批做），把十几次 30 分钟压到三四次。

基线：Phase 0 用 `cargo test` 各二进制的 `test result:` 行记录到 `docs/history/baseline-2026-09.md`（不再用记忆中的 1404）；门槛按二进制比较，合并/删除的测试须在 PR 中点名。

### 2.6 CI 与发布

`.github/workflows/ci.yml` 在根目录，无路径过滤；矩阵与发布目标同架构：macos-14（arm64）、macos-15-intel、ubuntu-24.04、ubuntu-24.04-arm；步骤：`rustup toolchain install`（读 rust-toolchain.toml）、apt/brew 装 bubblewrap（Linux）+ tmux、缓存、`./ci.sh`；独立 job `cargo deny check advisories licenses`（cargo-deny-action；**不**用 `[bans]`——ARCHITECTURE §11:447/508 明确否决了用 deny.toml 做直接依赖白名单，理由仍成立，`check-deps.sh` 保留；advisories/licenses 是该决策从未覆盖的部分，记为 amendment 而非 reversal）。

**workflow 搬迁的具体改动**（不止是去掉 `working-directory`）：`rust/.github/workflows/ci.yml:44-49` 的缓存 `path` 含 `rust/target`、`key` 用 `hashFiles('rust/Cargo.lock', 'rust/rust-toolchain.toml')`，`:60` 的 artifact `path: rust/docs/SIZE.md`——全部要去掉 `rust/` 前缀并随 SIZE.md 的新去向调整。**环境变量**：tmux 场景 01/10 断言 CJK 光标列，而 `tests/ui_tmux/lib.sh:138` 只在 `raw_has()` 里设 `LC_ALL=C`（那是给 grep 用的），没有为 iota 进程或 tmux 设 UTF-8 locale，GitHub Ubuntu runner 的 `LANG` 未必是 UTF-8——workflow 必须显式设 `LANG=C.UTF-8 LC_ALL=C.UTF-8`（job 级 `env:`），否则 CJK 宽度断言会以看不懂的方式失败。沙箱按 §2.5 加 `IOTA_SANDBOX_REQUIRED=1` 与 apparmor sysctl。

**`macos-15-intel` 是 GitHub 最后一代 Intel 镜像，有已公布的退役日程**——把它写死在 CI 与发布矩阵里，会在未来某次 tag 时突然失败。实施 Phase 2/3 时必须核实其当时的可用期，并把「是否继续发布 `x86_64-apple-darwin`」作为用户决策（§6.19）处理；若决定放弃，CI 矩阵与 dist targets 同时去掉这一行，README/COMPAT 记一行。

`ci.sh` 顺序：fmt → check-deps → check-residue（棘轮）→ clippy `--all-targets -D warnings` → `RUSTDOCFLAGS=-D warnings cargo doc --no-deps` → `cargo test`（含 layering）→ `IOTA_TMUX=1 IOTA_TMUX_REQUIRED=1 IOTA_SANDBOX_REQUIRED=1 cargo test --test ui_tmux` → `cargo build --release` → `size.sh --budget 12MiB`。**`size.sh` 的输出不再写进工作树**：今天 `ci.sh:33` 每次运行都改写 `docs/SIZE.md`，只要本地跑一次 ci.sh 工作树就脏——这在 Phase 0–2 之间（工作树正在做仓库手术）尤其碍事，所以 **Phase 0 入库时就先把输出改到 `target/size.md`** 并作为 CI artifact 上传，`docs/SIZE.md` 随 Phase 2 提交 A 删除。删除 `check-stubs.sh`（它解析 EVALUATION.md 的 WP 状态表）与往返脚本。`cargo tree -d` 今天显示 7 个重复 crate（base64、core-foundation、fancy-regex、getrandom、hashbrown、miniz_oxide、syn；含 dev 依赖）——不设门，记录即可。

**私有窗口的 Actions 成本必须计入**（见 Phase 0 步骤 5）：私有仓库的 Actions 分钟按倍率计费（macOS ×10；`ubuntu-24.04-arm` 与 `macos-15-intel` 在私有仓库同样计费），4-runner 矩阵 + tmux + release build 每次跑都很贵，免费额度会很快耗尽。因此私有窗口内 **CI 只跑 `ubuntu-24.04` 一个 runner**（它同时也是「首次 Linux 运行」这件事本身要做的），四 runner 矩阵等转公开后再打开。

发布：**cargo-dist**（已核实仍在维护：[axodotdev/cargo-dist releases](https://github.com/axodotdev/cargo-dist/releases) 2026-02-23 v0.31.0、2026-05-21 v0.32.0；钉住版本），`dist init` 生成 `release.yml`：tag `v*` 触发，四个原生 target（aws-lc-rs 需 C 工具链，原生 runner 免交叉）、tar.gz + `checksums.txt`、shell installer、Homebrew 发布到 `joyqi/homebrew-tap` 的 `Formula/iota.rb`（全新 formula，不动 `chatchain.rb`，无 formula_renames——brain `project-rename-iota` 的决策）、GitHub Release 说明取自 CHANGELOG.md。

**发布触发方式变了，这属于用户可见流程，必须写进 README/CHANGELOG 与 §6**：今天 `.github/workflows/release.yml:3-5` 是 `on: release: types: [published]`——也就是**你在 GitHub 网页上手动创建一个 Release 才会触发构建**；cargo-dist 的模型是 `push tag v*` 触发，且 Release 由它自己创建。切换后「以后不要再手动在网页上 Create release」，正确动作是 `git tag -a vX.Y.Z && git push origin vX.Y.Z`。Phase 3 步骤 1 落地时把这句写进 `docs/history/GO-ORIGIN.md` 之外的**发布说明**（README 的 Release 小节或 CHANGELOG 顶部注记）。

**备选**（cargo-dist 在实施时出现阻塞即切换，约 1 天）：手写 4-target matrix workflow + 手维护 `Formula/iota.rb`。发布依赖的账号动作：新仓库的 `HOMEBREW_TAP_TOKEN`、Pages、iota.sh 的 Cloudflare DNS（brain 记录仍未配置）。先发 `v1.0.0-rc.1` 演练整条管线并在用户 Mac 上 `brew install joyqi/tap/iota`，再打正式 tag。`.goreleaser.yml`、goreleaser job、pkg.go.dev ping 删除。

### 2.7 文档

**维护中**：`README.md`——今天根 README 的语言中立正文（用法/flags/env/配置/图片/工具集/agent 模式/命令/示例，约 500 行，与 COMPAT §A–C 交叉核对后保留），改写约 90 行 Go 框架内容（简介、Install 改为 Homebrew/installer/`cargo install --git`/源码构建 + MSRV 一句（§2.3 的硬需求）、Project Structure = 模块表、Dependencies = crate 表、Platforms、发布流程一句（tag 触发，不再手建 Release）、以及一句「与 chatchain 的关系」或明确沉默——见下）；`docs/ARCHITECTURE.md` 从零重写为原生架构（分层、模块职责、依赖方向、具名不变量索引、依赖理由表即今天的 §11、错误分类、异步模型；无嫁接账本/WP/CONTRACTS）。

`docs/COMPAT.md` = 今天的 DIVERGENCES.md 改题为「兼容性账本」，**F/I/D/T ids 冻结**，剥掉其头部承认已过期的 `src/<module>/…:line` Rust 侧坐标（重命名后必烂），§D 移入 history。**前言措辞**：不能写「iota ≤ 2.x（Go 时代）遗留的行为决策」——Go 版 iota **从未发布过任何版本**，2.x 是 chatchain 的编号，若 §6.2 选 1.0.0 这句会把读者引到一个不存在的历史上。正确写法是：「本账本记录相对 **Go 实现**（冻结于标签 `go-final`，其源头是 chatchain 2.16.1）的行为决策。Go 实现从未公开发布，因此这些不是升级注意事项，而是这一实现为什么这样做的记录。」

**CHANGELOG.md 与 README 的「Upgrading」句必须换对象**：原稿那句「`~/.iota.yaml`、`~/.iota/sessions` 原样沿用」面向的是 Go 版 iota 的用户，而 Go 版 iota 没有用户。真实的存量用户是 chatchain 2.16.1（`~/.chatchain*`），而 brain 页 `project-rename-iota` 已经定了「不迁移、不提旧名」。于是只有两个自洽的选项，必须按 §6.18 拍板其一并前后一致：(i) **明确说**——CHANGELOG 1.0.0 与 README 各一句「iota 由 chatchain 演化而来；chatchain 的配置与会话不会自动迁移」；(ii) **明确沉默**——两处都不出现 chatchain 三个字，`~/.iota.yaml`/`~/.iota/sessions` 只作为本项目的路径描述出现，不带「沿用」二字。不能像原稿那样两边都不提却又写着「原样沿用」。

`docs/TESTING.md` 新写（金字塔、fakes、golden 与 bless、tmux 门、沙箱门、人工门、从 `go-final` worktree 用 `IOTA_BIN` 覆盖重跑跨实现验证的手工步骤，见 §2.5）；`docs/TUI-VERIFY.md` 保留，去 "iota-rs"/wart 编号，结果表每次发布填写；`docs/design/*.md` 15 篇：12 篇行为规格保留并把 Go 标识符改指 Rust 模块（agent-mode、code-toolset、context-compaction、export、host-integration、interrupt、math-rendering、mcp-tool-naming、model-settings、session-format、shell-toolset、tool-defer；约 118 处 Go 引用，**预算 2 天**，评审指出此前无人预算），3 篇归档（ui-architecture 是 bubbletea、internal-llm-client 是 Go SDK 替换故事、tabbed-select 已标 superseded）；`CHANGELOG.md` 新建。

`site/index.html` 的 Go 痕迹有**三处**，不止安装块一行：`docs/index.html:491` 的注释行 `# Or via Go`、`:492` 的 `go install github.com/joyqi/iota@latest`、以及 `:585` 的页脚 `MIT License · Built with Go`。残留门按 §2.4.2 **不**禁止 `\bGo\b`，所以这三处不会被自动抓到——Phase 3 必须额外加一条显式断言（见该阶段退出门槛）。README.md:37 的 `go install` 行同样在此清单内。

**归档 `docs/history/`**（冻结，附 README：「描述 2026-08/09 的移植过程，不再维护」）：EVALUATION.md、MERGE-PLAN.md、ARCH-REVIEW.md + refactor/5.2-*.md、REPORT-PHASE-1.md（用户撰写，逐字搬动或原地保留由用户定）、DIVERGENCES §D、go-test-map.tsv、baseline-2026-09.md、ROUNDTRIP-FINAL.log、GO-ORIGIN.md（标签名、`git show go-final:<path>` 恢复法、Go 包→Rust 模块表即今天 ARCHITECTURE §2）、本路线图。注意这四份归档文档里还有 29 处 `rust/` 路径（EVALUATION.md 19、MERGE-PLAN.md 5、ARCHITECTURE.md 4、DIVERGENCES.md 1，实测），它们是 Phase 2 退出门槛作用域必须排除的原因（见该阶段）。

**brain**：CLI **可用**——虽然 `which brain` 为空（不在 PATH 上），但 `~/.claude/skills/brain-page/bin/brain.mjs` 存在且可直接执行。本路线图的调用约定是：**在 `/Users/joyqi/Work/iota` 目录下运行 `node ~/.claude/skills/brain-page/bin/brain.mjs <cmd>`**（已实测：`list-pages` 正常列出 37 页；brainRoot 解析到仓库之外的 `/Users/joyqi/Work/chatchain-brain`）。因此 brain 写入是**每阶段结束的常规步骤**，与代码同批完成，不再降级为「不进门槛的可选人工步骤」；只是它不进 CI 门槛（brainRoot 不在仓库里，CI 看不到它）。

内容：

- **根页重写**：`architecture`、`stack`（今天仍写「Go CLI」+ cobra/bubbletea/goreleaser）、`roadmap` 之外，还有三页也必须重写——`background.md`（开篇即「iota 是一个轻量、跨平台的命令行 AI 聊天工具（**Go**，MIT）」并引用 README 的 "built with Go" 原文）、`flow.md`（正文引用 `[[ui-bubbletea-v2]]` 解释 type-ahead 排队）、`mindmap.md`（含「bubbletea v2 inline 钉底 frame」节点）。六页齐动，否则记忆层里的项目仍是一个 Go 项目。
- **`rust-only-migration`**：追加本路线图路径及每阶段落地证据。
- **归档（`archive-page`）**：五个纯 Go 实现选型页——`ui-bubbletea-v2`、`vendored-ui-stack`、`readline-repl-over-charm`、`typeahead-surface-lite`、`dependency-baseline-2026-08`。这些页记录的是「选了哪个 Go 库」，实现没了页也就没了意义。
- **`update-truth`（不是归档）**：另有一批 active 页含 bubbletea/cobra/goldmark/chroma/lipgloss 等 Go 标识，但它们记录的是**行为决策**而非库选型，必须把标识换成 Rust 模块后**继续 active**——`internal-llm-client`、`self-implemented-markdown`、`tabbed-select-component`、`host-integration`、`export-command`、`debug-request-inspector`、`compaction-event-store`、`mcp-graceful-degradation`、`terminal-title-from-session`、`rust-rewrite-feasibility`。把它们一并归档会丢掉今天代码仍在遵守的规则。
- **`rust-port-headless`**：需要 `append-timeline` 一条 `kind: reversal` 并关闭——它的标题与正文仍说「代码放在仓库子目录 `rust/`（Cargo workspace，6 crate）」，Phase 2 之后这三点全部不成立（根目录、单 package、无 workspace）。
- **`rust-arch-cleanup`**：追加 PreviewHandle 方向的 reversal（§2.2）。
- **新页**：Windows 范围、版本号、发布管线（含发布触发方式的变化）。
- brainRoot 目录名仍带 chatchain（`/Users/joyqi/Work/chatchain-brain`），是否改名由用户定（§6.16）。

**`AGENTS.md` 入库的后果必须说透**（§6.17 拍板）：该文件今天的全部内容就是 brain 接线块（`AGENTS.md:1-15`），它引用 `./BRAIN.md`，而 `BRAIN.md` 被 `.gitignore:37` 忽略——对公开仓库的外部贡献者是一个死链接。更要紧的是 **iota 自己的 agent 模式会把项目根的 `AGENTS.md` 注入系统提示**（README:20：`--agent` 下「layered `AGENTS.md` instructions … are injected as a volatile system-prompt overlay」）——也就是说，用 iota 开发 iota 时，模型会被要求「通过 brain CLI 读写」，而它多半没有那个 CLI。两个自洽的选项：**要么不入库**（继续和 CLAUDE.md/BRAIN.md 一起被忽略），**要么把 `AGENTS.md` 改写成真正面向贡献者与 agent 的指南**（构建、测试、UI 触碰规则、残留门），把 brain 接线块留在被忽略的 `CLAUDE.md` 里。

## 3 已确认的功能缺口

| # | 缺口（证据） | 影响 | 处理 | 工作量 |
|---|---|---|---|---|
| 1 | `edit_file` 用 `String::from_utf8_lossy` 解码整文件再写回（`rust/src/tool/code/tools.rs:732,753`）；Go `tool/code.go:740-755` 保字节 | 通过 NUL 嗅探的 Latin-1/Shift-JIS 文件在**编辑区之外**的非法序列被静默改写为 EF BF BD，diff 由同一份有损缓冲生成故不可见——数据损坏 | 在 `&[u8]` 上 count/find/replace（memchr::memmem 已是传递依赖），写回原字节，仅 snippet/行号/diff 有损转换；测试：0xE9 在编辑区外存活 | M（1 天） |
| 2 | NO_COLOR / TERM=dumb 无解析器：`RenderOptions { color: true }` 硬编码于 `repl/turn.rs:610`、`replay.rs:265`；`hyperlink(.., true)` 于 `turn.rs:441`、`replay.rs:54`、`editpicker.rs:119`；`render_diff` 于 `group.rs:326-333`；`repl/styles.rs:9-46` 无条件 SGR；frame 侧另有 `src/ui/theme.rs` 的 7 个 SGR 常量与 ratatui 渲染的状态行/分隔线 | crossterm 的 Colored 门会剥掉颜色，但 bold/dim/underline/reverse 等属性照发、每个被抑制的颜色退化成裸 `\x1b[m`，TERM=dumb 完全不生效 | 启动时解析一次 `ColorMode`（NO_COLOR 非空 ‖ TERM=dumb）挂在 RunParams/UiHandles，喂给上述 7 个点与 styles 构造器；**并明确决定 `ui::theme` 是否纳入 ColorMode 并在 COMPAT 写明理由**（Go 侧 lipgloss v2 走 colorprofile，NO_COLOR 下**保留属性、只剥颜色**，所以「frame 侧也无属性」既做不到也不是对等目标）；解析器测试 + tmux 场景 15 + COMPAT 行 | M（1–1.5 天，触及 14 个 repl 文件，一次编译器驱动改动） |
| 3 | Composer/Field 键集是 bubbles 默认键表的子集（`ui/composer.rs:287-332`、`ui/surface/field.rs:76-113`）；两处只匹配 (ctrl, code)，Alt+字母**插入裸字母**（`composer.rs:325`、`field.rs:107`），Ctrl+N/P/T/D、Ctrl+Home/End、PgUp/PgDn、Ctrl+V 被丢弃 | 习惯 emacs 词移动/词删除的用户在编辑长草稿时行为退化且插入垃圾字符 | 加 Alt+←/→/B/F、Alt+Backspace、Alt+D/Delete、Ctrl+N/P、Ctrl+Home/End、Field 的 Ctrl+D；未处理的 Alt 和弦改为 no-op；键表测试；Ctrl+T、Alt+U/L/C、PgUp/PgDn、Ctrl+V 记入 COMPAT 为有意放弃 | S–M（1 天；**先给 `src/ui/keys.rs` 的路由补一个直接的键表测试模块**——该文件今天 93 行、`#[test]` 数为 0，所谓「9 行优先级表」只存在于文档里，现有的优先级测试在 `src/ui/event_loop/queue_tests.rs` 等处，改键之前需要一个直接钉住 keys.rs 路由的测试） |
| 4 | Responses 方言对空 schema 发 `"parameters":{}`（`provider/openresponses.rs:274,414`），Go 因 omitempty 省略；chat-completions 路径已过滤（`openai.rs:180`） | 仅 MCP 服务器宣告 `inputSchema: {}` 时线上字节不同；多数后端接受 | 同一 `.filter(\|p\| !p.is_empty())` + golden | S（半天） |
| 5 | 版本身份：`Cargo.toml:4` 0.1.0，`cli.rs:17` `disable_version_flag = true`（`--version` 报 exit 2），MCP clientInfo 硬编码 `iota/1.0.0`（`manager.rs:39,62`）；无 Rust 发布管线 | 无法发布；MCP 服务器看到假版本 | `#[command(version)]`；clientInfo 用 `env!("CARGO_PKG_VERSION")`；`--version` 测试；版本号由用户定（§6.2） | S（半天；不含管线） |
| 6 | Windows：crate 完全不能为 Windows 编译（`shell/exec.rs:136-240` 的 process_group/nix pipe/killpg/unix pipe 无 cfg 门；`Cargo.toml:59` nix 无条件），所有 `cfg(not(unix))` 分支是死代码；Go 通过 goreleaser 发 windows/amd64+arm64 | 删除 Go 树即静默放弃一个已发布平台；`Cargo.toml:3` 与 `cli.rs:15` 仍宣称 cross-platform | 必须显式决策（§6.3）：放弃则改措辞 + README/COMPAT I-07 记「macOS 与 Linux」；投入则见积压项（Windows exec 路径、%LocalAppData%、控制台 Ctrl 事件、CI/target） | 放弃 S；投入 6–8 天 |
| 7 | 重复 MCP wire-name 告警是 `tracing::warn!`（`mcp/manager.rs:347`），无 subscriber 且 release 编译掉 | 用户永远收不到；Go 打印告警 | 改走 `Streams::warning`；`IOTA_LOG` 文件 subscriber 顺带落地 | S |
| 8 | Browser 面板空 `dir`：Rust cwd→`"."`（`ui/surface/tabbed.rs:158-165`），Go cwd→`$HOME`（`internal/ui/tabbed.go:240-249`）；唯一调用者 `/file` 已自行解析（`repl/commands/file.rs:168-175`） | 出厂命令不可达，仅通用 Panel API 且 cwd 不可读时可见 | 删除空 PathBuf 哨兵（`Panel::browser` 要求已解析目录），或加 `.or_else(app::user_home)` | S |
| 9 | 未记录的小分歧与缺失的孪生测试：headless `--resume` 忽略 `-s`、非 TTY 拒绝提前到 MCP 连接之前、会话选择器时间戳用系统时区、东亚模糊宽度、32 KiB SKILL.md 上限、D-37 有损 UTF-8 范围（read_file/grep/bash 输出/anthropic 文本附件）、glob 稳定排序、`-l` 与 header timeout 的库错误文本、空凭据头约定、**CLI 呈现差异（stderr 无色 vs 有色、`--help` 版式）**；`TestAttachRawContent`/`TestWatchToolComposing`/`TestInputCursorColsCJK`/`TestFindConfigFile`/`-m -` 无测试 | 账本不完整；`go-final` 删除后无法再核对 | 每项一行 COMPAT（含 CLI 呈现差异那一行，它是审计 other_findings 里已确认但从未进入任何清单的一项）；每个缺失测试补一个 | S–M（1.5 天） |

## 4 分阶段路线

工作量按一名工程师驱动 AI agent 计，不含日历上的试用与 rc 浸泡窗口，也不含只有用户本人能做的终端人工验证（单列于本节末尾的总计）。

### Phase 0 — 冻结、入库、首次 Linux 运行、建立基线（3–4 天）

**目标**：每个字节可恢复；后续所有门槛有实测基线；不再有「只存在于一份工作副本」的东西；**Rust 树第一次在 Linux 上编译并跑通**。

**为什么不是原来的 1–2 天**：本阶段真正的内容是四件从未做过的事——写三个尚不存在的脚本、让 tmux 与沙箱门第一次在云上真的执行、Rust 树第一次上 Linux、以及仓库身份切换。原估算只覆盖了最后一件。

**步骤**
1. 提交 17 个 Go 文件：`go: land the last fixes`（Anthropic thinking 回放、`attachRawContent` 收尾轮、列表内 display math、去 legacy 迁移、README 修剪）。这些行为已在 Rust 树（`rust/src/provider/anthropic.rs:405-480`、`rust/src/repl/run.rs:835`、`rust/src/chat/run.rs:188`、`rust/src/markdown/mod.rs:456`）。`AGENTS.md` **暂不入库**，等 §6.17 拍板（它今天只是 brain 接线块，且会被 iota 自己的 agent 模式注入系统提示）。
2. 带注释标签 **`go-last-fixes`**（**不是** `go-final`）：「Go 实现的最后一批修复；此时 `rust/` 尚未入库」。`go-final` 这个名字**留给 Phase 2**——它必须指向最后一个同时含 Go 树与 `rust/tests-go/gofix` 的提交，即 Phase 2 提交 A 的父提交（见 §5）。在这里打 `go-final` 会得到一个不含 gofix 的标签，而 §5、§2.7 的 GO-ORIGIN 与「从 worktree 重建 gofix」全部依赖它含 gofix。
3. 提交 `rust/` 原样（含 spikes、tests-go、examples）：`rust: import the port as of 2026-09-08`，纯新增，不移动任何东西。
4. 紧接一个小提交 `ci: write the size report to target/ instead of the work tree`：`ci.sh:33` 今天每次运行都改写 `docs/SIZE.md`，Phase 0–2 之间工作树正在做仓库手术，本地跑一次 ci.sh 就脏。改为写 `target/size.md` 并在 CI 里作 artifact 上传（`rust/docs/SIZE.md` 本身留到 Phase 2 提交 A 删除）。
5. 本地清理引用：删除 44 个 chatchain 标签（`git tag -l 'v*' | xargs git tag -d`，它们不是 HEAD 祖先，删除无害）；`git fetch --prune`（或 `git remote prune origin`）清掉指向 chatchain 的 `refs/remotes/origin/main` 与 `refs/remotes/origin/image-gen`。真正的规则是**永不 `git push --tags`**。
6. **归档旧仓库**：`gh repo archive joyqi/chatchain`。实测它今天 `archived=false` 且是 public，而它正是 `origin` 当前的重定向目标——归档它可以在物理上杜绝误推，是本阶段最便宜的一道保险。
7. 仓库身份：`gh repo create joyqi/iota --private`（先私有，回应 C 的「公开历史不应出现 go.mod 与 rust/ 并存」，同时满足 B 的先推送去风险），确认 `gh api repos/joyqi/iota --jq .full_name` 打印 `joyqi/iota`（今天打印 `joyqi/chatchain`），再 `git push -u origin main && git push origin go-last-fixes`。**注意两个私有窗口的限制**：(i) 免费账户的**私有仓库不能用 GitHub Pages**，所以任何 Pages 验证都必须等到 Phase 2 转公开之后；(ii) 私有仓库的 Actions 分钟按倍率计费（macOS ×10，`ubuntu-24.04-arm`/`macos-15-intel` 同样计费），因此**私有窗口内 CI 只跑 `ubuntu-24.04` 一个 runner**。若你更愿意直接公开创建，也是自洽的：`go-final` 推送后公开历史里本来就会出现 Go 树与 `rust/` 并存的那几个提交，C 的顾虑并不成立——见 §6.1。
8. 根目录加 `.github/workflows/rust-ci.yml`（复制 `rust/.github/workflows/ci.yml`，去路径过滤，**保留** `working-directory: rust` 与 `rust/` 前缀的缓存路径——它们在 Phase 2 提交 B 才一起改），矩阵**只有 ubuntu-24.04**，加 tmux + bubblewrap 安装步骤，job 级 `env: LANG=C.UTF-8, LC_ALL=C.UTF-8`，跑 tmux 与沙箱时置 `IOTA_TMUX=1 IOTA_TMUX_REQUIRED=1 IOTA_SANDBOX_REQUIRED=1`，并加 `sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0`（或确认 bubblewrap 的 AppArmor profile 生效）。Go 的 `test.yml` 暂留。
9. **首次 Linux 运行（单独预算 1 天）**：这是 `shell/sandbox/linux.rs`、bwrap 语义、`jiff` 的 `tzdb-zoneinfo`、以及 apt 的 tmux 3.4（对比开发机的 3.7c——`rust/docs/TUI-VERIFY.md:15` 明说 wart W9 的行为依赖 tmux 版本）第一次接受真实检验。预期会有一批平台相关的小修，这一天是给它们的。
10. **写齐门槛脚本**（今天 `rust/scripts/` 只有 check-deps.sh、check-stubs.sh、direct-deps.allow、go-session-roundtrip.sh、size.sh 五项）：`scripts/check-residue.sh` + `residue-baseline.txt`、`scripts/export-go-map.sh`、`code-tokens.py`（或 §2.4.2 建议的 `examples/` proc_macro2 版）；给 `scripts/size.sh` 加 `--budget` 参数解析（今天它完全没有参数解析）；给 `tests/ui_tmux/main.rs` 的 `gate()` 加 `IOTA_TMUX_REQUIRED`（五行改动：每个 `say("SKIP: …"); return None;` 分支在该变量置位时改为 `panic!`），给 `tests/tool/shell.rs:344-352,484-492` 加 `IOTA_SANDBOX_REQUIRED`。**把 `IOTA_TMUX_REQUIRED` 提前到 Phase 0 是必需的**，否则本阶段的退出门槛「tmux 场景输出 PASS 行」只能靠人眼翻 Actions 日志，绿勾不证明任何事。
11. 基线脚本化并入库到 `docs/history/baseline-2026-09.md`：各测试二进制的 `test result:` 行；`time cargo test --no-run` 冷/热（回应「70k LOC + 九个二进制链接时间从未测量」）；`check-residue.sh --report` 的每文件计数；release 体积；以及 Linux 与 macOS 各一份。
12. `scripts/export-go-map.sh` 导出 853 个 `// Go:` 锚点到 `docs/history/go-test-map.tsv`；同一脚本把 `go-last-fixes` 树里全部 `func Test*` 与锚点比对，机械产出「未移植清单」（已知：TestAttachRawContent、TestWatchToolComposing、TestInputCursorColsCJK、TestFindConfigFile，以及 §D 记录的 imagen_test.go:172、observer_test.go:10）——不再依赖已丢失的 `scratchpad/t3/audit.py`。
13. brain（`node ~/.claude/skills/brain-page/bin/brain.mjs …`）：`rust-only-migration` 追加本文路径、标签名、Actions run id。

**退出门槛**：`git status` 干净；`git tag` 只有 `go-last-fixes`（**没有** `go-final`）；`gh api repos/joyqi/iota --jq .full_name` 返回 `joyqi/iota`（private）；`gh api repos/joyqi/chatchain --jq .archived` 返回 `true`；`git for-each-ref refs/remotes` 里没有 chatchain 的分支；`rust-ci.yml` 在 **ubuntu-24.04 上绿**，且日志里 tmux 场景输出 PASS 行、沙箱测试**没有** `SKIP:` 行（两个 `_REQUIRED` 变量生效的证据）；三个新脚本存在且可运行，`size.sh --budget` 认参数；baseline 与 go-test-map.tsv 已入库。

**风险**：推送前忘记身份检查落到 chatchain 存档（门槛第 3、4 条是硬前提）；GitHub 上 tmux 首次真跑可能抖动——预算半天调 `lib.sh` 轮询超时；Linux 首跑可能牵出 bwrap/时区/locale 三类问题，第 9 步的一天就是给它的。

### Phase 1 — 产品安全修复 + 切换日常使用（4–5 天 + 1–2 周日历试用窗口）

**目标**：在没有 Go 兜底之前，先关掉会伤用户的缺口，并在用户自己的终端上证明 TUI。

**步骤**
1. §3 #1 edit_file 字节保真（含 Latin-1 测试）。
2. §3 #2 ColorMode 解析器 + 7 个注入点 + `repl::styles` + 场景 15；同时**明确决定 `src/ui/theme.rs`（frame 侧）是否纳入 ColorMode**，并把结论写成一行 COMPAT（Go 的 lipgloss v2 在 NO_COLOR 下保留属性、只剥颜色，所以 frame 侧「一律无 SGR」不是对等目标）。
3. §3 #3 键表——**先给 `src/ui/keys.rs` 补一个直接的键表测试模块**（该文件今天 0 个 `#[test]`），再改路由；§3 #4 Responses 过滤；§3 #7 告警出口 + `IOTA_LOG`；§3 #8 哨兵删除。
4. §3 #5：启用 `--version`、clientInfo 同源、`[package] version` 设为用户拍板的数字加 `-rc.1`。
5. Windows 措辞：无论最终决策，先删 `Cargo.toml:3` 与 `cli.rs:15` 的 "cross-platform"，README 写「macOS 与 Linux」；若用户选择投入，进积压。
6. §3 #9 的 COMPAT 行（此时文件仍叫 DIVERGENCES.md）与缺失测试。**从本阶段起，代码与文档里对账本 id 的引用一律写成 `COMPAT I-07` 形式**（§2.4.2）——前缀先行不影响任何东西，却决定了 Phase 2 建立的初始残留基线是否干净。
7. `scripts/go-session-roundtrip.sh` **继续留在 `ci.sh` 里跑**（现状如此，不动）：本阶段是唯一可能用 `iota-go` 去读 Rust 写的会话的时期，也是「Go 能读 Rust 写的 bundle」这条唯一无自动化替代的证据链还有意义的时期（§2.5）。
8. 安装 release 构建为 `iota`；从 `go-last-fixes` `go build` 一份 `iota-go` 作回退；按 `docs/TUI-VERIFY.md` §1–§7 在 Ghostty 与 Terminal.app（文档指定必测）上逐项填结果表——这是本阶段的**用户人工时间**大头（7 节 × 2 终端）。
9. 试用窗口：用户只用 Rust 二进制工作；tier-1/tier-2 发现在窗口关闭前修复或记录。

**退出门槛**：新测试全绿；**NO_COLOR 的两条可测断言**——(i) chat 侧输出（transcript / markdown / diff / `repl::styles`）在 `NO_COLOR=1` 下**零 SGR**；(ii) frame 侧在 `NO_COLOR=1` 下**只允许属性类 SGR**（正则否定 `\x1b\[[0-9;]*(3[0-7]|38|4[0-7]|48|9[0-7])[;m]`，即无前景/背景色码），且 `ui::theme` 的归属与理由已写入 COMPAT；Latin-1 fixture 字节相同；TUI-VERIFY 至少两行已填且无 tier-1、无未记录的 tier-2；≥ 7 天日常使用无 tier-1 报告；`iota-go` 从未被用于数据恢复。

**风险**：试用会暴露未知的 TUI 保真问题（这正是目的；回退二进制与未删的 Go 树把风险兜住）；键表改动触及 `ui/keys.rs` 的路由优先级——先补测试再改。

### Phase 2 — 仓库切换：退役 Go，rust/ 到根（2–3 天）

**目标**：仓库就是 Rust 项目；Go 代只在 `go-final` 后面；CI 第一次在根目录、四个原生 runner 上真跑；仓库转公开；Pages 从 `site/` 部署。

**步骤**
1. **一次性跨实现证明**（回应「删除后 oracle 丢失」与「不再保留 Go 项目」的两难，采用 C 的一次性证明而非 B 的每周 Go job）：先落地 `tests/session/roundtrip.rs`（§2.5）；再给 `scripts/go-session-roundtrip.sh` 加 `IOTA_BIN` / `MKBUNDLE_BIN` 覆盖（缺省仍是 `cargo run -q --bin iota` / `--example mkbundle`）——**这一步是归档前的必需改动**，没有它，`docs/TESTING.md` 里「从 `go-final` worktree 重跑」的方法是假 oracle（脚本第 22 行 `cd` 到自身上级，第 3/5 步会用 worktree 里 go-final 时代的 Rust）；然后最后一次运行脚本，日志归档到 `docs/history/ROUNDTRIP-FINAL.log`；`docs/TESTING.md` 写清正确的手工重跑法：worktree 里**只** `go build` gofix 并跑其 `serve`/`verify`，第 3/5 步用 `IOTA_BIN=<当前 target/release/iota>` 与当前树的 `mkbundle`。仓库里不留 Go 脚本、不留 Go CI job。
2. **在此打 `go-final`**：`git tag -a go-final <当前 HEAD>`，即下一步提交 A 的**父提交**——这是最后一个同时含完整 Go 树、Phase 1 全部修复与 `rust/tests-go/gofix` 的提交。注释：「携带 Go 实现的最后一个提交（行为参照）；session bundle v1、JSON report、配置 schema 在此冻结；`rust/tests-go/gofix` 与往返脚本一并在此」。随后 `git push origin go-final`（Phase 0 推的是 `go-last-fixes`，两个标签都保留）。
3. 提交 A `repo: retire the Go implementation (kept at tag go-final)`：只删——`main.go go.mod go.sum cmd chat config provider tool mcp internal spikes .goreleaser.yml .github/workflows/{test,release}.yml rust/spikes rust/tests-go rust/scripts/go-session-roundtrip.sh rust/docs/SIZE.md`，`size.sh` 去 Go 分支（`scripts/size.sh:34-56`）。**`rust/examples/mkbundle.rs` 不在删除清单里**（§2.1）。纯删除提交，`git log --diff-filter=D` 与 `git show go-final:` 保持好用。
4. 提交 B0 `repo: move the Pages site to site/`：`git mv docs site`，`pages.yml` path/paths 改 `site`。**Pages 的实际部署验证推迟到步骤 7 之后**——免费账户的私有仓库不能用 Pages，在这里验证必然失败。
5. 提交 B `repo: rust/ becomes the root`：`git mv rust/{Cargo.toml,Cargo.lock,rust-toolchain.toml,ci.sh,scripts,src,tests,examples,docs,README.md} .`，`git mv site/design docs/design`，**同时建 `docs/history/` 并把 EVALUATION.md、MERGE-PLAN.md、ARCH-REVIEW.md + refactor/ 三份归档文档先搬进去**（它们里面共 28 处 `rust/` 路径，是下面残留门作用域的例外来源，先隔离掉最省事）；`git mv .github/workflows/rust-ci.yml .github/workflows/ci.yml`（去 `working-directory`，缓存 `path` 去掉 `rust/target` 的前缀、`key` 的 `hashFiles('rust/Cargo.lock', 'rust/rust-toolchain.toml')` 去前缀、artifact path 随 SIZE.md 的新去向改为 `target/size.md`，矩阵改四个原生 runner 并保留 job 级 `LANG/LC_ALL`）；`.gitignore` 改 Rust 模板（保留 33-38 行）；修 14 处 `rust/` 路径、`manifest.json:3`、`tests/ui_tmux/main.rs` 的 `workspace_root()`；`rust/README.md` 并入根 README（Install/Structure/Dependencies 以 Rust 侧为准，正文以 Go README 为准）。
6. 提交 C `ci: root workflow, tmux and sandbox required, size budget, residue ratchet`：`IOTA_TMUX_REQUIRED=1`、`IOTA_SANDBOX_REQUIRED=1` + apparmor sysctl、`CARGO_BIN_EXE_iota`、`size.sh --budget`、`check-residue.sh` + `residue-baseline.txt`（棘轮生效，初始基线 = 当前计数——因为 Phase 1 起就用了 `COMPAT ` 前缀，这个基线不会混入合法的账本引用）、`tests/layering.rs`、`check-stubs.sh` 删除（`no todo!()` 并入 residue 门）；`Cargo.toml` 元数据（description、readme、**`homepage = "https://iota.sh"`**、keywords、categories、`publish = false`、删 anyhow）；lib.rs crate 文档改写。
7. `gh repo edit joyqi/iota --visibility public`。
8. **转公开之后**才验证 Pages：`pages.yml` 从 `site/` 部署成功、iota.sh 或 `joyqi.github.io/iota` 能打开。brain：`architecture`/`stack`/`background`/`flow`/`mindmap` 五个根页各加一行「以下 Go 描述已成历史」（全文重写在 Phase 6）。

**退出门槛**：无 Go 工具链的全新克隆上 `./ci.sh` 绿；GitHub 四个 runner 全绿，且 tmux 场景为 PASS 行、沙箱测试无 `SKIP:` 行；`find . -name '*.go' -not -path './target/*'` 为空；**`grep -rn 'rust/' src tests scripts examples ci.sh .github README.md Cargo.toml` 为空**（作用域**不含 `docs/`**——提交 B 把 `rust/docs` 整体搬到 `docs/`，而 EVALUATION.md 19 处、MERGE-PLAN.md 5 处、ARCHITECTURE.md 4 处、DIVERGENCES.md 1 处的 `rust/` 路径要到 Phase 3/6 才处理，其中三份已在提交 B 移入 `docs/history/` 永久冻结；把 `docs` 放进作用域这条门槛必然失败）；`git log --follow src/repl/run.rs` 能追到 Phase 0 的导入提交；`git show go-final:chat/run.go` 与 `git show go-final:rust/tests-go/gofix/main.go` 都可用（后者是 §5 前提的直接验证）；仓库为 public 且 Pages 从 `site/` 部署成功；ROUNDTRIP-FINAL.log 已入库；残留基线已入库且门生效。

**风险**：重命名检测（纯 mv 提交、site 先单独移）；tmux 在四个 runner 上首次真跑的抖动（按场景加重试，不整体重试）；macos-15-intel 与 ubuntu-arm 从未构建过 aws-lc-rs（预算半天）；`macos-15-intel` 是最后一代 Intel 镜像，本阶段要顺带核实其退役日程（§6.19）。

### Phase 3 — 发布工程与 1.0（3–5 天 + rc 浸泡）

**目标**：Rust 二进制走真实管线发布；用户可见文档不再描述 Go。

**步骤**
1. `dist init` → `dist-workspace.toml` + `release.yml`（四 target、checksums、shell installer、Homebrew → `joyqi/homebrew-tap` `Formula/iota.rb`）；cargo-dist 版本钉住；确认 `Cargo.toml` 的 `homepage`/`description` 已就位（formula 的这两行直接取自它们）；若阻塞则切备选手写 workflow（§2.6）。**同时把发布触发方式的变化写进用户可见流程**：今天 `.github/workflows/release.yml:3-5` 是 `on: release: types: [published]`（在 GitHub 网页上手建 Release 才发布），cargo-dist 改为 `push tag v*` 触发且 Release 由它创建——README 的发布小节与 CHANGELOG 顶部注记要写明「以后不要手动在网页上 Create release；正确动作是 `git tag -a vX.Y.Z && git push origin vX.Y.Z`」。
2. `deny.toml` + cargo-deny job（仅 advisories/licenses）；ARCHITECTURE §11 追加 amendment 一段。
3. README 改写约 90 行 + 正文与 COMPAT §A–C 交叉核对（F-02/F-03/I-03/I-05 等用户可见变化进入正文）+ **MSRV 一句（硬需求，§2.3）** + 按 §6.18 拍板的「与 chatchain 的关系」一句或明确沉默（不能保留原稿那种面向不存在的 Go-iota 用户的「原样沿用」措辞）；`CHANGELOG.md` 1.0.0（以 COMPAT F/I 行与 Phase 1 修复为条目，含「已知限制：Windows」，以及同一条 chatchain 关系措辞）；`site/index.html` 的**三处** Go 痕迹（`index.html:491` 的 `# Or via Go`、`:492` 的 `go install …`、`:585` 的页脚 `Built with Go`）与 `README.md:37` 的 `go install` 行一并替换。
4. 文档处置后半：`DIVERGENCES.md` → `docs/COMPAT.md`（改题、**按 §2.7 的措辞写前言**——「相对 Go 实现（冻结于标签 `go-final`，其源头是 chatchain 2.16.1）的行为决策；Go 实现从未公开发布」、ids 冻结、剥 Rust 侧坐标、§D 入 history）；搬 REPORT-PHASE-1（按用户决定）与本路线图进 `docs/history/`；写 `docs/history/GO-ORIGIN.md`。
5. 打 `v1.0.0-rc.1`：四个 target 的产物齐全；用户 Mac 上 `brew install joyqi/tap/iota && iota --version`；干净 Linux 上 shell installer；恢复一个 `~/.iota/sessions` 里 Go 写的会话；体积 ≤ 预算；TUI-VERIFY 表按 rc 二进制刷新（**用户人工时间**）。
6. 打正式 tag（数字见 §6.2）；brain：`release-engineering` 决策页（含触发方式变化）、`roadmap` 根页。

**退出门槛**：`gh release view vX.Y.Z` 列出四个归档 + checksums + installer；干净机器上 `brew install joyqi/tap/iota && iota --version` 打印该版本，且 `iota.rb` 里有 `homepage "https://iota.sh"`；`cargo install --git https://github.com/joyqi/iota` 在一台 stable ≥ 1.98 的机器上可用；CI（含 deny）绿；`check-residue.sh` 在 README.md 与 site/ 上为零，**并且** `grep -cE 'go install|Built with Go' site/index.html README.md` = 0（残留门按 §2.4.2 不禁止 `\bGo\b`，这两类站点残留必须单独断言）；日常使用的就是发布提交。

**风险**：首次接触 dist/Homebrew 自动化（rc 演练兜住）；tap 权限与 token 是账号动作；不得触碰 `chatchain.rb`。

### Phase 4 — 测试树收敛：一层 fakes、无缝、无重复（4–6 天）

**目标**：测试树原生且密闭，后续结构与残留阶段不必再回头改 fixture。

**步骤**
1. `tests/common/{chat,fake_mcp,project,stub,transcript}.rs` 的 fakes 移入 `src/testing/`（`FakeProvider` builder 取代 18 个 `impl Provider`、dispatcher fakes、`ReplFixture` 取代 21 处 `RunParams` 字面量、`ScriptedUi` 访问器让 `OrderUi`/`FakeStream` 消失）；`tests/common/` 只剩 `wire.rs`/`session.rs`（wiremock/tempfile，dev-dependency），各二进制只挂用到的文件，`tests/common/mod.rs:5` 的 `#![allow(dead_code, unused_imports)]` 删除。**同批落地 `UPDATE_GOLDENS=1` bless 路径**（§2.4.3），覆盖 `tests/fixtures/mathtext/goldens-2d.txt`、`goldens-inline.txt`、`glyph-widths.txt` 与 `tests/fixtures/export/sample.md`、`sample.html` 五份；bless 只改文件、不改断言逻辑，且每次 bless 必须附 CHANGELOG 行（Phase 5 的门槛依赖这条）。
2. `src/ui/testutil.rs` 收敛七份 `Surf`、三份 `test_model`、两份 `SharedBuf`/`ChannelEvents`、spinner 字形常量；删 13 个 `#![allow(dead_code)]` 头让 rustc 列出死助手（约 −400 行）。
3. 8 个 `#[doc(hidden)]` 缝背后的测试移入 `src/repl/*/tests.rs`，缝删除；`merge_result`、`SET_NAMES`、22 个 session 测试专用 pub 降为 `pub(crate)`。
4. `NewSession{..}` 取代 13 处位置参数 `create("", "", false)`；两个 `RETRY_BACKOFF` 合一；`examples/mkbundle.rs` 与 `tests/session/roundtrip.rs` 共用同一份记录形状清单（mkbundle 的模块文档改写为「会话格式冒烟样本」，不再自称往返脚本输入）。
5. 108 个文件级 lint 头 → 每个 `tests/<area>/main.rs` 一个；11 处真实时钟 sleep 改 paused clock；`tests/provider/progress.rs`（9 行、无测试）删除；`tests/markdown/harness.rs` 复用 `src/text` 而非重实现标尺；`assert_cmd` 若能被 `CARGO_BIN_EXE_iota` 取代则删。
6. **残留改写试点**（回应「注释工作量无锚定」）：在 `src/mathtext/symbols.rs`（269 处坐标，本阶段不会被结构重构触碰）上做一次完整的 a/b/c 分类改写，用 `code-tokens.py` 作门，记录每小时处理数与 a/b/c 三类比例。**这是校准 Phase 5/6 估算的唯一数据点**，因此它的产出是本阶段的退出门槛之一，不是一句「据此校正」。
7. 触碰到的每个测试文件同时完成：删 `// Go:` 锚点、去 `test_` 前缀改行为句子、字节钉子分类（棘轮规则：触碰即归零）。**测试改名在本阶段全部做完**，这样 §2.4.1 (c) 类的 `Pinned by <file>::<fn>` 引用才有稳定的目标（顺序规则见 §2.4.1）。

**退出门槛**：各二进制测试数 ≥ Phase 0 基线减去 PR 中点名的合并；`cargo test` 墙钟不劣于基线（否则评估 cargo-nextest，进积压）；`grep -rn 'doc(hidden)' src` 为空；`grep -rln '#!\[allow(clippy::unwrap_used' src tests | wc -l` ≤ 10（**文件数**，不是命中数）；被替换 fake 的期望字符串逐字不变（diff 核对）；`UPDATE_GOLDENS=1 cargo test` 能在五份 golden 上重新生成且不改断言；**试点结果（每小时条目数、a/b/c 比例、`code-tokens.py` 的误报率）已写入 `docs/history/baseline-2026-09.md`，且 Phase 5/6 的估算已据此在本文件里修订**。

**风险**：合并 fakes 时静默削弱断言（一个 fake 一个 PR，期望字符串不得变）；ScriptedUi 膨胀成框架（保持为记录器）。

### Phase 5 — 原生结构（行为中立；一个模块一个 PR；注释随行重写）（12–16 天）

**目标**：完成 ARCH-REVIEW 开的头：无环、无扁平状态袋、无零值哨兵、无按 Go 文件切分的模块树；每个字节钉子不变。**回应评审异议 1（注释被改两遍）**：结构 PR 触碰的文件在同一 PR 内按 §2.4 完成注释改写（棘轮：触碰即归零），安全网是完整测试 + golden + tmux；纯注释 PR 只留给本阶段不触碰的文件（Phase 6）。

**步骤**
- 5a 环与分层：`tool/context.rs` 接收 RunCtx 等（13 个导入点 + `mcp/manager.rs:15`）；`markdown/preview.rs` 定义 PreviewHandle（brain reversal 先落）；删 MathRenderer/Mathtext ZST 让 mathtext 成叶子；三份 CSI/OSC 扫描器并入 `text::ansi`；`tests/layering.rs` 加新规则（含 `Pinned by` 校验）。
- 5b 重命名与合并（每个 PR 纯 `git mv` + 路径修正，与代码改动分开）：cmd→cli（cli.rs→args.rs、CliError→error.rs、interactive 拆三文件、window 并入 tuning）、chat→engine（once→cli/headless）、tool/{sets→builtins, fmt→display, registry+merge→dispatch, defer+defer_mode→defer/, yaml11→config/}、app/、config/、provider/{common→core, usage_conv→usage, image_util→images}、llm/models→client、text 的 `go_*` 改名、session/rawcodec→raw；ui/ 与 repl/ 按 §2.2 分组（含 `src/ui/theme.rs` → `src/ui/render/theme.rs`）；三处引用 crate 时代路径的模块文档改为 intra-doc 链接（`cargo doc -D warnings` 抓漂移）。
- 5c 和类型：`shell::Outcome`、`mcp::ServerState`、`ui::PanelResult`/`TabbedOutcome`/`KindState`（Rows 先做，占 80%）、`region::Preview{call: Option<CallClock>}`、`tool::Approval`、mathtext AST Option、`markdown::Block`（**最后做**，仅当 `tests/markdown` 2 046 行规格无需改动即保持绿）。
- 5d 缝类型化：facade 与 config 的 Option（Panel 字段、StatusData.model、ProviderConfig 12 字段、RunSettings、NewSession、PrefixOf、str_arg/int_arg、`supports_parallel` 拆为 `parallel_by_default()`+`parallel_for(args)`、`Env.project_root` 非 Option）；`Config::provider()` + 唯一 api_key 优先级；`CliError` 三分；20 个 `Result<_, String>`；图片 provider 错误类型化；`Role` 枚举；wire 响应 ""→Option、`finish_reason` 枚举、i64 槽位→usize、f32→f64。
- 5e repl/ui 形状：`Repl` 三分 + `TurnEngine<'a>`（推迟过的 5.2-3C 前半：删三处 `mem::take`、四个闭包别名换具名句柄、`ensure_model` 成方法、5–6 参数签名与「冻结契约」借口的 `too_many_arguments` 消失）；表驱动命令派发（先用 ScriptedUi 回放 12 条命令固定顺序并断言事件日志前后一致）；`InputState` + `impl Model { fn on_key }`；`input::editor` 共享行编辑器；`Emit::Test` 与 `cfg(test)` 分支移出 `Region::publish`；`TermSize::height_or_default()`。
- 5f 重复：`llm::stream` 泛型驱动；两个 HTML 转义器合一；`background.rs` 用 tokio；所有权查找不再每次调用重扫工具表；130 处内联 PoisonError → `sync::lock`；`Streams::warning` 前缀 + 删闭包穿参；`app::env::Env` 取代双缝与唯一直接 `std::env::var`。
- `udiff.rs`（375 行，golden 钉住）保留并记为自有实现（换 `similar` 会改 hunk 合并字节，见 §6.15）。
- **UI 触碰 PR 按批次验证**：5c 与 5e 会产生十几个触碰 `src/ui/**` / `src/repl/render/**` 的 PR。每个 PR 仍单独点名它钉住的 tmux 场景，但人工验证按批做——攒 3–5 个相关 PR 合并到一条集成分支后跑一次 TUI-VERIFY 对应小节，把结果行写进该批的每个 PR。这把十几次 ≤30 分钟压到三四次，是 §4 总计里「用户人工验证时间」能收住的前提。

**退出门槛**（每个 PR）：`ci.sh` 绿；`git diff --exit-code tests/fixtures`（golden 不许 re-bless，除非附 CHANGELOG 行）；触碰文件残留为零；触碰 `src/ui/**` 或 `src/repl/render/**` 的 PR 执行 §2.5 的 UI 触碰规则（可按上述批次记录）；ARCHITECTURE.md 与结构改动同 PR 更新。阶段末：`grep -rnE 'Result<[^>]*, String>' src` 非测试 = 0；`grep -rn PoisonError src` ≤ 1；`grep -rn 'fn go_' src` = 0；`tests/layering.rs` 全部规则通过；facade/config/RunSettings 类型上无 `is_empty()` 当缺席用。

**风险**：本阶段是唯一有真实行为风险的阶段——状态袋→枚举可能改变扁平字段掩盖的边角（字节钉子、ScriptedUi 记录、Rows 先做、一模块一 PR 兜底）；`Writer` 重写是最大单项，放最后并允许中止进积压；纯 mv 与纯代码不得混在一个提交。

### Phase 6 — 剩余残留清扫、测试命名、文档与 brain 收口、1.1（6–9 天）

**目标**：没有任何注释、测试名、文件名、文档或 brain 页把读者绑定到 Go 树或仓库外文档；工程文档与最终树一致。

**步骤**
1. 打标签 `pre-deport`（最后一个仍带 Go 坐标注释的提交，`git log -S` 可追回任何 rationale）。
2. 叶子优先的纯注释 PR（`code-tokens.py` 门；**不用 release 二进制哈希对比**——`panic::Location` 把行号编进二进制，改注释行数二进制就变，§2.4.2），覆盖 Phase 5 未触碰的文件：mathtext 其余四文件（parse 75、macros 72、layout 65、pict 53）、text、imgterm、host、session、agents、llm 方言文件、tests 各二进制；先修主动误导的注释（假的 paste 说明、"provisional"、`chat/run.rs` 里「D-12 未移植」的过期断言、mathtext「一文件对一 Go 文件」）。每处按 §2.4.1 的 a/b/c 分类，(a) 类删除后无文档的必须补现在时行为句。
3. 测试：剩余 `// Go:` 锚点删除、`fn test_` 归零、按被测单元改文件名（若 Phase 4 有遗留）、`tests/ui_tmux/main.rs` 的来源表改为「每个场景证明什么」、`tests/fixtures/README.md`。
4. `check-residue.sh` 从棘轮切为零容忍（基线文件删除），`Pinned by` 校验保持开启。
5. 文档：`docs/ARCHITECTURE.md` 对最终树重写（脚本核对模块清单 = `src/`）；`docs/TESTING.md`；`docs/COMPAT.md` 补齐；12 篇 design 文档改指 Rust 模块（2 天）；`docs/history/README.md`；README 模块表与计数刷新；`docs/TUI-VERIFY.md` 去 wart 编号。
6. brain（经 CLI：`cd /Users/joyqi/Work/iota && node ~/.claude/skills/brain-page/bin/brain.mjs …`）：**六个根页**（`architecture`/`stack`/`roadmap`/`background`/`flow`/`mindmap`）重写；五个 Go 实现选型页 `archive-page`；十个行为决策页 `update-truth` 换成 Rust 模块标识并保持 active；`rust-port-headless` 追加 `kind: reversal` 并关闭；`rust-arch-cleanup` 的 PreviewHandle reversal 确认；`rust-only-migration` 以最终门槛证据关闭；`tui-test-strategy` 更新为 Rust 金字塔；Windows/版本/发布页确认（清单见 §2.7）。
7. 通过 Phase 3 管线发 1.1.0（rc 演练重复一次；TUI-VERIFY 表刷新——**用户人工时间**）；CHANGELOG 写「无用户可见变化」——除非 §6.8 拍板的行为变化（go_value/go_map、go_pad、工具描述措辞）随本版落地。

**退出门槛**：`check-residue.sh` 在 src/ tests/ scripts/ docs/（history 除外）为零且 CI 零容忍；**`grep -rE 'fn test_' src tests | wc -l` = 0**（`grep -rc` 是逐文件计数、不是总和，不能当门槛用）；`grep -rlE 'iota-rs|Rust port' README.md docs --exclude-dir=history` 为空；每个 `Pinned by <file>::<fn>` 在测试树中可解析；`RUSTDOCFLAGS=-D warnings cargo doc` 绿；brain 六个根页无「Go CLI」表述且 `list-pages` 里没有 active 的 Go 选型页；1.1.0 经 Homebrew 装在用户机器上。

**风险**：sed 式删除丢掉真实 rationale（tool/ 约 80 处 Go 句子是唯一文档，如 `/// code.go:249-300.`）——分类规则禁止删 (b) 类，且 (a) 类必须补写行为句（否则 `missing_docs` + `-D warnings` 直接红）；agent 起草、工程师按类审阅是瓶颈，估算以 Phase 4 试点数据校正。

### 积压（迁移之后；不阻塞任何切换或发布）

| 项 | 内容 | 估算 |
|---|---|---|
| 引擎统一 | 一个 `TurnEngine` + 观察者 trait，`repl/turn/tools.rs` 不再复述 `engine::execute_with_tools` 的 walk 与并行 batch；两套 tool-loop 测试合一 | 6–8 天 |
| Windows（若拍板投入） | `shell/exec_windows.rs`（无进程组；杀直接子进程 + 等待；可移植匿名管道）、无沙箱+审批门模式、%LocalAppData%、控制台 Ctrl 事件、windows-latest CI 与 dist target、TUI-VERIFY 加 Windows Terminal 行（DECSTBM 内联引擎从未在那里跑过） | 6–8 天，可能翻倍 |
| `markdown::Writer` Block 枚举 | 若 Phase 5 未完成 | 3–4 天 |
| `Ui` trait 瘦身 | 七个调用小部件动词收进一个 `CallWidget`（ARCH-REVIEW §5.3 明确推迟） | 2–3 天 |
| 测试构建时间 | 若 Phase 0 测得链接时间过长：cargo-nextest 或合并二进制 | 1–2 天 |
| `IOTA_LOG` 完善 | 级别/格式/轮转 | 1 天 |

**总计**：迁移本体 **34–48 工程日**（Phase 0 3–4 + Phase 1 4–5 + Phase 2 2–3 + Phase 3 3–5 + Phase 4 4–6 + Phase 5 12–16 + Phase 6 6–9）。
**用户人工验证时间（单列，不可由 agent 代劳）**：约 **16–24 小时**——Phase 1 的 TUI-VERIFY 全表（7 节 × 2 终端，约 3–4 小时）、Phase 5 的 UI 触碰 PR 按批验证（3–4 批 × ≤30 分钟，约 2–3 小时；不按批则 6–9 小时）、Phase 3 rc 与正式版各刷新一次表（约 6–8 小时含 Homebrew/installer/会话恢复的真机验证）、Phase 6 的 1.1 再刷一次（约 3–4 小时），另加账号动作（建仓、归档 chatchain、tap token、Pages、Cloudflare DNS）约 1–2 小时。
**日历**：约 **11–13 周**（含 Phase 1 的 1–2 周试用窗口与 Phase 3/6 的 rc 浸泡）。

## 5 Go 退役方案

**何时**：Phase 2 提交 A，前提是 (1) 标签 `go-final` 已按 Phase 2 步骤 2 打在**提交 A 的父提交**上并推到 GitHub——那是最后一个同时含完整 Go 树、Phase 1 全部修复与 `rust/tests-go/gofix` 的提交（Phase 0 打的 `go-last-fixes` 早于 `rust/` 入库，**不含** gofix，不能承担这个角色）；(2) Phase 1 的试用门槛通过（TUI-VERIFY 在用户终端已填、≥ 7 天日常使用、`edit_file` 字节修复已落地）；(3) `tests/session/roundtrip.rs` 已落地，往返脚本已加 `IOTA_BIN`/`MKBUNDLE_BIN` 覆盖并与之并排跑过，日志已归档。此后路线图没有任何步骤需要工作树里的 Go：注释改写读 `git show go-final:<path>`；跨实现验证按 `docs/TESTING.md` 的 worktree 法手工重跑（worktree 里只 `go build` gofix，Rust 侧用 `IOTA_BIN` 指向当前树的产物——不这样做测的就是 go-final 时代的 Rust）；`gofix` 的 `replace => ../../..` 在 worktree 内解析。

**如何**：三个纯提交——A 只删（Go 模块全部 + `.goreleaser.yml` + Go workflows + 根 `spikes/` + `rust/spikes` + `rust/tests-go` + 往返脚本 + `size.sh` Go 分支 + `docs/SIZE.md`；**不含** `examples/mkbundle.rs`）；B0/B 只改名（docs→site、rust/*→根、三份归档文档进 `docs/history/`）；C 只加 CI/门。历史保持可读：`git log --diff-filter=D` 列出退役，`git log --follow` 穿过搬迁。

**保留什么**：标签 `go-final`（唯一指向 Go 代的指针；`docs/COMPAT.md` 前言、`docs/history/GO-ORIGIN.md`、`go-test-map.tsv` 引用它）与 `go-last-fixes`（Phase 0 的推送去风险点，保留无成本）；`examples/mkbundle.rs`（纯 Rust，转为会话格式冒烟样本 + 手工重跑法的 bundle 生成器）；`tests/fixtures/sessions/`（Go writer 写的 5 个 bundle + manifest，永久冻结的 v1 格式语料，来源写入 README 与 `doc` 字段；`generator`/`go_version` 字段作为数据保留）；`tests/cmd/session.rs:315-320` 与 `tests/session/golden.rs:16-31` 这两处内嵌的 Go 字节证据（§2.5 的证据链 1、2，它们让「未知 meta 键存活」和「Go 的 meta.json 布局」在没有 Go 工具链时依然被钉住）；mathtext 与 export golden（重新标注为本项目的渲染契约，可 bless）；`docs/design/` 12 篇行为规格；`docs/history/`（EVALUATION、MERGE-PLAN、ARCH-REVIEW + refactor、REPORT-PHASE-1、DIVERGENCES §D、go-test-map.tsv、baseline、ROUNDTRIP-FINAL.log、GO-ORIGIN.md、本文）；brain 的行为决策页（session-bundle-jsonl、tool-defer、mcp-tool-namespacing、compaction、agent-mode、math-rendering、host-integration、anthropic-thinking-replay …）保持 active。

**唯一真正失去自动化的证据**：「Go 能读 Rust 写的 bundle」（往返脚本第 5 步）。它只在 Phase 1 的回退窗口里有实际意义，退役后由「归档的 ROUNDTRIP-FINAL.log + `docs/TESTING.md` 记录的 worktree 手工重跑法」承接。反方向（Rust 读 Go 写的 bundle、未知 meta 键存活、Go 的 meta.json 字节布局）三条证据链留在 `cargo test` 里，无 Go 工具链可跑。

**哪里都不留（只在标签里）**：gofix、往返脚本、两个 spike crate、goreleaser 与 pkg.go.dev、Go workflows、README 的 go install/go build/Project Structure/Dependencies 段、站点的 `# Or via Go`/`go install`/`Built with Go` 三行、size 报告的 Go 对比行、WP 状态表作为 CI 输入、任何默认 CI job 里的 Go 工具链、44 个 chatchain 标签（本地删除、永不推送；`joyqi/chatchain` 在 Phase 0 归档后不再动，`chatchain.rb` 不是本项目的，不动）。

## 6 需要用户拍板的问题

1. **仓库身份与推送时机**：接受「Phase 0 私有创建 `joyqi/iota` 并立即推送，Phase 2 切换后转公开」，还是直接公开创建？私有窗口有两项实打实的代价：免费账户的私有仓库**不能用 GitHub Pages**（所以 Pages 验证必须推迟到转公开之后），且 Actions 分钟按倍率计费（macOS ×10），因此私有期内 CI 只能跑单个 `ubuntu-24.04` runner。而「公开历史不应出现 go.mod 与 rust/ 并存」这个顾虑其实不成立——`go-final` 一旦推送，那几个提交本来就在公开历史里。创建仓库、归档 `joyqi/chatchain`（今天 `archived=false`）、`HOMEBREW_TAP_TOKEN`、Pages、iota.sh 的 Cloudflare DNS 都是只有你能做的账号动作；推送前必须确认 `gh api repos/joyqi/iota --jq .full_name` 不再指向 chatchain。
2. **首个版本号**：1.0.0（推荐——brain 已定 iota 是全新项目、不带 chatchain 延续）还是 3.0.0（向 chatchain 2.16.1 老用户示意继承）。决定 tag、CHANGELOG、formula，也决定第 18 项的措辞。
3. **Windows**：1.0 声明「macOS 与 Linux」并把 Windows 放积压（推荐），还是在删除 Go 树之前投入 6–8 天真正移植？Go 版发过 windows/amd64+arm64，Rust 版今天连编译都不过。
4. **发行渠道**：Homebrew tap + shell installer + GitHub 归档是最小集；crates.io 要改包名 `iota-cli`（`iota` 已被 dtolnay 的 0.2.3 占用，已核实）还是不上；Linux musl 静态构建要不要。**无论是否上 crates.io，README 的 MSRV 一句都是硬需求**：`cargo install --git` 不读取源码树的 `rust-toolchain.toml`，用户机器上的 stable 低于 1.98 会直接编译失败。
5. **发布工具**：cargo-dist（已核实 2026-05 仍在发版）还是手写 matrix workflow + 手维护 formula。附带一个用户可见的流程变化：cargo-dist 由 `push tag v*` 触发并自己创建 Release，**以后不要再手动在 GitHub 网页上 Create release**（今天 `.github/workflows/release.yml:3-5` 正是 `release: [published]` 触发）。
6. **兼容账本**：DIVERGENCES.md 改题为 `docs/COMPAT.md` 并冻结 F/I/D/T ids（推荐，137 处 `D-nn` 与 155 处 `T-nn` 引用可原样解析），还是废除账本只靠测试 + CHANGELOG。若保留，还需接受「所有引用一律加 `COMPAT ` 前缀」这条从 Phase 1 起生效的书写规则（§2.4.2）。
7. **站点目录**：落地页移到 `site/`（推荐；index.html 不链接 design 文档，无损失）还是留在 `docs/` 而工程文档改放 `doc/`。
8. **允许的行为变化**：确认 Phase 1 的修复清单（§3 #1–#4、#7、#8）；此外是否趁「不再追求与 Go 字节相等」一并改：压缩提示词的 `map[...]` 参数渲染改 compact JSON（`go_value/go_map`）、`go_pad` 按显示宽度补齐、工具描述里的「Go regular expression (RE2)」措辞、`-l`/header timeout 的库错误文本、`Warning:` 前缀统一。**关于 `go_value/go_map` 的一个校准**：`src/repl/tokens.rs:120-137` 的 `go_value` 对数组/对象**今天已经**输出 compact JSON（其 doc 明说「Composite values (arrays, objects) print as COMPACT JSON where Go would print `[1 2]` / `map[k:v]`」，是移植时的有意选择），与 Go 字节不同的只剩顶层 `map[k1:v1 k2:v2]` 外壳与标量的少数写法——**已是半对等**，改动风险比路线图初稿暗示的小。每项都会有 CHANGELOG 行。
9. **Go 写的 fixture**：永久保留为冻结语料（推荐）还是从 Rust writer 再生（失去跨实现证据、换取可再生）；mathtext golden 同问。
10. **是否保留任何 opt-in 的 Go oracle 脚本**：本文选「一次性证明 + 归档日志 + 文档记录的手工重跑法」，仓库不留 Go 脚本；若你更想要 `scripts/go-oracle.sh`（worktree 方案、需本机 Go、必须带 `IOTA_BIN` 覆盖），请说明。
11. **试用条款**：删除 Go 前必须填表的终端（文档要求 Ghostty + Terminal.app；iTerm2/kitty/VS Code 可选？）与日常使用窗口长度（假设 7 天）。
12. **模块重命名幅度**：接受 cmd→cli、chat→engine、tool/sets→builtins、defer/、fmt→display、chat::once→cli/headless、app/ 等编译器驱动的改名，还是只做环/枚举/缝而保留今天的名字。
13. **PreviewHandle 方向反转**：接受推翻 `rust-arch-cleanup` 的记录并写 reversal，还是维持「markdown 依赖 ui::facade」。
14. **积压范围**：TurnEngine 统一、Writer 枚举、Ui trait 瘦身、`IOTA_LOG` 完善列为积压——确认，或提前任一项。
15. **`udiff.rs`**：保留自有 Myers 实现（推荐，golden 钉住）还是换 `similar` crate 并 re-bless。
16. **brain**：确认 §2.7 的三份清单——**归档**五个 Go 选型页（ui-bubbletea-v2、vendored-ui-stack、readline-repl-over-charm、typeahead-surface-lite、dependency-baseline-2026-08）、**`update-truth` 但保持 active** 的十个行为决策页（internal-llm-client、self-implemented-markdown、tabbed-select-component、host-integration、export-command、debug-request-inspector、compaction-event-store、mcp-graceful-degradation、terminal-title-from-session、rust-rewrite-feasibility）、以及 `rust-port-headless` 走 reversal 关闭；六个根页（architecture、stack、roadmap、background、flow、mindmap）全部重写；`docs/REPORT-PHASE-1.md` 可否逐字搬入 history；brainRoot 目录 `/Users/joyqi/Work/chatchain-brain` 是否改名（否则「无 chatchain 痕迹」的决策在记忆层永远差一半）。
17. **`AGENTS.md` 的入库策略**（从第 16 项里单列，因为它有一个非记忆层的后果）：该文件今天只有 brain 接线块（`AGENTS.md:1-15`），引用被 `.gitignore:37` 忽略的 `BRAIN.md`——对公开仓库的贡献者是死链接；而且 **iota 自己的 agent 模式会把项目根的 `AGENTS.md` 注入系统提示**（README:20），也就是用 iota 开发 iota 时，模型会被要求「通过 brain CLI 读写」而它多半没有那个 CLI。请在两者中选一：(a) **不入库**，`AGENTS.md` 继续和 CLAUDE.md/BRAIN.md/.mindmux 一起被忽略；(b) **入库但重写**为面向贡献者与 agent 的真指南（构建、测试、UI 触碰规则、残留门），brain 接线块移进被忽略的 `CLAUDE.md`。
18. **与 chatchain 的关系怎么说**（CHANGELOG 1.0.0 与 README 必须一致，不能既不提又写「原样沿用」）：(a) **明确说**——各一句「iota 由 chatchain 演化而来；chatchain 的配置与会话不会自动迁移」；(b) **明确沉默**——两处都不出现 chatchain，`~/.iota.yaml`/`~/.iota/sessions` 只作为本项目的路径描述出现。真实的存量用户是 chatchain 2.16.1（`~/.chatchain*`），Go 版 iota 从未发布、没有用户，所以原稿那句面向「Go-iota 用户」的 Upgrading 说明是没有对象的。`docs/COMPAT.md` 的前言按 §2.7 单独措辞，不受此项影响。
19. **x86_64 macOS 是否继续发布**：`macos-15-intel` 是 GitHub 最后一代 Intel 镜像，有已公布的退役日程；把它写死在 CI 与发布矩阵里，会在未来某次 tag 时突然失败。请决定：(a) 继续发 `x86_64-apple-darwin`，并接受「实施 Phase 2/3 时核实镜像可用期、退役前迁移到自托管或交叉编译」的后续维护；(b) 只发 `aarch64-apple-darwin`，CI 矩阵与 dist targets 同时去掉这一行，README/COMPAT 记一行。

## 7 附：被驳回的缺口声明

- **「44 个 chatchain 标签会让 `git describe`/cargo-dist 报 v2.16.1」**——不成立：`HEAD` 是孤儿提交，`git describe --tags HEAD` 直接失败；唯一风险是 `git push --tags`。
- **「Browser 面板空目录回退到 `.` 而不是 cwd」**——描述有误：两边都先取 cwd，差别只在 cwd 不可读时 Go 再退到 `$HOME`；出厂命令不可达。
- **「NO_COLOR 被完全忽略」**——部分有误：crossterm 的 Colored 门会剥掉颜色；真正的缺口是 SGR 属性照发、`\x1b[m` 退化与 TERM=dumb 不生效。
- **「Alt 和弦被静默丢弃」**——不准确：Alt+字母会插入裸字母（比丢弃更糟），只有 Ctrl+N/P/T/D、Ctrl+Home/End、PgUp/PgDn、Ctrl+V 被丢弃。
- **「Rust 的非 unix 分支是未测试的 stub」**——低估：crate 根本不能为 Windows 编译，这些分支是死代码。
- **「Windows 下 mode bits 与 Ctrl-C-only 是相对 Go 的缺口」**——不成立：Go 的 `os.WriteFile` 与 `signal.Notify` 在 Windows 上行为相同；阻塞点只有 `shell/exec.rs` 与缺失的构建。
- **「`cargo tree` 有 4 个重复 crate 版本 / 没有重复」**——两个数字都不可复现：现场为 7 个（含 dev 依赖）；以执行时输出为准，不设门。
- **「测试基线是 1404」**——记忆值：现场 737 + 655 个 `#[test]` 属性加 14 个 tmux 场景，按二进制的 `test result:` 行记录。
- **「chat/ 与 repl/ 是重复实现」**——不成立：repl 复用 chat（12 个 repl 文件导入它），重复只限 execution walk 与并行 batch 两处。
- **「OSC-52 剪贴板在移植中丢失」**——不成立：两棵树都没有，已记录。
- **「MCP verbose 日志在 Rust 丢失」**——不成立：Go 的 `logf` 在 `root.go:201` 为 nil，两边一样什么都不打。
- **「TLS 证书/代理行为有差异」**——不成立：rustls-platform-verifier 读系统证书库，系统代理环境变量被尊重。
- **「gofix 在 Go 树删除后会优雅 SKIP」**——不成立：脚本只在没有 Go 工具链时 SKIP，有 Go 的机器上 `go build` 因 `replace => ../../..` 消失而失败。
- **「`examples/mkbundle.rs` 依赖 Go 工具链，随 Go 一起删」**——不成立：它是纯 Rust（`use iota::session::SessionStore` 等），只是被往返脚本调用；删掉它会拔掉「Go 读 Rust 写的 bundle」这条手工验证路径唯一的 bundle 生成器，因为 headless `-m` 不创建会话。
- **「crates.io 上的 `iota` 可用」**——不成立：已被 dtolnay 的 `iota` 0.2.3 占用；上架需改包名 `iota-cli`。
- **「9 个 doc(hidden) 缝 / 396 个 test_ 函数 / 37 处 rust/ 路径 / 30 个 CliError 变体 / 4 个重复版本」**——计数错误：实测 8 / 559 / 14 / 33 / 7；所有作门槛的数字改由脚本现场生成。
- **「`chat/run.rs` 说 D-12 的参数摘要审批未移植」**——是过期注释，不是缺口（`src/tool/fmt.rs` 与 DIVERGENCES 均确认已移植）。
- **「TUI 已经过 tmux 与人工验证，可安全重构」**——反向被驳回：人工结果表全空、tmux 从未在 GitHub 上执行；这是本路线图把试用与 UI 触碰规则前置的原因。
- **「Rust 树在 Linux 上已经验证过」**——从未有人这样声称，但整份路线图曾隐含地这样假设：没有任何 Linux 运行记录存在，Phase 0 因此单独预算一天给首次 Linux 运行。
