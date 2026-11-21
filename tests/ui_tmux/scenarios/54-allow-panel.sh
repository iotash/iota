#!/usr/bin/env bash
# L4 scenario 54 (allow-panel, X-111) — granting by the typed `/allow <dir>`, taking back in the bare `/allow`'s
# panel, through a real terminal.
#
# The decisions are pinned in-process (`repl::commands::allow` tests: what a Grants commit revokes;
# `tests/repl/commands.rs`: the commits' notices, the tabs, the hint with nothing to list); what only a real
# terminal shows is the user's side of it:
#
#   54.1 `/allow ../zz-out` grants the sibling directory: the transcript says it is writable for this session,
#        and `/status` counts the grant;
#   54.2 the bare `/allow` opens `Grants` and `Static` (the config names a `write_roots:` entry) and no
#        directory browser; the Grants tab lists the grant with its level; Space checks it, Enter commits: the
#        transcript says it was revoked, and `/status`'s Grants row is gone;
#   54.3 the Static tab says what it is and lists the `write_roots:` entry with its source; it is read-only:
#        Enter there closes the surface and changes nothing.
# shellcheck source=../lib.sh
. "${TMUX_LIB:?}"

# The project is `proj/`; the directory to grant is a sibling, outside it. 160 columns: a macOS temp path is
# long, and the notice and the row must not wrap.
mkdir -p "$SCEN_TMP/proj" "$SCEN_TMP/zz-out" "$SCEN_TMP/rooted"

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

# ------------------------------------------------------------------ 54.1: the typed form grants
type_ '/allow ../zz-out'
key Enter
wait_all 'zz-out is writable for this session' || miss "/allow ../zz-out never granted the directory"
settle || miss "frame never settled after the grant"
type_ '/status'
key Enter
wait_vis '1 for this session (/allow lists them)' || miss "/status never counted the grant"
key Escape
wait_gone '(/allow lists them)' || miss "/status never closed"
settle || miss "frame never settled after /status"

# ------------------------------------------------------------------ 54.2: the Grants tab revokes it
type_ '/allow'
key Enter
wait_vis 'this session  ' || miss "the bare /allow never listed the grant"
check "the Grants tab is offered" "$(count_vis 'Grants')" 1
check "the Static tab is offered" "$(count_vis 'Static')" 1
check "no directory browser" "$(count_vis 'Enter open')" 0
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

# ------------------------------------------------------------------ 54.3: the Static tab, read-only
type_ '/allow'
key Enter
wait_vis 'No grants. /allow <dir>' || miss "the second /allow never opened on its empty Grants tab"
key Tab
wait_vis 'not grants made here' || miss "the Static tab never said what it is"
check "the row names its source and the root" "$(cap | grep -F 'write_roots:  ' | grep -cF 'rooted' | tr -d ' ')" 1
check "the tab is a viewer" "$(count_vis 'q/Esc close')" 1
key Enter
wait_gone 'not grants made here' || miss "Enter on the Static tab never closed the surface"
settle || miss "frame never settled after the Static tab"
check "Enter there granted nothing" "$(count_all 'is writable for this session')" 1
check "Enter there revoked nothing" "$(count_all '✓ revoked ')" 1
check_frame_intact "after the panel" 160

finish
