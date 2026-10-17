//! The `wasm_bindgen` shell: a canvas, an rAF loop, and a command bridge.
//!
//! Everything here that is not "talk to the browser" lives in [`crate::session`],
//! which builds on every target. That split is deliberate: the frame this module
//! presents is the frame `cargo test` compares against the desktop golden, so the
//! shell keeps no logic beyond presentation and scheduling.
//!
//! # The frame path
//!
//! ```text
//! requestAnimationFrame(ms) -> Session::step(dt) -> Uint8ClampedArray -> ImageData -> putImageData
//! ```
//!
//! The pixel storage is allocated once and reused — a 1280x720 frame is 3.7 MB,
//! and allocating one per frame at 60 fps is pure churn. [`Session::pixels`] is
//! **premultiplied** (the renderer's format) while `ImageData` is straight alpha,
//! so each frame copies into the scratch buffer, un-premultiplies in place, and
//! copies the result into a `Uint8ClampedArray` that the `ImageData` shares.
//!
//! # Why the loop owns no global state
//!
//! The rAF callback is a `Closure` held by [`WebApp`], and it captures `Weak`
//! references to the session and to its own holder. The JavaScript handle is
//! therefore the only owner of the loop: dropping the `WebApp` cancels the
//! pending frame and drops the closure. No `forget()`, no `Box::leak`, no
//! `thread_local!` — the same rule the rest of the workspace follows.
//!
//! # The accessibility tree is DOM, not AccessKit
//!
//! There is no `accesskit_web`, so the browser gets the platform-neutral
//! [`SemanticTree`] through [`crate::aria`]: one absolutely-positioned element
//! per node, inside an overlay that covers the canvas and is itself
//! `aria-hidden` from nothing (the canvas is). Nodes are `opacity: 0` rather
//! than `display: none` or `visibility: hidden`, because those remove a node
//! from the accessibility tree — the opposite of what this overlay is for. The
//! overlay is rebuilt only when the tree differs from the last one, so a still
//! scene costs one comparison per frame.
//!
//! The one element whose content is markup rather than text is a formula: its
//! `mathml` string is injected with `innerHTML`, so a MathML-aware reader walks
//! the structure instead of hearing one sentence. That string is produced by
//! the Typst compiler inside `rustim-text` — engine output, never host or user
//! input — which is the trust boundary that makes `innerHTML` acceptable here.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{CanvasRenderingContext2d, Document, Element, HtmlCanvasElement, ImageData};

use rustim_render::straight_rgba_in_place;
use rustim_scene::a11y::SemanticTree;

use crate::aria::{self, AriaNode};
use crate::session::{STEP, Session};

/// The longest wall-clock step a frame is allowed to represent.
///
/// A tab that was hidden for a minute comes back with a timestamp a minute
/// later; integrating that would jump the whole timeline in one frame. A tenth
/// of a second is the desktop loop's own ceiling for the same reason.
const MAX_STEP: f64 = 0.1;

type Tick = Closure<dyn FnMut(f64)>;

fn jserr(message: impl Into<String>) -> JsValue {
    JsValue::from_str(&message.into())
}

fn describe(error: &JsValue) -> String {
    error
        .as_string()
        .unwrap_or_else(|| "a JavaScript exception with no message".to_owned())
}

/// The state one animation frame touches.
struct Inner {
    session: Session,
    document: Document,
    context: CanvasRenderingContext2d,
    /// Shares its buffer with `array`; built once, drawn every frame.
    image: ImageData,
    array: js_sys::Uint8ClampedArray,
    /// Premultiplied pixels in, straight-alpha pixels out, reused every frame.
    scratch: Vec<u8>,
    /// The last rAF timestamp, for `dt`. `None` until the first frame.
    last_ms: Option<f64>,
    /// The pending rAF handle, so `Drop` can cancel it.
    handle: Option<i32>,
    /// The last presentation failure. A value, not a panic: the browser has no
    /// terminal to print one to, and a host can show it.
    error: Option<String>,
    /// The DOM node every `AriaNode` lives in, covering the canvas.
    overlay: Element,
    /// The last projected tree, for the compare-before-rebuild rule.
    last_tree: Option<SemanticTree>,
}

impl Inner {
    fn new(canvas_id: &str, scene: &str) -> Result<(Self, web_sys::Window), JsValue> {
        let window =
            web_sys::window().ok_or_else(|| jserr("no window: this module is a browser host"))?;
        let document = window.document().ok_or_else(|| jserr("no document"))?;
        let element = document
            .get_element_by_id(canvas_id)
            .ok_or_else(|| jserr(format!("no element with id `{canvas_id}`")))?;
        let canvas: HtmlCanvasElement = element
            .dyn_into()
            .map_err(|_| jserr(format!("`{canvas_id}` is not a <canvas>")))?;

        // The element's own attributes decide the frame size, so a host page can
        // size it. Zero is HTML's way of saying "nothing was set" — refused here
        // rather than guessed at, because a scene rendered into 0x0 is a blank
        // page with no error anywhere.
        let width = u16::try_from(canvas.width()).map_err(|_| {
            jserr(format!(
                "canvas width {} does not fit a frame",
                canvas.width()
            ))
        })?;
        let height = u16::try_from(canvas.height()).map_err(|_| {
            jserr(format!(
                "canvas height {} does not fit a frame",
                canvas.height()
            ))
        })?;

        let session = Session::new(width, height, scene).map_err(jserr)?;

        let context: CanvasRenderingContext2d = canvas
            .get_context("2d")?
            .and_then(|object| object.dyn_into().ok())
            .ok_or_else(|| jserr("the canvas has no 2d context"))?;

        let len = usize::from(width) * usize::from(height) * 4;
        let len =
            u32::try_from(len).map_err(|_| jserr("the canvas is too large for one buffer"))?;
        let array = js_sys::Uint8ClampedArray::new_with_length(len);
        // `new ImageData(data, sw, sh)` keeps the typed array rather than copying
        // it, so `array.copy_from` in `blit` is what `putImageData` reads.
        let image = ImageData::new_with_js_u8_clamped_array_and_sh(
            &array,
            u32::from(width),
            u32::from(height),
        )?;

        let overlay = overlay_for(&document, &canvas)?;

        Ok((
            Self {
                session,
                document,
                context,
                image,
                array,
                scratch: vec![0; len as usize],
                last_ms: None,
                handle: None,
                error: None,
                overlay,
                last_tree: None,
            },
            window,
        ))
    }

    /// Copy the renderer's frame to the canvas, converting the alpha format.
    fn blit(&mut self) {
        let pixels = self.session.pixels();
        let n = pixels.len().min(self.scratch.len());
        self.scratch[..n].copy_from_slice(&pixels[..n]);
        straight_rgba_in_place(&mut self.scratch[..n]);
        self.array.copy_from(&self.scratch[..n]);
        if let Err(error) = self.context.put_image_data(&self.image, 0.0, 0.0) {
            self.error = Some(format!("putImageData failed: {}", describe(&error)));
        }
    }

    /// Draw the state as it stands, without advancing the clock.
    fn refresh(&mut self) {
        self.session.refresh();
        self.blit();
        self.sync_a11y();
    }

    /// Project the semantic tree into the overlay, when it changed.
    ///
    /// A still scene costs one `SemanticTree` comparison per frame and no DOM
    /// work; a moving one rebuilds the handful of nodes that exist. Rebuilding
    /// rather than diffing is deliberate: five elements is below the point
    /// where a keyed update pays for its bookkeeping, and a rebuild cannot
    /// leave a stale node behind.
    fn sync_a11y(&mut self) {
        let tree = self.session.semantic();
        if self.last_tree.as_ref() == Some(&tree) {
            return;
        }
        let nodes = aria::project(&tree);
        self.overlay.set_inner_html("");

        let mut elements = Vec::with_capacity(nodes.len());
        for node in &nodes {
            match self.make_node(node) {
                Some(element) => elements.push(element),
                // `create_element` only fails on a document that is going away;
                // leaving the overlay empty beats a half-built tree.
                None => return,
            }
        }
        let by_id: HashMap<u64, Element> = nodes
            .iter()
            .zip(&elements)
            .map(|(node, element)| (node.id, element.clone()))
            .collect();
        for (node, element) in nodes.iter().zip(&elements) {
            for child in &node.children {
                if let Some(child_element) = by_id.get(child) {
                    let _ = element.append_child(child_element);
                }
            }
        }
        if let Some(root) = elements.first() {
            let _ = self.overlay.append_child(root);
        }

        // The announcement channel, present even when nothing is playing: an
        // `aria-live` region that is created at announcement time is a region
        // no reader is watching yet.
        if let Ok(live) = self.document.create_element("div") {
            let _ = live.set_attribute("role", "status");
            let _ = live.set_attribute("aria-live", "polite");
            let _ = live.set_attribute("aria-atomic", "true");
            live.set_text_content(Some(tree.live.as_deref().unwrap_or("")));
            let _ = self.overlay.append_child(&live);
        }

        self.last_tree = Some(tree);
    }

    fn make_node(&self, node: &AriaNode) -> Option<Element> {
        let element = self.document.create_element("div").ok()?;
        let _ = element.set_attribute("role", node.role);
        if !node.name.is_empty() {
            let _ = element.set_attribute("aria-label", &node.name);
        }
        if !node.description.is_empty() {
            let _ = element.set_attribute("aria-description", &node.description);
        }
        if node.hidden {
            let _ = element.set_attribute("aria-hidden", "true");
        }
        let _ = element.set_attribute("style", &placement(node.bounds));
        if let Some(mathml) = &node.mathml {
            // Engine output (Typst's own MathML), never host or user input —
            // the trust boundary that makes markup acceptable here.
            element.set_inner_html(mathml);
        }
        Some(element)
    }

    /// One animation frame.
    fn tick(&mut self, ms: f64) {
        let dt = match self.last_ms {
            Some(last) if ms > last => ((ms - last) / 1000.0).clamp(0.0, MAX_STEP),
            // A repeated or backwards timestamp (and the first frame) gets one
            // ordinary step rather than a zero-length one, so a paused-then-
            // resumed tab continues at the normal speed.
            Some(_) => 0.0,
            None => STEP,
        };
        self.last_ms = Some(ms);
        self.session.step(dt);
        self.blit();
        self.sync_a11y();
    }
}

/// The layer every semantic node is placed in.
///
/// The page normally provides `<div id="a11y">` inside a positioned wrapper
/// around the canvas; when it does not, one is created beside the canvas, so a
/// host page cannot forget it and silently lose the accessibility tree.
fn overlay_for(document: &Document, canvas: &HtmlCanvasElement) -> Result<Element, JsValue> {
    let overlay = match document.get_element_by_id("a11y") {
        Some(existing) => existing,
        None => {
            let element = document.create_element("div")?;
            element.set_id("a11y");
            match canvas.parent_element() {
                Some(parent) => {
                    parent.append_child(&element)?;
                }
                None => {
                    let body = document.body().ok_or_else(|| jserr("no <body>"))?;
                    body.append_child(&element)?;
                }
            }
            element
        }
    };
    overlay.set_attribute(
        "style",
        "position:absolute;left:0;top:0;width:100%;height:100%;overflow:hidden;",
    )?;
    Ok(overlay)
}

/// Invisible, but present in the accessibility tree.
///
/// `opacity: 0` rather than `display: none` or `visibility: hidden`: the latter
/// two remove the element from Chrome's accessibility tree, which is exactly
/// backwards for an overlay whose only job is to be read. The position follows
/// the node's measured bounds so a reader's virtual cursor highlights what it
/// is describing, and `pointer-events: none` keeps the overlay from eating
/// clicks meant for the canvas.
fn placement(bounds: Option<rustim_math::Rect>) -> String {
    let mut style = String::from("position:absolute;opacity:0;pointer-events:none;");
    if let Some(b) = bounds
        && b.x0.is_finite()
        && b.y0.is_finite()
        && b.x1.is_finite()
        && b.y1.is_finite()
    {
        style.push_str(&format!(
            "left:{}px;top:{}px;width:{}px;height:{}px;",
            b.x0,
            b.y0,
            (b.x1 - b.x0).max(1.0),
            (b.y1 - b.y0).max(1.0)
        ));
    }
    style
}

/// Ask the browser for the next frame, recording the handle for `Drop`.
///
/// A free function rather than a method because the callback must reschedule
/// itself while the `WebApp` is borrowed for the frame's work.
fn schedule(
    window: &web_sys::Window,
    holder: &Rc<RefCell<Option<Tick>>>,
    inner: &Weak<RefCell<Inner>>,
) {
    let requested = {
        let slot = holder.borrow();
        match slot.as_ref() {
            Some(callback) => window.request_animation_frame(callback.as_ref().unchecked_ref()),
            None => return,
        }
    };
    if let (Ok(handle), Some(inner)) = (requested, inner.upgrade()) {
        inner.borrow_mut().handle = Some(handle);
    }
}

/// A scene on a canvas, driven by the browser's frame clock.
#[wasm_bindgen]
pub struct WebApp {
    inner: Rc<RefCell<Inner>>,
    /// The callback itself, so `Drop` can free it. The closure holds only
    /// `Weak` handles, so there is no reference cycle to leak.
    tick: Rc<RefCell<Option<Tick>>>,
}

#[wasm_bindgen]
impl WebApp {
    /// Build the host around the `<canvas>` named `canvas_id`.
    ///
    /// The canvas's `width` and `height` attributes are the frame size. With
    /// `autoplay`, the rAF loop starts immediately; without it the host draws
    /// virtual time zero (the scene's `construct` has run, no frame has been
    /// advanced) and waits for [`advance`](Self::advance), which is what makes a
    /// browser frame reproducible enough to compare against the desktop still.
    #[wasm_bindgen(constructor)]
    pub fn new(canvas_id: &str, scene: &str, autoplay: bool) -> Result<WebApp, JsValue> {
        let (inner, window) = Inner::new(canvas_id, scene)?;
        let inner = Rc::new(RefCell::new(inner));
        let tick: Rc<RefCell<Option<Tick>>> = Rc::new(RefCell::new(None));

        let weak_inner = Rc::downgrade(&inner);
        let weak_tick = Rc::downgrade(&tick);
        let frame_window = window.clone();
        *tick.borrow_mut() = Some(Closure::<dyn FnMut(f64)>::new(move |ms: f64| {
            let Some(inner) = weak_inner.upgrade() else {
                return;
            };
            inner.borrow_mut().tick(ms);
            if let Some(holder) = weak_tick.upgrade() {
                schedule(&frame_window, &holder, &weak_inner);
            }
        }));

        let app = WebApp {
            inner: Rc::clone(&inner),
            tick: Rc::clone(&tick),
        };
        if autoplay {
            schedule(&window, &tick, &Rc::downgrade(&inner));
        } else {
            inner.borrow_mut().refresh();
        }
        Ok(app)
    }

    /// Apply one JSON command line and return its reply.
    ///
    /// `undefined` is the blank-line skip, not a refusal — the same protocol
    /// `rustim-app --script` implements, answered by the same functions in
    /// `rustim_scene::command`.
    pub fn apply(&self, line: &str) -> Option<String> {
        self.inner.borrow_mut().session.apply(line)
    }

    /// Advance `seconds` of virtual time in fixed steps, then draw once.
    ///
    /// The deterministic path: 10 seconds of virtual time is the same frame the
    /// desktop `--png` writes, whichever wall-clock speed the browser is running
    /// at.
    pub fn advance(&self, seconds: f64) {
        let mut inner = self.inner.borrow_mut();
        inner.session.advance(seconds);
        inner.blit();
        inner.sync_a11y();
    }

    /// Draw the current state without advancing the clock.
    pub fn render(&self) {
        self.inner.borrow_mut().refresh();
    }

    /// The last presentation or rasterisation failure, if any.
    pub fn render_error(&self) -> Option<String> {
        let inner = self.inner.borrow();
        inner
            .session
            .render_error()
            .map(str::to_owned)
            .or_else(|| inner.error.clone())
    }
}

impl Drop for WebApp {
    fn drop(&mut self) {
        if let Some(handle) = self.inner.borrow_mut().handle.take()
            && let Some(window) = web_sys::window()
        {
            let _ = window.cancel_animation_frame(handle);
        }
        // Explicit rather than implied: taking the closure out is what makes
        // "the loop ends when the handle drops" visible at the call site.
        self.tick.borrow_mut().take();
    }
}
