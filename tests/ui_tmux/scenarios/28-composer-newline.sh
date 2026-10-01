#!/usr/bin/env bash
# L4 #28 (DIVERGENCES X-65) — a newline in the composer, through the real byte path.
#
# The unit tests build `KeyEvent`s; only a pty proves the decode: Ctrl+J is LF, which crossterm
# reads as `Char('j')` + CONTROL in raw mode only (cooked, it is Enter). Alt+Enter is ESC CR.
# Shift+Enter cannot be typed into tmux — `send-keys S-Enter` arrives as a bare CR under every
# `extended-keys` setting — so it is injected as the CSI-u bytes a terminal that reports SHIFT
# sends (`ESC[13;2u`, `send-keys -H`). That proves iota's decode and routing, not that any
# terminal sends it.
#
# For each key: the draft grows to two composer rows and nothing is submitted, then Enter sends
# ONE message carrying both lines — the user block echoes them as one `❯` block, the model's
# echo starts with the first, and no second message echoes the second line on its own (a key
# that submitted would leave exactly that). Then the queue: a two-line item queued mid-turn is
# ONE frame row with ` ⏎ ` for the break, and ESC folds it back as a two-row draft.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

start 80 24 || finish
settle || bad "startup never settled"

# newline_turn <label> <first> <second> <key args…> — type <first>, press the key, type
# <second>, assert the unsent two-row draft, press Enter, assert ONE two-line message.
newline_turn() {
    local label="$1" one="$2" two="$3"
    shift 3
    type_ "$one"
    key "$@"
    type_ "$two"
    wait_vis "  $two" || bad "$label: the second line never rendered"
    settle || bad "$label: composer never settled"
    # Nothing was submitted: no reply, and line one exists only as the composer's own row.
    # (Not `hist_size`: once the screen is full, the composer's second row pushes a transcript
    # row into the scrollback by itself.)
    check "$label: nothing reached the model" "$(count_all "echo: $one")" 0
    check "$label: line one is only the draft" "$(count_all "❯ $one")" 1
    check "$label: two composer rows" "$(composer_block | wc -l | tr -d ' ')" 2
    check "$label: row 1 is the prompt" "$(count_composer "❯ $one")" 1
    check "$label: row 2 is a continuation" "$(count_composer "  $two")" 1
    check "$label: one prompt" "$(count_composer '❯')" 1

    key Enter
    wait_all "echo: $one" || bad "$label: the message never reached the model"
    settle || bad "$label: frame never settled after the submit"
    check_once "$label: echo row 1" "❯ $one"
    check "$label: echo row 2 is a continuation, not a second prompt" "$(count_all "❯ $two")" 0
    check "$label: no second message for line two" "$(count_all "echo: $two")" 0
    # The user block and the model's echo both carry line two; counted after the reply landed,
    # so the composer's own row is gone.
    check "$label: the model saw line two" "$(count_all "$two")" 2
    check "$label: the composer collapsed" "$(composer_block | wc -l | tr -d ' ')" 1
}

newline_turn "Ctrl+J" 'cj line one' 'cj line two' C-j
newline_turn "LF byte" 'lf line one' 'lf line two' -H 0a
newline_turn "Alt+Enter" 'alt line one' 'alt line two' M-Enter
newline_turn "Shift+Enter (CSI u)" 'sh line one' 'sh line two' -H 1b 5b 31 33 3b 32 75

# The queue: a two-line item typed while a turn streams is one `»` row, and ESC folds it back.
type_ 'stream 60'
key Enter
wait_vis 'l#05 line' || bad "the stream never started"
type_ 'q one'
key C-j
type_ 'q two'
key Enter
wait_vis '» q one ⏎ q two' || bad "the two-line queue row never rendered"
check "the queued item is ONE row" "$(count_vis 'q two')" 1
check_frame_intact "with a two-line item queued" 80
key Escape
wait_all 'Interrupted.' || bad "ESC never reached the loop"
settle || bad "frame never settled after the interrupt"
check "the queue row is gone (folded)" "$(count_vis '» q one')" 0
check "fold row 1" "$(count_composer '❯ q one')" 1
check "fold row 2" "$(count_composer '  q two')" 1

finish
