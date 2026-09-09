//! Server-sent events reader (internal/llm/sse.go): `Event` and the cancellable `Sse` body reader.

use std::pin::Pin;

use bytes::{Bytes, BytesMut};
use futures::{Stream, StreamExt};
use tokio_util::sync::CancellationToken;

use super::error::LlmError;

/// The logical end-of-stream sentinel (prefix match on the joined data).
const DONE: &[u8] = b"[DONE]";

/// One dispatched SSE event: the `event:` type (last wins) and the `data:` lines joined with `\n`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Event {
    /// The `event:` field; `""` when absent.
    pub kind: String,
    /// The joined `data:` payload.
    pub data: Vec<u8>,
}

type Body = Pin<Box<dyn Stream<Item = reqwest::Result<Bytes>> + Send>>;

/// A streaming SSE body. Grammar (sse.go:36-98): lines end at `\n`, all trailing `\r`/`\n` trimmed; a blank line
/// dispatches pending data (a `[DONE]`-prefixed payload sets `done` instead); `:` lines are comments; the field
/// splits at the first `:` with one leading space stripped from the value; `data` joins with `\n`, `event` last
/// wins, other fields are ignored. At EOF pending non-`[DONE]` data is dispatched. The line buffer is unbounded.
pub struct Sse {
    body: Body,
    buf: BytesMut,
    /// Bytes of `buf` already scanned for a `\n` (so a huge line is not rescanned per chunk).
    scanned: usize,
    saw_event: bool,
    done: bool,
    eof: bool,
    cancel: CancellationToken,
}

impl std::fmt::Debug for Sse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sse")
            .field("buffered", &self.buf.len())
            .field("saw_event", &self.saw_event)
            .field("done", &self.done)
            .field("eof", &self.eof)
            .finish_non_exhaustive()
    }
}

impl Sse {
    /// Wraps a byte stream; `cancel` aborts any pending read with `LlmError::Cancelled`.
    pub fn new(
        body: impl Stream<Item = reqwest::Result<Bytes>> + Send + 'static,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            body: Box::pin(body),
            buf: BytesMut::new(),
            scanned: 0,
            saw_event: false,
            done: false,
            eof: false,
            cancel,
        }
    }

    /// Ok(Some(ev)) next event; Ok(None) clean end; Err(Transport|Cancelled) otherwise.
    pub async fn next(&mut self) -> Result<Option<Event>, LlmError> {
        let mut kind = String::new();
        let mut data: Vec<u8> = Vec::new();
        let mut have_data = false;
        loop {
            let (line, eof) = self.read_line().await?;
            let line = trim_line_end(&line);

            if line.is_empty() && !eof {
                // Blank line: dispatch if an event accumulated.
                if !have_data {
                    kind.clear();
                    continue;
                }
                if data.starts_with(DONE) {
                    self.done = true;
                    data.clear();
                    have_data = false;
                    kind.clear();
                    continue; // drain the remainder
                }
                self.saw_event = true;
                return Ok(Some(Event { kind, data }));
            }

            if let Some((&first, _)) = line.split_first() {
                if first == b':' {
                    // Comment.
                    if !eof {
                        continue;
                    }
                } else {
                    let (field, value) = match line.iter().position(|&b| b == b':') {
                        Some(i) => {
                            let mut v = &line[i + 1..];
                            if let Some(rest) = v.strip_prefix(b" ") {
                                v = rest;
                            }
                            (&line[..i], v)
                        }
                        None => (line, &b""[..]),
                    };
                    match field {
                        b"data" => {
                            if have_data {
                                data.push(b'\n');
                            }
                            data.extend_from_slice(value);
                            have_data = true;
                        }
                        b"event" => kind = String::from_utf8_lossy(value).into_owned(),
                        _ => {}
                    }
                }
            }

            if eof {
                // A final event without a trailing blank line still counts.
                if have_data && !data.starts_with(DONE) {
                    self.saw_event = true;
                    return Ok(Some(Event { kind, data }));
                }
                return Ok(None);
            }
        }
    }

    /// At least one event OR the `[DONE]` sentinel was parsed.
    pub fn saw_event(&self) -> bool {
        self.saw_event || self.done
    }

    /// Whether the `[DONE]` sentinel was seen.
    pub fn done(&self) -> bool {
        self.done
    }

    /// Go `bufio.Reader.ReadBytes('\n')`: the next line INCLUDING its `\n`, or at EOF whatever remains with
    /// the `eof` flag set (an empty line + `eof` once the body is exhausted).
    async fn read_line(&mut self) -> Result<(Bytes, bool), LlmError> {
        loop {
            if let Some(pos) = self.buf[self.scanned..].iter().position(|&b| b == b'\n') {
                let line = self.buf.split_to(self.scanned + pos + 1).freeze();
                self.scanned = 0;
                return Ok((line, false));
            }
            self.scanned = self.buf.len();
            if self.eof {
                self.scanned = 0;
                return Ok((self.buf.split().freeze(), true));
            }
            let chunk = tokio::select! {
                biased;
                () = self.cancel.cancelled() => return Err(LlmError::Cancelled),
                c = self.body.next() => c,
            };
            match chunk {
                Some(Ok(bytes)) => self.buf.extend_from_slice(&bytes),
                Some(Err(e)) => {
                    self.eof = true;
                    return Err(LlmError::Transport(e));
                }
                None => self.eof = true,
            }
        }
    }
}

/// `bytes.TrimRight(line, "\r\n")`.
fn trim_line_end(line: &[u8]) -> &[u8] {
    let mut end = line.len();
    while end > 0 && matches!(line[end - 1], b'\r' | b'\n') {
        end -= 1;
    }
    &line[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trim_line_end_strips_every_trailing_cr_lf() {
        assert_eq!(trim_line_end(b"abc\r\n"), b"abc");
        assert_eq!(trim_line_end(b"abc\n\r\n\n"), b"abc");
        assert_eq!(trim_line_end(b"\r\n"), b"");
        assert_eq!(trim_line_end(b"a\rb\n"), b"a\rb");
    }
}
