#!/usr/bin/env bash
# L4 scenario 22 (docs/TUI-VERIFY.md, batch C) — the retryable error path on a real terminal.
#
# `retry_round` (`repl/turn/retry.rs`) is unit-pinned under a paused clock; what a human was
# asked to watch is the SHAPE of it: the classification rides the status row as a busy
# segment (`Provider server error (503) — retrying (attempt n/10)`), the recovery leaves ONE
# dim `⟳ … recovered after n attempt(s)` notice and nothing red, a message typed during the
# backoff is not lost and lands exactly once, and Ctrl+C during a backoff interrupts the turn
# like any other — no red block — before a second Ctrl+C exits. The mock's `fail:<status>:<n>`
# refuses the first n requests that carry the message, so the sequence is deterministic.
#
# Two retry layers stack here, and the budget is chosen for the outer one: the HTTP client
# (`llm/client.rs`, `DEFAULT_RETRIES` = 2) re-sends a 5xx twice on its own, under a 500 ms /
# 1 s backoff and the plain busy spinner, before the turn ever sees an error. So `fail:503:3`
# costs the turn ONE `retrying (attempt 1/10)`, and `fail:503:6` two — attempts 1–3 and 4–6
# refused, the seventh answered.
#
# A 400 at the end is the CONTROL: it is not retryable, so it must paint the red `✗` block —
# proof that the "no red" assertions above were live.
#
# The pane is 120 columns: the status row truncates to its width with `…`, and the busy
# segment — `⠋ Provider server error (503) — retrying (attempt 1/10)  1s (ESC to cancel)` —
# sits behind the model, token and context segments, past 80 columns once a turn has cost
# tokens. At 120 the whole label is readable and the assertion can name it.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

ESC="$(printf '\033')"
# A red foreground as `capture-pane -e` re-encodes it: the error block's `✗` is SGR 31
# (styles.rs `red`), which tmux stores as palette index 1 and writes back as `38;5;1` — so both
# spellings, and the bright pair, are red here; `38;5;31` (a blue) is not.
RED_SGR="${ESC}\\[([0-9;]*;)?(31|91|38;5;1|38;5;9)m"
red_cells() { tm capture-pane -e -pt s -S -400 2>/dev/null | LC_ALL=C grep -acE -- "$RED_SGR" | tr -d ' '; }

start 120 24 || finish
settle || bad "startup never settled"

# ------------------------------------------------------------------ A: two refusals, then the answer
type_ 'fail:503:6 hello'
key Enter
wait_vis 'retrying (attempt 1/' || bad "the first refusal never reached the status row"
check "the classification rides the status row" "$(count_vis 'Provider server error (503) — retrying (attempt 1/10)')" 1
# Typed during the first backoff: the loop is busy, so the line queues.
type_ 'steer'
key Enter
wait_vis '» steer' || bad "the line typed during the backoff never queued"
wait_vis 'retrying (attempt 2/' || bad "the second refusal never reached the status row"
wait_all 'echo: fail:503:6 hello' || bad "the turn never recovered"
# The queued line is the next turn.
wait_all 'echo: steer' || bad "the queued line never reached the model"
settle || bad "frame never settled after the recovery"
check_once "ONE recovery notice" '⟳ Provider server error (503) — recovered after 2 attempt(s)'
check_once "the answer landed once" 'echo: fail:503:6 hello'
check_once "the line typed during the backoff landed once" '❯ steer'
check_once "…and was answered once" 'echo: steer'
check "no red block anywhere" "$(red_cells)" 0
check "no ✗ row" "$(count_all '✗')" 0
check "the busy segment is gone from the status row" "$(count_vis 'retrying')" 0
check_frame_intact "after the recovery" 120

# ------------------------------------------------------------------ B: Ctrl+C during a backoff
type_ 'fail:503:3 again'
key Enter
wait_vis 'retrying (attempt 1/' || bad "the refusal never reached the status row"
check "the classification rides the status row again" "$(count_vis 'Provider server error (503) — retrying (attempt 1/10)')" 1
key C-c
wait_all 'Interrupted.' || bad "Ctrl+C during the backoff did not interrupt the turn"
settle || bad "frame never settled after the interrupt"
check_once "the interrupt notice is committed once" 'Interrupted.'
if alive; then ok "the app is still running"; else bad "Ctrl+C during a backoff exited the program"; finish; fi
check "no red block after an interrupted retry" "$(red_cells)" 0
check "no ✗ row after an interrupted retry" "$(count_all '✗')" 0
check "the busy segment is gone" "$(count_vis 'retrying')" 0
check_frame_intact "after the interrupted retry" 120

# ------------------------------------------------------------------ C: the control — a 400 is red
type_ 'fail:400:1 control'
key Enter
wait_all '✗ Request rejected (400' || bad "the non-retryable error never painted its block"
settle || bad "frame never settled after the error"
check_once "a non-retryable error is ONE ✗ block" '✗ Request rejected (400 Bad Request)'
if [ "$(red_cells)" -gt 0 ]; then
    ok "…painted red (the assertions above were live)"
else
    bad "the error block carries no red SGR — the no-red assertions above prove nothing"
fi
check "…and never retried" "$(count_all 'retrying')" 0
check_frame_intact "after the error block" 120

# ------------------------------------------------------------------ D: the exit gesture at idle
key C-c
if _poll_until 60 server_gone; then
    ok "Ctrl+C at idle exits the program"
else
    bad "Ctrl+C at idle did not exit"
fi

finish
