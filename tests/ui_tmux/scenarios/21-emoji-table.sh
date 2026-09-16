#!/usr/bin/env bash
# L4 scenario 21 (docs/TUI-VERIFY.md §6.4) — emoji, flags and VS16 in a table.
#
# Two rulers decide whether a bordered table lines up: the app's (`text/width.rs`, a grapheme
# ruler with the VS16 rule) pads every cell, the emulator's decides where the padding ends.
# `capture-pane` cannot tell them apart — it hands back glyphs, not cells — so every rendered
# row is typed into a scratch pane and measured by tmux's OWN cursor (`emu_width`, lib.sh): if
# the two rulers disagree on any glyph, that row's width differs from its neighbours'. The table
# carries a plain emoji, a flag (two regional indicators), a VS16 sequence and a skin-tone
# modifier — one of each place they can disagree — and the renderer's own defence is pinned
# too: variation selectors are stripped before layout (`markdown/blocks/table.rs`), so the
# bytes the terminal receives never carry U+FE0F.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

# rows_between <start marker> <end marker> — physical rows the payload occupied.
rows_between() {
    local s e
    s="$(capall | grep -nF -- "$1" | tail -1 | cut -d: -f1)"
    e="$(capall | grep -nF -- "$2" | tail -1 | cut -d: -f1)"
    if [ -n "$s" ] && [ -n "$e" ]; then echo $((e - s - 1)); else echo "?"; fi
}

# table_rows — every rendered row of the table, in order (borders and cells alike).
table_rows() { capall | grep -E '^[┌├└│]'; }

VS16="$(printf '\357\270\217')"

start 80 24 || finish
settle || bad "startup never settled"

type_ 'emoji'
key Enter
wait_all 'EMOJIEND' || bad "the table never arrived"
settle || bad "frame never settled after the turn"

# --- the shape: 4 data rows + header = 5 rendered rows, a rule between each pair, two borders
check "the table is 11 rendered rows" "$(table_rows | wc -l | tr -d ' ')" 11
check "…and no row wrapped (rows between the markers, blank spacers included)" "$(rows_between EMOJISTART EMOJIEND)" 13
check "the header row" "$(capall | grep -cE '^│ id +│ glyph +│ flag +│ vs16 +│ note +│$' | tr -d ' ')" 1
check_once "the emoji row" '│ smile'
check_once "the skin-tone row" '│ thumbs'
check_once "the CJK row" '│ cjk'

# --- the defence: no variation selector reaches the terminal
if capall | LC_ALL=C grep -qaF -- "$VS16"; then
    bad "a U+FE0F reached the terminal"
else
    ok "variation selectors are stripped before the table is laid out (no U+FE0F on screen)"
fi

# --- the law: every row is the same width by tmux's ruler
widths=""
first=""
mismatch=0
while IFS= read -r row; do
    w="$(emu_width "$row")"
    widths="$widths $w"
    if [ -z "$first" ]; then first="$w"; elif [ "$w" != "$first" ]; then mismatch=$((mismatch + 1)); fi
done <<EOR
$(table_rows)
EOR
if [ -n "$first" ] && [ "$mismatch" -eq 0 ]; then
    ok "all 11 rows are $first columns wide by tmux's own ruler"
else
    bad "the rows disagree on their width by tmux's ruler:$widths"
    table_rows
fi
check_frame_intact "after the emoji table" 80

finish
