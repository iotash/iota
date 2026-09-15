# iota 路线图

写于 2026-09-15。取代 `docs/history/MIGRATION-ROADMAP.md`（2026-09-08 的 74-agent 审计产物）。
那份文档的**分析**仍然有效——§2.4 注释改写规则、§2.5 测试金字塔、§3 缺口表、Phase 4/5 的步骤
分解——但它的**骨架是为移植搭的**，而项目已经不在移植：Go 树退役、仓库迁入 `iotash`、CLI 改为
agent-first、Windows 已支持、0.1.0 与 0.2.0 已发布。它的 67 条步骤里 17 条已完成、11 条前提消失。
这份只写今天还成立的事，按「最可能伤到用户」排序，不按原 Phase 顺序。

决策记录在 brain（`node ~/.claude/skills/brain-page/bin/brain.mjs read-page <id>`）；
偏离与回归记录在 `docs/DIVERGENCES.md`。

## 1. 必须（1.0 之前）

| # | 项 | 状态 | 依据 |
|---|---|---|---|
| 1 | `edit_file` 字节保真 | 完成 `7bec70f` | 数据损坏级，已随 0.1.0/0.2.0 发出；brain `port-regressions` R-01 |
| 2 | 模型选择器接候选集、并发展开 `provider:*`、combo box | 完成 `20e574b`…`ced2b73` | brain `config-three-layers` 的最后一步 |
| 3 | `NO_COLOR` / `TERM=dumb` / 非 tty 的 ColorMode | 完成 `e0ebf7e` | 管道与 CI 用户直接受影响 |
| 4 | 告警出口与 `IOTA_LOG` | 完成 `31b9d99` | release 里 `tracing::warn!` 被编译掉，用户永远看不到 |
| 5 | **Windows TUI 首次人工验证**（`docs/TUI-VERIFY.md` §9） | **未做，需要一台 Windows** | 已发两版 Windows 二进制，零证据；tmux 自动化结构上够不到 |
| 6 | 删本地 44 个 chatchain 标签 | 完成 2026-09-14 | 一次 `git push --tags` 就污染组织仓库 |
| 7 | MCP clientInfo 接 `CARGO_PKG_VERSION` | 完成 `887de83` | `mcp/manager.rs` 写死 `1.0.0`，每个 MCP 服务器都收到错误版本 |
| 8 | Responses API 空 schema 过滤 | 完成 `a699a60` | chat-completions 那路修了（`openai.rs:180`），`openresponses.rs:274` 没修 |
| 9 | `IOTA_TMUX_REQUIRED` / `IOTA_SANDBOX_REQUIRED` | 完成 `f385b8c` | 本机缺依赖时套件静默跳过并通过；2026-09-13 六轮 CI 修的问题大半由此藏起 |
| 10 | cargo-deny | 完成 `d97a95c`（拦下 rustls RUSTSEC-2026-0285，升 0.23.45） | 供应链检查，一个 `deny.toml` 加一个 CI job |

必须清单只剩第 5 条（Windows TUI 人工验证），其余九条已完成。

## 2. 该做（不阻塞 1.0）

- `src/ui/keys.rs`：98 行路由全部键盘输入，0 个 `#[test]`。先补键表测试再动任何路由。
- `tests/session/roundtrip.rs`：写出 bundle → 读回 → 逐字节相同。原是替代 Go 往返证明的，Go 没了它本身仍值得有。
- `scripts/size.sh --budget`：体积是 iota 相对同类唯一压倒性的差异化点（7.57 MiB vs crush 28 MB / opencode 46 MB / Claude Code 202 MB，见 brain `competitor-distribution-windows`），该做成硬门。
- `docs/design/` 补一篇配置三层的设计文档；`docs/history/README.md` 说明里面躺着的每份冻结文档。
- `tabbed.rs:160` 的 `current_dir().unwrap_or(".")` 哨兵改用 `app::user_home` 兜底。
- 官网同步：combo box、`NO_COLOR`、`IOTA_LOG` 三项落地后 `iotash/iota-website` 的文档页要跟上。

## 3. 重新规划后再做（目标成立，具体步骤已过时）

**测试树收敛**（原 Phase 4）：**已完成（2026-09-15，4a `91e2b4d…a5b60d3` + 4b `f6a0ecb…2790dff`）**。31 处 `impl Provider` → `src/testing/provider.rs` 的一个 `FakeProvider` builder；七份 `Surf` → `src/ui/testutil.rs`；8 条 `#[doc(hidden)]` 缝删除、缝后测试进 lib；`NewSession` 取代 47 处位置参数；lint 头 tests/ 下只剩 12 个 main.rs（unwrap-allow 文件 107 → 46）；`assert_cmd` 与 `tests/provider/progress.rs` 删除；`cargo test` 墙钟 29.5s → 13.7s。未做：session 的 24 个磁盘格式面 pub 降级（需把 71 个写盘读回测试搬进 src）；36 处真实时钟 sleep 保留（子进程 / wiremock socket / tmux 节拍 / UI 真线程，逐条理由在 `2790dff` 的提交信息）。

**原生结构**（原 Phase 5）：**已完成（2026-09-16，19 个 PR，三路 agent 各自 worktree 并行，逐 PR ff/cherry-pick 进 main，远端三平台每步全绿）**。
落地：`app/` 合并（vars/color/diag/paths，唯一 `Env` 缝，`VERSION` 归 app）；`tool/context.rs`（原 chat/turns）、`tool/{dispatch,approval,defer/,builtins/}`；`chat/` → `headless/` + `TurnParams`；`config/window.rs`、`Config::provider()`、唯一 api_key 优先级；`mcp/manager` 吸收 naming/status + `ServerState` 枚举；`shell/sandbox/` + `Outcome` 枚举；`background` 改 tokio、所有权一次查表；`cmd/{args,error,interactive/}` + `CliError` 三分；`ui/{runtime,render,input,surface}` 分组 + 共用 `Editor`（Ctrl+W 统一，X-35）；`repl/{turn,render,context,state}` 分组 + `Repl` 三分 + `TurnEngine`；`markdown::Writer` over `enum Block` + `blocks/`、`preview.rs` 定义 `PreviewHandle`（markdown → ui 边消失）、mathtext 成叶子、HTML 转义器与 `escape_len` 各一份；`tests/layering.rs` 钉住分层（ci.sh 的三条 grep 退役）；`Result<_, String>` 22 → 0；内联 `PoisonError` 121 → 4（helper 自身，`scripts/check-poison.sh` 作为回归门保留在 4）；`too_many_arguments` 4 → 0。
与规划不同的：`tool/yaml11.rs` 留在 tool/（layering 证明 tool 消费它，搬上去成环）；mathtext 的 `""` 哨兵只在三处成立（Delim 两侧、`read_delim_symbol`、BigOp 形态），其余 String 是真内容，不 Option 化；`llm::stream` 泛型未做（规划已降为可选）。
未做（进积压）：`shell/interp.rs` 的 `std::env::var` 与 `find_in_path` 的 PATH 读取未走 Env 缝（每次 spawn 重解析是既有行为）；session 的 24 个磁盘格式面 pub 降级（需把 71 个写盘读回测试搬进 src，见 4b 记录）。

**Go 坐标注释**（原 Phase 6 的核心）：**tool/ 部分已完成（2026-09-16，18 个提交，143 行 → 0；模型可见文本里的「Go regular expression」等刻意保留）**；`docs/ARCHITECTURE.md` §0 graft ledger / §12 work packages 已删，§2 改为按 `tests/layering.rs` 的分层声明写的模块树。其余目录的 `*.go:NNN` 坐标按「触碰即删」处理，不做全仓扫荡。

## 4. 明确放弃（前提已消失）

- `DIVERGENCES.md` 改名 `COMPAT.md`、345 处引用加 `COMPAT ` 前缀：我们没有 Go 兼容契约了。账本现在
  记的是 X 系列（Rust 自己的决定）与 R 系列（移植回归），不是「对 Go 的偏离」。前缀只为残留扫描器
  服务，没有扫描器就是空动作。
- `export-go-map.sh`、`go-last-fixes` 双标签、`pre-deport` 标签：都是给 Go 树留后路的，树已不在主线历史。
- `docs/history/go-test-map.tsv`、`baseline-2026-09.md`：同上。
- 原 Phase 6 步骤 7「发 1.1.0」：版本走 0.x。
- `scripts/go-session-roundtrip.sh` 留在 `ci.sh`：已删，`docs/history/ROUNDTRIP-FINAL.md` 是它的终态记录。

## 5. 1.0 的条件

第 1 节 10 条全部完成，**且**拿到一次真实的 TUI-VERIFY 读数（至少 §9 Windows + §1 IME/CJK +
§3 闪烁 + §5 窗口标题四节，在 Ghostty、Terminal.app 与 Windows Terminal 上各一次）。
在此之前 0.x 是诚实的版本号。发布流程本身已就绪（cargo-dist 五目标、三种 installer、
`iotash/homebrew-tap`、macOS 签名门、`CHANGELOG.md` 同源），差的是内容与证据，不是管线。
