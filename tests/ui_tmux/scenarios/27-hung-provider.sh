#!/usr/bin/env bash
# L4 #27 — a provider that stopped answering (docs/history/request-cancellation-recon.md).
#
# The mock's `hang:` script sends a turn's `200` head and then nothing, and gives the session-title
# pass no answer at all; `slowtitle:` hangs the title pass alone; `openend:` answers in full through
# `[DONE]` and then holds the body open. Six fresh panes:
#
#   1. The busy row tells the phases apart (`Waiting for the first token` once the head is in), ESC
#      interrupts, and the NEXT message goes out at once. Before the fix the loop sat in the title
#      join for the whole `TITLE_TIMEOUT` (30 s) with an empty cancel stack: no echo, no busy row,
#      ESC and Ctrl+C both dead.
#   2. The same, then Ctrl+C at idle: the program exits at once instead of after the title timeout.
#   3. `IOTA_STREAM_IDLE_TIMEOUT=2` (a test hook, read once at the binary edge; a user waits the
#      constant five minutes): the silent stream fails on its own after two seconds with the
#      `Response stalled` block, which names no knob, and is NOT retried.
#   4. The answer arrives at once and only the title pass hangs: the next message still goes out at
#      once — the loop never waits on a name (it waited the whole 30 s before).
#   5. The same, then Ctrl+C at idle: the exit is immediate, the placeholder name standing.
#   6. Under the same 2 s bound, a complete answer whose body never closes is a SUCCESS: the stream
#      ends at `[DONE]`, not at an EOF that never comes (it failed as `Response stalled` before).
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
check_once "the error says the request was not retried" 'the request was not retried'
check "the error names no knob" "$(count_all 'IOTA_STREAM_IDLE_TIMEOUT')" 0
check "a mid-stream stall is not retried" "$(count_all 'retrying (attempt')" 0
check "it is an error, not an interrupt" "$(count_all 'Interrupted.')" 0

# ---- 4. a hung title pass alone, then the next message ----
start 100 24 || finish
settle || bad "startup never settled"

type_ 'slowtitle:four'
key Enter
wait_all 'echo: slowtitle:four' || bad "the answer never arrived"
settle || bad "frame never settled after the answer"
type_ 'fourth'
key Enter
if _poll_until "$FAST" _all_has 'echo: fourth'; then
    ok "the next message went out at once — no wait on a title pass that never answers"
else
    bad "the next message waited on the title pass"
fi

# ---- 5. a hung title pass alone, then Ctrl+C at idle ----
start 100 24 || finish
settle || bad "startup never settled"
tm set-option -t s remain-on-exit on

type_ 'slowtitle:five'
key Enter
wait_all 'echo: slowtitle:five' || bad "the fifth pane's answer never arrived"
settle || bad "frame never settled after the answer"
key C-c
if _poll_until "$FAST" pane_dead; then
    ok "Ctrl+C at idle exits at once — no wait on a title pass that never answers"
else
    bad "the exit waited on the title pass"
fi

# ---- 6. a finished answer over a body that never closes ----
# Still under IOTA_STREAM_IDLE_TIMEOUT=2 (exported in 3).
start 120 24 || finish
settle || bad "startup never settled"

type_ 'openend:six'
key Enter
wait_all 'echo: openend:six' || bad "the sixth pane's answer never arrived"
# Three seconds: past the 2 s bound the drain-to-EOF reader used to fail on.
if _poll_until 30 _all_has 'Response stalled'; then
    bad "a complete answer failed as a stall because its body stayed open"
else
    ok "the stream ended at [DONE] — the open body is not a stall"
fi
check "the answer is not retried" "$(count_all 'retrying (attempt')" 0

finish
