#!/usr/bin/env bash
# L4 scenario 53 (docs/design/grants.md §5, X-112) — an MCP tool's grant, through the dialog and through `/allow`.
#
# The decisions are pinned in-process (`repl::turn::tools` tests: the dialog's items and what each grants;
# `repl::commands::allow` tests: the parsing and the wording); what only a real terminal shows is the user's
# side of it, against a real stdio server:
#
#   53.1 the model calls the server's tool — the approval dialog opens with its FOUR items: allow once, always
#        allow `srv:echo` in this session, in this project, deny (the agent is `mode: agent`, run in a project
#        of its own: only a run with a project is offered the project level, grants.md §4.2);
#   53.2 Esc denies it: the call does not run, and the round closes on the refusal;
#   53.3 `/allow srv:echo` grants it for this session, saying so; the bare `/allow`'s Grants tab lists it;
#   53.4 the same call again runs without a dialog and its output closes the round.
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
mcp_servers:
  srv:
    command: python3
    args: [\"$STUB\"]"
export CONFIG_BODY

CALL='call:mcp__srv__echo'
RAN='ran: echoed'

# The project is `proj/`: the scratch directory itself holds the scenario's home, and a root that is an ancestor
# of the home is never a project.
mkdir -p "$SCEN_TMP/proj"
_launch "$SCEN_TMP/proj" openai fake 100 30 || finish
settle || miss "startup never settled"

# ------------------------------------------------------------------ 53.1: the dialog's four items
type_ "$CALL"
key Enter
wait_vis 'allow?' || miss "the approval dialog never opened for the MCP tool"
wait_vis 'Deny' || miss "the dialog's items never showed"
check "allow once" "$(count_vis 'Allow once')" 1
check "always, this session" "$(count_vis 'Always allow srv:echo in this session')" 1
check "always, this project" "$(count_vis 'Always allow srv:echo in this project')" 1
check "deny" "$(count_vis 'Deny')" 1

# ------------------------------------------------------------------ 53.2: denied, the call does not run
key Escape
wait_gone 'allow?' || miss "the dialog never closed on Esc"
settle || miss "frame never settled after the denial"
check "the denied call did not run" "$(count_all "$RAN")" 0

# ------------------------------------------------------------------ 53.3: /allow grants it, and lists it
type_ '/allow srv:echo'
key Enter
wait_all '✓ srv:echo runs without asking in this session' || miss "/allow never said the grant"
type_ '/allow'
key Enter
wait_vis 'this session  srv:echo' || miss "the bare /allow's Grants tab never listed the grant"
key Escape
wait_gone 'this session  srv:echo' || miss "the surface never closed on Esc"
settle || miss "frame never settled after /allow"

# ------------------------------------------------------------------ 53.4: the call runs, unasked
asked="$(count_all 'allow?')"
type_ "$CALL"
key Enter
wait_all "$RAN" || miss "the granted call never ran (or its output never closed the round)"
settle || miss "frame never settled after the granted call"
check "no dialog for the granted call" "$(count_all 'allow?')" "$asked"
check_once "the call ran once" "$RAN"
check_frame_intact "after the granted call" 100

finish
