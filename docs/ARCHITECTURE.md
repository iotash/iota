# iota — BINDING architecture

Status: **binding** for the tree as it stands. `DIVERGENCES.md` records every user-visible difference from the
Go original and the decision behind each; `tests/layering.rs` pins the module graph this document describes
(§1.2, §2). Where the document and the tests disagree, the tests are right and the document is what gets fixed.

Every user-visible string below is pinned by a test: change one only by a decision recorded in `DIVERGENCES.md`.

---

## 1. Layout — ONE package (decision of 2026-09-02)

### 1.1 One package, one module tree

`Cargo.toml` is a single package, `iota`: a library (`src/lib.rs`) holding every module and a
thin binary (`src/main.rs`, ~40 lines). One build, like the Go original's, and it
replaces the ten-crate workspace of phases 1–3 (`docs/history/MERGE-PLAN.md`, executed 2026-09-02, records the
full old→new map). No workspace table, no per-area manifests, no `check-deps` plumbing beyond one
`[dependencies]` table; the only cargo feature is `testing` (the shared fakes in `src/testing/`, turned
on for tests by the self-dev-dependency `iota = { path = ".", features = ["testing"] }`).

Module names are the tree's own: `headless` is the `-m` loop, `repl` the interactive one, `session` the
store. Visibility is Rust-idiomatic: everything is `pub(crate)` unless `main.rs`, `tests/` or `examples/`
genuinely use it.

The module tree — one row per module, what it is for and what it may name — is §2, under the layer order that
`tests/layering.rs` pins.

Tests: `tests/<area>/main.rs` — twelve integration binaries (`cmd`, `headless`, `markdown`, `mathtext`, `mcp`,
`nocolor`, `provider`, `repl`, `session`, `shell_env`, `tool`, `ui_tmux`) plus `tests/layering.rs`, the
module-graph gate (§1.2). The `ui` area has no integration binary because every one of its former files
drove crate-private internals and now lives in-file as `#[cfg(test)] mod tests` under `src/ui/**`.
They share `tests/common/` fixtures and the bundles under `tests/fixtures/` (the session bundles there were
written by the Go original, so resuming one IS the interoperability test).

### 1.2 Layering invariants (former crate boundaries, now `tests/layering.rs`)

The crate boundaries that carried a design rule are one test binary, `tests/layering.rs` (until
2026-09-15 three greps in `ci.sh`):

- **the module graph points down.** The test scans every `crate::<module>` path in `src/` (`#[cfg(test)]`
  modules blanked, comments cut, `src/testing/` not scanned) and asserts each edge lands in a LOWER row of
  the declared order — the `LAYERS` table quoted in §2. Siblings in one row never name each other; product
  code never names the fakes (`testing`). An upward edge the tree still carried would sit in the test's
  `KNOWN_UPWARD` table with the PR that retires it — a new upward edge is red, and so is a row whose edge
  is gone; the table has been empty since phase 5;
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

- **One build**: one `cargo build --release`, one artifact, no member list to keep in step with
  a feature story that no longer exists (§11: one binary since 2026-09-01).
- **The crate-boundary hacks died with the boundaries**: the `impl_tunable_via_core!` macro (orphan rule)
  is a plain blanket `impl<T: HasCore + Send + Sync> Tunable for T`; `PrefixOf`, `mcp::ServerConfig` and
  `app::env::Env` live where they are used; no `#[path]`-mounted test remains (those became unit tests).
- **Link cost**: ~10 test binaries instead of ~70 each linking the whole library.
- What the split bought — dependency isolation, testability — is unchanged: every dependency is still
  in one table (`scripts/direct-deps.allow`), and every test still uses fakes, wiremock, temp dirs and
  in-process duplex servers; the `pub` surface is curated instead of implied by crate edges.

### 1.4 Manifest

`Cargo.toml` (the single `[package]`). Highlights:

- edition 2024, `rust-toolchain.toml` = `1.98.0` (+ rustfmt, clippy); MSRV of every dep verified (`rmcp` 1.88, `globset`/`ignore` 1.88, `reqwest` 1.85, `clap` 1.85, `sha2` 1.85).
- `serde_json = { features = ["raw_value"] }` — **no** `preserve_order`: `serde_json::Map` is BTreeMap-backed, so keys serialize sorted, the order the session records and the JSON report are pinned to; replay payloads are `Box<RawValue>` and byte-verbatim regardless.
- `reqwest = { default-features = false, features = ["json", "stream", "http2", "system-proxy", "rustls"] }` (no `multipart`: edits are OUT); ONE TLS backend, rustls with aws-lc-rs and the platform verifier — no ring alternative, no direct `rustls` dependency, no provider-installation step.
- `rmcp = { default-features = false, features = ["client", "transport-child-process", "transport-streamable-http-client-reqwest", "reqwest"] }` (rmcp's reqwest-with-rustls pairing) unconditionally; the dev-dependency adds `"server"` + `"transport-async-rw"` for the in-process echo server.
- Cargo features: **none** that affect the binary. The only `[features]` entry is `testing` (the shared test fakes), enabled for tests by the self-dev-dependency. ratatui's `scrolling-regions` is on, always.
- `jiff = { default-features = false, features = ["std", "tz-system", "tzdb-zoneinfo"] }` — `tz-system` so image file names carry LOCAL time.
- `tokio` features: `rt-multi-thread, macros, sync, time, process, io-util, signal, fs, net` (`net` = `tokio::net::unix::pipe`); the dev-dependency adds `test-util` (paused clock for the retry tests).
- Process supervision is the one per-platform dependency pair, both in `[target.'cfg(<os>)'.dependencies]` and both reached only from `src/shell/exec.rs`: `nix = { default-features = false, features = ["signal", "process", "fs"] }` on Unix (`killpg` + `Signal` + `Errno::ESRCH`), and on Windows `process-wrap = { default-features = false, features = ["tokio1", "job-object", "creation-flags", "tracing"] }` for the Job Object that `TerminateJobObject` kills as one, plus `windows = { features = ["Win32_System_Threading"] }` for the single constant `CREATE_NO_WINDOW` its `CreationFlags` wrapper takes. `windows` is a CARET range on purpose: it must resolve to the same copy of the crate process-wrap builds against or the flag type stops unifying.
- Release profile: `opt-level = "z"`, `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`, `strip = true`.
- Lints (`[lints]`): `unsafe_code = "forbid"`, `missing_docs = "warn"`, `clippy::all` + `clippy::pedantic` = warn (CI runs `-D warnings`), `unwrap_used`/`expect_used`/`panic` = deny (allowed under `cfg(test)`), justified allows: `module_name_repetitions`, `missing_errors_doc`, `missing_panics_doc`, `too_many_lines`, `struct_excessive_bools`, `must_use_candidate`.
- `#![forbid(unsafe_code)]` in `lib.rs` and `main.rs`; no `unsafe` module exists at all (`std::io::pipe` covers pipes, `nix` signals, `std::os::unix::process::CommandExt::process_group` process groups, and `process-wrap` the Win32 Job Object calls — that last one is the whole reason the crate is a dependency rather than forty lines of our own `windows-sys`).

---

## 2. Module tree and layering

One library crate, one tree. The rows of the table below follow the layer order `tests/layering.rs` pins,
bottom first: a module may name any module in a LOWER row, none in its own row or above, and the test
fails the build on a new upward edge (§1.2). The declaration is quoted from the test rather than restated,
so this section cannot drift from it:

```rust
const LAYERS: &[&[&str]] = &[
    &["app"],
    &["text", "sync", "imgterm"],
    &["llm"],
    &["provider"],
    &["shell", "agents"],
    &["tool"],
    &["mcp", "session", "mathtext"],
    &["config", "markdown", "headless"],
    &["ui"],
    &["host"],
    &["repl"],
    &["cmd"],
];
```

`KNOWN_UPWARD`, the test's table of tolerated upward edges, is empty: the tree carries none. `testing` is
not a layer — the fakes reach everywhere by design and product code never names them. Two placements the
order settles that read against intuition: `config` sits ABOVE `tool` and `session` because it is the
user's declaration in their vocabulary (`SET_NAMES`, `DeferMode`, the layered parameters), not something
they consume; `shell` and `agents` sit BELOW `tool` because `tool/builtins/shell.rs` is the policy over
the `shell/` mechanism and `tool/builtins/agent.rs` consumes the `agents/` overlay.

| module (under `src/`) | what it is | may name |
|---|---|---|
| `lib.rs`, `main.rs` | the module list with `BoxFuture`/`BoxError`; the thin binary — parse, `Env::process`, the runtime, signals, exit codes (`anyhow` only here) | everything |
| `app/{mod,env,paths,fs,color,diag}.rs` | what the process learns ONCE at its edge and injects everywhere: program identity and `HostDirs`; the one environment seam `Env` (variables + dirs; `process()` in `main`, `fixed()` in tests) with `${var}` expansion; the lexical path helpers; the one atomic file writer (`fs::write_atomic`: sibling temp file, sync, rename — the config rewrite and the MCP token store both go through it); the color decision (`NO_COLOR`, `TERM=dumb`, a piped stdout); the `IOTA_LOG` diagnostics tap | nothing in `src/` |
| `text/{mod,width,ansi}.rs` | the `%q`/`%v`/`Duration` formatters whose bytes the transcripts are pinned to; THE grapheme width ruler; the escape-aware string tools (strip, wrap, clip, SGR carry) — one ruler for `markdown`, `ui` and `repl` | `app` |
| `sync.rs` | the shared lock helpers | `app` |
| `imgterm.rs` | the half-block image rasteriser; the ONLY module that names the `image` crate — everything else sees `imgterm::Frame` | `app` |
| `llm/{mod,client,sse,error,models,chatcomp,responses,anthropic,google,images,reqlog,progress,multipart}.rs` | the wire layer: the HTTP/SSE client (retries, the jitter seam, the header timeout, cancellation), the SSE reader, the error taxonomy, the OpenAI-shaped model listing, one module per dialect, the `/debug` request log the client records into, the upload-progress reporter, the multipart writer | `text` and below |
| `provider/{mod,model,usage,sink,error,common,think,usage_conv,image_util}.rs`, `provider/{openai,openresponses,anthropic,google,imagen,images}.rs` | the `Provider`/`ToolProvider` traits with the capability traits and the per-call results; the data model (`Message`, `ToolDef`, `ToolCall`, `Usage`); the stream sink and the reasoning gate; `ProviderCore` with the blanket `Tunable` impls, the think-tag splitter, the usage converters; the seven adapters over `llm` | `llm` and below |
| `shell/{mod,exec,interp,jobs}.rs`, `shell/sandbox/{mod,darwin,linux,other}.rs` | process execution, the MECHANISM under the `shell` tool (its pins are §5): the child in its process group, the capped output pipe, the interpreter choice, the background-job registry, the Seatbelt/bwrap sandboxes | `provider` and below |
| `agents/{mod,skills}.rs` | the AGENTS.md overlay (root discovery, the chain, `compose_send_history`) and skills discovery | `provider` and below |
| `tool/{mod,context,approval,fmt,error,args,sets,dispatch,yaml11}.rs`, `tool/defer/{mod,mode}.rs`, `tool/builtins/{mod,shell,agent,ask}.rs`, `tool/builtins/code/{mod,tools,walk,udiff}.rs` | the tool contract (`Tool`, `Dispatcher`, `ToolEnv`, `PrefixOf`; the run context `RunCtx`/`TurnBudget`/`ArtifactSlot` in `context`; the answer to a gated call in `approval`; the call-header formatters in `fmt`), the argument readers, the set table, the two dispatchers (the registry and the live union), deferred groups and their four modes, YAML 1.1 bools — and in `builtins/` the four built-in sets: `shell` (the policy over `shell/`), `code` (the six file tools, the gitignore walk, the unified diff), `skills` (`load_skill`), `ask` (present only with an interactor) | `shell`, `agents` and below |
| `mcp/{mod,config,manager,transport,auth,error}.rs` | the rmcp manager: server config and the `--mcp` flag, the transports, the 30 s connect fan-out with its config-order merge, `mcp__<segment>__<tool>` naming, the live tool view, routing and close; `auth` the OAuth 2.1 side — the file token store, the login/logout flows, the runtime manager the HTTP transport is wrapped in for an `auth: oauth` server | `tool` and below |
| `session/{mod,meta,params,record,rawcodec,id,store,writer,loader,tuning,error}.rs` | the on-disk session bundle store: meta, the `messages.jsonl` records, the raw-content blob codec, ids, the store, writer and loader, the tuning replay — it never reads the process environment (§1.2); its root is an injected `HostDirs` | `tool` and below |
| `mathtext/{mod,delim,parse,symbols,macros,inline,pict,layout}.rs` | the LaTeX engine: the inline Unicode approximation, the 2D layout, the delimiter scanners — a leaf over `text` that the markdown hooks call directly | `text` and below |
| `config/{mod,provider,model,agent,params,strict,window,edit}.rs` | the YAML config model and its merge; `Config::provider` → `Endpoint` with the ONE key precedence; the key audit that refuses a misplaced key by its coordinate; the layered parameters; `parse_window_size`; the machine-managed `mcp_servers:` block (`edit`: locate the top-level block by line, rewrite that byte range alone, atomic write) | `mcp`, `session`, `tool` and below |
| `markdown/{mod,inline,link,style,sink,preview,highlight,html}.rs`, `markdown/blocks/{mod,code,table,list,quote,math}.rs` | the pure streaming markdown→ANSI renderer (`Writer` over one `Block` value, the five buffering blocks in `blocks/`), inline styling, the `Sink` seam and the `PreviewHandle` contract `ui` implements, the `CodeHighlighter` seam with its syntect impl, and `/export`'s HTML renderer | `mathtext`, `text` and below |
| `headless/{mod,once,run,batch,report,images,error}.rs` | the `-m` loop: `once`, `run_once` and `execute_with_tools` over a `TurnParams`, the parallel batches, the JSON/text report, image saving — what separates it from `repl` is that there is no terminal | `mcp`, `session`, `tool` and below |
| `ui/{mod,facade,testutil}.rs`, `ui/runtime/{mod,handle,msgs,event_loop,term,osc,oneshot}.rs`, `ui/render/{mod,region,frame,spans,theme,sink,debug}.rs`, `ui/input/{mod,editor,composer,keys,paste,suggest}.rs`, `ui/surface/{mod,tabbed,panels,search,field}.rs` | the inline terminal engine, the ONLY module that names `ratatui`/`crossterm`: the `Ui` facade `repl` talks to; `runtime/` the loop thread, its mailbox, the terminal writer, the OSC probes, the one-shot pre-REPL surface; `render/` the staging region, the frame, the ANSI→span parser (the `NO_COLOR` gate), the theme, the metered previews; `input/` the shared `Editor`, the composer, the key ladder, paste, completion; `surface/` the tabbed panels, the pickers, search, the one-line field | `markdown`, `text` and below |
| `host/{mod,ansi,cmux,background}.rs` | host integration: the presenter's per-capability fan-out (progress, attention ping, exit clean-up), the ANSI host (OSC 9 / 9;4 through the facade), the cmux host, the background probe | `ui` and below |
| `repl/{mod,run,state,liveparams,catalog,editpicker,errors,title,systemtab}.rs`, `repl/turn/{mod,tools,retry,phases,steer,interrupt,approval,interact}.rs`, `repl/render/{mod,transcript,group,uisink,styles,diff,banner,replay,mcpreport}.rs`, `repl/context/{mod,meter,tokens}.rs`, `repl/commands/…` | the interactive loop over the facade: `run.rs` the loop, `state.rs` its state as `Conversation`/`SessionSlot`/`UiHandles`, `turn/` one turn (`TurnEngine` with the turn-level retry, the tool walk, steering, interrupts, approvals), `render/` what it draws, `context/` the token accounting, `catalog.rs` the `/model` listing, `liveparams.rs` the live half of the layered parameters, `commands/` the slash commands (`mcp.rs`: the `/mcp` panel and the OAuth round trip through `McpHooks::manager`) | everything below |
| `cmd/{mod,args,error,resolve,assemble,tuning,list,config_cmd,mcp_cmd,io,signals}.rs`, `cmd/interactive/{mod,picker,title}.rs` | the command: the clap verb set, the three-stage error taxonomy, pure run resolution, MCP/dispatcher assembly, the tuning warnings, the listings, `iota config`, `iota mcp`, `Streams` (the process's ONE stderr writer), signals, and the interactive branch with the `iota resume` picker and the title stack | everything |
| `testing/{mod,provider,dispatch,scripted}.rs` | the shared fakes behind the `testing` feature — `FakeProvider`, the fake dispatchers, the recording sink, `ScriptedUi` — not a layer | — |

---

## 3. Core data model (`provider::model`, `provider::usage`)

The shape decisions:

- `Role { System, User, Assistant, Tool }` serialised as the lowercase strings.
- `Attachment { filename, mime_type, data: Vec<u8> }`; `ToolDef { name, description, input_schema: Option<JsonObject>, deferred }`; `ToolCall { id, name, arguments: JsonObject }`; `JsonObject = serde_json::Map<String, Value>` (sorted keys).
- `Raw(Box<RawValue>)` is the one spelling of a verbatim JSON payload: `serde_json::value::RawValue` has no `PartialEq` (verified in serde_json-1.0.151), so the newtype implements it as byte-equality of the JSON text and `#[serde(transparent)]` keeps the wire shape and the null-as-absent rule for `Option<Raw>`. `RawContent` is a typed enum per dialect: `OpenAi(Raw)`, `Anthropic(Vec<Raw>)`, `OpenResponses(Vec<Raw>)`, `Google(Raw)`. Each dialect trusts only its own variant and reconstructs otherwise. This is what lets `Message`, `RoundResult` and `GPart` derive `PartialEq` and the history-shape tests use `assert_eq!`.
- `Message { role, content, reasoning, attachments, tool_calls, tool_call_id, tool_call_name, is_error, raw_content, tools }`. `interrupted` and `usage` are dropped (session-only; no dialect reads them — DIVERGENCES D-11).
- `Usage { input, output, cache_read, cache_write, total: u64 }`, its derived figures pinned by tests.
- Tool results: `ToolOutput { text, is_error }` (model-facing) vs `ToolError` (hard error rendered `Error calling tool: {e}`).
- Run report structs live in `headless::report` with struct order = JSON key order; `serde_json::to_writer_pretty` + `"\n"` is the pinned byte shape (two-space indent, no HTML escaping).

---

## 4. Provider abstraction

### 4.1 Results, not getters
Every per-call product is a field of `ChatResult { text, usage, images }` or `RoundResult { content, reasoning, tool_calls, usage, raw_content, images }` — no getter to read at the right moment, no `begin_call()`. `UsageReporter`, `RawContentProvider`, `ImageOutputProvider` do not exist as traits; the capability-surface tests port as `as_tunable().is_none()` + `result.usage.is_none()`.

### 4.2 `&self` calls, `Send + Sync`
`Provider` and `ToolProvider` calls take `&self`: every per-call product is returned in `ChatResult`/`RoundResult`, so there is no per-call state to borrow. Providers hold only construction-time state (`ProviderCore`, flags, an installed `ToolSearcher`). Setters (`set_model`, `Tunable`, `set_tool_searcher`) take `&mut self` and are called before the run, so nothing is ever shared mutably.

### 4.3 Object safety without proc-macros
`iota::BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>`. Every async trait method is `fn name<'a>(&'a self, …) -> BoxFuture<'a, R>`; implementations write `Box::pin(async move { … })`. This is the single convention for `Provider`, `ToolProvider`, `Tool` and `Dispatcher` — no `async_trait` proc-macro: the object-safe traits need exactly one shape and a hand-boxed future is it. Plain `async fn`s are used everywhere else.

### 4.4 Enums
`ProviderKind` (seven kinds, `as_str`, `FromStr` with the exact `unknown provider type: …` error, `ALL` in listing order, `SUPPORTED_LIST`), `Effort { Low, Medium, High, XHigh, Max }` (`parse("")` → `Ok(None)`; invalid → `InvalidEffort`), `DeferMode`, `DeferState`, `OutputFormat`.

### 4.5 Streaming sink and the reasoning-close contract
`StreamSink { content, reasoning, reasoning_done }`, `NullSink` (headless), `ReasoningGate` (idempotent close, close-before-first-content, close on `Drop`). Dialects hold a gate, never the raw sink. Ordering pinned by `RecordingSink` tests: wire reasoning deltas → `gate.reasoning()`; first tool-argument delta → `gate.close()`; visible content goes through the think splitter which calls `gate.content()`.

### 4.6 Shared core
`ProviderCore { kind, model, temperature, top_p, effort }` + `HasCore`. Since the 2026-09-02 merge the traits and the providers share a crate, so `provider/common.rs` carries the plain blanket impls `impl<T: HasCore + Send + Sync> Tunable for T` and `impl<T: HasCore + Send + Sync> TopPTunable for T`; the per-provider `impl_tunable_via_core!` macro that the orphan rule (E0210) forced across the old crate boundary is gone. Test fakes implement `Tunable` directly on their own (local) types and coexist with the blanket impl.

### 4.7 Wire client, SSE, think splitter
The wire client, the SSE reader and the think-tag splitter are pinned by transcript fixtures. Uniform 2-minute response-header timeout on every client — injectable via `Client::with_header_timeout` for tests, applied to `image_util::fetch_image` too; retries 2 (0 for image dialects); jitter through the `Jitter` seam; cancellation = `tokio::select!` against the token in the retry sleep and around every body read. The OpenAI-shaped `GET /models` listing is one function (`llm::models::openai_model_ids`) shared by chat-completions and responses.

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

- `Tool` / `Dispatcher` with default capability methods; ownership is one lookup through the `Owner` trait (the registry, the MCP manager and the live union each vouch for what they own; `prefix_of` reads a table); `search_tools() -> Option<Vec<ToolDef>>` (the union forwards to the FIRST part that HAS the capability); `supports_parallel(name, Option<&JsonObject>)` per CALL (`None` = the argument-less probe).
- `RunCtx { cancel, budget, artifact }` (`tool::context`) cloned into every tool call.
- `ToolEnv { project_root: Option<PathBuf>, dirs: HostDirs, interactor: Option<Arc<dyn Interactor>> }`; `root()` = project_root else `dirs.cwd` else `current_dir()`, then `std::path::absolute`.
- The registry (sorted keys, first-wins names, the YAML 1.1 disable rule) and the live union of parts (`tool::dispatch`); `DeferDispatcher` (normal/system-tools), `MarkedDispatcher`/`SearchingDispatcher` (reference/tool-search); a `defer_mode` a dialect cannot speak is refused when the config loads (`DeferMode::supports`), so assembly has nothing left to decide.
- Approval/parallel: the REFUSAL lives in `headless::QuietHost`; tools only report. `glob`/`grep`/`list_dir`/`read_file` always run in parallel, and so does `shell` (DIVERGENCES X-05: the model wrote both command lines).
- Interactive-only capabilities (`presentation`, `header_summary`, `deferred_tools`) keep their trait defaults; no built-in implements `header_summary`/`presentation` (DIVERGENCES D-12), `DeferDispatcher`/`MarkedDispatcher` do implement `deferred_tools` (cheap, tested).
- The `shell` MECHANISM (`src/shell/`, the layer the `shell` tool is a policy over) is pinned: one interpreter child per call, in its own process group on Unix and a Job Object on Windows; ONE combined output pipe (`std::io::pipe()` — the same `pipe(2)`, portable and `O_CLOEXEC`) read under the byte cap; an explicit `PWD` on Unix and none on Windows; kill = `killpg(SIGKILL)` with ESRCH ignored / `TerminateJobObject`, then a 3 s wait for the pipe to drain (`timeout(3s, read_to_end)`); the result classified in one fixed order (timeout, signal, exit status). WHICH interpreter runs is `shell::interp`'s one pure answer over an injected machine — `bash -c` on Unix, on Windows the first of Git Bash, PowerShell and `cmd.exe` present (DIVERGENCES X-17) — and a sandbox binary appearing or disappearing later never changes a running set.

---

## 6. MCP manager (rmcp 3.1.4)

The rmcp API in use: `ServiceExt::serve(ClientInfo, transport) -> RunningService<RoleClient, ClientInfo>` (legacy `initialize` handshake by default; rmcp also implements `server/discover` via `ClientLifecycleMode::Auto`, not selected — D-02); `peer().list_all_tools()`; **`peer().call_tool_once(CallToolRequestParams::new(raw).with_arguments(args))`** — never `call_tool`, which drives SEP-2322 MRTR rounds through a handler that cannot answer; `Complete(r)` → text/is_error, `InputRequired`/`Task` → error texts (D-33); `RunningService::cancel(mut self)` consumes the service, so the production `Session` is `RmcpSession { running: tokio::sync::Mutex<Option<RunningService>>, peer: Peer }` (calls use the cloned peer, `close` takes the service out of the mutex); `TokioChildProcess::builder(cmd).stderr(Stdio::piped()).spawn() -> (proc, Option<ChildStderr>)`, `proc.id()`; `StreamableHttpClientTransport::with_client(http, StreamableHttpClientTransportConfig::with_uri(url).custom_headers(map))`; `CallToolResult { content: Vec<ContentBlock>, is_error: Option<bool>, .. }`, `ContentBlock::as_text()`; `Tool { name: Cow<str>, description: Option<Cow<str>>, input_schema: Arc<JsonObject>, .. }`.

- The client handler is rmcp's own `ClientInfo` (rmcp implements `ClientHandler` for it), built as `ClientInfo::new(ClientCapabilities::default(), Implementation::new("iota", "1.0.0"))` — `InitializeRequestParams` is `#[non_exhaustive]`, so there is no custom handler struct.
- Connect fan-out: `JoinSet`, per-server `tokio::time::timeout(30 s, connect_one)`; results merged in CONFIG order (config-file servers sorted by name, then `--mcp` flags in order) so segment suffixes are deterministic.
- Timeout → `connection timed out after 30s` (Go duration formatting); dropping the connect future drops the `TokioChildProcess` whose `ChildWithCleanup::drop` kills the child (rmcp `child_process.rs:44-55`).
- Headers: `custom_headers` (append; reserved names rejected → `connect failed: Header name 'accept' is reserved and conflicts with default headers`); `HeaderName`/`HeaderValue` parse failures → `connect failed: <err>`.
- `close()`: take sessions under the write lock; `timeout(10 s, running.cancel())` per session outside the lock (rmcp: close transport → stdin EOF → 3 s → kill); index cleared so later `call_tool` returns `unknown tool` (no panic).
- Headless host prints `Warning: mcp server <name>: <err>` for every failed server (the full error text — DIVERGENCES).
- Configuration types (`ServerConfig`, `parse_mcp_flag`, `McpFlagError`) live in `mcp::config` and are re-exported from `mcp`, so `cmd::assemble` parses `--mcp`/`mcp_servers` (and raises `--mcp: empty server specification`) without naming `Manager`; the manager produces the `tool::PrefixOf` the live union routes with.
- Internal seams (`Session`, `ServerResult`, `merge_result`) stay `pub(crate)`; their tests are unit tests over the in-crate duplex echo server (`mcp::testutil`, test-only). Only public-API tests (`connect_all` timeout canary, reserved-header failure, idempotent close) live under `tests/`.
- OAuth 2.1 (`mcp::auth`, brain page `mcp-cli-and-oauth`; rmcp's `auth` feature, whose `oauth2` 5.0.0 is why `deny.toml` skips eleven older crate versions): a server with `auth: oauth` connects through `AuthClient<reqwest::Client>` over an `AuthorizationManager` built from the token file alone — `~/.iota/mcp/auth/<name>.json`, mode 0600, holding rmcp's `StoredCredentials` beside the endpoint and the authorization-server metadata, so a refresh needs no discovery. No file, or a handshake the server answers 401 after the refresh failed too, is `McpError::NotLoggedIn` (`not logged in: run iota mcp login <name>`): that one server is `Failed`, the run goes on. `login` = discovery → dynamic registration (or the stored client id) → the URL to `$BROWSER` / the platform opener / stdout → a loopback listener on `127.0.0.1:<random>/callback` (a pasted redirect URL and a 5-minute deadline beside it) → the exchange writes the store; `logout` revokes (RFC 7009, best effort) and removes the file. `Manager::login`/`logout`/`reconnect` are the run-time entries (`/mcp login|logout`): a reconnect detaches the old session, its tools, segment and prefix (`State::session_of`) and merges the new result like any connect.

---

## 7. Config + CLI (`iota`)

- clap derive `Cli` is a CLOSED verb set — `run` / `list` / `resume` / `config` / `mcp` / `version`, with a bare `iota` meaning `run` (X-10) — plus the nine flags that describe ONE invocation (X-11). `iota mcp add|list|get|remove` (`cmd::mcp_cmd`, brain page `mcp-cli-and-oauth`) edits the `mcp_servers:` block of ONE file through `config::edit` — the scope's (`--scope user` = `~/.iota.yaml`, the default; `--scope project` = `./.iota.yaml`, where a header/env value must be a `${…}` reference) or the `-c` file alone; `list --probe` connects through the manager the way a run does. `-c/--config` is declared once on the root as a GLOBAL argument, so it is valid on either side of the verb; the other seven belong to `RunArgs`, and one of them given BEFORE another verb is refused by `Cli::check_flag_placement` (clap's `args_conflicts_with_subcommands` cannot be used for that: with it set, any root argument stops the verb from being recognised at all, which is what made a global `-c` impossible). `cmd::run` dispatches on the verb; `run` and `resume` share `run_agent`, which normalises both into an `Invocation { agent, resume, args }` so nothing downstream knows which word was typed.
- `run_agent`'s order: `-m` → one headless turn; otherwise → the interactive branch (`interactive/`: `mod.rs`, the `iota resume` picker in `picker.rs`, the title stack in `title.rs`), taken AFTER the agent, key, model, temperature, tuning, MCP-config and output-format checks, so every earlier byte-pinned error (`unknown agent "codr"…`, `--output-format applies to -m runs only`) still wins; a non-TTY stdout is then refused with `interactive mode requires a terminal; use -m/--message for piped input`. `resolve_run` therefore returns `message: Option<String>`. `--no-save` and a bare `iota resume` are rejected for `-m` runs only (`reject_unsupported`, `ArgsError::ResumeIdRequired`); the interactive branch reads them for real. `-m ""` → `--message must not be empty`.
- `Config` model with `serde_norway`, `#[serde(default)]`; bool fields through `tool::yaml11::deserialize_bool` (YAML 1.1 spellings); `tools: BTreeMap<String, serde_norway::Value>` raw; `mcp_servers: Option<Vec<String>>` (absent ≠ `[]`); `defer: Option<String>`. Unknown and misplaced keys are refused BEFORE the typed decode by `config::strict::audit`, which walks the raw `serde_norway::Value` so an error can name its coordinate (`agents.coder.tools.delegate`) — something `deny_unknown_fields` cannot (X-15). The document is therefore parsed twice: once as a `Value` for the audit, once into the typed shape, which keeps serde's line/column on a field's type error.
- `Config::load(explicit, &Env, warn)` fails on anything the user wrote wrong (a misplaced key, a dangling reference, an unusable `defer_mode`) and only WARNS for what says nothing about intent — an unreadable file, or one that is not YAML at all. `Config::sources` is the file list `iota config path` prints; `merge_file` replaces whole entries; `${var}` expanded once at merge time on key/url/system_file.
- `Config::provider(name) -> Endpoint<'_>` is the one place a provider name resolves to its endpoint: the `providers:` entry (a default one for an unconfigured built-in type) and the TYPE behind it. `Endpoint::api_key(&Env) -> ApiKey` is the one definition of the key precedence — the type's variable, else `key:`, else `Missing` — which `resolve_run`, `iota list providers` and the `/model` catalog all consume (X-34).
- `resolve_run(inv, cfg, &Env, stdin)` is pure and resolves in one fixed order — the agent (`Config::resolve_agent`), the key, the model, temperature, tuning, the output format — the order the byte-pinned errors are tested in.
- `config/params.rs` holds the LAYERED evaluation of `context_window`/`effort`/`temperature`/`top_p` (X-24, brain page `model-param-layering`): `Declared::of` folds `agents:` over the running model's `models:` entry, `Declared::evaluate` resolves that against the session's current value — keeping one the user set and DROPPING one an earlier model's declaration supplied — and `Declared::resume` restores a bundle's own record instead of evaluating. `ParamLayers` is the table a live chat re-evaluates against (the agent, every `models:` entry by `provider:id`, and the `defer_mode` the dispatcher was assembled with). The window arrives already parsed, so `config::window::parse_window_size` stays the caller's: a bad value aborts a run at startup (`SetupError::ContextWindow`, labelled with the layer that wrote it) and is a transcript warning mid-chat. `interactive/mod.rs::resolve_params` is the startup/resume moment, `repl::params` the `/model` one.
- `tuning::apply` = image → effort → top_p → temperature → json_edits → gen-params → tools/mcp warning.
- `assemble::build_mcp_configs` / `build_dispatcher` use only `mcp::config` types and take the MCP part as `Option<(Arc<dyn Dispatcher>, PrefixOf)>` (`None` = no server configured); `Manager` is named only in `cmd::run`, `interactive/mod.rs` and `mcp_cmd` (`list --probe`).

---

## 8. Headless run loop (`headless`)

`once(cancel, provider: &mut dyn Provider, dispatch: Arc<dyn Dispatcher>, opts: OnceOptions, out: &mut (dyn Write + Send)) -> Result<OnceOutcome, ChatError>` (a `Send` future, awaited via `block_on` only; `OnceOutcome.delta` is the turn's message delta):
1. `rec = RunRecorder::start()`, `budget = TurnBudget::new(opts.max_turns)`, `cx = RunCtx { cancel, budget, artifact: None }`.
2. install the tool searcher on the provider when it has the host (closure = `dispatch.search_tools(q).unwrap_or_default()`).
3. `run_once` with the request, the dispatcher, no local cap, the quiet host and the images dir.
4. If `cx.cancel.is_cancelled()` and the run failed → error becomes `ChatError::Interrupted`.
5. JSON: always `write_report` then return the run error (write error wins). Text: on error write nothing; else `reply\n` (non-empty), `🖼 saved: <path>` lines, then image error lines.

`run_once`: the history starts as `req.history` (the imported view) and that length is the **watermark**; the `-s` system message is pushed only when the imported history is empty, so a resumed session keeps its own. Tool path → `execute_with_tools(..) -> LoopOutcome { content, reasoning, images, usage }` and the images are the TERMINATING round's `RoundResult.images`; unary path → `provider.chat(compose_send_history(..))` and `result.images`. Both paths then `save_images_for_turn(&images, images_dir)` (children pass `None`), so a gemini/openresponses run with tools advertised still saves what it generated. The final assistant message is then pushed with the terminating round's usage and exactly the SAVED image subset (D-53), and `RunOutcome.delta` is everything past the watermark.

`execute_with_tools` (over a `TurnParams`: the context, the provider, the dispatcher, the round-0 tool set, the overlay, the local cap), per round and in this order: local cap → `budget.take()` → live `tools()` after round 0 → `take_pending_loads()` mount → `stream_chat_with_tools(.., compose_send_history(history, overlay), .., &mut NullSink)` → `rec.observe(usage, names)` → termination (reasoning-only rule; returns `LoopOutcome` with that round's images) → assistant message with `raw_content` → execution walk (`parallel_run` / `run_batch` / serial with the approval gate, `detail = dispatch.header_summary(..).unwrap_or_default()` — D-12). The refusal text is pinned by the tests.

Signals: `main` installs `ctrl_c` + `SIGTERM` listeners that cancel the root token; every long await selects on it; exit 130 after `mgr.close()`.

### 8.1 Session data flow (phase 2 slice 1)

The loop stays session-blind. `cmd::run` owns both ends, and the whole stage sits between the
headless-vs-interactive branch and the MCP connect, so `iota resume <id>` without `-m` still ends
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
leaves nothing behind — and the eight per-field setters the store once had collapse into one `update_meta(|m| …)` because the
schema forces one rule for all of them (write only once the bundle exists).

Interoperability with the bundles the Go original wrote is the whole point of the store, and it is tested
hermetically: `tests/cmd/session.rs` resumes the checked-in bundles the Go writer produced
(`tests/fixtures/sessions/`). The last two-way round trip against the Go binary itself is recorded in
`docs/history/ROUNDTRIP-FINAL.md`.

---

## 9. Error taxonomy

| module | type | Display contract |
|---|---|---|
| `provider` | `ProviderError` | `chat error: {0}`, `stream error: {0}`, `failed to list models: {0}`, `no response choices`, transparent `Permanent`, `interrupted`, `{0}` (Other) |
| `tool` | `ToolError` | `unknown tool: {0}`, `interrupted`, `{0}` (Transport) |
| `provider` | `UnknownProviderType` | `unknown provider type: {0} (supported: openai, anthropic, gemini, vertexai, openresponses, imagen, images)` |
| `mcp::config` | `McpFlagError` | `--mcp: empty server specification` (the only MCP error that aborts a run; feature-independent) |
| `llm` | `LlmError` | `StatusError` (`{method} "{url}": {status} {text} {body}`), `stream ended without any SSE events (server did not stream?)`, `received error while streaming: {0}`, `llm: malformed stream chunk: {0}`, `llm: malformed stream event: {0}`, `llm: malformed images response: {0}`, `llm: image stream ended without a completed image`, `RespFailure`, `llm: encode request: {0}`, `llm: authorize request: {0}`, transport/decode passthrough, `response headers not received within {d}` (header timeout, retried like transport), `llm: invalid model name {0:?}`, `interrupted` |
| `tool`, `agents`, `shell` | `SetError`, `SkillError`, `ShellError` | exact texts, each pinned by its module's tests |
| `mcp` | `McpError` | `server config must have either command or url`, `unsupported URL scheme: {0}`, `connect failed: {0}`, `list tools: {0}`, `connection timed out after {0}`, `{0}` (call: ServiceError text, `MRTR_UNSUPPORTED`, `TASK_UNSUPPORTED`) |
| `headless` | `ChatError` | the two `tool loop reached the --max-turns limit …` forms, `unknown agent {0:?}`, `interrupted`, transparent provider/child/io; `images::HOME_NOT_DEFINED` = `$HOME is not defined` |
| `session` | `SessionError` | `session {0} not found`, `cannot read session {id}: {source}`, `no session matches {0:?}`, `session id {0:?} is ambiguous: {1}`, `read session log: {0}`, `$HOME is not defined`, `{0}` (Io) — every one pinned by a test |
| `cmd` | `CliError` = `ArgsError` \| `SetupError` \| `RunError` (`cmd/error.rs`: the invocation, the setup, the run), `ConfigError` | every startup text the Go original printed, plus the surface's own: `NoAgent`, `UnknownAgent`, `ApiKeyRequired{env,provider}`, `ResumeIdRequired`, `ListTakesNoName`, `ConfigExists`, `NoHome`; `ConfigError::{Key, File}` carry a config coordinate and the file it was written in (X-15); `McpFlag(#[from] McpFlagError)`; `Session(#[from] SessionError)` as `#[error(transparent)]`, so each ported Go string reaches stderr unwrapped |

The listing no longer fetches anything, so the double-prefixed `failed to list models: failed to list models: <inner>` the Go original could print has no site left (X-13); the network model list survives only in the interactive `/model` picker.

---

## 10. Testing strategy

Principles: unit tests beside code (and ONLY there for `pub(crate)` seams — `tests/` is a separate crate, so no `#[path]` mount of a source file is ever used); integration tests in `tests/<area>/main.rs`, one binary per area; `wiremock` for HTTP; SSE transcripts as `const &str` fixtures; `tempfile`; fakes from `iota::testing`; shared `tests/common/` fixtures; ported tests keep their original names in snake_case; **no process-env mutation** (everything injected), so no `serial_test`; `#[tokio::test(flavor = "multi_thread")]` for contention tests; the end-to-end tests run the built binary (`CARGO_BIN_EXE_iota`) and set env on the child process only.

CI (`ci.sh`, one package, one binary — since 2026-09-02/2026-09-01): `cargo fmt --check` · `scripts/check-deps.sh` (direct deps ⊆ `scripts/direct-deps.allow`, via `cargo metadata`) · `scripts/check-stubs.sh` (no `todo!()`, no stub header) · `cargo clippy --all-targets -- -D warnings` (pedantic via `[lints]`) · `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` · `cargo test` (which runs the layering gate of §1.2, `tests/layering.rs` — three greps here until 2026-09-15) under `IOTA_TMUX_REQUIRED=1 IOTA_SANDBOX_REQUIRED=1` (since 2026-09-15: the single L4 tmux execution rides that one invocation, and a missing tmux / bash / mock port / bubblewrap is a red test naming it, not a `SKIP:` that passes; a plain `cargo test` without the variables still skips) · `cargo build --release` + `scripts/size.sh` writing the one size row into `target/size.md` (a CI artifact).

The session slice adds one testing rule to the list above: **no test hardcodes a fixture session id.** The Go-written bundles under `tests/fixtures/sessions/` carry random ids, and every id, prefix, count and expected string is read from their `manifest.json` (whose `expect` block was measured by loading each bundle back through the real Go loader). Regenerating the corpus therefore never invalidates a test.

---

## 11. Footprint plan

**One binary (decision of 2026-09-01: "跟 go 版本完全对齐，直接 build 出一个统一的二进制").** The phase-1 footprint design — cargo features that compiled providers, toolsets, MCP, the TLS backend, the TUI, token accounting and syntax highlighting in or out, a `tui-portable` fallback and a `tls-ring` backend — was removed in full. Like the Go binary there is exactly ONE shipped artifact carrying everything (`cargo build --release`), and the only cargo feature left anywhere is `testing` (the shared test fakes; it never affects the binary). Gone with it: every `#[cfg(feature = …)]` in the sources, the compiled-out runtime stubs (`toolset … not compiled into this build`, `provider type … not compiled into this build`, `Warning: mcp support is not compiled into this build`, `CliError::InteractiveUnavailable`), the three `build.rs` TLS guards, `iota::tls`, the per-crate `init_tls()` test helpers, the reduced-feature CI legs and the size matrix. The entry model is Go's exactly (§7).

- Release profile as §1.4. TLS: rustls with aws-lc-rs and the platform verifier (`reqwest/rustls`, rmcp `reqwest`), always — no ring backend, no `rustls` direct dependency, no provider installation.
- Direct-dependency allowlist in `scripts/direct-deps.allow`, enforced by `scripts/check-deps.sh` over `cargo metadata` (deliberately not cargo-deny's `[bans]`, which reads over the whole transitive graph and cannot say "may be named in Cargo.toml"); every new direct dependency needs a one-line justification in `docs/ARCHITECTURE.md`. **The transitive graph has its own gate since 2026-09-15: `deny.toml` at the root — licenses (exactly the set in today's graph, an unused entry is itself an error), advisories, duplicate versions (`deny`, the seven known transitive pairs skipped by range) and sources, over the five shipping targets — run by its own `cargo deny` job in `ci.yml` (cargo-deny-action) and by `./ci.sh` when `cargo-deny` is on PATH (it is not in the pinned toolchain, so that leg prints SKIP without it). Its first run caught rustls 0.23.43 (RUSTSEC-2026-0285, fixed by the lockfile bump to 0.23.45) and the retired bincode 1.x under syntect (RUSTSEC-2025-0141, ignored with its reason in the file).** The session store brought no new third-party crate (`sha2`, `rand` and `jiff` were already in the table), and neither an async runtime nor an HTTP client is reachable from it. Dropped: `dirs`, `which`, `url`, `unicode-width`, `path-clean`, `indexmap`, `hex`, `filetime`, `async-trait`, `serial_test`, `temp-env`, `mime_guess`, `shell-words`, and (2026-09-01) `rustls`, which only ever existed to install the ring provider.
- `tracing` compiled with `release_max_level_debug` (rmcp/process-wrap/hyper emit tracing; the two `debug!`s in `shell/exec.rs` are the crate's own). It was `release_max_level_off` until 2026-09-15, which is why the one `tracing::warn!` meant for the user never reached one (DIVERGENCES X-29): `tracing` is the developer's channel, heard only through `IOTA_LOG=<path>` (`src/app/diag.rs`), and user-facing warnings go through `Streams::warning` or the transcript.
- Measured (stripped, `z`, aarch64): the unified binary is the one row of `target/size.md`, written by `ci.sh` on every run (a CI artifact). Breakdown for orientation (measured 2026-09-01, just before the legs were removed): the TUI itself (ratatui + crossterm + the `ui` and `repl` modules) ≈ 0.4 MiB; the heavy parts are two embedded tables the Go binary carries as well — tiktoken-rs' bundled `o200k_base` ranks ≈ 3.6 MiB and syntect + two-face's syntax/theme dumps ≈ 1.1 MiB; then aws-lc-rs ≈ 3 MiB, hyper/reqwest ≈ 1.2 MiB, regex ≈ 1 MiB, rmcp ≈ 0.8 MiB, tokio ≈ 0.6 MiB. That breakdown has one correction since: on 2026-09-11 the ranks became `tiktoken`'s zstd-compressed table (≈ 0.78 MiB embedded, ≈ 0.87 MiB of binary), which took the unified binary from 10.25 MiB to 7.56 MiB and left syntect + two-face as the heaviest embedded table. Keeping `tracing` compiled to `debug` in release and adding `tracing-subscriber` for `IOTA_LOG` (2026-09-15, X-29) cost 7.59 → 7.72 MiB: the dependencies' event metadata and format strings, which `release_max_level_off` used to strip.

- **The TUI's direct dependencies:**
  - `markdown` + `text::{width,ansi}` — the pure streaming markdown→ANSI renderer + THE grapheme width ruler; zero terminal deps so every layer shares one ruler.
  - `ui` — the ONLY ratatui/crossterm importer (`tests/layering.rs`); owns the inline frame engine and implements `ui::facade::Ui`.
  - `repl` — the imperative interactive chat loop over the facade; keeps ratatui out of the loop and the loop out of the UI.
  - `ratatui` (=0.30.2) — spike-validated `Viewport::Inline` + `insert_before` scrollback engine; feature `unstable-rendered-line-info` for the W6 `line_count` law, `scrolling-regions` always on (wart W9 is a per-emulator verification item in `docs/TUI-VERIFY.md` §2, not a build variant).
  - `crossterm` (=0.29.0) — ratatui's default backend pairing (wart W8: ONE crossterm; never `crossterm_0_28`); raw mode, bracketed paste, events, DSR.
  - `unicode-width` (=0.2.2) + `unicode-segmentation` — the two halves of the grapheme ruler (uniseg-parity widths incl. the VS16 rule).
  - `vt100` (=0.16.2, dev-only) — L2b byte-stream terminal-semantics assertions (scroll-region proof, exact-width characterizations) without a real terminal.
  - `syntect` (=5.3.0) — chroma's replacement behind the `CodeHighlighter` seam, shared by the fenced-code renderer and the chat diff renderer. Built `default-features = false` with `regex-fancy`, so the regex engine is pure Rust (`fancy-regex`) and no `onig`/C toolchain enters the build.
  - `tiktoken` (=4.1.2, `default-features = false`, feature `vocab-o200k_base`) — the `o200k_base` token counter behind the live context meter, `/compact` and the auto-compaction offer. Chosen for the property Go bought with `tiktoken-go` + `tiktoken-go-loader`: it EMBEDS the rank table, so counting a conversation is never a network call and never a runtime download. It embeds it zstd-compressed and behind a feature PER VOCABULARY, which is why it replaced `tiktoken-rs` =0.12.0 on 2026-09-11: the same table costs ≈ 0.78 MiB instead of the 3.6 MiB `include_str!`ed `.tiktoken` asset, for byte-identical token id vectors (verified over five corpora, 1823 windows and sixteen adversarial cases; the `tiktoken-go` v0.1.8 parity goldens in `src/repl/context/tokens.rs` are the standing gate). `default-features = false` is load-bearing — the default `vocabs-all` would embed twelve vocabularies (+2.92 MiB) for the one we ask for. The lookup is memoized behind a `OnceLock` on first use (the crate owns the `&'static CoreBpe`), and a table that fails to load degrades to Go's `len(text)/4` byte heuristic rather than failing the chat. It brings `ruzstd` and `twox-hash` transitively (`regex` and `rustc-hash` are already in the graph), and it drops the THIRD copy of `fancy-regex` (0.17.0) `tiktoken-rs` pulled in beside syntect's; `anyhow`, `base64`, `bstr`, `bit-set` and `lazy_static` stay, each of them still reached another way.
  - `two-face` (=0.5.2) — bat's syntax and theme dumps for syntect: the language coverage chroma had out of the box, plus the `MonokaiExtended` / `Github` themes standing in for chroma's `monokai` / `github`.

- **The full-parity features' direct dependencies:**
  - `comrak` (=0.54.0, `default-features = false`) — `/export`'s Markdown → HTML: CommonMark + GFM in safe mode (raw HTML → `<!-- raw HTML omitted -->`, goldmark's text) with the `SyntaxHighlighterAdapter` seam the code-fence renderer needs. Default features are off so neither the CLI, `bon`, nor comrak's own `syntect-onig` feature enters the build — the latter would bundle syntect's default syntax/theme dumps and onig beside two-face's; our adapter reuses `markdown::highlight`'s syntax set. syntect gains its `html` feature (`ClassedHTMLGenerator`) for the same reason. New transitive crates: `caseless`, `jetscii`, `typed-arena`.
  - `image` (=0.25.10, `default-features = false`, features `png`/`jpeg`/`gif`/`webp`) — the half-block rasteriser's decoders: exactly the four formats Go's `imgterm` registers, all pure Rust, no rayon. Only `src/imgterm.rs` may name the crate (`tests/layering.rs`); everything else sees `imgterm::Frame`. Encoders are reachable only from tests. New transitive crates: `png`, `gif`, `weezl`, `color_quant`, `zune-jpeg`, `zune-core`, `image-webp`, `moxcms`, `byteorder-lite`, `num-traits`.
  - `memchr` (2, 2026-09-14) — `memmem::Finder`, the substring search that works on `&[u8]`. `edit_file` counts, checks uniqueness and replaces on the file's BYTES, never on a decoded `String`, so a source file in Latin-1, Shift-JIS or GBK comes back off disk byte for byte (DIVERGENCES R-01). Already in `Cargo.lock` (2.8.3) under `regex`, `globset` and `ignore` — no new transitive crate.
  - `http` (=1) — `/debug`'s recorded response is rebuilt through `http::Response` → `reqwest::Response` (`From<http::Response<T>>`); reqwest re-exports `header`/`Method`/`StatusCode`/`Version`/`Url` but not the crate, so naming `http::Response` needs the direct dependency. Already in `Cargo.lock` (1.5.0) — no new transitive crate.
  - `tracing-subscriber` (0.3, `default-features = false`, features `fmt` + `std`; 2026-09-15, roadmap §3 #7) — the consumer behind `IOTA_LOG=<path>` (`src/app/diag.rs`): a file subscriber with a `Targets` filter (this crate at DEBUG, everything else at INFO). It is tracing's own consumer and the one every tracing-emitting dependency here is written against; a hand-rolled `Subscriber` would have re-implemented span storage and field formatting for the sake of two crates. No `ansi` (a log file never wants SGR), no `env-filter` (the level is fixed; its `matchers`/`regex-automata` pull stays out), no `tracing-log`. New transitive crates: `sharded-slab`, `thread_local`.

---

## 13. Risks and mitigations

1. **rmcp lifecycle drift** (handshake version, child kill on drop, header rules) — isolated in `mcp/transport.rs`; `manager_connect_timeout` lands first as the canary; in-process duplex tests exercise the real handshake; divergences recorded.
2. **Byte parity of ~150 strings** — every Display text is a `thiserror` literal or a `const`; the `strings` tests pin them; `go_quote`/`go_float`/`go_duration` helpers have their own tables.
3. **SSE / think-splitter edge semantics** — hand-rolled, transcript fixtures verbatim, byte-at-a-time property test.
4. **Sandbox availability in CI** — probed; under `IOTA_SANDBOX_REQUIRED=1` (what `ci.sh` sets) a missing sandbox is a red test that names it, while a plain `cargo test` prints a `SKIP:` line; the macOS runner has sandbox-exec, the Ubuntu runner installs bubblewrap.
5. **YAML 1.1 bools** — one `yaml11` module used by config and the disable rule; tests pin `agent: yes`, `ask: False`, `network: on`.
6. **Cancellation completeness** — every long primitive selects on the token; `spawn_blocking` walks are capped; an end-to-end test sends SIGINT to a wiremock-stalled run and checks exit 130 + JSON `"error": "interrupted"`.
7. **Binary size** — one release build measured into `target/size.md` on every CI run; no feature pruning by decision (§11).
8. *(retired 2026-09-01 with the ring backend — there is one TLS provider and nothing to unify.)*
9. **Determinism vs Go completion order** (MCP segments) — config-order merge; ported tests hold under both.
10. **Lock discipline** — `Manager`'s `RwLock` is never held across `.await` (peer cloned, guard dropped); `CodeSet.reads` mutex never held across I/O; multi-thread stress tests.
11. **Compile-time traps** — `RawValue` has no `PartialEq` (→ `Raw` newtype), the orphan rule forbade the blanket `Tunable` impl across crates (→ a macro then; a plain blanket impl since the 2026-09-02 merge), `gen` is a 2024 keyword (→ `params`), `pub(crate)` seams cannot be driven from `tests/` (→ unit tests).

---

## 14. Rejected review findings

| finding | decision | reason |
|---|---|---|
| Replace the hand-rolled allowlist with `cargo deny check bans` and keep the file name `deny.toml` | **partially rejected** — the file is renamed to `scripts/direct-deps.allow` + `scripts/check-deps.sh` (so nothing masquerades as cargo-deny config), the `cargo tree` gate is replaced by the `--prefix none \| grep` form as proposed | cargo-deny's `[bans] allow/deny` lists apply to the whole transitive graph, not to direct dependencies, and `cargo-deny` is not part of the pinned 1.98.0 toolchain; a `cargo metadata` script expresses the intended rule exactly and needs no extra install in CI. *(2026-09-15: a `deny.toml` now exists beside it for what the script never claimed — licenses, advisories, duplicate versions, sources over the transitive graph — as its own CI job; the direct allowlist stays with the script, §11.)* |
| Move `ProviderCore`/`HasCore` + the blanket `Tunable` impls into the contracts module | **rejected then** (the per-provider macro was adopted); **moot since 2026-09-02** — in one crate the blanket impl is legal and is what ships (`provider/common.rs`); the test fakes' own `Tunable` impls coexist with it because they are local types | a blanket impl across a crate boundary was an orphan-rule violation; inside one crate it is the idiomatic form |
| Add `#[error("{0}")] Mcp(String)` to `CliError` | **replaced** by a typed `McpFlag(#[from] mcp::config::McpFlagError)` | the same finding set also required `assemble.rs` to compile without the `mcp` feature; moving the config types beside the contracts solves both, and a typed variant keeps the `thiserror` taxonomy honest (no stringly errors) |
| Flatten rmcp `CallToolResponse::InputRequired(r)` to the same `(joined text, is_error)` as `Complete` | **rejected as stated**; `call_tool_once` IS adopted | `InputRequiredResult` (rmcp model/mrtr.rs:229-246) carries no `content`/`is_error` — only `result_type`, `input_requests`, `request_state`, `_meta` — so there is nothing to flatten; `InputRequired`/`Task` map to explicit error texts instead (D-33) |
| Build `grep` regexes with `RegexBuilder::unicode(false)` to make `\w`/`\d`/`\s`/`\b` ASCII-only like RE2 | **rejected**; documented in D-18 | on `regex::Regex` (str-based) disabling Unicode makes any pattern that could match non-UTF-8 — `.`, negated classes — fail to COMPILE, a far larger divergence than Unicode-aware word classes |
| Put the `install_tls_provider()` ONCE helper in `testing` | **rejected**; per-crate `tests/common::init_tls()` instead *(moot since 2026-09-01: no ring backend, no helper)* | the contracts crate of the time had no `rustls` dependency and had to stay I/O-free; `install_default()` is already idempotent (returns `Err` when a provider exists), so each crate calls its own one-liner |
| Delete `design/DESIGN.md` | **softened**: kept with a `SUPERSEDED — NOT binding` banner, its manifest block removed | it documents which alternatives were considered and rejected; the banner and the removal of every pinnable manifest line remove the confusion risk |
| Port the sorted-key argument digest so `ask_approval`'s `detail` matches Go | **not adopted** (finding proposed pinning `header_summary(..).unwrap_or_default()`, which is what the headless loop does) | headless hosts never approve, so the detail is unobservable in production; `TestForwardedApprovalCarriesTheCallDetail` stays unported (D-12) |
