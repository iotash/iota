#!/usr/bin/env bash
# L4 scenario 31 (F3, X-69) — an Anthropic reply the output ceiling cuts off mid-ARGUMENTS, on a real terminal.
#
# The ScriptedUi and provider pins prove the call is dropped and named; what only a terminal (and a real
# `shell` tool) shows is the whole of it: the composing `[shell …]` row up while the arguments stream, then
# torn down — not left spinning, not settled into a call header — the red `✗ Output truncated` block naming
# the dropped call, the frame intact, and the call NEVER RUN: its command would have created a file, and the
# file is not there; no `tool_result` ever reached the wire.
#
# The mock's `toolcut:<cmd>` script (`mock.rs`, the Anthropic dialect) opens a `shell` `tool_use`, sends ONE
# `input_json_delta` of `{"command":"<cmd>` (string and object left open), holds it 1.5 s so the composing
# row is observable, stops the block and the message with `stop_reason: max_tokens`.
#
# A: the CONTROL first — `run:<cmd>` is the same call made whole and closed by `tool_use`: it DOES run (its
#    file appears, one `tool_result` goes out, `ran:` answers), and it leaves no red cell. So the "file
#    absent", "no tool_result" and "no red" readings of B are detectors seen firing, not ones that never
#    could; B's red block is in turn what makes A's zero a live reading (22-retry.sh's `RED_SGR`).
# B: the cut. C: the next message — answered on a history that holds the control's call and not the cut's.
#
# The agent runs `shell` unsandboxed and auto-approved (19-background-jobs.sh's config), so a call that
# escaped the drop would run at once rather than stop at an approval prompt that hides the bug.
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

ESC="$(printf '\033')"
# A red foreground as `capture-pane -e` re-encodes it (see 22-retry.sh): SGR 31 comes back as `38;5;1`.
RED_SGR="${ESC}\\[([0-9;]*;)?(31|91|38;5;1|38;5;9)m"
red_cells() { tm capture-pane -e -pt s -S -400 2>/dev/null | LC_ALL=C grep -acE -- "$RED_SGR" | tr -d ' '; }
red_rows_with() { tm capture-pane -e -pt s -S -400 2>/dev/null | LC_ALL=C grep -aF -- "$1" | LC_ALL=C grep -acE -- "$RED_SGR" | tr -d ' '; }

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
check "the control left no red cell" "$(red_cells)" 0
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
if [ "$(red_cells)" -gt 0 ]; then
    ok "the red detector fires on the cut (A's zero was live)"
else
    bad "the cut's block carries no red SGR — the no-red reading in A proves nothing"
fi
# Torn down: no composing row, no busy meter, no header the half call settled into.
check "the composing [shell …] row is gone from the pane" "$(count_vis '[shell …]')" 0
check "…and never reached the scrollback" "$(count_all '[shell …]')" 0
check "…its busy meter with it" "$(count_vis 'ESC to cancel')" 0
check "the half call settled into no header" "$(count_all '[shell touch l4-cut-ran')" 0
check "…and no output row of its own (the control's is the one)" "$(count_all '⎿ [command produced no output]')" 1
# Never run: no file, no new tool_result, not retried.
if [ -e "$CUT" ]; then bad "the dropped call RAN: $CUT exists"; else ok "the dropped call never ran (no $CUT)"; fi
check "no tool_result answered the cut (the control's is the one)" "$(answered)" 1
check "the cut reached the wire once (not retried, not replayed by a later turn)" "$(wire_count 'toolcut:')" 1
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
if [ -e "$CUT" ]; then bad "the dropped call ran later: $CUT exists"; else ok "the dropped call still never ran"; fi
check "one red block in the whole run" "$(red_rows_with '✗')" 1
check_frame_intact "after the next turn" 120

finish
