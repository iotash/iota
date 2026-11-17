#!/usr/bin/env bash
# L4 scenario 50 — the completion list floats over the conversation (sel-float, sel-top).
#
# A fresh session: the banner starts at the screen's top and the list opens in the blank rows under
# it — opening, narrowing and closing the list leave the banner's rows byte for byte where they
# were. Then, over a turn long enough to fill the screen, typing `/` opens the list and typing a
# letter narrows it. The rows above the list's area must not change when it opens (the
# conversation keeps its rectangle; the area is drawn over its last rows); the area is exactly the
# list, so narrowing brings its top edge — the upper separator — down and gives back the rows it
# no longer covers, the conversation as it was, never blank rows. One blank row parts the list
# from the composer. Closing the list gives every covered row back. Read on a real terminal with `capture-pane`, the user's claim.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

# capb — the pane with its escapes (`capture-pane -e`): the bytes a row is drawn with, compared.
capb() { tm capture-pane -ept s 2>/dev/null; }
# above <row> <capture> — the capture's rows 1..<row>.
above() { printf '%s\n' "$2" | sed -n "1,${1}p"; }
_list_open() { [ "$(list_block | wc -l | tr -d ' ')" -gt "$1" ]; }
_list_shut() { [ "$(list_block | wc -l | tr -d ' ')" = 0 ]; }

start 80 24 || finish
settle || miss "startup never settled"

# --- a fresh session: the banner at the top, the list under it -----------------------------------
fresh="$(cap)"
fresh_b="$(capb)"
shot "fresh"
check "the banner's card starts on the first row" "$(printf '%s\n' "$fresh" | head -1 | cut -c1-3)" "╭──"
banner="$(printf '%s\n' "$fresh" | grep -n '^╰' | head -1 | cut -d: -f1)"
check "the banner's card is five rows" "$banner" 5

type_ '/'
_poll_until 40 _list_open 3 || miss "typing / never opened the list over the banner"
settle || miss "the open list never settled"
shot "fresh, / — the list open"
s="$(capb)"
same_banner() {
    if [ "$(above "$banner" "$2")" = "$(above "$banner" "$fresh_b")" ]; then
        ok "$1 moved no banner row, byte for byte (rows 1-$banner)"
    else
        bad "$1 moved the banner"
        diff <(above "$banner" "$fresh_b") <(above "$banner" "$2") | cat -v | head -10
    fi
}
same_banner "opening the list" "$s"
check "the list covers no banner row: its edge is below the card" \
    "$(($(frame_top) > banner))" 1
type_ 'd'
_poll_until 40 _list_open 0 || miss "the narrowed list vanished"
settle || miss "the narrowed list never settled"
shot "fresh, /d — the list narrowed"
same_banner "narrowing" "$(capb)"
key BSpace
key BSpace
_poll_until 40 _list_shut || miss "the list over the banner never closed"
settle || miss "the closed list never settled"
# Everything down to the upper separator byte for byte; the composer row was typed into and
# emptied, so its blank cells are written now (tmux's `-e` encodes them differently) — the whole
# screen is compared as text.
fedge="$(frame_top "$fresh")"
if [ "$(above "$fedge" "$(capb)")" = "$(above "$fedge" "$fresh_b")" ] && [ "$(cap)" = "$fresh" ]; then
    ok "closing gives the fresh screen back (rows 1-$fedge byte for byte, the whole screen as text)"
else
    bad "the fresh screen after closing is not the one before opening"
    diff <(printf '%s\n' "$fresh_b") <(capb) | cat -v | head -10
fi

# --- a full screen: the area covers the tail, narrowing reveals it -------------------------------
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
rows_narrow="$(list_block "$narrow" | wc -l | tr -d ' ')"
nedge="$(frame_top "$narrow")"
shot "/d — the list narrowed"
check "the area's top edge comes down with the list" "$nedge" "$((edge + rows_open - rows_narrow))"
check "no blank row between the edge and the list" \
    "$(printf '%s\n' "$narrow" | sed -n "$((nedge + 1))p" | grep -cE '^(▸ |  )[a-z]' | tr -d ' ')" 1
if [ "$(above $((edge - 2)) "$narrow_b")" = "$(above $((edge - 2)) "$open_b")" ]; then
    ok "narrowing changed no row above the list, byte for byte (rows 1-$((edge - 2)))"
else
    bad "narrowing moved the rows above the list"
    diff <(above $((edge - 2)) "$open_b") <(above $((edge - 2)) "$narrow_b") | head -10
fi
check "the rows the narrowing gave back show the conversation as it was" \
    "$(above $((nedge - 2)) "$narrow")" "$(above $((nedge - 2)) "$closed")"
crow="$(composer_row)"
check "the narrowed list sits one blank row over the composer" "$((crow - 2))" \
    "$(printf '%s\n' "$narrow" | grep -nE '^(▸ |  )[a-z]' | tail -1 | cut -d: -f1)"
check "the gap row between the list and the composer is blank, no glyph, no colour" \
    "$(printf '%s\n' "$narrow_b" | sed -n "$((crow - 1))p" | tr -d ' ')" ""
check "the composer holds /d" "$(composer_line 1)" "/d"

key BSpace
key BSpace
_poll_until 40 _list_shut || miss "the list never closed"
settle || miss "the closed list never settled"
shot "closed again"
if [ "$(capb)" = "$closed_b" ]; then
    ok "closing gives the screen back, byte for byte"
else
    bad "the screen after closing is not the one before opening"
    diff <(printf '%s\n' "$closed_b") <(capb) | head -10
fi

finish
