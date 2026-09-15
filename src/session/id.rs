//! Session ids and fragment resolution (chat/session.go:211-280).

use rand::RngCore;

use crate::session::error::SessionError;
use crate::session::store::SessionInfo;

/// Crockford base32, lowercased: no `i`/`l`/`o`/`u`, so ids stay unambiguous to read and retype
/// (chat/session.go:213).
pub const SESSION_ID_ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

/// 12 characters ≈ 60 random bits (chat/session.go:218): short enough to type, long enough that local
/// collisions are vanishingly unlikely — and creation double-checks against the store anyway.
pub const SESSION_ID_LENGTH: usize = 12;

/// A fresh random id: [`SESSION_ID_LENGTH`] symbols drawn from [`SESSION_ID_ALPHABET`] by masking random
/// bytes with `0x1f`. The alphabet has 32 symbols and 32 divides 256, so the mask is exactly uniform —
/// the distribution `gonanoid.MustGenerate` produces, without a nanoid dependency.
pub(crate) fn generate_id() -> String {
    let mut buf = [0u8; SESSION_ID_LENGTH];
    rand::rng().fill_bytes(&mut buf);
    buf.iter()
        .map(|b| char::from(SESSION_ID_ALPHABET[usize::from(b & 0x1f)]))
        .collect()
}

/// `resolveSessionID` (chat/session.go:262-280): an EXACT (case-SENSITIVE) match wins immediately;
/// otherwise case-insensitive PREFIX matches are collected — none is
/// [`NoMatch`](SessionError::NoMatch), one resolves, several are
/// [`Ambiguous`](SessionError::Ambiguous) with the candidates joined by `", "` in listing order.
pub fn resolve_in(infos: &[SessionInfo], fragment: &str) -> Result<String, SessionError> {
    let lower = fragment.to_lowercase();
    let mut matches: Vec<&str> = Vec::new();
    for info in infos {
        if info.id == fragment {
            return Ok(info.id.clone());
        }
        if info.id.to_lowercase().starts_with(&lower) {
            matches.push(&info.id);
        }
    }
    match matches.as_slice() {
        [] => Err(SessionError::NoMatch(fragment.to_owned())),
        [only] => Ok((*only).to_owned()),
        several => Err(SessionError::Ambiguous(
            fragment.to_owned(),
            several.join(", "),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{SESSION_ID_ALPHABET, SESSION_ID_LENGTH, generate_id};

    /// Every generated id has the right length and only alphabet symbols; a small batch never repeats.
    #[test]
    fn generated_ids_stay_in_the_alphabet() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..200 {
            let id = generate_id();
            assert_eq!(id.len(), SESSION_ID_LENGTH, "id {id:?}");
            assert!(
                id.bytes().all(|b| SESSION_ID_ALPHABET.contains(&b)),
                "id {id:?} left the alphabet"
            );
            assert!(
                seen.insert(id.clone()),
                "duplicate id {id:?} in a small batch"
            );
        }
    }
}
