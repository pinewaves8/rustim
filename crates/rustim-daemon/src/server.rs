//! The socket side: accept WebSocket connections, dispatch JSON-RPC frames.
//!
//! # One connection is one client
//!
//! A connection carries at most one attachment at a time ([`Connection::attach`]
//! replaces it), a per-connection `seq` counter, and its own event forwarder
//! task. Nothing about a connection is shared with another, so a client that
//! reconnects starts clean — except that the *session* it names survives, which
//! is the whole reason `attach` takes an id.
//!
//! # Replies and events share one writer
//!
//! A WebSocket sink cannot be written from two places at once, so every outbound
//! frame — reply, error, event — goes through one `mpsc` channel into one writer
//! task. That makes frames from this connection strictly ordered; it does *not*
//! order a reply against an event that the same command produced, because the
//! actor hands the outcome back and broadcasts the event on two channels. The
//! protocol's correlation is by JSON-RPC `id` for replies and by the event's
//! `clock` for events, so no client should depend on the cross-channel order;
//! the stdio feed is the transport with a line order, and this one is not.
//!
//! # `seq` on a socket
//!
//! The protocol's `seq` is "the 1-based physical line number of the command this
//! replies to". A frame has no line number, so the daemon counts the *action
//! commands this connection has sent* — the closest honest analogue, and the
//! same counter in the same unit. It starts at 1, is incremented for a command
//! that parses and for one the protocol parser refuses (both are answered), and
//! is not touched by the daemon's own methods, which have no protocol reply.
//! JSON-RPC's `id` is the correlation a client should use; `seq` is kept because
//! the reply document is the protocol's and is not rewritten here.

use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;

use rustim_scene::command::{parse_command, reply_line};
use rustim_scene::{Event, command::MotionMode};

use crate::rpc::{self, RpcError};
use crate::session::{AdvanceError, AttachError, AttachSpec, CommandFailure, Hub, SessionHandle};

/// How many outbound frames may queue before the writer is considered stuck.
const OUTBOUND_BACKLOG: usize = 256;

/// Accept connections until the listener fails.
///
/// Each connection is its own tokio task; a failed handshake or a malformed
/// frame ends that connection and is logged, not the server. The session state
/// does not live here (see [`crate::session`]), so a dropped socket can never
/// stall a timeline.
pub async fn serve(listener: TcpListener, hub: Arc<Hub>) -> std::io::Result<()> {
    loop {
        let (stream, peer) = listener.accept().await?;
        let hub = hub.clone();
        tokio::spawn(async move {
            if let Err(detail) = serve_connection(stream, hub).await {
                eprintln!("rustim-daemon: connection {peer} ended: {detail}");
            }
        });
    }
}

/// One accepted connection, start to finish.
pub async fn serve_connection(stream: TcpStream, hub: Arc<Hub>) -> Result<(), String> {
    let ws = accept_async(stream)
        .await
        .map_err(|err| format!("websocket handshake: {err}"))?;
    let (mut sink, mut incoming) = ws.split();
    let (out, mut outbound) = mpsc::channel::<Message>(OUTBOUND_BACKLOG);
    let writer = tokio::spawn(async move {
        while let Some(message) = outbound.recv().await {
            if sink.send(message).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });

    let mut connection = Connection {
        hub,
        out: out.clone(),
        attached: None,
        events: None,
        seq: 0,
    };
    let mut result = Ok(());
    while let Some(message) = incoming.next().await {
        let message = match message {
            Ok(message) => message,
            Err(err) => {
                result = Err(err.to_string());
                break;
            }
        };
        match message {
            Message::Text(text) => {
                if !connection.dispatch(text.as_str()).await {
                    break;
                }
            }
            // The protocol is text; a binary frame is not a command in an
            // encoding we know, and guessing one would be worse than saying so.
            Message::Binary(_) => {
                connection
                    .send_error(
                        RpcError::new(
                            rpc::INVALID_REQUEST,
                            "the protocol is JSON text; binary frames are not commands",
                        ),
                        None,
                    )
                    .await;
            }
            Message::Ping(payload) => {
                let _ = connection.out.send(Message::Pong(payload)).await;
            }
            Message::Pong(_) | Message::Frame(_) => {}
            Message::Close(_) => break,
        }
    }
    connection.release();
    drop(out);
    drop(connection);
    let _ = writer.await;
    result
}

/// What one connection is: its attachment, its counter and its way out.
struct Connection {
    hub: Arc<Hub>,
    out: mpsc::Sender<Message>,
    attached: Option<Arc<SessionHandle>>,
    events: Option<tokio::task::JoinHandle<()>>,
    seq: u64,
}

impl Connection {
    /// Handle one text frame. Returns `false` when the connection should close.
    async fn dispatch(&mut self, text: &str) -> bool {
        let request = match rpc::parse(text) {
            Ok(request) => request,
            Err(error) => {
                // A parse error has no readable id; JSON-RPC requires `null`
                // there, and this is still an answer worth sending.
                return self.send_error(error, None).await;
            }
        };
        let id = request.id.clone();
        let outcome = match request.method.as_str() {
            "attach" => self.attach(request.params).await,
            "detach" => Ok(self.detach()),
            "close" => self.close(&request.params).await,
            "ping" => Ok(self.ping()),
            "advance" => self.advance(&request.params).await,
            method if rpc::is_action(method) => {
                return self.action(id.as_ref(), method, &request.params).await;
            }
            method => Err(RpcError::new(
                rpc::METHOD_NOT_FOUND,
                format!(
                    "no method `{method}`; the protocol's actions are {} and the daemon's own are {}",
                    rustim_scene::command::ACTIONS.join(", "),
                    rpc::DAEMON_METHODS.join(", "),
                ),
            )),
        };
        self.respond(id.as_ref(), outcome).await
    }

    /// Apply one protocol action on the attached session.
    async fn action(&mut self, id: Option<&Value>, method: &str, params: &Value) -> bool {
        let Some(session) = self.attached.clone() else {
            return self
                .respond(
                    id,
                    Err(RpcError::new(
                        rpc::NOT_ATTACHED,
                        "no session attached; send {\"method\":\"attach\"} first",
                    )),
                )
                .await;
        };
        let text = rpc::command_text(method, params);
        let Some(parsed) = parse_command(&text) else {
            // `command_text` always yields a non-empty object, so this arm is
            // unreachable; it returns a transport error rather than panicking
            // because a host must never die on its own serialisation.
            return self
                .respond(
                    id,
                    Err(RpcError::new(rpc::INTERNAL_ERROR, "the command was empty")),
                )
                .await;
        };
        self.seq += 1;
        let seq = self.seq;
        // A command the protocol parser refuses is still *answered*, under the
        // protocol's own vocabulary — the same `Unparseable` a file feed gives.
        let outcome = match parsed {
            Err(error) => Err(error),
            Ok(command) => match session.command(command).await {
                Ok(outcome) => Ok(outcome),
                Err(CommandFailure::Refused(error)) => Err(error),
                Err(CommandFailure::Closed) => {
                    self.release();
                    return self
                        .respond(
                            id,
                            Err(RpcError::new(
                                rpc::NO_SUCH_SESSION,
                                "the session closed before it could answer; attach again",
                            )),
                        )
                        .await;
                }
            },
        };
        self.respond(id, Ok(reply_value(seq, outcome))).await
    }

    async fn attach(&mut self, params: Value) -> Result<Value, RpcError> {
        let spec: AttachSpec = serde_json::from_value(params)
            .map_err(|err| RpcError::new(rpc::INVALID_PARAMS, format!("attach params: {err}")))?;
        let (session, created) = self.hub.attach(spec).await.map_err(|error| match error {
            AttachError::Invalid(detail) => RpcError::new(rpc::INVALID_PARAMS, detail),
            AttachError::NoSuchSession(id) => RpcError::new(
                rpc::NO_SUCH_SESSION,
                format!("no session `{id}`; it was never created or has been closed"),
            ),
            AttachError::Internal(detail) => RpcError::new(rpc::INTERNAL_ERROR, detail),
        })?;
        self.bind(session.clone());
        Ok(json!({
            "session": session.id(),
            "created": created,
            "scene": session.scene(),
            "clock": session.clock().name(),
            "width": session.width(),
            "height": session.height(),
            "motion": match session.motion() {
                MotionMode::Full => "full",
                MotionMode::Reduced => "reduced",
            },
        }))
    }

    fn detach(&mut self) -> Value {
        let session = self.attached.take().map(|s| s.id().to_owned());
        if let Some(task) = self.events.take() {
            task.abort();
        }
        json!({ "session": session, "attached": false })
    }

    async fn close(&mut self, params: &Value) -> Result<Value, RpcError> {
        let target = match params.get("session") {
            Some(Value::String(id)) => id.clone(),
            Some(_) => {
                return Err(RpcError::new(
                    rpc::INVALID_PARAMS,
                    "`session` must be a session id string",
                ));
            }
            None => match &self.attached {
                Some(session) => session.id().to_owned(),
                None => {
                    return Err(RpcError::new(
                        rpc::INVALID_PARAMS,
                        "close names no session and none is attached",
                    ));
                }
            },
        };
        let closed = self.hub.close(&target).await;
        if self.attached.as_ref().map(|s| s.id()) == Some(target.as_str()) {
            self.attached = None;
            if let Some(task) = self.events.take() {
                task.abort();
            }
        }
        Ok(json!({ "session": target, "closed": closed }))
    }

    fn ping(&self) -> Value {
        json!({
            "pong": true,
            "session": self.attached.as_ref().map(|s| s.id()),
            "clock": self.attached.as_ref().map(|s| s.clock_now()),
            "alive": self.attached.as_ref().map(|s| s.alive()),
        })
    }

    async fn advance(&mut self, params: &Value) -> Result<Value, RpcError> {
        let seconds = params
            .get("seconds")
            .and_then(Value::as_f64)
            .ok_or_else(|| {
                RpcError::new(rpc::INVALID_PARAMS, "advance needs a numeric `seconds`")
            })?;
        let Some(session) = self.attached.clone() else {
            return Err(RpcError::new(
                rpc::NOT_ATTACHED,
                "no session attached; send {\"method\":\"attach\"} first",
            ));
        };
        match session.advance(seconds).await {
            Ok(()) => Ok(json!({
                "advanced": true,
                "seconds": seconds,
                "clock": session.clock_now(),
            })),
            Err(AdvanceError::NotManual) => Err(RpcError::new(
                rpc::NOT_MANUAL,
                "this session advances itself (clock `auto`); create one with \"clock\":\"manual\" to drive it",
            )),
            Err(AdvanceError::Range(detail)) => Err(RpcError::new(rpc::INVALID_PARAMS, detail)),
            Err(AdvanceError::Closed) => {
                self.release();
                Err(RpcError::new(
                    rpc::NO_SUCH_SESSION,
                    "the session closed before it could answer; attach again",
                ))
            }
        }
    }

    /// Replace the attachment: subscribe the new session, stop the old
    /// forwarder. Attaching twice to the same session resubscribes — events
    /// between the two attaches are not replayed, because a subscriber's stream
    /// is what happened while it was listening.
    fn bind(&mut self, session: Arc<SessionHandle>) {
        if let Some(task) = self.events.take() {
            task.abort();
        }
        let receiver = session.subscribe();
        let out = self.out.clone();
        self.events = Some(tokio::spawn(forward_events(receiver, out)));
        self.attached = Some(session);
    }

    fn release(&mut self) {
        self.attached = None;
        if let Some(task) = self.events.take() {
            task.abort();
        }
    }

    /// Answer `id` with a result or a transport error. A notification (no id) is
    /// never answered, but its work has already been done.
    async fn respond(&mut self, id: Option<&Value>, outcome: Result<Value, RpcError>) -> bool {
        let Some(id) = id else {
            return true;
        };
        let line = match outcome {
            Ok(result) => rpc::result_message(id, result),
            Err(error) => error.message(Some(id)).to_string(),
        };
        self.send_text(line).await
    }

    async fn send_error(&mut self, error: RpcError, id: Option<&Value>) -> bool {
        let line = error.message(id).to_string();
        self.send_text(line).await
    }

    async fn send_text(&mut self, line: String) -> bool {
        self.out.send(Message::text(line)).await.is_ok()
    }
}

/// Forward one session's events to one connection until either end closes.
///
/// A lagging client loses the *oldest* events rather than stalling the actor:
/// `Lagged` is skipped, because the alternative — waiting — would make one slow
/// browser slow down every other client's timeline. A client that has fallen
/// this far behind should `query`; the stream is a notification, not a ledger.
async fn forward_events(
    mut receiver: tokio::sync::broadcast::Receiver<Event>,
    out: mpsc::Sender<Message>,
) {
    loop {
        match receiver.recv().await {
            Ok(event) => {
                if out
                    .send(Message::text(rpc::event_message(&event)))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        }
    }
}

/// The protocol's reply document as a JSON value.
///
/// `reply_line` already returns serialised JSON built from types that round-trip
/// by construction (§ "Commands round-trip byte-for-byte"), so the parse cannot
/// fail for a reason a caller could act on — the same reason `reply_line` itself
/// takes the `expect` it does.
fn reply_value(
    seq: u64,
    outcome: Result<rustim_scene::CommandOutcome, rustim_scene::CommandError>,
) -> Value {
    serde_json::from_str(&reply_line(seq, outcome)).expect("a reply is JSON by construction")
}
