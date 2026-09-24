#!/usr/bin/env bash
# L4 scenario 3 (TUI_TEST_PLAN §L4) — surface open/close self-heal.
#
# Part A (idle): /model opens a Select panel in the bottom zone, BELOW the composer; a
# cancelled panel restores the pane byte-for-byte (no ghost rows, no eaten rows); a
# committed one leaves exactly ONE one-line record and swaps the status model.
#
# Part B (mid-stream): a slash command typed while a turn streams cannot open a panel —
# the type-ahead law holds it in the queue (event_loop.rs: the drain stops at the first
# '/'), so what L4 proves here is the SHAPE of that: the queue row renders, the stream
# keeps flowing above it, the panel opens on the far side, and closing it leaves the frame
# whole and the history contiguous. (Recorded in DEVIATIONS3 [WP52]: the plan's literal
# "surface open DURING the stream" is unreachable in T1 by construction.)
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

start 80 24 || finish
settle || bad "startup never settled"

# ------------------------------------------------------------------ A: idle open/cancel
before="$(cap)"

type_ '/model'
key Enter
wait_vis '↑↓ move' || bad "the /model panel never opened"
settle || bad "panel never settled"

comp="$(composer_row)"
panel="$(row_of '▸ fake (current)')"
foot="$(row_of '↑↓ move')"
if [ -n "$comp" ] && [ -n "$panel" ] && [ "$panel" -gt "$comp" ]; then
    ok "panel renders BELOW the composer (composer row $comp, first option row $panel)"
else
    bad "panel not below the composer (composer=$comp option=$panel)"
fi
# The row above the options is the TAB BAR: since WP54 (T-12) /model is the full
# questionnaire, and the wiremock dialect reports usage and is tunable, so it earns
# Context + Effort + Temperature beside Model. (No System tab: this fixture runs
# without a system prompt.) Focused and unfocused chips carry identical " title "
# padding, which is why the bar width does not move as Tab walks it.
check "panel title row (the capability tab bar)" "$(cap | sed -n "$((panel - 1))p")" \
    " Model  │  Context  │  Effort  │  Temperature"
check "panel offers every model the provider listed" "$(count_vis '  gemini-pro')" 1
check "the footer is the last frame row" "$foot" "$((panel + 3))"
check "composer still the one input row" "$(count_composer '❯')" 1
check "the panel did not disturb the separator pair" "$(sep_width)" 79

key Escape
wait_gone '↑↓ move' || bad "the panel did not close on ESC"
settle || bad "frame never settled after cancel"
if [ "$(cap)" = "$before" ]; then
    ok "a cancelled panel restores the pane exactly (no record, no ghost rows)"
else
    bad "pane differs after a cancelled panel"
    diff <(printf '%s\n' "$before") <(printf '%s\n' "$(cap)") | head -20
fi

# ------------------------------------------------------------------ B: queued mid-stream
type_ 'stream 40'
key Enter
wait_vis 'l#05 line' || bad "the stream never started"

type_ '/model'
key Enter
wait_vis '» /model' || bad "the queued command row never rendered"
check "the queue row carries the management hint" "$(count_vis '» /model · ↑ edit')" 1

# PROGRESS, not a clock, says the stream kept flowing: the scrollback counter is sampled
# the moment the queue row renders and read again when the last line lands. The sample
# used to be a fixed 0.6 s window, which measured the HOST — on the macos runner the app
# inserted nothing inside it and the scenario called a healthy stream stalled (CI
# 34777246950), while the same run's contiguity check saw all forty lines. `l#39` cannot
# reach a 24-row pane without pushing rows past its top, so the comparison below is exact
# and the only way it fails is the stall it is named for.
queued="$(hist_size)"
wait_all 'l#39 line' || bad "the stream did not run to completion"
flowed="$(hist_size)"
if [ "$flowed" -gt "$queued" ]; then
    ok "the stream kept flowing above the queue row ($queued -> $flowed rows in scrollback)"
else
    bad "the stream stalled while a command sat in the queue ($queued -> $flowed)"
fi

wait_vis '↑↓ move' || bad "the queued /model never opened after the stream"

key Down
key Enter
wait_all 'Model switched to gemini-pro' || bad "the close record never landed"
settle || bad "frame never settled after the commit"

check_once "closing commits exactly ONE one-line record" 'Model switched to gemini-pro'
check "the status line swapped to the chosen model" "$(status_model)" "  gemini-pro"
check "no ghost panel rows after close" "$(count_vis '↑↓ move')" 0
check "no ghost option rows after close" "$(count_vis 'gpt-4o')" 0
check "history contiguous across open/close" "$(uniq_all 'l#[0-9][0-9]')" 40
check "no duplicated history rows" "$(dupes_all 'l#[0-9][0-9] line')" ""
check_frame_intact "after the panel closed" 80

# The frame walks back down on the next output, and the record is not stranded in a gap.
type_ 'hello'
key Enter
wait_all 'echo: hello' || bad "the chat did not accept input after the panel closed"
settle || bad "frame never settled after the follow-up turn"
check_frame_intact "after the follow-up turn" 80

finish
