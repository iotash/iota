#!/usr/bin/env bash
# L4 scenario 1 (TUI_TEST_PLAN §L4) — startup: banner → frame; the composer is the one
# input row between two full-width separators; the status line is the bottom zone; the
# REAL cursor sits at the draft's logical column and stays put while the app idles.
#
# Only the last claim needs a terminal (L2's TestBackend goldens own the geometry), which
# is why this scenario is thin: it exists for `#{cursor_x} #{cursor_y}`.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

start 80 24 || finish
settle || bad "frame never settled"

# --- the banner (X-46): a card — a rounded frame around three rows, the mark and the version,
#     the mode row, the directory — inserted above the frame by the region. Fixed strings
#     throughout (`check_once` is `grep -F`): the mark, the dots and the frame are multi-byte,
#     and in the C locale of the macOS runner a bracket expression over them would match
#     single bytes.
check_once "banner: the mark" 'ι> iota'
if capall | grep -F 'ι> iota' | grep -qE '^│ ι> iota  v[0-9]+\.[0-9]+\.[0-9]+ +│$'; then
    ok "banner: the version beside the mark, inside the frame"
else
    bad "banner: the mark row is not '│ ι> iota  vX.Y.Z … │'"
fi
# The mode row: a plain agent (no `workspace:`) is a chat, persisted into a fresh bundle.
if capall | grep -qE '^│ chat · session [a-z0-9]{12} +│$'; then
    ok "banner: the mode row (chat, session id)"
else
    bad "banner: the mode row is not '│ chat · session <id> … │'"
fi
# The directory row names the pane's cwd — the runner's, which is the crate root (not under the
# pane's redirected HOME, so no `~`). Its first 28 columns: a path longer than the 76 inside the
# frame is cut in the middle, and the head keeps 37, however deep the checkout.
check_once "banner: the directory row" "│ ${PWD:0:28}"
# …and its tail is on the SAME row, closed by the frame's edge: the cut, not a wrap. (Fixed
# strings again — `.{80,}` would count bytes in the C locale.)
if capall | grep -F "│ ${PWD:0:28}" | grep -F -- "${PWD: -12}" | grep -qF ' │'; then
    ok "banner: the directory row's tail is on the same row, inside the frame (no wrap)"
else
    bad "banner: the directory row wrapped or lost its tail"
fi
# The frame: one top edge, one bottom edge, and the rows in order between them — the mark
# row first, the directory row last.
check_once "banner: the frame's top edge" '╭─'
check_once "banner: the frame's bottom edge" '╰─'
top="$(row_of '╭─')"
check "banner: the mark row is the card's first" "$(row_of 'ι> iota')" "$((top + 1))"
check "banner: the mode row is the card's second" "$(row_of '│ chat · session ')" "$((top + 2))"
check "banner: the directory row is the card's third" "$(row_of "│ ${PWD:0:28}")" "$((top + 3))"
check "banner: the bottom edge closes the card" "$(row_of '╰─')" "$((top + 4))"

# --- the frame
check_frame_intact "startup" 80

comp="$(composer_row)"
bot="$(frame_bot)"
check "status row is the bottom zone" "$(status_model)" "  fake"
# The WP53 token half, in a real terminal: a fresh chat against a usage-reporting dialect
# shows the estimated context occupancy beside the model.
check "status row carries the context segment" "$(bottom_zone)" "  fake · ≈0% / 128k"

# Nothing renders below the status row: the bottom zone is the last frame row.
tail_rows="$(cap | tail -n "+$((bot + 2))" | grep -c '[^[:space:]]' | tr -d ' ')"
check "nothing below the status row" "$tail_rows" 0

# --- the real cursor: column 2 is the 2-column "❯ " prompt gutter, row is the composer's
#     (tmux rows are 0-based, capture-pane rows 1-based).
xy="$(cursor_xy)"
check "cursor at the empty draft's logical position" "$xy" "2 $((comp - 1))"

# …and it does not drift while the loop idles (W10: an idle loop still polls, but an idle
# poll must not move the cursor).
drift=0
i=0
while [ "$i" -lt 5 ]; do
    sleep 0.15
    [ "$(cursor_xy)" = "$xy" ] || drift=$((drift + 1))
    i=$((i + 1))
done
check "cursor pinned across 5 idle polls" "$drift" 0

# A keystroke moves it by exactly the glyph's display width — the IME anchor law, proved
# here on the real terminal and nowhere else.
type_ '中文'
wait_vis '❯ 中文' || bad "CJK draft never rendered"
check "cursor after two double-width runes" "$(cursor_xy)" "6 $((comp - 1))"
key BSpace
wait_gone '❯ 中文' || bad "backspace over a wide rune failed"
check "cursor after backspace over one wide rune" "$(cursor_xy)" "4 $((comp - 1))"

# --- the exit, with the banner's tail still in the staging window (X-49): Ctrl+D at the prompt
#     leaves. The window's rows — the card's last three and the blank under it, the window being
#     four rows — go to scrollback and the frame is repainted without them, so each is in the
#     history ONCE, not once in the scrollback and once more in the frame that had shown it. The
#     pane is kept past the exit (`remain-on-exit`) so the history can be read.
tm set-option -t s remain-on-exit on
key C-d
_poll_until 60 pane_dead || bad "Ctrl+D at the prompt did not exit"
check_once "the mode row is in the history once after the exit" '│ chat · session '
check_once "the directory row is in the history once after the exit" "│ ${PWD:0:28}"
check_once "the frame's bottom edge is in the history once after the exit" '╰─'
check_once "the mark row, already in the scrollback, is there once too" 'ι> iota'
check_once "the composer row stays on the screen after the exit" '❯'

finish
