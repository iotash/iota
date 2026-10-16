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
    model: m
    choices: [m, \"mock:*\", \"dead:*\"]"
export CONFIG_BODY

cursor_is() { [ "$(cursor_xy)" = "$1" ]; }

start_provider openai 80 30 || finish
settle || miss "startup never settled"

before="$(cap)"

# ------------------------------------------------------------------ A: the candidate set
type_ '/model'
key Enter
wait_vis 'Enter select' || miss "the model combo never opened"
settle || miss "the combo never settled"

comp="$(composer_row)"
# The set names two endpoints (`dead:*` too, though it lists nothing), so every row says which one it is
# on, and the entry's row carries the name it is chosen by.
first="$(row_of 'mock:fake [m] (current)')"
if [ -n "$comp" ] && [ -n "$first" ] && [ "$first" -gt "$comp" ]; then
    ok "the combo renders BELOW the composer (composer row $comp, first row $first)"
else
    bad "the combo is not below the composer (composer=$comp first=$first)"
    cap
fi
check "the mock's listing is offered" "$(count_vis 'gemini-pro')" 1
# Matched as a whole row: `gpt-4o` also appears inside the input row's placeholder.
check "…and so is the rest of it" "$(cap | grep -cE '^ +mock:gpt-4o *$' | tr -d ' ')" 1
check "the candidate the entry names is the current model" "$(count_vis 'mock:fake [m] (current)')" 1
# THE law of a mixed list: the dead endpoint costs its own rows and nothing else.
if cap | grep -qF 'dead:'; then
    ok "the failed source is one line in the panel's prompt row"
else
    bad "the dead provider's failure is not reported"
    cap
fi
check "the input row carries its placeholder" "$(count_vis 'model name (e.g. gpt-4o)')" 1
# The field is open from the first frame (§8b.1): the REAL cursor sits in it, immediately after
# its `❯` — not in the composer above, not on the list.
field_row="$(row_of 'model name (e.g. gpt-4o)')"
check "the real cursor sits in the field, right after its ❯" "$(cursor_xy)" "1 $((field_row - 1))"
check_frame_intact "with the combo open" 80

key Escape
wait_gone 'Enter select' || miss "the combo did not close on ESC"
settle || miss "frame never settled after cancel"
if [ "$(cap)" = "$before" ]; then
    ok "ESC restores the pane exactly (no switch, no ghost rows)"
else
    bad "pane differs after a cancelled combo"
    diff <(printf '%s\n' "$before") <(printf '%s\n' "$(cap)") | head -20
fi

# ------------------------------------------------------------------ B: typing filters
type_ '/model'
key Enter
wait_vis 'Enter select' || miss "the combo never reopened"
type_ 'gpt'
wait_vis 'as typed' || miss "the typed row never appeared"
settle || miss "the filter never settled"
check "the filter narrowed the list" "$(count_vis 'gemini-pro')" 0
check "…to the row that matches" "$(count_vis 'gpt-4o')" 1
check "the hint counts what the filter keeps" "$(count_vis '1 of 3')" 1

# ------------------------------------------------------------------ B2: arrows vs ←→ (§8b.4)
# The one thing that shows the difference is the REAL cursor: ↑↓ walk the rows and leave it in
# the field, ←→ (and Ctrl+A/E) move it inside the field's text.
read -r fx fy <<<"$(cursor_xy)"
key Down
settle || miss "the combo never settled after ↓"
check "↓ keeps the field's text" "$(count_vis '❯gpt')" 1
check "…and the real cursor stays in the field" "$(cursor_xy)" "$fx $fy"
key Up
settle || miss "the combo never settled after ↑"
key Left
_poll_until 20 cursor_is "$((fx - 1)) $fy" || true
check "← moves the text cursor one column back inside the field" "$(cursor_xy)" "$((fx - 1)) $fy"
key C-a
_poll_until 20 cursor_is "$((fx - 3)) $fy" || true
check "Ctrl+A puts it at the field's start" "$(cursor_xy)" "$((fx - 3)) $fy"
key C-e
_poll_until 20 cursor_is "$fx $fy" || true
check "Ctrl+E puts it back at the end" "$(cursor_xy)" "$fx $fy"
field_start=$((fx - 3))

# ------------------------------------------------------------------ B3: CJK in the field (§8b.9)
# The app's half of the IME law inside the combo: a composed rune lands in the field, the typed
# row carries it verbatim, and the real cursor sits two columns past the field's start.
key C-u
type_ '中'
wait_vis 'use "中" as typed' || miss "the typed row does not carry the composed rune"
# The list just shrank to the typed row alone: the surface re-lays itself out, so wait for
# the frame to hold still before reading the field row and the cursor off it.
settle || miss "the combo never settled after the composed rune"
check "a CJK rune in the field is the typed row, verbatim" "$(count_vis 'use "中" as typed')" 1
# The list shrank to the typed row alone, so the field row moved: read its row again.
check "…and the real cursor sits one wide rune past the field's start" "$(cursor_xy)" "$((field_start + 2)) $(($(row_of '❯中') - 1))"
# …and `/` is a character here too (§8b.2): nothing else claims it.
key C-u
type_ 'a/b'
wait_vis 'use "a/b" as typed' || miss "a slash did not type into the field"
settle || miss "the combo never settled after the slash"
check "a slash goes INTO the field" "$(count_vis 'use "a/b" as typed')" 1

# ------------------------------------------------------------------ C: submitting the text
# Ctrl+U clears the field (the shared emacs edit set); a name nothing matches leaves the typed
# row alone in the list, with the cursor already on it — Enter then means "use what I typed".
key C-u
type_ 'my-own-model'
wait_vis 'use "my-own-model" as typed' || miss "the typed row does not carry the text"
check "a name nothing matches leaves the list to the typed row" "$(count_vis 'gpt-4o')" 0
check "Enter is unambiguous, and says so" "$(count_vis 'Enter use typed')" 1
key Enter
wait_all 'Model switched to my-own-model' || miss "the typed text never committed"
settle || miss "frame never settled after the switch"
check_once "the switch is announced exactly once" 'Model switched to my-own-model'
check "no ghost combo rows after close" "$(count_vis 'Enter select')" 0
check_frame_intact "after committing the typed text" 80

# ------------------------------------------------------------------ C2: ESC with text in the field (§8b.6)
# `q` is a character here (it types), and ESC closes the WHOLE surface, text and all, restoring
# the pane exactly.
before_esc="$(cap)"
type_ '/model'
key Enter
wait_vis 'Enter select' || miss "the combo never opened for the ESC check"
type_ 'q'
wait_vis 'use "q" as typed' || miss "q did not type into the field"
settle || miss "the combo never settled after q"
check "q is a character in the field" "$(count_vis 'use "q" as typed')" 1
key Escape
wait_gone 'Enter select' || miss "ESC with text in the field did not close the surface"
settle || miss "frame never settled after the ESC"
if [ "$(cap)" = "$before_esc" ]; then
    ok "ESC with text in the field closes the whole surface and restores the pane exactly"
else
    bad "pane differs after ESC with text in the field"
    diff <(printf '%s\n' "$before_esc") <(printf '%s\n' "$(cap)") | head -20
fi
check "no switch on ESC" "$(count_all 'Model switched to')" 1

# ------------------------------------------------------------------ D: keyboard navigation
# A model reached from outside the candidate set is still the CURRENT row (the untouched-tab law),
# so ↓ lands on the first listed candidate.
type_ '/model'
key Enter
wait_vis 'mock:my-own-model (current)' || miss "the combo does not carry the current model"
settle || miss "the combo never settled"
key Down
key Enter
wait_all 'Model switched to fake' || miss "arrow navigation never reached the first candidate"
settle || miss "frame never settled after the second switch"
check "the status row followed the model" "$(status_model)" '  fake'
check_frame_intact "after the arrow-driven switch" 80

# ------------------------------------------------------------------ E: ESC during the fetch (§8b.8)
# A third source that answers its listing SLOWLY (the mock's `/slow/` path holds it two
# seconds) keeps `Fetching available models from 3 providers` on the status row long enough to
# press ESC into it: the command must abandon at once — no surface then, and none when the
# slow answer finally arrives.
CONFIG_BODY="providers:
  mock: {type: openai, key: test, url: \"http://127.0.0.1:$IOTA_PORT\"}
  dead: {type: openai, key: test, url: \"http://127.0.0.1:9\"}
  slow: {type: openai, key: test, url: \"http://127.0.0.1:$IOTA_PORT/slow\"}
models:
  m: mock:fake
agents:
  default:
    model: m
    choices: [m, \"mock:*\", \"dead:*\", \"slow:*\"]"
export CONFIG_BODY
start_provider openai 80 30 || finish
settle || miss "startup never settled (slow source)"
before_fetch="$(cap)"
type_ '/model'
key Enter
wait_vis 'Fetching available models from 3 providers' || miss "the fetch never showed on the status row"
key Escape
if _poll_until 10 _vis_lacks 'Fetching available models'; then
    ok "ESC abandons the fetch at once (under a second)"
else
    bad "the fetch outlived ESC"
fi
settle || miss "frame never settled after the abandoned fetch"
check "no surface after the abandoned fetch" "$(count_vis 'Enter select')" 0
if [ "$(cap)" = "$before_fetch" ]; then
    ok "the pane is exactly what it was before /model"
else
    bad "pane differs after the abandoned fetch"
    diff <(printf '%s\n' "$before_fetch") <(printf '%s\n' "$(cap)") | head -20
fi
# The slow listing lands about two seconds after the open; a surface must not pop up then.
if _poll_until 30 _vis_has 'Enter select'; then
    bad "a surface opened after the abandoned fetch's answer arrived"
else
    ok "…and none opens when the slow answer arrives"
fi
type_ 'hello'
key Enter
wait_all 'echo: hello' || miss "the chat did not accept input after the abandoned fetch"
settle || miss "frame never settled after the follow-up turn"
check_frame_intact "after the abandoned fetch" 80

finish
