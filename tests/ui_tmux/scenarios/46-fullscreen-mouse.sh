#!/usr/bin/env bash
# L4 scenario 46 — the mouse on the full screen (tui-plan S6), the reports written straight into
# the pane as a terminal sends them (`mouse`, lib.sh):
#
#   A. the wheel scrolls the conversation three lines a step and pauses following; down to the
#      bottom it follows the tail again, and new output shows;
#   B. a click on the composer puts the cursor under the pointer;
#   C. a drag held at the viewport's top row scrolls on, so the selection crosses three screens;
#      the release copies it — into tmux's paste buffer and through the local clipboard tool (a
#      stub first on PATH, so the developer's own clipboard is never touched) — and the buffer
#      pastes into another program whole;
#   D. a surface (/status) takes the interaction cap — half the screen, at most 15 rows — and
#      scrolls inside it; closing it gives the conversation back;
#   E. in a pane the frame alone fills (6 rows), the dialog is still on the screen, whole;
#   F. outside tmux (`$TMUX` unset in the pane) a drag's copy goes to the local tool AND to the
#      terminal: the pane's raw bytes carry exactly one OSC 52, its text the tool's, and the row
#      says `(pbcopy + OSC 52)`.
#
# Scenario 47 sends the reports through a tmux with `mouse on` instead, and turns the mouse off.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

CLIP="$SCEN_TMP/clip"
mkdir -p "$SCEN_TMP/bin"
for tool in pbcopy wl-copy; do
    printf '#!/bin/sh\ncat > "%s"\n' "$CLIP" >"$SCEN_TMP/bin/$tool"
    chmod +x "$SCEN_TMP/bin/$tool"
done
# Not over SSH (the runner may be): the local path is taken beside tmux's.
IOTA_ENV="-u SSH_CONNECTION -u SSH_TTY -u SSH_CLIENT PATH=$SCEN_TMP/bin:$PATH"

# The `l#NN` rows on screen, top to bottom, as their numbers.
nums() { cap | grep -oE '^l#[0-9]+ line' | sed -E 's/^l#0*([0-9]+) line/\1/'; }
top_num() { nums | head -1; }

start 80 24 || finish
settle || miss "startup never settled"
alt_mouse="$(tm display-message -pt s '#{mouse_any_flag}')"
check "the app asked for mouse reporting" "$alt_mouse" 1

# --- A. the wheel ---------------------------------------------------------------------------------
type_ 'stream 60 2'
key Enter
wait_vis 'l#59 line' || miss "the stream never ended"
settle || miss "the turn never settled"
t0="$(top_num)"
for _ in 1 2 3; do mouse 64 10 5; done
wait_vis 'Ctrl+End' || miss "the wheel never left the tail"
settle
t1="$(top_num)"
check "three wheel steps scroll nine lines" "$((10#$t0 - 10#$t1))" 9
if _vis_lacks 'l#59 line'; then ok "…the tail is off the screen"; else bad "l#59 still shown"; dump_pane; fi
for _ in 1 2 3 4; do mouse 65 10 5; done
wait_gone 'Ctrl+End' || miss "the wheel down never followed the tail again"
if _vis_has 'l#59 line'; then ok "the bottom follows the tail again"; else bad "no tail after the wheel down"; dump_pane; fi
type_ 'stream 3 2'
key Enter
wait_sent 'stream 3 2' || miss "the second turn was never sent"
settle || miss "the second turn never settled"
last_conv="$(cap | sed -n "1,$(($(frame_top) - 1))p" | grep -v '^[[:space:]]*$' | tail -1)"
check "new output shows at the bottom" "$(printf '%s' "$last_conv" | grep -c 'l#02 line')" 1

# --- B. a click on the composer -------------------------------------------------------------------
type_ 'hello world'
check "the draft shows" "$(wait_composer 'hello world' 1)" 1
crow="$(composer_row)"
# Column 4 (1-based) is the fourth glyph — the text starts at column 0, no prompt before it:
# before the second `l`.
mouse 0 4 "$crow"
mouse 0 4 "$crow" m
type_ 'X'
check "a click put the composer's cursor under the pointer" "$(wait_composer 'helXlo world' 1)" 1
key End
for _ in $(seq 12); do key BSpace; done
check "the draft is cleared" "$(wait_composer 'hel' 0)" 0

# --- C. a drag across three screens ---------------------------------------------------------------
type_ 'stream 100 2'
key Enter
wait_vis 'l#99 line' || miss "the long stream never ended"
settle || miss "the long stream never settled"
view_h=$(($(frame_top) - 1))
r99="$(row_of 'l#99 line')"
mouse 0 1 "$r99"
mouse 32 1 1
# Held at the top row, the view keeps scrolling: wait for the first line of the stream.
_poll_until 300 _vis_has 'l#00 line' || miss "the held drag never scrolled back to l#00"
mouse 0 1 1 m
wait_vis 'copied' || miss "the release copied nothing"
# The row names the ways that ran: the local stub and tmux's buffer; inside tmux no OSC 52 beside them.
local_tool=wl-copy
[ "$(uname)" = Darwin ] && local_tool=pbcopy
if _vis_has "($local_tool + tmux) · "; then
    ok "the status row names the ways ($local_tool + tmux)"
else
    bad "the status row does not name the ways: $(cap | grep -F copied)"
fi
_buffer() { tm show-buffer 2>/dev/null; }
_has_buffer() { _buffer | grep -qF 'l#98 line'; }
_poll_until 100 _has_buffer || miss "tmux's paste buffer never got the selection"
got="$(_buffer | grep -cE '^l#[0-9]+ line$' | tr -d ' ')"
if [ "$got" -ge $((3 * view_h)) ]; then
    ok "the selection crossed three screens ($got lines, viewport $view_h)"
else
    bad "only $got lines for a $view_h-row viewport"
fi
first_sel="$(_buffer | grep -oE '^l#[0-9]+ line' | head -1)"
check "it starts where the drag ended" "$first_sel" "l#00 line"
want="$(seq 0 98 | while read -r i; do printf 'l#%02d line\n' "$i"; done)"
check "the lines are whole and in order" "$(_buffer | grep -E '^l#[0-9]+ line$')" "$want"
check "it ends on the press's own cell" "$(_buffer | tail -1)" "l"
_has_clip() { [ -s "$CLIP" ] && grep -qF 'l#98 line' "$CLIP"; }
_poll_until 100 _has_clip || miss "the local clipboard tool never got the selection"
check "the local clipboard holds the same text" "$(cat "$CLIP")" "$(_buffer)"
# Into another program: a second window's `cat` takes tmux's paste.
tm new-window -d -t s -n paste "cat > $SCEN_TMP/pasted"
_paste_up() { tm list-windows -t s 2>/dev/null | grep -q paste; }
_poll_until 30 _paste_up || miss "the paste window never came up"
tm paste-buffer -t s:paste
tm send-keys -t s:paste C-d
_pasted() { grep -qF 'l#98 line' "$SCEN_TMP/pasted" 2>/dev/null; }
_poll_until 100 _pasted || miss "the paste never reached the other program"
check "the other program got every line" "$(grep -cE '^l#[0-9]+ line$' "$SCEN_TMP/pasted" | tr -d ' ')" 99
key C-End
wait_gone 'Ctrl+End' || true
key x
check "a key after the copy is typed" "$(wait_composer 'x' 1)" 1
key BSpace

# --- D. a surface takes the interaction cap -------------------------------------------------------
# /status is a View of 16 lines. An interaction area never exceeds half the screen or 15 rows: in
# 24 rows the View takes 12 and scrolls (G to its end), and the conversation keeps the rest;
# in a 14-row pane it takes 7.
type_ '/status'
key Enter
wait_vis 'Provider ' || miss "/status never opened"
settle
if _vis_has 'Provider ' && _vis_lacks 'Title '; then ok "/status takes 12 rows, its last line below them"; else bad "/status is not capped"; dump_pane; fi
if _vis_has 'l#99 line'; then ok "…the conversation keeps its rows above it"; else bad "the conversation yielded every row"; dump_pane; fi
# The hint is cut at 80 columns before its `NN%`: the scroll shows in what the View holds.
key G
_at_end() { _vis_has 'Title ' && _vis_lacks 'Provider '; }
if _poll_until 100 _at_end; then ok "G scrolls to its last line"; else bad "G never reached /status's last line"; dump_pane; fi
key g
wait_vis 'Provider ' || miss "g never came back to the top"
tm resize-window -t s -x 80 -y 14
_short() { [ "$(cap | wc -l | tr -d ' ')" -le 14 ] && _vis_has 'q/Esc close'; }
_poll_until 100 _short || miss "the pane never got short"
settle
# Half of 14 is 7: the conversation's one row, the spacer, two separators and the composer, then
# the View's title, its 7 rows and its hint.
check "in a short pane the frame starts on row 3 (a conversation row and the spacer above it)" "$(frame_top)" 3
check "…the View right under the composer" "$(cap | sed -n '6p')" " Status"
check "…7 rows of it" "$(( $(row_of 'q/Esc close') - $(row_of ' Status') - 1 ))" 7
if _vis_has 'l#99 line'; then ok "…the conversation keeps a row"; else bad "no conversation beside the View"; dump_pane; fi
tm resize-window -t s -x 80 -y 24
_tall() { [ "$(cap | wc -l | tr -d ' ')" -ge 24 ]; }
_poll_until 100 _tall || miss "the pane never grew back"
key Escape
wait_gone 'q/Esc close' || miss "/status never closed"
settle
if _vis_has 'l#99 line'; then ok "closing it gives the conversation back"; else bad "no conversation after the surface"; dump_pane; fi
check_frame_intact "after the surface" 80

# --- E. a pane the frame alone fills ---------------------------------------------------------------
# 6 rows: the spacer, two separators and the composer take four, so the room the frame leaves is
# two — less than a dialog's minimum. The surface still gets its rows: the screen keeps the
# frame's bottom (the dialog, its hint) and the composer above it yields.
tm resize-window -t s -x 80 -y 6
_six() { [ "$(cap | wc -l | tr -d ' ')" -le 6 ]; }
_poll_until 100 _six || miss "the pane never got to 6 rows"
type_ '/status'
key Enter
wait_vis 'q/Esc close' || miss "/status's hint is off a 6-row screen"
settle
check "6 rows: the dialog's hint is the last row" "$(cap | tail -1 | grep -c "q/Esc close")" 1
if _vis_has ' Status'; then ok "…and its title is on the screen"; else bad "the dialog's top is off the screen"; dump_pane; fi
key Escape
wait_gone 'q/Esc close' || miss "/status never closed in 6 rows"
tm resize-window -t s -x 80 -y 24
_poll_until 100 _tall || miss "the pane never grew back from 6 rows"
settle
check_frame_intact "after the 6-row dialog" 80

# --- F. outside tmux: the tool and OSC 52 ----------------------------------------------------------
# A fresh run whose environment has no $TMUX (nor $STY): to iota it is a plain terminal. The pane's
# raw bytes are tapped, since tmux itself does not pass the sequence on.
IOTA_ENV="-u TMUX -u STY $IOTA_ENV"
rm -f "$CLIP"
start 80 24 || finish
settle || miss "the run outside tmux never settled"
pipe_raw
type_ 'stream 3 2'
key Enter
wait_vis 'l#02 line' || miss "the short stream never ended"
settle || miss "the short stream never settled"
r1="$(row_of 'l#01 line')"
mouse 0 1 "$r1"
mouse 32 9 "$r1"
mouse 0 9 "$r1" m
wait_vis 'copied' || miss "the release outside tmux copied nothing"
if _vis_has "($local_tool + OSC 52) · "; then
    ok "outside tmux the row names the tool and the terminal ($local_tool + OSC 52)"
else
    bad "the row outside tmux: $(cap | grep -F copied)"
fi
_has_clip_l01() { [ -s "$CLIP" ] && grep -qF 'l#01' "$CLIP"; }
_poll_until 100 _has_clip_l01 || miss "the local tool never got the selection outside tmux"
_osc52s() { LC_ALL=C grep -aoE $'\033]52;c;[A-Za-z0-9+/=]*\a' "$RAW"; }
check "exactly one OSC 52 reached the terminal" "$(_osc52s | wc -l | tr -d ' ')" 1
sent="$(_osc52s | head -1 | sed -E $'s/^\033]52;c;//; s/\a$//' | base64 -d)"
check "…carrying what the local tool got" "$sent" "$(cat "$CLIP")"

finish
