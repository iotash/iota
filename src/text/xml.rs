//! A message that IS one element of a given name (`<hc from="agent">the message</hc>`, how
//! herdr-core hands a sub-agent's message to the conversation): its attributes and its inner text.
//! A strict scanner, not a parser, and it parses ONE name only — the one the caller passes, which is
//! the element a host wraps its own messages in (`host::MessageEnvelope`). Any other name, and
//! anything that is not exactly one element, is refused and the message is shown as it was
//! written: a user who pastes an XML snippet keeps it.
//!
//! The grammar, over the message trimmed of surrounding whitespace, with `N` the claimed name:
//!
//! ```text
//! element := '<' N (ws+ attr)* ws* '>' inner '</' N ws* '>'
//! attr    := name (ws* '=' ws* ( '"' [^"]* '"' | '\'' [^']* '\'' ))?
//! name    := [A-Za-z_] [A-Za-z0-9_.:-]*
//! ```
//!
//! Refused: a different name, anything before the `<` or after the closing `>`, a self-closing
//! `<N/>`, an unquoted value, an unclosed or mismatched closing tag, and an unbalanced inner text —
//! one that closes the element early (`<hc>x</hc><hc>y</hc>` is two elements, not one) or leaves an
//! inner element of the same name open (`<hc><hc>x</hc>`: the outer one is unclosed). Entity references are left
//! as they are, in the values and the inner text alike.

/// The parts of a message that is one element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Element<'a> {
    /// The tag name.
    pub name: &'a str,
    /// The attributes in document order: a bare attribute has no value.
    pub attrs: Vec<(&'a str, Option<&'a str>)>,
    /// What the element encloses, trimmed of surrounding whitespace.
    pub inner: &'a str,
}

/// `msg` as exactly one element named `claimed`, or `None` when it is anything else (module docs).
pub fn claimed_element<'a>(msg: &'a str, claimed: &str) -> Option<Element<'a>> {
    let s = msg.trim();
    let rest = s.strip_prefix('<')?;
    let (name, mut rest) = take_name(rest)?;
    if name != claimed {
        return None;
    }
    let mut attrs = Vec::new();
    loop {
        let trimmed = rest.trim_start();
        if let Some(r) = trimmed.strip_prefix('>') {
            rest = r;
            break;
        }
        // An attribute needs a blank before it; `/>` and anything else ends the scan.
        if trimmed.len() == rest.len() {
            return None;
        }
        let (key, r) = take_name(trimmed)?;
        let after = r.trim_start();
        if let Some(r) = after.strip_prefix('=') {
            let r = r.trim_start();
            let quote = r.chars().next().filter(|c| matches!(c, '"' | '\''))?;
            let body = &r[1..];
            let end = body.find(quote)?;
            attrs.push((key, Some(&body[..end])));
            rest = &body[end + 1..];
        } else {
            attrs.push((key, None));
            rest = r;
        }
    }
    let close = format!("</{name}");
    let at = rest.rfind(&close)?;
    if rest[at + close.len()..].trim_start() != ">" {
        return None;
    }
    let inner = &rest[..at];
    if !balanced(inner, name) {
        return None;
    }
    Some(Element {
        name,
        attrs,
        inner: inner.trim(),
    })
}

/// A name at the start of `s` and what follows it.
fn take_name(s: &str) -> Option<(&str, &str)> {
    let first = s.chars().next()?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    let end = s
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-')))
        .unwrap_or(s.len());
    Some((&s[..end], &s[end..]))
}

/// Whether every `<name` that `inner` opens is closed inside it, and no `</name>` in it closes the
/// outer element early: otherwise the outer element's close is an inner one's
/// (`<hc><hc>x</hc>` leaves the outer unclosed) or the message is two elements.
fn balanced(inner: &str, name: &str) -> bool {
    let mut depth = 0usize;
    let mut i = 0;
    while let Some(j) = inner[i..].find('<') {
        let at = i + j + 1;
        let tail = &inner[at..];
        if let Some(t) = tail.strip_prefix('/') {
            if let Some((n, r)) = take_name(t)
                && n == name
                && r.trim_start().starts_with('>')
            {
                if depth == 0 {
                    return false;
                }
                depth -= 1;
            }
        } else if let Some((n, r)) = take_name(tail)
            && n == name
            && r.starts_with(|c: char| c == '>' || c.is_whitespace())
        {
            depth += 1;
        }
        i = at;
    }
    depth == 0
}

#[cfg(test)]
mod tests {
    use super::{Element, claimed_element};

    /// herdr-core's name, the one a test claims.
    fn hc(msg: &str) -> Option<Element<'_>> {
        claimed_element(msg, "hc")
    }

    fn el<'a>(attrs: &[(&'a str, Option<&'a str>)], inner: &'a str) -> Element<'a> {
        Element {
            name: "hc",
            attrs: attrs.to_vec(),
            inner,
        }
    }

    #[test]
    fn the_claimed_element_with_no_attributes() {
        assert_eq!(hc("<hc>hi</hc>"), Some(el(&[], "hi")));
        // Surrounding whitespace is not text outside the element.
        assert_eq!(hc("  \n<hc >hi</hc >\n"), Some(el(&[], "hi")));
    }

    #[test]
    fn attributes_in_both_quote_styles_keep_their_order() {
        assert_eq!(
            hc(r#"<hc from="worker-2" to='lead' n = "3">done</hc>"#),
            Some(el(
                &[
                    ("from", Some("worker-2")),
                    ("to", Some("lead")),
                    ("n", Some("3"))
                ],
                "done"
            ))
        );
    }

    #[test]
    fn a_bare_attribute_has_no_value() {
        assert_eq!(
            hc(r#"<hc urgent from="a">x</hc>"#),
            Some(el(&[("urgent", None), ("from", Some("a"))], "x"))
        );
    }

    #[test]
    fn a_quoted_value_may_hold_spaces_and_a_closing_bracket() {
        assert_eq!(
            hc(r#"<hc title="a > b, c" by='x y'>ok</hc>"#),
            Some(el(
                &[("title", Some("a > b, c")), ("by", Some("x y"))],
                "ok"
            ))
        );
    }

    #[test]
    fn the_inner_text_keeps_newlines_angle_brackets_and_entities() {
        assert_eq!(
            hc("<hc from=\"a\">line one\nif a < b && c > d &amp; <b>x</b></hc>"),
            Some(el(
                &[("from", Some("a"))],
                "line one\nif a < b && c > d &amp; <b>x</b>"
            ))
        );
        // A nested element of the same name is closed inside, so the outer one still holds.
        assert_eq!(hc("<hc><hc>x</hc> y</hc>"), Some(el(&[], "<hc>x</hc> y")));
    }

    #[test]
    fn an_empty_inner_text() {
        assert_eq!(
            hc(r#"<hc from="a"></hc>"#),
            Some(el(&[("from", Some("a"))], ""))
        );
    }

    /// Only the claimed name is parsed: every other element, however well formed, is the user's text.
    #[test]
    fn every_other_name_is_left_alone() {
        for msg in [
            "<note>hi</note>",
            r#"<foo from="a">x</foo>"#,
            "<hcx>x</hcx>",
            "<HC>x</HC>",
            "<h>x</h>",
        ] {
            assert_eq!(hc(msg), None, "{msg:?}");
        }
        // The same message is claimed by the host whose name it carries.
        assert_eq!(
            claimed_element("<note>hi</note>", "note").map(|e| e.inner),
            Some("hi")
        );
    }

    #[test]
    fn everything_else_is_refused() {
        for msg in [
            "<hc>x</hd>",                   // mismatched closing tag
            "<hc>x",                        // unclosed
            "<hc>x</hc> and more",          // trailing text
            "see <hc>x</hc>",               // text before
            "<hc/>",                        // self-closing
            r#"<hc from="a"/>"#,            // self-closing with an attribute
            "<hc>x</hc><hc>y</hc>",         // two elements
            "<hc><hc>x</hc>",               // the outer element unclosed
            "<hc from=a>x</hc>",            // unquoted value
            r#"<hc from="a>x</hc>"#,        // unterminated value
            r#"<hc from="a"to="b">x</hc>"#, // no blank between attributes
            "< hc>x</hc>",                  // no name
            "hello, world",                 // not XML at all
            "a < b and b > c",
            "",
        ] {
            assert_eq!(hc(msg), None, "{msg:?}");
        }
    }
}
