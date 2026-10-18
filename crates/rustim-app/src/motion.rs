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
//! **macOS** reads `NSWorkspace.accessibilityDisplayShouldReduceMotion`, in
//! process, through `objc2-app-kit` — see the [`macos`] module. No subprocess and
//! no D-Bus: the framework getter answers with a `bool`, so there is nothing to
//! parse and nothing that can fail. It must be called on the **main thread**, and
//! it is: the one caller is `main`, before the event loop exists.
//!
//! **Windows** would be `SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, ...)`.
//! It is **not** implemented — nothing here can compile or run a Windows target to
//! check a binding — so that platform returns [`Motion::Full`].
//!
//! So the honest summary is that a user on Windows has the flag and not the
//! preference, while a user on macOS or on a GNOME-family Linux desktop has both.
//! Everything unread, unparsable or failing returns
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
    // macOS answers in process, and its answer is definitive — so it is asked
    // first and, when it answers, nothing else runs.
    //
    // Spelled `if let … { return … }` rather than a bare `return` on purpose: the
    // code below has to stay *reachable to the compiler* on macOS, because
    // `probe` must keep a caller in every build or `dead_code` under `-D warnings`
    // fails the macOS build. That is the trap the `command` split below was
    // written to avoid, approached from the other side.
    #[cfg(target_os = "macos")]
    if let Some(motion) = macos::reduce_motion() {
        return motion;
    }

    #[cfg(target_os = "linux")]
    let command: Option<(&str, &[&str])> = Some((
        "gsettings",
        &["get", "org.gnome.desktop.interface", "enable-animations"],
    ));
    // Windows (`SPI_GETCLIENTAREAANIMATION`) is named in the module doc and
    // deliberately unread. Not a `#[cfg]` arm returning `Full` — that would look
    // like a decision that had been *made* about that platform rather than one
    // that is outstanding, and it would leave the probe below dead code in every
    // Windows build, which `-D warnings` turns into a failed build.
    #[cfg(not(target_os = "linux"))]
    let command: Option<(&str, &[&str])> = None;

    probe(command).unwrap_or(Motion::Full)
}

/// The `bool` a platform reports, as the preference it means.
///
/// Broken out so the mapping is a pure function with its own test on every
/// platform — the same reason [`parse_output`] exists rather than the comparison
/// living inside the `gsettings` arm. macOS is the only caller today.
fn from_reduce_motion_flag(reduced: bool) -> Motion {
    if reduced {
        Motion::Reduced
    } else {
        Motion::Full
    }
}

/// The macOS read, in its own module so `objc2-app-kit` is named only in a build
/// that has it: a `use` at the top of this file would need a `#[cfg]` of its own
/// and would not resolve at all on Linux or Windows.
#[cfg(target_os = "macos")]
mod macos {
    use super::{Motion, from_reduce_motion_flag};
    use objc2_app_kit::NSWorkspace;

    /// `NSWorkspace.accessibilityDisplayShouldReduceMotion`.
    ///
    /// **Call from the main thread.** `sharedWorkspace` is a Cocoa singleton
    /// accessor and AppKit expects it on the main thread. The one caller is
    /// `main`, before the event loop is built, so that holds today — it is
    /// recorded here so a future caller does not move this onto the render thread
    /// or the stdin reader thread.
    ///
    /// Always `Some`: a `bool` getter cannot fail. The `Option` is the shape the
    /// platform probes share, not an error channel.
    pub(super) fn reduce_motion() -> Option<Motion> {
        // SAFETY: a nib-less singleton accessor. It takes no argument, cannot
        // fail, and the framework owns the object it returns.
        let workspace = unsafe { NSWorkspace::sharedWorkspace() };
        // SAFETY: reads a user default off a live `NSWorkspace`. The getter has
        // no precondition beyond the receiver being valid, which the line above
        // guarantees; it returns a `bool`, so no pointer leaves this call.
        let reduced = unsafe { workspace.accessibilityDisplayShouldReduceMotion() };
        Some(from_reduce_motion_flag(reduced))
    }
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

    /// The `bool` → preference mapping: the half of the macOS read that can be
    /// checked on every platform. The framework call itself is one line with no
    /// logic, so the double negative is the only place a mistake can hide.
    #[test]
    fn the_reduce_motion_flag_maps_to_the_preference_it_means() {
        assert_eq!(
            from_reduce_motion_flag(true),
            Motion::Reduced,
            "the platform saying yes means the user asked for less motion"
        );
        assert_eq!(from_reduce_motion_flag(false), Motion::Full);
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
