//! The approval gate (chat/approval.go): the ONE place a state-changing call is put to
//! the user — the conversation's own tool loop and delegated children both arrive here,
//! a child's question labelled with the agent that asked. The "allow for this session"
//! memory is shared for the same reason: the grant is "this session may edit files", and
//! a child running inside the session is part of it.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};

use crate::host::{Event, Kind, Presenter, State};
use crate::tool::DelegateApprover;
use crate::tool::fmt::display_tool_name;
use crate::tool::{Artifact, ArtifactKind};
use crate::ui::facade::{SelectSpec, Ui, UiError};
use tokio_util::sync::CancellationToken;

use crate::repl::transcript::Transcript;

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
    /// (the path it writes, the command it runs); `subject` names where the request came
    /// from when it was not this conversation (a delegated agent) — empty for the
    /// conversation's own calls. The error is reserved for the prompt itself failing; a
    /// refusal is `Ok(false)`, and a turn that continues after a denial is the point of
    /// asking.
    pub(crate) async fn ask(
        &self,
        cancel: &CancellationToken,
        name: &str,
        detail: &str,
        subject: &str,
    ) -> Result<bool, UiError> {
        if self.granted(name) {
            return Ok(true);
        }
        let mut label = display_tool_name(name);
        if !detail.is_empty() {
            label = format!("{label} {detail}");
        }
        if !subject.is_empty() {
            label = format!("{subject} › {label}");
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
            self.approved
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(name.to_owned());
        }
        Ok(true)
    }

    fn granted(&self, name: &str) -> bool {
        self.approved
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(name)
    }
}

/// The child-approval bridge (run.go:172-186): a `DelegateApprover` forwarding a
/// delegated child's state-changing calls to the parent's ONE gate, labelled with the
/// asking agent. Installed on `crate::chat::ChatDelegator` via `set_approver` by the run
/// loop; gate error → `"%s was not executed: %v"`, declined → `"The user declined this
/// call."` — the model-facing refusal texts.
pub(crate) fn delegate_approver(
    gate: Arc<ApprovalGate>,
    cancel: CancellationToken,
) -> DelegateApprover {
    Arc::new(move |name, detail, agent| {
        let gate = Arc::clone(&gate);
        let cancel = cancel.clone();
        Box::pin(async move {
            match gate.ask(&cancel, name, detail, agent).await {
                Err(e) => (false, format!("{name} was not executed: {e}")),
                Ok(false) => (false, "The user declined this call.".to_owned()),
                Ok(true) => (true, String::new()),
            }
        })
    })
}

/// Renders a `Note` artifact into the event row's trailing detail: a short fact about
/// the call meant for the user and withheld from the model (approval.go:84-89
/// `artifactNote`). The `Diff` kind belongs to the expanded path and is ignored here —
/// one side channel, read differently by the two renderers.
pub(crate) fn artifact_note(art: Option<&Artifact>) -> String {
    match art {
        Some(a) if a.kind == ArtifactKind::Note && !a.lines.is_empty() => a.lines.join(" · "),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use crate::tool::{Artifact, ArtifactKind};

    use super::artifact_note;

    // Go: chat/approval.go:84-89 — note lines join with " · "; diffs and empties render
    // nothing.
    #[test]
    fn test_artifact_note() {
        let note = Artifact {
            kind: ArtifactKind::Note,
            title: "delegate".to_owned(),
            lines: vec!["3 rounds".to_owned(), "1.2k tokens".to_owned()],
        };
        assert_eq!(artifact_note(Some(&note)), "3 rounds · 1.2k tokens");
        let diff = Artifact {
            kind: ArtifactKind::Diff,
            title: "a.txt".to_owned(),
            lines: vec!["+x".to_owned()],
        };
        assert_eq!(artifact_note(Some(&diff)), "");
        let empty = Artifact {
            kind: ArtifactKind::Note,
            title: String::new(),
            lines: Vec::new(),
        };
        assert_eq!(artifact_note(Some(&empty)), "");
        assert_eq!(artifact_note(None), "");
    }
}
