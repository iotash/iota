#!/usr/bin/env bash
# L4 scenario 57 (docs/design/tools-command.md §4, X-79) — Enter on a failed server's `/tools` page
# reconnects it IN PLACE, and the dialog does not jump.
#
# The rows are pinned in-process (`repl::commands::tools` tests: a retried server's page keeps its
# last failure and its row count; `ui::surface` tests: an enter action keeps the surface open); what
# only a real terminal shows is that nothing on the screen moves:
#
#   57.1 a stdio server that fails two seconds into its start — its page reads `failed`, with `why`;
#   57.2 Enter on the page: the prompt says `Reconnecting…` while the retry runs, the status row says
#        `connecting…`, the block reads `why (last attempt)` — and the prompt row, the status row and
#        the block's heading sit on the SAME pane rows as before Enter (the page was not closed and
#        reopened, and it did not shrink); when the retry fails again the rows are still where they were;
#   57.3 scrolled to its foot (a short pane, the page scrolls), Enter keeps the scroll: the foot stays
#        on its row and the top of the page stays off the screen through the retry.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

CONFIG_BODY="providers:
  mock: {type: openai, key: test, url: \"http://127.0.0.1:$IOTA_PORT\"}
models:
  m: mock:fake
agents:
  default:
    model: m
    choices: [m, \"mock:*\"]
mcp_servers:
  bad:
    command: sh
    args: [\"-c\", \"sleep 2; echo boom one >&2; echo boom two >&2; exit 1\"]"
export CONFIG_BODY

LIST_PROMPT='Enter: open · Esc: close'
PAGE_PROMPT='Enter: reconnect · Esc: back'
RETRY_PROMPT='Reconnecting… · Esc: back'
FAILED_PROMPT='Enter: try again · Esc: back'

# open_page — `/tools`, Down to `bad` (the row under `built in`), Enter to its page.
open_page() {
    type_ '/tools'
    key Enter
    wait_vis "$LIST_PROMPT" || { miss "the /tools list never opened"; return 1; }
    key Down
    key Enter
    wait_vis 'Esc: back' || { miss "Enter did not open bad's page"; return 1; }
    settle || miss "the page never settled"
}
close_page() {
    key Escape
    wait_vis "$LIST_PROMPT" || miss "ESC did not go back to the list"
    key Escape
    wait_gone "$LIST_PROMPT" || miss "ESC did not close the list"
    settle || miss "frame never settled after /tools"
}
# row_is <description> <fixed string> <row> — the string's first pane row is <row>.
row_is() { check "$1" "$(row_of "$2")" "$3"; }
# line_at <whole line> — the first pane row that is exactly that line (trailing blanks aside).
line_at() { cap | sed 's/ *$//' | grep -nxF -- "$1" | head -1 | cut -d: -f1; }
# line_is <description> <whole line> <row> — that line is pane row <row>.
line_is() { check "$1" "$(line_at "$2")" "$3"; }

# ------------------------------------------------------------------ 57.1: a server that fails
start_provider openai 100 40 || finish
wait_all 'MCP bad' || miss "the failure notice never appeared"
settle || miss "startup never settled"
open_page
check "the page says the server failed" "$(count_vis 'status        failed:')" 1
check "…and why" "$(line_at 'why' | grep -c .)" 1
top_row="$(line_at ' bad')"
prompt_row="$(row_of "$PAGE_PROMPT")"
status_row="$(row_of 'status        failed:')"
why_row="$(line_at 'why')"
tools_row="$(line_at 'tools (0)')"
[ -n "$top_row" ] && [ -n "$prompt_row" ] && [ -n "$status_row" ] && [ -n "$why_row" ] && [ -n "$tools_row" ] ||
    miss "the page's rows were not found"

# ------------------------------------------------------------------ 57.2: Enter retries in place
key Enter
wait_vis "$RETRY_PROMPT" || miss "Enter never started the reconnect"
wait_vis 'status        connecting…' || miss "the status row never said connecting…"
line_is "the page's top row stays put while it reconnects" ' bad' "$top_row"
row_is "…and the prompt row" "$RETRY_PROMPT" "$prompt_row"
row_is "…and the status row" 'status        connecting…' "$status_row"
line_is "…and the why block, now the last attempt's" 'why (last attempt)' "$why_row"
line_is "…and the tool list" 'tools (0)' "$tools_row"
check "the status row names the last failure" "$(count_vis '· last: failed:')" 1
wait_vis "$FAILED_PROMPT" || miss "the retry never landed"
line_is "the failed retry leaves the top row where it was" ' bad' "$top_row"
row_is "…and the prompt row" "$FAILED_PROMPT" "$prompt_row"
row_is "…and the status row" 'status        failed:' "$status_row"
line_is "…and why, its own again" 'why' "$why_row"
line_is "…and the tool list" 'tools (0)' "$tools_row"
check "the landed failure takes the block back" "$(count_vis 'why (last attempt)')" 0
close_page

# ------------------------------------------------------------------ 57.3: the scroll stays
start_provider openai 100 16 || finish
wait_all 'MCP bad' || miss "the failure notice never appeared (short pane)"
settle || miss "startup never settled (short pane)"
open_page
key G
settle || miss "the page never settled at its foot"
foot_row="$(line_at '  (none)')"
[ -n "$foot_row" ] || miss "the page's foot is not on the screen"
check "the page's top is scrolled off" "$(count_vis 'status        ')" 0
key Enter
wait_vis "$RETRY_PROMPT" || miss "Enter never started the reconnect (short pane)"
line_is "the foot stays on its row while it reconnects" '  (none)' "$foot_row"
check "…and the top stays scrolled off" "$(count_vis 'status        ')" 0
wait_vis "$FAILED_PROMPT" || miss "the retry never landed (short pane)"
line_is "the foot is still on its row after the retry" '  (none)' "$foot_row"
close_page
finish
