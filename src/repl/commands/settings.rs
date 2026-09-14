//! The full `/model` questionnaire beyond the Model tab (chat/settings.go +
//! chat/run.go:513-716) — WP54's half of T-12.
//!
//! **Tabs assemble by CAPABILITY.** Only panels the provider can actually act on appear:
//! a dialect without `Tunable` shows no Effort tab rather than a tab whose commit would
//! be dropped. Because the tab list therefore varies, the commit reads the panels back by
//! the INDEX each one recorded while it was being built ([`Extras`]) — never by position
//! in a fixed list.
//!
//! **The untouched-tab-is-a-no-op law.** Every row builder here inserts the CURRENT value
//! when the offered list does not carry it (context: sorted in; effort: appended; choice:
//! appended after the leading `"default"` row) and marks it `" (current)"` with the cursor
//! on it. Submitting a tab the user never visited must change nothing — otherwise opening
//! `/model` to adjust the temperature would silently reset a session's effort to the first
//! row of a list it was never in.
//!
//! **Commit from anywhere.** `enter_advances` stays false: Enter on ANY tab commits the
//! whole surface (the ask wizard is the only place that advances). Each knob whose value
//! actually moved applies to the provider, persists into the session bundle through the
//! ONE `update_meta` call site, and prints its own dim notice; no delta at all prints
//! `"No changes."`.

use std::sync::PoisonError;

use crate::provider::{Effort, ImageGenParams, Provider, ProviderKind};
use crate::session::{ParamSource, ParamSources};
use crate::ui::facade::{Panel, TabbedResult};

use crate::repl::commands::status::image_gen_label;
use crate::repl::run::Repl;

/// The Context tab's stock window sizes (chat/settings.go `contextPresets`).
const CONTEXT_PRESETS: [u64; 6] = [8_000, 32_000, 128_000, 200_000, 256_000, 1_000_000];

/// The Effort tab's choices; `""` is the "default" row, meaning the parameter is omitted
/// from requests entirely (chat/settings.go `effortLevels`). Levels are passed to the
/// provider verbatim — a value the model does not support fails visibly at the API and
/// the user picks another.
const EFFORT_LEVELS: [&str; 6] = ["", "low", "medium", "high", "xhigh", "max"];

/// Anthropic caps temperature at 1.0; every other dialect at 2.0 (chat/run.go:566-569).
const MAX_TEMP_ANTHROPIC: f64 = 1.0;
/// The default temperature ceiling.
const MAX_TEMP: f64 = 2.0;
/// The temperature slider's granularity.
const TEMP_STEP: f64 = 0.1;
/// The Negative-prompt field's width, matching the Model tab's manual field.
const NEGATIVE_INPUT_WIDTH: usize = 40;

// --- row builders (chat/settings.go) ----------------------------------------

/// Renders an effort level for display (`""` → `"default"`).
pub(crate) fn effort_label(level: &str) -> &str {
    if level.is_empty() { "default" } else { level }
}

/// Renders an optional temperature for display (`None` → `"default"`, otherwise the
/// fewest digits that round-trip — Go `strconv.FormatFloat(v, 'f', -1, 64)`).
pub(crate) fn format_temperature(v: Option<f64>) -> String {
    v.map_or_else(|| "default".to_owned(), |t| format!("{t}"))
}

/// Whether two optional temperatures hold the same value (both unset, or both set and
/// equal) — Go `floatPtrEqual`, kept as a named function because the commit's "did this
/// knob move?" question is exactly this comparison.
///
/// The comparison is EXACT on purpose: the slider hands back the same figure it was given
/// (its steps are integer indices), so "did the user move it?" is bit equality, not an
/// epsilon — and an epsilon would swallow a real one-step move at `step` below it.
#[allow(clippy::float_cmp)]
pub(crate) fn float_ptr_equal(a: Option<f64>, b: Option<f64>) -> bool {
    match (a, b) {
        (Some(x), Some(y)) => x == y,
        (x, y) => x.is_none() && y.is_none(),
    }
}

/// A List tab's rows: the values behind them, their labels, and the marked (current) row.
pub(crate) struct Rows<T> {
    /// The value each row commits.
    pub(crate) values: Vec<T>,
    /// The row labels, the current one suffixed ` (current)`.
    pub(crate) labels: Vec<String>,
    /// The current row.
    pub(crate) cursor: usize,
}

/// The Context tab's rows: the preset windows plus the current one (inserted SORTED when
/// it is not a preset), the current row marked.
pub(crate) fn context_window_rows(current: u64) -> Rows<u64> {
    let mut values = CONTEXT_PRESETS.to_vec();
    if !values.contains(&current) && current > 0 {
        values.push(current);
        values.sort_unstable();
    }
    let mut cursor = 0;
    let labels = values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let label = crate::text::tokens(*v);
            if *v == current {
                cursor = i;
                return format!("{label} (current)");
            }
            label
        })
        .collect();
    Rows {
        values,
        labels,
        cursor,
    }
}

/// The Effort tab's rows: the known levels plus the current one (APPENDED when unknown,
/// so an untouched tab stays a no-op), the current row marked. `""` is the default row.
///
/// The parameter is the Go string rather than [`Effort`] deliberately: a bundle written by
/// a newer build can carry a level this build's enum has no variant for, and the row
/// builder is where that case is absorbed — the commit converts back through
/// [`Effort::parse`], which is the one place an unknown level can be rejected.
pub(crate) fn effort_rows(current: &str) -> Rows<String> {
    let mut values: Vec<String> = EFFORT_LEVELS.iter().map(|s| (*s).to_owned()).collect();
    if !values.iter().any(|v| v == current) {
        values.push(current.to_owned());
    }
    let mut cursor = 0;
    let labels = values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let label = effort_label(v).to_owned();
            if v == current {
                cursor = i;
                return format!("{label} (current)");
            }
            label
        })
        .collect();
    Rows {
        values,
        labels,
        cursor,
    }
}

/// A List tab over an optional string parameter: row 0 is `"default"` (omit the
/// parameter), the offered options follow, and a configured value outside the list is
/// appended so an untouched tab stays a no-op (the `model_rows` convention).
pub(crate) fn choice_rows(current: &str, options: &[&str]) -> Rows<String> {
    let mut values = vec![String::new()];
    values.extend(options.iter().map(|o| (*o).to_owned()));
    if !values.iter().any(|v| v == current) {
        values.push(current.to_owned());
    }
    let mut cursor = 0;
    let labels = values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let label = if v.is_empty() {
                "default".to_owned()
            } else {
                v.clone()
            };
            if v == current {
                cursor = i;
                return format!("{label} (current)");
            }
            label
        })
        .collect();
    Rows {
        values,
        labels,
        cursor,
    }
}

// --- the capability-assembled tabs ------------------------------------------

/// Where each capability's tab landed in the panel list, plus the row VALUES its cursor
/// indexes. Built by [`Extras::assemble`] while the panels are appended, read back by
/// [`Extras::apply`] — the two halves of "commit reads the panels back by recorded
/// index".
#[derive(Default)]
pub(crate) struct Extras {
    ctx: Option<usize>,
    windows: Vec<u64>,
    /// The window the Context tab OPENED on. The commit compares against this rather than against the live
    /// budget, because a model switch in the same surface may have moved the live one: an untouched tab must
    /// stay a no-op, and a touched one must win (brain page `model-param-layering`).
    window_open: u64,
    effort: Option<usize>,
    levels: Vec<String>,
    /// The effort the Effort tab opened on (see [`Extras::window_open`]).
    effort_open: String,
    temp: Option<usize>,
    /// The temperature the Temperature tab opened on (see [`Extras::window_open`]).
    temp_open: Option<f64>,
    image: Option<usize>,
    aspect: Option<usize>,
    aspect_values: Vec<String>,
    size: Option<usize>,
    size_values: Vec<String>,
    negative: Option<usize>,
    json_edits: Option<usize>,
}

impl Extras {
    /// Appends every tab this provider can act on to `panels`, after the Model tab
    /// (chat/run.go:544-630). `window` is the live context budget; `history`/`overlay`
    /// feed the read-only System tab.
    ///
    /// The capability probes run in Go's order — Context, Effort, Temperature, Image,
    /// Aspect/Size/Negative, JSON edits, System — because the recorded indices ARE the
    /// commit contract and a reordering would silently rewire every knob.
    pub(crate) fn assemble(
        provider: &mut dyn Provider,
        window: u64,
        history: &[crate::provider::model::Message],
        overlay: &str,
        panels: &mut Vec<Panel>,
    ) -> Self {
        let mut ex = Self::default();
        let kind = provider.kind();
        // A dedicated image provider takes no system prompt — its request carries the
        // last user text alone — so the System tab stays out of that surface entirely.
        let image_provider = provider.as_image_gen_tunable().is_some();

        if provider.reports_usage() {
            let Rows {
                values: windows,
                labels,
                cursor,
            } = context_window_rows(window);
            ex.windows = windows;
            ex.window_open = window;
            ex.ctx = Some(panels.len());
            panels.push(list_panel("Context", labels, cursor));
        }

        if let Some(tunable) = provider.as_tunable() {
            let current = tunable.effort().map_or("", Effort::as_str).to_owned();
            let temperature = tunable.temperature();
            let Rows {
                values: levels,
                labels,
                cursor,
            } = effort_rows(&current);
            // Recorded BEFORE the rows are handed over: what the tab opened on is the commit's baseline.
            ex.levels = levels;
            ex.effort_open = current;
            ex.effort = Some(panels.len());
            panels.push(list_panel("Effort", labels, cursor));

            let max = if kind == ProviderKind::Anthropic {
                MAX_TEMP_ANTHROPIC
            } else {
                MAX_TEMP
            };
            ex.temp = Some(panels.len());
            ex.temp_open = temperature;
            panels.push(Panel::slider(
                "Temperature".to_owned(),
                0.0,
                max,
                TEMP_STEP,
                temperature,
            ));
        }

        // The image-generation switch: only for providers whose request builders consult
        // it (google modalities, responses builtin tool).
        if let Some(image) = provider.as_image_tunable() {
            let on = image.image_output();
            ex.image = Some(panels.len());
            panels.push(
                Panel::switch("Image".to_owned(), on).with_prompt(
                    "Request image generation (modalities / built-in tool)".to_owned(),
                ),
            );
        }

        // Generation-parameter tabs for dedicated image providers: the choice lists come
        // from the provider, and each knob appears only when the dialect has it (imagen
        // offers ratios + sizes + negative, images only sizes).
        if let Some(image_gen) = provider.as_image_gen_tunable() {
            let options = image_gen.image_gen_options();
            let current = image_gen.image_gen_params().clone();
            if !options.aspect_ratios.is_empty() {
                let Rows {
                    values,
                    labels,
                    cursor,
                } = choice_rows(
                    current.aspect_ratio.as_deref().unwrap_or_default(),
                    &options.aspect_ratios,
                );
                ex.aspect_values = values;
                ex.aspect = Some(panels.len());
                panels.push(
                    list_panel("Aspect", labels, cursor)
                        .with_prompt("Aspect ratio of generated images".to_owned()),
                );
            }
            if !options.image_sizes.is_empty() {
                let Rows {
                    values,
                    labels,
                    cursor,
                } = choice_rows(
                    current.image_size.as_deref().unwrap_or_default(),
                    &options.image_sizes,
                );
                ex.size_values = values;
                ex.size = Some(panels.len());
                panels.push(
                    list_panel("Size", labels, cursor)
                        .with_prompt("Output resolution tier".to_owned()),
                );
            }
            if options.negative_prompt {
                ex.negative = Some(panels.len());
                panels.push(
                    Panel::input(
                        "Negative".to_owned(),
                        current.negative_prompt.unwrap_or_default(),
                        "what to avoid (empty = none)".to_owned(),
                    )
                    .with_prompt("Negative prompt (backend support varies)".to_owned())
                    .with_input_width(NEGATIVE_INPUT_WIDTH),
                );
            }
        }

        // The edit wire format, where the dialect has two (OpenAI Images).
        if let Some(edits) = provider.as_image_edit_json_tunable() {
            let on = edits.json_edits();
            ex.json_edits = Some(panels.len());
            panels.push(
                Panel::switch("JSON edits".to_owned(), on)
                    .with_prompt("Send /images/edits as JSON instead of multipart".to_owned()),
            );
        }

        // Last tab, and read-only: the knobs above keep their positions (and their
        // recorded indices) while this one just shows what the chat is running under.
        if !image_provider
            && let Some(system) = crate::repl::systemtab::system_prompt_panel(history, overlay)
        {
            panels.push(system);
        }
        ex
    }

    /// Applies every knob whose value actually moved: provider, session bundle, notice.
    /// Returns whether anything changed (`false` makes the caller print `"No changes."`).
    pub(crate) fn apply(&self, r: &TabbedResult, repl: &mut Repl) -> bool {
        let mut changed = false;

        if let Some(i) = self.ctx
            && let Some(v) = self.windows.get(cursor_at(r, i)).copied()
            && v != self.window_open
        {
            repl.budget.set_window(v);
            by_hand(
                repl,
                |s| &mut s.context_window,
                move |m| {
                    m.set_context_window(v);
                },
            );
            repl.tr
                .notice(&format!("Context window: {}", repl.budget.status()));
            changed = true;
        }

        if let Some(i) = self.effort {
            let picked = self
                .levels
                .get(cursor_at(r, i))
                .cloned()
                .unwrap_or_default();
            if picked != self.effort_open {
                // An unparseable level cannot come from the rows this build offers; it
                // only exists as the "current" row a newer bundle wrote, and picking that
                // row is caught by the `picked != current` guard above.
                if let Ok(effort) = Effort::optional(&picked) {
                    if let Some(t) = repl.provider.as_tunable() {
                        t.set_effort(effort);
                    }
                    let level = picked.clone();
                    by_hand(
                        repl,
                        |s| &mut s.effort,
                        move |m| {
                            level.clone_into(&mut m.effort);
                        },
                    );
                    repl.tr
                        .notice(&format!("Effort: {}", effort_label(&picked)));
                    changed = true;
                }
            }
        }

        if let Some(i) = self.temp {
            let picked = r.panels.get(i).and_then(|p| p.value);
            if !float_ptr_equal(picked, self.temp_open) {
                if let Some(t) = repl.provider.as_tunable() {
                    t.set_temperature(picked);
                }
                by_hand(
                    repl,
                    |s| &mut s.temperature,
                    move |m| {
                        m.temperature = picked;
                    },
                );
                repl.tr
                    .notice(&format!("Temperature: {}", format_temperature(picked)));
                changed = true;
            }
        }

        if let Some(i) = self.image {
            let on = r.panels.get(i).is_some_and(|p| p.on);
            let current = repl
                .provider
                .as_image_tunable()
                .is_some_and(|img| img.image_output());
            if on != current {
                if let Some(img) = repl.provider.as_image_tunable() {
                    img.set_image_output(on);
                }
                update_meta(repl, |m| m.image = on);
                repl.tr.notice(&format!(
                    "Image generation: {}",
                    if on { "on" } else { "off" }
                ));
                changed = true;
            }
        }

        changed |= self.apply_image_gen(r, repl);

        if let Some(i) = self.json_edits {
            let on = r.panels.get(i).is_some_and(|p| p.on);
            let current = repl
                .provider
                .as_image_edit_json_tunable()
                .is_some_and(|edits| edits.json_edits());
            if on != current {
                if let Some(edits) = repl.provider.as_image_edit_json_tunable() {
                    edits.set_json_edits(on);
                }
                update_meta(repl, |m| m.json_edits = on);
                repl.tr.notice(&format!(
                    "Image edits sent as: {}",
                    if on { "JSON" } else { "multipart" }
                ));
                changed = true;
            }
        }
        changed
    }

    /// The three generation knobs commit TOGETHER (one notice, one bundle write): they
    /// start from the provider's current params, so a knob without a tab keeps its value
    /// (chat/run.go:686-700).
    fn apply_image_gen(&self, r: &TabbedResult, repl: &mut Repl) -> bool {
        let Some(current) = repl
            .provider
            .as_image_gen_tunable()
            .map(|g| g.image_gen_params().clone())
        else {
            return false;
        };
        let mut next = current.clone();
        if let Some(i) = self.aspect {
            next.aspect_ratio = self
                .aspect_values
                .get(cursor_at(r, i))
                .filter(|v| !v.is_empty())
                .cloned();
        }
        if let Some(i) = self.size {
            next.image_size = self
                .size_values
                .get(cursor_at(r, i))
                .filter(|v| !v.is_empty())
                .cloned();
        }
        if let Some(i) = self.negative {
            next.negative_prompt = r
                .panels
                .get(i)
                .map(|p| p.text.trim())
                .filter(|t| !t.is_empty())
                .map(str::to_owned);
        }
        if next == current {
            return false;
        }
        let label = image_gen_label(&next);
        if let Some(g) = repl.provider.as_image_gen_tunable() {
            g.set_image_gen_params(next.clone());
        }
        update_meta(repl, |m| {
            let ImageGenParams {
                aspect_ratio,
                image_size,
                negative_prompt,
            } = next;
            m.aspect_ratio = aspect_ratio.unwrap_or_default();
            m.image_size = image_size.unwrap_or_default();
            m.negative_prompt = negative_prompt.unwrap_or_default();
        });
        repl.tr.notice(&format!("Image params: {label}"));
        true
    }
}

/// A capability tab's List panel — the shape every row builder feeds.
fn list_panel(title: &str, items: Vec<String>, cursor: usize) -> Panel {
    Panel::list(title.to_owned(), items).with_cursor(cursor)
}

/// The committed cursor of panel `i` (0 when the surface returned fewer panels than were
/// opened — impossible through the engine, and a silent 0 beats an index panic).
fn cursor_at(r: &TabbedResult, i: usize) -> usize {
    r.panels.get(i).map_or(0, |p| p.cursor)
}

/// Commits one knob the user moved BY HAND: the value, and the source that says it is the session's own
/// from now on, in ONE bundle write (brain page `model-param-layering`).
///
/// A hand-set value outranks nothing — the next declaration still wins — but it survives a model switch that
/// finds no declaration at all, which a value inherited from the model being left behind must not.
fn by_hand(
    repl: &mut Repl,
    which: fn(&mut ParamSources) -> &mut ParamSource,
    value: impl FnOnce(&mut crate::session::SessionMeta),
) {
    *which(&mut repl.param_sources) = ParamSource::User;
    let sources = repl.param_sources;
    update_meta(repl, |m| {
        value(m);
        m.param_sources = Some(sources);
    });
}

/// The ONE session-bundle write for the questionnaire: what `/model` changes and what a
/// resumed session replays cannot drift, because they are the same call site.
fn update_meta(repl: &Repl, f: impl FnOnce(&mut crate::session::SessionMeta)) {
    if let Some(w) = repl
        .writer
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_mut()
    {
        let _ = w.update_meta(f);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Rows, choice_rows, context_window_rows, effort_label, effort_rows, float_ptr_equal,
        format_temperature,
    };

    // Go: chat/settings_test.go:8 TestContextWindowRowsPresetCurrent — a preset current
    // window must not grow the list, and its row is the marked cursor.
    #[test]
    fn test_context_window_rows_preset_current() {
        let Rows {
            values,
            labels,
            cursor,
        } = context_window_rows(128_000);
        assert_eq!(values.len(), 6, "a preset current must not grow the list");
        assert_eq!(values[cursor], 128_000);
        assert_eq!(labels[cursor], "128k (current)");
    }

    // Go: chat/settings_test.go:19 TestContextWindowRowsInsertsNonPresetSorted — a
    // non-preset current window is inserted IN ORDER, so the list still reads as a scale.
    #[test]
    fn test_context_window_rows_inserts_non_preset_sorted() {
        let Rows {
            values,
            labels,
            cursor,
        } = context_window_rows(64_000);
        assert_eq!(values.len(), 7, "a non-preset current must be inserted");
        assert!(values.windows(2).all(|w| w[0] <= w[1]), "values not sorted");
        assert_eq!(values[cursor], 64_000);
        assert_eq!(labels[cursor], "64k (current)");

        // A window of 0 (never configured) inserts nothing and marks nothing: the
        // budget's own default is what the loop actually runs with.
        let Rows {
            values,
            labels,
            cursor,
        } = context_window_rows(0);
        assert_eq!(values.len(), 6);
        assert_eq!(cursor, 0);
        assert!(labels.iter().all(|l| !l.contains("(current)")));
    }

    // Go: chat/settings_test.go:32 TestEffortRows — the unset level maps to the "default"
    // row, a known level is marked in place, and an unknown one (from a newer session) is
    // APPENDED so an untouched tab stays a no-op.
    #[test]
    fn test_effort_rows() {
        let Rows {
            values,
            labels,
            cursor,
        } = effort_rows("");
        assert_eq!(values.len(), 6);
        assert_eq!(values[cursor], "");
        assert_eq!(labels[cursor], "default (current)");

        let Rows {
            values,
            labels,
            cursor,
        } = effort_rows("high");
        assert_eq!(values.len(), 6);
        assert_eq!(values[cursor], "high");
        assert_eq!(labels[cursor], "high (current)");

        let Rows {
            values,
            labels,
            cursor,
        } = effort_rows("turbo");
        assert_eq!(
            cursor, 6,
            "an unknown level must be appended, not substituted"
        );
        assert_eq!(values[cursor], "turbo");
        assert_eq!(labels[cursor], "turbo (current)");
    }

    // Go: chat/settings_test.go:101 TestChoiceRows — "default" row first (omit the
    // parameter), the options follow, an out-of-list configured value is appended.
    #[test]
    fn test_choice_rows() {
        let opts = ["1:1", "3:2"];

        let Rows {
            values,
            labels,
            cursor: idx,
        } = choice_rows("", &opts);
        assert_eq!(values.len(), 3);
        assert_eq!(values[0], "");
        assert_eq!(labels[0], "default (current)");
        assert_eq!(idx, 0);

        let Rows {
            values,
            labels,
            cursor: idx,
        } = choice_rows("3:2", &opts);
        assert_eq!(idx, 2);
        assert_eq!(values[idx], "3:2");
        assert_eq!(labels[idx], "3:2 (current)");

        let Rows {
            values,
            labels,
            cursor: idx,
        } = choice_rows("21:9", &opts);
        assert_eq!(values.len(), 4);
        assert_eq!(idx, 3);
        assert_eq!(values[3], "21:9");
        assert_eq!(labels[3], "21:9 (current)");
    }

    // Go: chat/settings_test.go:80 TestFloatPtrEqual — the "did this knob move?" test the
    // temperature commit reads.
    #[test]
    fn test_float_ptr_equal() {
        let (a, b, c) = (0.7, 0.7, 0.8);
        assert!(float_ptr_equal(None, None));
        assert!(!float_ptr_equal(Some(a), None));
        assert!(!float_ptr_equal(None, Some(a)));
        assert!(float_ptr_equal(Some(a), Some(b)));
        assert!(!float_ptr_equal(Some(a), Some(c)));
    }

    /// The display forms the notices carry (chat/settings.go `effortLabel`,
    /// `formatTemperature`): the fewest digits that round-trip, never `0.7000000000000001`.
    #[test]
    fn labels_render_the_go_display_forms() {
        assert_eq!(effort_label(""), "default");
        assert_eq!(effort_label("xhigh"), "xhigh");
        assert_eq!(format_temperature(None), "default");
        assert_eq!(format_temperature(Some(0.7)), "0.7");
        assert_eq!(format_temperature(Some(1.0)), "1");
        assert_eq!(format_temperature(Some(1.5)), "1.5");
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, clippy::unnecessary_literal_bound)]
mod assemble_tests {
    use super::Extras;
    use crate::BoxFuture;
    use crate::provider::ImageGenTunable;
    use crate::provider::error::ProviderError;
    use crate::provider::model::Message;
    use crate::provider::{
        ChatResult, Effort, ImageEditJsonTunable, ImageGenOptions, ImageGenParams, ImageTunable,
        Provider, ProviderKind, Tunable,
    };
    use crate::ui::facade::{Panel, PanelKind};
    use tokio_util::sync::CancellationToken;

    /// A provider whose capabilities are switched on one at a time, so each tab can be
    /// shown to depend on exactly the probe that offers it.
    #[derive(Default)]
    struct Caps {
        kind: Option<ProviderKind>,
        usage: bool,
        tunable: bool,
        temperature: Option<f64>,
        effort: Option<Effort>,
        image: Option<bool>,
        image_gen: Option<(ImageGenOptions, ImageGenParams)>,
        json_edits: Option<bool>,
    }

    impl Provider for Caps {
        fn kind(&self) -> ProviderKind {
            self.kind.unwrap_or(ProviderKind::OpenAi)
        }
        fn model(&self) -> &str {
            "m"
        }
        fn set_model(&mut self, _model: String) {}
        fn list_models<'a>(
            &'a self,
            _cancel: &'a CancellationToken,
        ) -> BoxFuture<'a, Result<Vec<String>, ProviderError>> {
            Box::pin(std::future::ready(Ok(Vec::new())))
        }
        fn chat<'a>(
            &'a self,
            _cancel: &'a CancellationToken,
            _messages: &'a [Message],
        ) -> BoxFuture<'a, Result<ChatResult, ProviderError>> {
            Box::pin(std::future::ready(Ok(ChatResult::default())))
        }
        fn reports_usage(&self) -> bool {
            self.usage
        }
        fn as_tunable(&mut self) -> Option<&mut dyn Tunable> {
            self.tunable.then_some(self as &mut dyn Tunable)
        }
        fn as_image_tunable(&mut self) -> Option<&mut dyn ImageTunable> {
            self.image
                .is_some()
                .then_some(self as &mut dyn ImageTunable)
        }
        fn as_image_gen_tunable(&mut self) -> Option<&mut dyn ImageGenTunable> {
            self.image_gen
                .is_some()
                .then_some(self as &mut dyn ImageGenTunable)
        }
        fn as_image_edit_json_tunable(&mut self) -> Option<&mut dyn ImageEditJsonTunable> {
            self.json_edits
                .is_some()
                .then_some(self as &mut dyn ImageEditJsonTunable)
        }
    }

    impl Tunable for Caps {
        fn set_temperature(&mut self, t: Option<f64>) {
            self.temperature = t;
        }
        fn temperature(&self) -> Option<f64> {
            self.temperature
        }
        fn set_effort(&mut self, e: Option<Effort>) {
            self.effort = e;
        }
        fn effort(&self) -> Option<Effort> {
            self.effort
        }
    }

    impl ImageTunable for Caps {
        fn set_image_output(&mut self, on: bool) {
            self.image = Some(on);
        }
        fn image_output(&self) -> bool {
            self.image.unwrap_or(false)
        }
    }

    impl ImageGenTunable for Caps {
        fn set_image_gen_params(&mut self, p: ImageGenParams) {
            if let Some(slot) = self.image_gen.as_mut() {
                slot.1 = p;
            }
        }
        fn image_gen_params(&self) -> &ImageGenParams {
            static EMPTY: std::sync::OnceLock<ImageGenParams> = std::sync::OnceLock::new();
            self.image_gen
                .as_ref()
                .map_or_else(|| EMPTY.get_or_init(ImageGenParams::default), |g| &g.1)
        }
        fn image_gen_options(&self) -> ImageGenOptions {
            self.image_gen
                .as_ref()
                .map_or_else(ImageGenOptions::default, |g| g.0.clone())
        }
    }

    impl ImageEditJsonTunable for Caps {
        fn set_json_edits(&mut self, on: bool) {
            self.json_edits = Some(on);
        }
        fn json_edits(&self) -> bool {
            self.json_edits.unwrap_or(false)
        }
    }

    /// The tabs `assemble` appended, by title.
    fn tabs(p: &mut dyn Provider, history: &[Message], overlay: &str) -> (Vec<Panel>, Extras) {
        let mut panels = Vec::new();
        let ex = Extras::assemble(p, 128_000, history, overlay, &mut panels);
        (panels, ex)
    }

    fn titles(panels: &[Panel]) -> Vec<&str> {
        panels.iter().map(|p| p.title.as_str()).collect()
    }

    /// Go: chat/run.go:544-630 — tabs assemble by CAPABILITY: a provider that can do
    /// nothing gets no tabs at all beside the Model one the caller already built.
    #[test]
    fn a_bare_provider_gets_no_capability_tabs() {
        let (panels, _) = tabs(&mut Caps::default(), &[], "");
        assert!(titles(&panels).is_empty(), "{:?}", titles(&panels));
    }

    /// A text provider: Context (token accounting), Effort + Temperature (Tunable), then
    /// the read-only System tab LAST — the order the recorded indices depend on.
    #[test]
    fn a_text_provider_gets_context_effort_temperature_and_system() {
        let mut p = Caps {
            usage: true,
            tunable: true,
            effort: Some(Effort::High),
            temperature: Some(0.7),
            ..Caps::default()
        };
        let history = [Message::system("You are terse.")];
        let (panels, _) = tabs(&mut p, &history, "");
        assert_eq!(
            titles(&panels),
            ["Context", "Effort", "Temperature", "System"]
        );

        // Each tab opens on the provider's CURRENT value: an untouched surface is a no-op.
        assert_eq!(panels[0].items()[panels[0].cursor()], "128k (current)");
        assert_eq!(panels[1].items()[panels[1].cursor()], "high (current)");
        assert_eq!(panels[2].kind(), PanelKind::Slider);
        assert_eq!(panels[2].as_slider().and_then(|s| s.value), Some(0.7));
        assert_eq!(panels[3].kind(), PanelKind::View);
        assert!(panels[3].wrap());
    }

    /// The Context tab exists only for a provider whose usage the meter can settle
    /// against — offering a window knob to a dialect that reports nothing would be a
    /// setting with no observable effect (T-10).
    #[test]
    fn the_context_tab_needs_token_accounting() {
        let (panels, ex) = tabs(&mut Caps::default(), &[], "");
        assert!(!titles(&panels).contains(&"Context"));
        assert!(ex.ctx.is_none());

        let (panels, ex) = tabs(
            &mut Caps {
                usage: true,
                ..Caps::default()
            },
            &[],
            "",
        );
        assert_eq!(titles(&panels), ["Context"]);
        assert_eq!(ex.ctx, Some(0), "the recorded index is the commit contract");
    }

    /// Anthropic caps temperature at 1.0; everyone else at 2.0 (chat/run.go:566-569).
    #[test]
    fn the_temperature_ceiling_follows_the_dialect() {
        let mut openai = Caps {
            tunable: true,
            ..Caps::default()
        };
        let (panels, _) = tabs(&mut openai, &[], "");
        assert_eq!(panels[1].as_slider().map_or(0.0, |s| s.max), 2.0);
        assert_eq!(panels[1].as_slider().map_or(0.0, |s| s.min), 0.0);
        assert_eq!(panels[1].as_slider().map_or(0.0, |s| s.step), 0.1);

        let mut anthropic = Caps {
            kind: Some(ProviderKind::Anthropic),
            tunable: true,
            ..Caps::default()
        };
        let (panels, _) = tabs(&mut anthropic, &[], "");
        assert_eq!(panels[1].as_slider().map_or(0.0, |s| s.max), 1.0);
    }

    /// A dedicated image provider: the generation knobs it actually has, the JSON-edits
    /// switch, and NO System tab — its request carries the last user text alone, so a
    /// system prompt would be a lie about what is sent.
    #[test]
    fn an_image_provider_gets_its_knobs_and_no_system_tab() {
        let mut p = Caps {
            image_gen: Some((
                ImageGenOptions {
                    aspect_ratios: vec!["1:1", "3:2"],
                    image_sizes: vec!["1K", "2K"],
                    negative_prompt: true,
                },
                ImageGenParams {
                    aspect_ratio: Some("3:2".to_owned()),
                    image_size: None,
                    negative_prompt: Some("blurry".to_owned()),
                },
            )),
            json_edits: Some(true),
            ..Caps::default()
        };
        let history = [Message::system("You are terse.")];
        let (panels, ex) = tabs(&mut p, &history, "");
        assert_eq!(
            titles(&panels),
            ["Aspect", "Size", "Negative", "JSON edits"]
        );
        assert!(
            !titles(&panels).contains(&"System"),
            "an image provider takes no system prompt"
        );
        assert_eq!(ex.aspect, Some(0));
        assert_eq!(ex.size, Some(1));
        assert_eq!(ex.negative, Some(2));
        assert_eq!(ex.json_edits, Some(3));
        assert_eq!(panels[0].items()[panels[0].cursor()], "3:2 (current)");
        assert_eq!(panels[1].items()[panels[1].cursor()], "default (current)");
        assert_eq!(
            panels[2].as_input().map_or("", |i| i.text.as_str()),
            "blurry"
        );
        assert!(panels[3].on());

        // A dialect offering only sizes (openai images) shows only that tab.
        let mut only_sizes = Caps {
            image_gen: Some((
                ImageGenOptions {
                    image_sizes: vec!["1024x1024"],
                    ..ImageGenOptions::default()
                },
                ImageGenParams::default(),
            )),
            ..Caps::default()
        };
        let (panels, ex) = tabs(&mut only_sizes, &[], "");
        assert_eq!(titles(&panels), ["Size"]);
        assert!(ex.aspect.is_none() && ex.negative.is_none());
    }

    /// The image-output switch is its own capability, and it opens on the current state.
    #[test]
    fn the_image_switch_opens_on_the_current_state() {
        let mut p = Caps {
            image: Some(true),
            ..Caps::default()
        };
        let (panels, ex) = tabs(&mut p, &[], "");
        assert_eq!(titles(&panels), ["Image"]);
        assert_eq!(ex.image, Some(0));
        assert_eq!(panels[0].kind(), PanelKind::Switch);
        assert!(panels[0].on());
        assert_eq!(
            panels[0].prompt,
            "Request image generation (modalities / built-in tool)"
        );
    }

    /// The System tab shows the prompt AS SENT: agent mode's overlay is folded in through
    /// the same `compose_send_history` the wire uses (the tab-cannot-drift law).
    #[test]
    fn the_system_tab_carries_the_agent_overlay() {
        let history = [Message::system("Base prompt.")];
        let (panels, _) = tabs(&mut Caps::default(), &history, "Project rules.");
        assert_eq!(titles(&panels), ["System"]);
        assert_eq!(
            panels[0].lines(),
            ["Base prompt.", "", "Project rules."],
            "the overlay must be welded on exactly as the wire carries it"
        );

        // No system prompt in effect: no tab (the surface stays as it was).
        let (panels, _) = tabs(&mut Caps::default(), &[], "");
        assert!(panels.is_empty());
    }
}
