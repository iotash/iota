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
#     not claim it (W5's overhang; up to 8 rows before X-52, 3 after its first round, 0 while
#     the pass claimed it). Since B3 (2026-09-26) nothing grown above the cursor is claimed:
#     the frame's top staged row stays behind once per narrowing that rewraps it — 1 for each
#     idle narrowing and for a drag that starts from a full-width repaint, 2 mid-stream
#     (X-52 residual (1)). Counted in COPIES, each block against the copies before it.
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
# separator's extra piece must not stay behind. Asserted exactly: zero rows lost, zero
# separator rows added, zero stale frames, the composer on the pane's third-last row; the
# duplicated rows within each drag's measured number (`drag_block`).
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

# Stale frames in the scrollback: status rows beyond the live one.
stale_frames() { echo $(($(count_all '  fake · ') - 1)); }
# Duplicated streamed rows: the COPIES beyond <copies> (= streams so far), summed over the lines.
# A line already duplicated counts again for every further copy — counting the distinct lines
# with a duplicate (the old definition) let three blocks add a copy each of one line unseen
# (2026-09-27: `l#36` ×5 by the height drag, every block's "0 duplicated" green).
dup_rows() { capall | grep -oE 'l#[0-9][0-9] line' | sort | uniq -c | awk -v n="$1" '$1 > n { d += $1 - n } END { print d + 0 }'; }
# The duplicated lines with their counts (`l#36×3`) — printed under a failed duplicate check.
dup_list() { capall | grep -oE 'l#[0-9][0-9] line' | sort | uniq -c | awk -v n="$1" '$1 > n { printf "%s×%s ", $2, $1 }'; }
# Lost streamed rows (lines present fewer than <copies> times).
lost_rows() { capall | grep -oE 'l#[0-9][0-9] line' | sort | uniq -c | awk -v n="$1" '$1 < n' | wc -l | tr -d ' '; }
# Lines missing from the history altogether.
absent_rows() { echo $((40 - $(uniq_all 'l#[0-9][0-9] line'))); }
# The streamed lines present fewer than <copies> times, each with its count (`l#07×0`).
missing_rows() {
    local n c
    for n in $(seq -w 0 39); do
        c="$(count_all "l#$n line")"
        [ "$c" -lt "$1" ] && printf 'l#%s×%s ' "$n" "$c"
    done
}
# check_no_loss <label> <copies> — every streamed line is in the history at least <copies>
# times; a loss names the lines, so a red run says WHICH rows went (top of the stream, the rows
# on screen, the rows by the frame) instead of only how many.
check_no_loss() {
    local lost
    if [ "$2" -eq 1 ]; then lost="$(absent_rows)"; else lost="$(lost_rows "$2")"; fi
    check "$1" "$lost" 0
    [ "$lost" = 0 ] || echo "    MISSING: $(missing_rows "$2")"
}

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
check_no_loss "no streamed row is lost across the mid-stream grow" 1
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
    check_no_loss "no streamed row is lost across the $label" 1
    # B3 (2026-09-26): what the narrowing grew above the cursor is not claimed — measured: 1 row.
    check_budget "streamed rows duplicated by the $label (B3)" "$(($(dup_rows 1) - dups))" 1
    check "separator rows the $label added to the history" "$(extra_seps)" 0
    check_pinned "after the $label" "$h"
    check "stale frames pushed out by the $label (W9)" "$(($(stale_frames) - stale))" 0
    stale="$(stale_frames)"
}
idle_shrink "idle narrowing" 90 30
idle_shrink "idle shrink" 70 20

# ------------------------------------------------------------------ idle drags (§4.3)
# drag_block <label> <settle each step: 1|0|paced> <duplicated rows> <step…> — SIGWINCHes one
# after another, like a dragged corner, then the invariants: nothing lost, not one separator row
# added to the history, no stale frame, the composer flush with the bottom — and the rows left
# twice within the block's MEASURED number (X-52 residual (1), B3): the drag's first step
# rewraps the staged rows whose lines the last full-width repaint drew to the old width, and
# since B3 nothing a reflow grows above the cursor is claimed, so the frame's top staged row
# (`l#36`) stays behind once. A drag that starts inside another's burst (the frame still a
# margin short, its lines no wider than the new terminal) rewraps nothing: 0.
drag_block() {
    local label="$1" each="$2" dups="$3" step last seps0 stale0 dups0 blanks0
    shift 3
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
    check_no_loss "no streamed row is lost across the $label" 1
    check_budget "streamed rows the $label duplicated (X-52 (1), B3: measured $dups)" \
        "$(($(dup_rows 1) - dups0))" "$dups"
    [ "$(($(dup_rows 1) - dups0))" -le "$dups" ] || echo "    DUPLICATED: $(dup_list 1)"
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
# Measured 1 (15 of 15 runs: tmux 3.7c, 3.7c under six CPU burners, 3.4): it starts from the
# idle shrink's full-width repaint.
drag_block "settled 25-step drag" 1 1 69x20 68x20 67x20 66x20 65x20 64x20 63x20 62x20 61x20 60x20 \
    59x20 58x20 57x20 56x20 55x20 54x20 53x20 52x20 51x20 50x20 49x20 48x20 47x20 46x20 45x20
tm resize-window -t s -x 70 -y 20
settle || bad "frame never settled after the drag's release"
# A fast corner: eight SIGWINCHes 60 ms apart, narrowing and shortening at once.
# Measured 1 (15 of 15, as above): 2 while it lasts — its passes lag the steps — and the band
# the drag's end closes takes one of them.
drag_block "fast 8-step drag" 0 1 68x20 66x20 64x19 63x19 62x19 61x18 60x18 59x18
tm resize-window -t s -x 70 -y 20
settle || bad "frame never settled after the fast drag's release"
# A PACED drag, one step a second (a hand that pauses, a keyboard resize): the verifier's round-2
# repro — steps further apart than the old 750 ms window were separate drags, each rewrapping the
# restored full-width separators into two more blank rows.
# Measured 0 (15 of 15): it starts 0.4 s after the release, inside the release's burst.
drag_block "paced 10-step drag (1 s apart)" paced 0 69x20 68x20 67x20 66x20 65x20 64x20 63x20 62x20 61x20 60x20
tm resize-window -t s -x 70 -y 20
settle || bad "frame never settled after the paced drag's release"
# A HEIGHT drag down and back up within one drag (100 ms apart), both `scroll-on-clear` settings:
# the verifier's P0 — tmux pulls history rows back on a grow, the loop handled a stale size and
# erased the committed row above the frame. Loss is the one hard failure (X-52's budget).
#
# RECORDED, always: this block failed once on the macos-14 runner (CI 36305402235 — the frame
# three rows up, its top separator gone, the drag's end repaint writing the two restored
# columns on rows below it, two streamed lines lost) and not once in 60 local runs under load
# on tmux 3.7c and 3.4. The next red run must carry the bytes: the pane's output (raw-<soc>.out),
# the history before and after (hist-<soc>-{before,after}.txt), and a timeline of the steps with
# the byte offset and the cursor at each (steps-<soc>.log — the offset is what the pipe had
# written by then, a lower bound). A failing scenario keeps its scratch dir, and the CI job
# uploads it (ci.yml).
for soc in off on; do
    tm set-option -w -t s scroll-on-clear "$soc"
    dups="$(dup_rows 1)"
    f0="$FAIL"
    RAW="$SCEN_TMP/raw-$soc.out"
    pipe_raw
    capall >"$SCEN_TMP/hist-$soc-before.txt"
    steps="$SCEN_TMP/steps-$soc.log"
    : >"$steps"
    for h in 19 18 17 16 15 14 15 16 17 18 19 20; do
        tm resize-window -t s -x 70 -y "$h"
        echo "h=$h bytes=$(wc -c <"$RAW" | tr -d ' ') cursor=$(cursor_xy) hist=$(hist_size)" >>"$steps"
        sleep 0.1
    done
    settle || bad "frame never settled after the height drag (scroll-on-clear $soc)"
    echo "settled bytes=$(wc -c <"$RAW" | tr -d ' ') cursor=$(cursor_xy) hist=$(hist_size)" >>"$steps"
    check_frame_intact "after a height drag down and back (scroll-on-clear $soc)" 70
    check_no_loss "no streamed row is lost across a height drag down and back (scroll-on-clear $soc)" 1
    check "no streamed row is duplicated across a height drag down and back (scroll-on-clear $soc)" \
        "$(($(dup_rows 1) - dups))" 0
    capall >"$SCEN_TMP/hist-$soc-after.txt"
    tm pipe-pane -t s
    if [ "$FAIL" -gt "$f0" ]; then
        echo "    RECORDED (scroll-on-clear $soc): raw-$soc.out, steps-$soc.log, hist-$soc-{before,after}.txt in $SCEN_TMP"
        sed 's/^/    STEP: /' "$steps"
    fi
done
tm set-option -w -t s scroll-on-clear off
stale="$(stale_frames)"

# ------------------------------------------------------------------ mid-stream shrink (§4.2)
before="$(count_all 'l#39 line')"
# The copies the earlier blocks left count once more after the second stream (a line at k
# copies is at k+1): the shrink is measured against them, not charged for them.
dups="$(dup_rows 1)"
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
check_no_loss "no streamed row is lost across the mid-stream shrink (every line at least twice)" 2
# §4's budget since B3 (2026-09-26): the reflow's overhang above the cursor is not claimed —
# measured 2 in 15 of 15 runs (tmux 3.7c, under load, 3.4), counted as copies against the
# copies before the stream. (The "3" measured when B3 landed counted distinct lines, and one
# of them was the `l#36` the earlier blocks had already duplicated.)
check_budget "duplicated rows after the mid-stream shrink (§4 budget, B3)" "$(($(dup_rows 2) - dups))" 2
check_budget "stale frames pushed out by the mid-stream shrink (W9)" "$(($(stale_frames) - stale))" 1

# ------------------------------------------------------------------ still usable
type_ 'hello after resize'
key Enter
wait_all 'echo: hello after resize' || bad "input stopped working after the resizes"
settle || bad "frame never settled after the follow-up turn"
check_once "the follow-up echo committed once" '❯ hello after resize'
check_frame_intact "after the follow-up turn" 60

finish
