#!/usr/bin/env bash
# L4 scenario 50 — the completion list floats over the conversation (sel-float).
#
# Over a turn long enough to fill the screen, typing `/` opens the list and typing a letter narrows
# it. The rows above the list's area must not change: not when it opens (the conversation keeps
# its rectangle; the area is drawn over its last rows), and not when the matches narrow (the area
# keeps its tallest height, so its top edge — the upper separator — holds still too). Closing the
# list gives the covered rows back. Read on a real terminal with `capture-pane`, the user's claim.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

# capb — the pane with its escapes (`capture-pane -e`): the bytes a row is drawn with, compared.
capb() { tm capture-pane -ept s 2>/dev/null; }
# above <row> <capture> — the capture's rows 1..<row>.
above() { printf '%s\n' "$2" | sed -n "1,${1}p"; }
_list_open() { [ "$(list_block | wc -l | tr -d ' ')" -gt "$1" ]; }

start 80 24 || finish
settle || miss "startup never settled"

type_ 'stream 60 5'
key Enter
wait_vis 'l#59 line' || miss "the stream never ended"
settle || miss "the turn never settled"
closed="$(cap)"
closed_b="$(capb)"
shot "closed"

type_ '/'
_poll_until 40 _list_open 3 || miss "typing / never opened the list"
settle || miss "the open list never settled"
open="$(cap)"
open_b="$(capb)"
edge="$(frame_top "$open")"
rows_open="$(list_block "$open" | wc -l | tr -d ' ')"
shot "/ — the list open"
if [ "$(above $((edge - 2)) "$open_b")" = "$(above $((edge - 2)) "$closed_b")" ]; then
    ok "opening the list moved no conversation row, byte for byte (rows 1-$((edge - 2)))"
else
    bad "opening the list moved the conversation"
    diff <(above $((edge - 2)) "$closed_b") <(above $((edge - 2)) "$open_b") | head -10
fi
check "the conversation runs to the spacer: the row over it is a conversation row" \
    "$(above $((edge - 2)) "$open" | tail -1 | grep -cE '^l#[0-9]+ line' | tr -d ' ')" 1

type_ 'd'
_poll_until 40 eval '[ "$(list_block | wc -l | tr -d " ")" -lt '"$rows_open"' ]' ||
    miss "typing d never narrowed the list"
settle || miss "the narrowed list never settled"
narrow="$(cap)"
narrow_b="$(capb)"
shot "/d — the list narrowed"
check "the area's top edge holds its row" "$(frame_top "$narrow")" "$edge"
if [ "$(above "$edge" "$narrow_b")" = "$(above "$edge" "$open_b")" ]; then
    ok "narrowing changed no row above the list, its top edge included, byte for byte (rows 1-$edge)"
else
    bad "narrowing moved the rows above the list"
    diff <(above "$edge" "$open_b") <(above "$edge" "$narrow_b") | head -10
fi
check "the narrowed list sits on the composer" "$(($(composer_row) - 1))" \
    "$(printf '%s\n' "$narrow" | grep -nE '^(▸ |  )[a-z]' | tail -1 | cut -d: -f1)"
check "the composer holds /d" "$(composer_line 1)" "/d"

key BSpace
key BSpace
_poll_until 40 eval '[ "$(list_block | wc -l | tr -d " ")" = 0 ]' || miss "the list never closed"
settle || miss "the closed list never settled"
shot "closed again"
if [ "$(capb)" = "$closed_b" ]; then
    ok "closing gives the screen back, byte for byte"
else
    bad "the screen after closing is not the one before opening"
    diff <(printf '%s\n' "$closed_b") <(capb) | head -10
fi

finish
