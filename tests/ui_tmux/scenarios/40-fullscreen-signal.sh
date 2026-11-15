#!/usr/bin/env bash
# L4 scenario 40 — SIGTERM on the full screen (tui-plan S1) leaves a usable
# terminal: the main screen back, the tty canonical again, the shell's next prompt on screen.
#
# A: one SIGTERM mid-turn — the run winds down through its normal exit, so the conversation is
#    written out too, and the status is 130 (DIVERGENCES I-03).
# B: two SIGTERMs in a row — the second forces the exit (`signals::install`): the terminal is
#    restored (the alternate screen left first) but nothing is written out.
#
# SIGKILL has no answer: nothing runs, the pane stays on the alternate screen in raw mode — not
# tested, documented (tui-plan S1, the report).
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

iota_pid() {
    local pane child
    pane="$(tm display-message -pt s '#{pane_pid}' 2>/dev/null)"
    [ -n "$pane" ] || return 1
    for child in $(pgrep -P "$pane" 2>/dev/null); do
        case "$(ps -o comm= -p "$child" 2>/dev/null)" in
            *iota) echo "$child"; return 0 ;;
        esac
    done
    return 1
}
_iota_up() { IOTA_PID="$(iota_pid)"; }
alt_on() { tm display-message -pt s '#{alternate_on}'; }

# run_under_shell — the pane's shell runs iota full-screen, then prints its status and the tty mode
# (each marker split by an empty quote pair, so the typed line never matches it).
run_under_shell() {
    start_shell 80 24 || finish
    local cmd
    cmd="$(iota_cmd openai fake)"
    type_ "$cmd; echo \"r\"\"c=\$?\"; stty -a | tr ' ' '\\n' | grep -xE -- '-?icanon' | sed 's/^/t''ty=/'"
    key Enter
    wait_vis '❯' || miss "the composer never came up under the shell"
    _poll_until 50 _iota_up || miss "no iota process under the pane's shell"
    check "$1: the alternate screen is held" "$(alt_on)" 1
    type_ 'stream 60'
    key Enter
    wait_vis 'l#05 line' || miss "$1: the stream never started"
}

# usable <label> — what a shell needs back.
usable() {
    _poll_until 30 _vis_has 'tty=' || miss "$1: the shell never printed the tty mode"
    check "$1: the main screen is back" "$(alt_on)" 0
    check "$1: the tty is canonical again" "$(cap | grep -o 'tty=-*icanon' | tail -1)" "tty=icanon"
    type_ 'echo al''ive'
    key Enter
    if _poll_until 30 _vis_has 'alive'; then ok "$1: the shell takes a command"; else miss "$1: the shell does not answer"; fi
}

# --- A: one SIGTERM
run_under_shell A
kill -TERM "$IOTA_PID"
_poll_until 50 _vis_has 'rc=' || { bad "A: iota was still running 5 s after SIGTERM"; kill -KILL "$IOTA_PID" 2>/dev/null; }
check "A: status 130" "$(cap | grep -o 'rc=[0-9]*' | tail -1)" "rc=130"
usable A
if capall | grep -qF 'l#05 line'; then ok "A: the conversation was written out"; else bad "A: the conversation is not in the main screen"; fi

# --- B: two SIGTERMs
run_under_shell B
kill -TERM "$IOTA_PID"
kill -TERM "$IOTA_PID"
_poll_until 50 _vis_has 'rc=' || { bad "B: iota was still running 5 s after two SIGTERMs"; kill -KILL "$IOTA_PID" 2>/dev/null; }
check "B: status 130" "$(cap | grep -o 'rc=[0-9]*' | tail -1)" "rc=130"
usable B

finish
