//! Markdown → HTML for `/export` (T3 design D6): comrak in safe mode with the GFM extensions
//! (goldmark parity: raw HTML → `<!-- raw HTML omitted -->`) and [`ChromaAdapter`], a
//! `SyntaxHighlighterAdapter` over the two-face syntax set `markdown::highlight` already holds,
//! emitting chroma-shaped `<pre class="chroma">` blocks.
//! The token CSS is structure-only parity (chroma's classes ≠ syntect's — T-42): `Github` for
//! light, `OneHalfDark` standing in for chroma's `github-dark`.
//!
//! Everything OUTSIDE a fenced code block is byte-exact against goldmark's output for the
//! shapes assistants actually emit — paragraphs, emphasis, lists, tables, blockquotes,
//! task lists, autolinks and the safe-mode raw-HTML comment (`tests` beside this module
//! diff the Go reference bytes). Inside a fence the wrapper tags are Go's exactly
//! (`<pre class="chroma"><code>` when the language resolves, the plain
//! `<pre><code class="language-X">` when it does not — goldmark-highlighting falls through
//! to the default renderer whenever chroma has no lexer), while the token spans and their
//! classes are syntect's.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::Write as _;

use syntect::highlighting::{FontStyle, Theme};
use two_face::theme::EmbeddedThemeName;

use crate::markdown::highlight::{resolve, syntaxes, themes};

/// The light code theme (chroma `github`).
pub(crate) const HTML_LIGHT_THEME: EmbeddedThemeName = EmbeddedThemeName::Github;
/// The dark code theme (standing in for chroma `github-dark`; two-face has no such dump).
pub(crate) const HTML_DARK_THEME: EmbeddedThemeName = EmbeddedThemeName::OneHalfDark;
/// The root class of every highlighted block and of every token rule.
pub(crate) const CODE_ROOT_CLASS: &str = "chroma";

/// Renders `md` (comrak: `table`, `strikethrough`, `autolink`, `tasklist`; `unsafe = false`;
/// `github_pre_lang = false`; code fences through [`ChromaAdapter`]).
///
/// The option set is goldmark's `extension.GFM` in its default SAFE mode, which is what
/// `exportGoldmark` (chat/export.go:271-278) asks for: model output is never trusted with
/// raw HTML.
pub(crate) fn markdown_to_html(md: &str) -> String {
    let mut opts = comrak::Options::default();
    opts.extension.table = true;
    opts.extension.strikethrough = true;
    opts.extension.autolink = true;
    opts.extension.tasklist = true;
    // Safe mode: raw HTML becomes `<!-- raw HTML omitted -->`, exactly as goldmark does
    // (comrak html.rs:679,729).
    opts.render.r#unsafe = false;
    opts.render.escape = false;
    // `false` keeps the info string on the `<code>` tag rather than the `<pre>` tag, so the
    // adapter can read the language back out of the attributes it is handed.
    opts.render.github_pre_lang = false;

    let adapter = ChromaAdapter;
    let mut plugins = comrak::options::Plugins::default();
    plugins.render.codefence_syntax_highlighter = Some(&adapter);
    comrak::markdown_to_html_with_plugins(md, &opts, &plugins)
}

/// The code-fence renderer comrak calls (comrak `adapters::SyntaxHighlighterAdapter`).
///
/// The unit struct holds no state: the syntax set and the themes are the process-wide
/// lazies `markdown::highlight` already owns.
pub(crate) struct ChromaAdapter;

/// Which wrapper a fence gets, decided once from the info string.
enum Fence<'a> {
    /// The language resolved: chroma's wrapper and syntect's token spans.
    Highlighted,
    /// No language, or one no grammar claims: goldmark's default renderer shape, keeping
    /// whatever `class` comrak built.
    Plain(Option<&'a str>),
}

impl ChromaAdapter {
    /// Reads the language back out of the `class="language-X"` attribute comrak builds
    /// (comrak html.rs:543 with `github_pre_lang = false`) and decides the wrapper.
    fn fence<'a>(attributes: &'a HashMap<&'static str, Cow<'a, str>>) -> Fence<'a> {
        let class = attributes.get("class").map(std::convert::AsRef::as_ref);
        let lang = class
            .and_then(|c| c.strip_prefix("language-"))
            .unwrap_or("");
        if !lang.is_empty() && resolve(syntaxes(), lang).is_some() {
            return Fence::Highlighted;
        }
        Fence::Plain(class)
    }
}

impl comrak::adapters::SyntaxHighlighterAdapter for ChromaAdapter {
    fn write_highlighted(
        &self,
        output: &mut dyn std::fmt::Write,
        lang: Option<&str>,
        code: &str,
    ) -> std::fmt::Result {
        let set = syntaxes();
        let Some(syntax) = lang.filter(|l| !l.is_empty()).and_then(|l| resolve(set, l)) else {
            // Unknown (or absent) language: the text lands escaped inside the plain
            // `<pre><code>` wrapper `write_code_tag` opened — goldmark-highlighting's
            // fall-through when chroma has no lexer.
            return output.write_str(&escape_html(code));
        };
        let mut spans = syntect::html::ClassedHTMLGenerator::new_with_class_style(
            syntax,
            set,
            syntect::html::ClassStyle::Spaced,
        );
        for line in syntect::util::LinesWithEndings::from(code) {
            if spans
                .parse_html_for_line_which_includes_newline(line)
                .is_err()
            {
                // A grammar that blew up mid-parse: the half-built spans are already wrong,
                // so the block falls back to plain escaped text (the terminal highlighter
                // makes the same call — highlight.rs `render`).
                return output.write_str(&escape_html(code));
            }
        }
        output.write_str(&spans.finalize())
    }

    /// Writes NOTHING.
    ///
    /// comrak splits the wrapper across two calls, but `pre_attributes` is empty under
    /// `github_pre_lang = false`, so the `<pre>` tag cannot be decided until the language
    /// is known. Both tags are therefore emitted by [`Self::write_code_tag`], which does
    /// see the info string; comrak closes them with its own `</code></pre>`.
    fn write_pre_tag(
        &self,
        _output: &mut dyn std::fmt::Write,
        _attributes: HashMap<&'static str, Cow<'_, str>>,
    ) -> std::fmt::Result {
        Ok(())
    }

    /// Writes the whole `<pre …><code …>` wrapper (see [`Self::write_pre_tag`]).
    fn write_code_tag(
        &self,
        output: &mut dyn std::fmt::Write,
        attributes: HashMap<&'static str, Cow<'_, str>>,
    ) -> std::fmt::Result {
        match ChromaAdapter::fence(&attributes) {
            Fence::Highlighted => {
                write!(output, "<pre class=\"{CODE_ROOT_CLASS}\"><code>")
            }
            Fence::Plain(None) => output.write_str("<pre><code>"),
            Fence::Plain(Some(class)) => {
                write!(output, "<pre><code class=\"{}\">", escape_html(class))
            }
        }
    }
}

/// One `selector { … }` rule per line over `theme.scopes`, rooted at `.chroma`, with a first line
/// `.chroma { color: <fg>; background-color: <bg>; }` from the theme settings.
///
/// syntect's own `css_for_theme_with_class_style` emits a header comment and multi-line
/// blocks under a `.code` root (syntect html.rs:146-200), which
/// [`prefix_css_selectors`] — a verbatim port of Go's `prefixCSSSelectors` — cannot
/// prefix; this is the one-rule-per-line emitter the port needs, and it reproduces
/// chroma's `WriteCSS(WithCSSComments(false))` shape.
///
/// Only the foreground and the three font attributes are emitted. A theme's per-scope
/// BACKGROUND is deliberately dropped: syntect themes paint alarm backgrounds on
/// `invalid`/`illegal` scopes for anything a grammar cannot parse, which is precisely the
/// "a parse failure deserves no styling at all" law the terminal highlighter already
/// enforces (highlight.rs, law 2).
pub(crate) fn theme_css_rules(theme: &Theme) -> String {
    let mut out = String::new();
    let mut root = String::new();
    if let Some(fg) = theme.settings.foreground {
        let _ = write!(root, " color: #{:02x}{:02x}{:02x};", fg.r, fg.g, fg.b);
    }
    if let Some(bg) = theme.settings.background {
        let _ = write!(
            root,
            " background-color: #{:02x}{:02x}{:02x};",
            bg.r, bg.g, bg.b
        );
    }
    let _ = writeln!(out, ".{CODE_ROOT_CLASS} {{{root} }}");

    for item in &theme.scopes {
        let mut decls = String::new();
        if let Some(fg) = item.style.foreground {
            let _ = write!(decls, " color: #{:02x}{:02x}{:02x};", fg.r, fg.g, fg.b);
        }
        if let Some(fs) = item.style.font_style {
            if fs.contains(FontStyle::BOLD) {
                decls.push_str(" font-weight: bold;");
            }
            if fs.contains(FontStyle::ITALIC) {
                decls.push_str(" font-style: italic;");
            }
            if fs.contains(FontStyle::UNDERLINE) {
                decls.push_str(" text-decoration: underline;");
            }
        }
        if decls.is_empty() {
            continue;
        }
        for sel in &item.scope.selectors {
            let Some(sel) = class_selector(sel) else {
                continue;
            };
            let _ = writeln!(out, ".{CODE_ROOT_CLASS} {sel} {{{decls} }}");
        }
    }
    out
}

/// One selector's `.atom.atom .atom` form (syntect's private `scope_to_selector`, whose
/// class atoms [`syntect::html::ClassStyle::Spaced`] emits on the spans).
fn class_selector(sel: &syntect::highlighting::ScopeSelector) -> Option<String> {
    let scopes = sel.extract_scopes();
    let mut out = String::new();
    for scope in &scopes {
        let name = scope.build_string();
        let mut wrote = false;
        for atom in name.split('.').filter(|a| !a.is_empty()) {
            if !wrote && !out.is_empty() {
                out.push(' ');
            }
            wrote = true;
            out.push('.');
            out.push_str(&escape_css_identifier(atom));
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

/// syntect's `escape_css_identifier` (html.rs:280-294), so a selector and the class the
/// span carries agree on every non-`[A-Za-z_-]` byte.
fn escape_css_identifier(id: &str) -> String {
    let mut out = String::with_capacity(id.len());
    for (i, c) in id.char_indices() {
        if c.is_ascii_alphabetic() || c == '-' || c == '_' || (i > 0 && c.is_ascii_digit()) {
            out.push(c);
        } else {
            let _ = write!(out, "\\{:x} ", c as u32);
        }
    }
    out
}

/// The `exportChromaCSS` twin (chat/export.go:293-310): light rules, the dark rules under
/// `@media (prefers-color-scheme: dark)` prefixed `html:not([data-theme="light"])`, then the dark
/// rules prefixed `html[data-theme="dark"]`.
pub(crate) fn code_css() -> String {
    let light = theme_css_rules(themes().get(HTML_LIGHT_THEME));
    let dark = theme_css_rules(themes().get(HTML_DARK_THEME));
    let mut b = String::with_capacity(light.len() + dark.len() * 2 + 64);
    b.push_str(&light);
    b.push_str("@media (prefers-color-scheme: dark) {\n");
    b.push_str(&prefix_css_selectors(
        &dark,
        "html:not([data-theme=\"light\"])",
    ));
    b.push_str("}\n");
    b.push_str(&prefix_css_selectors(&dark, "html[data-theme=\"dark\"]"));
    b
}

/// Prefixes every selector of `css` with `prefix` (chat/export.go:315-325; blank lines dropped).
pub(crate) fn prefix_css_selectors(css: &str, prefix: &str) -> String {
    let mut b = String::with_capacity(css.len() + css.len() / 4);
    for line in css.split('\n') {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        b.push_str(prefix);
        b.push(' ');
        b.push_str(line);
        b.push('\n');
    }
    b
}

/// `html.EscapeString` (Go `html`): the five entities, in Go's spelling.
///
/// A private twin of `repl::commands::export::html_escape` — `markdown` sits below `repl`
/// in the layering and may not reach up into it.
fn escape_html(s: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::{
        HTML_DARK_THEME, HTML_LIGHT_THEME, code_css, markdown_to_html, prefix_css_selectors,
        theme_css_rules, themes,
    };
    use pretty_assertions::assert_eq;

    // Go: chat/export.go:271-278 (`exportGoldmark`, SAFE mode) — raw HTML in model output
    // is replaced by goldmark's exact comment, inline and block alike. Reference bytes from
    // the Go renderer.
    #[test]
    fn raw_html_is_omitted_in_safe_mode() {
        assert_eq!(
            markdown_to_html("text <b>bold</b> more\n\n<div>block</div>\n"),
            "<p>text <!-- raw HTML omitted -->bold<!-- raw HTML omitted --> more</p>\n<!-- raw HTML omitted -->\n"
        );
    }

    // Go: chat/export.go:273 (`extension.GFM`) — tables, strikethrough, task lists and
    // linkified bare URLs all render. The table/`<del>`/autolink bytes are goldmark's
    // exactly; the task-list input shape is comrak's (T-42 note).
    #[test]
    fn gfm_extensions_render() {
        let out = markdown_to_html(
            "| a | b |\n|---|---|\n| 1 | 2 |\n\n~~gone~~\n\n- [ ] todo\n- [x] done\n\nhttps://example.com\n",
        );
        assert!(
            out.starts_with(
                "<table>\n<thead>\n<tr>\n<th>a</th>\n<th>b</th>\n</tr>\n</thead>\n<tbody>\n<tr>\n<td>1</td>\n<td>2</td>\n</tr>\n</tbody>\n</table>\n"
            ),
            "{out}"
        );
        assert!(out.contains("<p><del>gone</del></p>\n"), "{out}");
        assert!(out.contains("type=\"checkbox\""), "{out}");
        assert!(
            out.contains("<p><a href=\"https://example.com\">https://example.com</a></p>\n"),
            "{out}"
        );
    }

    // Go: chat/export_test.go:139-146 — the assistant's Markdown is rendered (emphasis
    // included) and a fenced block carries the chroma root class. The inline/paragraph
    // bytes are goldmark's exactly.
    #[test]
    fn inline_markdown_matches_goldmark() {
        assert_eq!(
            markdown_to_html("a `code` span and *em* and **strong**\n"),
            "<p>a <code>code</code> span and <em>em</em> and <strong>strong</strong></p>\n"
        );
        assert_eq!(
            markdown_to_html("> quoted\n>\n> more\n"),
            "<blockquote>\n<p>quoted</p>\n<p>more</p>\n</blockquote>\n"
        );
        assert_eq!(
            markdown_to_html("It says **x**.\n"),
            "<p>It says <strong>x</strong>.</p>\n"
        );
    }

    // Go: chat/export_test.go:144 — a resolvable fence gets chroma's wrapper
    // (`<pre class="chroma"><code>`, no attributes on the code tag) and token spans.
    #[test]
    fn known_language_gets_the_chroma_wrapper() {
        let out = markdown_to_html("```go\nfunc main() {}\n```\n");
        assert!(
            out.starts_with("<pre class=\"chroma\"><code>"),
            "wrapper: {out}"
        );
        assert!(out.ends_with("</code></pre>\n"), "close: {out}");
        assert!(out.contains("<span class=\""), "no token spans: {out}");
        assert!(out.contains("main"), "code text lost: {out}");
    }

    // Go: chat/export.go:271-278 — goldmark-highlighting hands a fence back to the default
    // renderer whenever chroma has no lexer, so a bare fence and an unknown language keep
    // goldmark's plain shape byte for byte.
    #[test]
    fn unknown_and_bare_fences_keep_the_plain_wrapper() {
        assert_eq!(
            markdown_to_html("```\nplain text\n```\n"),
            "<pre><code>plain text\n</code></pre>\n"
        );
        assert_eq!(
            markdown_to_html("```notalanguage\nsome code\n```\n"),
            "<pre><code class=\"language-notalanguage\">some code\n</code></pre>\n"
        );
    }

    // New: fence text is escaped on every path — a `<script>` inside a code block must never
    // reach the document as a tag, highlighted or not. (Highlighting splits the text across
    // token spans, so only the unhighlighted paths carry the entity run contiguously.)
    #[test]
    fn fence_text_is_escaped() {
        for md in [
            "```\n<script>alert(1)</script>\n```\n",
            "```notalanguage\n<script>alert(1)</script>\n```\n",
            "```html\n<script>alert(1)</script>\n```\n",
        ] {
            let out = markdown_to_html(md);
            assert!(!out.contains("<script>"), "{md} → {out}");
            assert!(!out.contains("</script>"), "{md} → {out}");
            assert!(out.contains("&lt;"), "{md} → {out}");
        }
        for md in [
            "```\n<script>alert(1)</script>\n```\n",
            "```notalanguage\n<script>alert(1)</script>\n```\n",
        ] {
            assert!(markdown_to_html(md).contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        }
    }

    // Go: chat/export.go:315-325 `prefixCSSSelectors` — one prefixed rule per non-blank
    // trimmed line, blank lines dropped, trailing newline on every row.
    #[test]
    fn prefix_css_selectors_ports_verbatim() {
        assert_eq!(
            prefix_css_selectors(".a { color: #fff; }\n\n  .b { color: #000; }\n", "html.x"),
            "html.x .a { color: #fff; }\nhtml.x .b { color: #000; }\n"
        );
        assert_eq!(prefix_css_selectors("", "html.x"), "");
        assert_eq!(prefix_css_selectors("\n \n", "html.x"), "");
    }

    // Go: chat/export.go:293-310 — chroma's `WriteCSS(WithCSSComments(false))` shape: one
    // `selector { … }` per line, the root rule first, so `prefixCSSSelectors` can prefix
    // every line blindly.
    #[test]
    fn theme_rules_are_one_per_line_under_the_chroma_root() {
        for name in [HTML_LIGHT_THEME, HTML_DARK_THEME] {
            let css = theme_css_rules(themes().get(name));
            let mut lines = css.lines();
            let first = lines.next().expect("a root rule");
            assert!(
                first.starts_with(".chroma { ") && first.ends_with(" }"),
                "{name:?} root: {first}"
            );
            assert!(first.contains("color: #"), "{name:?} root: {first}");
            let mut n = 0;
            for line in lines {
                assert!(
                    line.starts_with(".chroma .") && line.ends_with(" }"),
                    "{name:?}: {line}"
                );
                assert_eq!(line.matches('{').count(), 1, "{name:?}: {line}");
                n += 1;
            }
            assert!(n > 20, "{name:?}: only {n} token rules");
            assert!(css.ends_with('\n'));
        }
    }

    // Go: chat/export_test.go:154 — the forced-dark scope is present, and so is the media
    // query the system-dark half hangs off (export_test.go:148).
    #[test]
    fn code_css_carries_both_dark_scopes() {
        let css = code_css();
        assert!(css.contains("@media (prefers-color-scheme: dark) {\n"));
        assert!(css.contains("html:not([data-theme=\"light\"]) .chroma"));
        assert!(css.contains("html[data-theme=\"dark\"] .chroma"));
        // The light half leads, unprefixed.
        assert!(css.starts_with(".chroma { "), "{}", &css[..80]);
        // Nothing but one-line rules, the media open/close, and the file ends closed.
        for line in css.lines() {
            assert!(
                line == "@media (prefers-color-scheme: dark) {"
                    || line == "}"
                    || (line.contains(" { ") && line.ends_with(" }")),
                "stray line: {line}"
            );
        }
    }
}
