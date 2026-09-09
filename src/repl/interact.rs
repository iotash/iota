//! `tool.Interactor` impl over `Ui::tabbed` (chat/interact.go): one tab per question
//! (short header as the chip, `Panel.prompt` as the body), wizard Enter
//! (`enter_advances`), and the engine's inline `"Other…"` editor for custom answers
//! (`Panel.custom`). Created UNBOUND (the dispatcher is built before the UI exists) and
//! bound to the live facade at the top of the run loop.

use std::sync::{Arc, OnceLock};

use crate::BoxFuture;
use crate::chat::turns::RunCtx;
use crate::tool::{AskAnswer, AskResult, AskSpec};
use crate::ui::facade::{ListBody, Panel, PanelBody, TabbedSpec, Ui};

use crate::repl::styles::dim;

/// The ask-wizard bridge: bound to the facade once the TUI is up; unbound asks decline.
pub(crate) struct Interactor {
    ui: OnceLock<Arc<dyn Ui>>,
}

impl Interactor {
    /// An unbound bridge (asks decline until [`Interactor::bind`]).
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            ui: OnceLock::new(),
        })
    }

    /// Binds the facade (once; later binds are ignored).
    pub(crate) fn bind(&self, ui: Arc<dyn Ui>) {
        let _ = self.ui.set(ui);
    }
}

impl crate::tool::Interactor for Interactor {
    /// Maps an `AskSpec` onto the tabbed surface (interact.go:28-81): unbound →
    /// Declined; cancel → Declined; the `"Other…"` index → the Custom answer. A facade
    /// error (closed / interrupted) also resolves Declined — the frozen seam is
    /// errorless, and a declined ask is the answer the tools already handle (Go returned
    /// the error as a hard tool failure; recorded in DEVIATIONS3).
    fn ask<'a>(&'a self, cx: &'a RunCtx, spec: AskSpec) -> BoxFuture<'a, AskResult> {
        Box::pin(async move {
            let declined = || AskResult {
                declined: true,
                answers: Vec::new(),
            };
            let Some(ui) = self.ui.get() else {
                return declined(); // unbound: non-interactive
            };
            let panels = spec
                .questions
                .iter()
                .map(|q| {
                    let items = q
                        .options
                        .iter()
                        .map(|o| {
                            if o.description.is_empty() {
                                o.label.clone()
                            } else {
                                format!("{}  {}", o.label, dim(&o.description))
                            }
                        })
                        .collect();
                    let rows = ListBody {
                        items,
                        custom: q.allow_custom,
                        ..ListBody::default()
                    };
                    Panel::of(
                        q.header.clone(),
                        if q.multiple {
                            PanelBody::Multi(rows)
                        } else {
                            PanelBody::List(rows)
                        },
                    )
                    .with_prompt(q.question.clone())
                })
                .collect();
            let spec_ui = TabbedSpec {
                panels,
                enter_advances: true,
                ..TabbedSpec::default()
            };
            let Ok(r) = ui.tabbed(&cx.cancel, spec_ui).await else {
                return declined();
            };
            if r.cancelled {
                return declined();
            }

            let mut res = AskResult {
                declined: false,
                answers: vec![AskAnswer::default(); spec.questions.len()],
            };
            for (i, q) in spec.questions.iter().enumerate() {
                let Some(pr) = r.panels.get(i) else { continue };
                let other_idx = q.options.len();
                if q.multiple {
                    // Picks and a custom answer COEXIST on a multi-select.
                    for &c in &pr.checked {
                        if q.allow_custom && c == other_idx {
                            res.answers[i].custom.clone_from(&pr.custom);
                        } else if let Some(o) = q.options.get(c) {
                            res.answers[i].selected.push(o.label.clone());
                        }
                    }
                    continue;
                }
                if q.allow_custom && pr.cursor == other_idx {
                    res.answers[i].custom.clone_from(&pr.custom);
                    continue;
                }
                if let Some(o) = q.options.get(pr.cursor) {
                    res.answers[i].selected = vec![o.label.clone()];
                }
            }
            res
        })
    }
}
