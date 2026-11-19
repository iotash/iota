//! The approval dialog (chat/approval.go): the ONE place a state-changing call is put to the user. It owns no
//! memory — whether to ask is [`crate::tool::approval::gate`]'s, and what the user grants goes to the run's
//! grant store ([`crate::tool::grants`]), which the walk writes before the call runs.

use std::path::Path;
use std::sync::Arc;

use crate::host::{Event, Kind, Presenter, State};
use crate::tool::fmt::display_tool_name;
use crate::tool::grants::Level;
use crate::ui::facade::{SelectSpec, Ui, UiError};
use tokio_util::sync::CancellationToken;

use crate::repl::render::transcript::Transcript;

/// How the user answered (a denial is [`Answer::Denied`], not an error).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Answer {
    /// "Deny", or the prompt was cancelled.
    Denied,
    /// "Allow once".
    Once,
    /// The offered grant ([`Offer`]), at this level: the call runs and the grant stays.
    Granted(Level),
}

/// What the dialog offers beyond "Allow once" for a call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Offer<'a> {
    /// A built-in tool grant — `Allow for this session` (a call bound to no location).
    Tool,
    /// An MCP tool grant, `<server>:<tool>` — `Always allow <server>:<tool> in this session`, then `…in this
    /// project`. Never the server's `*`: that is `/allow`'s, typed out.
    Mcp(&'a str),
    /// A directory grant — `Allow, and make <dir> writable for this session`, then `…for this project` (a
    /// `code` write outside every root, `<dir>` its real parent).
    Dir(&'a Path),
    /// Nothing: a protected path, or a directory never offered as a root (`/`, the home directory or an
    /// ancestor of it) — `Allow once` / `Deny`, asked every time.
    Nothing,
}

/// One approval dialog per conversation. The transcript's clock pauses while the user deliberates.
pub(crate) struct ApprovalGate {
    ui: Arc<dyn Ui>,
    tr: Arc<Transcript>,
    pres: Arc<Presenter>,
}

impl ApprovalGate {
    /// A dialog over the facade, the transcript and the host presenter.
    pub(crate) fn new(ui: Arc<dyn Ui>, tr: Arc<Transcript>, pres: Arc<Presenter>) -> Self {
        Self { ui, tr, pres }
    }

    /// Asks about one gated call (approval.go:47-80). `detail` is what the call is about (the path it writes,
    /// the command it runs); `offer` is the grants the items after the first give ([`Offer`]). The error is reserved for
    /// the prompt itself failing; a refusal is `Ok(Answer::Denied)`, and a turn that continues after a denial
    /// is the point of asking.
    pub(crate) async fn ask(
        &self,
        cancel: &CancellationToken,
        name: &str,
        offer: Offer<'_>,
        detail: &str,
    ) -> Result<Answer, UiError> {
        // The grant items, in the order their answers are: session, then project.
        let grants: Vec<(String, Level)> = match offer {
            Offer::Tool => vec![("Allow for this session".to_owned(), Level::Session)],
            Offer::Mcp(key) => [(Level::Session, "session"), (Level::Project, "project")]
                .into_iter()
                .map(|(level, word)| (format!("Always allow {key} in this {word}"), level))
                .collect(),
            Offer::Dir(dir) => [(Level::Session, "session"), (Level::Project, "project")]
                .into_iter()
                .map(|(level, word)| {
                    (
                        format!("Allow, and make {} writable for this {word}", dir.display()),
                        level,
                    )
                })
                .collect(),
            Offer::Nothing => Vec::new(),
        };
        let mut label = display_tool_name(name);
        if !detail.is_empty() {
            label = format!("{label} {detail}");
        }
        // The turn is blocked on the user: freeze the group clock so human deliberation
        // never inflates the timings, and tell the host (approval.go:58-69).
        self.tr.pause_for_input("waiting for approval");
        self.pres.set_state(State::NeedsInput);
        self.pres.notify(Event {
            kind: Kind::NeedsInput,
            text: format!("{label} needs approval"),
        });
        let choice = self
            .ui
            .select(
                cancel,
                SelectSpec {
                    title: format!("{label} — allow?"),
                    items: std::iter::once("Allow once".to_owned())
                        .chain(grants.iter().map(|(item, _)| item.clone()))
                        .chain(["Deny".to_owned()])
                        .collect(),
                    cursor: 0,
                },
            )
            .await;
        self.tr.resume_from_input();
        self.pres.set_state(State::Busy);
        let choice = choice?;
        if choice.cancelled {
            return Ok(Answer::Denied);
        }
        Ok(match choice.index {
            0 => Answer::Once,
            i => grants
                .get(i - 1)
                .map_or(Answer::Denied, |(_, level)| Answer::Granted(*level)),
        })
    }
}
