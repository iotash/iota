//! The approval gate (chat/approval.go): the ONE place a state-changing call is put to
//! the user. The "allow for this session" memory belongs to the conversation: the grant
//! is "this session may edit files".

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use crate::host::{Event, Kind, Presenter, State};
use crate::sync::lock;
use crate::tool::fmt::display_tool_name;
use crate::ui::facade::{SelectSpec, Ui, UiError};
use tokio_util::sync::CancellationToken;

use crate::repl::render::transcript::Transcript;

/// One approval gate per conversation (approval.go:26-33). Session grants are keyed by
/// wire tool name; the transcript's clock pauses while the user deliberates.
pub(crate) struct ApprovalGate {
    ui: Arc<dyn Ui>,
    tr: Arc<Transcript>,
    pres: Arc<Presenter>,
    approved: Mutex<HashSet<String>>,
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
    /// (the path it writes, the command it runs). The error is reserved for the prompt
    /// itself failing; a refusal is `Ok(false)`, and a turn that continues after a denial
    /// is the point of asking.
    pub(crate) async fn ask(
        &self,
        cancel: &CancellationToken,
        name: &str,
        detail: &str,
    ) -> Result<bool, UiError> {
        if self.granted(name) {
            return Ok(true);
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
            text: format!("{label} wants to modify files"),
        });
        let choice = self
            .ui
            .select(
                cancel,
                SelectSpec {
                    title: format!("{label} wants to modify files — allow?"),
                    items: vec![
                        "Allow once".to_owned(),
                        "Allow for this session".to_owned(),
                        "Deny".to_owned(),
                    ],
                    cursor: 0,
                },
            )
            .await;
        self.tr.resume_from_input();
        self.pres.set_state(State::Busy);
        let choice = choice?;
        if choice.cancelled || choice.index == 2 {
            return Ok(false);
        }
        if choice.index == 1 {
            lock(&self.approved).insert(name.to_owned());
        }
        Ok(true)
    }

    fn granted(&self, name: &str) -> bool {
        lock(&self.approved).contains(name)
    }
}
