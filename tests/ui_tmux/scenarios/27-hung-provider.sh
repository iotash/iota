#!/usr/bin/env bash
# L4 #27 — a provider that stopped answering (docs/history/request-cancellation-recon.md).
#
# The mock's `hang:` script sends a turn's `200` head and then nothing, and gives the session-title
# pass no answer at all. Three fresh panes:
#
#   1. The busy row tells the phases apart (`Waiting for the first token` once the head is in), ESC
#      interrupts, and the NEXT message goes out at once. Before the fix the loop sat in the title
#      join for the whole `TITLE_TIMEOUT` (30 s) with an empty cancel stack: no echo, no busy row,
#      ESC and Ctrl+C both dead.
#   2. The same, then Ctrl+C at idle: the program exits at once instead of after the title timeout.
#   3. `IOTA_STREAM_IDLE_TIMEOUT=2`: the silent stream fails on its own after two seconds with the
#      `Response stalled` block naming the knob, and is NOT retried.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

# Well under the 30 s title timeout, well over a slow runner's echo.
FAST=80 # tries of 100 ms

# ---- 1. ESC, then the next message ----
start 100 24 || finish
settle || bad "startup never settled"

type_ 'hang:one'
key Enter
wait_vis 'Waiting for the first token' || bad "the busy row never said the head was in"
check "the head arrived, so the row no longer says 'Waiting for the model'" \
    "$(count_vis 'Waiting for the model')" 0

key Escape
wait_all 'Interrupted.' || bad "ESC never reached the turn"
type_ 'second'
key Enter
if _poll_until "$FAST" _all_has 'echo: second'; then
    ok "the next message went out at once — no wait on the hung title pass"
else
    bad "the next message waited on the title pass"
fi

# ---- 2. ESC, then Ctrl+C at idle ----
start 100 24 || finish
settle || bad "startup never settled"
tm set-option -t s remain-on-exit on

type_ 'hang:two'
key Enter
wait_vis 'Waiting for the first token' || bad "the second pane's turn never got its head"
key Escape
wait_all 'Interrupted.' || bad "ESC never reached the second pane's turn"
settle || bad "frame never settled after the interrupt"
key C-c
if _poll_until "$FAST" pane_dead; then
    ok "Ctrl+C at idle exits at once — no wait on the hung title pass"
else
    bad "the exit waited on the title pass"
fi

# ---- 3. the stream idle bound ----
# The tmux server is started by this script and inherits its environment.
export IOTA_STREAM_IDLE_TIMEOUT=2
start 120 24 || finish
settle || bad "startup never settled"

type_ 'hang:three'
key Enter
wait_vis 'Waiting for the first token' || bad "the third pane's turn never got its head"
if _poll_until "$FAST" _all_has 'Response stalled'; then
    ok "the silent stream failed on its own at the idle bound"
else
    bad "the idle bound never fired"
fi
check_once "the error names the knob" 'set IOTA_STREAM_IDLE_TIMEOUT=<seconds>'
check "a mid-stream stall is not retried" "$(count_all 'retrying (attempt')" 0
check "it is an error, not an interrupt" "$(count_all 'Interrupted.')" 0

finish
