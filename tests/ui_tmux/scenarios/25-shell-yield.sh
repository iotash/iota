#!/usr/bin/env bash
# L4 scenario 25 (docs/TUI-VERIFY.md §8.6–8.9, DIVERGENCES X-47, X-50) — a foreground `shell` call that runs
# past its window lets go, and what the chat shows while the job it became is running.
#
# The yield itself is pinned in-process (`tests/tool/{jobs,shell}.rs`); what was left to a human is the
# shape on the terminal. The mock's `runfg:<cmd>` script is one foreground `shell` call, and the window
# is shortened to two seconds through the `IOTA_SHELL_YIELD` hook — read once at the binary edge, so the
# pane's iota waits two seconds where a user's waits twenty:
#
#   8.6 the yield       — the classic block's receipt row `⎿ still running after 2s → background job b1`
#                         lands under the call, and the model's reply closes the round with
#                         `ran: Still running after 2s as background job b1 (pid …` — the text it was
#                         told; the call never held the turn for the whole command; the committed
#                         header is painted in segments (X-83) — read with `capture-pane -e`, its `[`
#                         wears faint and its name cyan, two different SGR states;
#   8.7 /jobs           — while the job runs, `/j` completes to `/jobs`, and the command opens the Jobs
#                         tab with one row per job; Enter opens the job's page (the command in full,
#                         the clock and its start, the pid, the log path, the log's last lines), Esc
#                         returns to the list, Esc closes it; with a second job started (`run:`, a
#                         `background: true` call) the list is LIVE — its clock walks between two
#                         captures a second apart — and the Kill tab (Tab, ↓, Space, Enter) kills the
#                         checked job: the surface closes, the notice `finished: killed` lands and runs
#                         a turn, and the list reopened has one row fewer; after the first job's notice
#                         the row is gone and `/j` completes to nothing;
#   8.8 the status row  — while the job runs the row ends in `job b1 <command> Ns`, and the seconds walk
#                         between two captures; after the notice the segment is gone;
#   8.9 the host state  — back at the prompt with the job running, the terminal's progress indicator
#                         (OSC 9;4, tapped with `pipe-pane` as scenario 14 does) is still busy — 3 —
#                         and nothing clears it until the last job's notice turn ends, when it is 0.
#                         The tap starts after startup: an idle start emits no OSC 9;4 at all
#                         (scenario 14), so every state in it is this scenario's.
#
# The agent runs `shell` with the sandbox off and `auto_run: true`, so no approval prompt sits between
# the call and the yield.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

# The window: the tmux server is started by this script and inherits its environment.
export IOTA_SHELL_YIELD=2

CONFIG_BODY="providers:
  mock: {type: openai, key: test, url: \"http://127.0.0.1:$IOTA_PORT\"}
models:
  m: mock:fake
agents:
  default:
    model: m
    choices: [m, \"mock:*\"]
    tools: {shell: {sandbox: off, auto_run: true}}"
export CONFIG_BODY

# Twenty-five seconds: the page is opened and closed, a second job started and killed, and the list
# reopened while this job still runs, with room on a slow runner.
CMD='sleep 25; echo l4yield'
# The second job, backgrounded outright: longer than the first, so the first's notice is the last thing.
CMD2='sleep 40; echo second'
RECEIPT='still running after 2s → background job b1'

# A job's clock as the list shows it: `text::elapsed`, whose first second reads `<1s` — b2 is listed
# moments after it starts.
CLK='(<1|[0-9]+)s'
# The job segment's clock, off the status row ("" if the segment is absent).
job_clock() { bottom_zone | grep -oE 'job b1 .* [0-9]+s$' | grep -oE '[0-9]+s$'; }
# The clock on job `$1`'s row of the open list ("" if there is no such row) — the id, the clock, the
# command, two spaces apart; on the Kill tab the row starts with its checkbox.
row_clock() { cap | grep -oE "$1 +[0-9]+s  " | head -1 | grep -oE '[0-9]+s'; }
# The candidates row shows a command without the slash already typed: `⎿ jobs`.
has_jobs_candidate() { [ "$(count_composer '⎿ jobs')" -gt 0 ]; }
# The last OSC 9;4 state the pane was sent ("" if none yet).
last_progress() { LC_ALL=C grep -aoE '\]9;4;[0-9]' "$RAW" 2>/dev/null | tail -1 | cut -c6; }
progress_is_idle() { [ "$(last_progress)" = 0 ]; }
# label_sgr <name> — the SGR runs in front of the first committed `[<name> ` label's `[` and its name,
# as `<bracket>|<name>`, off the rendered grid with its SGR state (`capture-pane -e`; tmux re-spells
# the sequences — cyan comes back as `38;5;6` — so the runs are compared, not pinned byte for byte; the name's own reset sits between
# it and the blank).
label_sgr() {
    tm capture-pane -e -pt s -S -400 2>/dev/null | NAME="$1" perl -ne \
        'if (/((?:\e\[[0-9;]*m)*)\[((?:\e\[[0-9;]*m)*)\Q$ENV{NAME}\E(?:\e\[[0-9;]*m)* /) { print "$1|$2\n"; exit }'
}

start_provider openai 100 24 || finish
settle || miss "startup never settled"
pipe_raw

# ------------------------------------------------------------------ 8.6: the yield
type_ "runfg:$CMD"
key Enter
wait_all "$RECEIPT" || miss "the receipt row never landed"
wait_all 'ran: Still running after 2s as background job b1 (pid' || miss "the model never got the receipt"
settle || miss "frame never settled after the yield"
check_once "the call is echoed once" "❯ runfg:$CMD"
check_once "the call's header is committed once" "[shell $CMD]"
sgr="$(label_sgr shell)"
bracket_sgr="${sgr%%|*}"
name_sgr="${sgr#*|}"
if [ -n "$bracket_sgr" ] && [ -n "$name_sgr" ] && [ "$bracket_sgr" != "$name_sgr" ] \
    && printf '%s' "$bracket_sgr" | LC_ALL=C grep -qE '\[([0-9;]*;)?2(;[0-9;]*)?m' \
    && printf '%s' "$name_sgr" | LC_ALL=C grep -qE '\[([0-9;]*;)?(36|38;5;6)(;[0-9;]*)?m'; then
    ok "the header's bracket (faint) and name (cyan) wear different SGR"
else
    bad "the header's bracket and name are not painted apart: $(printf '%q' "$sgr")"
fi
check "the receipt row is under the header" "$(count_all "⎿ $RECEIPT")" 1
check "the model's receipt is the reply's first line" "$(count_all 'ran: Still running after 2s as background job b1 (pid')" 1
check "…and no notice yet: the job is still running" "$(count_all '[background job b1 finished')" 0
check_frame_intact "after the yield" 100

# ------------------------------------------------------------------ 8.9: the host state, while it runs
check "back at the prompt, the running job keeps the progress indicator busy (OSC 9;4;3)" "$(last_progress)" 3
check_raw "…and it was never cleared (no OSC 9;4;0)" ']9;4;0' no

# ------------------------------------------------------------------ 8.8: the status row, while it runs
first="$(job_clock)"
if [ -n "$first" ]; then ok "the status row ends in the job's clock ($first)"; else bad "no job segment on the status row: $(bottom_zone)"; fi
check "the segment names the job and its command" "$(bottom_zone | grep -cF "job b1 $CMD")" 1
# The clock moving is the event: polled for, up to three of its one-second ticks (JOB_TICK). A
# single read 1.2 s later called a repaint that landed a little late on a loaded runner "not moving".
_moved() { second="$($1)"; [ -n "$second" ] && [ "$second" != "$first" ]; }
second=""
if [ -n "$first" ] && _poll_until 30 _moved job_clock; then
    ok "the clock walks ($first → $second)"
else
    miss "the clock did not move ($first → $second)"
fi

# ------------------------------------------------------------------ 8.7: /jobs, while it runs
type_ '/j'
_poll_until 30 has_jobs_candidate || miss "/j did not complete to /jobs while the job runs"
check "the completion row offers /jobs" "$(count_composer '⎿ jobs')" 1
key C-u
wait_gone '❯ /j' || miss "Ctrl+U did not clear the composer"
type_ '/jobs'
key Enter
wait_vis '1 job running' || miss "the Jobs list never opened"
check "the list has the job's row" "$(cap | grep -cE "b1 +[0-9]+s +$CMD")" 1
check "the row carries no log path" "$(cap | grep -c 'iota-jobs/')" 0
# Enter: the page — the command in full, the clock counted back to a wall-clock start, the pid, the
# log (its directory fits one row; the path may wrap, the page wraps), the tail (nothing yet).
key Enter
wait_vis "command:  $CMD" || miss "Enter did not open the job's page"
check "the page has the clock and its start" "$(cap | grep -cE 'running:  [0-9]+s \(started [0-9]{2}:[0-9]{2}:[0-9]{2}\)')" 1
check "the page has the pid" "$(cap | grep -cE 'pid:      [0-9]+')" 1
check "the page names the log" "$(cap | grep -cE 'output:   .*iota-jobs/')" 1
check "the page has the tail's rule" "$(cap | grep -cF 'last 20 lines')" 1
check "…and no output yet" "$(cap | grep -cF '(no output yet)')" 1
key Escape
wait_vis '1 job running' || miss "ESC did not return to the list"
wait_gone "command:  $CMD" || miss "the page is still up beside the list"
check "the list is back with the job's row" "$(cap | grep -cE "b1 +[0-9]+s +$CMD")" 1
key Escape
wait_gone '1 job running' || miss "ESC did not close the list"
settle || miss "frame never settled after the list"
check "/jobs opened a list, not a turn" "$(count_all '❯ /jobs')" 0
check_frame_intact "after the list" 100

# ------------------------------------------------------------------ 8.7, live: a second job, the clock walks, the Kill tab
type_ "run:$CMD2"
key Enter
wait_all 'ran: Started background job b2 (pid' || miss "the second job never started"
settle || miss "frame never settled after the second job"
check "two jobs: the status row counts them" "$(bottom_zone | grep -cE '· 2 jobs [0-9]+s')" 1
type_ '/jobs'
key Enter
wait_vis '2 jobs running' || miss "the Jobs tab never opened with two jobs"
check "the surface has the Kill tab" "$(cap | grep -cF 'Kill')" 1
# A row is `▸ b1  12s  <command>` (the cursor's) or `  b1  12s  <command>`; on the Kill tab `[ ] ` sits
# between the marker and the id. Nothing else on the pane has an id, a clock and two spaces in a row.
check "both rows are there" "$(cap | grep -cE "b[12] +$CLK  ")" 2
first="$(row_clock b1)"
_row_clock_b1() { row_clock b1; }
second=""
if [ -n "$first" ] && _poll_until 30 _moved _row_clock_b1; then
    ok "the row's clock walks while the list is open ($first → $second)"
else
    miss "the row's clock did not move ($first → $second)"
fi
key Tab
wait_vis '[ ] ' || miss "Tab did not open the Kill tab"
check "the Kill tab has both rows, unchecked" "$(cap | grep -cE "\\[ \\] b[12] +$CLK  ")" 2
check "…under the same count" "$(count_vis '2 jobs running')" 1
key Down
key Space
_kill_row_checked() { cap | grep -qE "\\[x\\] b2 +$CLK  "; }
_poll_until 30 _kill_row_checked || miss "Space did not check b2: $(cap | grep -E '\[.\] b')"
check "only b2 is checked" "$(cap | grep -cF '[x] ')" 1
key Enter
wait_gone '2 jobs running' || miss "Enter on the Kill tab did not close the surface"
wait_all "[background job b2 finished: killed] $CMD2" || miss "the killed job's notice never arrived"
wait_all 'echo: [background job b2 finished: killed' || miss "the killed notice never became a turn"
settle || miss "frame never settled after the kill"
check "the killed notice landed once" "$(capall | grep -c '^\[background job b2 finished: killed\]' | tr -d ' ')" 1
check "the first job is untouched" "$(count_all '[background job b1 finished')" 0
check "the status row is back to one job" "$(bottom_zone | grep -cF 'job b1')" 1
type_ '/jobs'
key Enter
wait_vis '1 job running' || miss "the list did not reopen with one job"
check "b2's row is gone" "$(cap | grep -cE "b2 +$CLK  ")" 0
check "b1's row is still there" "$(cap | grep -cE "b1 +[0-9]+s +$CMD")" 1
key Escape
wait_gone '1 job running' || miss "ESC did not close the reopened list"
settle || miss "frame never settled after the reopened list"
check "no /jobs went to the model, and the kill printed nothing of its own" "$(count_all '❯ /jobs')" 0
check_frame_intact "after the kill" 100
check "the killed job's notice turn ended with b1 still running: still busy (OSC 9;4;3)" "$(last_progress)" 3
check_raw "…and never cleared on the way (no OSC 9;4;0)" ']9;4;0' no

# ------------------------------------------------------------------ the notice, and the two go away
wait_all '[background job b1 finished: exit 0 after' || miss "the job's notice never arrived"
wait_all 'echo: [background job b1 finished' || miss "the notice never became a turn"
settle || miss "frame never settled after the notice"
check "the notice landed once" "$(capall | grep -c '^\[background job b1 finished: exit 0 after' | tr -d ' ')" 1
check "the job's output reached the model, not the scrollback" "$(capall | grep -c '^l4yield$' | tr -d ' ')" 1
check "the status row's segment is gone" "$(bottom_zone | grep -cF 'job b1')" 0
_poll_until 60 progress_is_idle || miss "the last job's notice turn never cleared the progress indicator: $(last_progress)"
check "the last notice turn over, the progress indicator is cleared (OSC 9;4;0)" "$(last_progress)" 0
type_ '/j'
settle || miss "frame never settled after /j"
check "/j completes to nothing once the job is gone" "$(count_composer '⎿ jobs')" 0
key C-u
check_frame_intact "after the notice" 100

finish
