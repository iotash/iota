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
#     WIDTH SHRINK is where it can happen: tmux reflows rows wider than the new pane (lib.sh's
#     "tmux does not rewrap" predates 2.6's grid_reflow), and the old frame's separators wrap
#     into two rows each — what grew above the cursor would land twice if the resize pass did
#     not claim it (W5's overhang; up to 8 rows before X-52, 3 after its first round, 0 now).
#     Zero at idle; mid-stream a resize can fall between an insert and its draw, where the
#     anchor row is unknown, so it keeps §4's budget of 2.
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
# resize, and that bound is zero. tmux's `scroll-on-clear` (on by default) files a cleared
# screen into the history — which is how this scenario once passed "no row lost" while
# ratatui's own inline resize re-anchored the frame at row 0 behind an `ESC[2J` on every
# narrowing (composer at the top, the rows on screen erased wherever an emulator does not
# archive a clear: Ghostty, herdr — the 2026-09-23 report). The option is OFF here, so a
# clear loses rows exactly as it does there, and every narrowing also asserts that the
# composer stayed in the bottom rows instead of jumping to the top.
#
# The DRAG blocks are what a user's window corner produces: consecutive SIGWINCHes a step
# apart, settled or fast. Every narrowing step rewraps both full-width separators; a frame
# that was flush with the bottom must stay flush (W5's floor invariant), and the old top
# separator's extra piece must not stay behind. Asserted exactly: zero rows lost or
# duplicated, zero separator rows added, zero stale frames, the composer on the pane's
# third-last row.
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
# Blank rows in the HISTORY (off-screen rows only): a resize must never put one there — round 2
# of X-52 scrolled its resize band to the top of the screen, and the next reflow pushed two blank
# rows per drag step into the history.
hist_blanks() { tm capture-pane -pt s -S -400 -E -1 2>/dev/null | grep -c '^ *$'; }
# Separator rows anywhere in the history beyond the live pair (reflow overhang included).
extra_seps() { echo $(($(capall | grep -c '^┄') - 2)); }
# check_pinned <label> <pane height> — the frame is flush with the bottom: the composer two
# rows above the pane's last row (composer, lower separator, status). ratatui's narrowing
# re-anchor put it on row 2; the cursor-only anchor let it climb a row per narrowing step.
check_pinned() {
    check "$1: the composer sits flush at the bottom (row of $2)" "$(composer_row)" $(($2 - 2))
}

start 80 24 || finish
# Unmask a lost row (see the header): a clear must not be filed into the history.
tm set-option -w -t s scroll-on-clear off
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
# A width-only narrowing first: exact, and it shows iota that tmux reflows (W5 learns it —
# tmux eats the rows below the cursor on a row shrink BEFORE it rewraps, so a diagonal
# resize hides that evidence, and an unlearned one may leave the frame's top row behind
# once; `vt100_tests::a_diagonal_narrowing_is_exact_once_the_reflow_is_learned`).
idle_shrink() {
    local label="$1" w="$2" h="$3" dups
    dups="$(dup_rows 1)"
    tm resize-window -t s -x "$w" -y "$h"
    settle || bad "frame never settled after the $label"
    check_frame_intact "after the $label" "$w"
    check "status line survived the $label" "$(status_model)" "  fake"
    if alive; then ok "the app survived an idle SIGWINCH ($label)"; else bad "the app died on the $label"; fi
    check "no streamed row is lost across the $label" "$(absent_rows)" 0
    check "no streamed row is duplicated across the $label (§4.3: zero at idle)" "$(($(dup_rows 1) - dups))" 0
    check "separator rows the $label added to the history" "$(extra_seps)" 0
    check_pinned "after the $label" "$h"
    check "stale frames pushed out by the $label (W9)" "$(($(stale_frames) - stale))" 0
    stale="$(stale_frames)"
}
idle_shrink "idle narrowing" 90 30
idle_shrink "idle shrink" 70 20

# ------------------------------------------------------------------ idle drags (§4.3)
# drag_block <label> <settle each step: 1|0> <step…> — SIGWINCHes one after another, like a
# dragged corner, then the exact invariants: nothing lost or duplicated, not one separator
# row added to the history, no stale frame, the composer flush with the bottom.
drag_block() {
    local label="$1" each="$2" step last seps0 stale0 dups0 blanks0
    shift 2
    seps0="$(extra_seps)"
    blanks0="$(hist_blanks)"
    dups0="$(dup_rows 1)"
    stale0="$(stale_frames)"
    for step in "$@"; do
        tm resize-window -t s -x "${step%x*}" -y "${step#*x}"
        case "$each" in
            1) settle || bad "$label: frame never settled at $step" ;;
            paced) sleep 1 ;;
            *) sleep 0.06 ;;
        esac
        last="$step"
    done
    settle || bad "frame never settled after the $label"
    check_frame_intact "after the $label" "${last%x*}"
    if alive; then ok "the app survived the $label"; else bad "the app died during the $label"; fi
    check "no streamed row is lost across the $label" "$(absent_rows)" 0
    check "no streamed row is duplicated across the $label" "$(($(dup_rows 1) - dups0))" 0
    check "separator rows the $label added to the history" "$(($(extra_seps) - seps0))" 0
    # Under W5's burst layout only a drag's FIRST step rewraps the frame (it was full width);
    # that band is closed when the drag ends and may reach the history later — X-52's accepted
    # residual, at most 2 rows per DRAG, never per step.
    check_budget "blank rows the $label added to the history (X-52: ≤ 2 per drag)" \
        "$(($(hist_blanks) - blanks0))" 2
    check "stale frames the $label pushed out (W9)" "$(($(stale_frames) - stale0))" 0
    check_pinned "after the $label" "${last#*x}"
}
# The verifier's round-2 repro: 25 settled one-column steps (the frame used to climb a row
# per step, reach the top at ~16, and pile separators into the history from there).
drag_block "settled 25-step drag" 1 69x20 68x20 67x20 66x20 65x20 64x20 63x20 62x20 61x20 60x20 \
    59x20 58x20 57x20 56x20 55x20 54x20 53x20 52x20 51x20 50x20 49x20 48x20 47x20 46x20 45x20
tm resize-window -t s -x 70 -y 20
settle || bad "frame never settled after the drag's release"
# A fast corner: eight SIGWINCHes 60 ms apart, narrowing and shortening at once.
drag_block "fast 8-step drag" 0 68x20 66x20 64x19 63x19 62x19 61x18 60x18 59x18
tm resize-window -t s -x 70 -y 20
settle || bad "frame never settled after the fast drag's release"
# A PACED drag, one step a second (a hand that pauses, a keyboard resize): the verifier's round-2
# repro — steps further apart than the old 750 ms window were separate drags, each rewrapping the
# restored full-width separators into two more blank rows.
drag_block "paced 10-step drag (1 s apart)" paced 69x20 68x20 67x20 66x20 65x20 64x20 63x20 62x20 61x20 60x20
tm resize-window -t s -x 70 -y 20
settle || bad "frame never settled after the paced drag's release"
# A HEIGHT drag down and back up within one drag (100 ms apart), both `scroll-on-clear` settings:
# the verifier's P0 — tmux pulls history rows back on a grow, the loop handled a stale size and
# erased the committed row above the frame. Loss is the one hard failure (X-52's budget).
for soc in off on; do
    tm set-option -w -t s scroll-on-clear "$soc"
    for h in 19 18 17 16 15 14 15 16 17 18 19 20; do tm resize-window -t s -x 70 -y "$h"; sleep 0.1; done
    settle || bad "frame never settled after the height drag (scroll-on-clear $soc)"
    check_frame_intact "after a height drag down and back (scroll-on-clear $soc)" 70
    check "no streamed row is lost across a height drag down and back (scroll-on-clear $soc)" "$(absent_rows)" 0
    check "no streamed row is duplicated across a height drag down and back (scroll-on-clear $soc)" \
        "$(dup_rows 1)" 0
done
tm set-option -w -t s scroll-on-clear off
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
check_pinned "after the mid-stream shrink" 18
check "status line is intact after the shrink" "$(status_model)" "  fake"
# Two streams of the same 40 lines are in the history now: none may be missing a copy.
check "no streamed row is lost across the mid-stream shrink (every line at least twice)" "$(lost_rows 2)" 0
# §4's budget (measured: 0 — the resize pass claims the reflow's overhang).
check_budget "duplicated rows after the mid-stream shrink (§4 budget)" "$(dup_rows 2)" 2
check_budget "stale frames pushed out by the mid-stream shrink (W9)" "$(($(stale_frames) - stale))" 1

# ------------------------------------------------------------------ still usable
type_ 'hello after resize'
key Enter
wait_all 'echo: hello after resize' || bad "input stopped working after the resizes"
settle || bad "frame never settled after the follow-up turn"
check_once "the follow-up echo committed once" '❯ hello after resize'
check_frame_intact "after the follow-up turn" 60

finish
