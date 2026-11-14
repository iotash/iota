#!/usr/bin/env bash
# L4 scenario 42 — a key that arrives together with a resize is read at once (tui-plan S4, S2's two
# leads). crossterm's default unix event source reads the tty and the SIGWINCH pipe through ONE
# edge-triggered mio poll and returns the resize as soon as it sees it: the tty's readiness in the
# same batch is dropped, and an edge-triggered registration never reports it again until NEW bytes
# arrive. The key then sits in the kernel's buffer — an idle Ctrl+D "lost" until the next keystroke
# (scenario 39 under a resize, 1 run in ~5) — while the loop's level-triggered hangup watch sees the
# tty readable on every turn and spins (S2's orphan, PPID 1, spinning in `crossterm::event::poll`).
#
# A: a resize and a key are sent in the same instant, again and again, and each key must be in the
# composer within a second; then, the way the field met it, an idle Ctrl+D under a resize.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

ROUNDS=40
# The process's CPU time so far, in centiseconds (`ps -o time` is [[dd-]hh:]mm:ss.cc).
cputime() { ps -o time= -p "$pid" | tr -d ' ' | awk -F'[:.]' '{ n = NF; print (($(n-2) * 60) + $(n-1)) * 100 + $n }'; }
draft() { composer_block | head -1 | sed -E 's/^❯ ?//' | tr -d ' '; }

start 80 24 || finish
settle || miss "startup never settled"
pid="$(tm display-message -pt s '#{pane_pid}')"
stuck=0
spun=0
w=80
i=0
while [ "$i" -lt "$ROUNDS" ]; do
    w=$((w == 80 ? 79 : 80))
    want="$(printf 'x%.0s' $(seq 1 $((i + 1))))"
    tm resize-window -t s -x "$w" -y 24 \; send-keys -t s -l x
    if ! _poll_until 10 eval '[ "$(draft)" = "$want" ]'; then
        stuck=$((stuck + 1))
        # While a key is stranded, the loop must not spin: CPU time over one second, read twice.
        t0="$(cputime)"
        sleep 1 # the window the claim is about
        [ $(($(cputime) - t0)) -ge 50 ] && spun=$((spun + 1))
        # The next key frees it (and is the round's own).
        _poll_until 30 eval '[ "$(draft)" = "$want" ]' || true
    fi
    i=$((i + 1))
done
check "every key sent with a resize is read at once ($ROUNDS rounds)" "$stuck" 0
check "…and the loop never spins with input waiting" "$spun" 0

# The field case: an idle Ctrl+D under a resize ends the run.
key C-u
_poll_until 20 eval '[ -z "$(draft)" ]' || miss "the draft did not clear"
tm set-option -t s remain-on-exit on
tm resize-window -t s -x 70 -y 24 \; send-keys -t s C-d
if _poll_until 20 pane_dead; then ok "an idle Ctrl+D sent with a resize ends the run"; else bad "the Ctrl+D sent with a resize was not read"; fi

# B: the terminal hangs up while input is arriving — crossterm's old reader, entered on "readable",
# read the hung-up tty's EOF in a loop it never left (S2's orphan's stack: `try_read`, spinning). 20
# rounds: keys streaming in, the server killed under them; every iota must be gone within 2 s.
gone() { ! kill -0 "$1" 2>/dev/null; }
orphans=0
r=0
while [ "$r" -lt 20 ]; do
    start 80 24 || finish
    settle || miss "B: startup never settled"
    pid="$(tm display-message -pt s '#{pane_pid}')"
    (for _ in $(seq 1 200); do tm send-keys -t s -l y 2>/dev/null || break; done) &
    feeder=$!
    sleep 0.0$((r % 10)) # where in the stream the hangup lands
    tm kill-server >/dev/null 2>&1
    wait "$feeder" 2>/dev/null
    if ! _poll_until 20 gone "$pid"; then
        orphans=$((orphans + 1))
        echo "    round $r: iota $pid outlived its terminal: $(ps -o ppid=,stat=,%cpu= -p "$pid" 2>/dev/null)"
        kill -KILL "$pid" 2>/dev/null
    fi
    r=$((r + 1))
done
check "B: a hangup under streaming input ends iota every time (20 rounds)" "$orphans" 0

finish
