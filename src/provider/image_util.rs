//! Image helpers shared by the image providers (provider/imagen.go:117-124,226-235, images.go:169-229).

use crate::BoxError;
use crate::provider::model::{Attachment, Message, Role};
use futures::StreamExt;
use reqwest::header::CONTENT_TYPE;
use tokio_util::sync::CancellationToken;

use crate::llm::client::Client;
use crate::llm::error::LlmError;

/// Result images larger than this are rejected (`image exceeds 64 MB`).
pub(crate) const MAX_IMAGE_FETCH: usize = 64 << 20;

/// Last user message: (content, image/* attachments). No user message → ("", []).
///
/// The whole input of a stateless image-provider turn: earlier history is record, not input — editing
/// re-attaches explicitly (provider/imagen.go:117-124).
pub(crate) fn last_user_turn(messages: &[Message]) -> (String, Vec<&Attachment>) {
    for msg in messages.iter().rev() {
        if msg.role() == Role::User {
            return (
                msg.content.clone(),
                msg.attachments.iter().filter(|a| a.is_image()).collect(),
            );
        }
    }
    (String::new(), Vec::new())
}

/// images.go:169-182: declared (params stripped) if it starts with "image/"; else sniff; else "image/png".
pub fn image_mime(declared: &str, data: &[u8]) -> String {
    if !declared.is_empty() {
        // Go keeps the raw string when `mime.ParseMediaType` fails, so an unparseable declaration that still
        // begins with `image/` is returned verbatim.
        let settled = media_type_essence(declared).unwrap_or_else(|| declared.to_owned());
        if settled.starts_with("image/") {
            return settled;
        }
    }
    if let Some(sniffed) = sniff_image(data) {
        return sniffed.to_owned();
    }
    "image/png".to_owned()
}

/// `http.DetectContentType` subset: PNG, JPEG, GIF, WebP, BMP, ICO → Some("image/…"), else None.
///
/// The signatures and their order are net/http/internal/sniff.go's image block; every other sniffed type
/// (text, audio, video, archives) folds to `None` because `image_mime` only ever keeps an `image/` answer.
pub(crate) fn sniff_image(data: &[u8]) -> Option<&'static str> {
    /// Exact prefixes checked before the masked WebP signature.
    const BEFORE_WEBP: [(&[u8], &str); 5] = [
        (b"\x00\x00\x01\x00", "image/x-icon"),
        (b"\x00\x00\x02\x00", "image/x-icon"),
        (b"BM", "image/bmp"),
        (b"GIF87a", "image/gif"),
        (b"GIF89a", "image/gif"),
    ];
    /// Exact prefixes checked after it.
    const AFTER_WEBP: [(&[u8], &str); 2] = [
        (b"\x89PNG\r\n\x1a\n", "image/png"),
        (b"\xff\xd8\xff", "image/jpeg"),
    ];

    for (sig, ct) in BEFORE_WEBP {
        if data.starts_with(sig) {
            return Some(ct);
        }
    }
    // maskedSig{mask: FFFFFFFF00000000FFFFFFFFFFFF, pat: "RIFF\0\0\0\0WEBPVP"}.
    if data.len() >= 14 && &data[..4] == b"RIFF" && &data[8..14] == b"WEBPVP" {
        return Some("image/webp");
    }
    for (sig, ct) in AFTER_WEBP {
        if data.starts_with(sig) {
            return Some(ct);
        }
    }
    None
}

/// image/jpeg → ".jpg", image/webp → ".webp", else ".png".
pub(crate) fn ext_for_mime(mime: &str) -> &'static str {
    match mime {
        "image/jpeg" => ".jpg",
        "image/webp" => ".webp",
        _ => ".png",
    }
}

/// GET without auth; scheme must be http/https (`unsupported image url {url:?}`); status != 200 → `image url
/// returned {code}`; > 64 MiB → `image exceeds 64 MB`; mime = Content-Type essence, "image/png" unless it starts
/// with "image/".
///
/// The `execute` (response head) is wrapped in `tokio::time::timeout(HEADER_TIMEOUT, ..)` — POLICY I-02 applies
/// to EVERY provider HTTP call, this relay fetch included; expiry is a plain error text `response headers not
/// received within 2m0s`. The body read is never time-limited.
pub(crate) async fn fetch_image(
    cancel: &CancellationToken,
    client: &Client,
    url: &str,
) -> Result<(Vec<u8>, String), BoxError> {
    // The URL comes from the server; only http(s) is followed and the signature IS the credential, so no
    // static header of the provider client rides along (images.go:192-207) — `fetch_bare` sends none,
    // and records the GET into the run's `/debug` ring like any other request (its row's action is
    // the URL's last path segment, as Go's relay GET through the recording client).
    let target = match reqwest::Url::parse(url) {
        Ok(u) if u.scheme() == "http" || u.scheme() == "https" => u,
        _ => return Err(format!("unsupported image url {url:?}").into()),
    };
    let resp = client
        .fetch_bare(cancel, target.as_str())
        .await
        .map_err(|e| match e {
            LlmError::Transport(t) => Box::new(t) as BoxError,
            other => Box::new(other) as BoxError,
        })?;
    if resp.status() != reqwest::StatusCode::OK {
        return Err(format!("image url returned {}", resp.status().as_u16()).into());
    }
    // Strip content-type parameters ("image/jpeg; charset=binary"): the bare type feeds the extension maps.
    let declared = resp
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();

    // Bounded read: a broken relay cannot stream unbounded bytes into memory (Go reads 64 MiB + 1).
    let mut data: Vec<u8> = Vec::new();
    let mut body = resp.bytes_stream();
    while data.len() <= MAX_IMAGE_FETCH {
        let chunk = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(Box::new(LlmError::Cancelled)),
            c = body.next() => c,
        };
        match chunk {
            Some(Ok(bytes)) => data.extend_from_slice(&bytes),
            Some(Err(e)) => return Err(Box::new(e)),
            None => break,
        }
    }
    if data.len() > MAX_IMAGE_FETCH {
        return Err(format!("image exceeds {} MB", MAX_IMAGE_FETCH >> 20).into());
    }

    let mime = media_type_essence(&declared).unwrap_or(declared);
    let mime = if mime.starts_with("image/") {
        mime
    } else {
        "image/png".to_owned()
    };
    Ok((data, mime))
}

/// Go `mime.ParseMediaType`'s first return value: the lower-cased `type/subtype` before the first `;`.
/// `None` reproduces Go's error return (the caller then keeps the raw declaration).
fn media_type_essence(s: &str) -> Option<String> {
    let base = s.split(';').next().unwrap_or(s).trim();
    let (kind, sub) = base.split_once('/')?;
    if kind.is_empty()
        || sub.is_empty()
        || !kind.bytes().all(is_token_byte)
        || !sub.bytes().all(is_token_byte)
    {
        return None;
    }
    Some(base.to_ascii_lowercase())
}

/// RFC 2045 token byte: printable ASCII minus the tspecials and space.
fn is_token_byte(b: u8) -> bool {
    b.is_ascii_graphic()
        && !matches!(
            b,
            b'(' | b')'
                | b'<'
                | b'>'
                | b'@'
                | b','
                | b';'
                | b':'
                | b'\\'
                | b'"'
                | b'/'
                | b'['
                | b']'
                | b'?'
                | b'='
        )
}

#[cfg(test)]
mod tests {
    use super::{ext_for_mime, last_user_turn, media_type_essence, sniff_image};
    use crate::provider::model::{AssistantBody, Attachment, Body, Message};

    fn att(name: &str, mime: &str) -> Attachment {
        Attachment {
            filename: name.to_owned(),
            mime_type: mime.to_owned(),
            data: vec![1],
        }
    }

    #[test]
    fn last_user_turn_reads_only_the_trailing_user_message() {
        let messages = vec![
            Message {
                content: "a cat".to_owned(),
                attachments: vec![att("old.png", "image/png")],
                ..Message::default()
            },
            Message {
                attachments: vec![att("image-1.png", "image/png")],
                body: Body::Assistant(AssistantBody {
                    ..AssistantBody::default()
                }),
                ..Message::default()
            },
            Message {
                content: "add a robot".to_owned(),
                attachments: vec![att("ref.png", "image/png"), att("notes.txt", "text/plain")],
                ..Message::default()
            },
        ];
        let (prompt, refs) = last_user_turn(&messages);
        assert_eq!(prompt, "add a robot");
        assert_eq!(refs.len(), 1, "non-image attachments are dropped");
        assert_eq!(refs[0].filename, "ref.png");

        // No user message at all: the empty turn.
        let none = [Message::assistant("no user turn")];
        let (prompt, refs) = last_user_turn(&none);
        assert_eq!(prompt, "");
        assert!(refs.is_empty());
        assert_eq!(last_user_turn(&[]).0, "");
    }

    #[test]
    fn sniff_image_covers_the_go_signature_table() {
        assert_eq!(sniff_image(b"\x89PNG\r\n\x1a\n\0\0"), Some("image/png"));
        assert_eq!(sniff_image(b"\xff\xd8\xff\xe0"), Some("image/jpeg"));
        assert_eq!(sniff_image(b"GIF87a...."), Some("image/gif"));
        assert_eq!(sniff_image(b"GIF89a...."), Some("image/gif"));
        assert_eq!(
            sniff_image(b"RIFF\x01\x02\x03\x04WEBPVP8 "),
            Some("image/webp")
        );
        assert_eq!(sniff_image(b"RIFF\x01\x02\x03\x04WAVEfmt "), None);
        assert_eq!(sniff_image(b"BM\0\0"), Some("image/bmp"));
        assert_eq!(sniff_image(b"\x00\x00\x01\x00"), Some("image/x-icon"));
        assert_eq!(sniff_image(b"\x00\x00\x02\x00"), Some("image/x-icon"));
        assert_eq!(sniff_image(b"\x00\x01\x02\x03"), None);
        assert_eq!(sniff_image(b""), None);
    }

    #[test]
    fn ext_for_mime_maps_three_cases() {
        assert_eq!(ext_for_mime("image/jpeg"), ".jpg");
        assert_eq!(ext_for_mime("image/webp"), ".webp");
        assert_eq!(ext_for_mime("image/png"), ".png");
        // gif and anything unknown share the png extension (imagen.go:226-235).
        assert_eq!(ext_for_mime("image/gif"), ".png");
        assert_eq!(ext_for_mime(""), ".png");
    }

    #[test]
    fn media_type_essence_matches_parse_media_type() {
        assert_eq!(
            media_type_essence("image/jpeg; charset=binary").as_deref(),
            Some("image/jpeg")
        );
        assert_eq!(
            media_type_essence("IMAGE/JPEG").as_deref(),
            Some("image/jpeg")
        );
        assert_eq!(media_type_essence("image/jpeg extra"), None);
        assert_eq!(media_type_essence("not a type"), None);
        assert_eq!(media_type_essence(""), None);
    }
}
