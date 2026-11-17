#!/usr/bin/env bash
# L4 scenario 51 (xml-tab) — a pasted `<hc>` snippet on a PLAIN terminal is the user's text. Only a
# host claims an element (herdr-core claims `hc`, DIVERGENCES X-107; its tab is pinned inside a real
# hc core by tests/host/hc.rs), and a tmux pane is no host: the user block shows the message byte
# for byte, tags and attributes included, with no tab above it — on screen and in what the exit
# writes to the main screen.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

MSG='<hc from="worker-2">build is green</hc>'

start 80 24 || finish
settle || miss "frame never settled"
type_ "$MSG"
key Enter
wait_vis "▌ $MSG" || miss "the block never appeared verbatim"
wait_vis 'echo:' || miss "the reply never arrived"
settle || miss "the turn never settled"
blk="$(row_of "▌ $MSG")"
check "the block is the message byte for byte" \
    "$(cap | sed -n "${blk}p" | sed 's/ *$//')" "▌ $MSG"
check "no tab above it" "$(count_vis '  hc from:worker-2')" 0
check "the row above the block is the separator blank" \
    "$(cap | sed -n "$((blk - 1))p" | sed 's/ *$//')" ""

tm set-option -t s remain-on-exit on
key C-d
_poll_until 60 pane_dead || miss "Ctrl+D did not exit"
check_once "exit: the message is written out verbatim" "▌ $MSG"
check "exit: no tab is written out" "$(count_all '  hc from:worker-2')" 0

finish
