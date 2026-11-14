#!/usr/bin/env bash
# L4 scenario 44 — job control on the full screen (IOTA_FULLSCREEN=1, tui-plan S4). Ctrl+Z (a key in
# raw mode) and a SIGTSTP from outside both hand the terminal back before the process stops: the
# main screen, the tty canonical, bracketed paste off, the shell's prompt and a stopped job. `fg`
# takes it again and paints the whole screen — the conversation and the frame, at the size the pane
# has NOW (resized while the job was stopped) — never a stale frame. The run then works as before,
# and an idle Ctrl+D leaves the conversation in the shell's history.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"
export IOTA_FULLSCREEN=1

alt_on() { tm display-message -pt s '#{alternate_on}'; }
paste_mode() {
    case "$(LC_ALL=C grep -aoE "$(printf '\033')\\[\\?2004[hl]" "$RAW" 2>/dev/null | tail -1)" in
        *h) echo on ;;
        *l) echo off ;;
    esac
}
iota_pid() {
    local pane child
    pane="$(tm display-message -pt s '#{pane_pid}')"
    for child in $(pgrep -P "$pane"); do
        case "$(ps -o comm= -p "$child")" in *iota) echo "$child"; return 0 ;; esac
    done
    return 1
}
stopped() { case "$(ps -o stat= -p "$1" 2>/dev/null)" in T*) return 0 ;; esac; return 1; }
_alt_is_v() { [ "$(alt_on)" = "$1" ]; }

# handed_back <label> — what the shell must have once the job stopped.
handed_back() {
    _poll_until 50 stopped "$pid" || miss "$1: iota did not stop"
    _poll_until 30 _alt_is_v 0 || bad "$1: the alternate screen is still up"
    check "$1: the main screen is back" "$(alt_on)" 0
    check "$1: bracketed paste is off" "$(paste_mode)" "off"
    type_ "stty -a | tr ' ' '\\n' | grep -xE -- '-?icanon' | sed 's/^/t''ty=/'"
    key Enter
    _poll_until 30 _vis_has 'tty=' || miss "$1: the shell does not answer"
    check "$1: the tty is canonical" "$(cap | grep -o 'tty=-*icanon' | tail -1)" "tty=icanon"
}

# taken_again <label> <width> — `fg`, and the whole screen repainted at that width.
taken_again() {
    tm send-keys -t s C-l
    type_ 'fg'
    key Enter
    _poll_until 50 _alt_is_v 1 || miss "$2: fg did not take the screen back"
    if _poll_until 50 eval '[ -n "$(cap | grep -F "echo: before the stop")" ]'; then
        ok "$1: the conversation is repainted"
    else
        bad "$1: the conversation is not on the screen after fg"
        dump_pane
    fi
    check_frame_intact "$1" "$2"
    check "$1: the bottom zone is the pane's last row" "$(($(frame_bot) + 1))" "$(tm display-message -pt s '#{pane_height}')"
    check "$1: bracketed paste is on again" "$(paste_mode)" "on"
}

start_shell 80 24 || finish
pipe_raw
type_ "$(iota_cmd openai fake); echo \"r\"\"c=\$?\""
key Enter
wait_vis '❯' || miss "the composer never came up"
_poll_until 50 eval 'pid="$(iota_pid)"; [ -n "$pid" ]' || miss "no iota under the shell"
type_ 'before the stop'
key Enter
wait_vis 'echo: before the stop' || miss "the first turn never replied"
settle

# A: Ctrl+Z, a resize while stopped, fg.
key C-z
handed_back "A: Ctrl+Z"
tm resize-window -t s -x 70 -y 22
taken_again "A: fg" 70

# B: SIGTSTP from outside, fg.
kill -TSTP "$pid"
handed_back "B: SIGTSTP"
taken_again "B: fg" 70

# The run goes on, and leaves its conversation behind.
type_ 'after the stop'
key Enter
wait_vis 'echo: after the stop' || miss "a turn after fg never replied"
settle
# The job was resumed with `fg`, so the shell dropped the rest of the line that started it: the
# status is `fg`'s, read with an `echo` of its own.
key C-d
gone() { ! kill -0 "$pid" 2>/dev/null; }
_poll_until 50 gone || miss "Ctrl+D did not end the run"
type_ 'echo "r""c=$?"'
key Enter
_poll_until 30 _vis_has 'rc=' || miss "the shell did not answer after the exit"
check "exit 0" "$(cap | grep -o 'rc=[0-9]*' | tail -1)" "rc=0"
check "the conversation is in the history once: before" "$(capall | grep -cF 'echo: before the stop')" 1
check "…and after" "$(capall | grep -cF 'echo: after the stop')" 1

finish
