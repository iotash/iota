#!/usr/bin/env bash
# L4 scenario 41 — searching the full screen's conversation (IOTA_FULLSCREEN=1, tui-plan S3): a word
# ~110 screens back is found from the tail, highlighted and counted; n/p walk and wrap; Esc puts the
# view back exactly where it was; Enter leaves it on the hit with the way back shown; a query that
# matches nothing says so and moves nothing; and following the tail, `/` is still the first
# character of a slash command.
#
# Reads the visible pane only (`cap`, and `capture-pane -e` for the highlight).
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"
export IOTA_FULLSCREEN=1

search_line() { cap | sed -n '19p'; }
_screen_is() { [ "$(cap)" = "$1" ]; }

start 80 24 || finish
settle || miss "startup never settled"
type_ 'needle-early'
key Enter
wait_vis 'echo: needle-early' || miss "the first reply never came"
type_ 'stream 2000 1'
key Enter
wait_vis 'l#1999 line' || miss "the stream never ended"
settle || miss "the turn never settled"

# Scrolled back a little: the place Esc must return to.
key PPage
key PPage
_poll_until 20 _vis_has 'Ctrl+End' || miss "PgUp did not scroll back"
settle
before="$(cap)"

key C-r
type_ 'needle-early'
wait_vis 'echo: needle-early' || miss "the live query never reached the hit"
# (The field draws a blank cell for its cursor, so the gap is read squeezed.)
check "the search line shows the query and its count" "$(search_line | grep -oE '^/needle-early +[0-9]+/[0-9]+' | tr -s ' ')" "/needle-early 2/2"
if tm capture-pane -e -pt s | grep -F 'needle-early' | grep -qF "$(printf '\033')[7"; then
    ok "the hit is highlighted"
else
    bad "no highlight on the hit"
fi
key Escape
if _poll_until 30 _screen_is "$before"; then ok "Esc puts the view back exactly"; else bad "Esc came back somewhere else"; dump_pane; fi

# Again, applied: n/p wrap at the ends; Enter stays on the hit.
key C-r
type_ 'needle-early'
key Enter
wait_vis 'n/p walk' || miss "Enter did not apply the query"
key n
_poll_until 30 eval '[ -n "$(search_line | grep -F "1/2")" ]' || bad "n past the last hit did not wrap to the first"
key p
_poll_until 30 eval '[ -n "$(search_line | grep -F "2/2")" ]' || bad "p before the first hit did not wrap to the last"
key Enter
_poll_until 30 _vis_lacks 'n/p walk' || miss "Enter did not leave the search"
if _vis_has 'echo: needle-early' && _vis_has 'Ctrl+End'; then
    ok "Enter left the view on the hit, the way back shown"
else
    bad "Enter did not leave the view on the hit"
    dump_pane
fi
key C-End
wait_vis 'l#1999 line' || miss "Ctrl+End did not return to the tail"

# A query that matches nothing.
settle
tail="$(cap | head -17)"
key C-r
type_ 'zzqqxx'
wait_vis 'no match' || miss "no word that nothing matched"
check "a query matching nothing leaves the view alone" "$(cap | head -17)" "$tail"
key Escape
_poll_until 30 _vis_lacks 'no match' || miss "Esc did not end the search"

# Following the tail, `/` is the slash command's.
type_ '/'
if _poll_until 20 eval '[ "$(composer_row)" != "" ] && cap | grep -qE "^❯ /$"'; then
    ok "following the tail, / is typed into the composer"
else
    bad "/ did not reach the composer"
    dump_pane
fi
key BSpace

finish
