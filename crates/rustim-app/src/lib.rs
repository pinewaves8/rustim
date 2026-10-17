//! The application layer: input, announcements, and the accessibility adapter.
//!
//! # Why the binary has a library beside it
//!
//! Everything in this crate that can be tested without a window lives here
//! rather than in `main.rs`, because a binary target's modules are not reachable
//! from an integration test or from another crate. Concretely, the keyboard
//! classifier and the announcement throttle are pure state machines over data
//! structures — they are where the bugs are, and their bugs are invisible in a
//! screenshot — so they have to be *callable* by a test to be tested at all.
//!
//! `main.rs` is then the part that genuinely needs a display: an event loop, a
//! window, a `softbuffer` surface, and an AccessKit adapter. It is the only
//! place in the workspace that names `accesskit`, and it is deliberately not
//! testable — see `crate::a11y` for what crosses that line.
//!
//! # The dependency boundary
//!
//! This crate is not one of the `wasm32-unknown-unknown` kernel crates. It may
//! link a windowing system, an accessibility backend and a CPU rasteriser; the
//! six kernel crates below it may not. `CI` asserts the second half of that with
//! `cargo tree`, because a dependency added here by accident is cheap to fix and
//! expensive to find once the Web target exists.

pub mod a11y;
pub mod announce;
pub mod input;
pub mod motion;
pub mod text;

pub use a11y::{A11y, A11yCore, Request};
pub use announce::{Announcer, Narrator};
pub use input::{FocusState, Intent};

/// What the event loop is woken with from a thread that is not the event loop's.
///
/// `accesskit_winit` delivers its events this way: the platform's accessibility
/// backend calls in from its own thread, and the winit adapter's three handlers
/// (activation, action, deactivation) each push an [`accesskit_winit::Event`]
/// through the proxy rather than calling into the app directly. The `From` impl
/// is not a convenience — `Adapter::with_event_loop_proxy` is generic over
/// `T: From<Event>` and uses it to convert.
///
/// Only one variant so far, and it is still an enum: the proxy's type parameter
/// is fixed at event-loop construction, so a second kind of cross-thread wakeup
/// later (a file watcher, a network feed) can be added without rebuilding the
/// loop.
#[derive(Debug)]
pub enum AppEvent {
    AccessKit(accesskit_winit::Event),
}

impl From<accesskit_winit::Event> for AppEvent {
    fn from(event: accesskit_winit::Event) -> Self {
        Self::AccessKit(event)
    }
}
