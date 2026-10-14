# Releasing

A release is cut by pushing a tag. Creating a Release by hand on the GitHub web
page builds nothing — the tag is the trigger:

```bash
git tag -a v0.2.0 -m "v0.2.0"   # the tag is `v` + the `version` in Cargo.toml
git push origin v0.2.0
```

`.github/workflows/release.yml` does the rest: it builds the five targets of
[Platforms](https://iota.sh/docs/install#platforms) — natively, except the Intel
macOS build, which is cross-compiled on the arm64 macOS runner — packs each one as
`iota-<target>.tar.xz` (`.zip` on Windows) with a SHA-256, opens the GitHub
Release, publishes `iota-installer.sh` and `iota-installer.ps1` beside the
archives and commits `Formula/iota.rb` to
[iotash/homebrew-tap](https://github.com/iotash/homebrew-tap) — which needs a
`HOMEBREW_TAP_TOKEN` repository secret that can write to the tap.

That workflow is generated, never hand-edited: `dist-workspace.toml` is the
source of truth and `dist generate` rewrites the YAML from it. Steps that must
run inside the build job go in `.github/build-setup.yml`, which the same config
points at — that is where the NASM install the Windows build needs lives, since
a step added to the generated file by hand would not survive the next
regeneration. `dist plan` prints what a tag would produce, without building
anything.

**Only the Apple Silicon binary is signed.** The linker ad-hoc signs every
arm64 Mach-O it produces, because arm64 macOS will not start an unsigned one;
the Intel binary (`x86_64-apple-darwin`, cross-compiled on an arm64 runner)
gets no signature, and x86_64 macOS does not require one. The
`verify-macos-signing` job checks both before `host` may publish: a signature
that does not match its binary's contents fails it on either architecture, and
so does an arm64 binary with no signature at all; an unsigned x86_64 binary is
the expected state and passes. Windows has no equivalent here: Authenticode
needs a certificate from a paid signing service, which is outside what this
project runs, so `iota.exe` ships unsigned.
Expect SmartScreen to warn on first run, and expect the browser to flag the
download. What you can check instead is the archive: every asset is published
with a `.sha256` beside it, and `sha256.sum` lists them all.

```powershell
Get-FileHash .\iota-x86_64-pc-windows-msvc.zip -Algorithm SHA256
```

Compare that against the published `iota-x86_64-pc-windows-msvc.zip.sha256`.
The PowerShell installer does this check for you.

