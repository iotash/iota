#!/usr/bin/env bash
# L4 scenario 52 (theme-live, DIVERGENCES X-108) — the terminal's background, asked again while
# iota runs, through a REAL terminal and the REAL crossterm reader.
#
# The loop never reads the tty for the answer: it writes OSC 11 and finds the reply among the key
# events crossterm 0.29 makes of it (`src/ui/runtime/requery.rs`). The vt100 units script those
# events; only this scenario shows that crossterm really produces them from a terminal's bytes —
# the reply is taken (the shade flips), and not one character of it is typed into the composer.
#
# tmux answers OSC 11 with the pane's own background once one is set (`select-pane -P`), so the
# "OS switch" here is that style changing from dark to light. Focus is the trigger: tmux reports
# none unless asked to, so the scenario types the report itself (`CSI I`, as hex).
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

# The current list row's shade (`theme::row_bg`) as `capture-pane -e` prints it.
DARK='48;5;237'
LIGHT='48;5;253'
shade() { tm capture-pane -e -pt s 2>/dev/null | grep -cF -- "$1" | tr -d ' '; }
_shaded() { [ "$(shade "$1")" -gt 0 ]; }
focus_in() { tm send-keys -t s -H 1b 5b 49; }
queries() { LC_ALL=C grep -aoF -- ']11;?' "$RAW" 2>/dev/null | wc -l | tr -d ' '; }
_asked() { [ "$(queries)" -ge "$1" ]; }

start_shell 80 24 || finish
tm select-pane -t s -P 'bg=#101010'
pipe_raw
type_ "$(iota_cmd openai fake)"
key Enter
wait_frame || {
    miss "the binary never came up in the shell pane"
    finish
}
settle || miss "startup never settled"

# ------------------------------------------------------------------ A: a dark terminal
type_ '/'
key Down
_poll_until 60 _shaded "$DARK" || miss "the list's current row never took the dark shade"
check "dark: no light shade on screen" "$(shade "$LIGHT")" 0
before="$(queries)"
sleep 0.5 # the keyboard has been quiet for longer than KEY_QUIET

focus_in
if _poll_until 60 _asked "$((before + 1))"; then ok "focus asked the terminal for its background (OSC 11)"; else miss "focus asked nothing"; fi
sleep 0.5
check "a dark answer changes nothing" "$(shade "$LIGHT")" 0

# ------------------------------------------------------------------ B: the switch to light
tm select-pane -t s -P 'bg=#f5f5f5'
focus_in
_poll_until 60 _asked "$((before + 2))" || miss "the second focus asked nothing"
if _poll_until 60 _shaded "$LIGHT"; then ok "the light answer re-shaded the current row"; else miss "the row never took the light shade"; fi
check "…and the dark shade is gone" "$(shade "$DARK")" 0
check "not one character of the reply was typed" "$(composer_block | grep -cE 'rgb|11;' | tr -d ' ')" 0

# ------------------------------------------------------------------ C: and back, with typing on
tm select-pane -t s -P 'bg=#101010'
focus_in
_poll_until 60 _asked "$((before + 3))" || miss "the third focus asked nothing"
_poll_until 60 _shaded "$DARK" || miss "the row never went back to the dark shade"
key Escape
type_ 'still typing'
wait_draft 'still typing' || miss "the keys after the reply never reached the composer"
check "the composer holds what was typed and nothing else" \
    "$(composer_block | grep -cE 'rgb|11;' | tr -d ' ')" 0

finish
