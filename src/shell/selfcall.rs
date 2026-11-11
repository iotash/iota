//! The one command line the `shell` sandbox lets out: iota itself (brain page `harness-prompt`, DIVERGENCES
//! X-44). A command whose FIRST WORD is the running binary — `iota run coder -m "…"`, `iota mcp login nb`,
//! `/opt/homebrew/bin/iota mcp list` — runs outside the sandbox, because what it does (open a browser, reach the
//! model's API from a child agent) is exactly what the sandbox exists to refuse, and a user who put iota on the
//! machine already trusts it. Approval is the `shell` tool's business and is not changed here; this module only
//! answers the question.
//!
//! Except the commands that write the trust record or a config: `iota config trust`, `iota config untrust` and
//! `iota mcp add` are never self-calls (DIVERGENCES X-95). Let out, they would hand the model the one write the
//! protected-path table exists to refuse — `~/.iota/trusted.json`, `~/.iota.yaml`, `<root>/.iota.yaml` — and the
//! next run would start the config the model wrote. Kept in, the write fails and the model asks the user.
//!
//! The answer is conservative by construction:
//!
//! - the line is read with POSIX quoting (`'…'`, `"…"`, `\`) and the first word must resolve — an absolute or
//!   relative path as written, a bare name on `PATH` — to the SAME canonical file as the binary;
//! - the line must be one simple command of literal words: a pipe (`|`), a chain (`&&`, `||`, `;`, `&`), a
//!   newline, a subshell or process substitution (`(`, `)`), and any expansion — `$` outside single quotes,
//!   a glob or brace (`*`, `?`, `[`, `{`) outside quotes — make the words something the shell decides later,
//!   and it stays in the sandbox. Redirections (`>`, `<`, `2>&1`, `&>`) are allowed and dropped with their
//!   targets: they change where iota's output goes, not what runs;
//! - the writers are recognised by their verb words in order — `config` then `trust`/`untrust`, `mcp` then
//!   `add` — anywhere among the arguments, so no spelling of the global flags (`-c f`, `--config=f`, before or
//!   after the verb) hides one. A line that merely mentions both words outside those commands stays in too.
//!
//! The lexer is POSIX because the sandbox is: it exists on macOS and Linux only, where the interpreter is
//! `bash` (or another POSIX shell named by `IOTA_SHELL`). A PowerShell or `cmd.exe` line never reaches a
//! sandbox, so it never reaches this question.

use std::path::{Path, PathBuf};

/// Whether `command` is iota itself with arguments — first word resolving to `exe`, no compound structure, not
/// one of the writers. `exe` is the running binary as `HostDirs::exe` resolved it (canonical); `None` means the
/// process does not know its own path, and nothing is let out.
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
    let Some(words) = words(command) else {
        return false;
    };
    let Some((first, args)) = words.split_first() else {
        return false;
    };
    let Some(program) = look_path(first) else {
        return false;
    };
    crate::app::canonical(&program).is_some_and(|p| p == exe) && !writes_trust_or_config(args)
}

/// Whether iota's arguments name `config trust`, `config untrust` or `mcp add`: the verb word, then the action
/// word somewhere after it. The verb set is closed and neither word has an alias or an abbreviation, so the
/// words are all a parse would look at; the flags between them are skipped by not looking.
fn writes_trust_or_config(args: &[String]) -> bool {
    args.iter().enumerate().any(|(i, verb)| {
        let rest = &args[i + 1..];
        match verb.as_str() {
            "config" => rest.iter().any(|a| a == "trust" || a == "untrust"),
            "mcp" => rest.iter().any(|a| a == "add"),
            _ => false,
        }
    })
}

/// The command's words, unquoted, with redirections and their targets dropped — `None` when the line is empty
/// or is not ONE simple command of literal words.
///
/// One pass over the bytes, tracking the quote state. Inside single quotes everything is literal; inside
/// double quotes and outside quotes a `\` escapes the next character (and a `\`-newline is a line
/// continuation, gone). A `$` or `` ` `` anywhere but single quotes, and a glob or brace character outside
/// quotes, is an expansion: the shell would decide the words, so there is no answer. A redirection operator
/// (`>`, `>>`, `<`, `>&`, `&>`, `<<<` …) drops an unquoted all-digit word right before it (the fd of `2>`) and
/// the word after it (the target), so `2>&1` is neither a background `&` nor an argument.
pub(crate) fn words(command: &str) -> Option<Vec<String>> {
    let bytes = command.as_bytes();
    let mut lx = Lexer::default();
    let mut quote: Option<u8> = None;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(b'\'') => {
                if b == b'\'' {
                    quote = None;
                } else {
                    lx.word.push(b);
                }
                i += 1;
                continue;
            }
            Some(_) => {
                if b == b'"' {
                    quote = None;
                } else if b == b'\\' && i + 1 < bytes.len() {
                    i += 1;
                    if bytes[i] != b'\n' {
                        lx.word.push(bytes[i]);
                    }
                } else if b == b'$' || b == b'`' {
                    return None;
                } else {
                    lx.word.push(b);
                }
                i += 1;
                continue;
            }
            None => {}
        }
        match b {
            b'\'' | b'"' => {
                quote = Some(b);
                lx.in_word = true;
                lx.quoted = true;
            }
            b'\\' if i + 1 < bytes.len() => {
                i += 1;
                if bytes[i] != b'\n' {
                    lx.word.push(bytes[i]);
                    lx.in_word = true;
                    lx.quoted = true;
                }
            }
            b'|' | b';' | b'\n' | b'(' | b')' | b'`' | b'$' | b'*' | b'?' | b'[' | b'{' => {
                return None;
            }
            b'>' | b'<' | b'&' => {
                // `&>` is a redirection; any other `&` is a background job or a chain.
                if b == b'&' && bytes.get(i + 1) != Some(&b'>') {
                    return None;
                }
                lx.redirect()?;
                while matches!(bytes.get(i + 1), Some(b'>' | b'<' | b'&')) {
                    i += 1;
                }
            }
            b' ' | b'\t' => lx.end()?,
            _ => {
                lx.word.push(b);
                lx.in_word = true;
            }
        }
        i += 1;
    }
    if quote.is_some() {
        return None;
    }
    lx.end()?;
    if lx.target {
        return None;
    }
    (!lx.words.is_empty()).then_some(lx.words)
}

/// [`words`]' state between bytes.
#[derive(Default)]
struct Lexer {
    /// The words read so far.
    words: Vec<String>,
    /// The word being read.
    word: Vec<u8>,
    /// Whether a word is being read (`''` is a word, an empty one).
    in_word: bool,
    /// Whether the word being read had a quote or an escape — `"2">f` passes the argument `2`.
    quoted: bool,
    /// Whether the next word is a redirection's target.
    target: bool,
}

impl Lexer {
    /// The word being read ends: kept, or dropped when it is a redirection's target.
    fn end(&mut self) -> Option<()> {
        if self.in_word {
            let word = String::from_utf8(std::mem::take(&mut self.word)).ok()?;
            if !std::mem::take(&mut self.target) {
                self.words.push(word);
            }
        }
        self.in_word = false;
        self.quoted = false;
        Some(())
    }

    /// A redirection operator starts: the word before it ends (or is dropped, as the fd it names), and the
    /// next word is the target. An operator still waiting for its target is a syntax error.
    fn redirect(&mut self) -> Option<()> {
        if self.in_word && !self.quoted && self.word.iter().all(u8::is_ascii_digit) {
            self.word.clear();
            self.in_word = false;
        } else {
            self.end()?;
        }
        if self.target {
            return None;
        }
        self.target = true;
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{self_invocation_with, words};

    fn w(line: &str) -> Option<Vec<String>> {
        words(line)
    }

    fn v(words: &[&str]) -> Option<Vec<String>> {
        let words: Vec<String> = words.iter().map(|&s| s.to_owned()).collect();
        (!words.is_empty()).then_some(words)
    }

    /// The words, and whether the line is one simple command of literal words at all.
    #[test]
    fn words_read_posix_quoting_and_refuse_structure_and_expansion() {
        assert_eq!(
            w("iota mcp add x --url u"),
            v(&["iota", "mcp", "add", "x", "--url", "u"])
        );
        assert_eq!(w("  iota\tmcp list"), v(&["iota", "mcp", "list"]));
        assert_eq!(
            w("/opt/homebrew/bin/iota mcp list"),
            v(&["/opt/homebrew/bin/iota", "mcp", "list"])
        );
        assert_eq!(w("'io'\"ta\" run x"), v(&["iota", "run", "x"]));
        assert_eq!(w("io\\ta run"), v(&["iota", "run"]));
        assert_eq!(w("iota ''"), v(&["iota", ""]));
        // A line continuation is gone, inside double quotes and out.
        assert_eq!(
            w("iota config tr\\\nust \"un\\\ntrust\""),
            v(&["iota", "config", "trust", "untrust"])
        );
        // Redirections are not structure, and neither they nor their targets are arguments.
        assert_eq!(
            w("iota mcp list 2>&1 >/tmp/out"),
            v(&["iota", "mcp", "list"])
        );
        assert_eq!(w("iota mcp list &> log"), v(&["iota", "mcp", "list"]));
        assert_eq!(w("iota mcp list &>>log"), v(&["iota", "mcp", "list"]));
        assert_eq!(w("iota>out"), v(&["iota"]));
        assert_eq!(w("iota list < /dev/null x"), v(&["iota", "list", "x"]));
        // A quoted `2` before `>` is an argument, not a file descriptor.
        assert_eq!(w("iota \"2\">f"), v(&["iota", "2"]));
        // Quoted punctuation is literal — a child agent's task text may hold any of it.
        assert_eq!(
            w("iota run coder -m 'fix; then $test | tail && *report*'"),
            v(&[
                "iota",
                "run",
                "coder",
                "-m",
                "fix; then $test | tail && *report*"
            ])
        );
        assert_eq!(
            w("iota run coder -m \"a | b; c && d [e] {f} *g?\""),
            v(&["iota", "run", "coder", "-m", "a | b; c && d [e] {f} *g?"])
        );
        // Structure or an expansion outside single quotes: not one simple command of literal words.
        for line in [
            "iota mcp list | head",
            "iota mcp list || true",
            "iota mcp list && echo ok",
            "iota mcp list; echo ok",
            "iota mcp list &",
            "iota mcp list\necho ok",
            "(iota mcp list)",
            "iota mcp list >(cat)",
            "iota run x -m \"$(cat task)\"",
            "iota run x -m `cat task`",
            "iota run x -m \"`cat task`\"",
            "iota run x -m \"fix $HOME\"",
            "iota $VERB trust",
            "iota config tru*",
            "iota config trus?",
            "iota config trus[t]",
            "iota config {trust,x}",
            "echo hi | iota --version",
            "iota mcp list >",
            "iota mcp list > >x",
        ] {
            assert_eq!(w(line), None, "{line:?} must not be one simple command");
        }
        // Empty, whitespace, and an unterminated quote.
        assert_eq!(w(""), None);
        assert_eq!(w("   "), None);
        assert_eq!(w("iota run x -m 'unterminated"), None);
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

        assert!(is("iota"));
        assert!(is("iota -m 'hi'"));
        assert!(is("iota list"));
        assert!(is("iota mcp login nb"));
        assert!(is("iota config check"));
        assert!(is(&format!("{absolute} mcp list")));
        assert!(is("iota run coder -m 'fix the bug; run the tests'"));
        assert!(is("iota mcp list --probe 2>&1"));
        assert!(!is("iota mcp list | head"));
        assert!(!is("iota-foo mcp list"));
        assert!(!is("echo iota"));
        assert!(!is("nonexistent mcp list"));
        assert!(!is(""));

        // The writers of the trust record and of a config are never self-calls, however the global flag is
        // spelled or placed, and whichever binary spelling names iota.
        for writer in [
            "config trust",
            "config untrust",
            "mcp add x -- y",
            "mcp add x --url https://x/mcp",
            "mcp add --scope user x -- y",
        ] {
            for line in [
                format!("iota {writer}"),
                format!("iota -c f.yaml {writer}"),
                format!("iota -cf.yaml {writer}"),
                format!("iota --config f.yaml {writer}"),
                format!("iota --config=f.yaml {writer}"),
                format!("iota {writer} -c f.yaml"),
                format!("{absolute} {writer} 2>&1"),
                format!("'iota' {writer}"),
            ] {
                assert!(!is(&line), "{line:?} must stay in the sandbox");
            }
        }
        assert!(!is("iota config --path ./.iota.yaml trust"));
        assert!(!is("iota -c f.yaml 'config' \"trust\""));

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
