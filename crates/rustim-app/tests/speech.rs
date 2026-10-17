//! `--speak` is a windowed mode: self-voicing follows the frame loop's clock,
//! and a headless run has no time to speak during. The refusal has to happen
//! **before** a window is built, so — like `--a11y-probe` — it can only be
//! tested by running the real binary.
//!
//! What a *voice* would say is tested without a voice in the library
//! (`speech::tests`, a recording `Voice` in front of `Speaker`), and the
//! command construction is a pure function checked for all three platforms on
//! any one of them. This file covers the one property with no library surface:
//! the two combinations that would otherwise silently do half of what was
//! asked, refused with exit code 2 and nothing on stdout.

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
fn self_voicing_refuses_the_headless_modes() {
    for args in [
        &["--speak", "--png", "unused.png"][..],
        &["--speak", "--inspect"][..],
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
            err.contains("--speak needs the window"),
            "{args:?} refused for the wrong reason: {err}"
        );
    }
}
