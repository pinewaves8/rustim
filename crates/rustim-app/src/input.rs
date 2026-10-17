//! Input, as pure state and arithmetic over window events.
//!
//! Two families live here. The **keyboard** maps a key press to an [`Intent`] and
//! tracks which object the focus is on ([`FocusState`]). The **pointer** turns a
//! drag into a pan delta and a scroll into a multiplicative zoom factor. The
//! gesture arithmetic itself is shared with the browser host and lives in
//! [`rustim_math::gesture`] — this module only adapts winit's event shapes onto
//! it, so both hosts run one implementation.
//!
//! The engine had neither subsystem at first: `window_event` matched
//! `CloseRequested`, `Resized` and `RedrawRequested` and dropped everything else.
//! §4.6's fourth requirement — Tab to move between objects, arrow keys to adjust
//! one, space to play or pause, `,`/`.` to step a frame — and the manual camera
//! controls were added later, in that order.
//!
//! # Why the classification is separate from the handling
//!
//! [`classify`] takes winit types and returns a plain [`Intent`]; nothing else in
//! this module knows winit exists — the two `from_winit`/`from_event` adapters are
//! the only boundary — and [`FocusState`] knows nothing but node ids. That is what
//! lets the keymap and the gesture arithmetic be tested in CI, where there is no
//! window, no display and no accessibility bus — and this is exactly the layer
//! whose bugs are invisible in a screenshot.
//!
//! # Intents become commands, not direct mutations
//!
//! An adjustment is not applied here. It becomes a
//! [`Property::Opacity`](rustim_scene::Command) command, the same one an AI or a
//! host would send, and goes through the same validation. So an arrow key cannot
//! produce a value the command layer would refuse, and there is one
//! implementation of "what does setting opacity mean". Pan and zoom follow the
//! same rule at the call site in `main.rs`: they become `Command::Camera`, not
//! writes to `tree.camera`.

// The gesture vocabulary is shared with the browser host (`rustim-web`), so the
// pan/zoom algebra has one implementation rather than two that drift. What stays
// here is the winit adapter and the host's event wiring.
pub use rustim_math::gesture::{Drag, Pointer, ScrollDelta, clamped_zoom_factor, zoom_factor};
use rustim_mobject::NodeId;
use rustim_scene::SceneTree;
use winit::event::{ElementState, KeyEvent, MouseScrollDelta};
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};

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
    /// the clock — see [`rustim_scene::Command::Motion`]. It exists so a user
    /// who did not (or could not) tell the host their preference up front has a
    /// way to ask mid-session.
    ToggleMotion,
    /// `,` and `.`: step one frame backwards or forwards.
    StepFrame(i8),
    /// Enter: read the focused object aloud.
    AnnounceFocus,
    /// Escape: leave whatever the user is in.
    Escape,
}

/// What the keymap is allowed to look at, lifted out of a winit key event.
///
/// winit's `KeyEvent` cannot be constructed outside winit — it carries a private
/// `platform_specific` field — so a `classify` that took one could not be called
/// from a test at all. Rather than leave the keymap untested, the fields it
/// actually reads are named here, and [`Self::from_event`] is the two-line
/// adapter that fills them in. That adapter is the only part of the keymap that
/// a test cannot reach, and it contains no decisions.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyPress {
    /// The layout-aware key, which is what binds.
    pub logical: Key,
    /// Used only as a fallback when `logical` did not identify itself.
    pub physical: PhysicalKey,
    pub state: ElementState,
    pub repeat: bool,
    /// Whether Shift was held. A plain `bool` rather than winit's
    /// `ModifiersState`, because Shift is the only modifier the keymap reads and
    /// because modifier state is a *window* property in winit — it arrives in
    /// its own `ModifiersChanged` event, so it has to be tracked across events
    /// and handed in here rather than read off the keypress.
    pub shift: bool,
}

impl KeyPress {
    /// Lift the fields the keymap reads out of a winit event and the window's
    /// current modifier state.
    pub fn from_event(event: &KeyEvent, mods: ModifiersState) -> Self {
        Self {
            logical: event.logical_key.clone(),
            physical: event.physical_key,
            state: event.state,
            repeat: event.repeat,
            shift: mods.contains(ModifiersState::SHIFT),
        }
    }
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
    if press.state != ElementState::Pressed {
        return None;
    }
    let intent = match press.logical.as_ref() {
        Key::Named(NamedKey::Tab) => {
            if press.shift {
                Intent::FocusPrev
            } else {
                Intent::FocusNext
            }
        }
        Key::Named(NamedKey::ArrowRight | NamedKey::ArrowUp) => Intent::Adjust(OPACITY_STEP),
        Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowDown) => Intent::Adjust(-OPACITY_STEP),
        Key::Named(NamedKey::Space) => Intent::TogglePlayback,
        Key::Named(NamedKey::Enter) => Intent::AnnounceFocus,
        Key::Named(NamedKey::Escape) => Intent::Escape,
        // The full-width forms matter here and are not a curiosity: this is a
        // Chinese IME environment, and with the IME in full-width mode the comma
        // key produces U+FF0C, so an ASCII-only keymap silently does nothing on
        // the machine the engine is being built on.
        Key::Character("," | "，") => Intent::StepFrame(-1),
        Key::Character("." | "。") => Intent::StepFrame(1),
        // Shift is already part of the character here (a shifted `m` arrives as
        // `"M"`), and the full-width pair is spelled out for the same IME reason
        // as the comma.
        Key::Character("m" | "M" | "ｍ" | "Ｍ") => Intent::ToggleMotion,
        // Fallback for the case the full-width check cannot cover: an IME that
        // consumes the press outright leaves `logical_key` unidentified while the
        // *physical* key is still whatever was pressed. Restricted to the two
        // variants that mean "no logical key", so it can never shadow a key that
        // did identify itself.
        Key::Unidentified(_) | Key::Dead(_) => match press.physical {
            PhysicalKey::Code(KeyCode::Comma) => Intent::StepFrame(-1),
            PhysicalKey::Code(KeyCode::Period) => Intent::StepFrame(1),
            PhysicalKey::Code(KeyCode::KeyM) => Intent::ToggleMotion,
            _ => return None,
        },
        _ => return None,
    };
    if press.repeat && !matches!(intent, Intent::Adjust(_) | Intent::StepFrame(_)) {
        return None;
    }
    Some(intent)
}

/// Lift the vertical amount and its unit out of a winit scroll event.
///
/// This is the same "two-line adapter, no decisions" shape as
/// [`KeyPress::from_event`], and it exists for the same reason: the gesture
/// arithmetic in [`rustim_math::gesture`] can be tested without a window.
///
/// Horizontal scroll is dropped rather than folded in — nothing in the engine
/// pans sideways without a drag, and silently reusing the x axis for zoom would
/// make a two-finger horizontal swipe change the scale.
pub fn wheel_delta(delta: MouseScrollDelta) -> ScrollDelta {
    match delta {
        MouseScrollDelta::LineDelta(_, y) => ScrollDelta::Lines(f64::from(y)),
        MouseScrollDelta::PixelDelta(position) => ScrollDelta::Pixels(position.y),
    }
}

/// Which object the keyboard is currently talking about.
///
/// Kept as an id rather than an index, because ids are what the rest of the
/// engine addresses and because the tree can change between two presses — an
/// index would silently point at a different object.
#[derive(Debug, Default, Clone)]
pub struct FocusState {
    /// The focusable objects, in draw order. Rebuilt by [`Self::sync`].
    order: Vec<NodeId>,
    at: Option<NodeId>,
}

impl FocusState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuild the list of focusable objects from the tree.
    ///
    /// Call before reading or moving the focus. Not `sync` on every frame: it
    /// walks the whole tree, and nothing about the focus list changes unless a
    /// command did.
    ///
    /// An invisible object is not focusable — `opacity == 0.0` means the keyboard
    /// would be pointing at something the user cannot see, and an object with no
    /// picture is not on screen at all. The current focus is kept if it survives,
    /// so a scene that fades objects in does not reset the user's place every
    /// time one appears.
    ///
    /// [`rustim_mobject::Mobject::has_picture`] rather than a point count: a text mobject has no
    /// points — its glyphs come from a shaper — and dropping it here would put the
    /// one object in a text scene that a reader most wants to reach out of reach.
    pub fn sync(&mut self, tree: &SceneTree) {
        self.order.clear();
        for id in tree.draw_order() {
            let Some(m) = tree.get(id) else { continue };
            if m.opacity <= 0.0 || !m.has_picture() {
                continue;
            }
            self.order.push(id);
        }
        if let Some(at) = self.at
            && !self.order.contains(&at)
        {
            self.at = None;
        }
    }

    pub fn focused(&self) -> Option<NodeId> {
        self.at
    }

    /// The ids that can currently be focused, in order.
    pub fn order(&self) -> &[NodeId] {
        &self.order
    }

    /// Point the focus at `id` if it is focusable.
    pub fn set(&mut self, id: NodeId) {
        if self.order.contains(&id) {
            self.at = Some(id);
        }
    }

    /// The next focusable object, wrapping.
    ///
    /// Not `Iterator::next`: there is no sequence to yield and nothing to
    /// consume — this moves a cursor over a list the tree owns, and calling it
    /// twice is a legitimate thing to do rather than a sign of a bug. Hence the
    /// allow; implementing `Iterator` to silence it would be worse.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<NodeId> {
        self.step(1)
    }

    pub fn prev(&mut self) -> Option<NodeId> {
        self.step(-1)
    }

    /// Move `by` places, wrapping, and starting from the first object when
    /// nothing is focused yet — so the first Tab always lands somewhere rather
    /// than requiring two presses.
    fn step(&mut self, by: i32) -> Option<NodeId> {
        if self.order.is_empty() {
            self.at = None;
            return None;
        }
        let len = self.order.len() as i32;
        let next = match self.order.iter().position(|&id| Some(id) == self.at) {
            Some(i) => (i as i32 + by).rem_euclid(len),
            None if by >= 0 => 0,
            None => len - 1,
        };
        self.at = Some(self.order[next as usize]);
        self.at
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustim_mobject::shapes::{ShapeKind, ShapeParams};

    /// A press of `logical`, with an unrelated physical key so that a test
    /// passing for the *logical* reason cannot be passing because of the
    /// physical fallback.
    fn key(logical: Key, state: ElementState, repeat: bool) -> KeyPress {
        KeyPress {
            logical,
            physical: PhysicalKey::Code(KeyCode::KeyA),
            state,
            repeat,
            shift: false,
        }
    }

    fn named(k: NamedKey) -> KeyPress {
        key(Key::Named(k), ElementState::Pressed, false)
    }

    fn ch(s: &str) -> KeyPress {
        key(Key::Character(s.into()), ElementState::Pressed, false)
    }

    #[test]
    fn the_documented_keymap_is_the_one_implemented() {
        assert_eq!(classify(&named(NamedKey::Tab)), Some(Intent::FocusNext));
        assert_eq!(
            classify(&KeyPress {
                shift: true,
                ..named(NamedKey::Tab)
            }),
            Some(Intent::FocusPrev)
        );
        assert_eq!(
            classify(&named(NamedKey::ArrowRight)),
            Some(Intent::Adjust(OPACITY_STEP))
        );
        assert_eq!(
            classify(&named(NamedKey::ArrowUp)),
            Some(Intent::Adjust(OPACITY_STEP))
        );
        assert_eq!(
            classify(&named(NamedKey::ArrowLeft)),
            Some(Intent::Adjust(-OPACITY_STEP))
        );
        assert_eq!(
            classify(&named(NamedKey::ArrowDown)),
            Some(Intent::Adjust(-OPACITY_STEP))
        );
        assert_eq!(
            classify(&named(NamedKey::Space)),
            Some(Intent::TogglePlayback)
        );
        assert_eq!(
            classify(&named(NamedKey::Enter)),
            Some(Intent::AnnounceFocus)
        );
        assert_eq!(classify(&named(NamedKey::Escape)), Some(Intent::Escape));
        assert_eq!(classify(&ch(",")), Some(Intent::StepFrame(-1)));
        assert_eq!(classify(&ch(".")), Some(Intent::StepFrame(1)));
        assert_eq!(classify(&ch("m")), Some(Intent::ToggleMotion));
        // A shifted `m` arrives as the uppercase character, which winit reports
        // as such — `shift` is not consulted for it. The full-width pair is the
        // IME form, for the same reason as the comma.
        assert_eq!(classify(&ch("M")), Some(Intent::ToggleMotion));
        assert_eq!(classify(&ch("ｍ")), Some(Intent::ToggleMotion));
        assert_eq!(classify(&ch("Ｍ")), Some(Intent::ToggleMotion));
        // Anything else is not ours, and saying so is what keeps the classifier
        // from swallowing keys the IME or a future widget needs.
        assert_eq!(classify(&ch("a")), None);
        assert_eq!(classify(&named(NamedKey::F5)), None);
    }

    #[test]
    fn an_ime_that_eats_the_key_still_steps_frames() {
        // The case the full-width check cannot see: the press produced no logical
        // key at all, but it is physically the comma key.
        let eaten = |code: KeyCode| KeyPress {
            physical: PhysicalKey::Code(code),
            logical: Key::Dead(None),
            state: ElementState::Pressed,
            repeat: false,
            shift: false,
        };
        assert_eq!(
            classify(&eaten(KeyCode::Comma)),
            Some(Intent::StepFrame(-1))
        );
        assert_eq!(
            classify(&eaten(KeyCode::Period)),
            Some(Intent::StepFrame(1))
        );
        assert_eq!(classify(&eaten(KeyCode::KeyM)), Some(Intent::ToggleMotion));
        assert_eq!(classify(&eaten(KeyCode::KeyA)), None);
        // ...and it stays out of the way of a key that did identify itself: the
        // physical code here says "comma", the logical key says "Escape".
        let identified = KeyPress {
            logical: Key::Named(NamedKey::Escape),
            ..eaten(KeyCode::Comma)
        };
        assert_eq!(classify(&identified), Some(Intent::Escape));
    }

    #[test]
    fn full_width_punctuation_steps_frames_too() {
        // A Chinese IME in full-width mode sends these, and on this project's own
        // machine that is the default state. An ASCII-only keymap would look
        // correct in every test written in ASCII and do nothing in practice.
        assert_eq!(classify(&ch("，")), Some(Intent::StepFrame(-1)));
        assert_eq!(classify(&ch("。")), Some(Intent::StepFrame(1)));
    }

    #[test]
    fn key_releases_are_not_intents() {
        assert_eq!(
            classify(&key(
                Key::Named(NamedKey::Space),
                ElementState::Released,
                false
            )),
            None
        );
    }

    #[test]
    fn repeats_move_values_but_never_focus_or_playback() {
        let repeat = |k: NamedKey| classify(&key(Key::Named(k), ElementState::Pressed, true));
        // Holding an arrow fades; holding Tab must not ring through the scene.
        assert_eq!(
            repeat(NamedKey::ArrowRight),
            Some(Intent::Adjust(OPACITY_STEP))
        );
        assert_eq!(repeat(NamedKey::Tab), None);
        assert_eq!(repeat(NamedKey::Space), None);
        assert_eq!(repeat(NamedKey::Enter), None);
        assert_eq!(
            classify(&key(
                Key::Character(",".into()),
                ElementState::Pressed,
                true
            )),
            Some(Intent::StepFrame(-1))
        );
        // Holding `m` must not flip the mode thirty times a second.
        assert_eq!(
            classify(&key(
                Key::Character("m".into()),
                ElementState::Pressed,
                true
            )),
            None
        );
    }

    fn tree_with(n: usize) -> SceneTree {
        let mut tree = SceneTree::new();
        for _ in 0..n {
            tree.insert(rustim_mobject::shapes::build(
                ShapeKind::Square,
                &ShapeParams::default(),
            ));
        }
        tree
    }

    #[test]
    fn the_first_tab_lands_on_the_first_object() {
        let tree = tree_with(3);
        let mut focus = FocusState::new();
        focus.sync(&tree);
        assert_eq!(focus.next(), Some(tree.root_ids()[0]));
        assert_eq!(focus.next(), Some(tree.root_ids()[1]));
        assert_eq!(focus.next(), Some(tree.root_ids()[2]));
        // ...and wraps, rather than dead-ending at the last object.
        assert_eq!(focus.next(), Some(tree.root_ids()[0]));
    }

    #[test]
    fn shift_tab_walks_backwards_from_nothing_to_the_last() {
        let tree = tree_with(3);
        let mut focus = FocusState::new();
        focus.sync(&tree);
        assert_eq!(focus.prev(), Some(tree.root_ids()[2]));
        assert_eq!(focus.prev(), Some(tree.root_ids()[1]));
    }

    #[test]
    fn focus_survives_a_sync_that_still_contains_it() {
        let tree = tree_with(3);
        let mut focus = FocusState::new();
        focus.sync(&tree);
        // Twice, deliberately: the first Tab from an empty focus lands on the
        // *first* object (`the_first_tab_lands_on_the_first_object` pins that),
        // so reaching the second takes two presses. Focusing the second rather
        // than the first is what makes "the resync kept it" a real assertion —
        // a resync that reset to the start would produce the first id, not this.
        focus.next();
        focus.next();
        let second = tree.root_ids()[1];
        assert_eq!(focus.focused(), Some(second));
        focus.sync(&tree);
        assert_eq!(
            focus.focused(),
            Some(second),
            "a resync must not steal focus"
        );
    }

    #[test]
    fn an_invisible_object_is_not_focusable_and_losing_focus_is_reported() {
        let mut tree = tree_with(2);
        let mut focus = FocusState::new();
        focus.sync(&tree);
        assert_eq!(focus.next(), Some(tree.root_ids()[0]));
        // The focused object fades out: the focus is released rather than kept
        // pointing at something invisible, and the next Tab starts cleanly.
        let first = tree.root_ids()[0];
        tree.get_mut(first).unwrap().opacity = 0.0;
        focus.sync(&tree);
        assert_eq!(focus.focused(), None);
        assert_eq!(focus.order(), &[tree.root_ids()[1]]);
        assert_eq!(focus.next(), Some(tree.root_ids()[1]));
    }

    #[test]
    fn a_text_object_is_focusable_although_it_has_no_points() {
        // The case a point count gets wrong. A text mobject's geometry is a glyph
        // run that the shaper positioned and the scene pushes, so it has no points
        // at all — and it is the object in a text scene that a reader most wants
        // to reach. `has_picture` is what says so; `points.len() < 2` dropped it.
        let mut tree = SceneTree::new();
        let text = tree.insert(
            rustim_mobject::Mobject::new(Vec::new())
                .with_kind(rustim_mobject::NodeKind::Text)
                .with_label("勾股定理"),
        );
        let square = tree.insert(rustim_mobject::shapes::build(
            ShapeKind::Square,
            &ShapeParams::default(),
        ));
        let mut focus = FocusState::new();
        focus.sync(&tree);
        assert_eq!(focus.order(), &[text, square]);
        assert_eq!(focus.next(), Some(text));
    }

    #[test]
    fn an_empty_scene_has_nowhere_to_focus() {
        let tree = SceneTree::new();
        let mut focus = FocusState::new();
        focus.sync(&tree);
        assert_eq!(focus.next(), None);
        assert_eq!(focus.prev(), None);
        assert_eq!(focus.focused(), None);
    }

    #[test]
    fn focus_cannot_be_set_to_a_node_that_is_not_focusable() {
        let mut tree = tree_with(1);
        let mut focus = FocusState::new();
        focus.sync(&tree);
        let id = tree.root_ids()[0];
        let other = tree.insert(rustim_mobject::shapes::build(
            ShapeKind::Circle,
            &ShapeParams::default(),
        ));
        // `other` was inserted after the sync and is not in the list yet.
        focus.set(other);
        assert_eq!(focus.focused(), None);
        focus.set(id);
        assert_eq!(focus.focused(), Some(id));
    }

    // ---- pointer input -------------------------------------------------

    /// Winit's two scroll shapes keep their unit through the adapter; the
    /// arithmetic on them lives in `rustim_math::gesture` and is tested there.
    #[test]
    fn winit_scrolls_keep_their_unit() {
        use winit::dpi::PhysicalPosition;
        assert_eq!(
            wheel_delta(MouseScrollDelta::LineDelta(0.0, 2.0)),
            ScrollDelta::Lines(2.0)
        );
        assert_eq!(
            wheel_delta(MouseScrollDelta::PixelDelta(PhysicalPosition::new(
                3.0, -7.5
            ))),
            ScrollDelta::Pixels(-7.5)
        );
    }
}
