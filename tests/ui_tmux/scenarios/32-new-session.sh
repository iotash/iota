#!/usr/bin/env bash
# L4 scenario 32 (docs/design/new-session-command.md) — `/new` on a real terminal.
#
#   * a turn, then `/new`: one dim `New session <id>` divider, the old transcript still above it (no clear);
#   * the status row's token half back at zero after the divider (`↑ 11 ↓ 7` gone, the occupancy at 0%);
#   * a second message lands in the NEW bundle, the first stays in the old one;
#   * after exit, `iota resume <old id>` brings the old session back with its question.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

start 100 30 || finish
settle || bad "startup never settled"
old="$(cap | grep -o 'session [0-9a-z]\{12\}' | head -1 | cut -d' ' -f2)"
check_measured "old session id from the banner is 12 characters" "${#old}" 12
echo "old session: $old"

type_ 'first question'
key Enter
wait_sent 'first question' || bad "first message never sent"
settle || bad "the first turn never settled"
echo "---- status row after the first turn ----"
cap | grep -F '↑' | tail -1

type_ '/new'
key Enter
wait_vis 'New session ' || bad "no divider"
settle || bad "never settled after /new"
divider="$(cap | grep -F 'New session ' | tail -1 | sed 's/^ *//')"
new="$(printf '%s' "$divider" | awk '{print $3}')"
check_measured "new session id from the divider is 12 characters" "${#new}" 12
echo "new session: $new"
[ "$new" != "$old" ] && ok "new id differs from the old ($old → $new)" || bad "same id"
check "divider text" "$divider" "New session $new"
check_once "the old question is still on screen (no clear)" '❯ first question'
# Zero totals drop the ↑/↓ segment altogether: the row is the model and the occupancy alone.
status_row="$(cap | grep -F '/ 128k' | tail -1 | sed 's/^ *//')"
check "status row after /new: no ↑/↓, the occupancy back at zero" "$status_row" "fake · ≈0% / 128k"
echo "---- pane after /new ----"
cap | grep -v '^$'

type_ 'second question'
key Enter
wait_sent 'second question' || bad "second message never sent"
settle || bad "the second turn never settled"
key C-d
_poll_until 100 pane_dead || _poll_until 100 server_gone || true

old_dir="$(find "$SCEN_HOME/.iota/sessions" -maxdepth 3 -type d -name "$old" | head -1)"
new_dir="$(find "$SCEN_HOME/.iota/sessions" -maxdepth 3 -type d -name "$new" | head -1)"
[ -n "$old_dir" ] && ok "old bundle on disk: $old_dir" || bad "old bundle missing"
[ -n "$new_dir" ] && ok "new bundle on disk: $new_dir" || bad "new bundle missing"
[ "$(dirname "$old_dir")" = "$(dirname "$new_dir")" ] && ok "same bucket" || bad "different buckets"
grep -qF 'first question' "$old_dir/messages.jsonl" && ! grep -qF 'second question' "$old_dir/messages.jsonl" \
    && ok "old log: first question only" || bad "old log content"
grep -qF 'second question' "$new_dir/messages.jsonl" && ! grep -qF 'first question' "$new_dir/messages.jsonl" \
    && ok "new log: second question only" || bad "new log content"

start 100 30 resume "$old" || finish
settle || bad "resume never settled"
echo "---- iota resume $old ----"
capall | grep -v '^$' | head -20
check_once "resumed the old session" "Resumed session $old"
_all_has 'first question' && ok "its question is echoed back" || bad "no echo of the old question"

finish
