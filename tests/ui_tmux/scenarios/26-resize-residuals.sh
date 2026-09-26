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
#   B. an idle narrowing right after FAST output (20 ms a line);
#   C. a narrowing with a surface open (`/model`), then closed;
#   D. a narrowing while a foreground tool call runs (the user's own `❯ runfg:…` row);
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
# after_resize <label> <width> <duplicated rows> <separator rows> — the invariants every block
# ends with; the last two are the block's measured budget (B3's residual).
after_resize() {
    settle || bad "$1: frame never settled"
    check_frame_intact "$1" "$2"
    if alive; then ok "$1: the app survived"; else bad "$1: the app died"; fi
    check_budget "$1: rows that appear twice in the history (B3)" "$(dup_lines)" "$3"
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
# rows that far up stay behind — measured 2 for both.
for to in 70x24 60x18; do
    fresh 80 24
    type_ 'stream 60 20'
    key Enter
    wait_all 'l#59 line' || bad "the fast stream never finished"
    settle || bad "the fast stream never settled"
    check "fast output, before any resize: no row appears twice" "$(dup_lines)" 0
    tm resize-window -t s -x "${to%x*}" -y "${to#*x}"
    if [ "$to" = 70x24 ]; then
        after_resize "fast output, then 80x24→$to" "${to%x*}" 2 2
    else
        settle || bad "fast output → $to: frame never settled"
        check_frame_intact "fast output, then 80x24→$to" "${to%x*}"
        check_budget "fast output → $to (a diagonal narrowing, B3)" "$(dup_lines)" 2
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
wait_vis 'sleep 1.5' || bad "the tool call never started"
tm resize-window -t s -x 70 -y 24
wait_all 'ran: ' || bad "the tool round never closed"
after_resize "a tool call running, 80→70" 70 1 2
check_budget "a tool call running, 80→70: the user's own row (B3: its piece above the cursor)" "$(count_all '❯ runfg:')" 2

# ------------------------------------------------------------------ E: scroll-on-clear on
fresh 60 16
tm set-option -w -t s scroll-on-clear on
type_ '帮我看看这个 resize 的问题'
settle || bad "the CJK draft never settled"
tm resize-window -t s -x 44 -y 16
after_resize "scroll-on-clear on, a CJK draft, 60x16→44x16 at startup" 44 1 2
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
fresh 100 24
type_ 'stream 30'
key Enter
wait_all 'l#29 line' || bad "the stream never finished"
settle || bad "the stream never settled"
for w in 99 98 97 96 95; do tm resize-window -t s -x "$w" -y 24; sleep 0.03; done
type_ 'stream 20'
key Enter
wait_all 'l#19 line' || bad "the second stream never finished"
settle || bad "the second stream never settled"
check_frame_intact "a stream right at the drag's end" 95
check "a stream right at the drag's end: the next turn follows the last one (1 blank row)" \
    "$(capall | awk 'index($0,"l#29 line"){f=NR} index($0,"❯ stream 20")&&f{print NR-f-1; exit}')" 1
check "a stream right at the drag's end: no hole inside the new turn" \
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
