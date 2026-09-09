# Crate merge plan — 10 crates → one package `iota`

Status: EXECUTED 2026-09-02 (approved verbatim by the user the same day, including the two open points — the shared `text/` module and the `shell/` mechanism layer beside `tool/shell.rs`).

## 0. Principles

1. Build model = Go's: `rust/Cargo.toml` is ONE package (`iota`: lib + thin bin). No workspace,
   no per-area manifests, no `check-deps` allowlist plumbing beyond one `[dependencies]` table.
2. Top-level module names mirror Go packages where that aids recognition (`cmd`, `config`,
   `provider`, `llm`, `tool`, `shell`, `agents`, `mcp`, `chat`, `session`, `markdown`, `ui`,
   `app`, `vars`, `text`). Where the Rust decomposition is cleaner than Go's (headless `chat` vs
   interactive `repl` vs `session` — Go lumps all three into `chat/`), the Rust split is KEPT as
   modules; it is a module tree, not a crate tree, so it costs nothing.
3. Visibility flips to Rust-idiomatic: everything defaults to `pub(crate)`; `pub` only for the
   surface `main.rs` and `tests/` genuinely use (`cmd::run`, `cli::Cli`, `config::Config`,
   `chat::once`, `provider::new_provider`, `session::*` store API, `testing::*`). Doc comments
   stay (they exist); `missing_docs` then bites only on the real API.
4. The crate-boundary hacks die: `impl_tunable_via_core!` → a plain blanket impl (legal inside one
   crate); the three "shared only because two crates needed them" types go home
   (`PrefixOf` → `tool`, `mcp::ServerConfig` → `mcp::config`, `EnvSource` stays `vars`); the
   `#[path]`-mounted tests become in-file unit tests (`#[cfg(test)] mod tests`) — integration
   tests use the public API only.
5. Invariants that were crate boundaries become two greps in `ci.sh` (Go's own discipline):
   - only `src/ui/**` may name `ratatui`/`crossterm`;
   - `src/session/**` never reads the process environment.
6. Binary is byte-for-byte the same class of artefact (fat LTO already merged everything).

## 1. Module map (old → new)

| old crate / file | new module (rust/src/…) | Go counterpart | note |
|---|---|---|---|
| `iota-core/app.rs` | `app.rs` | internal/app | |
| `iota-core/vars.rs` | `vars.rs` | internal/vars | `EnvSource`, `VarResolver` stay here |
| `iota-core/paths.rs` | `paths.rs` | (filepath helpers) | |
| `iota-core/text.rs` | `text/mod.rs` | internal/timefmt + tokfmt | `elapsed`, `tokens`, Go %q/%v/Duration formatters |
| `iota-markdown/width.rs` | `text/width.rs` | internal/textwidth | the single width ruler (shared by markdown + ui) |
| `iota-markdown/ansi.rs` | `text/ansi.rs` | internal/ui/clip.go | wrap/strip/truncate; shared by markdown + ui + repl |
| `iota-core/model.rs`, `usage.rs` | `provider/model.rs`, `provider/usage.rs` | provider/provider.go (types) | |
| `iota-core/provider.rs`, `sink.rs` | `provider/mod.rs`, `provider/sink.rs` | provider/provider.go (traits) | `Provider`, capability traits, `StreamSink`, `ReasoningGate` |
| `iota-core/error.rs` | split: `provider/error.rs`, `tool/error.rs` | per-package errors | `ProviderError`/`PermanentError` → provider; `ToolError` → tool; `BoxError` → `lib.rs` |
| `iota-llm/{common,think,usage_conv,image_util}.rs` | `provider/{common,think,usage_conv,image_util}.rs` | provider/base.go, thinktag.go, usage.go | blanket `impl<T: HasCore> Tunable for T` replaces the macro |
| `iota-llm/{openai,anthropic,google,openresponses,imagen,images}.rs` | `provider/{openai,anthropic,google,openresponses,imagen,images}.rs` | provider/*.go | |
| `iota-llm/wire/*` | `llm/{mod,client,sse,error,models,chatcomp,responses,anthropic,google,images}.rs` | internal/llm | the wire layer keeps Go's name |
| `iota-core/tool.rs`, `delegate.rs`, `toolfmt.rs` | `tool/mod.rs` (Tool/Dispatcher/Env/Delegator seam types), `tool/fmt.rs` | tool/tool.go | `PrefixOf` lives here now |
| `iota-tools/{lib,registry,merge,defer,defer_mode,args,yaml11}.rs` | `tool/{sets,registry,merge,defer,defer_mode,args,yaml11}.rs` | tool/tool.go, defer*.go | `sets.rs` = the SET_NAMES/factory table |
| `iota-tools/{ask_set,agent_set,delegate_set}.rs` | `tool/{ask,agent,delegate}.rs` | tool/ask.go, agent.go, delegate.go | Go's flat naming |
| `iota-tools/shell/mod.rs` | `tool/shell.rs` | tool/shell.go | the `bash` tool policy layer |
| `iota-tools/shell/{exec,sandbox_*}.rs` | `shell/{mod,exec,sandbox_darwin,sandbox_linux,sandbox_other}.rs` | internal/shell | mechanism layer, Go's split |
| `iota-tools/code/*` | `tool/code/{mod,tools,walk,udiff}.rs` | tool/code.go | |
| `iota-tools/agents/*` | `agents/{mod,skills}.rs` | internal/agents | `Overlay`, `compose_send_history`, skills |
| `iota-core/mcp.rs` | `mcp/config.rs` | mcp/manager.go (ServerConfig) + mcp/vars.go | goes home |
| `iota-mcp/*` | `mcp/{mod,manager,naming,status,transport,error}.rs` | mcp/ | |
| `iota-core/ctx.rs` | `chat/turns.rs` | chat/turns.go | `RunCtx`, `TurnBudget`, `DelegationLedger`, `ArtifactSlot` |
| `iota-chat/*` | `chat/{mod,once,run,batch,report,images,delegator,error}.rs` | chat/chat.go, output.go, parallel.go, images.go, delegate.go | headless path |
| `iota-session/*` | `session/{mod,meta,record,rawcodec,id,store,writer,loader,tuning,error}.rs` | chat/session.go, settings.go | |
| `iota-markdown/{writer,inline,table,list,quote,code,link,math,style,sink,highlight,highlight_syntect}.rs` | `markdown/{mod,inline,table,list,quote,code,link,math,style,sink,highlight}.rs` | internal/markdown | `highlight_syntect` folds into `highlight.rs` (one impl now) |
| `iota-core/ui/mod.rs` | `ui/facade.rs` | docs/design/ui-architecture.md facade | the `Ui` trait + value types + guards |
| `iota-tui/*` | `ui/{mod,event_loop,frame,region,sink,composer,paste,keys,suggest,surface/…,handle,oneshot,term,osc,spans,theme,clipboard,debug,msgs}.rs` | internal/ui | the ONLY module allowed to name ratatui/crossterm |
| `iota-repl/*` | `repl/{mod,run,turn,toolloop,transcript,group,uisink,interrupt,retry,steer,approval,interact,diff,errors,styles,title,banner,mcpreport,meter,tokens,replay,systemtab,commands/…}.rs` | chat/run.go and friends (interactive) | |
| `iota/{cli,resolve,list,tuning,assemble,delegate,io,signals,window,interactive}.rs` | `cmd/{mod,cli,resolve,list,tuning,assemble,delegate,io,signals,window,interactive}.rs` | cmd/root.go, delegate.go | `cmd::run` is the lib entry |
| `iota/config/mod.rs` | `config.rs` | config/ | |
| `iota/main.rs` | `main.rs` | main.go | ~30 lines, `anyhow` only here |
| `iota-core/testing.rs`, `testing/scripted.rs` | `testing/{mod,scripted}.rs` | (test fakes) | behind the ONE surviving feature `testing`, unchanged |

Line totals are unchanged (~41k src); nothing is rewritten, only moved and re-pathed.

## 2. Tests (rust/tests/)

Integration test files become ONE binary per area (Rust's `tests/<area>/main.rs` form), so we get
~10 test binaries instead of ~70 (each `tests/x.rs` links the whole lib; 70 links of a 41k-line lib
is the one real cost of merging, and grouping removes it):

| binary | contents (old files) |
|---|---|
| `tests/provider/` | iota-llm/tests/{openai,anthropic,google,openresponses,imagen,images,think,tool_delta,usage_capability,usage_conv,wire}.rs + iota-core/tests/strings.rs |
| `tests/tool/` | iota-tools/tests/{framework,shell,code,agents,delegate_tool}.rs |
| `tests/mcp/` | iota-mcp/tests/{manager,naming}.rs |
| `tests/session/` | iota-session/tests/{golden,loader,meta,rawcodec,record,store,tuning,writer}.rs |
| `tests/chat/` | iota-chat/tests/{approval,delegator,loop,output,parallel,turns}.rs |
| `tests/markdown/` | iota-markdown/tests/{ansi,code,harness,highlight,inline,list,math_corpus,quote,spacing,table}.rs |
| `tests/repl/` | iota-repl/tests/* (15 files) |
| `tests/ui/` | iota-tui/tests/{browser,composer,facade,frame_goldens,queue,region,search,slider,suggest,surface,switch,view_search,vt100_semantics}.rs |
| `tests/ui_tmux/` | iota-tui/tests/tmux.rs + tmux/ scripts (stays env-gated IOTA_TMUX=1, its own binary so the gate is one leg) |
| `tests/cmd/` | iota/tests/{cli,config,delegate,interactive_cli,resolve,session}.rs |
| `tests/common/`, `tests/fixtures/` | merged shared fixtures (temp_project, sessions fixtures + manifest, mock helpers) |

Conversion rules: a test that today reaches a `pub(crate)` item through a `#[path]` mount (the
iota-tui convention) moves into that source file as `#[cfg(test)] mod tests` — no `#[path]` mounts
survive. `// Go: file:line` anchors and test names are preserved; the count must not drop
(1111 today; the L4 tmux 10 stay).

## 3. Manifest / scripts / docs

- `rust/Cargo.toml`: `[package] name = "iota"`, `[lib]` + `[[bin]]`, the merged `[dependencies]`
  (union of today's ten tables; `[dev-dependencies]` = wiremock, tempfile, assert_cmd,
  pretty_assertions, vt100), `[features] testing = []`, `[lints]` (was `[workspace.lints]`),
  `[profile.release]` unchanged, `[workspace]` empty table NOT needed (no members).
- `spikes/ratatui-inline` and `tests-go/gofix` unchanged (standalone).
- `scripts/check-deps.sh` + `direct-deps.allow`: keep (one table now, trivially small) — or
  drop; recommend keep (still catches a stray dep).
- `scripts/check-stubs.sh`: drop the per-package table (no stub headers exist; grep for
  `todo!()` stays as a one-liner in ci.sh).
- `ci.sh`: same legs; `--workspace` flags go; the two greps of §0.5 replace the crate gates.
- Docs: ARCHITECTURE §1 (layout → module tree, this table), README build section, EVALUATION
  addendum line, DIVERGENCES rows that mention crate names (text only). REPORT-PHASE-1.md untouched.
- `rust-port-artifacts` design docs are history; not edited.

## 4. What this does NOT change

Behaviour, strings, tests' assertions, the binary's contents, the Go tree. It is a pure
re-homing refactor gated by the full ci.sh (1111 tests) and the tmux L4 leg.

## 5. Execution outline (after approval)

1. Mechanical move with `git mv`-style renames (rust/ is untracked, so plain `mv`), rewrite
   `use iota_xxx::` paths, add `mod` declarations, flip visibility (`pub` → `pub(crate)` by
   default; curated `pub` list), delete 9 manifests + workspace table.
2. Replace the macro with the blanket impl; re-home the three shared types; convert `#[path]`
   tests to unit tests; regroup `tests/`.
3. `cargo build`, clippy pedantic `-D warnings`, rustdoc, `cargo test`, ci.sh, tmux ×3.
4. Docs + scripts as §3. Log judgement calls in DEVIATIONS3 under `[MERGE]`.
