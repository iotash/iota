//! Skills (internal/agents/skills.go): discovery under the skill roots, `SKILL.md` frontmatter validation, and
//! the `<available_skills>` catalog appended to the system prompt.

use std::{
    collections::HashSet,
    ffi::OsString,
    fmt::Write as _,
    io::Read as _,
    path::{Path, PathBuf},
};

use crate::app::DOT_DIR;
use crate::app::paths;

/// The file every skill directory must contain.
pub const SKILL_FILE_NAME: &str = "SKILL.md";
/// Maximum length of a skill name.
pub const SKILL_NAME_MAX_LEN: usize = 64;
/// Maximum length of a skill description.
pub const SKILL_DESC_MAX_LEN: usize = 1024;
/// Byte cap of the catalog.
pub const SKILLS_CATALOG_CAP: usize = 32 * 1024;
/// Byte cap of a single `SKILL.md` discovery read: validation needs only the frontmatter (`name` ≤ 64 bytes,
/// `description` ≤ 1024 characters), and `load_skill` re-reads the body under its own cap. Go reads the whole
/// file (`os.ReadFile`, skills.go); bounding the read means a frontmatter that closes beyond the cap is
/// rejected as unterminated instead of loading a pathological file wholesale.
pub const SKILL_DISCOVERY_CAP: usize = 32 * 1024;
/// The catalog's leading instruction.
pub const SKILLS_CATALOG_INSTRUCTION: &str = "To use a skill, call the load_skill tool with the skill's name and follow the instructions it returns; read files the skill references by calling load_skill again with the \"file\" argument, and run its bundled scripts with the shell tool.";

/// One discovered skill.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skill {
    /// Frontmatter `name` (equals the directory name).
    pub name: String,
    /// Frontmatter `description`.
    pub description: String,
    /// Absolute path of the `SKILL.md`.
    pub path: PathBuf,
}

impl Skill {
    /// The skill's directory (parent of `path`).
    pub fn dir(&self) -> &Path {
        parent_dir(&self.path)
    }
}

/// `filepath.Dir`: a path without a parent component is `"."`, never `""`.
fn parent_dir(path: &Path) -> &Path {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    }
}

/// Where a skill was discovered, matched against the discovery roots in precedence order
/// (skills.go:199-212 `SkillSourceTag`): `"project"` under `<root>/.agents/skills`,
/// `"user"` under `<home>/.iota/skills`, `"user (.agents)"` under `<home>/.agents/skills`,
/// and otherwise the skill's own directory — a skill reached through an injected root has
/// no tag to give but its location.
///
/// `home` is INJECTED (the D-26 rule): this module never reads the environment, so a test
/// pins the user-level roots without touching `$HOME`. Go compares a string prefix plus the
/// separator; [`Path::starts_with`] matches whole components, the same rule for the
/// `<root>/<name>/SKILL.md` shape every discovered skill has.
pub fn skill_source_tag(path: &Path, root: &Path, home: Option<&Path>) -> String {
    if path.starts_with(root.join(".agents").join("skills")) {
        return "project".to_owned();
    }
    if let Some(home) = home {
        if path.starts_with(home.join(DOT_DIR).join("skills")) {
            return "user".to_owned();
        }
        if path.starts_with(home.join(".agents").join("skills")) {
            return "user (.agents)".to_owned();
        }
    }
    parent_dir(path).display().to_string()
}

/// `[root/.agents/skills, home/.iota/skills (if home), home/.agents/skills (if home)]`.
pub(crate) fn skill_roots(root: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = vec![root.join(".agents").join("skills")];
    if let Some(home) = home {
        dirs.push(home.join(DOT_DIR).join("skills"));
        dirs.push(home.join(".agents").join("skills"));
    }
    dirs
}

/// skills.go:74-104; warnings `skill {path}: {err} (skipped)`; sorted `read_dir`; first name wins.
pub fn discover_skills(dirs: &[PathBuf]) -> (Vec<Skill>, Vec<String>) {
    let mut skills = Vec::new();
    let mut warnings = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue; // absent root: nothing to discover
        };
        // `os.ReadDir` yields entries sorted by file name; `read_dir` does not, and the rendered catalog must be
        // byte-stable across turns.
        let mut names: Vec<OsString> = entries.flatten().map(|e| e.file_name()).collect();
        names.sort();
        for name in names {
            let sub = dir.join(&name);
            // `metadata` follows symlinks, like `os.Stat`: plain files and dangling links are not candidates.
            match std::fs::metadata(&sub) {
                Ok(m) if m.is_dir() => {}
                _ => continue,
            }
            let path = sub.join(SKILL_FILE_NAME);
            // Never read more than the discovery cap from a single file: the cap must bound memory BEFORE the
            // read (the AGENTS.md-chain rule), and discovery re-runs on every `load_skill` resolve.
            let Ok(file) = std::fs::File::open(&path) else {
                continue; // a directory without SKILL.md is not a skill
            };
            let cap = u64::try_from(SKILL_DISCOVERY_CAP).unwrap_or(u64::MAX);
            let mut data = Vec::new();
            if file.take(cap).read_to_end(&mut data).is_err() {
                continue;
            }
            match parse_skill(&data, &name.to_string_lossy(), &path) {
                Ok(sk) => {
                    if seen.insert(sk.name.clone()) {
                        skills.push(sk); // otherwise shadowed by a higher-precedence directory
                    }
                }
                Err(e) => warnings.push(format!("skill {}: {e} (skipped)", path.display())),
            }
        }
    }
    (skills, warnings)
}

/// Validates a `SKILL.md` (frontmatter present, `name` valid and equal to `dir_name`, `description` present and
/// within the cap). Scalars are stringified (`name: 123` → `"123"`) via `serde_norway::Value`; a non-scalar value
/// reads as absent (yaml.v3 rejects it with a decode error no port could reproduce byte-for-byte).
pub fn parse_skill(data: &[u8], dir_name: &str, path: &Path) -> Result<Skill, SkillError> {
    let (front, _body) = split_frontmatter(data)?;
    let doc: serde_norway::Value =
        serde_norway::from_str(&front).map_err(|e| SkillError::InvalidYaml(e.to_string()))?;
    let name = scalar_field(&doc, "name");
    let description = scalar_field(&doc, "description");
    if name.is_empty() {
        return Err(SkillError::MissingName);
    }
    // Go caps the NAME in bytes (`len`) and the DESCRIPTION in runes (`utf8.RuneCountInString`).
    if name.len() > SKILL_NAME_MAX_LEN {
        return Err(SkillError::NameTooLong);
    }
    if !valid_skill_name(&name) {
        return Err(SkillError::InvalidName(name));
    }
    if name != dir_name {
        return Err(SkillError::NameMismatch(name, dir_name.to_owned()));
    }
    if description.is_empty() {
        return Err(SkillError::MissingDescription);
    }
    if description.chars().count() > SKILL_DESC_MAX_LEN {
        return Err(SkillError::DescriptionTooLong);
    }
    // `filepath.Abs` = Clean(Join(wd, path)); lexical, never resolving symlinks.
    let abs = std::path::absolute(path).map_or_else(|_| path.to_path_buf(), |p| paths::clean(&p));
    Ok(Skill {
        name,
        description,
        path: abs,
    })
}

/// yaml.v3 assigns any scalar to a string field (`name: 123` → `"123"`, `description: true` → `"true"`); a missing
/// key, an explicit null and a collection all read as absent.
fn scalar_field(doc: &serde_norway::Value, key: &str) -> String {
    match doc.get(key) {
        Some(serde_norway::Value::String(s)) => s.clone(),
        Some(serde_norway::Value::Number(n)) => n.to_string(),
        Some(serde_norway::Value::Bool(b)) => b.to_string(),
        _ => String::new(),
    }
}

/// The instructions after the frontmatter.
pub fn skill_body(data: &[u8]) -> Result<String, SkillError> {
    let (_front, body) = split_frontmatter(data)?;
    Ok(body.trim_start_matches('\n').to_owned())
}

/// Splits `data` into (front, body).
pub(crate) fn split_frontmatter(data: &[u8]) -> Result<(String, String), SkillError> {
    const DELIM: &str = "---";
    let text = String::from_utf8_lossy(data).replace("\r\n", "\n");
    if !text.starts_with("---\n") && text != DELIM {
        return Err(SkillError::MissingFrontmatter);
    }
    let rest = text.strip_prefix(DELIM).unwrap_or(&text);
    let rest = rest.strip_prefix('\n').unwrap_or(rest);
    if let Some(i) = rest.find("\n---\n") {
        return Ok((rest[..i].to_owned(), rest[i + DELIM.len() + 2..].to_owned()));
    }
    if let Some(front) = rest.strip_suffix("\n---") {
        return Ok((front.to_owned(), String::new()));
    }
    Err(SkillError::Unterminated)
}

/// `SKILLS_CATALOG_INSTRUCTION` + `<available_skills>` block, capped at `SKILLS_CATALOG_CAP` with an omission note.
pub fn skills_catalog(skills: &[Skill]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let mut b = String::with_capacity(SKILLS_CATALOG_INSTRUCTION.len() + 256);
    b.push_str(SKILLS_CATALOG_INSTRUCTION);
    b.push_str("\n\n<available_skills>\n");
    let mut omitted = 0usize;
    for sk in skills {
        // Every field is XML-escaped: descriptions come from arbitrary skill files and land inside the system
        // prompt — unescaped angle brackets could close the block and plant text outside it (prompt injection).
        let entry = format!(
            "<skill>\n<name>{}</name>\n<description>{}</description>\n</skill>\n",
            xml_escape(&sk.name),
            xml_escape(&sk.description)
        );
        if b.len() + entry.len() > SKILLS_CATALOG_CAP {
            omitted += 1;
            continue; // a later, shorter entry may still fit
        }
        b.push_str(&entry);
    }
    if omitted > 0 {
        let _ = writeln!(
            b,
            "<note>{omitted} more skill(s) omitted: catalog size cap reached</note>"
        );
    }
    b.push_str("</available_skills>");
    b
}

/// Single pass: `&` `<` `>`.
pub(crate) fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

/// `^[a-z0-9]+(-[a-z0-9]+)*$` hand-written.
pub(crate) fn valid_skill_name(s: &str) -> bool {
    !s.is_empty()
        && s.split('-').all(|seg| {
            !seg.is_empty()
                && seg
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

/// Why a `SKILL.md` was rejected (skills.go texts).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SkillError {
    /// The file does not start with `---`.
    #[error("missing YAML frontmatter (file must start with ---)")]
    MissingFrontmatter,
    /// No closing `---`.
    #[error("unterminated YAML frontmatter (missing closing ---)")]
    Unterminated,
    /// The frontmatter is not valid YAML.
    #[error("invalid frontmatter YAML: {0}")]
    InvalidYaml(String),
    /// No `name` field.
    #[error("frontmatter is missing required field \"name\"")]
    MissingName,
    /// `name` longer than 64 characters.
    #[error("name exceeds 64 characters")]
    NameTooLong,
    /// `name` is not lowercase alphanumerics separated by single hyphens.
    #[error("invalid name {0:?} (want lowercase alphanumerics separated by single hyphens)")]
    InvalidName(String),
    /// `name` differs from the directory name.
    #[error("name {0:?} does not match directory name {1:?}")]
    NameMismatch(String, String),
    /// No `description` field.
    #[error("frontmatter is missing required field \"description\"")]
    MissingDescription,
    /// `description` longer than 1024 characters.
    #[error("description exceeds 1024 characters")]
    DescriptionTooLong,
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{Skill, skill_roots, skill_source_tag, valid_skill_name, xml_escape};

    // New: the name grammar `^[a-z0-9]+(-[a-z0-9]+)*$` without a regex engine (skills.go:28).
    #[test]
    fn valid_skill_name_grammar() {
        for ok in ["a", "0", "my-skill", "a1-b2-c3", "skill-01"] {
            assert!(valid_skill_name(ok), "{ok} should be valid");
        }
        for bad in [
            "", "-a", "a-", "a--b", "A", "aB", "a_b", "a b", "a.b", "-", "ä",
        ] {
            assert!(!valid_skill_name(bad), "{bad} should be invalid");
        }
    }

    // New: the single-pass escaper handles `&` `<` `>` and leaves quotes alone (skills.go:258).
    #[test]
    fn xml_escape_pairs() {
        assert_eq!(xml_escape("a&b<c>d"), "a&amp;b&lt;c&gt;d");
        assert_eq!(xml_escape("\"q\" 'q'"), "\"q\" 'q'");
        assert_eq!(xml_escape("&amp;"), "&amp;amp;");
        assert_eq!(xml_escape(""), "");
    }

    // New: the roots' precedence and the home-less shape (skills.go:49-67).
    #[test]
    fn skill_roots_precedence() {
        let root = Path::new("/proj");
        assert_eq!(
            skill_roots(root, Some(Path::new("/home/u"))),
            vec![
                PathBuf::from("/proj/.agents/skills"),
                PathBuf::from("/home/u/.iota/skills"),
                PathBuf::from("/home/u/.agents/skills"),
            ]
        );
        assert_eq!(
            skill_roots(root, None),
            vec![PathBuf::from("/proj/.agents/skills")]
        );
    }

    // New: `Skill::dir` is `filepath.Dir` — "." for a bare file name.
    #[test]
    fn skill_dir_is_the_parent() {
        let sk = Skill {
            name: "a".to_owned(),
            description: "d".to_owned(),
            path: PathBuf::from("/abs/a/SKILL.md"),
        };
        assert_eq!(sk.dir(), Path::new("/abs/a"));
        let bare = Skill {
            path: PathBuf::from("SKILL.md"),
            ..sk
        };
        assert_eq!(bare.dir(), Path::new("."));
    }

    // New (T3): the four `SkillSourceTag` outcomes with an INJECTED home — the `/skills` view's
    // `[tag]` column (skills.go:199-212). A path under none of the roots names its own directory,
    // and without a home the two user rungs simply do not apply.
    #[test]
    fn skill_source_tag_names_the_root_it_came_from() {
        let root = Path::new("/proj");
        let home = Path::new("/home/u");
        assert_eq!(
            skill_source_tag(
                Path::new("/proj/.agents/skills/commit-helper/SKILL.md"),
                root,
                Some(home)
            ),
            "project"
        );
        assert_eq!(
            skill_source_tag(
                Path::new("/home/u/.iota/skills/a/SKILL.md"),
                root,
                Some(home)
            ),
            "user"
        );
        assert_eq!(
            skill_source_tag(
                Path::new("/home/u/.agents/skills/a/SKILL.md"),
                root,
                Some(home)
            ),
            "user (.agents)"
        );
        // Neither the project nor a user root: the directory is all there is to say.
        assert_eq!(
            skill_source_tag(Path::new("/opt/share/skills/a/SKILL.md"), root, Some(home)),
            "/opt/share/skills/a"
        );
        // Without a home the user rungs cannot match, so the same path falls through.
        assert_eq!(
            skill_source_tag(Path::new("/home/u/.iota/skills/a/SKILL.md"), root, None),
            "/home/u/.iota/skills/a"
        );
        // The project rung wins over a home that contains the project.
        assert_eq!(
            skill_source_tag(
                Path::new("/home/u/proj/.agents/skills/a/SKILL.md"),
                Path::new("/home/u/proj"),
                Some(home)
            ),
            "project"
        );
    }
}
