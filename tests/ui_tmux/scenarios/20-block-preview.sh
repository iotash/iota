#!/usr/bin/env bash
# L4 scenario 20 (docs/TUI-VERIFY.md §2.1, §2.2) — a long streamed document with buffered blocks.
#
# The stress shape §2 asks a human for: 40 source lines in one turn, with a 16-line fenced block
# and an 8-row table — the block kinds the renderer buffers. Since tui-plan S7b an open block is
# no longer a metered row (`label · N lines`) that morphs into the block on close: it is a block
# in the conversation that GROWS in place as each line arrives (scenario 49 reads it mid-block).
# The answer scrolls well past the top of a 24-row pane while blocks are still being replaced.
# What must hold, read back through the write-out: no counter row ever, every rendered row of
# every block there exactly once (a replaced block never appended twice), no raw source row
# (`| 01 | row_01 |`), and the FIRST line of the answer still there.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

start 80 24 || finish
settle || miss "startup never settled"

type_ 'blocks'
key Enter
wait_vis 'BLOCKSSTART' || miss "the document never started"
# The open blocks show their lines while they stream (§2.1's "watch it happen"), not a counter.
if wait_vis 'fn code_line_03'; then ok "the code block shows its lines while it streams"; else miss "the code block's lines never showed while it streamed"; fi
check "no metered counter row while the blocks stream" "$(cap | grep -cE 'rendering|· [0-9]+ lines' | tr -d ' ')" 0
wait_all 'BLOCKSEND' || miss "the document never completed"
settle || miss "frame never settled after the turn"

# --- the morph left nothing behind (§2.2: no gaps, no duplicated rows)
check "no metered counter row anywhere" "$(count_all ' lines')" 0
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

# --- the first line is still reachable (§2.2): the answer overran the viewport, so its head is
#     off the live screen, and in the conversation (`check_once 'BLOCKSSTART'` above).
if _vis_lacks 'BLOCKSSTART'; then
    ok "the answer scrolled past the top of the viewport and its head is still there"
else
    bad "the 40-line answer never scrolled — the stress shape did not happen"
fi
check "the user's block is in the history once" "$(count_all '▌ blocks')" 1
check "no ghost composer rows in the history" "$(capall | grep -cE '^blocks *$' | tr -d ' ')" 0
check_frame_intact "after the long document" 80

# Still usable, and the frame walks down onto the next turn.
type_ 'hello'
key Enter
wait_all 'echo: hello' || miss "input stopped working after the long document"
settle || miss "frame never settled after the follow-up turn"
check_frame_intact "after the follow-up turn" 80

finish
