#!/usr/bin/env bash
# L4 scenario 29 — a signal ends the run, and so does the terminal going away.
#
# 2026-10-02: fifty orphaned `iota` processes, PPID 1, each at 100% CPU and deaf to SIGTERM —
# what `tmux kill-server` left of the panes a bot experiment and this suite had started. The
# hangup made crossterm's reader spin on the tty's EOF inside one `event::poll` call, so the
# loop thread never came back to its mailbox; SIGHUP had cancelled the run, `close()` waited on
# that thread for good, and every later SIGTERM fell on a tokio handler nobody listened to.
#
# A: SIGTERM mid-turn, the terminal alive: the run winds down — the turn's message in the
#    session log, raw mode and bracketed paste handed back to the shell that started it — and
#    exits 130, the status DIVERGENCES I-03 gives a signalled run in either mode (it was 0 until
#    0.6.0: the wind-down is the normal exit path, and the status never noticed the signal).
# C: the same shell-hosted run left with an idle Ctrl+D exits 0 — the 130 is the signal's, not
#    every wind-down's.
# B: the pty hung up (`kill-server`): the process is gone well inside `CLOSE_GRACE` (5 s, the
#    backstop in `Ui::close`), so this measures the hangup watch, not the backstop. Without the
#    watch it exits at 5 s on the backstop alone, and without both it never does — a run that
#    is still there is SIGKILLed here, never left behind.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

# The pane's iota: the pane process itself, or (under a shell) its child of that name.
iota_pid() {
    local pane
    pane="$(tm display-message -pt s '#{pane_pid}' 2>/dev/null)"
    [ -n "$pane" ] || return 1
    case "$(ps -o comm= -p "$pane" 2>/dev/null)" in
        *iota) echo "$pane"; return 0 ;;
    esac
    local child
    for child in $(pgrep -P "$pane" 2>/dev/null); do
        case "$(ps -o comm= -p "$child" 2>/dev/null)" in
            *iota) echo "$child"; return 0 ;;
        esac
    done
    return 1
}
_iota_up() { IOTA_PID="$(iota_pid)"; }
gone() { ! kill -0 "$1" 2>/dev/null; }

# Bracketed paste as the pane's bytes last set it: `on` after `ESC[?2004h`, `off` after
# `ESC[?2004l`, empty before either. Read from the raw tap, not from tmux's
# `#{bracket_paste_flag}`: that format is newer than the tmux 3.4 the ubuntu runner installs,
# where it expands to nothing (CI 37025565018). The shell is `sh` (dash, or bash 3.2 on macOS),
# which never toggles the mode itself, so every toggle in the capture is iota's.
paste_mode() {
    case "$(LC_ALL=C grep -aoE "$(printf '\033')\\[\\?2004[hl]" "$RAW" 2>/dev/null | tail -1)" in
        *h) echo on ;;
        *l) echo off ;;
    esac
}
_paste_is() { [ "$(paste_mode)" = "$1" ]; }

# ---------------------------------------------------------------- A: SIGTERM mid-turn

start_shell 80 24 || finish
pipe_raw
cmd="$(iota_cmd openai fake)"
# After iota: its exit status, then whether the tty is cooked again (`icanon` set, not
# `-icanon`). One line each, so the capture reads them back; each marker is split by an empty
# quote pair, so the echo of the typed line itself never matches it.
type_ "$cmd; echo \"r\"\"c=\$?\"; stty -a | tr ' ' '\\n' | grep -xE -- '-?icanon' | sed 's/^/t''ty=/'"
key Enter
wait_vis '❯' || bad "A: the composer never came up under the shell"
_poll_until 50 _iota_up || bad "A: no iota process under the pane's shell"
_poll_until 30 _paste_is on
check "A: bracketed paste is on while iota runs" "$(paste_mode)" "on"

type_ 'stream 60'
key Enter
wait_vis 'l#05 line' || bad "A: the stream never started"
kill -TERM "$IOTA_PID"
if _poll_until 30 _vis_has 'rc='; then
    ok "A: SIGTERM mid-turn ends the run within 3 s"
else
    bad "A: iota was still running 3 s after SIGTERM"
    kill -KILL "$IOTA_PID" 2>/dev/null
fi
check "A: …with status 130 (DIVERGENCES I-03)" "$(cap | grep -o 'rc=[0-9]*' | tail -1)" "rc=130"
_poll_until 30 _vis_has 'tty=' || bad "A: the shell never printed the tty mode"
check "A: …raw mode handed back (the tty is canonical again)" "$(cap | grep -o 'tty=-*icanon' | tail -1)" "tty=icanon"
_poll_until 30 _paste_is off
check "A: …bracketed paste switched off" "$(paste_mode)" "off"
# The interrupt table persisted the turn: the bundle holds the message SIGTERM cut short.
log="$(find "$SCEN_HOME/.iota/sessions" -name messages.jsonl 2>/dev/null | head -1)"
if [ -n "$log" ] && grep -qF '"stream 60"' "$log"; then
    ok "A: …and the interrupted turn's message is in the session log"
else
    bad "A: the session log has no record of the interrupted turn (${log:-no log})"
fi

# ---------------------------------------------------------------- C: Ctrl+D is not a signal

start_shell 80 24 || finish
type_ "$cmd; echo \"r\"\"c=\$?\""
key Enter
wait_vis '❯' || bad "C: the composer never came up under the shell"
key C-d
if _poll_until 30 _vis_has 'rc='; then
    check "C: an idle Ctrl+D exits 0" "$(cap | grep -o 'rc=[0-9]*' | tail -1)" "rc=0"
else
    bad "C: iota was still running 3 s after an idle Ctrl+D"
fi

# ---------------------------------------------------------------- B: the terminal goes away

start 80 24 || finish
settle || bad "B: startup never settled"
_poll_until 50 _iota_up || bad "B: no iota process in the pane"
pid="$IOTA_PID"
tm kill-server >/dev/null 2>&1
if _poll_until 20 gone "$pid"; then
    ok "B: iota exits within 2 s of its terminal hanging up"
else
    # What it is doing is the regression's signature: state R and a pegged core is the spin.
    bad "B: iota outlived its terminal by 2 s: $(ps -o ppid=,stat=,%cpu= -p "$pid" 2>/dev/null)"
    kill -KILL "$pid" 2>/dev/null
fi

finish
