//! The `/model` surface's read-only System tab (chat/systemtab.go).
//!
//! The text shown is the prompt AS SENT, not as configured: the `-s` flag, the config
//! `system:`/`system_file:` keys, the interactive `-S` entry and a resumed session's
//! stored prompt all converge on `history[0]`, while the built-in harness (an agent with
//! tools) and agent mode's AGENTS.md overlay are folded in at send time. Composing through
//! the SAME [`crate::agents::compose_send_history`] the turn loop uses keeps that assembly
//! rule in one place — **the tab cannot drift from the wire**. A second, "display-only"
//! composition would be a second source of truth, and the one thing a read-only tab must
//! never do is lie about what is being sent.

use crate::agents::compose_send_history;
use crate::provider::model::Message;
use crate::ui::facade::Panel;

/// The System tab for this chat, or `None` when it carries no system prompt at all —
/// the surface then stays as it was rather than showing an empty view.
pub(crate) fn system_prompt_panel(
    history: &[Message],
    harness: &str,
    overlay: &str,
) -> Option<Panel> {
    let sent = compose_send_history(history, harness, overlay);
    let first = sent.first()?;
    if first.role() != crate::provider::model::Role::System || first.content.is_empty() {
        return None;
    }
    Some(
        Panel::view(
            "System".to_owned(),
            first.content.split('\n').map(str::to_owned).collect(),
        )
        .with_prompt("System prompt in effect (read-only)".to_owned())
        .with_wrap(true),
    )
}

#[cfg(test)]
mod tests {
    use super::system_prompt_panel;
    use crate::provider::model::Message;
    use crate::ui::facade::PanelKind;

    fn user(text: &str) -> Message {
        Message {
            content: text.to_owned(),
            ..Message::default()
        }
    }

    // Go: chat/systemtab_test.go:16 TestSystemPromptPanelAbsent — no system message (or
    // an empty one) means no tab: the /model surface stays as it was rather than showing
    // an empty view.
    #[test]
    fn test_system_prompt_panel_absent() {
        let cases: [(&str, Vec<Message>); 3] = [
            ("no history", Vec::new()),
            ("conversation only", vec![user("hi")]),
            (
                "empty system",
                vec![Message::system(String::new()), user("hi")],
            ),
        ];
        for (name, history) in cases {
            assert!(
                system_prompt_panel(&history, "", "").is_none(),
                "{name}: panel offered with no system prompt in effect"
            );
        }
    }

    // Go: chat/systemtab_test.go:36 TestSystemPromptPanelShape — the read-only tab is a
    // View panel that wraps, carrying the prompt split into lines.
    #[test]
    fn test_system_prompt_panel_shape() {
        let history = vec![
            Message::system("You are terse.\nAnswer in one line."),
            user("hi"),
        ];
        let p = system_prompt_panel(&history, "", "").expect("panel not offered");
        assert_eq!(p.title, "System");
        assert_eq!(p.kind(), PanelKind::View);
        assert!(p.wrap(), "the System tab must wrap: a prompt is prose");
        assert_eq!(p.prompt, "System prompt in effect (read-only)");
        assert_eq!(p.lines(), ["You are terse.", "Answer in one line."]);
        // Read-only means no way to commit it: a View carries no value, no text and no
        // checks, so the questionnaire's commit reads nothing back from this tab.
        assert!(
            p.items().is_empty()
                && p.as_input()
                    .map_or_else(String::new, |i| i.text.clone())
                    .is_empty()
                && !p.custom()
        );
    }

    // Go: chat/systemtab_test.go:59 TestSystemPromptPanelFromConfig — the config path end
    // to end: a `system_file:` prompt resolves to the same `history[0]` the tab reads, so
    // a prompt defined in the config file shows up like a `-s` one.
    //
    // Adjusted: `ProviderConfig::resolve_system` belongs to the command layer, and its own
    // resolution rules are pinned by the config suite; what belongs HERE is the second
    // half of the Go test — whatever text the resolution produced, seeded into
    // `history[0]`, is what the tab renders. The file read stands in for the resolver.
    #[test]
    fn test_system_prompt_panel_from_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("sys.md");
        std::fs::write(&file, b"You are a config-defined assistant.").expect("write");
        let resolved = std::fs::read_to_string(&file).expect("read");

        // cmd/root.go hands the resolved text to chat.Run, which seeds history[0].
        let history = vec![Message::system(resolved)];
        let p = system_prompt_panel(&history, "", "").expect("config prompt produced no tab");
        assert_eq!(p.lines().join("\n"), "You are a config-defined assistant.");
    }

    // Go: chat/systemtab_test.go:83 TestSystemPromptPanelIncludesOverlay — agent mode
    // appends the AGENTS.md overlay to the system message at send time, so the tab —
    // showing the prompt AS SENT — must carry both parts.
    #[test]
    fn test_system_prompt_panel_includes_overlay() {
        let history = vec![Message::system("Base prompt.")];
        let p = system_prompt_panel(&history, "", "# AGENTS.md\nProject rules.").expect("no panel");
        let body = p.lines().join("\n");
        assert!(
            body.contains("Base prompt.") && body.contains("Project rules."),
            "lines = {body:?}, want both the base prompt and the overlay"
        );
        // The overlay is welded on with the same blank line the wire carries.
        assert_eq!(body, "Base prompt.\n\n# AGENTS.md\nProject rules.");
    }

    // Go: chat/systemtab_test.go:97 TestSystemPromptPanelOverlayOnly — agent mode without
    // any user prompt: compose_send_history synthesizes the system message, and the tab
    // shows it.
    #[test]
    fn test_system_prompt_panel_overlay_only() {
        let p = system_prompt_panel(&[user("hi")], "", "Project rules.").expect("no panel");
        assert_eq!(p.lines().join("\n"), "Project rules.");
    }

    /// The built-in harness is part of what is sent, so the tab shows it — first, with the
    /// user's prompt inside `<instructions>` after it — and shows it even for a chat that set
    /// no prompt of its own.
    #[test]
    fn test_system_prompt_panel_includes_the_harness() {
        let history = vec![Message::system("Base prompt."), user("hi")];
        let p = system_prompt_panel(&history, "<environment>\nx\n</environment>", "")
            .expect("no panel");
        assert_eq!(
            p.lines(),
            [
                "<environment>",
                "x",
                "</environment>",
                "",
                "<instructions>",
                "Base prompt.",
                "</instructions>"
            ]
        );
        let p = system_prompt_panel(&[user("hi")], "HARNESS", "").expect("no panel");
        assert_eq!(p.lines(), ["HARNESS"]);
    }
}
