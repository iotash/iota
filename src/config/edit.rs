//! The machine-managed `mcp_servers:` block (brain page `mcp-cli-and-oauth`): `iota mcp add`/`remove` rewrite
//! ONE top-level section of a config file and leave every other byte as the user wrote it.
//!
//! A config file is hand-edited state — comments, blank lines, the order of the layers — so a command that
//! re-serialised the whole document would destroy what it did not understand. Instead the block is LOCATED by
//! line: a top-level key is a line that starts at column 0 with `key:`, and its block runs to the line before
//! the next column-0 content. Only that byte range is replaced, by the entries serialised afresh; when the
//! document has no such block, one is appended after a blank line. What is NOT kept is what sits inside the
//! block — a comment between two servers is dropped on the next rewrite, which the README says.
//!
//! The scan is exact for what iota accepts: every top-level key of a config is at column 0, and every line
//! of a block (a nested value, a block scalar, a flow collection's continuation, a quoted scalar's
//! continuation) is indented under it. A parser with source spans (`marked-yaml`) would have brought a
//! second YAML implementation into the binary for the one offset this needs.

use std::{collections::BTreeMap, ops::Range, path::Path};

use crate::config::{ConfigError, McpServerConfig};

/// The top-level key this module manages.
pub const MCP_SERVERS_KEY: &str = "mcp_servers";

/// Why a rewrite could not happen: the file could not be read or written, or what it holds is not a config
/// iota accepts (a block is never written into a document the next load would refuse).
#[derive(Debug, thiserror::Error)]
pub enum EditError {
    /// The file exists but could not be read.
    #[error("config {path}: {source}")]
    Read {
        /// The file as it was named.
        path: String,
        /// The read failure.
        #[source]
        source: std::io::Error,
    },
    /// The document is not one iota reads: a YAML error, or a key of the wrong layer.
    #[error("config {path}: {source}")]
    Config {
        /// The file as it was named.
        path: String,
        /// What is wrong inside it.
        #[source]
        source: ConfigError,
    },
    /// The atomic write failed.
    #[error("config {path}: {source}")]
    Write {
        /// The file as it was named.
        path: String,
        /// The write failure.
        #[source]
        source: std::io::Error,
    },
    /// The block could not be serialised (a value serde cannot represent — not reachable from the CLI).
    #[error("mcp_servers: {0}")]
    Serialize(#[from] serde_norway::Error),
}

/// One config file as read for a rewrite: its bytes as found and its own `mcp_servers:` entries.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FileServers {
    /// The document, verbatim (`""` when the file does not exist yet).
    pub text: String,
    /// The `mcp_servers:` entries this ONE file declares.
    pub servers: BTreeMap<String, McpServerConfig>,
}

/// Reads `path` for a rewrite. A missing file is an empty document (the write will create it); an unreadable
/// one, or one iota would refuse to load, is an error — nothing is written into a file the next run could not
/// read back.
pub fn read_mcp_servers(path: &Path) -> Result<FileServers, EditError> {
    let display = path.display().to_string();
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(FileServers::default()),
        Err(source) => {
            return Err(EditError::Read {
                path: display,
                source,
            });
        }
    };
    let servers =
        crate::config::decode_mcp_servers(&bytes).map_err(|source| EditError::Config {
            path: display.clone(),
            source,
        })?;
    // The bytes are kept as a `String` so the block can be located by line; a document that decoded as YAML
    // is UTF-8 already.
    let text = String::from_utf8(bytes).map_err(|e| EditError::Config {
        path: display,
        source: ConfigError::Parse(e.to_string()),
    })?;
    Ok(FileServers { text, servers })
}

/// Writes `servers` as the file's `mcp_servers:` block and the result to `path` atomically, keeping every byte
/// outside the block (see [`rewrite_mcp_servers`]).
pub fn write_mcp_servers(
    path: &Path,
    text: &str,
    servers: &BTreeMap<String, McpServerConfig>,
) -> Result<(), EditError> {
    let out = rewrite_mcp_servers(text, servers)?;
    crate::app::fs::write_atomic(path, out.as_bytes(), None).map_err(|source| EditError::Write {
        path: path.display().to_string(),
        source,
    })
}

/// `text` with its top-level `mcp_servers:` block replaced by `servers` — appended after one blank line when
/// the document has none, and REMOVED when `servers` is empty (a `mcp_servers: {}` line would say nothing).
/// Every byte outside the block is returned as it came in.
pub fn rewrite_mcp_servers(
    text: &str,
    servers: &BTreeMap<String, McpServerConfig>,
) -> Result<String, EditError> {
    let block = if servers.is_empty() {
        String::new()
    } else {
        #[derive(serde::Serialize)]
        struct Block<'a> {
            mcp_servers: &'a BTreeMap<String, McpServerConfig>,
        }
        serde_norway::to_string(&Block {
            mcp_servers: servers,
        })?
    };
    Ok(match locate_block(text, MCP_SERVERS_KEY) {
        Some(range) => {
            let mut out = String::with_capacity(text.len() + block.len());
            out.push_str(&text[..range.start]);
            out.push_str(&block);
            out.push_str(&text[range.end..]);
            if block.is_empty() && range.end == text.len() {
                // The block was the last thing in the file: the blank line that separated it from what came
                // before goes with it, so the file ends where its content does.
                let trimmed = out.trim_end_matches('\n').len();
                out.truncate(trimmed);
                if !out.is_empty() {
                    out.push('\n');
                }
            }
            out
        }
        None if block.is_empty() => text.to_owned(),
        None => {
            let mut out = text.to_owned();
            if !out.is_empty() {
                if !out.ends_with('\n') {
                    out.push('\n');
                }
                if !out.ends_with("\n\n") {
                    out.push('\n');
                }
            }
            out.push_str(&block);
            out
        }
    })
}

/// The byte range of the top-level block `key` in `text`: from the start of its key line to the end of its
/// last indented line (newline included), or `None` when no line at column 0 is `key:`.
///
/// Blank lines and column-0 comments AFTER the last indented line are not part of the block — a heading
/// comment above the next section belongs to that section. Blank lines and column-0 comments BETWEEN two
/// indented lines are inside it.
pub fn locate_block(text: &str, key: &str) -> Option<Range<usize>> {
    let mut lines = line_spans(text);
    let (start, mut end) = loop {
        let (span, line) = lines.next()?;
        if top_level_key(line) == Some(key) {
            break (span.start, span.end);
        }
    };
    for (span, line) in lines {
        if is_content_at_column_0(line) {
            break;
        }
        if is_indented_content(line) {
            end = span.end;
        }
    }
    Some(start..end)
}

/// Every line of `text` with its byte span (the newline included when there is one).
fn line_spans(text: &str) -> impl Iterator<Item = (Range<usize>, &str)> {
    let mut start = 0;
    std::iter::from_fn(move || {
        if start >= text.len() {
            return None;
        }
        let rest = &text[start..];
        let len = rest.find('\n').map_or(rest.len(), |i| i + 1);
        let span = start..start + len;
        start += len;
        Some((span, rest[..len].trim_end_matches(['\n', '\r'])))
    })
}

/// The key of a `key:` line at column 0: plain or quoted, followed by a space, a comment, or the end of the
/// line. Directives, document markers, sequence items and comments are never keys.
fn top_level_key(line: &str) -> Option<&str> {
    let first = line.chars().next()?;
    if first.is_whitespace() || matches!(first, '#' | '-' | '%' | '.' | '[' | '{') {
        return None;
    }
    let colon = line.find(':')?;
    let after = line[colon + 1..].chars().next();
    if !matches!(after, None | Some(' ' | '\t')) {
        return None;
    }
    let key = line[..colon].trim_end();
    Some(
        key.strip_prefix('"')
            .and_then(|k| k.strip_suffix('"'))
            .or_else(|| key.strip_prefix('\'').and_then(|k| k.strip_suffix('\'')))
            .unwrap_or(key),
    )
}

/// A line whose first byte is neither whitespace nor `#` — a top-level key, a document marker, a directive.
fn is_content_at_column_0(line: &str) -> bool {
    line.chars()
        .next()
        .is_some_and(|c| !c.is_whitespace() && c != '#')
}

/// A non-blank line that starts with whitespace: a nested value, or a comment inside the block.
fn is_indented_content(line: &str) -> bool {
    line.starts_with([' ', '\t']) && !line.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use pretty_assertions::assert_eq;

    use super::{locate_block, read_mcp_servers, rewrite_mcp_servers, write_mcp_servers};
    use crate::config::McpServerConfig;

    fn servers(entries: &[(&str, McpServerConfig)]) -> BTreeMap<String, McpServerConfig> {
        entries
            .iter()
            .map(|(n, c)| ((*n).to_owned(), c.clone()))
            .collect()
    }

    fn stdio(command: &str, args: &[&str]) -> McpServerConfig {
        McpServerConfig {
            command: command.to_owned(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            ..McpServerConfig::default()
        }
    }

    /// A document with everything a scan could trip on: a heading comment, odd indentation, a block scalar
    /// with a `key:`-shaped line, a flow mapping spanning lines, a column-0 comment INSIDE the block, and
    /// `providers:` AFTER `mcp_servers:`.
    const SAMPLE: &str = "\
# top comment

models:
  gpt:
      provider: openai   # deeper than usual
      id: gpt-5

# MCP tool servers
mcp_servers:
  fs:
    command: npx
    # an indented comment inside the block
    args: [\"-y\",
      \"server-fs\"]
# a column-0 comment inside the block
  old:
    url: https://old.example/mcp

# The endpoints, after the servers on purpose
providers:
  openai:
    key: k
agents:
  default:
    model: gpt
    system: |
      not_a_key: this line is a block scalar
";

    #[test]
    fn locate_block_finds_the_key_line_and_its_indented_tail() {
        let range = locate_block(SAMPLE, "mcp_servers").expect("the block is there");
        let block = &SAMPLE[range.clone()];
        assert!(block.starts_with("mcp_servers:\n"), "{block}");
        assert!(
            block.ends_with("    url: https://old.example/mcp\n"),
            "the block ends with its last indented line:\n{block}"
        );
        assert!(
            block.contains("# a column-0 comment inside the block"),
            "a column-0 comment between two entries is inside the block"
        );
        // The heading comment above and the blank line + heading below are NOT in the block.
        assert!(SAMPLE[..range.start].ends_with("# MCP tool servers\n"));
        assert!(SAMPLE[range.end..].starts_with("\n# The endpoints"));
        // Other keys, and a `key:`-shaped line inside a block scalar, are located by the same rule.
        assert_eq!(
            &SAMPLE[locate_block(SAMPLE, "providers").expect("providers")],
            "providers:\n  openai:\n    key: k\n"
        );
        assert!(locate_block(SAMPLE, "not_a_key").is_none());
        assert!(
            locate_block(SAMPLE, "gpt").is_none(),
            "only column-0 keys count"
        );
        // The last block of a file runs to the end of the file.
        let agents = locate_block(SAMPLE, "agents").expect("agents");
        assert_eq!(agents.end, SAMPLE.len());
        // Quoted keys, inline values and a bare key are all found; `---` is not a key.
        assert_eq!(
            locate_block("---\n\"mcp_servers\": {}\nx: 1\n", "mcp_servers"),
            Some(4..22)
        );
        assert_eq!(locate_block("mcp_servers:", "mcp_servers"), Some(0..12));
        assert_eq!(locate_block("mcp_servers:\n", "mcp_servers"), Some(0..13));
        assert!(locate_block("x: 1\n", "mcp_servers").is_none());
        assert!(locate_block("", "mcp_servers").is_none());
    }

    #[test]
    fn rewrite_keeps_every_byte_outside_the_block() {
        let range = locate_block(SAMPLE, "mcp_servers").expect("block");
        let new = servers(&[("fs", stdio("npx", &["-y", "server-fs", "${cwd}"]))]);
        let out = rewrite_mcp_servers(SAMPLE, &new).expect("rewrite");
        // Byte for byte: the prefix and the suffix are the sample's own.
        assert_eq!(&out[..range.start], &SAMPLE[..range.start]);
        assert!(out.ends_with(&SAMPLE[range.end..]));
        let written = &out[range.start..out.len() - (SAMPLE.len() - range.end)];
        assert_eq!(
            written,
            "mcp_servers:\n  fs:\n    command: npx\n    args:\n    - -y\n    - server-fs\n    - ${cwd}\n"
        );
        // …and what was written reads back as exactly those entries, in a document the loader accepts.
        let mut warnings = Vec::new();
        let cfg = crate::config::Config::parse(
            out.as_bytes(),
            &crate::app::env::Env::default(),
            &mut |w| {
                warnings.push(w);
            },
        )
        .expect("the rewritten document loads");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(cfg.mcp_servers, new);
        assert_eq!(cfg.models.len(), 1, "the other layers are untouched");
    }

    #[test]
    fn rewrite_covers_the_four_block_positions() {
        let one = servers(&[("s", stdio("srv", &[]))]);
        let block = "mcp_servers:\n  s:\n    command: srv\n";

        // Absent: appended after exactly one blank line, whether or not the file ends with a newline.
        assert_eq!(
            rewrite_mcp_servers("providers:\n  p: {key: k}\n", &one).expect("rewrite"),
            format!("providers:\n  p: {{key: k}}\n\n{block}")
        );
        assert_eq!(
            rewrite_mcp_servers("providers:\n  p: {key: k}", &one).expect("rewrite"),
            format!("providers:\n  p: {{key: k}}\n\n{block}")
        );
        assert_eq!(
            rewrite_mcp_servers("providers:\n  p: {key: k}\n\n\n", &one).expect("rewrite"),
            format!("providers:\n  p: {{key: k}}\n\n\n{block}"),
            "existing blank lines are not collapsed"
        );
        // Empty file: the block alone.
        assert_eq!(rewrite_mcp_servers("", &one).expect("rewrite"), block);
        // In the middle: replaced in place, neighbours untouched.
        let middle = "a: 1\nmcp_servers:\n  old: {command: x}\n\n# next\nb: 2\n";
        assert_eq!(
            rewrite_mcp_servers(middle, &one).expect("rewrite"),
            format!("a: 1\n{block}\n# next\nb: 2\n")
        );
        // At the end, with a stale inline value.
        assert_eq!(
            rewrite_mcp_servers("a: 1\nmcp_servers: {old: {command: x}}\n", &one).expect("rewrite"),
            format!("a: 1\n{block}")
        );
        // An empty map REMOVES the block: in the middle the neighbours close up; at the end the separating
        // blank line goes too; a document without the block is returned as it came.
        let none = BTreeMap::new();
        assert_eq!(
            rewrite_mcp_servers(middle, &none).expect("rewrite"),
            "a: 1\n\n# next\nb: 2\n"
        );
        assert_eq!(
            rewrite_mcp_servers(&format!("a: 1\n\n{block}"), &none).expect("rewrite"),
            "a: 1\n"
        );
        assert_eq!(rewrite_mcp_servers(block, &none).expect("rewrite"), "");
        assert_eq!(
            rewrite_mcp_servers("a: 1\n", &none).expect("rewrite"),
            "a: 1\n"
        );
    }

    #[test]
    fn every_default_field_is_left_out_of_a_written_entry() {
        let both = servers(&[
            (
                "fs",
                McpServerConfig {
                    command: "npx".to_owned(),
                    env: BTreeMap::from([("LOG".to_owned(), "info".to_owned())]),
                    defer: Some("file tools".to_owned()),
                    ..McpServerConfig::default()
                },
            ),
            (
                "gh",
                McpServerConfig {
                    url: "https://gh.example/mcp".to_owned(),
                    headers: BTreeMap::from([(
                        "Authorization".to_owned(),
                        "Bearer ${env:GH}".to_owned(),
                    )]),
                    ..McpServerConfig::default()
                },
            ),
        ]);
        assert_eq!(
            rewrite_mcp_servers("", &both).expect("rewrite"),
            "mcp_servers:\n  fs:\n    command: npx\n    env:\n      LOG: info\n    defer: file tools\n  gh:\n    url: https://gh.example/mcp\n    headers:\n      Authorization: Bearer ${env:GH}\n"
        );
    }

    #[test]
    fn read_and_write_go_through_the_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(".iota.yaml");
        // Missing: an empty document, not an error.
        let fresh = read_mcp_servers(&path).expect("missing file reads as empty");
        assert_eq!(fresh.text, "");
        assert!(fresh.servers.is_empty());

        std::fs::write(&path, SAMPLE).expect("write");
        let read = read_mcp_servers(&path).expect("read");
        assert_eq!(read.text, SAMPLE);
        assert_eq!(
            read.servers.keys().collect::<Vec<_>>(),
            ["fs", "old"],
            "the file's own entries"
        );

        let mut next = read.servers.clone();
        next.remove("old");
        write_mcp_servers(&path, &read.text, &next).expect("write");
        let again = read_mcp_servers(&path).expect("read back");
        assert_eq!(again.servers.keys().collect::<Vec<_>>(), ["fs"]);
        assert!(again.text.starts_with("# top comment\n"), "prefix kept");
        assert!(
            again
                .text
                .contains("\n# The endpoints, after the servers on purpose\nproviders:\n")
        );

        // A file iota would refuse to load is refused here too, naming the file and the coordinate.
        std::fs::write(&path, "providers:\n  p: {kye: k}\n").expect("write");
        let err = read_mcp_servers(&path).expect_err("a broken config is not rewritten");
        assert_eq!(
            err.to_string(),
            format!(
                "config {}: providers.p.kye: unknown key (want type, key, url)",
                path.display()
            )
        );
    }
}
