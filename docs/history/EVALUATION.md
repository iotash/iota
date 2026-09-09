# iota-rs — EVALUATION

Definitive evaluation of the Rust port of the Go CLI `iota` (phase 1: headless
`-m` / `-l` path). Sources: the footprint measurements in the eval scratchpad
(`MEASURE.md`, raw data `results.json`), the four behavioural-parity reports
(53 scenarios, both binaries driven against recording mocks), the VERIFY-pass
implementation report, the confirmed code-review findings, `DIVERGENCES.md`,
and `SIZE.md`. Measured on Darwin/arm64 (Darwin 25.6.0), rustc 1.98.0,
Go binary built `go build -trimpath -ldflags="-s -w"`. Date: 2026-08-31.

## Package status

Historical record of the work packages (columns: id, name, the crate that then owned it, landed).
Since 2026-09-02 (§12) the crates are modules of one package. `scripts/check-stubs.sh` reads the
rows again for the T3 fan-out (phase 4 below): a `todo!()` body is legal only under a `// WPxx-STUB`
header, and a header only while its row says `no`; once every row is `yes` the gate is the plain
"no `todo!()`, no header anywhere under `src/` and `tests/`" refusal.

| id | name | crate | landed |
|---|---|---|---|
| WP00 | scaffold | workspace | yes |
| WP01 | core-logic | iota-core | yes |
| WP02 | wire | iota-llm | yes |
| WP03 | openai | iota-llm | yes |
| WP04 | responses | iota-llm | yes |
| WP05 | anthropic | iota-llm | yes |
| WP06 | google | iota-llm | yes |
| WP07 | images | iota-llm | yes |
| WP08 | tool-framework | iota-tools | yes |
| WP09 | shell | iota-tools | yes |
| WP10 | code | iota-tools | yes |
| WP11 | agents | iota-tools | yes |
| WP12 | mcp | iota-mcp | yes |
| WP13 | chat-loop | iota-chat | yes |
| WP14 | config-resolve | iota | yes |
| WP15 | wiring-bin | iota | yes |

Every package reported green at hand-over; no package was left red.

Phase 2 · slice 1 (headless session store) landed as six further packages. They are listed here for the
record; no phase-2 file ever carried a stub header. Detail: §9.

| id | name | crate(s) | landed |
|---|---|---|---|
| WP-S0 | message-fields | iota-core | yes |
| WP-S1 | session-crate | iota-session | yes |
| WP-S2 | chat-delta | iota-chat | yes |
| WP-S3 | go-fixtures | (Go module + fixtures) | yes |
| WP-S4 | cli-resume | iota | yes |
| WP-S5 | docs-ci | (docs + ci.sh) | yes |

Phase 3 · TUI slice (scaffolded by WP40 per `scratchpad/tui/design/TUI_WPS.md`;
`check-stubs.sh` reads these rows exactly like the phase-1 table — a `no` row may still
carry `// WPxx-STUB` headers and `todo!()` bodies; each package flips its own row).
WP40–WP52 are tier 1; WP53–WP55 are tier 2, strictly additive.

| id | name | crate | landed |
|---|---|---|---|
| WP40 | tui-contracts-scaffold | workspace | yes |
| WP41 | markdown-core | iota-markdown | yes |
| WP42 | markdown-blocks | iota-markdown | yes |
| WP43 | tui-region-sink | iota-tui | yes |
| WP44 | tui-terminal-frame | iota-tui | yes |
| WP45 | tui-facade-handle | iota-tui + iota-core | yes |
| WP46 | tui-composer-queue | iota-tui | yes |
| WP47 | tui-surfaces | iota-tui | yes |
| WP48 | repl-transcript-diff | iota-repl + iota-core + iota-tools | yes |
| WP49 | repl-turn-engine | iota-repl + iota-chat + iota-tools | yes |
| WP50 | repl-run-commands | iota-repl + iota-session + iota-tools | yes |
| WP51 | bin-interactive-mcp | iota + iota-mcp | yes |
| WP52 | tmux-l4-docs | iota-tui + docs | yes |
| WP53 | tokens-compaction | iota-repl | yes |
| WP54 | model-panels-file | iota-tui + iota-repl | yes |
| WP55 | highlight-tooldelta | iota-markdown + iota-llm | yes |

Phase 4 · T3 full feature parity (scaffolded by WP60 per `scratchpad/t3/design/T3_WPS.md`;
`check-stubs.sh` reads these rows: a `no` row may still carry `// WPxx-STUB` headers and `todo!()`
bodies; each package flips its own row when it lands and strips its headers). One package now, so
the crate column names the modules the package owns (T3_WPS §2).

| id | name | crate | landed |
|---|---|---|---|
| WP60 | t3-scaffold | shared seams (facade/sink/provider/run/cmd/commands), every new module stub, test skeletons, fixtures | yes |
| WP61 | mathtext-parser-inline | `mathtext/{mod,symbols,macros,delim,parse,inline}` + the inline markdown hook | yes |
| WP62 | mathtext-layout-2d | `mathtext/{pict,layout}` + the display markdown hook | yes |
| WP63 | imgterm-widget-partials | `imgterm`, `ui/region`, `repl/{transcript,group,turn,replay}`, openresponses partials | yes |
| WP64 | edit-redo-picker | `ui/surface` Picker, `repl/editpicker`, `repl/commands/edit`, `llm/{images,multipart}`, `provider/images` | yes |
| WP65 | export | `repl/commands/export`, `markdown/{html,highlight}`, `session/{loader,store}` | yes |
| WP66 | debug-reqlog | `llm/{reqlog,client}`, `repl/commands/debug`, `provider/{image_util,imagen}` | yes |
| WP67 | host-progress-notify | `host/**`, `llm/progress`, `repl/phases`, `ui/{msgs,handle,event_loop,term,osc}` | yes |
| WP68 | skills-completeness | `repl/commands/skills`, `agents`, `repl/title`, `ui/suggest`, `tool/fmt` | yes |
| WP69 | t3-verify | tmux scenarios, docs, `NEEDS:` fixes, stub gate green | yes |

---

## 1. Summary verdict

**The Rust port is fit for its stated goal — a materially smaller footprint
for embedded use of the headless `-m` / `-l` path — and is behaviourally
faithful to the Go binary on that path.**

The evidence, in one paragraph: the default Rust binary is **5.4× smaller**
than the Go binary (4.4 MiB vs 23.7 MiB) and a minimal embedded build
(`openai,shell`) is **14.3× smaller** (1.7 MiB); peak RSS is **~2.6× lower**
in every measured scenario (≈10–11 MiB vs ≈26–27 MiB); startup is **~5.7×
faster** (3.9 ms vs 22.1 ms median to a fail-fast exit); and across **53
differential parity scenarios** (CLI errors, chat/report, tools/agent/
delegate, MCP) there are **0 divergent-bug outcomes** — 23 scenarios are
byte-identical, 19 differ only in ways an approved divergence entry declares,
8 are approved intentional fixes of Go bugs, and 3 are interactive-only flags
deliberately rejected. The full CI gate (fmt, dependency allowlist, stub gate,
clippy `-D warnings` with pedantic, rustdoc `-D warnings`, 391 tests, reduced
feature builds, aws-lc-rs leak gate, size matrix) exits 0.

> **Framing correction (2026-09-01).** The 5.4× and 14.3× figures above compared
> *headless-only* Rust builds — no TUI, no token accounting, no syntax highlighting,
> and for 14.3× a single provider, a single toolset and no TLS at all — against the
> *full* Go binary, which carries all of that. The like-for-like number is the
> unified one-binary Rust build against Go: **10,034,608 B (9.57 MiB) vs
> 24,827,602 B (23.68 MiB) = 2.47× smaller** (§11). The RSS, startup and parity
> findings are unaffected by this; the feature-pruned builds no longer exist.

What is missing or imperfect:

- **Scope**: everything interactive is out by design — TUI, sessions/resume,
  compaction/export, the ask toolset, image edits, observers, Windows (§7).
  The port is a headless tool, not a full replacement. *(Sessions and
  `--resume` have since shipped as phase 2 slice 1 — §9; the rest of this
  bullet still holds.)*
- **Review findings**: 16 confirmed findings, **all minor, none blocking**
  (§5). The most consequential are one error-precedence deviation the code
  claims not to have (`correctness-1`), an unenforced TLS-backend exclusivity
  invariant (`idiom-5`), credential header values not marked sensitive
  (`security-4`), and four behavioural deviations recorded only in scratchpad
  notes instead of `DIVERGENCES.md` (`completeness-1..4`).
- **Docs debt**: those four missing `DIVERGENCES.md` rows plus one noted in
  the cli-errors parity report (config-file YAML parse-warning text) should
  land before the scratchpad is discarded, or the record of the divergences is
  lost.

Verdict: **ship for embedded/headless use now; fix the findings in a small
follow-up; treat sessions/TUI/Windows as phase 2** (§8). *(That follow-up has
since landed — the two bullets above are the state at evaluation time; see the
§5 addendum and its `outcome` column for what each finding became.)*

---

## 2. Footprint

### 2.1 Binary size (stripped, bytes)

Release profile: `opt-level="z"`, `lto="fat"`, `codegen-units=1`,
`panic="abort"`, `strip=true`. Go: `go build -trimpath -ldflags="-s -w"`.

| Binary | Features | Bytes | MiB | vs Go |
|---|---|---:|---:|---:|
| iota-go | full CLI (Go) | 24,827,602 | 23.68 | 1.00× |
| iota-rs default | all providers, all tools, MCP, aws-lc-rs TLS | 4,578,016 | 4.37 | **5.42× smaller** |
| iota-rs all, no MCP | `all-providers,all-tools,tls-aws-lc` | 3,860,368 | 3.68 | 6.43× smaller |
| iota-rs min + ring TLS | `openai,shell,tls-ring` | 2,373,536 | 2.26 | 10.46× smaller |
| iota-rs min | `openai,shell` (no TLS backend) | 1,740,304 | 1.66 | **14.27× smaller** |

Measured feature costs: the `mcp` feature costs ~0.70 MiB (default vs no-MCP
build); the ring TLS backend costs ~0.63 MiB on the minimal build. `docs/SIZE.md`
is regenerated by `ci.sh` on every run, so these numbers cannot go stale.

*(Phase-1 record. Every row but the Go one is a headless-only build against the
full Go binary — see the correction in §1; the like-for-like figure and the
current single-row `docs/SIZE.md` are §11.)*

### 2.2 Peak RSS and wall time per scenario

Mock OpenAI server on localhost (no artificial latency), identical env and
cwd for both binaries, 3 runs each, medians shown (full per-run data in the
eval scratchpad's `results.json`).

| Scenario | Go RSS | Rust RSS | RSS ratio | Go wall (ms) | Rust wall (ms) | Wall ratio |
|---|---:|---:|---:|---:|---:|---:|
| list (`-l`) | 27,459,584 (26.2 MiB) | 10,534,912 (10.0 MiB) | 2.61× | 24.8 | 10.7 | 2.3× |
| message-text (unary, ~9 KB reply) | 27,557,888 | 10,715,136 | 2.57× | 24.2 | 10.9 | 2.2× |
| message-json (`--output-format json`) | 27,738,112 | 10,747,904 | 2.58× | 24.0 | 11.2 | 2.1× |
| tool-round (SSE tool call → sandboxed `echo` → SSE answer) | 28,737,536 (27.4 MiB) | 11,272,192 (10.8 MiB) | 2.55× | 33.9 | 19.2 | 1.8× |

Rust uses ~2.6× less peak memory in every scenario and finishes each run in
roughly half the wall time (both dominated by process startup; the mock adds
no delays).

### 2.3 Startup latency

Fail-fast run (`<bin> nope-provider -m hi -M m -k k`, exits 1 before any
network I/O), 20 runs:

| Binary | Median ms | Min | Max |
|---|---:|---:|---:|
| iota-go | 22.1 | 21.7 | 24.7 |
| iota-rs default | 3.9 | 3.6 | 5.0 |

Rust starts **~5.7× faster** — relevant for embedded invocations that shell
out per request.

---

## 3. Behavioural parity matrix

53 scenarios across four differential suites, every one run on both binaries
with identical inputs (fresh `HOME`, fixed minimal env, recording mocks that
capture every request body). Verdict legend: **identical** = byte-equal
stdout + stderr + exit (and, where captured, byte-identical mock traffic);
**equivalent** = differs only as a cited POLICY/DIVERGENCES entry declares;
**intended** = approved divergence with user-visible effect (bug fix F-xx or
scoped behaviour); **unsupported** = interactive-only feature deliberately
rejected; **bug** = undeclared behavioural divergence.

### 3.1 cli-errors (16 scenarios)

| # | Scenario | Verdict |
|---|---|---|
| s01 | no provider argument | equivalent (I-04: no cobra usage dump) |
| s02 | unknown provider with configured aliases | equivalent (I-04; 3-line error byte-equal) |
| s03 | missing API key | equivalent (I-04) |
| s04 | `-m ""` | intended (F-03: Go heads for the TUI; Rust errors cleanly) |
| s05 | `--mcp ""` | intended (F-02: Go panics with a goroutine trace; Rust errors, exit 1) |
| s06 | `--output-format` without `-m` | equivalent (I-04) |
| s07 | `-S` | unsupported (D-23: rejected with the declared error, no request sent) |
| s08 | `--resume` | unsupported (D-23) |
| s09 | `--no-save` | unsupported (D-23) |
| s10 | `-m` without `-M` | equivalent (I-04) |
| s11 | invalid config temperature 3.5 | equivalent (I-04; float rendering `3.5` matches, D-09) |
| s12 | unknown toolset key warning | **identical** (incl. captured POST bodies) |
| s13 | config file with YAML parse error | equivalent (warning frame byte-equal; embedded text is yaml.v3 vs serde_norway — D-15 class) |
| s14 | `-l` with no config | **identical** |
| s15 | `-l` with aliases configured | **identical** |
| s16 | `-l openai` against mock | **identical** (same `GET /v1/models`, same auth header) |

### 3.2 chat-and-report (12 scenarios)

| # | Scenario | Verdict |
|---|---|---|
| 1 | text-mode reply (unary, 9 KB) | **identical** (stdout and request bodies byte-equal) |
| 2 | JSON report | equivalent (`duration_ms` wall-clock only; every pinned key equal) |
| 3 | reasoning-only reply, text mode | **identical** (think block routed to reasoning, nothing printed) |
| 4 | reasoning-only reply, JSON | equivalent (`duration_ms` only) |
| 5 | HTTP 400 error | equivalent (I-04; `Error:` line byte-equal, neither retried) |
| 6 | 429 + `Retry-After: 1` then 200 | **identical** (both retried once after ~1 s; 2 hits each) |
| 7 | 500 + `x-should-retry: false` | equivalent (I-04; both honoured the no-retry header, 1 hit each) |
| 8 | plain JSON body where SSE expected | equivalent (I-04 + D-08; `ErrNoEvents` text byte-equal) |
| 9 | `-m -` message from stdin | **identical** (trimmed identically on the wire) |
| 10 | system prompt via `-s` | **identical** |
| 11 | system prompt via config `system:` | **identical** |
| 12 | system prompt via config `system_file:` | **identical** (file content verbatim incl. trailing newline) |

### 3.3 tools-and-agent (12 scenarios)

| # | Scenario | Verdict |
|---|---|---|
| s1 | bash + `auto_run`, sandboxed `echo parity` | **identical** (tool result `"parity\n"` in both) |
| s2 | bash without auto_run → refusal to the model | **identical** (refusal text byte-equal) |
| s3a | code toolset glob→grep→read→edit, `auto_write` | **identical** (all tool results and on-disk mutation byte-equal) |
| s3b | same loop, edit refused (no auto_write) | **identical** |
| s4-text | `--max-turns 2` exhaustion, text | equivalent (I-04; error text byte-equal) |
| s4-json | same, JSON report | equivalent (I-04 + `duration_ms`) |
| s5 | `--agent`: AGENTS.md chain + skills catalog + load_skill | **identical** (system message and skill body byte-equal) |
| s6a | delegate to read-only child on second provider | **identical** (child advertised 4 read tools; ledger equal) |
| s6a-json | same, JSON report | equivalent (`duration_ms` only; `delegated:` block equal) |
| s6b | delegate to write-capable child | **identical** (6 tools, description reflects capability) |
| s7 | `${var}` expansion in provider + MCP config | equivalent (I-05: Rust warns on the MCP connect failure, Go silent; expansions byte-equal) |
| s8 | parallel read-only tool batch | **identical** (4 results in call order, byte-equal) |

### 3.4 mcp (13 scenarios)

| # | Scenario | Verdict |
|---|---|---|
| s1 | `--mcp` stdio server, tool listing to the model | **identical** (LLM request byte-identical) |
| s2 | tool call routed to the server | equivalent (D-07/D-08 key order; D-02 SDK `_meta`) |
| s3 | server dies at connect | intended (I-05: Rust prints the warning + captured stderr; Go silent) |
| s4 | `defer:` group, search_tools manifest + load | equivalent (leg 1 byte-identical; D-07/D-08 on the replay leg) |
| s5a | `defer_mode: reference`, alias without `type:` | intended (F-01: Go silently degrades to normal; Rust runs the real protocol) |
| s5a2 | same with explicit `type: anthropic` | **identical** (request byte-identical) |
| s5b | `defer_mode: tool-search`, alias without `type:` | intended (F-01) |
| s5b2 | same with explicit `type: openresponses` | **identical** (both legs byte-identical) |
| s5c | `defer_mode: system-tools`, alias without `type:` | intended (F-01) |
| s5c2 | same with explicit `type: openai` | equivalent (frozen system-tools mount equal; D-07/D-08 key order) |
| s5d | `defer_mode: reference` on plain `openai` (mismatch) | intended (F-01: Rust's warning names the resolved type; Go prints a double-space blank) |
| s6 | colliding server names → `_2` segment | **identical** across 4 repeat runs (F-07/D-06 make Rust deterministic by construction) |
| s7 | `--mcp ""` | intended (F-02: Go panics, exit 2; Rust errors, exit 1) |

### 3.5 Tally

| Verdict | Count |
|---|---:|
| identical (byte-equal) | **23** |
| equivalent (declared, entry cited) | **19** |
| intended (approved fix / scoped behaviour) | **8** |
| rust-unsupported (interactive-only, D-23) | **3** |
| divergent-bug | **0** |
| **total** | **53** |

Every non-identical outcome cites its DIVERGENCES/POLICY entry; no scenario
produced an undeclared behavioural difference. Request bodies were also
compared: first-leg bodies are byte-identical in every MCP scenario, and the
only byte-level differences anywhere are Go's JSON HTML escaping (D-08) and
JSON key order inside replayed raw messages (D-07) — parse-identical.

---

## 4. Code quality & architecture assessment

### 4.1 Crate layout

Six workspace crates (`rust/`, edition 2024, toolchain pinned 1.98.0), exactly
as the binding ARCHITECTURE §1 prescribes, verified against all six
`Cargo.toml`s:

```
iota (bin+lib) ──► iota-chat ──► iota-tools ──► iota-core
      │               ▲                            ▲
      ├──► iota-llm ──┼────────────────────────────┤
      ├──► iota-mcp ──┴────────────────────────────┤
      └──► iota-tools ─────────────────────────────┘
```

- `iota-core`: contracts only (data model, traits, `RunCtx`, errors) — no
  HTTP, no process spawning; `iota-llm` / `iota-mcp` / `iota-tools` depend
  only on it and build in parallel; `iota-chat` never sees `reqwest` or
  `rmcp`, so the loop tests build without the heavy dependency graph.
- Heavy dependencies are isolated where the design says: `reqwest` in
  iota-llm/iota-mcp/iota only, `rmcp` in iota-mcp only, `serde_norway` in
  iota-tools/iota only. A direct-dependency allowlist
  (`scripts/direct-deps.allow` + `check-deps.sh`, 33 crates) gates additions.
- Line counts (implementation vs test): 16,390 implementation lines,
  18,005 test lines — a **1.10 : 1 test-to-code ratio**.

| crate | src impl | test lines (unit + `tests/`) |
|---|---:|---:|
| iota-core | 2,305 | 1,536 |
| iota-llm | 6,412 | 5,587 |
| iota-tools | 3,968 | 4,649 |
| iota-mcp | 876 | 1,127 |
| iota-chat | 1,011 | 2,330 |
| iota (lib+bin) | 1,818 | 2,776 |
| **total** | **16,390** | **18,005** |

### 4.2 Trait design

Conforms to ARCHITECTURE §4 and the frozen contracts (verified by full source
read — finding `idiom-12`):

- Five object-safe traits (`Provider`, `ToolProvider`, `Tool`, `Dispatcher`,
  `Delegator`) use one consistent hand-boxed `BoxFuture` convention — no
  `async_trait` proc-macro, no mixed styles.
- **Results, not getters**: Go's `LastUsage()/LastRawContent()/LastImages()`
  ordering hazard is designed away — every per-call product is returned in
  `ChatResult` / `RoundResult`, so provider calls take `&self` and providers
  hold only construction-time state.
- Optional capabilities are default trait methods returning `Option`
  (three-way `owns() -> Option<bool>` included) — no `Any` downcasting
  anywhere.
- `ReasoningGate` enforces the close-before-first-content streaming contract
  by construction (idempotent close, close on `Drop`); dialects hold the gate,
  never the raw sink.
- Determinism where Go was map-order random: `BTreeMap` / sorted merges
  throughout (MCP segments, delegate validation, JSON keys).

### 4.3 Error handling

- `thiserror` enums per crate with Display texts byte-copied from Go; a
  differential string audit of 167 fragments from the ten most user-facing Go
  files found 160 verbatim and 7 correctly absent (all on unreachable
  interactive paths).
- Workspace lints deny `clippy::unwrap_used`, `expect_used`, `panic` outside
  tests; `#![forbid(unsafe_code)]` in every crate and **zero `unsafe` in the
  tree** (`nix` + `CommandExt::process_group` cover the syscalls). `anyhow`
  appears only in the ~30-line `main.rs`.
- One disguised panic path survives: `default_http_client()` falls back to
  `reqwest::Client::new()`, which panics on the same TLS-init failure that
  made `build()` return `Err` (finding `idiom-9`).

### 4.4 Async and cancellation

- tokio multi-thread runtime; cancellation is an explicit
  `CancellationToken` threaded through `RunCtx` (never task-locals); every
  long await selects on it (retry sleeps, body reads, tool execution, MCP
  connect fan-out).
- SIGINT/SIGTERM cancel the root token, MCP servers are closed (bounded
  10 s), exit code 130, JSON mode reports `"error": "interrupted"` — pinned
  end-to-end by `cli_sigint_exits_130_with_interrupted_json` (I-03).
- Lock discipline holds as designed: the MCP `RwLock` and the `CodeSet` read
  ledger are never held across `.await`; contention is covered by dedicated
  multi-thread stress tests. One inefficiency under the manager's read lock
  was found (`idiom-7`, minor).

### 4.5 Tests

`cargo test --workspace --all-features`: **391 passing, 0 failed, 0 ignored**,
stable across three back-to-back runs (no flakes in the timing- or
contention-sensitive tests).

| crate | unit | integration | total | ported from Go |
|---|---:|---:|---:|---:|
| iota-core | 37 | 5 | 42 | 11 |
| iota-llm | 40 | 82 | 122 | 60 |
| iota-tools | 31 | 77 | 108 | 70 |
| iota-mcp | 9 | 9 | 18 | 11 |
| iota-chat | 7 | 33 | 40 | 22 |
| iota | 9 | 52 | 61 | 20 |
| **total** | **133** | **258** | **391** | **194** |

(Counts include the six tests added by the 2026-08-31 fix pass: +1 iota-core,
+3 iota-llm, +1 iota-tools, +1 iota; the ported-from-Go column is unchanged.)

Coverage of the Go suite: the Go tree contains 664 test functions, of which
~276 live in purely interactive rendering packages (`internal/ui`,
`internal/markdown`, `internal/mathtext`, `internal/imgterm`) and most of
`chat/` (190) is TUI/session code. **Every Go test the specs identified as
headless-reachable — 194 — is ported one-for-one**, keeping its Go name in
snake_case with a `// Go: <file>:<line>` anchor (`grep -rn "// Go: " crates`
reproduces the list). The unported balance is exactly the interactive-only
set recorded in DIVERGENCES §D (§7 below). The remaining 197 Rust tests are
new: behaviour Go left untested (retry-delay precedence, path cleaning,
Go-duration formatting, per-delegation dispatcher freshness, …) plus 16
`assert_cmd` end-to-end `cli_*` tests (17 after the fix pass) that drive the
built binary against `wiremock` with a cleared child environment, pinning cmd/root.go's error
order and main.go's exit codes.

Test hygiene: no test mutates the process environment (everything injected
via `HostDirs`/`EnvSource`), no test touches the real network, no
`#[ignore]` anywhere.

### 4.6 CI gates

*(As of the phase-1 evaluation; the reduced-feature legs, the leak gate and the
size matrix were retired on 2026-09-01 — the current gate list is §11.5.)*

`./ci.sh` exits 0 end-to-end, in order: `cargo fmt --all --check` ·
`check-deps.sh` (direct-dependency allowlist) · `check-stubs.sh` (16/16
landed, no `todo!()`) · `cargo clippy --workspace --all-targets
--all-features -- -D warnings` (with `clippy::pedantic`) ·
`RUSTDOCFLAGS="-D warnings" cargo doc` (every pub item documented) ·
`cargo test --workspace --all-features` · two reduced-feature release builds
(`openai,shell`; `openai,shell,tls-ring`) · the aws-lc-rs leak gate
(`cargo tree … | grep '^aws-lc-rs'` = 0 hits on the ring build) · ring-only
test runs for iota-llm and iota-mcp · default release build · size matrix
into `docs/SIZE.md`.

Gaps in the gates, found by review: the TLS-exclusivity invariant is only
checked for the two pinned combos, not arbitrary user builds (`idiom-5`);
iota-mcp's `rmcp` dependency bypasses the workspace version table
(`idiom-4`).

**Overall assessment: the implementation conforms closely to its binding
architecture; the workspace is idiomatic, deterministic, heavily tested, and
lint-clean at pedantic level. The confirmed findings (§5) are polish, not
structure.**

---

## 5. Confirmed review findings

Sixteen confirmed findings, **all minor severity**. They are reported here
verbatim in substance (condensed wording); **no code was changed for this
evaluation** — the `outcome`/`notes` columns record the separate fix pass that
followed it (addendum below). Files are repo-root-relative.

| id | severity | area | file | problem | proposed fix | outcome | notes |
|---|---|---|---|---|---|---|---|
| correctness-1 | minor | cli-config | rust/crates/iota/src/resolve.rs:110 | The `--output-format` parse error is raised earlier than Go: Rust parses it inside `resolve_run`, before provider construction, effort/top_p validation, `mcp_servers` and `tools.delegate` checks; Go parses it after all of those (root.go:249). With coexisting config errors (e.g. `effort: turbo` + `--output-format yaml`) the two binaries print different errors, and Go's tuning warnings are suppressed. lib.rs claims the order is kept "EXACTLY"; the change is not in DIVERGENCES.md. | Carry the raw flag value through `RunSettings` and parse it in `lib.rs::run` at Go's position (just before the `output_format_given && message.is_none()` check); or keep the early parse, record it in DIVERGENCES.md, and drop the "kept EXACTLY" claim. | **fixed** | `RunSettings` now carries `output_format_raw`; `lib.rs::run` calls `parse_output_format` at root.go:249's position, so the parse error regains Go's precedence (new e2e test `cli_output_format_parse_runs_after_tuning`). CONTRACTS §7.3 signature change logged. |
| security-3 | minor | resource limits (skill discovery) | rust/crates/iota-tools/src/agents/skills.rs:77 | `discover_skills` reads every candidate SKILL.md with an unbounded `std::fs::read` (then copies again in `split_frontmatter`); a multi-GB SKILL.md in an untrusted repo is loaded wholesale, and discovery re-runs on agent startup and on every `load_skill`. The same crate's AGENTS.md loader already caps before reading. Go parity (Go is also unbounded), but the mitigation pattern exists in-tree. | Read SKILL.md via `File::open().take(cap)` like `load_agents_chain`; a 64 KiB discovery cap is generous (frontmatter needs only the first few KB). `load_skill`'s body path already caps at 20 MB. | **fixed** | `discover_skills` reads through `File::open` + `take(SKILL_DISCOVERY_CAP)` (32 KiB); pinned by `test_discover_skills_caps_the_read`. Deliberate hardening divergence from Go's unbounded `os.ReadFile`. |
| security-4 | minor | header/key handling | rust/crates/iota-llm/src/wire/client.rs:74 | The hand-written `Debug` for `wire::client::Client` prints the `headers` map holding raw credentials (`x-api-key`, `Authorization: Bearer …`, `x-goog-api-key`); no value is marked `HeaderValue::set_sensitive(true)` (zero hits in the workspace), so any future debug/log line would render the literal key bytes. Latent today (no call site Debug-formats a Client; no tracing subscriber installed). | Call `set_sensitive(true)` on every credential header value before `with_header`, and/or render only header names in the Debug impl. | **fixed** | all six credential headers are built by `common::credential_header`, which calls `set_sensitive(true)`; `client_debug_never_prints_credentials` asserts `{:?}` renders `Sensitive`, never key bytes. |
| completeness-1 | minor | shell sandbox writable paths | rust/crates/iota-core/src/app.rs:62 | `cache_dir` treats a relative `$XDG_CACHE_HOME` as unset and falls back to `~/.cache`; Go's `os.UserCacheDir` errors on a relative value, so Go's Linux sandbox omits the cache dir from the writable set while Rust's bwrap sandbox grants `~/.cache`. Recorded only in the scratchpad DEVIATIONS.md ([WP01]), not in DIVERGENCES.md. | Add a DIVERGENCES.md row (§B or §C) for the relative-XDG_CACHE_HOME fallback and its effect on the Linux sandbox writable-path set. | **documented** | DIVERGENCES.md **D-36** (§C) — relative `$XDG_CACHE_HOME` falls back to `~/.cache`, adding one writable path to the Linux bwrap set. |
| completeness-2 | minor | config / agents / skills file decoding | rust/crates/iota/src/config/mod.rs:224 | Three user-visible reads decode file bytes with `from_utf8_lossy` (system_file content; the AGENTS.md chain; SKILL.md / load_skill) where Go's `string(data)` preserves invalid UTF-8 verbatim — a prompt with invalid UTF-8 reaches the provider with U+FFFD replacements. Recorded only in scratchpad DEVIATIONS.md ([WP14]/[WP11]). | Add one DIVERGENCES.md row covering the lossy UTF-8 decode across system_file, AGENTS.md chain parts, SKILL.md and load_skill reads. | **documented** | DIVERGENCES.md **D-37** — lossy UTF-8 decode of `system_file`, the AGENTS.md chain, SKILL.md and `load_skill` reads. |
| completeness-3 | minor | skills frontmatter parsing | rust/crates/iota-tools/src/agents/skills.rs:103 | A non-scalar frontmatter `name:` (e.g. `name: [a]`) reads as absent → `SkillError::MissingName`, where Go's yaml.v3 strict decode fails with a decode-error text. Same rejection, different error class/message; recorded only in scratchpad DEVIATIONS.md ([WP11]) — D-15 covers only the toolset-config decode text. | Fold the case into a DIVERGENCES.md row alongside D-15's different-YAML-library note. | **documented** | DIVERGENCES.md **D-38** — non-scalar frontmatter `name:`/`description:` reads as absent (`MissingName`); cross-referenced to the D-15 different-YAML-library class. |
| completeness-4 | minor | provider contract — mid-stream failures | rust/crates/iota-llm/src/openai.rs:343 | On a mid-stream failure the openai/google providers return only `Err(ProviderError::Stream)` and drop the partial content/reasoning Go returned beside its error; a usage-but-no-choices response similarly drops the usage Go recorded. Unobservable headlessly (chat.go:311 discards partials) but a Provider-contract semantic change recorded only in scratchpad DEVIATIONS.md ([WP03]/[WP06]). | Add a DIVERGENCES.md §C row noting partial content/usage beside a provider error is dropped (no headless consumer). | **documented** | DIVERGENCES.md **D-39** — partial content/reasoning/usage beside a provider error are dropped (no headless consumer; sink traffic identical). |
| idiom-3 | minor | DRY / error mapping | rust/crates/iota-llm/src/openai.rs:198 | Seven provider files carry six spellings of the same two-line LlmError→ProviderError mapping (Cancelled passes through, everything else wrapped): `wrap_err`, `map_err`, `stream_error`, three fixed-wrapper copies in google.rs, and a byte-identical duplicated pair in imagen.rs/images.rs. | Move the parameterised form into `common.rs` as `pub(crate) fn map_llm_err(e, wrap)` and delete the six copies; fixed-wrapper call sites become `map_llm_err(e, ProviderError::Chat)` etc. | **fixed** | one `common::map_llm_err(e, wrap)`; all six per-provider copies deleted, behaviour identical (`cancelled_is_never_wrapped` now pins the shared fn). |
| idiom-4 | minor | Cargo workspace hygiene | rust/crates/iota-mcp/Cargo.toml:30 | iota-mcp pins `rmcp = "3.1"` directly in both `[dependencies]` and `[dev-dependencies]` although the workspace table defines rmcp — three places for one version requirement that can silently drift (the whole MCP design is calibrated against rmcp 3.1.4; drift would invalidate the D-01/D-02/D-33 analysis). | `rmcp = { workspace = true }` in `[dependencies]`; `{ workspace = true, features = ["server", "transport-async-rw"] }` in `[dev-dependencies]`. | **fixed** | `rmcp = { workspace = true }` in `[dependencies]`, `{ workspace = true, features = ["server", "transport-async-rw"] }` in `[dev-dependencies]`; resolution verified byte-identical via `cargo metadata` + `cargo tree -e features`. |
| idiom-5 | minor | feature gating (TLS exclusivity) | rust/crates/iota-llm/Cargo.toml:24 | ARCHITECTURE §1.4/§11 declares the TLS backends an exclusive pair, but nothing enforces it: `cargo build -p iota --features tls-ring` (without `--no-default-features`) unifies both — reqwest/rustls re-adds aws-lc-rs while `install_tls_provider()` installs ring, so the binary carries both crypto providers. The CI grep only checks the two pinned combos. | Add `#[cfg(all(feature = "tls-aws-lc", feature = "tls-ring"))] compile_error!(…)` in iota-llm, iota-mcp and iota (the Cargo book's sanctioned use of `compile_error!`); or document loudly and extend the CI gate to a default+tls-ring build. | **fixed** | the finding's ALTERNATIVE, not its `compile_error!`: that guard is impossible while `ci.sh` runs `clippy/doc/test --workspace --all-features` (which unifies both TLS features and is indistinguishable from an accidental unification). New `build.rs` in iota, iota-llm and iota-mcp emits a non-fatal `cargo::warning`; runtime precedence (ring wins) documented on `install_tls_provider`/`tls.rs`; ARCHITECTURE §11 amended; the shipped ring combo stays hard-gated by ci.sh's `cargo tree \| grep '^aws-lc-rs'`. |
| idiom-7 | minor | efficiency / borrow discipline | rust/crates/iota-tools/src/defer_mode.rs:185 | `MarkedDispatcher::deferred_tools` re-queries `self.inner.tools()` inside the per-group loop: for G groups the full tool list is rebuilt G times per call, each rebuild cloning every ToolDef under the MCP manager's read lock. The live-view contract needs one snapshot per invocation (as sibling `DeferDispatcher::resolve` already does). | Snapshot `self.inner.tools()` once before the loop and filter per group (or reuse the resolve()-style bucketing). | **fixed** | `MarkedDispatcher::deferred_tools` snapshots `inner.tools()` once before the group loop; output order unchanged. `SearchingDispatcher` already complied. |
| idiom-8 | minor | API consistency across providers | rust/crates/iota-llm/src/openai.rs:50 | An API key that is not a legal header value is handled two ways under the same rationale: OpenAI silently omits the Authorization header; Anthropic/Google/OpenResponses/Images send an empty header value. Missing vs empty can produce different server responses (401 vs 400/proxy), so the same misconfiguration fails differently per provider; near-identical comments suggest the split is accidental. | Pick one convention (the empty-header majority: a guaranteed, attributable 401) and share it as a `common::bearer_or_empty`/`header_or_empty` helper in all five constructors. | **fixed** | all five constructors use `common::credential_header`; OpenAI no longer silently omits `Authorization` (empty-header convention, pinned by `illegal_key_sends_an_empty_sensitive_authorization_header`). |
| idiom-9 | minor | hidden panic path vs deny(panic) | rust/crates/iota-llm/src/wire/client.rs:358 | `default_http_client()` falls back from a failed `builder().build()` to `reqwest::Client::new()`, which is documented to panic on exactly that failure (TLS init) — converting a recoverable `Err` into a panic the workspace's `unwrap_used`/`expect_used`/`panic` denials cannot see because it lives inside reqwest. A disguised `.expect()` that dodges the lint. | Spell it honestly: `#[allow(clippy::expect_used)]` with `.expect("TLS backend failed to initialise")`, or return `Result` and surface the error at the binary edge like every other startup failure. | **fixed** | `default_http_client` is an explicit `#[allow(clippy::expect_used)]` + `.expect("TLS backend failed to initialize")`; signature unchanged. |
| idiom-10 | minor | cfg gating robustness | rust/crates/iota-llm/src/wire/mod.rs:24 | `is_zero` is gated on `any(openresponses, google)` but its third consumer, `wire/images.rs`, is gated on feature `images`; the build works only because `images = ["google"]` — the cfg silently depends on that transitive implication (the module doc even names images as a user). A future decoupling would produce a confusing cross-module compile error. | Name every consumer in the gate: `#[cfg(any(feature = "openresponses", feature = "google", feature = "images"))]` — redundant today, refactor-proof tomorrow. | **fixed** | gate widened to `any(openresponses, google, images)` — every consumer named. |
| idiom-11 | minor | path handling (lossy UTF-8 in the jail) | rust/crates/iota-core/src/paths.rs:50 | `paths::rel` round-trips both inputs through `to_string_lossy`, so non-UTF-8 components collapse to U+FFFD before comparison and two byte-distinct components can compare equal; `rel` feeds `within()` and the CodeSet project-root jail. Exploitability negligible (tool args arrive as JSON/UTF-8), but `clean` in the same module already does it losslessly over `Component`s. | Rewrite `rel()` over the cleaned paths' `Component` sequences (`&OsStr` byte comparison), keeping the Go-table semantics; keep `to_slash` lossy for display only. | **fixed** | `rel()` walks the cleaned paths' `Component`s as `OsStr` bytes; all 27 Go-table rows unchanged, new `#[cfg(unix)] paths_rel_non_utf8_components` pins it. `to_slash` stays lossy for display. |
| idiom-12 | minor | architecture conformance (assessment) | rust/docs/ARCHITECTURE.md:49 | Verified assessment, not a defect: the workspace conforms closely to the binding architecture — six crates as §1.1, dependency edges as §1.2, trait design as §4/G1/G30, clippy pedantic clean, full suite green. The only conformance gaps are the ones filed separately (the overstated "No I/O" claim for iota-core, unenforced TLS exclusivity `idiom-5`, rmcp bypassing the workspace table `idiom-4`). | No structural action; address idiom-4/idiom-5 and the iota-core doc claim to close the drift between code/docs and the stated architecture. | **no_change_needed** | assessment, not a defect: no structural action taken, and the assessment still holds after the fix pass (re-verified: clippy pedantic clean, full suite green). Two of its three named drifts are now closed (`idiom-4`, `idiom-5`); the third (`iota-core`'s "No I/O" claim, filed as `idiom-6`) was not among the sixteen confirmed findings and is untouched. |

**Addendum (2026-08-31, fix pass `fix-*` + `fix-verify`):** every finding above
was worked after this evaluation was written — 11 fixed in code, 4 recorded as
`DIVERGENCES.md` rows D-36…D-39, 1 (`idiom-12`) an assessment needing no action.
The fifth docs-debt row of §8.1 landed too (**D-40**, config-file YAML
parse-warning text). One conscious deviation from a finding's proposed fix:
`idiom-5` ships the finding's documented alternative (build-script warning +
runtime precedence + the existing `cargo tree` gate) instead of `compile_error!`,
which cannot coexist with CI's load-bearing `--all-features` invocations. One
frozen contract signature changed, as `correctness-1` requires: `RunSettings`
drops `output: OutputFormat` + `output_format_given` for `output_format_raw`
(CONTRACTS §7.3). Full `ci.sh` re-run green after the pass (391 tests, 0
failed); test count 385 → 391 (+6 regression tests, no ported test removed).

---

## 6. Intentional divergences (condensed)

Full detail: `docs/DIVERGENCES.md`. Summary:

**A. Approved bug fixes (F-01…F-09)** — Go bugs the port fixes by POLICY §3:
resolved-type `defer_mode` capability check (F-01); `--mcp ""` errors instead
of panicking (F-02); `-m ""` errors instead of falling into the TUI (F-03);
sparse tool-call indices no longer drop calls (F-04); `"error": null` treated
as absent (F-05); oversized `read_file` lines cut to fit instead of an
inescapable empty window (F-06); deterministic MCP server order (F-07) and
delegate-agent validation order (F-08); F-09 records a no-change parity case.

**B. Approved intentional divergences (I-01…I-08)**: YAML 1.1 bool spellings
accepted everywhere (I-01); uniform 2-minute response-header timeout on every
client (I-02); SIGINT/SIGTERM → clean cancel, MCP close, exit 130, JSON
`"error": "interrupted"` (I-03); runtime errors print `Error: <msg>` without
cobra's usage dump (I-04); MCP connect failures warn on stderr instead of
silence (I-05); delegated children never save images (I-06); Windows not a
phase-1 target (I-07); no chat-level retry, matching Go's headless path
(I-08).

**C. Design-level divergences (D-01…D-40 for phase 1; D-41…D-56 were added by
phase 2 slice 1 and are summarised in §9.3; T-01…T-40 by phase 3 and T-41…T-63
by T3 — see DIVERGENCES §C.2/§C.3 and §13 below)**, grouped:

- *MCP / rmcp semantics* (D-01…D-06, D-33): append-only custom headers with
  reserved-name rejection; initialize-only handshake (rmcp `Auto` mode would
  restore go-sdk's discover-first handshake with one call); rmcp's shutdown
  ladder (no SIGTERM stage); connect-timeout children killed immediately;
  `close()` leaves a safe manager (no panic); deterministic segment order;
  SEP-2322/2663 results become explicit error texts instead of empty results.
- *Serialization cosmetics* (D-07…D-10): sorted JSON keys, no HTML escaping,
  Go `%v`/`%q` approximations — parse-identical wire bytes.
- *Out-of-scope seams kept honest* (D-11…D-13, D-19…D-24, D-27, D-29, D-35).
  **Almost all of this group is now closed, and each row says so in place.**
  Session-only message fields were restored by phase 2 slice 1 (D-11);
  `--resume=<id>` with `-m` is supported and only the blank picker form is
  rejected headlessly (D-23); and T3 closed the rest on 2026-09-02/03 —
  interactive tool-display capabilities (D-12 by T-30), the `images` edit
  endpoints in BOTH forms (D-13 by T-41), the artifact side channel (D-19 by
  T-35), both stream observers (D-20 by T-11/T-51), overlay freshness (D-27 by
  T-37) and the incomplete-image-stream path (D-29). D-35 keeps only its
  headless half: the `ask` set contributes zero tools without an interactor,
  exactly as Go's `root.go:588-592` does, and the interactive build enables it.
- *Different engines, documented* (D-15…D-18, D-25, D-30, D-32, D-38, D-40):
  serde_norway vs yaml.v3 error texts — for the toolset decode (D-15), the
  config-file parse warning's embedded text (D-40) and a non-scalar skill
  frontmatter `name:`, which reads as absent instead of failing the decode
  (D-38); OS error texts; real git ignore semantics via the `ignore` crate;
  Unicode-aware `regex` vs RE2; jiff local time; reqwest redirect policy;
  globset vs doublestar (guaranteed-subset pinned by tests).
- *Embedded builds* (D-28): compiled-out toolsets/providers/MCP keep the
  config surface stable and warn instead of failing.
- *Rust-language and contract consequences* (D-36, D-37, D-39), added by the
  2026-08-31 fix pass to retire the `completeness-1…4` docs debt: a relative
  `$XDG_CACHE_HOME` falls back to `~/.cache` and so joins the Linux bwrap
  writable set; file bytes are decoded with `from_utf8_lossy` (`system_file`,
  the AGENTS.md chain, SKILL.md, `load_skill`) where Go's `string(data)` keeps
  invalid UTF-8 verbatim; partial content/reasoning/usage beside a provider
  error are dropped (no headless consumer).

---

## 7. Not ported / out of scope

> **Rewritten 2026-09-03, after T3 (§13).** Everything this section used to list as deferred has
> shipped. Two items remain, and one of them is not a gap at all.

- **OSC 52 clipboard** (DIVERGENCES T-21). Copying goes through the exec-based tools
  (`pbcopy` / `clip` / `wl-copy` / `xclip` / `xsel`) and says `no clipboard tool found` when none
  is installed — which is exactly what Go does. Go discussed OSC 52 and never shipped it, so
  porting it would LEAD the Go binary rather than follow it; it is deliberately out of T3 as well.
- **Windows** (I-07). The port targets macOS and Linux. Nothing Windows-specific is implemented
  or asserted.

Everything else that stood here is closed and the evidence is in place:

| what §7 used to list | where it is now |
|---|---|
| Interactive TUI (REPL, markdown, progress/status widgets, compaction) | phase 3, §10 |
| Math rendering, image display | T3: DIVERGENCES T-08/T-15 (mathtext), T-16/T-20/T-48 (imgterm, widget, `/edit` picker) |
| `/export`, `/debug` | T3: T-17 (+T-42/T-43/T-61), T-18 (+T-44/T-45/T-56) |
| `/skills` browser and expansion | T3: T-19 |
| Sessions, `--resume`, `--no-save`, the blank-`--resume` picker, `/save` | phase 2 slice 1 (§9) headlessly; T-29 interactively; D-54 |
| The `ask` toolset implementation | shipped with the interactive build (`src/tool/ask.rs`); D-35 records the headless half |
| Image edit endpoints for the `images` provider | T3: D-13 CLOSED — multipart (T-41) and JSON |
| Streaming observers (tool-call and image-partial, D-20) | T-11 (WP55) and T3 T-51; the only residue is the install/detach API SHAPE, an idiom |
| Interactive tool-display capabilities (D-12) and the artifact side channel (D-19) | T-30 and T-35, tests completed in T3 |
| Desktop notifications, terminal progress, the cmux host, upload progress | T3: T-13 / T-14 (+T-52/T-53/T-55) |

**Unported Go tests.** The authoritative list is DIVERGENCES §D, which T3 shrank to four entries —
the install/detach half of `provider/observer_test.go`, `TestImagenStreamChatAdapts`
(`provider/imagen_test.go:172`), `TestDetectBackgroundLatch`
(`internal/host/background_test.go:97`) and Windows-only expectations. Each carries a
justification there, and each is a Go IDIOM or a Go feature the port deliberately does not have,
never an untested behaviour. The audit that establishes it is `scratchpad/t3/audit.py`: it matches
every `func Test…` of the mirrored Go packages against the Rust tree's `// Go: file:line` anchors
and reports the residue. DIVERGENCES §C.2/§D and §D.1/§D.2 carry the per-file detail.

---

## 8. Recommendations and next steps

### 8.1 Close out the review findings (small, low-risk)

> **Done** — all five items below landed in the 2026-08-31 fix pass; the
> per-finding outcome is the `outcome` column of §5. Kept as written for
> the record of what the evaluation recommended.

1. **Docs first** (an afternoon): add the five missing DIVERGENCES.md rows —
   the four from `completeness-1..4` plus the config-file YAML parse-warning
   text noted by the cli-errors parity report — before the scratchpad
   DEVIATIONS.md is discarded.
2. **`correctness-1`**: either move the `--output-format` parse to Go's
   position or record the precedence change and drop the "kept EXACTLY"
   claim; this is the only finding that touches observable error ordering.
3. **Guard the invariants CI assumes**: the TLS `compile_error!` guard
   (`idiom-5`) and `rmcp = { workspace = true }` (`idiom-4`).
4. **Hygiene batch**: `set_sensitive(true)` on credential headers
   (`security-4`), the SKILL.md read cap (`security-3`), the shared
   `map_llm_err` / bearer-header helpers (`idiom-3`, `idiom-8`), the honest
   expect in `default_http_client` (`idiom-9`), the widened `is_zero` gate
   (`idiom-10`), the snapshot in `deferred_tools` (`idiom-7`), and the
   `Component`-based `rel()` (`idiom-11`).

### 8.2 Phase 2 candidates

- **Sessions**: re-enable `--resume`/`--no-save` on a headless session store
  first (the flags are already parsed and rejected at one place, D-23; the
  dropped `Message.interrupted`/`Message.usage` fields, D-11, are the only
  data-model additions needed). This also fixes the one Go behaviour the port
  cannot express today: replaying a previous conversation.
  > **Done** — shipped as phase 2 slice 1; see §9. The recommendation is kept
  > verbatim for the record, and it predicted the work correctly: the two
  > `Message` fields and the single rejection site were indeed the whole
  > data-model and CLI surface that had to move.
- **TUI on ratatui**: the seams are ready — `StreamSink` is the render
  boundary, observer hooks (D-20) and display capabilities (D-12) have trait
  defaults to fill in, and `iota-chat` is free of reqwest/rmcp so a TUI crate
  stacks on top without touching the loop. Budget for the markdown/mathtext
  rendering stack, which is most of the unported Go test surface.
- **Windows** (I-07): the port is `cfg(unix)`-gated in one crate
  (`iota-tools` shell/sandbox); a phase-2 Windows target needs a sandbox
  story (or an explicit unsandboxed mode), path/home handling, and signal →
  console-event mapping.
- **Size wins** *(superseded 2026-09-01 — §11: no feature pruning by decision)*: measured levers first — a no-MCP embedded build saves
  ~0.70 MiB; per-deployment provider/toolset pruning saves up to 2.8 MiB
  (default → `openai,shell`); ring TLS costs 0.63 MiB but avoids aws-lc-rs
  entirely. Next candidates: a `grep`-less `code` subfeature (regex is ~1 MiB
  of the estimate), evaluating `tls-ring` as the embedded default, and
  nightly `build-std` + `panic_immediate_abort` for the truly minimal
  profile. Install `cargo bloat` in CI so `SIZE.md` gains the per-crate
  attribution the script already supports.
- **MCP handshake parity**: if servers appear that require go-sdk's
  `server/discover` handshake, switch rmcp to `ClientLifecycleMode::Auto`
  (one call, D-02 records the analysis).

---

## 9. Phase 2 · slice 1 — headless session store

Delivered on top of the phase-1 port, in six file-disjoint work packages
(WP-S0 … WP-S5, table in "Package status"). The goal was **schema-level
interoperability with the Go binary's session bundles**, not a new feature:
either binary must be able to write a bundle the other resumes without losing
anything.

### 9.1 What landed

- **`crates/iota-session`** (a seventh workspace member: 1 633 lines under `src/`, 2 245 of tests):
  the whole of `chat/session.go` that has a headless meaning — `meta.json`
  (Go's struct order, 2-space indent, no trailing newline, unknown keys
  preserved), the append-only `messages.jsonl` with Go's exact `omitempty`
  matrix, the content-addressed `attachments/` store with dedup, 12-character
  Crockford-base32 ids, both bundle layouts (flat and `projects/<slug>/`),
  prefix resolution, the two listing views, the lazy writer, the loader with
  its compaction weave, the dialect raw-blob codec and the tuning replay. It
  is synchronous `std::fs` only: no async runtime, no HTTP client, and it
  never reads the process environment.
- **`iota-core`**: `Message.interrupted` and `Message.usage` restored in Go's
  field order (reverses D-11).
- **`iota-chat`**: history in, delta out. `RunRequest.history` seeds the loop
  (a non-empty imported history wins over `-s`, run.go:68-74), the turn's
  message delta comes back on `RunOutcome`/`OnceOutcome` (Go's
  `history[persisted:]`), every persisted assistant message carries its round's
  usage, and `save_images_for_turn` attaches exactly `collectImages`' saved
  subset. The crate still has **no** dependency on `iota-session`.
- **`iota`**: `--resume=<id|unique prefix>` on the `-m` path — resolve, load,
  replay the model and tuning, re-raise the deferred `ModelRequired`, print the
  banner, then persist the turn's delta on success only.
- **`rust/tests-go/gofix`**: a separate Go module (a `replace` directive on the
  read-only Go tree, not a cargo member) that generates the fixture corpus with
  the *real* Go writer, verifies a bundle through the real Go loader, and
  serves model replies. Five checked-in bundles plus a `manifest.json` whose
  `expect` block was measured through `chat.LoadSession`.

### 9.2 Tests

`cargo test --workspace --all-features`: **487 passing, 0 failed, 0 ignored**
(391 before the slice).

| crate | phase 1 | slice 1 | total | ported from Go (slice 1) |
|---|---:|---:|---:|---:|
| iota-core | 42 | +1 | 43 | 0 |
| iota-llm | 122 | 0 | 122 | 0 |
| iota-tools | 108 | 0 | 108 | 0 |
| iota-mcp | 18 | 0 | 18 | 0 |
| iota-session | — | +72 | 72 | 18 (as 22 tests) |
| iota-chat | 40 | +6 | 46 | 0 |
| iota | 61 | +17 | 78 | 0 |
| **total** | **391** | **+96** | **487** | **18** |

**Go tests ported**: 18 of the 21 functions in `chat/session_test.go` +
`chat/session_project_test.go`, as 22 Rust tests (Go's `TestApplySessionTuning`
has five subtests, each its own `#[test]`), each keeping its Go name in
snake_case with a `// Go: chat/<file>.go:<line>` anchor — `grep -rn "// Go: "
crates` now lists 219 anchors, 22 of them new. The three unported ones and the
reasons are in DIVERGENCES §D.1: `TestDeferredSaveBacklog` (no `--save` flag
this slice, D-54), `TestDeleteSessionInBucket` (no headless surface) and
`TestSessionLabelFlattensStoredTitle` (picker UI);
`TestLoadFullHistoryIgnoresCompaction` is adapted rather than ported, because
`LoadFullHistory` is `/export`-only.

**New tests**: 74 (50 in `iota-session` beyond the ported ones, 6 in
`iota-chat`, 17 in `iota`, 1 in `iota-core`) covering behaviour Go leaves
untested or that only the port can get wrong — unknown-meta-key survival, the
`meta.json` byte shape, the full jsonl `omitempty` golden matrix,
blank/corrupt/truncated-tail tolerance, the 32 MiB cap, attachment dedup and
missing-blob tolerance, the cross-dialect blob rules, `conv_count` vs
`message_count`, the CLI rejection matrix, and the resume end-to-end legs.

**Cross-binary acceptance, in two tiers** (design §8.2):

- *Tier A, hermetic, inside `cargo test`* — `crates/iota/tests/session.rs`
  resumes a **real Go-written bundle** (`--resume=08`, no `-M`) against
  `wiremock` and asserts: stderr is exactly
  `Resumed session <id> (6 messages)`, the model came from `meta.json`, the
  request carries the rehydrated attachment, the stored gemini blob's thought
  signature and the tool result, `generationConfig` reflects the persisted
  temperature and effort, `messages.jsonl` still starts with the Go bytes
  byte-for-byte and gains exactly the two expected lines, `meta.json` kept its
  unknown `future_key`, bumped `message_count` by 2 and left no `meta.json.tmp`.
- *Tier B, with a Go toolchain* — `scripts/go-session-roundtrip.sh`: Rust
  extends a Go-written bundle, then the Go binary re-reads it through
  `chat.LoadSession` + `chat.ResumeSession` (`view=8 raw_restored=1
  usage(in=1022 out=209 total=1216) meta_keys=14`), and finally Go loads a
  bundle Rust created from scratch. It runs in `ci.sh` and prints
  `SKIP: go not installed` (exit 0) where there is no Go toolchain.

Test hygiene is unchanged: no test reads or mutates the process environment,
none touches the network, none hardcodes a fixture session id (every id,
prefix, count and expected Go error string comes from `manifest.json`).

### 9.3 Divergences added

DIVERGENCES.md gains **17 rows, D-41 … D-56** (D-51 split into D-51a/D-51b),
and amends two: D-11 (the two `Message` fields are restored) and D-23
(`--resume` split — the valued form is supported, the blank picker form keeps a
rejection with its own text). In one line each:

- *New CLI behaviour*: headless `--resume=<id>` with `-m` (D-41, with bare `-m`
  still stateless — pinned by a test that asserts nothing appears under `HOME`);
  the blank form's new message (D-42); `ModelRequired` deferred and re-raised
  after the model replay (D-52); the banner on stderr with one newline (D-44);
  persist on success only (D-43); `--no-save` unchanged (D-54).
- *Format fidelity*: `meta.json` written temp+rename instead of in place (D-45,
  verified invisible to Go's locator, lister and loader); unknown meta keys
  preserved rather than dropped (D-46); `arguments` always `{}` (D-49); an
  empty anthropic/openresponses block list omits `raw` (D-50); the blob is
  carried verbatim and never parsed (D-51a); `+00:00` where Go prints `Z`
  (D-51b).
- *Rust-language consequences*: an unknown record `role` is skipped rather than
  carried verbatim (D-47, `Role` is a closed enum); an unparseable
  `meta.effort` warns and keeps the current setting (D-48, `Effort` is a closed
  enum); the 32 MiB log-line cap keeps Go's error frame with a different inner
  text (D-56).
- *Session-aware run behaviour*: generated images land in the bundle and only
  the successfully-saved ones are attached (D-53, with the honest note that
  mid-round images are dropped — the phase-1 loop surfaces only the terminating
  round's); usage rides every persisted assistant message (D-55).

Two notes travel beside the table rather than as rows: the **known Go-parity
hazard** (a defer-mode system-tools mount persists as a bare `{"role":"system"}`
record and displaces the real system prompt on the next resume — both binaries
do this, and the slice deliberately did not paper over it), and the **bundle
hardening beyond Go** (a traversal-shaped attachment `data_ref` is skipped, the
line cap fires before the allocation, directory reads are name-sorted with a
stable sort).

### 9.4 Still out of scope

- **`--save` / `--session-new`**: a headless run still cannot *create* a
  session from the CLI. `SessionStore::create` exists, is tested, and the Go
  binary loads what it writes (`examples/mkbundle.rs` + the round-trip script's
  step 5), but a create flag would be invented CLI surface with no Go behaviour
  to be interoperable with (D-54).
- **The interactive session picker** (bare `--resume`), `sessionLabel`,
  `humanizeTime`, `SessionInfo.Project`, `DeleteSession`, `LoadFullHistory`,
  the `/save` backlog and `/compact` — all TUI surfaces, and all of them
  **shipped since**: `/compact` and the meter in WP53 (T-10), the picker,
  `session_label`, `humanize_time` and `DeleteSession` in the TUI slice (§10),
  the `/save` backlog interactively under T-29 (D-54) and `LoadFullHistory` in
  T3 as `/export`'s source (T-17). What stays headless-only is the *absence of
  a create flag*: a headless run still cannot mint a session from the CLI. The
  store implements `append_compaction` and the loader's weave because a
  Go-written bundle can contain markers, but nothing headless produces one.
- **Interrupt-time persistence**: Go keeps a partial assistant message with
  `interrupted: true`; headless has no `finalizeInterrupt` seam, so a cancelled
  run persists nothing (D-43). The field is still read and written, so bundles
  Go wrote round-trip losslessly.
- **Mid-round generated images** (D-53) — a consequence of the frozen phase-1
  loop shape, recorded rather than silently accepted.
- Everything in §7 that phase 1 left out has since landed except OSC 52 and
  Windows; §7 was rewritten on 2026-09-03 and is the current record.

### 9.5 Gates

`./ci.sh` exits 0 end to end with the slice in. It gained: the two session
layering greps (`iota-chat` must not depend on `iota-session`;
`crates/iota-session/src` must not read the process environment),
`scripts/go-session-roundtrip.sh`, and a reduced-feature
`cargo clippy -p iota --no-default-features --features "openai,shell"
--all-targets -- -D warnings` *(retired 2026-09-01, §11)*. `check-deps.sh` reports 7 workspace members
and 34 allowed crates — the one new allowlist entry, `iota-session`, is
justified in ARCHITECTURE §11, and the slice added **no new third-party
dependency**.

The integration pass closed the one gap the slice handed over: `cargo clippy
-p iota --no-default-features --features "openai,shell" --all-targets -- -D
warnings` used to fail on three items in `crates/iota/tests/delegate.rs`
(`build_child_tools`, `iota_core::Delegator` and the `has` helper) that only
exist under `agent`/`delegate`+`code`. They now live behind the same feature
gate as their only callers, and **`ci.sh` runs that clippy invocation** right
before the matching release build — the workspace lint runs `--all-features`,
so a test file that only compiles with a feature on cannot be caught there.

### 9.6 Footprint impact

The session store is compiled into every build (it has no feature gate — the
`iota` crate depends on it unconditionally), so §2.1's phase-1 numbers moved.
Re-measured by the same `scripts/size-matrix.sh` run that `ci.sh` writes into
`docs/SIZE.md`, on the same host and toolchain:

| feature set | phase 1 | + slice 1 | delta | vs Go |
|---|---:|---:|---:|---:|
| default | 4,578,016 | 4,677,856 | +97.5 KiB (+2.2%) | 5.31× smaller |
| `openai,shell` | 1,740,304 | 1,823,632 | +81.4 KiB (+4.8%) | 13.61× smaller |
| `openai,shell,tls-ring` | 2,373,536 | 2,473,424 | +97.5 KiB (+4.2%) | 10.04× smaller |
| `all-providers,all-tools` (no MCP) | 3,860,368 | 3,943,888 | +81.6 KiB (+2.2%) | 6.30× smaller |

Under 100 KiB in every configuration, and no new third-party crate: `sha2`,
`rand` and `jiff` were already linked. Making the store a Cargo feature was
considered and not done — it would gate a *format*, and a build that cannot
read a bundle another build wrote is exactly the interoperability failure this
slice exists to prevent. §2.1, §1 and §2's ratios are the phase-1 measurement
and are left as the record of that date; the table above supersedes them.

---

## 10. Phase 3 · TUI slice — the interactive port

Sixteen packages (WP40–WP55; §"Package status" carries the table) turned the headless
`-m`/`-l` binary into the interactive one: three new crates — `iota-markdown` (the
streaming renderer), `iota-tui` (the ONLY ratatui/crossterm importer) and `iota-repl` (the
run loop, commands and turn engine) — plus the interactive branch in `iota` and the ask/
artifact/UI seams in `iota-core`. *(Written when the slice landed behind cargo features;
those features were removed on 2026-09-01 — §11. The numbers and layout below are the
record of that date.)* This section is the integration pass's record; the
per-package detail is in `scratchpad/tui/TUI_IMPLEMENTATION_REPORT.md` and the behavioural
divergences are §C.2 of `DIVERGENCES.md`.

### 10.1 What landed

| crate | src | tests | what it owns |
|---|---:|---:|---|
| `iota-markdown` | 3,012 | 1,913 | the width ruler, ANSI utilities, the line/spacing machine, inline + block renderers, the math guard set, the syntect highlighter (`highlight`) |
| `iota-tui` | 6,120 | 7,286 | the staging region, the inline `Terminal` and its five warts, the frame builder, the event loop, the `Ui` facade handle, composer/queue/suggest, the surface engine and every panel kind |
| `iota-repl` | 10,278 | 7,768 | the transcript layer, `describe_error`, the diff renderer, the turn engine (retry, tool loop, approval gate, artifacts), the run loop, the command table, sessions, the title state machine, tokens + compaction (`tokens`) |
| `iota` (branch) | — | — | the interactive branch, the MCP display seam, the session picker, the title stack |

The slice landed behind three Cargo features — `tui` (default), `tui-portable` (the wart-W9
fallback with ratatui's `scrolling-regions` off) and the T2 additions `tokens` /
`highlight` — all of which were removed on 2026-09-01 (§11): everything is now always in.

### 10.2 Tests

1,092 tests pass under `cargo test --workspace --all-features`, up from 761 when the slice
resumed. They are laid out as a pyramid, and no layer duplicates another:

| layer | what it proves | where | count |
|---|---|---|---:|
| L1 | pure units — the renderer corpus, the region, wrap math, the surface-key ladder, the transcript recorder, retry, the title machine, token arithmetic | unit tests beside their source + `#[path]`-mounted integration binaries | 949 |
| L2 | frame composition — geometry only, through ratatui's `TestBackend` | `iota-tui/tests/{frame_goldens,composer}.rs` | 33 |
| L2b | terminal semantics — the REAL writer stack's bytes, parsed by `vt100` (then on both feature legs; one leg since 2026-09-01) | `iota-tui/tests/vt100_semantics.rs` | 14 |
| L3 | facade contracts — the orderings Go could not unit-test, driven through `ScriptedUi` and the PUBLIC `iota_repl::run` | `iota-tui/tests/facade.rs`, `iota-repl/tests/{commands,settings,file,tokens,compact,toolloop}.rs` | 86 |
| L4 | real terminal — cursor, scrollback, bracketed paste, SIGWINCH | `iota-tui/tests/tmux.rs` + 10 scenario scripts, env-gated `IOTA_TMUX=1` | 10 scenarios / 184 assertions |

(L2–L4 count tests declared in those files; L1 is the executed remainder. A handful of unit
tests run twice, in their own crate and again in a binary that `#[path]`-mounts their
module — the convention that lets an integration test reach a crate-private seam.)

L4 ran five consecutive times green on this host (tmux 3.7c) — twice inside `ci.sh` and
three times standalone — with 0 FAIL and 0 WARTS each time.

What automation cannot reach is written down rather than waved at:
`docs/TUI-VERIFY.md` is a REQUIRED manual gate covering IME composition, the emulator's
partial-region scrollback policy (then a build choice; since 2026-09-01 a defect to fix,
there being no fallback build), flicker, resize reflow and the window-title stack, with a per-terminal
sign-off table for seven emulators.

### 10.3 CI gates added

*(As landed; the reduced-feature and `tui-portable` legs and the `cargo tree` gates were
retired on 2026-09-01 — §11.5.)*

`./ci.sh` exits 0 end to end. Beyond the phase-1/2 legs it now runs: clippy and a build for
the `tui` leg; a build for the `tui-portable` leg; the L2b suite under
`--no-default-features` (so the W9 fallback's byte shape is EXECUTED, not merely compiled);
the single `IOTA_TMUX=1` L4 execution against a freshly built shipping binary; and two
isolation greps — `cargo tree` for `--no-default-features --features "openai,shell"` must
contain no `ratatui`/`crossterm`/`unicode-width`/`unicode-segmentation`, and no crate but
`iota-tui` may name `ratatui` or `crossterm` in its manifest. Both hold.

### 10.4 The headless story is unchanged

*(The TUI-free build measured here no longer exists — §11; the `-m`/`-l` behaviour is
unchanged and its tests still run in the one binary.)*

- `cargo tree -p iota --no-default-features --features "openai,shell"` resolves 198 crates
  and contains no `ratatui`, `crossterm`, `unicode-width`, `unicode-segmentation`,
  `iota-tui`, `iota-repl`, `iota-markdown`, `syntect`, `two-face` or `tiktoken-rs`.
- `cargo test -p iota --no-default-features --features "all-providers,all-tools,mcp,tls-aws-lc"`
  — the TUI-free build — passes 84/84, `tests/cli.rs` and `tests/session.rs` included.
- A bare `-m` run of the shipping release binary with `HOME` pointed at an empty temp
  directory prints the reply, exits 0 and writes **nothing** under that HOME; the same
  binary invoked with no `-m` and piped stdout refuses with Go's exact text
  (`interactive mode requires a terminal; use -m/--message for piped input`, exit 1) and
  also writes nothing.
- The pre-TUI test count did not shrink: 761 → 1,092.

### 10.5 Footprint

*(Record of the feature matrix as it stood; only the first row survives as the one binary —
§11.4.)* Re-measured by the then `scripts/size-matrix.sh` run `ci.sh` wrote into `docs/SIZE.md`
(Darwin/arm64, rustc 1.98.0, release profile `opt-level=z, lto=fat, codegen-units=1,
panic=abort, strip=true`):

| feature set | bytes | size |
|---|---:|---:|
| default (`tui` + `tokens` + `highlight`) | 10,034,608 | 9.6 MiB |
| default − tui (TUI-free) | 4,694,736 | 4.5 MiB |
| default − tokens − highlight (`tui` only) | 5,095,072 | 4.9 MiB |
| default, `tui-portable` (wart W9) | 5,078,560 | 4.8 MiB |
| `openai,shell` | 1,857,040 | 1.8 MiB |

The interactive port itself costs **+400 KiB** over the TUI-free build (`tui-portable`
+384 KiB — the pair is only comparable with the T2 features off, since both imply `tui`).
The whole default set costs +5.1 MiB, of which `tokens` is +3.6 MiB (tiktoken-rs' bundled
`o200k_base` rank table, which is what makes counting offline) and `highlight` +1.1 MiB
(two-face's bat syntax set over syntect's own defaults, worth ~0.55 MiB of it). Both are
one `--no-default-features` line away for a size-sensitive build.

### 10.6 Defects found and fixed by the integration pass

Two behaviours that the package pass had characterised but not fixed, and eleven `NEEDS`
rows left by packages that could not edit another package's files, were closed here:

- **Wide runes on the `tui-portable` build.** Every double-width grapheme in a padded
  user-echo row inserted as the rune plus a spurious space (`❯ 中 文 一 行`) — deterministic,
  and `tui-portable` is what SHIPS on terminals that discard region-scrolled lines. Cause:
  `Terminal::insert_before`'s no-scrolling-regions path hands the WHOLE buffer to the
  backend, continuation cells included, and a covered cell reads back as a space.
  `LoopBackend::draw` now drops them, the same rule ratatui's own buffer diff applies.
  Pinned on both legs by an L2b test that `ci.sh` runs.
- **T-40, the frame's floor.** Below ~12 terminal rows the inline viewport overlapped the
  rows `insert_before` was scrolling out and the scrollback kept the damage. The staging
  window's cap is now dynamic — trimmed on a short terminal so the frame always leaves
  `max(2, screen_h/2)` rows above it, re-applied immediately on a height change. L4's
  short-window pin now passes where it used to report a WART.
- `/status` gained its token block (Context / Token count / Last turn / Session
  input+output+cache) behind the same capability gate `/compact` reads, closing the last
  T-10 gap left when `tokens` joined the default set.
- `/tools` and `/status` now take the MCP view as DATA (`crate::mcp::ServerStatus`) instead
  of pre-rendered lines, which deleted a duplicate of `tool_status_lines` and five SGR
  builders from the binary and brought three Go `toolstatus_test.go` ports with it.
- `/save` stamps the LIVE model and temperature onto the freshly minted meta, as Go's
  factory did by reading the provider — a mid-chat `/model` change now reaches the bundle.
- The interactive branch's six Go error texts became real `CliError` variants, and the ask
  toolset moved into `assemble::build_dispatcher` where Go has it.

---

## 11. 2026-09-01 — one-binary simplification

The user's decision: *简化架构，我现在不需要多个 target（比如 headless），跟 go 版本完全对齐，
直接 build 出一个统一的二进制即可.* It reverses the phase-1 footprint design (§2.1, §8.2,
ARCHITECTURE §11): like the Go binary there is exactly ONE shipped artifact,
`cargo build --release -p iota`, and it carries everything.

### 11.1 What was removed

- **Every cargo feature that changed the binary.** The workspace table's
  `default-features = false` internal deps; `iota-core/ui`; `iota-llm`'s five dialect
  features and `tls-aws-lc`/`tls-ring`; `iota-markdown/highlight`; `iota-mcp`'s
  `stdio`/`http`/`tls-aws-lc`/`tls-ring`; `iota-repl/tokens`; `iota-tools`'
  `shell`/`code`/`agent`/`delegate`; `iota-tui/scrolling-regions`; and all nineteen of
  `iota`'s. Every `optional = true` dependency became a plain one. The only `[features]`
  table left in the workspace is `iota-core`'s `testing` (test fakes for other crates'
  `[dev-dependencies]`), unchanged.
- **All 169 `#[cfg(feature = …)]` / `cfg(not(…))` / `cfg(any(…))` / `cfg_attr` sites** —
  the enabled branch became unconditional, the compiled-out branch was deleted. One site
  remains: `iota-core/src/lib.rs`'s `#[cfg(feature = "testing")]`.
- **The compiled-out runtime machinery**: the stub set factories and
  `SetError::NotCompiled` (`toolset "<n>": not compiled into this build (ignored)`), the
  `provider type <kind> is not compiled into this build` arms of `new_provider`,
  `Warning: mcp support is not compiled into this build` (`warn_mcp_not_compiled`), the
  `<kind> transport is not compiled into this build` arms of `connect_one`, and
  `CliError::InteractiveUnavailable` with its branch in `run()`. Zero such strings remain.
- **TLS choice**: the `tls-ring` backend, the direct `rustls` dependency (it only ever
  installed ring), the three `build.rs` soft guards (`iota`, `iota-llm`, `iota-mcp`),
  `crates/iota/src/tls.rs` / `install_provider`, `install_tls_provider` in `iota-llm` and
  `iota-mcp`, and every test harness `init_tls()` helper (their call sites simply went).
  The one backend is rustls with aws-lc-rs: `reqwest/rustls` in the workspace table, rmcp's
  `reqwest` in `iota-mcp`.
- **`tui-portable` and the no-scrolling-regions fallback**: ratatui's `scrolling-regions`
  is on in the workspace manifest, always. `meter.rs` (the inert T1 token shell) is gone and
  `meter_live.rs` is now `meter.rs`; `/compact` is a plain `commands::compact` module.
- **CI**: the reduced-feature clippy/build/test legs, the `tls-ring` legs, the aws-lc-rs
  `cargo tree` gate, the TUI-free `cargo tree` gate, the `tui-portable` build and L2b leg,
  and the size matrix (`scripts/size-matrix.sh` → `scripts/size.sh`, one row plus the Go
  comparison; the workflow artifact is `size-<os>`). `scripts/direct-deps.allow` drops
  `rustls`.
- **Docs**: DIVERGENCES D-22 and D-28 deleted (they described builds that no longer exist;
  the numbering keeps its gaps); the README feature matrix and embedded-build instructions
  removed; ARCHITECTURE §1.1/§1.3/§1.4/§7/§10/§11/§13 and TUI-VERIFY §2/§3 rewritten for
  the one-binary model.

### 11.2 The entry model is Go's exactly

`cmd/root.go`: `-l` → the listing; `-m` → one headless turn; otherwise → the interactive
chat, decided at root.go:259 after every earlier check; a non-TTY stdout is then refused
with the byte-exact `interactive mode requires a terminal; use -m/--message for piped
input`. `reject_unsupported` (D-23/D-42/D-54) is asked for `-m` runs only.
`assemble::build_dispatcher` keeps its `Option<(Arc<dyn Dispatcher>, PrefixOf)>` MCP
parameter: `None` still means "no server configured", not "feature off".

### 11.3 Tests

`cargo test --workspace`: **1,101 passed, 0 failed, 0 ignored** (before: 1,092 under
`--all-features`). Deleted, because they pinned only compiled-out behaviour:

| test | reason |
|---|---|
| `iota-tui/tests/vt100_semantics.rs::fallback_insert_lossless_without_scroll_regions` | pinned the `tui-portable` byte shape ("no DECSTBM at all"); never ran in the default configuration |
| the `cfg(not(scrolling-regions))` half of `over_screen_height_insert_characterization` (whole-screen scrollback reach) | same build; the scrolling-regions half stays, unconditionally |
| `iota-tools/tests/framework.rs::compiled_out_set_warns_not_compiled` | pinned `SetError::NotCompiled` and the compiled-out warning; its real half ("every `SET_NAMES` entry has a factory") survives as `every_built_in_set_has_a_factory` |

Renamed, not deleted: `cli_interactive_unavailable` → `cli_interactive_refuses_a_piped_stdout`,
`wide_runes_insert_intact_on_both_legs` → `wide_runes_insert_intact`. The count rose by nine
because `iota-repl/tests/toolloop.rs` path-mounts `../src/meter.rs`, which is now the live
meter and brings `../src/tokens.rs` with it — their unit tests run a second time in that
binary, the convention the file already documents. No other test was lost.

### 11.4 Size, like for like

Darwin/arm64, rustc 1.98.0, release profile `opt-level=z, lto=fat, codegen-units=1,
panic=abort, strip=true`; Go: `go build -trimpath -ldflags="-s -w"`, go1.27.0, rebuilt to a
temp path to confirm.

| binary | bytes | MiB |
|---|---:|---:|
| `iota` (Rust, the one binary) | 10,034,608 | 9.57 |
| `iota` (Go) | 24,827,602 | 23.68 |

**2.47× smaller.** The Rust figure is identical to §10.5's `default` row, which is the
point: the default set already carried everything, and the pruned variants are what went.
Breakdown (measured just before the legs were removed): the TUI itself ≈ 0.4 MiB; the heavy
parts are two embedded tables the Go binary carries as well — tiktoken-rs' `o200k_base`
ranks ≈ 3.6 MiB and syntect + two-face's syntax/theme dumps ≈ 1.1 MiB.

### 11.5 Gates

`./ci.sh` exits 0 end to end on this host (Darwin/arm64, rustc 1.98.0, tmux 3.7c, go1.27.0),
in order: `cargo fmt --all --check` · `check-deps.sh` (10 workspace members, 44 allowed
crates) · `check-stubs.sh` (32/32 landed) · `cargo clippy --workspace --all-targets -- -D
warnings` (pedantic) · `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` ·
`cargo test --workspace` · the two session layering greps · `scripts/go-session-roundtrip.sh`
(OK) · the TUI isolation grep · the single `IOTA_TMUX=1` L4 execution (10/10 scenarios, 184
assertions, 0 FAIL, 0 WARTS) · `cargo build --release -p iota` · `scripts/size.sh` →
`docs/SIZE.md` (one row, plus the Go binary rebuilt for comparison). Standalone,
`IOTA_TMUX=1 cargo test -p iota-tui --test tmux` ran three consecutive times green
(184/184, 0 WARTS each). Verification greps: exactly one `cfg(feature = …)` site in the
sources (`iota-core/src/lib.rs`, `testing`); zero occurrences of `not compiled`,
`compiled into this build`, `InteractiveUnavailable`, `warn_mcp_not_compiled`,
`install_tls_provider`, `init_tls`, `tls-ring` or `tui-portable` in any `.rs`, `.toml` or
`.sh` under `crates/`, `scripts/` or the workspace root.

---

## 12. 2026-09-02 — one package (the crate merge)

> **Addendum (2026-09-02).** The ten crates of `rust/crates/` were merged into ONE package, `iota`
> (lib + thin bin), mirroring the Go module's layout — `docs/MERGE-PLAN.md` (executed verbatim as
> approved) and ARCHITECTURE §1. A pure re-homing: no behaviour, string or test assertion changed;
> every `use iota_xxx::…` became a `crate::…` path, visibility defaults to `pub(crate)` with a curated
> `pub` surface, the crate-boundary hacks are gone (`impl_tunable_via_core!` → one blanket impl;
> `PrefixOf`/`ServerConfig`/`EnvSource` re-homed; zero `#[path]` test mounts), and the tests are
> regrouped into nine integration binaries under `tests/<area>/` (the former `iota-tui` files, all
> `#[path]` mounts of crate-private engine internals, became in-file unit tests). Counts: **1,040
> distinct tests pass** (490 unit + 550 integration, incl. the 10 tmux scenarios) — the same 1,041 test
> functions as before (one is platform-gated), verified name-for-name; the earlier headline of 1,101/1,111
> counted 60 unit tests that the `#[path]` mounts ran a second time plus the tmux leg twice. The tmux
> leg passed three consecutive standalone runs. `./ci.sh` exits 0 with the single-package legs
> (`cargo clippy --all-targets`, `cargo doc --no-deps`, `cargo test`, the two layering greps of
> ARCHITECTURE §1.2, `cargo build --release`). The §11.4 size row is unchanged in kind — fat LTO already
> merged everything — and re-measured by the same run (`docs/SIZE.md`): **9,951,152 B (9.49 MiB)**,
> 83,456 B (0.83 %) below the 2026-09-01 figure, against the same rebuilt Go binary (24,827,602 B; 2.49×). The tables above keep
> their crate names as the historical owners.
