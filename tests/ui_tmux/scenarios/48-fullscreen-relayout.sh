#!/usr/bin/env bash
# L4 scenario 48 — the conversation re-lays-out FROM SOURCE when the window's width changes
# (tui-plan S7a), the owner's own check: a reply with a table, a nested list, a code block and a
# quote on screen, then the window pulled from 80 columns to 160 and back to 60. At each width:
#   * nothing is lost — every block's marker word is on screen;
#   * the TABLE's box is whole (every row ends on a border glyph), narrower than the window, and
#     its long cells sit on one row each at 160 and wrap inside their column below that;
#   * the LIST's wrapped rows hang under the item text (4 columns for ` 1. `, 6 for the nested
#     `    • `), and at 160 each item is one row again;
#   * the CODE block's long line wraps under its two-column gutter, never at column 0, and is one
#     row again at 160;
#   * the QUOTE's `│` bar is on every row it wraps onto;
#   * the spacing reads the same: as many blank rows as at 80, never two in a row.
# Then, at 60 (the review's B1): a drag over the table and the code block copies them line for
# line, as shown — a laid-out block is many lines, never joined into one.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

# A stub clipboard tool, so the developer's own clipboard is never touched (scenario 46's way).
mkdir -p "$SCEN_TMP/bin"
for tool in pbcopy wl-copy; do
    printf '#!/bin/sh\ncat > "%s"\n' "$SCEN_TMP/clip" >"$SCEN_TMP/bin/$tool"
    chmod +x "$SCEN_TMP/bin/$tool"
done
IOTA_ENV="-u SSH_CONNECTION -u SSH_TTY -u SSH_CLIENT PATH=$SCEN_TMP/bin:$PATH"

start 80 60 || finish
settle || miss "startup never settled"
type_ 'relayout'
key Enter
wait_vis 'RELAYOUTEND' || miss "the reply never completed"
settle || miss "the frame never settled after the reply"

reply() { cap | sed -n '/RELAYOUTSTART/,/RELAYOUTEND/p'; }
blanks() { printf '%s\n' "$1" | grep -c '^$' | tr -d ' '; }
doubles() { printf '%s\n' "$1" | awk 'prev == "" && $0 == "" && NR > 1 { n++ } { prev = $0 } END { print n + 0 }'; }

base=""
for w in 80 160 60; do
    tm resize-window -t s -x "$w" -y 60
    _poll_until 50 eval '[ "$(tm display-message -pt s "#{pane_width}")" = "$w" ]' || miss "$w: the pane never took the width"
    settle || miss "$w: the frame never settled after the resize"
    r="$(reply)"
    lost=""
    for word in RELAYOUTSTART region conversation LISTHEAD listone listtwo nestedbullet listten 'fn main' quotehead RELAYOUTEND; do
        printf '%s\n' "$r" | grep -qF -- "$word" || lost="$lost $word"
    done
    check "$w: nothing lost" "$lost" ""

    table="$(printf '%s\n' "$r" | sed -n '/^┌/,/^└/p')"
    check "$w: the table's box is whole" "$(printf '%s\n' "$table" | grep -cvE '(┐|│|┤|┘)$' | tr -d ' ')" 0
    tw=$(($(printf '%s\n' "$table" | head -1 | grep -o '─' | wc -l | tr -d ' ') + 3))
    if [ "$tw" -gt 3 ] && [ "$tw" -lt "$w" ]; then ok "$w: the table is narrower than the window ($tw)"; else bad "$w: the table is $tw columns wide"; fi
    one="$(printf '%s\n' "$table" | grep -cF 'in the frame above the composer' | tr -d ' ')"
    check "$w: the long cell is on one row only at 160" "$one" "$([ "$w" = 160 ] && echo 1 || echo 0)"

    if [ "$w" = 160 ]; then
        check "$w: the long item is one row" "$(printf '%s\n' "$r" | grep -cF ' 1. listone has text long enough to wrap at sixty columns and then keep going a little' | tr -d ' ')" 1
        check "$w: the nested bullet is one row" "$(printf '%s\n' "$r" | grep -cF '    • nestedbullet also long enough to wrap when the screen is sixty columns wide' | tr -d ' ')" 1
        check "$w: the long code line is one row" "$(printf '%s\n' "$r" | grep -cE '^      let s = "q{70}";$' | tr -d ' ')" 1
    else
        check "$w: the long item's next row hangs at column 4" "$(printf '%s\n' "$r" | grep -A1 '^ 1\. listone' | tail -1 | grep -cE '^    [^ ]' | tr -d ' ')" 1
        check "$w: the nested bullet's next row hangs at column 6" "$(printf '%s\n' "$r" | grep -A1 '^    • nestedbullet' | tail -1 | grep -cE '^      [^ ]' | tr -d ' ')" 1
    fi
    check "$w: no code row starts at column 0" "$(printf '%s\n' "$r" | grep -c '^q' | tr -d ' ')" 0
    check "$w: the quote's every row has the bar" "$(printf '%s\n' "$r" | sed -n '/quotehead/,/^$/p' | grep -v '^$' | grep -cv '^│ ' | tr -d ' ')" 0

    check "$w: never two blank rows in a row" "$(doubles "$r")" 0
    if [ -z "$base" ]; then
        base="$(blanks "$r")"
    else
        check "$w: as many blank rows as at 80" "$(blanks "$r")" "$base"
    fi
done
[ "$(printf '%s\n' "$(reply)" | sed -n '/quotehead/,/^$/p' | grep -c '^│ ' | tr -d ' ')" -gt 1 ] && ok "60: the quote wraps onto more than one row" || bad "60: the quote did not wrap"

# The copy, at 60 columns: drag from the table's top border to the end of the code block.
shot="$(cap)"
top="$(printf '%s\n' "$shot" | grep -n '^┌' | head -1 | cut -d: -f1)"
bot="$(printf '%s\n' "$shot" | grep -n '^  }$' | head -1 | cut -d: -f1)"
if [ -n "$top" ] && [ -n "$bot" ]; then
    want="$(printf '%s\n' "$shot" | sed -n "${top},${bot}p" | sed 's/ *$//')"
    mouse 0 1 "$top"
    mouse 32 60 "$bot"
    mouse 0 60 "$bot" m
    _buffer() { tm show-buffer 2>/dev/null; }
    _has_buffer() { _buffer | grep -qF 'fn main'; }
    _poll_until 100 _has_buffer || miss "the drag over the table and the code block copied nothing"
    check "the copy has the table's and the code block's lines apart" "$(_buffer | wc -l | tr -d ' ')" "$(($(printf '%s\n' "$want" | wc -l | tr -d ' ') - 1))"
    check "…and is the rows as shown, line for line" "$(_buffer)" "$want"
else
    bad "the table or the code block is not on screen at 60 (top=$top bot=$bot)"
fi

finish
