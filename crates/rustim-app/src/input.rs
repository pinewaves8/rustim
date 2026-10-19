//! Input, as pure state and arithmetic over window events.
//!
//! **Both vocabularies here are shared with the browser host, and neither is
//! defined here.** The keymap — [`Key`], [`KeyPress`], [`Intent`] and
//! [`classify`] — lives in [`rustim_scene::input`], and the pan/zoom algebra in
//! [`rustim_math::gesture`]. What stays in this module is the winit side of both:
//! [`from_event`] and [`wheel_delta`], two adapters that convert a winit event
//! shape into those neutral ones, plus [`FocusState`], which is pure node ids.
//!
//! That split is what makes the browser host a peer rather than a second
//! implementation. The keymap has to be shared for the same reason the gesture
//! arithmetic is: `space` means play-or-pause in both hosts, or the engine has two
//! keyboards. And it has to live *below* this crate rather than here, because
//! `winit` has no wasm32 build — a keymap that named winit's types could not be
//! reached from the browser at all.
//!
//! The engine had neither subsystem at first: `window_event` matched
//! `CloseRequested`, `Resized` and `RedrawRequested` and dropped everything else.
//! §4.6's fourth requirement — Tab to move between objects, arrow keys to adjust
//! one, space to play or pause, `,`/`.` to step a frame — and the manual camera
//! controls were added later, in that order.
//!
//! # Why the adapters are separate from the decisions
//!
//! Nothing below the adapters knows winit exists, and [`FocusState`] knows nothing
//! but node ids. That is what lets the keymap and the gesture arithmetic be tested
//! in CI, where there is no window, no display and no accessibility bus — and this
//! is exactly the layer whose bugs are invisible in a screenshot.
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

// Two vocabularies are shared with the browser host (`rustim-web`), so each has
// one implementation rather than two that drift: the gesture algebra, in
// `rustim_math::gesture`, and the keymap, in `rustim_scene::input`. What stays
// here is the winit side of both — the adapters — and the host's event wiring.
pub use rustim_math::gesture::{Drag, Pointer, ScrollDelta, clamped_zoom_factor, zoom_factor};
use rustim_mobject::NodeId;
use rustim_scene::SceneTree;
pub use rustim_scene::input::{Code, Intent, Key, KeyPress, OPACITY_STEP, classify};
use winit::event::{ElementState, KeyEvent, MouseScrollDelta};
use winit::keyboard::{Key as WinitKey, KeyCode, ModifiersState, NamedKey, PhysicalKey};

/// Lift the fields the keymap reads out of a winit event and the window's
/// current modifier state.
///
/// This is the adapter the shared vocabulary exists for, and it is deliberately
/// the *only* part of the desktop keymap: every branch is a spelling conversion,
/// and the decision — which key means what — happens in
/// [`rustim_scene::input::classify`], where the browser host can reach it too.
/// A `KeyEvent` cannot be constructed outside winit, so this is also the one
/// function in the keymap no test can call, which is another reason for it to
/// contain nothing worth testing.
pub fn from_event(event: &KeyEvent, mods: ModifiersState) -> KeyPress {
    KeyPress {
        key: match &event.logical_key {
            WinitKey::Named(NamedKey::Tab) => Key::Tab,
            WinitKey::Named(NamedKey::ArrowUp) => Key::ArrowUp,
            WinitKey::Named(NamedKey::ArrowDown) => Key::ArrowDown,
            WinitKey::Named(NamedKey::ArrowLeft) => Key::ArrowLeft,
            WinitKey::Named(NamedKey::ArrowRight) => Key::ArrowRight,
            WinitKey::Named(NamedKey::Space) => Key::Space,
            WinitKey::Named(NamedKey::Enter) => Key::Enter,
            WinitKey::Named(NamedKey::Escape) => Key::Escape,
            // Exactly one character, which is every binding the keymap has. A
            // multi-character key — a ligature, a compose sequence — is not one of
            // ours, so it becomes `Other` and the physical fallback gets its say.
            WinitKey::Character(s) => {
                let mut chars = s.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => Key::Char(c),
                    _ => Key::Other,
                }
            }
            // `Unidentified` and `Dead` both mean "no logical key", which is the
            // case the physical fallback exists for.
            _ => Key::Other,
        },
        code: match event.physical_key {
            PhysicalKey::Code(KeyCode::Comma) => Code::Comma,
            PhysicalKey::Code(KeyCode::Period) => Code::Period,
            PhysicalKey::Code(KeyCode::KeyM) => Code::KeyM,
            _ => Code::Other,
        },
        pressed: event.state == ElementState::Pressed,
        repeat: event.repeat,
        shift: mods.contains(ModifiersState::SHIFT),
    }
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
