//! The browser host.
//!
//! This crate is a *host*, not a second engine. It owns exactly the two things
//! `rustim-app` owns on the desktop and nothing else:
//!
//! - a canvas presentation of a [`Session`]'s pixels, and
//! - a DOM/ARIA projection of the same [`SemanticTree`](rustim_scene::a11y::SemanticTree)
//!   the desktop adapter turns into AccessKit.
//!
//! The engine — scenes, the command protocol, the rasteriser — is reached
//! through the same crates the desktop uses. `rustim-app` itself is deliberately
//! not reused: winit, softbuffer and `accesskit_unix` have no browser
//! counterpart, and there is no `accesskit_web` to bridge to. §10.6 chose the
//! route this crate implements: `vello_cpu` → `putImageData`, and the
//! platform-neutral semantic tree → DOM/ARIA (where MathML is *more* expressive
//! than the native tree, so a formula's structure is not flattened).
//!
//! # What is target independent
//!
//! [`session`] and [`aria`] build on every target. That is not tidiness: it is
//! what puts the browser's own frame under `cargo test` on the machine that
//! builds it, comparing against the desktop golden, instead of only inside a
//! browser. The `wasm_bindgen` shell lives in `web.rs`, `#[cfg]`-ed out
//! everywhere else.

pub mod aria;
pub mod session;

#[cfg(target_arch = "wasm32")]
mod web;

pub use aria::AriaNode;
pub use session::Session;
#[cfg(target_arch = "wasm32")]
pub use web::WebApp;
