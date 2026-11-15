#!/usr/bin/env bash
# L4 scenario 47 — the mouse through tmux itself (tui-plan S6's compatibility tail). iota runs in the
# usual private server; a second, outer server's pane is attached to it, and the reports are written
# into THAT pane — so the inner tmux parses them as its terminal's mouse and does with them what its
# bindings say, as it would for a real wheel:
#
#   A. `mouse on`: the wheel and a drag reach iota (the wheel scrolls; the drag's release copies
#      into the inner server's paste buffer);
#   B. `mouse off`: tmux's option is its own use of the mouse — a pane that asks for reporting
#      still gets the wheel (tmux 3.8, measured here); and with nothing delivered at all the
#      keyboard does everything (it degrades, it never wedges);
#   C. the off switch (`mouse: false`) with `mouse on`: iota asks for no reporting at all, and the
#      wheel is tmux's (what tmux does with it is recorded, not asserted).
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

OUT_SOCKET="$TMUX_SOCKET-outer"
tmo() { "$TMUX_BIN" -L "$OUT_SOCKET" "$@"; }
cleanup_outer() { tmo kill-server >/dev/null 2>&1 || true; cleanup; }
trap cleanup_outer EXIT
IOTA_ENV="-u SSH_CONNECTION -u SSH_TTY -u SSH_CLIENT"

# attach — the outer server's pane, attached to the inner session, both without status lines so a
# cell is the same cell in both.
attach() {
    tm set-option -g status off
    tmo kill-server >/dev/null 2>&1 || true
    tmo -f /dev/null new-session -d -s m -x 80 -y 24 "$TMUX_BIN -L $TMUX_SOCKET attach -t s"
    tmo set-option -g status off
    _attached() { [ "$(tm list-clients 2>/dev/null | wc -l | tr -d ' ')" -ge 1 ]; }
    _poll_until 50 _attached || miss "the outer pane never attached"
}
# outer <button> <col> <row> [M|m] — one SGR report (lib.sh's `mouse`) into the OUTER pane: the
# inner tmux reads it from its terminal.
outer() {
    local hex
    hex="$(printf '\033[<%s;%s;%s%s' "$1" "$2" "$3" "${4:-M}" | od -An -tx1 | tr -s ' \n' ' ')"
    # shellcheck disable=SC2086 # one argument per byte
    tmo send-keys -t m -H $hex
}
tm_mouse() { tm set-option -g mouse "$1"; }
tmo_mouse_flag() { tm display-message -pt s '#{mouse_any_flag}'; }

start 80 24 || finish
attach
tm_mouse on
settle || miss "startup never settled"
check "iota asked for reporting" "$(tmo_mouse_flag)" 1

# --- A. mouse on ----------------------------------------------------------------------------------
type_ 'stream 60 2'
key Enter
wait_vis 'l#59 line' || miss "the stream never ended"
settle || miss "the turn never settled"
for _ in 1 2 3; do outer 64 10 5; done
wait_vis 'Ctrl+End' || miss "tmux with mouse on never passed the wheel on"
ok "mouse on: the wheel reaches iota through tmux"
for _ in 1 2 3 4; do outer 65 10 5; done
wait_gone 'Ctrl+End' || miss "the wheel down never came back to the tail"
tm delete-buffer >/dev/null 2>&1 || true
r="$(row_of 'l#50 line')"
outer 0 1 "$r"
outer 32 9 $((r + 2))
outer 0 9 $((r + 2)) m
_buffered() { tm show-buffer 2>/dev/null | grep -qF 'l#52 line'; }
_poll_until 100 _buffered || miss "the drag's release never reached the paste buffer"
check "mouse on: a drag through tmux copies what it covered" "$(tm show-buffer)" "$(printf 'l#50 line\nl#51 line\nl#52 line')"
wait_gone 'copied' || true

# --- B. mouse off ---------------------------------------------------------------------------------
# tmux's `mouse` option is its OWN use of the mouse. A pane that asks for reporting still gets it:
# the inner tmux asks its terminal for reporting on the pane's behalf and passes the reports on.
tm_mouse off
check "mouse off: tmux still asks its terminal for reporting for the pane" "$(tmo display-message -pt m '#{mouse_any_flag}')" 1
for _ in 1 2 3; do outer 64 10 5; done
wait_vis 'Ctrl+End' || miss "tmux with mouse off did not pass the wheel on"
ok "mouse off: the wheel still reaches iota"
key C-End
wait_gone 'Ctrl+End' || miss "Ctrl+End no longer follows"
# A terminal that delivers nothing (iTerm2 with reporting off in its profile, `tmux -CC`): the
# reports never come, and the keyboard does what it did before.
key PPage
wait_vis 'Ctrl+End' || miss "PgUp no longer scrolls"
key C-End
wait_gone 'Ctrl+End' || miss "Ctrl+End no longer follows"
type_ 'still here'
check "…and the keyboard still does everything" "$(wait_composer '❯ still here' 1)" 1
for _ in $(seq 10); do key BSpace; done

# --- C. the off switch ----------------------------------------------------------------------------
CONFIG_BODY="$(printf 'providers:\n  mock: {type: openai, key: test, url: "http://127.0.0.1:%s"}\nmodels:\n  m: mock:fake\nagents:\n  default: {model: m}\nmouse: false\n' "$IOTA_PORT")"
start 80 24 || finish
attach
tm_mouse on
settle || miss "the second start never settled"
check "mouse: false — no reporting asked for" "$(tmo_mouse_flag)" 0
type_ 'stream 60 2'
key Enter
wait_vis 'l#59 line' || miss "the stream never ended (off)"
settle
pipe_raw
key C-o
_poll_until 50 _alt_is 0 || true
key Space
_poll_until 50 _alt_is 1 || miss "the write-out never came back"
settle
if raw_lacks "$(printf '\033[?1000h')" && raw_lacks "$(printf '\033[?1006h')"; then
    ok "…not even when the full screen comes back"
else
    bad "mouse reporting asked for with mouse: false"
fi
for _ in 1 2 3; do outer 64 10 5; done
sleep 1
if _vis_lacks 'Ctrl+End'; then ok "the wheel is tmux's, not iota's"; else bad "iota scrolled with the mouse off"; dump_pane; fi
echo "with mouse: false and tmux mouse on, the wheel left: in_mode=$(tm display-message -pt s '#{pane_in_mode}') composer=[$(composer_block | head -1)]"

finish
