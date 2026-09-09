//! Wire-name tests (mcp/manager_test.go:17-164): pure string tests over `iota::mcp::naming`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use iota::mcp::naming::{WIRE_NAME_MAX_LEN, sanitize_name_segment, wire_tool_name};
use pretty_assertions::assert_eq;

/// The strictest tool-name charset among the supported providers (Gemini functionDeclaration names): no hyphens,
/// letter or underscore first (`manager_test.go:86`, `geminiNamePattern`).
fn gemini_name_ok(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

// Go: mcp/manager_test.go:17
#[test]
fn test_sanitize_name_segment() {
    let tests = [
        ("github", "github"),
        ("chrome-devtools", "chrome_devtools"),
        ("my_server_2", "my_server_2"),
        // Runs of "_" collapse and the ends are trimmed: a segment can never contain "__", so the first "__"
        // after "mcp__" always separates server from tool.
        ("https://mcp.example.com/sse", "https_mcp_example_com_sse"),
        ("npx -y server", "npx_y_server"),
        ("a__b", "a_b"),
        ("__tool__", "tool"),
        ("-x-", "x"),
        // Names that sanitize to nothing fall back to "srv".
        ("团队", "srv"),
        ("_", "srv"),
        ("", "srv"),
        // Non-ASCII scalars count as one replacement each and collapse with neighbours.
        ("团队-x", "x"),
        ("a团队b", "a_b"),
    ];
    for (input, want) in tests {
        assert_eq!(
            sanitize_name_segment(input),
            want,
            "sanitize_name_segment({input:?})"
        );
    }
}

// Go: mcp/manager_test.go:44
#[test]
fn test_wire_tool_name() {
    let tests = [
        ("simple", "github", "get_me", "mcp__github__get_me"),
        (
            "server sanitized",
            "chrome-devtools",
            "take_screenshot",
            "mcp__chrome_devtools__take_screenshot",
        ),
        (
            "url server collapses to single underscores",
            "https://mcp.example.com/sse",
            "search",
            "mcp__https_mcp_example_com_sse__search",
        ),
    ];
    for (name, server, tool, want) in tests {
        assert_eq!(wire_tool_name(server, tool), want, "{name}");
    }

    // Exactly at the limit: no truncation. "mcp__srv__" is 10 chars, so a 54-char tool name lands exactly on
    // WIRE_NAME_MAX_LEN.
    let exact = wire_tool_name("srv", &"a".repeat(54));
    assert_eq!(
        exact,
        format!("mcp__srv__{}", "a".repeat(54)),
        "64-char name should be untouched"
    );
    assert_eq!(exact.len(), WIRE_NAME_MAX_LEN);

    // One char over the limit: truncated back to exactly WIRE_NAME_MAX_LEN and still distinct from the name that
    // landed exactly on it.
    let over = wire_tool_name("srv", &"a".repeat(55));
    assert_eq!(
        over.len(),
        WIRE_NAME_MAX_LEN,
        "65-char name not capped: {over:?}"
    );
    assert_ne!(over, exact, "on-limit and over-limit names collided");
    for n in [&exact, &over] {
        assert!(
            gemini_name_ok(n),
            "wire name {n:?} violates the Gemini charset"
        );
    }
}

// Go: mcp/manager_test.go:92
#[test]
fn test_wire_tool_name_sanitized_tool() {
    let hyphen = wire_tool_name("github", "add-issue-comment");
    let under = wire_tool_name("github", "add_issue_comment");

    assert!(
        hyphen.starts_with("mcp__github__add_issue_comment_"),
        "hyphenated tool not sanitized+suffixed: {hyphen:?}"
    );
    assert_eq!(
        under, "mcp__github__add_issue_comment",
        "clean tool should compose without a suffix"
    );
    assert_ne!(hyphen, under, "raw names sanitizing alike collided");
    for n in [&hyphen, &under] {
        assert!(
            n.len() <= WIRE_NAME_MAX_LEN,
            "wire name over {WIRE_NAME_MAX_LEN} chars: {n:?}"
        );
        assert!(
            gemini_name_ok(n),
            "wire name {n:?} violates the Gemini charset"
        );
    }
    // The suffix is "_" + 8 lowercase hex chars of sha256("mcp__github__add-issue-comment")[..4].
    let tail = &hyphen[hyphen.len() - 9..];
    assert!(
        tail.starts_with('_')
            && tail[1..]
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    );

    // Lossy AND over the limit at once: still capped, still charset-clean.
    let long = wire_tool_name("srv", &"a-".repeat(40));
    assert_eq!(
        long.len(),
        WIRE_NAME_MAX_LEN,
        "long lossy name not capped: {long:?}"
    );
    assert!(gemini_name_ok(&long), "long lossy name not clean: {long:?}");
}

// Go: mcp/manager_test.go:126
#[test]
fn test_wire_tool_name_delimiter_ambiguity() {
    let split_tool = wire_tool_name("a", "b__c");
    let split_server = wire_tool_name("a__b", "c");

    assert_eq!(split_server, "mcp__a_b__c");
    assert!(
        split_tool.starts_with("mcp__a__b_c_"),
        "wire_tool_name(\"a\", \"b__c\") = {split_tool:?}, want \"mcp__a__b_c_<hash>\""
    );
    assert_ne!(
        split_tool, split_server,
        "delimiter-ambiguous compositions collided"
    );
}

// Go: mcp/manager_test.go:141
#[test]
fn test_wire_tool_name_truncation() {
    // Two long tool names that differ only past the truncation cut must yield distinct wire names of exactly
    // WIRE_NAME_MAX_LEN chars.
    let base = "a".repeat(80);
    let n1 = wire_tool_name("srv", &format!("{base}1"));
    let n2 = wire_tool_name("srv", &format!("{base}2"));

    for n in [&n1, &n2] {
        assert_eq!(n.len(), WIRE_NAME_MAX_LEN, "truncated name length: {n:?}");
        assert!(
            n.starts_with("mcp__srv__aaa"),
            "truncated name lost its prefix: {n:?}"
        );
        // Tail is "_" + 8 lowercase hex chars.
        let tail = &n[n.len() - 9..];
        assert!(
            tail.starts_with('_')
                && tail[1..]
                    .chars()
                    .all(|c| matches!(c, '0'..='9' | 'a'..='f')),
            "truncated name tail {tail:?} is not _hhhhhhhh"
        );
    }
    assert_ne!(n1, n2, "names differing past the cut collided");
}
