//! `/edit [prompt]` and `/redo` (chat/run.go:450-516), registered only for a dedicated image
//! provider: bare `/edit` opens the image picker and attaches the chosen canvas to the NEXT
//! message; `/edit <prompt>` attaches the last generated images and sends the prompt; `/redo`
//! re-sends the last user prompt with its attachments. A `Send` outcome falls through into the
//! run loop's message path with the expanded `content` (the echo still shows the typed line).
//!
//! The canvas is materialised as a user ATTACHMENT rather than remembered out of band: it is
//! explicit, it persists, and a resumed session replays the exact reference the model was given.
//! Consecutive `/edit`s therefore chain — each takes the newest assistant image, which is the
//! previous edit's result — while `/redo` reaches for the last USER turn, so a reworded retry
//! starts from the canvas that produced the rejected picture rather than from the picture.

use tokio_util::sync::CancellationToken;

use crate::chat::images::{last_generated_images, last_user_message};
use crate::repl::editpicker::{
    ImagePreviewer, generated_image_choices, image_choice_details, image_choice_labels,
};
use crate::repl::run::Repl;
use crate::repl::styles::truncate_runes;
use crate::repl::title::flatten_line;
use crate::session::SessionWriter;
use crate::ui::facade::{Panel, TabbedSpec};

/// What the arm decided (run.go:450-516).
pub(crate) enum EditOutcome {
    /// Back to the prompt.
    Continue,
    /// Send this content through the message path.
    Send(String),
}

/// run.go:462,488.
pub(crate) const NOTHING_TO_EDIT: &str = "Nothing to edit yet — generate an image first.";
/// The picker's chip title (run.go:467).
pub(crate) const EDIT_TITLE: &str = "Edit an image";
/// The picker's prompt row (run.go:469).
pub(crate) const EDIT_PROMPT: &str = "Pick the image to edit, then type your prompt";
/// run.go:502.
const NOTHING_TO_REDO: &str = "Nothing to redo yet.";
/// run.go:510.
const NOTHING_TO_REDO_BLANK: &str = "Nothing to redo yet — the last turn carried no prompt.";
/// How much of the redone prompt the echo shows (run.go:514).
const REDO_ECHO_RUNES: usize = 60;

/// The `/edit` arm (run.go:450-490).
///
/// `prompt` is the trimmed text after the command; `""` opens the picker and returns to the
/// composer (the `/file` rhythm — the prompt arrives as the next message), anything else attaches
/// the last generated images and falls through to send.
pub(crate) async fn cmd_edit(repl: &mut Repl, prompt: &str) -> EditOutcome {
    if prompt.is_empty() {
        let cancel = repl.cancel.clone();
        return pick_canvas(repl, &cancel).await;
    }
    let refs = last_generated_images(&repl.history);
    if refs.is_empty() {
        repl.tr.notice(NOTHING_TO_EDIT);
        return EditOutcome::Continue;
    }
    repl.pending.extend(refs);
    EditOutcome::Send(prompt.to_owned())
}

/// Bare `/edit`: pick the canvas from every image this session generated (preview beside the
/// list), then return to the composer for the prompt.
async fn pick_canvas(repl: &mut Repl, cancel: &CancellationToken) -> EditOutcome {
    let choices = generated_image_choices(&repl.history);
    if choices.is_empty() {
        repl.tr.notice(NOTHING_TO_EDIT);
        return EditOutcome::Continue;
    }
    // `images_path()` never creates the directory — a chat that has not saved a picture yet must
    // not grow one just because the picker opened.
    let img_dir = repl.with_writer_path(SessionWriter::images_path);
    let width = usize::from(repl.ui.width());
    let spec = TabbedSpec {
        panels: vec![
            Panel::picker(EDIT_TITLE.to_owned(), image_choice_labels(&choices))
                .with_prompt(EDIT_PROMPT.to_owned())
                .with_search(true)
                .with_details(image_choice_details(
                    &choices,
                    img_dir.as_deref(),
                    width,
                    crate::app::user_home().as_deref(),
                ))
                .with_preview(ImagePreviewer::new(choices.clone()).into_preview_fn()),
        ],
        ..TabbedSpec::default()
    };
    // A facade failure is treated exactly like a cancel: commands never end the loop.
    let Ok(r) = repl.ui.tabbed(cancel, spec).await else {
        return EditOutcome::Continue;
    };
    if r.cancelled {
        return EditOutcome::Continue;
    }
    let Some(choice) = r
        .panels
        .first()
        .and_then(|p| choices.get(p.cursor))
        .cloned()
    else {
        return EditOutcome::Continue;
    };
    repl.tr.notice(&format!(
        "Editing {} — type your prompt.",
        choice.att.filename
    ));
    repl.pending.push(choice.att);
    EditOutcome::Continue
}

/// The `/redo` arm (run.go:499-516).
///
/// The attachments come from the last USER message — all of them, the canvas that produced the
/// rejected result, never the result itself. A bare `/redo` re-rolls the same prompt (image
/// models roll differently per call); a reworded one retries from the same canvas.
pub(crate) fn cmd_redo(repl: &mut Repl, prompt: &str) -> EditOutcome {
    let Some(last) = last_user_message(&repl.history) else {
        repl.tr.notice(NOTHING_TO_REDO);
        return EditOutcome::Continue;
    };
    let prompt = if prompt.is_empty() {
        last.content.clone()
    } else {
        prompt.to_owned()
    };
    if prompt.trim().is_empty() {
        repl.tr.notice(NOTHING_TO_REDO_BLANK);
        return EditOutcome::Continue;
    }
    let refs = last.attachments.clone();
    repl.pending.extend(refs);
    repl.tr.notice(&format!(
        "Redoing: {}",
        truncate_runes(&flatten_line(&prompt), REDO_ECHO_RUNES)
    ));
    EditOutcome::Send(prompt)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    //! The two history helpers `/edit` and `/redo` are built on (`chat/edit_test.go`). They live in
    //! `chat::images` (headless-safe, no `repl` state) and are exercised here, beside their two
    //! callers, exactly as Go tests them beside `run.go`'s arms.

    use crate::chat::images::{last_generated_images, last_user_message};
    use crate::provider::model::{Attachment, Message, Role};

    fn att(name: &str, mime: &str, data: &[u8]) -> Attachment {
        Attachment {
            filename: name.to_owned(),
            mime_type: mime.to_owned(),
            data: data.to_vec(),
        }
    }

    fn msg(role: Role, content: &str, atts: Vec<Attachment>) -> Message {
        Message {
            attachments: atts,
            ..Message::of_role(role, content.to_owned())
        }
    }

    // Go: chat/edit_test.go:11 TestLastGeneratedImages
    #[test]
    fn test_last_generated_images() {
        // `last_generated_images` picks the /edit canvas: the newest assistant reply carrying
        // image attachments, skipping text-only replies in between.
        let img1 = att("image-1.png", "image/png", &[1]);
        let img2 = att("image-2.png", "image/png", &[2]);

        assert!(last_generated_images(&[]).is_empty(), "empty history");
        assert!(
            last_generated_images(&[
                msg(Role::User, "hi", Vec::new()),
                msg(Role::Assistant, "hello", Vec::new()),
            ])
            .is_empty(),
            "text-only history"
        );

        let history = vec![
            msg(Role::User, "a cat", Vec::new()),
            msg(Role::Assistant, "", vec![img1]),
            msg(Role::User, "a dog", Vec::new()),
            msg(Role::Assistant, "", vec![img2]),
            msg(Role::User, "thanks", Vec::new()),
            // A text reply does not shadow the canvas.
            msg(Role::Assistant, "you're welcome", Vec::new()),
        ];
        let got = last_generated_images(&history);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].filename, "image-2.png", "the newest generated image");

        // Non-image attachments never qualify as a canvas.
        assert!(
            last_generated_images(&[msg(
                Role::Assistant,
                "",
                vec![att("notes.txt", "text/plain", b"x")]
            )])
            .is_empty(),
            "non-image attachment treated as canvas"
        );
    }

    // Go: chat/edit_test.go:49 TestLastUserMessage
    #[test]
    fn test_last_user_message() {
        // /redo re-sends the last REQUEST: `last_user_message` is what it recovers, so rewording
        // works from the canvas that produced the rejected result — not from that result (which
        // is what another /edit would grab).
        let canvas = att("gen-1.png", "image/png", &[1]);
        let bad = att("gen-2.png", "image/png", &[2]);

        assert!(last_user_message(&[]).is_none(), "empty history");
        assert!(
            last_user_message(&[msg(Role::Assistant, "hi", Vec::new())]).is_none(),
            "no user turn"
        );

        let history = vec![
            msg(Role::User, "a cat", Vec::new()),
            msg(Role::Assistant, "", vec![canvas.clone()]),
            // The /edit turn.
            msg(Role::User, "add a heart", vec![canvas]),
            // The rejected result.
            msg(Role::Assistant, "", vec![bad]),
        ];
        let got = last_user_message(&history).expect("recovered request");
        assert_eq!(got.content, "add a heart");
        // The reference is the ORIGINAL canvas, never the rejected output.
        assert_eq!(got.attachments.len(), 1);
        assert_eq!(got.attachments[0].filename, "gen-1.png");
        // Contrast: /edit would take the rejected image as its canvas.
        let refs = last_generated_images(&history);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].filename, "gen-2.png", "sanity: /edit canvas");
    }
}
