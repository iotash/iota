//! The agent's candidate set, as the `/model` picker sees it (brain page `config-three-layers`).
//!
//! `agents.<name>.models` is a LIST of references, and the three forms cost different things to
//! turn into rows:
//!
//! - [`ModelRef::Entry`](crate::config::ModelRef::Entry) — a `models:` entry by name: its
//!   `provider:id` is already written down;
//! - [`ModelRef::Inline`](crate::config::ModelRef::Inline) — `provider:id`, written down just as
//!   directly;
//! - [`ModelRef::All`](crate::config::ModelRef::All) — `provider:*`, which is a QUESTION for that
//!   endpoint (`list_models`).
//!
//! So a catalog is the written-down half plus one lister per wildcard provider, and expanding it is
//! one concurrent round of listings — concurrent because a candidate set naming three relays should
//! cost one round trip's wait, not three, and because a source that never answers must not decide
//! how long the other two take (they share ONE cancel scope, so ESC drops all of them at once).
//!
//! **Partial failure is the normal case, not the error case.** A relay that does not implement
//! `/models` is the reason the picker has an input row at all: its wildcard contributes zero rows
//! and one note, and every other source is listed exactly as if it had never been asked. Nothing a
//! source does can take the picker away from the user.

use std::collections::BTreeMap;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::app::env::Env;
use crate::config::{Config, ModelRef, Resolved};
use crate::provider::{HttpTransport, Provider, ProviderKind, ProviderParams, new_provider};
use crate::ui::facade::Ui;

/// One row of the picker: a model id and the endpoint that serves it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Candidate {
    /// The `providers:` entry (or built-in type) name; `""` = "wherever this session already is".
    pub(crate) provider: String,
    /// The wire model id, verbatim.
    pub(crate) id: String,
}

impl Candidate {
    /// A model on the session's own endpoint.
    pub(crate) fn here(id: impl Into<String>) -> Self {
        Self {
            provider: String::new(),
            id: id.into(),
        }
    }

    /// How the row reads. A candidate on the endpoint the session is ALREADY talking to is written
    /// bare — that provider is the implicit context of the whole chat, and prefixing every row with
    /// it would be noise on the common single-provider list. Anything else carries the
    /// `provider:id` of `-M`, which is both the answer to "which one is this?" and the marker that
    /// it is not this session's to switch to (see [`ModelCatalog::pick`]).
    pub(crate) fn label(&self, session: &str) -> String {
        if self.provider.is_empty() || self.provider == session {
            return self.id.clone();
        }
        format!("{}:{}", self.provider, self.id)
    }
}

/// What committing a row (or the typed text) means for THIS session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Pick {
    /// A model on the session's own endpoint: switch to it.
    Here(String),
    /// A model on another endpoint. A session cannot move between providers — the history it
    /// replays is the dialect's own (signed thinking blocks, response items), the bundle records
    /// the type it was created under, and the dispatcher was assembled for it — so this is
    /// reported, never half-applied (the same law as a `defer_mode` a switch cannot hand over).
    Elsewhere(String, String),
}

/// One wildcard source: the endpoint to ask, or why it cannot be asked at all.
enum Source {
    /// A constructed provider, used for `list_models` alone.
    Live(Arc<dyn Provider>),
    /// The endpoint could not be built (an unknown `type:`); the message is the note.
    Broken(String),
    /// The session's own provider: the caller passes the LIVE one in, so the picker asks the
    /// instance the chat is actually running on rather than a second copy of it.
    Session,
}

/// The agent's candidate set plus the listers its wildcards need.
///
/// A chat holds one for its whole life: the config cannot change under a running session, so what
/// CAN be offered is fixed at startup and only the listings are asked for again.
#[derive(Default)]
pub struct ModelCatalog {
    /// `agents.<name>.models`, in declaration order — the order the picker lists them in.
    refs: Vec<ModelRef>,
    /// `models:` entries by name, `provider:` anchored.
    entries: BTreeMap<String, crate::config::ModelConfig>,
    /// The endpoint this session talks to; every row on it is one a pick can actually take.
    session: String,
    /// The `agents:` entry the chat runs, so the picker can spell the command that would start a
    /// run on another provider (`iota run <agent> -M provider:id`).
    agent: String,
    /// A lister per provider name a wildcard names.
    sources: BTreeMap<String, Source>,
    /// Every provider name the config knows, so a typed `provider:id` is read the way `-M` reads
    /// it: an endpoint reference only when the name is one iota knows, a raw model id otherwise
    /// (a relay's own ids contain colons).
    known: Vec<String>,
}

impl ModelCatalog {
    /// The catalog a run resolved to. Constructing a wildcard's endpoint is cheap (no I/O) and its
    /// failure is recorded rather than raised: a broken entry in the candidate set must not stop
    /// the picker from offering the rest.
    pub fn new(cfg: &Config, resolved: &Resolved, env: &Env, http: &HttpTransport) -> Self {
        let mut sources = BTreeMap::new();
        for name in resolved
            .agent
            .models
            .iter()
            .filter(|r| r.is_wildcard())
            .filter_map(ModelRef::provider)
        {
            if sources.contains_key(name) {
                continue;
            }
            let source = if name == resolved.provider_name {
                Source::Session
            } else {
                build_source(cfg, name, env, http)
            };
            sources.insert(name.to_owned(), source);
        }
        Self {
            refs: resolved.agent.models.clone(),
            entries: cfg
                .models
                .iter()
                .map(|(name, m)| {
                    let mut m = m.clone();
                    m.anchor_provider(name);
                    (name.clone(), m)
                })
                .collect(),
            session: resolved.provider_name.clone(),
            agent: if resolved.agent_name.is_empty() {
                resolved.name.clone()
            } else {
                resolved.agent_name.clone()
            },
            sources,
            known: cfg
                .providers
                .keys()
                .cloned()
                .chain(ProviderKind::ALL.iter().map(|k| k.as_str().to_owned()))
                .collect(),
        }
    }

    /// The endpoint the session talks to (`""` for a catalog that names none).
    pub(crate) fn session_provider(&self) -> &str {
        &self.session
    }

    /// The `agents:` entry the chat runs (`""` when it names none).
    pub(crate) fn agent(&self) -> &str {
        &self.agent
    }

    /// What a committed candidate means for this session (see [`Pick`]).
    pub(crate) fn pick(&self, c: &Candidate) -> Pick {
        if c.provider.is_empty() || c.provider == self.session {
            return Pick::Here(c.id.clone());
        }
        Pick::Elsewhere(c.provider.clone(), c.id.clone())
    }

    /// The candidate a typed string names, read exactly as `-M` reads its argument: `provider:id`
    /// is an endpoint reference only when `provider` is a name iota knows, and everything else —
    /// including a string with a colon in it — is a raw model id on the session's own endpoint.
    pub(crate) fn parse_typed(&self, text: &str) -> Candidate {
        let text = text.trim();
        if let Some((provider, id)) = text.split_once(':')
            && !id.is_empty()
            && id != "*"
            && self.known.iter().any(|k| k == provider)
        {
            return Candidate {
                provider: provider.to_owned(),
                id: id.to_owned(),
            };
        }
        Candidate::here(text)
    }

    /// The catalog expanded into rows: the written-down references in declaration order, then each
    /// wildcard's listing in the order its endpoint returned it, deduplicated by `provider:id`
    /// (the same model may be reachable through two entries, and one row per model is the point).
    pub(crate) async fn expand(
        &self,
        ui: &Arc<dyn Ui>,
        live: &dyn Provider,
        cancel: &CancellationToken,
    ) -> Expansion {
        let (listed, cancelled) = self.fetch(ui, live, cancel).await;
        if cancelled {
            return Expansion {
                cancelled: true,
                ..Expansion::default()
            };
        }
        let mut out = Expansion::default();
        // An agent that declares no candidate set is read as one implicit `<session>:*` — the
        // endpoint the chat is already on, asked exactly the way a wildcard asks — so the picker
        // has ONE path and a run without candidate sets keeps the list it always had.
        if self.refs.is_empty() {
            match listed.get(self.session.as_str()) {
                Some(Ok(ids)) => {
                    out.rows = ids
                        .iter()
                        .filter(|id| !id.is_empty())
                        .map(|id| Candidate {
                            provider: self.session.clone(),
                            id: id.clone(),
                        })
                        .collect();
                }
                Some(Err(e)) => out.notes.push(note(&self.session, e)),
                None => {}
            }
            return out;
        }
        let mut seen: Vec<(String, String)> = Vec::new();
        let push = |out: &mut Expansion, seen: &mut Vec<(String, String)>, c: Candidate| {
            if c.id.is_empty() {
                return;
            }
            let key = (c.provider.clone(), c.id.clone());
            if seen.contains(&key) {
                return;
            }
            seen.push(key);
            out.rows.push(c);
        };
        for r in &self.refs {
            match r {
                ModelRef::Entry(name) => {
                    if let Some(m) = self.entries.get(name) {
                        push(
                            &mut out,
                            &mut seen,
                            Candidate {
                                provider: m.provider.clone(),
                                id: m.id.clone(),
                            },
                        );
                    }
                }
                ModelRef::Inline { provider, id } => push(
                    &mut out,
                    &mut seen,
                    Candidate {
                        provider: provider.clone(),
                        id: id.clone(),
                    },
                ),
                ModelRef::All { provider } => match listed.get(provider) {
                    Some(Ok(ids)) => {
                        for id in ids {
                            push(
                                &mut out,
                                &mut seen,
                                Candidate {
                                    provider: provider.clone(),
                                    id: id.clone(),
                                },
                            );
                        }
                    }
                    Some(Err(e)) => {
                        let failure = note(provider, e);
                        if !out.notes.contains(&failure) {
                            out.notes.push(failure);
                        }
                    }
                    None => {}
                },
            }
        }
        out
    }

    /// Every wildcard's listing, asked for CONCURRENTLY under one busy row and one cancel scope.
    ///
    /// The cancel idiom is the picker's own (`commands::model`): the scope's child token is what
    /// the listings run under, so ESC aborts all of them at once instead of leaving the user
    /// hostage to whichever endpoint times out last — and the cancellation state is READ BEFORE
    /// this code fires the token itself, or the check would be unconditionally true.
    async fn fetch(
        &self,
        ui: &Arc<dyn Ui>,
        live: &dyn Provider,
        parent: &CancellationToken,
    ) -> (BTreeMap<String, Result<Vec<String>, String>>, bool) {
        let mut out: BTreeMap<String, Result<Vec<String>, String>> = BTreeMap::new();
        let mut ask: Vec<(&str, &dyn Provider)> = Vec::new();
        if self.refs.is_empty() {
            ask.push((self.session.as_str(), live));
        }
        for (name, source) in &self.sources {
            match source {
                Source::Session => ask.push((name, live)),
                Source::Live(p) => ask.push((name, &**p)),
                Source::Broken(e) => {
                    out.insert(name.clone(), Err(e.clone()));
                }
            }
        }
        if ask.is_empty() {
            return (out, false);
        }
        let child = parent.child_token();
        let scope = ui.push_cancel_scope(child.clone());
        let busy = ui.busy(&busy_label(ask.len()));
        let answers = futures::future::join_all(ask.into_iter().map(|(name, p)| {
            let child = &child;
            async move { (name, p.list_models(child).await) }
        }))
        .await;
        busy.stop();
        scope.pop();
        // READ the cancellation state before firing our own token (see the doc comment).
        let cancelled = child.is_cancelled() && !parent.is_cancelled();
        child.cancel();
        for (name, res) in answers {
            out.insert(name.to_owned(), res.map_err(|e| e.to_string()));
        }
        (out, cancelled)
    }
}

/// What an expansion produced.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Expansion {
    /// The rows, in declaration order, deduplicated.
    pub(crate) rows: Vec<Candidate>,
    /// One note per source that could not answer — `provider: what went wrong`.
    pub(crate) notes: Vec<String>,
    /// The user pressed ESC while the listings were in flight: abandon the command, quietly.
    pub(crate) cancelled: bool,
}

/// The busy row says how many endpoints are being asked, because the wait is the slowest of them
/// and a row that named only one would be lying about what is holding it up.
fn busy_label(n: usize) -> String {
    if n <= 1 {
        return "Fetching available models".to_owned();
    }
    format!("Fetching available models from {n} providers")
}

/// One wildcard endpoint, built for listing alone: no temperature, no model, the run's own
/// transport (so `/debug` records these calls too).
fn build_source(cfg: &Config, name: &str, env: &Env, http: &HttpTransport) -> Source {
    let (raw_type, provider_cfg) = cfg.get(name);
    let kind: ProviderKind = match raw_type.parse() {
        Ok(kind) => kind,
        Err(e) => return Source::Broken(e.to_string()),
    };
    let api_key = crate::cmd::resolve::resolve_key_from_env_or_config(
        crate::provider::provider_env_key(&raw_type),
        &provider_cfg,
        env,
    );
    match new_provider(
        kind,
        ProviderParams {
            api_key: &api_key,
            base_url: &provider_cfg.url,
            model: "",
            temperature: None,
        },
        Some(http.clone()),
    ) {
        Ok(p) => Source::Live(Arc::from(p)),
        Err(e) => Source::Broken(e.to_string()),
    }
}

/// One source's note for the picker's prompt row: the provider that could not answer and what it
/// said, clipped to the error's FIRST line — the prompt is a single row, and a wire error's
/// appendix would push everything else off it.
fn note(provider: &str, e: &str) -> String {
    let first = e.lines().next().unwrap_or(e).trim_end();
    if provider.is_empty() {
        return first.to_owned();
    }
    format!("{provider}: {first}")
}

#[cfg(test)]
mod tests {
    use super::{Candidate, ModelCatalog, Pick, Source, busy_label};
    use crate::config::{ModelConfig, ModelRef};
    use crate::testing::{FakeProvider, ScriptedUi};
    use crate::ui::facade::Ui;
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    /// A provider (model `""`) answering `list_models` with `models`.
    fn lister(models: &[&str]) -> FakeProvider {
        FakeProvider::new().with_model("").with_models(models)
    }

    /// A lister whose listing fails with `msg`.
    fn failing(msg: &str) -> FakeProvider {
        FakeProvider::new().with_model("").with_models_failing(msg)
    }

    /// A lister answering after `ms` — the delay is what makes concurrency observable (two 120 ms
    /// sources finish in ~120 ms concurrently, ~240 ms one after the other).
    fn slow(models: &[&str], ms: u64) -> FakeProvider {
        lister(models).with_models_after(Duration::from_millis(ms))
    }

    /// A catalog with hand-built sources: `new` needs a whole `Config`, and what these tests are
    /// about is the expansion, not the config plumbing (that is `tests/cmd/config.rs`'s).
    fn catalog(refs: Vec<ModelRef>, sources: Vec<(&str, Source)>) -> ModelCatalog {
        let mut entries = BTreeMap::new();
        entries.insert(
            "sonnet".to_owned(),
            ModelConfig {
                provider: "anthropic".to_owned(),
                id: "claude-sonnet-4".to_owned(),
                ..ModelConfig::default()
            },
        );
        ModelCatalog {
            refs,
            entries,
            session: "anthropic".to_owned(),
            agent: "default".to_owned(),
            sources: sources
                .into_iter()
                .map(|(n, s)| (n.to_owned(), s))
                .collect(),
            known: vec!["anthropic".to_owned(), "relay".to_owned()],
        }
    }

    fn ui() -> Arc<dyn Ui> {
        ScriptedUi::new(Vec::new()) as Arc<dyn Ui>
    }

    fn wildcard(p: &str) -> ModelRef {
        ModelRef::All {
            provider: p.to_owned(),
        }
    }

    /// The three reference forms, in declaration order: an entry and an inline reference are rows
    /// on the spot, a wildcard is whatever its endpoint listed.
    #[tokio::test]
    async fn expands_every_reference_form() {
        let cat = catalog(
            vec![
                ModelRef::Entry("sonnet".to_owned()),
                ModelRef::Inline {
                    provider: "relay".to_owned(),
                    id: "vendor/x".to_owned(),
                },
                wildcard("relay"),
            ],
            vec![("relay", Source::Live(Arc::new(lister(&["a", "b"]))))],
        );
        let live = lister(&["never asked"]);
        let got = cat.expand(&ui(), &live, &CancellationToken::new()).await;
        assert_eq!(
            got.rows,
            vec![
                Candidate {
                    provider: "anthropic".to_owned(),
                    id: "claude-sonnet-4".to_owned()
                },
                Candidate {
                    provider: "relay".to_owned(),
                    id: "vendor/x".to_owned()
                },
                Candidate {
                    provider: "relay".to_owned(),
                    id: "a".to_owned()
                },
                Candidate {
                    provider: "relay".to_owned(),
                    id: "b".to_owned()
                },
            ]
        );
        assert!(got.notes.is_empty());
        // The label law: the session's own endpoint is implicit, anything else is `provider:id`.
        assert_eq!(got.rows[0].label("anthropic"), "claude-sonnet-4");
        assert_eq!(got.rows[1].label("anthropic"), "relay:vendor/x");
    }

    /// `provider:*` on the session's OWN provider asks the live instance — the one the chat is
    /// running on, with its key and its base URL — not a second copy built from the config.
    #[tokio::test]
    async fn the_session_provider_is_asked_live() {
        let cat = catalog(
            vec![wildcard("anthropic")],
            vec![("anthropic", Source::Session)],
        );
        let live = lister(&["claude-a", "claude-b"]);
        let got = cat.expand(&ui(), &live, &CancellationToken::new()).await;
        assert_eq!(
            got.rows
                .iter()
                .map(|c| c.label("anthropic"))
                .collect::<Vec<_>>(),
            ["claude-a", "claude-b"]
        );
    }

    /// The same `provider:id` reachable twice is ONE row, and the first mention decides where it
    /// sits — an entry the agent listed first stays first even when a wildcard lists it again.
    #[tokio::test]
    async fn duplicates_collapse_to_the_first_mention() {
        let cat = catalog(
            vec![
                ModelRef::Inline {
                    provider: "relay".to_owned(),
                    id: "b".to_owned(),
                },
                wildcard("relay"),
            ],
            vec![("relay", Source::Live(Arc::new(lister(&["a", "b"]))))],
        );
        let live = lister(&[]);
        let got = cat.expand(&ui(), &live, &CancellationToken::new()).await;
        assert_eq!(
            got.rows.iter().map(|c| c.id.clone()).collect::<Vec<_>>(),
            ["b", "a"]
        );
    }

    /// THE law of a mixed list: one source failing costs exactly its own rows. Everything else is
    /// listed as if it had never been asked, and the failure is one note naming the provider.
    #[tokio::test]
    async fn a_failed_source_costs_only_its_own_rows() {
        let cat = catalog(
            vec![wildcard("relay"), wildcard("dead")],
            vec![
                ("relay", Source::Live(Arc::new(lister(&["a"])))),
                (
                    "dead",
                    Source::Live(Arc::new(failing("404 no such endpoint\ntrace: …"))),
                ),
            ],
        );
        let live = lister(&[]);
        let got = cat.expand(&ui(), &live, &CancellationToken::new()).await;
        assert_eq!(got.rows.len(), 1, "the healthy source still lists");
        assert_eq!(got.rows[0].id, "a");
        // One line, naming the provider and what went wrong — the prompt row is a single line.
        assert_eq!(got.notes, ["dead: 404 no such endpoint"]);
    }

    /// An endpoint that could not even be CONSTRUCTED (an unknown `type:`) is the same kind of
    /// partial failure: a note, never a raised error.
    #[tokio::test]
    async fn an_unbuildable_source_is_a_note() {
        let cat = catalog(
            vec![wildcard("relay"), wildcard("weird")],
            vec![
                ("relay", Source::Live(Arc::new(lister(&["a"])))),
                ("weird", Source::Broken("unknown provider type".to_owned())),
            ],
        );
        let live = lister(&[]);
        let got = cat.expand(&ui(), &live, &CancellationToken::new()).await;
        assert_eq!(got.rows.len(), 1);
        assert_eq!(got.notes, ["weird: unknown provider type"]);
    }

    /// Three wildcards are asked at once: the wall clock is the SLOWEST source, not their sum.
    #[tokio::test(start_paused = false)]
    async fn wildcards_are_asked_concurrently() {
        let cat = catalog(
            vec![wildcard("a"), wildcard("b"), wildcard("c")],
            vec![
                ("a", Source::Live(Arc::new(slow(&["a1"], 150)))),
                ("b", Source::Live(Arc::new(slow(&["b1"], 150)))),
                ("c", Source::Live(Arc::new(slow(&["c1"], 150)))),
            ],
        );
        let live = lister(&[]);
        let t0 = std::time::Instant::now();
        let got = cat.expand(&ui(), &live, &CancellationToken::new()).await;
        let elapsed = t0.elapsed();
        assert_eq!(got.rows.len(), 3);
        assert!(
            elapsed < Duration::from_millis(350),
            "three 150ms listings took {elapsed:?} — serial would be ≥450ms"
        );
    }

    /// ESC during the listings abandons the whole command: the scope's token is cancelled from
    /// outside, and the expansion says so instead of returning half a list.
    #[tokio::test(start_paused = true)]
    async fn esc_during_the_fetch_abandons_the_expansion() {
        let cat = catalog(
            vec![wildcard("a")],
            vec![("a", Source::Live(Arc::new(slow(&["a1"], 5_000))))],
        );
        let live = lister(&[]);
        let ui = ScriptedUi::new(Vec::new());
        let cancel = CancellationToken::new();
        let handle = Arc::clone(&ui) as Arc<dyn Ui>;
        let fetch = cat.expand(&handle, &live, &cancel);
        let fire = async {
            tokio::time::sleep(Duration::from_millis(30)).await;
            ui.fire_cancel_scopes();
        };
        let (got, ()) = tokio::join!(fetch, fire);
        assert!(got.cancelled, "a user cancel abandons the picker");
        assert!(got.rows.is_empty());
    }

    /// A shutdown is NOT a user cancellation: the parent token is the one that fired, and the
    /// command reports what it has rather than pretending the user walked away.
    #[tokio::test]
    async fn an_app_shutdown_is_not_a_user_cancel() {
        let cat = catalog(
            vec![wildcard("a")],
            vec![("a", Source::Live(Arc::new(lister(&["a1"]))))],
        );
        let live = lister(&[]);
        let cancel = CancellationToken::new();
        cancel.cancel();
        let got = cat.expand(&ui(), &live, &cancel).await;
        assert!(!got.cancelled);
    }

    /// No candidate set at all: the live provider IS the list, and its failure is a note — the
    /// picker never falls back to something else, because the input row is always there.
    #[tokio::test]
    async fn an_empty_candidate_set_lists_the_live_provider() {
        let cat = catalog(Vec::new(), Vec::new());
        let live = lister(&["a-model", "b-model"]);
        let got = cat.expand(&ui(), &live, &CancellationToken::new()).await;
        assert_eq!(
            got.rows
                .iter()
                .map(|c| c.label("anthropic"))
                .collect::<Vec<_>>(),
            ["a-model", "b-model"]
        );
        assert!(got.notes.is_empty());

        let dead = failing("no such endpoint");
        let got = cat.expand(&ui(), &dead, &CancellationToken::new()).await;
        assert!(got.rows.is_empty());
        assert_eq!(got.notes, ["anthropic: no such endpoint"]);
    }

    /// A typed string is read exactly as `-M` reads its argument.
    #[test]
    fn typed_text_follows_the_model_flag_rule() {
        let cat = catalog(Vec::new(), Vec::new());
        // A known provider name before the colon IS an endpoint reference.
        assert_eq!(
            cat.parse_typed("relay:vendor/x"),
            Candidate {
                provider: "relay".to_owned(),
                id: "vendor/x".to_owned()
            }
        );
        // An unknown one is not: the whole string is the model id (relays ship ids with colons).
        assert_eq!(cat.parse_typed("vendor:x"), Candidate::here("vendor:x"));
        assert_eq!(cat.parse_typed("  gpt-4o  "), Candidate::here("gpt-4o"));
    }

    /// A pick on the session's endpoint switches; one on another endpoint is reported.
    #[test]
    fn a_pick_knows_whether_this_session_can_take_it() {
        let cat = catalog(Vec::new(), Vec::new());
        assert_eq!(
            cat.pick(&Candidate {
                provider: "anthropic".to_owned(),
                id: "claude-x".to_owned()
            }),
            Pick::Here("claude-x".to_owned())
        );
        assert_eq!(
            cat.pick(&Candidate::here("claude-x")),
            Pick::Here("claude-x".to_owned())
        );
        assert_eq!(
            cat.pick(&Candidate {
                provider: "relay".to_owned(),
                id: "vendor/x".to_owned()
            }),
            Pick::Elsewhere("relay".to_owned(), "vendor/x".to_owned())
        );
    }

    #[test]
    fn the_busy_row_says_how_many_endpoints_are_asked() {
        assert_eq!(busy_label(1), "Fetching available models");
        assert_eq!(busy_label(3), "Fetching available models from 3 providers");
    }
}
