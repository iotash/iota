//! A bot's long-term memory, the write side (docs/design/bot-mode.md §3.2, §3.3, §3.5, §3.7): `MEMORY.md`
//! in the bot's directory — a two-key frontmatter over a Markdown body whose `##` sections are the scopes —
//! and the line edits the `remember` tool makes to it.
//!
//! One line is one entry. The tool's lines open with a source tag, `[user]` or `[inferred]`, and close with
//! the date it wrote them; a line without a tag was written by hand and only a human changes it. Everything
//! else in the file — the title, sections the tool does not know, blank lines — is kept as it was.
//!
//! The pure half ([`apply`]) turns the current text and an [`Edit`] into the new text or the model-facing
//! refusal; [`BotMemory::write`] is the I/O around it: the lazy first write, the `.prev` backup, and the
//! write notice the chat loop records once the turn is over ([`WriteLog`]).
//!
//! The read side is [`Snapshot`] (§3.4): the `<memory>` block a bot's every send carries, frozen between
//! the moments a refresh is due.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use crate::sync::lock;
use crate::text::go_quote;

mod snapshot;

pub use snapshot::{Current, Snapshot};

/// The memory file inside a bot's directory.
pub const MEMORY_FILE: &str = "MEMORY.md";
/// The copy of the file as it was before the tool's last write (§3.7 item 2).
pub const MEMORY_PREV_FILE: &str = "MEMORY.md.prev";
/// Hard cap of the file's body — everything after the frontmatter (§3.5).
pub const MEMORY_CAP: usize = 8 * 1024;
/// The soft threshold (75% of [`MEMORY_CAP`]): a write past it succeeds and asks for consolidation.
pub const MEMORY_SOFT_CAP: usize = 6 * 1024;
/// Cap of one line, its `- `, tag and date included.
pub const MEMORY_LINE_CAP: usize = 500;
/// The refusal for an `old` that names a hand-written line (§3.7 item 3).
pub const USER_LINE_REFUSAL: &str = "that line was written by the user; ask them to change it";

/// The three conventional sections, in the order the tool creates them.
const USER: &str = "User";
const PROJECT: &str = "Project:";
const OPEN_THREADS: &str = "Open threads";

/// Where a line came from: the user said it, or the model concluded it (or read it in tool output).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// `[user]`.
    User,
    /// `[inferred]`.
    Inferred,
}

impl Source {
    /// `user` or `inferred`, as the tool's `source` argument spells it.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "user" => Some(Self::User),
            "inferred" => Some(Self::Inferred),
            _ => None,
        }
    }

    /// The line's leading tag.
    pub const fn tag(self) -> &'static str {
        match self {
            Self::User => "[user]",
            Self::Inferred => "[inferred]",
        }
    }
}

/// A section `add` can file a line under — the scope of the line (§3.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Section {
    /// `## User`: global.
    User,
    /// `## Project: <name>`: one project's.
    Project(String),
    /// `## Open threads`: pending matters, global.
    OpenThreads,
}

impl Section {
    /// `User`, `Project: <name>` or `Open threads` (a leading `## ` is tolerated); anything else is the
    /// model-facing refusal. The argument becomes a heading line, so it is one line: a newline or any other
    /// control character is refused rather than folded.
    pub fn parse(s: &str) -> Result<Self, String> {
        if s.chars().any(breaks_line) {
            return Err(format!(
                "section is one heading line, without newlines or control characters: got {}",
                go_quote(s)
            ));
        }
        let s = s.trim();
        let s = s.strip_prefix("## ").unwrap_or(s).trim();
        if s == USER {
            return Ok(Self::User);
        }
        if s == OPEN_THREADS {
            return Ok(Self::OpenThreads);
        }
        if let Some(name) = s.strip_prefix(PROJECT) {
            let name = name.trim();
            if !name.is_empty() {
                return Ok(Self::Project(name.to_owned()));
            }
        }
        Err(format!(
            "section must be \"User\", \"Project: <name>\" or \"Open threads\", got {}",
            go_quote(s)
        ))
    }

    /// The heading text after `## `.
    pub fn heading(&self) -> String {
        match self {
            Self::User => USER.to_owned(),
            Self::Project(name) => format!("{PROJECT} {name}"),
            Self::OpenThreads => OPEN_THREADS.to_owned(),
        }
    }
}

/// The creation order of a heading: `User`, then projects, then open threads; `None` for a section a
/// human added, which the tool neither orders nor creates.
fn rank(heading: &str) -> Option<u8> {
    if heading == USER {
        Some(0)
    } else if heading.starts_with(PROJECT) {
        Some(1)
    } else if heading == OPEN_THREADS {
        Some(2)
    } else {
        None
    }
}

/// One `remember` call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Edit {
    /// A new line under `section`.
    Add {
        /// The entry (newlines are folded; the tag and the date are the tool's).
        text: String,
        /// Its source.
        source: Source,
        /// Where it goes.
        section: Section,
    },
    /// The one tagged line containing `old` becomes a new line, in the same place.
    Replace {
        /// A substring of exactly one line.
        old: String,
        /// The new entry.
        text: String,
        /// Its source.
        source: Source,
    },
    /// The one tagged line containing `old` goes.
    Remove {
        /// A substring of exactly one line.
        old: String,
    },
}

/// What a successful edit produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    /// The whole new file.
    pub file: String,
    /// The body before and after (the frontmatter excluded) — the user's diff.
    pub old_body: String,
    /// See `old_body`.
    pub new_body: String,
    /// The model-facing result: where it went, the size, and the section as it is now (§3.3).
    pub result: String,
    /// The transcript and log line the loop records after the turn (§3.7 item 1).
    pub notice: String,
}

/// The file split into its frontmatter lines (between the fences, `None` when there is none) and its body.
struct Doc {
    front: Option<Vec<String>>,
    body: Vec<String>,
}

impl Doc {
    fn parse(text: &str) -> Self {
        let mut lines = text.lines();
        if lines.next().map(str::trim_end) == Some("---") {
            let rest: Vec<&str> = lines.collect();
            if let Some(end) = rest.iter().position(|l| l.trim_end() == "---") {
                return Self {
                    front: Some(rest[..end].iter().map(|&l| l.to_owned()).collect()),
                    body: rest[end + 1..].iter().map(|&l| l.to_owned()).collect(),
                };
            }
        }
        // No (closed) frontmatter: the whole file is body.
        Self {
            front: None,
            body: text.lines().map(str::to_owned).collect(),
        }
    }

    /// The frontmatter's value for `key`, quotes dropped.
    fn front_value(&self, key: &str) -> Option<String> {
        self.front.as_ref()?.iter().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            (k.trim() == key).then(|| v.trim().trim_matches(['"', '\'']).to_owned())
        })
    }

    /// The frontmatter's `bot:` names `bot`, or names no one: a file copied from another bot's directory is
    /// that bot's data, refused on every read and write alike rather than shown to or edited by this one.
    fn check_owner(&self, bot: &str) -> Result<(), String> {
        match self.front_value("bot") {
            Some(owner) if owner != bot => Err(format!(
                "{MEMORY_FILE} says it belongs to bot {} (frontmatter bot:), not {}",
                go_quote(&owner),
                go_quote(bot)
            )),
            _ => Ok(()),
        }
    }

    fn body_text(&self) -> String {
        join_lines(&self.body)
    }

    /// The file with `bot` and `updated` rewritten — the frontmatter's other lines, if a human added any,
    /// kept after them.
    fn render(&self, bot: &str, today: &str) -> String {
        let mut out = format!("---\nbot: {bot}\nupdated: {today}\n");
        for l in self.front.iter().flatten() {
            let key = l.split_once(':').map(|(k, _)| k.trim());
            if key != Some("bot") && key != Some("updated") {
                out.push_str(l);
                out.push('\n');
            }
        }
        out.push_str("---\n");
        out.push_str(&self.body_text());
        out
    }

    /// The `## ` heading a body line index falls under: `(heading line index, heading text)`, or `None`
    /// for the lines ahead of the first section.
    fn section_of(&self, index: usize) -> Option<(usize, String)> {
        self.body[..=index]
            .iter()
            .enumerate()
            .rev()
            .find_map(|(i, l)| heading(l).map(|h| (i, h.to_owned())))
    }

    /// The end (exclusive) of the section whose heading is at `start`.
    fn section_end(&self, start: usize) -> usize {
        self.body[start + 1..]
            .iter()
            .position(|l| heading(l).is_some())
            .map_or(self.body.len(), |p| start + 1 + p)
    }

    /// The heading line of `name`, if the section exists.
    fn find_section(&self, name: &str) -> Option<usize> {
        self.body.iter().position(|l| heading(l) == Some(name))
    }

    /// Files `line` under `section`, creating the section when it is missing (in the conventional order,
    /// ahead of the first conventional section that ranks after it), and returns the line's index.
    fn insert(&mut self, section: &Section, line: String) -> usize {
        let name = section.heading();
        if let Some(start) = self.find_section(&name) {
            let end = self.section_end(start);
            // After the section's last entry — or right under its heading when it has none — so the blank
            // line that separates it from the next section stays where it was.
            let at = (start + 1..end)
                .rev()
                .find(|&i| is_entry(&self.body[i]))
                .map_or(start + 1, |i| i + 1);
            self.body.insert(at, line);
            return at;
        }
        let own = rank(&name).unwrap_or(u8::MAX);
        let before = self
            .body
            .iter()
            .position(|l| heading(l).and_then(rank).is_some_and(|r| r > own));
        if let Some(at) = before {
            self.body
                .splice(at..at, [format!("## {name}"), line, String::new()]);
            return at + 1;
        }
        while self.body.last().is_some_and(|l| l.trim().is_empty()) {
            self.body.pop();
        }
        if !self.body.is_empty() {
            self.body.push(String::new());
        }
        self.body.push(format!("## {name}"));
        self.body.push(line);
        self.body.len() - 1
    }

    /// The section around body line `index` as it reads now: `(label, text)`, the label being `## <name>`
    /// (empty for the lines ahead of the first section), the text its heading and lines, trailing blanks
    /// dropped.
    fn excerpt(&self, index: usize) -> (String, String) {
        if self.body.is_empty() {
            return (String::new(), String::new());
        }
        let index = index.min(self.body.len().saturating_sub(1));
        let (start, label) = match self.section_of(index) {
            Some((i, h)) => (i, format!("## {h}")),
            None => (0, String::new()),
        };
        let end = if label.is_empty() {
            self.body
                .iter()
                .position(|l| heading(l).is_some())
                .unwrap_or(self.body.len())
        } else {
            self.section_end(start)
        };
        let mut lines = &self.body[start..end];
        while let [rest @ .., last] = lines
            && last.trim().is_empty()
        {
            lines = rest;
        }
        (label, lines.join("\n"))
    }
}

/// `lines` joined with a trailing newline (none for no lines).
fn join_lines(lines: &[String]) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// The heading text of a `## ` line.
fn heading(line: &str) -> Option<&str> {
    line.strip_prefix("## ").map(str::trim)
}

/// An entry line: `- ` first.
fn is_entry(line: &str) -> bool {
    line.starts_with("- ")
}

/// A line the tool wrote: an entry whose first word is a source tag.
fn is_tagged(line: &str) -> bool {
    line.strip_prefix("- ").is_some_and(|rest| {
        [Source::User, Source::Inferred]
            .iter()
            .any(|s| rest.starts_with(s.tag()))
    })
}

/// A character that has no place inside one line of the file: a control character other than a tab (a
/// newline, a lone `\r`), or a Unicode line or paragraph separator.
fn breaks_line(c: char) -> bool {
    (c.is_control() && c != '\t') || matches!(c, '\u{2028}' | '\u{2029}')
}

/// The entry text as one line: newlines folded into spaces, and a leading `- `, a leading source tag and
/// a trailing `(YYYY-MM-DD)` dropped — those are the tool's to write.
fn normalize(text: &str) -> String {
    let joined = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let mut s = joined.strip_prefix("- ").unwrap_or(&joined).trim();
    for src in [Source::User, Source::Inferred] {
        if let Some(rest) = s.strip_prefix(src.tag()) {
            s = rest.trim_start();
        }
    }
    if let Some(open) = s.rfind(" (")
        && is_date_suffix(&s[open + 1..])
    {
        s = s[..open].trim_end();
    }
    s.to_owned()
}

/// `(YYYY-MM-DD)`.
fn is_date_suffix(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 12
        && b[0] == b'('
        && b[11] == b')'
        && b[5] == b'-'
        && b[8] == b'-'
        && [1, 2, 3, 4, 6, 7, 9, 10]
            .iter()
            .all(|&i| b[i].is_ascii_digit())
}

/// The line the tool writes for `text`, or the refusal: an empty text, a line break the folding leaves (a
/// lone `\r`, a control character), an over-long line. The checks judge the normalized text — the line as it
/// lands in the file.
fn make_line(text: &str, source: Source, today: &str) -> Result<String, String> {
    let text = normalize(text);
    if text.is_empty() {
        return Err("text is empty".to_owned());
    }
    if text.chars().any(breaks_line) {
        return Err(format!(
            "text is one line, without control characters: got {}",
            go_quote(&text)
        ));
    }
    let line = format!("- {} {text} ({today})", source.tag());
    if line.len() > MEMORY_LINE_CAP {
        return Err(format!(
            "a memory line is at most {MEMORY_LINE_CAP} bytes and this one is {}: shorten it to the one fact you need to recall, or split it into separate lines",
            line.len()
        ));
    }
    Ok(line)
}

/// The index of the one entry containing `old`, or the refusal: none or several match (the candidates
/// listed), or the match is a hand-written line.
fn find_one(doc: &Doc, old: &str) -> Result<usize, String> {
    if old.is_empty() {
        return Err("old is required: a substring of the line to change".to_owned());
    }
    let hits: Vec<usize> = (0..doc.body.len())
        .filter(|&i| is_entry(&doc.body[i]) && doc.body[i].contains(old))
        .collect();
    match hits.as_slice() {
        [one] if is_tagged(&doc.body[*one]) => Ok(*one),
        [one] => Err(format!("{USER_LINE_REFUSAL}: {}", doc.body[*one])),
        [] => {
            let all: Vec<&str> = doc
                .body
                .iter()
                .filter(|l| is_entry(l))
                .map(String::as_str)
                .collect();
            if all.is_empty() {
                Err(format!(
                    "no line contains {}: MEMORY.md has no lines yet",
                    go_quote(old)
                ))
            } else {
                Err(format!(
                    "no line contains {}; the lines are:\n{}",
                    go_quote(old),
                    all.join("\n")
                ))
            }
        }
        many => {
            let lines: Vec<&str> = many.iter().map(|&i| doc.body[i].as_str()).collect();
            Err(format!(
                "{} matches {} lines; make old long enough to name exactly one:\n{}",
                go_quote(old),
                many.len(),
                lines.join("\n")
            ))
        }
    }
}

/// `len` bytes in KiB with one decimal, rounded (`6.1`).
fn kib(len: usize) -> String {
    let tenths = (len * 10 + 512) / 1024;
    format!("{}.{}", tenths / 10, tenths % 10)
}

/// The soft-threshold sentence (§3.5) for a body of `len` bytes, or `None` below it. The write result carries
/// it, and so does a bot's memory-flush notice (§3.6.1).
pub fn soft_warning(len: usize) -> Option<String> {
    (len >= MEMORY_SOFT_CAP).then(|| {
        format!(
            "MEMORY.md is at {}% — consolidate soon (merge related lines with replace, drop stale ones with remove)",
            len * 100 / MEMORY_CAP
        )
    })
}

/// Applies `edit` to `current` (`None`: no file yet) for bot `bot` on `today`: the new file, or the
/// model-facing refusal. Nothing here touches the disk.
pub fn apply(
    current: Option<&str>,
    bot: &str,
    edit: &Edit,
    today: &str,
) -> Result<Applied, String> {
    let mut doc = match current {
        Some(text) => Doc::parse(text),
        None => Doc {
            front: None,
            body: vec![format!("# {bot} memory"), String::new()],
        },
    };
    doc.check_owner(bot)
        .map_err(|e| format!("{e}; nothing was written — ask the user to fix the file"))?;
    let old_body = if current.is_some() {
        doc.body_text()
    } else {
        String::new()
    };

    let (at, verb, shown) = match edit {
        Edit::Add {
            text,
            source,
            section,
        } => {
            let line = make_line(text, *source, today)?;
            let shown = line.clone();
            (doc.insert(section, line), "+1 line", shown)
        }
        Edit::Replace { old, text, source } => {
            let line = make_line(text, *source, today)?;
            let at = find_one(&doc, old)?;
            let was = std::mem::replace(&mut doc.body[at], line.clone());
            (at, "~1 line", format!("{line} (was: {was})"))
        }
        Edit::Remove { old } => {
            let at = find_one(&doc, old)?;
            let was = doc.body.remove(at);
            // The line above the removed one is in the same section (its heading, at worst).
            (at.saturating_sub(1), "-1 line", was)
        }
    };

    let new_body = doc.body_text();
    // The cap guards growth: a replace or remove that does not grow the file always goes through, so the
    // way out of a full file is never refused.
    let shrinks = !matches!(edit, Edit::Add { .. }) && new_body.len() <= old_body.len();
    if new_body.len() > MEMORY_CAP && !shrinks {
        let now = current.unwrap_or_default();
        return Err(format!(
            "MEMORY.md would be {} KiB, over its {} KiB cap; nothing was written. Consolidate first — merge related lines with replace, drop stale ones with remove — then retry.\n\nMEMORY.md now ({} / {} KiB):\n{now}",
            kib(new_body.len()),
            MEMORY_CAP / 1024,
            kib(old_body.len()),
            MEMORY_CAP / 1024,
        ));
    }

    let (label, section) = doc.excerpt(at);
    let place = if label.is_empty() {
        MEMORY_FILE.to_owned()
    } else {
        format!("{MEMORY_FILE} {label}")
    };
    let mut result = format!(
        "saved to {place} ({} / {} KiB)",
        kib(new_body.len()),
        MEMORY_CAP / 1024
    );
    if !section.is_empty() {
        let _ = write!(result, "\n\n{section}");
    }
    if let Some(warning) = soft_warning(new_body.len()) {
        let _ = write!(result, "\n\n{warning}");
    }
    let shown = shown.strip_prefix("- ").unwrap_or(&shown);
    Ok(Applied {
        file: doc.render(bot, today),
        old_body,
        new_body,
        result,
        notice: format!("memory: {place} {verb}: {shown}"),
    })
}

/// One write a turn made: the notice the loop records for it, and whether it saved a line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Written {
    /// `memory: MEMORY.md ## <section> +1 line: …` — one per write, a remove's included.
    pub notice: String,
    /// The write put a line into the file: an add or a replace. A remove saved nothing — the line it took
    /// out is in neither the file nor the summary's memory section (bot-mode.md §3.6.2 item 3).
    pub saved: bool,
}

/// The writes a turn made, waiting for the loop to record them once the turn is over (§3.7 item 1).
/// Shared between the tool and the loop; each write is taken exactly once.
#[derive(Clone, Debug, Default)]
pub struct WriteLog(Arc<Mutex<Vec<Written>>>);

impl WriteLog {
    /// Queues one write.
    pub fn push(&self, written: Written) {
        lock(&self.0).push(written);
    }

    /// Everything queued since the last take, oldest first.
    pub fn take(&self) -> Vec<Written> {
        std::mem::take(&mut *lock(&self.0))
    }
}

/// What this process last knew of the file's mtime — shared between the tool that writes the file and the
/// snapshot that reads it, so the snapshot can tell an edit from outside the process from the tool's own
/// write (§3.4, the fourth refresh moment).
#[derive(Debug, Default)]
struct Seen {
    /// The mtime after this process last read or wrote the file (`None`: there was no file).
    mtime: Option<SystemTime>,
    /// The tool found the file changed from outside before it wrote over it: the next check reloads even
    /// though the mtime is the tool's own by then.
    edited: bool,
}

/// One bot's memory on disk: its name, its directory, and the log its writes are announced through.
#[derive(Clone, Debug)]
pub struct BotMemory {
    name: String,
    dir: PathBuf,
    writes: WriteLog,
    seen: Arc<Mutex<Seen>>,
}

impl BotMemory {
    /// Bot `name`'s memory in `dir` (`<bots>/<name>`).
    pub fn new(name: &str, dir: PathBuf) -> Self {
        Self {
            name: name.to_owned(),
            dir,
            writes: WriteLog::default(),
            seen: Arc::default(),
        }
    }

    /// The bot's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// `<dir>/MEMORY.md`.
    pub fn path(&self) -> PathBuf {
        self.dir.join(MEMORY_FILE)
    }

    /// The writes the loop has not recorded yet.
    pub fn writes(&self) -> &WriteLog {
        &self.writes
    }

    /// Applies `edit` to the file on disk. A missing file is an empty memory and is created by the first
    /// write; an existing one is copied to `MEMORY.md.prev` first. On success the notice is queued on
    /// [`Self::writes`]; every failure is a model-facing refusal and leaves the file as it was.
    pub fn write(&self, edit: &Edit, today: &str) -> Result<Applied, String> {
        let path = self.path();
        // An edit from outside that the snapshot has not picked up yet is about to be folded into this
        // write; the mark keeps it from passing for the tool's own.
        if mtime(&path) != lock(&self.seen).mtime {
            lock(&self.seen).edited = true;
        }
        let current = read_existing(&path)?;
        let applied = apply(current.as_deref(), &self.name, edit, today)?;
        if let Some(old) = &current {
            crate::app::fs::write_atomic(
                &self.dir.join(MEMORY_PREV_FILE),
                old.as_bytes(),
                Some(0o644),
            )
            .map_err(|e| format!("cannot back up {}: {e}", path.display()))?;
        }
        crate::app::fs::write_atomic(&path, applied.file.as_bytes(), Some(0o644))
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        lock(&self.seen).mtime = mtime(&path);
        self.writes.push(Written {
            notice: applied.notice.clone(),
            saved: !matches!(edit, Edit::Remove { .. }),
        });
        Ok(applied)
    }

    /// Whether the file changed since this process last read or wrote it — an edit from outside.
    fn edited_outside(&self) -> bool {
        let seen = lock(&self.seen);
        seen.edited || mtime(&self.path()) != seen.mtime
    }

    /// Reads the file for a snapshot and records its mtime as seen. The mtime is taken first, so an edit
    /// that lands during the read shows up as a changed mtime at the next check rather than being missed.
    /// A file whose frontmatter names another bot is refused like an unreadable one — and still seen, so it
    /// is not re-read at every send until it changes.
    fn read_for_snapshot(&self) -> Result<Option<String>, String> {
        let before = mtime(&self.path());
        let text = self.read_owned();
        *lock(&self.seen) = Seen {
            mtime: before,
            edited: false,
        };
        text
    }

    /// The file's text (`None` when it does not exist), refused when it cannot be read or its frontmatter
    /// names another bot ([`Doc::check_owner`], the same check a write makes).
    fn read_owned(&self) -> Result<Option<String>, String> {
        let text = read_existing(&self.path())?;
        if let Some(text) = &text {
            Doc::parse(text).check_owner(&self.name)?;
        }
        Ok(text)
    }
}

/// The file's mtime, `None` when it cannot be read (a missing file above all).
fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// The file's text, `None` when it does not exist.
fn read_existing(path: &Path) -> Result<Option<String>, String> {
    match std::fs::read(path) {
        Ok(bytes) => String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| format!("{} is not UTF-8 text", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

#[cfg(test)]
mod tests;
