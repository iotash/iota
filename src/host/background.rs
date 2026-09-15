//! The host background probe (internal/host/background.go): before the OSC 11 round-trip, a
//! multiplexer that KNOWS its terminal background is asked — cmux answers `terminal.replay` with
//! a `render_grid.terminal_background` hex colour, whose luma decides dark vs light
//! (background.go:78-129). Every probe is seamed so the tests never need the binary.

use std::io::Read;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::host::Probe;
use crate::host::cmux::{CMUX_BIN, CMUX_ENV, CMUX_RPC_TIMEOUT};

/// The `cmux rpc` seam: `(path, surface id) -> stdout bytes`.
pub(crate) type CmuxQuery = Arc<dyn Fn(&Path, &str) -> Option<Vec<u8>> + Send + Sync>;

/// How often the watchdog re-checks a still-running RPC child.
const RPC_POLL: Duration = Duration::from_millis(10);

/// Runs `cmux rpc terminal.replay {"terminal_id":"<sid>"}` and returns its stdout
/// (background.go:100-105). Bounded hard by [`CMUX_RPC_TIMEOUT`]: this runs synchronously at turn
/// start and a wedged cmux must not stall the chat — the child is killed when the deadline passes.
/// A non-zero exit (Go's `Output()` error) is "don't know".
///
/// `std` has no `wait_timeout`, so the child is polled with `try_wait` while a helper thread
/// drains stdout — draining is what keeps a reply larger than the pipe buffer from deadlocking.
pub(crate) fn cmux_query_exec(path: &Path, sid: &str) -> Option<Vec<u8>> {
    // `json.Marshal(map[string]string{"terminal_id": sid})` → exactly `{"terminal_id":"<sid>"}`.
    let params = serde_json::to_string(&serde_json::json!({ "terminal_id": sid })).ok()?;
    let mut child = std::process::Command::new(path)
        .args(["rpc", "terminal.replay", &params])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut out = child.stdout.take()?;
    let reader = std::thread::Builder::new()
        .name("iota-cmux-rpc".to_owned())
        .spawn(move || {
            let mut buf = Vec::new();
            let _ = out.read_to_end(&mut buf);
            buf
        })
        .ok()?;
    let deadline = Instant::now() + CMUX_RPC_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(RPC_POLL);
            }
            Err(_) => break None,
        }
    };
    let buf = reader.join().ok()?;
    status
        .filter(std::process::ExitStatus::success)
        .map(|_| buf)
}

/// The cmux background of surface `sid` through `query` (background.go:91-104).
pub(crate) fn cmux_background(path: &Path, sid: &str, query: &CmuxQuery) -> Option<bool> {
    let out = query(path, sid)?;
    parse_cmux_background(&out)
}

/// The cmux probe (background.go:78-88): `CMUX_SURFACE_ID` non-empty and `cmux` on `PATH`, else
/// unknown.
pub(crate) fn cmux_background_probe(env: &Probe, query: &CmuxQuery) -> Option<bool> {
    let sid = (env.getenv)(CMUX_ENV);
    if sid.is_empty() {
        return None;
    }
    let path = (env.look_path)(CMUX_BIN)?;
    cmux_background(&path, &sid, query)
}

/// `{"render_grid":{"terminal_background":"#RRGGBB"}}` → dark? (background.go:107-117).
pub(crate) fn parse_cmux_background(out: &[u8]) -> Option<bool> {
    let v: serde_json::Value = serde_json::from_slice(out).ok()?;
    let hex = v.get("render_grid")?.get("terminal_background")?.as_str()?;
    dark_hex(hex)
}

/// Whether `#RRGGBB` is dark: `(299r + 587g + 114b) / 1000 < 128` (background.go:120-129).
pub(crate) fn dark_hex(s: &str) -> Option<bool> {
    let hex = s.strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |i: usize| u32::from_str_radix(&hex[i..i + 2], 16).ok();
    let (r, g, b) = (channel(0)?, channel(2)?, channel(4)?);
    Some((299 * r + 587 * g + 114 * b) / 1000 < 128)
}

/// The probe chain over an injected query, then `fallback` (background.go:30-37).
pub(crate) fn detect_background_with(
    env: &Probe,
    query: &CmuxQuery,
    fallback: impl FnOnce() -> bool,
) -> bool {
    cmux_background_probe(env, query).unwrap_or_else(fallback)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::{CmuxQuery, dark_hex, detect_background_with, parse_cmux_background};
    use crate::host::Probe;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    /// A `Probe` whose `CMUX_SURFACE_ID` is `sid` (empty = unset) and whose `PATH` scan answers
    /// `path`.
    fn env(sid: &'static str, path: Option<&'static str>) -> Probe {
        Probe {
            getenv: Box::new(move |k| {
                if k == "CMUX_SURFACE_ID" {
                    sid.to_owned()
                } else {
                    String::new()
                }
            }),
            look_path: Box::new(move |_| path.map(PathBuf::from)),
        }
    }

    // Go: internal/host/background_test.go:8 TestParseCmuxBackground
    #[test]
    fn test_parse_cmux_background() {
        for (name, json, want) in [
            (
                "light",
                r##"{"render_grid":{"terminal_background":"#FEFFFF"}}"##,
                Some(false),
            ),
            (
                "dark",
                r##"{"render_grid":{"terminal_background":"#1E1E1E"}}"##,
                Some(true),
            ),
            ("missing field", r#"{"render_grid":{}}"#, None),
            ("garbage", "not json", None),
            (
                "named color",
                r#"{"render_grid":{"terminal_background":"white"}}"#,
                None,
            ),
        ] {
            assert_eq!(parse_cmux_background(json.as_bytes()), want, "{name}");
        }
    }

    // Go: internal/host/background_test.go:30 TestDarkHexBoundary — `(299r+587g+114b)/1000 < 128`
    // is INTEGER division, so mid grey lands exactly on 128 and reads light.
    #[test]
    fn test_dark_hex_boundary() {
        assert_eq!(dark_hex("#808080"), Some(false), "mid gray must read light");
        assert_eq!(dark_hex("#000000"), Some(true), "black must read dark");
        assert_eq!(dark_hex("white"), None);
        assert_eq!(dark_hex("#12345"), None);
        assert_eq!(dark_hex("#gggggg"), None);
    }

    // Go: internal/host/background_test.go:61 TestDetectBackgroundCmuxProbe — the probe sees the
    // looked-up path and the surface id, and its answer wins over the OSC fallback.
    #[test]
    fn test_detect_background_cmux_probe() {
        let seen: Arc<Mutex<Vec<(PathBuf, String)>>> = Arc::default();
        let log = Arc::clone(&seen);
        let query: CmuxQuery = Arc::new(move |path: &Path, sid: &str| {
            log.lock()
                .unwrap()
                .push((path.to_path_buf(), sid.to_owned()));
            Some(br##"{"render_grid":{"terminal_background":"#FEFFFF"}}"##.to_vec())
        });
        let dark = detect_background_with(&env("surf-1", Some("/bin/cmux")), &query, || {
            panic!("the OSC fallback must not run once a host answered")
        });
        assert!(!dark, "cmux reported a light background");
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            [(PathBuf::from("/bin/cmux"), "surf-1".to_owned())]
        );
    }

    // Go: internal/host/background_test.go:84 TestDetectBackgroundFallsThroughToOSC
    #[test]
    fn test_detect_background_falls_through_to_osc() {
        let query: CmuxQuery = Arc::new(|_, _| panic!("no probe applies in a bare environment"));
        assert!(
            detect_background_with(&env("", None), &query, || true),
            "with no host probe, the OSC answer must stand"
        );
        // The env var alone is not enough: the CLI must be on PATH too.
        assert!(detect_background_with(&env("surf-1", None), &query, || {
            true
        }));
    }

    // Go: internal/host/background_test.go:97 TestDetectBackgroundLatch is NOT ported: Go latched
    // because termenv's OSC 11 query could block for seconds; `crate::ui::osc::detect_background`
    // has a 100 ms deadline and needs no latch (T3 spec §2.4, DIVERGENCES §D).
    #[test]
    fn a_probe_that_does_not_know_falls_through() {
        let query: CmuxQuery = Arc::new(|_, _| Some(b"not json".to_vec()));
        assert!(detect_background_with(
            &env("surf-1", Some("/bin/cmux")),
            &query,
            || true
        ));
    }
}
