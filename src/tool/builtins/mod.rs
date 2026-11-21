//! The built-in tool sets: what the framework ships, kept apart
//! from the contracts in `tool` and the set vocabulary in `tool::sets` — `sets::set_factory` is the one place
//! that names them. `shell` is the `shell` tool's policy layer over the `crate::shell` mechanism; `ask`
//! contributes tools only when the `Env` carries an interactor.

pub mod agent;
pub mod ask;
pub mod code;
pub mod memory;
pub mod shell;

/// The built-in tools that ask — those a tool grant can answer, so the names a config's `allow:` may list
/// (docs/design/config-allow.md §3.3): the ones whose argument-less probe (`requires_approval(None)`) is true
/// in a registry with every set enabled and no switch set. Pinned against that probe.
pub const ASKING: [&str; 3] = [shell::SHELL_TOOL_NAME, "edit_file", "write_file"];

/// Every built-in tool, by wire name, in no particular order: what tells "never asks" from "no such tool" in a
/// config's `allow:`. Pinned against a registry with every set enabled.
pub const NAMES: [&str; 11] = [
    "glob",
    "grep",
    "list_dir",
    "read_file",
    "edit_file",
    "write_file",
    shell::SHELL_TOOL_NAME,
    "load_skill",
    "choose",
    "confirm",
    memory::REMEMBER,
];

/// The `tools:` set an asking built-in belongs to: `shell` is its own, the writers are `code`'s.
pub fn set_of(tool: &str) -> &'static str {
    if tool == shell::SHELL_TOOL_NAME {
        "shell"
    } else {
        "code"
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{ASKING, NAMES};
    use crate::tool::{Dispatcher, Registry, ToolEnv};

    /// [`ASKING`] is exactly the tools whose argument-less probe asks, and [`NAMES`] every tool, in a registry
    /// with every set enabled (the ask tools with an interactor, `remember` with a bot's memory) and no
    /// switch set — so neither list can drift from the tools.
    #[test]
    fn the_asking_list_is_what_the_probe_says() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let env = ToolEnv {
            jobs: Some(crate::shell::jobs::Jobs::new(tmp.path())),
            interactor: Some(crate::repl::Interactor::new() as Arc<dyn crate::tool::Interactor>),
            memory: Some(crate::agents::memory::BotMemory::new(
                "bot",
                tmp.path().join("bot"),
            )),
            project_root: Some(tmp.path().to_path_buf()),
            ..ToolEnv::default()
        };
        let raw: crate::tool::sets::ToolsConfig =
            serde_norway::from_str("shell: {}\ncode: {}\nskills: {}\nask: {}").expect("yaml");
        let mut registry = Registry::build(&env, &raw, &mut |w| panic!("{w}"));
        registry.enable_set(&env, crate::tool::sets::MEMORY_SET, &mut |w| panic!("{w}"));

        let mut all: Vec<String> = registry.tools().into_iter().map(|t| t.name).collect();
        let mut asking: Vec<&str> = all
            .iter()
            .map(String::as_str)
            .filter(|n| registry.requires_approval(n, None))
            .collect();
        asking.sort_unstable();
        let mut want = ASKING;
        want.sort_unstable();
        assert_eq!(asking, want);
        all.sort();
        let mut names = NAMES.map(str::to_owned).to_vec();
        names.sort();
        assert_eq!(all, names);
    }
}
