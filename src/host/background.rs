//! The host background probe (internal/host/background.go): before the OSC 11 round-trip, a
//! multiplexer that KNOWS its terminal background is asked — cmux answers `terminal.replay` with
//! a `render_grid.terminal_background` hex colour, whose luma decides dark vs light
//! (background.go:78-129). Every probe is seamed so the tests never need the binary.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use crate::BoxFuture;
use crate::host::Probe;
use crate::host::cmux::{CMUX_BIN, CMUX_ENV, CMUX_RPC_TIMEOUT};

/// The `cmux rpc` seam: `(path, surface id) -> stdout bytes`, asynchronously.
pub(crate) type CmuxQuery =
    Arc<dyn Fn(&Path, &str) -> BoxFuture<'static, Option<Vec<u8>>> + Send + Sync>;

/// Runs `cmux rpc terminal.replay {"terminal_id":"<sid>"}` and returns its stdout
/// (background.go:100-105). Bounded hard by [`CMUX_RPC_TIMEOUT`]: this runs at turn start and a
/// wedged cmux must not stall the chat — past the deadline the child is dropped, and `kill_on_drop`
/// ends it. A non-zero exit (Go's `Output()` error) is "don't know". `output()` drains stdout while
/// it waits, so a reply larger than the pipe buffer cannot deadlock; until 2026-09-15 this was a
/// `try_wait` poll loop with a helper thread doing the draining.
pub(crate) fn cmux_query_exec(path: &Path, sid: &str) -> BoxFuture<'static, Option<Vec<u8>>> {
    let path: PathBuf = path.to_path_buf();
    // `json.Marshal(map[string]string{"terminal_id": sid})` → exactly `{"terminal_id":"<sid>"}`.
    let params = serde_json::to_string(&serde_json::json!({ "terminal_id": sid })).ok();
    Box::pin(async move {
        let params = params?;
        let mut cmd = tokio::process::Command::new(&path);
        cmd.args(["rpc", "terminal.replay", &params])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let out = tokio::time::timeout(CMUX_RPC_TIMEOUT, cmd.output())
            .await
            .ok()?
            .ok()?;
        out.status.success().then_some(out.stdout)
    })
}

/// The cmux background of surface `sid` through `query` (background.go:91-104).
pub(crate) async fn cmux_background(path: &Path, sid: &str, query: &CmuxQuery) -> Option<bool> {
    let out = query(path, sid).await?;
    parse_cmux_background(&out)
}

/// The cmux probe (background.go:78-88): `CMUX_SURFACE_ID` non-empty and `cmux` on `PATH`, else
/// unknown.
pub(crate) async fn cmux_background_probe(probe: &Probe, query: &CmuxQuery) -> Option<bool> {
    let sid = probe.env.var(CMUX_ENV)?;
    let path = (probe.look_path)(CMUX_BIN)?;
    cmux_background(&path, &sid, query).await
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

/// The probe chain over an injected query, then `fallback` (background.go:30-37) — a future, since the
/// fallback the binary supplies is a blocking tty round-trip it puts on a blocking thread.
pub(crate) async fn detect_background_with(
    probe: &Probe,
    query: &CmuxQuery,
    fallback: impl Future<Output = bool>,
) -> bool {
    match cmux_background_probe(probe, query).await {
        Some(dark) => dark,
        None => fallback.await,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::{CmuxQuery, dark_hex, detect_background_with, parse_cmux_background};
    use crate::BoxFuture;
    use crate::app::env::Env;
    use crate::host::Probe;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    /// A `Probe` whose `CMUX_SURFACE_ID` is `sid` (empty = unset) and whose `PATH` scan answers
    /// `path`.
    fn env(sid: &'static str, path: Option<&'static str>) -> Probe {
        Probe {
            env: Env::fixed(&[("CMUX_SURFACE_ID", sid)]),
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
    #[tokio::test]
    async fn test_detect_background_cmux_probe() {
        let seen: Arc<Mutex<Vec<(PathBuf, String)>>> = Arc::default();
        let log = Arc::clone(&seen);
        let query: CmuxQuery = Arc::new(move |path: &Path, sid: &str| {
            log.lock()
                .unwrap()
                .push((path.to_path_buf(), sid.to_owned()));
            Box::pin(std::future::ready(Some(
                br##"{"render_grid":{"terminal_background":"#FEFFFF"}}"##.to_vec(),
            )))
        });
        let dark = detect_background_with(&env("surf-1", Some("/bin/cmux")), &query, async {
            unreachable!("the OSC fallback must not run once a host answered")
        })
        .await;
        assert!(!dark, "cmux reported a light background");
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            [(PathBuf::from("/bin/cmux"), "surf-1".to_owned())]
        );
    }

    // Go: internal/host/background_test.go:84 TestDetectBackgroundFallsThroughToOSC
    #[tokio::test]
    async fn test_detect_background_falls_through_to_osc() {
        let query: CmuxQuery =
            Arc::new(|_: &Path, _: &str| -> BoxFuture<'static, Option<Vec<u8>>> {
                unreachable!("no probe applies in a bare environment")
            });
        assert!(
            detect_background_with(&env("", None), &query, async { true }).await,
            "with no host probe, the OSC answer must stand"
        );
        // The env var alone is not enough: the CLI must be on PATH too.
        assert!(detect_background_with(&env("surf-1", None), &query, async { true }).await);
    }

    // Go: internal/host/background_test.go:97 TestDetectBackgroundLatch is NOT ported: Go latched
    // because termenv's OSC 11 query could block for seconds; `crate::ui::runtime::osc::detect_background`
    // has a 100 ms deadline and needs no latch (T3 spec §2.4, DIVERGENCES §D).
    #[tokio::test]
    async fn a_probe_that_does_not_know_falls_through() {
        let query: CmuxQuery =
            Arc::new(|_, _| Box::pin(std::future::ready(Some(b"not json".to_vec()))));
        assert!(
            detect_background_with(&env("surf-1", Some("/bin/cmux")), &query, async { true }).await
        );
    }
}
