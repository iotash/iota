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

# --- the banner Go printed before the Program claimed the terminal (chat/run.go:86-105),
#     here inserted above the frame by the region.
check_once "banner: chat-started line" 'Chat started. Press Ctrl+C to exit.'
# `/compact` is registered because the fake dialect reports usage (WP53's `tokenAware`);
# the ONE-TABLE law is what this pins — the banner lists exactly what dispatches.
# The row fits 80 columns again (it wrapped while `/mcp` was in the table, 2026-09-18 to 09-20): one
# line, once.
check_once "banner: command list (one-table law)" 'Commands: /file, /session, /model, /compact, /export, /status, /tools, /debug'
if capall | grep -qE '^Session: [a-z0-9]{12}$'; then
    ok "banner: session id row"
else
    bad "banner: session id row missing"
fi

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
