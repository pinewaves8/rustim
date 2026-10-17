//! `--captions` is pixels, and the one thing a pixel test can prove is *where*
//! and *whether* it drew. The wording and geometry are pinned in the library
//! (`caption.rs`); this file runs the real binary because the flag, the headless
//! still path and the narration drain are only reachable through it.
//!
//! The band is compared against the plain frame with `bbox_must_match: false`
//! on purpose: the content box legitimately grows when a caption is added, and
//! the default tolerance would fail for a reason that has nothing to do with the
//! caption. What is asserted instead is the thing that matters — every changed
//! pixel lies in the bottom band.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rustim_export::compare::{Tolerance, compare_png_files};

const BIN: &str = env!("CARGO_BIN_EXE_rustim-app");
const HEIGHT: u32 = 720;

fn artifacts() -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/caption-artifacts");
    std::fs::create_dir_all(&dir).expect("a writable target directory");
    dir
}

fn render(tag: &str, extra: &[&str]) -> PathBuf {
    let out = artifacts().join(format!("{tag}.png"));
    let mut args: Vec<String> = vec![
        "--scene".into(),
        "pythagorean".into(),
        "--png".into(),
        out.to_string_lossy().into_owned(),
    ];
    args.extend(extra.iter().map(|s| (*s).to_owned()));
    let output = Command::new(BIN)
        .args(&args)
        .stdin(Stdio::null())
        .output()
        .expect("the binary under test is built");
    assert!(
        output.status.success(),
        "{tag} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    out
}

fn render_script(tag: &str, lines: &str) -> PathBuf {
    let script = artifacts().join(format!("{tag}.jsonl"));
    std::fs::write(&script, lines).expect("a writable script");
    render(tag, &["--captions", "--script", &script.to_string_lossy()])
}

#[test]
fn captions_change_only_the_bottom_band() {
    let plain = render("plain", &[]);
    let captioned = render("captioned", &["--captions"]);

    let tolerance = Tolerance {
        // The whole point of the comparison: the content box grew by the caption.
        bbox_must_match: false,
        ..Tolerance::default()
    };
    let diff = compare_png_files(&plain, &captioned, &tolerance).expect("both renders are PNGs");
    let band = diff
        .diff_bbox
        .expect("a captioned still differs from a plain one");
    assert!(
        band.y0 >= HEIGHT - 200,
        "the caption leaked above the bottom band: {band:?}"
    );
    assert!(band.y1 < HEIGHT, "{band:?}");
    assert!(diff.max > 0, "the caption has to be visible");
}

/// The same command twice must produce the same pixels. This is the
/// `fontique` HashMap-order class of bug: a caption that resolved a different
/// face per process would differ here and nowhere else.
#[test]
fn a_captioned_still_is_reproducible() {
    let a = render("repeat-a", &["--captions"]);
    let b = render("repeat-b", &["--captions"]);
    let diff = compare_png_files(&a, &b, &Tolerance::default()).expect("both renders are PNGs");
    assert_eq!(diff.max, 0, "two runs differ: {diff:?}");
    assert!(diff.diff_bbox.is_none(), "{diff:?}");
}

/// The still shows the **last narration due by t = 10**, so two scripts that
/// differ only in what came *before* it must render identically — and one that
/// ends on a different line must not.
#[test]
fn the_still_shows_the_last_narration_that_was_due() {
    let two = render_script(
        "narration-two",
        concat!(
            r#"{"action":"narrate","text":"第一条","at":0.0}"#,
            "\n",
            r#"{"action":"narrate","text":"第二条","at":9.9}"#,
            "\n",
        ),
    );
    let only_late = render_script(
        "narration-late",
        r#"{"action":"narrate","text":"第二条","at":0.0}"#,
    );
    let only_early = render_script(
        "narration-early",
        r#"{"action":"narrate","text":"第一条","at":0.0}"#,
    );

    let same = compare_png_files(&two, &only_late, &Tolerance::default()).expect("PNGs");
    assert_eq!(
        same.max, 0,
        "the still must show the last due line, not the first: {same:?}"
    );

    let different = compare_png_files(&two, &only_early, &Tolerance::default()).expect("PNGs");
    let band = different
        .diff_bbox
        .expect("different narration draws different text");
    assert!(
        band.y0 >= HEIGHT - 200,
        "the difference escaped the band: {band:?}"
    );
}

/// Captions are pixels; `--inspect` never renders one. Accepting the pair would
/// silently do half of what was asked, which is the class of refusal
/// `--a11y-probe` already establishes.
#[test]
fn captions_refuse_a_tree() {
    let out = Command::new(BIN)
        .args(["--inspect", "--captions"])
        .stdin(Stdio::null())
        .output()
        .expect("the binary under test is built");
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty(), "nothing may reach stdout");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("--captions needs a frame"),
        "refused for the wrong reason: {err}"
    );
}
