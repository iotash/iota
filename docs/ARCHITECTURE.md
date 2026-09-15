# iota-rs — BINDING architecture (headless `-m` / the listings)

Status: **binding**. Synthesised from the winning "idiomatic" proposal with every judged graft adopted or explicitly rejected (§0). `POLICY.md` wins over this document; this document wins over the specs' mapping notes; `CONTRACTS.md` is the frozen API implementers code against; `WORK_PACKAGES.md` is the fan-out plan; `DIVERGENCES.md` and `TEST_PLAN.md` complete the set.

> **Superseded in part (2026-09-10): the `delegate` toolset is retired.** Every row below that names
> `tool/delegate.rs`, `chat/delegator.rs`, `cmd/delegate.rs`, `Delegator`/`AgentInfo`/`DelegateSpec`/
> `DelegateResult`/`DelegateOutcome`/`DelegateApprover`, `Env.delegate`, `DelegationLedger` or the
> "delegate cycle" describes code that no longer exists — a child agent is a bash subprocess now
> (`DIVERGENCES.md` §C.4 X-01, brain page `subagents-via-bash`). The rows are left as the phase-1
> record; nothing else on this page changed.

**Phase 2 · slice 1 (headless session store) is folded in.** The session store (now `src/session/`) and the resume stage in `cmd::run_agent` are described in place — §1.1/§1.2 (the crate and the graph), §2 (its module map), §8.1 (the session data flow), §10 and §11. Everything else on this page is the phase-1 architecture, unchanged.

Every user-visible string below is copied from the Go source (file:line given in CONTRACTS.md). Implementers copy, never paraphrase.

---

## 0. Graft ledger (winner = idiomatic proposal)

| # | graft | decision | where |
|---|---|---|---|
| G1 | `Provider::chat`/`list_models`/`ToolProvider::stream_chat_with_tools` take `&self`; `Provider: Send + Sync`; `as_tool_provider(&self)` | **adopted** — every per-call product is returned in `ChatResult`/`RoundResult`, so there is no per-call state; deletes the borrow-juggling risk | CONTRACTS §2.4 |
| G2 | dispatcher passed as `Arc<dyn Dispatcher>` to `once`/`run_once`/`execute_with_tools` (the `ToolSearcher` closure is `'static`) | **adopted** | CONTRACTS §6.2 |
| G3 | `Delegator::run -> DelegateOutcome { result, error }`; ledger `add` unconditional | **adopted** | CONTRACTS §2.7 |
| G4 | `ToolEnv { project_root: Option<PathBuf>, .. }`, `ToolEnv::root() -> io::Result<PathBuf>` | **adopted** (+ `dirs: HostDirs` field, see G5) | CONTRACTS §2.6 |
| G5 | inject the environment (home/cwd/env vars) instead of reading it: `Config::load(.., &Env, ..)`, `skill_roots(root, home)`, `resolve_run(.., &Env, ..)`, `env::expand(s, &Env)` | **adopted and extended**: one `app::env::Env` value (the variables plus `HostDirs`: home, cwd, temp, cache) is built once in `main` (`Env::process`) and threaded everywhere; tests build `Env::fixed(..)`; **no test mutates process env**, so `serial_test`/`temp_env` are not needed at all | CONTRACTS §2.9, §2.10 |
| G6 | Go test names in snake_case + `// Go: <file>:<line>` anchor; Go-file→Rust-module and Go-test→Rust-test tables | **adopted** | §2 table, TEST_PLAN.md |
| G7 | shell mechanism pinned: `process_group(0)`, ONE pipe via `nix::unistd::pipe()` + `try_clone`, `tokio::net::unix::pipe::Receiver::from_owned_fd`, explicit `PWD`, `killpg(SIGKILL)` ESRCH-ignored, 3 s WaitDelay as `timeout(3s, read_to_end)`, classification order shell.go:97-121 | **adopted**, with three amendments from the Windows port: the pipe is `std::io::pipe()` — the same `pipe(2)`, portable and `O_CLOEXEC` — `process_group(0)`/`killpg` are now the UNIX half of a two-platform pair whose Windows half is a Job Object (2026-09-11), and the `bash` the child runs is one resolved interpreter, `shell::interp`'s answer, which on Windows is not always a POSIX shell and never carries an injected `PWD` (2026-09-12, DIVERGENCES X-17). Everything the row pins about Unix behaviour is unchanged | CONTRACTS §4.7 |
| G8 | rmcp header truth: custom headers are APPENDED, `accept`/`mcp-session-id`/`last-event-id` are REJECTED (`ReservedHeaderConflict`) → per-server `connect failed: …`; `Authorization` via `custom_headers`, never `auth_header` | **adopted** (verified rmcp-3.1.4 `http_header.rs:20-43`, `common/reqwest/streamable_http_client.rs:29-38`) | §6, DIVERGENCES |
| G9 | handler = `rmcp::model::ClientInfo` (rmcp implements `ClientHandler for ClientInfo`, `handler/client.rs:299`); no custom handler struct | **adopted** (`InitializeRequestParams` is `#[non_exhaustive]` → build with `ClientInfo::new(ClientCapabilities::default(), Implementation::new("iota", "1.0.0"))`) | §6 |
| G10 | compiled-out toolsets/providers/transports keep the config surface stable (stub factories that warn; `SET_NAMES` always five) | **adopted** | CONTRACTS §4.0, §3.0 |
| G11 *(retired 2026-09-01 — one binary, §11)* | opt-in `tls-ring` feature (`reqwest/rustls-no-provider` + `rustls/ring` + rmcp `reqwest-tls-no-provider`), ring provider installed in `main` and in every crate's test harness (`init_tls()`), CI aws-lc-rs leak gate (`cargo tree --prefix none \| grep`); `tracing` with `release_max_level_off`; `docs/SIZE.md` matrix; direct-dependency allowlist (`scripts/direct-deps.allow` + `check-deps.sh`, not a `deny.toml`) + one-line justification rule | **adopted as OPT-IN** (default stays `reqwest/rustls` = aws-lc-rs, exactly what POLICY's probe compiled) | §11, §14 |
| G12 | no `serde_json/preserve_order` | **adopted** — `serde_json::Map` is BTreeMap-backed → sorted keys = Go's `map[string]any` marshal order; replay payloads are `Box<RawValue>` and byte-verbatim regardless | §1.4 |
| G13 | `BudgetExt` for `Option<Arc<TurnBudget>>` | **adopted** | CONTRACTS §2.8 |
| G14 | `LlmError::ImageStreamIncomplete` | **adopted** | CONTRACTS §3.3 |
| G15 | shared fakes in `iota_core::testing` behind feature `testing` | **adopted** | CONTRACTS §2.11 |
| G16 | drop `rand` (SplitMix64), `which`, `dirs`, `url`, `unicode-width` | **partially adopted**: `dirs`, `which`, `url`, `unicode-width`, `path-clean`, `indexmap`, `hex`, `filetime` are NOT direct deps (`$HOME` read directly = Go parity; 15-line PATH scan; `reqwest::Url` for scheme checks; header formatting not ported; lexical clean hand-rolled; `std::fs::File::set_modified` in tests). `rand` is **kept** behind the injectable `Jitter` seam (minimal features) — a hand-rolled PRNG is untested surface for ~40 KB | §11 |
| G17 | `RunCtx::child()` | **adopted** | CONTRACTS §2.8 |
| G18 | `jiff` with `tz-system` (+ `tzdb-zoneinfo`) so image file names use LOCAL time | **adopted** | §1.4 |
| G19 | `is_list_fallback()` predicate on `LlmError`; `McpError::Timeout(Duration)` displayed in Go duration form (`30s`, `300ms`) | **adopted** | CONTRACTS §3.3, §5.5 |
| G20 | land `manager_connect_timeout` first as the rmcp canary; rely on rmcp `TokioChildProcess` drop-kill / `graceful_shutdown` instead of spawning the child by hand | **adopted**; SIGTERM stage **rejected** (rmcp's ladder is stdin-close → 3 s → SIGKILL; adding a SIGTERM before `cancel()` would reorder Go's ladder rather than reproduce it) | §6, DIVERGENCES |
| G21 | `fetch_models` prints `Fetching available models...` to stderr; tuning pass ends with the tools/mcp_servers warning | **adopted** (already in the winner's spec set; made explicit) | CONTRACTS §7.6 |
| G22 | per-dialect wire enum shapes as implementer contract | **adopted** | CONTRACTS §3.4–§3.8 |
| G23 | `parse_window_size` ported as a pure function (config parity) | **adopted** — `config::window` | CONTRACTS §7.4 |
| G24 | injectable `Jitter` on the wire client | **adopted** | CONTRACTS §3.2 |
| G25 | MCP failure warning prints the FULL error text (`Warning: mcp server <name>: <err>`) — POLICY wording, not "first line" | **adopted** | CONTRACTS §7.7 |
| G26 | `--max-turns` stays `i64` (Go treats negatives as unlimited) | **adopted** | CONTRACTS §7.1 |
| G27 | `Session` trait over `RunningService` with an in-memory duplex test impl | **adopted** | CONTRACTS §5.4 |
| G28 | `Message.usage` kept as a dead field / `Provider::stream_chat` / sink `tool_delta`/`image_partial` hooks / `Artifact` channel / image edit endpoints / `Interactor` (fidelity proposal) | **rejected** — POLICY OUT list; zero headless behaviour | DIVERGENCES |
| G29 | HTTP/1.1-only, UTC image names, 512 KiB worker stacks, `serde_json`-less `ProviderError::Wire(String)` (footprint proposal) | **rejected** — user-visible divergences or typed-error loss for no parity gain | — |
| G30 | `async_trait` proc-macro | **replaced by hand-boxed futures** (`iota_core::BoxFuture`): the crate is not in the local registry (`~/.cargo/registry/src` has no `async-trait`), a proc-macro adds compile time, and the four object-safe traits (`Provider`, `ToolProvider`, `Tool`, `Dispatcher`, `Delegator`) need exactly one shape. Used consistently everywhere. | CONTRACTS §2.1 |

---

## 1. Layout — ONE package (decision of 2026-09-02)

### 1.1 One package, one module tree

`rust/Cargo.toml` is a single package, `iota`: a library (`src/lib.rs`) holding every module and a
thin binary (`src/main.rs`, ~40 lines). This mirrors the Go module's one-package build exactly and
replaces the ten-crate workspace of phases 1–3 (`docs/MERGE-PLAN.md`, executed 2026-09-02, records the
full old→new map). No workspace table, no per-area manifests, no `check-deps` plumbing beyond one
`[dependencies]` table; the only cargo feature is `testing` (the shared fakes in `src/testing/`, turned
on for tests by the self-dev-dependency `iota = { path = ".", features = ["testing"] }`).

Top-level module names mirror Go packages where that aids recognition; where the Rust decomposition is
cleaner than Go's (`headless` = headless loop, `repl` = interactive loop, `session` = the store — Go lumps
all three into `chat/`) the Rust split is KEPT as modules. Visibility is Rust-idiomatic: everything is
`pub(crate)` unless `main.rs`, `tests/` or `examples/` genuinely use it.

| module (`src/…`) | Go counterpart | contents |
|---|---|---|
| `lib.rs` | — | module list, `BoxFuture`, `BoxError` |
| `main.rs` | main.go | parse, runtime, signals, exit codes (`anyhow` only here) |
| `app/mod.rs` | internal/app | `HostDirs`, the well-known names/dirs; the parent of the four edge modules below |
| `app/env.rs` | internal/vars, cmd/root.go (os.Getenv seam) | `Env` (the one process-environment seam: variables + `HostDirs`; `process()` in `main`, `fixed()` in tests), `${var}` expansion |
| `app/paths.rs` | (filepath helpers) | `clean`, `rel`, `to_slash` |
| `text/{mod,width,ansi}.rs` | internal/timefmt + tokfmt, internal/textwidth, internal/ui/clip.go | Go `%q`/`%v`/`Duration` formatters; THE grapheme width ruler; wrap/strip/truncate — shared by `markdown`, `ui`, `repl` |
| `provider/{mod,model,usage,sink,error}.rs` | provider/provider.go | `Provider` + capability traits, `ProviderKind`, `Effort`, the message/tool data model, `Usage`, `StreamSink`/`ReasoningGate`, `ProviderError` & co, `new_provider` |
| `provider/{common,think,usage_conv,image_util}.rs` | provider/base.go, thinktag.go, usage.go | `ProviderCore`/`HasCore` with the blanket `Tunable`/`TopPTunable` impls, the think-tag splitter, usage converters |
| `provider/{openai,anthropic,google,openresponses,imagen,images}.rs` | provider/*.go | the seven `Provider` adapters |
| `llm/{mod,client,sse,error,models,chatcomp,responses,anthropic,google,images}.rs` | internal/llm | the hand-rolled HTTP/SSE wire layer (keeps Go's name) |
| `llm/{reqlog,progress,multipart}.rs` | chat/reqlog.go, chat/progress.go, (Go `mime/multipart`) | T3: the `/debug` request log the client records into, the per-turn upload-progress reporter + task-local, the byte-exact multipart writer twin (WP66/WP67/WP64) |
| `tool/{mod,context,approval,fmt,error}.rs` | tool/tool.go, chat/turns.go, chat/approval.go, tool/headerfmt.go | `Tool`/`Dispatcher`/`ToolEnv`/`Delegator` seam types, `PrefixOf`, `ToolError`, the call-header formatters; `context` = the run context every tool takes (`RunCtx`, `TurnBudget`, `ArtifactSlot`) |
| `tool/{sets,dispatch,args,yaml11}.rs`, `tool/defer/{mod,mode}.rs` | tool/tool.go, defer*.go | the set table + framework (`dispatch.rs` = the two `Dispatcher` implementations, `Registry` and the merged union) |
| `tool/builtins/{mod,ask,agent,shell}.rs`, `tool/builtins/code/{mod,tools,walk,udiff}.rs` | tool/ask.go, agent.go, delegate.go, shell.go, code.go | the five built-in sets (`shell.rs` = the `shell` tool's POLICY layer; the tool is `shell` on every platform and under every interpreter, and its DESCRIPTION is what follows the interpreter, DIVERGENCES X-18/X-20) |
| `shell/{mod,exec,interp,jobs}.rs`, `shell/sandbox/{mod,darwin,linux,other}.rs` | internal/shell | process execution + sandboxes (the MECHANISM layer); `interp.rs` answers WHICH interpreter runs a command — `bash -c` on Unix, and on Windows the first of Git Bash, PowerShell and `cmd.exe` the machine has (DIVERGENCES X-17), as one pure function over an injected machine |
| `agents/{mod,skills}.rs` | internal/agents | `Overlay`, `compose_send_history`, skills |
| `mathtext/{mod,delim,parse,symbols,macros,inline,pict,layout}.rs` | internal/mathtext | the LaTeX math engine (T3, WP61/WP62): inline Unicode approximation, 2D layout (Go `Box` → `Pict`), the delimiter scanners; a leaf over `text` — the markdown hooks call `approx_inline`/`render_2d` directly (Phase 5 PR-4 deleted the `MathRenderer` trait) |
| `imgterm.rs` | internal/imgterm | the half-block image rasteriser (T3, WP63) — the ONLY module allowed to name the `image` crate (`tests/layering.rs`) |
| `host/{mod,ansi,cmux,background}.rs` | internal/host | host integration (T3, WP67): `Presenter` per-capability fan-out, the ANSI host (OSC 9 / 9;4 through the facade), the cmux host, the background probe |
| `mcp/{mod,config,manager,transport,error}.rs` | mcp/ | `ServerConfig`/`parse_mcp_flag` (`config`), the rmcp manager |
| `headless/{mod,once,run,batch,report,images,delegator,error}.rs` | chat/chat.go, output.go, parallel.go, images.go, delegate.go | the headless loop — what separates it from `repl` is that there is no terminal (the run context it shares with the tools is `tool/context.rs`) |
| `session/{mod,meta,params,record,rawcodec,id,store,writer,loader,tuning,error}.rs` | chat/session.go, settings.go | the on-disk bundle store (never reads the process environment — `tests/layering.rs`) |
| `markdown/{mod,inline,link,style,sink,preview,highlight}.rs` · `markdown/blocks/{mod,code,table,list,quote,math}.rs` | internal/markdown | the streaming markdown→ANSI renderer; `blocks/` = the five buffering block types, one file each (Phase 5 PR-19); `highlight.rs` = the `CodeHighlighter` seam AND its syntect impl |
| `markdown/html.rs` | (goldmark + chroma in chat/export.go) | T3, WP65: comrak safe-mode GFM → HTML with the syntect `SyntaxHighlighterAdapter` over the two-face syntax set, chroma-shaped `<pre class="chroma">` |
| `ui/facade.rs` | docs/design/ui-architecture.md | the `Ui` trait + value types + guards (what `repl` talks to) |
| `ui/{mod,testutil}.rs` · `ui/runtime/{mod,handle,msgs,event_loop,term,osc,oneshot}.rs` · `ui/render/{mod,region,frame,spans,theme,sink,debug}.rs` · `ui/input/{mod,editor,composer,keys,paste,suggest}.rs` · `ui/surface/…` | internal/ui | the inline terminal engine — the ONLY module allowed to name ratatui/crossterm (`tests/layering.rs`) |
| `repl/{mod,run,turn,toolloop,transcript,group,uisink,interrupt,retry,steer,approval,interact,diff,errors,styles,title,banner,mcpreport,meter,params,tokens,replay,systemtab,commands/…}.rs` | chat/run.go and friends | the interactive loop over the facade |
| `repl/{phases,editpicker}.rs`, `repl/commands/{export,debug,edit,skills}.rs` | chat/run.go:1184-1242, chat/editpicker.go, chat/export.go, chat/debug.go, chat/run.go:450-516, chat/agentmode.go | T3: the busy-phase controller + upload watcher (WP67), the `/edit` picker (WP64), `/export` (WP65), `/debug` (WP66), `/edit`+`/redo` (WP64), `/skills` (WP68) |
| `cmd/{mod,args,resolve,list,tuning,assemble,config_cmd,io,signals,interactive}.rs` | cmd/root.go, delegate.go | the command; `cmd::run` is the library entry `main.rs` awaits |
| `config/{mod,agent,model,provider,params,strict}.rs` | config/ | the YAML config model + merge, plus the key audit and the layered parameters |
| `testing/{mod,scripted}.rs` | (test fakes) | behind the `testing` feature only |

Tests: `tests/<area>/main.rs` — TEN integration binaries (`provider`, `tool`, `mcp`, `session`, `headless`,
`markdown`, `mathtext`, `repl`, `ui_tmux`, `cmd`; `mathtext` was added by T3/WP61 for the Go-generated
2D and inline goldens). The `ui` area has no integration binary because every one of its former files
drove crate-private internals and now lives in-file as `#[cfg(test)] mod tests` under `src/ui/**`.
They share `tests/common/` fixtures and the Go-written bundles under `tests/fixtures/`.
`examples/mkbundle.rs` is the round-trip script's Rust-created bundle.

### 1.2 Layering invariants (former crate boundaries, now `tests/layering.rs`)

The crate boundaries that carried a design rule are one test binary, `tests/layering.rs` (until
2026-09-15 three greps in `ci.sh`), Go's own discipline:

- **the module graph points down.** The test scans every `crate::<module>` path in `src/` (`#[cfg(test)]`
  modules blanked, comments cut, `src/testing/` not scanned) and asserts each edge lands in a LOWER row of
  the declared order, bottom first: `app` · `{text, vars, paths, sync, imgterm}` · `{color, diag}` · `llm` ·
  `provider` · `{shell, agents}` · `tool` · `{mcp, session, mathtext}` · `{config, markdown, chat}` · `ui` ·
  `host` · `repl` · `cmd`. Siblings in one row never name each other; product code never names the fakes
  (`testing`). The upward edges the tree still carries sit in the test's `KNOWN_UPWARD` table, each with
  the phase-5 PR that retires it — a new upward edge is red, and so is a row whose edge is gone;
- only `src/ui/**` may name `ratatui`/`crossterm` — the loop (`repl`), the renderer (`markdown`) and the
  command never see a terminal crate;
- `src/session/**` never reads the process environment (`std::env::var`) — the store takes its root from
  an injected `HostDirs`;
- only `src/imgterm.rs` may name the `image` crate — every other module sees `imgterm::Frame`;
- the process's stderr has one writer, `cmd::io::Streams` (`warning`/`caution`): no `eprintln!` and no
  `io::stderr()` anywhere else in product code.

The one-way `cmd → {headless, session}` edge is now a convention, not a manifest: `headless::run_once` still
returns the turn's message delta and `cmd` still owns the `SessionWriter`; the loop never names the store.

### 1.3 Why one package

- **Build model = Go's**: one `cargo build --release`, one artifact, no member list to keep in step with
  a feature story that no longer exists (§11: one binary since 2026-09-01).
- **The crate-boundary hacks died with the boundaries**: the `impl_tunable_via_core!` macro (orphan rule)
  is a plain blanket `impl<T: HasCore + Send + Sync> Tunable for T`; `PrefixOf`, `mcp::ServerConfig` and
  `app::env::Env` live where they are used; no `#[path]`-mounted test remains (those became unit tests).
- **Link cost**: ~10 test binaries instead of ~70 each linking the whole library.
- What the split bought — dependency isolation, testability — is unchanged: every dependency is still
  in one table (`scripts/direct-deps.allow`), and every test still uses fakes, wiremock, temp dirs and
  in-process duplex servers; the `pub` surface is curated instead of implied by crate edges.
- The 3–6-package POLICY range this section used to cite is retired with the workspace.

### 1.4 Manifest

`rust/Cargo.toml` (the single `[package]`). Highlights:

- edition 2024, `rust-toolchain.toml` = `1.98.0` (+ rustfmt, clippy); MSRV of every dep verified (`rmcp` 1.88, `globset`/`ignore` 1.88, `reqwest` 1.85, `clap` 1.85, `sha2` 1.85).
- `serde_json = { features = ["raw_value"] }` — **no** `preserve_order` (G12).
- `reqwest = { default-features = false, features = ["json", "stream", "http2", "system-proxy", "rustls"] }` (no `multipart`: edits are OUT); ONE TLS backend, rustls with aws-lc-rs and the platform verifier — no ring alternative, no direct `rustls` dependency, no provider-installation step.
- `rmcp = { default-features = false, features = ["client", "transport-child-process", "transport-streamable-http-client-reqwest", "reqwest"] }` (rmcp's reqwest-with-rustls pairing) unconditionally; the dev-dependency adds `"server"` + `"transport-async-rw"` for the in-process echo server.
- Cargo features: **none** that affect the binary. The only `[features]` entry is `testing` (the shared test fakes), enabled for tests by the self-dev-dependency. ratatui's `scrolling-regions` is on, always.
- `jiff = { default-features = false, features = ["std", "tz-system", "tzdb-zoneinfo"] }` (G18).
- `tokio` features: `rt-multi-thread, macros, sync, time, process, io-util, signal, fs, net` (`net` = `tokio::net::unix::pipe`); the dev-dependency adds `test-util` (paused clock for the retry tests).
- Process supervision is the one per-platform dependency pair, both in `[target.'cfg(<os>)'.dependencies]` and both reached only from `src/shell/exec.rs`: `nix = { default-features = false, features = ["signal", "process", "fs"] }` on Unix (`killpg` + `Signal` + `Errno::ESRCH`), and on Windows `process-wrap = { default-features = false, features = ["tokio1", "job-object", "creation-flags", "tracing"] }` for the Job Object that `TerminateJobObject` kills as one, plus `windows = { features = ["Win32_System_Threading"] }` for the single constant `CREATE_NO_WINDOW` its `CreationFlags` wrapper takes. `windows` is a CARET range on purpose: it must resolve to the same copy of the crate process-wrap builds against or the flag type stops unifying.
- Release profile: `opt-level = "z"`, `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`, `strip = true`.
- Lints (`[lints]`): `unsafe_code = "forbid"`, `missing_docs = "warn"`, `clippy::all` + `clippy::pedantic` = warn (CI runs `-D warnings`), `unwrap_used`/`expect_used`/`panic` = deny (allowed under `cfg(test)`), justified allows: `module_name_repetitions`, `missing_errors_doc`, `missing_panics_doc`, `too_many_lines`, `struct_excessive_bools`, `must_use_candidate`.
- `#![forbid(unsafe_code)]` in `lib.rs` and `main.rs`; no `unsafe` module exists at all (`std::io::pipe` covers pipes, `nix` signals, `std::os::unix::process::CommandExt::process_group` process groups, and `process-wrap` the Win32 Job Object calls — that last one is the whole reason the crate is a dependency rather than forty lines of our own `windows-sys`).

---

## 2. Module map (Rust module → Go file(s) ported)

The tables keep the phase-1 grouping (one per former crate) with each file named at its post-merge home.

### the contracts (formerly `iota-core`)
| module | Go | contents |
|---|---|---|
| `lib.rs` | — | module list, `BoxFuture`, `BoxError` |
| `provider/model.rs` | provider/provider.go:11-59 | `Role`, `Attachment`, `ToolDef`, `ToolCall`, `JsonObject`, `Raw` (comparable `Box<RawValue>` newtype), `RawContent`, `Message` |
| `mcp/config.rs` | mcp/manager.go:22-30,530-549, mcp/vars.go | `ServerConfig`, `parse_mcp_flag`, `expand_server_config`, `endpoint_of`, `McpFlagError` (feature-independent) |
| `provider/usage.rs` | provider/provider.go:61-130 | `Usage` + derived figures + `AddAssign` |
| `provider/mod.rs` | provider/provider.go:132-331, base.go | `ProviderKind`, `Effort`, `Provider`, `ToolProvider`, capability traits, `ChatResult`, `RoundResult`, `ImageGenParams/Options`, `ToolSearcher` |
| `provider/sink.rs` | provider.go:136-137 contract | `StreamSink`, `NullSink`, `ReasoningGate` |
| `tool/mod.rs` | tool/tool.go:33-330 | `ToolOutput`, `ToolError`, `Presentation`, `Tool`, `Dispatcher`, `DeferredToolStatus`, `DeferState`, `ToolEnv`, `PrefixOf` |
| `tool/mod.rs` (delegation half) | tool/tool.go:245-288 | `Delegator`, `AgentInfo`, `DelegateSpec`, `DelegateResult`, `DelegateOutcome` |
| `tool/context.rs` | chat/turns.go | `RunCtx`, `TurnBudget`, `BudgetExt`, `ArtifactSlot` |
| `app/env.rs` | internal/vars/vars.go, cmd/root.go (os.Getenv seam) | `Env` (`process`, `fixed`, `var`, `cwd`, `home`, `dirs`), `expand` |
| `app/mod.rs` | internal/app/app.go (+ os.UserCacheDir/TempDir rules) | `NAME`, `DOT_DIR`, `CONFIG_BASE`, `CONFIG_EXTS`, `HostDirs`, `user_home`, `cache_dir` |
| `app/paths.rs` | filepath.Clean/Rel semantics | `clean`, `rel`, `to_slash` (+ `within`, test-only) |
| `text/mod.rs` | tool/agent.go:240-276, fmt %q/%v | `split_lines`, `truncate_to_char_boundary`, `go_quote`, `go_float`, `go_duration` |
| `provider/error.rs`, `tool/error.rs` | provider.go:207-214, tool.go | `ProviderError`, `PermanentError`, `UnknownProviderType`, `InvalidEffort`; `ToolError` (`BoxError` is in `lib.rs`) |
| `testing/{mod,scripted}.rs` (feature `testing`) | chat/*_test.go fakes | `RecordingSink`, `SinkEvent`, `StaticDispatcher`, `FakeDelegator`, `FakeToolProvider` |

### the providers and the wire layer (formerly `iota-llm`)
| module | Go |
|---|---|
| `provider/mod.rs` (factory half) | provider/provider.go:310-331 (`new_provider`, `ProviderParams`) |
| `llm/mod.rs` | — (module list + `is_zero`) |
| `llm/client.rs` | internal/llm/client.go (Client, retry loop, StatusError, default_http_client, Jitter, header-timeout seam) |
| `llm/sse.rs` | internal/llm/sse.go |
| `llm/error.rs` | client.go:32-47 + per-dialect texts (`LlmError`, `RespFailure`) |
| `llm/models.rs` | internal/llm/chatcomp.go (Models) — the OpenAI-shaped `GET /models` shared by chatcomp and responses |
| `llm/chatcomp.rs` | internal/llm/chatcomp.go (minus Models) |
| `llm/responses.rs` | internal/llm/responses.go |
| `llm/anthropic.rs` | internal/llm/anthropic.go |
| `llm/google.rs` | internal/llm/google.go (generateContent + models + :predict) |
| `llm/images.rs` | internal/llm/images.go (generations + models + consume_images; NO edits) |
| `provider/think.rs` | provider/thinktag.go |
| `provider/usage_conv.rs` | provider/usage.go |
| `provider/common.rs` | provider/base.go (`ProviderCore`, `HasCore`, the blanket `Tunable`/`TopPTunable` impls), attachment data-URL helpers |
| `provider/image_util.rs` | provider/imagen.go:117-124,226-235, images.go:169-229 (`last_user_turn`, `image_mime`, `sniff_image`, `ext_for_mime`, `fetch_image`) |
| `provider/{openai,openresponses,anthropic,google,imagen,images}.rs` | provider/<same>.go |

### the tool framework and sets (formerly `iota-tools`)
| module | Go |
|---|---|
| `tool/context.rs` | chat/turns.go (`RunCtx`, `TurnBudget`, `BudgetExt`, `ArtifactSlot`) |
| `tool/approval.rs` | chat/approval.go, chat.go:348-364 (`Approval`: the answer to a gated call) |
| `tool/sets.rs` | tool/tool.go:329-341 (`SET_NAMES`, `set_factory`, `RawNode`, `ToolsConfig`, `SetFactory`, `SetError`) |
| `tool/dispatch.rs` | tool/tool.go:343-528 (`Registry`, `set_disabled`) + 538-673 (`merge`, the live union) |
| `tool/defer/mod.rs` | tool/defer.go |
| `tool/defer/mode.rs` | tool/defermode.go + defermode_protocol.go |
| `tool/yaml11.rs` | yaml.v3 bool leniency (new) |
| `tool/args.rs` | tool/tool.go:679-689, tool/agent.go:174-197,253-263 (`bool_arg`, `int_arg`, `str_arg`, `read_file_limited`) |
| `tool/builtins/ask.rs` | tool/ask.go:18-27 |
| `tool/delegate.rs` | tool/delegate.go |
| `tool/builtins/shell.rs` | tool/shell.go |
| `shell/exec.rs` (+ `shell/mod.rs`) | internal/shell/shell.go + proc_unix.go |
| `shell/sandbox/{mod,darwin,linux,other}.rs` | internal/shell/sandbox_*.go |
| `shell/interp.rs` | — (Go had one shell; DIVERGENCES X-17) |
| `tool/builtins/code/mod.rs` | tool/code.go:45-220 (config, `CodeSet`, jail, ledger, byte_count, looks_binary) |
| `tool/builtins/code/walk.rs` | tool/code.go:160-198 (gitignore walk) |
| `tool/builtins/code/tools.rs` | tool/code.go:226-878 (six tools) |
| `agents/mod.rs` | internal/agents/agentsmd.go |
| `agents/skills.rs` | internal/agents/skills.go |
| `tool/builtins/agent.rs` | tool/agent.go |

### the MCP manager (formerly `iota-mcp`)
| module | Go |
|---|---|
| `mcp/mod.rs` | re-exports, `#[cfg(test)] mod testutil` (duplex echo server) |
| `mcp/manager.rs` | mcp/manager.go:65-474 (+ the `merge_result` unit tests) |
| `mcp/transport.rs` | mcp/manager.go:337-398,478-512 (connect_one, make_transport, `Session`, `RmcpSession` over `call_tool_once`) |
| `mcp/error.rs` | manager.go error texts (`McpError`; `EmptyFlag` is `mcp/config.rs`'s) |

### the session store (formerly `iota-session`, phase 2 slice 1) — `src/session/`
| module | Go | contents |
|---|---|---|
| `mod.rs` | — | re-exports; the bundle-layout doc |
| `error.rs` | chat/session.go error texts | `SessionError` (`NotFound`, `CannotRead`, `NoMatch`, `Ambiguous`, `ReadLog`, `HomeNotDefined`, `Io`) |
| `meta.rs` | chat/session.go:39-65,481-489,752-760 | `SessionMeta` in Go's struct order with the `#[serde(flatten)] extra` map (D-46), `read`/`write` (temp+rename, no trailing newline — D-45), `now_rfc3339`/`parse_rfc3339`, plus `top_p` and `param_sources` (X-25) |
| `params.rs` | (new) | `ParamSource`/`ParamSources`/`Param`/`LayeredParams` — what a session runs under for the four layered parameters and where each value came from (X-24) |
| `record.rs` | chat/session.go:67-130 | the `messages.jsonl` line DTOs with Go's exact `omitempty` matrix (`arguments` and `usage.in`/`usage.out` always emitted) |
| `rawcodec.rs` | chat/session.go:529-538,796-804 | `raw_to_blob`/`blob_to_raw` — a pure function of `ProviderKind`, so the store never names the wire layer; the blob is never parsed (D-51a) |
| `id.rs` | chat/session.go:211-280 | the 12-char Crockford-base32 alphabet, bias-free generation, `resolve_in` (exact → unique prefix → ambiguous) |
| `store.rs` | chat/session.go:154-305,335-402,904-1028 | `SessionStore`, `project_slug`, `find_dir`/`dir`, `id_taken`/`new_id`, `list`/`list_all`, `resolve_id` (scope-first, only `NoMatch` widens), `create`/`resume`/`load` |
| `loader.rs` | chat/session.go:762-901, chat/compact.go:19-28 | chunked `scan_records` with the 32 MiB cap enforced while reading (D-56), `record_to_message`, `load_log` with the compaction weave |
| `writer.rs` | chat/session.go:307-748 | lazy `ensure_created`, `append_messages` (one fsync per batch, then one meta rewrite), `append_compaction`, `update_meta` (Go's eight `Set*` collapsed into one), `images_path`/`images_dir`, the content-addressed attachment store |
| `tuning.rs` | chat/session.go:414-455 | `apply_session_tuning`, gated on the provider tag first; the context window is returned, not pushed through Go's `setWindow` callback; `top_p` replays beside effort and temperature (X-25) |

### the headless loop (formerly `iota-chat`) — `src/headless/`
| module | Go |
|---|---|
| `mod.rs` | chat/output.go:29-53, chat/agentmode.go (`OutputFormat`, `parse_output_format`, `AgentOptions`) |
| `once.rs` | chat/chat.go:35-68 (+ `OnceOptions.history` / `OnceOutcome.delta`) |
| `run.rs` | chat/chat.go:73-127,284-379, chat/run.go:68-74,221-229,1092-1095, chat/delegate.go:25-51 (`run_once`, `execute_with_tools` over a `TurnParams`, `QuietHost`; the history watermark and the turn delta) |
| `batch.rs` | chat/parallel.go:42-57,106-140 |
| `report.rs` | chat/output.go:64-215 |
| `images.rs` | chat/images.go:21-58,117-148,156-170 (`save_images_for_turn` = `collectImages`' saved subset) |
| `delegator.rs` | chat/delegate.go:58-162 |
| `error.rs` | chat/chat.go:266-294 (`ChatError`) |

### the command (formerly `iota`) — `src/cmd/` + `src/config.rs` + `src/main.rs`
| module | Go |
|---|---|
| `main.rs` | main.go (+ signal/exit-code policy) |
| `cmd/mod.rs` | cmd/root.go:41-268 (`run_agent`) + cmd/root.go:284-334 (the resume stage on the `-m` path, D-41); the verb dispatch is `run` |
| `cmd/args.rs` | cmd/root.go:22-39,418-436, rebuilt as a verb set (X-10 … X-14) |
| `config/` | config/config.go, plus `config/strict.rs` (the key audit, X-15) and `config/params.rs` (the layered parameters, X-24) |
| `cmd/resolve.rs` | cmd/root.go:46-123,534-566 (`ModelRequired` deferred for a resume, D-52) |
| `config/window.rs` | chat/tokens.go:20-43 |
| `cmd/list.rs` | cmd/root.go:439-529, rebuilt as `iota list` (X-13) |
| `cmd/config_cmd.rs` | (new) `iota config check\|path\|init` (X-16) |
| `cmd/tuning.rs` | cmd/root.go:133-199 |
| `cmd/assemble.rs` | cmd/root.go:574-658 |
| `cmd/io.rs` | stderr warning sinks (`Warning: …`, `⚠ …`) |
| `cmd/signals.rs` | (new) SIGINT/SIGTERM → CancellationToken |

The TUI slice's three former crates are `src/markdown/`, `src/ui/` and `src/repl/` (their module list is §1.1; the per-file Go anchors are `grep -rn '// Go: ' src/{markdown,ui,repl}`).

---

## 3. Core data model (`provider::model`, `provider::usage`)

Exact definitions: CONTRACTS.md §2. Summary of the shape decisions:

- `Role { System, User, Assistant, Tool }` serialised as the lowercase strings.
- `Attachment { filename, mime_type, data: Vec<u8> }`; `ToolDef { name, description, input_schema: Option<JsonObject>, deferred }`; `ToolCall { id, name, arguments: JsonObject }`; `JsonObject = serde_json::Map<String, Value>` (sorted keys).
- `Raw(Box<RawValue>)` is the one spelling of a verbatim JSON payload: `serde_json::value::RawValue` has no `PartialEq` (verified in serde_json-1.0.151), so the newtype implements it as byte-equality of the JSON text and `#[serde(transparent)]` keeps the wire shape and the null-as-absent rule for `Option<Raw>`. `RawContent` is a typed enum per dialect: `OpenAi(Raw)`, `Anthropic(Vec<Raw>)`, `OpenResponses(Vec<Raw>)`, `Google(Raw)`. Each dialect trusts only its own variant and reconstructs otherwise (Go's failed type assertion). This is what lets `Message`, `RoundResult` and `GPart` derive `PartialEq` and the history-shape tests use `assert_eq!`.
- `Message { role, content, reasoning, attachments, tool_calls, tool_call_id, tool_call_name, is_error, raw_content, tools }`. `interrupted` and `usage` are dropped (session-only; no dialect reads them — DIVERGENCES D-11).
- `Usage { input, output, cache_read, cache_write, total: u64 }` with the Go formulas byte-for-byte.
- Tool results: `ToolOutput { text, is_error }` (model-facing) vs `ToolError` (hard error rendered `Error calling tool: {e}`).
- Run report structs live in `headless::report` with struct order = Go struct order = JSON key order; `serde_json::to_writer_pretty` + `"\n"` reproduces `SetEscapeHTML(false)` + `SetIndent("", "  ")` byte-for-byte.

---

## 4. Provider abstraction

### 4.1 Results, not getters
Go's `LastUsageFull()/LastRawContent()/LastImages()` become fields of `ChatResult { text, usage, images }` and `RoundResult { content, reasoning, tool_calls, usage, raw_content, images }`. The "read it NOW" hazard and `begin_call()` disappear. `UsageReporter`, `RawContentProvider`, `ImageOutputProvider` do not exist as traits; the capability-surface tests port as `as_tunable().is_none()` + `result.usage.is_none()`.

### 4.2 `&self` calls, `Send + Sync`
`Provider` and `ToolProvider` calls take `&self` (G1). Providers hold only construction-time state (`ProviderCore`, flags, an installed `ToolSearcher`). Setters (`set_model`, `Tunable`, `set_tool_searcher`) take `&mut self` and are called before the run; children get a fresh `Box<dyn Provider>` per delegation, so nothing is ever shared mutably.

### 4.3 Object safety without proc-macros
`iota::BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>`. Every async trait method is `fn name<'a>(&'a self, …) -> BoxFuture<'a, R>`; implementations write `Box::pin(async move { … })`. This is the single convention for `Provider`, `ToolProvider`, `Tool`, `Dispatcher`, `Delegator` (G30). Plain `async fn`s are used everywhere else.

### 4.4 Enums
`ProviderKind` (seven kinds, `as_str`, `FromStr` with the exact `unknown provider type: …` error, `ALL` in Go order, `SUPPORTED_LIST`), `Effort { Low, Medium, High, XHigh, Max }` (`parse("")` → `Ok(None)`; invalid → `InvalidEffort`), `DeferMode`, `DeferState`, `OutputFormat`.

### 4.5 Streaming sink and the reasoning-close contract
`StreamSink { content, reasoning, reasoning_done }`, `NullSink` (headless), `ReasoningGate` (idempotent close, close-before-first-content, close on `Drop`). Dialects hold a gate, never the raw sink. Ordering pinned by `RecordingSink` tests: wire reasoning deltas → `gate.reasoning()`; first tool-argument delta → `gate.close()`; visible content goes through the think splitter which calls `gate.content()`.

### 4.6 Shared core
`ProviderCore { kind, model, temperature, top_p, effort }` + `HasCore`. Since the 2026-09-02 merge the traits and the providers share a crate, so `provider/common.rs` carries the plain blanket impls `impl<T: HasCore + Send + Sync> Tunable for T` and `impl<T: HasCore + Send + Sync> TopPTunable for T`; the per-provider `impl_tunable_via_core!` macro that the orphan rule (E0210) forced across the old crate boundary is gone. Test fakes implement `Tunable` directly on their own (local) types and coexist with the blanket impl.

### 4.7 Wire client, SSE, think splitter
Byte-for-byte ports of client.go / sse.go / thinktag.go (CONTRACTS §3.2–§3.3, §3.9). Uniform 2-minute response-header timeout on every client (POLICY 4.b) — injectable via `Client::with_header_timeout` for tests, applied to `image_util::fetch_image` too; retries 2 (0 for image dialects); jitter through the `Jitter` seam; cancellation = `tokio::select!` against the token in the retry sleep and around every body read. The OpenAI-shaped `GET /models` listing is one function (`wire::models::openai_model_ids`) shared by chat-completions and responses.

### 4.8 Seven providers on five dialects
| provider | wire | core | capabilities | RawContent |
|---|---|---|---|---|
| `OpenAiProvider` | chatcomp | yes | tool, tunable, top_p | `OpenAi` |
| `OpenResponsesProvider` | responses (+chatcomp models) | yes | tool, tunable, top_p, image_tunable, tool_search_host | `OpenResponses` |
| `AnthropicProvider` | anthropic | yes | tool, tunable, top_p | `Anthropic` (replayed only with a deferred tool in the request) |
| `GoogleProvider` (gemini/vertexai) | google | yes | tool, tunable, top_p, image_tunable | `Google` (sanitised on replay) |
| `ImagenProvider` | google predict/models | no | image_gen_tunable | — |
| `ImagesProvider` | images generations/models | no | image_gen_tunable, image_edit_json_tunable | — |

---

## 5. Tool framework

- `Tool` / `Dispatcher` with default capability methods; three-way `owns() -> Option<bool>`; `search_tools() -> Option<Vec<ToolDef>>` (Merge forwards to the FIRST part that HAS the capability); `supports_parallel(name, Option<&JsonObject>)` per CALL (`None` = Go nil args).
- `RunCtx { cancel, budget, ledger }` cloned into every tool call and child; `RunCtx::child()`.
- `ToolEnv { project_root: Option<PathBuf>, dirs: HostDirs, delegate: Option<Arc<dyn Delegator>> }`; `root()` = project_root else `dirs.cwd` else `current_dir()`, then `std::path::absolute`.
- `Registry` (sorted keys, first-wins names, YAML-1.1 disable rule), `Merged` (live, owner-aware), `DeferDispatcher` (normal/system-tools), `MarkedDispatcher`/`SearchingDispatcher` (reference/tool-search), `resolve_defer_mode(name, ProviderKind, warn)` with the RESOLVED kind (POLICY fix).
- Approval/parallel: the REFUSAL lives in `headless::QuietHost`; tools only report. Only `glob`/`grep`/`list_dir`/`read_file` (always) and `delegate` (read-only agent) opt into parallel.
- The delegate cycle `ChatDelegator::run → run_once → execute_with_tools → dyn Dispatcher::call_tool → DelegateTool::call → dyn Delegator::run` crosses two boxed-future boundaries; no `async_recursion`.
- Interactive-only capabilities (`presentation`, `header_summary`, `deferred_tools`) keep their trait defaults; no built-in implements `header_summary`/`presentation` (DIVERGENCES D-12), `DeferDispatcher`/`MarkedDispatcher` do implement `deferred_tools` (cheap, tested).

---

## 6. MCP manager (rmcp 3.1.4)

Verified API (CONTRACTS §5.0 lists file:line): `ServiceExt::serve(ClientInfo, transport) -> RunningService<RoleClient, ClientInfo>` (legacy `initialize` handshake by default; rmcp also implements `server/discover` via `ClientLifecycleMode::Auto`, not selected — D-02); `peer().list_all_tools()`; **`peer().call_tool_once(CallToolRequestParams::new(raw).with_arguments(args))`** — never `call_tool`, which drives SEP-2322 MRTR rounds through a handler that cannot answer; `Complete(r)` → text/is_error, `InputRequired`/`Task` → error texts (D-33); `RunningService::cancel(mut self)` consumes the service, so the production `Session` is `RmcpSession { running: tokio::sync::Mutex<Option<RunningService>>, peer: Peer }` (calls use the cloned peer, `close` takes the service out of the mutex); `TokioChildProcess::builder(cmd).stderr(Stdio::piped()).spawn() -> (proc, Option<ChildStderr>)`, `proc.id()`; `StreamableHttpClientTransport::with_client(http, StreamableHttpClientTransportConfig::with_uri(url).custom_headers(map))`; `CallToolResult { content: Vec<ContentBlock>, is_error: Option<bool>, .. }`, `ContentBlock::as_text()`; `Tool { name: Cow<str>, description: Option<Cow<str>>, input_schema: Arc<JsonObject>, .. }`.

- Connect fan-out: `JoinSet`, per-server `tokio::time::timeout(30 s, connect_one)`; results merged in CONFIG order (config-file servers sorted by name, then `--mcp` flags in order) so segment suffixes are deterministic (POLICY 3).
- Timeout → `connection timed out after 30s` (Go duration formatting); dropping the connect future drops the `TokioChildProcess` whose `ChildWithCleanup::drop` kills the child (rmcp `child_process.rs:44-55`).
- Headers: `custom_headers` (append; reserved names rejected → `connect failed: Header name 'accept' is reserved and conflicts with default headers`); `HeaderName`/`HeaderValue` parse failures → `connect failed: <err>`.
- `close()`: take sessions under the write lock; `timeout(10 s, running.cancel())` per session outside the lock (rmcp: close transport → stdin EOF → 3 s → kill); index cleared so later `call_tool` returns `unknown tool` (no panic).
- Headless host prints `Warning: mcp server <name>: <err>` for every failed server (POLICY divergence, full text).
- Configuration types (`ServerConfig`, `parse_mcp_flag`, `McpFlagError`) live in `mcp::config` and are re-exported from `mcp`, so `cmd::assemble` parses `--mcp`/`mcp_servers` (and raises `--mcp: empty server specification`) without naming `Manager`; `iota-mcp` depends only on `iota-core` and produces `iota_core::tool::PrefixOf`.
- Internal seams (`Session`, `ServerResult`, `merge_result`) stay `pub(crate)`; their tests are unit tests inside `manager.rs` using an in-crate duplex echo server (`#[cfg(test)] mod testutil`). Only public-API tests (`connect_all` timeout canary, reserved-header failure, idempotent close) live under `tests/`.

---

## 7. Config + CLI (`iota`)

- clap derive `Cli` is a CLOSED verb set — `run` / `list` / `resume` / `config` / `version`, with a bare `iota` meaning `run` (X-10) — plus the nine flags that describe ONE invocation (X-11). `-c/--config` is declared once on the root as a GLOBAL argument, so it is valid on either side of the verb; the other seven belong to `RunArgs`, and one of them given BEFORE another verb is refused by `Cli::check_flag_placement` (clap's `args_conflicts_with_subcommands` cannot be used for that: with it set, any root argument stops the verb from being recognised at all, which is what made a global `-c` impossible). `cmd::run` dispatches on the verb; `run` and `resume` share `run_agent`, which normalises both into an `Invocation { agent, resume, args }` so nothing downstream knows which word was typed.
- `run_agent`'s order is Go's (cmd/root.go): `-m` → one headless turn; otherwise → the interactive branch (`interactive/`: `mod.rs`, the `iota resume` picker in `picker.rs`, the title stack in `title.rs`), taken at exactly Go's headless-vs-interactive branch (root.go:259), i.e. AFTER agent, key, model, temperature, tuning, MCP-config and output-format checks, so every earlier byte-pinned error (`unknown agent "codr"…`, `--output-format applies to -m runs only`) still wins; a non-TTY stdout is then refused byte-exact with Go's `interactive mode requires a terminal; use -m/--message for piped input`. `resolve_run` therefore returns `message: Option<String>`. `--no-save` and a bare `iota resume` are rejected for `-m` runs only (`reject_unsupported`, `ArgsError::ResumeIdRequired`); the interactive branch reads them for real. `-m ""` → `--message must not be empty`.
- `Config` model with `serde_norway`, `#[serde(default)]`; bool fields through `tool::yaml11::deserialize_bool` (YAML 1.1 spellings); `tools: BTreeMap<String, serde_norway::Value>` raw; `mcp_servers: Option<Vec<String>>` (absent ≠ `[]`); `defer: Option<String>`. Unknown and misplaced keys are refused BEFORE the typed decode by `config::strict::audit`, which walks the raw `serde_norway::Value` so an error can name its coordinate (`agents.coder.tools.delegate`) — something `deny_unknown_fields` cannot (X-15). The document is therefore parsed twice: once as a `Value` for the audit, once into the typed shape, which keeps serde's line/column on a field's type error.
- `Config::load(explicit, &Env, warn)` fails on anything the user wrote wrong (a misplaced key, a dangling reference, an unusable `defer_mode`) and only WARNS for what says nothing about intent — an unreadable file, or one that is not YAML at all. `Config::sources` is the file list `iota config path` prints; `merge_file` replaces whole entries; `${var}` expanded once at merge time on key/url/system_file.
- `Config::provider(name) -> Endpoint<'_>` is the one place a provider name resolves to its endpoint: the `providers:` entry (a default one for an unconfigured built-in type) and the TYPE behind it. `Endpoint::api_key(&Env) -> ApiKey` is the one definition of the key precedence — the type's variable, else `key:`, else `Missing` — which `resolve_run`, `iota list providers` and the `/model` catalog all consume (X-34).
- `resolve_run(inv, cfg, &Env, stdin)` is pure and follows root.go:46-123 order exactly, with the agent lookup (`Config::resolve_agent`) where Go had the four-namespace one.
- `config/params.rs` holds the LAYERED evaluation of `context_window`/`effort`/`temperature`/`top_p` (X-24, brain page `model-param-layering`): `Declared::of` folds `agents:` over the running model's `models:` entry, `Declared::evaluate` resolves that against the session's current value — keeping one the user set and DROPPING one an earlier model's declaration supplied — and `Declared::resume` restores a bundle's own record instead of evaluating. `ParamLayers` is the table a live chat re-evaluates against (the agent, every `models:` entry by `provider:id`, and the `defer_mode` the dispatcher was assembled with). The window arrives already parsed, so `config::window::parse_window_size` stays the caller's: a bad value aborts a run at startup (`SetupError::ContextWindow`, labelled with the layer that wrote it) and is a transcript warning mid-chat. `interactive/mod.rs::resolve_params` is the startup/resume moment, `repl::params` the `/model` one.
- `tuning::apply` = image → effort → top_p → temperature → json_edits → gen-params → tools/mcp warning.
- `assemble::build_mcp_configs` / `build_dispatcher` mirror root.go:574-658: they use only `mcp::config` types and take the MCP part as `Option<(Arc<dyn Dispatcher>, PrefixOf)>` (`None` = no server configured); `Manager` is named only in `cmd::run` and `interactive/mod.rs`. (`cmd/delegate.rs` and its `ChildFactory` went with the delegate toolset, X-01.)

---

## 8. Headless run loop (`headless`)

`once(cancel, provider: &mut dyn Provider, dispatch: Arc<dyn Dispatcher>, opts: OnceOptions, out: &mut (dyn Write + Send)) -> Result<OnceOutcome, ChatError>` (a `Send` future, awaited via `block_on` only; `OnceOutcome.delta` is the turn's message delta — phase 2 slice 1):
1. `rec = RunRecorder::start()`, `budget = TurnBudget::new(opts.max_turns)`, `ledger = Arc::new(DelegationLedger::default())`, `cx = RunCtx { cancel, budget, ledger: Some(..) }`.
2. install the tool searcher on the provider when it has the host (closure = `dispatch.search_tools(q).unwrap_or_default()`).
3. `run_once(&cx, &*provider, &req, dispatch, 0 /* local cap */, &mut host, images_dir)`.
4. If `cx.cancel.is_cancelled()` and the run failed → error becomes `ChatError::Interrupted`.
5. JSON: always `write_report` then return the run error (write error wins). Text: on error write nothing; else `reply\n` (non-empty), `🖼 saved: <path>` lines, then image error lines.

`run_once` (chat.go:73-127): the history starts as `req.history` (the imported view) and that length is the **watermark**; the `-s` system message is pushed only when the imported history is empty, so a resumed session keeps its own (run.go:68-74). Tool path → `execute_with_tools(..) -> LoopOutcome { content, reasoning, images, usage }` and the images are the TERMINATING round's `RoundResult.images` (Go read `LastImages()` after the loop); unary path → `provider.chat(compose_send_history(..))` and `result.images`. Both paths then `save_images_for_turn(&images, images_dir)` (children pass `None`), so a gemini/openresponses run with tools advertised still saves what it generated. The final assistant message (Go's `amsg`, run.go:1092-1095) is then pushed with the terminating round's usage and exactly the SAVED image subset (D-53), and `RunOutcome.delta` is everything past the watermark — Go's `history[persisted:]`.

`execute_with_tools` per round (chat.go:284-379 order): local cap → `budget.take()` → live `tools()` after round 0 → `take_pending_loads()` mount → `stream_chat_with_tools(.., compose_send_history(history, overlay), .., &mut NullSink)` → `rec.observe(usage, names)` → termination (reasoning-only rule; returns `LoopOutcome` with that round's images) → assistant message with `raw_content` → execution walk (`parallel_run` / `run_batch` / serial with the approval gate, `detail = dispatch.header_summary(..).unwrap_or_default()` — D-12). Refusal text verbatim (CONTRACTS §6.4).

`ChatDelegator::run`: unknown agent → `unknown agent "x"`; build error passthrough; per-task effort on the FRESH provider; own `RunRecorder`, shared budget; children never save images (POLICY); `cx.ledger.add(rounds, usage)` ALWAYS; returns `DelegateOutcome`.

Signals: `main` installs `ctrl_c` + `SIGTERM` listeners that cancel the root token; every long await selects on it; exit 130 after `mgr.close()`.

### 8.1 Session data flow (phase 2 slice 1)

The loop stays session-blind. `iota::run` owns both ends, and the whole stage sits between Go's
headless-vs-interactive branch (root.go:259) and the MCP connect, so `iota resume <id>` without `-m` still ends
in `interactive mode is not available…`, every earlier byte-pinned error still wins, and a bad session
id never spawns a server.

```
iota resume <id|prefix>
   └─ SessionStore::from_dirs(&dirs)          <home>/.iota/sessions      (never reads $HOME itself)
      └─ resolve_id(fragment, scope)          scope = the project bucket in agent mode, else flat;
         │                                    only NoMatch widens to the merged view
         └─ resume(&id, kind) ──► (SessionWriter, Session)
               │                    │
               │                    ├─ meta   ──► model replay (only if -M absent and the tag matches)
               │                    │            ──► ModelRequired re-raised here (D-52)
               │                    │            ──► apply_session_tuning(temperature/effort/image/
               │                    │                 gen-params/json-edits; explicit flags win)
               │                    │            ──► stderr "Resumed session <id> (<n> messages)" (D-44)
               │                    └─ messages ──► OnceOptions.history
               │                       images_path() ──► OnceOptions.images_dir  (D-53)
               ▼
          once(..) ──► OnceOutcome.delta ──► on SUCCESS ONLY: writer.append_messages(&delta)
                                              one batch, one fsync, one meta rewrite (D-43);
                                              a failure is `Warning: failed to save session: {e}`
```

Inside the bundle, `messages.jsonl` is the event store and `meta.json` is a derived index: the loader
takes the LAST system record first, then the tail the LAST compaction marker retained (with the summary
preamble woven in), while `usage` sums the WHOLE log, compacted-away rounds and markers included. The
writer is lazy — `SessionStore::create` touches no disk, so a session that never reaches a real turn
leaves nothing behind — and Go's eight `Set*` methods collapse into one `update_meta(|m| …)` because the
schema forces one rule for all of them (write only once the bundle exists).

Interoperability is the whole point of the slice, and it is tested from both sides: `tests/cmd/`
`session.rs` resumes checked-in bundles written by the real Go writer (tier A, hermetic), and
`scripts/go-session-roundtrip.sh` puts the Go binary back in the loop (tier B) — Rust extends a
Go-written bundle and Go re-reads it, then Go loads a bundle Rust created from scratch.

---

## 9. Error taxonomy

| crate | type | Display contract |
|---|---|---|
| core | `ProviderError` | `chat error: {0}`, `stream error: {0}`, `failed to list models: {0}`, `no response choices`, transparent `Permanent`, `interrupted`, `{0}` (Other) |
| core | `ToolError` | `unknown tool: {0}`, `interrupted`, `{0}` (Transport) |
| core | `UnknownProviderType` | `unknown provider type: {0} (supported: openai, anthropic, gemini, vertexai, openresponses, imagen, images)` |
| core | `McpFlagError` | `--mcp: empty server specification` (the only MCP error that aborts a run; feature-independent) |
| llm | `LlmError` | `StatusError` (`{method} "{url}": {status} {text} {body}`), `stream ended without any SSE events (server did not stream?)`, `received error while streaming: {0}`, `llm: malformed stream chunk: {0}`, `llm: malformed stream event: {0}`, `llm: malformed images response: {0}`, `llm: image stream ended without a completed image`, `RespFailure`, `llm: encode request: {0}`, `llm: authorize request: {0}`, transport/decode passthrough, `response headers not received within {d}` (header timeout, retried like transport), `llm: invalid model name {0:?}`, `interrupted` |
| tools | `SetError`, `SkillError`, `ShellError` | exact texts in CONTRACTS §4 |
| mcp | `McpError` | `server config must have either command or url`, `unsupported URL scheme: {0}`, `connect failed: {0}`, `list tools: {0}`, `connection timed out after {0}`, `{0}` (call: ServiceError text, `MRTR_UNSUPPORTED`, `TASK_UNSUPPORTED`) |
| chat | `ChatError` | the two `tool loop reached the --max-turns limit …` forms, `unknown agent {0:?}`, `interrupted`, transparent provider/child/io; `images::HOME_NOT_DEFINED` = `$HOME is not defined` |
| session | `SessionError` | `session {0} not found`, `cannot read session {id}: {source}`, `no session matches {0:?}`, `session id {0:?} is ambiguous: {1}`, `read session log: {0}`, `$HOME is not defined`, `{0}` (Io) — every one byte-equal to its chat/session.go line |
| iota | `CliError` = `ArgsError` \| `SetupError` \| `RunError` (`cmd/error.rs`: the invocation, the setup, the run), `ConfigError` | every root.go/config.go text (CONTRACTS §7.8) plus the surface's own: `NoAgent`, `UnknownAgent`, `ApiKeyRequired{env,provider}`, `ResumeIdRequired`, `ListTakesNoName`, `ConfigExists`, `NoHome`; `ConfigError::{Key, File}` carry a config coordinate and the file it was written in (X-15); `McpFlag(#[from] McpFlagError)`; `Session(#[from] SessionError)` as `#[error(transparent)]`, so each ported Go string reaches stderr unwrapped |

The listing no longer fetches anything, so Go's double-prefixed `failed to list models: failed to list models: <inner>` has no site left (X-13); the network model list survives only in the interactive `/model` picker.

---

## 10. Testing strategy

TEST_PLAN.md is normative. Principles: unit tests beside code (and ONLY there for `pub(crate)` seams — `tests/` is a separate crate, so no `#[path]` mount of a source file is ever used); integration tests in `tests/<area>/main.rs`, one binary per area; `wiremock` for HTTP; SSE transcripts as `const &str` copied from the Go tests; `tempfile`; fakes from `iota::testing`; shared `tests/common/` fixtures; every ported test keeps its Go name in snake_case with a `// Go: <file>:<line>` anchor; **no process-env mutation** (everything injected), so no `serial_test`; `#[tokio::test(flavor = "multi_thread")]` for contention tests; the end-to-end tests run the built binary (`CARGO_BIN_EXE_iota`) and set env on the child process only.

CI (`rust/ci.sh`, one package, one binary — since 2026-09-02/2026-09-01): `cargo fmt --check` · `scripts/check-deps.sh` (direct deps ⊆ `scripts/direct-deps.allow`, via `cargo metadata`) · `scripts/check-stubs.sh` (no `todo!()`, no stub header) · `cargo clippy --all-targets -- -D warnings` (pedantic via `[lints]`) · `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` · `cargo test` (which runs the layering gate of §1.2, `tests/layering.rs` — three greps here until 2026-09-15) under `IOTA_TMUX_REQUIRED=1 IOTA_SANDBOX_REQUIRED=1` (since 2026-09-15: the single L4 tmux execution rides that one invocation, and a missing tmux / bash / mock port / bubblewrap is a red test naming it, not a `SKIP:` that passes; a plain `cargo test` without the variables still skips) · `cargo build --release` + `scripts/size.sh` writing the one size row into `target/size.md` (a CI artifact).

The session slice adds one testing rule to the list above: **no test hardcodes a fixture session id.** The Go-written bundles under `tests/fixtures/sessions/` carry random ids, and every id, prefix, count and expected string is read from their `manifest.json` (whose `expect` block was measured by loading each bundle back through the real Go loader). Regenerating the corpus therefore never invalidates a test.

---

## 11. Footprint plan

**One binary (decision of 2026-09-01: "跟 go 版本完全对齐，直接 build 出一个统一的二进制").** The phase-1 footprint design — cargo features that compiled providers, toolsets, MCP, the TLS backend, the TUI, token accounting and syntax highlighting in or out, a `tui-portable` fallback and a `tls-ring` backend — was removed in full. Like the Go binary there is exactly ONE shipped artifact carrying everything (`cargo build --release`), and the only cargo feature left anywhere is `testing` (the shared test fakes; it never affects the binary). Gone with it: every `#[cfg(feature = …)]` in the sources, the compiled-out runtime stubs (`toolset … not compiled into this build`, `provider type … not compiled into this build`, `Warning: mcp support is not compiled into this build`, `CliError::InteractiveUnavailable`), the three `build.rs` TLS guards, `iota::tls`, the per-crate `init_tls()` test helpers, the reduced-feature CI legs and the size matrix. The entry model is Go's exactly (§7).

- Release profile as §1.4. TLS: rustls with aws-lc-rs and the platform verifier (`reqwest/rustls`, rmcp `reqwest`), always — no ring backend, no `rustls` direct dependency, no provider installation.
- Direct-dependency allowlist in `scripts/direct-deps.allow`, enforced by `scripts/check-deps.sh` over `cargo metadata` (deliberately not cargo-deny's `[bans]`, which reads over the whole transitive graph and cannot say "may be named in Cargo.toml"); every new direct dependency needs a one-line justification in `docs/ARCHITECTURE.md`. **The transitive graph has its own gate since 2026-09-15: `deny.toml` at the root — licenses (exactly the set in today's graph, an unused entry is itself an error), advisories, duplicate versions (`deny`, the seven known transitive pairs skipped by range) and sources, over the five shipping targets — run by its own `cargo deny` job in `ci.yml` (cargo-deny-action) and by `./ci.sh` when `cargo-deny` is on PATH (it is not in the pinned toolchain, so that leg prints SKIP without it). Its first run caught rustls 0.23.43 (RUSTSEC-2026-0285, fixed by the lockfile bump to 0.23.45) and the retired bincode 1.x under syntect (RUSTSEC-2025-0141, ignored with its reason in the file).** The one entry phase 2 slice 1 added: **`iota-session` — the fs-only session bundle store; keeps `sha2`, `rand` and on-disk format code out of `iota-chat`, which never depends on it.** (It brings no new third-party crate: `sha2`, `rand` and `jiff` were already in the workspace table. Its `[dependencies]` are `iota-core`, `serde`, `serde_json`, `sha2`, `rand`, `jiff`, `thiserror` — no async runtime and no HTTP client, and no `tokio`/`tokio-util` entry in either dependency table: `iota-core` re-exports `CancellationToken` (`provider.rs:4`, `lib.rs`) so a test fake can implement `iota_core::Provider` by naming `iota_core::CancellationToken`.) Dropped: `dirs`, `which`, `url`, `unicode-width`, `path-clean`, `indexmap`, `hex`, `filetime`, `async-trait`, `serial_test`, `temp-env`, `mime_guess`, `shell-words`, and (2026-09-01) `rustls`, which only ever existed to install the ring provider.
- `tracing` compiled with `release_max_level_debug` (rmcp/process-wrap/hyper emit tracing; the two `debug!`s in `shell/exec.rs` are the crate's own). It was `release_max_level_off` until 2026-09-15, which is why the one `tracing::warn!` meant for the user never reached one (DIVERGENCES X-29): `tracing` is the developer's channel, heard only through `IOTA_LOG=<path>` (`src/app/diag.rs`), and user-facing warnings go through `Streams::warning` or the transcript.
- Measured (stripped, `z`, aarch64): the unified binary is the one row of `docs/SIZE.md`, regenerated by `ci.sh` on every run next to the Go binary for comparison. Breakdown for orientation (measured 2026-09-01, just before the legs were removed): the TUI itself (ratatui + crossterm + the `ui` and `repl` modules) ≈ 0.4 MiB; the heavy parts are two embedded tables the Go binary carries as well — tiktoken-rs' bundled `o200k_base` ranks ≈ 3.6 MiB and syntect + two-face's syntax/theme dumps ≈ 1.1 MiB; then aws-lc-rs ≈ 3 MiB, hyper/reqwest ≈ 1.2 MiB, regex ≈ 1 MiB, rmcp ≈ 0.8 MiB, tokio ≈ 0.6 MiB. That breakdown has one correction since: on 2026-09-11 the ranks became `tiktoken`'s zstd-compressed table (≈ 0.78 MiB embedded, ≈ 0.87 MiB of binary), which took the unified binary from 10.25 MiB to 7.56 MiB and left syntect + two-face as the heaviest embedded table. Keeping `tracing` compiled to `debug` in release and adding `tracing-subscriber` for `IOTA_LOG` (2026-09-15, X-29) cost 7.59 → 7.72 MiB: the dependencies' event metadata and format strings, which `release_max_level_off` used to strip.

- **TUI slice direct-dependency additions (phase 3, WP40; TUI_CONTRACTS §1.1/§12):**
  - `markdown` + `text::{width,ansi}` (then `iota-markdown`) — the pure streaming markdown→ANSI renderer + THE grapheme width ruler; zero terminal deps so every layer shares one ruler.
  - `ui` (then `iota-tui`) — the ONLY ratatui/crossterm importer (`tests/layering.rs`); owns the inline frame engine and implements `ui::facade::Ui`.
  - `repl` (then `iota-repl`) — the imperative interactive chat loop over the facade; keeps ratatui out of the loop and the loop out of the UI.
  - `ratatui` (=0.30.2) — spike-validated `Viewport::Inline` + `insert_before` scrollback engine; feature `unstable-rendered-line-info` for the W6 `line_count` law, `scrolling-regions` always on (wart W9 is a per-emulator verification item in `docs/TUI-VERIFY.md` §2, not a build variant).
  - `crossterm` (=0.29.0) — ratatui's default backend pairing (wart W8: ONE crossterm; never `crossterm_0_28`); raw mode, bracketed paste, events, DSR.
  - `unicode-width` (=0.2.2) + `unicode-segmentation` — the two halves of the grapheme ruler (uniseg-parity widths incl. the VS16 rule).
  - `vt100` (=0.16.2, dev-only) — L2b byte-stream terminal-semantics assertions (scroll-region proof, exact-width characterizations) without a real terminal.
  - `syntect` (=5.3.0, WP55) — chroma's replacement behind the `CodeHighlighter` seam, shared by the fenced-code renderer and the chat diff renderer. Built `default-features = false` with `regex-fancy`, so the regex engine is pure Rust (`fancy-regex`) and no `onig`/C toolchain enters the build.
  - `tiktoken` (=4.1.2, `default-features = false`, feature `vocab-o200k_base`; WP53) — the `o200k_base` token counter behind the live context meter, `/compact` and the auto-compaction offer. Chosen for the property Go bought with `tiktoken-go` + `tiktoken-go-loader`: it EMBEDS the rank table, so counting a conversation is never a network call and never a runtime download. It embeds it zstd-compressed and behind a feature PER VOCABULARY, which is why it replaced `tiktoken-rs` =0.12.0 on 2026-09-11: the same table costs ≈ 0.78 MiB instead of the 3.6 MiB `include_str!`ed `.tiktoken` asset, for byte-identical token id vectors (verified over five corpora, 1823 windows and sixteen adversarial cases; the `tiktoken-go` v0.1.8 parity goldens in `src/repl/tokens.rs` are the standing gate). `default-features = false` is load-bearing — the default `vocabs-all` would embed twelve vocabularies (+2.92 MiB) for the one we ask for. The lookup is memoized behind a `OnceLock` on first use (the crate owns the `&'static CoreBpe`), and a table that fails to load degrades to Go's `len(text)/4` byte heuristic rather than failing the chat. It brings `ruzstd` and `twox-hash` transitively (`regex` and `rustc-hash` are already in the graph), and it drops the THIRD copy of `fancy-regex` (0.17.0) `tiktoken-rs` pulled in beside syntect's; `anyhow`, `base64`, `bstr`, `bit-set` and `lazy_static` stay, each of them still reached another way.
  - `two-face` (=0.5.2, WP55) — bat's syntax and theme dumps for syntect: the language coverage chroma had out of the box, plus the `MonokaiExtended` / `Github` themes standing in for chroma's `monokai` / `github`.

- **T3 full-parity direct-dependency additions (WP60; T3_CONTRACTS §9/§11, T3_DESIGN D3/D6/D7):**
  - `comrak` (=0.54.0, `default-features = false`, WP65) — `/export`'s Markdown → HTML: CommonMark + GFM in safe mode (raw HTML → `<!-- raw HTML omitted -->`, goldmark's text) with the `SyntaxHighlighterAdapter` seam the code-fence renderer needs. Default features are off so neither the CLI, `bon`, nor comrak's own `syntect-onig` feature enters the build — the latter would bundle syntect's default syntax/theme dumps and onig beside two-face's; our adapter reuses `markdown::highlight`'s syntax set. syntect gains its `html` feature (`ClassedHTMLGenerator`) for the same reason. New transitive crates: `caseless`, `jetscii`, `typed-arena`.
  - `image` (=0.25.10, `default-features = false`, features `png`/`jpeg`/`gif`/`webp`, WP63) — the half-block rasteriser's decoders: exactly the four formats Go's `imgterm` registers, all pure Rust, no rayon. Only `src/imgterm.rs` may name the crate (`tests/layering.rs`); everything else sees `imgterm::Frame`. Encoders are reachable only from tests. New transitive crates: `png`, `gif`, `weezl`, `color_quant`, `zune-jpeg`, `zune-core`, `image-webp`, `moxcms`, `byteorder-lite`, `num-traits`.
  - `memchr` (2, 2026-09-14) — `memmem::Finder`, the substring search that works on `&[u8]`. `edit_file` counts, checks uniqueness and replaces on the file's BYTES, never on a decoded `String`, so a source file in Latin-1, Shift-JIS or GBK comes back off disk byte for byte (DIVERGENCES R-01). Already in `Cargo.lock` (2.8.3) under `regex`, `globset` and `ignore` — no new transitive crate.
  - `http` (=1, WP66) — `/debug`'s recorded response is rebuilt through `http::Response` → `reqwest::Response` (`From<http::Response<T>>`); reqwest re-exports `header`/`Method`/`StatusCode`/`Version`/`Url` but not the crate, so naming `http::Response` needs the direct dependency. Already in `Cargo.lock` (1.5.0) — no new transitive crate.
  - `tracing-subscriber` (0.3, `default-features = false`, features `fmt` + `std`; 2026-09-15, roadmap §3 #7) — the consumer behind `IOTA_LOG=<path>` (`src/app/diag.rs`): a file subscriber with a `Targets` filter (this crate at DEBUG, everything else at INFO). It is tracing's own consumer and the one every tracing-emitting dependency here is written against; a hand-rolled `Subscriber` would have re-implemented span storage and field formatting for the sake of two crates. No `ansi` (a log file never wants SGR), no `env-filter` (the level is fixed; its `matchers`/`regex-automata` pull stays out), no `tracing-log`. New transitive crates: `sharded-slab`, `thread_local`.

---

## 12. Work packages

**Phase 2 · slice 1** shipped as six file-disjoint packages on top of the sixteen below: WP-S0 `message-fields` (the two `Message` fields both consumers need) and WP-S3 `go-fixtures` (the Go fixture generator and the checked-in corpus) start in parallel; WP-S1 `session-crate` and WP-S2 `chat-delta` follow WP-S0; WP-S4 `cli-resume` integrates all three; WP-S5 `docs-ci` closes the docs and CI. `crates/iota/src/lib.rs` was the one deliberately shared file — WP-S2 carried two mechanical keep-green lines there so the workspace stayed green at every DAG point, and WP-S4 rebased over them. Per-package landing status is `docs/EVALUATION.md` §9.

**Phase 3 · the TUI slice** shipped as WP40–WP59, and **phase 4 · T3 (full feature parity)** as
WP60–WP69: a scaffold package (WP60) that pre-touched every shared seam and left `// WP6x-STUB`
headers, seven feature packages fanning out file-disjointly (`mathtext` parser+inline / layout+2D,
`imgterm` + widget + partial frames, `/edit`+`/redo`+Picker+image edit endpoints, `/export`,
`/debug`+`RequestLog`, `host`+progress+notify+upload progress, `/skills`+completeness) and one
verify package (WP69: the four new tmux scenarios, `./ci.sh`, the completeness audit and these
docs). Per-package landing status is `docs/EVALUATION.md` §13.

See WORK_PACKAGES.md (16 packages). Critical path: WP00 scaffold (≈4.5k lines: workspace + manifests + every module with doc-commented contract types, serde derives and `todo!()` bodies under a standard stub-file allow header; every shared test fixture; `compose_send_history` real; ≈3 days with WP01) → seven packages fan out at once (core bodies, wire, tool framework, shell, code, mcp, chat loop, config/resolve) → four dialect packages after wire (responses needs only wire, thanks to `wire/models.rs`) → images after google → WP15 wiring + end-to-end last.

---

## 13. Risks and mitigations

1. **rmcp lifecycle drift** (handshake version, child kill on drop, header rules) — isolated in `mcp/transport.rs`; `manager_connect_timeout` lands first as the canary; in-process duplex tests exercise the real handshake; divergences recorded.
2. **Byte parity of ~150 strings** — every Display text is a `thiserror` literal or a `const`; per-crate `strings` tests pin them; `go_quote`/`go_float`/`go_duration` helpers have their own tables.
3. **SSE / think-splitter edge semantics** — hand-rolled, transcript fixtures verbatim, byte-at-a-time property test.
4. **Sandbox availability in CI** — probe-and-skip with a visible `SKIP:` line; macOS runner has sandbox-exec; Ubuntu runner installs bubblewrap.
5. **YAML 1.1 bools** — one `yaml11` module used by config and the disable rule; tests pin `agent: yes`, `ask: False`, `network: on`.
6. **Cancellation completeness** — every long primitive selects on the token; `spawn_blocking` walks are capped; an end-to-end test sends SIGINT to a wiremock-stalled run and checks exit 130 + JSON `"error": "interrupted"`.
7. **Binary size** — one release build measured into `docs/SIZE.md` on every CI run, next to the Go binary; no feature pruning by decision (§11).
8. *(retired 2026-09-01 with the ring backend — there is one TLS provider and nothing to unify.)*
9. **Determinism vs Go completion order** (MCP segments) — config-order merge; ported tests hold under both.
10. **Delegate cycle compile-time recursion** — boxed futures at both `dyn` edges; verified by the WP00 stub build.
11. **Lock discipline** — `Manager`'s `RwLock` is never held across `.await` (peer cloned, guard dropped); `CodeSet.reads` mutex never held across I/O; multi-thread stress tests.
12. **Hidden shared files in fan-out** — the scaffold owns `iota-llm/src/lib.rs` factory arms, `iota-llm/src/wire/mod.rs` (`is_zero`), `iota-tools/src/lib.rs` set table, `wire/google.rs` predict half with feature-gated stubs, and every shared `tests/common` fixture (`iota-llm`, `iota-tools`, `iota`); `wire/models.rs` belongs to WP02 so WP03 and WP04 never share a file; dialect/toolset packages only fill their own files.
13. **Compile-time traps caught before fan-out** — `RawValue` has no `PartialEq` (→ `Raw` newtype), the orphan rule forbade the blanket `Tunable` impl across crates (→ macro; a plain blanket impl since the 2026-09-02 merge), `gen` is a 2024 keyword (→ `params`), `pub(crate)` seams cannot be driven from `tests/` (→ unit tests), a `todo!()` module cannot pass `-D warnings` without the standard allow header. WP00's acceptance greps for each.

---

## 14. Rejected review findings

| finding | decision | reason |
|---|---|---|
| Replace the hand-rolled allowlist with `cargo deny check bans` and keep the file name `deny.toml` | **partially rejected** — the file is renamed to `scripts/direct-deps.allow` + `scripts/check-deps.sh` (so nothing masquerades as cargo-deny config), the `cargo tree` gate is replaced by the `--prefix none \| grep` form as proposed | cargo-deny's `[bans] allow/deny` lists apply to the whole transitive graph, not to direct dependencies, and `cargo-deny` is not part of the pinned 1.98.0 toolchain; a `cargo metadata` script expresses the intended rule exactly and needs no extra install in CI. *(2026-09-15: a `deny.toml` now exists beside it for what the script never claimed — licenses, advisories, duplicate versions, sources over the transitive graph — as its own CI job; the direct allowlist stays with the script, §11.)* |
| Move `ProviderCore`/`HasCore` + the blanket `Tunable` impls into `iota-core` | **rejected then** (the per-provider macro was adopted); **moot since 2026-09-02** — in one crate the blanket impl is legal and is what ships (`provider/common.rs`); the test fakes' own `Tunable` impls coexist with it because they are local types | a blanket impl across a crate boundary was an orphan-rule violation; inside one crate it is the idiomatic form |
| Add `#[error("{0}")] Mcp(String)` to `CliError` | **replaced** by a typed `McpFlag(#[from] iota_core::mcp::McpFlagError)` | the same finding set also required `assemble.rs` to compile without the `mcp` feature; moving the config types to iota-core solves both, and a typed variant keeps the `thiserror` taxonomy honest (no stringly errors) |
| Flatten rmcp `CallToolResponse::InputRequired(r)` to the same `(joined text, is_error)` as `Complete` | **rejected as stated**; `call_tool_once` IS adopted | `InputRequiredResult` (rmcp model/mrtr.rs:229-246) carries no `content`/`is_error` — only `result_type`, `input_requests`, `request_state`, `_meta` — so there is nothing to flatten; `InputRequired`/`Task` map to explicit error texts instead (D-33) |
| Build `grep` regexes with `RegexBuilder::unicode(false)` to make `\w`/`\d`/`\s`/`\b` ASCII-only like RE2 | **rejected**; documented in D-18 | on `regex::Regex` (str-based) disabling Unicode makes any pattern that could match non-UTF-8 — `.`, negated classes — fail to COMPILE, a far larger divergence than Unicode-aware word classes |
| Put the `install_tls_provider()` ONCE helper in `iota_core::testing` | **rejected**; per-crate `tests/common::init_tls()` instead *(moot since 2026-09-01: no ring backend, no helper)* | iota-core has no `rustls` dependency and must stay I/O-free; `install_default()` is already idempotent (returns `Err` when a provider exists), so each crate calls its own one-liner |
| Delete `design/DESIGN.md` | **softened**: kept with a `SUPERSEDED — NOT binding` banner, its manifest block removed | it documents which alternatives were considered and rejected (ARCHITECTURE §0); the banner and the removal of every pinnable manifest line remove the confusion risk |
| Port the sorted-key argument digest so `ask_approval`'s `detail` matches Go | **not adopted** (finding proposed pinning `header_summary(..).unwrap_or_default()`, which is what CONTRACTS now states) | headless hosts never approve, so the detail is unobservable in production; `TestForwardedApprovalCarriesTheCallDetail` stays unported (D-12) |
