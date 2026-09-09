#!/usr/bin/env bash
# L4 scenario 7 (TUI_TEST_PLAN §L4) — TRUE bracketed paste.
#
# `load-buffer` + `paste-buffer -p` is the only way to get a real bracketed block: `-p`
# wraps it in ESC[200~ / ESC[201~, and tmux translates the embedded newlines to CR on the
# way in — which is exactly wart W7, and the reason `paste::normalize` exists. Synthesised
# keystrokes would prove none of it.
#
# What must hold: ONE Paste event (a 3-line block that arrived as keystrokes would have
# submitted on its first newline), a one-row `[#N …]` tag in the composer, and a submit
# that expands the tag in full for the model while the transcript shows the bounded echo.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

start 80 24 || finish
settle || bad "startup never settled"
before_hist="$(hist_size)"

printf 'paste line one\npaste line two\n第三行中文\n' | tm load-buffer -
tm paste-buffer -pt s
wait_vis '[#1 paste line one… 3 lines]' || bad "the paste tag never appeared"
settle || bad "composer never settled after the paste"

# ONE event: nothing was submitted (the block's newlines never reached the key handler),
# the tag is a single composer row, and the store holds exactly one entry (id #1).
check "the paste arrived as ONE event — nothing submitted" "$(hist_size)" "$before_hist"
check "the tag collapses the block to one composer row" "$(count_composer '❯ [#1 paste line one… 3 lines]')" 1
check "still a one-row composer" "$(composer_block | wc -l | tr -d ' ')" 1
check "the tag counts the block's lines, CR-normalised" "$(count_composer '3 lines]')" 1
check_frame_intact "with a paste tag in the composer" 80

key Enter
wait_all 'echo: paste line one' || bad "the expanded paste never reached the model"
settle || bad "frame never settled after the submit"

# The echo: the tag expands to the block, one transcript row per line, CJK intact.
check_once "echo row 1" '❯ paste line one'
check_once "echo row 2" '  paste line two'
check_once "echo row 3 (CJK survives the round trip)" '  第三行中文'
check "the tag left the composer on submit" "$(count_composer '[#1 paste')" 0

# The model received the FULL expansion, not the tag.
check_once "the model saw line 1" 'echo: paste line one'
check "the model saw line 3" "$(count_all '第三行中文')" 2

# A second paste takes the next id — the store is per-session and append-only.
printf 'second block\nsecond line\n' | tm load-buffer -
tm paste-buffer -pt s
wait_vis '[#2 second block… 2 lines]' || bad "the second paste did not take id #2"
ok "a second paste takes the next tag id"

finish
