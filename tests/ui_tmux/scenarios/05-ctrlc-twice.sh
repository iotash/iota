#!/usr/bin/env bash
# L4 scenario 5 (TUI_TEST_PLAN §L4) — Ctrl+C mid-turn, then Ctrl+C again.
#
# The two-step emerges from one rule (`fail_waiter_interrupted`): Ctrl+C with a live cancel
# scope fires the innermost scope, Ctrl+C with none fails the parked waiter, which is the
# run loop's cue to exit. Only a real terminal delivers the keystroke as `^C` rather than a
# signal, so only L4 can tell the two apart.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

start 80 24 || finish
settle || miss "startup never settled"

# The banner is up: its mark, once (the exit gesture it used to name is the composer's own —
# Ctrl+C at idle exits, which is what the rest of this scenario proves).
check_once "the banner is up" 'ι> iota'

type_ 'stream 60'
key Enter
wait_vis 'l#05 line' || miss "the stream never started"

key C-c
wait_all 'Interrupted.' || miss "Ctrl+C was lost during the stream"
settle || miss "frame never settled after the cancel"

check_once "the first Ctrl+C cancels the turn" 'Interrupted.'
if alive; then
    ok "…and the app is still running"
else
    bad "the first Ctrl+C exited the program"
    finish
fi
a="$(view_rows)"
check_view_read "after the cancel" "$a"
# A NEGATIVE claim's window, not a wait (lib.sh rule 3): a stream that Ctrl+C failed to stop would
# insert a row every 60 ms (LINE_GAP), about sixteen inside it.
sleep 1.0
b="$(view_rows)"
check "the cancelled stream really stopped" "$a" "$b"
check_frame_intact "after the cancel" 80

# Idle now: no scope to cancel, so the same key exits. The pane is kept past the exit
# (`remain-on-exit`) so what the exit left behind can be read back.
tm set-option -t s remain-on-exit on
key C-c
if _poll_until 60 pane_dead; then
    ok "the second Ctrl+C exits the program"
else
    bad "the second Ctrl+C did not exit"
fi

# The exit writes the conversation out once (X-49's duplicate frame is gone with the inline
# terminal): the cancelled turn's last row and the banner, sixty streamed lines above it.
check_once "the last committed row is in the history once after the exit" 'Interrupted.'
check_once "…and the banner is there once too" 'ι> iota'

finish
