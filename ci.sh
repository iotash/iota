#!/usr/bin/env bash
# CI entry point. Run from any directory; expects the pinned toolchain from
# rust-toolchain.toml (rustfmt + clippy), tmux for the terminal suite and, on
# Linux, bubblewrap for the sandbox tests (both legs print a visible SKIP line
# when the tool is absent).
#
# ONE package, ONE binary (decisions of 2026-09-01 and 2026-09-02; docs/ARCHITECTURE.md
# §1/§11, docs/MERGE-PLAN.md): every lint/doc/test invocation below is the plain
# package one, and the size measurement is a single release build.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"

cargo fmt --check
./scripts/check-deps.sh                      # direct deps ⊆ scripts/direct-deps.allow (cargo metadata; no cargo-deny)
./scripts/check-stubs.sh                     # no `todo!()` body and no `// WPxx-STUB` header anywhere
cargo clippy --all-targets -- -D warnings    # clippy::pedantic via [lints]
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
cargo test
# Layering invariants that used to be crate boundaries (ARCHITECTURE §1.2; Go's own discipline):
# 1. only src/ui/** may name ratatui/crossterm — the loop, the renderer and the command never see a terminal crate;
if grep -rlE 'ratatui|crossterm' src | grep -v '^src/ui/'; then echo "only src/ui/ may name ratatui/crossterm"; exit 1; fi
# 2. the session store never reads the process environment — it takes its root from HostDirs (injected).
if grep -rq 'std::env::var' src/session; then echo "src/session must not read the process environment"; exit 1; fi
# 3. only src/imgterm.rs may name the `image` crate — every other module sees `imgterm::Frame` (ARCHITECTURE §1.2, T3).
if grep -rlE '\bimage::' src | grep -v '^src/imgterm.rs$'; then echo "only src/imgterm.rs may name the image crate"; exit 1; fi
IOTA_TMUX=1 cargo test --test ui_tmux        # L4 — THE single tmux execution (env-gated; the suite builds the debug binary itself)
# The ONE release build; its stripped size is reported to target/size.md (a CI artifact, not a
# tracked file).
cargo build --release
./scripts/size.sh > target/size.md
