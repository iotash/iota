#!/usr/bin/env bash
# L4 scenario 45 — the full screen's two forced ways out (IOTA_FULLSCREEN=1, tui-plan S4).
#
# A: a panic. Nothing in the tree can panic under the lint wall, so a debug build's test hook does
#    it on request (`IOTA_TEST_PANIC`, typed as a message). The panic hook hands the terminal back
#    FIRST: the raw bytes leave the alternate screen before the panic's report is written, so the
#    report is on the shell's screen; the tty is canonical; the next iota starts cleanly. (A panic's
#    run writes no conversation out — the buffer lives on the loop thread.)
# B: SIGTERM twice. The first starts the wind-down; a stdio MCP server that never leaves (the stub
#    ignores SIGTERM and stdin closing) holds it in `manager.close()` for seconds, after the UI is
#    already down. A second SIGTERM then ends the run at once — only `signals::install`'s forced
#    branch can exit before that close returns — with status 130 and the terminal usable.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"
export IOTA_FULLSCREEN=1

STUB="$(dirname "$TMUX_LIB")/stubs/stuck-mcp.py"
alt_on() { tm display-message -pt s '#{alternate_on}'; }
_alt_is_v() { [ "$(alt_on)" = "$1" ]; }
iota_pid() {
    local pane child
    pane="$(tm display-message -pt s '#{pane_pid}')"
    for child in $(pgrep -P "$pane"); do
        case "$(ps -o comm= -p "$child")" in *iota) echo "$child"; return 0 ;; esac
    done
    return 1
}
gone() { ! kill -0 "$1" 2>/dev/null; }
now_ms() { perl -MTime::HiRes=time -e 'printf "%d", time*1000'; }
# launch <label> <env/args prefix> <args> — iota under the shell, the status and tty mode after it.
launch() {
    tm send-keys -t s C-l
    : >"$RAW"
    type_ "$2 $(iota_cmd openai fake) ${3:-}; echo \"r\"\"c=\$?\"; stty -a | tr ' ' '\\n' | grep -xE -- '-?icanon' | sed 's/^/t''ty=/'"
    key Enter
    wait_vis '❯' || miss "$1: the composer never came up"
    _poll_until 50 eval 'pid="$(iota_pid)"; [ -n "$pid" ]' || miss "$1: no iota under the shell"
    type_ 'before the end'
    key Enter
    wait_vis 'echo: before the end' || miss "$1: the first turn never replied"
    settle
}
usable() {
    _poll_until 50 _vis_has 'tty=' || miss "$1: the shell never printed the tty mode"
    check "$1: the main screen is back" "$(alt_on)" 0
    check "$1: the tty is canonical" "$(cap | grep -o 'tty=-*icanon' | tail -1)" "tty=icanon"
}

start_shell 100 30 || finish
pipe_raw

# --- A: a panic
launch A "IOTA_TEST_PANIC=__boom__"
type_ '__boom__'
key Enter
_poll_until 100 _vis_has 'rc=' || miss "A: the run did not end on the panic"
usable A
if _vis_has 'IOTA_TEST_PANIC'; then ok "A: the panic's report is on the shell's screen"; else bad "A: no panic report on the screen"; dump_pane; fi
leave_at="$(raw_offset "$(printf '\033')[?1049l")"
report_at="$(raw_offset 'IOTA_TEST_PANIC: the test hook')"
if [ -n "$leave_at" ] && [ -n "$report_at" ] && [ "$leave_at" -lt "$report_at" ]; then
    ok "A: the hook left the alternate screen before the report ($leave_at < $report_at)"
else
    bad "A: the alternate screen was not left before the report (leave=$leave_at report=$report_at)"
fi
if [ "$(cap | grep -o 'rc=[0-9]*' | tail -1)" != "rc=0" ]; then ok "A: a failing status"; else bad "A: exit 0 after a panic"; fi

# --- B: SIGTERM once — the wind-down waits on the stuck server, the UI already down.
launch B1 "" "--mcp 'python3 $STUB'"
pgrep -f stuck-mcp.py >/dev/null || miss "B1: the stub MCP server is not running"
t0="$(now_ms)"
kill -TERM "$pid"
_poll_until 50 _alt_is_v 0 || miss "B1: the UI never closed"
_poll_until 200 gone "$pid" || { bad "B1: iota never exited"; kill -KILL "$pid"; }
took=$(($(now_ms) - t0))
if [ "$took" -ge 1500 ]; then ok "B1: one SIGTERM winds down through the MCP close (${took} ms)"; else bad "B1: the wind-down did not wait on the stub (${took} ms) — B2 proves nothing"; fi
usable B1
pkill -KILL -f stuck-mcp.py

# --- B: SIGTERM twice — the second, sent while the close is held, ends the run at once.
launch B2 "" "--mcp 'python3 $STUB'"
kill -TERM "$pid"
_poll_until 50 _alt_is_v 0 || miss "B2: the UI never closed"
if gone "$pid"; then bad "B2: iota was gone before the second SIGTERM"; fi
t0="$(now_ms)"
kill -TERM "$pid"
_poll_until 50 gone "$pid" || { bad "B2: the second SIGTERM did not end the run"; kill -KILL "$pid"; }
took=$(($(now_ms) - t0))
if [ "$took" -lt 1000 ]; then ok "B2: the second SIGTERM took the forced branch (${took} ms)"; else bad "B2: the second SIGTERM waited (${took} ms)"; fi
usable B2
check "B2: status 130" "$(cap | grep -o 'rc=[0-9]*' | tail -1)" "rc=130"
check "B2: the conversation was written out by the UI's close (B1's copy and its own; A's panic wrote none)" "$(capall | grep -cF 'echo: before the end')" 2
pkill -KILL -f stuck-mcp.py

# --- C: the next iota in the same terminal starts and ends cleanly.
launch C ""
key C-d
_poll_until 50 _vis_has 'rc=' || miss "C: Ctrl+D did not end the run"
check "C: exit 0" "$(cap | grep -o 'rc=[0-9]*' | tail -1)" "rc=0"
usable C

finish
