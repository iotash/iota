#!/usr/bin/env bash
# L4 scenario 11 (T3_TEST_PLAN §7) — /export: the format picker and the file it writes.
#
# The three things only a real terminal proves: the picker is a Select panel in the bottom
# zone like every other surface (so cancelling it restores the pane byte-for-byte), the
# commit leaves exactly ONE record line, and the export actually lands on disk — in the
# pane's cwd, which is why this scenario uses `start_provider` (`new-session -c "$SCEN_TMP"`)
# rather than `start`. Everything about the DOCUMENT (block order, escaping, the chroma
# wrappers) is proved byte-exactly in L1/L3; here we only open the file and look at its
# first characters.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

start_provider openai 80 24 || finish
settle || bad "startup never settled"

# One turn, so the export has two messages to write.
type_ 'hello'
key Enter
wait_all 'echo: hello' || bad "the turn never completed"
settle || bad "frame never settled after the turn"

# ------------------------------------------------------------------ A: open and cancel
before="$(cap)"

type_ '/export'
key Enter
wait_vis 'Export format' || bad "the /export picker never opened"
settle || bad "the picker never settled"

comp="$(composer_row)"
first="$(row_of 'HTML')"
if [ -n "$comp" ] && [ -n "$first" ] && [ "$first" -gt "$comp" ]; then
    ok "the picker renders BELOW the composer (composer row $comp, first option row $first)"
else
    bad "the picker is not below the composer (composer=$comp option=$first)"
fi
# tmux trims a row's trailing whitespace, so the chip's closing pad is not in the capture.
check "the picker's title chip" "$(cap | sed -n "$((first - 1))p")" " Export format"
check "both formats are offered" "$(count_vis 'Markdown')" 1
check "the footer carries the move hint" "$(count_vis '↑↓ move')" 1
check "the picker did not disturb the separator pair" "$(sep_width)" 80

key Escape
wait_gone '↑↓ move' || bad "the picker did not close on ESC"
settle || bad "frame never settled after cancel"
if [ "$(cap)" = "$before" ]; then
    ok "a cancelled picker restores the pane exactly (no export, no ghost rows)"
else
    bad "pane differs after a cancelled picker"
    diff <(printf '%s\n' "$before") <(printf '%s\n' "$(cap)") | head -20
fi
if [ -z "$(find "$SCEN_TMP" -mindepth 1 -maxdepth 1 -type f -name 'iota-*' -print -quit)" ]; then
    ok "a cancelled picker wrote no file"
else
    bad "a cancelled picker still exported: $(find "$SCEN_TMP" -mindepth 1 -maxdepth 1 -type f -name 'iota-*')"
fi

# ------------------------------------------------------------------ B: commit Markdown
type_ '/export'
key Enter
wait_vis 'Export format' || bad "the /export picker never reopened"
key Down
key Enter
wait_all 'Exported 2 messages → ' || bad "the export record never landed"
settle || bad "frame never settled after the export"

check_once "committing leaves exactly ONE record line" 'Exported 2 messages → '
check "no ghost picker rows after close" "$(count_vis '↑↓ move')" 0
check_frame_intact "after the export" 80

# The file itself: named `iota-<slug|id>-<time>.md`, written into the pane's cwd, and a
# Markdown document opens with the `# ` of its title heading.
file="$(find "$SCEN_TMP" -mindepth 1 -maxdepth 1 -type f -name 'iota-*.md' -print -quit)"
if [ -n "$file" ]; then
    ok "the export landed in the pane's cwd ($(basename "$file"))"
    check "the Markdown document opens with its title heading" "$(head -c 2 "$file")" '# '
    if grep -qF 'echo: hello' "$file"; then
        ok "the document carries the turn's answer"
    else
        bad "the answer is missing from the export"
    fi
else
    bad "no iota-*.md file under $SCEN_TMP"
    ls -la "$SCEN_TMP" | head -20
fi

# ------------------------------------------------------------------ C: commit HTML
# The first option is HTML, so Enter alone commits it. The document's SHAPE is the golden's
# business (`tests/fixtures/export/sample.html`); here it is the file's first line and the
# answer inside it — the export a user opens in a browser is the one this pane wrote.
type_ '/export'
key Enter
wait_vis 'Export format' || bad "the /export picker never opened a third time"
key Enter
wait_all_more 'Exported 2 messages → ' 1 || bad "the HTML export record never landed"
settle || bad "frame never settled after the HTML export"
check "a second commit leaves a second record line" "$(count_all 'Exported 2 messages → ')" 2
check_frame_intact "after the HTML export" 80

file="$(find "$SCEN_TMP" -mindepth 1 -maxdepth 1 -type f -name 'iota-*.html' -print -quit)"
if [ -n "$file" ]; then
    ok "the HTML export landed in the pane's cwd ($(basename "$file"))"
    check "the HTML document opens with its doctype" "$(head -1 "$file")" '<!DOCTYPE html>'
    if grep -qF 'echo: hello' "$file"; then
        ok "the HTML document carries the turn's answer"
    else
        bad "the answer is missing from the HTML export"
    fi
else
    bad "no iota-*.html file under $SCEN_TMP"
    ls -la "$SCEN_TMP" | head -20
fi

finish
