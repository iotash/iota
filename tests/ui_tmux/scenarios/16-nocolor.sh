#!/usr/bin/env bash
# L4 scenario 16 (MIGRATION-ROADMAP §3 #2) — `NO_COLOR` in a real terminal.
#
# Two layers, two laws (docs/DIVERGENCES.md X-27, X-28):
#   * the CHAT side (transcript, markdown, styles) emits no escape at all — a committed
#     heading row carries neither a color nor bold/underline;
#   * the FRAME side (composer, status row, separators) drops every foreground and
#     background color but keeps its attributes, so the frame is still a frame.
#
# `capture-pane -e` is the chat-side oracle: it prints the RENDERED grid with the SGR
# state of every cell, so a committed row can be inspected on its own. `pipe-pane` is the
# frame-side oracle: the whole byte stream the terminal received, grepped for any color
# parameter. A control run WITHOUT the variable follows and must show both — a "no color"
# assertion against a binary that never painted would prove nothing.
#
# The binary is launched from a shell pane (`start_shell`) so the environment is the
# scenario's to set and the raw tap is attached before the first frame (see 14).
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

ESC="$(printf '\033')"
# A CSI whose parameter list carries a foreground or background color: 30–37/90–97,
# 40–47/100–107, or the 38/48 extended forms. Attribute-only sequences (1, 2, 4, 7, 22,
# 27, 0) and the 39/49 defaults do not match.
COLOR_SGR="${ESC}\\[[0-9;]*(3[0-7]|38|4[0-7]|48|9[0-7]|10[0-7])[;m]"

# An SGR with at least one non-zero parameter — what a STYLED cell carries. `capture-pane
# -e` also writes a bare `\E[0m` where the previous row left attributes open, which is
# tmux's bookkeeping and not a style on the row.
STYLED_SGR="${ESC}\\[[0-9;]*[1-9][0-9;]*m"

raw_has_color() { LC_ALL=C grep -qaE -- "$COLOR_SGR" "$RAW" 2>/dev/null; }
row_is_styled() { printf '%s' "$1" | LC_ALL=C grep -qaE -- "$STYLED_SGR"; }
# The tap stays attached for the whole scenario (`pipe-pane -o` TOGGLES — a second
# `pipe_raw` would detach it); between runs the capture file is simply emptied.
reset_raw() { : >"$RAW"; }

# heading_row <first|last> — a committed `Heading` row with its SGR state: the H1 of the
# markdown document (the composer echoes `md`, never the word). The NO_COLOR run comes
# first on a fresh pane and reads the first hit; the control run reads the last.
heading_row() {
    if [ "$1" = last ]; then
        tm capture-pane -e -pt s -S -400 2>/dev/null | grep -F 'Heading' | tail -1
    else
        tm capture-pane -e -pt s -S -400 2>/dev/null | grep -F 'Heading' | head -1
    fi
}

# run_turn <label> — types `md`, waits for the document to land, settles.
run_turn() {
    type_ 'md'
    key Enter
    wait_all 'done.' || bad "$1: the markdown document never completed"
    settle || bad "$1: frame never settled after the turn"
}

# quit <label> — two Ctrl+C at idle exit the binary; the shell proves it by echoing a
# marker the composer could only have shown with its quotes still in it.
quit() {
    key C-c
    key C-c
    type_ "echo BACK_IN_THE_SH''ELL"
    key Enter
    if _poll_until 60 _vis_has 'BACK_IN_THE_SHELL'; then
        ok "$1: the binary exited and the shell pane is back"
    else
        bad "$1: the binary did not exit"
    fi
}

start_shell 80 24 || finish

# ------------------------------------------------------------------ A: NO_COLOR=1
pipe_raw
type_ "env NO_COLOR=1 $(iota_cmd openai fake)"
key Enter
wait_vis '❯' || {
    bad "NO_COLOR: the binary never came up in the shell pane"
    finish
}
settle || bad "NO_COLOR: startup never settled"
run_turn NO_COLOR

# The frame side: attributes may stay, colors may not (X-28).
if raw_has_color; then
    bad "NO_COLOR: a color SGR reached the terminal: $(LC_ALL=C grep -aoE -- "$COLOR_SGR" "$RAW" | head -3 | tr '\n' ' ')"
else
    ok "NO_COLOR: not one foreground or background color in the whole byte stream"
fi
# …and the attributes the frame is built from are still there (the separators are faint,
# the user block is reverse video): the gate removes colors, not the frame.
check_raw "NO_COLOR: the frame keeps faint" "${ESC}[2m" yes
check_raw "NO_COLOR: the frame keeps reverse video" "${ESC}[7m" yes
# The chat side: nothing at all (X-27).
row="$(heading_row first)"
if [ -n "$row" ] && ! row_is_styled "$row"; then
    ok "NO_COLOR: the committed heading row is bare text"
else
    bad "NO_COLOR: the committed heading row still carries SGR: $(printf '%q' "$row")"
fi
# And it is still a frame: the document rendered, the composer and the status row are there.
check_once "NO_COLOR: heading rendered" 'Heading'
check_once "NO_COLOR: list item rendered" '• one'
check_once "NO_COLOR: table border rendered" '┌─────┬─────┐'
check_frame_intact "NO_COLOR" 80
quit NO_COLOR

# ------------------------------------------------------------------ B: the control run
reset_raw
type_ "$(iota_cmd openai fake)"
key Enter
wait_vis '❯' || {
    bad "control: the binary never came up in the shell pane"
    finish
}
settle || bad "control: startup never settled"
run_turn control

if raw_has_color; then
    ok "control: the painted frame writes color SGR (the assertion above is live)"
else
    bad "control: no color SGR in a painted run — the NO_COLOR assertion would be vacuous"
fi
row="$(heading_row last)"
if [ -n "$row" ] && row_is_styled "$row"; then
    ok "control: the committed heading row is styled"
else
    bad "control: the committed heading row carries no SGR: $(printf '%q' "$row")"
fi
quit control

finish
