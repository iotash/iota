#!/usr/bin/env bash
# L4 scenario 18 (docs/TUI-VERIFY.md §7.1, §7.2) — the host channels tmux CAN reach.
#
# Scenario 14 pinned the two easy progress states (busy during a turn, cleared at idle) and
# left three claims to a human: the WARNING state at an approval prompt, the desktop
# notification that is gated on focus, and a quit straight out of a turn. All three are bytes
# on `pipe-pane`:
#
#   * an unsandboxed `shell` call without `auto_run` asks first, and asking is
#     `State::NeedsInput` → `OSC 9;4;4;100` (`repl/turn/approval.rs`, `host/ansi.rs`);
#   * "tmux never blurs a pane" is true of tmux's own client, but the loop reads focus off the
#     same input bytes any terminal would send: `ESC [ O` (lost) and `ESC [ I` (gained), which
#     `send-keys -H` writes into the pane like a keystroke. With those, the gate is testable in
#     both directions: silent while focused, `OSC 9;<first line of the answer>` after a blur,
#     silent again after the refocus (`ui/runtime/event_loop.rs::drain_notify`);
#   * Ctrl+C mid-stream cancels the turn (bar cleared, `OSC 9;4;0`) and the next one exits with
#     focus reporting turned off — the mid-turn quit of §7.1, in the only shape it can take.
#
# The agent enables the `shell` set with the sandbox off and NO auto_run — the one
# configuration that asks. §7.3 (`notify: false`) and §7.4 (cmux) stay where they were.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

ESC="$(printf '\033')"
BEL="$(printf '\007')"

CONFIG_BODY="providers:
  mock: {type: openai, key: test, url: \"http://127.0.0.1:$IOTA_PORT\"}
models:
  m: mock:fake
agents:
  default:
    model: m
    choices: [m, \"mock:*\"]
    tools: {shell: {sandbox: off}}"
export CONFIG_BODY

# focus <lost|gained> — the focus-report bytes a terminal sends, written as raw input.
focus() {
    case "$1" in
        lost) key -H 1b 5b 4f ;;
        gained) key -H 1b 5b 49 ;;
    esac
}

# turn <text> — one echo turn, waited for and settled.
turn() {
    type_ "$1"
    key Enter
    wait_all "echo: $1" || bad "the turn '$1' never completed"
    settle || bad "frame never settled after '$1'"
}

start_shell 80 24 || finish
pipe_raw
type_ "$(iota_cmd openai fake)"
key Enter
wait_vis '❯' || {
    bad "the binary never came up in the shell pane"
    finish
}
settle || bad "startup never settled"

# ------------------------------------------------------------------ A: the approval prompt (§7.1)
type_ 'runfg:echo approved'
key Enter
wait_vis 'allow?' || bad "the approval prompt never opened"
wait_raw ']9;4;4;100' || bad "the approval prompt did not put the progress bar in its warning state"
check_raw "an approval prompt sets the warning state (OSC 9;4;4;100)" ']9;4;4;100' yes
check "the prompt names the call" "$(count_vis 'wants to modify files')" 1
key Enter
wait_all 'ran: approved' || bad "the approved call never ran"
settle || bad "frame never settled after the approved call"
check_raw "…and the turn went busy again after the answer (OSC 9;4;3)" ']9;4;3' yes
check_once "the call ran once" 'ran: approved'
check_frame_intact "after the approval" 80

# ------------------------------------------------------------------ B: focus-gated notification (§7.2)
turn 'focused hello'
check_raw "a turn finished while FOCUSED rings no notification" "${ESC}]9;echo: focused hello" no

focus lost
turn 'blurred hello'
wait_raw "${ESC}]9;echo: blurred hello${BEL}${BEL}" || bad "no notification after a blur"
check_raw "a turn finished while UNFOCUSED writes OSC 9 with the answer's first line" "${ESC}]9;echo: blurred hello${BEL}${BEL}" yes

focus gained
turn 'refocused hello'
check_raw "a turn finished after the REFOCUS is silent again" "${ESC}]9;echo: refocused hello" no
check "the gate did not disturb the frame" "$(count_composer '❯')" 1

# ------------------------------------------------------------------ C: a quit out of a turn (§7.1)
: >"$RAW"
type_ 'stream 60'
key Enter
wait_vis 'l#05 line' || bad "the stream never started"
wait_raw ']9;4;3' || bad "the stream did not set the bar busy"
key C-c
wait_all 'Interrupted.' || bad "Ctrl+C did not cancel the stream"
wait_raw ']9;4;0' || bad "the cancelled turn did not clear the bar"
check_raw "the cancelled turn clears the bar (OSC 9;4;0)" ']9;4;0' yes
key C-c
_poll_until 60 raw_has '[?1004l' || bad "the exit never turned focus reporting off"
clear_at="$(raw_offset ']9;4;0')"
off_at="$(raw_offset '[?1004l')"
if [ -n "$clear_at" ] && [ -n "$off_at" ] && [ "$clear_at" -lt "$off_at" ]; then
    ok "the bar is cleared before focus reporting goes off (byte $clear_at < $off_at)"
else
    bad "the bar was not cleared before the exit (clear=$clear_at off=$off_at)"
fi
if alive; then
    ok "the shell pane survives the binary"
else
    bad "the shell pane died with the binary"
fi

finish
