//! The image picker behind bare `/edit` (chat/editpicker.go): every generated image of the
//! conversation, newest first, labelled by its generation time and prompt, with a hyperlinked path
//! detail and a half-block preview rendered on demand and cached per index.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::imgterm;
use crate::provider::model::{Attachment, Message, Role};
use crate::repl::replay::file_exists;
use crate::ui::facade::PreviewFn;

/// The layout a saved image's name encodes (`20260725-110419-0.png`; chat/images.go:55).
const STAMP_LAYOUT: &str = "%Y%m%d-%H%M%S";
/// How many bytes of the base name the stamp occupies.
const STAMP_LEN: usize = 15;

/// One generated image the user may pick (editpicker.go:20-26).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ImageChoice {
    /// The attachment (filename, mime, bytes).
    pub(crate) att: Attachment,
    /// The user prompt that produced it.
    pub(crate) prompt: String,
    /// When it was generated (parsed from the filename), if known.
    pub(crate) at: Option<jiff::Zoned>,
}

/// Every generated image of `history`, newest first (editpicker.go:29).
///
/// Only ASSISTANT attachments qualify: a user attachment is something the user supplied — or an
/// `/edit` canvas copy — never a generation, so the picker never offers the same picture twice.
/// The prompt of the round rides along as the label's main text.
pub(crate) fn generated_image_choices(history: &[Message]) -> Vec<ImageChoice> {
    let mut out = Vec::new();
    let mut prompt = String::new();
    for m in history {
        match m.role() {
            Role::User => prompt.clone_from(&m.content),
            Role::Assistant => {
                for att in &m.attachments {
                    if !att.mime_type.starts_with("image/") || att.data.is_empty() {
                        continue;
                    }
                    out.push(ImageChoice {
                        at: image_time(&att.filename),
                        att: att.clone(),
                        prompt: crate::repl::title::flatten_line(&prompt),
                    });
                }
            }
            _ => {}
        }
    }
    out.reverse();
    out
}

/// The generation time encoded in a saved image's name (`YYYYMMDD-HHMMSS-…`; editpicker.go:72).
///
/// Parsed AND formatted in the system zone, exactly as Go's `time.ParseInLocation` + `Format`
/// pair does, so the label round-trips whatever the host's zone is. A name too short, or one that
/// does not parse, is simply "unknown" and the label drops its time prefix.
pub(crate) fn image_time(filename: &str) -> Option<jiff::Zoned> {
    let base = Path::new(filename).file_name()?.to_str()?;
    let stamp = base.get(..STAMP_LEN)?;
    let civil = jiff::civil::DateTime::strptime(STAMP_LAYOUT, stamp).ok()?;
    civil.to_zoned(jiff::tz::TimeZone::system()).ok()
}

/// The picker rows: `"{HH:MM} · {prompt}"`, or the prompt alone (editpicker.go:90).
///
/// NOT truncated here — the picker knows its column budget and truncates by display width, so a
/// narrow terminal (no preview pane) still gets as much of the text as fits.
pub(crate) fn image_choice_labels(choices: &[ImageChoice]) -> Vec<String> {
    choices
        .iter()
        .map(|c| {
            let text = if c.prompt.is_empty() {
                &c.att.filename
            } else {
                &c.prompt
            };
            match &c.at {
                Some(at) => format!("{} · {text}", at.strftime("%H:%M")),
                None => text.clone(),
            }
        })
        .collect()
}

/// The dim detail rows: `"🖼 " + hyperlinked shortened path` for files that exist under `img_dir`
/// (editpicker.go:108).
///
/// The DISPLAY text is shortened to fit while the link target stays the full absolute path — so
/// the row never wraps and ⌘-click still opens the original. A file no longer on disk gets no
/// link at all, never a dead one.
pub(crate) fn image_choice_details(
    choices: &[ImageChoice],
    img_dir: Option<&Path>,
    width: usize,
    home: Option<&Path>,
) -> Vec<String> {
    choices
        .iter()
        .map(|c| {
            let (Some(dir), false) = (img_dir, c.att.filename.is_empty()) else {
                return String::new();
            };
            let path = dir.join(&c.att.filename);
            if !file_exists(&path) {
                return String::new();
            }
            let path = path.to_string_lossy().into_owned();
            let shown = shorten_path(&path, width.saturating_sub(4), home);
            format!(
                "🖼 {}",
                crate::markdown::link::hyperlink(
                    &format!("file://{path}"),
                    &shown,
                    crate::color::enabled(),
                )
            )
        })
        .collect()
}

/// `~`-abbreviates and left-truncates `path` to `width` columns (editpicker.go:127).
///
/// Truncation happens BEFORE the text is wrapped in a hyperlink — slicing an OSC 8 sequence apart
/// would corrupt the row.
pub(crate) fn shorten_path(path: &str, width: usize, home: Option<&Path>) -> String {
    let width = width.max(8);
    let mut display = path.to_owned();
    if let Some(h) = home
        .map(|h| h.to_string_lossy().into_owned())
        .filter(|h| !h.is_empty())
        && let Some(rest) = path.strip_prefix(&format!("{h}{}", std::path::MAIN_SEPARATOR))
    {
        display = format!("~{}{rest}", std::path::MAIN_SEPARATOR);
    }
    if crate::text::width::str_width(&display) <= width {
        return display;
    }
    let base = Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if crate::text::width::str_width(&base) <= width {
        return base;
    }
    crate::repl::styles::truncate_runes(&base, width)
}

/// The preview renderer: decodes lazily, caches decoded frames per index, remembers failures
/// (editpicker.go:140-170).
///
/// A multi-MB PNG decoded on every keystroke would be visible lag, and a re-decode of bytes that
/// already failed would repeat it on every frame — so both outcomes are remembered; only the
/// RASTERISATION is redone when the pane geometry changes.
pub(crate) struct ImagePreviewer {
    /// The rows, in picker order.
    choices: Vec<ImageChoice>,
    /// Decoded pictures, keyed by row index.
    decoded: HashMap<usize, imgterm::Frame>,
    /// Rows whose bytes do not decode.
    failed: HashSet<usize>,
}

impl ImagePreviewer {
    /// A previewer over `choices` with an empty cache.
    pub(crate) fn new(choices: Vec<ImageChoice>) -> Self {
        Self {
            choices,
            decoded: HashMap::new(),
            failed: HashSet::new(),
        }
    }

    /// The preview rows of `index` at `max_cols × max_rows`; `"(cannot preview {filename})"` on a
    /// decode failure, empty for an out-of-range index.
    pub(crate) fn render(&mut self, index: usize, max_cols: usize, max_rows: usize) -> Vec<String> {
        let Some(choice) = self.choices.get(index) else {
            return Vec::new();
        };
        if self.failed.contains(&index) {
            return Vec::new();
        }
        let frame = match self.decoded.entry(index) {
            Entry::Occupied(o) => o.into_mut(),
            Entry::Vacant(v) => {
                let Ok(f) = imgterm::decode(&choice.att.data) else {
                    // Remembered, not retried: a broken attachment would otherwise re-decode on
                    // every frame.
                    self.failed.insert(index);
                    return vec![format!("(cannot preview {})", choice.att.filename)];
                };
                v.insert(f)
            }
        };
        imgterm::render_frame(frame, max_cols, max_rows)
    }

    /// The `Panel.preview` closure over this previewer. It runs on the UI loop thread (like
    /// `Panel::refresh`), so it OWNS the decode cache rather than sharing it.
    pub(crate) fn into_preview_fn(mut self) -> PreviewFn {
        Box::new(move |index, max_cols, max_rows| self.render(index, max_cols, max_rows))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::{
        ImageChoice, ImagePreviewer, generated_image_choices, image_choice_details,
        image_choice_labels, image_time, shorten_path,
    };
    use crate::provider::model::{Attachment, Message, Role};
    use std::path::Path;

    /// A 64×64 solid PNG — large enough that the pane geometry, not the source size, bounds the
    /// render (imgterm never upscales). Go's `testPNG` encodes the same picture in process; the
    /// `image` crate is sealed inside `src/imgterm.rs` (ci.sh layering grep), so it is checked in.
    const GRAY_64X64_PNG: &[u8] = include_bytes!("../../tests/fixtures/images/gray-64x64.png");

    fn test_png() -> Vec<u8> {
        GRAY_64X64_PNG.to_vec()
    }

    fn png(name: &str, data: &[u8]) -> Attachment {
        Attachment {
            filename: name.to_owned(),
            mime_type: "image/png".to_owned(),
            data: data.to_vec(),
        }
    }

    /// The DISPLAY half of a detail row: the text between the OSC 8 target and its terminator.
    fn visible(detail: &str) -> String {
        let body = detail.trim_start_matches("🖼 ");
        let after = body.split_once("\u{1b}\\").map_or(body, |(_, r)| r);
        after
            .split_once("\u{1b}]8;;")
            .map_or(after, |(l, _)| l)
            .to_owned()
    }

    fn msg(role: Role, content: &str, atts: Vec<Attachment>) -> Message {
        Message {
            attachments: atts,
            ..Message::of_role(role, content.to_owned())
        }
    }

    // Go: chat/editpicker_test.go:32 TestGeneratedImageChoices
    #[test]
    fn test_generated_image_choices() {
        // The picker lists model-generated images newest first, carrying each round's prompt;
        // user attachments (uploads and /edit canvas copies) never qualify.
        let data = test_png();
        let history = vec![
            msg(Role::System, "sys", Vec::new()),
            msg(Role::User, "a cat\nsecond line", Vec::new()),
            msg(
                Role::Assistant,
                "",
                vec![png("20260725-093820-0.png", &data)],
            ),
            // The canvas copy rides the user turn — and must not become a choice.
            msg(
                Role::User,
                "add a hat",
                vec![png("20260725-093820-0.png", &data)],
            ),
            msg(
                Role::Assistant,
                "",
                vec![png("20260725-101500-0.png", &data)],
            ),
            msg(Role::User, "look at this", vec![png("upload.png", &data)]),
            msg(Role::Assistant, "text only", Vec::new()),
        ];

        let choices = generated_image_choices(&history);
        assert_eq!(choices.len(), 2, "the 2 generated images");
        assert_eq!(
            choices[0].att.filename, "20260725-101500-0.png",
            "newest first"
        );
        assert_eq!(choices[0].prompt, "add a hat");
        assert_eq!(choices[1].prompt, "a cat second line", "flattened");

        let labels = image_choice_labels(&choices);
        assert!(
            labels[0].starts_with("10:15 · add a hat"),
            "label = {:?}",
            labels[0]
        );
        // Labels stay ONE line: a multi-row label would break the surface's one-item-one-row
        // bookkeeping.
        for l in &labels {
            assert!(!l.contains(['\n', '\r', '\t']), "label spans rows: {l:?}");
        }
    }

    /// An unparseable or too-short stamp is "unknown", and the label then carries the text alone.
    #[test]
    fn labels_drop_an_unknown_time() {
        assert!(image_time("nope.png").is_none());
        assert!(image_time("20261301-999999-0.png").is_none());
        assert!(image_time("20260725-101500-0.png").is_some());
        let choices = vec![ImageChoice {
            att: png("nope.png", &[1]),
            prompt: String::new(),
            at: None,
        }];
        // An empty prompt falls back to the file name (editpicker.go:94).
        assert_eq!(image_choice_labels(&choices), vec!["nope.png".to_owned()]);
    }

    // Go: chat/editpicker_test.go:73 TestImageChoiceDetails
    #[test]
    fn test_image_choice_details() {
        // Details link the on-disk file with a shortened display text; a missing file yields no
        // link at all (never a dead one).
        let dir = tempfile::tempdir().expect("tempdir");
        let choices = vec![
            ImageChoice {
                att: png("here.png", &[]),
                prompt: String::new(),
                at: None,
            },
            ImageChoice {
                att: png("gone.png", &[]),
                prompt: String::new(),
                at: None,
            },
        ];
        std::fs::write(dir.path().join("here.png"), [1]).expect("write");

        let details = image_choice_details(&choices, Some(dir.path()), 100, None);
        assert!(
            details[0].contains("here.png"),
            "existing file must get a detail line: {:?}",
            details[0]
        );
        assert_eq!(details[1], "", "missing file must get no link");

        // A narrow width falls back to the file name — the display text is sized BEFORE the
        // hyperlink wraps it, so the escape is never sliced. (Go asserts the dir is absent from
        // the whole row because `go test` runs under `color.NoColor`, where `Hyperlink` returns
        // bare text; the Rust twin always emits the OSC 8 target, so the DISPLAY half is what
        // carries the claim — the link target keeps the full path either way.)
        let narrow = image_choice_details(&choices, Some(dir.path()), 14, None);
        let shown = visible(&narrow[0]);
        assert_eq!(shown, "here.png", "narrow detail = {:?}", narrow[0]);
        assert!(
            narrow[0].contains(&*dir.path().to_string_lossy()),
            "the LINK keeps the full path: {:?}",
            narrow[0]
        );
        assert_eq!(
            visible(&details[0]),
            dir.path().join("here.png").display().to_string()
        );
        // No images dir (an ephemeral chat) means no details at all.
        assert_eq!(image_choice_details(&choices, None, 100, None), ["", ""]);
    }

    /// `shorten_path` prefers `~`, then the bare name, then a rune truncation; the floor is 8
    /// columns (editpicker.go:127-142).
    #[test]
    fn shorten_path_steps_down() {
        // `shorten_path` works on strings and abbreviates a home prefix with MAIN_SEPARATOR, so the
        // fixtures are spelled the way the platform spells them: a `C:\Users\u` home never prefixes
        // a `/`-spelled path.
        let sep = std::path::MAIN_SEPARATOR;
        let home = format!("{sep}home{sep}u");
        let pic = format!("{home}{sep}pics{sep}a.png");
        let deep = format!("{sep}var{sep}x{sep}aaaaaaaaaaaaaaaa.png");
        let home = Path::new(&home);
        assert_eq!(
            shorten_path(&pic, 40, Some(home)),
            format!("~{sep}pics{sep}a.png")
        );
        assert_eq!(shorten_path(&pic, 10, Some(home)), "a.png");
        assert_eq!(shorten_path(&deep, 8, None), "aaaaaaaa…");
        // A width below the floor is raised to 8, not honoured.
        assert_eq!(shorten_path(&deep, 0, None), "aaaaaaaa…");
        // Without a home the path stays absolute.
        assert_eq!(shorten_path(&pic, 40, None), pic);
    }

    // Go: chat/editpicker_test.go:100 TestImagePreviewerCachesDecode
    #[test]
    fn test_image_previewer_caches_decode() {
        // The previewer decodes each image once and re-rasterises per geometry.
        let choices = vec![
            ImageChoice {
                att: png("ok.png", &test_png()),
                prompt: String::new(),
                at: None,
            },
            ImageChoice {
                att: png("broken.png", &[1, 2]),
                prompt: String::new(),
                at: None,
            },
        ];
        let mut p = ImagePreviewer::new(choices);

        let rows = p.render(0, 20, 6);
        assert!(
            !rows.is_empty() && rows.join("").contains('▀'),
            "preview rows = {rows:?}"
        );
        assert!(p.decoded.contains_key(&0), "decoded image not cached");
        let wider = p.render(0, 40, 12);
        assert!(
            wider.len() > rows.len(),
            "larger geometry must produce more rows: {} vs {}",
            wider.len(),
            rows.len()
        );

        let got = p.render(1, 20, 6);
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].contains("cannot preview"), "{got:?}");
        assert!(
            p.failed.contains(&1),
            "a failed decode must be remembered, not retried each frame"
        );
        assert!(
            p.render(1, 20, 6).is_empty(),
            "a remembered failure is silent"
        );
        assert!(p.render(99, 20, 6).is_empty(), "out-of-range index");
    }

    /// The closure form keeps the cache alive across calls (it is what a `Panel` carries).
    #[test]
    fn preview_fn_owns_the_cache() {
        let choices = vec![ImageChoice {
            att: png("ok.png", &test_png()),
            prompt: String::new(),
            at: None,
        }];
        let mut f = ImagePreviewer::new(choices).into_preview_fn();
        assert!(f(0, 20, 6).join("").contains('▀'));
        assert!(f(0, 40, 12).len() > f(0, 20, 6).len());
    }
}
