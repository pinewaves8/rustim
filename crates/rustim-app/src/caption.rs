//! The caption band: the live region's text, drawn at the bottom of the frame.
//!
//! The engine already speaks through an accessibility live region — focus,
//! `animation ended`, the motion switch, a script's narration — but that reaches
//! exactly the people who have a screen reader attached. A caption is the same
//! sentence for everyone else: one line of text, shaped once, painted over the
//! scene's own draw list.
//!
//! # It is a screen layer, not a world object
//!
//! A caption must not move when the camera pans or thickens when it zooms. So it
//! is drawn **in device pixels**, under an identity transform, appended after
//! everything the scene emitted. [`Caption::push_ops`] deliberately does not take
//! a `Viewport` or a `Camera`: there is nothing for them to say, and a caller
//! that had them in hand might be tempted to compose them in.
//!
//! # The transform, which is the whole trick
//!
//! [`crate::text::text_ops`] computes `place = to_device · world ·
//! layout_to_local(w, h)`, where `layout_to_local` puts the shaper's y-down block
//! at the origin, centred, y-up. A caption wants layout point `(gx, gy)` at
//! device pixel `(x0 + gx, y0 + gy)` — no centring, no flip of its own. Passing
//!
//! ```text
//! to_device = S(1, -1)
//! world     = T(x0 + w/2, -(y0 + h/2))
//! ```
//!
//! composes to exactly that: the two y flips (`to_device`'s and
//! `layout_to_local`'s) cancel, and the width/height halves cancel the centring.
//! The net linear part is the **identity**, which matters beyond arithmetic:
//! `glifo` only takes its hinted path when the composite scale is positive and
//! uniform, and a net negative y scale would silently move the run to the
//! unhinted path. See `text.rs`'s module docs for the mirrored-glyph failure this
//! direction avoids.
//!
//! # What it shows
//!
//! Whatever the host hands it, which in this binary is the live region's text
//! (`A11yCore::live_text`) plus narration. So a listener pressing Tab sees
//! `圆，半径 2 的圆，面积 12.57` too, and a scripted lecture is legible with the
//! sound off. The band has no opinion about where the sentence came from.

use std::io;
use std::time::{Duration, Instant};

use rustim_color::{BG, FG};
use rustim_math::{Affine2, Rect, rect_path};
use rustim_render::{DrawList, DrawOp};
use rustim_text::{FontProvider, ShapedText, TextStyle};

use crate::text::push_text;

/// Side and bottom margin of the band, in device pixels.
const MARGIN: f64 = 24.0;
/// Breathing room between the bar's edge and the text.
const PADDING: f64 = 12.0;
/// The caption's cap size in device pixels.
///
/// `TextStyle::size` is world units per em, but this module passes
/// `unit_scale = 1.0`, so here one unit *is* one pixel. That is the point of a
/// screen layer: 24 px stays 24 px at every camera zoom and every window size.
const SIZE: f64 = 24.0;

/// How long a line stays on screen once nothing replaces it.
///
/// Not a constant: a long sentence is read more slowly than a short one, and a
/// fixed five seconds would cut off the narration and leave a one-word
/// announcement up for far too long. The reading-rate estimate is deliberately
/// generous (about eight characters a second) and clamped at both ends.
pub fn hold_seconds(text: &str) -> f64 {
    let chars = text.chars().count() as f64;
    (2.5 + chars * 0.12).clamp(3.0, 12.0)
}

/// The one-line description a run opens with.
///
/// Spoken and captioned when the first frame settles, so a session starts by
/// saying what it is. `objects` counts **mobjects**, not nodes: the root
/// container the accessibility tree synthesises is not an object.
pub fn scene_summary(scene: &str, objects: usize) -> String {
    format!("场景 {scene}，{objects} 个对象")
}

/// The bottom band, its shaped text, and when it was last changed.
pub struct Caption {
    provider: FontProvider,
    style: TextStyle,
    text: String,
    shaped: Option<ShapedText>,
    /// The frame size `shaped` was wrapped for. A resize can change the wrap
    /// width, and a stale layout would either overflow the frame or leave a
    /// line break the new width does not need.
    shaped_for: (u16, u16),
    shown_at: Instant,
}

impl Caption {
    /// Load the bundled font and prepare an empty band.
    ///
    /// The provider is built once and reused: it parses the 19 MB CJK face, and
    /// doing that per caption would make every announcement cost a font load.
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            provider: FontProvider::with_bundled()?,
            style: TextStyle {
                size: SIZE,
                ..TextStyle::default()
            },
            text: String::new(),
            shaped: None,
            shaped_for: (0, 0),
            shown_at: Instant::now(),
        })
    }

    /// Show this line from now on. Empty or all-whitespace text clears the band.
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.set_text_at(text, Instant::now());
    }

    /// [`Caption::set_text`] with an explicit clock, for tests.
    pub fn set_text_at(&mut self, text: impl Into<String>, now: Instant) {
        let text = text.into();
        if text.trim().is_empty() {
            self.clear();
            return;
        }
        if text == self.text {
            // The same sentence re-announced (Enter on the focused object, a
            // repeated summary). It is already on screen; hold it longer rather
            // than re-shape text that has not changed.
            self.shown_at = now;
            return;
        }
        self.text = text;
        self.shaped = None;
        self.shown_at = now;
    }

    /// Remove the line and its layout.
    pub fn clear(&mut self) {
        self.text.clear();
        self.shaped = None;
    }

    /// The line currently shown, or `""`.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Drop the line once it has been up for [`hold_seconds`].
    ///
    /// Returns whether anything was cleared, so a caller can tell whether the
    /// frame it is about to build still has text. Headless captures do not call
    /// this: `settle` runs in microseconds of wall time and a still has to keep
    /// its caption.
    pub fn expire(&mut self, now: Instant) -> bool {
        if self.text.is_empty() {
            return false;
        }
        let held = now.saturating_duration_since(self.shown_at);
        if held >= Duration::from_secs_f64(hold_seconds(&self.text)) {
            self.clear();
            return true;
        }
        false
    }

    /// Append the band and its text to a frame that has already been built.
    ///
    /// Does nothing when there is no text, when nothing could be shaped, or
    /// when the frame is too small to hold the band — an inverted or off-screen
    /// rectangle is worse than a missing caption, and a 0-pixel window is a
    /// legitimate (if brief) event rather than an error.
    pub fn push_ops(&mut self, list: &mut DrawList) {
        let (w, h) = (list.width, list.height);
        if self.text.is_empty() || w == 0 || h == 0 {
            return;
        }
        if self.shaped.is_none() || self.shaped_for != (w, h) {
            let max_width = (f64::from(w) - 2.0 * (MARGIN + PADDING)).max(1.0) as f32;
            // `unit_scale = 1.0`: caption sizes are pixels, not world units.
            self.shaped = Some(
                self.provider
                    .shape(&self.text, &self.style, 1.0, Some(max_width)),
            );
            self.shaped_for = (w, h);
        }
        let Some(shaped) = self.shaped.as_ref() else {
            return;
        };
        if shaped.runs.iter().all(|run| run.glyphs.is_empty()) {
            return;
        }

        let text_w = f64::from(shaped.width);
        let text_h = f64::from(shaped.height);
        let bar_w = (text_w + 2.0 * PADDING).min(f64::from(w));
        let bar_h = text_h + 2.0 * PADDING;
        let bar_x0 = (f64::from(w) - bar_w) / 2.0;
        let bar_y1 = f64::from(h) - MARGIN;
        let bar_y0 = bar_y1 - bar_h;
        if bar_y0 < 0.0 || bar_w <= 0.0 || bar_h <= 0.0 {
            return;
        }
        let bar = Rect::new(bar_x0, bar_y0, bar_x0 + bar_w, bar_y1);

        // Device space for the bar, then the text's own placement.
        list.push(DrawOp::SetTransform(Affine2::IDENTITY));
        list.push(DrawOp::SetPaint(BG));
        list.push(DrawOp::FillPath(rect_path(bar)));
        // Top-left of the layout box, in device pixels. The text is not centred
        // in the bar: a wrapped line starts at the same x every time, which is
        // what makes the band stable while the text changes length.
        let x0 = bar_x0 + PADDING;
        let y0 = bar_y0 + PADDING;
        let to_device = Affine2::scale_non_uniform(1.0, -1.0);
        let world = Affine2::translate((x0 + text_w / 2.0, -(y0 + text_h / 2.0)));
        push_text(list, shaped, world, to_device, FG);
    }
}

/// The ops [`Caption::push_ops`] would emit, for a test that wants geometry.
#[cfg(test)]
fn ops_for(caption: &mut Caption, width: u16, height: u16) -> Vec<DrawOp> {
    let mut list = DrawList::new(width, height);
    caption.push_ops(&mut list);
    list.ops
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustim_math::{Point, Shape};

    fn caption(text: &str) -> Caption {
        let mut caption = Caption::new().expect("assets/fonts");
        caption.set_text(text);
        caption
    }

    fn bar_of(ops: &[DrawOp]) -> Rect {
        ops.iter()
            .find_map(|op| match op {
                DrawOp::FillPath(path) => Some(path.bounding_box()),
                _ => None,
            })
            .expect("the band paints a bar")
    }

    /// The run transform: the last `SetTransform` in the list.
    fn place_of(ops: &[DrawOp]) -> Affine2 {
        ops.iter()
            .rev()
            .find_map(|op| match op {
                DrawOp::SetTransform(a) => Some(*a),
                _ => None,
            })
            .expect("the text is placed by a transform")
    }

    #[test]
    fn the_band_sits_at_the_bottom_and_inside_the_frame() {
        let mut caption = caption("你好，世界");
        let ops = ops_for(&mut caption, 1280, 720);
        let bar = bar_of(&ops);
        assert_eq!(bar.y1, 720.0 - MARGIN);
        assert!((bar.center().x - 640.0).abs() < 1e-6, "{bar:?}");
        assert!(bar.x0 >= 0.0 && bar.x1 <= 1280.0, "{bar:?}");
        assert!(bar.y0 > 0.0, "{bar:?}");
    }

    /// The coordinate claim in the module docs: layout `(0, 0)` is the text
    /// block's top-left pixel and `(0, h)` is its bottom-left, *not* mirrored.
    #[test]
    fn the_layout_origin_lands_at_the_top_left_pixel() {
        let mut caption = caption("你好，世界");
        let (w, h) = (1280, 720);
        let ops = ops_for(&mut caption, w, h);
        let bar = bar_of(&ops);
        let shaped = caption.shaped.as_ref().unwrap();
        let place = place_of(&ops);
        let x0 = bar.x0 + PADDING;
        let y0 = bar.y0 + PADDING;

        let top_left = place * Point::new(0.0, 0.0);
        assert!((top_left.x - x0).abs() < 1e-6, "{top_left:?}");
        assert!((top_left.y - y0).abs() < 1e-6, "{top_left:?}");

        let bottom_left = place * Point::new(0.0, f64::from(shaped.height));
        assert!((bottom_left.x - x0).abs() < 1e-6, "{bottom_left:?}");
        assert!(
            (bottom_left.y - (y0 + f64::from(shaped.height))).abs() < 1e-6,
            "{bottom_left:?}"
        );

        // Positive uniform scale, so `glifo` keeps the hinted path.
        let [a, b, c, d, _, _] = place.as_coeffs();
        assert_eq!((a, b, c, d), (1.0, 0.0, 0.0, 1.0), "{place:?}");
    }

    #[test]
    fn the_text_block_fits_inside_the_bar() {
        let mut caption = caption("你好，世界");
        let ops = ops_for(&mut caption, 1280, 720);
        let bar = bar_of(&ops);
        let shaped = caption.shaped.as_ref().unwrap();
        assert!(
            f64::from(shaped.width) + 2.0 * PADDING <= bar.width() + 1e-6,
            "{} vs {bar:?}",
            shaped.width
        );
        assert!(
            f64::from(shaped.height) + 2.0 * PADDING <= bar.height() + 1e-6,
            "{} vs {bar:?}",
            shaped.height
        );
    }

    #[test]
    fn a_long_line_wraps_within_the_frame() {
        let mut caption = caption(
            "这是一段很长的旁白，用来验证字幕在窄窗口里会换行而不会跑出画面之外，也不能把文字画到边框外面。",
        );
        let ops = ops_for(&mut caption, 480, 320);
        let shaped = caption.shaped.as_ref().unwrap();
        assert!(
            shaped.width <= 480.0 - 2.0 * (MARGIN as f32 + PADDING as f32) + 1e-3,
            "{}",
            shaped.width
        );
        assert!(shaped.height > 40.0, "wrapped: {} px tall", shaped.height);
        let bar = bar_of(&ops);
        assert!(bar.x0 >= 0.0 && bar.x1 <= 480.0, "{bar:?}");
        assert!(bar.y0 >= 0.0, "{bar:?}");
    }

    #[test]
    fn an_empty_line_draws_nothing() {
        let mut caption = caption("");
        assert!(ops_for(&mut caption, 1280, 720).is_empty());

        let mut caption = Caption::new().expect("assets/fonts");
        caption.set_text("  \n ");
        assert!(ops_for(&mut caption, 1280, 720).is_empty());
        assert_eq!(caption.text(), "");
    }

    #[test]
    fn the_same_text_is_not_reshaped_but_holds_longer() {
        let mut caption = Caption::new().expect("assets/fonts");
        let then = Instant::now();
        caption.set_text_at("同一句话", then);
        let _ = ops_for(&mut caption, 1280, 720);
        let shaped = caption.shaped.as_ref().unwrap() as *const ShapedText;

        let later = then + Duration::from_secs(1);
        caption.set_text_at("同一句话", later);
        assert_eq!(
            caption.shaped.as_ref().unwrap() as *const ShapedText,
            shaped,
            "the same sentence keeps its layout"
        );
        assert_eq!(caption.shown_at, later, "but the clock restarts");
    }

    #[test]
    fn a_new_text_is_re_wrapped_for_the_new_width() {
        let mut caption = caption("换行宽度取决于帧宽，帧宽变了就要重排。");
        let _ = ops_for(&mut caption, 1280, 720);
        let wide = caption.shaped.as_ref().unwrap().height;

        let _ = ops_for(&mut caption, 360, 720);
        let narrow = caption.shaped.as_ref().unwrap().height;
        assert!(narrow > wide, "{narrow} should wrap more than {wide}");
    }

    #[test]
    fn a_line_expires_after_its_reading_time() {
        let mut caption = Caption::new().expect("assets/fonts");
        let then = Instant::now();
        caption.set_text_at("短句", then);

        let hold = Duration::from_secs_f64(hold_seconds("短句"));
        assert!(!caption.expire(then + hold - Duration::from_millis(1)));
        assert_eq!(caption.text(), "短句");

        assert!(caption.expire(then + hold));
        assert_eq!(caption.text(), "");
        assert!(caption.shaped.is_none(), "layout cleared too");
        assert!(!caption.expire(then + hold), "nothing left to expire");
    }

    #[test]
    fn hold_time_grows_with_the_line_and_is_clamped() {
        assert_eq!(hold_seconds(""), 3.0);
        assert_eq!(hold_seconds("短"), 3.0);
        let medium = hold_seconds("这是一句中等长度的旁白，大约二十来个字");
        assert!(medium > 3.0 && medium < 12.0, "{medium}");
        assert_eq!(hold_seconds(&"长".repeat(500)), 12.0);
    }

    #[test]
    fn the_summary_counts_objects_in_the_scenes_words() {
        assert_eq!(
            scene_summary("pythagorean", 4),
            "场景 pythagorean，4 个对象"
        );
        assert_eq!(scene_summary("blank", 0), "场景 blank，0 个对象");
    }

    /// A frame shorter than the margin + bar is skipped rather than painted
    /// upside down.
    #[test]
    fn a_frame_too_small_for_the_band_draws_nothing() {
        let mut caption = caption("你好");
        assert!(
            ops_for(&mut caption, 1280, 12).is_empty(),
            "a 12 px tall frame cannot hold a 24 px caption"
        );
    }
}
