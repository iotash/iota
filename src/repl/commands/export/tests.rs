use super::{
    EXPORT_DARK_VARS, EXPORT_PICKER_ITEMS, EXPORT_PICKER_TITLE, EXPORT_SLUG_MAX, EXPORT_TOGGLE_JS,
    ExportFormat, ExportMeta, THEME_STORAGE_KEY, THEME_TOGGLE_ID, build_export_html,
    build_export_markdown, conversation_count, detect_export_format, expand_home, export_file_name,
    export_meta_line, export_page_css, export_title, go_abs, go_base, go_ext, go_json_indent,
    slugify, split_rounds, validate_export_target,
};
use crate::provider::model::{Attachment, Body, Message, ToolBody, ToolCall};
use pretty_assertions::assert_eq;
use std::path::Path;

/// A fixed instant, so every golden is deterministic (Go's tests pass an explicit
/// `time.Date(…, time.UTC)`).
fn at(hour: i8, minute: i8, second: i8) -> jiff::Zoned {
    jiff::civil::date(2026, 7, 7)
        .at(hour, minute, second, 0)
        .to_zoned(jiff::tz::TimeZone::UTC)
        .expect("a valid UTC instant")
}

fn meta(title: &str, id: &str, model: &str) -> ExportMeta {
    ExportMeta {
        title: title.to_owned(),
        session_id: id.to_owned(),
        model: model.to_owned(),
        date: at(9, 30, 0),
    }
}

/// `exportTestHistory` (`chat/export_test.go:78`): a two-round history exercising every
/// export feature — system prompt, attachment, reasoning, a tool round, an interruption.
pub(super) fn export_test_history() -> Vec<Message> {
    vec![
        Message::system("be terse"),
        Message {
            attachments: vec![Attachment {
                filename: "a.txt".to_owned(),
                mime_type: "text/plain".to_owned(),
                data: b"x".to_vec(),
            }],
            ..Message::user("read the file")
        },
        Message::assistant("")
            .with_reasoning("let me think about this".to_owned())
            .with_tool_calls(vec![ToolCall {
                id: "c1".to_owned(),
                name: "load_skill".to_owned(),
                arguments: [("path".to_owned(), serde_json::json!("a.txt"))]
                    .into_iter()
                    .collect(),
            }]),
        Message {
            content: "file contents".to_owned(),
            body: Body::Tool(ToolBody {
                call_id: "c1".to_owned(),
                call_name: "load_skill".to_owned(),
                ..ToolBody::default()
            }),
            ..Message::default()
        },
        Message::assistant("It says **x**."),
        Message::user("thanks"),
        Message::assistant("partial…").with_interrupted(true),
    ]
}

// Go: chat/export_test.go:11 TestDetectExportFormat — nine cases: `.md`/`.markdown` in any
// case select Markdown and keep the name; a name with no extension gets `.html`; every
// other extension keeps the name and exports HTML.
#[test]
fn test_detect_export_format() {
    let cases: [(&str, &str, ExportFormat); 9] = [
        ("notes.md", "notes.md", ExportFormat::Markdown),
        ("NOTES.MD", "NOTES.MD", ExportFormat::Markdown),
        ("log.markdown", "log.markdown", ExportFormat::Markdown),
        ("Log.Markdown", "Log.Markdown", ExportFormat::Markdown),
        ("report", "report.html", ExportFormat::Html),
        ("page.html", "page.html", ExportFormat::Html),
        ("Page.HTML", "Page.HTML", ExportFormat::Html),
        // unknown extension keeps the name, exports HTML
        ("data.txt", "data.txt", ExportFormat::Html),
        ("dir/nested", "dir/nested.html", ExportFormat::Html),
    ];
    for (input, want_path, want_format) in cases {
        assert_eq!(
            detect_export_format(input),
            (want_path.to_owned(), want_format),
            "detect_export_format({input:?})"
        );
    }
    assert_eq!(ExportFormat::Html.ext(), "html");
    assert_eq!(ExportFormat::Markdown.ext(), "md");
}

// Go: chat/export_test.go:35 TestSlugify — punctuation runs fold to one dash, leading junk
// starts nothing, unicode letters survive, and a long title is capped with no trailing dash.
#[test]
fn test_slugify() {
    for (input, want) in [
        ("Hello, World!", "hello-world"),
        ("  spaces   and\tpunct?! ", "spaces-and-punct"),
        ("already-fine", "already-fine"),
        ("...", ""),
        ("", ""),
        ("中文 标题", "中文-标题"), // unicode letters survive
    ] {
        assert_eq!(slugify(input), want, "slugify({input:?})");
    }

    // Long titles are capped (~40 runes) with no trailing dash.
    let long = slugify(&"word ".repeat(20));
    assert!(
        long.chars().count() <= EXPORT_SLUG_MAX,
        "slug length {} exceeds cap {EXPORT_SLUG_MAX}: {long:?}",
        long.chars().count()
    );
    assert!(
        !long.ends_with('-'),
        "capped slug has a trailing dash: {long:?}"
    );
}

// Go: chat/export_test.go:60 TestExportFileName — the title slug wins; an empty title falls
// back to the session id; both empty fall back to `"session"`.
#[test]
fn test_export_file_name() {
    let now = at(9, 30, 15);
    assert_eq!(
        export_file_name("Fix the Build!", "k7qz3xv9m2ht", ExportFormat::Html, &now),
        "iota-fix-the-build-20260707-093015.html"
    );
    assert_eq!(
        export_file_name("", "k7qz3xv9m2ht", ExportFormat::Markdown, &now),
        "iota-k7qz3xv9m2ht-20260707-093015.md"
    );
    assert_eq!(
        export_file_name("", "", ExportFormat::Html, &now),
        "iota-session-20260707-093015.html"
    );
}

// Go: chat/export_test.go:92 TestBuildExportMarkdown — every block the document owes, the
// reasoning that must NOT appear, and exactly one round separator.
#[test]
fn test_build_export_markdown() {
    let out = build_export_markdown(&meta("My Chat", "abc123", "gpt-x"), &export_test_history());
    for want in [
        "# My Chat",
        "> Session abc123 · gpt-x · 2026-07-07 09:30",
        "<summary>System prompt</summary>",
        "be terse",
        "## User",
        "## Assistant",
        "(attachment: a.txt)",
        "> ⚙ 1 tool call(s)",
        "It says **x**.", // assistant content verbatim
        "\n\n---\n\n",    // rule between rounds
        "(interrupted)",
    ] {
        assert!(out.contains(want), "markdown missing {want:?}\n---\n{out}");
    }
    assert!(
        !out.contains("let me think"),
        "markdown must skip reasoning:\n{out}"
    );
    assert_eq!(
        out.matches("---").count(),
        1,
        "expected exactly one round separator:\n{out}"
    );
}

// Go: chat/export_test.go:121 TestBuildExportHTML — user text is escaped and never
// interpreted, assistant Markdown is rendered, code blocks carry the chroma class, and both
// dark-mode hooks are present.
#[test]
fn test_build_export_html() {
    let msgs = vec![
        Message::user("<script>alert(1)</script>"),
        Message::assistant("**bold** and\n\n```go\nfunc main() {}\n```"),
    ];
    let out = build_export_html(&meta("My Chat", "abc123", "gpt-x"), &msgs);

    // User content is escaped — never interpreted as HTML.
    assert!(
        !out.contains("<script>alert"),
        "user <script> passed through unescaped"
    );
    assert!(
        out.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
        "escaped user content missing"
    );
    // Assistant markdown is rendered.
    assert!(
        out.contains("<strong>bold</strong>"),
        "assistant markdown not rendered"
    );
    // Code blocks carry chroma classes (class mode), styled for both themes.
    assert!(
        out.contains("class=\"chroma\""),
        "chroma class-mode output missing"
    );
    // Dark mode: media query plus the toggle hook.
    assert!(
        out.contains("prefers-color-scheme"),
        "prefers-color-scheme CSS missing"
    );
    assert!(
        out.contains("id=\"theme-toggle\"") && out.contains("data-theme"),
        "theme toggle hook missing"
    );
    assert!(
        out.contains("html[data-theme=\"dark\"] .chroma"),
        "forced-dark chroma scope missing"
    );
}

// Go: chat/export_test.go:161 TestBuildExportHTMLDetails — reasoning and tool activity
// collapse into `<details>`; the tool result, the attachment note and the interruption row
// are all present.
#[test]
fn test_build_export_html_details() {
    let out = build_export_html(&meta("T", "", ""), &export_test_history());
    for want in [
        "<summary>System prompt</summary>",
        "<summary>Reasoning</summary>",
        "let me think about this",
        "<summary>⚙ load_skill</summary>",
        "file contents", // tool result
        "(attachment: a.txt)",
        "<p class=\"interrupted\">(interrupted)</p>",
        // the arguments block, escaped exactly as Go escapes it
        "<pre>{\n  &#34;path&#34;: &#34;a.txt&#34;\n}</pre>",
    ] {
        assert!(out.contains(want), "html missing {want:?}");
    }
}

// Go: chat/export_test.go:182 TestValidateExportTarget — a trailing separator, a dot-only
// base and a base that is nothing but an extension are all refused.
#[test]
fn test_validate_export_target() {
    for bad in ["mydir/", ".", "..", ".md", ".html", "dir/.markdown"] {
        let err = validate_export_target(bad)
            .expect_err(&format!(
                "validate_export_target({bad:?}) = Ok, want an error"
            ))
            .to_string();
        assert!(err.contains(&format!("{bad:?}")), "{bad:?} → {err}");
    }
    for good in ["chat.html", "notes.md", "dir/chat", "a.tar.gz"] {
        assert_eq!(
            validate_export_target(good),
            Ok(()),
            "validate_export_target({good:?})"
        );
    }
    assert_eq!(
        validate_export_target("mydir/").unwrap_err().to_string(),
        "\"mydir/\" is a directory path; give a file name"
    );
    assert_eq!(
        validate_export_target(".md").unwrap_err().to_string(),
        "\".md\" has no usable file name"
    );
}

// Go: chat/export_test.go:198 TestExportImageOnlyReply — an imagen round whose assistant
// reply is an image attachment with no text keeps its Assistant side in both documents.
#[test]
fn test_export_image_only_reply() {
    let history = vec![
        Message::user("a red circle"),
        Message {
            attachments: vec![Attachment {
                filename: "image-1.png".to_owned(),
                mime_type: "image/png".to_owned(),
                data: vec![9],
            }],
            ..Message::assistant("")
        },
    ];
    let m = meta("T", "", "");
    let md = build_export_markdown(&m, &history);
    assert!(
        md.contains("## Assistant") && md.contains("(image: image-1.png)"),
        "markdown dropped the image reply:\n{md}"
    );
    let out = build_export_html(&m, &history);
    assert!(
        out.contains("data:image/png;base64,CQ=="),
        "HTML export must embed the generated image as a data URI"
    );
    assert!(out.contains("<img alt=\"image-1.png\" src=\"data:image/png;base64,CQ==\">"));
}

// New (the contract's `go_base`/`go_ext` twins): `filepath.Base`/`filepath.Ext` semantics,
// including the dot-file case `Path::extension()` gets wrong.
#[test]
fn go_base_and_ext_follow_filepath() {
    for (input, want) in [
        ("", "."),
        ("/", "/"),
        ("///", "/"),
        ("mydir/", "mydir"),
        ("a/b/c.txt", "c.txt"),
        ("dir/.markdown", ".markdown"),
        ("chat.html", "chat.html"),
        ("..", ".."),
    ] {
        assert_eq!(go_base(input), want, "go_base({input:?})");
    }
    for (input, want) in [
        ("", ""),
        (".md", ".md"),
        ("a.tar.gz", ".gz"),
        ("chat.html", ".html"),
        ("dir/chat", ""),
        ("dir.d/chat", ""),
        ("dir.d/.x", ".x"),
    ] {
        assert_eq!(go_ext(input), want, "go_ext({input:?})");
    }
}

// New (T-43): `json.MarshalIndent` + `SetEscapeHTML(true)` — two-space indent, sorted keys
// and the five `\uXXXX` rewrites `encoding/json` applies inside string values.
#[test]
fn go_json_indent_escapes_like_encoding_json() {
    let args: crate::provider::model::JsonObject = [
        ("z".to_owned(), serde_json::json!(1)),
        ("a".to_owned(), serde_json::json!("<b>&</b>")),
        ("m".to_owned(), serde_json::json!({"k": "\u{2028}\u{2029}"})),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        go_json_indent(&args),
        "{\n  \"a\": \"\\u003cb\\u003e\\u0026\\u003c/b\\u003e\",\n  \"m\": {\n    \"k\": \"\\u2028\\u2029\"\n  },\n  \"z\": 1\n}"
    );
}

// New: `filepath.Abs` = `Clean(Join(cwd, path))` — `std::path::absolute` alone keeps `..`.
#[test]
fn go_abs_folds_parent_components() {
    let cwd = std::env::current_dir().expect("cwd");
    assert_eq!(go_abs("a/../b.html"), cwd.join("b.html").to_string_lossy());
    assert_eq!(go_abs("./c.md"), cwd.join("c.md").to_string_lossy());
    // A rooted path is spelled by the platform, not by this test: on Windows `/tmp/y.html` is
    // rooted but NOT absolute, and it is `std::path::absolute` that supplies the drive letter —
    // the same call `go_abs` makes before it folds. On Unix the two spellings are the same string.
    let rooted = |p: &str| {
        std::path::absolute(p)
            .expect("absolute")
            .to_string_lossy()
            .into_owned()
    };
    assert_eq!(go_abs("/tmp/x/../y.html"), rooted("/tmp/y.html"));
    // `..` at the root is the root, exactly like `filepath.Clean`.
    assert_eq!(go_abs("/../z.md"), rooted("/z.md"));
    // A path with nothing to fold keeps every component it came with.
    assert_eq!(go_abs("/tmp/plain.html"), rooted("/tmp/plain.html"));
}

// New (export.go:534-541): only a leading `~/` expands, and a host with no home leaves the
// path UNCHANGED — unlike `/file`, which errors.
#[test]
fn expand_home_only_touches_a_leading_tilde_slash() {
    let home = Path::new("/home/u");
    // `home.join(rest)` is what the expansion does, and what it spells with is the platform's own
    // separator — a `~/notes.md` under `C:\Users\u` is `C:\Users\u\notes.md`.
    assert_eq!(
        expand_home("~/notes.md", Some(home)),
        home.join("notes.md").to_string_lossy()
    );
    assert_eq!(expand_home("~/notes.md", None), "~/notes.md");
    assert_eq!(expand_home("~notes.md", Some(home)), "~notes.md");
    assert_eq!(expand_home("a/~/b.md", Some(home)), "a/~/b.md");
    assert_eq!(expand_home("~", Some(home)), "~");
}

// New: the header line skips the fields a session does not have, always carries the date,
// and only carries the count when asked (export.go:137-150).
#[test]
fn meta_line_skips_missing_fields() {
    let full = meta("t", "abc123", "gpt-x");
    assert_eq!(
        export_meta_line(&full, 6, true),
        "Session abc123 · gpt-x · 2026-07-07 09:30 · 6 messages"
    );
    assert_eq!(
        export_meta_line(&full, 0, false),
        "Session abc123 · gpt-x · 2026-07-07 09:30"
    );
    assert_eq!(
        export_meta_line(&meta("t", "", ""), 1, true),
        "2026-07-07 09:30 · 1 messages"
    );
    // The title fallback (export.go:128).
    assert_eq!(export_title(&meta("  My Chat  ", "", "")), "My Chat");
    assert_eq!(export_title(&meta("   ", "", "")), "iota session");
}

// New: system messages never count as conversation, and a leading assistant run opens an
// opener-less round (export.go:154, :174-190).
#[test]
fn rounds_split_on_user_messages() {
    let msgs = vec![
        Message::assistant("orphan"),
        Message::system("s1"),
        Message::user("u1"),
        Message::assistant("a1"),
        Message::system("s2"),
        Message::user("u2"),
    ];
    assert_eq!(conversation_count(&msgs), 4);
    let (system, rounds) = split_rounds(&msgs);
    assert_eq!(
        system
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>(),
        ["s1", "s2"]
    );
    assert_eq!(rounds.len(), 3);
    assert!(rounds[0].user.is_none(), "the orphan reply opens a round");
    assert_eq!(rounds[0].replies.len(), 1);
    assert_eq!(rounds[1].user.as_ref().expect("u1").content, "u1");
    assert_eq!(rounds[1].replies.len(), 1);
    assert!(rounds[2].replies.is_empty());
}

// New: an empty history and a system-only history both refuse to export (export.go:575-578
// reads this count).
#[test]
fn a_system_only_history_has_nothing_to_export() {
    assert_eq!(conversation_count(&[]), 0);
    assert_eq!(conversation_count(&[Message::system("s")]), 0);
}

// New (export.go:329-405): the page stylesheet splices the dark palette under BOTH scopes,
// because CSS cannot merge a media query with an attribute override.
#[test]
fn page_css_carries_the_dark_palette_twice() {
    let css = export_page_css();
    assert_eq!(css.matches(EXPORT_DARK_VARS).count(), 2);
    assert!(css.starts_with(":root {\n  color-scheme: light;\n"));
    assert!(css.contains(
        "@media (prefers-color-scheme: dark) {\n  :root:not([data-theme=\"light\"]) {\n"
    ));
    assert!(css.contains("\n:root[data-theme=\"dark\"] {\n"));
    assert!(css.ends_with("img { max-width: 100%; }\n"));
}

// New (export.go:408-420): the toggle script reads and writes the one storage key and hangs
// off the one button id, both of which the document also emits.
#[test]
fn toggle_js_uses_the_pinned_hooks() {
    assert!(EXPORT_TOGGLE_JS.contains(&format!("localStorage.getItem(\"{THEME_STORAGE_KEY}\")")));
    assert!(EXPORT_TOGGLE_JS.contains(&format!(
        "localStorage.setItem(\"{THEME_STORAGE_KEY}\", next)"
    )));
    assert!(EXPORT_TOGGLE_JS.contains(&format!("getElementById(\"{THEME_TOGGLE_ID}\")")));
    let out = build_export_html(&meta("T", "", ""), &[Message::user("hi")]);
    assert!(out.contains(&format!(
        "<button id=\"{THEME_TOGGLE_ID}\" type=\"button\">Toggle theme</button>"
    )));
    assert!(out.ends_with(&format!(
        "</main>\n<script>\n{EXPORT_TOGGLE_JS}\n</script>\n</body>\n</html>\n"
    )));
}

// New (run.go:807): the picker's title and rows, and the index → format mapping the arm
// applies (`Index == 1` is Markdown, anything else HTML).
#[test]
fn picker_rows_are_html_then_markdown() {
    assert_eq!(EXPORT_PICKER_TITLE, "Export format");
    assert_eq!(EXPORT_PICKER_ITEMS, ["HTML", "Markdown"]);
}
