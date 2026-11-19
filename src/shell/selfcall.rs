//! The one command line the `shell` sandbox lets out: iota itself (brain page `harness-prompt`, DIVERGENCES
//! X-44). A command whose FIRST WORD is the running binary — `iota run coder -m "…"`, `iota mcp login nb`,
//! `/opt/homebrew/bin/iota mcp list` — runs outside the sandbox, because what it does (open a browser, reach the
//! model's API from a child agent) is exactly what the sandbox exists to refuse, and a user who put iota on the
//! machine already trusts it. Approval is the `shell` tool's business and is not changed here; this module only
//! answers the question.
//!
//! Except the commands that write the trust record or a config: `iota config trust`, `iota config untrust`,
//! `iota config init`, `iota mcp add` and `iota mcp remove` are never self-calls (DIVERGENCES X-95). Let out, they would hand the model the one write the
//! protected-path table exists to refuse — `~/.iota/trusted.json`, `~/.iota.yaml`, `<root>/.iota.yaml` — and the
//! next run would start the config the model wrote. Kept in, the write fails and the model asks the user.
//!
//! And the lines that CHOOSE what the child loads or starts: a line carrying `-c`/`--config` or `--mcp` is never
//! a self-call either (DIVERGENCES X-44, amended). Let out, `iota -c scratch.yaml run a -m …` would start a
//! config the model wrote inside the project — `sandbox: off`, `auto_run: true`, any `write_roots` — and
//! `iota run a --mcp 'sh -c "…"' -m …` an arbitrary command as a server, both outside the sandbox.
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
//! - the writers are recognised by their verb words in order — `config` then `trust`/`untrust`/`init`, `mcp`
//!   then `add`/`remove` — anywhere among the arguments, so no spelling of the global flags (`-c f`, `--config=f`, before or
//!   after the verb) hides one. A line that merely mentions both words outside those commands stays in too;
//! - the choosing flags are recognised as whole words anywhere among the arguments: `--config`/`--mcp` alone
//!   or with `=value`, and `-c` alone, attached (`-cf.yaml`) or with `=` (`-c=f.yaml`) — clap takes all three
//!   for a short flag with a value. A `-c` inside a quoted `-m` message is inside that word and is not one.
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
    crate::app::canonical(&program).is_some_and(|p| p == exe)
        && !writes_trust_or_config(args)
        && !chooses_what_runs(args)
}

/// The long flags that choose what the child loads or starts: the config it reads, a server it starts.
const CHOOSING_LONG: &[&str] = &["--config", "--mcp"];

/// The short flags among them, each one taking a value.
const CHOOSING_SHORT: &[&str] = &["-c"];

/// Whether iota's arguments carry a flag of [`CHOOSING_LONG`] (the word, or `--flag=value`) or of
/// [`CHOOSING_SHORT`] (the word, `-c=value`, or the attached `-cvalue`). A cluster that puts a short flag
/// after another (`-Vc f`) is not looked for: iota's only valueless short flags are `-h` and `-V`, which
/// print and exit before anything is loaded.
fn chooses_what_runs(args: &[String]) -> bool {
    args.iter().any(|a| {
        CHOOSING_LONG.iter().any(|f| {
            a.strip_prefix(f)
                .is_some_and(|r| r.is_empty() || r.starts_with('='))
        }) || CHOOSING_SHORT.iter().any(|f| a.starts_with(f))
    })
}

/// Whether iota's arguments name `config trust|untrust|init` or `mcp add|remove` — every command that writes
/// `~/.iota.yaml`, `<root>/.iota.yaml` or the trust record: the verb word, then the action
/// word somewhere after it. The verb set is closed and neither word has an alias or an abbreviation, so the
/// words are all a parse would look at; the flags between them are skipped by not looking.
fn writes_trust_or_config(args: &[String]) -> bool {
    args.iter().enumerate().any(|(i, verb)| {
        let rest = &args[i + 1..];
        match verb.as_str() {
            "config" => rest
                .iter()
                .any(|a| matches!(a.as_str(), "trust" | "untrust" | "init")),
            "mcp" => rest.iter().any(|a| matches!(a.as_str(), "add" | "remove")),
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
            "config init",
            "mcp remove x",
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

    /// A line that chooses the child's config or starts a server of its own is never a self-call (X-44,
    /// amended): `-c`, `--config` and `--mcp`, as a word, an `=` form or — for `-c` — attached, before or
    /// after the verb. A `-c` or `--mcp` inside a quoted message is part of that word and changes nothing,
    /// and a flag that merely begins with the same letters is another flag.
    #[test]
    fn a_line_that_chooses_a_config_or_a_server_stays_in() {
        let dir = tempfile::tempdir().expect("tempdir");
        let iota = dir.path().join("iota");
        std::fs::write(&iota, b"#!/bin/sh\n").expect("fake iota");
        let exe = crate::app::canonical(&iota).expect("canonical");
        let look_path = |name: &str| -> Option<PathBuf> { (name == "iota").then(|| iota.clone()) };
        let is = |cmd: &str| self_invocation_with(cmd, &exe, &look_path);

        for line in [
            "iota -c scratch.yaml run a -m hi",
            "iota run a -c scratch.yaml -m hi",
            "iota run a -m hi -c scratch.yaml",
            "iota -cscratch.yaml run a -m hi",
            "iota -c=scratch.yaml run a -m hi",
            "iota --config scratch.yaml run a -m hi",
            "iota --config=scratch.yaml run a -m hi",
            "iota -c scratch.yaml",
            "iota -c scratch.yaml list",
            "iota 'list' \"-c\" scratch.yaml",
            "iota run a --mcp 'sh -c \"touch x\"' -m hi",
            "iota run a --mcp='sh -c x' -m hi",
            "iota --mcp 'npx srv' -m hi",
            "iota resume --mcp 'npx srv' -m hi",
            "iota run a -m hi --mcp https://x/mcp 2>&1",
        ] {
            assert!(!is(line), "{line:?} must stay in the sandbox");
        }

        // The words are read with POSIX quoting: a flag inside the message is message text.
        assert!(is("iota run a -m 'call it with -c f.yaml'"));
        assert!(is("iota run a -m \"pass --mcp=x and --config f\""));
        assert!(is("iota run a -m hi"));
        assert!(is("iota -M sonnet run a -m hi"));
        // Another flag that shares a prefix is not one of them.
        assert!(is("iota run a --mcpx -m hi"));
        assert!(is("iota run a --configure -m hi"));
    }

    /// Without a known executable nothing is ever let out.
    #[test]
    fn no_known_binary_means_no_exception() {
        assert!(!super::is_self_invocation("iota mcp list", None));
    }
}
