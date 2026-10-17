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

#[test]
fn the_probe_refuses_every_headless_flag() {
    for args in [
        &["--a11y-probe", "--inspect"][..],
        &["--a11y-probe", "--png", "unused.png"][..],
        &["--a11y-probe", "--script", "/dev/null", "--reply"][..],
    ] {
        let out = run(args);
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
