#!/usr/bin/env bash
# L4 scenario 59 (composer-editor-picker) — with no `editor:`, Ctrl+G asks which editor, once.
#
# The rows, the retry and the write are pinned in-process (`repl::editor` tests over stub scripts and
# the scripted facade); what only the real binary shows is the list where it is drawn and the file it
# leaves behind:
#
#   59.1 a fresh HOME whose config has no `editor:` and `$EDITOR` naming a stub: Ctrl+G opens the
#        `Editor` list BELOW the composer, its first row — preselected — the environment's editor as
#        it would be stored, labelled where it came from, `Other…` last;
#   59.2 `Other…` with a command that is not there: one `⚠` line names it, and the list comes back
#        with the cursor on `Other…`;
#   59.3 the environment's row: the stub edits the draft, and `~/.iota.yaml` then holds
#        `editor: stub-ed`, every line it had kept;
#   59.4 the second Ctrl+G runs the editor straight away — no list.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

mkdir -p "$SCEN_TMP/bin"
ED="$SCEN_TMP/bin/stub-ed"
cat >"$ED" <<'SH'
#!/bin/sh
printf 'edited by the stub\n' >>"$1"
SH
chmod +x "$ED"

CONFIG_BODY="providers:
  mock: {type: openai, key: test, url: \"http://127.0.0.1:$IOTA_PORT\"}
models:
  m: mock:fake
agents:
  default: {model: m}"
export CONFIG_BODY
IOTA_ENV="-u VISUAL PATH=$SCEN_TMP/bin:$PATH EDITOR=stub-ed"
export IOTA_ENV
CFG="$SCEN_HOME/.iota.yaml"

ENV_ROW='stub-ed · from $EDITOR'
# The row the list's cursor is on (`▸ …`), the marker and the padding dropped.
cursor_row() { cap | sed -n 's/^ *▸ //p' | sed 's/ *$//' | head -1; }
_cursor_is() { [ "$(cursor_row)" = "$1" ]; }
# press <key> until the cursor is on <row> (at most 20 presses).
press_until() {
    local i
    for i in $(seq 20); do
        _cursor_is "$2" && return 0
        key "$1"
        sleep 0.1
    done
    _cursor_is "$2"
}

start_provider openai 100 30 || finish
settle || miss "startup never settled"
check "the config starts with no editor:" "$(grep -c '^editor:' "$CFG")" 0

# ------------------------------------------------------------------ 59.1 the list
type_ 'pick me'
wait_draft 'pick me' || miss "59.1: the draft never showed"
key C-g
wait_vis "$ENV_ROW" || miss "59.1: the list never showed the environment's editor"
settle
check "59.1: the environment's row is preselected" "$(cursor_row)" "$ENV_ROW"
env_at="$(row_of "$ENV_ROW")"
other_at="$(row_of 'Other…')"
composer_at="$(row_of 'pick me')"
if [ "$env_at" -gt "$composer_at" ]; then ok "59.1: the list is below the composer"; else bad "59.1: the list (row $env_at) is not below the composer (row $composer_at)"; fi
if [ "$other_at" -gt "$env_at" ]; then ok "59.1: Other… comes after the environment's row"; else bad "59.1: Other… (row $other_at) is not after row $env_at"; fi
if [ "$env_at" -gt "$(frame_bot)" ]; then ok "59.1: the list is the bottom zone"; else bad "59.1: the list is above the lower separator"; fi

# ------------------------------------------------------------------ 59.2 a custom command that is not there
press_until Down 'Other…' || miss "59.2: the cursor never reached Other…"
key Enter
sleep 0.2
type_ 'iota-no-such-editor --wait'
key Enter
wait_vis '⚠ editor iota-no-such-editor: command not found' || miss "59.2: no ⚠ line names the missing command"
wait_vis "$ENV_ROW" || miss "59.2: the list did not come back"
settle
check "59.2: the list comes back on Other…" "$(cursor_row)" 'Other…'
check "59.2: nothing was written" "$(grep -c '^editor:' "$CFG")" 0
check "59.2: the draft stays" "$(composer_line 1)" "pick me"

# ------------------------------------------------------------------ 59.3 the environment's editor
press_until Up "$ENV_ROW" || miss "59.3: the cursor never came back to the environment's row"
key Enter
wait_draft 'edited by the stub' || miss "59.3: the edited draft never came back"
wait_gone "$ENV_ROW" || bad "59.3: the list is still up"
check "59.3: the draft is the edited one" "$(composer_block | sed 's/ *$//' | tr '\n' '|')" "pick me|edited by the stub|"
check "59.3: the config holds the choice" "$(grep '^editor:' "$CFG")" "editor: stub-ed"
check "59.3: …and still everything it had" "$(grep -c '^  default: {model: m}' "$CFG")" 1
wait_all "editor: stub-ed — saved to" || bad "59.3: no line says where the choice was saved"

# ------------------------------------------------------------------ 59.4 the second Ctrl+G
key C-g
_poll_until 120 eval '[ "$(count_composer "edited by the stub")" = 2 ]' || miss "59.4: the second Ctrl+G did not run the editor"
check "59.4: no list the second time" "$(count_vis "$ENV_ROW")" 0
key Enter
wait_vis 'echo: pick me' || miss "59.4: the edited draft did not submit"

finish
