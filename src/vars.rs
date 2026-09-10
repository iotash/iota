//! `${var}` expansion (internal/vars/vars.go) and the process-environment seams (`VarResolver`, `EnvSource`).

use std::{
    borrow::Cow,
    path::{Path, PathBuf},
};

use crate::app::DOT_DIR;

/// What `expand` can look up: environment variables, the working directory and the home directory.
pub trait VarResolver: Send + Sync {
    /// The environment variable `name`, if set.
    fn env_var(&self, name: &str) -> Option<String>;
    /// The working directory, if known.
    fn cwd(&self) -> Option<PathBuf>;
    /// The home directory, if known.
    fn home(&self) -> Option<PathBuf>;
}

/// internal/vars/vars.go: single pass over `${name}` (no rescans, `${}` is not a match, names are case-sensitive).
/// `env:NAME` → `env_var(NAME).unwrap_or_default()` (always substituted); `workspaceFolder`|`cwd` → `cwd()`;
/// `userHome` → `home()`; `appHome` → `home()/.iota`; `pathSeparator`|`"/"` → `MAIN_SEPARATOR`; unknown or failed
/// lookup → original `${…}` text.
/// Fast path: input without `"${"` is returned Borrowed.
pub(crate) fn expand<'a>(s: &'a str, r: &dyn VarResolver) -> Cow<'a, str> {
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
                match resolve(name, r) {
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
fn resolve(name: &str, r: &dyn VarResolver) -> Option<String> {
    if let Some(var) = name.strip_prefix("env:") {
        return Some(r.env_var(var).unwrap_or_default());
    }
    match name {
        "workspaceFolder" | "cwd" => r.cwd().map(|d| path_string(&d)),
        "userHome" => r.home().map(|h| path_string(&h)),
        "appHome" => r.home().map(|h| path_string(&h.join(DOT_DIR))),
        "pathSeparator" | "/" => Some(std::path::MAIN_SEPARATOR.to_string()),
        _ => None,
    }
}

fn path_string(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Process-environment seam used by CLI resolution and `-l` (key lookup). Lives here,
/// not in the `iota` crate, so `crate::testing` fixtures and every crate's tests can build one.
pub trait EnvSource: Send + Sync {
    /// The variable `name`, or None when unset (or empty).
    fn var(&self, name: &str) -> Option<String>;
}

/// `std::env::var(name).ok().filter(|v| !v.is_empty())` (Go `os.Getenv(..) != ""`).
#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessEnv;

impl EnvSource for ProcessEnv {
    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|v| !v.is_empty())
    }
}

impl<F: Fn(&str) -> Option<String> + Send + Sync> EnvSource for F {
    fn var(&self, name: &str) -> Option<String> {
        self(name)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        borrow::Cow,
        collections::HashMap,
        path::{MAIN_SEPARATOR, PathBuf},
    };

    use super::{EnvSource, ProcessEnv, VarResolver, expand};

    /// A map-backed resolver (the in-crate stand-in for `crate::testing::map_resolver`, which needs the
    /// `testing` feature).
    struct MapResolver {
        vars: HashMap<String, String>,
        cwd: Option<PathBuf>,
        home: Option<PathBuf>,
    }

    impl VarResolver for MapResolver {
        fn env_var(&self, name: &str) -> Option<String> {
            self.vars.get(name).cloned()
        }

        fn cwd(&self) -> Option<PathBuf> {
            self.cwd.clone()
        }

        fn home(&self) -> Option<PathBuf> {
            self.home.clone()
        }
    }

    fn resolver() -> MapResolver {
        MapResolver {
            vars: HashMap::from([
                ("VARS_TEST_TOKEN".to_owned(), "sekrit".to_owned()),
                ("NESTED".to_owned(), "${cwd}".to_owned()),
            ]),
            cwd: Some(PathBuf::from("/wd")),
            home: Some(PathBuf::from("/home/u")),
        }
    }

    // Go: internal/vars/vars_test.go:10
    #[test]
    fn test_expand() {
        let r = resolver();
        let sep = MAIN_SEPARATOR.to_string();
        let cases: Vec<(&str, String)> = vec![
            ("", String::new()),
            ("plain", "plain".to_owned()),
            ("${userHome}/x", "/home/u/x".to_owned()),
            ("${appHome}/sys.md", "/home/u/.iota/sys.md".to_owned()),
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
        let bare = MapResolver {
            vars: HashMap::new(),
            cwd: None,
            home: None,
        };
        assert_eq!(expand("${userHome}", &bare), "${userHome}");
        assert_eq!(expand("${appHome}", &bare), "${appHome}");
        assert_eq!(expand("${cwd}", &bare), "${cwd}");
        assert_eq!(expand("${env:X}", &bare), "");
        assert_eq!(expand("${/}", &bare), sep);
    }

    #[test]
    fn env_source_closure_and_process_env() {
        let closure = |name: &str| (name == "A").then(|| "1".to_owned());
        let src: &dyn EnvSource = &closure;
        assert_eq!(src.var("A"), Some("1".to_owned()));
        assert_eq!(src.var("B"), None);

        // ProcessEnv filters empty values and reads the real environment (never mutated here).
        let unset = "IOTA_CORE_TEST_DEFINITELY_UNSET_VARIABLE_42";
        assert_eq!(ProcessEnv.var(unset), None);
        assert_eq!(
            ProcessEnv.var("PATH"),
            std::env::var("PATH").ok().filter(|v| !v.is_empty())
        );
        let boxed: Box<dyn EnvSource> = Box::new(ProcessEnv);
        assert_eq!(boxed.var(unset), None);
    }
}
