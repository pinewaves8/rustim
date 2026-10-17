//! The AccessKit adapter — the only module in the workspace that names
//! `accesskit`.
//!
//! # Layering
//!
//! [`rustim_scene::a11y`] already projects the scene graph into a
//! [`SemanticTree`]: plain, serialisable data with no platform types in it. This
//! module turns that data into `accesskit::Node`s and hands them to the winit
//! adapter. Splitting it that way is not tidiness — `rustim-scene` is one of the
//! six crates that must compile to `wasm32-unknown-unknown`, and the Web target
//! will not use AccessKit at all (there is no official web backend; a browser
//! build goes through DOM/ARIA). The two targets share the semantic data, not
//! the tree.
//!
//! # Why there are two types here
//!
//! [`A11y`] owns an `accesskit_winit::Adapter`, and an adapter cannot be built
//! without an `ActiveEventLoop` — so nothing that touches one can be tested.
//! Everything with logic in it therefore lives in [`A11yCore`]: the focus, the
//! announcement throttle, the action translation, and the projection to a
//! `TreeUpdate`. `A11y` is a wrapper that holds the adapter and forwards. The
//! split is what makes `full_update` and `on_action` reachable from
//! `cargo test --workspace`, which is the whole reason the crate grew a library
//! target.
//!
//! # Facts that dictated the shape of this file
//!
//! Each of these was read out of the vendored sources rather than remembered,
//! and each of them silently breaks the accessibility layer when ignored:
//!
//! - **There is no `NodeBuilder`.** `Node::new(role)` plus setters.
//! - **The accessible name property is `label`, not `name`** — but a node whose
//!   role is `Role::Label` has its name taken from `value` instead, because the
//!   backends compute `if node.label_comes_from_value() { value } else { label }`
//!   and `label_comes_from_value()` is exactly `role == Role::Label`. So the
//!   announcement node must **not** be a `Label`, and a text object must put its
//!   text in `value`. `set_label` on a `Role::Label` node is silently ignored on
//!   macOS and Windows alike.
//! - **`Adapter::with_event_loop_proxy` cannot return an initial tree**, so the
//!   *first* update a consumer receives has to be a complete snapshot
//!   (`tree: Some(..)`). Every later one is a difference from it, and the two
//!   reset points that make that safe are `InitialTreeRequested` and
//!   `AccessibilityDeactivated`; see [`A11yCore::invalidate`].
//! - **`update_if_active`'s closure is not called at all when no assistive
//!   technology is listening** — all three backends return before invoking it in
//!   their inactive arm. So the projection and the diff cost nothing unattached,
//!   and the push counters read zero, which is correct rather than broken.
//! - **An incremental update must satisfy the consumer's relationship rules**,
//!   not merely look complete: a node's parent must arrive in the same update
//!   when a child is added or removed, a removed node must not be named, and
//!   `focus` must be present every time and must still exist. [`A11yCore::to_update`]
//!   states the rules, and the tests replay every update through
//!   `accesskit_consumer` — the implementation the platform backends themselves
//!   use — because no assertion over our own `TreeUpdate` can prove them.
//!
//! # Focus: why the app announces it and an AT does not
//!
//! `TreeUpdate::focus` is what tells the platform where the reader's focus is.
//! When an assistive technology sends `Action::Focus`, it moved the focus itself
//! and will echo it, so pushing text into the live region as well would make
//! every navigation speak twice. When the *keyboard* moves the focus, the app has
//! to say so itself, because nothing else will. That asymmetry is deliberate.
//!
//! It is also **unverified on a real screen reader**: this machine has no AT-SPI
//! session bus and no reader, so all that can be checked here is that the tree is
//! well-formed and that actions round-trip into commands. Verifying the audible
//! result needs a machine with a reader; see the manual checklist in `CLAUDE.md`.

use std::time::Instant;

use accesskit::{
    Action, ActionData, ActionRequest, Live, Node, NodeId as AkNodeId, Role, TreeId, TreeInfo,
    TreeUpdate,
};
use accesskit_winit::Adapter;
use rustim_mobject::NodeId;
use rustim_scene::command::{Command, MotionMode, Property};
use rustim_scene::{SceneContext, SemanticNode, SemanticRole, SemanticTree};
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::window::Window;

use crate::announce::{Announcer, Narrator};
use crate::input::{FocusState, OPACITY_STEP};

/// The window, which is the root of the accessibility tree.
///
/// Zero is not a scene id: `slotmap` never hands out raw 0 at all — a zero
/// generation is rewritten to 1 by `KeyData::from_ffi`, so the smallest real id
/// is `0x1_0000_0001` — which is what makes small integers free for nodes that
/// are not mobjects.
pub const ROOT_NODE: AkNodeId = AkNodeId(0);

/// The announcement live region.
pub const LIVE_NODE: AkNodeId = AkNodeId(1);

/// Something an assistive technology asked for, translated out of AccessKit.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    /// Apply this to the scene — the same command an AI or a host would send,
    /// through the same validation. So an assistive technology cannot ask for a
    /// state the engine would refuse: there is only one command layer.
    ///
    /// Boxed because `Command` carries a whole `ShapeParams` (a quarter of a
    /// kilobyte of optional fields) and this enum is returned by value from an
    /// action handler. `Request::TogglePlayback` is two bytes; without the box
    /// every request — including that one — would be a command-sized copy.
    Command(Box<Command>),
    /// Start or stop the animation clock. Not a scene command, because whether
    /// the clock runs is a property of the *loop*, not of the scene graph.
    TogglePlayback,
}

/// Everything about accessibility that does not need a window.
///
/// See the module docs for why this is separate from [`A11y`].
pub struct A11yCore {
    /// The text currently on the live-region node. A field rather than a
    /// recomputed value because it *is* the node's state: a backend announces
    /// when the label changes, so re-writing the same string announces nothing
    /// (useful — it is free de-duplication) and writing an empty string after
    /// every announcement would make every repeat a change and defeat the
    /// throttle.
    live: String,
    announcer: Announcer,
    focus: FocusState,
    /// The exact tree last handed to a backend, and the window title that went
    /// with it. `None` means the next update must be a **full** one: nothing has
    /// been sent yet, or [`A11yCore::invalidate`] has declared whatever was sent
    /// no longer applies.
    sent: Option<Sent>,
    /// How many updates of each kind have gone out. They count inside the
    /// adapter's `update_if_active` closure, so with no assistive technology
    /// attached both stay at zero — which is the correct reading, not a bug.
    full_pushes: usize,
    incremental_pushes: usize,
}

/// One snapshot that reached a backend, kept only to be diffed against.
struct Sent {
    tree: SemanticTree,
    title: String,
}

impl Default for A11yCore {
    fn default() -> Self {
        Self::new()
    }
}

impl A11yCore {
    pub fn new() -> Self {
        Self {
            live: String::new(),
            announcer: Announcer::new(),
            focus: FocusState::new(),
            sent: None,
            full_pushes: 0,
            incremental_pushes: 0,
        }
    }

    /// Forget what was last sent, so the next update is a complete snapshot.
    ///
    /// Required when the platform may have dropped our tree — it activates a
    /// fresh observer, or a deactivation/reactivation cycle replaced its state —
    /// because an incremental update describes a *difference from what the
    /// consumer holds*, and after a reset it holds nothing. `main` calls this on
    /// `InitialTreeRequested` and `AccessibilityDeactivated`; see the notes at
    /// those sites for why winit lets that ordering be trusted.
    pub fn invalidate(&mut self) {
        self.sent = None;
    }

    /// How many complete snapshots have been handed to a backend.
    pub fn full_pushes(&self) -> usize {
        self.full_pushes
    }

    /// How many updates carried only what changed.
    pub fn incremental_pushes(&self) -> usize {
        self.incremental_pushes
    }

    /// The focused node, if the keyboard or an AT has put it somewhere.
    pub fn focused(&self) -> Option<NodeId> {
        self.focus.focused()
    }

    /// The text the live region currently carries.
    pub fn live_text(&self) -> &str {
        &self.live
    }

    /// Release any announcement that is due at `now`.
    pub fn flush(&mut self, now: Instant) {
        self.announcer.flush(
            now,
            &mut LiveSink {
                out: &mut self.live,
            },
        );
    }

    /// Report a semantic milestone: something happened that a reader needs to
    /// know and that no other channel will carry.
    ///
    /// Cautious by construction. A property change is *not* a milestone — the
    /// platform reports those itself through the node's value, and routing them
    /// here would make every frame of an animation speakable. The two producers
    /// this engine has are a focus the *app* moved (as opposed to one an
    /// assistive technology moved, which it already knows about) and an
    /// animation reaching its end.
    pub fn offer(&mut self, text: impl Into<String>) {
        self.announcer.offer(text);
    }

    /// Project the scene into an AccessKit update — complete the first time,
    /// differential afterwards.
    ///
    /// Takes the context as an argument rather than holding it, so the borrow of
    /// `self` and the borrow the adapter needs stay separate.
    ///
    /// # What a differential update may and may not say
    ///
    /// AccessKit applies an update to the tree the consumer already holds, so
    /// the rules are about *relationships*, not about the picture:
    ///
    /// - a node is resent only when its semantic data changed, which includes its
    ///   `children` list — so a child added, removed or reordered necessarily
    ///   brings its parent along in the same update;
    /// - a removed node is never named, only dropped from its parent's new
    ///   children list;
    /// - `focus` is sent on **every** update, because the consumer validates it
    ///   against the tree it holds, and a focus left over from a node the scene
    ///   has since deleted would be refused — hence the release below, which
    ///   happens *before* projection;
    /// - the window node carries the title and the top-level children, so it is
    ///   resent when either changes;
    /// - the live region is resent when its text changes, including when it
    ///   changes *to nothing* (an empty label, not an omission — omitting it
    ///   would leave the old announcement spoken forever).
    ///
    /// The saving is real for a still scene and for small edits, and it is not
    /// much for a camera move: every node's pixel bounds change, so every node
    /// is resent. That is the honest boundary — this optimises *semantic* change,
    /// not motion.
    pub fn to_update(&mut self, ctx: &SceneContext, title: &str) -> TreeUpdate {
        // A focus on a node the scene has deleted has to be given up here. The
        // keyboard and the assistive technology only resync on their own events,
        // and a `FadeOut` can remove the focused node while nobody is pressing
        // anything; leaving the id in place makes the consumer's global
        // validation reject the whole update.
        if let Some(id) = self.focus.focused()
            && ctx.tree.get(id).is_none()
        {
            self.focus = FocusState::new();
        }
        let live = if self.live.is_empty() {
            None
        } else {
            Some(self.live.clone())
        };
        let semantic = ctx.semantic_tree(live);

        let update = match self.sent.take() {
            None => {
                self.full_pushes += 1;
                full_update(&semantic, title, self.focus.focused())
            }
            Some(prev) => {
                self.incremental_pushes += 1;
                incremental_update(&prev, &semantic, title, self.focus.focused())
            }
        };
        self.sent = Some(Sent {
            tree: semantic,
            title: title.to_string(),
        });
        update
    }

    // -----------------------------------------------------------------------
    // Interaction, shared by the keyboard and assistive technology
    // -----------------------------------------------------------------------

    /// Move the focus one object forward or backward, and say where it landed.
    ///
    /// The sync walks the tree, so this is per keypress and not per frame.
    pub fn focus_step(&mut self, by: i32, ctx: &SceneContext) -> Option<NodeId> {
        self.focus.sync(&ctx.tree);
        let landed = if by >= 0 {
            self.focus.next()
        } else {
            self.focus.prev()
        };
        if landed.is_some() {
            self.speak_focus(ctx);
        }
        landed
    }

    /// Drop the focus.
    pub fn focus_clear(&mut self) {
        self.focus = FocusState::new();
    }

    /// Read the focused object aloud, on request.
    ///
    /// This is Enter. It is genuinely needed — the platform announced the object
    /// when the focus arrived, and this is the user asking again — and it goes
    /// through the throttle like everything else, so holding Enter does not
    /// stutter.
    pub fn announce_focus(&mut self, ctx: &SceneContext) {
        self.speak_focus(ctx);
    }

    /// Adjust the focused object's opacity and return the command that does it.
    ///
    /// Returned rather than applied, so the keyboard path and the assistive path
    /// both end up in `main`'s single queue of commands and neither can mutate
    /// the tree behind the command layer's back.
    pub fn adjust(&mut self, delta: f64, ctx: &SceneContext) -> Option<Command> {
        let target = self.focus.focused()?;
        let current = ctx.tree.get(target)?.opacity;
        // Clamped here rather than left to the command layer, which would
        // *refuse* an out-of-range value: an arrow key at the end of the range
        // should be a no-op, not an error message.
        let value = (current + delta).clamp(0.0, 1.0);
        Some(Command::Set {
            target,
            property: Property::Opacity(value),
        })
    }

    /// Set the focus directly, as an assistive technology does.
    pub fn focus_on(&mut self, id: NodeId, ctx: &SceneContext) {
        self.focus.sync(&ctx.tree);
        self.focus.set(id);
    }

    /// Translate an assistive technology's action into something the app can do.
    ///
    /// A missing or stale target is **not** checked here. `NodeId::from_raw`
    /// accepts any `u64`, so an out-of-date assistive technology can name a
    /// deleted node and the command layer answers `CommandError::NoSuchNode` —
    /// the refusal-as-a-value rule from `CLAUDE.md`. Validating here would mean
    /// two places deciding what exists.
    pub fn on_action(&mut self, req: &ActionRequest, ctx: &SceneContext) -> Option<Request> {
        match req.action {
            Action::Focus => {
                // The AT moved the focus, so it knows; no announcement from us.
                self.focus_on(NodeId::from_raw(req.target_node.0), ctx);
                None
            }
            Action::Increment => Self::command(self.adjust(OPACITY_STEP, ctx)),
            Action::Decrement => Self::command(self.adjust(-OPACITY_STEP, ctx)),
            Action::SetValue => match req.data {
                Some(ActionData::NumericValue(v)) => {
                    let target = self.focus.focused()?;
                    Some(Request::Command(Box::new(Command::Set {
                        target,
                        property: Property::Opacity(v.clamp(0.0, 1.0)),
                    })))
                }
                _ => None,
            },
            Action::Click => Some(Request::TogglePlayback),
            // The `ActionHandler` contract is "do nothing if unsupported", and
            // naming the fallback is what stops a later reader from assuming
            // every action in that enum is wired up.
            _ => None,
        }
    }

    /// Wrap a command in the box [`Request::Command`] carries.
    fn command(cmd: Option<Command>) -> Option<Request> {
        cmd.map(|c| Request::Command(Box::new(c)))
    }

    /// Queue an announcement of wherever the focus is, if it is anywhere.
    fn speak_focus(&mut self, ctx: &SceneContext) {
        let Some(id) = self.focus.focused() else {
            return;
        };
        if let Some(text) = describe_target(ctx, id) {
            self.announcer.offer(text);
        }
    }
}

/// The announcement text for a focused object.
///
/// Name then description, separated by a comma because the description is a full
/// phrase ("边长为 3 的正方形，面积为 9") that would otherwise run into the name
/// without a pause.
pub fn speak_of(node: &SemanticNode) -> String {
    if node.description.is_empty() {
        node.name.clone()
    } else {
        format!("{}，{}", node.name, node.description)
    }
}

/// What the `m` key says after it flips the motion mode.
///
/// A function rather than a literal at the call site so the wording is reachable
/// from a test: the live region is the only channel a blind user has for a
/// setting that changes nothing they can otherwise hear.
pub fn motion_announcement(mode: MotionMode) -> &'static str {
    match mode {
        MotionMode::Reduced => "动效：减弱",
        MotionMode::Full => "动效：完整",
    }
}

fn describe_target(ctx: &SceneContext, id: NodeId) -> Option<String> {
    let semantic = ctx.semantic_tree(None);
    let node = semantic.get(id)?;
    Some(speak_of(node))
}

/// Writes announcement text into the live-region node's label.
struct LiveSink<'a> {
    out: &'a mut String,
}

impl Narrator for LiveSink<'_> {
    fn announce(&mut self, text: &str) {
        self.out.clear();
        self.out.push_str(text);
    }
}

/// Project a [`SemanticTree`] into a **complete** AccessKit update.
///
/// Pure and total: the one part of this module that was testable even before the
/// core was split out, which is why it is a free function rather than a method.
/// This is the update that carries `tree: Some(..)` — the first one after a
/// consumer attaches, and the one after [`A11yCore::invalidate`].
pub fn full_update(s: &SemanticTree, title: &str, focused: Option<NodeId>) -> TreeUpdate {
    let mut nodes: Vec<(AkNodeId, Node)> = Vec::with_capacity(s.nodes.len() + 2);
    nodes.push((ROOT_NODE, window_node(s, title)));
    for n in s.nodes.iter().filter(|n| n.id != s.root) {
        nodes.push((AkNodeId(n.id.raw()), node_of(n)));
    }
    // Always present, even with nothing to say: a live region that appears and
    // disappears generates a node-added event, which on macOS speaks the label —
    // an announcement nobody asked for, at startup.
    nodes.push((LIVE_NODE, live_node(s)));

    TreeUpdate {
        nodes,
        tree: Some(TreeInfo {
            root: ROOT_NODE,
            toolkit_name: Some("rustim".to_string()),
            toolkit_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        }),
        tree_id: TreeId::ROOT,
        focus: focus_of(focused),
    }
}

/// The AccessKit node id the update should focus.
fn focus_of(focused: Option<NodeId>) -> AkNodeId {
    focused.map_or(ROOT_NODE, |f| AkNodeId(f.raw()))
}

/// The ids the semantic tree's own root holds, in draw order.
///
/// The semantic root is a container, not a node: AccessKit's root is the
/// *window*, so the window node takes over its role and its children are what
/// says which objects belong at the top.
fn top_children(s: &SemanticTree) -> Vec<NodeId> {
    s.nodes
        .iter()
        .find(|n| n.id == s.root)
        .map(|n| n.children.clone())
        .unwrap_or_default()
}

/// The window node: title, top-level objects, and the live region last.
fn window_node(s: &SemanticTree, title: &str) -> Node {
    let mut root = Node::new(Role::Window);
    root.set_label(title.to_string());
    root.set_children(
        top_children(s)
            .into_iter()
            .map(|c| AkNodeId(c.raw()))
            .chain(std::iter::once(LIVE_NODE))
            .collect::<Vec<_>>(),
    );
    root
}

/// The announcement node, which carries the live text or an empty label.
fn live_node(s: &SemanticTree) -> Node {
    let mut live = Node::new(Role::Status);
    live.set_live(Live::Polite);
    live.set_live_atomic();
    live.set_label(s.live.clone().unwrap_or_default());
    live
}

/// The difference between the last snapshot a backend received and this one.
///
/// Rules, in the order they matter — see [`A11yCore::to_update`] for why:
/// changed nodes only; parents ride along because a changed `children` list is
/// a change; the window node on a title or top-level change; the live region on
/// a text change; and `focus` every time.
fn incremental_update(
    prev: &Sent,
    s: &SemanticTree,
    title: &str,
    focused: Option<NodeId>,
) -> TreeUpdate {
    let mut nodes = Vec::new();

    if top_children(&prev.tree) != top_children(s) || prev.title != title {
        nodes.push((ROOT_NODE, window_node(s, title)));
    }
    if prev.tree.live != s.live {
        // Some(text) → None has to write an *empty* label. Omitting the node
        // would leave the consumer holding the old announcement.
        nodes.push((LIVE_NODE, live_node(s)));
    }

    // A node the previous snapshot did not have, or one whose semantic data
    // (including children, bounds and hidden) moved. One hash of the old
    // snapshot: this runs once per frame while an AT is attached, and a scan per
    // node would make it quadratic for no gain.
    let old: std::collections::HashMap<u64, &SemanticNode> =
        prev.tree.nodes.iter().map(|n| (n.id.raw(), n)).collect();
    for n in s.nodes.iter().filter(|n| n.id != s.root) {
        if old.get(&n.id.raw()) != Some(&n) {
            nodes.push((AkNodeId(n.id.raw()), node_of(n)));
        }
    }

    TreeUpdate {
        nodes,
        // `None` is the statement "apply this to what you have". The consumer
        // keeps the tree it was given by the last complete update.
        tree: None,
        tree_id: TreeId::ROOT,
        focus: focus_of(focused),
    }
}

fn node_of(n: &SemanticNode) -> Node {
    let role = role_of(n.role);
    let mut node = Node::new(role);

    // A `Role::Label` node's name comes from `value`, not `label` — see the
    // module docs. For every other role it is the other way round.
    if role == Role::Label {
        node.set_value(n.name.clone());
    } else {
        node.set_label(n.name.clone());
    }
    // A description identical to the name is not a description. That is what a
    // text object looks like — its text *is* its description, by design, so that
    // a reader is told the sentence — and handing both properties to the platform
    // is how one string gets read out twice.
    if !n.description.is_empty() && n.description != n.name {
        node.set_description(n.description.clone());
    }
    if !n.children.is_empty() {
        let children: Vec<AkNodeId> = n.children.iter().map(|c| AkNodeId(c.raw())).collect();
        node.set_children(children);
    }
    if let Some(b) = n.bounds {
        node.set_bounds(accesskit::Rect {
            x0: b.x0,
            y0: b.y0,
            x1: b.x1,
            y1: b.y1,
        });
    }
    if n.hidden {
        node.set_hidden();
    }
    node.add_action(Action::Focus);

    if let Some(v) = &n.value {
        // These fields are what make a screen reader's increment verb and an
        // arrow key the same operation: both become the one `Property::Opacity`
        // command.
        node.set_numeric_value(v.current);
        node.set_min_numeric_value(v.min);
        node.set_max_numeric_value(v.max);
        node.set_numeric_value_step(v.step);
        // ...but *not* the value text on a `Role::Label`, where `value` is the
        // node's text content. Writing "不透明度 100%" there would rename every
        // text object to a description of its opacity — the same
        // `label_comes_from_value` trap from the module docs, arriving from the
        // other direction. A `Label` keeps its text as its name and offers
        // increment/decrement without a spoken value.
        if role != Role::Label {
            node.set_value(v.text.clone());
        }
        node.add_action(Action::Increment);
        node.add_action(Action::Decrement);
        node.add_action(Action::SetValue);
    }
    node
}

/// The AccessKit role for a semantic role.
///
/// Total, and deliberately so: a fallback arm would let a new [`SemanticRole`]
/// variant reach a screen reader as whatever `Role::default()` happens to be,
/// instead of failing to compile.
fn role_of(r: SemanticRole) -> Role {
    match r {
        SemanticRole::Image => Role::Image,
        SemanticRole::Group => Role::Group,
        SemanticRole::Label => Role::Label,
        SemanticRole::Heading => Role::Heading,
        SemanticRole::Math => Role::Math,
        SemanticRole::Status => Role::Status,
    }
}

/// The window-facing half: an adapter, plus the core above.
pub struct A11y {
    adapter: Adapter,
    core: A11yCore,
    title: String,
}

impl A11y {
    /// Create the adapter.
    ///
    /// **Must run before the window is first shown** — `accesskit_winit` panics
    /// otherwise, which is why `main` creates the window invisible, builds this,
    /// and only then makes the window visible. There is no way around that
    /// ordering: all three of the adapter's constructors take an
    /// `&ActiveEventLoop`, which exists only inside `resumed`.
    pub fn new(
        event_loop: &ActiveEventLoop,
        window: &Window,
        proxy: EventLoopProxy<crate::AppEvent>,
        title: impl Into<String>,
    ) -> Self {
        Self {
            adapter: Adapter::with_event_loop_proxy(event_loop, window, proxy),
            core: A11yCore::new(),
            title: title.into(),
        }
    }

    /// Forward a window event to the adapter.
    ///
    /// `accesskit_winit` requires this for **every** window event and *before*
    /// the application handles it, so `main` calls it as the first statement of
    /// `window_event` rather than in a match arm it might later stop matching.
    pub fn process_event(&mut self, window: &Window, event: &WindowEvent) {
        self.adapter.process_event(window, event);
    }

    /// The core, for the interaction the keyboard drives.
    pub fn core(&mut self) -> &mut A11yCore {
        &mut self.core
    }

    pub fn focused(&self) -> Option<NodeId> {
        self.core.focused()
    }

    /// Push the update, and release any announcement that is due.
    ///
    /// Call once per frame, **before** `request_redraw` — the tree that goes out
    /// has to describe the state that was just rendered, and the initial-tree
    /// request can arrive before any frame at all, so it is routed through this
    /// same path rather than answered separately.
    ///
    /// The first update after construction (and after [`Self::invalidate`]) is a
    /// complete snapshot, because the winit adapter, built with an event-loop
    /// proxy, cannot hand back a synchronous initial tree. Every later one sends
    /// only what changed — see [`A11yCore::to_update`] for the rules and
    /// [`A11yCore::invalidate`] for when the difference is no longer meaningful.
    ///
    /// The closure runs only while an assistive technology is attached, so an
    /// unattached app builds no update at all; the push counters are incremented
    /// inside it for exactly that reason.
    pub fn update(&mut self, ctx: &SceneContext, now: Instant) {
        self.core.flush(now);
        let title = self.title.clone();
        // `&mut self.adapter` and `&mut self.core` are disjoint fields, so the
        // closure can borrow the core while the adapter is borrowed mutably.
        let adapter = &mut self.adapter;
        let core = &mut self.core;
        adapter.update_if_active(|| core.to_update(ctx, &title));
    }

    /// Declare the backend's copy of the tree unusable; the next update is full.
    ///
    /// Forwarded from [`A11yCore::invalidate`], where the reason lives.
    pub fn invalidate(&mut self) {
        self.core.invalidate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustim_anim::Motion;
    use rustim_math::Viewport;
    use rustim_mobject::shapes::{ShapeKind, ShapeParams};

    fn ctx() -> SceneContext {
        let mut ctx = SceneContext::new(Viewport::default(), Motion::Full);
        ctx.add(rustim_mobject::shapes::build(
            ShapeKind::Square,
            &ShapeParams {
                width: Some(3.0),
                height: Some(3.0),
                label: Some("a 的正方形".into()),
                ..Default::default()
            },
        ));
        ctx
    }

    fn update_of(ctx: &SceneContext, live: Option<String>) -> TreeUpdate {
        full_update(&ctx.semantic_tree(live), "rustim — 勾股定理", None)
    }

    fn find(u: &TreeUpdate, id: AkNodeId) -> &Node {
        &u.nodes
            .iter()
            .find(|(n, _)| *n == id)
            .unwrap_or_else(|| panic!("no node {id:?}"))
            .1
    }

    #[test]
    fn the_window_is_the_root_and_owns_every_top_level_object() {
        let ctx = ctx();
        let u = update_of(&ctx, None);
        assert_eq!(u.tree.as_ref().unwrap().root, ROOT_NODE);
        assert_eq!(u.tree_id, TreeId::ROOT);
        let root = find(&u, ROOT_NODE);
        assert_eq!(root.role(), Role::Window);
        assert_eq!(root.label(), Some("rustim — 勾股定理"));
        // The square and the live region, and nothing else.
        assert_eq!(root.children().len(), 2);
        assert!(root.children().contains(&LIVE_NODE));
        assert!(root.children().contains(&AkNodeId(ctx.root_ids()[0].raw())));
    }

    #[test]
    fn every_mobject_becomes_a_node_and_the_reserved_root_does_not() {
        let ctx = ctx();
        let u = update_of(&ctx, None);
        // One per mobject, plus the window and the live region.
        assert_eq!(u.nodes.len(), ctx.tree.len() + 2);
        for id in ctx.tree.draw_order() {
            let node = find(&u, AkNodeId(id.raw()));
            assert_eq!(node.role(), Role::Image);
            assert!(node.supports_action(Action::Focus));
        }
        // The semantic root is a container, not a node: emitting it would put a
        // slotmap null key into the tree as a magic large number.
        assert!(
            !u.nodes
                .iter()
                .any(|(n, _)| n.0 == rustim_scene::a11y::ROOT.raw())
        );
    }

    #[test]
    fn an_object_carries_its_name_description_bounds_and_value() {
        let ctx = ctx();
        let u = update_of(&ctx, None);
        let id = AkNodeId(ctx.root_ids()[0].raw());
        let n = find(&u, id);
        assert_eq!(n.label(), Some("a 的正方形"));
        let description = n.description().unwrap_or_default();
        assert!(
            description.contains('9'),
            "the area should be described: {description}"
        );
        // Bounds are in screen pixels, so a 3x3 world square at the default
        // viewport is a rectangle with real extent and a *positive* height — the
        // Y flip is the thing that silently produces an inverted box.
        let b = n.bounds().expect("a square has bounds");
        assert!(b.x1 > b.x0 && b.y1 > b.y0, "inverted bounds: {b:?}");
        assert_eq!(n.numeric_value(), Some(1.0));
        assert_eq!(n.min_numeric_value(), Some(0.0));
        assert_eq!(n.max_numeric_value(), Some(1.0));
        assert!(n.supports_action(Action::Increment));
        assert!(n.supports_action(Action::Decrement));
    }

    #[test]
    fn a_text_node_puts_its_text_in_value_because_label_is_ignored_there() {
        // The trap from the module docs: a `Role::Label` node's name is read
        // from `value`, so setting only `label` makes text objects silent on
        // macOS and Windows both.
        let mut ctx = SceneContext::new(Viewport::default(), Motion::Full);
        let mut m = rustim_mobject::shapes::build(ShapeKind::Square, &ShapeParams::default());
        m.kind = rustim_mobject::NodeKind::Text;
        m.label = Some("斜边".into());
        ctx.add(m);
        let u = update_of(&ctx, None);
        let n = find(&u, AkNodeId(ctx.root_ids()[0].raw()));
        assert_eq!(n.role(), Role::Label);
        assert_eq!(n.value(), Some("斜边"), "text goes in `value`");
    }

    #[test]
    fn the_live_region_is_a_status_node_carrying_the_announcement() {
        let ctx = ctx();
        let u = update_of(&ctx, Some("a 的正方形，面积为 9".into()));
        let n = find(&u, LIVE_NODE);
        // Not `Role::Label`: that role takes its name from `value`, so an
        // announcement written to `label` would never be read.
        assert_eq!(n.role(), Role::Status);
        assert_eq!(n.live(), Some(Live::Polite));
        assert!(n.is_live_atomic());
        assert_eq!(n.label(), Some("a 的正方形，面积为 9"));
        assert!(n.children().is_empty());
    }

    #[test]
    fn the_live_region_exists_even_with_nothing_to_say() {
        // Otherwise its first appearance is a node-added event, which on macOS
        // speaks the label — an announcement nobody asked for.
        let ctx = ctx();
        let u = update_of(&ctx, None);
        assert_eq!(find(&u, LIVE_NODE).label().unwrap_or_default(), "");
    }

    #[test]
    fn focus_defaults_to_the_window_and_tracks_a_real_node() {
        let ctx = ctx();
        assert_eq!(update_of(&ctx, None).focus, ROOT_NODE);
        let id = ctx.root_ids()[0];
        let u = full_update(&ctx.semantic_tree(None), "t", Some(id));
        assert_eq!(u.focus, AkNodeId(id.raw()));
    }

    #[test]
    fn a_hidden_object_is_marked_hidden_rather_than_omitted() {
        // Omitting it would renumber everything after it for a reader that
        // counts; marking it lets a reader say "3 of 4 objects".
        let mut ctx = ctx();
        let id = ctx.root_ids()[0];
        ctx.tree.get_mut(id).unwrap().opacity = 0.0;
        let u = update_of(&ctx, None);
        assert!(find(&u, AkNodeId(id.raw())).is_hidden());
    }

    #[test]
    fn every_role_maps_to_something_a_reader_understands() {
        // Pinned rather than left to a default: a role that silently became
        // `Unknown` would still compile.
        assert_eq!(role_of(SemanticRole::Image), Role::Image);
        assert_eq!(role_of(SemanticRole::Group), Role::Group);
        assert_eq!(role_of(SemanticRole::Label), Role::Label);
        assert_eq!(role_of(SemanticRole::Heading), Role::Heading);
        assert_eq!(role_of(SemanticRole::Math), Role::Math);
        assert_eq!(role_of(SemanticRole::Status), Role::Status);
    }

    #[test]
    fn the_tree_is_well_formed() {
        // AccessKit requires every node to be the root or a child of another
        // node in the same update. A tree that violates it is rejected at
        // runtime by the backend, where the only symptom is silence.
        let ctx = ctx();
        let u = update_of(&ctx, Some("你好".into()));
        let ids: std::collections::HashSet<AkNodeId> = u.nodes.iter().map(|(id, _)| *id).collect();
        assert!(ids.contains(&ROOT_NODE));
        for (id, _) in &u.nodes {
            if *id == ROOT_NODE {
                continue;
            }
            let has_parent = u.nodes.iter().any(|(_, p)| p.children().contains(id));
            assert!(has_parent, "{id:?} has no parent in the update");
        }
    }

    #[test]
    fn focus_navigation_walks_the_scene_and_speaks_where_it_landed() {
        let mut ctx = ctx();
        ctx.add(rustim_mobject::shapes::build(
            ShapeKind::Circle,
            &ShapeParams::default(),
        ));
        let mut core = A11yCore::new();
        let first = ctx.root_ids()[0];
        assert_eq!(core.focus_step(1, &ctx), Some(first));
        assert_eq!(core.focused(), Some(first));
        // The announcement is held by the throttle, not lost.
        assert!(core.announcer.pending().is_some());
        assert_eq!(core.focus_step(-1, &ctx), Some(ctx.root_ids()[1]));
        assert_eq!(core.focus_step(1, &ctx), Some(first), "wraps");
        core.focus_clear();
        assert_eq!(core.focused(), None);
    }

    #[test]
    fn an_announcement_reaches_the_live_region_only_after_the_throttle_allows_it() {
        let ctx = ctx();
        let mut core = A11yCore::new();
        core.focus_step(1, &ctx);
        // Held: nothing on the node yet, so nothing is re-announced.
        assert_eq!(core.live_text(), "");
        let now = Instant::now();
        core.flush(now);
        assert!(!core.live_text().is_empty());
        // The flushed text is what the tree carries, so the node's label
        // changes exactly when something new is said — which is the only thing
        // a backend announces on.
        let u = core.to_update(&ctx, "t");
        assert_eq!(find(&u, LIVE_NODE).label(), Some(core.live_text()));
    }

    /// The motion switch is otherwise silent to a blind user: it changes timing,
    /// and timing is precisely what the reduced path removes. So it gets an
    /// announcement of its own, through the same live region as everything else.
    #[test]
    fn a_motion_switch_is_announced_through_the_live_region() {
        let mut core = A11yCore::new();
        core.offer(motion_announcement(MotionMode::Reduced));
        assert_eq!(core.live_text(), "", "offered, not yet spoken");
        let now = Instant::now();
        core.flush(now);
        assert_eq!(core.live_text(), "动效：减弱");

        core.offer(motion_announcement(MotionMode::Full));
        let later = now + core.announcer.min_interval();
        core.flush(later);
        assert_eq!(core.live_text(), "动效：完整");
    }

    #[test]
    fn adjusting_targets_the_focused_object_and_clamps_at_the_ends() {
        let mut ctx = ctx();
        let mut core = A11yCore::new();
        // Nothing focused: nothing to adjust, and no error either.
        assert_eq!(core.adjust(-OPACITY_STEP, &ctx), None);

        let id = ctx.root_ids()[0];
        core.focus_step(1, &ctx);
        assert_eq!(
            core.adjust(-OPACITY_STEP, &ctx),
            Some(Command::Set {
                target: id,
                property: Property::Opacity(0.95),
            })
        );

        ctx.tree.get_mut(id).unwrap().opacity = 0.0;
        // Clamped, so holding the arrow key at the end of the range is a no-op
        // rather than a value the command layer would refuse.
        assert_eq!(
            core.adjust(-OPACITY_STEP, &ctx),
            Some(Command::Set {
                target: id,
                property: Property::Opacity(0.0),
            })
        );
    }

    #[test]
    fn an_assistive_action_becomes_the_same_command_the_keyboard_would_send() {
        let ctx = ctx();
        let mut core = A11yCore::new();
        let id = ctx.root_ids()[0];
        let request = |action, data| ActionRequest {
            action,
            target_tree: TreeId::ROOT,
            target_node: AkNodeId(id.raw()),
            data,
        };

        // An assistive technology focusing a node sets the focus without
        // announcing: it moved the focus itself, so it already knows.
        assert_eq!(core.on_action(&request(Action::Focus, None), &ctx), None);
        assert_eq!(core.focused(), Some(id));
        assert_eq!(core.announcer.pending(), None);

        assert_eq!(
            core.on_action(&request(Action::Increment, None), &ctx),
            Some(Request::Command(Box::new(Command::Set {
                target: id,
                property: Property::Opacity(1.0),
            })))
        );
        assert_eq!(
            core.on_action(&request(Action::Decrement, None), &ctx),
            Some(Request::Command(Box::new(Command::Set {
                target: id,
                property: Property::Opacity(0.95),
            })))
        );
        assert_eq!(
            core.on_action(
                &request(Action::SetValue, Some(ActionData::NumericValue(0.25))),
                &ctx
            ),
            Some(Request::Command(Box::new(Command::Set {
                target: id,
                property: Property::Opacity(0.25),
            })))
        );
        assert_eq!(
            core.on_action(&request(Action::Click, None), &ctx),
            Some(Request::TogglePlayback)
        );
        // Unsupported actions are dropped, per the handler contract.
        assert_eq!(core.on_action(&request(Action::ScrollUp, None), &ctx), None);
    }

    #[test]
    fn a_stale_target_is_not_filtered_here() {
        // A deleted node named by an out-of-date assistive technology has to
        // reach the command layer and be refused there — one place decides what
        // exists. Filtering here would make it two.
        let ctx = ctx();
        let mut core = A11yCore::new();
        let request = ActionRequest {
            action: Action::Increment,
            target_tree: TreeId::ROOT,
            target_node: AkNodeId(0x1_0000_0009),
            data: None,
        };
        assert_eq!(core.on_action(&request, &ctx), None, "nothing focused");
    }

    // -----------------------------------------------------------------------
    // Incremental updates, validated by a real consumer
    // -----------------------------------------------------------------------
    //
    // An update is only meaningful relative to the tree a platform is holding,
    // so these tests hand each `TreeUpdate` to `accesskit_consumer` — the same
    // crate the platform backends use — and let *it* decide whether the update
    // is well formed. Asserting over our own node list cannot catch the failure
    // mode that matters: a legal-looking update that describes the wrong tree.

    use accesskit_consumer::{FullNodeId, NodeRef, Tree as ConsumerTree, TreeChangeHandler};

    /// A consumer-side record of what one update changed.
    #[derive(Debug, Default)]
    struct Changes {
        added: Vec<AkNodeId>,
        updated: Vec<AkNodeId>,
        removed: Vec<AkNodeId>,
    }

    /// The local part of a consumer id (the tree index is packed into the high
    /// half, and there is one tree).
    fn local(id: FullNodeId) -> AkNodeId {
        AkNodeId((u128::from(id) >> 64) as u64)
    }

    impl TreeChangeHandler for Changes {
        fn node_added(&mut self, node: &NodeRef) {
            self.added.push(local(node.id()));
        }
        fn node_updated(&mut self, _: &NodeRef, node: &NodeRef) {
            self.updated.push(local(node.id()));
        }
        fn focus_moved(&mut self, _: Option<&NodeRef>, _: Option<&NodeRef>) {}
        fn node_removed(&mut self, node: &NodeRef) {
            self.removed.push(local(node.id()));
        }
    }

    /// A consumer seeded with the first update the core produces.
    fn consumer(ctx: &SceneContext, core: &mut A11yCore) -> ConsumerTree {
        let first = core.to_update(ctx, "t");
        assert!(first.tree.is_some(), "the first update must be complete");
        ConsumerTree::new(first, false)
    }

    fn apply(consumer: &mut ConsumerTree, update: TreeUpdate) -> Changes {
        let mut changes = Changes::default();
        consumer.update_and_process_changes(update, &mut changes);
        changes
    }

    fn has(consumer: &ConsumerTree, id: AkNodeId) -> bool {
        consumer
            .state()
            .node_by_tree_local_id(id, TreeId::ROOT)
            .is_some()
    }

    #[test]
    fn an_unchanged_scene_sends_nothing_but_the_focus() {
        let ctx = ctx();
        let mut core = A11yCore::new();
        let mut consumer = consumer(&ctx, &mut core);

        let update = core.to_update(&ctx, "t");
        assert!(update.tree.is_none(), "a difference, not a snapshot");
        assert!(update.nodes.is_empty(), "nothing changed");
        assert_eq!(update.focus, ROOT_NODE);
        let changes = apply(&mut consumer, update);
        assert!(
            changes.added.is_empty() && changes.updated.is_empty() && changes.removed.is_empty()
        );
        assert_eq!((core.full_pushes(), core.incremental_pushes()), (1, 1));
    }

    #[test]
    fn a_new_object_reaches_the_consumer() {
        let mut ctx = ctx();
        let mut core = A11yCore::new();
        let mut consumer = consumer(&ctx, &mut core);

        let id = ctx.add(rustim_mobject::shapes::build(
            ShapeKind::Circle,
            &ShapeParams::default(),
        ));
        let update = core.to_update(&ctx, "t");
        let names: Vec<AkNodeId> = update.nodes.iter().map(|(id, _)| *id).collect();
        assert!(
            names.contains(&ROOT_NODE),
            "the new node's parent has to ride along: {names:?}"
        );
        assert!(names.contains(&AkNodeId(id.raw())));
        let changes = apply(&mut consumer, update);
        assert!(changes.added.contains(&AkNodeId(id.raw())));
        assert!(has(&consumer, AkNodeId(id.raw())));
    }

    #[test]
    fn a_removed_object_leaves_the_consumer_and_is_never_named() {
        let mut ctx = ctx();
        let doomed = ctx.add(rustim_mobject::shapes::build(
            ShapeKind::Circle,
            &ShapeParams::default(),
        ));
        let mut core = A11yCore::new();
        let mut consumer = consumer(&ctx, &mut core);
        assert!(has(&consumer, AkNodeId(doomed.raw())));

        ctx.tree.remove_subtree(doomed);
        let update = core.to_update(&ctx, "t");
        assert!(
            !update
                .nodes
                .iter()
                .any(|(id, _)| *id == AkNodeId(doomed.raw())),
            "a removed node must not appear in the update"
        );
        let changes = apply(&mut consumer, update);
        assert!(changes.removed.contains(&AkNodeId(doomed.raw())));
        assert!(!has(&consumer, AkNodeId(doomed.raw())));
    }

    /// The panic regression: a `FadeOut` can delete the focused node while the
    /// keyboard and the AT are both idle, so nothing else would ever resync the
    /// focus. The consumer validates `focus` against the tree it holds and
    /// rejects an update that names a node it has just been told is gone.
    #[test]
    fn removing_the_focused_node_releases_the_focus() {
        let mut ctx = ctx();
        let id = ctx.root_ids()[0];
        let mut core = A11yCore::new();
        let mut consumer = consumer(&ctx, &mut core);
        core.focus_on(id, &ctx);
        let _ = apply(&mut consumer, core.to_update(&ctx, "t"));

        ctx.tree.remove_subtree(id);
        assert_eq!(core.focused(), Some(id), "nothing has resynced yet");
        let update = core.to_update(&ctx, "t");
        assert_eq!(core.focused(), None, "the projection gives the focus up");
        assert_eq!(update.focus, ROOT_NODE);
        apply(&mut consumer, update);
    }

    #[test]
    fn invalidating_forces_a_complete_snapshot() {
        let ctx = ctx();
        let mut core = A11yCore::new();
        let mut consumer = consumer(&ctx, &mut core);
        let _ = apply(&mut consumer, core.to_update(&ctx, "t"));

        core.invalidate();
        let update = core.to_update(&ctx, "t");
        assert!(update.tree.is_some(), "a full update after invalidation");
        assert!(
            update.nodes.len() > ctx.tree.len(),
            "every scene node has to be present, not just the changed ones"
        );
        apply(&mut consumer, update);
        assert_eq!((core.full_pushes(), core.incremental_pushes()), (2, 1));
    }

    #[test]
    fn a_title_change_resends_only_the_window() {
        let ctx = ctx();
        let mut core = A11yCore::new();
        let mut consumer = consumer(&ctx, &mut core);

        let update = core.to_update(&ctx, "另一首");
        assert_eq!(update.nodes.len(), 1, "the window, and nothing else");
        assert_eq!(update.nodes[0].0, ROOT_NODE);
        assert_eq!(update.nodes[0].1.label(), Some("另一首"));
        apply(&mut consumer, update);
        assert_eq!(
            consumer.state().root().label().as_deref(),
            Some("另一首"),
            "the consumer took the new title"
        );
    }

    #[test]
    fn a_cleared_announcement_is_resent_as_an_empty_label() {
        let ctx = ctx();
        let mut core = A11yCore::new();
        let mut consumer = consumer(&ctx, &mut core);

        core.offer("说过的");
        core.flush(Instant::now());
        let _ = apply(&mut consumer, core.to_update(&ctx, "t"));

        // Some → None: the live node must be present with an empty label.
        // Omitting it would leave the old announcement on the consumer forever.
        core.live.clear();
        let update = core.to_update(&ctx, "t");
        let live = update
            .nodes
            .iter()
            .find(|(id, _)| *id == LIVE_NODE)
            .map(|(_, n)| n)
            .expect("the live node is resent when its text goes away");
        assert_eq!(live.label(), Some(""));
        apply(&mut consumer, update);
    }

    #[test]
    fn a_reparent_lands_on_the_consumer() {
        let mut ctx = ctx();
        let parent = ctx.root_ids()[0];
        let child = ctx.add(rustim_mobject::shapes::build(
            ShapeKind::Circle,
            &ShapeParams::default(),
        ));
        let mut core = A11yCore::new();
        let mut consumer = consumer(&ctx, &mut core);

        assert!(ctx.tree.reparent(child, parent));
        let update = core.to_update(&ctx, "t");
        let names: Vec<AkNodeId> = update.nodes.iter().map(|(id, _)| *id).collect();
        assert!(names.contains(&ROOT_NODE), "the old parent lost a child");
        assert!(
            names.contains(&AkNodeId(parent.raw())),
            "the new parent gained one"
        );
        apply(&mut consumer, update);

        let node = consumer
            .state()
            .node_by_tree_local_id(AkNodeId(parent.raw()), TreeId::ROOT)
            .expect("the parent");
        let children: Vec<AkNodeId> = node.children().map(|c| local(c.id())).collect();
        assert_eq!(children, vec![AkNodeId(child.raw())]);
    }
}
