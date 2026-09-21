#!/usr/bin/env bash
# L4 scenario 2 (TUI_TEST_PLAN §L4) — one streamed markdown turn.
#
# Three claims only a real terminal can settle:
#   * every rendered row reaches NATIVE scrollback exactly once (the insert_before path
#     preserves history — wart W9's whole reason for existing);
#   * the live call preview MORPHS in place: while the model thinks, tmux's scrollback
#     counter does not move, so the preview is a widget and not a rolling source window;
#   * the rows above the turn — the banner — are not eaten by the frame walking down.
#
# A plain streamed turn runs first, both for its own history assertions and to walk the
# frame down to the bottom of the pane: only there does an accidental insert actually
# scroll, which is what makes the morph assertion below mean something.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

start 80 24 || finish
settle || bad "startup never settled"

# --- turn one: 20 plain lines, streamed.
type_ 'stream 20'
key Enter
wait_all 'l#19 line' || bad "the 20-line stream never completed"
settle || bad "frame never settled after the stream"

check "history contiguous: 20 distinct lines" "$(uniq_all 'l#[0-9][0-9]')" 20
dup="$(dupes_all 'l#[0-9][0-9] line')"
check "no line duplicated by the insert path" "$dup" ""
check_once "user echo committed once" '❯ stream 20'
check_frame_intact "after the plain stream" 80

pushed="$(hist_size)"
if [ "$pushed" -gt 0 ]; then
    ok "frame walked to the bottom; $pushed rows live in native scrollback"
else
    bad "nothing scrolled — the morph assertion below would be vacuous"
fi

# --- turn two: reasoning, then markdown.
type_ 'think'
key Enter

wait_vis '⎿' || bad "call-preview status row never appeared"
# The widget's whole shape in one comparison, with the three live cells (spinner frame,
# elapsed clock, and — WP53 — the thinking meter's running token
# estimate) normalised away: label row, then the ⎿ status row carrying the meter, the clock
# and the cancel hint the active scope earns.
prev="$(row_of 'Thinking')"
if [ -n "$prev" ]; then
    check "preview widget shape" \
        "$(cap | sed -n "${prev},$((prev + 1))p" | norm)" \
        "$(printf '* Thinking\n  ⎿ N tokens · Ts · ESC to cancel')"
else
    bad "preview widget label missing"
fi
a="$(hist_size)"
sleep 0.6
b="$(hist_size)"
check "preview morphs in place (no rows inserted while thinking)" "$a" "$b"

wait_all '◇ thought for' || bad "thinking marker never committed"
wait_all 'done.' || bad "markdown body never completed"
settle || bad "frame never settled after the markdown turn"

check_once "the ◇ marker is committed exactly once" '◇ thought for'
check_once "heading" 'Heading'
check_once "inline emphasis is rendered, not literal" 'some text here'
check_once "list item 1" '• one'
check_once "list item 2" '• two'
check_once "table top border" '┌─────┬─────┐'
check_once "table header cell" '│ a   │ b   │'
check_once "table body cell" '│ 1   │ 2   │'
check_once "table bottom border" '└─────┴─────┘'
check_once "code block body" '  code'
check_once "closing paragraph" 'done.'

# The preview never leaks its own rows into history.
check "no leftover preview status rows in history" "$(count_all '⎿')" 0
if capall | grep -q 'Thinking'; then
    bad "the transient 'Thinking' label leaked into scrollback"
else
    ok "the transient 'Thinking' label never reached scrollback"
fi

# --- nothing above was eaten, and the frame is still whole.
check "the 20 streamed lines are still intact above" "$(uniq_all 'l#[0-9][0-9]')" 20
check_once "banner survived both turns" '▀█▀ █▀▀█ ▀▀█▀▀ █▀▀█'
check_frame_intact "after the markdown turn" 80

finish
