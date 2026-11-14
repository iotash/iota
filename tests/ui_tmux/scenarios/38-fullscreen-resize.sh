#!/usr/bin/env bash
# L4 scenario 38 — the full screen (IOTA_FULLSCREEN=1, tui-plan S1) through drags: a resize is one
# full repaint, so whatever the pace, the frame is the bottom rows at the width the pane has, and
# the conversation above it has no hole and no duplicate. There is no drag protocol: the full width
# is back within the 50 ms merge, never after DRAG_SETTLE's 2 s.
#
# A: a paced drag (10 steps, 300 ms apart) — judged after every step, within 1.5 s of it.
# B: a fast drag (30 steps, no pause, width and height both moving) — judged at rest.
# C: a drag while a stream runs — every streamed row reachable afterwards, walking back with PgUp
#    (row names repeat the first turn's up to l#39; the drag runs through l#40 on).
#
# Reads the visible pane only (`cap`).
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"
export IOTA_FULLSCREEN=1

nums() { cap | grep -oE '^l#[0-9]+ line' | sed -E 's/^l#0*([0-9]+) line/\1/'; }
contiguous() {
    local prev="" n
    for n in $(nums); do
        if [ -n "$prev" ] && [ "$n" -ne $((prev + 1)) ]; then return 1; fi
        prev="$n"
    done
    [ -n "$prev" ]
}

# _laid_out <w> <h> — one capture shows the frame at width w with its bottom zone on row h, the
# streamed rows contiguous, and nothing between the last of them and the frame.
_laid_out() {
    local s t b last gap
    s="$(cap)"
    t="$(frame_top "$s")"
    b="$(frame_bot "$s")"
    [ -n "$t" ] && [ -n "$b" ] || return 1
    [ "$(row_width "$t" "$s")" = "$1" ] && [ "$(row_width "$b" "$s")" = "$1" ] || return 1
    [ "$((b + 1))" = "$2" ] || return 1
    contiguous || return 1
    last="$(printf '%s\n' "$s" | grep -nE '^l#[0-9]+ line' | tail -1 | cut -d: -f1)"
    gap="$(printf '%s\n' "$s" | sed -n "$((last + 1)),$((t - 1))p" | grep -c '[^[:space:]]' | tr -d ' ')"
    [ "$gap" = 0 ]
}

# step <w> <h> — one resize, judged within 1.5 s (the full width never waits for a settle).
fails=0
step() {
    tm resize-window -t s -x "$1" -y "$2"
    if ! _poll_until 15 _laid_out "$1" "$2"; then
        fails=$((fails + 1))
        echo "    step $1x$2 not laid out:"
        dump_pane
    fi
}

start 100 30 || finish
settle || miss "startup never settled"
type_ 'stream 40 5'
key Enter
wait_vis 'l#39 line' || miss "the stream never ended"
settle || miss "the turn never settled"
_poll_until 10 _laid_out 100 30 || { bad "the turn is not laid out at 100x30"; dump_pane; }

# --- A: paced
for wh in 96x29 92x28 88x27 84x26 80x25 84x24 88x25 92x27 96x28 100x30; do
    step "${wh%x*}" "${wh#*x}"
    sleep 0.3 # the pace of the drag under test
done
check "A: a paced drag — every step laid out at full width on the bottom" "$fails" 0

# --- B: fast — 30 steps without a pause, then judged at rest.
w=100
h=30
i=0
while [ "$i" -lt 30 ]; do
    if [ "$i" -lt 15 ]; then w=$((w - 2)); h=$((h - 1)); else w=$((w + 2)); h=$((h + 1)); fi
    tm resize-window -t s -x "$w" -y "$h"
    i=$((i + 1))
done
if _poll_until 15 _laid_out 100 30; then
    ok "B: a fast 30-step drag — laid out at full width on the bottom once it stops"
else
    bad "B: after the fast drag the screen is not laid out"
    dump_pane
fi
check_frame_intact "B: at rest" 100
tm resize-window -t s -x 60 -y 16
if _poll_until 15 _laid_out 60 16; then ok "B: and after one large jump"; else bad "B: one large jump not laid out"; dump_pane; fi

# --- C: a drag through a stream.
tm resize-window -t s -x 100 -y 30
_poll_until 15 _laid_out 100 30 || { bad "C: not laid out before the stream"; dump_pane; }
type_ 'stream 80 30'
key Enter
# The drag starts at l#40: the rows it runs through (l#40 on) exist in this turn only, so the
# reachability count below cannot be met by the first turn's rows of the same names.
wait_vis 'l#40 line' || miss "C: the stream never reached l#40"
for wh in 90x26 80x22 70x18 80x22 90x26 100x30 90x24 80x20; do
    tm resize-window -t s -x "${wh%x*}" -y "${wh#*x}"
    sleep 0.15 # the pace of the drag under test
done
wait_vis 'l#79 line' || miss "C: the stream never ended"
settle || miss "C: the turn never settled"
_poll_until 15 _laid_out 80 20 || { bad "C: not laid out after the stream"; dump_pane; }
# Walk back with PgUp, gathering the rows each screen shows; every one of the 80 must turn up.
seen="$(nums)"
i=0
while _vis_lacks 'ι> iota' && [ "$i" -lt 60 ]; do
    prev="$(cap)"
    key PPage
    _poll_until 20 eval '[ "$(cap)" != "$prev" ]' || break
    if ! contiguous; then
        # The frame's staging window holds the tail while scrolled; judge the viewport only.
        all="$(nums)"
        n="$(printf '%s\n' "$all" | wc -l | tr -d ' ')"
        view="$(printf '%s\n' "$all" | head -n "$((n - 4))")"
        prevn=""
        for k in $view; do
            if [ -n "$prevn" ] && [ "$k" -ne $((prevn + 1)) ] && [ "$k" -ne 0 ]; then
                bad "C: a hole or a duplicate while scrolled: $(printf '%s ' $all)"
                dump_pane
                break
            fi
            prevn="$k"
        done
    fi
    seen="$seen
$(nums)"
    i=$((i + 1))
done
check "C: every row of the second stream is reachable" \
    "$(printf '%s\n' "$seen" | grep -E '^[0-9]+$' | sort -un | awk '$1 < 80' | wc -l | tr -d ' ')" 80

finish
