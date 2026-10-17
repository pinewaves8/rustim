//! Scenes that more than one host runs.
//!
//! The demo scenes in `rustim-app` are shaped for that binary: they exist to
//! exercise one feature each and their goldens live in its test directory. A
//! scene in this crate is different in one respect only — a second host needs
//! the *same* construction, not a copy of it. The browser build is that host:
//! `rustim-web` renders [`Pythagorean`] into a canvas, and the claim that the
//! browser "runs the Pythagorean scene" is only checkable because the desktop
//! golden and the canvas pixels come from this one `construct`.
//!
//! Keeping the copy count at one has a second effect worth stating: the wasm
//! kernel build now also compiles this crate, so a scene that reached for the
//! filesystem, a clock or a windowing API would fail that build rather than
//! surfacing as a browser-only bug.

pub mod pythagorean;

pub use pythagorean::Pythagorean;
