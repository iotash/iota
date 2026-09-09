//! Wire-name composition for MCP tools (mcp/manager.go:82-155): `mcp__<segment>__<tool>` with sanitisation, a 64-byte
//! cap and a sha256 disambiguation suffix. Pure functions.

use crate::text::truncate_to_char_boundary;
use sha2::{Digest, Sha256};

/// Maximum length of a wire tool name.
pub const WIRE_NAME_MAX_LEN: usize = 64;

/// Prefix of every MCP wire tool name.
pub(crate) const WIRE_NAME_PREFIX: &str = "mcp__";

/// Segment used when sanitisation leaves nothing.
pub(crate) const EMPTY_SEGMENT: &str = "srv";

/// Length of the disambiguation suffix: `'_'` plus 8 lowercase hex digits (4 bytes of sha256).
const SUFFIX_LEN: usize = 9;

/// Lowercase hex alphabet of the disambiguation suffix (Go `hex.EncodeToString`).
const HEX: [u8; 16] = *b"0123456789abcdef";

/// `[A-Za-z0-9]` kept; any other char (one per Unicode scalar) → `'_'` with runs collapsed; trim `'_'`; empty →
/// `"srv"`.
pub fn sanitize_name_segment(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_underscore = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            prev_underscore = false;
            out.push(c);
        } else {
            if prev_underscore {
                continue;
            }
            prev_underscore = true;
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        EMPTY_SEGMENT.to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// `base = "mcp__" + segment + "__"`; `wire = base + sanitize(tool)`; if `wire == base + tool && len <= 64` → `wire`;
/// else `suffix = "_" + lowercase hex of sha256(base + RAW tool)[..4]`; if `len(wire) + 9 > 64` → `wire[..55]`;
/// `wire + suffix`.
pub(crate) fn compose_wire_name(segment: &str, tool: &str) -> String {
    let base = format!("{WIRE_NAME_PREFIX}{segment}__");
    let raw = format!("{base}{tool}");
    let mut wire = format!("{base}{}", sanitize_name_segment(tool));
    if wire == raw && wire.len() <= WIRE_NAME_MAX_LEN {
        return wire;
    }
    let digest = Sha256::digest(raw.as_bytes());
    let mut suffix = String::with_capacity(SUFFIX_LEN);
    suffix.push('_');
    for byte in &digest[..4] {
        suffix.push(char::from(HEX[usize::from(byte >> 4)]));
        suffix.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    if wire.len() + suffix.len() > WIRE_NAME_MAX_LEN {
        // Go slices bytes; the sanitised wire is ASCII for every sanitised segment, so the boundary-safe cut is the
        // same cut — it only differs (and avoids a panic) for an unsanitised non-ASCII segment.
        let cut = truncate_to_char_boundary(&wire, WIRE_NAME_MAX_LEN - suffix.len()).len();
        wire.truncate(cut);
    }
    wire.push_str(&suffix);
    wire
}

/// `compose_wire_name(sanitize_name_segment(server), tool)`.
pub fn wire_tool_name(server: &str, tool: &str) -> String {
    compose_wire_name(&sanitize_name_segment(server), tool)
}
