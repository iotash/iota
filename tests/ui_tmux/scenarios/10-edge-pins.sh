#!/usr/bin/env bash
# L4 edge pins (TUI_TEST_PLAN §L4 "Edge pins"): a wide rune straddling the last column, a
# line exactly the terminal width (T-01), a terminal shorter than the frame needs, and a
# paste larger than a screen.
#
# These are the places where the two rulers (iota's grapheme ruler and the emulator's cell
# accounting) can disagree; a disagreement costs a row, and a lost row desyncs the frame
# forever. Each pin measures ROWS, not glyphs, because the row count is what the region
# sizes its inserts from.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

# rows_between <start marker> <end marker> — physical rows the payload occupied.
rows_between() {
    local s e
    s="$(capall | grep -nF -- "$1" | tail -1 | cut -d: -f1)"
    e="$(capall | grep -nF -- "$2" | tail -1 | cut -d: -f1)"
    if [ -n "$s" ] && [ -n "$e" ]; then echo $((e - s - 1)); else echo "?"; fi
}

start 80 24 || finish
settle || bad "startup never settled"

# ------------------------------------------------- pin 1: a line EXACTLY 80 columns (T-01)
# Go's sanitizeOverflow is absent by design (DIVERGENCES T-01): a full-width line must not
# spill an extra row, or every insert after it is one row out.
type_ 'exact'
key Enter
wait_all 'EXACTEND' || bad "the exact-width payload never arrived"
settle || bad "frame never settled after the exact-width turn"
check "a line of exactly 80 columns occupies ONE row" "$(rows_between EXACTSTART EXACTEND)" 1
row="$(capall | grep -m1 '^xxxx')"
check "…with its last column intact" "$(printf '%s' "$row" | wc -c | tr -d ' ')" 80

# ------------------------------------------------- pin 2: a wide rune straddling col 79/80
# 1 + 40 double-width runes = 81 columns: the 40th cannot fit and must move to the next
# row whole. A ruler that counted it as one column would emit an 80-cell row plus a
# straggler and lose the accounting.
type_ 'straddle'
key Enter
wait_all 'WIDEEND' || bad "the straddle payload never arrived"
settle || bad "frame never settled after the straddle turn"
check "81 columns of CJK wrap into exactly TWO rows" "$(rows_between WIDESTART WIDEEND)" 2
head_row="$(capall | grep -m1 '^y中')"
wide="$(printf '%s' "$head_row" | grep -o '中' | wc -l | tr -d ' ')"
check "the head row stops at 79 columns: 1 + 39 wide runes (no straddle)" "$wide" 39
tail_row="$(capall | grep -nF 'WIDEEND' | tail -1 | cut -d: -f1)"
check "the wrapped tail is the single leftover rune" "$(capall | sed -n "$((tail_row - 1))p")" '中'

# ------------------------------------------------- pin 3: a paste larger than a screen
{
    i=0
    while [ "$i" -lt 50 ]; do
        printf 'pl%02d payload\n' "$i"
        i=$((i + 1))
    done
} | tm load-buffer -
tm paste-buffer -pt s
wait_vis '[#1 pl00 payload… 50 lines]' || bad "the oversized paste tag never appeared"
check "a 50-line paste is still ONE composer row" "$(composer_block | wc -l | tr -d ' ')" 1
check_frame_intact "with an oversized paste tag" 80

key Enter
wait_all 'echo: pl00 payload' || bad "the oversized paste never reached the model"
settle || bad "frame never settled after the oversized submit"
check_once "the transcript echo stops at the 20th line" '  pl19 payload'
check_once "…and says how many it hid" '  … +30 more lines'
check "the transcript echo really stopped there" "$(count_all '  pl20 payload')" 0
check_once "…while the MODEL got the whole block" 'pl49 payload'
check_frame_intact "after the oversized submit" 80

# ------------------------------------------------- pin 4: a terminal shorter than the frame
# CHARACTERISATION, not an aspiration. The frame's floor is about ten rows (4 staging-tail
# rows + spacer + 2 separators + composer + status), so below ~12 the inline viewport and
# the inserted history overlap: the frame stops being drawn intact and the overlapped
# history rows keep the damage (a terminal's scrollback is immutable). What must hold is
# that it is only cosmetic — the process survives, input keeps working, and growing the
# window back restores a clean frame.
start 60 10 || finish
settle || bad "small-terminal startup never settled"
check_frame_intact "60x10 at idle, before any insert" 60

type_ 'stream 12'
key Enter
# Deliberately NOT `wait_all 'l#11 line'`: on a pane this short the overlap can make the
# last rows illegible, and this pin exists to record that, not to demand it away. Wait for
# the pane to stop moving instead — the spinner animates until the turn is over.
wait_all 'l#0' || wart "60x10: not one streamed row is legible"
settle || bad "60x10: the pane never stopped moving"
if alive; then ok "60x10: the app survives an insert taller than the frame's floor"; else bad "60x10: the app died"; fi
legible="$(uniq_all 'l#[0-9][0-9]')"
if [ "$(cap | grep -c '^───' | tr -d ' ')" -eq 2 ] && [ "$legible" -eq 12 ]; then
    ok "60x10: the frame and all 12 rows survived intact"
else
    wart "60x10: frame overlapped by the inserted history, $legible/12 rows legible (frame floor ≈ 10 rows leaves no insert room) — cosmetic and recorded in docs/TUI-VERIFY.md §6"
fi

tm resize-window -t s -x 60 -y 24
settle || bad "frame never settled after growing the window back"
check_frame_intact "60x24, grown back from 10 rows" 60
type_ 'hello'
key Enter
wait_all 'echo: hello' || bad "input did not survive the small-terminal episode"
settle || bad "frame never settled after the recovery turn"
check_frame_intact "after the recovery turn" 60

finish
