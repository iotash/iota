#!/usr/bin/env bash
# L4 scenario 17 (docs/TUI-VERIFY.md §5) — the window title and the title stack.
#
# Three bytes-and-a-readback claims, none of which `capture-pane` can see: the title stack is
# pushed (`CSI 22;0 t`, `cmd/interactive/title.rs`) on plain stdout BEFORE the loop sets the
# first title (OSC 0, crossterm's `SetTitle` via `ui/runtime/osc.rs`); tmux reads the title
# back (`#{pane_title}`), so a CJK session title landed by the async title pass is provably
# the bytes the terminal saw; and a clean exit pops the stack (`CSI 23;0 t`) only AFTER the
# loop released the terminal (mode 1004 off) — the teardown order of `TUI_DESIGN` §8.4.
#
# tmux keeps a title stack of its own (screen_push_title / screen_pop_title), so the pop is
# also readable: the pane's title after the exit is what it was before the binary started.
# §5.3 (a crash leaves the title stale) and §5.4 (a terminal that ignores the stack) stay
# manual — the first needs a kill nobody wants in CI, the second a terminal that is not tmux.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

ESC="$(printf '\033')"
BEL="$(printf '\007')"

start_shell 80 24 || finish
# The title the shell pane carries before the binary — what the pop must restore.
_poll_until 30 sh -c '[ -n "$1" ]' _ "$(pane_title)"
shell_title="$(pane_title)"
pipe_raw

type_ "$(iota_cmd openai fake)"
key Enter
wait_vis '❯' || {
    bad "the binary never came up in the shell pane"
    finish
}
settle || bad "startup never settled"

# ------------------------------------------------------------------ A: the start (§5.1)
check_raw "the title stack is pushed (CSI 22;0 t)" "${ESC}[22;0t" yes
check_raw "the window title is set by OSC 0 — the app's name while the session has none" "${ESC}]0;iota${BEL}" yes
push_at="$(raw_offset "${ESC}[22;0t")"
title_at="$(raw_offset "${ESC}]0;")"
if [ -n "$push_at" ] && [ -n "$title_at" ] && [ "$push_at" -lt "$title_at" ]; then
    ok "the push precedes the first title (byte $push_at < $title_at)"
else
    bad "the push does not precede the first title (push=$push_at title=$title_at)"
fi
check "tmux reads the title back" "$(pane_title)" "iota"

# ------------------------------------------------------------------ B: a CJK session title (§5.1)
# The first turn seeds the async title pass; the mock's `title:` directive makes it answer the
# word after the colon, so the session is named `你好世界` and the tab must follow.
type_ 'title:你好世界 hello'
key Enter
wait_all 'echo: title:你好世界 hello' || bad "the turn never completed"
_poll_until 100 pane_title_is '你好世界' || bad "the session title never reached the tab (title now: $(pane_title))"
check "the tab carries the composed CJK title" "$(pane_title)" '你好世界'
check_raw "…as one OSC 0 with the UTF-8 bytes intact" "${ESC}]0;你好世界${BEL}" yes
settle || bad "frame never settled after the turn"
check_frame_intact "with the session titled" 80

# ------------------------------------------------------------------ C: a clean exit (§5.2)
key C-c
key C-c
_poll_until 60 raw_has "${ESC}[23;0t" || bad "the title stack was never popped"
check_raw "exit pops the title stack (CSI 23;0 t)" "${ESC}[23;0t" yes
focus_off="$(raw_offset "${ESC}[?1004l")"
pop_at="$(raw_offset "${ESC}[23;0t")"
if [ -n "$focus_off" ] && [ -n "$pop_at" ] && [ "$focus_off" -lt "$pop_at" ]; then
    ok "the pop follows the loop's release of the terminal (mode 1004 off at byte $focus_off < $pop_at)"
else
    bad "the pop does not follow the terminal's release (1004l=$focus_off pop=$pop_at)"
fi
if alive; then
    ok "the shell pane survives the binary"
else
    bad "the shell pane died with the binary"
    finish
fi
_poll_until 30 pane_title_is "$shell_title" || true
check "the pane's title is what it was before the binary (tmux's own stack)" "$(pane_title)" "$shell_title"

finish
