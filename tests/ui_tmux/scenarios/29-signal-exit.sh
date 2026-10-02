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
#    session log, raw mode and bracketed paste handed back to the shell that started it.
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

# ---------------------------------------------------------------- A: SIGTERM mid-turn

start_shell 80 24 || finish
cmd="$(iota_cmd openai fake)"
# After iota: its exit status, then whether the tty is cooked again (`icanon` set, not
# `-icanon`). One line each, so the capture reads them back; each marker is split by an empty
# quote pair, so the echo of the typed line itself never matches it.
type_ "$cmd; echo \"r\"\"c=\$?\"; stty -a | tr ' ' '\\n' | grep -xE -- '-?icanon' | sed 's/^/t''ty=/'"
key Enter
wait_vis '❯' || bad "A: the composer never came up under the shell"
_poll_until 50 _iota_up || bad "A: no iota process under the pane's shell"
check "A: bracketed paste is on while iota runs" "$(tm display-message -pt s '#{bracket_paste_flag}')" "1"

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
# The status itself is not pinned: an interactive run ends its loop on a cancelled read the way it
# does on Ctrl+D (exit 0) — the 130 of DIVERGENCES I-03 is the headless contract.
_poll_until 30 _vis_has 'tty=' || bad "A: the shell never printed the tty mode"
check "A: …raw mode handed back (the tty is canonical again)" "$(cap | grep -o 'tty=-*icanon' | tail -1)" "tty=icanon"
check "A: …bracketed paste switched off" "$(tm display-message -pt s '#{bracket_paste_flag}')" "0"
# The interrupt table persisted the turn: the bundle holds the message SIGTERM cut short.
log="$(find "$SCEN_HOME/.iota/sessions" -name messages.jsonl 2>/dev/null | head -1)"
if [ -n "$log" ] && grep -qF '"stream 60"' "$log"; then
    ok "A: …and the interrupted turn's message is in the session log"
else
    bad "A: the session log has no record of the interrupted turn (${log:-no log})"
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
