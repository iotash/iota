//! Half-block image rendering for the terminal (internal/imgterm/imgterm.go): a decoded picture
//! is scaled to fit `max_cols × max_rows` cells (two pixels per cell, upper half `▀` in the
//! foreground colour, lower half in the background colour) with Go's integer geometry verbatim
//! (spec `images.md` §3.1, CONTRACTS §3.1).
//!
//! Every returned line carries its own SGR state and ends reset — the shape the chat transcript,
//! the resume echo and the edit picker all demand of committed rows.
//!
//! This is the ONLY module allowed to name the `image` crate (ci.sh grep; ARCHITECTURE §1.2):
//! every other module sees a [`Frame`].

use std::fmt;
use std::fmt::Write as _;

use image::imageops::FilterType;

/// The upper half block every cell is drawn with (imgterm.go:22).
pub const UPPER_HALF: &str = "▀";

/// A decode failure: `decode image: {reason}` (imgterm.go:34).
#[derive(Debug)]
pub struct ImgtermError(String);

impl fmt::Display for ImgtermError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "decode image: {}", self.0)
    }
}

impl std::error::Error for ImgtermError {}

/// A decoded RGBA picture — the crate's only `image::` type, sealed here so the edit picker's
/// cache and the replay echo never name the crate.
pub struct Frame(image::RgbaImage);

impl Frame {
    /// `(width, height)` in pixels.
    pub fn dimensions(&self) -> (u32, u32) {
        self.0.dimensions()
    }
}

impl fmt::Debug for Frame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (w, h) = self.dimensions();
        write!(f, "Frame({w}×{h})")
    }
}

/// Decodes `data` (png/jpeg/gif/webp — the four formats Go registers) into a [`Frame`].
///
/// # Errors
/// [`ImgtermError`] when the bytes are not one of the four supported formats, or are truncated.
pub fn decode(data: &[u8]) -> Result<Frame, ImgtermError> {
    match image::load_from_memory(data) {
        Ok(img) => Ok(Frame(img.to_rgba8())),
        Err(e) => Err(ImgtermError(e.to_string())),
    }
}

/// [`decode`] then [`render_frame`] (imgterm.go:31 `Render`).
///
/// The tighter of the two constraints wins — a generated image must never take over the screen —
/// and pictures already smaller than the budget render 1:1 (never upscaled).
///
/// # Errors
/// [`ImgtermError`] when `data` does not decode; callers fall back to just naming the file.
pub fn render(data: &[u8], max_cols: usize, max_rows: usize) -> Result<Vec<String>, ImgtermError> {
    Ok(render_frame(&decode(data)?, max_cols, max_rows))
}

/// Renders `img` into at most `max_cols × max_rows` cells of half blocks, one SGR-coloured row
/// per line, every line ending `\x1b[0m` (imgterm.go:40-90).
///
/// A zero-sized picture renders nothing.
#[must_use]
pub fn render_frame(img: &Frame, max_cols: usize, max_rows: usize) -> Vec<String> {
    let max_cols = max_cols.max(1);
    let max_rows = max_rows.max(1);
    let (src_w, src_h) = img.dimensions();
    if src_w == 0 || src_h == 0 {
        return Vec::new();
    }
    let (w, h) = fit(src_w, src_h, max_cols, max_rows);
    let scaled = image::imageops::resize(&img.0, w, h, FilterType::Triangle);

    let mut lines = Vec::with_capacity(h.div_ceil(2) as usize);
    let mut b = String::new();
    let mut y = 0;
    while y < h {
        b.clear();
        for x in 0..w {
            let (tr, tg, tb) = blend_on_black(*scaled.get_pixel(x, y));
            if y + 1 < h {
                let (br, bg, bb) = blend_on_black(*scaled.get_pixel(x, y + 1));
                let _ = write!(
                    b,
                    "\x1b[38;2;{tr};{tg};{tb}m\x1b[48;2;{br};{bg};{bb}m{UPPER_HALF}"
                );
            } else {
                // Odd final row: only the top half is image; the bottom keeps the terminal's
                // own background.
                let _ = write!(b, "\x1b[49m\x1b[38;2;{tr};{tg};{tb}m{UPPER_HALF}");
            }
        }
        b.push_str("\x1b[0m");
        lines.push(b.clone());
        y += 2;
    }
    lines
}

/// Go's integer geometry (imgterm.go:41-62) verbatim: the pixel box a source of `src_w × src_h`
/// occupies inside `max_cols` columns and `max_rows` cell rows (one cell = two pixel rows).
fn fit(src_w: u32, src_h: u32, max_cols: usize, max_rows: usize) -> (u32, u32) {
    let src_w = u64::from(src_w);
    let src_h = u64::from(src_h);
    let max_px_h = (max_rows as u64).saturating_mul(2);
    let mut w = src_w.min(max_cols as u64);
    let mut h = (src_h * w + src_w / 2) / src_w;
    if h > max_px_h {
        // Height-bound: shrink the width to keep the aspect.
        h = max_px_h;
        w = (src_w * h + src_h / 2) / src_h;
        if w < 1 {
            w = 1;
        }
    }
    if h < 1 {
        h = 1;
    }
    // `w ≤ src_w` and `h ≤ max(2·max_rows, 1)` — both fit a u32 by construction, but the
    // saturating cast keeps the function total for absurd budgets.
    (
        u32::try_from(w).unwrap_or(u32::MAX),
        u32::try_from(h).unwrap_or(u32::MAX),
    )
}

/// Composites a pixel over BLACK — terminals have no alpha channel, and dark blends degrade most
/// gracefully across light and dark themes (imgterm.go:93-98).
///
/// Go read the scaled `image.RGBA`'s PREMULTIPLIED samples and dropped the alpha; the `image`
/// crate stores straight alpha, so the multiply happens here (`c·a/255`).
fn blend_on_black(px: image::Rgba<u8>) -> (u8, u8, u8) {
    let a = u32::from(px.0[3]);
    let mul = |c: u8| u8::try_from(u32::from(c) * a / 255).unwrap_or(u8::MAX);
    (mul(px.0[0]), mul(px.0[1]), mul(px.0[2]))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    //! `internal/imgterm/imgterm_test.go`: the half-block cell shape, the two size caps and the
    //! odd-row fallback. Fixtures are encoded in-process (the Go test's `encode` helper).

    use super::{UPPER_HALF, render};

    /// Go `encode` (`imgterm_test.go:13`): a PNG built from explicit pixels.
    fn encode(w: u32, h: u32, px: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
        let img = image::RgbaImage::from_fn(w, h, |x, y| image::Rgba(px(x, y)));
        let mut buf = Vec::new();
        image::codecs::png::PngEncoder::new(&mut buf)
            .write_image(&img, w, h, image::ExtendedColorType::Rgba8)
            .expect("png encode");
        buf
    }

    use image::ImageEncoder as _;

    // Go: internal/imgterm/imgterm_test.go:30 TestRenderHalfBlocks — a 2×2 image renders as ONE
    // line of two half-block cells, each cell's fg the top pixel and bg the bottom pixel, the
    // line reset-terminated.
    #[test]
    fn test_render_half_blocks() {
        let data = encode(2, 2, |_, y| {
            if y == 0 {
                [255, 0, 0, 255] // top row red
            } else {
                [0, 0, 255, 255] // bottom row blue
            }
        });
        let lines = render(&data, 80, 24).expect("decode");
        assert_eq!(lines.len(), 1, "lines = {lines:?}, want 1");
        let want =
            "\x1b[38;2;255;0;0m\x1b[48;2;0;0;255m▀\x1b[38;2;255;0;0m\x1b[48;2;0;0;255m▀\x1b[0m";
        assert_eq!(lines[0], want);
    }

    // Go: internal/imgterm/imgterm_test.go:51 TestRenderScalesToMaxCols — wide images downscale
    // to `max_cols` with the aspect kept; rows = ceil(h/2), every line self-contained.
    #[test]
    fn test_render_scales_to_max_cols() {
        let data = encode(200, 100, |_, _| [9, 9, 9, 255]);
        let lines = render(&data, 40, 24).expect("decode");
        assert_eq!(lines.len(), 10, "100 · 40/200 = 20 px tall → 10 cell rows");
        for (i, ln) in lines.iter().enumerate() {
            assert!(ln.ends_with("\x1b[0m"), "line {i} not reset-terminated");
        }
    }

    // Go: internal/imgterm/imgterm_test.go:69 TestRenderOddHeight — an odd final pixel row paints
    // only foregrounds, leaving the terminal's own background below.
    #[test]
    fn test_render_odd_height() {
        let data = encode(1, 3, |_, _| [1, 2, 3, 255]);
        let lines = render(&data, 80, 24).expect("decode");
        assert_eq!(lines.len(), 2, "rows = {lines:?}, want 2");
        assert!(
            lines[1].contains("\x1b[49m") && !lines[1].contains("\x1b[48;2"),
            "odd row must keep the terminal background: {:?}",
            lines[1]
        );
    }

    // Go: internal/imgterm/imgterm_test.go:83 TestRenderBadData — undecodable bytes are an error
    // whose Display is Go's `decode image: %w`.
    #[test]
    fn test_render_bad_data() {
        let err = render(b"not an image", 80, 24).expect_err("want a decode error");
        assert!(err.to_string().starts_with("decode image: "), "err = {err}");
    }

    // Go: internal/imgterm/imgterm_test.go:91 TestRenderHeightCap — a tall image is height-bound:
    // rows never exceed `max_rows` and the width shrinks to keep the aspect.
    #[test]
    fn test_render_height_cap() {
        let data = encode(100, 400, |_, _| [5, 5, 5, 255]);
        let lines = render(&data, 72, 14).expect("decode");
        assert!(lines.len() <= 14, "rows = {}, want ≤ 14", lines.len());
        // 28 px tall → width 100·28/400 = 7 cells.
        assert_eq!(lines[0].matches(UPPER_HALF).count(), 7);
    }

    /// New (no Go twin — Go's `image.Decode` cannot produce a zero-sized picture through the
    /// same path): a budget below one cell still renders exactly one cell row.
    #[test]
    fn zero_budget_clamps_to_one_cell() {
        let data = encode(4, 4, |_, _| [0, 0, 0, 255]);
        let lines = render(&data, 0, 0).expect("decode");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].matches(UPPER_HALF).count(), 1);
    }

    /// New: alpha composites over black (`c·a/255`), the Go premultiplied read's twin.
    #[test]
    fn alpha_blends_over_black() {
        let data = encode(1, 2, |_, _| [255, 255, 255, 0]);
        let lines = render(&data, 80, 24).expect("decode");
        assert_eq!(
            lines[0], "\x1b[38;2;0;0;0m\x1b[48;2;0;0;0m▀\x1b[0m",
            "a fully transparent picture blends to black"
        );
    }
}
