#!/usr/bin/env bash
# L4 scenario 54 (allow-panel, X-111) — the bare `/allow`'s tabs, through a real terminal.
#
# The decisions are pinned in-process (`ui::surface::tabbed::browser_tests`: the directory picker's
# Enter/Space split; `repl::commands::allow` tests: what a Grants commit revokes; `tests/repl/commands.rs`:
# the commits' notices); what only a real terminal shows is the user's side of it:
#
#   54.1 the bare `/allow` opens its tabs — `Add`, a directory browser on the project root that lists `./`
#        and `../` and no file, its hint naming Enter and Space — then `Grants`, and `Static` (the config
#        names a `write_roots:` entry);
#   54.2 Enter on `../` descends (the surface stays open), Space on a directory chooses it: the transcript
#        says it is writable for this session, and `/status` counts the grant;
#   54.3 the Grants tab lists it with its level; Space checks it, Enter commits: the transcript says it was
#        revoked, and `/status`'s Grants row is gone;
#   54.4 the Static tab says what it is and lists the `write_roots:` entry with its source; it is read-only:
#        Enter there closes the surface and changes nothing.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

# The project is `proj/`; the directory to grant is a sibling, outside it, named to sort last. 160 columns: a
# macOS temp path is long, and the notice and the row must not wrap.
mkdir -p "$SCEN_TMP/proj" "$SCEN_TMP/zz-out" "$SCEN_TMP/rooted"
echo "file" >"$SCEN_TMP/proj/a-file.txt"

# The default config plus one `write_roots:` entry, so the Static tab is shown.
CONFIG_BODY="providers:
  mock: {type: openai, key: test, url: \"http://127.0.0.1:$IOTA_PORT\"}
models:
  m: mock:fake
agents:
  default: {model: m, choices: [m, \"mock:*\"]}
write_roots: [\"$SCEN_TMP/rooted\"]"
export CONFIG_BODY

_launch "$SCEN_TMP/proj" openai fake 160 30 || finish
settle || miss "startup never settled"

# ------------------------------------------------------------------ 54.1: two tabs, a directory browser
type_ '/allow'
key Enter
wait_vis '↑↓ move · Enter open · Space choose · q/Esc cancel' || miss "the bare /allow never opened its Add tab"
check "the Grants tab is offered" "$(count_vis 'Grants')" 1
check "the Static tab is offered" "$(count_vis 'Static')" 1
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

# ------------------------------------------------------------------ 54.4: the Static tab, read-only
type_ '/allow'
key Enter
wait_vis 'Space choose' || miss "the third /allow never opened"
key Tab
key Tab
wait_vis 'not grants, so none can be revoked here' || miss "the Static tab never said what it is"
check "the row names its source and the root" "$(cap | grep -F 'write_roots:  ' | grep -cF 'rooted' | tr -d ' ')" 1
check "the tab is a viewer" "$(count_vis 'q/Esc close')" 1
key Enter
wait_gone 'not grants, so none can be revoked here' || miss "Enter on the Static tab never closed the surface"
settle || miss "frame never settled after the Static tab"
check "Enter there granted nothing" "$(count_all 'is writable for this session')" 1
check "Enter there revoked nothing" "$(count_all '✓ revoked ')" 1
check_frame_intact "after the panel" 160

finish
