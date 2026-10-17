//! Reading the platform's "reduce motion" preference, best effort.
//!
//! The preference itself is [`rustim_anim::Motion`], and it lives in the semantic
//! kernel because the timeline is what reads it. *Reading* it is a platform
//! question — a subprocess, a D-Bus call, a framework binding — and so it lives
//! here, for the same reason `accesskit` does: `rustim-anim` compiles to
//! `wasm32-unknown-unknown`, and a crate that spawns a process does not.
//!
//! # What is covered, and what is deliberately not
//!
//! **Linux**, via `gsettings get org.gnome.desktop.interface enable-animations`:
//! the GNOME-family schemas, which is also what GTK-family desktops that keep the
//! schema (UKUI, on this machine, does; it answers `true`) honour. KDE keeps the
//! same intent in its own configuration and is **not** read here, and the XDG
//! desktop portal has no such key at all.
//!
//! **macOS** would be `NSWorkspace.shared.accessibilityDisplayShouldReduceMotion`.
//! It is not implemented: `accesskit_macos` declares `objc2-app-kit` without the
//! `NSWorkspace` feature, so the binding would have to be added, and nothing here
//! can compile or run a macOS target to check it.
//!
//! **Windows** would be `SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, ...)`.
//! It is not implemented, for the same reason and with the same consequence: on
//! both platforms this returns [`Motion::Full`].
//!
//! So the honest summary is that a user on macOS or Windows has the flag and not
//! the preference, and a user on a non-GNOME Linux desktop has the flag and
//! *usually* the preference. Everything unread, unparsable or failing returns
//! [`Motion::Full`] — a probe that cannot answer must not silently disable a
//! scene's animation, and "the setting says animate" is the answer that keeps the
//! picture a user sees identical to the one every golden image was made from.

use rustim_anim::Motion;

/// The preference as the platform reports it, or [`Motion::Full`] if it cannot be
/// read.
///
/// Never fails and never blocks for longer than the subprocess takes: this is a
/// best-effort read at startup, and the caller has already had the chance to
/// override it with the command-line flag.
pub fn from_platform() -> Motion {
    #[cfg(target_os = "linux")]
    let command: Option<(&str, &[&str])> = Some((
        "gsettings",
        &["get", "org.gnome.desktop.interface", "enable-animations"],
    ));
    // macOS (`NSWorkspace.accessibilityDisplayShouldReduceMotion`) and Windows
    // (`SPI_GETCLIENTAREAANIMATION`) are named in the module doc and deliberately
    // unread. Not a `#[cfg]` arm returning `Full` — that would look like a
    // decision that had been *made* about those platforms rather than one that is
    // outstanding, and it would leave the probe below dead code in every build of
    // those two platforms, which `-D warnings` turns into a failed build.
    #[cfg(not(target_os = "linux"))]
    let command: Option<(&str, &[&str])> = None;

    probe(command).unwrap_or(Motion::Full)
}

/// Run a command — if there is one — and read the two answers out of its stdout.
///
/// Broken out from [`from_platform`] so the *plumbing* has a test that does not
/// need the machine to have a GNOME schema installed: `/bin/echo true` and
/// `/bin/echo false` exercise the same path the real command does, and a program
/// that does not exist exercises the failure.
fn probe(command: Option<(&str, &[&str])>) -> Option<Motion> {
    let (program, args) = command?;
    let out = std::process::Command::new(program)
        .args(args)
        .output()
        .ok()?;
    // A nonzero exit means the setting could not be read (a missing schema is the
    // common case, and it prints its complaint on stderr) — the same "no answer" as
    // a parse failure, and no reason to look at stdout.
    if !out.status.success() {
        return None;
    }
    parse_output(&String::from_utf8(out.stdout).ok()?)
}

/// `true` / `false` from a command's stdout, or `None` for anything else.
///
/// `gsettings get` prints a GVariant: `true\n` or `false\n`, bare and lowercase.
/// Anything else — an empty string, a localized `unknown`, a schema's error text
/// that somehow reached stdout — is not an answer, and saying `None` rather than
/// guessing is what keeps this a probe rather than a parser with opinions.
fn parse_output(text: &str) -> Option<Motion> {
    match text.trim() {
        // `enable-animations = false` is the reduced-motion setting. The double
        // negative is the whole reason this mapping is spelled out in one place.
        "false" => Some(Motion::Reduced),
        "true" => Some(Motion::Full),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_answers_parse_and_the_rest_does_not() {
        assert_eq!(parse_output("true\n"), Some(Motion::Full));
        assert_eq!(parse_output("false\n"), Some(Motion::Reduced));
        assert_eq!(parse_output("  false  "), Some(Motion::Reduced));
        for other in [
            "",
            "\n",
            "unknown",
            "enabled",
            "TRUE",
            "1",
            "No such schema",
        ] {
            assert_eq!(parse_output(other), None, "{other:?} is not an answer");
        }
    }

    /// The plumbing, not the schema: whatever a command prints on stdout is what
    /// gets parsed.
    #[test]
    fn the_probe_reads_the_command_s_stdout() {
        assert_eq!(
            probe(Some(("/bin/echo", &["false"]))),
            Some(Motion::Reduced)
        );
        assert_eq!(probe(Some(("/bin/echo", &["true"]))), Some(Motion::Full));
        assert_eq!(probe(Some(("/bin/echo", &["maybe"]))), None);
    }

    /// A probe that cannot run has no answer, and no answer is not "reduced": the
    /// fallback has to be the mode every baseline was made in.
    #[test]
    fn a_probe_that_cannot_run_answers_nothing() {
        assert_eq!(
            probe(Some(("rustim-no-such-command-anywhere", &[]))),
            None,
            "a missing program is not an answer"
        );
        assert_eq!(
            probe(Some(("/bin/false", &[]))),
            None,
            "neither is a nonzero exit"
        );
        assert_eq!(probe(None), None, "and neither is having nothing to run");
    }
}
