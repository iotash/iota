#!/usr/bin/env bash
# L4 scenario 13 (T3_TEST_PLAN §7) — the image path end to end: generation widget,
# half-block picture, `/edit` picker, `/redo`.
#
# This is the only layer where the half-block rows meet a real terminal: L1 pins the bytes
# `imgterm::render` produces and L3 pins the transcript's block shape, but nothing below
# this proves that a progressive frame paints into the generation widget, that the finished
# picture MORPHS that widget in place (one block, not two), or that the `/edit` picker —
# a `PanelKind::Picker`, the only panel kind with an inline preview — renders without
# tearing the frame.
#
# The provider is the `images` dialect: the mock answers `POST /images/generations` (and
# `/images/edits`) with one `image_generation.partial_image` frame and then `.completed`,
# both carrying the checked-in 2×2 PNG. A 2×2 source rasterises to ONE row of half-blocks
# (imgterm never upscales), which is all this scenario needs.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

start_provider images 80 30 || finish
settle || bad "startup never settled"

# The generation widget with a progressive frame already painted into it, in ONE capture:
# the widget's live elapsed row (`⎿ `) and half-block preview rows at the same instant.
# Two captures could straddle the moment the finished picture morphs the widget away, so
# the winning capture is kept in $SNAP and every assertion below reads THAT, not a fresh one.
# The banner's wordmark is half blocks too (X-46), so a frame counts only UNDER the widget
# header — the `⠋ image` row (`/ image/`: the model id on the status row has no leading space).
SNAP=""
_frame_in_widget() {
    SNAP="$(cap)"
    printf '%s\n' "$SNAP" | awk '/ image/{h=1} h && /⎿ /{a=1} h && /▀/{b=1} END{exit !(a && b)}'
}

# ------------------------------------------------------------------ A: generate
type_ 'a red square'
key Enter
# The widget goes up for the whole round-trip; the mock holds the partial frame so it is
# observable rather than a single-frame flicker.
if _poll_until 120 _frame_in_widget; then
    ok "a progressive frame painted into the live generation widget"
    # `⠋ image` is the widget header (the model id `gpt-image-1` on the status row has no
    # leading space, so it is not a second match); the frame is the row directly under it.
    hdr="$(printf '%s\n' "$SNAP" | grep -nF ' image' | head -1 | cut -d: -f1)"
    # The first half-block row below the header (the wordmark above it does not count).
    body="$(printf '%s\n' "$SNAP" | tail -n "+$((hdr + 1))" | grep -nF '  ▀' | head -1 | cut -d: -f1)"
    [ -n "$body" ] && body=$((body + hdr))
    check "exactly one widget carries the 'image' label" \
        "$(printf '%s\n' "$SNAP" | grep -cF ' image' | tr -d ' ')" 1
    if [ -n "$hdr" ] && [ -n "$body" ] && [ "$body" -eq "$((hdr + 1))" ]; then
        ok "the frame IS the widget's body (header row $hdr, preview row $body)"
    else
        bad "the preview row is not the widget's body (header=$hdr preview=$body)"
        printf '%s\n' "$SNAP"
    fi
else
    bad "no progressive frame inside the generation widget"
    cap
fi
wait_all '🖼 saved: ' || bad "the picture was never committed"
settle || bad "frame never settled after the generation"

check_once "the saved-image caption lands exactly once" '🖼 saved: '
# …under the user echo: the banner's wordmark above it is half blocks too.
if capall | sed -n '/❯ a red square/,$p' | grep -qF '  ▀'; then
    ok "the picture rendered as indented half-block rows"
else
    bad "no half-block rows in the history"
    capall | tail -20
fi
check_frame_intact "after the generation" 80

# ------------------------------------------------------------------ B: the /edit picker
before="$(cap)"

type_ '/edit'
key Enter
wait_vis 'Edit an image' || bad "the /edit picker never opened"
settle || bad "the picker never settled"

comp="$(composer_row)"
chip="$(row_of ' Edit an image')"
if [ -n "$comp" ] && [ -n "$chip" ] && [ "$chip" -gt "$comp" ]; then
    ok "the picker renders BELOW the composer (composer row $comp, chip row $chip)"
else
    bad "the picker chip is not below the composer (composer=$comp chip=$chip)"
fi
# tmux trims a row's trailing whitespace, so the chip's closing pad is not in the capture.
check "the picker's chip title" "$(cap | sed -n "${chip}p")" " Edit an image"
check "the picker states what it wants" "$(count_vis 'Pick the image to edit')" 1
if cap | grep -qF '▸ '; then
    ok "the picker highlights a row"
else
    bad "no selection marker in the picker"
    cap | sed -n "$chip,\$p"
fi
check "the picker did not disturb the separator pair" "$(sep_width)" 79

key Escape
wait_gone 'Edit an image' || bad "the picker did not close on ESC"
settle || bad "frame never settled after cancel"
if [ "$(cap)" = "$before" ]; then
    ok "a cancelled picker restores the pane exactly"
else
    bad "pane differs after a cancelled picker"
    diff <(printf '%s\n' "$before") <(printf '%s\n' "$(cap)") | head -20
fi

# ------------------------------------------------------------------ C: /redo
type_ '/redo'
key Enter
wait_all 'Redoing: a red square' || bad "the /redo echo never landed"
# The first picture's caption is in the scrollback already: wait for the SECOND.
wait_all_more '🖼 saved: ' 1 || bad "the redo produced no picture"
settle || bad "frame never settled after the redo"
check "the redo committed a second picture" "$(count_all '🖼 saved: ')" 2
check_frame_intact "after the redo" 80

# ------------------------------------------------------------------ D: /edit <prompt>
type_ '/edit make it blue'
key Enter
wait_all_more '🖼 saved: ' 2 || bad "the edit produced no picture"
settle || bad "frame never settled after the edit"
check "the edit committed a third picture" "$(count_all '🖼 saved: ')" 3
check_frame_intact "after the edit" 80

finish
