#!/usr/bin/env bash
# L4 scenario 36 (config-trust) — the question a project's `.iota.yaml` asks, on a real terminal.
#
# `tests/cmd/trust.rs` pins every start that is NOT asked (headless, a piped stdin, `-c`) through the
# binary; what is left is the one start that is: an interactive run whose stdin and stderr are a
# terminal. The project file starts an MCP server that touches a marker — the marker existing is the
# proof that project-controlled code ran.
#
#   36.1 the question comes BEFORE anything of the file runs: what the file does is on screen, the
#        marker absent;
#   36.2 Enter is "not now": one line says the file is not used, the chat starts on the home config,
#        and the marker stays absent;
#   36.3 `t` trusts it: the chat starts and the file's server ran;
#   36.4 trusted, the next start asks nothing.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

QUESTION='Trust it? [t]rust / [N]ot now:'
PROJ="$SCEN_TMP/proj"
MARKER="$PROJ/marker"
mkdir -p "$PROJ"
cat >"$PROJ/.iota.yaml" <<EOF
mcp_servers:
  planted: {command: sh, args: [-c, "touch '$MARKER'"]}
EOF
write_iota_config openai fake

marker_absent() { [ ! -e "$MARKER" ]; }
marker_present() { [ -e "$MARKER" ]; }

# run_iota — the chat started from the shell in the project directory, the home the scenario's.
run_iota() {
    type_ "clear; cd '$PROJ' && env -u HERDR_ENV HOME='$SCEN_HOME' '$IOTA_BIN'"
    key Enter
}

# quit — an idle Ctrl+D ends the chat: the pane's foreground process is the shell again (`sh` is bash
# on macOS, so the test is that it is no longer iota).
shell_back() {
    local c
    c="$(tm display-message -pt s '#{pane_current_command}' 2>/dev/null)"
    [ -n "$c" ] && [ "$c" != "iota" ]
}
quit() {
    key C-d
    _poll_until 100 shell_back || miss "the chat did not exit"
}

start_shell 100 30 || finish
_poll_until 50 shell_back || miss "the shell never came up"

# 36.1 + 36.2
run_iota
wait_vis "$QUESTION" || miss "the question never came"
if _vis_has 'is a project config you have not trusted.' && _vis_has 'mcp_servers.planted: runs sh -c'; then
    ok "the question shows what the file does"
else
    miss "the question does not show the file's MCP server"
fi
if marker_absent; then ok "nothing of the file ran while it was asked"; else bad "the marker exists before the answer"; fi
key Enter
wait_vis '❯' || miss "the chat never started after not now"
if _all_has 'Not using' && _all_has 'for this run'; then ok "not now is said in one line"; else miss "no line says the file is skipped"; fi
check "the chat runs on the home config" "$(status_model | sed 's/^ *//')" "fake"
if marker_absent; then ok "not now: the file's server never ran"; else bad "not now: the marker exists"; fi
quit

# 36.3
run_iota
wait_vis "$QUESTION" || miss "the question never came the second time"
type_ t
key Enter
wait_vis '❯' || miss "the chat never started after trust"
if _poll_until 100 marker_present; then ok "trusted: the file's server ran"; else bad "trusted: the marker never appeared"; fi
quit

# 36.4
rm -f "$MARKER"
run_iota
wait_vis '❯' || miss "the trusted start never reached the chat"
if _vis_has "$QUESTION"; then bad "a trusted file was asked about again"; else ok "a trusted file asks nothing"; fi
if _poll_until 100 marker_present; then ok "and its server ran"; else bad "the trusted start never ran the server"; fi
quit

finish
