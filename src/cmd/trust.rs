//! The startup gate on the project config (`crate::config::trust`): what a run does with a `./.iota.yaml`
//! the user has not trusted.
//!
//! It is [`Config::load`](crate::config::Config::load)'s `admit`, so it runs while the config is being read —
//! before a provider is opened, an MCP server started, a tool built or a model turn sent. Nothing the
//! project file describes exists yet to answer the question, and the answer is read from the process's own
//! terminal.
//!
//! - trusted with these bytes: merged, silently;
//! - an interactive start on a terminal: the user is shown what the file does and asked once — trust it (the
//!   bytes are recorded) or not now (the file is skipped for this run, said in one line);
//! - anything else — `-m`, a piped stdin or stderr, `iota list`, `iota config check`: never asked; the file
//!   is skipped with one warning naming it, `iota config trust` and `-c`, and the run goes on with the home
//!   config.

use std::io::BufRead;
use std::path::Path;

use crate::app::HostDirs;
use crate::config::trust::{Standing, TrustStore, summary};

/// The `admit` for [`Config::load`](crate::config::Config::load): `ask` = an interactive start whose stdin
/// and stderr are both a terminal; a `false` here never reads stdin.
pub(crate) fn gate(
    dirs: &HostDirs,
    ask: bool,
) -> impl FnMut(&Path, &[u8], &mut dyn FnMut(String)) -> bool + '_ {
    move |path, bytes, warn| {
        let store = TrustStore::of(dirs);
        let standing = store
            .as_ref()
            .map_or(Standing::Unknown, |s| s.standing(path, bytes));
        if standing == Standing::Trusted {
            return true;
        }
        if !ask {
            warn(skipped(path, standing));
            return false;
        }
        let trusted = ask_user(path, standing, bytes, &mut std::io::stdin().lock(), warn);
        if !trusted {
            warn(format!(
                "Not using {} for this run (`iota config trust` trusts it)",
                path.display()
            ));
            return false;
        }
        match store.map(|s| s.trust(path, bytes)) {
            Some(Ok(())) => {}
            Some(Err(e)) => warn(format!(
                "Warning: could not record the trust of {}: {e} (trusted for this run only)",
                path.display()
            )),
            None => warn(format!(
                "Warning: no home directory to record the trust of {} in (trusted for this run only)",
                path.display()
            )),
        }
        true
    }
}

/// The one line a run that is not asked prints for a project file it skips.
fn skipped(path: &Path, standing: Standing) -> String {
    let why = match standing {
        Standing::Changed => "it has changed since it was trusted",
        _ => "it is not trusted",
    };
    format!(
        "Warning: skipping project config {} ({why}): run `iota config trust` to trust it, or pass it with -c",
        path.display()
    )
}

/// What the file does, one line each: shown before the question and by `iota config trust`.
pub(crate) fn describe(bytes: &[u8]) -> Vec<String> {
    let lines = summary(bytes);
    if lines.is_empty() {
        return vec![
            "It starts no MCP server, sets no provider url, and loosens no sandbox or write setting."
                .to_owned(),
        ];
    }
    std::iter::once("It sets:".to_owned())
        .chain(lines.into_iter().map(|l| format!("  {l}")))
        .collect()
}

/// Shows what `path` does (through `say`, the run's stderr) and asks once, the answer typed on the line
/// below the question. `t`/`trust`/`y`/`yes` trust it; anything else — Enter, end of input, a read error —
/// is "not now".
fn ask_user(
    path: &Path,
    standing: Standing,
    bytes: &[u8],
    input: &mut dyn BufRead,
    say: &mut dyn FnMut(String),
) -> bool {
    let why = match standing {
        Standing::Changed => "has changed since you trusted it",
        _ => "is a project config you have not trusted",
    };
    say(format!("{} {why}.", path.display()));
    for line in describe(bytes) {
        say(line);
    }
    say("Trust it? [t]rust / [N]ot now".to_owned());
    let mut answer = String::new();
    if input.read_line(&mut answer).is_err() {
        return false;
    }
    matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "t" | "trust" | "y" | "yes"
    )
}

#[cfg(test)]
mod tests {
    use super::{Standing, ask_user};
    use std::path::Path;

    fn answer(typed: &str) -> (bool, Vec<String>) {
        let mut shown = Vec::new();
        let trusted = ask_user(
            Path::new("/repo/.iota.yaml"),
            Standing::Unknown,
            b"mcp_servers:\n  x: {command: sh}\n",
            &mut typed.as_bytes(),
            &mut |l| shown.push(l),
        );
        (trusted, shown)
    }

    /// The question shows what the file does; only an explicit yes trusts it — Enter and end of input are
    /// "not now".
    #[test]
    fn only_an_explicit_yes_trusts() {
        let (trusted, shown) = answer("t\n");
        assert!(trusted);
        assert_eq!(
            shown,
            [
                "/repo/.iota.yaml is a project config you have not trusted.",
                "It sets:",
                "  mcp_servers.x: runs sh",
                "Trust it? [t]rust / [N]ot now",
            ]
        );
        assert!(answer("Yes\n").0);
        assert!(!answer("\n").0, "Enter is not now");
        assert!(!answer("").0, "end of input is not now");
        assert!(!answer("n\n").0);
    }
}
