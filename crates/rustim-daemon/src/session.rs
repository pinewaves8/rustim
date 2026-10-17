//! Sessions: a scene and its context, owned by one thread, addressed by id.
//!
//! # A daemon is sessions, not a process
//!
//! The desktop binary has one scene and one timeline, and its lifetime is the
//! process's. A daemon is shared: several clients may attach to the *same*
//! session (one assistant and a display, a reconnect after a dropped socket, a
//! second auditor), and a client may run more than one session at once. So the
//! unit of ownership here is a [`SessionHandle`] — a name plus channels to the
//! thread that owns the state — and the [`Hub`] is the map from name to handle.
//! `attach` with no `session` creates one; `attach` with an id joins an existing
//! one; an unknown id is a refusal, never a silent fresh session, because a
//! client that thinks it rejoined a lecture and is given an empty stage has no
//! way to tell from the replies.
//!
//! # Why a thread per session
//!
//! `SceneContext` is deliberately **not `Send`**: it holds `Box<dyn Scene>`,
//! `Box<dyn Animation>` and `Box<dyn Typesetter>`, and none of those traits
//! carries a `Send` bound — the kernel has no concurrency model, and adding one
//! to the trait objects would be a kernel-wide decision made to suit the
//! transport. A session is therefore owned by a dedicated OS thread running a
//! current-thread tokio runtime: the state never crosses a thread boundary, and
//! the network runtime only ever sees plain data (`Command`, `CommandOutcome`,
//! `Event`, `f64`) through channels.
//!
//! That has a second benefit worth stating: `construct` and the first
//! typesetting call (font parsing, Typst's library) take real time and are
//! synchronous. On a shared executor they would stall every other connection's
//! I/O; on a session thread they stall only that session, which is the right
//! blast radius. The cost is one thread per live session, which is the scale
//! this daemon is for — a handful of sessions, not thousands.
//!
//! # The clock
//!
//! A session either advances itself or is advanced by its clients:
//!
//! * [`ClockMode::Auto`] — a fixed [`STEP`] every tick, forever. The step is
//!   constant rather than measured, so the same commands at the same clock times
//!   produce the same state on a slow machine; the ticker skips missed ticks
//!   rather than bursting to catch up, so a stalled process does not fast-forward
//!   a lecture when it wakes.
//! * [`ClockMode::Manual`] — nothing moves until a client sends `advance`. This
//!   is the mode for a test, a frame-by-frame exporter on the far end of the
//!   socket, or an assistant that wants the picture to change only when it says
//!   so.
//!
//! `advance` on an auto session is a refusal rather than a second clock: two
//! sources of time would make the state unreproducible, and the refusal names
//! the fix.
//!
//! # Events
//!
//! The session drains the kernel's event log after every tick and every
//! command, and re-broadcasts what the subscription admits
//! ([`SceneContext::wants_event`]). The subscription is **session state**, not
//! connection state — the same `{"action":"events"}` command every host already
//! speaks — so a reconnect sees the subscription it left and every attached
//! client receives the same stream. Draining happens whether or not anyone is
//! subscribed: an undrained [`MAX_EVENTS`] tail is the one thing in the engine
//! that grows with input the host did not ask for.
//!
//! The daemon never emits [`Event::FrameReady`]. It draws nothing, so it has no
//! frames to report, and minting one per tick would turn a notification into a
//! heartbeat that claims an artifact exists. A client that wants frames renders
//! them itself (`rustim-web` is the browser host for exactly that).
//!
//! [`MAX_EVENTS`]: rustim_scene::MAX_EVENTS
//! [`Event::FrameReady`]: rustim_scene::Event::FrameReady

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::Deserialize;
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use rustim_anim::Motion;
use rustim_math::Viewport;
use rustim_scene::command::MotionMode;
use rustim_scene::{Command, CommandError, CommandOutcome, Event, Scene, SceneContext};

use crate::typeset::DaemonTypesetter;

/// The virtual timestep every host uses — the still path, the exporter, the
/// browser's deterministic advance and this daemon's auto clock.
pub const STEP: f64 = 1.0 / 60.0;

/// How many virtual seconds one `advance` may cross.
///
/// A guard, not a preference: `advance` is a loop of `STEP` updates, and a
/// hostile or confused client sending `1e9` would hold its session thread for
/// hours. Ten minutes is far past any legitimate seek a script would ask of a
/// socket (a seeking client has `seek` for a *time*, which is O(beats)).
pub const MAX_ADVANCE_SECONDS: f64 = 600.0;

pub const DEFAULT_WIDTH: u16 = 1280;
pub const DEFAULT_HEIGHT: u16 = 720;

/// Events buffered per subscriber before a slow connection starts losing the
/// oldest. The kernel's own tail is far larger; this is the socket's buffer, and
/// a client that falls this far behind needs `query`, not a longer queue.
const EVENT_BACKLOG: usize = 1024;

/// Commands buffered for one session thread before senders wait.
const REQUEST_BACKLOG: usize = 64;

/// The scenes a daemon session can run.
///
/// Shorter than the desktop binary's list on purpose: the other six scenes are
/// shaped to exercise one feature each and are compiled into `rustim-app`'s
/// registry. What a remote client needs is a stage it builds itself (`blank`)
/// and the reference figure with a published golden (`pythagorean`).
pub fn scene_names() -> &'static [&'static str] {
    &["blank", "pythagorean"]
}

/// Whether the session advances itself or waits to be told.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClockMode {
    /// A fixed [`STEP`] every tick, forever.
    #[default]
    Auto,
    /// Nothing moves until `advance` says how far.
    Manual,
}

impl ClockMode {
    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Manual => "manual",
        }
    }
}

/// What `attach` was asked for, before defaults are applied.
///
/// Every field but `session` is a hint on creation and a *constraint* on a
/// reattach: joining an existing session with a different scene, clock or size
/// is refused rather than silently ignored, because a client that states
/// something and is answered "ok" must not be lied to about what it joined.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AttachSpec {
    /// Join this session id. Absent creates a new session.
    pub session: Option<String>,
    /// The scene to construct (see [`scene_names`]). Absent uses the daemon's
    /// default.
    pub scene: Option<String>,
    /// The clock. Absent uses the daemon's default.
    pub clock: Option<ClockMode>,
    /// Viewport width in pixels; bounds in `query` and `camera focus` use it.
    pub width: Option<u16>,
    pub height: Option<u16>,
    /// Motion policy the session starts with; `{"action":"motion"}` changes it
    /// later.
    pub motion: Option<MotionMode>,
}

/// Why an `attach` was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachError {
    /// The params named something no session can be built from.
    Invalid(String),
    /// The session id is not (or is no longer) in the hub.
    NoSuchSession(String),
    /// The session thread could not be started, or its scene panicked.
    Internal(String),
}

/// Why a command could not be answered.
#[derive(Debug, Clone, PartialEq)]
pub enum CommandFailure {
    /// The session was closed while the command was in flight.
    Closed,
    /// The engine understood the command and refused it. This is not a
    /// transport failure: the caller answers with the protocol's own reply.
    Refused(CommandError),
}

/// Why `advance` was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdvanceError {
    Closed,
    /// The session runs an auto clock and cannot be advanced by hand.
    NotManual,
    /// The amount is unusable; the string is the sentence to show.
    Range(String),
}

/// One message to a session thread.
///
/// The command is boxed for the same reason `rustim_app::a11y::Request` boxes
/// it: `Command` is over a quarter-kilobyte (mostly `ShapeParams`), and every
/// other variant here is a few words. One move per command is not a cost worth
/// two hundred bytes of channel slot.
enum Request {
    Command(
        Box<Command>,
        oneshot::Sender<Result<CommandOutcome, CommandError>>,
    ),
    Advance(f64, oneshot::Sender<()>),
    Shutdown,
}

/// A live session: its identity, and the channels to the thread that owns it.
///
/// Cloneable through `Arc` (the hub and every attached connection hold one).
/// Dropping the last handle drops the request sender, which ends the thread.
pub struct SessionHandle {
    id: String,
    scene: String,
    clock: ClockMode,
    width: u16,
    height: u16,
    motion: MotionMode,
    requests: mpsc::Sender<Request>,
    events: broadcast::Sender<Event>,
    now: watch::Receiver<f64>,
}

impl std::fmt::Debug for SessionHandle {
    /// The identity, not the channels: a handle appears in test failures and
    /// operator logs, and the useful facts there are which session and what it
    /// runs.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionHandle")
            .field("id", &self.id)
            .field("scene", &self.scene)
            .field("clock", &self.clock)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("alive", &self.alive())
            .finish()
    }
}

impl SessionHandle {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn scene(&self) -> &str {
        &self.scene
    }

    pub fn clock(&self) -> ClockMode {
        self.clock
    }

    pub fn width(&self) -> u16 {
        self.width
    }

    pub fn height(&self) -> u16 {
        self.height
    }

    pub fn motion(&self) -> MotionMode {
        self.motion
    }

    /// The lecture clock as of the thread's last report.
    ///
    /// A `watch` snapshot rather than a request: `ping` should not queue behind
    /// a long command, and a clock that is a frame or two stale is the right
    /// answer for a liveness check.
    pub fn clock_now(&self) -> f64 {
        *self.now.borrow()
    }

    /// Whether the session thread is still accepting work.
    pub fn alive(&self) -> bool {
        !self.requests.is_closed()
    }

    /// A new subscriber to this session's event stream.
    ///
    /// The stream exists whether or not anyone subscribes; the subscription
    /// command decides what the kernel *publishes*, and this decides who hears
    /// it. A receiver joins at the current point — events that happened before a
    /// client attached are not replayed, because they describe a timeline the
    /// client was not watching. `query` is how a newcomer learns the state.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    /// Apply one command on the session thread, in order with everything else.
    pub async fn command(&self, command: Command) -> Result<CommandOutcome, CommandFailure> {
        let (reply, answer) = oneshot::channel();
        self.requests
            .send(Request::Command(Box::new(command), reply))
            .await
            .map_err(|_| CommandFailure::Closed)?;
        answer
            .await
            .map_err(|_| CommandFailure::Closed)?
            .map_err(CommandFailure::Refused)
    }

    /// Advance a manual session by `seconds` of virtual time.
    pub async fn advance(&self, seconds: f64) -> Result<(), AdvanceError> {
        if self.clock != ClockMode::Manual {
            return Err(AdvanceError::NotManual);
        }
        if !seconds.is_finite() || seconds < 0.0 {
            return Err(AdvanceError::Range(
                "`seconds` must be a finite, non-negative number".to_owned(),
            ));
        }
        if seconds > MAX_ADVANCE_SECONDS {
            return Err(AdvanceError::Range(format!(
                "`seconds` must be at most {MAX_ADVANCE_SECONDS}"
            )));
        }
        let (reply, answer) = oneshot::channel();
        self.requests
            .send(Request::Advance(seconds, reply))
            .await
            .map_err(|_| AdvanceError::Closed)?;
        answer.await.map_err(|_| AdvanceError::Closed)
    }

    fn shutdown(&self) {
        let _ = self.requests.try_send(Request::Shutdown);
    }
}

/// The daemon's defaults, applied to every `attach` that does not say otherwise.
#[derive(Debug, Clone)]
pub struct HubOptions {
    /// Scene for a session created without one.
    pub scene: String,
    /// Clock for a session created without one.
    pub clock: ClockMode,
    /// Whether sessions get a typesetter. `false` makes `hello` advertise no
    /// `text` and no `formula` — the honest answer for a deployment that chose
    /// not to pay for the shaper.
    pub typeset: bool,
}

impl Default for HubOptions {
    fn default() -> Self {
        Self {
            scene: "blank".to_owned(),
            clock: ClockMode::Auto,
            typeset: true,
        }
    }
}

/// Every live session, by id.
///
/// Ids are `s1`, `s2`, … allocated monotonically and **never reused**, including
/// after a `close`: a client holding `s3` must hear "that session is gone", not
/// silently reach whichever session happened to take the number next.
pub struct Hub {
    sessions: tokio::sync::Mutex<HashMap<String, Arc<SessionHandle>>>,
    next: AtomicU64,
    options: HubOptions,
}

impl Hub {
    /// Build a hub. Must be called inside a tokio runtime (it creates channels
    /// that only exist there) — the session threads are spawned on attach.
    pub fn new(options: HubOptions) -> Result<Self, String> {
        if !scene_names().contains(&options.scene.as_str()) {
            return Err(format!(
                "unknown default scene `{}`; this daemon knows: {}",
                options.scene,
                scene_names().join(", ")
            ));
        }
        Ok(Self {
            sessions: tokio::sync::Mutex::new(HashMap::new()),
            next: AtomicU64::new(0),
            options,
        })
    }

    pub fn options(&self) -> &HubOptions {
        &self.options
    }

    /// Join an existing session, or create one.
    ///
    /// The returned flag says which happened, so the reply can tell a reconnecting
    /// client that its session survived rather than that it was replaced.
    pub async fn attach(
        &self,
        spec: AttachSpec,
    ) -> Result<(Arc<SessionHandle>, bool), AttachError> {
        if let Some(id) = spec.session.clone() {
            let sessions = self.sessions.lock().await;
            let Some(handle) = sessions.get(&id).cloned() else {
                return Err(AttachError::NoSuchSession(id));
            };
            drop(sessions);
            check_constraints(&handle, &spec)?;
            return Ok((handle, false));
        }

        let scene = spec
            .scene
            .clone()
            .unwrap_or_else(|| self.options.scene.clone());
        if !scene_names().contains(&scene.as_str()) {
            return Err(AttachError::Invalid(format!(
                "unknown scene `{scene}`; this daemon knows: {}",
                scene_names().join(", ")
            )));
        }
        let clock = spec.clock.unwrap_or(self.options.clock);
        let width = spec.width.unwrap_or(DEFAULT_WIDTH);
        let height = spec.height.unwrap_or(DEFAULT_HEIGHT);
        if width == 0 || height == 0 {
            return Err(AttachError::Invalid(format!(
                "a {width}x{height} viewport has no pixels to project into"
            )));
        }
        let motion = spec.motion.unwrap_or(MotionMode::Full);
        let id = format!("s{}", self.next.fetch_add(1, Ordering::SeqCst) + 1);
        let handle = spawn_session(
            id,
            scene,
            clock,
            width,
            height,
            motion,
            self.options.typeset,
        )
        .await?;
        self.sessions
            .lock()
            .await
            .insert(handle.id.clone(), handle.clone());
        Ok((handle, true))
    }

    /// A live session by id.
    pub async fn get(&self, id: &str) -> Option<Arc<SessionHandle>> {
        self.sessions.lock().await.get(id).cloned()
    }

    /// Close a session: stop its thread and forget its id. Returns whether it
    /// existed.
    pub async fn close(&self, id: &str) -> bool {
        let handle = self.sessions.lock().await.remove(id);
        match handle {
            Some(handle) => {
                handle.shutdown();
                true
            }
            None => false,
        }
    }

    /// How many sessions are live.
    pub async fn len(&self) -> usize {
        self.sessions.lock().await.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.len().await == 0
    }
}

/// Refuse an attach that contradicts what the session already is.
fn check_constraints(handle: &SessionHandle, spec: &AttachSpec) -> Result<(), AttachError> {
    let mismatch = |field: &str, stated: String, actual: String| {
        AttachError::Invalid(format!(
            "session `{}` has {field} `{actual}`, not `{stated}`; attach without `{field}` to join it",
            handle.id
        ))
    };
    if let Some(scene) = &spec.scene
        && scene != handle.scene()
    {
        return Err(mismatch("scene", scene.clone(), handle.scene().to_owned()));
    }
    if let Some(clock) = spec.clock
        && clock != handle.clock()
    {
        return Err(mismatch(
            "clock",
            clock.name().to_owned(),
            handle.clock().name().to_owned(),
        ));
    }
    if let Some(width) = spec.width
        && width != handle.width()
    {
        return Err(mismatch(
            "width",
            width.to_string(),
            handle.width().to_string(),
        ));
    }
    if let Some(height) = spec.height
        && height != handle.height()
    {
        return Err(mismatch(
            "height",
            height.to_string(),
            handle.height().to_string(),
        ));
    }
    Ok(())
}

fn make_scene(name: &str) -> Result<Box<dyn Scene>, AttachError> {
    match name {
        "blank" => Ok(Box::new(BlankScene)),
        "pythagorean" => Ok(Box::new(rustim_scenes::Pythagorean)),
        other => Err(AttachError::Invalid(format!("unknown scene `{other}`"))),
    }
}

/// An empty stage, for a client that will build the scene itself.
struct BlankScene;

impl Scene for BlankScene {
    fn construct(&mut self, _ctx: &mut SceneContext) {}
}

/// Start one session thread and wait until its scene has been constructed.
///
/// The handshake is not decoration: `construct` defines the ids a client is about
/// to address, and a reply saying the session exists must imply they do. Running
/// it on the caller's side is impossible (`SceneContext` is not `Send`), so the
/// thread reports back instead.
async fn spawn_session(
    id: String,
    scene_name: String,
    clock: ClockMode,
    width: u16,
    height: u16,
    motion: MotionMode,
    typeset: bool,
) -> Result<Arc<SessionHandle>, AttachError> {
    let (event_tx, _) = broadcast::channel(EVENT_BACKLOG);
    let (now_tx, now_rx) = watch::channel(0.0);
    let (request_tx, request_rx) = mpsc::channel(REQUEST_BACKLOG);
    let (ready_tx, ready_rx) = oneshot::channel();

    let thread_events = event_tx.clone();
    let thread_scene = scene_name.clone();
    let spawned = std::thread::Builder::new()
        .name(format!("rustim-session-{id}"))
        .spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
            else {
                // Dropping `ready_tx` reports the failure as a closed channel.
                return;
            };
            let mut scene = match make_scene(&thread_scene) {
                Ok(scene) => scene,
                Err(_) => return,
            };
            let viewport = Viewport {
                frame_width: width,
                frame_height: height,
            };
            let mut ctx = SceneContext::new(viewport, Motion::from(motion));
            if typeset {
                ctx.set_typesetter(Box::new(DaemonTypesetter::new()));
            }
            scene.construct(&mut ctx);
            let _ = ready_tx.send(());
            runtime.block_on(run_session(
                scene,
                ctx,
                clock,
                request_rx,
                thread_events,
                now_tx,
            ));
        });
    if let Err(err) = spawned {
        return Err(AttachError::Internal(format!(
            "could not start the session thread: {err}"
        )));
    }
    ready_rx.await.map_err(|_| {
        AttachError::Internal("the session thread stopped before its scene was built".to_owned())
    })?;
    Ok(Arc::new(SessionHandle {
        id,
        scene: scene_name,
        clock,
        width,
        height,
        motion,
        requests: request_tx,
        events: event_tx,
        now: now_rx,
    }))
}

/// The session thread's loop: one clock, one context, one queue.
async fn run_session(
    mut scene: Box<dyn Scene>,
    mut ctx: SceneContext,
    clock: ClockMode,
    mut requests: mpsc::Receiver<Request>,
    events: broadcast::Sender<Event>,
    now: watch::Sender<f64>,
) {
    let _ = now.send(ctx.clock());
    match clock {
        ClockMode::Auto => {
            let mut ticker = tokio::time::interval(Duration::from_secs_f64(STEP));
            // Skip, not Burst: a process that was descheduled for a second must
            // not replay sixty steps at once, which would both lie about wall
            // time and make the state depend on how busy the machine was.
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // The first tick resolves immediately; consume it so `construct`'s
            // state is what clock 0 shows.
            ticker.tick().await;
            loop {
                tokio::select! {
                    biased;
                    request = requests.recv() => {
                        let Some(request) = request else { break };
                        if !service(request, &mut scene, &mut ctx, &events, &now) {
                            break;
                        }
                        let _ = now.send(ctx.clock());
                    }
                    _ = ticker.tick() => {
                        scene.update(STEP, &mut ctx);
                        publish(&mut ctx, &events);
                        let _ = now.send(ctx.clock());
                    }
                }
            }
        }
        ClockMode::Manual => {
            while let Some(request) = requests.recv().await {
                if !service(request, &mut scene, &mut ctx, &events, &now) {
                    break;
                }
                let _ = now.send(ctx.clock());
            }
        }
    }
    // A shutdown is not a failure: the hub closed the session, or the daemon is
    // exiting and dropped the last handle. The library stays silent; a server
    // that wants a log line has the hub's answers to print.
}

/// Handle one message. Returns `false` when the thread should stop.
fn service(
    request: Request,
    scene: &mut Box<dyn Scene>,
    ctx: &mut SceneContext,
    events: &broadcast::Sender<Event>,
    now: &watch::Sender<f64>,
) -> bool {
    match request {
        Request::Command(command, reply) => {
            let outcome = ctx.handle(&command);
            // The scene-visible hook, on the same terms every other host uses:
            // every command that reached the tree, and not a query.
            if let Ok(outcome) = &outcome
                && let Some(diff) = outcome.diff()
            {
                scene.on_command(diff, ctx);
            }
            // Events go out after the outcome is handed back, so a `seek`'s
            // crossed beats follow the reply that asked for them. The two
            // channels are independent, so this is a best-effort ordering; a
            // client orders by the event's `clock`, not by arrival.
            let _ = reply.send(outcome);
            publish(ctx, events);
            true
        }
        Request::Advance(seconds, reply) => {
            let steps = (seconds / STEP).round() as u64;
            for _ in 0..steps {
                scene.update(STEP, ctx);
            }
            publish(ctx, events);
            // The clock reaches the watch **before** the reply does. `advance`
            // answers with the clock the advance produced, and the caller reads
            // it the instant this resolves; reporting afterwards would hand back
            // the time from before the call it is answering.
            let _ = now.send(ctx.clock());
            let _ = reply.send(());
            true
        }
        Request::Shutdown => false,
    }
}

/// Drain the kernel's event log and re-broadcast what the subscription admits.
///
/// The unconditional `take_events` is the point: a session nobody subscribed to
/// still fills the kernel's bounded tail, and a host that only drained when
/// someone was listening would leave the cap to do the dropping. The filter is
/// applied on this side of the drain for the same reason the stdio feed does it —
/// the log is the engine's record, the subscription is the client's interest.
fn publish(ctx: &mut SceneContext, events: &broadcast::Sender<Event>) {
    for event in ctx.take_events() {
        if ctx.wants_event(event.kind()) {
            // Err means no subscribers; a session is allowed to be quiet.
            let _ = events.send(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustim_scene::CommandOutcome;

    async fn hub() -> Hub {
        Hub::new(HubOptions {
            typeset: false,
            ..HubOptions::default()
        })
        .expect("the defaults name a real scene")
    }

    fn blank(clock: ClockMode) -> AttachSpec {
        AttachSpec {
            scene: Some("blank".to_owned()),
            clock: Some(clock),
            ..AttachSpec::default()
        }
    }

    #[tokio::test]
    async fn a_new_session_is_constructed_and_drivable() {
        let hub = hub().await;
        let (session, created) = hub.attach(blank(ClockMode::Manual)).await.expect("attach");
        assert!(created);
        assert_eq!(session.id(), "s1");
        assert_eq!(session.scene(), "blank");
        let outcome = session
            .command(
                rustim_scene::command::parse_command(
                    r#"{"action":"add","type":"circle","params":{"radius":1.0}}"#,
                )
                .unwrap()
                .unwrap(),
            )
            .await
            .expect("apply");
        assert!(matches!(outcome, CommandOutcome::Applied(diff) if diff.created.len() == 1));
        hub.close(session.id()).await;
    }

    #[tokio::test]
    async fn a_second_attach_joins_rather_than_replaces() {
        let hub = hub().await;
        let (first, _) = hub.attach(blank(ClockMode::Manual)).await.expect("attach");
        let (second, created) = hub
            .attach(AttachSpec {
                session: Some(first.id().to_owned()),
                ..AttachSpec::default()
            })
            .await
            .expect("rejoin");
        assert!(!created);
        assert_eq!(first.id(), second.id());
        assert_eq!(hub.len().await, 1);
        hub.close(first.id()).await;
    }

    #[tokio::test]
    async fn an_unknown_session_is_a_refusal_not_a_new_session() {
        let hub = hub().await;
        let error = hub
            .attach(AttachSpec {
                session: Some("s99".to_owned()),
                ..AttachSpec::default()
            })
            .await
            .expect_err("no such session");
        assert_eq!(error, AttachError::NoSuchSession("s99".to_owned()));
        assert_eq!(hub.len().await, 0);
    }

    #[tokio::test]
    async fn ids_are_not_reused_after_close() {
        let hub = hub().await;
        let (first, _) = hub.attach(blank(ClockMode::Manual)).await.expect("attach");
        assert_eq!(first.id(), "s1");
        hub.close(first.id()).await;
        let (second, _) = hub.attach(blank(ClockMode::Manual)).await.expect("attach");
        assert_eq!(second.id(), "s2");
        hub.close(second.id()).await;
    }

    #[tokio::test]
    async fn a_constraint_that_contradicts_the_session_is_refused() {
        let hub = hub().await;
        let (first, _) = hub.attach(blank(ClockMode::Manual)).await.expect("attach");
        let error = hub
            .attach(AttachSpec {
                session: Some(first.id().to_owned()),
                clock: Some(ClockMode::Auto),
                ..AttachSpec::default()
            })
            .await
            .expect_err("clock mismatch");
        assert!(matches!(error, AttachError::Invalid(_)), "{error:?}");
        hub.close(first.id()).await;
    }

    #[tokio::test]
    async fn a_manual_session_advances_only_when_told() {
        let hub = hub().await;
        let (session, _) = hub.attach(blank(ClockMode::Manual)).await.expect("attach");
        // Queued work: a beat that would run on its own under a ticking clock.
        session
            .command(
                rustim_scene::command::parse_command(
                    r#"{"action":"add","type":"square","params":{"width":1.0,"label":"q"}}"#,
                )
                .unwrap()
                .unwrap(),
            )
            .await
            .expect("add");
        session
            .command(
                rustim_scene::command::parse_command(
                    r#"{"action":"play","animation":{"target":"q","type":"fade_in","duration":1.0}}"#,
                )
                .unwrap()
                .unwrap(),
            )
            .await
            .expect("play");
        assert_eq!(session.clock_now(), 0.0);
        session.advance(0.5).await.expect("advance");
        assert!(
            (session.clock_now() - 0.5).abs() < 1e-9,
            "{}",
            session.clock_now()
        );
        hub.close(session.id()).await;
    }

    #[tokio::test]
    async fn advance_is_refused_on_an_auto_session() {
        let hub = hub().await;
        let (session, _) = hub.attach(blank(ClockMode::Auto)).await.expect("attach");
        assert_eq!(
            session.advance(1.0).await.expect_err("auto"),
            AdvanceError::NotManual
        );
        hub.close(session.id()).await;
    }

    #[tokio::test]
    async fn a_malformed_session_names_the_scene_it_could_build() {
        let hub = hub().await;
        let error = hub
            .attach(AttachSpec {
                scene: Some("axes".to_owned()),
                ..AttachSpec::default()
            })
            .await
            .expect_err("not a daemon scene");
        let AttachError::Invalid(message) = error else {
            panic!("expected Invalid");
        };
        assert!(message.contains("pythagorean"), "{message}");
    }
}
