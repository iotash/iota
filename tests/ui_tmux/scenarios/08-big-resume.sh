#!/usr/bin/env bash
# L4 scenario 8 (TUI_TEST_PLAN §L4) — the big resume echo.
#
# A 32-round bundle is resumed; the whole conversation is replayed into the buffer (tui-plan S2),
# and this fixture gives each round 14 body lines, so the echo is about five hundred rows against a
# twenty-four-row pane. What L4 proves is that it does not eat the frame: every echoed row lands
# exactly once, the separator pair and the status row are still there.
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

# The WHOLE conversation is replayed into the buffer (tui-plan S2), every round once.
check "every round is echoed" "$(uniq_all 'r[0-9][0-9] question')" 32
check "…none twice" "$(dupes_all 'r[0-9][0-9] question')" ""
check_once "…the first round's answer too" 'r00 answer'

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

finish
