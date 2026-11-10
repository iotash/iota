//! Project-config trust: whether `./.iota.yaml|yml` may take effect.
//!
//! A project file can start MCP servers, repoint a provider (and `${VAR}`-expand a key into it), turn the shell
//! sandbox off and waive approval — so cloning a repository and running iota in it must not run what the
//! repository says until the user has said yes to it. The home file is the user's own and `-c <file>` is a
//! file the user named; only the project layer is gated (see [`crate::config::Config::load`]).
//!
//! The record lives in the user's own directory, never in the repository: `<home>/.iota/trusted.json`, a
//! JSON object from a file's REAL path (symlinks resolved) to the SHA-256 of its bytes. An entry holds only
//! while both match, so an edited file asks again — trusting a path once and forever is how Cursor shipped
//! CVE-2025-54136.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use serde_norway::Value;
use sha2::{Digest as _, Sha256};

use crate::app::HostDirs;

/// The record's file name under `<home>/.iota`.
const TRUST_FILE: &str = "trusted.json";

/// Where a project file stands against the record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// Recorded, with these exact bytes.
    Trusted,
    /// Never recorded (or no record can be kept: no home).
    Unknown,
    /// Recorded, but its bytes have changed since.
    Changed,
}

/// The trust record of one user.
#[derive(Debug, Clone)]
pub struct TrustStore {
    /// `<home>/.iota/trusted.json`.
    path: PathBuf,
}

impl TrustStore {
    /// The record under `dirs.home`; `None` without a home, where nothing can be trusted.
    pub fn of(dirs: &HostDirs) -> Option<Self> {
        dirs.app_home().map(|h| Self {
            path: h.join(TRUST_FILE),
        })
    }

    /// The file the record is kept in.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `file`'s standing, `file` being the path its bytes were read from. A record that cannot be read or
    /// parsed holds nothing: every file asks again rather than being trusted by accident.
    pub fn standing(&self, file: &Path, bytes: &[u8]) -> Standing {
        let entries = self.read();
        match entries.get(&key(file)) {
            Some(digest) if *digest == hash(bytes) => Standing::Trusted,
            Some(_) => Standing::Changed,
            None => Standing::Unknown,
        }
    }

    /// Records `bytes` as the trusted content of `file`, replacing any earlier entry. Written atomically,
    /// mode 0600.
    pub fn trust(&self, file: &Path, bytes: &[u8]) -> std::io::Result<()> {
        let mut entries = self.read();
        entries.insert(key(file), hash(bytes));
        self.write(&entries)
    }

    /// Removes `file`'s entry; `false` when there was none.
    pub fn forget(&self, file: &Path) -> std::io::Result<bool> {
        let mut entries = self.read();
        if entries.remove(&key(file)).is_none() {
            return Ok(false);
        }
        self.write(&entries)?;
        Ok(true)
    }

    fn read(&self) -> BTreeMap<String, String> {
        std::fs::read(&self.path)
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default()
    }

    fn write(&self, entries: &BTreeMap<String, String>) -> std::io::Result<()> {
        let mut data = serde_json::to_vec_pretty(entries).map_err(std::io::Error::other)?;
        data.push(b'\n');
        crate::app::fs::write_atomic(&self.path, &data, Some(0o600))
    }
}

/// The record's key for `file`: its real path when it resolves (a symlinked or `..`-spelled path is the
/// same file), else the path as given — the case of a file that has been deleted since it was trusted.
fn key(file: &Path) -> String {
    crate::app::canonical(file)
        .unwrap_or_else(|| file.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// `sha256:<hex>` of `bytes`.
fn hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::from("sha256:");
    for b in digest.iter() {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// What a project file would DO, one line per setting that runs code, sends the key somewhere, or loosens a
/// guard: the MCP servers it starts or connects, a provider `url` (and a `${…}` key) it sets, `sandbox`,
/// `auto_run`, `auto_write`, `write_roots`, `outside_writes`. Read from the raw document, so the user sees the
/// values as written. Empty when the file sets none of them.
pub fn summary(bytes: &[u8]) -> Vec<String> {
    let Ok(doc) = serde_norway::from_slice::<Value>(bytes) else {
        return vec!["(not valid YAML: it would be ignored)".to_owned()];
    };
    let mut out = Vec::new();
    for (name, server) in entries(doc.get("mcp_servers")) {
        if let Some(command) = scalar(server.get("command")) {
            let mut line = format!("mcp_servers.{name}: runs {command}");
            if let Some(Value::Sequence(args)) = server.get("args") {
                for arg in args.iter().filter_map(|a| scalar(Some(a))) {
                    if arg.contains(char::is_whitespace) || arg.is_empty() {
                        line.push_str(&format!(" {arg:?}"));
                    } else {
                        line.push(' ');
                        line.push_str(&arg);
                    }
                }
            }
            out.push(line);
        } else if let Some(url) = scalar(server.get("url")) {
            out.push(format!("mcp_servers.{name}: connects to {url}"));
        }
    }
    for (name, provider) in entries(doc.get("providers")) {
        if let Some(url) = scalar(provider.get("url")) {
            out.push(format!("providers.{name}.url: {url}"));
        }
        if let Some(key) = scalar(provider.get("key")).filter(|k| k.contains("${")) {
            out.push(format!("providers.{name}.key: {key}"));
        }
    }
    for (name, agent) in entries(doc.get("agents")) {
        let tools = agent.get("tools");
        let shell = tools.and_then(|t| t.get("shell"));
        let code = tools.and_then(|t| t.get("code"));
        let settings = [
            ("shell.sandbox", shell.and_then(|s| s.get("sandbox"))),
            ("shell.auto_run", shell.and_then(|s| s.get("auto_run"))),
            ("code.auto_write", code.and_then(|c| c.get("auto_write"))),
        ];
        for (at, value) in settings {
            // The defaults (`sandbox: auto`, a waiver set to false) loosen nothing.
            if let Some(v) = scalar(value).filter(|v| v != "auto" && v != "false") {
                out.push(format!("agents.{name}.tools.{at}: {v}"));
            }
        }
    }
    if let Some(Value::Sequence(roots)) = doc.get("write_roots") {
        let roots: Vec<String> = roots.iter().filter_map(|r| scalar(Some(r))).collect();
        if !roots.is_empty() {
            out.push(format!("write_roots: {}", roots.join(", ")));
        }
    }
    if let Some(v) = scalar(doc.get("outside_writes")) {
        out.push(format!("outside_writes: {v}"));
    }
    out
}

/// The `(name, entry)` pairs of a top-level map; nothing when it is absent or not a map.
fn entries(section: Option<&Value>) -> impl Iterator<Item = (String, &Value)> {
    section
        .and_then(Value::as_mapping)
        .into_iter()
        .flatten()
        .filter_map(|(k, v)| scalar(Some(k)).map(|k| (k, v)))
}

/// A scalar as it reads: a string verbatim, a bool or number in its YAML spelling; `None` for null, a
/// collection, or nothing.
fn scalar(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{Standing, TrustStore, summary};
    use crate::app::HostDirs;

    fn store(home: &std::path::Path) -> TrustStore {
        TrustStore::of(&HostDirs {
            home: Some(home.to_path_buf()),
            ..HostDirs::default()
        })
        .expect("a home")
    }

    /// An entry holds for the exact bytes it recorded: one changed byte is `Changed`, and forgetting it makes
    /// the file `Unknown` again.
    #[test]
    fn an_entry_holds_only_for_the_bytes_it_recorded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join(".iota.yaml");
        std::fs::write(&file, b"a: 1\n").expect("write");
        let store = store(&dir.path().join("home"));
        assert_eq!(store.standing(&file, b"a: 1\n"), Standing::Unknown);
        store.trust(&file, b"a: 1\n").expect("trust");
        assert_eq!(store.standing(&file, b"a: 1\n"), Standing::Trusted);
        assert_eq!(store.standing(&file, b"a: 2\n"), Standing::Changed);
        // The same file through another spelling of its path is the same entry.
        let dotted = dir.path().join(".").join(".iota.yaml");
        assert_eq!(store.standing(&dotted, b"a: 1\n"), Standing::Trusted);
        assert!(store.forget(&file).expect("forget"));
        assert!(!store.forget(&file).expect("forget again"));
        assert_eq!(store.standing(&file, b"a: 1\n"), Standing::Unknown);
    }

    /// The record is written atomically (no temp file left beside it) and readable by its owner alone.
    #[cfg(unix)]
    #[test]
    fn the_record_is_private_and_written_whole() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join(".iota.yaml");
        std::fs::write(&file, b"x: 1\n").expect("write");
        let store = store(&dir.path().join("home"));
        store.trust(&file, b"x: 1\n").expect("trust");
        let mode = std::fs::metadata(store.path())
            .expect("meta")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        let names: Vec<String> = std::fs::read_dir(store.path().parent().expect("parent"))
            .expect("read_dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["trusted.json"]);
    }

    /// A record that is not JSON trusts nothing.
    #[test]
    fn a_broken_record_trusts_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join(".iota.yaml");
        let store = store(&dir.path().join("home"));
        std::fs::create_dir_all(store.path().parent().expect("parent")).expect("mkdir");
        std::fs::write(store.path(), b"{not json").expect("write");
        assert_eq!(store.standing(&file, b""), Standing::Unknown);
    }

    /// The summary names what runs, where the key goes, and every loosened guard — and only those.
    #[test]
    fn the_summary_shows_what_the_file_does() {
        let lines = summary(
            br#"
providers:
  openai: {type: openai, url: "https://example.test/v1", key: "${OPENAI_API_KEY}"}
  quiet: {type: anthropic}
mcp_servers:
  local: {command: sh, args: [-c, "touch marker"]}
  remote: {url: "https://mcp.example.test"}
agents:
  coder:
    tools:
      shell: {sandbox: "off", auto_run: true}
      code: {auto_write: true}
  calm:
    tools:
      shell: {sandbox: auto, auto_run: false}
write_roots: [/tmp/out, ~/notes]
outside_writes: ask
"#,
        );
        assert_eq!(
            lines,
            [
                "mcp_servers.local: runs sh -c \"touch marker\"",
                "mcp_servers.remote: connects to https://mcp.example.test",
                "providers.openai.url: https://example.test/v1",
                "providers.openai.key: ${OPENAI_API_KEY}",
                "agents.coder.tools.shell.sandbox: off",
                "agents.coder.tools.shell.auto_run: true",
                "agents.coder.tools.code.auto_write: true",
                "write_roots: /tmp/out, ~/notes",
                "outside_writes: ask",
            ]
        );
        assert!(summary(b"models:\n  m: openai:gpt\n").is_empty());
    }
}
