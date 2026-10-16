//! Pixel-level comparison of two rendered frames.
//!
//! This is the one-off P0 comparison (`p0-spikes/compare_manim.py`) turned into
//! code, so the same numbers can be *asserted* by a test instead of read off a
//! terminal once and written into a document. It lives in `rustim-export`
//! rather than inside a test because P5's visual-regression CLI (§7.3 — the
//! `fframes` `snapshot`/`onion` pair) reuses it, and because the `png` crate
//! this crate already depends on ships the decoder as well as the encoder, so
//! measuring costs no new dependency.
//!
//! # What it measures, and why two things
//!
//! - **Content bounding boxes** — the box of pixels that differ from the
//!   frame's background. Equal boxes mean the *layout* is identical: a shift of
//!   one pixel moves an outermost edge and changes the box. This is the metric
//!   that proved in P0 that the coordinate system matched Manim's exactly
//!   (§5.2: `(343, 90, 936, 629)` from both renderers).
//! - **A difference histogram** — how many pixels differ by more than each of
//!   [`THRESHOLDS`], plus the mean and maximum per-pixel difference. This is
//!   where anti-aliasing differences land: they move a few dozen edge pixels by
//!   one or two levels and leave the layout intact.
//!
//! The two fail for different reasons, so they are reported as data
//! ([`ImageDiff`]) and judged independently ([`Tolerance`]).
//!
//! # Deviations from the sketch in the plan
//!
//! [`compare_png_files`] takes a `&Tolerance` as well as the two paths. The
//! content bounding box is defined relative to a background colour, and no
//! amount of reading the pixels recovers which colour that is — a scene that
//! fills most of the frame would have the modal colour be a shape, not the
//! background. Passing the tolerance in is more honest than a hidden default.
//!
//! [`ImageDiff`] stores the full 256-bin difference histogram and derives
//! `counts` from it, rather than storing a six-entry array and hoping it agrees.
//! 2 KiB per comparison is nothing next to not having an invariant to maintain.

use std::path::Path;

use rustim_color::Srgb;

/// Per-pixel difference levels the histogram is reported at.
///
/// The same ladder the P0 script printed, so the numbers in `方案建议.md` §5.2
/// and the numbers in a CI log are directly comparable.
pub const THRESHOLDS: [u8; 6] = [0, 2, 8, 16, 32, 64];

/// Differences above this level are localised by [`ImageDiff::busiest`].
///
/// 32 because it is well clear of anti-aliasing noise: a pixel this far off is
/// geometry in the wrong place, not a differently-sampled edge.
const BUSY_THRESHOLD: u8 = 32;

/// An inclusive bounding box in pixel coordinates.
///
/// Not `rustim_math::Rect`: that one is `f64` and lives in the semantic kernel,
/// which this crate deliberately does not reach into for a measurement type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bbox {
    pub x0: u32,
    pub y0: u32,
    /// Inclusive, so a one-pixel box is `x0 == x1`.
    pub x1: u32,
    pub y1: u32,
}

impl Bbox {
    pub fn width(&self) -> u32 {
        self.x1 - self.x0 + 1
    }

    pub fn height(&self) -> u32 {
        self.y1 - self.y0 + 1
    }
}

impl std::fmt::Display for Bbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "({}, {})..({}, {}) {}x{}",
            self.x0,
            self.y0,
            self.x1,
            self.y1,
            self.width(),
            self.height()
        )
    }
}

/// When two frames count as equivalent.
///
/// [`Default`] is the acceptance rule `方案建议.md` §5.2 measured, which
/// `CLAUDE.md` records as the standard and the P1 plan fixes as the pass mark:
/// identical content bounding boxes, no pixel differing by more than 8, and a
/// maximum per-channel error of 3. No "close enough" allowances — if a
/// comparison needs a looser rule, that is a decision to make explicitly at the
/// call site rather than a default that quietly absorbs regressions.
#[derive(Debug, Clone, Copy)]
pub struct Tolerance {
    /// The colour a "blank" pixel has. Both bounding boxes are taken relative
    /// to it.
    pub background: [u8; 3],
    /// A pixel counts as content when its largest channel difference from the
    /// background exceeds this. 8 reproduces §5.2.
    pub content_threshold: u8,
    /// Require the two content bounding boxes to be equal, pixel for pixel.
    pub bbox_must_match: bool,
    /// At most `max_pixels_above` pixels may differ by more than this.
    pub channel_threshold: u8,
    pub max_pixels_above: usize,
    /// No pixel may differ by more than this.
    pub max_error: u8,
}

impl Tolerance {
    /// The §5.2 acceptance rule, for a frame on the 3b1b background.
    pub fn rgba8(background: [u8; 3]) -> Self {
        Self {
            background,
            content_threshold: 8,
            bbox_must_match: true,
            channel_threshold: 8,
            max_pixels_above: 0,
            max_error: 3,
        }
    }
}

impl Default for Tolerance {
    /// [`rustim_color::BG`] — `#121212`, the colour every scene fills first.
    ///
    /// Converted from the palette constant rather than written as a literal, so
    /// the two cannot drift; `components` are plain sRGB bytes over 255 for
    /// `AlphaColor<Srgb>` (asserted by `rustim-color`'s own test).
    fn default() -> Self {
        Self::rgba8(srgb8(rustim_color::BG))
    }
}

fn srgb8(c: Srgb) -> [u8; 3] {
    let [r, g, b, _] = c.components;
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    [q(r), q(g), q(b)]
}

/// The result of comparing two frames. Data only; nothing here decides a pass.
#[derive(Debug, Clone)]
pub struct ImageDiff {
    pub width: u32,
    pub height: u32,
    /// The content box of the first image, or `None` if it is all background.
    pub bbox_a: Option<Bbox>,
    pub bbox_b: Option<Bbox>,
    /// The box of pixels where the two images differ *at all*. Tighter than the
    /// content boxes, and the first thing to look at when they stop matching.
    pub diff_bbox: Option<Bbox>,
    /// Mean and maximum of the per-pixel maximum channel difference.
    pub mean: f64,
    pub max: u8,
    /// The rows and columns carrying the most disagreement above
    /// [`BUSY_THRESHOLD`], most-loaded first.
    ///
    /// A systematic offset shows up as a few dense rows or columns; diffuse
    /// anti-aliasing noise does not, which is what makes this worth printing.
    pub busiest: ([usize; 6], [usize; 6]),
    /// `histogram[d]` is the number of pixels whose largest channel difference
    /// is exactly `d`.
    histogram: [usize; 256],
}

impl ImageDiff {
    pub fn total_pixels(&self) -> usize {
        self.width as usize * self.height as usize
    }

    /// How many pixels differ by more than each of [`THRESHOLDS`].
    pub fn counts(&self) -> [usize; 6] {
        THRESHOLDS.map(|t| self.count_above(t))
    }

    /// How many pixels differ by more than `threshold`.
    pub fn count_above(&self, threshold: u8) -> usize {
        self.histogram[usize::from(threshold) + 1..].iter().sum()
    }

    /// Which acceptance criteria this comparison fails, each as a sentence.
    ///
    /// Empty means it passes. Returning the reasons rather than a bare `bool`
    /// is what lets `passes` be its special case and lets a failing test print
    /// exactly what broke instead of the whole report.
    pub fn failures(&self, t: &Tolerance) -> Vec<String> {
        let mut out = Vec::new();
        if t.bbox_must_match && self.bbox_a != self.bbox_b {
            out.push(format!(
                "content bounding boxes differ: a={:?} b={:?}",
                self.bbox_a, self.bbox_b
            ));
        }
        let above = self.count_above(t.channel_threshold);
        if above > t.max_pixels_above {
            out.push(format!(
                "{above} pixel(s) differ by more than {} (allowed {})",
                t.channel_threshold, t.max_pixels_above
            ));
        }
        if self.max > t.max_error {
            out.push(format!(
                "maximum channel error is {} (allowed {})",
                self.max, t.max_error
            ));
        }
        out
    }

    pub fn passes(&self, t: &Tolerance) -> bool {
        self.failures(t).is_empty()
    }

    /// A human-readable report, in the format the P0 script printed.
    pub fn summary(&self) -> String {
        let total = self.total_pixels().max(1);
        let pct = |n: usize| 100.0 * n as f64 / total as f64;
        let mut s = format!("{}x{}, {} pixels\n", self.width, self.height, total);

        let opt = |b: Option<Bbox>| match b {
            Some(b) => b.to_string(),
            None => "none (blank frame)".to_string(),
        };
        s.push_str(&format!("  content bbox  a = {}\n", opt(self.bbox_a)));
        s.push_str(&format!("                b = {}\n", opt(self.bbox_b)));
        s.push_str(&format!("  differing bbox  = {}\n", opt(self.diff_bbox)));

        for (t, n) in THRESHOLDS.iter().zip(self.counts()) {
            s.push_str(&format!(
                "  pixels differing by > {t:>3}: {n:>9}  {:7.3}%\n",
                pct(n)
            ));
        }
        s.push_str(&format!(
            "  mean abs channel error: {:.3}   max: {}\n",
            self.mean, self.max
        ));
        if self.count_above(BUSY_THRESHOLD) > 0 {
            s.push_str(&format!(
                "  busiest rows    : {:?}\n  busiest columns : {:?}\n",
                self.busiest.0, self.busiest.1
            ));
        }
        s
    }

    /// The report plus a verdict, which is what a failing test should print.
    pub fn report(&self, t: &Tolerance) -> String {
        let failures = self.failures(t);
        let mut s = self.summary();
        if failures.is_empty() {
            s.push_str("  verdict: PASS\n");
        } else {
            for f in failures {
                s.push_str(&format!("  FAIL: {f}\n"));
            }
        }
        s
    }
}

/// Compare two PNG files.
///
/// Both are normalised to 8-bit RGBA first — greyscale expands to `(v, v, v,
/// 255)`, greyscale+alpha to `(v, v, v, a)`, RGB to RGB with alpha 255 — so a
/// Manim reference frame (RGB8) and a rustim one (RGBA8) can be compared
/// without either side having to match the other's format.
///
/// Differing image sizes are an error rather than a diff: there is no
/// meaningful pixel-to-pixel comparison to make, and a "0% different" verdict
/// on a mis-sized render is the kind of silent pass this whole module exists to
/// prevent.
pub fn compare_png_files(a: &Path, b: &Path, t: &Tolerance) -> std::io::Result<ImageDiff> {
    let (ia, ib) = (read_png(a)?, read_png(b)?);
    if (ia.width, ia.height) != (ib.width, ib.height) {
        return Err(std::io::Error::other(format!(
            "size mismatch: {} is {}x{}, {} is {}x{}",
            a.display(),
            ia.width,
            ia.height,
            b.display(),
            ib.width,
            ib.height
        )));
    }
    Ok(compare(&ia.pixels, &ib.pixels, ia.width, ia.height, t))
}

/// One decoded frame, normalised to 8-bit RGBA.
struct Image {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

fn read_png(path: &Path) -> std::io::Result<Image> {
    let file = std::fs::File::open(path)?;
    let mut dec = png::Decoder::new(std::io::BufReader::new(file));
    // EXPAND + STRIP_16: palette to RGB, sub-8-bit greyscale to 8-bit, 16-bit to
    // 8-bit. Without it every colour type would need its own decoder arm here.
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = dec
        .read_info()
        .map_err(|e| std::io::Error::other(format!("{}: {e}", path.display())))?;
    let size = reader.output_buffer_size().ok_or_else(|| {
        std::io::Error::other(format!("{}: frame size is not knowable", path.display()))
    })?;
    let mut buf = vec![0u8; size];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| std::io::Error::other(format!("{}: {e}", path.display())))?;

    let pixels = expand(&buf, info.color_type).ok_or_else(|| {
        std::io::Error::other(format!(
            "{}: unsupported colour type {:?}",
            path.display(),
            info.color_type
        ))
    })?;
    let want = info.width as usize * info.height as usize * 4;
    if pixels.len() < want {
        return Err(std::io::Error::other(format!(
            "{}: decoded {} bytes, need {want} for {}x{}",
            path.display(),
            pixels.len(),
            info.width,
            info.height
        )));
    }
    let mut pixels = pixels;
    pixels.truncate(want);
    Ok(Image {
        width: info.width,
        height: info.height,
        pixels,
    })
}

/// Widen whatever the decoder produced to 8-bit RGBA.
fn expand(buf: &[u8], color: png::ColorType) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(buf.len() / 3 * 4 + 4);
    match color {
        png::ColorType::Rgba => out.extend_from_slice(buf),
        png::ColorType::Rgb => {
            for px in buf.as_chunks::<3>().0 {
                out.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
        }
        png::ColorType::Grayscale => {
            for &v in buf {
                out.extend_from_slice(&[v, v, v, 255]);
            }
        }
        png::ColorType::GrayscaleAlpha => {
            for px in buf.as_chunks::<2>().0 {
                out.extend_from_slice(&[px[0], px[0], px[0], px[1]]);
            }
        }
        // Indexed is expanded away by `normalize_to_color8`, so reaching this
        // means the transformation was not applied.
        png::ColorType::Indexed => return None,
    }
    Some(out)
}

fn compare(a: &[u8], b: &[u8], width: u32, height: u32, t: &Tolerance) -> ImageDiff {
    let px_count = width as usize * height as usize;
    let mut histogram = [0usize; 256];
    let mut sum: u64 = 0;
    let mut max = 0u8;
    let mut diff_bbox: Option<Bbox> = None;
    let mut content_a: Option<Bbox> = None;
    let mut content_b: Option<Bbox> = None;
    let mut busy_rows = vec![0usize; height as usize];
    let mut busy_cols = vec![0usize; width as usize];

    let grow = |bbox: &mut Option<Bbox>, x: u32, y: u32| {
        *bbox = Some(match *bbox {
            None => Bbox {
                x0: x,
                y0: y,
                x1: x,
                y1: y,
            },
            Some(r) => Bbox {
                x0: r.x0.min(x),
                y0: r.y0.min(y),
                x1: r.x1.max(x),
                y1: r.y1.max(y),
            },
        });
    };

    for i in 0..px_count {
        let (x, y) = ((i % width as usize) as u32, (i / width as usize) as u32);
        let pa = &a[i * 4..i * 4 + 4];
        let pb = &b[i * 4..i * 4 + 4];

        let d = (0..3)
            .map(|c| pa[c].abs_diff(pb[c]))
            .max()
            .unwrap_or_default();
        histogram[usize::from(d)] += 1;
        sum += u64::from(d);
        max = max.max(d);
        if d > 0 {
            grow(&mut diff_bbox, x, y);
        }
        if d > BUSY_THRESHOLD {
            busy_rows[y as usize] += 1;
            busy_cols[x as usize] += 1;
        }
        if (0..3).any(|c| pa[c].abs_diff(t.background[c]) > t.content_threshold) {
            grow(&mut content_a, x, y);
        }
        if (0..3).any(|c| pb[c].abs_diff(t.background[c]) > t.content_threshold) {
            grow(&mut content_b, x, y);
        }
    }
    ImageDiff {
        width,
        height,
        bbox_a: content_a,
        bbox_b: content_b,
        diff_bbox,
        mean: sum as f64 / px_count.max(1) as f64,
        max,
        busiest: (top_six(&busy_rows), top_six(&busy_cols)),
        histogram,
    }
}

/// The indices of the six largest entries, largest first.
fn top_six(counts: &[usize]) -> [usize; 6] {
    let mut idx: Vec<usize> = (0..counts.len()).filter(|&i| counts[i] > 0).collect();
    idx.sort_by(|&l, &r| counts[r].cmp(&counts[l]).then(l.cmp(&r)));
    let mut out = [usize::MAX; 6];
    for (slot, &i) in out.iter_mut().zip(idx.iter()) {
        *slot = i;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(width: u32, height: u32, fill: [u8; 4]) -> Vec<u8> {
        fill.iter()
            .copied()
            .cycle()
            .take((width * height * 4) as usize)
            .collect()
    }

    fn put(px: &mut [u8], width: u32, x: u32, y: u32, color: [u8; 4]) {
        let i = ((y * width + x) * 4) as usize;
        px[i..i + 4].copy_from_slice(&color);
    }

    fn tol() -> Tolerance {
        Tolerance::rgba8([0, 0, 0])
    }

    #[test]
    fn identical_frames_pass_and_report_nothing() {
        let a = img(4, 4, [0, 0, 0, 255]);
        let d = compare(&a, &a, 4, 4, &tol());
        assert!(d.passes(&tol()), "{}", d.report(&tol()));
        assert_eq!(d.max, 0);
        assert_eq!(d.mean, 0.0);
        assert_eq!(d.counts(), [0; 6]);
        assert!(d.diff_bbox.is_none());
        assert!(
            d.bbox_a.is_none(),
            "an all-background frame has no content box"
        );
    }

    #[test]
    fn counts_are_strictly_above_the_threshold() {
        // One pixel at each of the ladder values, plus one beyond the top.
        let mut a = img(8, 1, [0, 0, 0, 255]);
        for (x, v) in [2u8, 8, 16, 32, 64, 200].into_iter().enumerate() {
            put(&mut a, 8, x as u32, 0, [v, 0, 0, 255]);
        }
        let b = img(8, 1, [0, 0, 0, 255]);
        let d = compare(&a, &b, 8, 1, &tol());
        // Thresholds are strict (`d > t`), matching the P0 script's `d > t`: the
        // pixel that differs by exactly 2 is not counted at threshold 2, and the
        // one differing by exactly 64 is not counted at 64.
        assert_eq!(d.counts(), [6, 5, 4, 3, 2, 1]);
        assert_eq!(d.count_above(255), 0);
        assert_eq!(d.max, 200);
        // `counts` is derived from the histogram, so these two can only disagree
        // if the derivation is wrong — which is why there is no second copy of
        // the numbers to keep in step.
        assert_eq!(d.counts()[2], d.count_above(8));
    }

    #[test]
    fn a_one_pixel_shift_moves_the_content_box() {
        let mut a = img(16, 16, [0, 0, 0, 255]);
        let mut b = img(16, 16, [0, 0, 0, 255]);
        for y in 4..12u32 {
            for x in 4..12u32 {
                put(&mut a, 16, x, y, [255, 255, 255, 255]);
                put(&mut b, 16, x + 1, y, [255, 255, 255, 255]);
            }
        }
        let d = compare(&a, &b, 16, 16, &tol());
        assert_eq!(d.bbox_a.unwrap().x0, 4);
        assert_eq!(d.bbox_b.unwrap().x0, 5);
        assert!(
            !d.passes(&tol()),
            "a shifted figure must fail the bbox rule: {}",
            d.report(&tol())
        );
        // And the failure names the box, rather than leaving the reader to
        // reconstruct it from a count of differing pixels.
        let f = d.failures(&tol());
        assert!(
            f.iter()
                .any(|s| s.contains("content bounding boxes differ")),
            "{f:?}"
        );
    }

    #[test]
    fn a_sub_threshold_wobble_stays_within_the_layout_rule() {
        // Every pixel is off by 3 — the §5.2 maximum error — but nothing has
        // moved, so the boxes agree and the default tolerance passes.
        let a = img(8, 8, [0, 0, 0, 255]);
        let b = img(8, 8, [3, 3, 3, 255]);
        let d = compare(&a, &b, 8, 8, &tol());
        assert_eq!(d.max, 3);
        assert!(d.passes(&tol()), "{}", d.report(&tol()));

        let c = img(8, 8, [4, 4, 4, 255]);
        let d = compare(&a, &c, 8, 8, &tol());
        assert!(!d.passes(&tol()), "max error 4 must fail a limit of 3");
    }

    #[test]
    fn a_lone_bright_pixel_is_counted_and_rejected() {
        let a = img(8, 8, [0, 0, 0, 255]);
        let mut b = a.clone();
        put(&mut b, 8, 3, 5, [255, 0, 0, 255]);
        let d = compare(&a, &b, 8, 8, &tol());
        assert_eq!(d.count_above(8), 1);
        assert!(!d.passes(&tol()));
        let f = d.failures(&tol());
        assert!(
            f.iter()
                .any(|s| s.contains("1 pixel(s) differ by more than 8")),
            "{f:?}"
        );
        assert_eq!(d.diff_bbox.unwrap().to_string(), "(3, 5)..(3, 5) 1x1");
    }

    #[test]
    fn busiest_localises_a_dense_row() {
        let a = img(8, 8, [0, 0, 0, 255]);
        let mut b = a.clone();
        for x in 0..8u32 {
            put(&mut b, 8, x, 6, [255, 255, 255, 255]);
        }
        let d = compare(&a, &b, 8, 8, &tol());
        assert_eq!(d.busiest.0[0], 6, "row 6 is where the change is");
        assert_eq!(
            d.busiest.0[1],
            usize::MAX,
            "no other row crosses the busy threshold"
        );
    }

    /// The whole path a golden test takes: render, encode, decode, compare.
    #[test]
    fn a_flat_frame_matches_itself_through_a_png() {
        let mut r = rustim_render::CpuRenderer::new(16, 16);
        let mut list = rustim_render::DrawList::new(16, 16);
        list.background = Some(rustim_color::BG);
        rustim_render::Renderer::render(&mut r, &list).unwrap();

        let dir = std::env::temp_dir().join("rustim-compare-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("flat.png");
        crate::write_png(&r, &path).unwrap();

        let d = compare_png_files(&path, &path, &Tolerance::default()).unwrap();
        assert!(
            d.passes(&Tolerance::default()),
            "a file must match itself: {}",
            d.report(&Tolerance::default())
        );
        // The default tolerance really is the 3b1b background: were it not, the
        // flat frame would have a content box of the whole image.
        assert!(d.bbox_a.is_none());
        assert_eq!(d.counts(), [0; 6]);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn grey_and_rgb_expand_to_rgba() {
        assert_eq!(
            expand(&[10, 20], png::ColorType::GrayscaleAlpha),
            Some(vec![10, 10, 10, 20])
        );
        assert_eq!(
            expand(&[7, 8, 9], png::ColorType::Rgb),
            Some(vec![7, 8, 9, 255])
        );
        assert_eq!(
            expand(&[4], png::ColorType::Grayscale),
            Some(vec![4, 4, 4, 255])
        );
        assert_eq!(
            expand(&[1, 2, 3, 4], png::ColorType::Rgba),
            Some(vec![1, 2, 3, 4])
        );
        assert_eq!(expand(&[0], png::ColorType::Indexed), None);
    }

    #[test]
    fn a_size_mismatch_is_an_error_not_a_pass() {
        let mut r = rustim_render::CpuRenderer::new(8, 8);
        rustim_render::Renderer::render(&mut r, &rustim_render::DrawList::new(8, 8)).unwrap();
        let mut s = rustim_render::CpuRenderer::new(9, 8);
        rustim_render::Renderer::render(&mut s, &rustim_render::DrawList::new(9, 8)).unwrap();

        let dir = std::env::temp_dir().join("rustim-compare-test");
        std::fs::create_dir_all(&dir).unwrap();
        let (p, q) = (dir.join("8x8.png"), dir.join("9x8.png"));
        crate::write_png(&r, &p).unwrap();
        crate::write_png(&s, &q).unwrap();

        let err = compare_png_files(&p, &q, &Tolerance::default()).unwrap_err();
        assert!(err.to_string().contains("size mismatch"), "{err}");
        let _ = std::fs::remove_file(p);
        let _ = std::fs::remove_file(q);
    }

    #[test]
    fn the_default_background_is_the_palette_background() {
        let [r, g, b, _] = rustim_color::BG.components;
        let q = |v: f32| (v * 255.0 + 0.5) as u8;
        assert_eq!(
            Tolerance::default().background,
            [q(r), q(g), q(b)],
            "the comparator's default background must be rustim_color::BG"
        );
        assert_eq!(Tolerance::default().background, [0x12, 0x12, 0x12]);
    }
}
