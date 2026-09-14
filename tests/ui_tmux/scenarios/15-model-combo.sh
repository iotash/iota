#!/usr/bin/env bash
# L4 scenario 15 (MIGRATION-ROADMAP Phase 1b §10) — `/model`'s combo box.
#
# The combo is the one panel kind whose input row is ALWAYS open, which makes it the one place
# where the surface's key ladder routes letters, arrows and Enter differently from every other
# panel. L1 proves the routing (`src/ui/surface/tests.rs`) and L3 proves what the command does
# with the commit (`tests/repl/commands.rs`); only here do they meet a real terminal — a real
# Escape byte, a real arrow sequence, a real frame under the composer.
#
# The config is the scenario's subject, so it brings its own: an agent whose candidate set mixes
# a `models:` entry, the mock's `provider:*` wildcard and a SECOND wildcard pointed at a closed
# port. That last one is the point of the whole design — one source failing must cost exactly its
# own rows, and the picker must still be a picker.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

CONFIG_BODY="providers:
  mock: {type: openai, key: test, url: \"http://127.0.0.1:$IOTA_PORT\"}
  dead: {type: openai, key: test, url: \"http://127.0.0.1:9\"}
models:
  m: mock:fake
agents:
  default:
    models: [m, \"mock:*\", \"dead:*\"]"
export CONFIG_BODY

start_provider openai 80 30 || finish
settle || bad "startup never settled"

before="$(cap)"

# ------------------------------------------------------------------ A: the candidate set
type_ '/model'
key Enter
wait_vis 'Enter select' || bad "the model combo never opened"
settle || bad "the combo never settled"

comp="$(composer_row)"
first="$(row_of 'fake (current)')"
if [ -n "$comp" ] && [ -n "$first" ] && [ "$first" -gt "$comp" ]; then
    ok "the combo renders BELOW the composer (composer row $comp, first row $first)"
else
    bad "the combo is not below the composer (composer=$comp first=$first)"
    cap
fi
check "the mock's listing is offered" "$(count_vis 'gemini-pro')" 1
# Matched as a whole row: `gpt-4o` also appears inside the input row's placeholder.
check "…and so is the rest of it" "$(cap | grep -cE '^ +gpt-4o *$' | tr -d ' ')" 1
check "the candidate the entry names is the current model" "$(count_vis 'fake (current)')" 1
# THE law of a mixed list: the dead endpoint costs its own rows and nothing else.
if cap | grep -qF 'dead:'; then
    ok "the failed source is one line in the panel's prompt row"
else
    bad "the dead provider's failure is not reported"
    cap
fi
check "the input row carries its placeholder" "$(count_vis 'model name (e.g. gpt-4o)')" 1
check_frame_intact "with the combo open" 80

key Escape
wait_gone 'Enter select' || bad "the combo did not close on ESC"
settle || bad "frame never settled after cancel"
if [ "$(cap)" = "$before" ]; then
    ok "ESC restores the pane exactly (no switch, no ghost rows)"
else
    bad "pane differs after a cancelled combo"
    diff <(printf '%s\n' "$before") <(printf '%s\n' "$(cap)") | head -20
fi

# ------------------------------------------------------------------ B: typing filters
type_ '/model'
key Enter
wait_vis 'Enter select' || bad "the combo never reopened"
type_ 'gpt'
wait_vis 'as typed' || bad "the typed row never appeared"
settle || bad "the filter never settled"
check "the filter narrowed the list" "$(count_vis 'gemini-pro')" 0
check "…to the row that matches" "$(count_vis 'gpt-4o')" 1
check "the hint counts what the filter keeps" "$(count_vis '1 of 3')" 1

# ------------------------------------------------------------------ C: submitting the text
# Ctrl+U clears the field (the shared emacs edit set); a name nothing matches leaves the typed
# row alone in the list, with the cursor already on it — Enter then means "use what I typed".
key C-u
type_ 'my-own-model'
wait_vis 'use "my-own-model" as typed' || bad "the typed row does not carry the text"
check "a name nothing matches leaves the list to the typed row" "$(count_vis 'gpt-4o')" 0
check "Enter is unambiguous, and says so" "$(count_vis 'Enter use typed')" 1
key Enter
wait_all 'Model switched to my-own-model' || bad "the typed text never committed"
settle || bad "frame never settled after the switch"
check_once "the switch is announced exactly once" 'Model switched to my-own-model'
check "no ghost combo rows after close" "$(count_vis 'Enter select')" 0
check_frame_intact "after committing the typed text" 80

# ------------------------------------------------------------------ D: keyboard navigation
# A model reached from outside the candidate set is still the CURRENT row (the untouched-tab law),
# so ↓ lands on the first listed candidate.
type_ '/model'
key Enter
wait_vis 'my-own-model (current)' || bad "the combo does not carry the current model"
settle || bad "the combo never settled"
key Down
key Enter
wait_all 'Model switched to fake' || bad "arrow navigation never reached the first candidate"
settle || bad "frame never settled after the second switch"
check "the status row followed the model" "$(status_model)" '  fake'
check_frame_intact "after the arrow-driven switch" 80

finish
