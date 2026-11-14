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
# The frame keeps the turn's last rows in its staging window (TAIL_KEEP = 4, drawn as today in
# S1), so a scrolled screen shows the viewport's rows from l#00 and, in the frame, l#56..l#59.
all="$(nums)"
n="$(printf '%s\n' "$all" | wc -l | tr -d ' ')"
view="$(printf '%s\n' "$all" | head -n "$((n - 4))")"
staged="$(printf '%s\n' "$all" | tail -n 4 | tr '\n' ' ')"
if contiguous "$view" && [ "$(printf '%s\n' "$view" | head -1)" = 0 ]; then
    ok "scrolled back, the viewport's rows run from l#00 without a hole"
else
    bad "scrolled: $(printf '%s ' $all)"
    dump_pane
fi
check "…and the frame's staging window keeps the tail" "$staged" "56 57 58 59 "
fs_frame "scrolled back" 80 24
# One more PgUp at the top moves nothing (the 2 s window of a negative claim).
if page PPage; then bad "PgUp at the top moved the screen"; dump_pane; else ok "PgUp at the top is a no-op"; fi

# PgDn back to the tail: l#55 is the viewport's own last row (l#56.. are the frame's).
i=0
while _vis_lacks 'l#55 line' && [ "$i" -lt 30 ]; do
    page NPage || break
    i=$((i + 1))
done
if _vis_has 'l#55 line'; then ok "PgDn returned to the tail ($i presses)"; else miss "PgDn never returned to the tail"; fi
settle
if contiguous; then ok "at the tail, the rows are contiguous again"; else bad "at the tail: $(nums | tr '\n' ' ')"; dump_pane; fi
check "the tail sits on the frame again" "$(row_of 'l#59 line')" "$last"

# Following the tail: the next turn's rows show without a key.
type_ 'echo-after'
key Enter
wait_vis 'echo: echo-after' || miss "the next turn's reply is not on screen"
fs_frame "the next turn" 80 24

finish
