#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The key tables, pinned row by row (`TUI_CONTRACTS` §6; Phase 5 PR-15's safety net): the
//! composer precedence ladder of `keys.rs` (rows 1–7), the enumerated edit set it hands the
//! composer (rows 8–9, `Composer::handle_edit_key`), and the same edit set as the surface's
//! one-line `Field` implements it — the two editors PR-15 folds into one. Only add tests here;
//! a behaviour these pin that reads wrong is a decision for that PR, not a fix on the way.
//!
//! Every test drives decoded crossterm events, because that is where the tree's own key
//! handling begins: the byte level — a lone ESC against an ESC-prefixed sequence, CSI against
//! SS3 arrows and `Home`/`End`, the bracketed-paste markers — is crossterm's parser, and it
//! hands this code `KeyCode::Esc`, `Char('b')` + ALT, `KeyCode::Home`, `Event::Paste`. What
//! the tables do with each decoded form is pinned below (the real byte path runs under tmux
//! in `tests/ui_tmux`).

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::ui::event_loop::Model;
use crate::ui::facade::{Input, Panel, Suggestion, TabbedResult, TabbedSpec, UiError};
use crate::ui::msgs::UiMsg;
use crate::ui::surface::field::Field;
use crate::ui::testutil::{ch, ctrl, down, enter, key, test_model, type_text, up};

/// A key with explicit modifiers.
fn mods(code: KeyCode, m: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, m)
}

/// A key event of a kind other than `Press` (kitty / Windows report releases and repeats).
fn kind(code: KeyCode, k: KeyEventKind) -> KeyEvent {
    KeyEvent::new_with_kind(code, KeyModifiers::NONE, k)
}

/// Parks a `read_input` caller; the receiver sees what the loop hands it.
fn park(m: &mut Model, id: u64) -> oneshot::Receiver<Result<Input, UiError>> {
    let (tx, rx) = oneshot::channel();
    m.apply(UiMsg::ReadReq { id, reply: tx });
    rx
}

/// Installs the slash-command table.
fn commands(m: &mut Model, values: &[&str]) {
    m.apply(UiMsg::Commands(
        values
            .iter()
            .map(|v| Suggestion {
                value: (*v).to_owned(),
                label: String::new(),
                desc: String::new(),
            })
            .collect(),
    ));
}

/// Submits each entry to a parked reader, so it enters the history without queueing.
fn history(m: &mut Model, entries: &[&str]) {
    for (i, e) in entries.iter().enumerate() {
        let mut rx = park(m, u64::try_from(i).unwrap() + 100);
        type_text(m, e);
        enter(m);
        rx.try_recv()
            .expect("the reader was not served")
            .expect("read err");
    }
}

/// Opens a one-panel list surface; the receiver sees the result it closes with.
fn open_surface(m: &mut Model) -> oneshot::Receiver<TabbedResult> {
    let (tx, rx) = oneshot::channel();
    m.apply(UiMsg::TabbedOpen {
        spec: TabbedSpec {
            panels: vec![Panel::list("/model".to_owned(), vec!["a".to_owned()])],
            ..TabbedSpec::default()
        },
        reply: tx,
    });
    rx
}

/// The composer's cursor as (column including the 2-col prompt, visible row) at 80 columns.
fn cursor(m: &Model) -> (u16, u16) {
    m.composer.cursor_pos(80)
}

// ---- the ladder --------------------------------------------------------------------------

/// Row 0: only `KeyEventKind::Press` is routed. A release or a repeat of a text key inserts
/// nothing, of Enter submits nothing, of Ctrl+C / ESC fires no scope.
#[test]
fn only_key_presses_are_routed() {
    let mut m = test_model();
    let turn = CancellationToken::new();
    m.apply(UiMsg::ScopePush(turn.clone()));
    for k in [KeyEventKind::Release, KeyEventKind::Repeat] {
        m.handle_key(kind(KeyCode::Char('x'), k));
        m.handle_key(kind(KeyCode::Enter, k));
        m.handle_key(KeyEvent::new_with_kind(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
            k,
        ));
        m.handle_key(kind(KeyCode::Esc, k));
    }
    assert_eq!(m.composer.value(), "", "a non-press must not insert");
    assert!(m.queue.is_empty(), "a non-press must not submit");
    assert!(!turn.is_cancelled(), "a non-press must not fire a scope");
    assert_eq!(m.cancels.len(), 1);
}

/// Row 1: an open surface owns EVERY key. Ctrl+C and ESC close it cancelled and never touch
/// the turn scope or a parked reader; text, Tab and Enter never reach the composer — Enter
/// commits the surface instead.
#[test]
fn row_1_an_open_surface_owns_every_key() {
    let mut m = test_model();
    let turn = CancellationToken::new();
    m.apply(UiMsg::ScopePush(turn.clone()));
    commands(&mut m, &["/model"]);
    type_text(&mut m, "/mo");

    let mut surface = open_surface(&mut m);
    m.handle_key(ctrl('c'));
    assert!(surface.try_recv().expect("surface reply").cancelled);
    assert!(
        !turn.is_cancelled(),
        "Ctrl+C in a surface must not fire the turn"
    );
    assert_eq!(m.cancels.len(), 1);

    let mut surface = open_surface(&mut m);
    m.handle_key(key(KeyCode::Esc));
    assert!(surface.try_recv().expect("surface reply").cancelled);
    assert!(
        !turn.is_cancelled(),
        "ESC in a surface must not fire a scope"
    );

    m.apply(UiMsg::ScopePop);
    let mut reader = park(&mut m, 1);
    let mut surface = open_surface(&mut m);
    m.handle_key(ch('x'));
    m.handle_key(key(KeyCode::Tab));
    assert_eq!(
        m.composer.value(),
        "/mo",
        "surface keys must not edit the composer"
    );
    assert!(
        m.composer.suggestion_base.is_empty(),
        "Tab must not start a composer cycle"
    );
    m.handle_key(ctrl('c'));
    assert!(surface.try_recv().expect("surface reply").cancelled);
    assert!(
        reader.try_recv().is_err(),
        "Ctrl+C in a surface must not interrupt the reader"
    );

    let mut surface = open_surface(&mut m);
    m.handle_key(key(KeyCode::Enter));
    let r = surface.try_recv().expect("Enter must commit the surface");
    assert!(!r.cancelled);
    assert_eq!(
        m.composer.value(),
        "/mo",
        "Enter in a surface must not submit the draft"
    );
    assert!(m.queue.is_empty() && reader.try_recv().is_err());
}

/// Row 2: Ctrl+C and Ctrl+D with scopes fire the TURN scope (index 0), taking every scope
/// above it with it.
#[test]
fn row_2_ctrl_c_and_ctrl_d_fire_the_turn_scope_and_everything_above() {
    for k in [ctrl('c'), ctrl('d')] {
        let mut m = test_model();
        let turn = CancellationToken::new();
        let tool = CancellationToken::new();
        m.apply(UiMsg::ScopePush(turn.clone()));
        m.apply(UiMsg::ScopePush(tool.clone()));
        m.handle_key(k);
        assert!(turn.is_cancelled() && tool.is_cancelled(), "{k:?}");
        assert!(m.cancels.is_empty(), "{k:?}: the stack must be drained");
    }
}

/// Row 2 at idle: with no scope, Ctrl+C / Ctrl+D hand a parked reader `Err(Interrupted)`;
/// with nobody parked they do nothing at all (no insert, no submit).
#[test]
fn row_2_idle_ctrl_c_or_ctrl_d_interrupts_the_parked_reader() {
    for k in [ctrl('c'), ctrl('d')] {
        let mut m = test_model();
        type_text(&mut m, "draft");
        let mut reader = park(&mut m, 1);
        m.handle_key(k);
        assert_eq!(
            reader.try_recv().expect("the reader must be answered"),
            Err(UiError::Interrupted),
            "{k:?}"
        );
        assert_eq!(
            m.composer.value(),
            "draft",
            "{k:?}: the draft survives an interrupt"
        );

        m.handle_key(k); // nobody parked now
        assert_eq!(m.composer.value(), "draft", "{k:?}");
        assert!(m.queue.is_empty(), "{k:?}");
    }
}

/// Row 2 matches the lowercase control letters only: Ctrl+Shift+C (`Char('C')`), Ctrl+X
/// and the rest of the control range neither interrupt nor insert.
#[test]
fn row_2_only_lowercase_ctrl_c_and_ctrl_d_interrupt() {
    let mut m = test_model();
    let turn = CancellationToken::new();
    m.apply(UiMsg::ScopePush(turn.clone()));
    type_text(&mut m, "ab");
    for k in [
        mods(
            KeyCode::Char('C'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ),
        mods(
            KeyCode::Char('D'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ),
        ctrl('x'),
        ctrl('z'),
        ctrl('l'),
    ] {
        m.handle_key(k);
        assert!(!turn.is_cancelled(), "{k:?} fired the scope");
        assert_eq!(m.composer.value(), "ab", "{k:?} inserted");
    }
}

/// Row 3: ESC fires the INNERMOST scope only; the turn scope below it stays.
#[test]
fn row_3_esc_fires_the_innermost_scope_only() {
    let mut m = test_model();
    let turn = CancellationToken::new();
    let tool = CancellationToken::new();
    m.apply(UiMsg::ScopePush(turn.clone()));
    m.apply(UiMsg::ScopePush(tool.clone()));
    m.handle_key(key(KeyCode::Esc));
    assert!(tool.is_cancelled() && !turn.is_cancelled());
    assert_eq!(m.cancels.len(), 1);
    m.handle_key(key(KeyCode::Esc));
    assert!(turn.is_cancelled());
    assert!(m.cancels.is_empty());
}

/// Row 3 with no scope: ESC falls through to the edit set as a state-reset no-op — the
/// draft and a parked reader are untouched, but a completion cycle and a history walk end.
#[test]
fn row_3_esc_with_no_scope_resets_state_and_nothing_else() {
    let mut m = test_model();
    let mut reader = park(&mut m, 1);
    type_text(&mut m, "abc");
    m.handle_key(key(KeyCode::Esc));
    assert_eq!(m.composer.value(), "abc");
    assert!(
        reader.try_recv().is_err(),
        "an idle ESC must not interrupt the reader"
    );

    // The completion cycle ends: the base and the index clear, the completed value stays.
    commands(&mut m, &["/model", "/mode"]);
    m.composer.set_value("/mo");
    m.handle_key(key(KeyCode::Tab));
    assert_eq!(m.composer.value(), "/model");
    assert_eq!(
        (
            m.composer.suggestion_base.as_str(),
            m.composer.suggestion_index
        ),
        ("/mo", Some(0))
    );
    m.handle_key(key(KeyCode::Esc));
    assert_eq!(m.composer.value(), "/model");
    assert!(m.composer.suggestion_base.is_empty() && m.composer.suggestion_index.is_none());

    // The history walk ends: the next ↑ starts a fresh walk from the newest entry.
    let mut m = test_model();
    history(&mut m, &["one", "two"]);
    up(&mut m);
    assert_eq!(m.composer.value(), "two");
    m.handle_key(key(KeyCode::Esc));
    assert_eq!(m.composer.value(), "two", "ESC keeps the recalled text");
    up(&mut m);
    assert_eq!(
        m.composer.value(),
        "two",
        "after ESC ↑ walks again from the newest entry"
    );
}

/// Row 4: Tab cycles the matches of the prefix captured at the FIRST press, wrapping, and
/// writes each candidate into the composer; Enter is never claimed. A Tab with no match —
/// a command nothing completes, or a line that is not a command — is a consumed no-op.
#[test]
fn row_4_tab_cycles_the_first_press_prefix_and_never_claims_enter() {
    let mut m = test_model();
    commands(&mut m, &["/model", "/mode", "/help"]);
    let mut reader = park(&mut m, 1);
    type_text(&mut m, "/mo");
    for want in ["/model", "/mode", "/model"] {
        m.handle_key(key(KeyCode::Tab));
        assert_eq!(m.composer.value(), want);
        assert_eq!(
            m.composer.suggestion_base, "/mo",
            "the first-press prefix is the base"
        );
    }
    enter(&mut m);
    let got = reader
        .try_recv()
        .expect("Enter must submit")
        .expect("read err");
    assert_eq!(got.text, "/model");

    type_text(&mut m, "/zzz");
    m.handle_key(key(KeyCode::Tab));
    assert_eq!(m.composer.value(), "/zzz");
    assert!(m.composer.suggestion_base.is_empty(), "no match, no cycle");
    m.composer.reset();
    type_text(&mut m, "hello");
    m.handle_key(key(KeyCode::Tab));
    assert_eq!(
        m.composer.value(),
        "hello",
        "Tab never inserts a tab character"
    );
}

/// Row 4: any key that is not Tab ends the completion cycle before it is routed on.
#[test]
fn row_4_any_other_key_ends_the_completion_cycle() {
    let mut m = test_model();
    commands(&mut m, &["/model", "/mode"]);
    type_text(&mut m, "/mo");
    m.handle_key(key(KeyCode::Tab));
    m.handle_key(ch('s'));
    assert_eq!(m.composer.value(), "/models");
    assert!(m.composer.suggestion_base.is_empty() && m.composer.suggestion_index.is_none());

    m.composer.reset();
    type_text(&mut m, "/mo");
    m.handle_key(key(KeyCode::Tab));
    m.handle_key(key(KeyCode::Backspace));
    assert_eq!(m.composer.value(), "/mode");
    assert!(m.composer.suggestion_base.is_empty());
}

/// Row 5: ↑ on a BLANK composer (whitespace counts as blank) pops the newest queued item,
/// one per press; with nothing queued the same ↑ is a history walk.
#[test]
fn row_5_up_on_a_blank_composer_pops_the_newest_queued_item() {
    let mut m = test_model();
    type_text(&mut m, "a");
    enter(&mut m);
    type_text(&mut m, "b");
    enter(&mut m);
    up(&mut m);
    assert_eq!(m.composer.value(), "b");
    assert_eq!(m.queue_rows(), vec!["a"]);

    m.handle_key(ctrl('u'));
    type_text(&mut m, "  ");
    up(&mut m);
    assert_eq!(m.composer.value(), "a", "a whitespace-only draft is blank");
    assert!(m.queue.is_empty());

    up(&mut m); // nothing queued, draft non-blank: row 6
    assert_eq!(
        m.composer.value(),
        "b",
        "↑ walks the history once the queue is empty"
    );
}

/// Row 6: ↑/↓ walk the history only while the composer holds ONE wrapped row — newest
/// first, stopping at the oldest, ↓ back to the saved draft and then no further; a
/// multi-row draft (two logical lines, or one line wrapping) keeps the arrows for cursor
/// movement and never enters the history.
#[test]
fn row_6_arrows_walk_the_history_only_in_a_single_row_draft() {
    let mut m = test_model();
    history(&mut m, &["one", "two"]);
    type_text(&mut m, "dr");
    up(&mut m);
    assert_eq!(m.composer.value(), "two");
    up(&mut m);
    assert_eq!(m.composer.value(), "one");
    up(&mut m);
    assert_eq!(m.composer.value(), "one", "the oldest entry is the floor");
    down(&mut m);
    assert_eq!(m.composer.value(), "two");
    down(&mut m);
    assert_eq!(
        m.composer.value(),
        "dr",
        "↓ past the newest restores the draft"
    );
    down(&mut m);
    assert_eq!(
        m.composer.value(),
        "dr",
        "a further ↓ falls to the edit set: a no-op here"
    );

    m.composer.set_value("a\nb");
    assert_eq!(cursor(&m), (3, 1));
    up(&mut m);
    assert_eq!(
        m.composer.value(),
        "a\nb",
        "a two-line draft never recalls history"
    );
    assert_eq!(
        cursor(&m),
        (3, 0),
        "↑ moved the cursor a row up, holding its column"
    );
    down(&mut m);
    assert_eq!(cursor(&m), (3, 1));

    m.composer.set_value(&"x".repeat(200)); // one logical line, three wrapped rows
    assert_eq!(cursor(&m).1, 2);
    up(&mut m);
    assert_eq!(cursor(&m).1, 1, "a wrapping line keeps ↑ for row movement");
    assert_eq!(m.composer.value().len(), 200);
}

/// Row 7: Enter submits the TRIMMED draft — to a parked reader, else onto the queue — and
/// collapses the composer to one empty row; a blank draft submits nothing; the modifiers
/// on Enter are ignored (no terminal's Shift+Enter inserts a newline here); what was
/// submitted is the newest history entry.
#[test]
fn row_7_enter_submits_trimmed_or_ignores_blank() {
    let mut m = test_model();
    let mut reader = park(&mut m, 1);
    type_text(&mut m, "  hi  ");
    enter(&mut m);
    let got = reader.try_recv().expect("served").expect("read err");
    assert_eq!(got.text, "hi");
    assert_eq!(m.composer.value(), "");
    assert_eq!(m.composer.rows(80).len(), 1);

    let mut reader = park(&mut m, 2);
    type_text(&mut m, "   ");
    enter(&mut m);
    assert!(reader.try_recv().is_err(), "a blank draft submits nothing");
    assert_eq!(m.composer.value(), "", "…but the composer still collapses");
    m.apply(UiMsg::ReadCancel { id: 2 });

    for mo in [
        KeyModifiers::SHIFT,
        KeyModifiers::ALT,
        KeyModifiers::CONTROL,
    ] {
        type_text(&mut m, "x");
        m.handle_key(mods(KeyCode::Enter, mo));
        assert_eq!(
            m.composer.value(),
            "",
            "Enter with {mo:?} must still submit"
        );
    }
    assert_eq!(
        m.queue_rows(),
        vec!["x", "x", "x"],
        "no reader parked: queued"
    );

    m.queue.clear();
    up(&mut m);
    assert_eq!(
        m.composer.value(),
        "x",
        "a submit is the newest history entry"
    );
}

// ---- the edit set (rows 8–9), through the ladder ------------------------------------------

/// Home / Ctrl+A and End / Ctrl+E are line-scoped: the start and end of the logical line
/// under the cursor, not of the draft.
#[test]
fn row_8_home_and_end_and_their_control_twins_are_line_scoped() {
    let mut m = test_model();
    m.composer.set_value("ab\ncd");
    m.handle_key(key(KeyCode::Home));
    type_text(&mut m, "X");
    assert_eq!(m.composer.value(), "ab\nXcd");
    m.handle_key(ctrl('e'));
    type_text(&mut m, "Y");
    assert_eq!(m.composer.value(), "ab\nXcdY");
    m.handle_key(ctrl('a'));
    type_text(&mut m, "Z");
    assert_eq!(m.composer.value(), "ab\nZXcdY");
    m.handle_key(key(KeyCode::End));
    type_text(&mut m, "W");
    assert_eq!(m.composer.value(), "ab\nZXcdYW");
    up(&mut m); // the first line, end of "ab"
    m.handle_key(key(KeyCode::Home));
    type_text(&mut m, "Q");
    assert_eq!(m.composer.value(), "Qab\nZXcdYW");
}

/// ← / Ctrl+B and → / Ctrl+F step by grapheme — a CJK glyph or a base + combining mark
/// is one step — and stop at the edges.
#[test]
fn row_8_left_and_right_and_their_control_twins_step_by_grapheme() {
    let mut m = test_model();
    m.composer.set_value("a中e\u{301}b");
    m.handle_key(key(KeyCode::Left));
    m.handle_key(key(KeyCode::Left));
    type_text(&mut m, "1");
    assert_eq!(
        m.composer.value(),
        "a中1e\u{301}b",
        "← crossed e+U+0301 as one grapheme"
    );
    m.handle_key(ctrl('b'));
    m.handle_key(ctrl('b'));
    type_text(&mut m, "2");
    assert_eq!(m.composer.value(), "a2中1e\u{301}b");
    m.handle_key(key(KeyCode::Right));
    type_text(&mut m, "3");
    assert_eq!(m.composer.value(), "a2中31e\u{301}b");
    m.handle_key(ctrl('f'));
    type_text(&mut m, "4");
    assert_eq!(m.composer.value(), "a2中314e\u{301}b");

    m.handle_key(key(KeyCode::Home));
    m.handle_key(key(KeyCode::Left));
    assert_eq!(cursor(&m), (2, 0), "← at the start stays");
    m.handle_key(key(KeyCode::End));
    m.handle_key(key(KeyCode::Right));
    assert_eq!(cursor(&m).0, 2 + 9, "→ at the end stays");
}

/// Ctrl+K kills to the end of the LINE (never the newline), Ctrl+U to its start.
#[test]
fn row_8_ctrl_k_and_ctrl_u_kill_within_the_line() {
    let mut m = test_model();
    m.composer.set_value("abc\ndef");
    m.handle_key(key(KeyCode::Home));
    m.handle_key(key(KeyCode::Right));
    m.handle_key(ctrl('k'));
    assert_eq!(m.composer.value(), "abc\nd");

    m.composer.set_value("abc\ndef");
    m.handle_key(key(KeyCode::Home));
    m.handle_key(key(KeyCode::Right));
    m.handle_key(ctrl('u'));
    type_text(&mut m, "X");
    assert_eq!(m.composer.value(), "abc\nXef");

    m.composer.set_value("ab\ncd");
    up(&mut m); // end of "ab"
    m.handle_key(ctrl('k'));
    assert_eq!(
        m.composer.value(),
        "ab\ncd",
        "Ctrl+K at a line end does not eat the newline"
    );
}

/// Ctrl+W skips whitespace back, then deletes to the start of the previous word — any
/// Unicode whitespace bounds a word, and the line start is never crossed.
#[test]
fn row_8_ctrl_w_deletes_the_previous_word_within_the_line() {
    let mut m = test_model();
    for (value, want) in [
        ("foo bar  ", "foo "),
        ("foo\nbar", "foo\n"),
        ("a\tb", "a\t"),
        ("   ", ""),
        ("单 词", "单 "),
    ] {
        m.composer.set_value(value);
        m.handle_key(ctrl('w'));
        assert_eq!(m.composer.value(), want, "Ctrl+W on {value:?}");
    }
}

/// Backspace / Ctrl+H remove the grapheme before the cursor, Delete the one after; both
/// are no-ops at their edge; Backspace with ANY modifier is still a single-grapheme backspace.
#[test]
fn row_8_backspace_and_delete_remove_one_grapheme() {
    let mut m = test_model();
    m.composer.set_value("a中e\u{301}");
    m.handle_key(key(KeyCode::Backspace));
    assert_eq!(m.composer.value(), "a中");
    m.handle_key(key(KeyCode::Backspace));
    assert_eq!(m.composer.value(), "a");
    m.handle_key(ctrl('h'));
    assert_eq!(m.composer.value(), "");
    m.handle_key(key(KeyCode::Backspace));
    assert_eq!(
        m.composer.value(),
        "",
        "Backspace on an empty draft is a no-op"
    );

    m.composer.set_value("e\u{301}b");
    m.handle_key(key(KeyCode::Home));
    m.handle_key(key(KeyCode::Delete));
    assert_eq!(m.composer.value(), "b");
    m.handle_key(key(KeyCode::Delete));
    assert_eq!(m.composer.value(), "");
    m.handle_key(key(KeyCode::Delete));
    assert_eq!(m.composer.value(), "", "Delete at the end is a no-op");

    m.composer.set_value("ab");
    m.handle_key(mods(KeyCode::Backspace, KeyModifiers::CONTROL));
    assert_eq!(m.composer.value(), "a");
    m.handle_key(mods(KeyCode::Backspace, KeyModifiers::ALT));
    assert_eq!(m.composer.value(), "");
}

/// Row 9: text insert takes every char key that is not a control chord — plain, shifted,
/// CJK, emoji, a bare combining mark — and Alt+char too: an ESC-prefixed sequence
/// (`ESC b`) is decoded as Alt+b and lands as the letter, not as a word motion.
#[test]
fn row_9_text_insert_takes_plain_shifted_alt_and_non_ascii_chars() {
    let mut m = test_model();
    m.handle_key(ch('a'));
    m.handle_key(ch('A'));
    m.handle_key(key(KeyCode::Char('中')));
    m.handle_key(key(KeyCode::Char('😀')));
    m.handle_key(ch('e'));
    m.handle_key(key(KeyCode::Char('\u{301}')));
    m.handle_key(mods(KeyCode::Char('b'), KeyModifiers::ALT));
    m.handle_key(ctrl('x'));
    assert_eq!(m.composer.value(), "aA中😀e\u{301}b");
    assert_eq!(
        cursor(&m),
        (2 + 1 + 1 + 2 + 2 + 1 + 1, 0),
        "the cursor follows by width"
    );
}

/// Keys the table does not name — function keys, paging, Insert, the control chords the
/// surface uses (Ctrl+P/N) and other control letters — change nothing, cursor included.
#[test]
fn unbound_keys_are_no_ops() {
    let mut m = test_model();
    m.composer.set_value("ab");
    m.handle_key(key(KeyCode::Home));
    for k in [
        key(KeyCode::F(1)),
        key(KeyCode::PageUp),
        key(KeyCode::PageDown),
        key(KeyCode::Insert),
        key(KeyCode::Null),
        key(KeyCode::BackTab),
        ctrl('p'),
        ctrl('n'),
        ctrl('x'),
        ctrl('l'),
        mods(
            KeyCode::Char('a'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ), // Ctrl+Alt+a: not the Ctrl+A arm's twin
    ] {
        m.handle_key(k);
        assert_eq!(m.composer.value(), "ab", "{k:?} changed the draft");
        assert_eq!(cursor(&m), (2, 0), "{k:?} moved the cursor");
    }
}

/// Any edit key ends history navigation: after it, ↑ starts a fresh walk from the newest
/// entry and ↓ restores what was on screen as the new draft.
#[test]
fn any_edit_ends_the_history_walk() {
    let mut m = test_model();
    history(&mut m, &["one", "two"]);
    up(&mut m);
    up(&mut m);
    assert_eq!(m.composer.value(), "one");
    m.handle_key(ch('x'));
    assert_eq!(m.composer.value(), "onex");
    up(&mut m);
    assert_eq!(
        m.composer.value(),
        "two",
        "a fresh walk from the newest entry"
    );
    down(&mut m);
    assert_eq!(
        m.composer.value(),
        "onex",
        "the edited text is the saved draft now"
    );

    up(&mut m);
    up(&mut m);
    assert_eq!(m.composer.value(), "one");
    m.handle_key(key(KeyCode::Left)); // a cursor move counts as an edit
    up(&mut m);
    assert_eq!(m.composer.value(), "two");
}

/// A bracketed paste arrives as `Event::Paste`, never as keys: it neither submits nor runs
/// through the table — a multi-line paste becomes a `[#N …]` tag (CR/LF normalized), a
/// single-line one inserts verbatim, and a parked reader stays parked.
#[test]
fn a_paste_is_not_a_key() {
    let mut m = test_model();
    let mut reader = park(&mut m, 1);
    m.route_paste("a\r\nb");
    assert!(
        m.composer.value().starts_with("[#1 a… 2 lines]"),
        "{:?}",
        m.composer.value()
    );
    assert!(reader.try_recv().is_err(), "a paste never submits");
    m.composer.reset();
    m.route_paste("plain\ttext");
    assert_eq!(m.composer.value(), "plain\ttext");
    assert!(m.queue.is_empty());
}

// ---- the same edit set as the surface's one-line field implements it --------------------

/// The field's `handle_key` arms, one by one: Home/Ctrl+A, End/Ctrl+E, ←/Ctrl+B and →/Ctrl+F
/// by grapheme, Ctrl+K, Ctrl+U, Backspace/Ctrl+H (any modifier), Delete, text insert.
#[test]
fn the_field_shares_the_edit_set() {
    let mut f = Field::new();
    f.set_value("a中e\u{301}b");
    assert_eq!(f.cursor_col(), 5);
    f.handle_key(&key(KeyCode::Home));
    assert_eq!(f.cursor_col(), 0);
    f.handle_key(&key(KeyCode::End));
    assert_eq!(f.cursor_col(), 5);
    f.handle_key(&ctrl('a'));
    assert_eq!(f.cursor_col(), 0);
    f.handle_key(&ctrl('e'));
    assert_eq!(f.cursor_col(), 5);

    f.handle_key(&key(KeyCode::Left));
    f.handle_key(&key(KeyCode::Left));
    f.handle_key(&ch('1'));
    assert_eq!(
        f.value(),
        "a中1e\u{301}b",
        "← crossed e+U+0301 as one grapheme"
    );
    f.handle_key(&ctrl('b'));
    f.handle_key(&ctrl('b'));
    f.handle_key(&ch('2'));
    assert_eq!(f.value(), "a2中1e\u{301}b");
    f.handle_key(&key(KeyCode::Right));
    f.handle_key(&ch('3'));
    assert_eq!(f.value(), "a2中31e\u{301}b");
    f.handle_key(&ctrl('f'));
    f.handle_key(&ch('4'));
    assert_eq!(f.value(), "a2中314e\u{301}b");
    f.handle_key(&key(KeyCode::End));
    f.handle_key(&key(KeyCode::Right));
    assert_eq!(f.cursor_col(), 9, "→ at the end stays");
    f.handle_key(&key(KeyCode::Home));
    f.handle_key(&key(KeyCode::Left));
    assert_eq!(f.cursor_col(), 0, "← at the start stays");

    f.set_value("abcdef");
    f.handle_key(&key(KeyCode::Home));
    f.handle_key(&key(KeyCode::Right));
    f.handle_key(&ctrl('k'));
    assert_eq!(f.value(), "a");
    f.set_value("abcdef");
    f.handle_key(&key(KeyCode::Home));
    f.handle_key(&key(KeyCode::Right));
    f.handle_key(&ctrl('u'));
    f.handle_key(&ch('X'));
    assert_eq!(f.value(), "Xbcdef");

    f.set_value("a中e\u{301}");
    f.handle_key(&key(KeyCode::Backspace));
    f.handle_key(&ctrl('h'));
    assert_eq!(f.value(), "a");
    f.handle_key(&mods(KeyCode::Backspace, KeyModifiers::ALT));
    assert_eq!(f.value(), "");
    f.handle_key(&key(KeyCode::Backspace));
    assert_eq!(f.value(), "", "Backspace on an empty field is a no-op");
    f.set_value("e\u{301}b");
    f.handle_key(&key(KeyCode::Home));
    f.handle_key(&key(KeyCode::Delete));
    assert_eq!(f.value(), "b");
    f.handle_key(&key(KeyCode::End));
    f.handle_key(&key(KeyCode::Delete));
    assert_eq!(f.value(), "b", "Delete at the end is a no-op");

    f.clear();
    f.handle_key(&ch('a'));
    f.handle_key(&ch('A'));
    f.handle_key(&key(KeyCode::Char('中')));
    f.handle_key(&key(KeyCode::Char('😀')));
    f.handle_key(&mods(KeyCode::Char('b'), KeyModifiers::ALT));
    f.handle_key(&ctrl('x'));
    assert_eq!(
        f.value(),
        "aA中😀b",
        "plain, shifted, CJK, emoji and Alt+char insert; Ctrl+X does not"
    );
}

/// The field's Ctrl+W is its own: trailing whitespace skipped, then back to the last SPACE
/// — a tab is not a word boundary here (the composer's Ctrl+W treats it as one). Pinned as
/// written; PR-15 decides which rule the shared editor keeps.
#[test]
fn the_field_ctrl_w_cuts_back_to_the_last_space() {
    let mut f = Field::new();
    for (value, want) in [
        ("foo bar  ", "foo "),
        ("a\tb", ""),
        ("   ", ""),
        ("单 词", "单 "),
    ] {
        f.set_value(value);
        f.handle_key(&ctrl('w'));
        assert_eq!(f.value(), want, "Ctrl+W on {value:?}");
    }
}

/// The field does not answer the ladder's keys — arrows up/down, Tab, Enter, ESC — nor the
/// keys nothing binds; the surface ladder above it owns those.
#[test]
fn the_field_ignores_what_the_surface_ladder_owns() {
    let mut f = Field::new();
    f.set_value("ab");
    f.handle_key(&key(KeyCode::Home));
    for k in [
        key(KeyCode::Up),
        key(KeyCode::Down),
        key(KeyCode::Tab),
        key(KeyCode::Enter),
        key(KeyCode::Esc),
        key(KeyCode::F(1)),
        key(KeyCode::PageDown),
        ctrl('p'),
        ctrl('n'),
        ctrl('c'),
    ] {
        f.handle_key(&k);
        assert_eq!(f.value(), "ab", "{k:?} changed the field");
        assert_eq!(f.cursor_col(), 0, "{k:?} moved the cursor");
    }
}
