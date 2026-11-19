#!/usr/bin/env bash
# L4 scenario 54 (allow-panel, X-111) — the bare `/allow`'s two tabs, through a real terminal.
#
# The decisions are pinned in-process (`ui::surface::tabbed::browser_tests`: the directory picker's
# Enter/Space split; `repl::commands::allow` tests: what a Grants commit revokes; `tests/repl/commands.rs`:
# the commits' notices); what only a real terminal shows is the user's side of it:
#
#   54.1 the bare `/allow` opens two tabs — `Add`, a directory browser on the project root that lists `./`
#        and `../` and no file, its hint naming Enter and Space — and `Grants`;
#   54.2 Enter on `../` descends (the surface stays open), Space on a directory chooses it: the transcript
#        says it is writable for this session, and `/status` counts the grant;
#   54.3 the Grants tab lists it with its level; Space checks it, Enter commits: the transcript says it was
#        revoked, and `/status`'s Grants row is gone.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

# The project is `proj/`; the directory to grant is a sibling, outside it, named to sort last. 160 columns: a
# macOS temp path is long, and the notice and the row must not wrap.
mkdir -p "$SCEN_TMP/proj" "$SCEN_TMP/zz-out"
echo "file" >"$SCEN_TMP/proj/a-file.txt"

_launch "$SCEN_TMP/proj" openai fake 160 30 || finish
settle || miss "startup never settled"

# ------------------------------------------------------------------ 54.1: two tabs, a directory browser
type_ '/allow'
key Enter
wait_vis '↑↓ move · Enter open · Space choose · q/Esc cancel' || miss "the bare /allow never opened its Add tab"
check "the Grants tab is offered" "$(count_vis 'Grants')" 1
check "./ is a row" "$(cap | grep -cE '^ *(▸ )?\./ *$' | tr -d ' ')" 1
check "../ is a row" "$(cap | grep -cE '^ *(▸ )?\.\./ *$' | tr -d ' ')" 1
check "no file is listed" "$(count_vis 'a-file.txt')" 0

# ------------------------------------------------------------------ 54.2: Enter descends, Space chooses
key Down
key Enter
wait_vis 'zz-out/' || miss "Enter on ../ never descended"
check "the surface is still open" "$(count_vis 'Space choose')" 1
key G
key Space
wait_all 'zz-out is writable for this session' || miss "Space never granted the directory"
wait_gone 'Space choose' || miss "the surface never closed on the choice"
settle || miss "frame never settled after the grant"
type_ '/status'
key Enter
wait_vis '1 for this session (/allow lists them)' || miss "/status never counted the grant"
key Escape
wait_gone '(/allow lists them)' || miss "/status never closed"
settle || miss "frame never settled after /status"

# ------------------------------------------------------------------ 54.3: the Grants tab revokes it
type_ '/allow'
key Enter
wait_vis 'Space choose' || miss "the second /allow never opened"
key Tab
wait_vis 'this session  ' || miss "the Grants tab never listed the grant"
check "the row names the directory" "$(cap | grep -F 'this session  ' | grep -cF 'zz-out' | tr -d ' ')" 1
key Space
key Enter
wait_all 'zz-out (this session)' || miss "the Grants commit never said the revocation"
check "it said revoked" "$(count_all '✓ revoked ')" 1
settle || miss "frame never settled after the revocation"
type_ '/status'
key Enter
wait_vis 'Messages' || miss "/status never opened again"
check "no Grants row after the revocation" "$(count_vis '(/allow lists them)')" 0
key Escape
settle || miss "frame never settled after the last /status"
check_frame_intact "after the panel" 160

finish
