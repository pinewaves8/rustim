//! The browser's keys, and what this host does about them.
//!
//! Two halves, and the split is the point. [`keydown`] is the adapter — a DOM
//! `KeyboardEvent`'s fields into the vocabulary the desktop also uses, exactly as
//! `rustim_app::input::from_event` converts a winit event. [`handle`] is the
//! execution, which is *not* the same on both sides: a browser has no frame clock
//! for a user to step and its focus ring is the DOM's own, so it honours a subset
//! and says so rather than pretending the rest are no-ops.
//!
//! The decision — which key means what — is neither half. It lives in
//! [`rustim_scene::input::classify`], so `space` cannot come to mean play-or-pause
//! here and something else in a window.
//!
//! Both halves are pure over a [`Session`], which is target-independent, so the
//! whole of this module is tested by `cargo test -p rustim-web` with no browser
//! and no wasm.

use rustim_scene::command::MotionMode;
use rustim_scene::input::{Intent, KeyPress};
use rustim_scene::{Command, NodeId};

use crate::session::Session;

/// Translate one DOM `keydown` into the shared vocabulary.
///
/// `key` is `KeyboardEvent.key` — the *character* the press produced, or a name
/// like `"Tab"` — and `code` is `KeyboardEvent.code`, the physical key. Both are
/// needed for the same reason the desktop needs the logical and the physical key:
/// an input method that consumes a press leaves `key` as `"Unidentified"` while
/// `code` still says which key it was.
///
/// `pressed` is always true because a `keydown` is by definition a press; a
/// keyup is not routed here at all, which is the browser's answer to the
/// desktop's `ElementState` check.
pub fn keydown(key: &str, code: &str, repeat: bool, shift: bool) -> KeyPress {
    KeyPress {
        key: match key {
            "Tab" => rustim_scene::input::Key::Tab,
            "ArrowUp" => rustim_scene::input::Key::ArrowUp,
            "ArrowDown" => rustim_scene::input::Key::ArrowDown,
            "ArrowLeft" => rustim_scene::input::Key::ArrowLeft,
            "ArrowRight" => rustim_scene::input::Key::ArrowRight,
            // The DOM spells the space bar `" "` — one space — while `.code` has
            // `"Space"`. That asymmetry is the reason `key` and `code` cannot be
            // folded into one field.
            " " => rustim_scene::input::Key::Space,
            "Enter" => rustim_scene::input::Key::Enter,
            "Escape" => rustim_scene::input::Key::Escape,
            // Every binding the keymap has is one character, including the
            // full-width forms a Chinese input method produces — so a
            // multi-character `key` is not one of ours, and becomes `Other` where
            // the physical code gets its say.
            other => {
                let mut chars = other.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => rustim_scene::input::Key::Char(c),
                    _ => rustim_scene::input::Key::Other,
                }
            }
        },
        code: match code {
            "Comma" => rustim_scene::input::Code::Comma,
            "Period" => rustim_scene::input::Code::Period,
            "KeyM" => rustim_scene::input::Code::KeyM,
            _ => rustim_scene::input::Code::Other,
        },
        pressed: true,
        repeat,
        shift,
    }
}

/// Act on one key press, if this host can.
///
/// Returns whether anything happened, which is what a caller needs to decide
/// whether to `preventDefault`: a key this answers `false` for is one the page
/// should keep, because it is a browser shortcut or a scroll rather than ours.
///
/// `focused` is the node the DOM focus is on. The browser tracks its own focus
/// ring — the overlay's elements are focusable and the browser moves between them
/// — so this host has no `FocusState`, and the question "which object is the
/// keyboard talking about" is answered by the caller from the DOM.
pub fn handle(session: &mut Session, press: &KeyPress, focused: Option<NodeId>) -> bool {
    let Some(intent) = rustim_scene::input::classify(press) else {
        return false;
    };
    match intent {
        // The two toggles read the engine rather than keeping a copy: `space`
        // means "the other one", and the session state is where "the current one"
        // lives. It is the read half of exactly these commands.
        Intent::TogglePlayback => {
            let cmd = if session.session_state().paused {
                Command::Resume
            } else {
                Command::Pause
            };
            session.command(&cmd).is_ok()
        }
        Intent::ToggleMotion => {
            let mode = match session.session_state().motion {
                MotionMode::Full => MotionMode::Reduced,
                MotionMode::Reduced => MotionMode::Full,
            };
            session.command(&Command::Motion { mode }).is_ok()
        }
        // An arrow key builds an absolute value, because the protocol has no
        // "adjust by" command. `opacity_step` is the shared arithmetic, so a
        // press here and a press in a window land on the same value.
        Intent::Adjust(delta) => {
            let Some(id) = focused else { return false };
            let Some(current) = session.node_opacity(id) else {
                return false;
            };
            session
                .command(&rustim_scene::command::opacity_step(id, current, delta))
                .is_ok()
        }
        // **Deliberately not honoured, and named rather than silently dropped.**
        // A browser has no frame clock for a user to step — the page's own
        // `requestAnimationFrame` drives the timeline — and Tab, Enter and Escape
        // are the DOM's focus ring, which the browser already implements and
        // which this host must not shadow with a second one. Answering `false`
        // lets the page keep them.
        Intent::StepFrame(_)
        | Intent::FocusNext
        | Intent::FocusPrev
        | Intent::AnnounceFocus
        | Intent::Escape => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Session;

    fn session() -> Session {
        Session::new(320, 240, "pythagorean").expect("the scene exists")
    }

    fn press(key: &str) -> KeyPress {
        keydown(key, "", false, false)
    }

    /// A node the overlay would expose, the engine would still hold, and a viewer
    /// could actually see.
    ///
    /// Two filters, both of them real rather than test scaffolding. The first drops
    /// the semantic tree's **synthetic root** — `nodes[0]` is a `Group` the
    /// projection invents, not a mobject, so the engine answers no question about it.
    /// The second is the same rule `FocusState` uses: the Pythagorean scene fades its
    /// objects in, so at virtual time zero every one of them is at zero opacity and
    /// there is nothing for the keyboard to be about. A step past the opening is what
    /// a user does before pressing anything.
    fn visible_node(session: &mut Session) -> NodeId {
        session.advance(5.0);
        let tree = session.semantic();
        tree.nodes
            .iter()
            .map(|n| n.id)
            .find(|id| session.node_opacity(*id).is_some_and(|o| o > 0.0))
            .expect("the scene fades something in")
    }

    #[test]
    fn the_dom_spellings_reach_the_shared_keymap() {
        // The translation, not the keymap — that is tested where it lives. What is
        // checked here is the part the browser contributes: `" "` rather than
        // `"Space"` for the space bar, `"Unidentified"` for an eaten press, and
        // the full-width characters a Chinese IME sends.
        assert!(matches!(
            rustim_scene::input::classify(&press(" ")),
            Some(Intent::TogglePlayback)
        ));
        assert!(matches!(
            rustim_scene::input::classify(&press("Tab")),
            Some(Intent::FocusNext)
        ));
        assert!(matches!(
            rustim_scene::input::classify(&keydown("Unidentified", "Comma", false, false)),
            Some(Intent::StepFrame(-1))
        ));
        assert!(matches!(
            rustim_scene::input::classify(&press("，")),
            Some(Intent::StepFrame(-1))
        ));
        // A key that is nobody's business stays nobody's: the page keeps it.
        assert_eq!(rustim_scene::input::classify(&press("a")), None);
    }

    #[test]
    fn space_stops_and_starts_the_timeline() {
        let mut session = session();
        assert!(!session.session_state().paused, "a fresh session runs");

        assert!(handle(&mut session, &press(" "), None), "space was ours");
        assert!(session.session_state().paused, "and it stopped the clock");

        assert!(handle(&mut session, &press(" "), None));
        assert!(!session.session_state().paused, "then started it again");
    }

    #[test]
    fn m_flips_the_motion_policy_both_ways() {
        let mut session = session();
        assert_eq!(session.session_state().motion, MotionMode::Full);

        assert!(handle(&mut session, &press("m"), None));
        assert_eq!(session.session_state().motion, MotionMode::Reduced);

        assert!(handle(&mut session, &press("m"), None));
        assert_eq!(session.session_state().motion, MotionMode::Full);
    }

    #[test]
    fn an_arrow_moves_the_focused_nodes_opacity_and_clamps_at_the_ends() {
        let mut session = session();
        let id = visible_node(&mut session);
        let start = session.node_opacity(id).expect("the node exists");
        assert!(start > 0.0, "the scene faded it in");

        assert!(handle(&mut session, &press("ArrowLeft"), Some(id)));
        let down = session.node_opacity(id).expect("still there");
        assert_eq!(down, (start - rustim_scene::input::OPACITY_STEP).max(0.0));

        // Holding it at either end is a no-op rather than a refusal: the value is
        // clamped *before* the command is built, which is what `opacity_step`
        // exists for. An out-of-range `set` would be refused, and a refusal is a
        // worse answer to "the user leaned on the key".
        for _ in 0..40 {
            handle(&mut session, &press("ArrowLeft"), Some(id));
        }
        assert_eq!(session.node_opacity(id), Some(0.0), "clamped at invisible");

        for _ in 0..40 {
            handle(&mut session, &press("ArrowRight"), Some(id));
        }
        assert_eq!(session.node_opacity(id), Some(1.0), "and at opaque");
    }

    #[test]
    fn an_arrow_with_nothing_focused_does_nothing() {
        let mut session = session();
        let id = visible_node(&mut session);
        let before = session.node_opacity(id);
        assert!(
            !handle(&mut session, &press("ArrowRight"), None),
            "there is no object for the arrow to be about"
        );
        assert_eq!(session.node_opacity(id), before, "and nothing moved");
    }

    #[test]
    fn the_desktops_own_intents_are_not_honoured_here() {
        // Named rather than silently dropped: the browser has no frame clock to
        // step, and Tab/Enter/Escape belong to its focus ring. `false` is what
        // tells the page to keep them.
        let mut session = session();
        for key in [",", ".", "Tab", "Enter", "Escape"] {
            assert!(
                !handle(&mut session, &press(key), None),
                "{key} is the browser's own"
            );
        }
    }
}
