#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! L4 — the tmux end-to-end harness (`TUI_TEST_PLAN` §L4).
//!
//! The ONLY layer that proves real-terminal behaviour: the physical cursor, native
//! scrollback, TRUE bracketed paste and a real SIGWINCH. Everything else (geometry,
//! orderings, byte shapes) is proved cheaper in L1–L3 and L2b and is deliberately NOT
//! repeated here.
//!
//! **Shape.** Rust owns the gate, the mock provider and the scheduling; `bash` owns the
//! tmux driving, because `capture-pane | grep` is what the spike proved on this host.
//! Each scenario is a standalone script under `tests/ui_tmux/scenarios/`, sourcing
//! `tests/ui_tmux/lib.sh` (the ratatui inline spike's tmux-check.sh helpers,
//! generalised). A scenario exits non-zero when any of its assertions fail and prints one
//! `PASS:`/`FAIL:` line each, so a failure names itself.
//!
//! **ENV-GATED (single-execution rule).** The suite runs only with `IOTA_TMUX=1` set —
//! read, never written. `cargo test` therefore never pays for tmux, and
//! `ci.sh`'s dedicated `IOTA_TMUX=1 cargo test --test ui_tmux` leg is the one
//! execution. Without the variable, without tmux, or without a built `iota` binary every
//! test prints a `SKIP:` line and passes.
//!
//! **Provider.** A dependency-free SSE mock on `127.0.0.1:<ephemeral>`, reached through
//! config file (`providers.mock` pointed at `http://127.0.0.1:<port>`) — no environment is mutated and
//! nothing touches the network. The reply is scripted by the user message the request
//! carries, so a scenario picks its own transcript by typing into the composer.
//!
//! **Serialisation.** Scenarios take a process-wide lock: ten concurrent tmux servers on
//! one host trade real signal for timing flakes, and the whole suite still runs in about
//! twenty-five seconds.
//!
//! **Provenance.** Each scenario descends from a gate of the ratatui inline spike
//! (tmux-check.sh, 35/35 on this host; kept at tag go-final) and from a numbered
//! scenario of `TUI_TEST_PLAN` §L4:
//!
//! | scenario | plan | spike gate |
//! |---|---|---|
//! | `01-startup.sh` | #1 | G1 (pinned frame), G3 (real cursor over CJK) |
//! | `02-markdown-turn.sh` | #2 | G1 (native scrollback, wrap accounting) |
//! | `03-surface-selfheal.sh` | #3 | G2 (selector below composer, no ghosts, self-heal) |
//! | `04-esc-midstream.sh` | #4 | G5 (ESC during the busy render loop) |
//! | `05-ctrlc-twice.sh` | #5 | G5 (double Ctrl+C exits) |
//! | `06-resize.sh` | #6 | G4 (resize while streaming and while idle) |
//! | `07-paste.sh` | #7 | G3 (true bracketed paste, one event) |
//! | `08-big-resume.sh` | #8 | G1 (contiguous history under chunked inserts) |
//! | `09-non-tty.sh` | #9 | — (cmd/root.go:397-401) |
//! | `10-edge-pins.sh` | edge pins | G1 (wrap accounting), G3 (wide runes) |
//! | `11-export-picker.sh` | T3 §7 | — (`/export`: the format picker and the file it writes) |
//! | `12-debug.sh` | T3 §7 | — (`/debug`: the recording switch and the two-tab inspector) |
//! | `13-edit-picker.sh` | T3 §7 | — (images: widget, half-blocks, `/edit` picker, `/redo`) |
//! | `14-osc-signals.sh` | T3 §7 | — (`pipe-pane`: mode 1004 and the OSC 9;4 progress bytes) |

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};

/// The mock provider's port, bound once for the whole test process.
static PORT: OnceLock<Option<u16>> = OnceLock::new();

/// The resolved `iota` binary, built and announced once for the whole test process.
static BIN: OnceLock<Option<PathBuf>> = OnceLock::new();

// The mock is a module of this one binary (`tests/ui_tmux/mock.rs`), never a test target of its own.
mod mock;

/// Prints straight to the process stdout, bypassing libtest's capture.
///
/// `println!` output from a PASSING test is swallowed unless the runner is given
/// `--nocapture`, and `ci.sh` does not pass it — but the `SKIP:` lines are required to be
/// visible (`TUI_TEST_PLAN` §L4 "probe-and-skip"), and so is a scenario's own PASS/FAIL
/// log. `std::io::stdout()` is the real handle and is not intercepted.
fn say(msg: &str) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(msg.as_bytes());
    let _ = out.write_all(b"\n");
    let _ = out.flush();
}

/// One scenario at a time (see the module note on serialisation).
fn serial_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// The package root (`…/rust`): this crate's manifest directory.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// This binary's own directory (`tests/ui_tmux`): the scenario scripts and `lib.sh` live beside `main.rs`.
fn tests_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("ui_tmux")
}

/// Where the `iota` binary can be, cheapest first: this test binary's own profile
/// directory (the usual `CARGO_TARGET_DIR` case), then the workspace's default `target/`.
fn binary_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(profile) = exe.parent().and_then(Path::parent)
    {
        out.push(profile.join("iota"));
    }
    let target = workspace_root().join("target");
    out.push(target.join("debug").join("iota"));
    out.push(target.join("release").join("iota"));
    out
}

/// Resolves the `iota` binary the scenarios drive — and makes it DETERMINISTIC.
///
/// `target/debug/iota` is whatever the last `cargo build` in the tree wrote, which may
/// be stale relative to the sources under test. Driving whichever binary happens to be lying
/// there would make the layer-4 result mean something different from run to run, so this
/// asks cargo for a fresh build first: the relink is a fraction of a second on a warm tree,
/// and cargo does not hold the target-directory lock while the outer `cargo test` runs its
/// test binaries (measured).
///
/// `IOTA_TMUX_BIN` overrides everything — that is how you point the suite at a release
/// binary deliberately. If the build fails or overruns, any binary already in the tree is
/// used rather than skipping the whole layer; only a tree with none at all skips.
fn iota_binary() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("IOTA_TMUX_BIN") {
        let path = PathBuf::from(explicit);
        return path.is_file().then_some(path);
    }
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    if let Ok(mut child) = std::process::Command::new(cargo)
        .args(["build", "--bin", "iota"])
        .current_dir(workspace_root())
        .spawn()
    {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
                _ => {
                    let _ = child.kill();
                    say(
                        "NOTE: `cargo build --bin iota` did not finish — falling back to whatever binary the tree has",
                    );
                    break;
                }
            }
        }
    }
    binary_candidates().into_iter().find(|p| p.is_file())
}

/// Everything a scenario script needs, resolved once the gate is open.
struct Gate {
    tmux: String,
    binary: PathBuf,
    port: u16,
}

/// Applies the env gate and the probes. `None` means "print a SKIP and pass".
///
/// Order matters: the env gate is first so a sandbox without tmux never pays for the
/// binary probe, and the binary probe is last because it is the only step that can build.
fn gate() -> Option<Gate> {
    if std::env::var_os("IOTA_TMUX").is_none() {
        say("SKIP: IOTA_TMUX not set");
        return None;
    }
    let tmux = std::env::var("TMUX_BIN").unwrap_or_else(|_| "tmux".to_owned());
    let probe = std::process::Command::new(&tmux).arg("-V").output();
    if !probe.is_ok_and(|o| o.status.success()) {
        say("SKIP: tmux not found");
        return None;
    }
    if std::process::Command::new("bash")
        .arg("--version")
        .output()
        .is_err()
    {
        say("SKIP: bash not found");
        return None;
    }
    let resolved = BIN.get_or_init(|| {
        let found = iota_binary();
        // Say WHICH binary — a layer-4 result is only readable next to the build it drove.
        if let Some(p) = &found {
            let bytes = std::fs::metadata(p).map_or(0, |m| m.len());
            say(&format!("L4 binary: {} ({bytes} bytes)", p.display()));
        }
        found
    });
    let Some(binary) = resolved.clone() else {
        say("SKIP: iota binary not built (run `cargo build --bin iota` first)");
        return None;
    };
    let Some(port) = *PORT.get_or_init(|| mock::start().ok()) else {
        say("SKIP: mock provider could not bind 127.0.0.1");
        return None;
    };
    Some(Gate { tmux, binary, port })
}

/// Runs one scenario script and fails the test when it reports a failure.
///
/// The script's whole log is echoed through [`say`] so a CI run shows every `PASS:`/
/// `FAIL:`/`WART:` line, not just the exit status.
fn run_scenario(script: &str) {
    let _serial = serial_lock();
    let Some(g) = gate() else { return };
    let scratch = std::env::temp_dir().join(format!(
        "iota-tmux-{}-{}",
        std::process::id(),
        script.replace(['/', '.'], "-")
    ));
    let _ = std::fs::remove_dir_all(&scratch);
    if std::fs::create_dir_all(&scratch).is_err() {
        say("SKIP: scratch directory could not be created");
        return;
    }
    let dir = tests_dir();
    let path = dir.join("scenarios").join(script);
    let out = std::process::Command::new("bash")
        .arg(&path)
        .env("TMUX_BIN", &g.tmux)
        .env(
            "TMUX_SOCKET",
            format!(
                "iota-test-{}-{}",
                std::process::id(),
                script.replace(['.', '-'], "")
            ),
        )
        .env("IOTA_BIN", &g.binary)
        .env("IOTA_PORT", g.port.to_string())
        .env("SCEN_TMP", &scratch)
        .env("TMUX_LIB", dir.join("lib.sh"))
        .output();
    let out = match out {
        Ok(o) => o,
        Err(e) => panic!("could not run {}: {e}", path.display()),
    };
    say(&format!("==== {script} ===="));
    say(String::from_utf8_lossy(&out.stdout).trim_end());
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !stderr.trim().is_empty() {
        say(&format!("---- stderr ----\n{}", stderr.trim_end()));
    }
    // A failing scenario keeps its scratch dir (session bundles, the captured stderr) —
    // an L4 failure is exactly the kind you cannot reproduce from the log alone.
    if out.status.success() {
        let _ = std::fs::remove_dir_all(&scratch);
    }
    assert!(
        out.status.success(),
        "scenario {script} reported failures; scratch kept at {}",
        scratch.display()
    );
}

// ------------------------------------------------------------------ the fourteen scenarios

/// L4 #1 — startup: banner, then the frame; separators span the terminal; the composer is
/// the one input row between them; the real cursor sits at the draft's logical column and
/// does not drift while the app idles.
#[test]
fn tmux_startup_frame_and_pinned_cursor() {
    run_scenario("01-startup.sh");
}

/// L4 #2 — one streamed markdown turn: every rendered line lands in the scrollback exactly
/// once, the `◇ thought for …` marker lands once, the banner above is not eaten, and the
/// live preview MORPHS in place instead of scrolling a rolling source window.
#[test]
fn tmux_streamed_markdown_turn_history_exact() {
    run_scenario("02-markdown-turn.sh");
}

/// L4 #3 — surface open/close self-heal: the panel renders BELOW the composer, a cancelled
/// panel restores the pane byte-for-byte, a committed one leaves exactly ONE record line,
/// and a command submitted mid-stream queues (type-ahead law) and opens on the far side
/// with the history still contiguous.
#[test]
fn tmux_surface_open_close_self_heal() {
    run_scenario("03-surface-selfheal.sh");
}

/// L4 #4 — ESC mid-stream: `Interrupted.` is committed once, the type-ahead queue and the
/// half-typed draft fold back into the composer, and the insert counter freezes.
#[test]
fn tmux_esc_mid_stream_folds_queue() {
    run_scenario("04-esc-midstream.sh");
}

/// L4 #5 — Ctrl+C mid-turn cancels the turn and leaves the app running; a second Ctrl+C at
/// idle exits the process.
#[test]
fn tmux_double_ctrl_c_exits() {
    run_scenario("05-ctrlc-twice.sh");
}

/// L4 #6 — a real SIGWINCH mid-stream (80×24 → 100×28) and an idle shrink (→ 70×20): no
/// crash, no ghost frame, separators follow the new width, the stream completes
/// contiguously and input still works afterwards.
#[test]
fn tmux_resize_mid_stream_and_idle() {
    run_scenario("06-resize.sh");
}

/// L4 #7 — TRUE bracketed paste (`load-buffer` + `paste-buffer -p`): one Paste event, a
/// one-row tag in the composer, a bounded echo on submit and W7 CR normalisation.
#[test]
fn tmux_bracketed_paste_round_trip() {
    run_scenario("07-paste.sh");
}

/// L4 #8 — resuming a 32-round session whose echo window overflows the screen: the
/// chunking law holds (no eaten separators or status row) and every echoed line appears
/// exactly once.
#[test]
fn tmux_big_resume_echo_chunking() {
    run_scenario("08-big-resume.sh");
}

/// L4 #9 — a piped run has no interactive mode: the byte-exact refusal on stderr, exit 1,
/// nothing on stdout.
#[test]
fn tmux_non_tty_refusal() {
    run_scenario("09-non-tty.sh");
}

/// L4 edge pins — a wide rune straddling the last column, a line exactly the terminal
/// width (T-01), a terminal shorter than the frame needs, and a paste larger than a
/// screen.
#[test]
fn tmux_edge_pins() {
    run_scenario("10-edge-pins.sh");
}

/// T3 §7 #11 — `/export`: the format picker opens below the composer, a cancel restores the
/// pane and writes nothing, and a commit leaves ONE record line and a file in the pane's cwd.
#[test]
fn tmux_export_picker_writes_a_file() {
    run_scenario("11-export-picker.sh");
}

/// T3 §7 #12 — `/debug`: `on` puts the `debug` segment on the status row, the inspector lists
/// the round-trip the binary itself made, Enter drills into `↑ Request`/`↓ Response` and Esc
/// walks back out one level at a time.
#[test]
fn tmux_debug_inspector_two_tabs() {
    run_scenario("12-debug.sh");
}

/// T3 §7 #13 — the image path: the generation widget rises, a progressive frame paints into
/// it, the finished picture morphs it into half-block rows with one `🖼 saved:` caption, and
/// `/edit`'s Picker (the one panel kind with an inline preview) opens and cancels cleanly.
#[test]
fn tmux_image_widget_and_edit_picker() {
    run_scenario("13-edit-picker.sh");
}

/// T3 §7 #14 — the out-of-band channels, read off `pipe-pane` because `capture-pane` cannot
/// see them: mode 1004 on at startup, OSC 9;4;3 during a turn, 9;4;0 at idle, 1004 off at exit.
#[test]
fn tmux_osc_progress_and_focus_bytes() {
    run_scenario("14-osc-signals.sh");
}
