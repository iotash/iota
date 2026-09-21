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

# --- the banner (X-46): three rows, the wordmark on the left and a fact on the right of each —
#     the version, the mode row, the directory — inserted above the frame by the region.
#     Fixed strings throughout (`check_once` is `grep -F`): the wordmark is multi-byte, and in the
#     C locale of the macOS runner a bracket expression over it would match single bytes.
check_once "banner: the wordmark's first row" '▀█▀ █▀▀█ ▀▀█▀▀ █▀▀█'
if capall | grep -F '▀█▀ █▀▀█ ▀▀█▀▀ █▀▀█' | grep -qE '   v[0-9]+\.[0-9]+\.[0-9]+$'; then
    ok "banner: the version beside the wordmark"
else
    bad "banner: the version row is not '<wordmark>   vX.Y.Z'"
fi
# The mode row: a plain agent (no `workspace:`) is a chat, persisted into a fresh bundle.
if capall | grep -qE '   chat · session [a-z0-9]{12}$'; then
    ok "banner: the mode row (chat, session id)"
else
    bad "banner: the mode row is not 'chat · session <id>'"
fi
# The directory row names the pane's cwd — the runner's, which is the crate root (not under the
# pane's redirected HOME, so no `~`). Its first 40 columns: 23 + 40 never wraps at 80, however
# deep the checkout.
check_once "banner: the directory row" "   ${PWD:0:40}"

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

finish
