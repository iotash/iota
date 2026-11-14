#!/usr/bin/env bash
# L4 scenario 37 — the full screen (IOTA_FULLSCREEN=1, tui-plan S1): the alternate screen is
# held, the frame is the screen's bottom rows at full width, the conversation sits on it, and
# PgUp walks the whole session back to the banner while PgDn returns to the tail.
#
# Reads the visible pane only (`cap`): the alternate screen has no history to read.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"
export IOTA_FULLSCREEN=1

alt_on() { tm display-message -pt s '#{alternate_on}'; }

# fs_frame <label> <width> <height> — the frame is intact at the width and its bottom zone is the
# pane's last row (nothing under it, no gap: the frame is pinned to the bottom).
fs_frame() {
    check_frame_intact "$1" "$2"
    check "$1: the bottom zone is the pane's last row" "$(($(frame_bot) + 1))" "$3"
}

# The `l#NN` rows on screen, in screen order, as their numbers.
nums() { cap | grep -oE '^l#[0-9]+ line' | sed -E 's/^l#0*([0-9]+) line/\1/'; }
# Whether the numbers given (default: all on screen) rise by one from row to row (no hole, no
# duplicate).
contiguous() {
    local prev="" n list
    list="${1-$(nums)}"
    for n in $list; do
        if [ -n "$prev" ] && [ "$n" -ne $((prev + 1)) ]; then return 1; fi
        prev="$n"
    done
    [ -n "$prev" ]
}

# page <key> — presses it and waits for the screen to change (a press at either end changes nothing:
# the wait gives up after 2 s and the caller's own check decides).
_differs() { [ "$(cap)" != "$1" ]; }
page() {
    local prev
    prev="$(cap)"
    key "$1"
    _poll_until 20 _differs "$prev"
}

start 80 24 || finish
settle || miss "startup never settled"
check "the alternate screen is held" "$(alt_on)" 1
fs_frame "startup" 80 24
if _vis_has 'ι> iota'; then ok "the banner is on screen"; else miss "no banner"; fi

type_ 'stream 60 5'
key Enter
wait_vis 'l#59 line' || miss "the stream never ended"
settle || miss "the turn never settled"
fs_frame "after the turn" 80 24
if contiguous; then ok "the conversation's rows are contiguous"; else bad "a hole or a duplicate: $(nums | tr '\n' ' ')"; dump_pane; fi
if _vis_lacks 'l#00 line'; then ok "the start of the turn is above the viewport"; else bad "l#00 still on screen"; fi
# The last row of the turn sits on the frame: nothing but the frame's own rows under it.
last="$(row_of 'l#59 line')"
gap="$(cap | sed -n "$((last + 1)),$(($(frame_top) - 1))p" | grep -c '[^[:space:]]' | tr -d ' ')"
check "nothing between the tail and the frame" "$gap" 0

# PgUp half a screen at a time, to the banner.
i=0
while _vis_lacks 'ι> iota' && [ "$i" -lt 30 ]; do
    page PPage || break
    i=$((i + 1))
done
if _vis_has 'ι> iota'; then ok "PgUp reached the banner ($i presses)"; else miss "PgUp never reached the banner"; fi
if _vis_has 'l#00 line'; then ok "…and the first row of the turn"; else bad "the first row of the turn is not on screen"; dump_pane; fi
# Nothing is staged in the frame (tui-plan S2): the scrolled screen shows the viewport's rows
# only, from l#00 on, and the tail is nowhere on it.
if contiguous && [ "$(nums | head -1)" = 0 ]; then
    ok "scrolled back, the rows run from l#00 without a hole"
else
    bad "scrolled: $(nums | tr '\n' ' ')"
    dump_pane
fi
if _vis_lacks 'l#59 line'; then ok "…and the tail is not on screen twice"; else bad "l#59 is on a scrolled-back screen"; dump_pane; fi
if _vis_has 'Ctrl+End'; then ok "the way back to the tail is shown"; else bad "no indicator while scrolled back"; dump_pane; fi
fs_frame "scrolled back" 80 24
# One more PgUp at the top moves nothing (the 2 s window of a negative claim).
if page PPage; then bad "PgUp at the top moved the screen"; dump_pane; else ok "PgUp at the top is a no-op"; fi

# PgDn back to the tail; reaching it follows again (the indicator goes).
i=0
while _vis_lacks 'l#59 line' && [ "$i" -lt 30 ]; do
    page NPage || break
    i=$((i + 1))
done
if _vis_has 'l#59 line'; then ok "PgDn returned to the tail ($i presses)"; else miss "PgDn never returned to the tail"; fi
settle
if contiguous; then ok "at the tail, the rows are contiguous again"; else bad "at the tail: $(nums | tr '\n' ' ')"; dump_pane; fi
check "the tail sits on the frame again" "$(row_of 'l#59 line')" "$last"
if _vis_lacks 'Ctrl+End'; then ok "following again: no indicator"; else bad "the indicator outlived the return"; dump_pane; fi

# Ctrl+Home, then output while paused: the view does not move, the indicator counts it, and
# Ctrl+End follows again.
page C-Home || miss "Ctrl+Home moved nothing"
if _vis_has 'ι> iota'; then ok "Ctrl+Home shows the first line"; else bad "Ctrl+Home is not at the top"; dump_pane; fi
top="$(cap | head -8)"
type_ 'stream 30 5'
key Enter
wait_vis 'new lines · Ctrl+End' || miss "no new-lines count while paused"
check "new output does not move a paused view" "$(cap | head -8)" "$top"
key C-End
wait_vis 'l#29 line' || miss "Ctrl+End did not return to the tail"
settle
if _vis_lacks 'Ctrl+End'; then ok "Ctrl+End follows again"; else bad "still paused after Ctrl+End"; dump_pane; fi
fs_frame "back on the tail" 80 24

# Following the tail: the next turn's rows show without a key.
type_ 'echo-after'
key Enter
wait_vis 'echo: echo-after' || miss "the next turn's reply is not on screen"
fs_frame "the next turn" 80 24

finish
