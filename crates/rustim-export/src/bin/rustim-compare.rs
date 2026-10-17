//! `rustim-compare` — the visual-regression CLI.
//!
//! Two PNGs in, the same measurements the golden suite asserts out, and an exit
//! code a script can branch on:
//!
//! | code | meaning |
//! |---|---|
//! | 0 | within the §5.2 tolerance |
//! | 1 | the images differ beyond it |
//! | 2 | the question could not be asked (bad path, mismatched sizes, usage) |
//!
//! The default tolerance is exactly the golden suite's: identical content
//! bounding boxes, no pixel differing by more than 8, maximum channel error 3.
//! `--bbox-any` relaxes the first rule, which is what a caller wants when a
//! change legitimately adds content (a caption, a new object) — the whole
//! reason `Tolerance`'s fields are public.
//!
//! Deliberately not a subcommand of `rustim-app`: this tool reads PNGs and
//! knows nothing about scenes, so it works on a Manim reference frame, a CI
//! artifact, or a screenshot.

use std::path::PathBuf;
use std::process::ExitCode;

use rustim_export::compare::{Tolerance, compare_png_files};

const USAGE: &str = "usage: rustim-compare [--bbox-any] A.png B.png";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut tolerance = Tolerance::default();
    let mut paths: Vec<PathBuf> = Vec::new();
    for arg in &args {
        match arg.as_str() {
            "--bbox-any" => tolerance.bbox_must_match = false,
            "--help" | "-h" => {
                println!("{USAGE}");
                println!("compare two PNGs with the golden suite's tolerance (§5.2)");
                println!(
                    "  --bbox-any  do not require the content bounding boxes to match \
                     (the content legitimately changed)"
                );
                println!("exit: 0 same, 1 different, 2 could not compare");
                return ExitCode::SUCCESS;
            }
            other if other.starts_with('-') => {
                eprintln!("unknown flag: {other}\n{USAGE}");
                return ExitCode::from(2);
            }
            other => paths.push(PathBuf::from(other)),
        }
    }
    let [a, b] = paths.as_slice() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };

    match compare_png_files(a, b, &tolerance) {
        Ok(diff) => {
            print!("{}", diff.report(&tolerance));
            if diff.passes(&tolerance) {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(e) => {
            eprintln!("rustim-compare: {e}");
            ExitCode::from(2)
        }
    }
}
