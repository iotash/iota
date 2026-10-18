#!/usr/bin/env bash
# L4 scenario 4 (TUI_TEST_PLAN §L4) — ESC mid-stream.
#
# ESC reaches the loop while the render path is busy (the spike's gate 5, in-process here),
# and its effect is ATOMIC: the turn is cancelled, `Interrupted.` is committed once, and
# the type-ahead queue plus the half-typed draft fold back into the composer as one
# multi-row draft — interrupt means "the situation changed", so nothing auto-sends.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

start 80 24 || finish
settle || miss "startup never settled"

# The stream holds after its sixth line while the queue and the draft are typed (`hold@6`), and is
# released just before ESC — the keys land mid-stream however slow the runner, and ESC still meets a
# stream that is FLOWING (it waits for the next line), so "frozen after ESC" below is about ESC.
HOLD="s04-$$"
type_ "stream 60 60 hold@6:$HOLD"
key Enter
wait_vis 'l#05 line' || miss "the stream never started"

# One queued submit and one half-typed draft, so the fold has both halves to join.
type_ 'queued'
key Enter
wait_vis '» queued' || miss "the queued submit never rendered"
type_ 'half'
wait_vis '❯ half' || miss "the half-typed draft never rendered"

release "$HOLD"
wait_all 'l#06 line' || miss "the released stream never moved on"
key Escape
wait_all 'Interrupted.' || miss "ESC never reached the loop"
settle || miss "frame never settled after the interrupt"

check_once "the interrupt notice is committed exactly once" 'Interrupted.'
check "the queue row is gone (folded, not dropped)" "$(count_vis '» queued')" 0
check "fold row 1: the queued submit heads the draft" "$(count_composer '❯ queued')" 1
check "fold row 2: the half-typed draft follows it" "$(count_composer '  half')" 1
check "the folded draft is TWO composer rows, not a submit" "$(composer_block | wc -l | tr -d ' ')" 2
check "…and it is still ONE prompt" "$(count_composer '❯')" 1
# The real cursor follows the fold onto the SECOND composer row, at the end of the half-typed
# text — where an IME would anchor after a multi-row draft (docs/TUI-VERIFY.md §1.4).
comp="$(composer_row)"
check "the real cursor sits at the end of the folded draft's last row" "$(cursor_xy)" "6 $comp"

# The turn is really cancelled: no further inserts, and the cancel hint is gone with the
# scope that owned it.
frozen_a="$(hist_size)"
# A NEGATIVE claim's window, not a wait (lib.sh rule 3): a stream that ESC failed to stop would
# insert a row every 60 ms (LINE_GAP), about sixteen inside it.
sleep 1.0
frozen_b="$(hist_size)"
check "insert counter frozen after ESC" "$frozen_a" "$frozen_b"
check "no ESC hint without an active scope" "$(count_vis 'ESC to cancel')" 0
check "no partial reply was committed" "$(count_all 'echo:')" 0

# The frame survived: two separators, the (now two-row) composer between them, status
# below.
check "separators still span the terminal" "$(sep_width)" 80
check "status line is back in the bottom zone" "$(status_model)" "  fake"

# And the folded draft is a real draft: submitting it sends both lines.
key Enter
wait_all 'echo: queued' || miss "the folded draft never reached the model"
settle || miss "frame never settled after the resubmit"
check "the folded draft submitted as ONE two-line message" "$(count_all '❯ queued')" 1
check "the composer is empty again" "$(count_composer '❯ queued')" 0
check_frame_intact "after resubmitting the folded draft" 80

finish
