#!/usr/bin/env bash
# Direct-dependency allowlist check (CONTRACTS.md §0.9, ARCHITECTURE.md §11).
#
# Reads `cargo metadata --no-deps` and fails when the package declares a direct
# dependency (normal, dev or build) whose crate name is not listed in
# scripts/direct-deps.allow. Deliberately NOT cargo-deny: its [bans] tables
# apply to the whole transitive graph and its binary is not part of the pinned
# toolchain. Needs only cargo + python3 (bash 3.2 compatible).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
allow="$root/scripts/direct-deps.allow"

meta="$(mktemp -t iota-metadata.XXXXXX)"
trap 'rm -f "$meta"' EXIT

(cd "$root" && cargo metadata --format-version 1 --no-deps > "$meta")

python3 - "$allow" "$meta" <<'PY'
import json
import sys

allow_path, meta_path = sys.argv[1], sys.argv[2]
allowed = set()
with open(allow_path, encoding="utf-8") as fh:
    for line in fh:
        line = line.split("#", 1)[0].strip()
        if line:
            allowed.add(line)

with open(meta_path, encoding="utf-8") as fh:
    meta = json.load(fh)

members = set(meta["workspace_members"])
bad = []
for pkg in meta["packages"]:
    if pkg["id"] not in members:
        continue
    for dep in pkg["dependencies"]:
        name = dep["name"]
        if name == pkg["name"]:
            continue  # the self-dev-dependency that turns the `testing` feature on for tests
        if name not in allowed:
            kind = dep.get("kind") or "normal"
            bad.append(f"{pkg['name']}: {name} ({kind})")

if bad:
    print("check-deps: direct dependencies not in scripts/direct-deps.allow:", file=sys.stderr)
    for entry in sorted(bad):
        print(f"  - {entry}", file=sys.stderr)
    sys.exit(1)
print(f"check-deps: OK ({len(members)} package, {len(allowed)} allowed crates)")
PY
