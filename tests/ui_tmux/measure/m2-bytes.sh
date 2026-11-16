#!/usr/bin/env bash
# M2 (tui-plan §7) — not a check: what the tmux iota runs in sends to ITS terminal, per gesture
# (S1–S4 measured the inline terminal beside it; it is gone). X-55's harness: a 120×40 pane, the bytes counted on
# tmux's outer side. Here that side is a pane of a second, measuring tmux server, attached to the
# first and tapped with `pipe-pane`; status lines are off in both so the pane is exactly 120×40.
#
#   typing   — 11 characters typed, one at a time, then deleted
#   stream   — `stream 100 20`, from Enter to the settled frame
#   drag     — 30 resize steps of the outer window (120×40 → 90×25 → 120×40), then at rest
#
# Run: IOTA_TMUX=1 IOTA_TMUX_MEASURE=1 cargo test --test ui_tmux tmux_measure_m2 -- --nocapture
# (TMUX_BIN picks the tmux under test; the measuring server is the same binary).
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

OUT_SOCKET="$TMUX_SOCKET-outer"
OUT="$SCEN_TMP/outer.bytes"
tmo() { "$TMUX_BIN" -L "$OUT_SOCKET" "$@"; }
cleanup_outer() { tmo kill-server >/dev/null 2>&1 || true; cleanup; }
trap cleanup_outer EXIT

bytes() { wc -c <"$OUT" | tr -d ' '; }
# What iota itself wrote (the inner pane's tap, lib.sh's RAW): its bytes and its DEC 2026 blocks —
# on tmux 3.7c each block is one whole-pane repaint (X-55).
ibytes() { wc -c <"$RAW" | tr -d ' '; }
iblocks() { LC_ALL=C grep -ao "$(printf '\033')\\[?2026h" "$RAW" | wc -l | tr -d ' '; }
report() {
    echo "$world: $1: outer $(($(bytes) - b0)) B; iota wrote $(($(ibytes) - i0)) B in $(($(iblocks) - k0)) sync blocks"
}
mark() { b0="$(bytes)"; i0="$(ibytes)"; k0="$(iblocks)"; }
# quiet — the outer tap has stopped growing for 0.5 s (the measured gesture is over).
quiet() {
    local prev now same=0 i=0
    prev="$(bytes)"
    while [ "$i" -lt 100 ]; do
        sleep 0.1
        now="$(bytes)"
        if [ "$now" = "$prev" ]; then same=$((same + 1)); [ "$same" -ge 5 ] && return 0; else same=0; fi
        prev="$now"
        i=$((i + 1))
    done
}

measure_world() {
    world="$1"
    start 120 40 || finish
    tm set-option -g status off
    settle
    tmo kill-server >/dev/null 2>&1 || true
    tmo -f /dev/null new-session -d -s m -x 120 -y 40 "$TMUX_BIN -L $TMUX_SOCKET attach -t s"
    tmo set-option -g status off
    tmo resize-window -t m -x 120 -y 40
    : >"$OUT"
    pipe_raw
    tmo pipe-pane -t m -o "cat >> $OUT"
    _poll_until 50 _frame_up || miss "$world: the composer is not on screen"
    settle
    quiet
    echo "$world: pane $(tm display-message -pt s '#{pane_width}x#{pane_height}')"

    mark
    for c in h e l l o _ w o r l d; do
        type_ "$c"
        sleep 0.05 # a typist's pace
    done
    quiet
    report "typing 11 characters"
    mark
    for _ in 1 2 3 4 5 6 7 8 9 10 11; do
        key BSpace
        sleep 0.05
    done
    quiet
    report "deleting them"

    type_ 'stream 100 20'
    quiet
    mark
    key Enter
    wait_vis 'l#99 line' || miss "$world: the stream never ended"
    settle
    quiet
    report "stream 100 20"

    mark
    local i=0 w=120 h=40
    while [ "$i" -lt 30 ]; do
        if [ "$i" -lt 15 ]; then w=$((w - 2)); h=$((h - 1)); else w=$((w + 2)); h=$((h + 1)); fi
        tmo resize-window -t m -x "$w" -y "$h"
        sleep 0.05 # the pace of the drag
        i=$((i + 1))
    done
    settle
    quiet
    report "30-step drag"
    tmo kill-server >/dev/null 2>&1 || true
}

echo "tmux under test: $("$TMUX_BIN" -V)"
measure_world fullscreen

finish
