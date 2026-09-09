//! The v2.15.0 in-composer completion (suggest.go; `TUI_DESIGN` §6): `match_suggestions`
//! whole-line prefix filtering, first-press prefix cycling, and the two frame slots —
//! the candidates row (INSIDE the composer block) with its render-width outward-growing
//! window, and the selected candidate's description (the status row's slot).
//!
//! The row deliberately stays a HINT rather than becoming a menu: a menu would have to
//! own keys the composer already owns — Enter above all. Tab writes the candidate
//! straight into the composer instead, so completing is one key and sending is still
//! just Enter. What the row owes the user is honesty about what it is not showing: the
//! window follows the highlight and counts what it hides (`+N`), never silently
//! dropping candidates.

use crate::text::ansi::{ansi_width, truncate_ansi};
use crate::ui::facade::Suggestion;

use super::composer::Composer;
use super::theme::{CYAN, FAINT, RESET};

/// Separates candidates on the row (suggest.go:26).
const SUGGEST_GAP: &str = "  ";

/// Filters the table by whole-line prefix (suggest.go matchSuggestions). Entries whose
/// value carries an argument (`"/skills brain-page"`) stay hidden until the line has a
/// space, so `"/skills"` offers the command itself rather than its whole catalog;
/// whole-line prefix matching keeps ordinary commands quiet after their arguments
/// start — nothing registered begins with `"/file some/path"`.
pub(crate) fn match_suggestions<'a>(commands: &'a [Suggestion], line: &str) -> Vec<&'a Suggestion> {
    if !line.starts_with('/') || line.contains('\n') {
        return Vec::new();
    }
    let typed_arg = line.contains(' ');
    commands
        .iter()
        .filter(|c| (typed_arg || !c.value.contains(' ')) && c.value.starts_with(line))
        .collect()
}

/// What a candidate contributes to the row: its explicit label, else the value with
/// its leading slash dropped — a candidate shows only the part the user has not
/// already typed (suggest.go suggestLabel).
fn suggest_label(s: &Suggestion) -> &str {
    if s.label.is_empty() {
        s.value.strip_prefix('/').unwrap_or(&s.value)
    } else {
        &s.label
    }
}

/// Tab (key-table row 4): cycles the matches of the prefix captured at the FIRST
/// press, writing each candidate's value straight into the composer; Enter is never
/// claimed (model.go:421-435). A Tab with no matches is consumed as a no-op.
pub(crate) fn tab_complete(c: &mut Composer, commands: &[Suggestion]) {
    let base = if c.suggestion_base.is_empty() {
        c.value().to_owned()
    } else {
        c.suggestion_base.clone()
    };
    let ms = match_suggestions(commands, &base);
    if ms.is_empty() {
        return;
    }
    if c.suggestion_base.is_empty() {
        c.suggestion_base = base;
        c.suggestion_index = None;
    }
    let next = match c.suggestion_index {
        None => 0,
        Some(i) => (i + 1) % ms.len(),
    };
    c.suggestion_index = Some(next);
    let value = ms[next].value.clone();
    c.set_value(&value);
}

/// The two frame slots completion occupies (suggest.go suggestRows): the candidates
/// row — placed INSIDE the composer block, above the lower separator — and the
/// selected candidate's description, which takes the status row's slot ONLY while
/// something is selected (a description under an unselected row reads as if that row
/// were chosen).
pub(crate) fn frame_slots(
    composer: &Composer,
    commands: &[Suggestion],
    width: u16,
) -> (Option<String>, Option<String>) {
    let base = if composer.suggestion_base.is_empty() {
        composer.value()
    } else {
        composer.suggestion_base.as_str()
    };
    let ms = match_suggestions(commands, base);
    if ms.is_empty() {
        return (None, None);
    }
    let selected = if composer.suggestion_base.is_empty() {
        None
    } else {
        composer.suggestion_index.filter(|&i| i < ms.len())
    };
    let cycling = selected.is_some();
    let cur = selected.unwrap_or(0);
    let candidates = suggest_candidates(&ms, cur, cycling, width);
    let desc = selected.and_then(|i| {
        let d = &ms[i].desc;
        if d.is_empty() {
            None
        } else {
            Some(format!(
                "  {FAINT}{}{RESET}",
                truncate_ansi(d, usize::from(width).saturating_sub(2).max(4), "…")
            ))
        }
    });
    (Some(candidates), desc)
}

/// Lays the labels out on one row, windowed so the highlighted one is always visible
/// and counting whatever falls outside. The window is computed in WHOLE candidates —
/// half a label is not a candidate — and grown against the RENDERED width, because the
/// indent, the `"… "` marker and the `"+N"` counter all take columns the labels cannot
/// have (suggest.go suggestCandidates).
fn suggest_candidates(ms: &[&Suggestion], cur: usize, cycling: bool, width: u16) -> String {
    let labels: Vec<&str> = ms.iter().map(|s| suggest_label(s)).collect();
    let w = usize::from(width);

    // Grow outwards from the highlight: it is the one label that must survive, and
    // expanding around it keeps its neighbours — the ones Tab reaches next — in view.
    let (mut lo, mut hi) = (cur, cur + 1);
    loop {
        let mut grew = false;
        if hi < labels.len() && fits(&labels, lo, hi + 1, cur, cycling, w) {
            hi += 1;
            grew = true;
        }
        if lo > 0 && fits(&labels, lo - 1, hi, cur, cycling, w) {
            lo -= 1;
            grew = true;
        }
        if !grew {
            break;
        }
    }
    // A single label wider than the row is the one case the window cannot solve; the
    // final truncate keeps the frame intact.
    truncate_ansi(
        &render_candidates(&labels, lo, hi, cur, cycling),
        w.max(4),
        "…",
    )
}

/// Whether one window's rendered row still fits the width (suggest.go suggestFits).
fn fits(labels: &[&str], lo: usize, hi: usize, cur: usize, cycling: bool, w: usize) -> bool {
    ansi_width(&render_candidates(labels, lo, hi, cur, cycling)) <= w
}

/// Draws one window: the indent, a `"… "` when labels precede it, the labels, and a
/// count of everything outside. Candidates stay faint — the row is chrome under the
/// composer — and only the selection lifts out of it in cyan
/// (suggest.go renderCandidates).
fn render_candidates(labels: &[&str], lo: usize, hi: usize, cur: usize, cycling: bool) -> String {
    // The tool-result marker, reused: the row is a child of the composer line above
    // it, and an indent alone left it floating.
    let mut b = format!("  {FAINT}⎿ {RESET}");
    if lo > 0 {
        b.push_str(FAINT);
        b.push_str("… ");
        b.push_str(RESET);
    }
    for (i, label) in labels.iter().enumerate().take(hi).skip(lo) {
        if i > lo {
            b.push_str(SUGGEST_GAP);
        }
        if i == cur && cycling {
            b.push_str(CYAN);
        } else {
            b.push_str(FAINT);
        }
        b.push_str(label);
        b.push_str(RESET);
    }
    let hidden = labels.len() - (hi - lo);
    if hidden > 0 {
        b.push_str(FAINT);
        b.push_str("  +");
        b.push_str(&hidden.to_string());
        b.push_str(RESET);
    }
    b
}

#[cfg(test)]
mod tests {
    #![allow(dead_code)]
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    //! WP46 completion suite: `match_suggestions` argument-entry laws, Tab cycling
    //! against the first-press prefix, the render-width outward window, and the
    //! candidates/description hues (`model_test.go` + `suggest.go`).
    //!
    //! The modules under test are crate-private by design (`TUI_CONTRACTS` §5), so these tests live
    //! in-file (formerly a `#[path]`-mounted `tests/suggest.rs` of the terminal crate; merged 2026-09-02).

    use crate::text::ansi::strip_sgr;
    use crate::text::width::str_width;
    use crate::ui::composer::Composer;
    use crate::ui::facade::Suggestion;
    use crate::ui::suggest::{frame_slots, match_suggestions, tab_complete};
    use crate::ui::theme::{CYAN, FAINT, RESET};

    /// Builds a suggestion table from plain values (Go `cmdTable`).
    fn cmd_table(vals: &[&str]) -> Vec<Suggestion> {
        vals.iter()
            .map(|v| Suggestion {
                value: (*v).to_owned(),
                ..Suggestion::default()
            })
            .collect()
    }

    fn vals(ms: &[&Suggestion]) -> Vec<String> {
        ms.iter().map(|m| m.value.clone()).collect()
    }

    /// Entries whose value carries an argument stay hidden until the line has a space;
    /// whole-line prefix matching silences ordinary commands once arguments start.
    // Go: model_test.go:2079
    #[test]
    fn test_match_suggestions_argument_entries() {
        let cmds = cmd_table(&[
            "/file",
            "/skills",
            "/skills brain-page",
            "/skills code-review",
        ]);

        let got = vals(&match_suggestions(&cmds, "/skill"));
        assert_eq!(got, vec!["/skills"], "before the space: {got:?}");
        let got = vals(&match_suggestions(&cmds, "/skills"));
        assert_eq!(got, vec!["/skills"], "on the bare command: {got:?}");
        let got = match_suggestions(&cmds, "/skills ");
        assert_eq!(got.len(), 2, "after the space: {:?}", vals(&got));
        let got = vals(&match_suggestions(&cmds, "/skills br"));
        assert_eq!(got, vec!["/skills brain-page"], "narrowing: {got:?}");
        // An ordinary command's arguments still silence the list — nothing registered
        // begins with that text.
        let got = match_suggestions(&cmds, "/file some/path");
        assert!(
            got.is_empty(),
            "ordinary arguments suggested {:?}",
            vals(&got)
        );
    }

    /// A `"/"` prefix shows suggestions; Tab cycles the matches of the prefix captured at
    /// the FIRST press, writing values into the composer; the cycle wraps around.
    // Go: model_test.go:685
    #[test]
    fn test_slash_tab_completion() {
        let cmds = cmd_table(&["/file", "/session", "/status", "/model"]);
        let mut c = Composer::new();
        c.set_value("/s");
        let (cand, _) = frame_slots(&c, &cmds, 80);
        let row = strip_sgr(&cand.expect("suggestion row missing"));
        assert!(
            row.contains("session") && row.contains("status"),
            "suggestion row missing candidates:\n{row}"
        );
        tab_complete(&mut c, &cmds);
        assert_eq!(c.value(), "/session", "tab");
        tab_complete(&mut c, &cmds);
        assert_eq!(
            c.value(),
            "/status",
            "tab tab must cycle the ORIGINAL prefix"
        );
        tab_complete(&mut c, &cmds);
        assert_eq!(c.value(), "/session", "cycle wrap");
    }

    /// The row window follows the highlight: cycling all 12 candidates at width 40 keeps
    /// the highlighted label visible and the composer holding its value; whatever the
    /// window hides is counted (`+N`), and neither row ever exceeds the width.
    // Go: model_test.go:2111
    #[test]
    fn test_suggest_row_window_follows_highlight() {
        let values: Vec<String> = (0..12).map(|i| format!("/skills s{i:02}")).collect();
        let mut tbl = vec![Suggestion {
            value: "/skills".to_owned(),
            ..Suggestion::default()
        }];
        for (i, v) in values.iter().enumerate() {
            tbl.push(Suggestion {
                value: v.clone(),
                label: format!("s{i:02}"),
                ..Suggestion::default()
            });
        }
        let mut c = Composer::new();
        c.set_value("/skills ");

        for (i, want_value) in values.iter().enumerate() {
            tab_complete(&mut c, &tbl);
            let (cand, _) = frame_slots(&c, &tbl, 40);
            let rows = strip_sgr(&cand.expect("candidates row missing"));
            let want = format!("s{i:02}");
            assert!(
                rows.contains(&want),
                "highlight {i} ({want}) scrolled out of view:\n{rows}"
            );
            assert_eq!(c.value(), want_value, "step {i} composer value mismatch");
        }
        // Whatever the window hides is counted, never silently dropped.
        let (cand, desc) = frame_slots(&c, &tbl, 40);
        let cand = cand.expect("candidates row missing");
        assert!(
            strip_sgr(&cand).contains('+'),
            "no overflow count with 12 candidates in 40 columns:\n{}",
            strip_sgr(&cand)
        );
        for r in [Some(cand), desc].into_iter().flatten() {
            let w = str_width(&strip_sgr(&r));
            assert!(
                w <= 40,
                "row overflows the width ({w}): {:?}",
                strip_sgr(&r)
            );
        }
    }

    /// A candidate's row shows its bare label, never the already-typed command prefix;
    /// the description appears only once something is selected.
    // Go: model_test.go:2151
    #[test]
    fn test_suggest_rows_show_label_and_description() {
        let cmds = vec![Suggestion {
            value: "/skills brain-page".to_owned(),
            label: "brain-page".to_owned(),
            desc: "Read and write brain pages".to_owned(),
        }];
        let mut c = Composer::new();
        c.set_value("/skills ");

        let (cand, desc) = frame_slots(&c, &cmds, 80);
        assert!(
            desc.is_none(),
            "description shown before anything was selected: {desc:?}"
        );
        let got = strip_sgr(&cand.expect("candidates row missing"));
        assert!(
            !got.contains("/skills brain-page"),
            "row repeated the typed prefix:\n{got}"
        );
        assert!(got.contains("brain-page"), "row missing the label:\n{got}");

        tab_complete(&mut c, &cmds);
        let (_, desc) = frame_slots(&c, &cmds, 80);
        let desc = desc.expect("description missing after selecting");
        assert!(
            strip_sgr(&desc).contains("Read and write brain pages"),
            "description missing after selecting:\n{}",
            strip_sgr(&desc)
        );
    }

    /// The per-skill rows the agent-mode command table registers (`"/skills <name>"`, Label =
    /// the bare name) behave on the row exactly like Go's: typing `"/skills"` offers ONE
    /// candidate — the command, not its whole catalog — and only after the space do the skills
    /// appear, labelled bare. The table itself is built beside `repl::commands`; the rows are
    /// spelled out here so `ui` keeps naming nothing above it.
    // Go: chat/skillcmd_test.go:97 TestSkillCommandsCompletion (the completion-row half)
    #[test]
    fn test_skill_rows_hide_until_a_space_then_show_bare_names() {
        let cmds = vec![
            Suggestion {
                value: "/skills".to_owned(),
                label: String::new(),
                desc: "List skills, or run one: /skills <name>".to_owned(),
            },
            Suggestion {
                value: "/skills brain-page".to_owned(),
                label: "brain-page".to_owned(),
                desc: "brain pages".to_owned(),
            },
            Suggestion {
                value: "/skills code-review".to_owned(),
                label: "code-review".to_owned(),
                desc: String::new(),
            },
        ];
        // Bare: one candidate, the command itself.
        assert_eq!(vals(&match_suggestions(&cmds, "/skills")), vec!["/skills"]);
        let mut c = Composer::new();
        c.set_value("/skills");
        let (cand, _) = frame_slots(&c, &cmds, 80);
        let row = strip_sgr(&cand.expect("candidates row missing"));
        assert!(
            !row.contains("brain-page") && !row.contains("code-review"),
            "the catalog leaked before the space:\n{row}"
        );

        // After the space the skills appear, narrowed by prefix and labelled bare.
        assert_eq!(
            vals(&match_suggestions(&cmds, "/skills b")),
            vec!["/skills brain-page"]
        );
        let mut c = Composer::new();
        c.set_value("/skills b");
        let (cand, desc) = frame_slots(&c, &cmds, 80);
        let row = strip_sgr(&cand.expect("candidates row missing"));
        assert!(
            row.contains("brain-page") && !row.contains("/skills brain-page"),
            "the row must show the bare name, not the typed prefix:\n{row}"
        );
        assert!(
            desc.is_none(),
            "description shown before selecting: {desc:?}"
        );
        // Tab writes the WHOLE value back, so Enter sends `/skills brain-page`.
        tab_complete(&mut c, &cmds);
        assert_eq!(c.value(), "/skills brain-page");
        let (_, desc) = frame_slots(&c, &cmds, 80);
        assert!(
            strip_sgr(&desc.expect("description missing")).contains("brain pages"),
            "the skill's description is what makes a long catalog navigable"
        );
    }

    /// The two rows must not read as one list: candidates stay faint chrome with only the
    /// selection lifted out in cyan; no leading slash is repeated; the row carries the
    /// faint continuation marker; the description stays faint.
    // Go: model_test.go:2177
    #[test]
    fn test_suggest_row_hues() {
        let cmds = vec![
            Suggestion {
                value: "/status".to_owned(),
                label: String::new(),
                desc: "Provider, tokens, tools".to_owned(),
            },
            Suggestion {
                value: "/session".to_owned(),
                label: String::new(),
                desc: "Resume or delete".to_owned(),
            },
        ];
        let mut c = Composer::new();
        c.set_value("/s");
        tab_complete(&mut c, &cmds);

        let (cand, desc) = frame_slots(&c, &cmds, 80);
        let cand = cand.expect("candidates row missing");
        let desc = desc.expect("description missing");
        // The row is chrome under the composer: faint throughout, with only the
        // selection lifted out of it.
        assert!(
            cand.contains(&format!("{CYAN}status{RESET}")),
            "selected candidate not cyan:\n{cand:?}"
        );
        assert!(
            cand.contains(&format!("{FAINT}session{RESET}")),
            "unselected candidate not faint:\n{cand:?}"
        );
        // A command shows without the slash the composer already carries.
        let plain = strip_sgr(&cand);
        assert!(
            !plain.contains("/status"),
            "candidate repeated the leading slash:\n{plain}"
        );
        assert!(
            plain.starts_with("  ⎿ "),
            "row missing the continuation marker:\n{plain:?}"
        );
        assert!(desc.contains(FAINT), "description not faint:\n{desc:?}");
    }
}
