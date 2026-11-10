//! Output streams (stderr warning sinks): `Streams` bundles the process stdout/stderr — tests inject buffers — and
//! prints the two warning shapes, `Warning: …` lines verbatim and `⚠ …` cautions.

use std::io::Write;

/// The two output streams a run writes to. Boxed `Write + Send` so `run`'s future (and `crate::headless::once`, which
/// takes `&mut (dyn Write + Send)`) stays `Send`.
pub struct Streams {
    /// Standard output: the reply, the JSON report, the `-l` listing.
    pub stdout: Box<dyn Write + Send>,
    /// Standard error: warnings, `Fetching available models...`, `Error: …`.
    pub stderr: Box<dyn Write + Send>,
}

impl Streams {
    /// The process's own stdout and stderr.
    pub fn process() -> Self {
        Self {
            stdout: Box::new(std::io::stdout()),
            stderr: Box::new(std::io::stderr()),
        }
    }

    /// Writes `{msg}\n` to stderr — the message already carries its `Warning: ` prefix where Go prints one.
    pub fn warning(&mut self, msg: &str) {
        let _ = writeln!(self.stderr, "{msg}");
    }

    /// Whether the process's stdin AND stderr are a terminal: a question asked on stderr can then be
    /// answered on stdin by the person at it (`cmd::trust`).
    pub fn can_ask() -> bool {
        use std::io::IsTerminal as _;
        std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
    }

    /// Writes `⚠ {msg}\n` to stderr (registry / dispatcher assembly warnings, root.go's `⚠` lines).
    pub fn caution(&mut self, msg: &str) {
        let _ = writeln!(self.stderr, "⚠ {msg}");
    }
}
