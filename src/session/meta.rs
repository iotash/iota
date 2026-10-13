//! `meta.json` — the session's metadata record (chat/session.go:39-65, :481-489, :752-760).
//!
//! Field order IS Go's struct order, so `serde_json::to_string_pretty` reproduces
//! `json.MarshalIndent(m, "", "  ")`; the file is written WITHOUT a trailing newline, and through a
//! `meta.json.tmp` + `rename` pair rather than Go's in-place `os.WriteFile` (DIVERGENCES D-45).

use std::path::Path;

use crate::provider::error::InvalidEffort;
use crate::provider::{Effort, ImageGenParams, Provider};
use crate::session::error::SessionError;
use crate::session::params::{LayeredParams, ParamSources};

/// The image settings a provider runs under, as a meta records them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImageSettings {
    /// Image output switched on (`ImageTunable`).
    pub output: bool,
    /// The image-generation knobs (`ImageGenTunable`).
    pub generation: ImageGenParams,
    /// Edits on the JSON wire format (`ImageEditJsonTunable`).
    pub json_edits: bool,
}

impl ImageSettings {
    /// What `provider` runs under: each read through its capability, off/empty without it.
    pub fn of(provider: &mut dyn Provider) -> Self {
        Self {
            output: provider
                .as_image_tunable()
                .is_some_and(|t| t.image_output()),
            generation: provider
                .as_image_gen_tunable()
                .map(|t| t.image_gen_params().clone())
                .unwrap_or_default(),
            json_edits: provider
                .as_image_edit_json_tunable()
                .is_some_and(|t| t.json_edits()),
        }
    }
}

/// The schema version every bundle is written with, and the only one read. `2` (2026-10-11): `provider` is the
/// provider NAME the session runs on. A `1` bundle — Go's, and every Rust build's before it — recorded the
/// provider TYPE there; it is refused ([`SessionError::OldFormat`]) and converted by [`CONVERT_SCRIPT`].
pub const SESSION_SCHEMA_VERSION: i64 = 2;
/// The one-time conversion of v1 bundles, as the refusal names it: a path in the iota source tree.
pub const CONVERT_SCRIPT: &str = "scripts/session-v1-to-v2.py";
/// The metadata file inside a bundle.
pub const META_FILE: &str = "meta.json";
/// The fixed temp name the atomic rewrite renames from (D-45); invisible to Go's locator and lister.
pub const META_TMP_FILE: &str = "meta.json.tmp";

/// `skip_serializing_if` for Go's `omitempty` on an `int`. Serde hands these predicates a reference, so the
/// by-value lint does not apply.
#[allow(clippy::trivially_copy_pass_by_ref)]
pub(crate) fn is_zero_i64(v: &i64) -> bool {
    *v == 0
}

/// `skip_serializing_if` for Go's `omitempty` on a `uint64`.
#[allow(clippy::trivially_copy_pass_by_ref)]
pub(crate) fn is_zero_u64(v: &u64) -> bool {
    *v == 0
}

/// `skip_serializing_if` for Go's `omitempty` on a `bool`.
#[allow(clippy::trivially_copy_pass_by_ref)]
pub(crate) fn is_false(v: &bool) -> bool {
    !*v
}

/// `meta.json` (chat/session.go:39-65). `v`, `id`, `created_at`, `updated_at`, `provider`, `model` and
/// `message_count` carry NO `omitempty` and are always emitted; everything else follows Go's exact
/// `omitempty` matrix.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)] // legacy bundles may omit anything; `flatten` also demands every field be defaultable
pub struct SessionMeta {
    /// Schema version (`v`).
    #[serde(rename = "v")]
    pub version: i64,
    /// The session id (also the bundle directory name).
    pub id: String,
    /// RFC3339, second precision, local offset.
    pub created_at: String,
    /// RFC3339; restamped by every [`SessionMeta::write`].
    pub updated_at: String,
    /// The provider the session runs on — the `providers:` entry name, or a built-in type name for an
    /// endpoint no entry declares. It names an endpoint of the CONFIG: its type, URL and key are read from there
    /// when the session is resumed, never from here; and it follows the session when the session moves (`-M` on
    /// a resume, `/model`). ALWAYS emitted, even empty.
    pub provider: String,
    /// The model in use on that provider. ALWAYS emitted, even empty.
    pub model: String,
    /// Recorded temperature; `None` omits the key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Recorded `top_p`; `None` omits the key. Newer than the Go format: the knob was config-only until the
    /// layering made it one of the four a session carries (brain page `model-param-layering`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    /// Recorded `max_tokens`; `None` omits the key (the dialect's default). No Go twin: Go sent a fixed
    /// 4096 (DIVERGENCES X-68).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Recorded context window; 0 omits the key.
    #[serde(skip_serializing_if = "is_zero_i64")]
    pub context_window: i64,
    /// Recorded reasoning effort; empty omits the key.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub effort: String,
    /// Whether image output was switched on; false omits the key.
    #[serde(skip_serializing_if = "is_false")]
    pub image: bool,
    /// Image-generation aspect ratio (imagen/images sessions); empty omits the key.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub aspect_ratio: String,
    /// Image-generation size; empty omits the key.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub image_size: String,
    /// Image-generation negative prompt; empty omits the key.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub negative_prompt: String,
    /// Whether edits use the JSON wire format; false omits the key.
    #[serde(skip_serializing_if = "is_false")]
    pub json_edits: bool,
    /// Where the session was started (the project root in agent mode); empty omits the key. Old bundles
    /// simply lack it.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub cwd: String,
    /// The generated session title; empty omits the key.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub title: String,
    /// The `agents:` entry the session ran under; empty omits the key. Bundles written before the
    /// three-layer config — and runs whose positional argument named a model or a provider rather than an
    /// agent — simply lack it. It is a RECORD, not a replay instruction: a resume reassembles from the
    /// CURRENT config (decision of 2026-09-09), and an agent that has been deleted since falls back to the
    /// provider and model the meta carries (see `crate::session::warn_if_session_agent_is_gone`).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub agent: String,
    /// Where each of the four layered parameters got its value: a config declaration, the user's own hand,
    /// or the built-in default. It is what lets a later `/model` switch tell a window this session INHERITED
    /// from a model (drop it when the next model declares none) from one the user typed (keep it).
    ///
    /// `None` is a bundle written before the key existed, and the difference is not cosmetic: a bundle that
    /// HAS the key has said everything, so a parameter it records no value for is one the session
    /// deliberately runs without, while in one that lacks it only a value actually present was ever
    /// recorded. [`SessionMeta::sources`] is the reading; [`ParamSources::LEGACY`] is what absence means.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub param_sources: Option<ParamSources>,
    /// Records written to the log, system messages included, compaction markers excluded. ALWAYS emitted.
    pub message_count: i64,
    /// Keys a future Go build wrote that this build does not model — preserved verbatim across a Rust
    /// rewrite (D-46). Rust NEVER adds a key of its own here (Go would drop it on its next rewrite).
    #[serde(flatten)]
    pub extra: crate::provider::model::JsonObject,
}

impl SessionMeta {
    /// Reads `<dir>/meta.json` (chat/session.go:752-760). A parse failure arrives as
    /// [`SessionError::Io`] with serde's own text, which `resume`/`load` wrap in `cannot read session …`.
    pub fn read(dir: &Path) -> Result<SessionMeta, SessionError> {
        let data = std::fs::read(dir.join(META_FILE))?;
        Ok(serde_json::from_slice(&data)?)
    }

    /// Stamps `updated_at = now_rfc3339()`, serialises with 2-space indentation and NO trailing newline,
    /// writes `<dir>/meta.json.tmp` (0644) and renames it over `meta.json` (chat/session.go:481-489 plus
    /// D-45).
    pub fn write(&mut self, dir: &Path) -> Result<(), SessionError> {
        self.updated_at = now_rfc3339();
        let data = serde_json::to_string_pretty(self)?;
        let tmp = dir.join(META_TMP_FILE);
        write_0644(&tmp, data.as_bytes())?;
        std::fs::rename(&tmp, dir.join(META_FILE))?;
        Ok(())
    }
}

/// `time.Now().Format(time.RFC3339)`: second precision, local numeric offset, no fractional seconds
/// (`+00:00` where Go's UTC prints `Z` — D-51b).
pub fn now_rfc3339() -> String {
    jiff::Zoned::now()
        .strftime("%Y-%m-%dT%H:%M:%S%:z")
        .to_string()
}

/// `time.Parse(time.RFC3339, s)`: accepts `Z` and `±hh:mm`. `None` on failure — Go's zero time, which
/// sorts last in the listing views.
pub(crate) fn parse_rfc3339(s: &str) -> Option<jiff::Timestamp> {
    s.parse().ok()
}

/// `os.WriteFile(path, data, 0o644)`: create-or-truncate with mode 0644 (umask applied, like Go). Shared
/// with the attachment store, which writes with the same mode.
#[cfg(unix)]
pub(crate) fn write_0644(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o644)
        .open(path)?;
    f.write_all(data)
}

/// Non-unix hosts have no mode bits to set (phase-1 DIVERGENCES I-07).
#[cfg(not(unix))]
pub(crate) fn write_0644(path: &Path, data: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, data)
}

impl SessionMeta {
    /// Whether this bundle was recorded on `on`, the provider name a run is on.
    pub fn recorded_on(&self, on: &str) -> bool {
        self.provider == on
    }

    /// Stamps the five layered parameters the session runs under, and the source of each — `window` is
    /// `None` for a provider with no token accounting, which has no window to record.
    pub fn stamp(&mut self, window: Option<u64>, params: &LayeredParams) {
        if let Some(window) = window {
            self.set_context_window(window);
        }
        params.effort.value.clone_into(&mut self.effort);
        self.temperature = params.temperature.value;
        self.top_p = params.top_p.value;
        self.max_tokens = params.max_tokens.value;
        self.param_sources = Some(params.sources());
    }

    /// Records the image settings a provider runs under (all off/empty for one without the capability).
    pub fn set_image(&mut self, image: &ImageSettings) {
        self.image = image.output;
        self.json_edits = image.json_edits;
        let opt = |v: &Option<String>| v.clone().unwrap_or_default();
        self.aspect_ratio = opt(&image.generation.aspect_ratio);
        self.image_size = opt(&image.generation.image_size);
        self.negative_prompt = opt(&image.generation.negative_prompt);
    }

    /// Records the context window (the disk field is Go's `int`; a `u64` beyond it saturates).
    pub fn set_context_window(&mut self, window: u64) {
        self.context_window = i64::try_from(window).unwrap_or(i64::MAX);
    }

    /// The recorded effort as a level; `Ok(None)` when none was recorded.
    pub fn effort(&self) -> Result<Option<Effort>, InvalidEffort> {
        Effort::optional(&self.effort)
    }

    /// The recorded image-generation knobs; an empty value is unset.
    pub fn image_gen_params(&self) -> ImageGenParams {
        ImageGenParams::from_raw(&self.aspect_ratio, &self.image_size, &self.negative_prompt)
    }

    /// Where each layered parameter's value came from, with a pre-layering bundle read as
    /// [`ParamSources::LEGACY`].
    pub fn sources(&self) -> ParamSources {
        self.param_sources.unwrap_or(ParamSources::LEGACY)
    }

    /// Whether this bundle's record of the four layered parameters is COMPLETE — i.e. whether it was
    /// written by a build that knows the layering, so a parameter with no value is a deliberate absence
    /// rather than a gap for the config to fill.
    pub fn records_params(&self) -> bool {
        self.param_sources.is_some()
    }

    /// The recorded context window; `None` when the bundle never recorded one.
    ///
    /// This — and its three siblings below — is what "the bundle HAS this parameter" means for the resume
    /// rule: a recorded value is restored with its source, a missing one is evaluated from the config. The
    /// test is the VALUE being there, not the source tag, because the Go-era `omitempty` spellings (`0`,
    /// `""`, absent) are all the same absence and only the value can say which parameters a pre-layering
    /// bundle actually carried.
    pub fn recorded_window(&self) -> Option<u64> {
        u64::try_from(self.context_window).ok().filter(|w| *w > 0)
    }

    /// The recorded effort; `None` when the bundle never recorded one.
    pub fn recorded_effort(&self) -> Option<&str> {
        Some(&self.effort)
            .filter(|e| !e.is_empty())
            .map(String::as_str)
    }

    /// The recorded temperature; `None` when the bundle never recorded one.
    pub fn recorded_temperature(&self) -> Option<f64> {
        self.temperature
    }

    /// The recorded `top_p`; `None` when the bundle never recorded one.
    pub fn recorded_top_p(&self) -> Option<f64> {
        self.top_p
    }

    /// The recorded `max_tokens`; `None` when the bundle never recorded one — or recorded one no config
    /// could have declared ([`Self::invalid_max_tokens`]), which a resume ignores.
    pub fn recorded_max_tokens(&self) -> Option<u32> {
        self.max_tokens.filter(|n| *n > 0)
    }

    /// Whether this bundle's record of `max_tokens` is COMPLETE: its source map carries the key, so an
    /// absent value is the dialect's default deliberately recorded. A map written before the key
    /// existed (the four-parameter one) says nothing about it, and neither does a bundle with no map
    /// (X-68), nor one whose value is invalid — that record is ignored, not read as the default.
    pub fn records_max_tokens(&self) -> bool {
        !self.invalid_max_tokens() && self.param_sources.is_some_and(|s| s.max_tokens.is_some())
    }

    /// The bundle records `max_tokens: 0`: the config refuses it (`want 1 or more`), so only a hand-edited
    /// meta carries it, and a resume — on this endpoint or a move — warns and ignores it (X-68).
    pub fn invalid_max_tokens(&self) -> bool {
        self.max_tokens == Some(0)
    }
}

#[cfg(test)]
mod tests {
    use super::{now_rfc3339, parse_rfc3339};

    /// `now_rfc3339` is second-precision RFC3339 with a numeric offset, and parses back.
    #[test]
    fn now_rfc3339_round_trips() {
        let now = now_rfc3339();
        assert_eq!(
            now.len(),
            25,
            "expected 2006-01-02T15:04:05±07:00, got {now}"
        );
        assert!(!now.contains('.'), "fractional seconds leaked: {now}");
        assert!(parse_rfc3339(&now).is_some(), "unparsable: {now}");
        // Go writes `Z` at offset 0 and jiff writes `+00:00`; both must parse (D-51b).
        assert!(parse_rfc3339("2026-08-31T09:28:23Z").is_some());
        assert!(parse_rfc3339("2026-08-31T17:28:23+08:00").is_some());
        assert_eq!(parse_rfc3339("not a time"), None);
        assert_eq!(parse_rfc3339(""), None);
    }
}
