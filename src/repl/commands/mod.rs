//! The slash-command table and the dispatch chain (chat/completion.go, chat/run.go's main
//! loop; `TUI_DESIGN` §9).
//!
//! **The one-table law.** The composer's completion list (what `/` offers) and the dispatch
//! chain read ONE table, so a conditional command can never be advertised without existing —
//! or exist without being advertised. Go held it in package globals rebound once at startup
//! (and on every skill-catalog change); here it is an owned [`CommandTable`] on the run
//! loop's stack. The startup banner listed the table too until X-46; the completion row is
//! where the law is pinned now (`tests/repl/commands.rs`, `file.rs`, `tokens.rs`).
//!
//! **Dispatch is an imperative prefix chain, not a registry.** The loop matches
//! `input == cmd || input.starts_with(cmd + " ")` in Go's FIXED order, first match wins,
//! and unknown `/xyz` text is a normal message. That ordering — and the fall-through of
//! the input EXPANSIONS (`/edit`, `/redo`, `/skills <name>` rewrite the content and continue
//! into the message path) — is the spec, which is why there is no generic dispatcher here:
//! only [`match_cmd`], the matcher every arm calls.
//!
//! **What is registered** (completion.go:10-37, all twelve): the base eight — `/file`,
//! `/session`, `/model`, `/compact` (only while token accounting is live), `/export`, `/status`,
//! `/tools`, `/debug` — then the image pair `/edit`/`/redo` (dedicated image providers), `/save`
//! (chats that started ephemeral) and `/skills` plus one `/skills <name>` row per discovered skill
//! (agent mode). Inactive conditional commands are fully invisible: no completion, no dispatch.
//!
//! No management command joins the table: `/mcp` (with `/mcp login|logout`) was here from 2026-09-18 to
//! 2026-09-20 and left again — the MCP tab of `/tools` shows every server's state, a login is
//! `iota mcp login <name>` from a shell, and configuration is for the config toolset, not a slash
//! command (DIVERGENCES X-42).

pub(crate) mod compact;
pub(crate) mod debug;
pub(crate) mod edit;
pub(crate) mod export;
pub(crate) mod file;
pub(crate) mod model;
pub(crate) mod save;
pub(crate) mod session;
pub(crate) mod settings;
pub(crate) mod skills;
pub(crate) mod status;
pub(crate) mod tools;

use crate::ui::facade::Suggestion;

/// One row of the command table: the bare value (no trailing space) and the description
/// the completion list shows beside it, so a command's purpose need not be guessed from
/// its name.
struct CmdSpec {
    value: &'static str,
    desc: &'static str,
}

/// The unconditional commands, in Go's table order (chat/completion.go:10-19). `/compact`
/// sits between `/model` and `/export` and is skipped while token accounting is off.
/// Descriptions are byte-exact.
const BASE: &[CmdSpec] = &[
    CmdSpec {
        value: "/file",
        desc: "Attach a file, or browse for one",
    },
    CmdSpec {
        value: "/session",
        desc: "Resume or delete a saved session",
    },
    CmdSpec {
        value: "/model",
        desc: "Model, context window, effort, temperature",
    },
    CmdSpec {
        value: "/compact",
        desc: "Summarize older context to reclaim the window",
    },
    CmdSpec {
        value: "/export",
        desc: "Write this chat out as HTML or Markdown",
    },
    CmdSpec {
        value: "/status",
        desc: "Provider, tokens, tools, session id",
    },
    CmdSpec {
        value: "/tools",
        desc: "Available tools and MCP server state",
    },
    CmdSpec {
        value: "/debug",
        desc: "Browse API traffic; toggle request recording",
    },
];

/// The agent-mode group (chat/completion.go:28-30); the per-skill rows follow it.
const AGENT: &[CmdSpec] = &[CmdSpec {
    value: "/skills",
    desc: "List skills, or run one: /skills <name>",
}];

/// The ephemeral-session group (chat/completion.go:31-33).
const SAVE: &[CmdSpec] = &[CmdSpec {
    value: "/save",
    desc: "Start persisting this ephemeral session",
}];

/// The dedicated-image-provider group (chat/completion.go:34-37).
const IMAGE: &[CmdSpec] = &[
    CmdSpec {
        value: "/edit",
        desc: "Edit the last generated image",
    },
    CmdSpec {
        value: "/redo",
        desc: "Re-send the last request",
    },
];

/// Which conditional groups are on (chat/completion.go `commandFlags`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CmdFlags {
    /// The chat STARTED ephemeral, so `/save` can begin persisting it.
    pub(crate) save: bool,
    /// Token accounting is live, so `/compact` exists.
    pub(crate) compact: bool,
    /// Agent mode is on → `/skills` + one row per discovered skill (completion.go:28-30,92-95).
    pub(crate) agent: bool,
    /// A dedicated image provider → `/edit`, `/redo` (completion.go:34-37,86-88).
    pub(crate) image: bool,
}

/// The completion-facing view of a skill (completion.go:76 `skillEntry`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SkillEntry {
    /// The skill's name (`/skills <name>`).
    pub(crate) name: String,
    /// Its one-line description (the row's dim column).
    pub(crate) description: String,
}

/// The effective command table (chat/completion.go `activeSlashCommands`).
pub(crate) struct CommandTable {
    flags: CmdFlags,
    skills: Vec<SkillEntry>,
}

impl CommandTable {
    /// The table for `flags`, with no skills yet.
    pub(crate) fn new(flags: CmdFlags) -> Self {
        Self {
            flags,
            skills: Vec::new(),
        }
    }

    /// Replaces the per-skill rows (completion.go:63-73 `setSkillCommands`); agent mode calls
    /// it at startup and again whenever the catalog changes on disk. The rows only ever show
    /// under the `agent` flag.
    pub(crate) fn set_skills(&mut self, entries: Vec<SkillEntry>) {
        self.skills = entries;
    }

    /// The active rows, in dispatch order — what the composer's completion list gets
    /// (chat/completion.go:78-97 `rebuildCommands`): base (`/compact` iff `compact`) → image →
    /// save → agent (+ the per-skill rows).
    pub(crate) fn active(&self) -> Vec<Suggestion> {
        let mut out: Vec<Suggestion> = Vec::with_capacity(BASE.len() + self.skills.len() + 4);
        for c in BASE {
            if c.value == "/compact" && !self.flags.compact {
                continue;
            }
            out.push(suggestion(c));
        }
        if self.flags.image {
            out.extend(IMAGE.iter().map(suggestion));
        }
        if self.flags.save {
            out.extend(SAVE.iter().map(suggestion));
        }
        if self.flags.agent {
            out.extend(AGENT.iter().map(suggestion));
            // Registered as whole values so the list matches them by prefix like any other
            // command, labelled with the bare name: the `/skills ` part is already typed
            // (completion.go:66-70).
            out.extend(self.skills.iter().map(|sk| Suggestion {
                value: format!("/skills {}", sk.name),
                label: sk.name.clone(),
                desc: sk.description.clone(),
            }));
        }
        out
    }

    /// Whether `/save` is registered — the dispatch chain's own visibility check, read
    /// from the SAME flags the completion list is built from.
    pub(crate) fn save_enabled(&self) -> bool {
        self.flags.save
    }

    /// Whether `/edit` and `/redo` are registered.
    pub(crate) fn image_enabled(&self) -> bool {
        self.flags.image
    }

    /// Whether `/skills` (and the per-skill rows) are registered.
    pub(crate) fn agent_enabled(&self) -> bool {
        self.flags.agent
    }
}

fn suggestion(c: &CmdSpec) -> Suggestion {
    Suggestion {
        value: c.value.to_owned(),
        label: String::new(),
        desc: c.desc.to_owned(),
    }
}

/// The dispatch matcher (chat/run.go's chain): `input` is `cmd`, or `cmd` followed by a
/// space. Returns the TRIMMED argument (`""` for the bare form), or `None` when the
/// command does not match.
pub(crate) fn match_cmd<'a>(input: &'a str, cmd: &str) -> Option<&'a str> {
    if input == cmd {
        return Some("");
    }
    input
        .strip_prefix(cmd)
        .filter(|rest| rest.starts_with(' '))
        .map(str::trim)
}

#[cfg(test)]
mod tests {
    use super::{BASE, CmdFlags, CommandTable, SkillEntry, match_cmd};

    fn has(table: &CommandTable, value: &str) -> bool {
        table.active().iter().any(|s| s.value == value)
    }

    /// The command names alone (Go's `commandNames`, chat/completion.go:102-110, which fed the
    /// banner until X-46): rows carrying a `label` are per-skill entries, not commands.
    fn names(table: &CommandTable) -> Vec<String> {
        table
            .active()
            .into_iter()
            .filter(|s| s.label.is_empty())
            .map(|s| s.value)
            .collect()
    }

    /// `setActiveCommands(agent, save, compact, image)` as a tuple, in Go's argument order.
    fn table((agent, save, compact, image): (bool, bool, bool, bool)) -> CommandTable {
        CommandTable::new(CmdFlags {
            save,
            compact,
            agent,
            image,
        })
    }

    // Go: chat/completion_test.go:21 TestConditionalCommandVisibility — /skills joins the
    // table only in agent mode, /save only for sessions that started ephemeral, the toggles
    // compose, /compact needs token accounting, /edit + /redo need an image provider, the
    // base table is never mutated, and every base row carries a description.
    #[test]
    fn test_conditional_command_visibility() {
        let t = table((false, false, true, false));
        assert!(
            !has(&t, "/skills") && !has(&t, "/save"),
            "conditional commands visible with both toggles off"
        );
        let t = table((true, false, true, false));
        assert!(
            has(&t, "/skills") && !has(&t, "/save"),
            "agent-only toggle leaked or missed"
        );
        let t = table((false, true, true, false));
        assert!(
            !has(&t, "/skills") && has(&t, "/save"),
            "save-only toggle leaked or missed"
        );
        let t = table((true, true, true, false));
        assert!(
            has(&t, "/skills") && has(&t, "/save"),
            "both toggles must compose"
        );
        // A token-less provider (compact=false) drops /compact from the table.
        let t = table((false, false, false, false));
        assert!(
            !has(&t, "/compact"),
            "/compact visible without token accounting"
        );
        let t = table((false, false, true, false));
        assert!(
            has(&t, "/compact"),
            "/compact missing for a token-aware provider"
        );
        // /edit exists only on image providers (the typical shape: image=true comes with
        // compact=false).
        let t = table((false, false, false, true));
        assert!(
            has(&t, "/edit") && has(&t, "/redo"),
            "/edit and /redo missing for an image provider"
        );
        let t = table((false, false, true, false));
        assert!(
            !has(&t, "/edit") && !has(&t, "/redo"),
            "image commands visible on a text provider"
        );
        // The base table itself is never mutated.
        let base: Vec<&str> = BASE.iter().map(|c| c.value).collect();
        assert!(
            !base.contains(&"/skills")
                && !base.contains(&"/save")
                && !base.contains(&"/edit")
                && base.contains(&"/compact"),
            "base slashCommands polluted"
        );
        // Every base entry carries a description: the list shows it beside the name, and a
        // blank column is the same as not having the feature.
        for c in BASE {
            assert!(!c.desc.is_empty(), "{} has no description", c.value);
        }
        // Go's fixed order with every group on (completion.go:78-97).
        let t = table((true, true, true, true));
        assert_eq!(
            names(&t),
            [
                "/file", "/session", "/model", "/compact", "/export", "/status", "/tools",
                "/debug", "/edit", "/redo", "/save", "/skills"
            ]
        );
        assert!(t.save_enabled() && t.image_enabled() && t.agent_enabled());
        let off = CommandTable::new(CmdFlags::default());
        assert!(!off.save_enabled() && !off.image_enabled() && !off.agent_enabled());
        // The two always-on T3 rows are in the table under every flag combination.
        for t in [
            table((false, false, false, false)),
            table((true, true, true, true)),
        ] {
            assert!(has(&t, "/export") && has(&t, "/debug"));
        }
    }

    // Go: chat/skillcmd_test.go:97 TestSkillCommandsCompletion (the table half) — each skill
    // registers a whole `/skills <name>` entry labelled with the bare name, after `/skills`;
    // outside agent mode neither the command nor its skills exist.
    #[test]
    fn test_skill_commands_completion() {
        let skills = vec![
            SkillEntry {
                name: "brain-page".to_owned(),
                description: "brain pages".to_owned(),
            },
            SkillEntry {
                name: "code-review".to_owned(),
                description: String::new(),
            },
        ];
        let mut t = table((true, false, true, false));
        t.set_skills(skills.clone());
        let joined = t
            .active()
            .iter()
            .map(|c| c.value.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        for want in ["/skills", "/skills brain-page", "/skills code-review"] {
            assert!(
                joined.contains(want),
                "command table missing {want:?}: {joined}"
            );
        }
        // The row shows the bare name, not the already-typed prefix, and the description.
        let row = t
            .active()
            .into_iter()
            .find(|c| c.value == "/skills brain-page")
            .expect("skill row");
        assert_eq!(
            row.label, "brain-page",
            "skill entry label wants the bare name"
        );
        assert_eq!(row.desc, "brain pages");
        // The per-skill rows follow /skills and are not commands.
        let values: Vec<String> = t.active().into_iter().map(|c| c.value).collect();
        let skills_at = values.iter().position(|v| v == "/skills").expect("/skills");
        assert_eq!(
            &values[skills_at..],
            ["/skills", "/skills brain-page", "/skills code-review"]
        );
        assert!(!names(&t).iter().any(|n| n.starts_with("/skills ")));

        // Outside agent mode neither the command nor its skills exist.
        let mut t = table((false, false, true, false));
        t.set_skills(skills);
        for c in t.active() {
            assert!(
                !c.value.starts_with("/skill"),
                "skill commands leaked outside agent mode: {:?}",
                c.value
            );
        }
    }

    /// The prefix-match rule every dispatch arm uses: the bare command, or the command
    /// plus a space; a longer name never matches a shorter command.
    #[test]
    fn match_cmd_takes_the_bare_form_and_a_spaced_argument() {
        assert_eq!(match_cmd("/save", "/save"), Some(""));
        assert_eq!(match_cmd("/save  my chat  ", "/save"), Some("my chat"));
        assert_eq!(match_cmd("/save ", "/save"), Some(""));
        assert_eq!(match_cmd("/saved", "/save"), None);
        assert_eq!(match_cmd("say /save", "/save"), None);
    }
}
