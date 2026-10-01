use std::fmt::Write as _;

use super::{
    BotMemory, Edit, MEMORY_CAP, MEMORY_FILE, MEMORY_LINE_CAP, MEMORY_PREV_FILE, Section, Source,
    USER_LINE_REFUSAL, apply,
};

const TODAY: &str = "2026-09-30";

/// The §3.2 example, verbatim.
const EXAMPLE: &str = "---
bot: coder
updated: 2026-09-30
---

# coder memory

## User
- [user] 回复用中文，技术名词保留原文 (2026-09-30)
- [user] 提交前只跑 clippy + cargo test，完整 ci.sh 每个 PR 收尾跑一次 (2026-09-15)
- 不要用 rebase

## Project: iota
- [inferred] 发布流程见 notes/release (2026-09-28)

## Open threads
- [user] 等 bot-retention.sh 的两组试运行看 flush 的方向 (2026-09-30)
";

fn add(text: &str, source: Source, section: Section) -> Edit {
    Edit::Add {
        text: text.to_owned(),
        source,
        section,
    }
}

fn replace(old: &str, text: &str) -> Edit {
    Edit::Replace {
        old: old.to_owned(),
        text: text.to_owned(),
        source: Source::User,
    }
}

fn remove(old: &str) -> Edit {
    Edit::Remove {
        old: old.to_owned(),
    }
}

/// The first write creates the whole shape: frontmatter, title, the section.
#[test]
fn a_first_add_creates_the_file() {
    let a = apply(
        None,
        "coder",
        &add("回复用中文", Source::User, Section::User),
        TODAY,
    )
    .expect("add");
    assert_eq!(
        a.file,
        "---\nbot: coder\nupdated: 2026-09-30\n---\n# coder memory\n\n## User\n- [user] 回复用中文 (2026-09-30)\n"
    );
    assert_eq!(a.old_body, "");
    assert!(
        a.result.starts_with(
            "saved to MEMORY.md ## User (0.1 / 8 KiB)\n\n## User\n- [user] 回复用中文 (2026-09-30)"
        ),
        "{}",
        a.result
    );
    assert_eq!(
        a.notice,
        "memory: MEMORY.md ## User +1 line: [user] 回复用中文 (2026-09-30)"
    );
}

/// `add` files the line at the end of its section's entries, folds newlines, and the tool's own tag and
/// date are not doubled when the model writes them anyway.
#[test]
fn add_goes_under_its_section() {
    let a = apply(
        Some(EXAMPLE),
        "coder",
        &add(
            "- [inferred] 用 cargo nextest\n跑测试 (2026-01-01)",
            Source::Inferred,
            Section::Project("iota".to_owned()),
        ),
        "2026-10-01",
    )
    .expect("add");
    assert!(a.file.contains(
        "## Project: iota\n- [inferred] 发布流程见 notes/release (2026-09-28)\n- [inferred] 用 cargo nextest 跑测试 (2026-10-01)\n\n## Open threads"
    ), "{}", a.file);
    // The result carries the section as it reads now — and only that section.
    assert!(
        a.result
            .contains("## Project: iota\n- [inferred] 发布流程见")
    );
    assert!(
        a.result.ends_with("用 cargo nextest 跑测试 (2026-10-01)"),
        "{}",
        a.result
    );
    assert!(!a.result.contains("## User"));
    // `updated` is today's, the hand-written line survives untouched.
    assert!(
        a.file
            .starts_with("---\nbot: coder\nupdated: 2026-10-01\n---\n")
    );
    assert!(a.file.contains("\n- 不要用 rebase\n"));
}

/// Missing sections are created in the conventional order, ahead of the first one ranking after them;
/// a section a human added stays where it was.
#[test]
fn missing_sections_are_created_in_order() {
    let file = "---\nbot: b\nupdated: 2026-01-01\n---\n# b memory\n\n## Open threads\n- [user] x (2026-01-01)\n\n## Mine\nfree text\n";
    let a = apply(
        Some(file),
        "b",
        &add("p", Source::User, Section::Project("iota".to_owned())),
        TODAY,
    )
    .expect("project");
    let b = apply(
        Some(&a.file),
        "b",
        &add("u", Source::User, Section::User),
        TODAY,
    )
    .expect("user");
    let body = b.file.split_once("---\n# ").expect("front").1;
    assert_eq!(
        body,
        "b memory\n\n## User\n- [user] u (2026-09-30)\n\n## Project: iota\n- [user] p (2026-09-30)\n\n## Open threads\n- [user] x (2026-01-01)\n\n## Mine\nfree text\n"
    );
    // With nothing ranking after it, a new section goes at the end.
    let c = apply(
        Some(&b.file),
        "b",
        &add("t", Source::Inferred, Section::OpenThreads),
        TODAY,
    )
    .expect("threads");
    assert!(
        c.file
            .contains("## Open threads\n- [user] x (2026-01-01)\n- [inferred] t (2026-09-30)\n")
    );
}

#[test]
fn section_names_are_the_three_conventions() {
    assert_eq!(Section::parse("User"), Ok(Section::User));
    assert_eq!(Section::parse("## Open threads"), Ok(Section::OpenThreads));
    assert_eq!(
        Section::parse("Project:  iota "),
        Ok(Section::Project("iota".to_owned()))
    );
    for bad in ["Project:", "Project:   ", "## Project:", "Notes", "user"] {
        assert!(
            Section::parse(bad)
                .expect_err(bad)
                .starts_with("section must be \"User\", \"Project: <name>\" or \"Open threads\""),
        );
    }
}

/// A section is one heading line: a newline would let the argument write lines of its own — an untagged
/// one under `## User` that later passes for the user's.
#[test]
fn a_section_is_one_heading_line() {
    for bad in [
        "Project: x\n## User\n- Bearer FAKE",
        "User\n- planted",
        "Project: x\ry",
        "Project: x\u{2028}y",
        "Project: a\u{7}b",
    ] {
        assert!(
            Section::parse(bad)
                .expect_err(bad)
                .starts_with("section is one heading line"),
            "{bad:?}"
        );
    }
}

/// A newline in `text` cannot split the entry: it is folded before any check, so the injection lands as
/// part of the one tagged line; a break the folding leaves (a lone `\r`) is refused.
#[test]
fn a_text_is_one_line() {
    let a = apply(
        Some(EXAMPLE),
        "coder",
        &add(
            "x\n## User\n- planted",
            Source::Inferred,
            Section::OpenThreads,
        ),
        TODAY,
    )
    .expect("folded");
    let body = a.new_body;
    assert_eq!(body.lines().count(), a.old_body.lines().count() + 1);
    assert_eq!(body.matches("## User").count(), 2, "{body}");
    assert!(body.contains("\n- [inferred] x ## User - planted (2026-09-30)\n"));
    assert!(!body.lines().any(|l| l == "- planted"));

    for bad in ["x\ry", "x\u{2029}y", "x\u{1b}[2Jy"] {
        let e = apply(
            Some(EXAMPLE),
            "coder",
            &add(bad, Source::User, Section::User),
            TODAY,
        )
        .expect_err(bad);
        assert!(e.starts_with("text is one line"), "{bad:?}: {e}");
        let e = apply(Some(EXAMPLE), "coder", &replace("发布流程", bad), TODAY).expect_err(bad);
        assert!(e.starts_with("text is one line"), "{bad:?}: {e}");
    }
}

/// `replace` rewrites the one line in place, with the new source and today's date.
#[test]
fn replace_changes_one_tagged_line_in_place() {
    let a = apply(
        Some(EXAMPLE),
        "coder",
        &replace("发布流程", "发布流程见 RELEASING.md"),
        TODAY,
    )
    .expect("replace");
    assert!(a.file.contains(
        "## Project: iota\n- [user] 发布流程见 RELEASING.md (2026-09-30)\n\n## Open threads"
    ));
    assert!(!a.file.contains("notes/release"));
    assert!(
        a.result
            .contains("## Project: iota\n- [user] 发布流程见 RELEASING.md")
    );
    assert_eq!(
        a.notice,
        "memory: MEMORY.md ## Project: iota ~1 line: [user] 发布流程见 RELEASING.md (2026-09-30) (was: - [inferred] 发布流程见 notes/release (2026-09-28))"
    );
}

/// `remove` drops the line; the result shows its section without it.
#[test]
fn remove_drops_one_tagged_line() {
    let a = apply(Some(EXAMPLE), "coder", &remove("bot-retention"), TODAY).expect("remove");
    assert!(!a.file.contains("bot-retention"));
    assert!(a.file.ends_with("## Open threads\n"), "{}", a.file);
    assert!(
        a.result.ends_with("\n\n## Open threads"),
        "the emptied section is still the one shown: {}",
        a.result
    );
    assert_eq!(
        a.notice,
        "memory: MEMORY.md ## Open threads -1 line: [user] 等 bot-retention.sh 的两组试运行看 flush 的方向 (2026-09-30)"
    );
    // The last line of a section that is followed by another shows its own section, not the next one.
    let b = apply(Some(EXAMPLE), "coder", &remove("发布流程"), TODAY).expect("remove");
    assert!(b.result.contains("## Project: iota"), "{}", b.result);
    assert!(!b.result.contains("## Open threads"), "{}", b.result);
}

/// The three ways `old` can miss, for both actions that take it.
#[test]
fn old_must_name_exactly_one_tagged_line() {
    for edit in [replace("nope", "x"), remove("nope")] {
        let e = apply(Some(EXAMPLE), "coder", &edit, TODAY).expect_err("no hit");
        assert!(
            e.starts_with("no line contains \"nope\"; the lines are:\n- [user] 回复用中文"),
            "{e}"
        );
        assert!(
            e.contains("- 不要用 rebase"),
            "every line is a candidate: {e}"
        );
    }
    for edit in [replace("[user]", "x"), remove("[user]")] {
        let e = apply(Some(EXAMPLE), "coder", &edit, TODAY).expect_err("many");
        assert!(
            e.starts_with(
                "\"[user]\" matches 3 lines; make old long enough to name exactly one:\n"
            ),
            "{e}"
        );
        assert!(!e.contains("[inferred]"), "only the hits are listed: {e}");
    }
    for edit in [replace("rebase", "x"), remove("rebase")] {
        let e = apply(Some(EXAMPLE), "coder", &edit, TODAY).expect_err("hand-written");
        assert_eq!(e, format!("{USER_LINE_REFUSAL}: - 不要用 rebase"));
    }
    let e = apply(None, "coder", &remove("x"), TODAY).expect_err("empty");
    assert_eq!(e, "no line contains \"x\": MEMORY.md has no lines yet");
    let e = apply(Some(EXAMPLE), "coder", &remove(""), TODAY).expect_err("no old");
    assert!(e.starts_with("old is required"), "{e}");
}

/// A heading or a free-text line is not an entry: `old` never matches it.
#[test]
fn only_entries_are_matched() {
    let e = apply(Some(EXAMPLE), "coder", &remove("coder memory"), TODAY).expect_err("title");
    assert!(e.starts_with("no line contains"), "{e}");
}

/// A body of `n` bytes: the title plus tagged filler lines.
fn file_of(n: usize) -> String {
    let mut body = String::from("# b memory\n\n## User\n");
    let mut i = 0;
    while body.len() + 100 <= n {
        let _ = writeln!(body, "- [inferred] {i:04} {}", "x".repeat(80));
        i += 1;
    }
    let pad = n - body.len();
    if pad > 0 {
        // One last line of exactly the missing size (`- ` + text + `\n`).
        let _ = writeln!(
            body,
            "- [inferred] {}",
            "y".repeat(pad.saturating_sub(14).max(1))
        );
    }
    format!("---\nbot: b\nupdated: 2026-01-01\n---\n{body}")
}

fn body_len(file: &str) -> usize {
    file.split_once("---\n")
        .and_then(|(_, r)| r.split_once("---\n"))
        .map_or(0, |(_, b)| b.len())
}

/// Soft threshold: the write goes through and says how full the file is.
#[test]
fn past_the_soft_threshold_the_write_asks_for_consolidation() {
    let small = apply(
        Some(&file_of(4000)),
        "b",
        &add("z", Source::User, Section::User),
        TODAY,
    )
    .expect("small");
    assert!(!small.result.contains("consolidate"), "{}", small.result);

    let file = file_of(6700);
    let a = apply(
        Some(&file),
        "b",
        &add("z", Source::User, Section::User),
        TODAY,
    )
    .expect("soft");
    let len = body_len(&a.file);
    assert!((6144..MEMORY_CAP).contains(&len), "{len}");
    assert!(
        a.result.ends_with(&format!(
            "\n\nMEMORY.md is at {}% — consolidate soon (merge related lines with replace, drop stale ones with remove)",
            len * 100 / MEMORY_CAP
        )),
        "{}",
        a.result
    );
    assert!(
        a.result.starts_with("saved to MEMORY.md ## User (6."),
        "{}",
        a.result
    );
}

/// Hard cap: the write is refused with the whole file in the error, unless it shrinks the file.
#[test]
fn past_the_hard_cap_only_shrinking_edits_go_through() {
    let file = file_of(MEMORY_CAP - 10);
    assert!(body_len(&file) <= MEMORY_CAP);
    let e = apply(
        Some(&file),
        "b",
        &add("one more line", Source::User, Section::User),
        TODAY,
    )
    .expect_err("over");
    assert!(
        e.starts_with("MEMORY.md would be 8.0 KiB, over its 8 KiB cap; nothing was written."),
        "{e}"
    );
    assert!(
        e.contains("\n\nMEMORY.md now (8.0 / 8 KiB):\n---\nbot: b\n"),
        "{e}"
    );
    assert!(e.ends_with(&file), "the error carries the current file");

    // A file a human pushed past the cap still accepts the edits that make it smaller…
    let over = file_of(MEMORY_CAP + 300);
    apply(Some(&over), "b", &remove("0001"), TODAY).expect("remove shrinks");
    apply(Some(&over), "b", &replace("0002", "short"), TODAY).expect("replace shrinks");
    // …but not one that grows it.
    let e = apply(Some(&over), "b", &replace("0003", &"w".repeat(200)), TODAY).expect_err("grows");
    assert!(e.contains("over its 8 KiB cap"), "{e}");
}

/// A section name has no length cap of its own (the 100-byte cap is gone): a long, well-formed
/// `Project:` heading is accepted, written as one heading line, and found again by the next write.
#[test]
fn a_long_section_name_is_accepted() {
    let name = format!("{}-service", "p".repeat(300));
    let heading = format!("Project: {name}");
    assert!(heading.len() > 100, "{}", heading.len());
    assert_eq!(Section::parse(&heading), Ok(Section::Project(name.clone())));
    let a = apply(
        None,
        "b",
        &add("first", Source::User, Section::Project(name.clone())),
        TODAY,
    )
    .expect("a long section name");
    let b = apply(
        Some(&a.file),
        "b",
        &add("second", Source::User, Section::Project(name.clone())),
        TODAY,
    )
    .expect("the same section again");
    assert!(
        b.file.ends_with(&format!(
            "\n## {heading}\n- [user] first (2026-09-30)\n- [user] second (2026-09-30)\n"
        )),
        "{}",
        b.file
    );
    assert_eq!(b.file.matches("## Project:").count(), 1, "{}", b.file);
}

/// The 8 KiB cap judges the whole body, headings included: a write that would take it past 8 KiB by
/// opening a new section under a long name is refused, nothing written, while one that lands exactly on
/// the cap goes through.
#[test]
fn a_new_section_cannot_carry_the_body_past_the_cap() {
    let file = file_of(MEMORY_CAP - 600);
    let open = |len: usize| {
        apply(
            Some(&file),
            "b",
            &add("z", Source::User, Section::Project("n".repeat(len))),
            TODAY,
        )
    };
    let probe = body_len(&open(150).expect("well under the cap").file);
    let exact = 150 + MEMORY_CAP - probe;
    let fits = open(exact).expect("exactly the cap");
    assert_eq!(body_len(&fits.file), MEMORY_CAP);
    let e = open(exact + 1).expect_err("one byte over");
    assert!(
        e.starts_with("MEMORY.md would be 8.0 KiB, over its 8 KiB cap; nothing was written."),
        "{e}"
    );
    assert!(e.ends_with(&file), "the error carries the file as it was");
}

#[test]
fn one_line_is_at_most_500_bytes() {
    let e = apply(
        None,
        "b",
        &add(&"a".repeat(MEMORY_LINE_CAP), Source::User, Section::User),
        TODAY,
    )
    .expect_err("long");
    assert_eq!(
        e,
        "a memory line is at most 500 bytes and this one is 522: shorten it to the one fact you need to recall, or split it into separate lines"
    );
    // `- [user] ` + text + ` (2026-09-30)` = 22 bytes of the tool's own.
    apply(
        None,
        "b",
        &add(
            &"a".repeat(MEMORY_LINE_CAP - 22),
            Source::User,
            Section::User,
        ),
        TODAY,
    )
    .expect("exactly the cap");
    let e = apply(None, "b", &add(" \n ", Source::User, Section::User), TODAY).expect_err("empty");
    assert_eq!(e, "text is empty");
    // The cap judges the folded line: surrounding blanks and newlines do not count against it.
    let padded = format!("\n  {}  \n\n", "a".repeat(MEMORY_LINE_CAP - 22));
    apply(None, "b", &add(&padded, Source::User, Section::User), TODAY).expect("folded to the cap");
}

/// `bot:` must name this bot; a file without it (written by hand) gets it on the first write, and a
/// human's extra frontmatter lines are kept.
#[test]
fn the_frontmatter_names_the_bot() {
    let e = apply(
        Some(EXAMPLE),
        "other",
        &add("x", Source::User, Section::User),
        TODAY,
    )
    .expect_err("wrong bot");
    assert!(
        e.starts_with(
            "MEMORY.md says it belongs to bot \"coder\" (frontmatter bot:), not \"other\""
        ),
        "{e}"
    );

    let hand = "# notes\n- 不要用 rebase\n";
    let a = apply(
        Some(hand),
        "b",
        &add("x", Source::User, Section::User),
        TODAY,
    )
    .expect("no front");
    assert_eq!(
        a.file,
        "---\nbot: b\nupdated: 2026-09-30\n---\n# notes\n- 不要用 rebase\n\n## User\n- [user] x (2026-09-30)\n"
    );
    let extra = "---\nupdated: 2020-01-01\nbot: \"b\"\nowner: me\n---\nbody\n";
    let a = apply(
        Some(extra),
        "b",
        &add("x", Source::User, Section::User),
        TODAY,
    )
    .expect("extra");
    assert!(
        a.file
            .starts_with("---\nbot: b\nupdated: 2026-09-30\nowner: me\n---\nbody\n"),
        "{}",
        a.file
    );
}

#[test]
fn the_disk_write_is_lazy_backed_up_and_announced() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bot_dir = dir.path().join("bots").join("coder");
    let mem = BotMemory::new("coder", bot_dir.clone());
    let path = bot_dir.join(MEMORY_FILE);
    let prev = bot_dir.join(MEMORY_PREV_FILE);

    // A refused write creates nothing.
    mem.write(&remove("x"), TODAY)
        .expect_err("nothing to remove");
    assert!(!path.exists() && !bot_dir.exists());

    // The first write creates the file; there is nothing to back up yet.
    mem.write(&add("one", Source::User, Section::User), TODAY)
        .expect("first");
    let first = std::fs::read_to_string(&path).expect("created");
    assert!(first.contains("- [user] one (2026-09-30)"));
    assert!(!prev.exists());

    // The second keeps the first as `.prev`.
    mem.write(&add("two", Source::Inferred, Section::User), TODAY)
        .expect("second");
    assert_eq!(std::fs::read_to_string(&prev).expect("prev"), first);

    // A refused write leaves both files alone.
    let now = std::fs::read_to_string(&path).expect("now");
    mem.write(&add(" ", Source::User, Section::User), TODAY)
        .expect_err("empty");
    assert_eq!(std::fs::read_to_string(&path).expect("now"), now);
    assert_eq!(std::fs::read_to_string(&prev).expect("prev"), first);

    // Each successful write queued one notice, taken once.
    assert_eq!(
        mem.writes().take(),
        [
            "memory: MEMORY.md ## User +1 line: [user] one (2026-09-30)",
            "memory: MEMORY.md ## User +1 line: [inferred] two (2026-09-30)",
        ]
    );
    assert!(mem.writes().take().is_empty());
}
