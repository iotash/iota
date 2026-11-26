#!/usr/bin/env bash
# L4 scenario 56 (docs/design/fullscreen-tui.md §尾块, X-119) — the running call's widget is the conversation's
# tail block: it sits under the last line of output and scrolls with it.
#
#   A following — during a `sleep 4` call the widget (`⠋ [shell sleep 4; echo …]` and its `⎿` clock row) is on
#     screen, above the frame;
#   B scrolled back — PgUp twice takes the widget's rows off the screen, while the status row still says the turn
#     is running (`Running tools`): no thumbnail, no copy above the frame;
#   C back — Ctrl+End brings them back;
#   D settled — the call's committed form appears exactly once, and no widget row is left.
#
# The in-process pins are `the_widget_sits_under_the_last_line_and_the_frame_keeps_its_height` and
# `scrolling_back_takes_the_widget_off_and_a_step_row_is_new_activity` (event_loop/vt100_tests.rs).
# `shell` runs unsandboxed and auto-approved (33-trailing-text.sh's config), so no dialog stands in between.
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
    tools: {shell: {sandbox: off}}
    allow: [shell]"
export CONFIG_BODY

# A spinner glyph: an alternation, never a bracket expression (the C-locale note on `norm`).
SPIN='(⠋|⠙|⠹|⠸|⠼|⠴|⠦|⠧|⠇|⠏)'
HEADER='[shell sleep 4; echo w-done]'
# The widget's header row on screen.
_widget_rows() { cap | grep -cE -- "^$SPIN \\[shell sleep 4" | tr -d ' '; }
_widget_up() { [ "$(_widget_rows)" = 1 ]; }
_widget_off() { [ "$(_widget_rows)" = 0 ] && ! cap | grep -qF '⎿'; }

start_provider openai 100 24 || finish
settle || miss "startup never settled"

# Something to scroll back over.
type_ 'stream 40 100'
key Enter
wait_all 'l#39 line' || miss "the stream never finished"
settle || miss "the stream never settled"

# ------------------------------------------------------------------ A: following
type_ 'runfg:sleep 4; echo w-done'
key Enter
if _poll_until 60 _widget_up; then ok "A: the widget is up while the call runs"; else bad "A: no widget while the call ran"; fi
check "A: …with its clock row" "$(cap | grep -cE -- '⎿ [0-9<]+s$' | tr -d ' ')" 1
check "A: …above the frame" "$([ "$(row_of '[shell sleep 4')" -lt "$(frame_top)" ] && echo y)" y
shot "A: the call running, following the output"

# ------------------------------------------------------------------ B: scrolled back
key PPage
key PPage
if _poll_until 40 _widget_off; then ok "B: PgUp takes the widget's rows off the screen"; else bad "B: the widget stayed on the screen"; fi
check "B: …the status row still says the turn is running" "$(bottom_zone | grep -cF 'Running tools' | tr -d ' ')" 1
check "B: …the way back is offered" "$(count_vis 'Ctrl+End')" 1
check "B: …the call is not over yet" "$(count_all 'ran: w-done')" 0
shot "B: scrolled back during the call"

# ------------------------------------------------------------------ C: back to the tail
key C-End
if _poll_until 40 _widget_up; then ok "C: Ctrl+End brings the widget back"; else bad "C: no widget after Ctrl+End"; fi
shot "C: back at the tail"

# ------------------------------------------------------------------ D: settled
wait_all 'ran: w-done' || miss "D: the follow-up round never answered"
settle || miss "D: never settled"
check_once "D: the call's committed form, once" "$HEADER"
check "D: no widget row left" "$(_widget_rows)" 0
check_frame_intact "after D" 100

finish
