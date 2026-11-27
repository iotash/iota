#!/usr/bin/env bash
# L4 scenario 58 (composer-editor-flow) — Ctrl+G opens the composer's draft in the `editor:` command
# and brings it back, through the real binary and a real tty.
#
# The outcome table is pinned in-process (`repl::editor` tests over stub scripts, `keys` tests for
# row 1c, the facade tests over the real loop); what only a real terminal shows is the handoff:
#
#   58.1 a stub editor that appends a line: the draft it was handed is the composer's plus one
#        newline, the draft comes back with the line under it, the status row says `draft updated`,
#        and the draft file is gone;
#   58.2 one that exits 1: the draft is left as it was, the status row says the status;
#   58.3 one that empties the file: the draft is cleared, `draft cleared`;
#   58.4 one that holds the terminal while the pane is resized (risk 4): the main screen is the
#        editor's meanwhile, and the frame comes back at the new width with the edited draft, which
#        then submits as any draft does;
#   58.5 a command that is not there: one `⚠` line names it, the draft stays, and the alternate
#        screen never went away;
#   58.6 job control inside the editor (risk 1): an editor that stops its process group, as vim's
#        Ctrl+Z does, stops iota with it — the shell's prompt, a stopped job — and `fg` resumes both:
#        the editor finishes and the draft comes back.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

ED="$SCEN_TMP/stub-ed"
cat >"$ED" <<'EOF'
#!/bin/sh
dir=$(dirname "$0")
cp "$1" "$dir/ed-seen"
echo "stub editor has the terminal"
case "$(cat "$dir/ed-mode")" in
append) printf 'added by the editor\n' >>"$1" ;;
fail) echo 'not kept' >>"$1"; exit 1 ;;
empty) : >"$1" ;;
stop) kill -TSTP 0; printf 'after fg\n' >>"$1" ;;
hold)
    : >"$dir/ed-holding"
    while [ ! -e "$dir/ed-release" ]; do sleep 0.1; done
    printf 'after the resize\n' >>"$1"
    ;;
esac
EOF
chmod +x "$ED"
mode() { printf '%s\n' "$1" >"$SCEN_TMP/ed-mode"; }

config() {
    CONFIG_BODY="providers:
  mock: {type: openai, key: test, url: \"http://127.0.0.1:$IOTA_PORT\"}
models:
  m: mock:fake
agents:
  default: {model: m}
editor: $1"
    export CONFIG_BODY
}

status_has() { bottom_zone | grep -qF -- "$1"; }
# status_says <description> <fixed string> — the status row says it (the notice shows a few seconds).
status_says() {
    if _poll_until 50 status_has "$2"; then ok "$1"; else bad "$1: the status row says '$(bottom_zone)'"; fi
}
alt_on() { tm display-message -pt s '#{alternate_on}'; }
_alt_is_v() { [ "$(alt_on)" = "$1" ]; }
drafts_left() { find "$SCEN_HOME/.iota/editor" -type f 2>/dev/null | wc -l | tr -d ' '; }

config "$ED"
start_provider openai 80 24 || finish
settle || miss "startup never settled"

# ------------------------------------------------------------------ 58.1 appended
mode append
type_ 'first draft'
wait_draft 'first draft' || miss "the draft never showed"
key C-g
wait_draft 'added by the editor' || miss "58.1: the edited draft never came back"
status_says "58.1: the status row says the draft was updated" 'draft updated'
check "58.1: the editor was handed the draft and one newline" "$(od -c "$SCEN_TMP/ed-seen" | head -1 | tr -s ' ')" "$(printf 'first draft\n' | od -c | head -1 | tr -s ' ')"
check "58.1: the composer's first row is the draft" "$(composer_line 1)" "first draft"
check "58.1: …and its second the editor's line" "$(composer_line 2)" "added by the editor"
check "58.1: the draft file is gone" "$(drafts_left)" 0
check "58.1: the alternate screen is back" "$(alt_on)" 1

# ------------------------------------------------------------------ 58.2 a failed editor
mode fail
key C-g
status_says "58.2: the status row says the exit status" 'editor exited with status 1 — draft unchanged'
settle
check "58.2: the draft is as it was" "$(composer_block | sed 's/ *$//' | tr '\n' '|')" "first draft|added by the editor|"
check "58.2: the editor's text is not kept" "$(count_composer 'not kept')" 0

# ------------------------------------------------------------------ 58.3 emptied
mode empty
key C-g
wait_draft_gone 'first draft' || miss "58.3: the draft was not cleared"
status_says "58.3: the status row says the draft was cleared" 'draft cleared'
settle
check "58.3: one empty composer row" "$(composer_block | tr -d ' \n')" ""

# ------------------------------------------------------------------ 58.4 a resize while lent
mode hold
rm -f "$SCEN_TMP/ed-holding" "$SCEN_TMP/ed-release"
type_ 'resize me'
wait_draft 'resize me' || miss "58.4: the draft never showed"
key C-g
_poll_until 50 test -e "$SCEN_TMP/ed-holding" || miss "58.4: the editor never started"
_poll_until 30 _alt_is_v 0 || bad "58.4: the alternate screen is still up while the editor runs"
wait_vis 'stub editor has the terminal' || bad "58.4: the editor's output is not on the main screen"
tm resize-window -t s -x 70 -y 22
sleep 0.3
: >"$SCEN_TMP/ed-release"
wait_draft 'after the resize' || miss "58.4: the draft never came back after the resize"
_poll_until 30 _alt_is_v 1 || bad "58.4: the alternate screen did not come back"
settle
check "58.4: the top separator spans the new width" "$(row_width "$(frame_top)")" 70
check "58.4: the bottom separator spans the new width" "$(sep_width)" 70
check "58.4: the bottom zone is the pane's last row" "$(($(frame_bot) + 1))" "$(tm display-message -pt s '#{pane_height}')"
check "58.4: the draft is the edited one" "$(composer_block | sed 's/ *$//' | tr '\n' '|')" "resize me|after the resize|"
key Enter
wait_vis 'echo: resize me' || miss "58.4: the edited draft did not submit"
settle

# ------------------------------------------------------------------ 58.5 a command that is not there
config iota-no-such-editor
start_provider openai 80 24 || finish
settle || miss "the second start never settled"
type_ 'keep me'
wait_draft 'keep me' || miss "58.5: the draft never showed"
pipe_raw
key C-g
wait_vis '⚠ editor iota-no-such-editor: command not found' || miss "58.5: no ⚠ line names the missing command"
settle
check "58.5: the draft stays" "$(composer_line 1)" "keep me"
check_raw "58.5: the alternate screen was never left" "$(printf '\033[?1049l')" no
check "58.5: no draft file was written" "$(drafts_left)" 0

# ------------------------------------------------------------------ 58.6 Ctrl+Z inside the editor
iota_pid() {
    local pane child
    pane="$(tm display-message -pt s '#{pane_pid}')"
    for child in $(pgrep -P "$pane"); do
        case "$(ps -o comm= -p "$child")" in *iota) echo "$child"; return 0 ;; esac
    done
    return 1
}
stopped() { case "$(ps -o stat= -p "$1" 2>/dev/null)" in T*) return 0 ;; esac; return 1; }
mode stop
config "$ED"
start_shell 80 24 || finish
type_ "$(iota_cmd openai fake)"
key Enter
wait_frame || miss "58.6: the composer never came up under the shell"
_poll_until 50 eval 'pid="$(iota_pid)"; [ -n "$pid" ]' || miss "58.6: no iota under the shell"
settle
type_ 'stop me'
wait_draft 'stop me' || miss "58.6: the draft never showed"
key C-g
_poll_until 50 stopped "$pid" || miss "58.6: iota did not stop with the editor"
_poll_until 30 _alt_is_v 0 || bad "58.6: the alternate screen is up with the job stopped"
type_ 'echo "sh""ell=up"'
key Enter
_poll_until 30 _vis_has 'shell=up' || miss "58.6: the shell does not have the terminal"
ok "58.6: the shell has the terminal with the job stopped"
type_ 'fg'
key Enter
wait_draft 'after fg' || miss "58.6: the draft never came back after fg"
_poll_until 30 _alt_is_v 1 || bad "58.6: the alternate screen did not come back"
check "58.6: the draft is the edited one" "$(composer_block | sed 's/ *$//' | tr '\n' '|')" "stop me|after fg|"
status_says "58.6: the status row says the draft was updated" 'draft updated'
key C-d
gone() { ! kill -0 "$pid" 2>/dev/null; }
_poll_until 50 gone || miss "58.6: Ctrl+D did not end the run"

finish
