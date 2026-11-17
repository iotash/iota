#!/usr/bin/env bash
# L4 scenario 51 (xml-tab) — a message that IS one XML element, the shape a host such as herdr
# hands a sub-agent's message over in: the user block shows the inner text alone, and the tag
# with its attributes is a tab on the row directly above it, `  hc from:worker-2 `, filled in the
# bar's gray (`48;5;245`) from column 0 to one column past the text. Under `NO_COLOR` the tab is
# the plain row with no escape. The exit writes both rows to the main screen, in order.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

MSG='<hc from="worker-2">build is green</hc>'

tab_check() {
    local what="$1"
    local blk tab
    blk="$(row_of '▌ build is green')"
    tab="$(row_of '  hc from:worker-2')"
    check "$what: the tab is the row directly above the block" "$tab" "$((blk - 1))"
    check "$what: the block shows no tag" "$(cap | grep -cF '▌ <hc' | tr -d ' ')" 0
    check "$what: the tab row is the tab alone" \
        "$(cap | sed -n "${tab}p" | sed 's/ *$//')" "  hc from:worker-2"
}

# --- the dark theme
start 80 24 || finish
settle || miss "frame never settled"
type_ "$MSG"
key Enter
wait_vis '▌ build is green' || miss "the block never appeared"
wait_vis 'echo:' || miss "the reply never arrived"
settle || miss "the turn never settled"
tab_check "colour"
tab="$(row_of '  hc from:worker-2')"
row_e="$(tm capture-pane -ept s | sed -n "${tab}p")"
if printf '%s' "$row_e" | grep -qF '48;5;245'; then
    ok "colour: the tab is filled in the bar's gray"
else
    bad "colour: the tab row carries no 48;5;245: $(printf '%q' "$row_e")"
fi
echo "---- capture (colour, -e) ----"
tm capture-pane -ept s | sed -n "$((tab - 1)),$((tab + 2))p" | cat -v
echo "---- end ----"

# --- the exit writes the tab and the block out, in order
tm set-option -t s remain-on-exit on
key C-d
_poll_until 60 pane_dead || miss "Ctrl+D did not exit"
check_once "exit: the tab is in the history once" '  hc from:worker-2'
check "exit: the block follows the tab" "$(count_after '  hc from:worker-2' '▌ build is green')" 1

# --- NO_COLOR
IOTA_ENV="NO_COLOR=1" start 80 24 || finish
settle || miss "frame never settled (NO_COLOR)"
type_ "$MSG"
key Enter
wait_vis '▌ build is green' || miss "the block never appeared (NO_COLOR)"
wait_vis 'echo:' || miss "the reply never arrived (NO_COLOR)"
settle || miss "the turn never settled (NO_COLOR)"
tab_check "NO_COLOR"
tab="$(row_of '  hc from:worker-2')"
row_e="$(tm capture-pane -ept s | sed -n "${tab}p")"
if printf '%s' "$row_e" | grep -qF "$(printf '\033')"; then
    bad "NO_COLOR: the tab row carries an escape: $(printf '%q' "$row_e")"
else
    ok "NO_COLOR: the tab row is plain text"
fi
echo "---- capture (NO_COLOR, -e) ----"
tm capture-pane -ept s | sed -n "$((tab - 1)),$((tab + 2))p" | cat -v
echo "---- end ----"

finish
