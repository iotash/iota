#!/usr/bin/env bash
# L4 scenario 34 (docs/design/busy-span.md, DIVERGENCES X-76) — the status row's busy over the whole turn.
#
# One turn layer on the status row, from the moment the loop accepts the message until the turn has landed;
# its phases only relabel it, its clock is the turn's with the user's waits taken out, and the cancel hint
# is its alone. On a real terminal, phase by phase:
#
#   A waiting for the model — `hang:` holds the reply after its head: `Waiting for the first token` with the
#     hint; ESC ends the turn and the row is bare again.
#   B the text streaming    — `stream 40 100`: `Responding` with the hint, while the lines arrive.
#   C an approval           — `runfg:` asks before the `shell` call: the surface takes the bottom zone, so
#     the status row (and its hint) is out of sight; the user takes three seconds to answer.
#   D the call running      — `Running tools` with the hint while `sleep 4` runs; the call's widget shows
#     its own clock and no hint (one on screen); the turn's clock left the approval's three seconds out.
#   E a job after the turn  — `run:` backgrounds `sleep 15`: once the turn has landed the row ends in the
#     static `job b1 …` segment — no spinner, no hint, no turn layer (point 5, B).
#
# `shell` runs unsandboxed and NOT auto-approved, so every call asks (scenario 18's config).
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

CONFIG_BODY="providers:
  mock: {type: openai, key: test, url: \"http://127.0.0.1:$IOTA_PORT\"}
models:
  m: mock:fake
agents:
  default:
    model: m
    choices: [m, \"mock:*\"]
    tools: {shell: {sandbox: off}}"
export CONFIG_BODY

# A spinner glyph, as an alternation (the C-locale note on `norm`).
SPIN='(⠋|⠙|⠹|⠸|⠼|⠴|⠦|⠧|⠇|⠏)'
# The status row's turn clock after `$1`, in seconds ("" while it is under the 2 s threshold or absent).
turn_clock() { bottom_zone | grep -oE -- "$1  [0-9]+s" | grep -oE '[0-9]+' | head -1; }
_clock_shown() { [ -n "$(turn_clock "$1")" ]; }
# The zone has `$1` and the hint, in ONE capture.
_zone_has() { local z; z="$(bottom_zone)"; printf '%s\n' "$z" | grep -qF -- "$1" && printf '%s\n' "$z" | grep -qF '(ESC to cancel)'; }

start_provider openai 110 24 || finish
settle || bad "startup never settled"
check "idle: no busy on the status row" "$(count_vis 'ESC to cancel')" 0

# ------------------------------------------------------------------ A: waiting for the model
type_ 'hang:a'
key Enter
if _poll_until 120 _zone_has 'Waiting for the first token'; then
    ok "A: the status row names the wait, with the hint"
else
    bad "A: no 'Waiting for the first token (ESC to cancel)' on the status row"
fi
_poll_until 40 _clock_shown 'Waiting for the first token' || bad "A: the turn's clock never showed"
shot "A: waiting for the model"
key Escape
wait_gone 'ESC to cancel' || bad "A: ESC left the busy up"
settle || bad "A: never settled"
check "A: the turn over, the row is bare" "$(bottom_zone | grep -cE -- "$SPIN" | tr -d ' ')" 0

# ------------------------------------------------------------------ B: the text streaming
type_ 'stream 40 100'
key Enter
wait_vis 'l#05' || bad "B: the stream never started"
if _zone_has 'Responding'; then ok "B: Responding while the text streams"; else bad "B: no 'Responding' on the status row"; fi
check "B: …one hint on screen" "$(count_vis 'ESC to cancel')" 1
shot "B: the text streaming (Responding)"
wait_all 'l#39 line' || bad "B: the stream never finished"
settle || bad "B: never settled"
check "B: the turn over, no hint" "$(count_vis 'ESC to cancel')" 0

# ------------------------------------------------------------------ C/D: an approval, then the call running
type_ 'runfg:sleep 4; echo fg-done'
key Enter
t0=$(date +%s)
wait_vis 'allow?' || bad "C: the approval never opened"
check "C: the surface hides the status row (no hint, no phase)" \
    "$(count_vis 'ESC to cancel')$(count_vis 'Running tools')" 00
shot "C: waiting on the approval (the bottom zone is the surface)"
sleep 3
key Enter # Allow once
if _poll_until 60 _zone_has 'Running tools'; then ok "D: Running tools while the call runs"; else bad "D: no 'Running tools' on the status row"; fi
_poll_until 40 _clock_shown 'Running tools' || bad "D: the turn's clock never showed"
clock="$(turn_clock 'Running tools')"
wall=$(($(date +%s) - t0))
shot "D: the call running (Running tools)"
check "D: one hint on screen — the status row's" "$(count_vis 'ESC to cancel')" 1
check "D: the call's widget is up, its clock row without a hint" \
    "$(cap | grep -cE -- "^$SPIN \\[shell sleep 4" | tr -d ' ')$(cap | grep -cE -- '⎿ [0-9<]+s$' | tr -d ' ')" 11
if [ -n "$clock" ] && [ "$((clock + 2))" -le "$wall" ]; then
    ok "D: the approval's wait is out of the turn's clock (clock ${clock}s, wall ${wall}s)"
else
    bad "D: the turn's clock counted the wait (clock '${clock}'s, wall ${wall}s)"
fi
wait_all 'ran: fg-done' || bad "D: the follow-up round never answered"
settle || bad "D: never settled"

# ------------------------------------------------------------------ E: a job running after the turn
type_ 'run:sleep 15; echo bg-done'
key Enter
wait_vis 'allow?' || bad "E: the approval never opened"
key Enter
wait_all 'ran: Started' || wait_all 'ran: ' || bad "E: the round never closed"
_job_static() { bottom_zone | grep -qF 'job b1' && ! bottom_zone | grep -qE -- "$SPIN|ESC"; }
if _poll_until 60 _job_static; then
    ok "E: the turn landed — the row ends in the static job segment, no spinner, no hint"
else
    bad "E: the job segment is not alone on the status row"
fi
shot "E: a job running after the turn (static job segment)"
check "E: no turn layer left" "$(count_vis 'Waiting for the model')$(count_vis 'Running tools')" 00
check_frame_intact "after E" 110

finish
