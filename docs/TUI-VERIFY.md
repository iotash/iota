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

Sections 1–8 are that matrix. **Section 9 is Windows Terminal's own list** — a separate
section rather than an eighth row, because what differs there is the mechanism (console input,
no `/dev/tty`, a job object instead of a process group), not the emulator. None of it has been
run.

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
      (`src/ui/{render/region,runtime/term}.rs`). There is no other build to ship there.

> ✅ **The wide-rune defect is FIXED** (VERIFY pass; it was open when this file was written).
> Layer 4 found it deterministically: every double-width grapheme in a padded USER-ECHO row
> inserted as the rune plus a spurious space, so `❯ 中文一行` rendered `❯ 中 文 一 行`.
> Cause: a wide grapheme OWNS the cells to its right and a covered cell reads back as a
> space; ratatui's buffer diff drops those cells, but a backend handed the WHOLE buffer
> (`Terminal::insert_before`'s `draw_lines` path) prints them. `LoopBackend::draw`
> (`src/ui/runtime/term.rs`) now drops the covered cells, the same rule ratatui's own
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
written from the ui event-loop thread (`src/ui/runtime/term.rs`), so a wrong byte here is invisible
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
(`Ui::enqueue`). The queue laws are unit-pinned (`ui::runtime::event_loop::queue_tests`) and the loop's
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

## 8b. The `/model` combo box — NOT YET RUN

**Status: not one item below has been executed.** Added with the combo box (MIGRATION-ROADMAP
Phase 1b §10); L1 pins the key ladder (`src/ui/surface/tests.rs`), L3 the command
(`tests/repl/commands.rs`) and L4 the real-terminal shape (`tests/ui_tmux/scenarios/15-model-combo.sh`).
What is left for a human is the half a capture cannot see: where the REAL cursor sits in a
field that shares its row with a hint, and what an IME does over it.

Set up once — an agent whose candidate set mixes sources, one of which cannot answer:

```yaml
providers:
  openai: {key: ${env:OPENAI_API_KEY}}
  relay:  {type: openai, key: x, url: "http://127.0.0.1:9"}   # nothing listens: the failing source
models:
  gpt5: openai:gpt-5.2
agents:
  default: {models: [gpt5, "openai:*", "relay:*"]}
```

- [ ] **8b.1 The field is open from the first frame.** `/model`: the Model tab's last row is
      `❯` + a dim `model name (e.g. gpt-4o)` placeholder + the hint (`N models · ↑↓ move · Enter
      select · Esc cancel`). The real terminal cursor must sit in the FIELD, immediately after
      the `❯` — not in the composer above and not on the list.
- [ ] **8b.2 Typing filters, and `/` is a character.** Type `gpt`: the list narrows live and the
      hint counts what it keeps (`2 of 14`). Type `/` (as in `anthropic/claude-3.5-sonnet`): it
      must go INTO the field — no search prompt opens, nothing else claims it.
- [ ] **8b.3 The typed row.** With text that matches no row exactly, the last row reads
      `use "…" as typed` and is navigable. Type a name nothing matches at all: that row must be
      the ONLY row left (not the whole list again) and the cursor must be on it, with the hint
      reading `Enter use typed`.
- [ ] **8b.4 Arrows move the list, ←→ move the text.** ↑↓ (and Ctrl+P/N) walk the rows while the
      field keeps its text; ←→ (and Ctrl+A/E) move the text cursor INSIDE the field — watch the
      real cursor, which is the only thing that shows the difference.
- [ ] **8b.5 Enter, both ways.** Enter on a listed row switches to it; Enter on the typed row
      switches to what you typed. Each prints exactly one `Model switched to …`, and the status
      row follows.
- [ ] **8b.6 ESC cancels the surface.** With text in the field, ESC must close the whole surface
      (not just clear the field) and restore the pane exactly — no switch, no ghost rows. `q` is
      a character here and must type.
- [ ] **8b.7 The failing source.** With `relay:*` in the set, opening `/model` must still list
      everything else, with `relay: …` as the panel's dim prompt row — and NOTHING printed over
      the transcript before the surface opened.
- [ ] **8b.8 ESC during the fetch.** Press ESC while `Fetching available models from 2
      providers` is up: the command must abandon quietly and at once — no waiting out the dead
      endpoint's timeout, no surface afterwards.
- [ ] **8b.9 IME in the field.** With a CJK input method, compose into the combo field: the
      preedit must render at the field's cursor (§1's law, in a one-line field that shares its
      row with a hint), committing must filter the list, and the typed row must carry the
      composed text verbatim.
- [ ] **8b.10 A row from another provider.** Pick a `relay:…` row while the session runs on
      `openai`: one line must say the session keeps its endpoint and name the `iota run … -M
      relay:…` that starts one there — and the model must NOT change.

## 8c. `NO_COLOR` — NOT YET RUN

**Status: not one item below has been executed.** Added with the color switch (MIGRATION-ROADMAP
§3 #2; DIVERGENCES X-27, X-28). L1 pins the parser (`src/app/color.rs`) and the frame's byte→cell gate
(`src/ui/render/spans.rs`), L3 scans a whole scripted run for escapes (`tests/nocolor/main.rs`) and L4
reads a committed row back from a real terminal and greps the raw byte stream
(`tests/ui_tmux/scenarios/16-nocolor.sh`). What is left for a human is legibility: whether a frame
with no color is still a frame you can use.

```sh
NO_COLOR=1 target/release/iota          # then the same session with TERM=dumb
```

- [ ] **8c.1 The frame reads.** With `NO_COLOR=1`, the separators are faint, the `❯` prompt is
      plain, the status row is plain, and a committed user block is still reverse video. Nothing
      is invisible and no row is painted in a color.
- [ ] **8c.2 The chat is bare.** Ask for a markdown reply with a heading, a list, a table and a
      fenced code block: every row is plain text — no bold heading, no faint bullet, no syntax
      colors — and the layout (indents, borders) is unchanged.
- [ ] **8c.3 A diff is legible without its shading.** Run a tool that edits a file: the `+`/`-`
      rows arrive as `NNN + code` / `NNN - code` with no background block, aligned as before.
- [ ] **8c.4 The surfaces still show their cursor.** `/model`, `/tools`, `/session`: the cursor
      row keeps its `▸` marker and a focused tab chip is still reverse video, so every surface
      is navigable with no cyan anywhere.
- [ ] **8c.5 The input field has an edge.** `/model`'s combo field and `/file`'s field have no
      background tint now; the `❯` and the hint must still make the field's extent obvious.
- [ ] **8c.6 Images keep their pixels.** An image reply renders its half-blocks in color — the
      one thing `NO_COLOR` deliberately does not strip.
- [ ] **8c.7 `TERM=dumb` behaves the same.** Repeat 8c.1–8c.3 with `TERM=dumb` instead of the
      variable (the emulator's own TERM is what the app sees, so set it on the command line).

## 9. Windows Terminal — NOT YET RUN

**Status: not one item below has been executed.** There is no Windows machine here, and
`tests/ui_tmux` needs tmux, so the entire Windows surface is unverified — both by automation
and by hand. This section exists so that the first person with a Windows box has a list rather
than a hunch, and so nobody mistakes "Windows compiles in CI" for "the TUI works on Windows".

Since 2026-09-13 a release carries an `x86_64-pc-windows-msvc` binary, so this section joins
the gate the first time a Windows release is announced.

The terminal is **Windows Terminal** (the default console host on Windows 11 and the only one
with a working VT implementation). `conhost.exe` — what you still get from the classic
`cmd.exe` shortcut or from a Windows Server image — is explicitly **out of the matrix**: run
the list there if you like, but a failure that reproduces only on `conhost` is recorded, not
fixed.

Sections §1–§8 apply to Windows Terminal too and are worth running, but this list comes first:
it is the set where the *mechanism* differs from Unix rather than the emulator.

- [ ] **9.1 It starts at all.** `iota.exe` with a configured provider reaches the composer,
      the separators are drawn at the window width, and one turn round-trips. Anything short
      of that makes the rest of this list moot — record where it stopped.
- [ ] **9.2 ANSI and color.** Colors, bold and dim render as colors, bold and dim — not as
      literal `←[0m` text. Windows Terminal enables `ENABLE_VIRTUAL_TERMINAL_PROCESSING` on
      its own, but crossterm also falls back to a WinAPI path when it believes ANSI is
      unavailable, and that path implements only a subset. If escape bytes appear as text,
      record the exact bytes: that says the fallback is in play, which would affect §9.7 too.
- [ ] **9.3 The 256-color / truecolor palette.** Run something with a diff or a syntax block.
      Colors should match what the same output looks like on macOS, not collapse to the
      16-color set.
- [ ] **9.4 Cursor positioning.** The text cursor sits at the composer's insertion point at
      rest, after an arrow-key move, and after a wrap to a second composer row — not at column
      0, not one row below the frame. This is what §1 tests for an IME; here the question is
      just whether the cursor lands where the app put it.
- [ ] **9.5 Wide characters.** A line of CJK text occupies exactly two columns per glyph, the
      right-hand separator stays flush, and a run that would straddle the last column wraps
      whole (§6.3's check, on this terminal). Windows Terminal and the grapheme ruler must
      agree on the width or every line after the first is off by one.
- [ ] **9.6 Emoji, flags and VS16.** §6.4's table, here. Emoji that Windows Terminal renders
      single-width while the ruler counts them double (or the reverse) shows up as a ragged
      right edge; record the exact characters, not "emoji are broken".
- [ ] **9.7 Bracketed paste — the one with a known mechanism gap.** Paste a **multi-line**
      block into the composer. On Unix the terminal wraps it in `ESC[200~`/`ESC[201~` and
      crossterm delivers one `Event::Paste`, so the newlines stay inside the draft. Crossterm
      0.29 reads Windows console input through the WinAPI reader, which produces **no**
      `Event::Paste` at all (`EnableBracketedPaste::execute_winapi` is a hard
      `ErrorKind::Unsupported`), so the paste may instead arrive as ordinary key events —
      and every embedded newline would then read as Enter, i.e. **send** each line as its own
      message. Record exactly what happened: one draft, or N submitted turns. N submitted
      turns is a **tier 1** finding.
- [ ] **9.8 Oversized paste.** §6.5's check on this terminal: paste a file larger than the
      screen and confirm the composer shows a placeholder rather than the whole text.
- [ ] **9.9 Ctrl+C at idle.** At the prompt with no turn running, Ctrl+C interrupts the parked
      waiter; a second one exits. The process must exit *cleanly* — raw mode off, cursor
      shown, bracketed paste off — leaving a usable shell prompt, not a terminal that echoes
      nothing.
- [ ] **9.10 Ctrl+C mid-turn.** During a streamed answer, Ctrl+C cancels the turn and returns
      to the composer without killing the process; the partial answer stays in history.
- [ ] **9.11 Ctrl+D.** Identical to Ctrl+C in both states (`ui/input/keys.rs` treats `Char('c')` and
      `Char('d')` as one row). On Windows there is no EOF convention behind Ctrl+D, so this is
      purely a key binding — confirm the console host does not swallow it first.
- [ ] **9.12 Ctrl+C reaches a running command.** Start a long `shell` call, press Ctrl+C.
      Unix kills the process group; Windows uses a job object (`process-wrap`). Confirm the
      child is actually gone — check Task Manager or `Get-Process` — and that no console
      window flashed up while it ran (`CREATE_NO_WINDOW`).
- [ ] **9.13 Resize, wider.** Start a stream, widen the window mid-stream, let it finish.
      Count orphaned rows as §4 does. Windows Terminal reflows its buffer on resize, which
      tmux does not do at all, so this is the first place the whole reflow class is even
      visible — expect findings here and write down what you see rather than a verdict.
- [ ] **9.14 Resize, narrower.** The same, narrowing. Narrowing is the direction that
      rewraps history the app has already handed over.
- [ ] **9.15 Resize at idle.** Both directions with no turn running: exactly one composer row
      at the new width, separators at the new width, no orphan.
- [ ] **9.16 Very narrow window.** Drag to roughly 40 columns. The frame must degrade, not
      corrupt: no panic, no rows drawn past the edge.
- [ ] **9.17 The window title.** §5, here. The tab title becomes the session title on start
      and is **restored** on a clean exit. crossterm emits OSC 0 through its ANSI path but
      calls `SetConsoleTitleW` through the WinAPI fallback; either is acceptable, but a tab
      still named `iota` after `/quit` is a finding. Note which of the two you think ran —
      §9.2 tells you.
- [ ] **9.18 The background detect declines, quietly.** `osc.rs::open_tty` returns
      `ErrorKind::Unsupported` on Windows (there is no `/dev/tty`, and `conhost` answers OSC
      11 with nothing), so `detect_background` takes its documented default: **dark**. What
      this item is checking is that declining is *silent and harmless*: no error printed at
      startup, no 100 ms stall before the first frame, no stray `ESC]11;?` bytes echoed into
      the buffer, and adaptive shades that look right on Windows Terminal's dark default.
- [ ] **9.19 …and wrong on a light theme.** Switch Windows Terminal to a light profile and
      start again. The UI will still assume dark, because nothing on Windows asks. Record how
      bad it is — legible-but-not-pretty is a tier 3 note; unreadable text is a tier 2 finding
      that argues for a Windows background detect via `CONIN$`/`CONOUT$` rather than a
      per-terminal excuse.
- [ ] **9.20 Terminal progress and notifications.** §7.1 and §7.2 on this terminal: Windows
      Terminal implements OSC 9;4 (the taskbar progress ring) and OSC 9 (a toast). Both are
      emitted unchanged from Unix — confirm the ring appears during a turn, turns to the
      warning state when iota waits for you, and clears on exit.
- [ ] **9.21 Git Bash vs PowerShell.** Run once on a machine **with** Git for Windows and once
      **without** (or with `IOTA_SHELL` forced). The `shell` tool's description must name the
      interpreter that actually ran, and the call header's chained `cd` must be in that
      shell's dialect. This is X-17/X-18 in `DIVERGENCES.md` — unit-tested on macOS, never
      once observed on Windows.
- [ ] **9.22 Background jobs.** §8's list, here. The log path under the Windows temp
      directory, the notice arriving at idle, and `/quit` killing the job (§8.5) all go
      through the job-object path rather than `killpg`.

### Windows results

| terminal | Windows build | version | §9.1–9.6 | §9.7–9.8 paste | §9.9–9.12 keys | §9.13–9.16 resize | §9.17–9.19 title/theme | §9.20–9.22 host/shell | verdict |
|---|---|---|---|---|---|---|---|---|---|
| Windows Terminal | | | | | | | | | |

An empty cell means *not yet verified*. Every cell in this table is empty on purpose.

## 10. Sign-off

Dogfood until dry: one report → one fix → repeat, ranked as the Go migration ranked them.

| tier | class | bar |
|---|---|---|
| 1 | crash / corruption / lost input | must be fixed before release |
| 2 | fidelity — wrong glyph, wrong position, lost history row | must be fixed or written into `DIVERGENCES.md` |
| 3 | polish — flicker, spacing, an orphaned row within budget | recorded, may ship |

### Results

Fill one row per terminal per release. An empty cell means *not yet verified* — never
assume a pass.

| terminal | version | §1 IME | §2 scrollback | §3 flicker | §4 orphans | §5 title | §6 edges | §7 host | §8 jobs | §8b combo | §8c no color | verdict |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Ghostty | | | | | | | | | | |
| Terminal.app | | | | | | | | | | |
| iTerm2 | | | | | | | | | | |
| kitty | | | | | | | | | | |
| Alacritty | | | | | | | | | |
| VS Code | | | | | | | | | |
| xterm | | | | | | | | | |

**Automated coverage on this host, for the record:** tmux 3.7c, `IOTA_TMUX=1 cargo test
--test ui_tmux` against the one binary — ten scenarios, **184 assertions,
0 FAIL, 0 WARTS, five consecutive green runs** (two inside `ci.sh`, three standalone) —
where WP52 recorded 182 assertions with a WART on §6.1. That WART is gone: the short-window
pin now reports `60x10: the frame and all 12 rows survived intact` (T-40 CLOSED).

There is no second binary to run it against: the `tui-portable` fallback was removed on
2026-09-01. The L2b suite (`cargo test --lib ui::runtime::event_loop::vt100_tests`, part of
`cargo test --workspace`) carries the scroll-region byte proof and the wide-rune pin from
§2.3.

That covers §2's mechanism (history contiguous, every row exactly once), §4.4, §6.1, §6.2,
§6.3 and §6.5 under tmux only. It covers **none** of §1, §3 or §5, and tmux does not
rewrap, so it covers none of §4.1–§4.3 either. It covers none of §9 in any sense: tmux does
not run on Windows, and the Windows CI job builds and unit-tests the crate without ever
opening a terminal.
