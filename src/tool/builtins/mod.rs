//! The built-in tool sets: what the framework ships, kept apart
//! from the contracts in `tool` and the set vocabulary in `tool::sets` — `sets::set_factory` is the one place
//! that names them. `shell` is the `shell` tool's POLICY layer over the `crate::shell` mechanism; `ask`
//! contributes tools only when the `Env` carries an interactor.

pub mod agent;
pub mod ask;
pub mod code;
pub mod shell;
