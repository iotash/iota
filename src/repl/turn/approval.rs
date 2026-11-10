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
    /// (the path it writes, the command it runs); `scope` is what "allow for this session"
    /// grants — `None` the tool, `Some(dir)` the tool in that directory, and then the
    /// option names it. The error is reserved for the prompt itself failing; a refusal is
    /// `Ok(Answer::Denied)`, and a turn that continues after a denial is the point of asking.
    pub(crate) async fn ask(
        &self,
        cancel: &CancellationToken,
        name: &str,
        scope: Option<&Path>,
        detail: &str,
    ) -> Result<Answer, UiError> {
        let key = (name.to_owned(), scope.map(Path::to_path_buf));
        if lock(&self.approved).contains(&key) {
            return Ok(Answer::Session);
        }
        let session = match scope {
            None => "Allow for this session".to_owned(),
            Some(dir) => format!("Allow in {} for this session", dir.display()),
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
                    items: vec!["Allow once".to_owned(), session, "Deny".to_owned()],
                    cursor: 0,
                },
            )
            .await;
        self.tr.resume_from_input();
        self.pres.set_state(State::Busy);
        let choice = choice?;
        if choice.cancelled || choice.index == 2 {
            return Ok(Answer::Denied);
        }
        if choice.index == 1 {
            lock(&self.approved).insert(key);
            return Ok(Answer::Session);
        }
        Ok(Answer::Once)
    }

    /// Forgets every session grant: the conversation they belonged to is over (`/new`, a `/session`
    /// resume).
    pub(crate) fn reset(&self) {
        lock(&self.approved).clear();
    }
}
