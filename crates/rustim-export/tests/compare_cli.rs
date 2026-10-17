//! The visual-regression CLI is a thin shell over `compare`, but the exit code
//! is the contract a CI job or an agent reads, and that can only be tested by
//! running the binary.
//!
//! The images are generated here with the same PNG writer the exporter uses, so
//! the test needs no checked-in fixtures and no external program.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_rustim-compare");
const BG: [u8; 3] = [0x12, 0x12, 0x12];

fn dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rustim-compare-cli-{}-{tag}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn frame(width: u32, height: u32, block: &[(u32, u32, u8)]) -> Vec<u8> {
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let v = block
                .iter()
                .find(|(bx, by, _)| *bx == x && *by == y)
                .map_or(BG[0], |(_, _, v)| *v);
            pixels.extend_from_slice(&[v, v, v, 255]);
        }
    }
    pixels
}

fn write_png(path: &Path, width: u32, height: u32, pixels: &[u8]) {
    rustim_export::write_rgba_png(pixels, width, height, path).unwrap();
}

fn run(args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .output()
        .expect("the binary under test is built")
}

#[test]
fn identical_images_pass_with_exit_zero() {
    let d = dir("same");
    let a = d.join("a.png");
    write_png(&a, 8, 8, &frame(8, 8, &[(2, 2, 60)]));

    let out = run(&[a.to_str().unwrap(), a.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("verdict: PASS"), "{text}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_visible_difference_fails_with_exit_one() {
    let d = dir("diff");
    let a = d.join("a.png");
    let b = d.join("b.png");
    write_png(&a, 8, 8, &frame(8, 8, &[(2, 2, 60)]));
    write_png(&b, 8, 8, &frame(8, 8, &[(2, 2, 250)]));

    let out = run(&[a.to_str().unwrap(), b.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("FAIL"), "{text}");
    assert!(text.contains("maximum channel error"), "{text}");
    let _ = std::fs::remove_dir_all(&d);
}

/// A one-level difference that moves the measured content box: the layout rule
/// is the only one broken, so relaxing exactly that rule must turn the verdict
/// around. The far pixel sits at the content threshold — value 9 is nine away
/// from the `#121212` background (content), value 10 is only eight away (not),
/// and the two differ by one, inside the max-error rule.
#[test]
fn bbox_any_relaxes_only_the_layout_rule() {
    let d = dir("bbox");
    let a = d.join("a.png");
    let b = d.join("b.png");
    write_png(&a, 8, 8, &frame(8, 8, &[(2, 2, 60), (7, 7, 9)]));
    write_png(&b, 8, 8, &frame(8, 8, &[(2, 2, 60), (7, 7, 10)]));

    let strict = run(&[a.to_str().unwrap(), b.to_str().unwrap()]);
    assert_eq!(
        strict.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&strict.stdout)
    );

    let relaxed = run(&["--bbox-any", a.to_str().unwrap(), b.to_str().unwrap()]);
    assert_eq!(
        relaxed.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&relaxed.stdout)
    );
    assert!(
        String::from_utf8_lossy(&relaxed.stdout).contains("verdict: PASS"),
        "{}",
        String::from_utf8_lossy(&relaxed.stdout)
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_question_that_cannot_be_asked_is_exit_two() {
    let d = dir("bad");
    let a = d.join("a.png");
    let square = d.join("square.png");
    let flat = d.join("flat.png");
    write_png(&a, 8, 8, &frame(8, 8, &[]));
    write_png(&square, 8, 8, &frame(8, 8, &[]));
    write_png(&flat, 8, 9, &frame(8, 9, &[]));

    let missing = run(&[a.to_str().unwrap(), d.join("nope.png").to_str().unwrap()]);
    assert_eq!(missing.status.code(), Some(2));
    assert!(!missing.stderr.is_empty());

    let mismatched = run(&[square.to_str().unwrap(), flat.to_str().unwrap()]);
    assert_eq!(mismatched.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&mismatched.stderr).contains("size mismatch"),
        "{}",
        String::from_utf8_lossy(&mismatched.stderr)
    );

    let usage = run(&[a.to_str().unwrap()]);
    assert_eq!(usage.status.code(), Some(2));
    let _ = std::fs::remove_dir_all(&d);
}
