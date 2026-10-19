//! The keyboard, as a platform-neutral vocabulary two hosts can share.
//!
//! The desktop host sees keys as `winit` events and the browser host sees them as
//! DOM events, and the **keymap is the one thing they must agree on**: `space`
//! means play-or-pause in both, or the engine has two keyboards and a user has to
//! learn which one they are talking to. So the decisions live here — a neutral
//! [`Key`], the [`Intent`] it maps to, and [`classify`] — and each host keeps only
//! the adapter that fills a [`KeyPress`] in from its own event type.
//!
//! That is the arrangement [`crate::a11y`] uses for the semantic tree and
//! `rustim_math::gesture` uses for the pointer: a vocabulary two hosts share
//! belongs where both can reach it, and neither can reach the other's event types.
//! `winit` has no wasm32 build, so a keymap living in the desktop host could not
//! be reached by the browser at all — and a second keymap in the browser would be
//! a second answer to "what does `m` do".
//!
//! # Intents become commands, not direct mutations
//!
//! An adjustment is not applied here. It becomes a
//! [`Property::Opacity`](crate::Command) command, the same one an AI or a host
//! would send, and goes through the same validation. So an arrow key cannot
//! produce a value the command layer would refuse, and there is one
//! implementation of "what does setting opacity mean". Each host executes the
//! intents it can and ignores the rest — a browser has no frame-stepping, and the
//! desktop has no such thing as a DOM focus ring.

/// How much one arrow-key press changes the focused object's opacity.
///
/// The step the accessibility layer advertises for the same value, so a screen
/// reader's increment action and a key press move it by the same amount.
pub const OPACITY_STEP: f64 = 0.05;

/// What a key event asks the engine to do.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Intent {
    /// Tab.
    FocusNext,
    /// Shift+Tab.
    FocusPrev,
    /// An arrow key. The sign is the direction: right and up add, left and down
    /// subtract.
    ///
    /// All four arrows move the same single parameter this step. That is not a
    /// placeholder for an axis field — a positional nudge is a different intent
    /// with a different command behind it, and it should be added as such rather
    /// than as a second meaning bolted onto an `axis` field nothing reads.
    Adjust(f64),
    /// Space.
    TogglePlayback,
    /// `m`: flip between full and reduced motion.
    ///
    /// The switch takes effect at the next beat started, not on one already on
    /// the clock — see [`crate::Command::Motion`]. It exists so a user who did
    /// not (or could not) tell the host their preference up front has a way to
    /// ask mid-session.
    ToggleMotion,
    /// `,` and `.`: step one frame backwards or forwards.
    StepFrame(i8),
    /// Enter: read the focused object aloud.
    AnnounceFocus,
    /// Escape: leave whatever the user is in.
    Escape,
}

/// The keys the keymap reads.
///
/// Deliberately not a general key vocabulary: it names exactly what [`classify`]
/// looks at, so a host's adapter has a small, closed target to map onto and
/// anything else becomes [`Key::Other`] rather than being guessed at.
///
/// A character key is [`Key::Char`] rather than a string, because every character
/// the keymap binds is one character — including the full-width forms a Chinese
/// IME produces, which is why the comma case exists at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Tab,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Space,
    Enter,
    Escape,
    /// The character this press produced, if it produced exactly one.
    Char(char),
    /// A key this vocabulary does not name.
    ///
    /// The interesting case, and the reason [`KeyPress::code`] exists: an IME that
    /// consumes the press outright leaves the host with no logical key at all,
    /// while the *physical* key is still whatever was pressed.
    Other,
}

/// The physical keys the fallback in [`classify`] reads.
///
/// Only the three the keymap can act on. Everything else is
/// [`Code::Other`] — a physical key is not a place to be thorough, it is a
/// fallback for the one case where the logical key did not identify itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    Comma,
    Period,
    KeyM,
    Other,
}

/// What the keymap is allowed to look at, lifted out of a host's key event.
///
/// The desktop's `KeyEvent` cannot be constructed outside `winit` — it carries a
/// private `platform_specific` field — so a `classify` that took one could not be
/// called from a test at all, and the browser host could not call it ever. Rather
/// than leave the keymap untested, the fields it actually reads are named here;
/// each host's adapter fills them in, and that adapter is the only part of the
/// keymap a test cannot reach and it contains no decisions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KeyPress {
    /// The layout-aware key, which is what binds.
    pub key: Key,
    /// Used only as a fallback when `key` did not identify itself.
    pub code: Code,
    pub pressed: bool,
    pub repeat: bool,
    /// Whether Shift was held. A plain `bool` rather than a modifier set, because
    /// Shift is the only modifier the keymap reads and because modifier state is a
    /// *window* property on the desktop — it arrives in its own event, so it has
    /// to be tracked across events and handed in here rather than read off the
    /// keypress. On the web it is a field of the same event, and the adapter reads
    /// it there.
    pub shift: bool,
}

/// Map a key press to an intent, or `None` if this key is not ours.
///
/// # Repeats
///
/// A held arrow key repeats, and holding one to fade an object in is the
/// expected behaviour, so [`Intent::Adjust`] and [`Intent::StepFrame`] accept
/// repeats. Tab and the others do not: a repeat there means the focus rings
/// through the scene, or playback toggles thirty times a second, and neither is
/// something a user did on purpose.
pub fn classify(press: &KeyPress) -> Option<Intent> {
    if !press.pressed {
        return None;
    }
    let intent = match press.key {
        Key::Tab => {
            if press.shift {
                Intent::FocusPrev
            } else {
                Intent::FocusNext
            }
        }
        Key::ArrowRight | Key::ArrowUp => Intent::Adjust(OPACITY_STEP),
        Key::ArrowLeft | Key::ArrowDown => Intent::Adjust(-OPACITY_STEP),
        Key::Space => Intent::TogglePlayback,
        Key::Enter => Intent::AnnounceFocus,
        Key::Escape => Intent::Escape,
        // The full-width forms matter here and are not a curiosity: this is a
        // Chinese IME environment, and with the IME in full-width mode the comma
        // key produces U+FF0C, so an ASCII-only keymap silently does nothing on
        // the machine the engine is being built on.
        Key::Char(',' | '，') => Intent::StepFrame(-1),
        Key::Char('.' | '。') => Intent::StepFrame(1),
        // Shift is already part of the character here (a shifted `m` arrives as
        // `'M'`), and the full-width pair is spelled out for the same IME reason
        // as the comma.
        Key::Char('m' | 'M' | 'ｍ' | 'Ｍ') => Intent::ToggleMotion,
        // Fallback for the case the full-width check cannot cover: an IME that
        // consumes the press outright leaves the logical key unidentified while the
        // *physical* key is still whatever was pressed. Restricted to the keys that
        // mean "no logical key", so it can never shadow a key that did identify
        // itself.
        Key::Other => match press.code {
            Code::Comma => Intent::StepFrame(-1),
            Code::Period => Intent::StepFrame(1),
            Code::KeyM => Intent::ToggleMotion,
            Code::Other => return None,
        },
        _ => return None,
    };
    if press.repeat && !matches!(intent, Intent::Adjust(_) | Intent::StepFrame(_)) {
        return None;
    }
    Some(intent)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A press of `key`, with an unrelated physical code so that a test passing
    /// for the *logical* reason cannot be passing because of the fallback.
    fn key(k: Key, pressed: bool, repeat: bool) -> KeyPress {
        KeyPress {
            key: k,
            code: Code::Other,
            pressed,
            repeat,
            shift: false,
        }
    }

    fn named(k: Key) -> KeyPress {
        key(k, true, false)
    }

    fn ch(c: char) -> KeyPress {
        key(Key::Char(c), true, false)
    }

    fn press(k: Key) -> KeyPress {
        key(k, true, false)
    }

    #[test]
    fn the_documented_keymap_is_the_one_implemented() {
        assert_eq!(classify(&named(Key::Tab)), Some(Intent::FocusNext));
        assert_eq!(
            classify(&KeyPress {
                shift: true,
                ..named(Key::Tab)
            }),
            Some(Intent::FocusPrev)
        );
        assert_eq!(
            classify(&named(Key::ArrowRight)),
            Some(Intent::Adjust(OPACITY_STEP))
        );
        assert_eq!(
            classify(&named(Key::ArrowUp)),
            Some(Intent::Adjust(OPACITY_STEP))
        );
        assert_eq!(
            classify(&named(Key::ArrowLeft)),
            Some(Intent::Adjust(-OPACITY_STEP))
        );
        assert_eq!(
            classify(&named(Key::ArrowDown)),
            Some(Intent::Adjust(-OPACITY_STEP))
        );
        assert_eq!(classify(&named(Key::Space)), Some(Intent::TogglePlayback));
        assert_eq!(classify(&named(Key::Enter)), Some(Intent::AnnounceFocus));
        assert_eq!(classify(&named(Key::Escape)), Some(Intent::Escape));
        assert_eq!(classify(&ch(',')), Some(Intent::StepFrame(-1)));
        assert_eq!(classify(&ch('.')), Some(Intent::StepFrame(1)));
        assert_eq!(classify(&ch('m')), Some(Intent::ToggleMotion));
        // A shifted `m` arrives as the uppercase character, which the host reports
        // as such — `shift` is not consulted for it. The full-width pair is the
        // IME form, for the same reason as the comma.
        assert_eq!(classify(&ch('M')), Some(Intent::ToggleMotion));
        assert_eq!(classify(&ch('ｍ')), Some(Intent::ToggleMotion));
        assert_eq!(classify(&ch('Ｍ')), Some(Intent::ToggleMotion));
        // Anything else is not ours, and saying so is what keeps the classifier
        // from swallowing keys the IME or a future widget needs.
        assert_eq!(classify(&ch('a')), None);
        assert_eq!(classify(&named(Key::Other)), None);
    }

    #[test]
    fn an_ime_that_eats_the_key_still_steps_frames() {
        // The case the full-width check cannot see: the press produced no logical
        // key at all, but it is physically the comma key.
        let eaten = |code: Code| KeyPress {
            key: Key::Other,
            code,
            pressed: true,
            repeat: false,
            shift: false,
        };
        assert_eq!(classify(&eaten(Code::Comma)), Some(Intent::StepFrame(-1)));
        assert_eq!(classify(&eaten(Code::Period)), Some(Intent::StepFrame(1)));
        assert_eq!(classify(&eaten(Code::KeyM)), Some(Intent::ToggleMotion));
        assert_eq!(classify(&eaten(Code::Other)), None);
        // ...and it stays out of the way of a key that did identify itself: the
        // physical code here says "comma", the logical key says "Escape".
        let identified = KeyPress {
            key: Key::Escape,
            ..eaten(Code::Comma)
        };
        assert_eq!(classify(&identified), Some(Intent::Escape));
    }

    #[test]
    fn full_width_punctuation_steps_frames_too() {
        // A Chinese IME in full-width mode sends these, and on this project's own
        // machine that is the default state. An ASCII-only keymap would look
        // correct in every test written in ASCII and do nothing in practice.
        assert_eq!(classify(&ch('，')), Some(Intent::StepFrame(-1)));
        assert_eq!(classify(&ch('。')), Some(Intent::StepFrame(1)));
    }

    #[test]
    fn key_releases_are_not_intents() {
        assert_eq!(classify(&key(Key::Space, false, false)), None);
    }

    #[test]
    fn repeats_move_values_but_never_focus_or_playback() {
        let repeat = |k: Key| classify(&key(k, true, true));
        // Holding an arrow fades; holding Tab must not ring through the scene.
        assert_eq!(repeat(Key::ArrowRight), Some(Intent::Adjust(OPACITY_STEP)));
        assert_eq!(repeat(Key::Tab), None);
        assert_eq!(repeat(Key::Space), None);
        assert_eq!(repeat(Key::Enter), None);
        assert_eq!(repeat(Key::Char(',')), Some(Intent::StepFrame(-1)));
        // Holding `m` must not flip the mode thirty times a second.
        assert_eq!(repeat(Key::Char('m')), None);
    }

    #[test]
    fn a_char_is_only_ever_one_character() {
        // The vocabulary's own invariant, and the reason it is a `char` rather
        // than a string: every binding is a single character, so a host's adapter
        // has a total function to write — a multi-character key is `Other`, and
        // `Other` falls through to the physical code.
        assert_eq!(
            classify(&press(Key::Char('，'))),
            Some(Intent::StepFrame(-1))
        );
        assert_eq!(classify(&press(Key::Other)), None);
    }
}
