//! A byte-exact twin of Go's `mime/multipart.Writer` as `internal/llm/images.go:180-208` drives
//! it (T3 design §4.4, CONTRACTS §3.6): `--<boundary>\r\n` first, `\r\n--<boundary>\r\n` between
//! parts, headers in sorted key order, a bare `\r\n`, the bytes, and `\r\n--<boundary>--\r\n` to
//! close. Three escaping rules (stdlib `escapeQuotes` for field names, the raw field constant for
//! file-part names, images.go's `quoteEscaper` for filenames). The `Bytes` body is what
//! `Client::send_payload` records and clones per attempt — reqwest's `multipart` feature stays
//! off (T-41).

use bytes::Bytes;

/// A multipart/form-data body under construction.
///
/// Parts are appended in call order (Go's `Writer` is append-only too), which is what puts the
/// text fields ahead of the file parts: relay gateways sniff `model` in the form's leading bytes,
/// and a multi-MB image part first pushes it out of the sniff window (images.go:180-186).
pub(crate) struct MultipartForm {
    /// The boundary token, without the leading dashes.
    boundary: String,
    /// The encoded body so far.
    buf: Vec<u8>,
    /// How many parts have been opened (0 = the next one omits the leading CRLF).
    parts: usize,
}

impl MultipartForm {
    /// A form with a fresh boundary: 30 random bytes as 60 lowercase hex chars (Go
    /// `randomBoundary`, GOROOT `mime/multipart/writer.go`).
    pub(crate) fn new() -> Self {
        Self::with_boundary(&random_boundary())
    }

    /// A form with an explicit boundary (tests, and the byte golden).
    pub(crate) fn with_boundary(b: &str) -> Self {
        Self {
            boundary: b.to_owned(),
            buf: Vec::new(),
            parts: 0,
        }
    }

    /// Writes one part's delimiter, its headers in SORTED key order and the blank line that
    /// ends them (GOROOT `Writer::CreatePart`).
    fn open_part(&mut self, headers: &mut [(&'static str, String)]) {
        if self.parts == 0 {
            self.buf.extend_from_slice(b"--");
        } else {
            self.buf.extend_from_slice(b"\r\n--");
        }
        self.buf.extend_from_slice(self.boundary.as_bytes());
        self.buf.extend_from_slice(b"\r\n");
        self.parts += 1;
        // `slices.Sorted(maps.Keys(header))`: `Content-Disposition` precedes `Content-Type`.
        headers.sort_unstable_by(|a, b| a.0.cmp(b.0));
        for (k, v) in headers {
            self.buf.extend_from_slice(k.as_bytes());
            self.buf.extend_from_slice(b": ");
            self.buf.extend_from_slice(v.as_bytes());
            self.buf.extend_from_slice(b"\r\n");
        }
        self.buf.extend_from_slice(b"\r\n");
    }

    /// `WriteField` → `CreateFormField`: ONE header
    /// `Content-Disposition: form-data; name="<escape_std(name)>"`.
    pub(crate) fn field(&mut self, name: &str, value: &str) {
        self.open_part(&mut [(
            "Content-Disposition",
            format!("form-data; name=\"{}\"", escape_std(name)),
        )]);
        self.buf.extend_from_slice(value.as_bytes());
    }

    /// A file part (images.go:200-204 `CreatePart`): `Content-Disposition: form-data;
    /// name="<field VERBATIM>"; filename="<escape_quotes(filename)>"` then `Content-Type: <mime>`
    /// (ALWAYS written, `Content-Type: ` when `mime` is empty — Go's `h.Set` stores the empty
    /// value and the writer prints it).
    pub(crate) fn file(&mut self, field: &str, filename: &str, mime: &str, data: &[u8]) {
        self.open_part(&mut [
            (
                "Content-Disposition",
                format!(
                    "form-data; name=\"{field}\"; filename=\"{}\"",
                    escape_quotes(filename)
                ),
            ),
            ("Content-Type", mime.to_owned()),
        ]);
        self.buf.extend_from_slice(data);
    }

    /// Closes the form: `("multipart/form-data; boundary=<b>", body)` — the header value Go's
    /// `FormDataContentType` renders (the boundary is quoted only when it carries a tspecial or
    /// a space, which a hex boundary never does).
    pub(crate) fn finish(mut self) -> (String, Bytes) {
        self.buf.extend_from_slice(b"\r\n--");
        self.buf.extend_from_slice(self.boundary.as_bytes());
        self.buf.extend_from_slice(b"--\r\n");
        let b = if self.boundary.contains([
            '(', ')', '<', '>', '@', ',', ';', ':', '\\', '"', '/', '[', ']', '?', '=', ' ',
        ]) {
            format!("\"{}\"", self.boundary)
        } else {
            self.boundary.clone()
        };
        (
            format!("multipart/form-data; boundary={b}"),
            Bytes::from(self.buf),
        )
    }
}

/// 30 random bytes rendered as 60 lowercase hex chars (Go `randomBoundary`).
fn random_boundary() -> String {
    use rand::RngCore as _;
    let mut buf = [0u8; 30];
    rand::rng().fill_bytes(&mut buf);
    let mut s = String::with_capacity(60);
    for b in buf {
        // `{b:02x}` without the formatting machinery: two lowercase nibbles.
        s.push(char::from_digit(u32::from(b >> 4), 16).unwrap_or('0'));
        s.push(char::from_digit(u32::from(b & 0x0f), 16).unwrap_or('0'));
    }
    s
}

/// images.go:170 `quoteEscaper` (filenames): `\` → `\\`, `"` → `\"`.
///
/// Deliberately NOT the stdlib rule: images.go builds the file part's `Content-Disposition`
/// itself with its own two-entry replacer, so a CR or LF in a filename would pass through where
/// `WriteField` would percent-encode it.
pub(crate) fn escape_quotes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(c),
        }
    }
    out
}

/// GOROOT `mime/multipart/writer.go` `escapeQuotes` (`WriteField` names): `\` → `\\`, `"` → `\"`,
/// CR → `%0D`, LF → `%0A`.
pub(crate) fn escape_std(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\r' => out.push_str("%0D"),
            '\n' => out.push_str("%0A"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{MultipartForm, escape_quotes, escape_std};

    /// The body as a lossy string (every fixture below is ASCII).
    fn body(form: MultipartForm) -> (String, String) {
        let (ct, b) = form.finish();
        (ct, String::from_utf8_lossy(&b).into_owned())
    }

    /// The byte layout, against a reference a real `mime/multipart.Writer` produced: a scratch
    /// Go program (outside the Go tree) driving the writer exactly as
    /// `internal/llm/images.go:180-208` drives it, with `SetBoundary("BOUND")` and the bytes
    /// dumped through `strconv.Quote`. Every string below is that dump.
    #[test]
    fn multipart_layout_matches_the_go_writer() {
        let mut f = MultipartForm::with_boundary("BOUND");
        f.field("model", "gpt-image-1");
        f.field("prompt", "add a robot");
        f.field("n", "1");
        f.file("image", "image-1.png", "image/png", &[7]);
        let (ct, got) = body(f);
        assert_eq!(ct, "multipart/form-data; boundary=BOUND");
        assert_eq!(
            got,
            concat!(
                "--BOUND\r\n",
                "Content-Disposition: form-data; name=\"model\"\r\n",
                "\r\n",
                "gpt-image-1",
                "\r\n--BOUND\r\n",
                "Content-Disposition: form-data; name=\"prompt\"\r\n",
                "\r\n",
                "add a robot",
                "\r\n--BOUND\r\n",
                "Content-Disposition: form-data; name=\"n\"\r\n",
                "\r\n",
                "1",
                "\r\n--BOUND\r\n",
                "Content-Disposition: form-data; name=\"image\"; filename=\"image-1.png\"\r\n",
                "Content-Type: image/png\r\n",
                "\r\n",
                "\u{7}",
                "\r\n--BOUND--\r\n",
            )
        );
    }

    /// `Content-Disposition` precedes `Content-Type` because Go writes the header map in sorted
    /// key order, and the type header is emitted even when the mime is empty.
    #[test]
    fn file_part_headers_are_sorted_and_the_type_is_unconditional() {
        let mut f = MultipartForm::with_boundary("B");
        f.file("image[]", "a.bin", "", &[]);
        let (_, got) = body(f);
        assert_eq!(
            got,
            concat!(
                "--B\r\n",
                "Content-Disposition: form-data; name=\"image[]\"; filename=\"a.bin\"\r\n",
                "Content-Type: \r\n",
                "\r\n",
                "\r\n--B--\r\n",
            )
        );
        let cd = got.find("Content-Disposition").expect("disposition");
        let ctp = got.find("Content-Type").expect("type");
        assert!(cd < ctp, "sorted header order");
    }

    /// The three escaping rules are DIFFERENT and each is applied where Go applies it: the
    /// stdlib rule on a text field's name, nothing at all on a file part's field name, and
    /// images.go's two-entry replacer on the filename.
    #[test]
    fn the_three_escaping_rules() {
        assert_eq!(escape_std("a\"b\\c\rd\ne"), "a\\\"b\\\\c%0Dd%0Ae");
        assert_eq!(escape_quotes("a\"b\\c\rd"), "a\\\"b\\\\c\rd");

        // The full form, byte for byte, from the same Go dump.
        let mut f = MultipartForm::with_boundary("B");
        f.field("we\"ird\r\n", "v");
        f.file("image[]", "q\"u\\ote.png", "", b"x");
        let (_, got) = body(f);
        assert_eq!(
            got,
            concat!(
                "--B\r\n",
                "Content-Disposition: form-data; name=\"we\\\"ird%0D%0A\"\r\n",
                "\r\n",
                "v",
                "\r\n--B\r\n",
                "Content-Disposition: form-data; name=\"image[]\"; filename=\"q\\\"u\\\\ote.png\"\r\n",
                "Content-Type: \r\n",
                "\r\n",
                "x",
                "\r\n--B--\r\n",
            )
        );
    }

    /// An empty form still writes the closing delimiter (Go's `Close` with no `lastpart`).
    #[test]
    fn empty_form_closes() {
        let (_, got) = body(MultipartForm::with_boundary("B"));
        assert_eq!(got, "\r\n--B--\r\n");
    }

    /// A fresh boundary is 60 lowercase hex characters, and two forms never share one.
    #[test]
    fn fresh_boundaries_are_sixty_hex_chars() {
        let (ct, _) = MultipartForm::new().finish();
        let b = ct
            .strip_prefix("multipart/form-data; boundary=")
            .expect("prefix");
        assert_eq!(b.len(), 60, "{b}");
        assert!(
            b.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
            "{b}"
        );
        let (other, _) = MultipartForm::new().finish();
        assert_ne!(ct, other);
    }
}
