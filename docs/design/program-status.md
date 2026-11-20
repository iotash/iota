# Program status — OSC 7501 for any terminal that answers, not only herdr-core

Status: **design, not approved** (2026-11-20) · `main` = `dd12acd` · hc 0.1.21, tmux 3.8 ·
research: `~/.iota/reports/iota/rex-host.md` · the protocol:
`https://www.superlogical.com/rex/docs/build/program-status` (rev 0.4, read 2026-11-20).
No code changes with this document; §9 lists the tasks that make it true. "Measured" means
run on this machine on 2026-11-20 (an isolated hc core, tmux inside it); every claim about
rex is from its documentation and is marked so. §1–§9 answer the brief's nine questions in
order; §10 is what a rex install would settle; §11 the divergence rows as drafted.

## The problem

iota writes the Program Status Protocol — `ESC ] 7501 ; state=<word>:app=iota ST` — and
reads the terminal's answer to the protocol's feature query, but only inside herdr-core:
`detect_hc` returns before asking anything unless `HC_TERMINAL_ID` is set
(`src/host/hc.rs:239-240`), and the writer is a private part of `HcHost`
(`src/host/hc.rs:89-155`). The protocol is not hc's. rex authored it, and any terminal may
implement it; such a terminal gets OSC 9;4 from iota today, which cannot say "blocked on
the user", while the sequence it would understand sits one `?` away.

The gate is an environment variable standing in for a question the code already asks the
terminal. hc.rs says why the variable cannot be trusted for the answer (`:14-16`: "a
variable names an ancestor, not what is across the tty"); the same holds for the question.

## The decision

**State reporting is the terminal's capability, not a host's.** A run asks its controlling
terminal the protocol's own query once, before the first frame, and writes OSC 7501 when —
and only when — the terminal answers that it accepts the four states iota writes. A host
is what the environment names (herdr-core, herdr, cmux): it keeps identity, its own
channel if it has one, and its `<environment>` lines.

Four rules follow, each argued in its section:

1. A host with a state channel of its own (herdr's socket, cmux's CLI) wins; inside it the
   terminal is not asked and 7501 is not written (§2).
2. The answer is the only gate. No answer — a mute terminal, a multiplexer in between, no
   terminal, a background process — means not one report byte, with no environment rule
   behind it (§2, §4).
3. One reporter per controlling terminal: a run started inside another run's session does
   not ask and does not write. `HC_TERMINAL_ID` is no longer withheld from children (§6).
4. Nothing is told to the model about it (§7).

`src/host/hc.rs` shrinks to identity; rex needs no host at all to get its tab spinner, and
a later `RexHost` is `<environment>` lines only.

## 1. What moves

A new module, **`src/host/program_status.rs`**, in the `host` layer where `hc.rs` is: it
names `crate::ui::facade::Ui` (the in-band writer) and `crate::ui::runtime::osc`
(the round trip), both below it, so `LAYERS` and `KNOWN_UPWARD` in `tests/layering.rs` do
not change. It takes from `hc.rs`, unchanged in bytes:

| piece | today | note |
|---|---|---|
| `word`, `report` | `hc.rs:78-95` | the four words and `state=clear`; `HC_APP` becomes `APP` |
| `PROBE_QUERY`, `PROBE_ANSWER`, `answered` | `hc.rs:69-71,258` | `answered` becomes `accepts`: it also reads `states=` (§3) |
| `probe_tty` | `hc.rs:252` | gains the nested-run rule (§6) |
| `Sink`, the `/dev/tty`-on-first-report writer | `hc.rs:98-155` | the whole in-band lifetime: `Tty` → `Write`/`Off`, `Ui` after `write_through` |
| `impl StateReporter`, `Closer`, `InBand` | `hc.rs:195-215` | on a new `ProgramStatus` host, `name() = "program-status"` |

`ProgramStatus` is a `Host` with exactly those three capabilities. It is not a detected
host: it is built at assembly beside the detected one and handed to the presenter on the
**terminal side**, in front of the ANSI host. `Presenter::over(detected, fallback, notify)`
takes the terminal's hosts as a list — `[ProgramStatus?, AnsiHost]` interactively,
`[ProgramStatus?]` in a `-m` run — and nothing else in the presenter changes: the first
`StateReporter` takes the state, so OSC 9;4 is not written where 7501 is; the ANSI host
keeps the OSC 9 ping; `close` runs every closer in reverse. `detected_host()` (the
banner's `in <host>`) names what a detector matched, as now.

What **`hc.rs` keeps**: `HC_NAME`, `HC_TERMINAL_ID`, `TERM_PROGRAM`, `detect_hc` (its
logic is not touched: the id, then the terminal's answer, then `TERM_PROGRAM` when the
terminal cannot be asked), the `hc` message envelope, and the `<environment>` lines.
`HcHost` is `{ terminal }` — no sink, no `StateReporter`, no `Closer`, no `InBand`, no
`ChildEnvironment` (§6). hc has no socket iota uses; nothing of that kind exists to keep.

`Probe::program_status` stays the injected closure; the real one (`cmd::host_probe`)
remembers its answer, so the hc detector and the terminal-side rule ask the tty once
between them.

## 2. Detection

**When.** Once per run, at assembly (`cmd/mod.rs:506`, where `host::detect` already
runs), for the interactive chat and for `-m` alike — before the screen exists, because the
round trip reads the tty itself and must not share it with the event loop
(`osc.rs:116-118`). Never again while the run lives: a terminal does not start or stop
speaking a protocol under a running program, and the loop cannot read a reply without
losing keys.

**The rule**, after `host::detect` has returned the innermost host or none:

| detected host | terminal asked? | 7501 written | state goes to |
|---|---|---|---|
| herdr, cmux (own `StateReporter`) | no | no | the host's channel |
| herdr-core | already, by `detect_hc` | iff it accepted | 7501, else OSC 9;4 |
| none | yes | iff it accepted | 7501, else OSC 9;4 |

So nothing reports twice by construction: `ProgramStatus` is not built when the detected
host has a state channel, and where it is built it shadows OSC 9;4 in the presenter. A
terminal inside herdr or cmux is not asked at all — no new bytes reach a pane whose
handling of the query is unknown (herdr's is: `herdr-core-host.md` §2).

**The three outcomes** of the query (`ESC ] 7501 ; ? ST` chased by `ESC [ 6 n`):

- **Answers** — the reply carries `ESC ] 7501 ; ?` and the states it lists (or the
  protocol's default five, when it lists none) include `working`, `blocked`, `idle` and
  `error`: the terminal reads the protocol. `Some(true)`. Measured, hc 0.1.21:
  `ESC]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth ESC\`
  then `ESC[1;1R`, first byte after 0.2 ms.
- **Mute** — the DSR's reply alone: a terminal, or a multiplexer standing in for one,
  that does not read 7501. `Some(false)`. Measured: tmux 3.8, 0.1 ms.
- **Garbage or nothing** — bytes that are not the answer (a key typed inside the window,
  a reply past the 256-byte cap, an answer that omits one of the four states) count as
  mute; no `R` within 100 ms, no controlling terminal, not the terminal's foreground
  process group, a nested run (§6), or Windows is `None`.

Only `Some(true)` builds the writer. **`None` no longer falls back to the environment for
writing** — today `HC_TERMINAL_ID` with `TERM_PROGRAM != herdr` writes blind when the
terminal could not be asked (`hc.rs:243`). That rule stays for hc's *identity* (which
`<environment>` lines the model reads) and goes for the bytes: without a terminal there
was nothing to write to; after a timeout the terminal is not one that answers in 0.2 ms;
and a background process writing the root record is the bug §6 exists for. The one
behaviour lost: `iota -m … &` from an interactive shell in an hc terminal stops reporting.
It does not own the terminal.

**A host-specific detector also matched.** herdr-core: `<environment>` says
`host: herdr-core` and `hc terminal: <id>`; the state goes out as 7501, as today. herdr or
cmux: their lines, their channel. No host: `<environment>` gains nothing (§7).

**Residual, recorded.** An anonymous 7501 terminal started from a herdr pane or a cmux
surface inherits `HERDR_*` / `CMUX_SURFACE_ID`; the environment detector matches, the
terminal is not asked, and the state goes to the stale pane. hc solved this for itself by
being asked first (`DETECTORS`); an anonymous terminal has no name to be asked under, and
asking inside every herdr pane and cmux surface is the change this design declines. For
rex the later `RexHost` (identity, §7) takes hc's place in front of herdr.

## 3. The state machine

Unchanged from the hc host; the presenter's inputs (deduplication, a running job holds
`Busy`, an open dialog is `NeedsInput`) are untouched.

| the chat | report | when |
|---|---|---|
| ready for input | `state=idle:app=iota` | the loop's first `Idle`, once the banner is up; after every turn; after an interrupt |
| a turn runs, or a job | `state=working:app=iota` | `Busy`, and `Idle` with a job running or its notice owed |
| blocked on the user | `state=blocked:app=iota` | an approval, an interactive tool, any open dialog |
| the turn failed | `state=error:app=iota` | `Error`, until the next state |
| exits | `state=clear` | `Presenter::close`, both modes |

Every report carries `app=iota` because a report replaces its record whole. No `kind`, no
`msg`, no `title`, no `progress`: `State` carries none of them.

**Clearing.** *Idle* is a report, not a clear: the record says a program is here and at
rest. *Interrupt* (Esc on a turn) lands as the loop's `Idle` — the protocol asks for
exactly that ("When the user interrupts or cancels a program, report `idle`"). *Exit* is
`state=clear` with no `id`, which removes every record on the terminal; it rides
`Presenter::close`, and this design adds no exit path of its own. A run that dies without
closing (SIGKILL) leaves its record to the protocol's lifetime rule: the terminal MUST
drop `working` and `blocked` when the attached process exits or a shell prompt begins
(OSC 133 `A`). Ctrl+Z leaves the record as it is; not changed here.

**A terminal that answers but does not accept a state.** The protocol ignores a whole
report whose `state` it does not recognise, so a terminal without `error` would keep
showing `working` after a failed turn. The reply's `states=` list is how a program finds
out. iota's rule is all or nothing: the writer is built only when the four words are
accepted; otherwise the terminal sees no 7501 at all and gets OSC 9;4. A fallback table
(`error` → `idle`, `blocked` → `working`) was considered and left out: no terminal that
needs one exists, and the base five "do not change in later revisions".

## 4. The tmux question

Measured (tmux 3.8 inside an isolated hc 0.1.21 core; `hc get` read the record):

| tmux option | a pane writes | result |
|---|---|---|
| default (`allow-passthrough off`) | the query | DSR only — tmux answers for itself, 0.1 ms |
| default | a report, plain or DCS-wrapped | dropped; `reported: false` |
| `allow-passthrough on` | a DCS-wrapped report (`ESC P tmux; … ESC \`) | reaches hc; the record is set |
| `on` | a DCS-wrapped query | hc's reply comes back **after** the DSR's `R` |
| `on` | a wrapped report from a pane that is not visible | dropped |
| `all` | the same | reaches hc and replaces the visible pane's record |

So 7501 does not pass through tmux by default, and can be made to with an option the user
must set. iota does **not** wrap, for three reasons the table shows: the option is off by
default and iota cannot read it from inside a pane; the reply trails tmux's own DSR
answer, so the read that ends at `R` (`osc.rs:157-159`) would return "mute" and leave the
reply in the input; and the outer terminal has one root record for all of tmux's panes —
with `all`, a hidden pane's `blocked` replaced the front pane's state, and either pane's
exit `clear` removes the other's.

**The conservative branch needs no tmux rule.** iota never looks at `$TMUX` for this: it
asks, tmux answers the DSR alone, the outcome is mute, and the pane keeps OSC 9;4 — the
shipped behaviour, pinned by
`tests/host/hc.rs::inside_tmux_inside_hc_the_terminal_keeps_the_progress`. "Cannot tell"
is §2's `None`: nothing is written. A tmux that one day implements the protocol per pane
will answer the query, and iota will report to it with no change.

Not measured: tmux older than 3.8, GNU screen, zellij (expected mute or `None`; either is
safe), and tmux inside rex.

## 5. The probe's cost and safety

**Mechanism — reused, not rewritten.** `osc::round_trip(PROBE_QUERY)`: `/dev/tty` opened
read+write, refused before any byte when this process is not the terminal's foreground
process group (raw mode from the background is `SIGTTOU`, measured), raw mode, the query
and `ESC[6n` in one write, then bytes read one at a time, each only after `select(2)` said
one is there, up to the `R` or `REPLY_TIMEOUT` (100 ms). Past the deadline nothing is in
flight: no thread on the tty, no read a later key would complete
(`osc::tests::a_mute_terminal_times_out_and_keeps_the_next_byte`). The DSR stays the
terminator although the protocol suggests `CSI c`: every terminal answers both, and the
shared exchange ends at `R`.

**Cost.** One more round trip before the first frame where there was none: a plain
terminal, interactive or `-m`. Measured 0.2–0.5 ms in hc and 0.1 ms in tmux; over SSH it
is one network round trip, next to the OSC 11 one the chat already makes. The 100 ms is
paid only by a terminal that answers neither sequence — `script(1)`, a bare pty in CI —
once per run. No cost where it does not run: inside herdr or cmux, a nested run, no
controlling terminal (a pipe, cron), a background process, Windows, and every command
that is not a run (`iota mcp list --probe` asks only with `HC_TERMINAL_ID`, as today).
Writing the two queries in one exchange would save the second round trip; it is left out
because OSC 11 is asked later, in the interactive branch, after the host's own background
probe.

**Is a probe in every run acceptable?** Yes, on those numbers, with two accepted risks:

- **A late reply.** The existing matcher (`requery::ReplyMatcher`) knows `ESC ] 11 ;`
  only. A 7501 answer that arrives after the deadline reaches the interactive loop as
  `Alt+]` and then `7501;?:states=…` typed into the composer (crossterm has no OSC event);
  in a `-m` run it stays in the tty's queue for whoever reads next. It takes a terminal
  that speaks 7501 *and* is more than 100 ms away. hc answers where the pty is; rex's
  server holds the pty and is expected to (unmeasured, §10). If a real terminal shows it,
  the fix is to arm the same matcher with the 7501 prefix for its grace second after a
  missed probe (§12).
- **Another reader of the tty in a `-m` pipeline** (`iota -m … | less`): the reply may be
  read by the other program. hc terminals have this today; it becomes true of every
  terminal. The protocol allows the alternative — writing without asking — and this
  design declines it: an unknown OSC in a terminal that never said it reads it is what
  the hc host was written to avoid (`hc.rs:8-9`).

## 6. Children

Today's rule is an environment variable: `HC_TERMINAL_ID` is withheld (X-80), so a nested
`iota -m` finds no id, does not ask, does not write. A protocol that needs no variable
needs another rule, and the nested run is a real threat to it — measured for hc
(`herdr-core-host.md` §4): a child that reports replaces the chat's record and its exit
`clear` removes it.

Two kinds of child exist. The shell tool's commands get a process group of their own
(`shell/exec.rs:292`), so `round_trip` refuses them before a byte is written, and with
§2's "`None` writes nothing" they are already safe. MCP stdio servers are spawned without
one (`mcp/transport.rs:477`): an `iota` started by a server is in the foreground group,
would ask the tty the chat's event loop is reading, and would report.

**The rule: one reporter per controlling terminal.** Every run gives its children
`IOTA_STATUS_SESSION=<its session id>` (`getsid(0)`; Unix — Windows has no probe to
guard), through the mechanism that gives
`IOTA_SOCKET` (`ChildEnv::giving`, which must hold more than one pair). A run whose own
session id equals the variable's is running on the terminal of the run that set it: it
does not ask (`probe_tty` returns `None` first) and so does not write. Anything else — no
variable, another session — asks as usual.

The session id, not a flag, because a controlling terminal belongs to a session. A flag
(`IOTA_NESTED=1`) would also silence a worker the model starts in a terminal of its own
(`hc run … iota -m`, `tmux new-window iota`) wherever the new terminal inherits the
client's environment — tmux does not; hc and rex are unmeasured — and a worker that shows
no state is the regression this feature must not cause. A new pane is a new session, so
the comparison fails there and the worker reports.

**`HC_TERMINAL_ID` is no longer withheld**, and the `hc note` line leaves `<environment>`.
Both reasons it was withheld (`hc.rs:24-28`) are now covered without it, and withholding
cost the model its own terminal id: `hc run` lost its parent. A nested run in an hc
terminal now detects hc by the environment rule, reads `hc terminal: <id>`, and reports
nothing. herdr's `HERDR_ENV` stays withheld — a different reason (its hooks), not touched.

Not prevented, as today: a child that writes 7501 on its own (Claude Code does), and two
unrelated programs sharing one terminal.

## 7. What the model reads

**Nothing new.** A terminal that answers with no host detected adds no `<environment>`
line and the banner stays a plain terminal's. The block carries facts the model can act
on — an id a CLI takes — and every line is paid for in every request; "the tab shows your
state" is neither. The probe's outcome goes to `tracing::debug!` for the person asking
why a tab is not spinning.

A **rex pane** needs, beyond the state, what a herdr pane has: `host: rex`,
`rex block: <REX_BLOCK>`, `rex session: <REX_SESSION>`, so the model can `rex split` or
`rex capture` beside itself. That is a separate, later task (`rex-host`, §9): an identity
host like the new `HcHost`, detected by `REX_BLOCK` and the terminal's answer, placed in
front of herdr. Under this design it contains no state code, and the research's risk 2
(withhold `REX_BLOCK` or not) is gone: nothing needs withholding.

## 8. Tests and the pin

**Without a real rex** — unit, no terminal:

- The byte pin moves with the writer:
  `host::program_status::tests::each_state_is_one_exact_report_and_close_clears`,
  `…::the_facade_takes_over_the_writes`.
- `…::the_reply_is_read_for_the_answer_and_its_states`: hc's measured bytes; the bare
  `ESC]7501;?ESC\`; a BEL-terminated reply; a `states=` list without `error` (mute); the
  DSR alone; empty.
- `…::the_probe_over_a_socket_pair`: the `osc::tests::fake_tty` pattern (`exchange`
  becomes `pub(crate)`): the query and its DSR leave in one write; an answering end, a
  DSR-only end, a silent end that costs the deadline and keeps the next byte.
- `…::the_terminal_is_asked_only_when_no_host_reports_state`: §2's table through the
  assembly function, counting calls — herdr and cmux never ask, hc and the terminal side
  ask once between them.
- `…::a_run_in_its_parents_session_does_not_ask` (session id injected).
- `host::hc::tests::the_detection_matrix_yields_one_host_each` gains a "writer" column;
  `children_are_spawned_without_the_id` is deleted;
  `hc_names_itself_and_the_terminal_and_nothing_else` pins the two lines.
- `host::tests`: the presenter over `[hc, program-status, terminal]` — state to 7501, ping
  to OSC 9, both closed, `detected_host() == Some("herdr-core")`.

Real terminals already on a developer machine:

- `tests/host/hc.rs` (isolated core, skipped without `hc` unless `IOTA_HC_REQUIRED=1`):
  the eight existing tests keep passing; new
  `a_terminal_that_answers_is_reported_to_without_its_id` runs iota under
  `env -u HC_TERMINAL_ID` — hc standing in for an anonymous 7501 terminal — and reads
  `working` from `hc get` with no `in herdr-core` on the banner;
  `a_nested_headless_iota_leaves_the_chat_s_record` now also asserts the child saw the id.
- `tests/cmd/cli.rs::cli_children_carry_the_status_session`: a real run's shell call and
  MCP server both see the variable, and `IOTA_SOCKET` is still there.
- `host::program_status::tests::a_real_terminal_that_answers_nothing_gets_no_report`:
  `-m` under `script(1)` (`ui::testutil::under_script`) — the typescript holds the query
  once and no `state=`.
- `tests/ui_tmux`: unchanged; every scenario now makes the round trip in tmux (mute).

**Needs a real terminal — unmeasured, not guessed:** everything about rex (§10); how
each mainstream emulator treats the *query* (ghostty outside hc, iTerm2, Terminal.app,
kitty, WezTerm, Alacritty, VS Code, the Linux console, Windows Terminal over SSH). All are
expected to drop the unknown OSC and answer the DSR. The check, in each, before task 3
ships (bytes other than `ESC [ … R` in the dump, or anything left on the screen, is a
finding):

```
stty raw -echo min 0 time 3; printf '\033]7501;?\033\\\033[6n'; dd bs=256 count=1 2>/dev/null | od -c; stty sane
```

## 9. Tasks

Every task's gate is `ci.sh`'s:

```
cargo fmt --check
cargo clippy --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
IOTA_TMUX_REQUIRED=1 IOTA_SANDBOX_REQUIRED=1 cargo test
cargo build --release
```

plus `cargo test <name>` passing for each test it names, and, for tasks 1–3,
`IOTA_HC_REQUIRED=1 cargo test --test host` on a machine with `hc`. "Artifact" is the one
module a task is about; its tests, its CHANGELOG line, its DIVERGENCES row (§11) and the
paragraphs of `docs/ARCHITECTURE.md` and `docs/design/herdr-core-host.md` it makes untrue
travel with it. Each task leaves the product working.

| # | slug | kind | artifact | depends on |
|---|---|---|---|---|
| 1 | `program-status-module` | refactor | `src/host/program_status.rs` | — |
| 2 | `program-status-nested` | fix | `src/app/env.rs` | 1 |
| 3 | `program-status-any-terminal` | feature | `src/cmd/mod.rs` | 2 |
| 4 | `program-status-site` | docs | `~/Work/iota-website/docs/hosts.md` | 3 |
| 5 | `rex-host` | feature, after §10 | `src/host/rex.rs` | 3, a rex install |

**1. `program-status-module`** — §1: the writer, the probe and the reply reader move;
`HcHost` becomes identity; `Presenter::over` takes the terminal's hosts as a list; the
writer is built at assembly **only when hc was detected and the terminal accepted** — hc's
behaviour and no other terminal's, except §2's `None` (no blind write) and §3's
`states=`. Done when the gate passes and these exist:
`host::program_status::tests::{each_state_is_one_exact_report_and_close_clears, the_facade_takes_over_the_writes, the_reply_is_read_for_the_answer_and_its_states, the_probe_over_a_socket_pair}`,
`host::hc::tests::the_detection_matrix_yields_one_host_each` (with the writer column),
and `grep -n 7501 src/host/hc.rs | grep -v '//'` prints nothing.

**2. `program-status-nested`** — §6: `ChildEnv` gives a list; every run gives
`IOTA_STATUS_SESSION`; `probe_tty` returns `None` in the giver's session; hc's
`ChildEnvironment` and `HC_CHILD_NOTE` are removed. Lands before 3 so no terminal ever
has the generic writer without the rule. X-80 amendment. Done when the gate passes and
these exist:
`host::program_status::tests::a_run_in_its_parents_session_does_not_ask`,
`host::hc::tests::hc_names_itself_and_the_terminal_and_nothing_else`,
`tests/cmd/cli.rs::cli_children_carry_the_status_session`,
`tests/host/hc.rs::a_nested_headless_iota_leaves_the_chat_s_record` (asserting the id);
`host::hc::tests::children_are_spawned_without_the_id` is deleted.

**3. `program-status-any-terminal`** — §2: the assembly rule loses "hc was detected" and
becomes the table — any terminal that accepts, unless the detected host reports state
itself. The new X row. Done when the gate passes, §8's emulator check has been run in the
terminals the owner uses and its results are in the row, and these exist:
`host::program_status::tests::{the_terminal_is_asked_only_when_no_host_reports_state, a_real_terminal_that_answers_nothing_gets_no_report}`,
`tests/host/hc.rs::a_terminal_that_answers_is_reported_to_without_its_id`.

**4. `program-status-site`** — `hosts.md` gets a section "Program status (OSC 7501)"
between the plain terminal and herdr-core, holding the state table and the "asks the
terminal once" paragraph; the herdr-core section keeps the envelope and the terminal id
and loses the withheld-id paragraph; `system-prompt.md` loses the `hc note` line; Nesting
says where the terminal is and is not asked. Done when, in `~/Work/iota-website`,
`npm run build` and `npm run typecheck` pass and `grep -rn "hc note" docs` is empty.

**5. `rex-host`** — §7. Not startable until §10 is measured; its shape depends on rex's
reply and on what a pane inherits.

Order: 1 → 2 → 3 → 4; 5 when a rex is at hand.

## 10. What cannot be decided without a rex install

Not decidable from the documentation, each with what it would change:

1. **Whether rex answers the query at all.** The spec says a supporting terminal MUST;
   "In Rex" does not say rex does. If it does not, this design gives rex nothing, and the
   owner must choose between writing blind (the spec permits it) and a `REX_BLOCK`
   detector as the gate.
2. **Whether rex notifies twice.** rex documents a notification from a record change
   *and* from OSC 9. iota would send `blocked` and the ANSI host's OSC 9 ping. If both
   fire, `ProgramStatus` must take the `Notifier` capability and say nothing, as herdr
   does; if hc then loses a ping it relies on, the two terminals want opposite things and
   the owner decides.
3. **Where the query is answered for a remote client**, and so whether §5's late reply is
   real.
4. **What a pane inherits** — from the server (the hc/herdr collision, research risk 1)
   and from the client that asked for it (§6's reason for the session id).
5. **How `done` and `error` left behind are shown** (§12).

A 30-minute session on such a machine, in order:

| min | do | settles |
|---|---|---|
| 5 | `env \| grep -E 'REX\|TERM'`; §8's one-liner, timed, locally and from a second device | 1, 3; the reply's bytes; `TERM_PROGRAM` |
| 5 | by hand: `printf '\033]7501;state=working:app=iota\033\\'`, then `blocked`, `error`, `idle`, `done`, `clear`; watch the tab, the picker, `rex events` | what each state looks like; 5 |
| 5 | `blocked` plus `printf '\033]9;hello\007'` with the window in the background | 2 |
| 5 | the branch's `iota` after task 3: a turn, an approval, a failed turn, exit; then `iota -m` | the feature, end to end |
| 5 | tmux inside rex: the one-liner; `rex split` from a shell with a marker variable set, `env` in the new pane; a rex server started from a herdr pane | §4's last line; 4 |
| 5 | `kill -9` an iota mid-turn under a shell with and without OSC 133; `rex send` into an open approval | the stale record; research risk 3 |

## 11. Divergences (drafts — the PM applies them)

- **X-80** — `Amended <date>: herdr-core withholds nothing. `HC_TERMINAL_ID` was withheld
  so a nested `iota -m` would not ask the chat's terminal and report over its record;
  both are now prevented without the variable (the new row), and the model's shell has
  its terminal id again (`hc run` finds its parent). The `hc note` line is gone from
  `<environment>`. herdr still withholds `HERDR_ENV`. `ChildEnv::giving` holds a list:
  `IOTA_SOCKET` and `IOTA_STATUS_SESSION`. Pinned by
  `host::hc::tests::hc_names_itself_and_the_terminal_and_nothing_else`,
  `tests/cmd/cli.rs::cli_children_carry_the_status_session`,
  `tests/host/hc.rs::a_nested_headless_iota_leaves_the_chat_s_record`.`
- **New row (next free id)** — *program status is the terminal's* | Go: no OSC 7501;
  iota until `<date>`: written only inside herdr-core, by `HcHost`, gated on
  `HC_TERMINAL_ID` | A run asks its controlling terminal `ESC]7501;?` once at assembly
  (`host::program_status`, the OSC 11 round trip, ≤100 ms) unless the detected host
  reports state itself (herdr, cmux) or the run is in the session of the run that
  started it (`IOTA_STATUS_SESSION`). Only an answer that accepts `working`, `blocked`,
  `idle` and `error` builds the `ProgramStatus` host, which sits in front of the ANSI
  host: 7501 instead of OSC 9;4, the OSC 9 ping unchanged, `state=clear` on close. No
  answer writes nothing — no environment rule, no DCS wrapping for tmux. `HcHost` is
  identity, the envelope and `<environment>`. `<environment>` and the banner say nothing
  about it. | Decided `<date>` (docs/design/program-status.md). The protocol is rex's, not
  hc's; the variable gated a question the code already asked the terminal. Measured: hc
  0.1.21 answers in 0.2 ms; tmux 3.8 answers the DSR alone, drops a report, and with
  `allow-passthrough` returns the reply after its own DSR and shares one record among
  its panes. Pinned by `host::program_status::tests::*` and
  `tests/host/hc.rs::a_terminal_that_answers_is_reported_to_without_its_id`.
- `docs/ARCHITECTURE.md` — the `host/` row (the module list, "the herdr-core host (OSC
  7501 in band…)") and the sentence on the two startup probes follow the same wording.
- `docs/design/herdr-core-host.md` — status line: §2's writing rule, §3 and §4's
  withheld id are superseded by this document.

## 12. Deferred

- **`done` / `error` left behind when a `-m` run ends.** The protocol recommends it ("so
  a record is left behind for the user to find") and rex's session picker is built on it;
  today the run clears. It needs the presenter to be told how a run ended, and a look at
  how hc and rex show `done` (§10).
- **`kind` on `blocked`** (`permission` for an approval, `question` for `ask`): `State`
  would have to carry a reason.
- **The late-reply matcher** (§5), if a terminal shows the need.
- **One exchange for OSC 11 and 7501** (§5).
- **A fallback table for a partial `states=`** (§3).
- **Reporting through tmux** with `id=<pane>` child records (§4): only once a terminal
  shows child records usefully.
