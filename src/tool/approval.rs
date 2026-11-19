//! Whether a call is put to the user, and the answer: [`gate`] is the ONE decision both walks (the chat's and
//! the headless loop's) make before a call runs; [`Approval`] is what an approval oracle answers for a gated
//! call — it runs, or is refused with the text the model reads in place of a result.

use crate::provider::model::JsonObject;
use crate::tool::grants::{Grants, ToolKey};
use crate::tool::{ApprovalSubject, Dispatcher};

/// What an approval oracle answers for one gated call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Approval {
    /// The call runs.
    Allow,
    /// The call does not run; the text becomes its (error) tool result, so the model learns why.
    Deny(String),
}

/// What [`gate`] decided for one call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Gate {
    /// The call runs unasked: the config does not gate it, or a tool grant covers it.
    Run,
    /// The user must be asked (a walk with nobody to ask refuses). `subject` is what the call is bound to —
    /// resolved ONCE, so the prompt, the directory it offers and what the call must hit agree — and `key` is
    /// what "allow for this session" grants when the call is bound to nothing.
    Ask {
        /// The call's location ([`Dispatcher::approval_subject`]); `None` = the question is about the tool.
        subject: Option<ApprovalSubject>,
        /// The tool grant that would answer the question.
        key: ToolKey,
    },
}

/// Whether the named call runs unasked. A call the config does not gate ([`Dispatcher::requires_approval`])
/// runs; one bound to a location (an outside or protected write) is always asked — a tool grant never answers
/// it, so an inside grant never covers an outside write; any other runs when `grants` holds its tool. A
/// directory grant is not consulted here: it makes a write under it INSIDE, which the tool itself decides.
pub fn gate(d: &dyn Dispatcher, grants: &Grants, name: &str, args: &JsonObject) -> Gate {
    if !d.requires_approval(name, Some(args)) {
        return Gate::Run;
    }
    let subject = d.approval_subject(name, args);
    let key = ToolKey::Builtin(name.to_owned());
    if subject.is_none() && grants.allows(&key) {
        return Gate::Run;
    }
    Gate::Ask { subject, key }
}

#[cfg(all(test, unix))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use std::sync::Arc;

    use super::{Gate, gate};
    use crate::provider::model::JsonObject;
    use crate::tool::Registry;
    use crate::tool::builtins::code::{CodeSet, EditFile, Outside, WriteFile};
    use crate::tool::grants::{Grants, Level, ToolKey};

    fn args(path: &str) -> JsonObject {
        match serde_json::json!({"path": path, "content": "x"}) {
            serde_json::Value::Object(m) => m,
            _ => unreachable!(),
        }
    }

    /// The writers' tool grant ("allow for this session" on an inside write) stops the question for inside
    /// writes only: an outside write and a protected one are still asked, with their subject, through that
    /// tool and the other.
    #[test]
    fn a_tool_grant_never_answers_a_call_bound_to_a_location() {
        let t = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(t.path()).expect("real").join("proj");
        std::fs::create_dir_all(&root).expect("root");
        let grants = Arc::new(Grants::default());
        let cs = CodeSet::new(
            root.clone(),
            Vec::new(),
            false,
            Outside::Ask,
            None,
            Arc::clone(&grants),
        )
        .expect("set");
        let mut reg = Registry::default();
        reg.add(Arc::new(WriteFile(Arc::clone(&cs))));
        reg.add(Arc::new(EditFile(cs)));
        let inside = args("in.txt");
        let outside = args("../out/x.txt");
        let protected = args(".git/hooks/pre-commit");

        assert_eq!(
            gate(&reg, &grants, "write_file", &inside),
            Gate::Ask {
                subject: None,
                key: ToolKey::Builtin("write_file".to_owned())
            }
        );
        for tool in ["write_file", "edit_file"] {
            grants.grant_tool(Level::Session, ToolKey::Builtin(tool.to_owned()));
        }
        for tool in ["write_file", "edit_file"] {
            assert_eq!(gate(&reg, &grants, tool, &inside), Gate::Run, "{tool}");
            for (what, a) in [("outside", &outside), ("protected", &protected)] {
                match gate(&reg, &grants, tool, a) {
                    Gate::Ask {
                        subject: Some(s), ..
                    } => assert!(s.target.starts_with(t.path().canonicalize().unwrap())),
                    other => panic!("{tool} {what}: {other:?}"),
                }
            }
        }
    }
}
