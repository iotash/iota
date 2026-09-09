//! The ported measuring instruments (markdown spec §Rust-mapping):
//! `visible`/`strip_ansi`/`sgr_params`/`trimmed_lines`/`blanks_between`/
//! `render_md_chunked`/`previewTrace` — the executable spec's instruments, ported
//! FIRST (`markdown_test.go`:14-69,1055-1071,1469-1565). Included by the sibling test
//! files as `crate::harness` (one module of the `markdown` test binary).
#![allow(dead_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use iota::markdown::{CodeTheme, PreviewHandle, RenderOptions, Sink, Writer, new_writer_to};

/// Shared byte buffer behind the `io::Write` the plain writer shape needs.
#[derive(Clone, Default)]
pub struct SharedOut(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for SharedOut {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl SharedOut {
    /// The captured output as a string.
    pub fn string(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

/// A fixed-width no-preview Sink capturing into a shared String — the `Writer::new`
/// shape for no-color renders (the crate has no process-global color flag).
pub struct CapSink {
    /// The captured rendered output.
    pub out: Arc<Mutex<String>>,
    /// The fixed width the sink reports.
    pub width: usize,
}

impl Sink for CapSink {
    fn write(&mut self, rendered: &str) {
        self.out.lock().expect("captured output").push_str(rendered);
    }

    fn width(&self) -> usize {
        self.width
    }

    fn block_preview(&mut self, _label: &str) -> Option<Box<dyn PreviewHandle>> {
        None
    }
}

/// Runs `src` through the pre-extraction test-writer shape (width 80, color on, no
/// previews — Go newTestWriter) and returns the RAW output, escapes intact.
pub fn render_md_raw(src: &str) -> String {
    let out = SharedOut::default();
    let mut w = new_writer_to(Box::new(out.clone()), 80);
    w.write(src.as_bytes());
    w.flush();
    out.string()
}

/// Runs `src` through a Writer and returns the visible output (Go renderMD).
pub fn render_md(src: &str) -> String {
    visible(&render_md_raw(src))
}

/// Runs `src` at an explicit width/color through `Writer::new` and returns RAW output.
pub fn render_md_opts(src: &str, width: usize, color: bool) -> String {
    let out = Arc::new(Mutex::new(String::new()));
    let mut w = Writer::new(
        Box::new(CapSink {
            out: Arc::clone(&out),
            width,
        }),
        RenderOptions {
            color,
            code_theme: CodeTheme::Monokai,
        },
    );
    w.write(src.as_bytes());
    w.flush();
    drop(w);
    out.lock().expect("captured output").clone()
}

/// Feeds `src` in fixed-size byte chunks (exercising split fence/formula lines across
/// Write calls) and returns the visible output (Go renderMDChunked).
pub fn render_md_chunked(src: &str, chunk: usize) -> String {
    let out = SharedOut::default();
    let mut w = new_writer_to(Box::new(out.clone()), 80);
    let b = src.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let end = (i + chunk).min(b.len());
        w.write(&b[i..end]);
        i = end;
    }
    w.flush();
    visible(&out.string())
}

/// Removes ALL ANSI escapes — SGR and OSC (hyperlinks) alike (Go xansi.Strip).
pub fn strip_ansi(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if b[i] == 0x1b && i + 1 < b.len() {
            match b[i + 1] {
                b'[' => {
                    let mut j = i + 2;
                    while j < b.len() && !(0x40..=0x7e).contains(&b[j]) {
                        j += 1;
                    }
                    i = (j + 1).min(b.len());
                    continue;
                }
                b']' => {
                    let mut j = i + 2;
                    i = loop {
                        if j >= b.len() {
                            break b.len();
                        }
                        if b[j] == 0x07 {
                            break j + 1;
                        }
                        if b[j] == 0x1b && b.get(j + 1) == Some(&b'\\') {
                            break j + 2;
                        }
                        j += 1;
                    };
                    continue;
                }
                b'\\' => {
                    i += 2;
                    continue;
                }
                _ => {}
            }
        }
        let n = s[i..].chars().next().map_or(1, char::len_utf8);
        out.push_str(&s[i..i + n]);
        i += n;
    }
    out
}

/// Strips ANSI escapes, leaving the text the user actually sees.
pub fn visible(s: &str) -> String {
    strip_ansi(s)
}

/// Every parameter of every SGR sequence in `s`, as a set — styles asserted
/// regardless of how the params are grouped into sequences (Go sgrParams).
pub fn sgr_params(s: &str) -> HashSet<String> {
    let mut params = HashSet::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == 0x1b && i + 1 < b.len() && b[i + 1] == b'[' {
            let mut j = i + 2;
            while j < b.len() && (b[j].is_ascii_digit() || b[j] == b';') {
                j += 1;
            }
            if j < b.len() && b[j] == b'm' {
                for p in s[i + 2..j].split(';') {
                    if !p.is_empty() {
                        params.insert(p.to_owned());
                    }
                }
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    params
}

/// Splits rendered output into lines with trailing padding spaces removed (block
/// renders pad to a uniform width) — Go trimmedLines.
pub fn trimmed_lines(s: &str) -> Vec<String> {
    s.trim_end_matches('\n')
        .split('\n')
        .map(|l| l.trim_end_matches(' ').to_owned())
        .collect()
}

/// Line-exact comparison over `trimmed_lines` (Go assertLines).
#[track_caller]
pub fn assert_lines(got: &str, want: &[&str]) {
    let lines = trimmed_lines(got);
    assert_eq!(
        lines.len(),
        want.len(),
        "got {} lines, want {}:\n{got}",
        lines.len(),
        want.len()
    );
    for (i, w) in want.iter().enumerate() {
        assert_eq!(&lines[i], w, "line {i} of:\n{got}");
    }
}

/// Blank lines strictly between the first line containing `start` and the next line
/// containing `end` (Go blanksBetween) — the anchor-based instrument behind the
/// "exactly one blank around every block" invariant.
#[track_caller]
pub fn blanks_between(rendered: &str, start: &str, end: &str) -> usize {
    let lines: Vec<&str> = rendered.trim_end_matches('\n').split('\n').collect();
    let Some(from) = lines.iter().position(|l| l.trim().contains(start)) else {
        panic!("start anchor {start:?} not found in:\n{rendered}");
    };
    let mut blanks = 0;
    for line in &lines[from + 1..] {
        if line.trim().contains(end) {
            return blanks;
        }
        if line.trim().is_empty() {
            blanks += 1;
        }
    }
    panic!("end anchor {end:?} not found after {start:?} in:\n{rendered}");
}

/// The previewTrace scripted sink (markdown_test.go:1469-1504): records the
/// interleaving of committed lines and preview opens — what block spacing during
/// streaming actually looks like. Events: `LINE` / `BLANK` / `PREVIEW`.
pub struct TraceSink {
    /// The recorded `LINE`/`BLANK`/`PREVIEW` events, in order.
    pub events: Arc<Mutex<Vec<String>>>,
    buf: String,
}

struct NopHandle;

impl PreviewHandle for NopHandle {
    fn write_raw_line(&mut self, _line: &str) {}
    fn close(&mut self) {}
}

impl Sink for TraceSink {
    fn write(&mut self, rendered: &str) {
        self.buf.push_str(rendered);
        while let Some(i) = self.buf.find('\n') {
            let mut line: String = self.buf.drain(..=i).collect();
            line.pop(); // the '\n'
            let kind = if strip_ansi(&line).trim().is_empty() {
                "BLANK"
            } else {
                "LINE"
            };
            self.events.lock().expect("trace").push(kind.to_owned());
        }
    }

    fn width(&self) -> usize {
        80
    }

    fn block_preview(&mut self, _label: &str) -> Option<Box<dyn PreviewHandle>> {
        self.events
            .lock()
            .expect("trace")
            .push("PREVIEW".to_owned());
        Some(Box::new(NopHandle))
    }
}

/// Runs `src` through a Writer over a [`TraceSink`] and returns the event trace
/// (Go renderTrace).
pub fn render_trace(src: &str) -> Vec<String> {
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut w = Writer::new(
        Box::new(TraceSink {
            events: Arc::clone(&events),
            buf: String::new(),
        }),
        RenderOptions {
            color: true,
            code_theme: CodeTheme::Monokai,
        },
    );
    w.write(src.as_bytes());
    w.flush();
    drop(w);
    events.lock().expect("trace").clone()
}

/// The trace contract (Go assertPreviewFollowsBlank): the first PREVIEW event is
/// immediately preceded by exactly one BLANK, never two.
#[track_caller]
pub fn assert_preview_follows_blank(events: &[String]) {
    for (i, e) in events.iter().enumerate() {
        if e != "PREVIEW" {
            continue;
        }
        assert!(
            i > 0 && events[i - 1] == "BLANK",
            "preview not preceded by its separator: {events:?}"
        );
        assert!(
            !(i >= 2 && events[i - 2] == "BLANK"),
            "doubled separator before the preview: {events:?}"
        );
        return;
    }
    panic!("no preview opened: {events:?}");
}

// ---- instrument self-tests (run once per including binary; cheap) ----

#[test]
fn instrument_sgr_params_and_strip() {
    let s = "\x1b[1;4mTop\x1b[0m plain \x1b]8;;http://x\x1b\\docs\x1b]8;;\x1b\\";
    let p = sgr_params(s);
    assert!(p.contains("1") && p.contains("4") && p.contains("0"));
    assert_eq!(strip_ansi(s), "Top plain docs");
    assert_eq!(trimmed_lines("a  \nb\n"), ["a", "b"]);
}
