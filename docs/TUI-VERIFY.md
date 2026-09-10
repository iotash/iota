# iota-rs — manual TUI verification gate

**Status: REQUIRED before any release. The binary always carries the TUI (one binary since
2026-09-01). Nothing in this file is optional, and nothing in it is automated — that is the
point.**

The test pyramid stops one step short of the user. L1–L3 prove logic and orderings without a
terminal, L2b proves the byte shape the loop emits (`vt100`), and L4 drives a real terminal
under tmux 3.7c (`tests/ui_tmux/main.rs`, `IOTA_TMUX=1 cargo test --test
ui_tmux`). What none of them can reach:

- **an input method editor.** tmux has no IME. A preedit buffer is drawn by the *emulator*,
  over the cells the app owns, positioned at the cursor the app last set. The only way to
  know that iota's cursor is where an IME expects it is to type Chinese, Japanese or Korean
  into it and look.
- **the emulator's own scrollback policy.** tmux 3.7c preserves lines scrolled out of a
  partial scrolling region (wart W9). Whether Terminal.app, kitty or a given VS Code build
  does is a per-emulator fact — and since there is no fallback build any more, an emulator
  that discards them is a defect to fix in `src/ui`, not a platform to build differently.
- **flicker, and how a redraw *feels*.** A frame can be byte-perfect and still strobe.
- **reflow.** A rewrapping emulator rewraps history the app has already handed over; tmux
  does not rewrap at all, so the whole class is invisible to L4.

Run every section below on every terminal in the matrix. Record what you saw, not what you
expected.

---

## How to run a session

```sh
cargo build --release                       # the one binary; the TUI is always in
target/release/iota openai -k "$KEY" -M <model>     # or any configured provider
```

For the scrollback and flicker sections a scripted provider is easier than a real one; the
L4 mock (`tests/ui_tmux/mock.rs`) answers `stream 30` / `stream 100` with
exactly that many numbered lines. Start it from a scratch test and point `-u` at it, or use
any provider that will produce a long, boring answer on demand.

Terminals in the matrix, in the order they matter:

| # | terminal | why it is in the list |
|---|---|---|
| 1 | Ghostty | the primary target; fast, and the CJK cell-diff artifact T-32 guards against was seen here |
| 2 | Terminal.app | the macOS floor: no synchronized output, conservative scroll handling |
| 3 | iTerm2 | the most common macOS third-party terminal |
| 4 | kitty | its own scrollback and graphics model |
| 5 | Alacritty | GPU renderer, minimal feature set |
| 6 | VS Code integrated terminal | xterm.js — a different implementation family entirely |
| 7 | xterm | the reference implementation the escape sequences are written against |

---

## 1. IME / CJK composition (the reason this file exists)

The composer's real cursor is set every frame from
`Composer::cursor_pos` — display columns through the grapheme ruler, not byte or `char`
counts. An IME anchors its candidate window to that cursor. If the two disagree, the
candidate window lands somewhere else on the screen and the user cannot see what they are
typing.

For each terminal:

- [ ] **1.1 Basic composition.** Switch to a CJK input method. Type `nihao` / `にほん` /
      `한국` without committing. The preedit underline renders at the prompt, and the
      candidate window is anchored **at the cursor**, not at column 0 and not at the top of
      the screen.
- [ ] **1.2 Commit.** Accept a candidate. The committed text replaces the preedit, the
      cursor advances by two columns per wide rune, and no cell is left half-painted.
- [ ] **1.3 Mid-line composition.** Move the cursor into the middle of an existing CJK
      draft (←/→) and compose there. The candidate window follows the cursor.
- [ ] **1.4 Multi-row draft.** Build a draft long enough to wrap to 2–5 composer rows
      (`MAX_COMPOSER_ROWS` is 5), then compose on the last row. The candidate window is
      anchored to the correct *row*, and the frame does not bounce.
- [ ] **1.5 Composition while output streams.** Start a long answer, then compose while
      lines are being inserted above the frame. The preedit is **stable**: inserts must not
      clear it, move it or steal the cursor. (This is the one that fails first — inserts
      move the physical cursor, and only the next `draw` restores it: wart W4.)
- [ ] **1.6 Paste during composition.** With a preedit open, paste (⌘V / Ctrl+Shift+V).
      Nothing is lost; the preedit either commits first or survives intact.
- [ ] **1.7 Resize with a preedit open.** Resize the window while composing. No crash, no
      duplicated frame; the preedit may be dropped by the IME (acceptable), but the
      composer's committed text must survive.
- [ ] **1.8 Terminal.app bottom-row scenario.** Terminal.app historically mispositions the
      candidate window when the cursor is on the very last row of the window. This should
      be **unreachable by construction**: the status row always sits below the composer, so
      the cursor is never on the bottom row. Confirm it — resize to a short window and try.

## 2. Partial-region scrollback — LOAD-BEARING (wart W9)

The build scrolls a *partial* region (`DECSTBM` + `CSI S`) to make room for inserted history
without repainting the screen. Whether the lines that scroll out of that region reach the
emulator's scrollback is emulator-specific and **not negotiable at runtime** — and, since
2026-09-01, not at build time either: the `tui-portable` fallback (whole-screen scrolling via
newlines) was removed with the rest of the feature model. **A failing emulator is a bug to
fix, not a build to switch to.** That is exactly why this check stays.

For each terminal:

- [ ] **2.1** Stream 30+ lines in one turn.
- [ ] **2.2** Scroll up (mouse wheel / ⌘↑ / Shift+PageUp). Is the FIRST line of the answer
      still reachable? Is the history contiguous — no gaps, no duplicated rows?
- [ ] **2.3** If any line was discarded: record it as a **DEFECT** — the terminal, its
      version, and the observation — and fix it in the `ui` module's insert path
      (`src/ui/{region,term}.rs`). There is no other build to ship there.

> ✅ **The wide-rune defect is FIXED** (VERIFY pass; it was open when this file was written).
> Layer 4 found it deterministically: every double-width grapheme in a padded USER-ECHO row
> inserted as the rune plus a spurious space, so `❯ 中文一行` rendered `❯ 中 文 一 行`.
> Cause: a wide grapheme OWNS the cells to its right and a covered cell reads back as a
> space; ratatui's buffer diff drops those cells, but a backend handed the WHOLE buffer
> (`Terminal::insert_before`'s `draw_lines` path) prints them. `LoopBackend::draw`
> (`src/ui/term.rs`) now drops the covered cells, the same rule ratatui's own
> diff and `TestBackend` apply. Pinned by `tests/vt100_semantics.rs::wide_runes_insert_intact`
> (part of `cargo test`). Still worth an eyeball here: the pin is a byte
> assertion, not a look.

## 3. Flicker (Ghostty and Terminal.app are mandatory)

There is no synchronized-output mode in this build. A 100-line stream is the stress case.

- [ ] **3.1** Stream ~100 lines. Watch the *frame*, not the text: does the composer row, the
      separator pair or the status line visibly blink, tear or jump?
- [ ] **3.2** Open and close `/model` a few times mid-stream. A surface open changes the
      frame height, which recreates the inline viewport and clears (warts W1/W3) — one full
      repaint per height change is expected; a *sustained* flicker is not.

## 4. Resize reflow — count the orphans

A rewrapping emulator rewraps rows the app already handed to history, while the app tracks
its own viewport top. Some emulators will strand a row above the new viewport. This is an
**accepted cost** (the Go spike had the same class), but the budget must be known.

- [ ] **4.1** Start a stream, resize the window wider mid-stream, let it finish. Count
      orphaned/duplicated rows in the scrollback. Record the number.
- [ ] **4.2** Same, narrower.
- [ ] **4.3** Resize at idle, both directions. There should be **no** orphan here — the
      resize pass (autoresize → clear → draw → DSR resync, wart W5) runs before any insert.
- [ ] **4.4** After every resize: exactly one composer row, separators at the new width,
      the status line present. (L4 asserts this under tmux; confirm it where reflow is
      real.)

Budget: **≤ 2 orphaned rows per mid-stream resize** is accepted. More than that on a given
terminal is a finding.

## 5. Window title (the title stack)

- [ ] **5.1** On start the window/tab title becomes the session title (or the model while
      no title exists). Composed CJK titles render correctly.
- [ ] **5.2** On a clean exit (Ctrl+C twice) the title is **restored** to what it was before
      — the title stack is pushed (`ESC [ 22 ; 0 t`) before the loop and popped
      (`ESC [ 23 ; 0 t`) after the facade releases the terminal.
- [ ] **5.3** After a crash or a `kill -9`, the title is expected to stay stale. Confirm it
      is only stale, not corrupted.
- [ ] **5.4** Terminals that ignore the title stack (some VS Code builds) should simply not
      restore. That is not a finding; record it.

## 6. Small terminals and the automated pins worth an eyeball

L4 pins these under tmux (`tests/tmux/scenarios/10-edge-pins.sh`); they are listed here
because the *look* of them is what a user reports.

- [ ] **6.1 Short window (< 12 rows).** The frame's floor used to be about ten rows — four
      staging tail rows, the spacer, two separators, the composer and the status line — and
      below roughly twelve rows the inline viewport and the inserted history overlapped,
      permanently damaging the scrollback rows they overlapped (T-40). **Now guarded**: the
      staging window's cap is dynamic (`Region::tail_keep`), trimmed on a short terminal so
      the frame always leaves `max(2, screen_h/2)` rows above it, and a height change
      re-applies it immediately. At 10 rows nothing stages and the frame is its 5-row
      minimum. Confirm on each terminal: at 8, 10 and 12 rows, stream 12 lines — the frame
      stays intact, history is contiguous, there is no crash and no lost input, and growing
      the window back refills the staging window.
- [ ] **6.2 Exact-width line.** A line exactly the terminal's width must occupy exactly one
      row with its last column intact (T-01). Verify at 80 and at one unusual width.
- [ ] **6.3 Wide rune at the boundary.** A CJK run that would straddle the last column wraps
      the whole rune to the next row — no half-painted cell, no lost row.
- [ ] **6.4 Emoji and flags.** A table containing emoji, flag sequences and VS16 characters
      renders with aligned borders. (The width ruler and the emulator's cell accounting are
      two different rulers; this is where they disagree.)
- [ ] **6.5 Oversized paste.** Paste a file larger than the screen. The composer shows a
      one-row `[#1 … N lines]` tag; the submitted transcript echo stops at 20 lines with a
      `… +N more lines` row; the model receives the whole thing.

## 7. Host channels (T3 — the only T3 surface automation cannot reach)

Everything else T3 shipped is pinned by tests. These two channels are not: `cmux` has no CI
binary, and tmux never blurs a pane, so focus-gated notification has unit pins only. Both are
written from the ui event-loop thread (`src/ui/term.rs`), so a wrong byte here is invisible
until a human looks at a real terminal.

- [ ] **7.1 Terminal progress (OSC 9;4).** Start a turn: the terminal's own progress
      indicator must turn on (`\x1b]9;4;3\x07` — Ghostty draws a bar, Windows Terminal a tab
      ring). At an approval prompt it must go to the warning state (`;4;4;100`). On exit it
      must clear (`;4;0`) — check by quitting mid-turn as well as after an idle turn.
- [ ] **7.2 Desktop notification (OSC 9), focus-gated.** With the window **unfocused**,
      finishing a turn must ring the bell and raise a desktop notification carrying the
      answer's first line. With the window **focused**, the same turn must stay silent. Verify
      both directions on each terminal — the gate is `\x1b[?1004h` focus reporting, and a
      terminal that does not implement it will notify while focused.
- [ ] **7.3 `notify: false`.** With the provider's `notify: false` in the config, 7.2 must
      produce no bell and no notification while everything else is unchanged.
- [ ] **7.4 cmux surface.** With `CMUX_SURFACE_ID` set and `cmux` on `PATH`: the sidebar row
      keyed `iota` reads `Running` (bolt, blue) during a turn, `Needs input` (bell) at an
      approval, `Idle` (pause, grey) after, and disappears on exit. **No OSC 9;4 bytes may be
      written there** (the cmux host owns the channel), and the code theme must follow a cmux
      light/dark switch between turns.

## 8. Background-job notices (phase C — a wake-up automation cannot stage)

A finished background job enters the conversation through the facade's input queue
(`Ui::enqueue`). The queue laws are unit-pinned (`ui::event_loop::queue_tests`) and the loop's
two arrivals are covered at the REPL level (`tests/repl/jobs.rs`), but nothing automated shows
what the arrival LOOKS like on a real terminal — the L4 mock provider cannot emit a tool call,
so no tmux scenario can start a job.

Set up once: an agent with `tools: {shell: {sandbox: off, auto_run: true}}`, and ask the model
to run something slow in the background (`sleep 20; echo done`).

- [ ] **8.1 Idle wake-up.** With the job running, sit at the prompt and type NOTHING. When the
      job ends, one dim line must appear —
      `[background job b1 finished: exit 0 after 20s] sleep 20; echo done` — followed
      immediately by a normal turn (the model answers it). No `❯` block, no bell of its own.
- [ ] **8.2 The draft survives.** Repeat 8.1 but leave a half-typed line in the composer while
      the job finishes. The notice must land, the turn must run, and the draft must still be
      there, cursor where you left it, when the turn ends.
- [ ] **8.3 Mid-turn arrival.** Start a job, then start a long turn (a streamed answer or a
      tool loop). The notice must appear at a ROUND boundary — after the running activity group
      settles, never inside a call's rows — as the same dim line, and the model must react to it
      in the same turn.
- [ ] **8.4 Queued while typing ahead.** Start a job, then type two messages ahead without
      waiting. When the job ends its headline must appear as a `»` queue row among them, and
      pressing ↑ must recall YOUR newest line, stepping over it.
- [ ] **8.5 ESC keeps the job.** Start a job, start a turn, press ESC. The turn ends, the queue
      folds back into the composer as usual — and the job must still be running (its notice
      arrives later). Then `/quit`: the job must be gone (`ps` for the command).

## 9. Sign-off

Dogfood until dry: one report → one fix → repeat, ranked as the Go migration ranked them.

| tier | class | bar |
|---|---|---|
| 1 | crash / corruption / lost input | must be fixed before release |
| 2 | fidelity — wrong glyph, wrong position, lost history row | must be fixed or written into `DIVERGENCES.md` |
| 3 | polish — flicker, spacing, an orphaned row within budget | recorded, may ship |

### Results

Fill one row per terminal per release. An empty cell means *not yet verified* — never
assume a pass.

| terminal | version | §1 IME | §2 scrollback | §3 flicker | §4 orphans | §5 title | §6 edges | §7 host | §8 jobs | verdict |
|---|---|---|---|---|---|---|---|---|---|---|
| Ghostty | | | | | | | | | |
| Terminal.app | | | | | | | | | |
| iTerm2 | | | | | | | | | |
| kitty | | | | | | | | | |
| Alacritty | | | | | | | | | |
| VS Code | | | | | | | | | |
| xterm | | | | | | | | | |

**Automated coverage on this host, for the record:** tmux 3.7c, `IOTA_TMUX=1 cargo test
--test ui_tmux` against the one binary — ten scenarios, **184 assertions,
0 FAIL, 0 WARTS, five consecutive green runs** (two inside `ci.sh`, three standalone) —
where WP52 recorded 182 assertions with a WART on §6.1. That WART is gone: the short-window
pin now reports `60x10: the frame and all 12 rows survived intact` (T-40 CLOSED).

There is no second binary to run it against: the `tui-portable` fallback was removed on
2026-09-01. The L2b suite (`cargo test --lib ui::event_loop::vt100_tests`, part of
`cargo test --workspace`) carries the scroll-region byte proof and the wide-rune pin from
§2.3.

That covers §2's mechanism (history contiguous, every row exactly once), §4.4, §6.1, §6.2,
§6.3 and §6.5 under tmux only. It covers **none** of §1, §3 or §5, and tmux does not
rewrap, so it covers none of §4.1–§4.3 either.
