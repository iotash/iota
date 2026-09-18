#!/usr/bin/env bash
# L4 scenario 23 (brain page `mcp-cli-and-oauth`) — the OAuth round trip from inside the chat.
#
# The chain itself is pinned in-process (`tests/mcp/oauth.rs`) and through the CLI
# (`tests/cmd/mcp.rs`); what was left to a human is what it LOOKS like in the REPL:
#
#   23.1 the startup notice — an `auth: oauth` server with no token is ONE red line naming
#        `/mcp login nb`, and the chat is usable (the other server, the model, the composer);
#   23.2 the `/mcp` panel — the server reads `disconnected` with `auth: oauth (not logged in)`;
#   23.3 `/mcp login nb` — the URL lands in the transcript, `$BROWSER` (a script that follows
#        the redirect with curl) brings the callback back, the token file appears, the server
#        reconnects and its tool count is announced;
#   23.4 the panel again — `connected`, `auth: oauth (logged in)`;
#   23.5 `/mcp logout nb` — the file goes, the server is `disconnected` / `not logged in` again.
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
    models: [m, \"mock:*\"]
mcp_servers:
  nb:
    url: http://127.0.0.1:$IOTA_OAUTH_PORT/mcp
    auth: oauth"
export CONFIG_BODY

# The browser stand-in: follows the authorization URL through the 302 to iota's loopback
# callback, detached so the pane never waits on it.
cat >"$SCEN_TMP/browser.sh" <<'EOS'
#!/bin/sh
curl -sL "$1" >/dev/null 2>&1 &
EOS
export BROWSER="sh $SCEN_TMP/browser.sh"
TOKEN_FILE="$SCEN_HOME/.iota/mcp/auth/nb.json"

start_provider openai 100 30 || finish
settle || bad "startup never settled"

# ------------------------------------------------------------------ 23.1: the startup notice
wait_all '⚠ MCP nb not logged in: /mcp login nb' || bad "the not-logged-in notice never appeared"
check_once "the notice is one line" '⚠ MCP nb not logged in: /mcp login nb'
check "…and not the generic failure shape" "$(count_all 'MCP nb failed')" 0
check_frame_intact "after the notice" 100
[ -e "$TOKEN_FILE" ] && bad "a token file exists before any login"

# ------------------------------------------------------------------ 23.2: the panel, not logged in
type_ '/mcp'
key Enter
wait_vis 'auth: oauth (not logged in)' || bad "the panel never showed the login state"
check "the server is disconnected" "$(count_vis 'nb  [disconnected]')" 1
check "the panel names the way in" "$(count_vis '/mcp login <name>')" 1
key Escape
wait_gone 'auth: oauth (not logged in)' || bad "ESC did not close the panel"
settle || bad "frame never settled after the panel"

# ------------------------------------------------------------------ 23.3: /mcp login nb
type_ '/mcp login nb'
key Enter
wait_all 'Open this URL to log in:' || bad "the URL never landed in the transcript"
wait_all "http://127.0.0.1:$IOTA_OAUTH_PORT/authorize?" || bad "the authorization URL is not the mock's"
wait_all 'Logged in to nb; the token expires in' || bad "the login never completed"
wait_all 'MCP nb: connected (1 tools)' || bad "the server never reconnected"
settle || bad "frame never settled after the login"
check_once "the login is announced once" 'Logged in to nb; the token expires in'
check_once "the reconnect is announced once" 'MCP nb: connected (1 tools)'
if [ -f "$TOKEN_FILE" ]; then ok "the token file exists ($TOKEN_FILE)"; else bad "no token file at $TOKEN_FILE"; fi
# The mode bits off `ls -l`, which GNU and BSD print alike. (`stat` does not: BSD's `stat -f '%Lp'` is, on
# GNU coreutils, a request for FILESYSTEM status whose unknown directive prints `?p` and exits 0 — so a
# `stat -f … || stat -c …` chain never reached the GNU form and called a 0600 file "not mode 600" on Ubuntu.)
mode_bits="$(ls -ld -- "$TOKEN_FILE" | cut -c2-10)"
check "the token file is mode 600 (owner read/write, nothing else)" "$mode_bits" "rw-------"
check_frame_intact "after the login" 100

# ------------------------------------------------------------------ 23.4: the panel, logged in
type_ '/mcp'
key Enter
wait_vis 'auth: oauth (logged in)' || bad "the panel never showed the logged-in state"
check "the server is connected" "$(count_vis 'nb  [connected]')" 1
check "…with its tool" "$(count_vis 'tools (1): echo')" 1
key Escape
wait_gone 'auth: oauth (logged in)' || bad "ESC did not close the panel"
settle || bad "frame never settled after the second panel"

# ------------------------------------------------------------------ 23.5: /mcp logout nb
type_ '/mcp logout nb'
key Enter
wait_all 'logged out of nb (forgot' || bad "the logout never completed"
settle || bad "frame never settled after the logout"
[ -e "$TOKEN_FILE" ] && bad "the token file survived the logout"
type_ '/mcp'
key Enter
wait_vis 'auth: oauth (not logged in)' || bad "the panel never showed the logged-out state"
check "the server is disconnected again" "$(count_vis 'nb  [disconnected]')" 1
check "…with the way back in" "$(count_vis 'error: not logged in: run iota mcp login nb')" 1
key Escape
wait_gone 'auth: oauth (not logged in)' || bad "ESC did not close the panel"
check_frame_intact "after the logout" 100

finish
