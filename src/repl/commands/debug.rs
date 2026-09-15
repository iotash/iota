//! `/debug [on|off]` (chat/debug.go, run.go:862-915): `on`/`off` toggle request recording on the
//! run's [`RequestLog`] (a notice + the status row's `debug` segment); bare `/debug` opens the
//! two-tab inspector — `Messages` (newest first, `{time} {action} {summary} {status} {dur}` rows,
//! refreshed every 500 ms) and a `Verbose` switch — whose Enter drills into `↑ Request` /
//! `↓ Response` pretty-printed views, in a loop until cancelled.
//!
//! Columns are told apart by STYLE, never by separators: the row-selection highlight washes the
//! colours out, so each column gets its own treatment (dim time, cyan action, UNDERLINED summary,
//! outcome-coloured status, dim duration) and no row leaks the raw method or URL — they carry no
//! meaning to someone scanning the log.
//!
//! Two of debug.go's functions are NOT here: `lastUserText` and `contentText` live in
//! [`crate::llm::reqlog`], because `RequestEntry::new` pre-computes each entry's summary from
//! them at capture time and the wire layer cannot reach up into the loop (DEVIATIONS3 `[WP66]`).
//! [`summary_col`] therefore reads the cached `RequestEntry::summary`, which is Go's
//! `strings.Join(strings.Fields(lastUserText(e.ReqBody)), " ")` computed once instead of once
//! per 500 ms refresh tick.

use std::sync::Arc;
use std::time::Duration;

use crate::llm::reqlog::{RequestEntry, ResponseHalf};
use crate::repl::render::styles::{cyan, dim, green, red, underline, yellow};
use crate::repl::run::Repl;
use crate::text::go_duration;
use crate::text::width::{str_width, truncate_cols};
use crate::ui::facade::{Panel, TabbedSpec};

/// The summary column's width (debug.go:13).
pub(crate) const SUMMARY_COL_WIDTH: usize = 30;
/// Notice after `/debug on` (run.go:867,894).
pub(crate) const RECORDING_ON: &str = "Request recording ON — activity groups stay expanded";
/// Notice after `/debug off` (run.go:872,896).
pub(crate) const RECORDING_OFF: &str = "Request recording OFF";
/// The list tab's title (run.go:880).
pub(crate) const TAB_MESSAGES: &str = "Messages";
/// The switch tab's title (run.go:882).
pub(crate) const TAB_VERBOSE: &str = "Verbose";
/// The request view's title (run.go:910).
pub(crate) const TAB_REQUEST: &str = "↑ Request";
/// The response view's title (run.go:911).
pub(crate) const TAB_RESPONSE: &str = "↓ Response";
/// The list's refresh period.
pub(crate) const DEBUG_REFRESH_MS: u64 = 500;

/// Placeholder for an entry with no user text (debug.go:40).
const SUMMARY_EMPTY: &str = "—";
/// Status of an entry whose round-trip has not answered yet (debug.go:55).
const STATUS_PENDING: &str = "…";
/// Status of an entry whose round-trip failed at the transport (debug.go:58).
const STATUS_ERROR: &str = "ERR";

/// One row per entry: `{at}  {action}  {summary}  {status}  {dur}` (debug.go:22-31).
pub(crate) fn request_rows(entries: &[Arc<RequestEntry>]) -> Vec<String> {
    entries
        .iter()
        .map(|e| {
            let resp = e.response();
            let at = dim(&e.time.strftime("%H:%M:%S").to_string());
            let action = cyan(&go_pad(&action_from_url(&e.url), 6));
            let dur = if resp.duration.is_zero() {
                String::new()
            } else {
                dim(&go_duration(round_ms(resp.duration)))
            };
            format!(
                "{at}  {action}  {}  {}  {dur}",
                summary_col(e),
                status_col(&resp)
            )
        })
        .collect()
}

/// The padded, underlined summary column (`—` dim when empty; debug.go:35-48).
///
/// The padding is plain spaces OUTSIDE the SGR pair, so the underline covers the text and not
/// the column's tail.
pub(crate) fn summary_col(e: &RequestEntry) -> String {
    if e.summary.is_empty() {
        // e.g. a model listing carries no user text.
        return dim(SUMMARY_EMPTY)
            + &" ".repeat(SUMMARY_COL_WIDTH.saturating_sub(str_width(SUMMARY_EMPTY)));
    }
    let text = truncate_width(&e.summary, SUMMARY_COL_WIDTH);
    let pad = SUMMARY_COL_WIDTH.saturating_sub(str_width(&text));
    underline(&text) + &" ".repeat(pad)
}

/// The coloured status column (`…` pending, `ERR` red, the code green/red/yellow; debug.go:53-76).
pub(crate) fn status_col(resp: &ResponseHalf) -> String {
    let (code, style): (&str, fn(&str) -> String) = if resp.err.is_none() {
        match resp.status.split_whitespace().next() {
            None => (STATUS_PENDING, dim),
            Some(code) => (
                code,
                match code.parse::<u32>() {
                    Ok(200..=299) => green,
                    Ok(400..) => red,
                    Ok(_) => yellow,
                    Err(_) => dim,
                },
            ),
        }
    } else {
        (STATUS_ERROR, red)
    };
    style(&go_pad(code, 3))
}

/// Cuts `s` at `w` display columns, appending `…` when it cut (debug.go:80-95).
pub(crate) fn truncate_width(s: &str, w: usize) -> String {
    truncate_cols(s, w)
}

/// `Chat` | `Image` | `Models` | the last path segment | `Request` (debug.go:99-122).
///
/// Chat and Image are tested BEFORE Models because Gemini's `generateContent` path — and
/// Imagen's `:predict` path — also contain `/models`.
pub(crate) fn action_from_url(raw: &str) -> String {
    let parsed = reqwest::Url::parse(raw)
        .ok()
        .map(|u| u.path().to_owned())
        .filter(|p| !p.is_empty());
    let path = parsed.as_deref().unwrap_or(raw);
    let lower = path.to_lowercase();
    if [
        "chat/completions",
        "/messages",
        "/responses",
        "generatecontent",
    ]
    .iter()
    .any(|n| lower.contains(n))
    {
        return "Chat".to_owned();
    }
    if [":predict", "/images/generations", "/images/edits"]
        .iter()
        .any(|n| lower.contains(n))
    {
        return "Image".to_owned();
    }
    if lower.contains("/models") {
        return "Models".to_owned();
    }
    match path.trim_matches('/').rsplit('/').next() {
        Some(last) if !last.is_empty() => last.to_owned(),
        _ => "Request".to_owned(),
    }
}

/// `[dim "{METHOD} {url}", dim date, ""] ++ pretty body` (debug.go:203-209).
pub(crate) fn request_detail_lines(e: &RequestEntry) -> Vec<String> {
    let mut out = vec![
        dim(&format!("{} {}", e.method, e.url)),
        dim(&e.time.strftime("%Y-%m-%d %H:%M:%S").to_string()),
        String::new(),
    ];
    out.extend(pretty_body_lines(&e.req_body));
    out
}

/// `[dim "{status}   {dur}", ""] ++ pretty body` (debug.go:211-219).
pub(crate) fn response_detail_lines(e: &RequestEntry) -> Vec<String> {
    let resp = e.response();
    let status = if resp.err.is_none() {
        if resp.status.is_empty() {
            "(pending)".to_owned()
        } else {
            resp.status.clone()
        }
    } else {
        format!("error: {}", resp.err.as_deref().unwrap_or_default())
    };
    let mut out = vec![
        dim(&format!(
            "{status}   {}",
            go_duration(round_ms(resp.duration))
        )),
        String::new(),
    ];
    out.extend(pretty_body_lines(&resp.resp_body));
    out
}

/// `(empty)` | the JSON re-indented | the raw lines (debug.go:223-233).
///
/// A single JSON document is indented; anything else (an SSE stream of `data:` lines, say) is
/// shown verbatim, trailing newlines trimmed.
pub(crate) fn pretty_body_lines(body: &[u8]) -> Vec<String> {
    if body.is_empty() {
        return vec![dim("(empty)")];
    }
    let text = json_indent(body).unwrap_or_else(|| String::from_utf8_lossy(body).into_owned());
    text.trim_end_matches('\n')
        .split('\n')
        .map(str::to_owned)
        .collect()
}

/// Go `json.Indent(dst, body, "", "  ")` twin: a WHITESPACE-ONLY re-indent of the original
/// bytes, so key order, number spelling and string escapes survive byte for byte. `None` when
/// `body` is not exactly one valid JSON value (Go's scanner reports the same).
///
/// Round-tripping through `serde_json::Value` would be the obvious shortcut and is wrong here:
/// it re-orders object keys, re-spells `1.50` as `1.5` and rewrites escapes.
pub(crate) fn json_indent(body: &[u8]) -> Option<String> {
    let mut out: Vec<u8> = Vec::with_capacity(body.len() + body.len() / 4);
    let mut scanner = Scanner::new(body);
    scanner.value(&mut out, 0)?;
    scanner.skip_whitespace();
    // Go's `Indent` fails on anything after the first value (its scanner reports `scanError`).
    if scanner.pos < scanner.src.len() {
        return None;
    }
    Some(String::from_utf8_lossy(&out).into_owned())
}

/// The `/debug` arm (run.go:862-915).
///
/// `on`/`off` are the only recognised arguments; anything else opens the inspector, exactly as
/// bare `/debug` does. The loop is Go's v1 shape: commit → apply the switch wherever focus was →
/// drill into the highlighted row → reopen the list.
pub(crate) async fn cmd_debug(repl: &mut Repl, arg: &str) {
    let cancel = &repl.cancel.clone();
    match arg {
        "on" | "off" => {
            set_recording(repl, arg == "on");
            return;
        }
        _ => {}
    }
    loop {
        let log = Arc::clone(&repl.reqlog);
        let rows = move || request_rows(&log.entries());
        let spec = TabbedSpec {
            refresh_every_ms: DEBUG_REFRESH_MS,
            panels: vec![
                Panel::list(TAB_MESSAGES.to_owned(), rows())
                    .with_search(true)
                    .with_refresh(Box::new(rows)),
                Panel::switch(TAB_VERBOSE.to_owned(), repl.reqlog.verbose()),
            ],
            ..TabbedSpec::default()
        };
        let Ok(result) = repl.ui.tabbed(cancel, spec).await else {
            break;
        };
        if result.cancelled {
            break;
        }
        // Enter commits ALL tabs: the Verbose switch applies wherever focus was (flip on the
        // switch tab, Tab back, drill in — the flip still lands).
        if let Some(p) = result.panels.get(1)
            && p.on != repl.reqlog.verbose()
        {
            set_recording(repl, p.on);
        }
        if result.focused == 1 {
            break; // nothing to drill into from the switch tab
        }
        let entries = repl.reqlog.entries();
        let i = result.panels.first().map_or(usize::MAX, |p| p.cursor);
        let Some(entry) = entries.get(i) else {
            break;
        };
        // Drill into the entry, then reopen the list (v1 loop shape); the viewer's own commit is
        // discarded — it is a viewer, not a picker.
        let drill = TabbedSpec {
            panels: vec![
                Panel::view(TAB_REQUEST.to_owned(), request_detail_lines(entry)).with_wrap(true),
                Panel::view(TAB_RESPONSE.to_owned(), response_detail_lines(entry)).with_wrap(true),
            ],
            ..TabbedSpec::default()
        };
        let _ = repl.ui.tabbed(cancel, drill).await;
    }
}

/// Flips recording, prints the matching dim notice and republishes the status row's `debug`
/// segment (run.go:866-873,891-898).
fn set_recording(repl: &Repl, on: bool) {
    repl.reqlog.set_verbose(on);
    repl.tr
        .notice(if on { RECORDING_ON } else { RECORDING_OFF });
    repl.push_status();
}

/// Go `Duration.Round(time.Millisecond)`: half away from zero.
fn round_ms(d: Duration) -> Duration {
    const MS: u128 = 1_000_000;
    let ns = d.as_nanos();
    let rem = ns % MS;
    let rounded = if rem + rem < MS {
        ns - rem
    } else {
        ns - rem + MS
    };
    Duration::from_nanos(u64::try_from(rounded).unwrap_or(u64::MAX))
}

/// Go `fmt.Sprintf("%-Ns", s)`: right-pad to `n` BYTES (`…` is three bytes wide, so a pending
/// status column pads to nothing — exactly as Go renders it).
fn go_pad(s: &str, n: usize) -> String {
    let mut out = s.to_owned();
    out.push_str(&" ".repeat(n.saturating_sub(s.len())));
    out
}

/// A byte scanner over one JSON document, re-emitting it with two-space indentation and no other
/// change (the `json_indent` engine — Go's `encoding/json` scanner, reduced to what `Indent`
/// needs).
struct Scanner<'a> {
    src: &'a [u8],
    pos: usize,
}

impl<'a> Scanner<'a> {
    fn new(src: &'a [u8]) -> Self {
        Self { src, pos: 0 }
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.src.get(self.pos), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip_whitespace();
        self.src.get(self.pos).copied()
    }

    fn eat(&mut self, b: u8) -> Option<()> {
        (self.peek()? == b).then(|| self.pos += 1)
    }

    /// `\n` + `indent * depth` (Go's `newline`).
    fn newline(out: &mut Vec<u8>, depth: usize) {
        out.push(b'\n');
        out.extend(std::iter::repeat_n(b' ', depth * 2));
    }

    /// One JSON value, re-indented into `out`; `None` on the first byte that is not valid JSON.
    fn value(&mut self, out: &mut Vec<u8>, depth: usize) -> Option<()> {
        match self.peek()? {
            b'{' => self.container(out, depth, b'{', b'}'),
            b'[' => self.container(out, depth, b'[', b']'),
            b'"' => self.string(out),
            b't' => self.literal(out, b"true"),
            b'f' => self.literal(out, b"false"),
            b'n' => self.literal(out, b"null"),
            b'-' | b'0'..=b'9' => self.number(out),
            _ => None,
        }
    }

    /// An object or array. Both are emitted `{}` / `[]` when empty (Go delays the indent for
    /// exactly that reason), one member per line otherwise.
    fn container(&mut self, out: &mut Vec<u8>, depth: usize, open: u8, close: u8) -> Option<()> {
        self.eat(open)?;
        out.push(open);
        if self.peek()? == close {
            self.pos += 1;
            out.push(close);
            return Some(());
        }
        loop {
            Self::newline(out, depth + 1);
            if open == b'{' {
                self.string(out)?;
                self.eat(b':')?;
                out.extend_from_slice(b": ");
            }
            self.value(out, depth + 1)?;
            match self.peek()? {
                b',' => {
                    self.pos += 1;
                    out.push(b',');
                }
                c if c == close => {
                    self.pos += 1;
                    Self::newline(out, depth);
                    out.push(close);
                    return Some(());
                }
                _ => return None,
            }
        }
    }

    /// A quoted string, copied byte for byte (escapes and all).
    fn string(&mut self, out: &mut Vec<u8>) -> Option<()> {
        self.eat(b'"')?;
        out.push(b'"');
        loop {
            let c = *self.src.get(self.pos)?;
            self.pos += 1;
            out.push(c);
            match c {
                b'"' => return Some(()),
                // A raw control byte is invalid inside a JSON string.
                0x00..=0x1f => return None,
                b'\\' => {
                    let esc = *self.src.get(self.pos)?;
                    self.pos += 1;
                    out.push(esc);
                    match esc {
                        b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => {}
                        b'u' => {
                            for _ in 0..4 {
                                let h = *self.src.get(self.pos)?;
                                if !h.is_ascii_hexdigit() {
                                    return None;
                                }
                                self.pos += 1;
                                out.push(h);
                            }
                        }
                        _ => return None,
                    }
                }
                _ => {}
            }
        }
    }

    /// `true` / `false` / `null`, copied verbatim.
    fn literal(&mut self, out: &mut Vec<u8>, word: &[u8]) -> Option<()> {
        if self.src.get(self.pos..self.pos + word.len())? != word {
            return None;
        }
        self.pos += word.len();
        out.extend_from_slice(word);
        Some(())
    }

    /// A number, copied verbatim — the spelling (`1.50`, `1e5`, `-0`) is preserved.
    fn number(&mut self, out: &mut Vec<u8>) -> Option<()> {
        let start = self.pos;
        if self.src.get(self.pos) == Some(&b'-') {
            self.pos += 1;
        }
        match self.src.get(self.pos)? {
            b'0' => self.pos += 1,
            b'1'..=b'9' => self.digits(),
            _ => return None,
        }
        if self.src.get(self.pos) == Some(&b'.') {
            self.pos += 1;
            if !self.src.get(self.pos)?.is_ascii_digit() {
                return None;
            }
            self.digits();
        }
        if matches!(self.src.get(self.pos), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.src.get(self.pos), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if !self.src.get(self.pos)?.is_ascii_digit() {
                return None;
            }
            self.digits();
        }
        out.extend_from_slice(self.src.get(start..self.pos)?);
        Some(())
    }

    fn digits(&mut self) {
        while self.src.get(self.pos).is_some_and(u8::is_ascii_digit) {
            self.pos += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        SUMMARY_COL_WIDTH, action_from_url, go_pad, json_indent, pretty_body_lines,
        request_detail_lines, request_rows, response_detail_lines, round_ms, status_col,
        summary_col, truncate_width,
    };
    use crate::llm::reqlog::{RequestEntry, ResponseHalf};
    use crate::text::ansi::strip_sgr;
    use crate::text::width::str_width;
    use pretty_assertions::assert_eq;
    use std::sync::Arc;
    use std::time::Duration;

    /// 2026-07-10 15:04:05 UTC — Go's fixture instant (`debug_test.go:66`).
    fn at() -> jiff::Zoned {
        jiff::civil::date(2026, 7, 10)
            .at(15, 4, 5, 0)
            .to_zoned(jiff::tz::TimeZone::UTC)
            .unwrap()
    }

    /// An entry stamped at the fixture instant.
    fn entry(method: &str, url: &str, body: &[u8]) -> RequestEntry {
        let mut e = RequestEntry::new(method, url, body);
        e.time = at();
        e
    }

    fn done(e: &RequestEntry, status: &str, ms: u64) {
        e.set_status(status.to_owned());
        e.set_duration(Duration::from_millis(ms));
    }

    fn resp(status: &str, err: &str) -> ResponseHalf {
        ResponseHalf {
            status: status.to_owned(),
            err: (!err.is_empty()).then(|| err.to_owned()),
            ..ResponseHalf::default()
        }
    }

    /// Go: `chat/debug_test.go:14` `TestActionFromURL` — API endpoints map to short, provider-agnostic
    /// action names. Chat and Image are decided BEFORE Models because Gemini's `generateContent`
    /// path and Imagen's `:predict` path also contain `/models`.
    #[test]
    fn action_from_url_maps_endpoints() {
        for (url, want) in [
            ("https://api.anthropic.com/v1/messages", "Chat"),
            ("https://api.openai.com/v1/chat/completions", "Chat"),
            ("https://api.openai.com/v1/responses", "Chat"),
            (
                "https://generativelanguage.googleapis.com/v1beta/models/gemini:generateContent",
                "Chat",
            ),
            ("https://api.openai.com/v1/models", "Models"),
            (
                "https://generativelanguage.googleapis.com/v1beta/models",
                "Models",
            ),
            (
                "https://generativelanguage.googleapis.com/v1beta/models/imagen-4.0:predict",
                "Image",
            ),
            (
                "https://zenmux.ai/v1/publishers/bytedance/models/doubao-seedream-5.0:predict",
                "Image",
            ),
            ("https://api.openai.com/v1/images/generations", "Image"),
            ("https://api.openai.com/v1/images/edits", "Image"),
        ] {
            assert_eq!(action_from_url(url), want, "{url}");
        }
    }

    /// The fallbacks Go reaches past its table: a relay image GET is named by its last path
    /// segment, and a URL with no usable segment is a bare `Request` (debug.go:119-122).
    #[test]
    fn action_from_url_falls_back_to_the_last_segment() {
        assert_eq!(
            action_from_url("https://cdn.example/img/out.png"),
            "out.png"
        );
        assert_eq!(action_from_url("https://cdn.example/"), "Request");
        assert_eq!(action_from_url("https://cdn.example"), "Request");
        // Unparseable (a bare path): Go keeps the raw string and splits that.
        assert_eq!(action_from_url("/v1/thing"), "thing");
        assert_eq!(action_from_url(""), "Request");
    }

    /// Go: `chat/debug_test.go:63` `TestRequestRowsStyled` — a row carries the time, action, summary
    /// and status WITH styling, and never the raw method or URL.
    #[test]
    fn request_rows_are_styled_and_leak_no_url() {
        let e = entry(
            "POST",
            "https://api.anthropic.com/v1/messages",
            r#"{"messages":[{"role":"user","content":"你好世界"}]}"#.as_bytes(),
        );
        done(&e, "200 OK", 1200);
        let rows = request_rows(&[Arc::new(e)]);
        let row = &rows[0];
        assert!(row.contains('\x1b'), "row is not styled: {row:?}");
        let plain = strip_sgr(row);
        for want in ["15:04:05", "Chat", "你好世界", "200", "1.2s"] {
            assert!(plain.contains(want), "row {plain:?} missing {want:?}");
        }
        assert!(
            !plain.contains("POST") && !plain.contains("/v1/messages"),
            "row leaked the raw method/URL: {plain:?}"
        );
    }

    /// A round-trip still in flight has no duration column and a dim `…` status; the columns are
    /// separated by exactly two spaces (debug.go:30).
    #[test]
    fn a_pending_row_has_no_duration() {
        let e = entry("POST", "https://api.openai.com/v1/chat/completions", b"{}");
        let rows = request_rows(&[Arc::new(e)]);
        let plain = strip_sgr(&rows[0]);
        assert_eq!(
            plain,
            format!("15:04:05  Chat    {:<30}  …  ", "—"),
            "pending row shape"
        );
    }

    /// Go: `chat/debug_test.go:92` `TestStatusColOutcome` — the status column is coloured by outcome
    /// and a bodiless request shows the `—` summary placeholder.
    #[test]
    fn status_col_colours_by_outcome() {
        let ok = status_col(&resp("200 OK", ""));
        let bad = status_col(&resp("401 Unauthorized", ""));
        assert!(strip_sgr(&ok).contains("200") && strip_sgr(&bad).contains("401"));
        assert_ne!(strip_sgr(&ok), ok, "2xx must carry ANSI");
        assert_ne!(strip_sgr(&bad), bad, "4xx must carry ANSI");
        assert_ne!(ok, bad, "2xx and 4xx must differ in style");

        let listing = entry("GET", "https://api.openai.com/v1/models", b"");
        let summary = strip_sgr(&summary_col(&listing));
        assert!(
            summary.trim_start().starts_with('—'),
            "empty summary should be the — placeholder, got {summary:?}"
        );
        assert_eq!(
            str_width(&summary),
            SUMMARY_COL_WIDTH,
            "padded to 30 columns"
        );
    }

    /// The remaining status shapes: a transport failure is `ERR`, a 3xx is yellow, a pending entry
    /// is a dim `…` and a non-numeric status keeps the dim style (debug.go:53-76). `%-3s` pads by
    /// BYTES, so `…` (three bytes) pads to nothing.
    #[test]
    fn status_col_covers_every_branch() {
        assert_eq!(
            strip_sgr(&status_col(&resp("", "dial tcp: refused"))),
            "ERR"
        );
        assert_eq!(strip_sgr(&status_col(&resp("", ""))), "…");
        assert_eq!(strip_sgr(&status_col(&resp("302 Found", ""))), "302");
        assert_eq!(strip_sgr(&status_col(&resp("OK", ""))), "OK ");
        // An error wins over a status that already arrived.
        assert_eq!(
            strip_sgr(&status_col(&resp("200 OK", "broken pipe"))),
            "ERR"
        );
        // 3xx yellow differs from both 2xx green and 4xx red.
        let (y, g, r) = (
            status_col(&resp("302 Found", "")),
            status_col(&resp("200 OK", "")),
            status_col(&resp("500 Internal Server Error", "")),
        );
        assert_ne!(y[..5], g[..5]);
        assert_ne!(y[..5], r[..5]);
        assert_eq!(go_pad("ab", 4), "ab  ");
        assert_eq!(go_pad("…", 3), "…", "three bytes already fill %-3s");
    }

    /// The summary column collapses whitespace, truncates at 30 COLUMNS (CJK counted double) and
    /// pads with plain spaces outside the SGR pair, so the underline covers only the text.
    #[test]
    fn summary_col_truncates_by_display_width() {
        let long = "x".repeat(40);
        let e = entry(
            "POST",
            "https://api.openai.com/v1/chat/completions",
            format!(r#"{{"messages":[{{"role":"user","content":"{long}"}}]}}"#).as_bytes(),
        );
        let col = summary_col(&e);
        let plain = strip_sgr(&col);
        assert_eq!(str_width(&plain), SUMMARY_COL_WIDTH);
        assert!(plain.ends_with('…'));
        assert!(
            col.ends_with("\u{1b}[0m") || col.ends_with(' '),
            "padding sits outside the SGR pair"
        );
        assert_eq!(truncate_width("abc", 30), "abc");
        assert_eq!(truncate_width("中文标题", 4), "中…");
    }

    /// Go rounds the duration to the millisecond, half away from zero, before rendering it.
    #[test]
    fn durations_round_to_the_millisecond() {
        assert_eq!(
            round_ms(Duration::from_nanos(1_499_999)),
            Duration::from_millis(1)
        );
        assert_eq!(
            round_ms(Duration::from_micros(1500)),
            Duration::from_millis(2)
        );
        assert_eq!(round_ms(Duration::ZERO), Duration::ZERO);
    }

    /// Go: chat/debug.go:203-219 — the two drill-down heads, including the `(pending)` and
    /// `error: …` response shapes and the three-space gap before the duration.
    #[test]
    fn detail_lines_head_the_request_and_the_response() {
        let e = entry(
            "POST",
            "https://api.openai.com/v1/responses",
            br#"{"input":"hi"}"#,
        );
        let req: Vec<String> = request_detail_lines(&e)
            .iter()
            .map(|l| strip_sgr(l))
            .collect();
        assert_eq!(
            req,
            vec![
                "POST https://api.openai.com/v1/responses".to_owned(),
                "2026-07-10 15:04:05".to_owned(),
                String::new(),
                "{".to_owned(),
                r#"  "input": "hi""#.to_owned(),
                "}".to_owned(),
            ]
        );
        // Pending: no status, no body, zero duration.
        let pending: Vec<String> = response_detail_lines(&e)
            .iter()
            .map(|l| strip_sgr(l))
            .collect();
        assert_eq!(
            pending,
            vec![
                "(pending)   0s".to_owned(),
                String::new(),
                "(empty)".to_owned()
            ]
        );
        // Answered.
        done(&e, "201 Created", 1200);
        e.append_body(b"pong");
        let ok: Vec<String> = response_detail_lines(&e)
            .iter()
            .map(|l| strip_sgr(l))
            .collect();
        assert_eq!(
            ok,
            vec![
                "201 Created   1.2s".to_owned(),
                String::new(),
                "pong".to_owned()
            ]
        );
        // A transport failure replaces the status line.
        let broken = entry("GET", "https://x/y", b"");
        broken.set_err("dial tcp: refused".to_owned(), Duration::from_millis(30));
        assert_eq!(
            strip_sgr(&response_detail_lines(&broken)[0]),
            "error: dial tcp: refused   30ms"
        );
    }

    /// `json_indent` is a WHITESPACE-ONLY re-indent: key order, number spelling and string escapes
    /// survive byte for byte, and a body that is not exactly one JSON value is left alone.
    #[test]
    fn json_indent_is_whitespace_only() {
        assert_eq!(
            json_indent(br#"{"b":1,"a":2}"#).as_deref(),
            Some("{\n  \"b\": 1,\n  \"a\": 2\n}"),
            "keys keep the document's order — never serde's"
        );
        assert_eq!(
            json_indent(br#"{"n":1.50,"e":1e5,"z":-0}"#).as_deref(),
            Some("{\n  \"n\": 1.50,\n  \"e\": 1e5,\n  \"z\": -0\n}"),
            "numbers keep their spelling"
        );
        assert_eq!(
            json_indent(br#"{"s":"a<b\n\"q\"\u003cz"}"#).as_deref(),
            Some(
                r#"{
  "s": "a<b\n\"q\"\u003cz"
}"#
            ),
            "string bytes are copied verbatim — no escape is added, none is resolved"
        );
        assert_eq!(
            json_indent(br#"{"a":[1,2],"b":{},"c":[]}"#).as_deref(),
            Some("{\n  \"a\": [\n    1,\n    2\n  ],\n  \"b\": {},\n  \"c\": []\n}"),
            "empty containers stay on one line"
        );
        // Whitespace anywhere outside a string is re-made; a top-level scalar passes through.
        assert_eq!(json_indent(b"  { }  ").as_deref(), Some("{}"));
        assert_eq!(json_indent(b"123").as_deref(), Some("123"));
        assert_eq!(json_indent(br#""hi""#).as_deref(), Some(r#""hi""#));
        assert_eq!(json_indent(b"true\n").as_deref(), Some("true"));
    }

    /// Anything that is not one valid JSON value is `None` — most importantly an SSE stream, which
    /// the drill-down then shows verbatim.
    #[test]
    fn json_indent_rejects_everything_else() {
        for bad in [
            &b"data: {\"a\":1}\n\ndata: [DONE]\n\n"[..],
            b"{\"a\":1} trailing",
            b"{\"a\":1}{\"b\":2}",
            b"{\"a\":}",
            b"{'a':1}",
            b"[1,2,]",
            b"{\"a\":01}",
            b"",
        ] {
            assert_eq!(
                json_indent(bad),
                None,
                "{:?} is not one JSON value",
                String::from_utf8_lossy(bad)
            );
        }
    }

    /// Go: chat/debug.go:223-233 — an empty body is `(empty)`, a JSON body is indented, anything
    /// else is shown verbatim with its trailing newlines trimmed.
    #[test]
    fn pretty_body_lines_picks_json_or_raw() {
        assert_eq!(pretty_body_lines(b"").len(), 1);
        assert_eq!(strip_sgr(&pretty_body_lines(b"")[0]), "(empty)");
        assert_eq!(
            pretty_body_lines(br#"{"a":1}"#),
            vec!["{".to_owned(), r#"  "a": 1"#.to_owned(), "}".to_owned()]
        );
        assert_eq!(
            pretty_body_lines(b"data: one\n\ndata: two\n\n"),
            vec![
                "data: one".to_owned(),
                String::new(),
                "data: two".to_owned(),
            ]
        );
    }
}
