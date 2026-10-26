#!/usr/bin/env bash
# L4 scenario 35 (DIVERGENCES X-84, docs/design/rate-limit-and-interrupt.md §7.3) — the notice gate on a
# real terminal: what the user of the GLM session saw loop is now one block and dim lines.
#
# The gate's transitions are unit-pinned (`repl::noticegate`) and the turns it lets through are pinned at
# the REPL level (`tests/repl/rate_limit.rs`, under a paused clock); what is left is what it LOOKS like:
#
#   A  ESC holds the queue — a job's notice queued behind a streaming turn is put aside when ESC stops
#      it (one dim `⏸ … held` line), opens no turn of its own, and goes with the next typed message;
#   B  a rate limit holds the rest — a job's notice refused with 429 climbs the 3-rung ladder on the status
#      row and paints ONE `✗ Rate limited (429)` block with the next try in its hint; the next job's
#      notice, finishing during the ladder, is put aside with a `held until` line and no second block.
#
# The 429 comes from the mock's `fail:<status>:<n>` directive, carried by the job's OUTPUT so only the
# notice's turn is refused: the command prints `fail:429:99` without containing the directive itself
# (`printf 'fa%s…' il`), since the mock reads the first `fail:` of each user message. The mock names no
# `retry-after`, so the ladder is the wire layer's two retries under each of 2/4/8 s — about 20 s, longer
# than `wait_all` polls; the block is waited for with a wider poll. The 60 s probe is left to the REPL tests.
# The agent runs `shell` with the sandbox off and `auto_run: true`, as scenario 19 does.
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
    wait_all "ran: Started background job b$1" || miss "job b$1 never started"
    settle || miss "frame never settled after job b$1 started"
}

# joined_count <fixed string> — occurrences in the whole history with the rows joined: a `⏸` line carries the
# job's command and wraps, so its tail is not on the row its head is. The REPL breaks the rows itself, so tmux
# does not see a wrap to join; the capture is `-J` only to keep each row's trailing blanks — a plain one trims
# them, so a space the REPL wrote in the last column was lost and `with your` read `withyour`, which it did
# whenever the job took 1–9 s (`after 2s`, macOS CI) rather than `<1s`.
joined_count() { tm capture-pane -pJt s -S -400 2>/dev/null | tr -d '\n' | grep -oF -- "$1" | wc -l | tr -d ' '; }

# headline_count <n> — the dim headline rows of job b<n>'s notice (the row STARTS with the bracket).
headline_count() { capall | grep -c "^\[background job b$1 finished: exit 0 after" | tr -d ' '; }

# A job that ends when the scenario touches b<n>.go, printing `fail:429:99` (the mock's directive).
refused_job() { printf "%s" "until [ -e b$1.go ]; do sleep 0.1; done; printf 'fa%s:429:99' il; echo"; }

start_provider openai 120 30 || finish
settle || miss "startup never settled"

# ------------------------------------------------------------------ A: ESC holds the queue
start_job 1 'until [ -e b1.go ]; do sleep 0.1; done; echo done'
HOLD="s35-$$"
type_ "stream 60 60 hold@3:$HOLD"
key Enter
wait_vis 'l#02 line' || miss "the stream never started"
: >"$SCEN_TMP/b1.go"
wait_vis '» [background job b1 finished' || miss "the job's notice never queued behind the turn"
key Escape
wait_all 'Interrupted.' || miss "ESC never reached the loop"
wait_all '⏸ [background job b1 finished' || miss "the queued notice was not put aside"
release "$HOLD"
settle || miss "frame never settled after the interrupt"
check "the notice is put aside once" "$(count_all '⏸ [background job b1 finished')" 1
check "…saying it waits for the user" "$(joined_count 'held; it goes with your next message')" 1
sleep 2
check "the held notice opened no turn" "$(count_all 'echo: [background job b1')" 0
check "…and printed no headline row of its own" "$(headline_count 1)" 0

type_ 'go'
key Enter
wait_all 'echo: go' || miss "the typed message never reached the model"
settle || miss "frame never settled after the typed message"
check "the held notice went with the message: its headline, once" "$(headline_count 1)" 1
check_once "the message was answered once" 'echo: go'
check "the notice was not answered on its own" "$(count_all 'echo: [background job b1')" 0
check_frame_intact "after the held notice went" 120

# ------------------------------------------------------------------ B: a rate limit holds the rest
start_job 2 "$(refused_job 2)"
start_job 3 "$(refused_job 3)"
: >"$SCEN_TMP/b2.go"
wait_vis 'Rate limited (429) — retrying (attempt 1/3)' || miss "the 429 never reached the status row on its own rungs"
: >"$SCEN_TMP/b3.go"
_poll_until 450 _all_has '✗ Rate limited (429)' || miss "the ladder never gave up"
wait_all '⏸ [background job b3 finished' || miss "the notice behind the refusal was not put aside"
settle || miss "frame never settled after the refusal"
check "the hint names what happens next" "$(count_all 'Background-job notices wait until')" 1
check "the notice behind it is put aside once" "$(count_all '⏸ [background job b3 finished')" 1
check "…saying until when" "$(joined_count 'held until')" 1
sleep 3
check "ONE 429 block for two finished jobs" "$(count_all '✗ Rate limited (429)')" 1
check "the held notice opened no turn" "$(count_vis 'retrying')" 0
check "no answer to either notice" "$(count_all 'echo: [background job b')" 0
check_frame_intact "after the refusal" 120

# ------------------------------------------------------------------ the exit gesture at idle
key C-c
key C-c
_poll_until 60 server_gone || miss "the exit gesture did not exit"

finish
