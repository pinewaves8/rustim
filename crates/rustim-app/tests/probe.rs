//! `--a11y-probe` is a windowed mode, and the flags it refuses must be rejected
//! **before** a window is built — so the gate lives in `main` and can only be
//! tested by running the real binary, exactly as the golden suite does.
//!
//! The report's wording is tested in the library (`a11y::tests::ProbeReport`);
//! this file covers the one property that has no library surface: three
//! combinations that would otherwise answer the wrong question, all refused with
//! exit code 2 and nothing on stdout.

use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_rustim-app");

fn run(args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("the binary under test is built")
}

/// A path to an empty script that **exists**, so these refusals are about the
/// *flags* and not about a path the binary could not open.
///
/// `/dev/null` was the spelling here, and it does not exist on Windows — which
/// would have made the third case pass there for the wrong reason, or fail,
/// depending on when the gate runs. The temp directory is portable.
fn empty_script() -> std::path::PathBuf {
    let path = std::env::temp_dir().join("rustim-probe-refusal.jsonl");
    std::fs::write(&path, "").expect("write the empty script");
    path
}

#[test]
fn the_probe_refuses_every_headless_flag() {
    let script = empty_script();
    let script = script.to_str().expect("a utf-8 temp path");
    for args in [
        vec!["--a11y-probe", "--inspect"],
        vec!["--a11y-probe", "--png", "unused.png"],
        vec!["--a11y-probe", "--script", script, "--reply"],
    ] {
        let out = run(&args);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{args:?} should have been a refusal"
        );
        assert!(
            out.stdout.is_empty(),
            "{args:?} wrote to stdout: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("--a11y-probe needs the window"),
            "{args:?} refused for the wrong reason: {err}"
        );
    }
}
