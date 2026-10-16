#!/usr/bin/env bash
# L4 scenario 30 (F3, X-69) — an Anthropic reply the output ceiling cuts off mid-THOUGHT, on a real terminal.
#
# The ScriptedUi pin (`tests/repl/stall.rs` `a_thought_cut_off_by_the_ceiling_is_never_the_answer`) proves
# the error's words, the rollback and the next request through a fake provider; what only a terminal shows
# is how the cut LOOKS: the red `✗ Output truncated` block with its detail and hint, the `Thinking` widget
# wound down into its `◇ thought` marker rather than left spinning, the frame intact under the block, and
# not one row of the half-thought printed as an answer.
#
# The mock's `thinkcut` script (`mock.rs`, the Anthropic dialect) streams ~1.5 s of `halfthought #` deltas,
# seals the block with a `signature_delta`, and stops with `stop_reason: max_tokens` — no text block at all.
# `GET /v1/messages/log` hands back every body the client sent, so the rollback is read off the wire: the
# request after the cut carries neither the question nor the thought.
#
# A: the CONTROL first — an ordinary turn that ends (a sealed thought, then `echo: hello`) leaves no red
#    row and no `✗`. B's red block is what proves A's zero was a live reading, not a detector that never
#    fires (22-retry.sh's 400 control, the other way round; lib.sh `red_rows`, after `check_red_detector`).
# B: the cut. C: the next message — answered, and its request clean.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

command -v curl >/dev/null 2>&1 || { bad "curl not found (the mock's request log is read with it)"; finish; }

# Red is read by lib.sh's `red_rows`/`red_rows_with` (SGR parameters walked, so the extended colour
# `38;5;31` is not red), and the detector is judged on known bytes — red and look-alikes — first.
check_red_detector

# The mock's request log: every `/v1/messages` body so far, one per line.
wire() { curl -s "http://127.0.0.1:$IOTA_PORT/v1/messages/log"; }
# The TURN request (one that advertises tools — the title pass does not) whose last user text is `$1`: the
# text that closes the `messages` array (`system` or `tools` follows it).
turn_request() { wire | grep -F '"tools":[' | grep -F -- "\"text\":\"$1\"}]}]," | tail -1; }

start_provider anthropic 120 30 || finish
settle || miss "startup never settled"

# ------------------------------------------------------------------ A: the control — a turn that ends
type_ 'hello'
key Enter
wait_all 'echo: hello' || miss "the control turn never answered"
settle || miss "frame never settled after the control turn"
check_once "the control turn's answer landed once" 'echo: hello'
check "the control turn left no red row" "$(red_rows)" 0
check "…and no ✗ row" "$(count_all '✗')" 0
check_frame_intact "after the control turn" 120

# ------------------------------------------------------------------ B: the cut, mid-thought
type_ 'thinkcut please'
key Enter
# The widget is up while the thought streams: what must be wound down afterwards.
wait_vis 'Thinking' || miss "the Thinking widget never came up"
wait_all '✗ Output truncated' || miss "the cut never painted its block"
settle || miss "frame never settled after the cut"
check_once "the cut is ONE ✗ Output truncated block" '✗ Output truncated'
check_once "…its detail names the stop and the ceiling" 'stop_reason: max_tokens: the output hit its 32000-token limit'
check_once "…its hint opens with the knob" 'Raise max_tokens: in the config'
check "…the headline row is painted red" "$(red_rows_with '✗ Output truncated')" 1
if [ "$(red_rows)" -gt 0 ]; then
    ok "the red detector fires on the cut (A's zero was live)"
else
    bad "the cut's block carries no red SGR — the no-red reading in A proves nothing"
fi
check "not one row of the half-thought is printed" "$(count_all 'halfthought')" 0
check "no echo: answer row follows the question" "$(count_all 'echo: thinkcut')" 0
# The widget wound down: the transient label and its meter are gone, the thought settled into its marker.
check "the Thinking widget is gone from the pane" "$(count_vis 'Thinking')" 0
check "…and its meter with it" "$(count_vis 'ESC to cancel')" 0
check "the thought settled into a ◇ marker (one per turn so far)" "$(count_all '◇ thought for')" 2
check "the transient label never reached the scrollback" "$(count_all 'Thinking')" 0
check "…and the cut was not retried" "$(count_all 'retrying')" 0
check_frame_intact "after the cut" 120

# ------------------------------------------------------------------ C: the next message, on a clean history
type_ 'after'
key Enter
wait_all 'echo: after' || miss "the message after the cut was never answered"
settle || miss "frame never settled after the next turn"
check_once "the next message was answered once" 'echo: after'
next="$(turn_request after)"
if [ -n "$next" ]; then
    ok "the next turn's request is in the mock's log"
else
    bad "no turn request ending in 'after' in the mock's log"
    wire | cut -c1-400
fi
check "…it carries no half-thought" "$(printf '%s' "$next" | grep -cF 'halfthought' | tr -d ' ')" 0
check "…nor the rolled-back question" "$(printf '%s' "$next" | grep -cF 'thinkcut' | tr -d ' ')" 0
check "…but the control turn is still history" "$(printf '%s' "$next" | grep -cF '"text":"echo: hello"' | tr -d ' ')" 1
check "the cut reached the wire once (not retried, not replayed by the next turn)" "$(wire | grep -F '"tools":[' | grep -cF 'thinkcut' | tr -d ' ')" 1
# The session log on disk: the rolled-back turn is not recorded either.
sess="$(cat "$SCEN_HOME"/.iota/sessions/*/messages.jsonl 2>/dev/null)"
if [ -n "$sess" ]; then
    check "the session log does not record the cut turn" "$(printf '%s\n' "$sess" | grep -cF 'thinkcut' | tr -d ' ')" 0
    check "…nor the half-thought" "$(printf '%s\n' "$sess" | grep -cF 'halfthought' | tr -d ' ')" 0
    check "…while the next turn is recorded" "$(printf '%s\n' "$sess" | grep -cF 'echo: after' | tr -d ' ')" 1
else
    bad "no session log under $SCEN_HOME/.iota/sessions"
fi
check "one red ✗ row in the whole run" "$(red_rows_with '✗')" 1
check_frame_intact "after the next turn" 120

finish
