//! `/skills [name [instructions]]` (chat/agentmode.go, run.go:923-937), registered in agent mode:
//! bare `/skills` opens a read-only `Skills` view of the discovered catalog with each skill's source
//! tag; `/skills <name>` expands the skill's `SKILL.md` into a `<skill …>` block and sends it (with
//! the optional instructions appended) through the message path — the echo shows the typed line.
//!
//! **Running a skill is an input EXPANSION, not a dispatch.** The skill's instructions BECOME the
//! message being sent, so the turn below carries them like any other typed line (mid-turn steering
//! included) and the model side needs no new concept. The transcript still echoes what the user
//! typed: a whole `SKILL.md` in the `❯` block would bury the screen. That split — echo shows
//! `input.display`, history and the compact offer carry the expansion — is why the run loop keeps a
//! `content` distinct from the line ([`crate::repl::run`]'s dispatch chain).
//!
//! The tag and the stated directory are what let the model resolve the skill's relative references:
//! the same two facts `load_skill` hands it on activation (tool/agent.go:117-133).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::agents::skills::{Skill, skill_body, skill_source_tag};
use crate::repl::run::Repl;
use crate::repl::styles::{bold, dim, red, yellow};
use crate::text::go_quote;
use crate::ui::facade::ViewSpec;

/// The view's rows (agentmode.go:27-49).
///
/// An empty catalog is not an error, it is a question — so the rows say WHERE discovery looked
/// (`dirs`, the overlay's own roots) rather than only that it found nothing. A populated catalog is
/// two rows per skill: the bold name with its yellow `[source]` tag, then the dim description.
/// Invalid skills are never silently dropped: they follow under `Skipped (invalid):` with the
/// reason discovery recorded.
pub(crate) fn skills_status_lines(
    sks: &[Skill],
    warnings: &[String],
    dirs: &[PathBuf],
    root: &Path,
    home: Option<&Path>,
) -> Vec<String> {
    if sks.is_empty() && warnings.is_empty() {
        let mut lines = vec![dim("No skills discovered. Searched:")];
        lines.extend(dirs.iter().map(|d| dim(&format!("  {}", d.display()))));
        return lines;
    }
    let mut lines = Vec::with_capacity(sks.len() * 2 + warnings.len() + 2);
    for sk in sks {
        lines.push(format!(
            "{}  {}",
            bold(&sk.name),
            yellow(&format!("[{}]", skill_source_tag(&sk.path, root, home)))
        ));
        lines.push(dim(&format!("  {}", sk.description)));
    }
    if !warnings.is_empty() {
        lines.push(String::new());
        lines.push(red("Skipped (invalid):"));
        lines.extend(warnings.iter().map(|w| dim(&format!("  {w}"))));
    }
    lines
}

/// The skill named `name`, case-folded (agentmode.go:64-71).
///
/// The user is TYPING this name, not reading it back from a prompt, so matching tolerates their
/// casing. Go uses `strings.EqualFold` (Unicode simple case folding); the closest portable stand-in
/// is a `to_lowercase` comparison, which agrees on every name the grammar allows (`^[a-z0-9]+(-[a-z0-9]+)*$`,
/// skills.go:28 — ASCII only) and differs from `EqualFold` only for characters no skill name may carry.
pub(crate) fn find_skill<'a>(sks: &'a [Skill], name: &str) -> Option<&'a Skill> {
    let want = name.to_lowercase();
    sks.iter().find(|sk| sk.name.to_lowercase() == want)
}

/// `(expanded text, skill name)` for `arg = "<name> [instructions]"`, or the error text
/// (agentmode.go:82-111).
///
/// A name the catalog does not have is an error, not a message: sending the literal `/skills nope`
/// to the model would waste a turn on a typo. A body whose frontmatter no longer parses is an error
/// too — discovery validated the file, so reaching here means it changed since, and half a manifest
/// is worse than nothing.
pub(crate) fn expand_skill(sks: &[Skill], arg: &str) -> Result<(String, String), String> {
    let arg = arg.trim();
    if arg.is_empty() {
        return Err("usage: /skills <name> [instructions]".to_owned());
    }
    // `strings.Cut(arg, " ")`: the first space splits the name from whatever else was typed.
    let (name, extra) = arg.split_once(' ').unwrap_or((arg, ""));
    let Some(sk) = find_skill(sks, name) else {
        return Err(format!(
            "no skill named {} — /skills lists what is available",
            go_quote(name)
        ));
    };
    let data = std::fs::read(&sk.path)
        .map_err(|e| format!("cannot read skill {}: {e}", go_quote(&sk.name)))?;
    let body = skill_body(&data).map_err(|e| format!("skill {}: {e}", go_quote(&sk.name)))?;

    let mut out = format!(
        "<skill name={} location={}>\n",
        go_quote(&sk.name),
        go_quote(&sk.path.to_string_lossy())
    );
    // `write!` into a String is infallible; the Result is discarded rather than unwrapped.
    let _ = write!(
        out,
        "References are relative to {}.\n\n",
        sk.dir().display()
    );
    out.push_str(body.trim());
    out.push_str("\n</skill>");
    // The user's own text follows the block, never inside it: the model reads the skill first and
    // the instruction second.
    let extra = extra.trim();
    if !extra.is_empty() {
        out.push_str("\n\n");
        out.push_str(extra);
    }
    Ok((out, sk.name.clone()))
}

/// `"{n} B"` | `"{:.1} KB"` (agentmode.go:114-119).
///
/// One of THREE byte renderers in the Go tree, deliberately not merged: `humanSize`
/// ([`crate::repl::commands::file::human_size`], which also has an MB rung) labels an attachment,
/// `formatByteSize` narrates upload progress, and this one sizes the skill-loaded notice.
pub(crate) fn byte_size(n: usize) -> String {
    if n < 1024 {
        return format!("{n} B");
    }
    #[allow(clippy::cast_precision_loss)] // display only; the ratio is what is shown
    let f = n as f64;
    format!("{:.1} KB", f / 1024.0)
}

/// What the arm decided (run.go:923-937).
pub(crate) enum SkillsOutcome {
    /// Back to the prompt.
    Continue,
    /// Send this expansion through the message path.
    Send(String),
}

/// The `/skills` arm (run.go:923-937).
///
/// The catalog is re-probed FIRST, so a skill written since the last turn is listed and runnable
/// now. The refresh's change flags are DISCARDED here: the reload notices belong to the message
/// path (run.go:965-976), and a viewer that narrated them would print them twice for one change.
pub(crate) async fn cmd_skills(repl: &mut Repl, arg: &str) -> SkillsOutcome {
    // `/skills` is dispatched only in agent mode, where the overlay exists.
    let Some(overlay) = repl.overlay.as_mut() else {
        return SkillsOutcome::Continue;
    };
    let _ = overlay.refresh();
    if arg.is_empty() {
        let lines = skills_status_lines(
            overlay.skills(),
            overlay.warnings(),
            overlay.skill_dirs(),
            repl.agent.root.as_path(),
            repl.agent.home.as_deref(),
        );
        // A viewer, not a picker: the result is discarded like every other `/…` view.
        let _ = repl
            .ui
            .view(
                &repl.cancel,
                ViewSpec {
                    title: "Skills".to_owned(),
                    lines,
                    height: 0,
                },
            )
            .await;
        return SkillsOutcome::Continue;
    }
    match expand_skill(overlay.skills(), arg) {
        // `printErr("%v", err)`: the error text verbatim, no `Error: ` prefix.
        Err(text) => {
            repl.tr.error(&text);
            SkillsOutcome::Continue
        }
        Ok((expanded, name)) => {
            repl.tr.notice(&format!(
                "Skill {name} loaded ({}).",
                byte_size(expanded.len())
            ));
            SkillsOutcome::Send(expanded)
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    //! The chat-side `/skills` helpers (`chat/skills_test.go`, `chat/skillcmd_test.go`).
    //! Discovery, catalog rendering and overlay freshness are tested beside `agents::skills`.

    use std::path::{Path, PathBuf};

    use pretty_assertions::assert_eq;

    use super::{byte_size, expand_skill, find_skill, skills_status_lines};
    use crate::agents::skills::{SKILL_FILE_NAME, Skill};
    use crate::text::ansi::strip_sgr;

    /// Go: `chat/skillcmd_test.go:12` `writeSkill` — a real `SKILL.md` under `<dir>/<name>/`, and the
    /// [`Skill`] discovery would have produced for it.
    fn write_skill(dir: &Path, name: &str, body: &str) -> Skill {
        let sd = dir.join(name);
        std::fs::create_dir_all(&sd).expect("skill dir");
        let path = sd.join(SKILL_FILE_NAME);
        let content = format!("---\nname: {name}\ndescription: does {name}\n---\n\n{body}\n");
        std::fs::write(&path, content).expect("write SKILL.md");
        Skill {
            name: name.to_owned(),
            description: format!("does {name}"),
            path,
        }
    }

    // Go: chat/skills_test.go:13 TestSkillsStatusLines — each discovered skill with its source tag
    // and description, then the invalid-skill warnings; an empty catalog explains where it looked.
    #[test]
    fn test_skills_status_lines() {
        let root = Path::new("/tmp/proj");
        let dirs = crate::agents::skills::skill_roots(root, None);
        let sks = [Skill {
            name: "commit-helper".to_owned(),
            description: "Write commit messages".to_owned(),
            path: PathBuf::from("/tmp/proj/.agents/skills/commit-helper/SKILL.md"),
        }];
        let warnings = ["bad-skill: name does not match directory".to_owned()];
        let lines = skills_status_lines(&sks, &warnings, &dirs, root, None);
        let joined = strip_sgr(&lines.join("\n"));
        for want in [
            "commit-helper",
            "[project]",
            "Write commit messages",
            "Skipped (invalid)",
            "bad-skill",
        ] {
            assert!(joined.contains(want), "missing {want:?} in:\n{joined}");
        }
        // The exact shape: name + tag, description, blank, headline, warning.
        assert_eq!(
            joined,
            "commit-helper  [project]\n  Write commit messages\n\nSkipped (invalid):\n  bad-skill: name does not match directory"
        );

        // Empty discovery explains where it looked.
        let empty = strip_sgr(&skills_status_lines(&[], &[], &dirs, root, None).join("\n"));
        assert!(
            empty.contains("No skills discovered") && empty.contains(".agents/skills"),
            "empty view unhelpful:\n{empty}"
        );
        // The roots it names are the ones that were searched, in precedence order — spelled the way
        // the platform spells them, since the rows render a `Path`, not a string we built.
        assert_eq!(
            empty,
            format!(
                "No skills discovered. Searched:\n  {}",
                root.join(".agents").join("skills").display()
            )
        );
    }

    // New: a skill discovered through an injected root that is neither the project's nor a user
    // one is tagged with its own directory, and a description-less warning list still renders.
    #[test]
    fn skills_status_lines_tags_an_injected_root_by_directory() {
        let root = Path::new("/proj");
        let sks = [Skill {
            name: "elsewhere".to_owned(),
            description: "off the map".to_owned(),
            path: PathBuf::from("/opt/skills/elsewhere/SKILL.md"),
        }];
        let lines = skills_status_lines(&sks, &[], &[], root, None);
        assert_eq!(
            strip_sgr(&lines.join("\n")),
            "elsewhere  [/opt/skills/elsewhere]\n  off the map"
        );
        // Warnings alone (nothing valid discovered) still take the populated branch.
        let only_warnings = skills_status_lines(&[], &["bad: broken".to_owned()], &[], root, None);
        assert_eq!(
            strip_sgr(&only_warnings.join("\n")),
            "\nSkipped (invalid):\n  bad: broken"
        );
    }

    // Go: chat/skillcmd_test.go:29 TestExpandSkill — the sent message carries the skill's
    // instructions plus whatever else was typed, tagged with the name and the directory its
    // relative references resolve against.
    #[test]
    fn test_expand_skill() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sk = write_skill(
            dir.path(),
            "brain-page",
            "Read the page, then write it back.",
        );
        let sks = [sk.clone()];

        let (got, name) =
            expand_skill(&sks, " brain-page  now do the thing ").expect("expandSkill");
        assert_eq!(name, "brain-page");
        for want in [
            "<skill name=\"brain-page\"".to_owned(),
            // The location is a Go-quoted string, so a Windows path arrives with its separators
            // escaped — the raw `display()` spelling is not what the block carries.
            crate::text::go_quote(&sk.path.to_string_lossy()),
            format!("References are relative to {}", sk.dir().display()),
            "Read the page, then write it back.".to_owned(),
            "</skill>".to_owned(),
            "now do the thing".to_owned(),
        ] {
            assert!(got.contains(&want), "expansion missing {want:?}:\n{got}");
        }
        // The frontmatter is consumed, never forwarded.
        assert!(
            !got.contains("description: does brain-page"),
            "frontmatter leaked into the message:\n{got}"
        );
        // The user's own text follows the block, not inside it.
        assert!(
            got.find("now do the thing") > got.find("</skill>"),
            "trailing text landed inside the skill block:\n{got}"
        );
        // The whole block, byte for byte.
        assert_eq!(
            got,
            format!(
                "<skill name=\"brain-page\" location={}>\nReferences are relative to {}.\n\nRead the page, then write it back.\n</skill>\n\nnow do the thing",
                crate::text::go_quote(&sk.path.to_string_lossy()),
                sk.dir().display()
            )
        );
    }

    // Go: chat/skillcmd_test.go:64 TestExpandSkillNameOnly — without extra text the block stands
    // alone.
    #[test]
    fn test_expand_skill_name_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sks = [write_skill(dir.path(), "solo", "Just do it.")];
        let (got, _) = expand_skill(&sks, " solo").expect("expandSkill");
        assert!(
            got.trim().ends_with("</skill>"),
            "expansion should end at the block:\n{got}"
        );
    }

    // Go: chat/skillcmd_test.go:78 TestExpandSkillErrors — a name the catalog does not have is an
    // error, not a message; the user types the name, so matching tolerates their casing.
    #[test]
    fn test_expand_skill_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sks = [write_skill(dir.path(), "known", "body")];

        assert_eq!(
            expand_skill(&sks, "").unwrap_err(),
            "usage: /skills <name> [instructions]",
            "bare /skills accepted; want a usage error"
        );
        assert_eq!(
            expand_skill(&sks, " nope").unwrap_err(),
            "no skill named \"nope\" — /skills lists what is available",
            "unknown skill accepted"
        );
        let (_, name) = expand_skill(&sks, " KNOWN").expect("case-insensitive lookup failed");
        assert_eq!(name, "known");
    }

    // New: a `SKILL.md` that vanished (or lost its frontmatter) since discovery is an error naming
    // the skill, not a half-expanded block (agentmode.go:92-101).
    #[test]
    fn expand_skill_reports_a_file_that_changed_since_discovery() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sk = write_skill(dir.path(), "gone", "body");
        std::fs::remove_file(&sk.path).expect("remove");
        let err = expand_skill(std::slice::from_ref(&sk), "gone").unwrap_err();
        assert!(
            err.starts_with("cannot read skill \"gone\": "),
            "read error text: {err}"
        );

        std::fs::write(&sk.path, "no frontmatter here\n").expect("rewrite");
        assert_eq!(
            expand_skill(std::slice::from_ref(&sk), "gone").unwrap_err(),
            "skill \"gone\": missing YAML frontmatter (file must start with ---)"
        );
    }

    // New: the lookup is by name only — the catalog order decides ties and a miss is None.
    #[test]
    fn find_skill_is_case_folded() {
        let sks = [
            Skill {
                name: "alpha".to_owned(),
                description: String::new(),
                path: PathBuf::from("/a/SKILL.md"),
            },
            Skill {
                name: "beta".to_owned(),
                description: String::new(),
                path: PathBuf::from("/b/SKILL.md"),
            },
        ];
        assert_eq!(
            find_skill(&sks, "BeTa").map(|s| s.name.as_str()),
            Some("beta")
        );
        assert_eq!(
            find_skill(&sks, "alpha").map(|s| s.name.as_str()),
            Some("alpha")
        );
        assert!(find_skill(&sks, "gamma").is_none());
        assert!(find_skill(&[], "alpha").is_none());
    }

    // New: the notice's size renderer (agentmode.go:114-119) — bytes below 1 KiB, one decimal
    // above it, and never the MB rung `humanSize` has.
    #[test]
    fn byte_size_table() {
        assert_eq!(byte_size(0), "0 B");
        assert_eq!(byte_size(1), "1 B");
        assert_eq!(byte_size(1023), "1023 B");
        assert_eq!(byte_size(1024), "1.0 KB");
        assert_eq!(byte_size(1536), "1.5 KB");
        assert_eq!(byte_size(1024 * 1024), "1024.0 KB");
    }
}
