//! Lexical path helpers with Go `filepath` semantics (`Clean`, `Rel`, `ToSlash`); nothing here touches the
//! filesystem.

use std::{
    ffi::OsStr,
    path::{Component, MAIN_SEPARATOR, Path, PathBuf},
};

/// `filepath.Clean` (lexical): collapses `.`/`..`/repeated separators; never touches the filesystem; `""` → `"."`.
pub(crate) fn clean(p: &Path) -> PathBuf {
    let mut prefix: Option<&OsStr> = None;
    let mut rooted = false;
    let mut parts: Vec<&OsStr> = Vec::new();
    for c in p.components() {
        match c {
            Component::Prefix(pre) => prefix = Some(pre.as_os_str()),
            Component::RootDir => rooted = true,
            Component::CurDir => {}
            Component::ParentDir => match parts.last() {
                // Eliminate the inner `..` together with the non-`..` element that precedes it.
                Some(last) if *last != OsStr::new("..") => {
                    parts.pop();
                }
                // `..` at the root of a rooted path is dropped (`/..` → `/`).
                _ if rooted => {}
                // A leading `..` of a relative path is kept.
                _ => parts.push(OsStr::new("..")),
            },
            Component::Normal(s) => parts.push(s),
        }
    }
    let mut out = PathBuf::new();
    if let Some(pre) = prefix {
        out.push(pre);
    }
    if rooted {
        out.push(MAIN_SEPARATOR.to_string());
    }
    for part in parts {
        out.push(part);
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// `filepath.Rel(base, target)` (lexical, both cleaned); None when they share no common root or need `..` through
/// the root (an `Option`, not `Result<_, ()>`, which trips `clippy::result_unit_err`). Walks the cleaned paths'
/// components as `OsStr` (byte comparison), so byte-distinct non-UTF-8 components never collapse to equal.
pub(crate) fn rel(base: &Path, target: &Path) -> Option<PathBuf> {
    let base = clean(base);
    let targ = clean(target);
    if base == targ {
        return Some(PathBuf::from("."));
    }
    if base.has_root() != targ.has_root() {
        return None;
    }
    // Go empties a `"."` base; a `"."` target keeps its element and the final clean folds it away.
    let b = if base == Path::new(".") {
        Vec::new()
    } else {
        elements(&base)
    };
    let t = elements(&targ);
    // Position both at the first differing element.
    let common = b.iter().zip(&t).take_while(|(x, y)| x == y).count();
    if b.get(common).copied() == Some(OsStr::new("..")) {
        return None;
    }
    let ups = b.len() - common;
    let mut out = PathBuf::new();
    for _ in 0..ups {
        out.push("..");
    }
    for e in &t[common..] {
        out.push(*e);
    }
    if ups > 0 {
        // Go cleans the composed result (`../.` → `..`).
        out = clean(&out);
    }
    Some(out)
}

/// The elements Go's `Rel` loop walks over a CLEANED path: the root contributes a single empty element (`/` is
/// `[""]`, `/a` is `["", "a"]`, exactly Go's separator split), `.` stays an element, and every other component
/// is its `OsStr`.
fn elements(p: &Path) -> Vec<&OsStr> {
    p.components()
        .map(|c| match c {
            Component::RootDir => OsStr::new(""),
            c => c.as_os_str(),
        })
        .collect()
}

/// `filepath.ToSlash`.
pub(crate) fn to_slash(p: &Path) -> String {
    let s = p.to_string_lossy();
    if MAIN_SEPARATOR == '/' {
        s.into_owned()
    } else {
        s.replace(MAIN_SEPARATOR, "/")
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{clean, rel, to_slash};

    // Expectations generated with Go 1.27 `filepath.Clean`.
    #[test]
    fn paths_clean_table() {
        let cases = [
            ("", "."),
            (".", "."),
            ("..", ".."),
            ("/", "/"),
            ("//", "/"),
            ("a", "a"),
            ("a/", "a"),
            ("a//b", "a/b"),
            ("a/./b", "a/b"),
            ("a/../b", "b"),
            ("a/b/../..", "."),
            ("a/b/../../..", ".."),
            ("../a", "../a"),
            ("./a", "a"),
            ("/..", "/"),
            ("/../a", "/a"),
            ("/a/../..", "/"),
            ("a/b/./../c/", "a/c"),
            ("/a/b/c/../../d", "/a/d"),
            ("./", "."),
            ("../..", "../.."),
            ("a/..", "."),
            ("/a/", "/a"),
            ("a/b//c/./d/../e/", "a/b/c/e"),
        ];
        for (input, want) in cases {
            assert_eq!(
                clean(Path::new(input)),
                PathBuf::from(want),
                "Clean({input:?})"
            );
        }
    }

    // Expectations generated with Go 1.27 `filepath.Rel`.
    #[test]
    fn paths_rel_and_within() {
        let cases: [(&str, &str, Option<&str>); 27] = [
            ("/a", "/a/b", Some("b")),
            ("/a/b", "/a", Some("..")),
            ("/a", "/b", Some("../b")),
            ("/a/b", "/a/b", Some(".")),
            ("a", "a/b/c", Some("b/c")),
            ("a/b", "a/c", Some("../c")),
            (".", "a", Some("a")),
            ("a", ".", Some("..")),
            ("", "a", Some("a")),
            ("a", "", Some("..")),
            ("/", "/a", Some("a")),
            ("/a", "/", Some("..")),
            ("/a", "b", None),
            ("a", "/b", None),
            ("..", "a", None),
            ("../a", "b", None),
            ("a", "../b", Some("../../b")),
            ("/a/b/c", "/a/d", Some("../../d")),
            ("/x/y", "/x/y/../z", Some("../z")),
            ("a/b/", "a/b/c", Some("c")),
            ("/a/b", "/a/bc", Some("../bc")),
            ("/a", "/a/", Some(".")),
            ("/", "/", Some(".")),
            (".", ".", Some(".")),
            ("..", "..", Some(".")),
            ("../..", "../a", None),
            ("a/..", "b", Some("b")),
        ];
        for (base, target, want) in cases {
            assert_eq!(
                rel(Path::new(base), Path::new(target)),
                want.map(PathBuf::from),
                "Rel({base:?}, {target:?})"
            );
        }

        assert_eq!(to_slash(Path::new("a/b/c")), "a/b/c");
        assert_eq!(to_slash(Path::new("/")), "/");
    }

    // New (idiom-11): components compare as bytes — two byte-distinct non-UTF-8 components both decode lossily
    // to `a\u{FFFD}`, so the former string-based `rel` treated the sibling as inside the base.
    #[cfg(unix)]
    #[test]
    fn paths_rel_non_utf8_components() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

        let base = PathBuf::from(OsStr::from_bytes(b"/p/a\xff"));
        let inside = PathBuf::from(OsStr::from_bytes(b"/p/a\xff/c"));
        let sibling = PathBuf::from(OsStr::from_bytes(b"/p/a\xfe/c"));
        assert_eq!(rel(&base, &inside), Some(PathBuf::from("c")));
        assert_eq!(
            rel(&base, &sibling),
            Some(PathBuf::from(OsStr::from_bytes(b"../a\xfe/c")))
        );
    }
}
