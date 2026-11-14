#!/usr/bin/env bash
# L4 scenario 39 — leaving the full screen (IOTA_FULLSCREEN=1, tui-plan S1): an idle Ctrl+D hands
# back the main screen, and the conversation is written into it at the width the pane has at exit
# — every row once, rows wider than that width wrapped, the frame not at all — so it survives in
# the shell's history.
#
# The pane is kept (`remain-on-exit`) and read back AFTER the exit with `capall`: the main screen
# and its history, which is where the shell's scrollback lives.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"
export IOTA_FULLSCREEN=1

start 100 24 || finish
settle || miss "startup never settled"
type_ 'stream 50 5'
key Enter
wait_vis 'l#49 line' || miss "the stream never ended"
settle || miss "the turn never settled"
# A row as wide as the pane, then the pane narrowed: the dump wraps it at the exit width.
type_ 'exact'
key Enter
wait_vis 'EXACTEND' || miss "the exact-width reply never ended"
settle || miss "the second turn never settled"
tm resize-window -t s -x 60 -y 24
_poll_until 20 eval '[ "$(row_width "$(frame_bot)")" = 60 ]' || miss "the frame never took 60 columns"

tm set-option -t s remain-on-exit on
# An idle Ctrl+D that lands after the turn's last frame but before the REPL parks its next read
# meets no waiter and no cancel scope and is dropped (both worlds; inline, the wait for the frame's
# width above sits through DRAG_SETTLE and hides the gap). One more press then is still idle.
key C-d
_poll_until 20 pane_dead || key C-d
_poll_until 50 pane_dead || miss "iota did not exit on an idle Ctrl+D"
check "the main screen is back" "$(tm display-message -pt s '#{alternate_on}')" 0

all="$(capall)"
count() { printf '%s\n' "$all" | grep -cF -- "$1" | tr -d ' '; }
check "the banner is in the history once" "$(count 'ι> iota')" 1
check "the user's block is in the history once" "$(count '❯ stream 50 5')" 1
missing=0
dupes=0
k=0
while [ "$k" -lt 50 ]; do
    n="$(count "$(printf 'l#%02d line' "$k")")"
    [ "$n" = 0 ] && missing=$((missing + 1))
    [ "$n" -gt 1 ] && dupes=$((dupes + 1))
    k=$((k + 1))
done
check "every streamed row is in the history" "$missing" 0
check "…and none twice" "$dupes" 0
check "the reply after it is there too" "$(count 'EXACTEND')" 1
# 80 `x` at 60 columns: one row of 60 and one of 20.
check "the 80-column row wrapped at the exit width" \
    "$(printf '%s\n' "$all" | grep -cxE 'x{60}|x{20}' | tr -d ' ')" 2
check "no frame separator was written out" "$(count '┄┄┄┄')" 0
check "no status row was written out" "$(count ' fake · ')" 0

finish
