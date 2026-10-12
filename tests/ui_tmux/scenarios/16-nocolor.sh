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
# The control run shares the pane, and so the scrollback, with the NO_COLOR run: every
# marker it waits for is already there once, and every row it reads back exists already
# once, bare. So it waits for one MORE copy of the document's last line (`wait_all_more`)
# and reads the SECOND heading row by ordinal — never `tail -1`, which handed back the
# NO_COLOR run's bare row whenever the control document had not landed yet (CI
# 34870469581, the macos leg: `settle` had called a turn in progress settled).
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

# heading_row <n> — the n-th committed `Heading` row, with its SGR state: the H1 of the
# markdown document (the composer echoes `md`, never the word). The NO_COLOR run comes
# first on a fresh pane and reads the first; the control run reads the second, and gets
# nothing — not the first run's row — when its own document is not there yet.
heading_row() {
    tm capture-pane -e -pt s -S -400 2>/dev/null | grep -F 'Heading' | sed -n "${1}p"
}

# run_turn <label> — types `md`, waits for THIS turn's document to land (one more `done.`
# than the scrollback held before it), settles.
run_turn() {
    local before
    before="$(count_all 'done.')"
    type_ 'md'
    key Enter
    wait_all_more 'done.' "$before" || bad "$1: the markdown document never completed"
    settle || bad "$1: frame never settled after the turn"
}

# iota_running — the pane's shell has an `iota` child (scenario 29 reads the pane the same way).
iota_running() {
    local pane child
    pane="$(tm display-message -pt s '#{pane_pid}' 2>/dev/null)"
    [ -n "$pane" ] || return 1
    for child in $(pgrep -P "$pane" 2>/dev/null); do
        case "$(ps -o comm= -p "$child" 2>/dev/null)" in
            *iota) return 0 ;;
        esac
    done
    return 1
}
_iota_gone() { ! iota_running; }

# launch <label> <command> — starts the binary from the shell and waits for THIS run's banner: one
# more `ι> iota` than the scrollback held before it. Not `wait_vis '❯'`: the composer row the run
# before left on screen satisfies that at once, and a message typed before the new binary owns the
# terminal is lost — the control run's `md` was, under load, and its document never came.
launch() {
    local before
    before="$(count_all 'ι> iota')"
    type_ "$2"
    key Enter
    if ! wait_all_more 'ι> iota' "$before"; then
        bad "$1: the binary never came up in the shell pane"
        finish
    fi
    settle || bad "$1: startup never settled"
}

QUITS=0
# quit <label> — two Ctrl+C at idle exit the binary; the shell proves it by echoing a
# marker the composer could only have shown with its quotes still in it.
#
# The marker is typed only once the process is GONE. Until its loop takes `Quit` the binary
# still reads the terminal, so a line typed behind the Ctrl+C is the binary's to swallow, not
# the shell's to run: the exit takes ~23 ms after the first Ctrl+C here and the marker landed
# ~25 ms after it — a race won by a few milliseconds, lost on CI 38201248863 (ubuntu) as "did
# not exit" by a binary that had exited (the control run after it started in that same shell).
quit() {
    iota_running || bad "$1: no iota under the shell to quit"
    key C-c
    key C-c
    if ! _poll_until 60 _iota_gone; then
        bad "$1: the binary did not exit"
        return
    fi
    # Numbered per call, so a later quit cannot pass on the marker an earlier one left on screen.
    QUITS=$((QUITS + 1))
    type_ "echo BACK_IN_THE_SH''ELL_$QUITS"
    key Enter
    if _poll_until 60 _vis_has "BACK_IN_THE_SHELL_$QUITS"; then
        ok "$1: the binary exited and the shell pane is back"
    else
        bad "$1: the binary exited but the shell never answered"
    fi
}

start_shell 80 24 || finish

# ------------------------------------------------------------------ A: NO_COLOR=1
pipe_raw
launch NO_COLOR "env NO_COLOR=1 $(iota_cmd openai fake)"
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
row="$(heading_row 1)"
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
# A surface is still navigable without color (§8c.4, §8c.5): the cursor row keeps its `▸`
# marker, the combo's field keeps its `❯` edge and placeholder — and the surface painted no
# color either.
type_ '/model'
key Enter
wait_vis 'Enter select' || bad "NO_COLOR: the /model combo never opened"
settle || bad "NO_COLOR: the combo never settled"
check "NO_COLOR: the surface keeps its ▸ cursor marker" "$(count_vis '▸ fake [m] (current)')" 1
check "NO_COLOR: the combo field keeps its placeholder" "$(count_vis 'model name (e.g. gpt-4o)')" 1
check "NO_COLOR: …behind a ❯ edge of its own (the composer's and the field's)" "$(cap | grep -c '^❯' | tr -d ' ')" 2
if raw_has_color; then
    bad "NO_COLOR: the surface painted a color"
else
    ok "NO_COLOR: the surface painted no color"
fi
key Escape
wait_gone 'Enter select' || bad "NO_COLOR: the combo did not close on ESC"
settle || bad "NO_COLOR: frame never settled after the combo"
quit NO_COLOR

# ------------------------------------------------------------------ B: the control run
reset_raw
launch control "$(iota_cmd openai fake)"
run_turn control

if raw_has_color; then
    ok "control: the painted frame writes color SGR (the assertion above is live)"
else
    bad "control: no color SGR in a painted run — the NO_COLOR assertion would be vacuous"
fi
# The document landed a second time — the row below is this run's, not the first run's.
check "control: the heading rendered once more" "$(count_all 'Heading')" 2
row="$(heading_row 2)"
if [ -n "$row" ] && row_is_styled "$row"; then
    ok "control: the committed heading row is styled"
else
    bad "control: the committed heading row carries no SGR: $(printf '%q' "$row")"
fi
quit control

# ------------------------------------------------------------------ C: TERM=dumb (§8c.7)
# The other spelling of the same switch (`app/color.rs`): the terminal's TERM says it cannot
# interpret escapes, so the chat side is bare and the frame carries attributes and no color —
# exactly run A, read the same way (the THIRD heading row is this run's).
reset_raw
launch TERM=dumb "env TERM=dumb $(iota_cmd openai fake)"
run_turn TERM=dumb
if raw_has_color; then
    bad "TERM=dumb: a color SGR reached the terminal: $(LC_ALL=C grep -aoE -- "$COLOR_SGR" "$RAW" | head -3 | tr '\n' ' ')"
else
    ok "TERM=dumb: not one foreground or background color in the whole byte stream"
fi
check_raw "TERM=dumb: the frame keeps faint" "${ESC}[2m" yes
check_raw "TERM=dumb: the frame keeps reverse video" "${ESC}[7m" yes
check "TERM=dumb: the heading rendered once more" "$(count_all 'Heading')" 3
row="$(heading_row 3)"
if [ -n "$row" ] && ! row_is_styled "$row"; then
    ok "TERM=dumb: the committed heading row is bare text"
else
    bad "TERM=dumb: the committed heading row still carries SGR: $(printf '%q' "$row")"
fi
check_frame_intact "TERM=dumb" 80
quit TERM=dumb

finish
