//! `/export [path]` (chat/export.go, run.go:804-820): writes the conversation out as a
//! self-contained HTML page (inline CSS, dark-mode toggle, highlighted code) or as Markdown — the
//! FULL on-disk log for a saved session (`load_full_history`), the in-memory view for an
//! ephemeral one — under `iota-<slug|id>-<time>.<ext>` when no path is given (a format picker asks
//! which). Every helper is pure and unit-tested beside this arm.
//!
//! The document builders are infallible in Rust: Go's two error returns came from
//! `chroma.WriteCSS` and `goldmark.Convert`, neither of which has a fallible twin here
//! (the CSS emitter writes into a `String`, comrak returns one).

use std::path::Path;

use crate::markdown::html::{code_css, markdown_to_html};
use crate::provider::model::{JsonObject, Message, Role};
use crate::repl::run::Repl;
use crate::repl::styles::{dim, red};
use crate::repl::uisink::LineCommitter;
use crate::session::SessionError;
use crate::text::go_quote;
use crate::tool::fmt::display_tool_name;
use crate::ui::facade::SelectSpec;

/// The two export formats (export.go:36-44).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExportFormat {
    /// `.html`.
    Html,
    /// `.md`.
    Markdown,
}

impl ExportFormat {
    /// The file extension without the dot.
    pub(crate) fn ext(self) -> &'static str {
        match self {
            Self::Html => "html",
            Self::Markdown => "md",
        }
    }
}

/// The slug's rune cap (export.go:78).
pub(crate) const EXPORT_SLUG_MAX: usize = 40;

/// The dark-mode CSS variables (export.go:329-339).
pub(crate) const EXPORT_DARK_VARS: &str = "  color-scheme: dark;\n  --bg: #0d1117;\n  --fg: #e6edf3;\n  --muted: #8b949e;\n  --border: #30363d;\n  --bubble-bg: #161b22;\n  --bubble-border: #30363d;\n  --code-bg: #161b22;\n  --accent: #58a6ff;";

/// The theme toggle script (export.go:408-420).
pub(crate) const EXPORT_TOGGLE_JS: &str = r#"(function () {
  var root = document.documentElement;
  var stored = localStorage.getItem("iota-theme");
  if (stored) root.setAttribute("data-theme", stored);
  document.getElementById("theme-toggle").addEventListener("click", function () {
    var sysDark = window.matchMedia("(prefers-color-scheme: dark)").matches;
    var cur = root.getAttribute("data-theme") || (sysDark ? "dark" : "light");
    var next = cur === "dark" ? "light" : "dark";
    root.setAttribute("data-theme", next);
    localStorage.setItem("iota-theme", next);
  });
})();"#;

/// The `localStorage` key of the toggle.
pub(crate) const THEME_STORAGE_KEY: &str = "iota-theme";
/// The toggle button's id — the one hook the `<button>` and the script share.
pub(crate) const THEME_TOGGLE_ID: &str = "theme-toggle";

/// `str::contains` in const context, so the two hook constants above are checked against
/// [`EXPORT_TOGGLE_JS`] at COMPILE time instead of being documented and left to drift (the
/// script is a verbatim copy of Go's and cannot interpolate them).
const fn toggle_js_mentions(needle: &str) -> bool {
    let (h, n) = (EXPORT_TOGGLE_JS.as_bytes(), needle.as_bytes());
    if n.is_empty() || n.len() > h.len() {
        return false;
    }
    let mut i = 0;
    while i + n.len() <= h.len() {
        let mut j = 0;
        while j < n.len() && h[i + j] == n[j] {
            j += 1;
        }
        if j == n.len() {
            return true;
        }
        i += 1;
    }
    false
}

const _: () = assert!(
    toggle_js_mentions(THEME_STORAGE_KEY),
    "the toggle script must read and write THEME_STORAGE_KEY"
);
const _: () = assert!(
    toggle_js_mentions(THEME_TOGGLE_ID),
    "the toggle script must bind to THEME_TOGGLE_ID"
);

/// The picker's title (run.go:807).
pub(crate) const EXPORT_PICKER_TITLE: &str = "Export format";
/// The picker's rows, in Go's order (run.go:807) — index 1 is Markdown, anything else HTML.
pub(crate) const EXPORT_PICKER_ITEMS: [&str; 2] = ["HTML", "Markdown"];

/// chat/export.go:341-353: the light `:root` tokens through the media-query opener, up to the
/// first [`EXPORT_DARK_VARS`] splice.
pub(crate) const PAGE_CSS_HEAD: &str = r#":root {
  color-scheme: light;
  --bg: #ffffff;
  --fg: #1f2328;
  --muted: #6a737d;
  --border: #d8dee4;
  --bubble-bg: #eef4fb;
  --bubble-border: #cfe0f4;
  --code-bg: #f6f8fa;
  --accent: #0969da;
}
@media (prefers-color-scheme: dark) {
  :root:not([data-theme="light"]) {
"#;

/// chat/export.go:355-358: what sits between the two [`EXPORT_DARK_VARS`] splices.
pub(crate) const PAGE_CSS_MID: &str = r#"
  }
}
:root[data-theme="dark"] {
"#;

/// chat/export.go:360-405: every element rule after the second [`EXPORT_DARK_VARS`] splice.
pub(crate) const PAGE_CSS_TAIL: &str = r#"
}
* { box-sizing: border-box; }
body {
  margin: 0 auto;
  padding: 2rem 1.25rem 4rem;
  max-width: 48rem;
  background: var(--bg);
  color: var(--fg);
  font-family: system-ui, -apple-system, "Segoe UI", Roboto, "Helvetica Neue", Arial, sans-serif;
  line-height: 1.6;
}
header { position: relative; border-bottom: 1px solid var(--border); padding-bottom: 1rem; margin-bottom: 1rem; }
header h1 { margin: 0 0 0.35rem; font-size: 1.5rem; padding-right: 6rem; }
p.meta { margin: 0; color: var(--muted); font-size: 0.85rem; }
#theme-toggle {
  position: absolute; top: 0.25rem; right: 0;
  padding: 0.25rem 0.6rem; font-size: 0.8rem;
  color: var(--muted); background: transparent;
  border: 1px solid var(--border); border-radius: 6px; cursor: pointer;
}
#theme-toggle:hover { color: var(--fg); }
section.round { border-bottom: 1px solid var(--border); padding: 1.25rem 0; }
section.round:last-child { border-bottom: none; }
div.bubble {
  background: var(--bubble-bg);
  border: 1px solid var(--bubble-border);
  border-radius: 0.75rem;
  padding: 0.7rem 1rem;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}
p.attachment, p.interrupted { color: var(--muted); font-size: 0.85rem; font-style: italic; margin: 0.35rem 0; }
details.muted { color: var(--muted); font-size: 0.9rem; margin: 0.75rem 0; }
details.muted summary { cursor: pointer; }
p.tool-label { margin: 0.5rem 0 0.25rem; font-size: 0.8rem; }
a { color: var(--accent); }
pre, code {
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, "Liberation Mono", monospace;
}
code { font-size: 0.9em; }
:not(pre) > code { background: var(--code-bg); padding: 0.15em 0.35em; border-radius: 4px; }
pre { background: var(--code-bg); padding: 0.75rem 1rem; border-radius: 8px; overflow-x: auto; }
blockquote { margin: 0.75rem 0; padding: 0 1rem; color: var(--muted); border-left: 3px solid var(--border); }
table { border-collapse: collapse; margin: 0.75rem 0; display: block; overflow-x: auto; }
th, td { border: 1px solid var(--border); padding: 0.35rem 0.7rem; }
img { max-width: 100%; }
"#;
/// The page CSS with [`EXPORT_DARK_VARS`] spliced twice (export.go:341-405).
///
/// CSS cannot merge the media query with the `data-theme` override, so the dark palette is
/// emitted under both scopes, exactly as Go does.
pub(crate) fn export_page_css() -> String {
    format!("{PAGE_CSS_HEAD}{EXPORT_DARK_VARS}{PAGE_CSS_MID}{EXPORT_DARK_VARS}{PAGE_CSS_TAIL}")
}

/// What the export describes (export.go:119).
#[derive(Clone, Debug)]
pub(crate) struct ExportMeta {
    /// The session title (`""` → `"iota session"`).
    pub(crate) title: String,
    /// The session id (`""` while ephemeral).
    pub(crate) session_id: String,
    /// The provider's model.
    pub(crate) model: String,
    /// When the export was made.
    pub(crate) date: jiff::Zoned,
}

/// One user round with its replies (export.go:170).
#[derive(Clone, Debug)]
pub(crate) struct ExportRound {
    /// The user message (`None` for a leading assistant run — Go's `user.Role == ""`).
    pub(crate) user: Option<Message>,
    /// The assistant/tool messages that followed.
    pub(crate) replies: Vec<Message>,
}

/// export.go:50-59: a directory path or a path with no usable file name is refused.
///
/// A trailing separator would silently create a hidden `.html` INSIDE the directory; a base
/// that is nothing but an extension (`.md`) would create a dot-file.
pub(crate) fn validate_export_target(path: &str) -> Result<(), String> {
    if path.ends_with(std::path::MAIN_SEPARATOR) || path.ends_with('/') {
        return Err(format!(
            "{} is a directory path; give a file name",
            go_quote(path)
        ));
    }
    let base = go_base(path);
    if base.is_empty() || base == "." || base == ".." || base == go_ext(base) {
        return Err(format!("{} has no usable file name", go_quote(path)));
    }
    Ok(())
}

/// export.go:65-74: the format by extension (`.md`/`.markdown` → Markdown, else HTML with `.html`
/// appended when the extension is missing). Matching is case-insensitive.
pub(crate) fn detect_export_format(path: &str) -> (String, ExportFormat) {
    match go_ext(path).to_lowercase().as_str() {
        ".md" | ".markdown" => (path.to_owned(), ExportFormat::Markdown),
        "" => (format!("{path}.html"), ExportFormat::Html),
        _ => (path.to_owned(), ExportFormat::Html),
    }
}

/// `filepath.Base` twin: trailing separators stripped, then the last element; `"."` for an
/// empty path and a bare separator for a path that was nothing but separators.
pub(crate) fn go_base(path: &str) -> &str {
    if path.is_empty() {
        return ".";
    }
    let mut path = path;
    while let Some(rest) = path.strip_suffix(is_sep) {
        path = rest;
    }
    match path.rfind(is_sep) {
        Some(i) => {
            let tail = &path[i + 1..];
            if tail.is_empty() { "/" } else { tail }
        }
        None if path.is_empty() => "/",
        None => path,
    }
}

/// `filepath.Ext` twin: from the LAST dot of the final element, inclusive — so a dot-file's
/// extension IS its whole base (`".md"` → `".md"`, which `validate_export_target` refuses).
pub(crate) fn go_ext(path: &str) -> &str {
    for (i, c) in path.char_indices().rev() {
        if is_sep(c) {
            break;
        }
        if c == '.' {
            return &path[i..];
        }
    }
    ""
}

/// `os.IsPathSeparator` for the platforms this binary targets.
fn is_sep(c: char) -> bool {
    c == '/' || c == std::path::MAIN_SEPARATOR
}

/// export.go:81-102: lowercase alphanumeric runs joined by `-`, capped at [`EXPORT_SLUG_MAX`].
///
/// The cap is checked BEFORE each rune, so the separating dash can push the buffer one over;
/// the explicit truncation and the trailing-dash trim after the loop are Go's, in order.
pub(crate) fn slugify(s: &str) -> String {
    let mut b: Vec<char> = Vec::new();
    let mut dash = false;
    for r in s.to_lowercase().chars() {
        if b.len() >= EXPORT_SLUG_MAX {
            break;
        }
        if r.is_alphanumeric() {
            if dash {
                b.push('-');
            }
            dash = false;
            b.push(r);
        } else if !b.is_empty() {
            dash = true;
        }
    }
    b.truncate(EXPORT_SLUG_MAX);
    let out: String = b.into_iter().collect();
    out.trim_end_matches('-').to_owned()
}

/// `iota-{slug|id|session}-{%Y%m%d-%H%M%S}.{ext}` (export.go:107-116), in LOCAL time.
pub(crate) fn export_file_name(
    title: &str,
    id: &str,
    format: ExportFormat,
    now: &jiff::Zoned,
) -> String {
    let mut slug = slugify(title);
    if slug.is_empty() {
        slug = slugify(id);
    }
    if slug.is_empty() {
        "session".clone_into(&mut slug);
    }
    format!(
        "{}-{slug}-{}.{}",
        crate::app::NAME,
        now.strftime("%Y%m%d-%H%M%S"),
        format.ext()
    )
}

/// The document title (export.go:128; `"iota session"` fallback).
pub(crate) fn export_title(meta: &ExportMeta) -> String {
    let t = meta.title.trim();
    if t.is_empty() {
        format!("{} session", crate::app::NAME)
    } else {
        t.to_owned()
    }
}

/// `Session {id}` · model · date · `{n} messages`, joined ` · ` (export.go:137-150).
///
/// Fields the session does not have are skipped; the date is always present.
pub(crate) fn export_meta_line(meta: &ExportMeta, count: usize, with_count: bool) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(4);
    if !meta.session_id.is_empty() {
        parts.push(format!("Session {}", meta.session_id));
    }
    if !meta.model.is_empty() {
        parts.push(meta.model.clone());
    }
    parts.push(meta.date.strftime("%Y-%m-%d %H:%M").to_string());
    if with_count {
        parts.push(format!("{count} messages"));
    }
    parts.join(" · ")
}

/// Exported conversation messages (export.go:154): everything but the system prompt, which is
/// rendered as a header section rather than a turn.
pub(crate) fn conversation_count(msgs: &[Message]) -> usize {
    msgs.iter().filter(|m| m.role() != Role::System).count()
}

/// `(system messages, rounds)` (export.go:174-190).
///
/// A leading assistant/tool message with no user opener (possible in unusual logs) starts a
/// round whose `user` is `None`.
pub(crate) fn split_rounds(msgs: &[Message]) -> (Vec<Message>, Vec<ExportRound>) {
    let mut system: Vec<Message> = Vec::new();
    let mut rounds: Vec<ExportRound> = Vec::new();
    for m in msgs {
        match m.role() {
            Role::System => system.push(m.clone()),
            Role::User => rounds.push(ExportRound {
                user: Some(m.clone()),
                replies: Vec::new(),
            }),
            Role::Assistant | Role::Tool => {
                if rounds.is_empty() {
                    rounds.push(ExportRound {
                        user: None,
                        replies: Vec::new(),
                    });
                }
                if let Some(r) = rounds.last_mut() {
                    r.replies.push(m.clone());
                }
            }
        }
    }
    (system, rounds)
}

/// The Markdown document (export.go:200-261): blocks joined by `"\n\n"` with a trailing `"\n"`.
///
/// Assistant content is embedded verbatim (it already IS Markdown); reasoning is skipped; tool
/// activity collapses to ONE quoted marker per round; attachments and interruption are noted
/// inline; the system prompt becomes a collapsed `<details>` at the top.
pub(crate) fn build_export_markdown(meta: &ExportMeta, msgs: &[Message]) -> String {
    let (system, rounds) = split_rounds(msgs);

    let mut blocks: Vec<String> = Vec::new();
    blocks.push(format!("# {}", export_title(meta)));
    blocks.push(format!("> {}", export_meta_line(meta, 0, false)));
    for sys in &system {
        blocks.push(format!(
            "<details>\n<summary>System prompt</summary>\n\n{}\n\n</details>",
            sys.content.trim()
        ));
    }

    for (i, r) in rounds.iter().enumerate() {
        if i > 0 {
            blocks.push("---".to_owned());
        }
        if let Some(user) = &r.user {
            blocks.push("## User".to_owned());
            for att in &user.attachments {
                blocks.push(format!("(attachment: {})", att.filename));
            }
            let c = user.content.trim();
            if !c.is_empty() {
                blocks.push(c.to_owned());
            }
        }

        let tool_calls: usize = r.replies.iter().map(|m| m.tool_calls().len()).sum();
        // Image providers reply with attachments and no text — those turns still have an
        // Assistant side to export.
        let has_reply = tool_calls > 0
            || r.replies.iter().any(|m| {
                m.role() == Role::Assistant
                    && (!m.content.trim().is_empty() || !m.attachments.is_empty())
            });
        if !has_reply {
            continue;
        }

        blocks.push("## Assistant".to_owned());
        if tool_calls > 0 {
            blocks.push(format!("> ⚙ {tool_calls} tool call(s)"));
        }
        for m in &r.replies {
            if m.role() != Role::Assistant {
                continue;
            }
            let c = m.content.trim();
            if !c.is_empty() {
                blocks.push(c.to_owned());
            }
            for att in &m.attachments {
                blocks.push(format!("(image: {})", att.filename));
            }
            if m.interrupted() {
                blocks.push("(interrupted)".to_owned());
            }
        }
    }
    format!("{}\n", blocks.join("\n\n"))
}

/// The HTML document (export.go:429-529): one self-contained page, all CSS inline, no external
/// references.
///
/// Assistant Markdown goes through comrak in safe mode; user content is HTML-escaped into
/// bubble blocks (user text is NEVER interpreted); reasoning and tool calls collapse into
/// `<details>`; generated images embed as data URIs so the file survives the session bundle.
/// Order inside a reply is Reasoning → Content → tool calls → attachments → interrupted, which
/// is the live stream order.
pub(crate) fn build_export_html(meta: &ExportMeta, msgs: &[Message]) -> String {
    use base64::Engine as _;
    use std::fmt::Write as _;

    let (system, rounds) = split_rounds(msgs);
    let title = html_escape(&export_title(meta));

    let mut b = String::with_capacity(64 * 1024);
    b.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n");
    b.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    let _ = writeln!(b, "<title>{title}</title>");
    b.push_str("<style>\n");
    b.push_str(&export_page_css());
    b.push_str(&code_css());
    b.push_str("</style>\n</head>\n<body>\n");

    b.push_str("<header>\n");
    let _ = writeln!(b, "<h1>{title}</h1>");
    let _ = writeln!(
        b,
        "<p class=\"meta\">{}</p>",
        html_escape(&export_meta_line(meta, conversation_count(msgs), true))
    );
    let _ = write!(
        b,
        "<button id=\"{THEME_TOGGLE_ID}\" type=\"button\">Toggle theme</button>\n</header>\n<main>\n"
    );

    for sys in &system {
        b.push_str("<details class=\"muted system\"><summary>System prompt</summary><pre>");
        b.push_str(&html_escape(&sys.content));
        b.push_str("</pre></details>\n");
    }

    for r in &rounds {
        b.push_str("<section class=\"round\">\n");
        if let Some(user) = &r.user {
            for att in &user.attachments {
                let _ = writeln!(
                    b,
                    "<p class=\"attachment\">(attachment: {})</p>",
                    html_escape(&att.filename)
                );
            }
            let _ = writeln!(
                b,
                "<div class=\"bubble\">{}</div>",
                html_escape(&user.content)
            );
        }

        // Tool results are matched to their calls by id.
        let results: std::collections::HashMap<&str, &Message> = r
            .replies
            .iter()
            .filter(|m| m.role() == Role::Tool)
            .map(|m| (m.tool_call_id(), m))
            .collect();

        for m in &r.replies {
            if m.role() != Role::Assistant {
                continue;
            }
            if !m.reasoning().is_empty() {
                b.push_str("<details class=\"muted\"><summary>Reasoning</summary><pre>");
                b.push_str(&html_escape(m.reasoning()));
                b.push_str("</pre></details>\n");
            }
            if !m.content.is_empty() {
                b.push_str("<div class=\"assistant\">\n");
                b.push_str(&markdown_to_html(&m.content));
                b.push_str("</div>\n");
            }
            for tc in m.tool_calls() {
                let _ = writeln!(
                    b,
                    "<details class=\"muted tool\"><summary>⚙ {}</summary>",
                    html_escape(&display_tool_name(&tc.name))
                );
                if !tc.arguments.is_empty() {
                    let _ = writeln!(
                        b,
                        "<pre>{}</pre>",
                        html_escape(&go_json_indent(&tc.arguments))
                    );
                }
                if let Some(res) = results.get(tc.id.as_str()) {
                    let label = if res.is_error() { "Error" } else { "Result" };
                    let _ = writeln!(
                        b,
                        "<p class=\"tool-label\">{label}</p><pre>{}</pre>",
                        html_escape(&res.content)
                    );
                }
                b.push_str("</details>\n");
            }
            // Generated images embed as data URIs — the export stays a single
            // self-contained file even after the session bundle is deleted.
            for att in &m.attachments {
                if att.mime_type.starts_with("image/") {
                    let _ = writeln!(
                        b,
                        "<img alt=\"{}\" src=\"data:{};base64,{}\">",
                        html_escape(&att.filename),
                        html_escape(&att.mime_type),
                        base64::engine::general_purpose::STANDARD.encode(&att.data)
                    );
                } else {
                    let _ = writeln!(
                        b,
                        "<p class=\"attachment\">(attachment: {})</p>",
                        html_escape(&att.filename)
                    );
                }
            }
            if m.interrupted() {
                b.push_str("<p class=\"interrupted\">(interrupted)</p>\n");
            }
        }
        b.push_str("</section>\n");
    }

    let _ = write!(
        b,
        "</main>\n<script>\n{EXPORT_TOGGLE_JS}\n</script>\n</body>\n</html>\n"
    );
    b
}

/// `html.EscapeString` twin: `&` `'` `<` `>` `"`, in Go's exact entity spellings.
pub(crate) fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '\'' => out.push_str("&#39;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&#34;"),
            _ => out.push(c),
        }
    }
    out
}

/// `json.MarshalIndent(v, "", "  ")` twin with `SetEscapeHTML(true)` inside string values
/// (T-43).
///
/// `Arguments` is Go's `map[string]any`, whose keys marshal SORTED; `JsonObject` is a
/// `serde_json::Map` over a `BTreeMap` (`preserve_order` is off), so the key order already
/// matches. What `serde_json` does NOT do is Go's HTML escaping, so the five characters
/// `encoding/json` rewrites are rewritten here. All five can only occur inside a JSON string
/// literal — the structural bytes are `{}[]:,` and whitespace, and no number, boolean or
/// `null` contains them — so a flat replacement is exact.
pub(crate) fn go_json_indent(args: &JsonObject) -> String {
    let Ok(s) = serde_json::to_string_pretty(args) else {
        // Unreachable for a `Map<String, Value>`; Go printed `%v` here (export.go:496).
        return format!("{args:?}");
    };
    s.replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

/// `filepath.Abs` twin (= `Clean(Join(cwd, path))`); an error keeps `path` (export.go:562-564).
///
/// `std::path::absolute` only drops `.` and repeated separators — `filepath.Clean` also folds
/// `..` lexically, which is what the printed path must show.
pub(crate) fn go_abs(path: &str) -> String {
    use std::path::{Component, PathBuf};
    let Ok(abs) = std::path::absolute(path) else {
        return path.to_owned();
    };
    let mut out = PathBuf::new();
    for c in abs.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                // At the root `..` is the root itself, exactly like `filepath.Clean`.
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out.to_str().map_or_else(|| path.to_owned(), str::to_owned)
}

/// A leading `~/` becomes `home` (export.go:534-541).
///
/// Only a leading `"~/"`, and a host with no home directory leaves the path UNCHANGED —
/// unlike `/file`, which errors.
pub(crate) fn expand_home(path: &str, home: Option<&Path>) -> String {
    let Some(rest) = path.strip_prefix("~/") else {
        return path.to_owned();
    };
    home.and_then(|h| h.join(rest).to_str().map(str::to_owned))
        .unwrap_or_else(|| path.to_owned())
}

/// `os.OpenFile(path, O_WRONLY|O_CREATE|O_EXCL, 0o644)` + write + close (export.go:592-609).
///
/// `O_EXCL` makes the no-overwrite guarantee atomic: creation fails if the target appeared
/// between any earlier check and this write.
#[cfg(unix)]
fn create_new_0644(path: &Path, doc: &str) -> std::io::Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(path)?;
    f.write_all(doc.as_bytes())
}

/// Non-unix hosts have no mode bits to set (DIVERGENCES I-07).
#[cfg(not(unix))]
fn create_new_0644(path: &Path, doc: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    f.write_all(doc.as_bytes())
}

/// The pure-ish core of [`cmd_export`] (export.go:548-612): validate → format → load → build →
/// write, with every line committed through `w`.
///
/// `load_full` is only called when `on_disk` is true — an ephemeral or not-yet-persisted
/// session exports the in-memory `history` instead, which is exactly Go's `sw.onDisk()` gate.
/// The home directory is supplied rather than probed, so the whole function is testable
/// without touching the process environment.
pub(crate) fn export_chat(
    w: &mut LineCommitter,
    arg: &str,
    on_disk: bool,
    load_full: impl FnOnce() -> Result<Vec<Message>, SessionError>,
    history: &[Message],
    meta: &ExportMeta,
    home: Option<&Path>,
) {
    if arg.is_empty() {
        return; // the format prompt lives in the run loop (run.go:806-814)
    }
    let expanded = expand_home(arg, home);
    if let Err(e) = validate_export_target(&expanded) {
        w.write(&format!("{}\n", red(&format!("Error: {e}"))));
        return;
    }
    let (path, format) = detect_export_format(&expanded);
    let path = go_abs(&path);

    let msgs = if on_disk {
        match load_full() {
            Ok(m) => m,
            Err(e) => {
                w.write(&format!("{}\n", red(&format!("Error: {e}"))));
                return;
            }
        }
    } else {
        history.to_vec()
    };
    let count = conversation_count(&msgs);
    if count == 0 {
        w.write(&format!("{}\n", dim("Nothing to export yet.")));
        return;
    }

    let doc = match format {
        ExportFormat::Markdown => build_export_markdown(meta, &msgs),
        ExportFormat::Html => build_export_html(meta, &msgs),
    };
    if let Err(e) = create_new_0644(Path::new(&path), &doc) {
        let line = if e.kind() == std::io::ErrorKind::AlreadyExists {
            format!("Error: {path} already exists")
        } else {
            format!("Error: {e}")
        };
        w.write(&format!("{}\n", red(&line)));
        return;
    }
    w.write(&format!(
        "{}\n",
        dim(&format!("Exported {count} messages → {path}"))
    ));
}

/// The `/export` arm (run.go:804-820 + export.go:548-612).
///
/// With no argument the format picker runs first and the target name is generated from the
/// session title (falling back to its id, then `"session"`); a cancelled picker — or a facade
/// error — prints NOTHING. The generated name then travels the same
/// `expand_home` → `validate_export_target` → `detect_export_format` path as a typed one.
///
/// The writer slot is snapshotted BEFORE the picker await, so its lock is never held across
/// a suspension point.
pub(crate) async fn cmd_export(repl: &mut Repl, arg: &str) {
    let cancel = &repl.cancel.clone();
    let (title, id, on_disk) = {
        let slot = repl
            .writer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        slot.as_ref().map_or_else(
            || (String::new(), String::new(), false),
            |w| (w.meta().title.clone(), w.id().to_owned(), w.on_disk()),
        )
    };

    let mut arg = arg.to_owned();
    if arg.is_empty() {
        let spec = SelectSpec {
            title: EXPORT_PICKER_TITLE.to_owned(),
            items: EXPORT_PICKER_ITEMS
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            cursor: 0,
        };
        let Ok(r) = repl.ui.select(cancel, spec).await else {
            return;
        };
        if r.cancelled {
            return;
        }
        let format = if r.index == 1 {
            ExportFormat::Markdown
        } else {
            ExportFormat::Html
        };
        arg = export_file_name(&title, &id, format, &jiff::Zoned::now());
    }

    let meta = ExportMeta {
        title,
        session_id: id.clone(),
        model: repl.provider.model().to_owned(),
        date: jiff::Zoned::now(),
    };
    let kind = repl.provider.kind();
    let store = &repl.store;
    let mut w = LineCommitter::default();
    export_chat(
        &mut w,
        &arg,
        on_disk,
        || store.load_full(&id, kind),
        &repl.history,
        &meta,
        crate::app::user_home().as_deref(),
    );
    let lines = w.flush();
    if !lines.is_empty() {
        repl.tr.notice_lines(&lines);
    }
}

#[cfg(test)]
mod tests;
