#!/usr/bin/env bash
# L4 scenario 14 (T3_TEST_PLAN §7) — the out-of-band terminal channels: mode 1004 focus
# reporting and the OSC 9;4 progress indicator.
#
# `capture-pane` returns the RENDERED grid, so none of these bytes are visible to the other
# thirteen scenarios: they are instructions to the terminal, not glyphs. `pipe-pane` taps
# the pane's output bytes instead, which is the only end-to-end proof that the sequences
# `src/ui/osc.rs` spells and `src/ui/term.rs` emits on change actually leave the process —
# and, at exit, that `Term::drop` cleans up after itself.
#
# The pane runs a bare `sh` first (`start_shell`) so the tap is attached BEFORE the binary
# starts: `start` launches iota as the session command, and a `pipe-pane` issued afterwards
# races the startup bytes (mode 1004 is written before the first frame).
#
# NOT asserted here: the OSC 9 desktop notification. It is gated on the window being
# UNFOCUSED and tmux never blurs a pane, so no tmux scenario can reach the gated branch;
# `src/ui/event_loop.rs`'s vt100 tests are its pin, and docs/TUI-VERIFY.md §N carries the
# manual gate. (docs/DIVERGENCES.md T-14.)
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

start_shell 80 24 || finish
pipe_raw

type_ "$(iota_cmd openai fake)"
key Enter
wait_vis '❯' || {
    bad "the binary never came up in the shell pane"
    finish
}
settle || bad "startup never settled"

# ------------------------------------------------------------------ A: startup
check_raw "focus reporting is enabled at startup (CSI ?1004h)" '[?1004h' yes
check_raw "an idle loop advertises no progress bar (no OSC 9;4;3)" ']9;4;3' no

# ------------------------------------------------------------------ B: a turn is busy
type_ 'stream 40'
key Enter
wait_raw ']9;4;3' || bad "the busy progress state was never emitted during the turn"
ok "the turn set the terminal's progress indicator busy (OSC 9;4;3)"

wait_all 'l#39 line' || bad "the stream did not run to completion"
settle || bad "frame never settled after the turn"
wait_raw ']9;4;0' || bad "the progress indicator was never cleared when the turn ended"
ok "going idle cleared the progress indicator (OSC 9;4;0)"

# ------------------------------------------------------------------ C: exit cleans up
key C-c
key C-c
_poll_until 60 raw_has '[?1004l' || bad "focus reporting was never disabled on exit"
check_raw "exit disables focus reporting (CSI ?1004l)" '[?1004l' yes
if alive; then
    ok "the pane survives the binary (the shell is still there)"
else
    bad "the shell pane died with the binary"
fi

finish
