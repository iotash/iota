#!/usr/bin/env bash
# L4 scenario 8 (TUI_TEST_PLAN §L4) — the big resume echo, i.e. the chunking law (T-06).
#
# A 32-round bundle is resumed; the echo window is the last three rounds
# (`RESUME_ECHO_ROUNDS`), and this fixture gives each of them 14 body lines, so the block
# the region must insert is about ninety rows against a twenty-four-row pane. The region
# chunks it at max(2, h/2); what L4 proves is that the chunking does not eat the frame:
# every echoed row lands exactly once, the separator pair and the status row are still
# there, and nothing from outside the window is echoed.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

SESSION_ID=aaaabbbbcccc
write_session "$SESSION_ID" 32 14

start 80 24 resume "$SESSION_ID" || finish
wait_all 'r31 body line 13' || bad "the resume echo never completed"
settle || bad "frame never settled after the resume echo"

check_once "the resume notice names the bundle" "Resumed session $SESSION_ID (64 messages)"
check_once "banner still printed after the resume notice" '▀█▀ █▀▀█ ▀▀█▀▀ █▀▀█'
check_once "the mode row says the chat was resumed" "   chat · resumed $SESSION_ID"

# The whole echo window, exactly once each — 42 body rows plus the three user echoes.
check "every echoed body row is present" "$(uniq_all 'r(29|30|31) body line [0-9][0-9]')" 42
check "no echoed body row is duplicated" "$(dupes_all 'r(29|30|31) body line [0-9][0-9]')" ""
check_once "echo window round 29 header" '❯ r29 question'
check_once "echo window round 30 header" '❯ r30 question'
check_once "echo window round 31 header" '❯ r31 question'

# …and only that window: the chunking law must not drag older rounds in.
check "rounds outside the window are not echoed" "$(count_all 'r00 question')" 0
check "…nor their answers" "$(count_all 'r28 answer')" 0

# The frame survived a block three times the pane height.
check_frame_intact "after the oversized echo" 80
check "status line survived the chunked insert" "$(status_model)" "  fake"

# The resumed chat still works, and the new turn appends to the same bundle.
type_ 'hello'
key Enter
wait_all 'echo: hello' || bad "the resumed chat did not accept a new turn"
settle || bad "frame never settled after the new turn"
check_frame_intact "after a turn on the resumed session" 80
if grep -q '"content":"hello"' "$SCEN_HOME/.iota/sessions/$SESSION_ID/messages.jsonl"; then
    ok "the new turn appended to the resumed bundle"
else
    bad "the new turn did not reach the session log"
fi

finish
