//! The approval gate (chat/approval.go): the ONE place a state-changing call is put to
//! the user. The "allow for this session" memory belongs to the conversation — `/new` and a
//! `/session` resume start it empty ([`ApprovalGate::reset`]): the grant is "this session
//! may edit files" — or, for a call bound to a location (a `code` write outside every
//! root), "this session may write in that directory with that tool".

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::host::{Event, Kind, Presenter, State};
use crate::sync::lock;
use crate::tool::fmt::display_tool_name;
use crate::ui::facade::{SelectSpec, Ui, UiError};
use tokio_util::sync::CancellationToken;

use crate::repl::render::transcript::Transcript;

/// How the gate allowed a call (a denial is [`Answer::Denied`], not an error).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Answer {
    /// "Deny", or the prompt was cancelled.
    Denied,
    /// "Allow once".
    Once,
    /// "Allow for this session", now or by an earlier grant.
    Session,
}

/// What "allow for this session" would grant for a call — the gate's key for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Grant<'a> {
    /// The tool (a call not bound to a location).
    Tool,
    /// The tool in this directory (a `code` write outside every root).
    Dir(&'a Path),
    /// Nothing: the option is not offered and no earlier grant answers (a write to a protected path).
    Never,
}

/// One approval gate per conversation (approval.go:26-33). Session grants are keyed by
/// wire tool name and the call's grant scope (`ApprovalSubject::scope`): a grant for one
/// scope never covers another, nor the same scope through another tool, nor the tool's
/// unscoped calls. The transcript's clock pauses while the user deliberates.
pub(crate) struct ApprovalGate {
    ui: Arc<dyn Ui>,
    tr: Arc<Transcript>,
    pres: Arc<Presenter>,
    approved: Mutex<HashSet<(String, Option<PathBuf>)>>,
}

impl ApprovalGate {
    /// A gate over the facade, the transcript and the host presenter with no session grants yet.
    pub(crate) fn new(ui: Arc<dyn Ui>, tr: Arc<Transcript>, pres: Arc<Presenter>) -> Self {
        Self {
            ui,
            tr,
            pres,
            approved: Mutex::new(HashSet::new()),
        }
    }

    /// Resolves one gated call (approval.go:47-80). `detail` is what the call is about
    /// (the path it writes, the command it runs); `grant` is what "allow for this session"
    /// grants ([`Grant`]) — for a directory the option names it, and for [`Grant::Never`]
    /// the prompt is "Allow once" / "Deny" and is asked every time. The error is reserved for the prompt itself failing; a refusal is
    /// `Ok(Answer::Denied)`, and a turn that continues after a denial is the point of asking.
    pub(crate) async fn ask(
        &self,
        cancel: &CancellationToken,
        name: &str,
        grant: Grant<'_>,
        detail: &str,
    ) -> Result<Answer, UiError> {
        let (key, session) = match grant {
            Grant::Tool => (
                Some((name.to_owned(), None)),
                Some("Allow for this session".to_owned()),
            ),
            Grant::Dir(dir) => (
                Some((name.to_owned(), Some(dir.to_path_buf()))),
                Some(format!("Allow in {} for this session", dir.display())),
            ),
            Grant::Never => (None, None),
        };
        if key
            .as_ref()
            .is_some_and(|k| lock(&self.approved).contains(k))
        {
            return Ok(Answer::Session);
        }
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
                        .chain(session)
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
        match (choice.index, key) {
            (0, _) => Ok(Answer::Once),
            (1, Some(key)) => {
                lock(&self.approved).insert(key);
                Ok(Answer::Session)
            }
            _ => Ok(Answer::Denied),
        }
    }

    /// Forgets every session grant: the conversation they belonged to is over (`/new`, a `/session`
    /// resume).
    pub(crate) fn reset(&self) {
        lock(&self.approved).clear();
    }
}
