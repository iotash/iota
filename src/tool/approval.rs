//! The answer to an approval request (chat/approval.go, chat.go:348-364): a gated call either runs
//! or is refused, and a refusal carries the text the model reads in place of a result.

/// What an approval oracle answers for one gated call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Approval {
    /// The call runs.
    Allow,
    /// The call does not run; the text becomes its (error) tool result, so the model learns why.
    Deny(String),
}
