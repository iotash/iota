#!/usr/bin/env bash
# L4 scenario 24 (brain page `harness-prompt`, DIVERGENCES X-44) — the one call the sandbox lets out.
#
# The rule is pinned in-process (`shell::selfcall`, `tests/tool/shell.rs`: a sandboxed set spawns a call
# whose first word is the running binary without the sandbox, and only that call); what was left to a human
# is what the user is ASKED. The agent enables `shell` with its defaults — the sandbox on, no `auto_run` —
# which is the one configuration that never asks about anything… until iota itself is the command:
#
#   24.1 `iota mcp list` from the model — the approval prompt opens (a sandboxed set asks about the call
#        that leaves the sandbox), its title carrying `(outside the sandbox)`;
#   24.2 allowed once, the call runs and its output closes the round — `iota mcp list` read the pane's own
#        config and found nothing — and the settled call header carries the same mark;
#   24.3 the control: `echo hi | iota mcp list` is an ordinary sandboxed call — no prompt (the round
#        completes with nobody at the prompt to answer one), no mark.
#
# `iota` resolves through `PATH`: a symlink in the scenario's own `bin`, put ahead of `PATH` for the pane,
# so the header reads `iota mcp list` rather than a truncated absolute path. The binary the symlink resolves
# to is the pane's own, which is what the rule compares.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

# The rule needs a sandbox to leave. Without one — Linux without bubblewrap, a nested sandbox — the
# scenario has nothing to show, and says so; under IOTA_SANDBOX_REQUIRED (ci.sh) that is a failure.
sandbox_probe() {
    case "$(uname -s)" in
        Darwin) [ -x /usr/bin/sandbox-exec ] && /usr/bin/sandbox-exec -p '(version 1)(allow default)' /bin/echo probe >/dev/null 2>&1 ;;
        Linux) command -v bwrap >/dev/null 2>&1 && bwrap --ro-bind / / --dev-bind /dev /dev --proc /proc --die-with-parent -- /bin/echo probe >/dev/null 2>&1 ;;
        *) return 1 ;;
    esac
}
if ! sandbox_probe; then
    if [ -n "${IOTA_SANDBOX_REQUIRED:-}" ]; then
        bad "IOTA_SANDBOX_REQUIRED is set and no OS sandbox runs here"
    else
        echo "SKIP: no OS sandbox runs here (sandbox-exec on macOS, bwrap on Linux)"
    fi
    finish
fi

mkdir -p "$SCEN_TMP/bin"
ln -sf "$IOTA_BIN" "$SCEN_TMP/bin/iota"
export PATH="$SCEN_TMP/bin:$PATH"

CONFIG_BODY="providers:
  mock: {type: openai, key: test, url: \"http://127.0.0.1:$IOTA_PORT\"}
models:
  m: mock:fake
agents:
  default:
    models: [m, \"mock:*\"]
    tools: {shell: }"
export CONFIG_BODY

MARK='(outside the sandbox)'
EMPTY='ran: No MCP servers configured'

start_provider openai 100 24 || finish
settle || bad "startup never settled"

# ------------------------------------------------------------------ 24.1: the prompt, and the mark
type_ 'runfg:iota mcp list'
key Enter
wait_vis 'allow?' || bad "the approval prompt never opened for the call that leaves the sandbox"
check "the prompt's title carries the mark" "$(count_vis "shell iota mcp list $MARK wants to modify files")" 1

# ------------------------------------------------------------------ 24.2: allowed once, it runs
key Enter
wait_all "$EMPTY" || bad "the allowed call never ran (or its output never closed the round)"
settle || bad "frame never settled after the allowed call"
check_once "the call ran once" "$EMPTY"
check "the settled call header carries the mark" "$(count_all "[shell iota mcp list $MARK]")" 1
check "the prompt is gone" "$(count_vis 'allow?')" 0
check_frame_intact "after the allowed call" 100

# ------------------------------------------------------------------ 24.3: the control — iota in a pipe stays in
type_ 'runfg:echo hi | iota mcp list'
key Enter
# Nobody answers a prompt here: the round completing at all is the proof that none opened.
wait_all_more "$EMPTY" 1 || bad "the piped call never ran — was it asked about?"
settle || bad "frame never settled after the piped call"
check "the piped call's header is unmarked" "$(count_all '[shell echo hi | iota mcp list]')" 1
check "…and carries no mark anywhere" "$(count_all "echo hi | iota mcp list $MARK")" 0
check_frame_intact "after the piped call" 100

finish
