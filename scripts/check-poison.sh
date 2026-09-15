#!/usr/bin/env bash
# PoisonError ratchet: the number of inline `PoisonError` handlings in src/ is pinned here and may
# only go DOWN. New lock handling goes through the shared helper, not another `map_err`; a change
# that retires some lowers the baseline in the same commit (Phase 5 PR-13 takes it to 0 and this
# script with it). bash 3.2 compatible.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

baseline=77
count="$(grep -rcE 'PoisonError' src --include='*.rs' | awk -F: '{s+=$2} END{print s+0}')"
if (( count > baseline )); then
    echo "check-poison: $count inline PoisonError handlings in src/, baseline $baseline — use the shared lock helper, do not add another" >&2
    exit 1
fi
if (( count < baseline )); then
    echo "check-poison: $count < baseline $baseline — lower \`baseline\` in scripts/check-poison.sh in this same commit" >&2
    exit 1
fi
echo "check-poison: OK ($count)"
