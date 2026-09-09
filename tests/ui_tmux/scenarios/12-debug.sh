#!/usr/bin/env bash
# L4 scenario 12 (T3_TEST_PLAN §7) — /debug: the recording switch and the two-tab inspector.
#
# What a real terminal adds over the L1/L3 pins: the `debug` segment really reaches the
# status row through a live repaint (not a rebuilt frame in a test double), the inspector
# is a Tabbed surface in the bottom zone whose Enter DRILLS IN and whose Esc walks back out
# one level at a time, and a round-trip the binary itself made is what the list shows.
# The row's columns, the JSON re-indent and the ring's eviction are proved in L1.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

start 80 24 || finish
settle || bad "startup never settled"

status_ends_with_debug() { case "$(bottom_zone)" in *"· debug") return 0 ;; *) return 1 ;; esac; }

# ------------------------------------------------------------------ A: /debug on
check "the status row carries no debug marker while recording is off" \
    "$(count_vis '· debug')" 0

type_ '/debug on'
key Enter
wait_all 'Request recording ON' || bad "the /debug on notice never landed"
settle || bad "frame never settled after /debug on"

check_once "recording ON prints exactly one notice" \
    'Request recording ON — activity groups stay expanded'
if status_ends_with_debug; then
    ok "the status row ends with the debug segment ($(bottom_zone | sed 's/.*· /· /'))"
else
    bad "the status row has no debug segment: '$(bottom_zone)'"
fi

# ------------------------------------------------------------------ B: a recorded turn
type_ 'hello'
key Enter
wait_all 'echo: hello' || bad "the turn never completed"
settle || bad "frame never settled after the turn"

# ------------------------------------------------------------------ C: the inspector
type_ '/debug'
key Enter
wait_vis 'Messages' || bad "the /debug inspector never opened"
settle || bad "the inspector never settled"

comp="$(composer_row)"
bar="$(row_of ' Messages  │  Verbose')"
if [ -n "$comp" ] && [ -n "$bar" ] && [ "$bar" -gt "$comp" ]; then
    ok "the inspector renders BELOW the composer (composer row $comp, tab bar row $bar)"
else
    bad "the tab bar is not below the composer (composer=$comp bar=$bar)"
fi
check "the two tabs are Messages and Verbose" "$(count_vis ' Messages  │  Verbose')" 1
if cap | grep -q 'Chat.*200'; then
    ok "the turn's round-trip is listed (a Chat row answered 200)"
else
    bad "no 'Chat … 200' row in the inspector"
    cap | sed -n "$bar,\$p"
fi

# ------------------------------------------------------------------ D: drill in and back
key Enter
wait_vis '↑ Request' || bad "Enter did not drill into the highlighted entry"
settle || bad "the drill-down never settled"
check "the drill-down offers both halves" "$(count_vis ' ↑ Request  │  ↓ Response')" 1

key Escape
wait_vis ' Messages  │  Verbose' || bad "Esc did not walk back to the list"
settle || bad "the list never settled after the walk back"
check "the request view is gone after the walk back" "$(count_vis '↑ Request')" 0

key Escape
wait_gone ' Messages  │  Verbose' || bad "Esc did not close the inspector"
settle || bad "frame never settled after closing the inspector"
check "no ghost inspector rows" "$(count_vis 'Verbose')" 0
check_frame_intact "after the inspector closed" 80
if status_ends_with_debug; then
    ok "recording is still on after browsing (the switch was not touched)"
else
    bad "the debug segment vanished across the inspector: '$(bottom_zone)'"
fi

# ------------------------------------------------------------------ E: /debug off
type_ '/debug off'
key Enter
wait_all 'Request recording OFF' || bad "the /debug off notice never landed"
settle || bad "frame never settled after /debug off"
check "the debug segment is gone" "$(count_vis '· debug')" 0

finish
