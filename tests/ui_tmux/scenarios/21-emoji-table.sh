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

# --- can this emulator judge the table at all? Its ruler must give the app's answer — two
# columns — for each glyph class the table carries. brew's tmux on the macOS runner does not
# (CI 2026-09-16: the same bytes were 15 rows of mixed widths there and 11 rows of one width
# under tmux 3.7c), and an emulator whose ruler differs from the app's cannot line up ANY
# table: that is a fact about the emulator, recorded as a wart with the numbers, never a
# failure of the renderer. The rows themselves, the stripped U+FE0F and the frame are still
# checked on every emulator.
ruler_ok=1
ruler_note=""
for g in "😀" "🇯🇵" "☕" "👍🏽"; do
    w="$(emu_width "$g")"
    if [ "$w" != "2" ]; then ruler_ok=0; ruler_note="$ruler_note $g=$w"; fi
done
if [ "$ruler_ok" -eq 1 ]; then
    ok "tmux's ruler agrees with the app's: emoji, flag, VS16 base and skin tone are 2 columns each"
else
    wart "tmux's ruler disagrees with the app's on:$ruler_note — the alignment below is recorded, not enforced"
fi

# --- the shape: 4 data rows + header = 5 rendered rows, a rule between each pair, two borders
rows="$(table_rows | wc -l | tr -d ' ')"
between="$(rows_between EMOJISTART EMOJIEND)"
if [ "$ruler_ok" -eq 1 ]; then
    check "the table is 11 rendered rows" "$rows" 11
    check "…and no row wrapped (rows between the markers, blank spacers included)" "$between" 13
else
    wart "the table is $rows rendered rows and $between rows between the markers under this emulator's ruler (11 and 13 under the app's)"
fi
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
elif [ "$ruler_ok" -eq 0 ]; then
    wart "the rows disagree on their width by tmux's ruler:$widths — its ruler is not the app's (see above)"
else
    bad "the rows disagree on their width by tmux's ruler:$widths"
    table_rows
fi
check_frame_intact "after the emoji table" 80

finish
