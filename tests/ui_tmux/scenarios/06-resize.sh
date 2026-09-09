#!/usr/bin/env bash
# L4 scenario 6 (TUI_TEST_PLAN §L4) — a REAL SIGWINCH, mid-stream and at idle.
#
# `resize-window` is the only way to produce the genuine article: ratatui's geometry, the
# app's self-tracked viewport top and the emulator's own reflow all go stale at once (spike
# warts W2/W5). What must survive: no crash, exactly ONE composer row (no ghost frame),
# separators that follow the new width, a stream that finishes contiguously, and a composer
# that still accepts input.
#
# Reflow orphans are a recorded cost, not a failure: a rewrapping emulator may strand a row
# above the new viewport. They are reported as WART lines, exactly as the spike did.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

start 80 24 || finish
settle || bad "startup never settled"
check "separators start at the initial width" "$(sep_width)" 80

# ------------------------------------------------------------------ mid-stream grow
type_ 'stream 40'
key Enter
wait_vis 'l#05 line' || bad "the stream never started"

tm resize-window -t s -x 100 -y 28
wait_all 'l#39 line' || bad "the stream did not survive a mid-stream resize"
settle || bad "frame never settled after the mid-stream resize"

if alive; then ok "the app survived a mid-stream SIGWINCH"; else bad "the app died on resize"; fi
check_frame_intact "after the mid-stream grow" 100
check "status line is intact" "$(status_model)" "  fake"
check "history contiguous across the resize" "$(uniq_all 'l#[0-9][0-9]')" 40

dup="$(dupes_all 'l#[0-9][0-9] line')"
if [ -z "$dup" ]; then
    ok "no duplicated history rows after the mid-stream resize"
else
    wart "mid-stream resize left duplicated history row(s): $(echo "$dup" | tr '\n' ' ') — reflow desync, the accepted cost recorded in the spike report (G4)"
fi

# ------------------------------------------------------------------ idle shrink
tm resize-window -t s -x 70 -y 20
settle || bad "frame never settled after the idle shrink"
check_frame_intact "after the idle shrink" 70
check "status line survived the shrink" "$(status_model)" "  fake"
if alive; then ok "the app survived an idle SIGWINCH"; else bad "the app died on the idle shrink"; fi

# ------------------------------------------------------------------ still usable
type_ 'hello after resize'
key Enter
wait_all 'echo: hello after resize' || bad "input stopped working after the resizes"
settle || bad "frame never settled after the follow-up turn"
check_once "the follow-up echo committed once" '❯ hello after resize'
check_frame_intact "after the follow-up turn" 70

finish
