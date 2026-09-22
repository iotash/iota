#!/usr/bin/env bash
# L4 scenario 19 (docs/TUI-VERIFY.md §8) — background-job notices on a real terminal.
#
# The queue laws are unit-pinned (`ui::runtime::event_loop::queue_tests`) and the two arrivals
# are covered at the REPL level (`tests/repl/jobs.rs`); what was left to a human is what the
# arrival LOOKS like. The mock's `run:<cmd>` script is one `shell` tool call with
# `background: true`, so a scenario can start a real job (`sleep N; echo done`) and watch its
# notice come back through `Ui::enqueue`:
#
#   8.1 idle wake-up      — one dim headline and a normal turn, no `❯` block;
#   8.2 the draft survives — a half-typed line is still there, cursor where it was;
#   8.3 mid-turn arrival  — the headline lands AFTER the running call's rows and BEFORE the
#                           model's reply, i.e. at the round boundary of a tool turn;
#   8.4 queued while typing ahead — the headline is a `»` row among the typed-ahead lines,
#                           and ↑ recalls the user's newest line, stepping over it;
#   8.5 ESC keeps the job — the interrupt leaves the job running; the exit gesture kills it.
#
# There is no `/quit` command: the exit is Ctrl+C at idle, twice (scenario 05), and
# `Jobs::kill_all` runs on the way out (`repl/run.rs`). The agent runs `shell` with the sandbox
# off and `auto_run: true`, so no approval prompt sits between the call and the job.
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
    tools: {shell: {sandbox: off, auto_run: true}}"
export CONFIG_BODY

# start_job <n> <cmd> — one `run:` turn: the call goes out, the receipt names job b<n>, the turn ends.
start_job() {
    type_ "run:$2"
    key Enter
    wait_all "ran: Started background job b$1" || bad "job b$1 never started"
}

# row_all <fixed string> — 1-based row of the LAST match in the whole history ("" if none).
row_all() { capall | grep -nF -- "$1" | tail -1 | cut -d: -f1; }

# headline_count <n> — dim notice rows for job b<n> (the row STARTS with the bracket; the model's
# echo of the same text does not).
headline_count() { capall | grep -c "^\[background job b$1 finished: exit 0 after" | tr -d ' '; }

job_running() { pgrep -f 'echo l4marker' >/dev/null 2>&1; }
job_gone() { ! job_running; }

start_provider openai 80 24 || finish
settle || bad "startup never settled"

# ------------------------------------------------------------------ 8.1: idle wake-up
start_job 1 'sleep 2; echo done'
settle || bad "frame never settled after the call"
check_once "the call is echoed once" '❯ run:sleep 2; echo done'
check "the call's receipt is the model's whole reply" "$(count_all 'ran: Started background job b1 (pid')" 1

wait_all '[background job b1 finished' || bad "the job's notice never arrived"
wait_all 'echo: [background job b1 finished' || bad "the notice never became a turn"
settle || bad "frame never settled after the wake-up turn"
check "the notice is ONE dim headline" "$(headline_count 1)" 1
check "…not a ❯ block" "$(count_all '❯ [background')" 0
check "…answered by a normal turn" "$(capall | grep -cE '^echo: \[background job b1 finished: exit 0 after [0-9.]+s\] sleep 2; echo done$' | tr -d ' ')" 1
# The job's `done` reaches the model (the echo repeats it on its own row) and nothing else
# prints it: the notice is the headline alone, the output stays in the log file.
check "the job's output reached the model, not the scrollback" "$(capall | grep -c '^done$' | tr -d ' ')" 1
check_frame_intact "after the idle wake-up" 80

# ------------------------------------------------------------------ 8.2: the draft survives
start_job 2 'sleep 2; echo done'
type_ 'half draft'
wait_vis '❯ half draft' || bad "the draft never rendered"
comp="$(composer_row)"
check "the cursor sits at the draft's end" "$(cursor_xy)" "12 $((comp - 1))"

wait_all 'echo: [background job b2 finished' || bad "the second notice never became a turn"
settle || bad "frame never settled after the second wake-up"
check "the notice landed" "$(headline_count 2)" 1
check "the draft is still in the composer" "$(count_composer '❯ half draft')" 1
comp="$(composer_row)"
check "…with the cursor where it was" "$(cursor_xy)" "12 $((comp - 1))"
check_frame_intact "with the draft kept" 80
key C-u
wait_gone '❯ half draft' || bad "Ctrl+U did not clear the draft"

# ------------------------------------------------------------------ 8.3: mid-turn arrival
start_job 3 'sleep 2; echo done'
type_ 'runfg:sleep 3'
key Enter
wait_all 'echo: [background job b3 finished' || bad "the notice never reached the model mid-turn"
settle || bad "frame never settled after the tool turn"
check "the notice landed once" "$(headline_count 3)" 1
check_once "the tool turn was echoed once" '❯ runfg:sleep 3'
user_row="$(row_all '❯ runfg:sleep 3')"
call_row="$(row_all '[shell sleep 3]')"
notice_row="$(capall | grep -n '^\[background job b3 finished: exit 0' | tail -1 | cut -d: -f1)"
reply_row="$(row_all 'echo: [background job b3 finished')"
if [ -n "$user_row" ] && [ -n "$call_row" ] && [ -n "$notice_row" ] && [ -n "$reply_row" ] \
    && [ "$user_row" -lt "$call_row" ] && [ "$call_row" -lt "$notice_row" ] && [ "$notice_row" -lt "$reply_row" ]; then
    ok "the headline lands at the round boundary: after the call's rows ($call_row), before the reply ($reply_row)"
else
    bad "the headline is not at the round boundary (user=$user_row call=$call_row notice=$notice_row reply=$reply_row)"
    capall | tail -20
fi
check "no ❯ block for the mid-turn notice" "$(count_all '❯ [background')" 0
check_frame_intact "after the mid-turn arrival" 80

# ------------------------------------------------------------------ 8.4: queued while typing ahead
start_job 4 'sleep 1; echo done'
type_ 'stream 60'
key Enter
wait_vis 'l#02 line' || bad "the stream never started"
type_ 'ahead one'
key Enter
type_ 'ahead two'
key Enter
wait_vis '» ahead two' || bad "the typed-ahead rows never rendered"
wait_vis '» [background job b4 finished' || bad "the job's headline never queued as a » row"
check "the typed-ahead lines queue as » rows" "$(count_vis '» ahead one')" 1
check "…both of them" "$(count_vis '» ahead two')" 1
key Up
wait_vis '❯ ahead two' || bad "↑ did not recall the newest typed line"
check "↑ recalls the user's newest line into the composer" "$(count_composer '❯ ahead two')" 1
check "…taking it off the queue" "$(count_vis '» ahead two')" 0
check "…stepping over the job's headline" "$(count_vis '» [background job b4 finished')" 1
check "…and leaving the older line queued" "$(count_vis '» ahead one')" 1
key Enter
wait_all 'echo: ahead two' || bad "the recalled line never reached the model"
settle || bad "frame never settled after the queue drained"
check_once "the older line ran once" '❯ ahead one'
check_once "the recalled line ran once" '❯ ahead two'
check "the queued headline became its own turn" "$(headline_count 4)" 1
check_once "…answered once" 'echo: [background job b4 finished'
check_frame_intact "after the queue drained" 80

# ------------------------------------------------------------------ 8.5: ESC keeps the job; the exit kills it
start_job 5 'sleep 30; echo l4marker'
if job_running; then ok "the job is running (pgrep sees its command line)"; else bad "the job is not running"; fi
type_ 'stream 60'
key Enter
wait_vis 'l#05 line' || bad "the stream never started"
key Escape
wait_all 'Interrupted.' || bad "ESC never reached the loop"
settle || bad "frame never settled after the interrupt"
check_once "the interrupt notice is committed once" 'Interrupted.'
if job_running; then ok "ESC left the job running"; else bad "ESC killed the job"; fi
check "no notice for a job still running" "$(headline_count 5)" 0

key C-c
key C-c
_poll_until 60 server_gone || bad "the exit gesture did not exit"
if _poll_until 50 job_gone; then
    ok "the exit killed the job (its command line is gone from ps)"
else
    bad "the job outlived the binary"
    pkill -f 'echo l4marker' || true
fi

finish
