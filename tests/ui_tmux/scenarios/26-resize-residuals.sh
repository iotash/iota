#!/usr/bin/env bash
# L4 scenario 26 (docs/TUI-VERIFY.md §4.3, DIVERGENCES X-52) — the resize cases scenario 06's one
# long session cannot isolate. Each block starts a FRESH pane, so every non-blank row the pane ever
# shows is unique: after the resize, every streamed line must still be there (the hard rule), and
# the rows left twice — a duplicated row, a separator row beyond the pair — stay within the
# block's MEASURED budget. Since B3 (2026-09-26, X-52 residual (1)) nothing a reflow grew above
# the cursor is claimed, so the frame's first rows that far up stay behind as duplicates; each
# cap below is the number measured on tmux 3.7c when B3 landed, and a larger one is a regression.
# `scroll-on-clear` is off, so a clear loses rows here exactly as it does in Ghostty and herdr.
#
#   A. a drastic narrowing — the old width 2× and 3× the new (a maximized window restored, a
#      full-width pane halved) — with the startup banner still staged in the frame;
#   B. an idle narrowing right after FAST output (20 ms a line) — 2 duplicated rows both ways: the
#      stream's last two staged rows (`l#56`, `l#57`), the frame's top rows when it narrows;
#   C. a narrowing with a surface open (`/model`), then closed;
#   D. a narrowing while a foreground tool call runs — 1 duplicated row: the user's own
#      `❯ runfg:…` row, the frame's top staged row then (a narrowing that landed BEFORE the Enter
#      left E's two banner rows instead: the block's wait, fixed 2026-09-28 — `wait_sent`);
#   E. tmux's DEFAULT `scroll-on-clear on`: a narrowing right after startup can re-anchor the frame
#      on row 0 — an erase-below from the home position would make tmux file the old frame and
#      the banner into the history;
#   F. X-52's accepted residual: the session's first narrowing with a surface's input cursor on
#      the frame's last row and nothing below it to rewrap — nothing shows iota that the emulator
#      reflows, so the old top separator's piece may stay behind once.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

# check_budget <label> <actual> <max> — an upper bound, reported with the number.
check_budget() {
    if [ "$2" -le "$3" ]; then ok "$1 ($2 ≤ $3)"; else bad "$1: $2 exceeds the budget of $3"; fi
}

CONFIG_BODY="providers:
  mock: {type: openai, key: test, url: \"http://127.0.0.1:$IOTA_PORT\"}
models:
  m: mock:fake
agents:
  default:
    model: m
    choices: [m, \"mock:*\"]
    tools: {shell: {sandbox: off, auto_run: true}}"
export CONFIG_BODY

# Non-blank rows that appear more than once in the whole history.
# (C locale: in a UTF-8 one, sort and uniq collate `❯` and a `┄┄┄` row as equal.)
# The live separator PAIR is two equal rows by design; separator rows are counted apart.
dup_lines() { capall | sed 's/ *$//' | grep -v '^$' | grep -v '^┄' | LC_ALL=C sort | LC_ALL=C uniq -d | wc -l | tr -d ' '; }
seps_all() { capall | grep -c '^┄'; }
fresh() {
    start "$1" "$2" || finish
    tm set-option -w -t s scroll-on-clear off
    settle || bad "startup never settled"
}
# after_resize <label> <width> <duplicated rows> <separator rows> [<which rows>] — the invariants
# every block ends with; the last two are the block's measured budget (B3's residual), and
# <which rows> names what the duplicated ones are.
after_resize() {
    settle || bad "$1: frame never settled"
    check_frame_intact "$1" "$2"
    if alive; then ok "$1: the app survived"; else bad "$1: the app died"; fi
    check_budget "$1: rows that appear twice in the history (B3${5:+: $5})" "$(dup_lines)" "$3"
    capall | sed 's/ *$//' | grep -v '^$' | grep -v '^┄' | LC_ALL=C sort | LC_ALL=C uniq -d | sed 's/^/    DUP: /'
    check_budget "$1: separator rows in the whole history (B3)" "$(seps_all)" "$4"
}

# ------------------------------------------------------------------ A: 2× and 3× narrowings
for to in 100 67; do
    fresh 201 30
    tm resize-window -t s -x "$to" -y 30

    after_resize "201→$to at startup" "$to" 3 2
done

# ------------------------------------------------------------------ B: fast output, then idle
# 70x24 is a width-only narrowing, 60x18 a diagonal one (tmux eats the rows below the cursor
# before it rewraps): since B3 neither claims what grew above the cursor, and the frame's first
# rows that far up stay behind — the stream's last two staged rows, `l#56` and `l#57`. Measured
# 2026-09-28, 100 runs (tmux 3.7c and 3.4; alone, under six CPU burners, three suites at once):
# 2 in 99 for each (1 once for 60x18, 0 once for 70x24). One 3 for 60x18 (2026-09-27, a whole
# suite beside loaded scenario-06 loops) printed no rows and has not come back; a 3 now shows
# which row it is.
for to in 70x24 60x18; do
    fresh 80 24
    type_ 'stream 60 20'
    key Enter
    wait_all 'l#59 line' || bad "the fast stream never finished"
    settle || bad "the fast stream never settled"
    check "fast output, before any resize: no row appears twice" "$(dup_lines)" 0
    tm resize-window -t s -x "${to%x*}" -y "${to#*x}"
    if [ "$to" = 70x24 ]; then
        after_resize "fast output, then 80x24→$to" "${to%x*}" 2 2 "the stream's last two staged rows"
    else
        settle || bad "fast output → $to: frame never settled"
        check_frame_intact "fast output, then 80x24→$to" "${to%x*}"
        check_budget "fast output → $to (a diagonal narrowing, B3: the stream's last two staged rows)" \
            "$(dup_lines)" 2
        # Which rows, as after_resize prints them.
        capall | sed 's/ *$//' | grep -v '^$' | grep -v '^┄' | LC_ALL=C sort | LC_ALL=C uniq -d | sed 's/^/    DUP: /'
        check "fast output → $to: separator rows in the whole history" "$(seps_all)" 2
    fi
    check "fast output → $to: every streamed line is there" "$(uniq_all 'l#[0-9][0-9] line')" 60
done

# ------------------------------------------------------------------ C: a surface open
fresh 80 24
type_ 'stream 30'
key Enter
wait_all 'l#29 line' || bad "the stream never finished"
settle || bad "the stream never settled"
type_ '/model'
key Enter
wait_vis '↑↓ move' || bad "the /model panel never opened"
settle || bad "the panel never settled"
tm resize-window -t s -x 68 -y 24
settle || bad "the panel never settled after the narrowing"
key Escape
wait_gone '↑↓ move' || bad "the /model panel never closed"
after_resize "/model open, 80→68" 68 1 4
check_budget "/model open, 80→68: empty composer rows in the history (B3)" "$(capall | grep -c '^❯ *$')" 2

# ------------------------------------------------------------------ D: a tool call running
fresh 80 24
type_ 'runfg:sleep 1.5; seq -f "t#%02g out" 0 30'
key Enter
# The narrowing must land while the tool call runs: after the Enter (see `wait_sent`), inside
# its 1.5 s sleep.
wait_sent 'runfg:sleep 1.5' || bad "the tool call never started"
tm resize-window -t s -x 70 -y 24
wait_all 'ran: ' || bad "the tool round never closed"
# Measured 2026-09-28 with the wait above, 30 runs (three suites at once, six CPU burners, tmux
# 3.7c and 3.4): 1 in 29, 0 once. Before it, 5 of 94 runs narrowed before the Enter landed and
# measured E's banner rows (2) — a startup narrowing, not this block's case.
after_resize "a tool call running, 80→70" 70 1 2 "the user's own row, the frame's top staged row"
check_budget "a tool call running, 80→70: the user's own row (B3: its piece above the cursor)" "$(count_all '❯ runfg:')" 2

# ------------------------------------------------------------------ E: scroll-on-clear on
fresh 60 16
tm set-option -w -t s scroll-on-clear on
type_ '帮我看看这个 resize 的问题'
settle || bad "the CJK draft never settled"
tm resize-window -t s -x 44 -y 16
# B3 at startup: the banner is still staged in the frame, and what the narrowing grew above the
# cursor leaves the frame's first rows behind — the banner's last two rows when its box is
# narrow (a short checkout path), one identical right-border piece when the path fills 60
# columns. Measured 2026-09-26 over scroll-on-clear on/off, with and without the draft: 2 at
# most, never a lost row.
after_resize "scroll-on-clear on, a CJK draft, 60x16→44x16 at startup (B3: the banner's last rows)" 44 2 2
check "scroll-on-clear on: the draft is in the box once" "$(count_all '帮我看看这个 resize 的问题')" 1

# ------------------------------------------------------------------ F: the unlearned surface case
fresh 100 24
type_ 'stream 30'
key Enter
wait_all 'l#29 line' || bad "the stream never finished"
settle || bad "the stream never settled"
type_ '/model'
key Enter
wait_vis '↑↓ move' || bad "the /model panel never opened"
settle || bad "the panel never settled"
tm resize-window -t s -x 84 -y 24
settle || bad "the panel never settled after the narrowing"
key Escape
wait_gone '↑↓ move' || bad "the /model panel never closed"
settle || bad "frame never settled"
check_budget "surface cursor on the last row, first narrowing: rows that appear twice (B3)" "$(dup_lines)" 1
check_budget "surface cursor on the last row, first narrowing: separator rows (B3)" \
    "$(seps_all)" 4

# ------------------------------------------------------------------ G: a stream right at the drag's end
# The verifier's round-2 finding: a stream that starts exactly as a drag ends left the drag's band
# as blank rows in the history. The turn starting ends the drag and closes its band first.
# Asserted on the OUTCOME: the second turn has finished (its `l#19` is the SECOND one in the
# history — the first stream printed one too, and waiting for "a" `l#19` waited for nothing; that
# is why this block measured a turn still in flight on the slow CI runners, 2026-09), and the
# drag has ended (the frame back at the full width).
fresh 100 24
type_ 'stream 30'
key Enter
wait_all 'l#29 line' || bad "the stream never finished"
settle || bad "the stream never settled"
for w in 99 98 97 96 95; do tm resize-window -t s -x "$w" -y 24; sleep 0.03; done
before="$(count_all 'l#19 line')"
type_ 'stream 20'
key Enter
wait_all_more 'l#19 line' "$before" || bad "the second stream never finished"
settle || bad "the second stream never settled"
check_frame_intact "a stream right at the drag's end" 95
# One blank row between turns at rest. When the drag's last resize pass lands AFTER the turn's first
# key (a slow runner: the pass waits RESIZE_QUIET for the burst to end), its band keeps a blank row
# between the turns — X-52 residual (1), a drag's blank rows. Measured under load (ten CPU burners,
# 2026-09-27): 2 in 7 of 130 runs on tmux 3.7c and 2 of 80 on tmux 3.4, 1 otherwise; never more.
gap="$(capall | awk 'index($0,"l#29 line"){f=NR} index($0,"❯ stream 20")&&f{print NR-f-1; exit}')"
if [ -z "$gap" ]; then
    check_measured "a stream right at the drag's end: the next turn follows the last one" "" 1
else
    check_budget "a stream right at the drag's end: blank rows before the next turn (1 at rest, +1 band row, X-52 (1))" "$gap" 2
fi
check_measured "a stream right at the drag's end: no hole inside the new turn" \
    "$(capall | awk 'index($0,"❯ stream 20"){f=1} f&&index($0,"l#00 line"){g=1} g&&/^ *$/{b++} g&&index($0,"l#19 line"){print b+0; exit}')" 0

# ------------------------------------------------------------------ H: a drag with /model open
# OVER BUDGET, known (X-52, the frozen protocol): the surface's input cursor sits on the frame's
# last row, the drag's first step cannot show the reflow, and the split separator pieces reach
# the history — the verifier measured 6 separator rows, the frozen build 4. Capped at 6 by name.
fresh 100 40
type_ 'stream 30'
key Enter
wait_all 'l#29 line' || bad "the stream never finished"
settle || bad "the stream never settled"
type_ '/model'
key Enter
wait_vis '↑↓ move' || bad "the /model panel never opened"
settle || bad "the panel never settled"
for w in 99 98 97 96 95 94 93 92 91 90; do tm resize-window -t s -x "$w" -y 40; sleep 0.05; done
settle || bad "the panel never settled after the drag"
key Escape
wait_gone '↑↓ move' || bad "the /model panel never closed"
settle || bad "frame never settled"
check_budget "a drag with /model open: rows that appear twice (B3)" "$(dup_lines)" 1
check_budget "a drag with /model open: separator rows in the history (X-52 OVER BUDGET, known: ≤ 6)" \
    "$(seps_all)" 6

finish
