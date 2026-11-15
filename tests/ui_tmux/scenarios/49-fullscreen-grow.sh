#!/usr/bin/env bash
# L4 scenario 49 (tui-plan S7b) — a block that is still being written grows line by line.
#
# The mock's `grow` script streams a 24-line fenced code block, then a 24-row table, one line a
# delta, and HOLDS after code line 12 and after table row 6 (the scenario releases each), so what
# the pane shows mid-block is read while the block is still open. Before S7b an open block was a
# metered row (`rendering code… · 12 lines`) and appeared whole when it closed; now it is a block
# in the conversation that the buffer replaces in place as each line arrives, at the width of the
# screen — so a resize mid-stream lays out again what has arrived.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

# code_rows — the visible rows carrying a piece of a code line, as seen now.
code_rows() { cap | grep -E 'grow_[0-9][0-9]|long enough to wrap|wrap$'; }
# wait_width <w> — the frame laid out again at <w> columns. `settle` cannot be used while the
# stream is held: the status row's spinner and clock keep the frame changing.
_sep_is() { [ "$(sep_width)" = "$1" ]; }
wait_width() { _poll_until 120 _sep_is "$1" && sleep 0.3; }

start 80 24 || finish
settle || miss "startup never settled"

CODE="s49c-$$"
TABLE="s49t-$$"
type_ "grow 24 hold@12:$CODE thold@6:$TABLE"
key Enter
wait_vis 'let grow_11' || miss "the code block never streamed"
sleep 0.3
shot "mid code block, 80 columns"

# --- the open block shows its lines, not a counter
check "mid-stream: the twelve lines that arrived are on screen" "$(cap | grep -cE '^  let grow_(0[0-9]|1[01]) ' | tr -d ' ')" 12
check "mid-stream: no line past the hold is shown" "$(count_vis 'let grow_12')" 0
check "mid-stream: no metered counter row" "$(cap | grep -cE 'rendering|· [0-9]+ lines' | tr -d ' ')" 0
check "mid-stream: each line one row at 80 (no wrap)" "$(count_vis 'long enough to wrap')" 12

# --- a resize mid-stream lays out again what has arrived
tm resize-window -t s -x 50 -y 24
wait_width 50 || miss "the frame never laid out again at 50 columns"
shot "mid code block, 50 columns"
check "at 50: the arrived lines wrap under the gutter (no code row at column 0)" \
    "$(code_rows | grep -cvE '^  ' | tr -d ' ')" 0
if [ "$(code_rows | wc -l | tr -d ' ')" -gt "$(count_vis 'let grow_')" ]; then
    ok "at 50: the code lines wrap onto more rows than lines"
else
    bad "at 50: the code lines did not re-lay-out to the width"
fi

release "$CODE"
wait_vis 'trow_05' || miss "the table never streamed"
sleep 0.3
shot "mid table, 50 columns"

# --- the table grows its rows, boxed, as they arrive
check "mid-table: the six rows that arrived are boxed" "$(cap | grep -cE '^│ 0[0-5] +│ trow_0[0-5] +│$' | tr -d ' ')" 6
check "mid-table: no row past the hold" "$(count_vis 'trow_06')" 0
check "mid-table: the box is closed below the last row" "$(cap | grep -cE '^└' | tr -d ' ')" 1
check "mid-table: no raw source row" "$(count_vis '| 05 | trow_05 |')" 0

tm resize-window -t s -x 80 -y 24
wait_width 80 || miss "the frame never laid out again at 80 columns"
release "$TABLE"
wait_all 'GROWEND' || miss "the document never completed"
settle || miss "frame never settled after the turn"

# --- every block written once, whole: the write-out and the history read it back
check "every code line rendered exactly once" "$(uniq_all 'let grow_[0-9][0-9]')" 24
check "no code line is duplicated" "$(dupes_all 'let grow_[0-9][0-9]')" ""
check "every table row rendered exactly once" "$(uniq_all 'trow_[0-9][0-9]')" 24
check "no table row is duplicated" "$(dupes_all 'trow_[0-9][0-9]')" ""
check "no metered counter row anywhere" "$(capall | grep -cE 'rendering (code|table)' | tr -d ' ')" 0
check_once "the document's first marker landed once" 'GROWSTART'
check_frame_intact "after the grown blocks" 80

finish
