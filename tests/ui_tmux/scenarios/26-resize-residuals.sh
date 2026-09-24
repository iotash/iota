#!/usr/bin/env bash
# L4 scenario 26 (docs/TUI-VERIFY.md §4.3, DIVERGENCES X-52) — the resize cases scenario 06's one
# long session cannot isolate. Each block starts a FRESH pane, so every non-blank row the pane ever
# shows is unique: after the resize, no row may appear twice anywhere in the history, no separator
# row may be added, and every streamed line must still be there. `scroll-on-clear` is off, so a
# clear loses rows here exactly as it does in Ghostty and herdr.
#
#   A. a drastic narrowing — the old width 2× and 3× the new (a maximized window restored, a
#      full-width pane halved) — with the startup banner still staged in the frame;
#   B. an idle narrowing right after FAST output (20 ms a line);
#   C. a narrowing with a surface open (`/model`), then closed;
#   D. a narrowing while a foreground tool call runs (the user's own `❯ runfg:…` row).
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
# after_resize <label> <width> — the exact invariants every block ends with.
after_resize() {
    settle || bad "$1: frame never settled"
    check_frame_intact "$1" "$2"
    if alive; then ok "$1: the app survived"; else bad "$1: the app died"; fi
    check "$1: no row appears twice in the history" "$(dup_lines)" 0
    capall | sed 's/ *$//' | grep -v '^$' | grep -v '^┄' | LC_ALL=C sort | LC_ALL=C uniq -d | sed 's/^/    DUP: /'
    check "$1: separator rows in the whole history" "$(seps_all)" 2
}

# ------------------------------------------------------------------ A: 2× and 3× narrowings
for to in 100 67; do
    fresh 201 30
    tm resize-window -t s -x "$to" -y 30

    after_resize "201→$to at startup" "$to"
done

# ------------------------------------------------------------------ B: fast output, then idle
# 70x24 is a width-only narrowing: exact. 60x18 is the session's first resize AND diagonal —
# the ACCEPTED RESIDUAL of X-52: tmux eats the rows below the cursor before it rewraps, so this
# resize cannot tell that tmux reflows, claims no overhang, and the frame's first staged row
# stays behind once. Bounded at that one row here (a second would be a regression); the same
# resize after any width-only narrowing is exact (scenario 06, vt100_tests).
for to in 70x24 60x18; do
    fresh 80 24
    type_ 'stream 60 20'
    key Enter
    wait_all 'l#59 line' || bad "the fast stream never finished"
    settle || bad "the fast stream never settled"
    check "fast output, before any resize: no row appears twice" "$(dup_lines)" 0
    tm resize-window -t s -x "${to%x*}" -y "${to#*x}"
    if [ "$to" = 70x24 ]; then
        after_resize "fast output, then 80x24→$to" "${to%x*}"
    else
        settle || bad "fast output → $to: frame never settled"
        check_frame_intact "fast output, then 80x24→$to" "${to%x*}"
        check_budget "fast output → $to (first resize, diagonal: the accepted residual)" "$(dup_lines)" 1
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
after_resize "/model open, 80→68" 68
check "/model open, 80→68: empty composer rows in the history" "$(capall | grep -c '^❯ *$')" 1

# ------------------------------------------------------------------ D: a tool call running
fresh 80 24
type_ 'runfg:sleep 1.5; seq -f "t#%02g out" 0 30'
key Enter
wait_vis 'sleep 1.5' || bad "the tool call never started"
tm resize-window -t s -x 70 -y 24
wait_all 'ran: ' || bad "the tool round never closed"
after_resize "a tool call running, 80→70" 70
check "a tool call running, 80→70: the user's own row" "$(count_all '❯ runfg:')" 1

finish
