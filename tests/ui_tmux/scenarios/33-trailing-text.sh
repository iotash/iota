#!/usr/bin/env bash
# L4 scenario 33 (docs/design/busy-span.md §2.7) — a model that composes a tool call and then keeps talking.
#
# The text after the call spills: it is committed below where the call's widget goes. Before the fix the
# spill also marked the content block open and nothing closed it, so the walk's `open_call` deferred:
#
#   A (anonymous) — the arguments arrive before the call's name (`runanon:`), so no composing widget rose;
#                   the call then RAN with nothing on screen, and its widget appeared only when it was
#                   over, its clock starting there (`<1s`).
#   B (named)     — the composing widget `[shell …]` rose (`runtail:`), and the call ran under that label,
#                   never relabelled to its real header.
#
# Both now run under their own live widget: `⠋ [shell sleep 4; echo …]` with its `⎿` clock row, while the
# command sleeps, before the follow-up round's `ran:` answer. The ScriptedUi pin is
# `a_call_with_trailing_text_is_raised_before_it_runs` (src/repl/turn/tools/tests.rs).
#
# The agent runs `shell` unsandboxed and auto-approved (19-background-jobs.sh's config), so no approval
# prompt sits between the call and its execution.
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
    tools: {shell: {sandbox: off, auto_run: true}}"
export CONFIG_BODY

# A spinner glyph: an alternation, never a bracket expression (the C-locale note on `norm`).
SPIN='(⠋|⠙|⠹|⠸|⠼|⠴|⠦|⠧|⠇|⠏)'
# The live widget row for `$1` (a fixed header prefix) is on screen: spinner, space, the header.
_live() { cap | grep -qE -- "^$SPIN $(printf '%s' "$1" | sed 's/[][\.*^$]/\\&/g')"; }
wait_live() { _poll_until 30 _live "$1"; }

start_provider openai 100 24 || finish
settle || bad "startup never settled"

# ------------------------------------------------------------------ A: anonymous composer, trailing text
type_ 'runanon:sleep 4; echo anon-done'
key Enter
wait_all 'TRAILTEXT after the call' || bad "A: the trailing text never landed"
if wait_live '[shell sleep 4; echo anon-done]'; then
    ok "A: the call's live widget is up while it runs"
    shot "A: anonymous composer + trailing text, mid-execution"
    check "A: …before the call is over (no ran: yet)" "$(count_all 'ran: anon-done')" 0
    check "A: …with its clock row" "$(cap | grep -cF '⎿' | tr -d ' ')" 1
else
    bad "A: no live widget for the call while it ran"
    shot "A: anonymous composer + trailing text, mid-execution (no widget)"
fi
wait_all 'ran: anon-done' || bad "A: the follow-up round never answered"
settle || bad "A: frame never settled"
check_once "A: the call's header committed once" '[shell sleep 4; echo anon-done]'
check_frame_intact "after A" 100

# ------------------------------------------------------------------ B: named composer, trailing text
type_ 'runtail:sleep 4; echo tail-done'
key Enter
wait_all_more 'TRAILTEXT after the call' 1 || bad "B: the trailing text never landed"
if wait_live '[shell sleep 4; echo tail-done]'; then
    ok "B: the call runs under its own header"
    shot "B: named composer + trailing text, mid-execution"
    check "B: …before the call is over (no ran: yet)" "$(count_all 'ran: tail-done')" 0
    check "B: …and the composing label is gone" "$(cap | grep -cE -- "^$SPIN \\[shell …\\]" | tr -d ' ')" 0
else
    bad "B: the call never ran under its own header"
    shot "B: named composer + trailing text, mid-execution (composing label)"
fi
wait_all 'ran: tail-done' || bad "B: the follow-up round never answered"
settle || bad "B: frame never settled"
check_once "B: the call's header committed once" '[shell sleep 4; echo tail-done]'
check_frame_intact "after B" 100

finish
