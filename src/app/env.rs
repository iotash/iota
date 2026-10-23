//! The process environment as ONE injected value (cmd/root.go's `os.Getenv` seam + internal/vars/vars.go):
//! the variables, and the directories [`HostDirs`] resolved once at the binary edge. `main` builds one
//! [`Env::process`] and threads it through everything that reads a variable — `${var}` expansion, key
//! lookup, `NO_COLOR`, `IOTA_LOG`, the host detectors — and every test builds a fixed one, so nothing below
//! `main` reads `std::env` for these and no test mutates the process environment.

use std::{borrow::Cow, collections::HashMap, fmt, path::Path, sync::Arc};

use super::{DOT_DIR, HostDirs};

/// Variables a child process is spawned without — the one mechanism every spawn point that runs what the
/// model asked for applies (`shell::exec::spawn`, `mcp::transport::spawn_stdio`). WHICH names is not this
/// module's knowledge: `crate::host::child_env` gathers them from the detected host. Cheap to clone; the
/// default withholds nothing.
///
/// It also carries the one variable a child is given that the run itself was not: an interactive chat's
/// `IOTA_SOCKET` (`session::sock`), so a script the model starts can message the chat it came from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChildEnv {
    withheld: Arc<[String]>,
    given: Option<(String, String)>,
}

impl ChildEnv {
    /// Withholds `names`, in order, each once.
    pub fn new(names: impl IntoIterator<Item = String>) -> Self {
        let mut withheld: Vec<String> = Vec::new();
        for name in names {
            if !withheld.contains(&name) {
                withheld.push(name);
            }
        }
        Self {
            withheld: withheld.into(),
            given: None,
        }
    }

    /// Also gives every child `name=value`.
    #[must_use]
    pub fn giving(mut self, name: &str, value: &str) -> Self {
        self.given = Some((name.to_owned(), value.to_owned()));
        self
    }

    /// The withheld names.
    pub fn names(&self) -> &[String] {
        &self.withheld
    }

    /// Removes every withheld name from `cmd`'s environment and sets every given one. Call it BEFORE the
    /// caller's own `env`s: a variable a config sets explicitly (an MCP entry's `env:`) is the user's choice,
    /// not an inheritance.
    pub fn apply(&self, cmd: &mut tokio::process::Command) {
        for name in self.withheld.iter() {
            cmd.env_remove(name);
        }
        if let Some((name, value)) = &self.given {
            cmd.env(name, value);
        }
    }
}

/// The variable lookup behind an [`Env`]: `None` when unset.
type VarFn = dyn Fn(&str) -> Option<String> + Send + Sync;

/// The environment a run sees. Cheap to clone (the lookup is shared); the directories ride along so that
/// one value answers every question the process environment used to.
#[derive(Clone)]
pub struct Env {
    vars: Arc<VarFn>,
    /// Process-level directories (home, cwd, temp, cache).
    pub dirs: HostDirs,
}

impl Env {
    /// The real process: `std::env::var` over [`HostDirs::from_env`]. Built ONCE, in `main`.
    pub fn process() -> Self {
        Self::from_fn(|name| std::env::var(name).ok(), HostDirs::from_env())
    }

    /// An environment answered by `vars` over `dirs`.
    pub fn from_fn(
        vars: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
        dirs: HostDirs,
    ) -> Self {
        Self {
            vars: Arc::new(vars),
            dirs,
        }
    }

    /// A fixed environment (tests, fixtures): exactly `vars`, over default (empty) directories —
    /// [`with_dirs`](Self::with_dirs) adds those. Replaces `t.Setenv`.
    pub fn fixed(vars: &[(&str, &str)]) -> Self {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        Self::from_fn(move |name| map.get(name).cloned(), HostDirs::default())
    }

    /// The same variables over `dirs`.
    #[must_use]
    pub fn with_dirs(mut self, dirs: HostDirs) -> Self {
        self.dirs = dirs;
        self
    }

    /// The variable `name`, or None when unset OR empty — Go's `os.Getenv(..) != ""` reading, the one every
    /// switch here wants (`NO_COLOR=` is not set, an exported-but-empty key is no key).
    pub fn var(&self, name: &str) -> Option<String> {
        (self.vars)(name).filter(|v| !v.is_empty())
    }

    /// The working directory, if known.
    pub fn cwd(&self) -> Option<&Path> {
        self.dirs.cwd.as_deref()
    }

    /// The home directory, if known.
    pub fn home(&self) -> Option<&Path> {
        self.dirs.home.as_deref()
    }
}

impl Default for Env {
    /// No variables, no directories.
    fn default() -> Self {
        Self::fixed(&[])
    }
}

impl fmt::Debug for Env {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Env")
            .field("dirs", &self.dirs)
            .finish_non_exhaustive()
    }
}

/// internal/vars/vars.go: single pass over `${name}` (no rescans, `${}` is not a match, names are case-sensitive).
/// `env:NAME` → `env.var(NAME).unwrap_or_default()` (always substituted); `workspaceFolder`|`cwd` → `cwd()`;
/// `userHome` → `home()`; `appHome` → `home()/.iota`; `pathSeparator`|`"/"` → `MAIN_SEPARATOR`; unknown or failed
/// lookup → original `${…}` text.
/// Fast path: input without `"${"` is returned Borrowed.
pub(crate) fn expand<'a>(s: &'a str, env: &Env) -> Cow<'a, str> {
    if !s.contains("${") {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find('}') {
            // `\$\{([^}]+)\}`: the name is everything up to the FIRST `}` and must be non-empty.
            Some(end) if end > 0 => {
                let name = &after[..end];
                match resolve(name, env) {
                    Some(value) => out.push_str(&value),
                    None => out.push_str(&rest[start..=start + 2 + end]),
                }
                rest = &after[end + 1..];
            }
            // `${}` and an unterminated `${` are not matches: copy the opener and scan on.
            _ => {
                out.push_str("${");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

/// vars.go:39-60: the value for one variable name, or None when the name is unknown or its lookup failed.
fn resolve(name: &str, env: &Env) -> Option<String> {
    if let Some(var) = name.strip_prefix("env:") {
        return Some(env.var(var).unwrap_or_default());
    }
    match name {
        "workspaceFolder" | "cwd" => env.cwd().map(path_string),
        "userHome" => env.home().map(path_string),
        "appHome" => env.home().map(|h| path_string(&h.join(DOT_DIR))),
        "pathSeparator" | "/" => Some(std::path::MAIN_SEPARATOR.to_string()),
        _ => None,
    }
}

fn path_string(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use std::{
        borrow::Cow,
        path::{MAIN_SEPARATOR, PathBuf},
    };

    use super::{Env, HostDirs, expand};

    fn env() -> Env {
        Env::fixed(&[("VARS_TEST_TOKEN", "sekrit"), ("NESTED", "${cwd}")]).with_dirs(HostDirs {
            cwd: Some(PathBuf::from("/wd")),
            home: Some(PathBuf::from("/home/u")),
            ..HostDirs::default()
        })
    }

    // Go: internal/vars/vars_test.go:10
    #[test]
    fn test_expand() {
        let r = env();
        let sep = MAIN_SEPARATOR.to_string();
        let cases: Vec<(&str, String)> = vec![
            ("", String::new()),
            ("plain", "plain".to_owned()),
            ("${userHome}/x", "/home/u/x".to_owned()),
            // `appHome` is `home.join(".iota")` — a `join`, so the platform's separator.
            ("${appHome}/sys.md", format!("/home/u{sep}.iota/sys.md")),
            ("${cwd}", "/wd".to_owned()),
            ("${workspaceFolder}", "/wd".to_owned()),
            ("${env:VARS_TEST_TOKEN}", "sekrit".to_owned()),
            ("${env:VARS_TEST_UNSET}", String::new()),
            ("${unknownVar}", "${unknownVar}".to_owned()),
            ("a${/}b", format!("a{sep}b")),
            (
                "${userHome}${pathSeparator}deep",
                format!("/home/u{sep}deep"),
            ),
            ("${}", "${}".to_owned()),
            ("${", "${".to_owned()),
            ("$${cwd}", "$/wd".to_owned()),
            ("${env:NESTED}", "${cwd}".to_owned()),
            ("${CWD}", "${CWD}".to_owned()),
            ("x${cwd}y${unknown}z", "x/wdy${unknown}z".to_owned()),
        ];
        for (input, want) in cases {
            assert_eq!(expand(input, &r), want, "Expand({input:?})");
        }
        assert!(
            expand("${appHome}", &r).ends_with(".iota"),
            "appHome must end in .iota"
        );

        // The fast path borrows; a substitution owns.
        assert!(matches!(expand("plain", &r), Cow::Borrowed(_)));
        assert!(matches!(expand("", &r), Cow::Borrowed(_)));
        assert!(matches!(expand("${cwd}", &r), Cow::Owned(_)));

        // A failed lookup leaves the reference untouched; env: is always substituted.
        let bare = Env::default();
        assert_eq!(expand("${userHome}", &bare), "${userHome}");
        assert_eq!(expand("${appHome}", &bare), "${appHome}");
        assert_eq!(expand("${cwd}", &bare), "${cwd}");
        assert_eq!(expand("${env:X}", &bare), "");
        assert_eq!(expand("${/}", &bare), sep);
    }

    /// `var` reads an empty value as unset whatever the source; `fixed`, `from_fn` and `process` all answer
    /// through it. `process` reads the real environment (never mutated here).
    #[test]
    fn var_reads_empty_as_unset_from_every_source() {
        let fixed = Env::fixed(&[("K", "v"), ("EMPTY", "")]);
        assert_eq!(fixed.var("K"), Some("v".to_owned()));
        assert_eq!(fixed.var("EMPTY"), None);
        assert_eq!(fixed.var("MISSING"), None);
        assert!(fixed.cwd().is_none() && fixed.home().is_none());

        let closure = Env::from_fn(
            |name| (name == "A").then(|| "1".to_owned()),
            HostDirs::default(),
        );
        assert_eq!(closure.var("A"), Some("1".to_owned()));
        assert_eq!(closure.var("B"), None);

        let process = Env::process();
        let unset = "IOTA_CORE_TEST_DEFINITELY_UNSET_VARIABLE_42";
        assert_eq!(process.var(unset), None);
        assert_eq!(
            process.var("PATH"),
            std::env::var("PATH").ok().filter(|v| !v.is_empty())
        );
        assert_eq!(process.dirs, HostDirs::from_env());
        // A clone shares the lookup and carries the directories.
        let copy = process.clone();
        assert_eq!(copy.var("PATH"), process.var("PATH"));
        assert_eq!(copy.dirs, process.dirs);
        assert!(format!("{copy:?}").starts_with("Env { dirs: "));
    }

    /// A given variable is set on the child after the withheld ones are removed.
    #[test]
    fn a_child_is_given_what_it_is_given() {
        let env =
            super::ChildEnv::new(["HERDR_ENV".to_owned()]).giving("IOTA_SOCKET", "/run/1.sock");
        let mut cmd = tokio::process::Command::new("true");
        env.apply(&mut cmd);
        let envs: Vec<_> = cmd.as_std().get_envs().collect();
        assert!(envs.contains(&(std::ffi::OsStr::new("HERDR_ENV"), None)));
        assert!(envs.contains(&(
            std::ffi::OsStr::new("IOTA_SOCKET"),
            Some(std::ffi::OsStr::new("/run/1.sock"))
        )));
    }
}
