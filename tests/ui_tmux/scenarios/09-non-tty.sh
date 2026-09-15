#!/usr/bin/env bash
# L4 scenario 9 (TUI_TEST_PLAN §L4) — a piped run has no interactive mode.
#
# `tests/cmd/interactive_cli.rs` asserts the same refusal by running the built binary;
# what THIS scenario adds is the real pipe — stdin is genuinely not a tty, which is the
# condition `cmd/root.go:397-401` tests and the one an in-process double cannot create.
# The text is Go's, byte for byte, and it must arrive before any side effect: no session
# bundle, no MCP connect, no terminal takeover.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

WANT='Error: interactive mode requires a terminal; use -m/--message for piped input'

write_iota_config openai fake
out="$(echo hi | env HOME="$SCEN_HOME" "$IOTA_BIN" 2>"$SCEN_TMP/err.txt")"
code=$?

check "exit status" "$code" 1
check "stdout is empty" "$out" ""
check "stderr is Go's refusal, byte for byte" "$(cat "$SCEN_TMP/err.txt")" "$WANT"
check "the refusal is the WHOLE of stderr" "$(wc -l <"$SCEN_TMP/err.txt" | tr -d ' ')" 1

# The refusal precedes every side effect: a doomed run creates no session bundle.
if [ -d "$SCEN_HOME/.iota/sessions" ] && [ -n "$(ls -A "$SCEN_HOME/.iota/sessions" 2>/dev/null)" ]; then
    bad "a refused run still created a session bundle"
else
    ok "a refused run creates no session bundle"
fi

# The same refusal for `iota resume` with no id, which is resolved AFTER the tty check (root.go order).
echo hi | env HOME="$SCEN_HOME" "$IOTA_BIN" resume >/dev/null 2>"$SCEN_TMP/err2.txt"
check "a piped bare \`iota resume\` refuses the same way" "$(cat "$SCEN_TMP/err2.txt")" "$WANT"

finish
