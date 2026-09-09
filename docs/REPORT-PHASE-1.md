# iota-rs — Phase 1 重写效果评估报告

日期：2026-08-31。性质：在 `EVALUATION.md`（34-agent 权威评估）之上的一次**独立复核评估** ——
通读 `rust/docs/` 四份文档（ARCHITECTURE / DIVERGENCES / EVALUATION / SIZE），外加一组只读
抽验命令（不改任何代码）核对硬指标。本报告不重复 EVALUATION 的细节，只给结论、复核结果与
风险判断；分歧台账以 `DIVERGENCES.md` 为准，硬数据以 `EVALUATION.md` / `SIZE.md` 为准。

---

## 0. 总体判断

**这次重写超出了预期目标，是一次高质量、可验证的成功移植** —— 但它是一个
"headless 嵌入式内核"的成功，不是整个产品的成功，且引入了长期双维护成本。

以"嵌入式 headless 路径"为标准：交出了 5–14× 体积、~2.6× 内存、5.7× 启动的硬收益，
行为对等做到逐字节可审计（53 场景 0 未声明分歧），代码质量门禁全绿且文档数字可复现。
代价是 ~3.4 万行新代码的长期双维护，以及 TUI / 会话 / Windows 留给 phase 2。

当前状态按 EVALUATION 的结论"**可先 ship 嵌入场景、跟进 16 条 minor 发现**"是站得住的，
前提是先处理 §5 的第 1–3 条（记录保全、唯一的行为顺序偏离、不变量守卫）。

---

## 1. 初始目标 vs 实际结果

项目动机（brain：`rust-port-headless` / `rust-rewrite-feasibility`）：嵌入式部署，更小的
二进制与内存。实测（Darwin/arm64，数据源 EVALUATION §2 / SIZE.md）：

| 指标 | Go | Rust | 改善 |
|---|---:|---:|---:|
| 二进制（默认全功能） | 23.68 MiB | 4.37 MiB | **5.4×** |
| 二进制（`openai,shell` 最小嵌入） | — | 1.66 MiB | **14.3×** |
| 二进制（最小 + ring TLS） | — | 2.26 MiB | 10.5× |
| 峰值 RSS（各场景） | 26–27 MiB | 10–11 MiB | **~2.6×** |
| 启动中位（fail-fast，20 次） | 22.1 ms | 3.9 ms | **5.7×** |
| 单次运行墙钟（mock 无延迟） | — | — | ~2× |

要点：

- ARCHITECTURE §11 事前预估默认构建 ≈ 8–10 MB，实测 4.4 MiB —— **比自己的保守预估还好
  一倍**；feature 分层（`mcp` −0.70 MiB、ring TLS −0.63 MiB、provider/toolset 裁剪最多
  −2.8 MiB）让嵌入场景可按部署裁剪。
- `SIZE.md` 由 `ci.sh` 每次运行重新生成，数字不会腐烂 —— 好设计。

---

## 2. 行为保真度：整个工程最扎实的部分

53 个差分对等场景（双二进制对 recording mock 跑同样输入，逐字节比对
stdout/stderr/exit/请求体），结果（EVALUATION §3.5）：

| 结论 | 数量 |
|---|---:|
| 逐字节相同（identical） | 23 |
| 等价（引用 DIVERGENCES 条目） | 19 |
| 有意修复 / 范围行为（intended） | 8 |
| 交互专属 flag 被拒（D-23） | 3 |
| **未声明的行为分歧（divergent-bug）** | **0** |

支撑纪律：

- **52 条分歧全部台账化**：F-01…F-09（修掉的 Go bug）、I-01…I-08（有意偏离）、
  D-01…D-35（设计级偏离）；每条非 identical 场景都引用具体条目编号。
- 顺带修掉 9 个 Go 真 bug（`--mcp ""` panic、`-m ""` 掉进 TUI、稀疏 tool-call 索引丢调用、
  `"error": null` 误判、read_file 超长单行死窗口、MCP/delegate map 随机序等）。
- 这不是"重写差不多像"，是"重写到逐字节可审计"。

---

## 3. 工程质量

- **分层**：6-crate 严格符合绑定架构；`iota-chat` 测不到 reqwest/rmcp，重依赖隔离在叶子
  crate；33 个直接依赖走 allowlist 门禁（`scripts/direct-deps.allow` + `check-deps.sh`）。
- **门禁**：全仓 `#![forbid(unsafe_code)]`、`unwrap/expect/panic` deny、clippy pedantic
  `-D warnings`、rustdoc `-D warnings`、stub 门禁、aws-lc-rs 泄漏门禁、feature 矩阵构建。
- **设计层面消掉 Go 的历史坑**：`LastUsage()/LastRawContent()` 时序竞争 → 结果随返回值
  （`ChatResult`/`RoundResult`）；map 随机序 → `BTreeMap`/按名排序；环境注入
  （`HostDirs`/`EnvSource`）让测试不改进程 env。
- **测试**：16,390 实现行 : 18,005 测试行（1.10:1）；194 个 Go 测试带 `// Go: file:line`
  锚点一对一移植，其余 191 个为设计新增（含 16 个 assert_cmd 端到端）。

---

## 4. 独立抽验：文档数字是否可信

评估不是只转述文档 —— 用只读命令复核了硬指标，**全部对得上**：

| 文档声称 | 复核命令（只读） | 实测 |
|---|---|---|
| 385 tests / 0 failed / 0 ignored | `cargo test --workspace --all-features` | ✅ 385 passed, 0 failed |
| 16,390 实现 + 18,005 测试行 | src 去 `#[cfg(test)]` 计数 + `tests/` 计数 | ✅ 16,390 + 4,360 + 13,645 = 18,005，精确吻合 |
| 194 个 Go 测试锚点 | `grep -rn "// Go: " crates` | ✅ 197 处（略多，含少量多锚点/非测试引用，无异常） |
| 全仓零 unsafe | `grep -rn "unsafe " crates` | ✅ 2 处命中均为测试 fixture 字符串（`"unsafe prompt"`），无真实 unsafe |
| 零 `todo!()` / 桩 | `grep -rn "todo!()\|unimplemented!()"` | ✅ 0 处 |
| ci.sh 门禁与文档一致 | 阅读 `ci.sh` / workspace `Cargo.toml` | ✅ 顺序与内容符合 §10/§4.6 描述 |

结论：**EVALUATION.md 的数字是可复现的，没有注水**。加上评估流程本身
（"34-agent 评估 + 对抗验证、16 条发现全部实锤为 minor、评估期间未改任何代码"），
这份自评的可信度远高于一般的项目自评。

---

## 5. 问题与风险（按认可的优先级）

1. **文档债有时间窗**：4 条行为偏离（completeness-1…4）+ 1 条 YAML 警告文本只记在
   scratchpad 的 DEVIATIONS.md 里，没进 `DIVERGENCES.md`。scratchpad 一旦丢弃，记录就丢
   —— 应最先补上（一个下午的工作量）。
2. **correctness-1 是唯一涉及可观察行为顺序的发现**：`--output-format` 解析提前于 Go 的
   位置，而 `lib.rs` 注释仍声称顺序 "kept EXACTLY"。要么改回 Go 位置，要么记录偏离并删
   注释 —— **代码注释与事实相悖比偏离本身更糟**。
3. **测量平台单一**：所有体积/RSS/启动数据都是 Darwin/arm64。嵌入式部署大概率是
   Linux —— 建议在 Linux 上复测一轮同样的测量再宣布嵌入目标达成（SIZE.md 随 CI 重新
   生成，但 RSS/启动表不会）。
4. **双维护成本（最大的战略风险）**：Go 仍是全功能真源，headless 路径的任何 Go 侧演进
   现在都要人工同步到 Rust。差分测试框架（53 场景）目前是 scratchpad 产物 ——
   - 若 Go 还会继续开发：值得把差分套件固化成 CI job 当漂移检测器；
   - 若 Go 侧冻结、Rust 是未来方向：应尽快推进 phase 2，缩小"两套真源"的窗口。
5. **未固化的不变量**：TLS 互斥只靠两个 pinned 组合的 CI grep，任意用户
   `--features tls-ring`（不关 default）会把两个 crypto provider 链进同一个二进制
   （idiom-5，`compile_error!` 一行可解）；`security-4` 凭据 header 未 `set_sensitive`
   是潜伏泄漏点（今天无调用点，明天可能就有）。
6. **范围缺口**（已知且诚实记录，EVALUATION §7）：TUI / 会话 / 图片编辑端点 / 观察者 /
   Windows 全部未移植；`-S`/`--resume`/`--no-save` 明确拒绝（D-23）。若嵌入目标含
   Windows（I-07），需另立 phase（沙箱故事 + 路径/信号映射）。

---

## 6. 建议的下一步顺序

1. 补 5 条 DIVERGENCES 行（时间敏感，防记录丢失）；
2. `correctness-1` 二选一了断（改回 Go 位置，或记录偏离 + 删 "kept EXACTLY" 注释）；
3. TLS `compile_error!` 守卫 + `rmcp = { workspace = true }`（两个一行修复，守住 CI 假设
   的不变量）；
4. hygiene 批量小修：`set_sensitive`（security-4）、SKILL.md 读上限（security-3）、
   `map_llm_err` / bearer-header helper（idiom-3/8）、`default_http_client` 诚实的
   expect（idiom-9）、`is_zero` 门禁加宽（idiom-10）、`deferred_tools` 快照（idiom-7）、
   `rel()` 走 `Component`（idiom-11）；
5. Linux 平台复测一轮 footprint（体积 / RSS / 启动）；
6. 然后再谈 phase 2：会话 headless store（`--resume`/`--no-save`，D-23 拒绝点只有一处，
   数据模型只差 D-11 两个字段）→ ratatui TUI（seam 已留好：`StreamSink` 渲染边界、
   D-12/D-20 的 trait 默认方法待填）→ Windows。

---

## 7. 一句话总结

以"嵌入式 headless 路径"为标准，这次 Go→Rust 重写交出了 5–14× 体积、~2.6× 内存、
5.7× 启动的硬收益，行为对等逐字节可审计（53 场景 0 未声明分歧），代码质量门禁全绿且
文档数字可复现 —— 属于罕见的高完成度移植；代价是 ~3.4 万行新代码的长期双维护，以及把
TUI / 会话 / Windows 留给了 phase 2。**先补记录、了断 correctness-1、守住不变量，即可
放心 ship 嵌入场景。**
