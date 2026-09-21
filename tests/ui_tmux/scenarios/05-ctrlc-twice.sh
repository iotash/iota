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
settle || bad "startup never settled"

# The banner is up: its wordmark, once (the exit gesture it used to name is the composer's own —
# Ctrl+C at idle exits, which is what the rest of this scenario proves).
check_once "the banner is up" '▀█▀ █▀▀█ ▀▀█▀▀ █▀▀█'

type_ 'stream 60'
key Enter
wait_vis 'l#05 line' || bad "the stream never started"

key C-c
wait_all 'Interrupted.' || bad "Ctrl+C was lost during the stream"
settle || bad "frame never settled after the cancel"

check_once "the first Ctrl+C cancels the turn" 'Interrupted.'
if alive; then
    ok "…and the app is still running"
else
    bad "the first Ctrl+C exited the program"
    finish
fi
a="$(hist_size)"
sleep 1.0
b="$(hist_size)"
check "the cancelled stream really stopped" "$a" "$b"
check_frame_intact "after the cancel" 80

# Idle now: no scope to cancel, so the same key exits.
key C-c
if _poll_until 60 server_gone; then
    ok "the second Ctrl+C exits the program"
else
    bad "the second Ctrl+C did not exit"
fi

finish
