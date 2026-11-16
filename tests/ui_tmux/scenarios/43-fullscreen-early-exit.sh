#!/usr/bin/env bash
# L4 scenario 43 — the full screen's point of no return is its first frame (tui-plan S4): a run
# that ends before it — `--help`, a config that does not parse, a session that
# does not exist — never enters the alternate screen; its message is on the shell's screen, the tty
# is canonical again, and the next iota in the same terminal starts and exits cleanly.
#
# The pane runs a shell with the raw tap on (`pipe_raw`): `?1049h` is a byte to the terminal, not a
# glyph, so only the tap can say it was never sent.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

ALT_ON="$(printf '\033')[?1049h"
# run_in_shell <label> <command> — types the command with the status and tty markers after it (each
# split by an empty quote pair, so the typed line never matches) and waits for the markers.
run_in_shell() {
    : >"$RAW"
    type_ "$2; echo \"r\"\"c=\$?\"; stty -a | tr ' ' '\\n' | grep -xE -- '-?icanon' | sed 's/^/t''ty=/'"
    key Enter
    _poll_until 100 _vis_has 'tty=' || miss "$1: the command never finished"
}
rc() { cap | grep -o 'rc=[0-9]*' | tail -1; }
tty_mode() { cap | grep -o 'tty=-*icanon' | tail -1; }

start_shell 100 30 || finish
pipe_raw
bin="env -u HERDR_ENV HOME=$SCEN_HOME $IOTA_BIN"

run_in_shell "--help" "$bin --help"
if _vis_has 'Commands:'; then ok "--help: the usage is on the shell's screen"; else bad "--help: no usage"; dump_pane; fi
check_raw "--help: the alternate screen was never entered" "$ALT_ON" no
check "--help: exit 0" "$(rc)" "rc=0"
tm send-keys -t s C-l

CONFIG_BODY='providers: [unclosed' write_iota_config openai fake
run_in_shell "bad config" "$bin"
if cap | grep -qiE 'error|yaml|config'; then ok "bad config: the error is on the shell's screen"; else bad "bad config: no error shown"; dump_pane; fi
check_raw "bad config: the alternate screen was never entered" "$ALT_ON" no
if [ "$(rc)" != "rc=0" ]; then ok "bad config: a failing status ($(rc))"; else bad "bad config: exit 0"; fi
check "bad config: the tty is canonical" "$(tty_mode)" "tty=icanon"
tm send-keys -t s C-l

run_in_shell "missing session" "$(iota_cmd openai fake resume nosuchsessn1)"
if cap | grep -qiE 'nosuchsessn1|no session|not found'; then ok "missing session: the error is on the shell's screen"; else bad "missing session: no error shown"; dump_pane; fi
check_raw "missing session: the alternate screen was never entered" "$ALT_ON" no
if [ "$(rc)" != "rc=0" ]; then ok "missing session: a failing status ($(rc))"; else bad "missing session: exit 0"; fi
check "missing session: the tty is canonical" "$(tty_mode)" "tty=icanon"
tm send-keys -t s C-l

# The next iota in the same terminal: up on the full screen, out with Ctrl+D, the shell back.
: >"$RAW"
type_ "$(iota_cmd openai fake); echo \"r\"\"c=\$?\""
key Enter
_poll_until 100 _frame_up || miss "the next iota never came up"
check_raw "the next iota entered the alternate screen" "$ALT_ON" yes
key C-d
_poll_until 50 _vis_has 'rc=' || miss "the next iota did not exit"
check "…and exits 0" "$(rc)" "rc=0"
check "…on the main screen" "$(tm display-message -pt s '#{alternate_on}')" 0

finish
