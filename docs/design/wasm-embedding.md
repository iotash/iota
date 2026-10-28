# Running iota in the browser: WebAssembly feasibility

Status: **Research** · 2026-10-28 · based on `wasm-feasibility` = `4742e11` (v0.7.5) · rustc 1.98.0 · reference point: fx.sh's browser build as its own docs describe it

The owner's question: iota is Rust — can it compile to WebAssembly and run in the browser the way fx.sh's site does? This memo answers from the code: `Cargo.toml`, `Cargo.lock`, the registry sources of the dependencies, `cargo tree --target wasm32-unknown-unknown`, and two probe `cargo check` runs against that target (§7). No code changes.

**Short answer.** The agent core (provider adapters, wire client, tool loop, markdown, token meter) compiles to wasm after a moderate, mostly mechanical refactor; the terminal (ratatui/crossterm), the `shell` tool, the OS sandbox, stdio MCP, Unix sockets and the on-disk session store do not and cannot — the same list fx omits. A browser build of iota is therefore a *different product* (a chat loop over a host-provided workspace), exactly as fx's is; the native CLI stays the product. Worth doing only as a website demo, in the slim shape of §5, after the first milestone of §4 settles one open question (`Send`, §6 risk 1).

## 1. What fx ships, and what it implies

Per fx's docs (as quoted in the brief): the browser build is `fx-core.wasm` + `fx-term.wasm` (~5 MB), run through JSPI, networking delegated to browser `fetch`, the terminal through xterm.js/wterm, and "the WebAssembly build omits native processes, OS sandboxing and MCP by design — the host provides transport, storage and terminal I/O". The homepage runs the real terminal, but the host supplies the workspace. JSPI is there because fx's Zig loop is synchronous: it lets a synchronous wasm call suspend on an async JS promise such as `fetch`.

What that implies for any terminal agent:

- A browser tab has no processes, no tty, no filesystem, no sockets, no environment. Everything an agent *does* to a machine is gone; what remains is the model loop and the rendering.
- "Runs in the browser" therefore names a second product shape, whose workspace, storage and transport are adapters the page provides. fx's docs say so; its site demonstrates the loop and the terminal look, not fx the tool.
- The API key lives wherever the model call is made: in the tab, or behind a proxy the operator runs.
- One point in iota's favour: iota is async end to end (`BoxFuture`, tokio futures), so it needs only `wasm-bindgen-futures`, not JSPI. The only synchronous parts — the `"iota-tui"` thread and the `spawn_blocking` calls — are exactly what a browser build omits.

## 2. Module readiness

Evidence first:

- `cargo tree --target wasm32-unknown-unknown` resolves 235 crates against 266 for the native target. `hyper`, `h2`, `rustls`, `aws-lc-sys`, `nix`, `socket2`, `signal-hook` drop out; `wasm-bindgen`, `js-sys`, `web-sys` appear. reqwest, jiff and getrandom declare browser backends in their manifests, which is why those three crates are already in `Cargo.lock`.
- What stays and should not: `tokio` with `rt-multi-thread`/`process`/`fs`/`net`/`signal` (tokio-1.54.1 `lib.rs:503-516`: `compile_error!("Only features sync,macros,io-util,rt,time are supported on wasm.")`), `mio` (`lib.rs:44`: "This wasm target is unsupported by mio. If using Tokio, disable the net feature."), `crossterm`/`ratatui`, rmcp's `process-wrap` 9.1.
- `cargo check --target wasm32-unknown-unknown --lib` stops at `mio` with 48 errors before tokio's own `compile_error!` is reached. With `--keep-going`, 160 crates check clean and exactly three leaves fail: `mio` (48 errors), `crossterm` (9: its `sys` module has no wasm arm) and `getrandom` (1: the missing `wasm_js` backend cfg). tokio sits behind mio and is never attempted; syntect, two-face, tiktoken, ruzstd, image, jiff, regex, comrak and serde_norway all pass. The native platform crates are already gone from the wasm graph; what is left to fix is tokio's feature list and those three.

Legend — **as-is**: compiles unchanged on `wasm32-unknown-unknown` · **backend**: needs a wasm implementation behind an existing seam, or a `cfg(target_family = "wasm")` split · **impossible**: no browser equivalent exists; omit, as fx does.

| module | verdict | evidence |
|---|---|---|
| `ui::facade::Ui`, `UiStreamSink` (`src/ui/facade.rs:828-838`) | as-is | A trait over `BoxFuture` (`lib.rs:29`, `Send`) and `CancellationToken`; `Ui: Send + Sync`. The TUI implements it as a mailbox — `std::sync::mpsc` requests, `tokio::sync::oneshot` replies (`ui/runtime/msgs.rs:4-9`) — so a JS-driven implementation meets the same bounds without `unsafe` (which `Cargo.toml` forbids): the page drains the mailbox and resolves the oneshots. |
| `ui::runtime` (the `"iota-tui"` thread, `inline_term`, OSC probes) | impossible | `thread::Builder::new().spawn` (`handle.rs:155`), raw mode, DSR, `event::poll` on a tty. The inline-viewport design (ARCHITECTURE §12) exists to live inside a real terminal's scrollback physics; a browser has none. A new `Ui` impl (DOM, or ANSI into xterm.js) replaces it, under `src/ui/` so `tests/layering.rs` stays true. |
| `repl::run` | backend | `tokio::spawn` ×4 (`run.rs:762,1395,1551`, `inbound.rs:98`), `spawn_blocking` (`commands/jobs.rs:238`), `std::thread` (`title.rs`), 22 `std::fs` sites (export, session commands), the jobs registry over `shell`. The loop itself is facade calls and provider futures; the spawns, timers and files need seams. |
| `cmd::interactive`, `main.rs` | impossible | `Tui::start`, the OSC-11 probe on a blocking thread, `signals::install`, the title stack, the `<home>/.iota/run/<pid>.sock` bind, `Builder::new_multi_thread().enable_all()` (`main.rs:32`). A browser entry is a second thin binary-equivalent over the library, as `main.rs` is the native one; `Env::fixed` and `HostDirs` (`app/env.rs:78-95`) are already the seams a host uses instead of process env and argv. |
| `llm::Client`, `provider::*` (reqwest) | backend | reqwest swaps its whole stack by target (`reqwest-0.13.5/Cargo.toml:245-300`): on wasm it is `fetch` through web-sys, and `http2`, `rustls`, `system-proxy` are inert. `Response::bytes_stream` exists on wasm (`wasm/response.rs:137`), so `llm::sse` reads unchanged. The wasm `ClientBuilder` offers only `user_agent`/`default_headers`: `connect_timeout` and `http2_keep_alive_*` (`llm/client.rs:629-631`) must be cfg'd out, and the `tokio::time::timeout` around `execute` (`client.rs:359,419`) needs the timer shim below. **The open question**: the wasm `Response` wraps `web_sys::Response` (`wasm/response.rs:21`) and is `!Send`, while `Provider`, `Tool`, `Dispatcher`, `Ui` and `BoxFuture` are all `Send` — §6 risk 1. Browser realities: CORS (Anthropic's API, for one, demands an explicit opt-in header for direct browser calls; a self-hosted endpoint needs CORS headers), and the key is in the tab. |
| tokio | backend | Keep `rt` (current-thread), `sync`, `macros`, `time`, `io-util`; drop the five others under `cfg(target_family = "wasm")`. `tokio::time` compiles on wasm but has no clock (`std::time::Instant::now` panics on `wasm32-unknown-unknown`): the 52 `tokio::time::` and 65 `Instant::now` sites need one `crate::sync::time` seam — std/tokio natively, a browser timer (`wasmtimer`/`web-time`) on wasm. `tokio::select!` (23 sites) is macro-only and fine; the 29 `tokio::spawn` sites need a `spawn` seam over `spawn_local`. |
| crossterm / ratatui | impossible / omit | crossterm's event source is `mio` + `signal-hook` + `rustix` under `cfg(unix)` (`crossterm-0.29.0/Cargo.toml:207-225`); nothing in it means anything in a browser. ratatui's `Buffer` and widgets are pure Rust but are reached only through `src/ui/render`, which exists to drive a terminal; ARCHITECTURE §11 measures the TUI at ≈0.4 MiB — omitted. |
| `shell::exec`, `shell::sandbox`, the `shell` tool, `process-wrap`/`nix` | impossible | `tokio::process::Command` (17 sites), `setpgid`/`killpg` (`exec.rs:284,309`), `sandbox-exec`/`bwrap` (`sandbox/darwin.rs:11`, `linux.rs:21`), Job Objects. fx omits exactly this. A remote executor over a WebSocket is a new `Tool` set (`tool/mod.rs:128`, a small object-safe trait), not a port. |
| `tool::builtins::code` (six file tools), `agents` (AGENTS.md, skills, memory) | backend | `tools.rs` (1000 lines) and `agents` (29 sites) call `std::fs` directly and walk with `ignore`; on `wasm32-unknown-unknown` `std::fs` compiles and every call fails with `Unsupported` at runtime. A workspace adapter is a second `Tool` set over a JS-provided store, or a `WorkspaceFs` trait threaded through `ToolEnv` (`tool/mod.rs:331`). The trap: on `wasm32-wasip1` `std::fs` works against a browser WASI shim, but reqwest's fetch backend is cfg'd to `target_os = "unknown"` and tokio `net` still errors — no single wasm target gives both files and fetch. |
| rmcp: stdio transport, OAuth login | impossible | `TokioChildProcess` (`mcp/transport.rs:480`), `process-wrap`; the OAuth loopback `TcpListener` (`mcp/auth.rs:594-601`). |
| rmcp: streamable-HTTP client | backend (unverified) | Built on the same `reqwest::Client` (`transport.rs:525`); rmcp's manifest cfg's only `chrono` for wasm (`rmcp-3.1.4/Cargo.toml:904`). Whether its client compiles on wasm is untested here; if it does, remote MCP servers are the one tool source a browser iota keeps. |
| `host::herdr` (Unix socket + CLI fallback), `host::cmux`, `host::background` | backend (cfg stub) | `tokio::net::UnixStream` (`herdr.rs:408`) and `tokio::process` for the CLI. Detection is env-driven and `DETECTORS` falls through to `AnsiHost` (OSC 9 through the facade), so a browser build needs only `cfg` stubs for the two hosts; the OSC fallback becomes the page's notification hook. |
| `session::sock` (agent messaging) | as-is | Already split `cfg(unix)`/`cfg(not(unix))` (`sock.rs:233-315`, "a chat on a platform without Unix sockets"): the not-unix path compiles on wasm and reports that there is no socket. |
| `session::store`/`writer`/`lock`/`meta` | backend | 84 `std::fs` sites; `File::try_lock` (`lock.rs:6`), `rename` (`meta.rs:175`), unix mode bits under cfg. Same runtime-`Unsupported` story as the code tools: the demo either runs without persistence (the store takes an injected `HostDirs`; `HomeNotDefined` already exists) or gets an IndexedDB/OPFS store behind `SessionStore`. |
| jiff (`tz-system`, `tzdb-bundle-platform`) | as-is + 1 flag | jiff pulls `js-sys`/`wasm-bindgen` on `wasm32-unknown` and bundles `jiff-tzdb-platform` on `target_family = "wasm"` (`jiff-0.2/Cargo.toml:220-231`): +202,414 B of zoneinfo (`concatenated-zoneinfo.dat`) that the macOS/Linux binaries do not carry; the `js` feature is needed for `TimeZone::system()` to read the browser's zone. |
| `rand`/`getrandom` | as-is + 1 flag | getrandom 0.3 compiles on `wasm32-unknown-unknown` only with its `wasm_js` feature and `--cfg getrandom_backend="wasm_js"` (`getrandom-0.3.4/Cargo.toml:50,74,127`): one `.cargo/config.toml` line — the one `getrandom` error of the probe. |
| image codecs (`imgterm.rs`) | as-is / omit | png/jpeg/gif/webp are pure Rust (`zune-jpeg`, `png`, `gif`, `image-webp`; no C). But the half-block rasteriser exists because a terminal cannot show pixels; a DOM UI shows an `<img>`. Keep only behind an xterm.js UI. |
| syntect + two-face (`markdown/highlight.rs`, `html.rs`, `repl/render/diff.rs`) | as-is | `fancy-regex`, pure Rust; the dumps are `include_bytes!`: `syntaxes-fancy-newlines.bin` 959,011 B + `themes.bin` 62,593 B. `CodeHighlighter` is already a seam (`markdown::highlight`), so a slim build can swap in a JS highlighter. |
| tiktoken + ruzstd | as-is | Pure Rust; `o200k_base.tkv.zst` 814,833 B embedded, inflated on the first count. |
| `config` (serde_norway), `text`, `mathtext`, `markdown`, `headless` | as-is | Pure data and rendering code. `headless::run_once`/`execute_with_tools` (`headless/run.rs:126,279`) take a provider, a dispatcher, a `QuietHost` and a `dyn Write + Send` — no terminal: the natural first wasm entry. |

## 3. Two product shapes

**A — browser demo.** fx's shape.

- Runs where: the wasm core (llm + provider + tool loop + markdown + a browser `Ui` impl, or `headless`) in the tab; the model call from the tab to the provider by `fetch`.
- Workspace: an adapter the page supplies — an in-memory file store behind a browser `code` tool set, or a remote shell over a WebSocket to a server-side sandbox. The remote-shell variant is already half of B with the loop on the wrong side: the server has the sandbox and the files, the browser has the key and the loop, and every tool call crosses the network twice.
- Keys: either the visitor pastes their own (held in the tab, as fx's host must), or a proxy with a per-session budget signs the call — and a proxy means the operator runs a server anyway.
- UI: a DOM transcript (streaming markdown → HTML; `markdown::html` already exists for `/export`) is cheaper and better than driving xterm.js with ANSI. xterm.js earns its place only if the demo must *look like* the CLI.

**B — real web agent.** iota, the native binary, unchanged, runs in a per-user sandbox (a container, plus its own `bwrap`) attached to a pty; a small bridge (ttyd, or a Rust pty–WebSocket shim) carries bytes to xterm.js in the browser. Keys stay in the server's `~/.iota.yaml`; `shell`, the sandbox, stdio MCP, sessions on disk, the agent socket and the host presenter all work because they run where they always ran. Zero changes in this repository; the work is hosting — auth, isolation, quotas, egress policy.

fx is A. iota's product — "the terminal is the interface", `shell` under an OS sandbox, `code` tools on the project, MCP servers, sessions as plain files, `iota run <agent> -m` children (README) — is B: everything a user would come for sits on the impossible rows of §2. A is a website feature; B is the only shape in which "iota in a browser tab" is still iota.

## 4. The smallest first milestone

**M1 — the loop streams in a tab.** Build `wasm32-unknown-unknown` for a `cfg(target_family = "wasm")` subset of the library: `app`, `text`, `llm`, `provider`, `tool` (the framework plus an empty dispatcher), `headless::run_once`, `markdown`. A fifty-line page: a textarea, a provider URL, a key field, a `<pre>` that streams the reply. No facade, no tools, no sessions. Steps:

1. The Cargo target split: tokio down to `rt`/`sync`/`macros`/`time`/`io-util` for wasm; rmcp, crossterm, ratatui and nix off for wasm.
2. The getrandom `wasm_js` cfg and the jiff `js` feature.
3. The `time` and `spawn` seams (~170 mechanical call sites).
4. The reqwest builder cfg (`connect_timeout`, keep-alives).
5. Whatever the `Send` question demands — the step whose size is unknown.
6. A `#[wasm_bindgen]` entry that builds `Env::fixed`, a provider and `run_once`, and streams into the page.

What it proves, and nothing cheaper does: (1) whether `!Send` fetch futures fit the `Send` trait surface or force a `MaybeSend` rewrite — the go/no-go for everything after; (2) that SSE streaming works through browser `fetch` with CORS against at least one real endpoint; (3) the measured size of the core, replacing §5's arithmetic. If M1 is green: M2 is a DOM `Ui` impl over the facade mailbox plus `repl::run` with an in-memory `code` set; M3 is persistence and polish.

## 5. Size

From ARCHITECTURE §11's measured native breakdown (stripped, `opt-level = "z"`, aarch64; the binary is 8.29 MiB in the 2026-10-26 `target/size.md`) and the files the dependencies embed:

| | full wasm core (everything that compiles) | slim core |
|---|---:|---:|
| native binary, for reference | 8.29 MiB | 8.29 MiB |
| minus what wasm never builds: aws-lc-rs ≈3.0, hyper/reqwest native ≈1.2, TUI ≈0.4, rmcp's stdio/process half | −4.6 MiB | −4.6 MiB |
| plus jiff's bundled tzdb (wasm only) | +0.2 MiB | +0.2 MiB |
| syntect + two-face (≈1.1 MiB in §11; 1.02 MiB of it tables) | kept | −1.1 MiB (JS highlighter via `CodeHighlighter`) |
| image decoders (absent from §11's list of parts ≥0.6 MiB, so under that) | kept | −0.2…0.5 MiB (the DOM decodes) |
| tiktoken table + ruzstd (≈0.87 MiB) | kept | kept (the meter is a feature) |
| **estimate, uncompressed** | **≈3.5–4.0 MiB** | **≈2.3–2.6 MiB** |
| over the wire (brotli, rule of thumb ≈3×) | ≈1.3 MiB | ≈0.8 MiB |

Caveats: wasm code density differs from arm64 (usually within ±20 % at `z`); the `web-sys` fetch shim is small but not zero; `regex` (≈1 MiB in §11) stays in both columns. Either column is under fx's ~5 MB, and M1 replaces the arithmetic with a measurement.

## 6. Effort, risks, recommendation

**Effort.**

- M1: 1–2 weeks for someone who knows the crate, dominated by the seams and the `Send` question, not by the build.
- A complete A (DOM `Ui`, in-memory workspace tool set, key handling, the page, hosting): 4–8 weeks more, plus a second implementation of the code tools to keep in step with `tools.rs`.
- B: 1–2 weeks of hosting work, no repository changes, ongoing operations.

**Top three risks.**

1. **`Send` across the trait surface.** `BoxFuture` is `Send` (`lib.rs:29`); `Ui`, `UiStreamSink`, `Provider`, `Tool`, `Dispatcher`, `Interactor` are `Send + Sync`; reqwest's wasm `Response` and futures are not, and `unsafe_code = "forbid"` rules out `unsafe impl Send`. The fixes are a cfg'd `MaybeSend` marker threaded through every `dyn` bound (invasive across 109 k lines) or a `send_wrapper` at the wire layer (sound single-threaded, ugly). Unknown until M1.
2. **A second target forever.** Every PR must keep wasm green: a sixth target in `deny.toml`, the grep gates, time/spawn seams nobody may bypass. The 2026-09-01 decision (§11) removed the feature matrix on purpose; `cfg(target_family = "wasm")` reintroduces a matrix by another name.
3. **It is not iota.** The demo has no shell, no sandbox, no stdio MCP, no files on disk, and holds the key in the tab (or behind a proxy that costs the operator money per visitor). A visitor who then installs the CLI meets a different program; a visitor who trusts the page with a key is exposed. The workspace adapter drifts from the real `code` tools.

**Recommendation.** Worth it for the website, not for the product: do M1 (two weeks; one go/no-go answer and a real size number), and build A in the slim shape only if M1 is green and the site wants a live demo. For "use iota from a browser", do B — it is hosting, and the repository is already right for it. Do not: port the ratatui/crossterm TUI to xterm.js (the §12 design is about real terminals; a DOM transcript is better and 0.4 MiB cheaper); thread a VFS trait through 215 `std::fs` sites for a demo; add cargo features (the §11 decision stands — `cfg` on the target only); ship the full two-face and image tables in the demo; or admit the wasm target to `ci.sh`'s gates before M1 has answered risk 1.

## 7. What was run

- `cargo tree --target wasm32-unknown-unknown -e normal` vs the native tree (crate counts and the platform crates present in each).
- `rustup target add wasm32-unknown-unknown`, then `cargo check --target wasm32-unknown-unknown --lib`, once plain and once with `--keep-going` (the failing leaves and the 160 clean crates above). The target is installed on the owner's machine as a side effect; nothing in the repository changed.
- Registry sources read for the claims cited by line: tokio 1.54.1, mio 1.2.2, reqwest 0.13.5, crossterm 0.29.0, rmcp 3.1.4, jiff 0.2, getrandom 0.3.4, two-face 0.5.2, tiktoken 4.1.2, jiff-tzdb 0.1.8.
- Call-site counts are `grep` over `src/` with test files excluded; module sizes from `wc -l` (109,490 lines in all).
