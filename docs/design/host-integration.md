# Terminal host integration (internal/host)

Status: **Shipped** · Depends on: internal/ui (the ANSI fallback wraps its facade)

iota signals its lifecycle to whatever terminal environment ("host") it
runs inside: a progress indicator while a turn works, a warning state while
it is blocked on the user, and attention pings when something finishes or
needs approval while the user is away. Different hosts speak different
protocols — the plain terminal reads ANSI escapes, cmux ignores them and
listens to its own CLI, herdr takes JSON lines on its socket, herdr-core reads
its own OSC 7501 off the terminal — so the signals are abstracted once and
adapted per host.

## Shape

```
repl / -m ──▶ host::Presenter ──▶ the FIRST detected host, then the fallback
                                    ├── herdr-core (detected: HC_TERMINAL_ID + the tty answers OSC 7501)
                                    ├── herdr      (detected: HERDR_ENV=1 + pane id + socket)
                                    ├── cmux       (detected: CMUX_SURFACE_ID + CLI on PATH)
                                    └── ANSI       (always present, always last; interactive only)
```

- **Semantic layer** (`host.go`): `State` (Idle / Busy / NeedsInput / Error)
  and `Event` (an attention ping whose `Text` is always a content digest —
  the answer's first line, the error headline, the tool asking — never a
  fixed phrase). The chat loop emits these at its turn anchors and knows
  nothing about escapes or CLIs.
- **Capability interfaces**: a detected `Host` advertises what it can display
  by implementing `StateReporter`, `Notifier`, and/or `io.Closer` — the same
  optional-interface pattern the provider layer uses (`UsageReporter`,
  `Tunable`, …).
- **Per-capability fallback**: the `Presenter` resolves each signal
  independently — first detected host implementing the capability wins, the
  ANSI fallback covers the rest. cmux implements only `StateReporter`, so
  inside cmux the sidebar shows state while notifications still ride the
  ANSI OSC 9 path (which cmux's ingress surfaces as desktop notifications).
- **Detection registry**: `DETECTORS` in `src/host/mod.rs`, innermost host
  first, probing through the injectable `Probe` (environment + `PATH`
  lookup). Only the FIRST match is kept (the innermost host is exclusive,
  2026-09-21): a nested host inherits its parent's variables, and reporting
  through them lands on the wrong surface. The order is `[hc, herdr, cmux]`:
  - hc before herdr — an hc terminal started from a herdr pane inherits the
    pane's `HERDR_*`; a herdr pane (or a tmux) inside an hc terminal does not
    answer the OSC 7501 query the hc detector asks the terminal, so it falls
    through;
  - herdr before cmux — a herdr pane inside a cmux window inherits the
    server's `CMUX_SURFACE_ID`.
  Adding a host = one file + one entry at the place its nesting dictates,
  and a row in the detection matrix (`host::hc::tests`).

## Hosts

**ANSI** (`ansi.go`) — the default. State rides the ConEmu OSC 9;4 progress
protocol through bubbletea's `View.ProgressBar` (the renderer diffs states
and clears on exit; Ghostty 1.2+, Windows Terminal, iTerm2 3.6.6+, kitty
0.47+ render it). Busy → indeterminate, NeedsInput → warning at 100, Error →
error at 100. Notifications are one write of OSC 9 (desktop notification)
plus a BEL, emitted only while the terminal is UNFOCUSED — focus reporting
(`View.ReportFocus`) is an ANSI-host mechanism, so the gate lives in the ui
model.

**cmux** (`cmux.go`) — detected via the `CMUX_SURFACE_ID` env var cmux
injects into every pane plus its CLI on PATH. State drives the sidebar
through the public CLI: `set-status iota … --icon … --color …` (the
icons/colors mirror what cmux's own Claude Code integration sends, so the
row reads identically) and `workspace loading on|off --id iota` for the
spinner. A single worker goroutine executes commands off the chat loop; a
capacity-1 last-wins mailbox coalesces bursts so a stale state never lands
late; every call is best-effort with a 2s timeout — sidebar dressing must
never slow or fail a turn. `Close` clears the row and spinner (they would
outlive the process) and flushes bounded.

**herdr** (`src/host/herdr.rs`) — the multiplexer. The pane's lifecycle
(`working` / `blocked` / `idle`; `Error` is `idle`), the session and the
release go to herdr's Unix socket as JSON lines, from one worker over a
bounded ordered queue. It also tells the model the pane, and withholds
`HERDR_ENV` from children (below).

**herdr-core** (`src/host/hc.rs`, docs/design/herdr-core-host.md) — the
terminal core behind herdr's desktop app, a separate product from herdr. Its
state channel is IN BAND: OSC 7501 written to the terminal
(`state=working|blocked|idle|error:app=iota`, `state=clear` on exit), and
only when the hc detector matched — which takes `HC_TERMINAL_ID` AND the
terminal's own answer to `ESC ] 7501 ; ? ST` (chased by a DSR, the OSC 11
detect's round-trip; a variable names an ancestor, not what is across the
tty). It is the one `InBand` host: the presenter hands it the facade
(`show_turn_on`) and the UI loop writes the sequence between draws; a `-m`
run has no facade, so it writes `/dev/tty` itself. It withholds
`HC_TERMINAL_ID` from children (below). No `Notifier` (OSC 9 stays the
terminal's), no session.

## Contracts

- The `Presenter` is driven from the chat-loop goroutine only; hosts own any
  internal concurrency. It dedups repeated states (command dispatch
  re-asserts Idle liberally; a host may pay per update).
- The config `notify: false` switch is Presenter policy: it silences every
  host's `Notify`. Presence policy ("is anyone watching") is HOST-local: a
  host implementing `Notifier` takes over the whole attention decision —
  ANSI gates by terminal focus, a config-hooks host would not gate at all.
- Notification text carries no program prefix — the host names the sender
  (macOS shows Ghostty/cmux, cmux attributes by pane), and banner space is
  scarce. The OSC 9 `4;`-prefix collision (such a payload parses as a
  progress report) is defused at the MECHANISM: the ui model prepends a
  space, so arbitrary content digests stay safe by construction.

- **What a host knows about its environment stays in the host** (Rust,
  2026-10-18, DIVERGENCES X-80). A host whose variables would let a CHILD of
  the chat speak to it as the chat implements `ChildEnvironment`: it names
  the variables, and the core only applies the gathered `ChildEnv` where it
  spawns — the shell tool's commands and the MCP stdio servers, both. herdr
  withholds `HERDR_ENV` (its agent hooks report only when it is `1`, and a
  nested `claude -p` otherwise claims iota's pane for good) and tells the
  model so in `<environment>`; herdr-core withholds `HC_TERMINAL_ID` (a
  nested `iota -m` would otherwise probe the chat's tty and clear its record)
  and tells the model to name the terminal and pass `hc run --parent <id>`;
  cmux and the ANSI terminal withhold nothing.
  The host is detected once, at assembly, because both children need the
  names before the presenter exists.

## Future hosts the design anticipates

- **tmux**: OSC must be wrapped in DCS passthrough to cross tmux — a tmux
  host implementing BOTH `StateReporter` and `Notifier` with wrapped
  sequences (bypassing tea's ProgressBar field) is the only place that fix
  can live.
- **User-config hooks**: "on needs-input run this command" is a Host adapter
  built from config, slotting into the same registry.
- **cmux deep integration**: registering as a cmux vault agent unlocks real
  lifecycle (`set_agent_lifecycle` over the control socket: hibernation,
  needs-input aggregation) — at that point cmux suppresses raw OSC
  notifications, and its adapter must implement `Notifier` via `cmux notify`.
