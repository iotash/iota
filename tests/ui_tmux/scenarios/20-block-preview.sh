#!/usr/bin/env bash
# L4 scenario 20 (docs/TUI-VERIFY.md §2.1, §2.2) — a long streamed document with buffered blocks.
#
# Scenario 02 proves a small document's preview MORPHS in place; this one is the stress shape
# §2 asks a human for: 40 source lines in one turn, with a 16-line fenced block and an 8-row
# table — the two block kinds the renderer BUFFERS behind a metered preview row
# (`markdown/preview.rs`: one row `label · N lines`, morphed into the rendered block on
# close) — so the answer scrolls well past the top of a 24-row pane while rows are still being
# inserted and rewritten. What must hold, read back through the scrollback: the preview rows
# are gone, every rendered row of every block is there exactly once, no raw source row
# (`| 01 | row_01 |`) leaked past the morph, and the FIRST line of the answer is still there.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

start 80 24 || finish
settle || bad "startup never settled"

type_ 'blocks'
key Enter
wait_vis 'BLOCKSSTART' || bad "the document never started"
# The buffered blocks show their metered row while they stream (§2.1's "watch it happen").
wait_vis ' lines' || bad "no metered preview row appeared while the blocks streamed"
ok "a buffered block streams behind its metered preview row"
wait_all 'BLOCKSEND' || bad "the document never completed"
settle || bad "frame never settled after the turn"

# --- the morph left nothing behind (§2.2: no gaps, no duplicated rows)
check "no metered preview row survives the morph" "$(count_all ' lines')" 0
check "every code line rendered exactly once" "$(uniq_all 'fn code_line_[0-9][0-9]')" 16
check "no code line is duplicated" "$(dupes_all 'fn code_line_[0-9][0-9]')" ""
check "every table row rendered exactly once" "$(uniq_all 'row_[0-9][0-9]')" 8
check "no table row is duplicated" "$(dupes_all 'row_[0-9][0-9]')" ""
check "the table rendered with borders, not as its source rows" "$(count_all '| 01 | row_01 |')" 0
check "…the first data row is bordered" "$(capall | grep -cE '^│ 01 +│ row_01 +│$' | tr -d ' ')" 1
check "every list item rendered exactly once" "$(uniq_all 'item_[0-9][0-9]')" 8
check "no list item is duplicated" "$(dupes_all 'item_[0-9][0-9]')" ""
check_once "the document's first marker landed once" 'BLOCKSSTART'
check_once "…and its last" 'BLOCKSEND'
check_once "the heading above the blocks is intact" 'Blocks'

# --- the first line is still reachable (§2.2): the answer overran the pane, so its head is in
#     the scrollback tmux preserved, above the banner-relative marker.
if [ "$(hist_size)" -gt 0 ]; then
    ok "the answer scrolled past the top ($(hist_size) rows in scrollback) and its head is still there"
else
    bad "the 40-line answer never scrolled — the stress shape did not happen"
fi
check "no ghost frame rows in the history" "$(count_all '❯ blocks')" 1
check_frame_intact "after the long document" 80

# Still usable, and the frame walks down onto the next turn.
type_ 'hello'
key Enter
wait_all 'echo: hello' || bad "input stopped working after the long document"
settle || bad "frame never settled after the follow-up turn"
check_frame_intact "after the follow-up turn" 80

finish
