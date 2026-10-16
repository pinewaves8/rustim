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
//! codegen — the three scenes here were measured byte-identical between debug and
//! release on the floor machine, but that is luck about this build, not a
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
const SCENES: [&str; 4] = ["pythagorean", "primitives", "composite", "text"];

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
fn render(scene: &str) -> PathBuf {
    let out = artifacts_dir().join(format!("{scene}-{}.png", platform()));
    std::fs::create_dir_all(out.parent().expect("artifacts dir has a parent"))
        .expect("create artifacts dir");

    let output = Command::new(BIN)
        .arg("--scene")
        .arg(scene)
        .arg("--png")
        .arg(&out)
        .output()
        .expect("run rustim-app");
    assert!(
        output.status.success(),
        "`rustim-app --scene {scene} --png` failed with {}:\n{}{}",
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
