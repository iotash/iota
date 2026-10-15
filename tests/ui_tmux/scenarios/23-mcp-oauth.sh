#!/usr/bin/env bash
# L4 scenario 23 (brain page `mcp-cli-and-oauth`) — a server that asks for a login, seen from the chat.
#
# The chain itself is pinned in-process (`tests/mcp/oauth.rs`) and through the CLI
# (`tests/cmd/mcp.rs`); what was left to a human is what it LOOKS like in the REPL. The entry
# says nothing about `auth` (the default, `auto`): the server's own 401 at the first handshake
# is what asks for the login, and the token file is what makes the next connect an OAuth one.
# The login and the logout are the CLI's — the chat has no `/mcp` (X-42) — so this script runs
# them beside the pane, against the same HOME and config, through the same `$BROWSER` stand-in.
#
#   23.1 the startup notice — a server whose handshake was answered 401, with no token, is ONE
#        red line naming `iota mcp login nb`, not the generic failure; the chat is usable (the
#        model, the composer);
#   23.2 `/tools` (X-79) — the server's row in the list reads `needs login`, and its page says
#        the same sentence as the notice;
#   23.3 `iota mcp login nb` beside the chat — the URL is the mock's, `$BROWSER` (a script that
#        follows the redirect with curl) brings the callback back, the CLI announces the login,
#        the token file appears with mode 600; the pane saw none of it;
#   23.4 the SAME chat picks the new token up: Enter on the server's page reconnects it, the
#        page reads `connected` with the server's tool and the transcript says so; then the chat
#        started again — no notice this time, and the page reads `connected` from the start;
#   23.5 `iota mcp logout nb` — the CLI says it forgot the file, and the file is gone.
#
# The mock authorization server + MCP endpoint is the runner's (`tests/common/oauth_mock.rs`,
# started once per test process; its port is `$IOTA_OAUTH_PORT`). Under the runner's C locale
# every grep here is `-F` on the lib's helpers — no bracket expression sees a multibyte glyph.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

: "${IOTA_OAUTH_PORT:?IOTA_OAUTH_PORT must be the mock authorization server port}"
command -v curl >/dev/null 2>&1 || { bad "curl not found (the browser stand-in needs it)"; finish; }

CONFIG_BODY="providers:
  mock: {type: openai, key: test, url: \"http://127.0.0.1:$IOTA_PORT\"}
models:
  m: mock:fake
agents:
  default:
    model: m
    choices: [m, \"mock:*\"]
mcp_servers:
  nb:
    url: http://127.0.0.1:$IOTA_OAUTH_PORT/mcp"
export CONFIG_BODY

# The browser stand-in: follows the authorization URL through the 302 to iota's loopback
# callback, detached so the login never waits on it.
cat >"$SCEN_TMP/browser.sh" <<'EOB'
#!/bin/sh
curl -sL "$1" >/dev/null 2>&1 &
EOB
export BROWSER="sh $SCEN_TMP/browser.sh"
TOKEN_FILE="$SCEN_HOME/.iota/mcp/auth/nb.json"
NOTICE='⚠ MCP nb not logged in: run iota mcp login nb'
# The list's and the page's prompt lines (`/tools`, X-79): what says which level is open.
LIST_PROMPT='Enter: open · Esc: close'
PAGE_PROMPT='Esc: back'

# iota_mcp <verb> [args…] — the CLI beside the pane: the HOME the pane runs under (the config
# `start_provider` wrote there, the token directory under it), the same browser stand-in, and
# the file named on the command line, as a user would. stdout and stderr land in $CLI_OUT.
CLI_OUT="$SCEN_TMP/cli.out"
iota_mcp() {
    env HOME="$SCEN_HOME" BROWSER="$BROWSER" "$IOTA_BIN" -c "$SCEN_HOME/.iota.yaml" mcp "$@" >"$CLI_OUT" 2>&1
}
cli_has() { grep -qF -- "$1" "$CLI_OUT"; }
# check_cli <description> <fixed string> — the CLI's output has it (the output is shown when not).
check_cli() {
    if cli_has "$2"; then ok "$1"; else bad "$1: $(printf '%q' "$2") not in the CLI's output"; cat "$CLI_OUT"; fi
}

# open_list / open_page / close_viewer — `/tools` (X-79): the list, Down to nb (the row under
# `built in`) and Enter to its page, and ESC back — the page to the list, the list to the composer.
open_list() {
    type_ '/tools'
    key Enter
    wait_vis "$LIST_PROMPT" || { bad "the /tools list never opened"; cap; return 1; }
    settle || bad "the list never settled"
}
open_page() {
    key Down
    key Enter
    wait_vis "$PAGE_PROMPT" || { bad "Enter did not open nb's page"; cap; return 1; }
    settle || bad "the page never settled"
}
close_viewer() {
    key Escape
    wait_vis "$LIST_PROMPT" || bad "ESC did not go back to the list"
    key Escape
    wait_gone "$LIST_PROMPT" || bad "ESC did not close the list"
    settle || bad "frame never settled after the viewer"
}

start_provider openai 100 30 || finish
settle || bad "startup never settled"

# ------------------------------------------------------------------ 23.1: the startup notice
# Nothing declared the login: the 401 the bare handshake got is what makes this "not logged in"
# rather than "failed: connect failed: …" — and the line names the CLI, not a slash command.
wait_all "$NOTICE" || bad "the not-logged-in notice never appeared"
check_once "the notice is one line, naming the CLI" "$NOTICE"
check "…and not the generic failure shape" "$(count_all 'MCP nb failed')" 0
check_frame_intact "after the notice" 100
[ -e "$TOKEN_FILE" ] && bad "a token file exists before any login"

# ------------------------------------------------------------------ 23.2: /tools, not logged in
open_list
check "the server's row says it needs a login, in the CLI's words" "$(count_vis 'needs login: run iota mcp login nb')" 1
open_page
check "…and so does its page" "$(count_vis 'status        needs login: run iota mcp login nb')" 1
close_viewer

# ------------------------------------------------------------------ 23.3: iota mcp login nb, beside the chat
# The CLI waits for the callback, so it runs detached and is given 30 s; a login that hangs is
# killed and reported rather than left to its own five-minute deadline.
iota_mcp login nb &
cli_pid=$!
cli_done() { ! kill -0 "$cli_pid" 2>/dev/null; }
if _poll_until 300 cli_done; then
    wait "$cli_pid"
    check "the login exits 0" "$?" 0
else
    kill "$cli_pid" 2>/dev/null
    bad "the login did not finish within 30 s"
    cat "$CLI_OUT"
fi
check_cli "the URL is printed" 'Open this URL to log in:'
check_cli "…and it is the mock's" "http://127.0.0.1:$IOTA_OAUTH_PORT/authorize?"
check_cli "the login is announced" 'Logged in to nb; the token expires in'
if [ -f "$TOKEN_FILE" ]; then ok "the token file exists ($TOKEN_FILE)"; else bad "no token file at $TOKEN_FILE"; fi
# The mode bits off `ls -l`, which GNU and BSD print alike. (`stat` does not: BSD's `stat -f '%Lp'` is, on
# GNU coreutils, a request for FILESYSTEM status whose unknown directive prints `?p` and exits 0 — so a
# `stat -f … || stat -c …` chain never reached the GNU form and called a 0600 file "not mode 600" on Ubuntu.)
mode_bits="$(ls -ld -- "$TOKEN_FILE" | cut -c2-10)"
check "the token file is mode 600 (owner read/write, nothing else)" "$mode_bits" "rw-------"
# The pane was not part of it: nothing new in the history, the frame as it was.
settle || bad "frame never settled while the CLI logged in"
check "the chat saw nothing of the login" "$(count_all 'Logged in to nb')" 0
check_frame_intact "after the login" 100

# ------------------------------------------------------------------ 23.4: logged in — reconnect, then a new chat
# The chat already running picks the new token up when the server is reconnected from its page (X-79):
# the OAuth transport this time, the page `connected` with the server's tool, one transcript line.
open_list
open_page
key Enter
wait_vis 'Reconnected · 1 tool' || bad "the page never said the reconnect worked"
check "the page reads connected" "$(count_vis 'status        connected')" 1
check "…with its tool" "$(count_vis '  echo')" 1
close_viewer
check "the transcript says it was reconnected" "$(count_all 'MCP nb reconnected · 1 tool')" 1
# The next chat connects through the OAuth transport from the start — no notice, connected at once.
start_provider openai 100 30 || finish
settle || bad "the second startup never settled"
open_list
open_page
wait_vis 'status        connected' || bad "the page never showed the server connected"
check "the server is connected" "$(count_vis 'status        connected')" 1
check "…with its tool" "$(count_vis '  echo')" 1
close_viewer
check "no not-logged-in notice this time" "$(count_all 'not logged in')" 0
check_frame_intact "after the second startup" 100

# ------------------------------------------------------------------ 23.5: iota mcp logout nb
iota_mcp logout nb
check "the logout exits 0" "$?" 0
check_cli "the logout says what it forgot" "Logged out of nb (forgot $TOKEN_FILE)"
if [ -e "$TOKEN_FILE" ]; then bad "the token file survived the logout"; else ok "the token file is gone"; fi
check_frame_intact "after the logout" 100

finish
