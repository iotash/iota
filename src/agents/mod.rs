//! AGENTS.md overlay (internal/agents/agentsmd.go): project root discovery, the AGENTS.md chain from the root down
//! to the working directory, the one-shot `Overlay` (chain + skills catalog), and `compose_send_history`, which
//! the chat loop calls on every round. Skills live in `skills`.

pub mod skills;

use std::{
    borrow::Cow,
    io::Read as _,
    path::{Path, PathBuf},
};

use crate::app::paths;
use crate::provider::model::{Message, Role};
use crate::text::truncate_to_char_boundary;

use self::skills::{Skill, discover_skills, skill_roots, skills_catalog};

/// The instructions file looked up at every level of the chain.
pub(crate) const AGENTS_FILE_NAME: &str = "AGENTS.md";
/// Byte cap of the concatenated chain.
pub const AGENTS_CHAIN_CAP: usize = 32 * 1024;
/// Appended when the chain was cut at `AGENTS_CHAIN_CAP`.
pub const AGENTS_TRUNCATION_MARK: &str = "\n\n<!-- AGENTS.md chain truncated at 32 KiB -->";

/// Walk `cwd` upward; first dir where `symlink_metadata(dir/.git)` is Ok; at the root → `cwd` itself (never
/// canonicalised).
pub fn project_root(cwd: &Path) -> PathBuf {
    let mut dir = cwd;
    loop {
        // `symlink_metadata` is `os.Lstat`: a directory (normal checkout), a FILE (linked worktree) and even a
        // dangling symlink all count.
        if std::fs::symlink_metadata(dir.join(".git")).is_ok() {
            return dir.to_path_buf();
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => return cwd.to_path_buf(),
        }
    }
}

/// The concatenated AGENTS.md chain and the files that contributed to it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentsChain {
    /// Chain text (possibly truncated with `AGENTS_TRUNCATION_MARK`).
    pub content: String,
    /// The AGENTS.md files read, root first.
    pub files: Vec<PathBuf>,
}

/// Directories from `root` down to `cwd` (inclusive) whose AGENTS.md is consulted, root first.
pub(crate) fn agents_dirs(root: &Path, cwd: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![root.to_path_buf()];
    // A cwd outside the root (no relation, "." or a `..` path) degrades to the root alone.
    let Some(rel) = paths::rel(root, cwd) else {
        return dirs;
    };
    let rel = paths::to_slash(&rel);
    if rel == "." || rel == ".." || rel.starts_with("../") {
        return dirs;
    }
    let mut dir = root.to_path_buf();
    for part in rel.split('/') {
        dir.push(part);
        dirs.push(dir.clone());
    }
    dirs
}

/// Reads and joins the chain (agentsmd.go:110-129).
pub fn load_agents_chain(root: &Path, cwd: &Path) -> AgentsChain {
    let mut files = Vec::new();
    let mut parts: Vec<String> = Vec::new();
    for dir in agents_dirs(root, cwd) {
        let path = dir.join(AGENTS_FILE_NAME);
        // `os.Stat` + `Mode().IsRegular()`: a symlink to a regular file is followed and counts.
        match std::fs::metadata(&path) {
            Ok(m) if m.is_file() => {}
            _ => continue,
        }
        files.push(path.clone());
        // Never read more than the chain cap from a single file: the cap must bound memory BEFORE the read, or a
        // pathological multi-gigabyte AGENTS.md would be loaded wholesale. A read failure skips the CONTENT only
        // (the file stays in `files`, like Go's statAgentsChain).
        let Ok(file) = std::fs::File::open(&path) else {
            continue;
        };
        let cap = u64::try_from(AGENTS_CHAIN_CAP).unwrap_or(u64::MAX);
        let mut data = Vec::new();
        if file
            .take(cap.saturating_add(1))
            .read_to_end(&mut data)
            .is_err()
        {
            continue;
        }
        // Go keeps invalid UTF-8 (`string(data)`); Rust replaces it (DIVERGENCES D-16 spirit).
        parts.push(
            String::from_utf8_lossy(&data)
                .trim_end_matches('\n')
                .to_owned(),
        );
    }
    let mut content = parts.join("\n\n");
    if content.len() > AGENTS_CHAIN_CAP {
        content = truncate_to_char_boundary(&content, AGENTS_CHAIN_CAP).to_owned()
            + AGENTS_TRUNCATION_MARK;
    }
    AgentsChain { content, files }
}

/// Per-file modification stamps of a probed set; `None` where the filesystem reports none
/// (Go's zero `time.Time`, which compares equal to another zero).
type Stamps = Vec<Option<std::time::SystemTime>>;

/// The AGENTS.md chain's files and their mtimes (agentsmd.go:76-91 `statAgentsChain`).
///
/// Per-file stamps, never a collapsed newest: an mtime-preserving replace (`cp -p`,
/// `rsync -t`) of an OLDER chain file must still be detected.
fn stat_agents_chain(root: &Path, cwd: &Path) -> (Vec<PathBuf>, Stamps) {
    let mut files = Vec::new();
    let mut stamps = Vec::new();
    for dir in agents_dirs(root, cwd) {
        let path = dir.join(AGENTS_FILE_NAME);
        // The same predicate `load_agents_chain` uses, so the two file lists agree.
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        files.push(path);
        stamps.push(meta.modified().ok());
    }
    (files, stamps)
}

/// The skills roots and every discovered SKILL.md, with their mtimes (skills.go:175-195
/// `probeSkills`).
///
/// The roots catch additions and removals (a directory's mtime moves when an entry is
/// created or deleted); the per-skill files catch an in-place edit that leaves every
/// directory untouched. A skill whose file vanished is simply absent — the shrunken path
/// list is itself the change signal.
fn probe_skills(dirs: &[PathBuf], skills: &[Skill]) -> (Vec<PathBuf>, Stamps) {
    let mut paths = Vec::new();
    let mut stamps = Vec::new();
    for dir in dirs {
        let Ok(meta) = std::fs::metadata(dir) else {
            continue;
        };
        if !meta.is_dir() {
            continue;
        }
        paths.push(dir.clone());
        stamps.push(meta.modified().ok());
    }
    for skill in skills {
        let Ok(meta) = std::fs::metadata(&skill.path) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        paths.push(skill.path.clone());
        stamps.push(meta.modified().ok());
    }
    (paths, stamps)
}

/// The AGENTS.md + skills overlay, with the per-message freshness probe the interactive
/// loop runs before every send (`Overlay::refresh` — the D-27 lift, T-37).
pub struct Overlay {
    root: PathBuf,
    cwd: PathBuf,
    skill_dirs: Vec<PathBuf>,
    chain: AgentsChain,
    chain_stamps: Stamps,
    skills: Vec<Skill>,
    warnings: Vec<String>,
    skill_paths: Vec<PathBuf>,
    skill_stamps: Stamps,
}

impl Overlay {
    /// chain + `discover_skills(skill_roots(root, home))`.
    pub fn new(root: &Path, cwd: &Path, home: Option<&Path>) -> Overlay {
        Self::with_skill_dirs(root, cwd, skill_roots(root, home))
    }

    /// [`Overlay::new`] with EXPLICIT skill-discovery directories (agentsmd.go:178
    /// `newOverlayDirs`), so a test injects temp dirs and never scans the real home.
    pub fn with_skill_dirs(root: &Path, cwd: &Path, skill_dirs: Vec<PathBuf>) -> Overlay {
        let chain = load_agents_chain(root, cwd);
        let (_, chain_stamps) = stat_agents_chain(root, cwd);
        let (skills, warnings) = discover_skills(&skill_dirs);
        let (skill_paths, skill_stamps) = probe_skills(&skill_dirs, &skills);
        Overlay {
            root: root.to_path_buf(),
            cwd: cwd.to_path_buf(),
            skill_dirs,
            chain,
            chain_stamps,
            skills,
            warnings,
            skill_paths,
            skill_stamps,
        }
    }

    /// The discovered skills, in catalog order (the completion table's per-skill rows —
    /// completion.go:63-73; T3).
    pub fn skills(&self) -> &[Skill] {
        &self.skills
    }

    /// The directories discovery scanned, in precedence order — what the `/skills` view lists
    /// under `"No skills discovered. Searched:"` (agentmode.go:29-32, which re-derives them
    /// from the root). Reading them back from the overlay means the view names the roots that
    /// were actually searched, including the ones a test injected through
    /// [`Overlay::with_skill_dirs`].
    pub fn skill_dirs(&self) -> &[PathBuf] {
        &self.skill_dirs
    }

    /// Probes the AGENTS.md chain's and the skills roots' freshness and rebuilds whichever
    /// changed (agentsmd.go:196 `Refresh`; the D-27 lift, T-37). Returns
    /// `(agents_changed, skills_changed)` SEPARATELY so the interactive loop prints the
    /// matching reload notice — and only on a real change (run.go:965-976).
    pub fn refresh(&mut self) -> (bool, bool) {
        let mut agents_changed = false;
        let (files, stamps) = stat_agents_chain(&self.root, &self.cwd);
        if files != self.chain.files || stamps != self.chain_stamps {
            self.chain = load_agents_chain(&self.root, &self.cwd);
            self.chain_stamps = stamps;
            agents_changed = true;
        }
        let mut skills_changed = false;
        let (paths, stamps) = probe_skills(&self.skill_dirs, &self.skills);
        if paths != self.skill_paths || stamps != self.skill_stamps {
            let (skills, warnings) = discover_skills(&self.skill_dirs);
            self.skills = skills;
            self.warnings = warnings;
            let (paths, stamps) = probe_skills(&self.skill_dirs, &self.skills);
            self.skill_paths = paths;
            self.skill_stamps = stamps;
            skills_changed = true;
        }
        (agents_changed, skills_changed)
    }

    /// chain-only, catalog-only, or chain + "\n\n" + catalog; "" when both empty.
    pub fn content(&self) -> String {
        let catalog = skills_catalog(&self.skills);
        if self.chain.content.is_empty() {
            return catalog;
        }
        if catalog.is_empty() {
            return self.chain.content.clone();
        }
        format!("{}\n\n{catalog}", self.chain.content)
    }

    /// Skill discovery warnings.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Number of discovered skills.
    pub fn skill_count(&self) -> usize {
        self.skills.len()
    }

    /// Number of AGENTS.md files in the chain.
    pub fn file_count(&self) -> usize {
        self.chain.files.len()
    }

    /// Byte size of the assembled AGENTS.md chain ALONE (agentsmd.go:237 `ChainSize`) —
    /// the startup banner's `"(%d files, %.1f KB)"`; [`Overlay::content`] may additionally
    /// carry the skills catalog.
    pub fn chain_size(&self) -> usize {
        self.chain.content.len()
    }
}

/// "" → `Borrowed(history)`. `history[0]` is System → Owned clone with `content += "\n\n" + overlay`; else Owned
/// with a synthetic leading System message. `history` is never mutated (agentsmd.go `ComposeSendHistory`).
pub fn compose_send_history<'a>(history: &'a [Message], overlay: &str) -> Cow<'a, [Message]> {
    if overlay.is_empty() {
        return Cow::Borrowed(history);
    }
    if history.first().is_some_and(|m| m.role() == Role::System) {
        let mut out = history.to_vec();
        if let Some(system) = out.first_mut() {
            system.content.push_str("\n\n");
            system.content.push_str(overlay);
        }
        return Cow::Owned(out);
    }
    let mut out = Vec::with_capacity(history.len() + 1);
    out.push(Message::system(overlay));
    out.extend_from_slice(history);
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use std::{
        borrow::Cow,
        path::{Path, PathBuf},
    };

    use crate::provider::model::{Message, Role};

    use super::{Overlay, agents_dirs, compose_send_history};

    /// `os.Chtimes` twin: stamps `path` (file OR directory) with `secs` seconds since the
    /// epoch so a freshness assertion never depends on filesystem timestamp granularity.
    fn set_mtime(path: &Path, secs: u64) {
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
        let times = std::fs::FileTimes::new().set_modified(t).set_accessed(t);
        open_for_stamping(path).set_times(times).expect("set mtime");
    }

    /// A directory cannot be opened for writing on Unix; `futimens` accepts a read-only descriptor.
    #[cfg(not(windows))]
    fn open_for_stamping(path: &Path) -> std::fs::File {
        std::fs::File::options()
            .read(true)
            .open(path)
            .expect("open for stamping")
    }

    /// Windows asks for two things a read handle does not carry: `SetFileTime` needs
    /// `FILE_WRITE_ATTRIBUTES`, and a DIRECTORY opens at all only with `FILE_FLAG_BACKUP_SEMANTICS`
    /// — which is why `File::options().read(true)` fails here on both counts.
    #[cfg(windows)]
    fn open_for_stamping(path: &Path) -> std::fs::File {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        std::fs::File::options()
            .access_mode(FILE_WRITE_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .expect("open for stamping")
    }

    fn write_agents(dir: &Path, body: &str) -> PathBuf {
        std::fs::create_dir_all(dir).expect("mkdir");
        let path = dir.join(super::AGENTS_FILE_NAME);
        std::fs::write(&path, body).expect("write AGENTS.md");
        path
    }

    fn skill_md(name: &str, desc: &str) -> String {
        format!("---\nname: {name}\ndescription: {desc}\n---\n\nbody\n")
    }

    fn write_skill(root: &Path, name: &str, body: &str) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).expect("mkdir skill");
        let path = dir.join("SKILL.md");
        std::fs::write(&path, body).expect("write SKILL.md");
        path
    }

    // Go: internal/agents/agentsmd_test.go:126 TestOverlayFreshness — an unchanged chain
    // reports no change and recomposes to the same bytes; an mtime bump, a NEW chain file
    // and a REMOVED one each rebuild (T-37).
    #[test]
    fn test_overlay_freshness() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let sub = root.join("sub");
        std::fs::create_dir_all(&sub).expect("mkdir sub");
        let root_file = write_agents(root, "v1");
        set_mtime(&root_file, 1_000_000);

        // No skill dirs: the test never scans the developer's real home skills.
        let mut o = Overlay::with_skill_dirs(root, &sub, Vec::new());
        assert_eq!(o.content(), "v1");

        assert_eq!(
            o.refresh(),
            (false, false),
            "unchanged mtimes must report no change"
        );
        assert_eq!(o.content(), "v1", "a no-op refresh must not recompose");

        std::fs::write(&root_file, "v2").expect("rewrite");
        set_mtime(&root_file, 1_000_002);
        assert!(o.refresh().0, "an mtime change must be detected");
        assert_eq!(o.content(), "v2");

        write_agents(&sub, "SUB");
        assert!(o.refresh().0, "a new chain file must be detected");
        assert_eq!(o.content(), "v2\n\nSUB");
        assert_eq!(o.file_count(), 2);

        std::fs::remove_file(sub.join(super::AGENTS_FILE_NAME)).expect("rm");
        assert!(o.refresh().0, "a removed chain file must be detected");
        assert_eq!(o.content(), "v2");
        assert_eq!(o.file_count(), 1);
    }

    // Go: internal/agents/skills_test.go:238 TestOverlaySkillsFreshness — the skills half
    // moves on its own: a new skill bumps the ROOT's mtime, and the AGENTS.md chain
    // reports no change of its own (T-37).
    #[test]
    fn test_overlay_skills_freshness() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let skills_dir = root.join(".agents").join("skills");
        write_agents(root, "RULES");
        write_skill(&skills_dir, "alpha", &skill_md("alpha", "first skill"));
        set_mtime(&skills_dir, 1_000_000);

        let mut o = Overlay::with_skill_dirs(root, root, vec![skills_dir.clone()]);
        assert_eq!(o.skill_count(), 1);
        let content = o.content();
        assert!(
            content.starts_with("RULES"),
            "the overlay opens with the AGENTS.md chain: {content:?}"
        );
        assert!(
            content.contains("<name>alpha</name>"),
            "the overlay carries the skills catalog: {content:?}"
        );

        assert_eq!(
            o.refresh(),
            (false, false),
            "unchanged skills roots must report no change"
        );
        assert_eq!(o.content(), content, "a no-op refresh is byte-identical");

        write_skill(&skills_dir, "beta", &skill_md("beta", "second skill"));
        set_mtime(&skills_dir, 1_000_002);
        let (agents_changed, skills_changed) = o.refresh();
        assert!(!agents_changed, "the AGENTS.md chain did not change");
        assert!(skills_changed, "a changed skill set must be detected");
        assert_eq!(o.skill_count(), 2);
        assert!(o.content().contains("<name>beta</name>"));
    }

    // Go: internal/agents/skills_test.go:289 TestOverlayDetectsSkillEdit — an in-place
    // SKILL.md rewrite leaves every directory mtime untouched; the per-skill stamps are
    // what catches it (T-37).
    #[test]
    fn test_overlay_detects_skill_edit() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let skills_dir = root.join(".agents").join("skills");
        let path = write_skill(&skills_dir, "alpha", &skill_md("alpha", "old description"));

        let mut o = Overlay::with_skill_dirs(root, root, vec![skills_dir]);
        assert!(o.content().contains("old description"));

        std::fs::write(&path, skill_md("alpha", "new description")).expect("rewrite");
        set_mtime(&path, 4_000_000_000);

        let (_, skills_changed) = o.refresh();
        assert!(skills_changed, "an in-place SKILL.md edit must be detected");
        assert!(
            o.content().contains("new description"),
            "the catalog must recompose"
        );
    }

    // Go: internal/agents/agentsmd_test.go:187
    #[test]
    fn compose_send_history_borrowed_when_empty() {
        let history = vec![Message::system("sys"), Message::user("hi")];

        // Empty overlay: the exact same slice, no copy (agent off = today's bytes).
        let out = compose_send_history(&history, "");
        assert!(matches!(out, Cow::Borrowed(_)), "empty overlay must borrow");
        assert!(
            std::ptr::eq(out.as_ptr(), history.as_ptr()),
            "empty overlay should return the history slice itself"
        );
        assert_eq!(out.len(), history.len());

        // Overlay appends to the existing system message on a copy.
        let out = compose_send_history(&history, "OVERLAY");
        assert!(matches!(out, Cow::Owned(_)));
        assert_eq!(out[0].content, "sys\n\nOVERLAY");
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].content, "hi");
        assert_eq!(
            history[0].content, "sys",
            "overlay leaked into clean history"
        );

        // No user system prompt: a synthetic system message is inserted.
        let no_sys = vec![Message::user("hi")];
        let out = compose_send_history(&no_sys, "OVERLAY");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].role(), Role::System);
        assert_eq!(out[0].content, "OVERLAY");
        assert_eq!(out[1].content, "hi");
        assert_eq!(no_sys.len(), 1);
        assert_eq!(no_sys[0].role(), Role::User);
    }

    // New: the lexical half of the chain search (agentsmd.go:59-71) — cumulative components root→cwd, and the
    // degenerate cases that collapse to the root alone.
    #[test]
    fn agents_dirs_walks_root_to_cwd() {
        let root = Path::new("/p");
        assert_eq!(
            agents_dirs(root, Path::new("/p/a/b")),
            vec![
                PathBuf::from("/p"),
                PathBuf::from("/p/a"),
                PathBuf::from("/p/a/b"),
            ]
        );
        // cwd == root, cwd above the root, a sibling, and an unrelated (relative) cwd: root alone.
        for cwd in ["/p", "/", "/q/x", "rel"] {
            assert_eq!(
                agents_dirs(root, Path::new(cwd)),
                vec![PathBuf::from("/p")],
                "cwd {cwd}"
            );
        }
    }
}
