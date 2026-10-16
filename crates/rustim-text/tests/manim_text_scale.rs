//! Pins [`MANIM_FONT_SIZE_UNITS`] to a measurement of real Manim output.
//!
//! The constant answers one question — *how many world units of em does Manim's
//! `font_size = 48` mean?* — and `方案建议.md` §2.4 offers two numbers that look
//! like the answer and are not: `TEXT2SVG_ADJUSTMENT_FACTOR` (4.8) and
//! `TEXT_MOB_SCALE_FACTOR` (0.05) belong to Manim's SVG pipeline, where they
//! describe how an SVG user unit is scaled *after* `svg2paths` has produced one.
//! `48 / 4.8 * 0.05` is `0.5`, which is where this crate's placeholder came from,
//! and it is wrong by a third: the expression drops the points-to-pixels step in
//! between, so the two constants cannot simply be multiplied.
//!
//! # How the distance is crossed
//!
//! Manim's `Text.height` is the **ink** box, not a line box — `Text(" ").height`
//! is `0`, and `Text("x")` is an x-height while `Text("H")` is a cap height — and
//! this engine's shaper reports a line box. The two numbers are therefore not
//! comparable, and comparing them anyway is what produced the `0.5`.
//!
//! The bridge is a quantity that is *not* an ink measurement on this side:
//! **cap height is a metric in the font's own `OS/2` table**, the ink box of a
//! straight-topped capital *is* its cap height, and the fixture records Manim's
//! ink for seven such letters. So
//!
//! ```text
//! em(world units) = Manim's ink height for "H" / (cap_height / units_per_em)
//! ```
//!
//! Both inputs are measured — one by Manim, one out of the font file this
//! repository bundles — and nothing in between is a re-derivation of Manim's
//! SVG constants.
//!
//! # Why seven letters, and why the same face
//!
//! A height in world units is a property of the face as much as of the size, so
//! the script renders in `Noto Sans CJK SC` — the same `NotoSansCJK-Regular.ttc`
//! this repository bundles — rather than in Manim's fontconfig default. And seven
//! letters rather than one, because a cap height is only a ruler if the letters
//! actually have the cap height the table claims: a letter whose outline
//! overshoots, or whose terminals round off past the metric, would silently
//! corrupt the constant. They share a cap height and nothing else, so agreement
//! across all seven is the evidence, and [`cap_letters_agree`] is the test that
//! reads it.

use std::path::PathBuf;

use rustim_text::{FontProvider, MANIM_FONT_SIZE_UNITS, TextStyle};
use serde_json::Value;

/// How far the two derivations may sit from the constant.
///
/// Not tight, and deliberately so: this compares two independent renderers
/// through two independent stacks — Manim's Pango/SVG path against this engine's
/// parley/glifo one — so the last digit is a coincidence, not a property. Manim
/// also snaps SVG coordinates to a 1/64 grid, which alone is worth about a tenth
/// of a percent on this string. What the test is for is catching a *wrong order
/// of magnitude*: `0.5`, `4.8`, `0.05`, a scale applied twice, or a viewport's
/// pixel scale left in the number. A half of a percent catches all of those and
/// does not fire on fontconfig choosing a subtly different instance of a file
/// that is, after all, the same file.
const RELATIVE_TOLERANCE: f64 = 5e-3;

/// Manim's default `font_size`, and the size the fixture's cap letters were
/// rendered at.
const MANIM_DEFAULT_FONT_SIZE: f64 = 48.0;

fn fixture() -> Value {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/manim-text-scale.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    serde_json::from_str(&text).expect("the fixture is JSON")
}

/// Manim's ink box for the reference string at `font_size`, in world units.
fn manim_ink(fixture: &Value, font_size: f64) -> (f64, f64) {
    let row = fixture["measurements"]
        .as_array()
        .expect("measurements")
        .iter()
        .find(|m| m["font_size"].as_f64() == Some(font_size))
        .unwrap_or_else(|| panic!("no measurement at font_size {font_size}"));
    (
        row["height"].as_f64().expect("height"),
        row["width"].as_f64().expect("width"),
    )
}

/// The bundled face's cap height as a fraction of its em, from its own `OS/2`
/// table.
///
/// Read out of the file rather than taken from documentation, because the whole
/// point is that this number and Manim's measurement come from the same font.
fn cap_height_fraction() -> f64 {
    use skrifa::instance::{LocationRef, Size};
    use skrifa::{FontRef, MetadataProvider};

    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts");
    let file = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "ttc" || e == "ttf"))
        .expect("the bundled font file");
    let data = std::fs::read(&file).expect("reading the bundled font");

    let font = FontRef::from_index(&data, 0).expect("face 0 of the bundled collection");
    let metrics = font.metrics(Size::unscaled(), LocationRef::default());
    let cap = metrics
        .cap_height
        .expect("the bundled face has an OS/2 cap height");
    f64::from(cap) / f64::from(metrics.units_per_em)
}

/// This engine's measurement of the same string at the same em size.
fn rustim(font_size_units: f64) -> rustim_text::ShapedText {
    let mut provider =
        FontProvider::with_bundled().expect("assets/fonts must be in the repository");
    let style = TextStyle {
        size: font_size_units,
        ..Default::default()
    };
    provider.shape(fixture()["text"].as_str().expect("text"), &style, 1.0, None)
}

/// The em size, in world units, that Manim's `font_size = 48` corresponds to.
fn measured_em() -> (f64, f64) {
    let f = fixture();
    let cap_row = f["cap_letters"]
        .as_array()
        .expect("cap_letters")
        .iter()
        .find(|c| c["letter"].as_str() == Some("H"))
        .expect("the fixture must record an `H`");
    let ink = cap_row["height"].as_f64().expect("height");
    let em = ink / cap_height_fraction();
    (em, ink)
}

#[test]
fn the_constant_is_what_manim_measures() {
    let f = fixture();
    assert_eq!(
        f["font"].as_str(),
        Some("Noto Sans CJK SC"),
        "the fixture must have been rendered in the face this repository bundles"
    );
    let (em, ink) = measured_em();
    let relative_error = (em - MANIM_FONT_SIZE_UNITS).abs() / MANIM_FONT_SIZE_UNITS;
    assert!(
        relative_error <= RELATIVE_TOLERANCE,
        "MANIM_FONT_SIZE_UNITS is {MANIM_FONT_SIZE_UNITS}, but Manim's font_size = \
         {MANIM_DEFAULT_FONT_SIZE} measures {em:.6} world units of em: an `H` ink box of \
         {ink} world units over a cap height of {:.6} em.\n  \
         rerun `tools/manim-fixtures/text_scale.py` if Manim or the font changed",
        cap_height_fraction(),
    );
}

/// The cap height is only a ruler if the letters really do have it.
#[test]
fn cap_letters_agree() {
    let f = fixture();
    let caps = f["cap_letters"].as_array().expect("cap_letters");
    assert!(caps.len() >= 3, "one letter is not evidence");
    let heights: Vec<f64> = caps
        .iter()
        .map(|c| c["height"].as_f64().expect("height"))
        .collect();
    let (min, max) = heights
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &h| {
            (lo.min(h), hi.max(h))
        });
    assert!(
        (max - min) / max <= RELATIVE_TOLERANCE,
        "these letters are all supposed to be cap height and are not: {:?}",
        caps.iter()
            .map(|c| (
                c["letter"].as_str().unwrap_or_default(),
                c["height"].as_f64().unwrap_or_default()
            ))
            .collect::<Vec<_>>()
    );
}

/// The relation really is linear, so that one ratio is enough to pin the constant.
#[test]
fn manim_scales_text_linearly_with_font_size() {
    let f = fixture();
    let (height_48, width_48) = manim_ink(&f, MANIM_DEFAULT_FONT_SIZE);
    for font_size in [24.0, 96.0] {
        let (height, width) = manim_ink(&f, font_size);
        let factor = font_size / MANIM_DEFAULT_FONT_SIZE;
        assert!(
            (height - height_48 * factor).abs() / (height_48 * factor) < RELATIVE_TOLERANCE,
            "height at font_size {font_size} is not {factor}x the height at 48: \
             {height} vs {}",
            height_48 * factor,
        );
        assert!(
            (width - width_48 * factor).abs() / (width_48 * factor) < RELATIVE_TOLERANCE,
            "width at font_size {font_size} is not {factor}x the width at 48: {width} vs {}",
            width_48 * factor
        );
    }
}

/// This engine's text is linear in the em size too — the other half of the ratio.
#[test]
fn this_engine_scales_text_linearly_with_the_em_size() {
    let base = rustim(1.0);
    for size in [0.125, 0.25, 2.0] {
        let scaled = rustim(size);
        let ratio = f64::from(scaled.height) / f64::from(base.height);
        assert!(
            (ratio - size).abs() / size < RELATIVE_TOLERANCE,
            "height at {size} world units per em is {ratio}x the height at 1.0"
        );
    }
}

/// The default `TextStyle` really is the constant, and it really does draw.
///
/// The em derivation above is arithmetic over two measurements; this is the test
/// that the *engine* is built on top of it. It also checks the scale a second
/// time, through a quantity that shares no code with the cap height: the advance
/// width of the reference string in ems. Manim reports an ink width and this
/// engine an advance, so the two differ by the side bearings — which for a string
/// whose first and last glyphs are `X` and a CJK ideograph is a couple of percent,
/// not the third that a wrong constant is worth.
#[test]
fn the_default_style_draws_at_the_measured_em() {
    let f = fixture();
    let (_, manim_width) = manim_ink(&f, MANIM_DEFAULT_FONT_SIZE);
    let (em, _) = measured_em();
    assert!(
        (em - MANIM_FONT_SIZE_UNITS).abs() / MANIM_FONT_SIZE_UNITS <= RELATIVE_TOLERANCE,
        "the em this test is built on is not the constant"
    );

    let shaped = rustim(MANIM_FONT_SIZE_UNITS);
    assert!(
        !shaped.is_empty(),
        "the default style must draw: a scene with no text is not a match"
    );
    let run = shaped.runs.first().expect("a glyph run");
    // f32 on the run's side, so this is `f32::EPSILON`-sized slack and not a
    // rounding of the constant.
    assert!(
        (f64::from(run.size) - MANIM_FONT_SIZE_UNITS).abs() < 1e-6,
        "the run is at {} but the default em is {MANIM_FONT_SIZE_UNITS}",
        run.size
    );

    // Both sides as a fraction of the em, which is the only form in which an ink
    // width and an advance width can be compared.
    let manim_width_in_em = manim_width / em;
    let ours_in_em = f64::from(shaped.width) / MANIM_FONT_SIZE_UNITS;
    let error = (ours_in_em - manim_width_in_em).abs() / manim_width_in_em;
    assert!(
        error <= 0.05,
        "`{}` is {ours_in_em:.4} em wide here and {manim_width_in_em:.4} em in Manim \
         ({:.2}% out)",
        f["text"].as_str().unwrap_or_default(),
        error * 100.0,
    );
}
