#!/usr/bin/env bash
# L4 scenario 55 (docs/design/allow-completion.md §4.3, X-111) — the `/allow ` completion list, walked key by key
# against the stdio server of scenario 53 (one tool, `echo`), configured here as `github`.
#
# The rows are pinned in-process (`repl::commands::allow` tests: every row, its `after`, the cleaning;
# `tests/repl/commands.rs`: the rows arriving when the server connects); what only a real terminal shows is the
# walk itself, through the real composer and its list:
#
#   55.1 `/allow ` lists the built-ins that ask (`shell`: the agent has the toolset, not in its `allow:`) and the
#        server, `github:` and its count (once it has connected);
#   55.2 `g` leaves `github:` alone; Tab writes `/allow github:`;
#   55.3 Enter on that branch row submits nothing: the list shows level 2, `*` and the server's tool;
#   55.4 `e`, Tab, Space, Tab, Enter — `/allow github:echo project` — grants the tool for the project;
#   55.5 the model's call of the tool runs without a dialog.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

command -v python3 >/dev/null 2>&1 || { bad "python3 not found (the stub MCP server needs it)"; finish; }
STUB="$(dirname "$TMUX_LIB")/stubs/echo-mcp.py"

CONFIG_BODY="providers:
  mock: {type: openai, key: test, url: \"http://127.0.0.1:$IOTA_PORT\"}
models:
  m: mock:fake
agents:
  default:
    mode: agent
    model: m
    choices: [m, \"mock:*\"]
    tools: {shell: }
mcp_servers:
  github:
    command: python3
    args: [\"$STUB\"]"
export CONFIG_BODY

CALL='call:mcp__github__echo'
RAN='ran: echoed'

# The list holds / no longer holds an item with this label (highlighted or not), polled.
_listed() { [ "$(count_list "$1")" -ge 1 ]; }
_unlisted() { [ "$(count_list "$1")" = 0 ]; }
wait_listed() { _poll_until 120 _listed "$1"; }
wait_unlisted() { _poll_until 120 _unlisted "$1"; }

# The project is `proj/`: a root that is an ancestor of the scenario's home is never a project.
mkdir -p "$SCEN_TMP/proj"
_launch "$SCEN_TMP/proj" openai fake 100 30 || finish
settle || miss "startup never settled"

# ------------------------------------------------------------------ 55.1: level 1
type_ '/allow '
wait_listed 'github:' || miss "the server's row never arrived in the /allow list"
wait_vis 'MCP server · 1 tool' || miss "the server's row never said its count"
check "a built-in that asks is listed" "$(count_list shell)" 1
check "no tool row on level 1" "$(count_list echo)" 0

# ------------------------------------------------------------------ 55.2: g, Tab
type_ 'g'
wait_unlisted shell || miss "g never hid the built-ins"
check "g leaves the server alone" "$(count_list 'github:')" 1
key Tab
wait_draft '/allow github:' || miss "Tab never wrote the server's prefix"

# ------------------------------------------------------------------ 55.3: Enter on the branch descends
key Enter
wait_listed '\*' || miss "Enter on github: never showed its * row"
wait_listed echo || miss "Enter on github: never showed its tool"
wait_vis 'every tool of github, those it exposes later included' || miss "the * row's description"
wait_vis 'Answers echoed.' || miss "the tool row never showed the server's description"
check "Enter on the branch submitted nothing" "$(count_all 'runs without asking')" 0
wait_draft '/allow github:' || miss "the line did not stay after Enter on the branch"

# ------------------------------------------------------------------ 55.4: e Tab ␣ Tab Enter
type_ 'e'
key Tab
wait_draft '/allow github:echo' || miss "Tab never wrote the tool's line"
type_ ' '
wait_listed project || miss "the scope row never showed behind /allow github:echo "
key Tab
wait_draft '/allow github:echo project' || miss "Tab never wrote the scope"
key Enter
wait_all "✓ github:echo runs without asking in this project (" || miss "/allow never said the project grant"
settle || miss "frame never settled after the grant"

# ------------------------------------------------------------------ 55.5: the call runs, unasked
type_ "$CALL"
key Enter
wait_all "$RAN" || miss "the granted call never ran (or its output never closed the round)"
settle || miss "frame never settled after the granted call"
check "no dialog for the granted call" "$(count_all 'allow?')" 0
check_once "the call ran once" "$RAN"
check_frame_intact "after the granted call" 100

finish
