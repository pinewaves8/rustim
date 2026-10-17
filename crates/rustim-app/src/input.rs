//! Input, as pure state and arithmetic over window events.
//!
//! Two families live here. The **keyboard** maps a key press to an [`Intent`] and
//! tracks which object the focus is on ([`FocusState`]). The **pointer** turns a
//! drag into a pan delta ([`Drag`]) and a scroll into a multiplicative zoom factor
//! ([`WheelDelta`], [`zoom_factor`], [`clamped_zoom_factor`]). The wheel's factor
//! is applied by `rustim_math::Camera::panned` / `zoomed_about`; the clamp here is
//! the host's range, not the kernel's.
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

use rustim_math::Camera;
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

/// A wheel or trackpad scroll, reduced to the one axis zoom reads.
///
/// winit reports two shapes of scroll and they are not convertible into one
/// another — a wheel emits *notches* (`LineDelta`) while a trackpad emits
/// *pixels* (`PixelDelta`), and the number of pixels in a notch is an OS
/// setting, not a constant. So the delta keeps its unit and the conversion to a
/// zoom factor happens in [`zoom_factor`], where each unit gets its own rate.
///
/// The sign convention is winit's: positive `y` is a scroll away from the user,
/// which is zoom in. Horizontal scroll is dropped rather than folded in —
/// nothing in the engine pans sideways without a drag, and silently reusing the
/// x axis for zoom would make a two-finger horizontal swipe change the scale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WheelDelta {
    /// Wheel notches (`MouseScrollDelta::LineDelta`).
    Lines(f64),
    /// Platform pixels (`MouseScrollDelta::PixelDelta`).
    Pixels(f64),
}

impl WheelDelta {
    /// Lift the vertical amount and its unit out of a winit scroll event.
    ///
    /// This is the same "two-line adapter, no decisions" shape as
    /// [`KeyPress::from_event`], and it exists for the same reason: the
    /// arithmetic above it can be tested without a window.
    pub fn from_winit(delta: MouseScrollDelta) -> Self {
        match delta {
            MouseScrollDelta::LineDelta(_, y) => Self::Lines(f64::from(y)),
            MouseScrollDelta::PixelDelta(position) => Self::Pixels(position.y),
        }
    }

    /// The vertical amount, in this variant's own unit.
    pub fn y(self) -> f64 {
        match self {
            Self::Lines(y) | Self::Pixels(y) => y,
        }
    }
}

/// Zoom change per wheel notch: one notch is `e^0.25 ≈ 1.284`, about 28%.
pub const WHEEL_ZOOM_PER_LINE: f64 = 0.25;

/// Zoom change per platform pixel of trackpad scroll.
///
/// A trackpad sends many small events where a wheel sends one, so the rate is
/// correspondingly smaller; 100 px of two-finger travel is `e^0.2 ≈ 1.22`.
pub const WHEEL_ZOOM_PER_PIXEL: f64 = 0.002;

/// The host's zoom range. `0.05` shows 160 world units, `64` shows an eighth.
///
/// The kernel accepts any finite positive zoom — a script may ask for a
/// specific view — so this clamp belongs to the pointer, not to the camera: it
/// is the range in which a *gesture* remains reversible. Without it a stuck
/// wheel reaches the floating point floor and the scene is gone.
pub const MIN_ZOOM: f64 = 0.05;
/// See [`MIN_ZOOM`].
pub const MAX_ZOOM: f64 = 64.0;

/// A scroll's zoom **factor** — multiplicative, not additive.
///
/// `exp` is the whole reason: equal scroll steps multiply the zoom by equal
/// ratios, so ten notches in and ten out return to exactly where the user
/// started (up to rounding), and the gesture feels the same at every scale.
/// Adding a constant would make a notch a huge jump when zoomed out and an
/// invisible one when zoomed in.
///
/// A non-finite amount returns `1.0`, the identity, rather than poisoning the
/// camera — the same posture [`Drag`] has toward malformed positions.
pub fn zoom_factor(delta: WheelDelta) -> f64 {
    let rate = match delta {
        WheelDelta::Lines(_) => WHEEL_ZOOM_PER_LINE,
        WheelDelta::Pixels(_) => WHEEL_ZOOM_PER_PIXEL,
    };
    let exponent = delta.y() * rate;
    if exponent.is_finite() {
        exponent.exp()
    } else {
        1.0
    }
}

/// Re-express `factor` so that `current * factor` lands inside the host range.
///
/// Returns `1.0` — no zoom — for a camera or factor the multiplication cannot
/// survive, and when the camera is already at the limit in the direction the
/// scroll asks for. The result is a factor, not a zoom, because
/// [`Camera::zoomed_about`](rustim_math::Camera::zoomed_about) is what applies
/// it and it must keep the point under the cursor invariant.
pub fn clamped_zoom_factor(current: f64, factor: f64) -> f64 {
    if !current.is_finite() || current <= 0.0 || !factor.is_finite() || factor <= 0.0 {
        return 1.0;
    }
    (current * factor).clamp(MIN_ZOOM, MAX_ZOOM) / current
}

/// The camera a new pointer delta applies to.
///
/// A cursor can move several times between two frames — a 1000 Hz mouse, a slow
/// frame — and each delta has to continue from the **previous gesture's**
/// result, not from the camera still on screen. Reading the tree every time
/// computes every move of the frame from the same starting point and keeps only
/// the last, so a fast drag trails the pointer. `pending` is `None` once the
/// frame has applied the commands the gesture produced.
pub fn gesture_base(pending: Option<Camera>, live: Camera) -> Camera {
    pending.unwrap_or(live)
}

/// The left button's drag state, as much as panning needs.
///
/// `last` is **not** cleared on [`Self::press`]: the last cursor position winit
/// reported is normally the position of the press itself, so keeping it means
/// the first movement after the button goes down produces a correct delta
/// instead of being swallowed as the anchor. It is cleared on release, because
/// a drag that ends off-window must not leave a stale anchor for the next one.
#[derive(Debug, Default, Clone, Copy)]
pub struct Drag {
    active: bool,
    last: Option<(f64, f64)>,
}

impl Drag {
    pub fn new() -> Self {
        Self::default()
    }

    /// The button went down.
    pub fn press(&mut self) {
        self.active = true;
    }

    /// The button came up, or focus was lost.
    pub fn release(&mut self) {
        self.active = false;
        self.last = None;
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Feed one cursor position; answer the screen-space delta to pan by.
    ///
    /// `None` when the button is not down, or when this is the first position
    /// the drag has seen — there is nothing to subtract from yet, and guessing
    /// would jump the scene by an arbitrary amount.
    pub fn moved(&mut self, at: (f64, f64)) -> Option<(f64, f64)> {
        let previous = self.last.replace(at);
        if !self.active {
            return None;
        }
        let (x0, y0) = previous?;
        Some((at.0 - x0, at.1 - y0))
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
    use rustim_math::Viewport;
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

    /// A drag reports a delta only while the button is held, and the delta is
    /// the pointer's own screen-space travel.
    #[test]
    fn a_drag_pans_only_while_the_button_is_down() {
        let mut drag = Drag::new();
        assert_eq!(drag.moved((100.0, 100.0)), None, "not dragging yet");
        assert_eq!(drag.moved((110.0, 105.0)), None);

        drag.press();
        // The press did not clear the anchor, so the first move after it is
        // measured from where the pointer already was.
        assert_eq!(drag.moved((120.0, 100.0)), Some((10.0, -5.0)));
        assert!(drag.is_active());

        drag.release();
        assert_eq!(drag.moved((200.0, 200.0)), None);
        assert!(!drag.is_active());
    }

    /// A drag that goes down before winit has reported a position has no
    /// anchor; the first movement establishes one instead of panning by the
    /// distance from the origin.
    #[test]
    fn a_drag_with_no_previous_position_does_not_jump() {
        let mut drag = Drag::new();
        drag.press();
        assert_eq!(drag.moved((640.0, 360.0)), None);
        assert_eq!(drag.moved((641.0, 361.0)), Some((1.0, 1.0)));
    }

    /// A gesture in the same frame as an earlier one continues from it, so a
    /// burst of cursor positions is not collapsed to the last delta.
    #[test]
    fn a_gesture_continues_from_the_previous_one_within_a_frame() {
        let live = Camera::default();
        let pending = Camera::new((-1.0, 0.5), 2.0);
        assert_eq!(gesture_base(None, live), live);
        assert_eq!(gesture_base(Some(pending), live), pending);
        // Two 90 px deltas applied to the pending camera add up — at zoom 2 the
        // scale is 180 px/unit, so each is half a world unit, not the whole.
        let vp = Viewport::default();
        let step = pending.panned(&vp, (90.0, 0.0));
        let twice = gesture_base(Some(step), live).panned(&vp, (90.0, 0.0));
        assert_eq!(twice.center, (-2.0, 0.5));
    }

    /// Wheel and trackpad deltas keep their unit through the adapter.
    #[test]
    fn winit_scrolls_keep_their_unit() {
        use winit::dpi::PhysicalPosition;
        assert_eq!(
            WheelDelta::from_winit(MouseScrollDelta::LineDelta(0.0, 2.0)),
            WheelDelta::Lines(2.0)
        );
        assert_eq!(
            WheelDelta::from_winit(MouseScrollDelta::PixelDelta(PhysicalPosition::new(
                3.0, -7.5
            ))),
            WheelDelta::Pixels(-7.5)
        );
    }

    /// Zoom is multiplicative: two notches are one notch squared, in either
    /// unit, and scrolling back out cancels the scroll in.
    #[test]
    fn wheel_zoom_is_multiplicative() {
        let one = zoom_factor(WheelDelta::Lines(1.0));
        let two = zoom_factor(WheelDelta::Lines(2.0));
        assert!((two - one * one).abs() < 1e-12);
        assert!(one > 1.0, "a notch away from the user zooms in");

        let pixel = zoom_factor(WheelDelta::Pixels(125.0));
        assert!(pixel > 1.0);
        assert!(zoom_factor(WheelDelta::Pixels(-125.0)) < 1.0);

        // In and out by the same amount returns to 1.0 to within rounding.
        let round_trip = zoom_factor(WheelDelta::Lines(3.0)) * zoom_factor(WheelDelta::Lines(-3.0));
        assert!((round_trip - 1.0).abs() < 1e-15, "{round_trip}");
    }

    #[test]
    fn a_malformed_scroll_is_the_identity() {
        assert_eq!(zoom_factor(WheelDelta::Lines(f64::NAN)), 1.0);
        assert_eq!(zoom_factor(WheelDelta::Pixels(f64::INFINITY)), 1.0);
    }

    /// The host clamp is reached from both directions and never overshoots.
    #[test]
    fn the_pointer_zoom_is_clamped_to_its_range() {
        assert_eq!(clamped_zoom_factor(1.0, 2.0), 2.0);
        // At the top, zooming in is a no-op...
        let at_top = clamped_zoom_factor(MAX_ZOOM, 2.0);
        assert_eq!(at_top, 1.0);
        assert_eq!(MAX_ZOOM * at_top, MAX_ZOOM);
        // ...and from just below it, the factor is cut to land exactly on the
        // limit rather than beyond it.
        let near = clamped_zoom_factor(MAX_ZOOM / 2.0, 4.0);
        assert_eq!(near, 2.0);
        assert_eq!(clamped_zoom_factor(MIN_ZOOM, 0.5), 1.0);
        let near_bottom = clamped_zoom_factor(MIN_ZOOM * 2.0, 0.25);
        assert_eq!(near_bottom, 0.5);
        // Nonsense in either argument is "do not zoom".
        assert_eq!(clamped_zoom_factor(0.0, 2.0), 1.0);
        assert_eq!(clamped_zoom_factor(-1.0, 2.0), 1.0);
        assert_eq!(clamped_zoom_factor(1.0, f64::NAN), 1.0);
        assert_eq!(clamped_zoom_factor(1.0, 0.0), 1.0);
    }
}
