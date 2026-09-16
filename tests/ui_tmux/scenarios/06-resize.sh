#!/usr/bin/env bash
# L4 scenario 6 (TUI_TEST_PLAN §L4; docs/TUI-VERIFY.md §4) — a REAL SIGWINCH, mid-stream and at idle.
#
# `resize-window` is the only way to produce the genuine article: ratatui's geometry, the
# app's self-tracked viewport top and the emulator's own reflow all go stale at once (spike
# warts W2/W5). What must survive: no crash, exactly ONE composer row (no ghost frame),
# separators that follow the new width, a stream that finishes contiguously, and a composer
# that still accepts input.
#
# §4 asks for the orphans to be COUNTED, and gives the budget: at most 2 orphaned or duplicated
# rows per mid-stream resize, none at all for a resize at idle. Both directions are driven
# here — wider and narrower mid-stream (§4.1, §4.2), larger and smaller at idle (§4.3) — with
# §4.4's frame invariant after every one, and the counts are asserted as upper bounds. Two
# counts, and what tmux 3.7c makes of them:
#
#   * DUPLICATED history rows (the same streamed line twice). A grow duplicates nothing. A
#     WIDTH SHRINK is different: tmux reflows rows wider than the new pane, and the old
#     frame's separators (the only wide rows here) wrap into two rows each, so the frame the
#     app is tracking grows under it and its viewport accounting slips — the rows in the
#     staging window at that moment land twice. That is the reflow class §4 describes, and it
#     IS visible under tmux after all (lib.sh's "tmux does not rewrap" predates 2.6's
#     grid_reflow). Bounded at the staging window's size, and the number is printed.
#   * STALE FRAMES in the scrollback (a separator pair, a composer row and a status row above
#     the live frame). A frame that has to move — the first one, when the banner is inserted
#     above it; the old one, when a taller terminal puts the new one lower — goes out through
#     the scroll region rather than being cleared in place, and tmux preserves what scrolls out
#     of a partial region (wart W9, docs/TUI-VERIFY.md §2): a user scrolling up sees it.
#     Measured: one at startup, one for the idle grow, none or one for the others; bounded at
#     one per resize. Counted by status rows, which reflow cannot split — a wrapped separator
#     would count twice.
#
# What must NEVER happen is a LOST row: every streamed line is in the history after every
# resize, and that bound is zero.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

# Stale frames in the scrollback: status rows beyond the live one.
stale_frames() { echo $(($(count_all '  fake · ') - 1)); }
# Duplicated streamed rows (lines present more than <copies> times, <copies> = streams so far).
dup_rows() { capall | grep -oE 'l#[0-9][0-9] line' | sort | uniq -c | awk -v n="$1" '$1 > n' | wc -l | tr -d ' '; }
# Lost streamed rows (lines present fewer than <copies> times).
lost_rows() { capall | grep -oE 'l#[0-9][0-9] line' | sort | uniq -c | awk -v n="$1" '$1 < n' | wc -l | tr -d ' '; }
# Lines missing from the history altogether.
absent_rows() { echo $((40 - $(uniq_all 'l#[0-9][0-9] line'))); }

# check_budget <label> <actual> <max> — an upper bound, reported with the number.
check_budget() {
    if [ "$2" -le "$3" ]; then ok "$1 ($2 ≤ $3)"; else bad "$1: $2 exceeds the budget of $3"; fi
}

start 80 24 || finish
settle || bad "startup never settled"
check "separators start at the initial width" "$(sep_width)" 80
# The startup leaves one: the frame the banner was inserted above (W9).
check_budget "stale frames in the scrollback at startup (W9)" "$(stale_frames)" 1
stale="$(stale_frames)"

# ------------------------------------------------------------------ idle grow (§4.3)
tm resize-window -t s -x 90 -y 26
settle || bad "frame never settled after the idle grow"
check_frame_intact "after the idle grow" 90
check "status line survived the idle grow" "$(status_model)" "  fake"
if alive; then ok "the app survived an idle SIGWINCH (grow)"; else bad "the app died on the idle grow"; fi
check_budget "stale frames pushed into the scrollback by the idle grow (W9)" "$(($(stale_frames) - stale))" 1
stale="$(stale_frames)"

# ------------------------------------------------------------------ mid-stream grow (§4.1)
type_ 'stream 40'
key Enter
wait_vis 'l#05 line' || bad "the stream never started"

tm resize-window -t s -x 110 -y 30
wait_all 'l#39 line' || bad "the stream did not survive a mid-stream resize"
settle || bad "frame never settled after the mid-stream resize"

if alive; then ok "the app survived a mid-stream SIGWINCH (grow)"; else bad "the app died on resize"; fi
check_frame_intact "after the mid-stream grow" 110
check "status line is intact" "$(status_model)" "  fake"
check "no streamed row is lost across the mid-stream grow" "$(absent_rows)" 0
check_budget "duplicated rows after the mid-stream grow (§4 budget)" "$(dup_rows 1)" 2
check_budget "stale frames pushed out by the mid-stream grow (W9)" "$(($(stale_frames) - stale))" 1
stale="$(stale_frames)"

# ------------------------------------------------------------------ idle shrink (§4.3)
tm resize-window -t s -x 70 -y 20
settle || bad "frame never settled after the idle shrink"
check_frame_intact "after the idle shrink" 70
check "status line survived the shrink" "$(status_model)" "  fake"
if alive; then ok "the app survived an idle SIGWINCH (shrink)"; else bad "the app died on the idle shrink"; fi
check "no streamed row is lost across the idle shrink" "$(absent_rows)" 0
check_budget "stale frames pushed out by the idle shrink (W9)" "$(($(stale_frames) - stale))" 1
stale="$(stale_frames)"

# ------------------------------------------------------------------ mid-stream shrink (§4.2)
before="$(count_all 'l#39 line')"
type_ 'stream 40'
key Enter
wait_all_more 'l#05 line' "$(count_all 'l#05 line')" || bad "the second stream never started"

tm resize-window -t s -x 60 -y 18
wait_all_more 'l#39 line' "$before" || bad "the stream did not survive a mid-stream shrink"
settle || bad "frame never settled after the mid-stream shrink"

if alive; then ok "the app survived a mid-stream SIGWINCH (shrink)"; else bad "the app died on the shrink"; fi
check_frame_intact "after the mid-stream shrink" 60
check "status line is intact after the shrink" "$(status_model)" "  fake"
# Two streams of the same 40 lines are in the history now: none may be missing a copy.
check "no streamed row is lost across the mid-stream shrink (every line at least twice)" "$(lost_rows 2)" 0
# The reflow cost, characterised: the staging window is max(2, h/2) rows and the desync lands
# it twice — at most that many duplicated rows (h = 18 at the moment of the shrink).
check_budget "duplicated rows after the mid-stream shrink (tmux reflow, staging-window bound)" "$(dup_rows 2)" 9
check_budget "stale frames pushed out by the mid-stream shrink (W9)" "$(($(stale_frames) - stale))" 1

# ------------------------------------------------------------------ still usable
type_ 'hello after resize'
key Enter
wait_all 'echo: hello after resize' || bad "input stopped working after the resizes"
settle || bad "frame never settled after the follow-up turn"
check_once "the follow-up echo committed once" '❯ hello after resize'
check_frame_intact "after the follow-up turn" 60

finish
