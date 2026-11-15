#!/usr/bin/env bash
# L4 scenario 8 (TUI_TEST_PLAN §L4) — the big resume echo.
#
# A 32-round bundle is resumed; the echo window is the last three rounds (`RESUME_ECHO_ROUNDS`, on
# every path — DIVERGENCES X-98), and this fixture gives each of them 14 body lines, so the echo is
# about ninety rows against a twenty-four-row pane. What L4 proves is that it does not eat the frame:
# every echoed row lands exactly once, the separator pair and the status row are still there, nothing
# from outside the window is replayed, and the exit writes out only what was replayed (X-99).
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

SESSION_ID=aaaabbbbcccc
write_session "$SESSION_ID" 32 14

start 80 24 resume "$SESSION_ID" || finish
wait_all 'r31 body line 13' || miss "the resume echo never completed"
settle || miss "frame never settled after the resume echo"

check_once "the resume notice names the bundle" "Resumed session $SESSION_ID (64 messages)"
check_once "banner still printed after the resume notice" 'ι> iota'
check_once "the mode row says the chat was resumed" "│ chat · resumed $SESSION_ID"

# The whole echo window, exactly once each — 42 body rows plus the three user echoes.
check "every echoed body row is present" "$(uniq_all 'r(29|30|31) body line [0-9][0-9]')" 42
check "no echoed body row is duplicated" "$(dupes_all 'r(29|30|31) body line [0-9][0-9]')" ""
check_once "echo window round 29 header" '❯ r29 question'
check_once "echo window round 30 header" '❯ r30 question'
check_once "echo window round 31 header" '❯ r31 question'

# …and only that window: older rounds are not in the buffer.
check "only the three rounds are echoed" "$(uniq_all 'r[0-9][0-9] question')" 3
check "rounds outside the window are not echoed" "$(count_all 'r00 question')" 0
check "…nor their answers" "$(count_all 'r28 answer')" 0

# The frame survived a block three times the pane height.
check_frame_intact "after the oversized echo" 80
check "status line survived the chunked insert" "$(status_model)" "  fake"

# The resumed chat still works, and the new turn appends to the same bundle.
#
# Two waits, each on its own event: the Enter was read (the draft left the composer and sits above
# the frame — `wait_sent`), THEN the reply landed. One `wait_all` for the reply could not tell a
# submit that never happened from a reply that never came, and printed nothing either way (CI
# 38759202470, the ubuntu leg: both checks below failed with no frame to read). A miss now shows
# the pane as it stood.
type_ 'hello'
key Enter
wait_sent 'hello' || miss "the Enter that sends 'hello' was never read"
wait_all 'echo: hello' || miss "the resumed chat did not accept a new turn"
settle || miss "frame never settled after the new turn"
check_frame_intact "after a turn on the resumed session" 80
if grep -q '"content":"hello"' "$SCEN_HOME/.iota/sessions/$SESSION_ID/messages.jsonl"; then
    ok "the new turn appended to the resumed bundle"
else
    bad "the new turn did not reach the session log"
fi

# The exit writes out the buffer: the three replayed rounds and the new turn, once each, nothing older.
tm set-option -t s remain-on-exit on
key C-d
_poll_until 50 pane_dead || miss "iota did not exit on an idle Ctrl+D"
all="$(capall)"
check "the exit wrote the three replayed rounds" "$(printf '%s\n' "$all" | grep -oE 'r[0-9][0-9] question' | sort -u | wc -l | tr -d ' ')" 3
check "…each once" "$(printf '%s\n' "$all" | grep -oE 'r[0-9][0-9] question' | sort | uniq -d)" ""
check "…and nothing older" "$(printf '%s\n' "$all" | grep -cF 'r28 answer' | tr -d ' ')" 0
check "the new turn was written out too" "$(printf '%s\n' "$all" | grep -cF 'echo: hello' | tr -d ' ')" 1

finish
