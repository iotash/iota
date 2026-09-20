//! The one command line the `shell` sandbox lets out: iota itself (brain page `harness-prompt`, DIVERGENCES
//! X-44). A command whose FIRST WORD is the running binary — `iota mcp add …`, `iota run coder -m "…"`,
//! `/opt/homebrew/bin/iota mcp login nb` — runs outside the sandbox, because what it does (write a config
//! file in `$HOME`, open a browser, reach the model's API from a child agent) is exactly what the sandbox
//! exists to refuse, and a user who put iota on the machine already trusts it. Approval is the `shell`
//! tool's business and is not changed here; this module only answers the question.
//!
//! The answer is conservative by construction:
//!
//! - the first word is read with POSIX quoting (`'…'`, `"…"`, `\`) and must resolve — an absolute or
//!   relative path as written, a bare name on `PATH` — to the SAME canonical file as the binary;
//! - the rest of the command must be one simple command: a pipe (`|`), a chain (`&&`, `||`, `;`, `&`), a
//!   newline, a command substitution (`$(…)`, `` `…` ``) anywhere OUTSIDE single quotes makes the whole line
//!   something other than "iota, with arguments", and it stays in the sandbox. Redirections (`>`, `<`,
//!   `2>&1`, `&>`) are allowed: they change where iota's output goes, not what runs.
//!
//! The lexer is POSIX because the sandbox is: it exists on macOS and Linux only, where the interpreter is
//! `bash` (or another POSIX shell named by `IOTA_SHELL`). A PowerShell or `cmd.exe` line never reaches a
//! sandbox, so it never reaches this question.

use std::path::{Path, PathBuf};

/// Whether `command` is iota itself with arguments — first word resolving to `exe`, no compound structure.
/// `exe` is the running binary as `HostDirs::exe` resolved it (canonical); `None` means the process does not
/// know its own path, and nothing is let out.
pub fn is_self_invocation(command: &str, exe: Option<&Path>) -> bool {
    exe.is_some_and(|exe| self_invocation_with(command, exe, &super::exec::find_in_path))
}

/// [`is_self_invocation`] over an injected `PATH` lookup, so a test can stand a fake binary in for the
/// real one without touching the process environment.
pub fn self_invocation_with(
    command: &str,
    exe: &Path,
    look_path: &dyn Fn(&str) -> Option<PathBuf>,
) -> bool {
    let Some(first) = first_word(command) else {
        return false;
    };
    let Some(program) = look_path(&first) else {
        return false;
    };
    crate::app::canonical(&program).is_some_and(|p| p == exe)
}

/// The command's first word, unquoted — `None` when the line is empty or is not ONE simple command.
///
/// One pass over the bytes, tracking the quote state: outside quotes and inside double quotes a
/// command substitution counts as structure; only inside single quotes is everything literal. A `\`
/// escapes the next character outside single quotes. Redirection operators are skipped as the two
/// characters they are (`>`, `<`, `>&`, `&>`), so `2>&1` is not read as a background `&`.
pub(crate) fn first_word(command: &str) -> Option<String> {
    let bytes = command.as_bytes();
    let mut word = Vec::new();
    let mut in_word = false;
    let mut word_done = false;
    let mut quote: Option<u8> = None;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(b'\'') => {
                if b == b'\'' {
                    quote = None;
                } else if !word_done {
                    word.push(b);
                }
                i += 1;
                continue;
            }
            Some(_) => {
                // Inside double quotes: `\` still escapes, and `$(` / `` ` `` still substitute.
                if b == b'"' {
                    quote = None;
                } else if b == b'\\' && i + 1 < bytes.len() {
                    i += 1;
                    if !word_done {
                        word.push(bytes[i]);
                    }
                } else if b == b'`' || (b == b'$' && bytes.get(i + 1) == Some(&b'(')) {
                    return None;
                } else if !word_done {
                    word.push(b);
                }
                i += 1;
                continue;
            }
            None => {}
        }
        match b {
            b'\'' | b'"' => {
                quote = Some(b);
                in_word = true;
            }
            b'\\' if i + 1 < bytes.len() => {
                i += 1;
                if !word_done {
                    word.push(bytes[i]);
                }
                in_word = true;
            }
            b'|' | b';' | b'\n' | b'`' => return None,
            b'$' if bytes.get(i + 1) == Some(&b'(') => return None,
            b'&' => {
                // `>&` and `&>` are redirections; any other `&` is a background job or a chain.
                let after_redirect = i > 0 && bytes[i - 1] == b'>';
                let before_redirect = bytes.get(i + 1) == Some(&b'>');
                if !after_redirect && !before_redirect {
                    return None;
                }
                if in_word && !word_done {
                    word_done = true;
                }
            }
            b' ' | b'\t' | b'>' | b'<' => {
                if in_word && !word_done {
                    word_done = true;
                }
            }
            _ => {
                if !word_done {
                    word.push(b);
                }
                in_word = true;
            }
        }
        i += 1;
    }
    if quote.is_some() || word.is_empty() {
        return None;
    }
    String::from_utf8(word).ok()
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{first_word, self_invocation_with};

    /// The first word, and whether the line is one simple command at all.
    #[test]
    fn first_word_reads_posix_quoting_and_refuses_structure() {
        assert_eq!(
            first_word("iota mcp add x --url u").as_deref(),
            Some("iota")
        );
        assert_eq!(first_word("  iota\tmcp list").as_deref(), Some("iota"));
        assert_eq!(
            first_word("/opt/homebrew/bin/iota mcp list").as_deref(),
            Some("/opt/homebrew/bin/iota")
        );
        assert_eq!(first_word("'io'\"ta\" run x").as_deref(), Some("iota"));
        assert_eq!(first_word("io\\ta run").as_deref(), Some("iota"));
        // Redirections are not structure.
        assert_eq!(
            first_word("iota mcp list 2>&1 >/tmp/out").as_deref(),
            Some("iota")
        );
        assert_eq!(first_word("iota mcp list &> log").as_deref(), Some("iota"));
        assert_eq!(first_word("iota>out").as_deref(), Some("iota"));
        // Quoted punctuation is literal — a child agent's task text may hold any of it.
        assert_eq!(
            first_word("iota run coder -m 'fix; then test | tail && report'").as_deref(),
            Some("iota")
        );
        assert_eq!(
            first_word("iota run coder -m \"a | b; c && d\"").as_deref(),
            Some("iota")
        );
        // Structure outside quotes: not one simple command.
        for line in [
            "iota mcp list | head",
            "iota mcp list || true",
            "iota mcp list && echo ok",
            "iota mcp list; echo ok",
            "iota mcp list &",
            "iota mcp list\necho ok",
            "iota run x -m \"$(cat task)\"",
            "iota run x -m `cat task`",
            "iota run x -m \"`cat task`\"",
            "echo hi | iota --version",
        ] {
            assert_eq!(
                first_word(line),
                None,
                "{line:?} must not be one simple command"
            );
        }
        // Empty, whitespace, and an unterminated quote.
        assert_eq!(first_word(""), None);
        assert_eq!(first_word("   "), None);
        assert_eq!(first_word("iota run x -m 'unterminated"), None);
    }

    /// The whole rule over a fake binary and a fake `PATH`: the bare name and the absolute path both
    /// resolve to the binary; a look-alike name, a different program and a compound line do not.
    #[test]
    fn self_invocation_compares_the_resolved_first_word_to_the_binary() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).expect("bin");
        let iota = bin.join("iota");
        std::fs::write(&iota, b"#!/bin/sh\n").expect("fake iota");
        let other = bin.join("iota-foo");
        std::fs::write(&other, b"#!/bin/sh\n").expect("fake iota-foo");
        let exe = crate::app::canonical(&iota).expect("canonical");
        let look_path = |name: &str| -> Option<PathBuf> {
            let p = Path::new(name);
            if p.is_absolute() {
                return p.is_file().then(|| p.to_path_buf());
            }
            let candidate = bin.join(name);
            candidate.is_file().then_some(candidate)
        };
        let is = |cmd: &str| self_invocation_with(cmd, &exe, &look_path);
        // The absolute spelling as a POSIX shell would read it: forward slashes, which Windows accepts too
        // (a backslash is an escape to the lexer, as it is to bash).
        let absolute = iota.display().to_string().replace('\\', "/");

        assert!(is("iota mcp add x --url https://x/mcp"));
        assert!(is(&format!("{absolute} mcp list")));
        assert!(is("iota run coder -m 'fix the bug; run the tests'"));
        assert!(is("iota mcp list --probe 2>&1"));
        assert!(!is("iota mcp list | head"));
        assert!(!is("iota-foo mcp list"));
        assert!(!is("echo iota"));
        assert!(!is("nonexistent mcp list"));
        assert!(!is(""));

        // A symlink to the binary resolves to it (Homebrew's `bin/iota` → `Cellar/…`).
        #[cfg(unix)]
        {
            let link = bin.join("iota-link");
            std::os::unix::fs::symlink(&iota, &link).expect("symlink");
            assert!(is("iota-link mcp list"));
        }
    }

    /// Without a known executable nothing is ever let out.
    #[test]
    fn no_known_binary_means_no_exception() {
        assert!(!super::is_self_invocation("iota mcp list", None));
    }
}
