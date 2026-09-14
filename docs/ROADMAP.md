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
| 4 | 告警出口与 `IOTA_LOG` | 进行中 | release 里 `tracing::warn!` 被编译掉，用户永远看不到 |
| 5 | **Windows TUI 首次人工验证**（`docs/TUI-VERIFY.md` §9） | **未做，需要一台 Windows** | 已发两版 Windows 二进制，零证据；tmux 自动化结构上够不到 |
| 6 | 删本地 44 个 chatchain 标签 | 完成 2026-09-14 | 一次 `git push --tags` 就污染组织仓库 |
| 7 | MCP clientInfo 接 `CARGO_PKG_VERSION` | 待做 | `mcp/manager.rs` 写死 `1.0.0`，每个 MCP 服务器都收到错误版本 |
| 8 | Responses API 空 schema 过滤 | 待做 | chat-completions 那路修了（`openai.rs:180`），`openresponses.rs:274` 没修 |
| 9 | `IOTA_TMUX_REQUIRED` / `IOTA_SANDBOX_REQUIRED` | 待做 | 本机缺依赖时套件静默跳过并通过；2026-09-13 六轮 CI 修的问题大半由此藏起 |
| 10 | cargo-deny | 待做 | 供应链检查，一个 `deny.toml` 加一个 CI job |

7–10 合起来约半天。

## 2. 该做（不阻塞 1.0）

- `src/ui/keys.rs`：98 行路由全部键盘输入，0 个 `#[test]`。先补键表测试再动任何路由。
- `tests/session/roundtrip.rs`：写出 bundle → 读回 → 逐字节相同。原是替代 Go 往返证明的，Go 没了它本身仍值得有。
- `scripts/size.sh --budget`：体积是 iota 相对同类唯一压倒性的差异化点（7.57 MiB vs crush 28 MB / opencode 46 MB / Claude Code 202 MB，见 brain `competitor-distribution-windows`），该做成硬门。
- `docs/design/` 补一篇配置三层的设计文档；`docs/history/README.md` 说明里面躺着的每份冻结文档。
- `tabbed.rs:160` 的 `current_dir().unwrap_or(".")` 哨兵改用 `app::user_home` 兜底。
- 官网同步：combo box、`NO_COLOR`、`IOTA_LOG` 三项落地后 `iotash/iota-website` 的文档页要跟上。

## 3. 重新规划后再做（目标成立，具体步骤已过时）

**测试树收敛**（原 Phase 4）：29 处 `impl Provider`、105 个文件带 `#![allow(clippy::unwrap_used…)]`、
`struct Surf` 9 份、`src/ui/testutil.rs` 不存在。目标与原 Phase 4 步骤 1–5 一致，可直接照做；
它是结构重构的前置，应先于下一条。约 4–6 天。

**原生结构**（原 Phase 5）：无环、无扁平状态袋、无零值哨兵（`Result<_, String>` 20 处、内联
`PoisonError` 121 处、`too_many_arguments` 4 处）。**方向对，但 §2.2 画的目标模块树是 9 月 8 日
的**——里面有已退役的东西（`tool/context.rs` 装 `DelegationLedger`），没有这周加的东西
（`config/{params,strict}.rs`、`shell/{interp,jobs}.rs`）。**先对着今天的 `src/` 重画树，再一模块一
PR 地做。** 原 Phase 5 的纪律保留：纯 `git mv` 与代码改动分开提交；golden 不许 re-bless 除非附
CHANGELOG 行；触碰 `src/ui/**` 的 PR 按批跑 TUI-VERIFY。

**Go 坐标注释**（原 Phase 6 的核心）：2755 处 `*.go:NNN`、833 处 `// Go:`、159 处 `WPnn`、93 处
`CONTRACTS` 指向没人能打开的文件。**问题是真的，原方案的棘轮机器（`check-residue.sh` 基线、
`code-tokens.py`、a/b/c 分类、`Pinned by` 校验）对一人项目过重。** 改为一次性工作：原文档自己承认
真正承载 rationale 的只有 tool/ 里约 80 处 Go 句子——把那 80 处改写成行为句，其余在结构重构触碰时
顺手删，不触碰的文件留着不动。`docs/ARCHITECTURE.md` 的 §0 graft ledger / §2 Go 映射表 / §12 work
packages 三节随重构一起重写。

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
