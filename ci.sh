#!/usr/bin/env bash
# CI entry point. Run from any directory; expects the pinned toolchain from
# rust-toolchain.toml (rustfmt + clippy), tmux for the terminal suite and, on
# Linux, bubblewrap for the sandbox tests. Both are REQUIRED here — a missing one is
# a red test that names it, never a SKIP that passes (see the two exports below).
#
# ONE package, ONE binary (decisions of 2026-09-01 and 2026-09-02; docs/ARCHITECTURE.md
# §1/§11, docs/MERGE-PLAN.md): every lint/doc/test invocation below is the plain
# package one, and the size measurement is a single release build.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"

# Whatever ends this script — the last line, a red leg, or the job timeout in
# `.github/workflows/ci.yml` (a cancelled step arrives as SIGINT before the SIGKILL) — the L4
# suite's tmux servers must not outlive it. They are the one thing this tree starts that never
# exits on its own: a scenario killed before its own EXIT trap leaves its private server running,
# still holding the `iota` binary it was driving. The dead socket FILES leak too — tmux unlinks
# one only when it shuts down cleanly, and this machine had 1265 of them in /tmp — so a socket
# that answers nothing is removed rather than left to accumulate.
#
# The sweep is by socket, and those sockets are the suite's alone (`iota-test-<pid>-<scenario>`,
# tests/ui_tmux/main.rs): a developer's own tmux lives on `default` and is never touched. Two
# copies of this script sharing one machine would sweep each other's, which cargo's
# target-directory lock already makes a thing that does not happen.
sweep_tmux_servers() {
  local tmux="${TMUX_BIN:-tmux}" sock
  command -v "$tmux" >/dev/null 2>&1 || return 0
  shopt -s nullglob
  for sock in "${TMUX_TMPDIR:-/tmp}/tmux-$(id -u)"/iota-test-*; do
    "$tmux" -S "$sock" kill-server >/dev/null 2>&1 || true
    rm -f "$sock"
  done
}
trap sweep_tmux_servers EXIT
trap 'sweep_tmux_servers; exit 130' INT TERM

# Nothing this gate runs may skip itself. The tmux suite (tests/ui_tmux) and the two sandbox
# tests (tests/tool/shell.rs) probe for what they need — tmux, bash, a built binary, a mock
# port; sandbox-exec or bubblewrap — and without these two variables answer a miss with a
# `SKIP:` line and a PASS. That is how a machine missing a dependency stays green for weeks
# (2026-09-13: six rounds of CI for problems the local run had been skipping over). With them
# set, a miss is a red test that names what is absent. `IOTA_TMUX_REQUIRED` also ENABLES the
# tmux suite, so the one `cargo test` below is THE single L4 execution (the suite builds the
# debug binary itself); a plain `cargo test` without the variables still skips as before.
export IOTA_TMUX_REQUIRED=1 IOTA_SANDBOX_REQUIRED=1

cargo fmt --check
./scripts/check-deps.sh                      # direct deps ⊆ scripts/direct-deps.allow (cargo metadata; the one thing deny.toml cannot say)
./scripts/check-stubs.sh                     # no `todo!()` body and no `// WPxx-STUB` header anywhere
# The transitive-graph gate (deny.toml): licenses, advisories, duplicate versions, sources.
# cargo-deny is not in the pinned toolchain, so this leg prints a SKIP without it — the second
# optional dependency of this script, beside the cross toolchains; on GitHub it is its own job
# (ci.yml, cargo-deny-action) and never skips.
if command -v cargo-deny >/dev/null 2>&1; then
  cargo deny --locked check
else
  echo "SKIP cargo-deny: not on PATH (brew install cargo-deny, or cargo install cargo-deny --locked)"
fi
cargo clippy --all-targets -- -D warnings    # clippy::pedantic via [lints]

# The line above only ever sees THIS host's target, so `src/shell/sandbox_linux.rs` and every
# `cfg(windows)` arm are invisible to it and a pedantic lint that fires only there arrives as a red
# CI leg (2026-09-13: `unnecessary_wraps` in sandbox_linux.rs, never once seen on a Mac). clippy is a
# front end and needs no linker, so the other two targets ARE lintable from here — what they need is
# rustup's std for the target plus a cross compiler for the tree's one C dependency (aws-lc-sys,
# under reqwest/rustls). Each leg prints a visible SKIP line when either is missing — the cross
# toolchains are a Mac developer's convenience, optional like cargo-deny above and nothing else
# here; on the CI runners everything below skips, since each one is already linting its own
# platform natively.
cross_lint() {
  local target=$1 cc=$2
  if ! rustup target list --installed | grep -qx "$target"; then
    echo "SKIP cross-lint $target: run \`rustup target add $target\`"
  elif ! command -v "$cc" >/dev/null 2>&1; then
    echo "SKIP cross-lint $target: no $cc on PATH (building aws-lc-sys for $target needs one)"
  else
    cargo clippy --target "$target" --all-targets -- -D warnings
  fi
}
cross_lint x86_64-pc-windows-gnu   x86_64-w64-mingw32-gcc   # brew install mingw-w64
cross_lint x86_64-unknown-linux-gnu x86_64-linux-gnu-gcc    # brew install messense/macos-cross-toolchains/x86_64-unknown-linux-gnu
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
# Layering invariants that used to be crate boundaries (ARCHITECTURE §1.2; Go's own discipline):
# 1. only src/ui/** may name ratatui/crossterm — the loop, the renderer and the command never see a terminal crate;
if grep -rlE 'ratatui|crossterm' src | grep -v '^src/ui/'; then echo "only src/ui/ may name ratatui/crossterm"; exit 1; fi
# 2. the session store never reads the process environment — it takes its root from HostDirs (injected).
if grep -rq 'std::env::var' src/session; then echo "src/session must not read the process environment"; exit 1; fi
# 3. only src/imgterm.rs may name the `image` crate — every other module sees `imgterm::Frame` (ARCHITECTURE §1.2, T3).
if grep -rlE '\bimage::' src | grep -v '^src/imgterm.rs$'; then echo "only src/imgterm.rs may name the image crate"; exit 1; fi
# Every test binary, the tmux suite included: THE single L4 execution, enabled and made
# mandatory by IOTA_TMUX_REQUIRED above (tests/ui_tmux/main.rs).
cargo test
# The ONE release build; its stripped size is reported to target/size.md (a CI artifact, not a
# tracked file).
cargo build --release
./scripts/size.sh > target/size.md
