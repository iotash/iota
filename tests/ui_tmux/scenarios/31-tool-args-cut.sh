#!/usr/bin/env bash
# L4 scenario 31 (F3, X-69) — an Anthropic reply the output ceiling cuts off mid-ARGUMENTS, on a real terminal.
#
# The ScriptedUi and provider pins prove the call is dropped and named; what only a terminal (and a real
# `shell` tool) shows is the whole of it: the composing `[shell …]` row up while the arguments stream, then
# torn down — not left spinning, not settled into a call header — the red `✗ Output truncated` block naming
# the dropped call, the frame intact, and the call NOT DISPATCHED — read off the wire and the history, not
# off the disk: no `tool_result` answers it (a dispatched call answers with one even when it fails), and the
# next request's history holds no trace of it.
#
# The marker file is NOT that proof. The half JSON never assembles a `command` (the provider falls back to
# empty input, and `shell` refuses an input without one), so even a dispatched cut call would leave no file;
# "no marker file observed" is reported as just that. A cut whose arguments DID complete is pinned below
# the terminal (`tests/provider/anthropic.rs`), not here.
#
# The mock's `toolcut:<cmd>` script (`mock.rs`, the Anthropic dialect) opens a `shell` `tool_use`, sends ONE
# `input_json_delta` of `{"command":"<cmd>` (string and object left open), holds it 1.5 s so the composing
# row is observable, stops the block and the message with `stop_reason: max_tokens`.
#
# A: the CONTROL first — `run:<cmd>` is the same call made whole and closed by `tool_use`: it DOES run (its
#    file appears, one `tool_result` goes out, `ran:` answers), and it leaves no red row. So B's "no
#    tool_result" and "no red" readings are detectors seen firing, not ones that never could; B's red block
#    is in turn what makes A's zero a live reading (lib.sh `red_rows`, after `check_red_detector`).
# B: the cut. C: the next message — answered on a history that holds the control's call and not the cut's.
#
# The agent runs `shell` unsandboxed and auto-approved (19-background-jobs.sh's config), so a call that
# escaped the drop would go straight to dispatch (and answer with a `tool_result`) rather than wait at an
# approval prompt that hides the bug.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

command -v curl >/dev/null 2>&1 || { bad "curl not found (the mock's request log is read with it)"; finish; }

CONFIG_BODY="providers:
  mock: {type: anthropic, key: test, url: \"http://127.0.0.1:$IOTA_PORT\"}
models:
  m: mock:fake
agents:
  default:
    model: m
    choices: [m, \"mock:*\"]
    tools: {shell: {sandbox: off, auto_run: true}}"
export CONFIG_BODY

# Red is read by lib.sh's `red_rows`/`red_rows_with` (SGR parameters walked, so the extended colour
# `38;5;31` is not red), and the detector is judged on known bytes — red and look-alikes — first.
check_red_detector

# The mock's request log: every `/v1/messages` body so far, one per line.
wire() { curl -s "http://127.0.0.1:$IOTA_PORT/v1/messages/log"; }
# Requests (title pass excluded) carrying `$1` anywhere.
wire_count() { wire | grep -F '"tools":[' | grep -cF -- "$1" | tr -d ' '; }
# The calls ANSWERED on the wire: distinct `tool_use_id`s of the `tool_result`s sent (every later request
# replays the earlier ones, so a plain count would grow with the history, not with the runs).
answered() { wire | grep -oE '"tool_use_id":"[^"]*"' | sort -u | wc -l | tr -d ' '; }
# The TURN request whose last user text is `$1`: the text that closes the `messages` array.
turn_request() { wire | grep -F '"tools":[' | grep -F -- "\"text\":\"$1\"}]}]," | tail -1; }

# The pane's cwd is $SCEN_TMP (start_provider), and so is the shell tool's: the markers land there.
CUT="$SCEN_TMP/l4-cut-ran"
WHOLE="$SCEN_TMP/l4-whole-ran"

start_provider anthropic 120 30 || finish
settle || bad "startup never settled"

# ------------------------------------------------------------------ A: the control — the whole call runs
type_ 'run:touch l4-whole-ran'
key Enter
wait_all 'ran: ' || bad "the control call's round never closed"
settle || bad "frame never settled after the control call"
if [ -e "$WHOLE" ]; then ok "the whole call ran (its file exists — the file check is live)"; else bad "the control call never ran: $WHOLE is missing"; fi
check_once "…its header committed once" '[shell touch l4-whole-ran]'
check "…one call answered on the wire (the wire check is live)" "$(answered)" 1
check "the control left no red row" "$(red_rows)" 0
check "…and no ✗ row" "$(count_all '✗')" 0
check_frame_intact "after the control call" 120

# ------------------------------------------------------------------ B: the cut, mid-arguments
type_ 'toolcut:touch l4-cut-ran'
key Enter
# The composing row is up while the arguments stream: what must be torn down afterwards.
wait_vis '[shell …]' || bad "the composing [shell …] row never came up"
wait_all '✗ Output truncated' || bad "the cut never painted its block"
settle || bad "frame never settled after the cut"
check_once "the cut is ONE ✗ Output truncated block" '✗ Output truncated'
check_once "…its detail names the dropped call" 'the output hit its 32000-token limit; the tool call it was composing was dropped (shell)'
check "…the headline row is painted red" "$(red_rows_with '✗ Output truncated')" 1
if [ "$(red_rows)" -gt 0 ]; then
    ok "the red detector fires on the cut (A's zero was live)"
else
    bad "the cut's block carries no red SGR — the no-red reading in A proves nothing"
fi
# Torn down: no composing row, no busy meter, no header the half call settled into.
check "the composing [shell …] row is gone from the pane" "$(count_vis '[shell …]')" 0
check "…and never reached the scrollback" "$(count_all '[shell …]')" 0
check "…its busy meter with it" "$(count_vis 'ESC to cancel')" 0
check "the half call settled into no header" "$(count_all '[shell touch l4-cut-ran')" 0
check "…and no '[command produced no output]' row of its own (the control's is the one)" "$(count_all '⎿ [command produced no output]')" 1
# Not dispatched: no tool_result answers it (the wire), not retried. The file is only a side observation —
# a half call has no `command` to run (see the header).
check "no tool_result answered the cut — not dispatched (the control's is the one)" "$(answered)" 1
if [ -e "$CUT" ]; then bad "a marker file appeared: $CUT exists"; else ok "no marker file observed (no $CUT)"; fi
check "the cut reached the wire once (not retried so far; C reads the next request)" "$(wire_count 'toolcut:')" 1
check "…and not retried" "$(count_all 'retrying')" 0
check_frame_intact "after the cut" 120

# ------------------------------------------------------------------ C: the next message, on a clean history
type_ 'after'
key Enter
wait_all 'echo: after' || bad "the message after the cut was never answered"
settle || bad "frame never settled after the next turn"
check_once "the next message was answered once" 'echo: after'
next="$(turn_request after)"
if [ -n "$next" ]; then
    ok "the next turn's request is in the mock's log"
else
    bad "no turn request ending in 'after' in the mock's log"
    wire | cut -c1-400
fi
check "…it carries the control's call" "$(printf '%s' "$next" | grep -cF 'touch l4-whole-ran' | tr -d ' ')" 1
check "…but not the dropped one" "$(printf '%s' "$next" | grep -cF 'l4-cut-ran' | tr -d ' ')" 0
check "…exactly one tool_use in its history" "$(printf '%s' "$next" | grep -oF '"type":"tool_use"' | wc -l | tr -d ' ')" 1
if [ -e "$CUT" ]; then bad "a marker file appeared later: $CUT exists"; else ok "still no marker file observed"; fi
check "one red ✗ row in the whole run" "$(red_rows_with '✗')" 1
check_frame_intact "after the next turn" 120

finish
