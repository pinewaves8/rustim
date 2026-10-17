//! Golden-image regression tests.
//!
//! Each scene is rendered by **the real binary**, through the same argument
//! parsing and the same headless path a user would get, and the result is
//! compared pixel by pixel against a checked-in baseline. Rendering inside the
//! test — building a `SceneContext` by hand — would test a near-copy of the
//! pipeline and would not notice a regression in argument handling, in the
//! still-frame timing, or in the viewport the app actually constructs.
//!
//! # Baselines are per platform
//!
//! `crates/rustim-app/tests/goldens/<scene>/<os>-<arch>.png`. `vello_cpu`
//! rasterises in f32, and NEON and SSE/AVX do not round identically, so a
//! single shared baseline would fail everywhere it was not generated. §10.2(3)
//! of `方案建议.md` settles this: per-platform baselines are a consequence of the
//! cross-platform requirement, not a workaround.
//!
//! That means the first CI run on a platform without a baseline **fails**, by
//! design — silently skipping would let a whole platform go unchecked, which is
//! the failure mode §7.2 picks out in other projects. The failing run uploads
//! its renders as an artifact, and someone commits them once.
//!
//! # Regenerating
//!
//! ```text
//! BLESS=1 cargo test -p rustim-app --test goldens
//! ```
//!
//! Blessing writes whatever the current build produces, so it is only correct
//! when the change is meant. It should also be run in the same profile the suite
//! is run in, since the render and the baseline have to come from the same float
//! codegen — all eight scenes have been measured **byte-identical between debug
//! and release** on the floor machine, but that is luck about this build, not a
//! property of the pipeline.
//!
//! # What is *not* here
//!
//! The comparison against real Manim output. That needs the Python venv, so it
//! cannot run in CI; it stays a local, on-demand check (`crates/rustim-math`
//! carries the numerical half of the same idea, from fixtures generated once).

use std::path::{Path, PathBuf};
use std::process::Command;

use rustim_export::{Tolerance, compare_png_files};

/// The binary under test, built by cargo for this test run.
const BIN: &str = env!("CARGO_BIN_EXE_rustim-app");

/// Every scene the suite pins, in the order `--help` lists them.
///
/// Kept in step with the binary by
/// [`every_scene_the_binary_offers_has_a_baseline`]: a scene added to the
/// registry without a golden fails the suite rather than quietly going
/// untested, and a scene removed from it fails too, rather than leaving a stale
/// baseline behind.
const SCENES: [&str; 8] = [
    "pythagorean",
    "primitives",
    "composite",
    "text",
    "formula",
    "transform",
    "rotate",
    "axes",
];

fn platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

fn baselines_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/goldens")
}

fn baseline_path(scene: &str) -> PathBuf {
    baselines_dir()
        .join(scene)
        .join(format!("{}.png", platform()))
}

/// Where renders are kept so a failing CI run has something to upload.
///
/// Under `target/`, so it is never committed and never stale: a baseline is the
/// only image in this repository that a human decided on.
fn artifacts_dir() -> PathBuf {
    let target = std::env::var_os("CARGO_TARGET_DIR").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"),
        PathBuf::from,
    );
    target.join("golden-artifacts")
}

/// Render one scene to a file and return where it landed.
///
/// **`--motion full` is passed explicitly, and it is not decoration.** Without it
/// the motion mode comes from the machine's desktop settings, which would make a
/// baseline depend on something the suite does not control — the same reason the
/// `text` scene ships its font rather than asking the system for one. That
/// `--motion reduced` renders the same bytes is a property to *assert*
/// (`reduced_motion_leaves_the_settled_frame_identical`), not one to rely on for
/// what a baseline means.
fn render(scene: &str) -> PathBuf {
    render_at(scene, "full", "golden")
}

/// Render one scene at a chosen motion mode.
///
/// **`tag` names the caller, and it is not decoration either.** Two tests can ask
/// for the same `(scene, motion)` — the golden test and the reduced-motion
/// comparison, which renders the full mode a second time — and cargo runs tests in
/// parallel threads. Without the tag they share an artifact path, and the reader
/// intermittently gets a file the other thread has just truncated:
/// `transform-full-linux-aarch64.png: unexpected end of file`, from a test whose
/// own two renders were perfectly fine.
fn render_at(scene: &str, motion: &str, tag: &str) -> PathBuf {
    let out = artifacts_dir().join(format!("{scene}-{motion}-{tag}-{}.png", platform()));
    std::fs::create_dir_all(out.parent().expect("artifacts dir has a parent"))
        .expect("create artifacts dir");

    let output = Command::new(BIN)
        .arg("--scene")
        .arg(scene)
        .arg("--motion")
        .arg(motion)
        .arg("--png")
        .arg(&out)
        .output()
        .expect("run rustim-app");
    assert!(
        output.status.success(),
        "`rustim-app --scene {scene} --motion {motion} --png` failed with {}:\n{}{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        out.exists(),
        "the binary reported success but wrote no file at {}",
        out.display()
    );
    out
}

fn check(scene: &str) {
    let rendered = render(scene);
    let baseline = baseline_path(scene);

    if std::env::var_os("BLESS").is_some() {
        std::fs::create_dir_all(baseline.parent().expect("baseline has a parent"))
            .expect("create baseline dir");
        std::fs::copy(&rendered, &baseline).expect("write baseline");
        println!("blessed {}", baseline.display());
        return;
    }

    assert!(
        baseline.exists(),
        "no golden baseline for `{scene}` on {}.\n  \
         expected   {}\n  rendered   {}\n  \
         run `BLESS=1 cargo test -p rustim-app --test goldens`, review the image, and commit it",
        platform(),
        baseline.display(),
        rendered.display(),
    );

    let tolerance = Tolerance::default();
    let diff = compare_png_files(&baseline, &rendered, &tolerance).unwrap_or_else(|e| {
        panic!(
            "comparing {} against {}: {e}",
            baseline.display(),
            rendered.display()
        )
    });
    assert!(
        diff.passes(&tolerance),
        "`{scene}` no longer matches its baseline {}.\n{}",
        baseline.display(),
        diff.report(&tolerance),
    );
}

#[test]
fn pythagorean_matches_its_baseline() {
    check("pythagorean");
}

#[test]
fn primitives_matches_its_baseline() {
    check("primitives");
}

#[test]
fn composite_matches_its_baseline() {
    check("composite");
}

/// Text is the one scene whose baseline depends on a file in this repository.
///
/// The glyphs come from `assets/fonts`, loaded at run time rather than compiled
/// in, and that is what makes the baseline reproducible on three platforms: no
/// system font is consulted. It also makes an accidental loss of that directory
/// visible here rather than only to a user — the render comes out empty and
/// mismatches the committed image instead of the suite passing on a blank scene.
#[test]
fn text_matches_its_baseline() {
    check("text");
}

/// The formula scene is the one whose geometry comes from a *third-party*
/// typesetting engine rather than from this repository.
///
/// That is what makes it worth a baseline rather than a unit test. The glyph
/// paths themselves are checked in `rustim-text` — orientation, size, the shape
/// of the point array — but four things can only be seen in an image: that a
/// fraction's rule (a *stroke* in Typst's frame, expanded here into a filled
/// quad) is drawn at all, that it sits between the numerals, that a letter with
/// a counter keeps its hole rather than filling in solid, and that a Chinese
/// character is typeset from the bundled face rather than from the font Typst
/// would have picked. Each of those has been wrong during this step, and each
/// was invisible to every test that does not rasterise.
#[test]
fn formula_matches_its_baseline() {
    check("formula");
}

/// The transform scene is the only one whose still is the *end* of an animation
/// rather than a static arrangement.
///
/// It is here because that is the one thing no unit test can see. The morph's
/// shape is pinned in `rustim-anim` against hand-computed midpoints; what an
/// image adds is that the settled frame is a circle and a triangle and not the
/// triangle and square it started from — i.e. that the alignment ran, that both
/// padded and ragged paths rasterise, and that the style lerp arrived. A refused
/// alignment no-ops and leaves the start shapes on screen, which looks like a
/// plausible picture and is exactly the failure a test that never rasterises
/// cannot catch.
#[test]
fn transform_matches_its_baseline() {
    check("transform");
}

/// The rotate scene is the only one where the third beat makes an object
/// *disappear*, and the still is the evidence that it did.
///
/// What an image adds over the unit tests: `FadeOut`'s opacity ramp is pinned in
/// `rustim-anim`, and its `finish` hook is pinned through the scheduler — but only
/// a render shows that the removal reached the mutating tree the draw walk reads,
/// that a **rotated arrow** rasterises (the arrow is the one shape whose outline
/// the rasteriser has never been asked for at an angle), and that the circle grew
/// about *its own* centre: a scale about the origin instead would have carried the
/// circle across the frame, and no unit test asserts where the scene put it.
#[test]
fn rotate_matches_its_baseline() {
    check("rotate");
}

/// The axes scene is the only one with a coordinate system, a sampled curve, a
/// number line **and** a non-default camera.
///
/// What only an image can say about it: that a plot is a ribbon and not the
/// area under the curve (a filled polyline is a plausible-looking wrong
/// picture), that the welded axes and ticks are solid with empty quadrants,
/// that tick labels shaped through the shaper land on the ticks the geometry
/// placed, and — because the still is the end of a camera beat — that the camera
/// reaches **all three** of the projections: fills, glyph runs and the
/// accessibility bounds. A camera that moved the tree walk and not the text
/// would leave the labels behind and no unit test compares them to the fills.
#[test]
fn axes_matches_its_baseline() {
    check("axes");
}

/// The camera beat under reduced motion: the beat is not interpolated, so the
/// camera lands on its destination in one step — and the settled frame has to be
/// the same one a full run ends on.
///
/// This is the reduced-motion identity stated over the one animation that writes
/// no object: `CameraMove`'s `interpolate(1.0)` is the only thing that has to
/// agree with the full run's last frame, and the label glyphs are what make a
/// half-moved camera visible rather than merely a different crop.
#[test]
fn reduced_motion_leaves_the_axes_scene_identical() {
    let full = render_at("axes", "full", "vs-reduced");
    let reduced = render_at("axes", "reduced", "vs-reduced");

    let tolerance = Tolerance::default();
    let diff = compare_png_files(&full, &reduced, &tolerance).unwrap_or_else(|e| {
        panic!(
            "comparing {} against {}: {e}",
            full.display(),
            reduced.display()
        )
    });
    assert_eq!(
        diff.max,
        0,
        "the settled axes frame moved under reduced motion — the camera beat's \
         end state must not depend on which branch played it.\n{}",
        diff.report(&tolerance),
    );
    assert!(
        diff.diff_bbox.is_none(),
        "no pixel may differ at all, not merely none by more than the tolerance"
    );
}

/// The same identity as the transform scene's, over a scene that exercises a
/// *different* `finish` call site.
///
/// `Sequence`'s reduced branch never hands a beat to the scheduler, so a `FadeOut`
/// under `--motion reduced` is finished there rather than in
/// `AnimationScheduler::update`. Both routes have to end with the square gone and
/// the arrow at the same angle, and comparing the two stills is what says so.
#[test]
fn reduced_motion_leaves_the_rotate_scene_identical() {
    let full = render_at("rotate", "full", "vs-reduced");
    let reduced = render_at("rotate", "reduced", "vs-reduced");

    let tolerance = Tolerance::default();
    let diff = compare_png_files(&full, &reduced, &tolerance).unwrap_or_else(|e| {
        panic!(
            "comparing {} against {}: {e}",
            full.display(),
            reduced.display()
        )
    });
    assert_eq!(
        diff.max,
        0,
        "the settled rotate frame moved under reduced motion — the reduced \
         branch's `finish` and the scheduler's must leave the tree in the same \
         state.\n{}",
        diff.report(&tolerance),
    );
    assert!(
        diff.diff_bbox.is_none(),
        "no pixel may differ at all, not merely none by more than the tolerance"
    );
}

/// **The only assertion in the workspace that can see a `finish` that never ran.**
///
/// The rotate scene fades a square out as its third beat; a `FadeOut` that ramps the
/// opacity to zero but never removes its node leaves a fully transparent mobject in
/// the tree, and **no image can tell the difference** — it draws nothing either way.
/// The accessibility tree can: the node is still there to be reached by Tab and
/// announced by a screen reader, which for a vanished object is a lie.
///
/// Counting is deliberately on the accessibility projection rather than on
/// `SceneTree::root_ids` (which `rustim-anim`'s own tests already assert), because
/// that is the *consumer* that would be wrong; and it is done by running the real
/// binary, because the count the module under test reports about itself is the same
/// one that would be wrong about itself.
#[test]
fn the_rotate_scene_leaves_two_objects_behind() {
    let output = Command::new(BIN)
        .args(["--scene", "rotate", "--inspect"])
        .output()
        .expect("run --inspect");
    assert!(
        output.status.success(),
        "`--scene rotate --inspect` failed with {}:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );

    let tree: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("--inspect must emit JSON");
    let nodes = tree["nodes"].as_array().expect("nodes must be an array");
    let root = &tree["root"];

    // The root is one of the nodes, so two objects is three entries.
    assert_eq!(
        nodes.len(),
        3,
        "expected the arrow and the circle, and nothing else, in:\n{}",
        serde_json::to_string_pretty(&tree).unwrap_or_default(),
    );

    let root_node = nodes
        .iter()
        .find(|n| &n["id"] == root)
        .expect("the root id must name a node");
    let children = root_node["children"]
        .as_array()
        .expect("children must be an array");
    assert_eq!(children.len(), 2, "the root group has two members");

    // The square was the one carrying that label, so its absence is the claim; the
    // other two labels are asserted too, so a scene edit that faded out the wrong
    // object cannot pass by keeping the count right.
    let names: Vec<&str> = nodes
        .iter()
        .filter_map(|n| n["name"].as_str())
        .filter(|n| !n.is_empty())
        .collect();
    assert_eq!(
        names,
        ["位移", "圆的面积"],
        "the square that faded out is the one that must be missing"
    );
}

/// The scene list here and the scene registry in the binary must not drift.
///
/// Under `--motion reduced` a beat is not interpolated: it lands on its end state
/// and holds for its own length (`rustim_anim::Sequence::start_next`). The still
/// is taken after `STILL_SECONDS` of virtual time, long after the script drains
/// either way — and a *finished* animation is its end state, which is what makes
/// this an identity rather than a resemblance.
///
/// So the assertion is `max == 0`, stronger than the golden suite's tolerance:
/// not one channel of one pixel may differ. The scene is `transform` because it is
/// the only one whose beats move geometry through a real morph; a fade's end state
/// is a scalar and would agree for trivial reasons.
#[test]
fn reduced_motion_leaves_the_settled_frame_identical() {
    let full = render_at("transform", "full", "vs-reduced");
    let reduced = render_at("transform", "reduced", "vs-reduced");

    let tolerance = Tolerance::default();
    let diff = compare_png_files(&full, &reduced, &tolerance).unwrap_or_else(|e| {
        panic!(
            "comparing {} against {}: {e}",
            full.display(),
            reduced.display()
        )
    });
    assert_eq!(
        diff.max,
        0,
        "the settled frame moved under reduced motion — that means some \
         animation's `interpolate(1.0)` is not the state a full run ends on, \
         which is a bug in that animation rather than in the mode.\n{}",
        diff.report(&tolerance),
    );
    assert!(
        diff.diff_bbox.is_none(),
        "no pixel may differ at all, not merely none by more than the tolerance"
    );
}

/// The scene list here and the scene registry in the binary must not drift.
///
/// A golden suite is only as good as its coverage, and coverage that depends on
/// someone remembering to add a test is coverage that quietly decays. This reads
/// the binary's own `--help` output rather than importing the registry, which is
/// possible because `rustim-app` is a binary crate.
#[test]
fn every_scene_the_binary_offers_has_a_baseline() {
    let output = Command::new(BIN)
        .arg("--help")
        .output()
        .expect("run --help");
    assert!(output.status.success(), "--help must succeed");
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    let listed: Vec<&str> = text
        .lines()
        .find_map(|l| l.strip_prefix("scenes: "))
        .expect("--help must list the scenes")
        .split(", ")
        .collect();

    assert_eq!(
        listed, SCENES,
        "--help and the golden suite disagree about which scenes exist.\n\
         add the new scene to SCENES (with a test), or remove the stale entry"
    );
}

/// The baseline a run looks for names this platform, arch included.
#[test]
fn the_baseline_path_is_per_platform() {
    let p = baseline_path("pythagorean");
    let name = p.file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(name, format!("{}.png", platform()));
    assert!(
        name.contains(std::env::consts::ARCH),
        "NEON and SSE do not rasterise identically, so the arch has to be in the name: {name}"
    );
}
