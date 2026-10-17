//! The MCP envelope: JSON-RPC 2.0 over newline-delimited stdio.
//!
//! The wire shape is the Model Context Protocol's, not ours, and the whole job
//! of this module is to answer four methods — `initialize`, `ping`,
//! `tools/list`, `tools/call` — and turn a tool call into a protocol command.
//! The session behind the [`Session`] trait is the only thing the envelope
//! knows about the engine, which is what lets the unit tests drive a real
//! `SceneContext` on the current thread while the binary drives a session
//! thread from `rustim-daemon`.
//!
//! # One message per line
//!
//! MCP's stdio transport is newline-delimited JSON: every message is one line,
//! and JSON escapes any newline inside a string, so the framing is total.
//! Standard output carries messages and nothing else — logs go to standard
//! error — because a client reads this pipe as a protocol, not as a console.
//!
//! # Two kinds of failure
//!
//! * A **JSON-RPC `error`** answers a request the adapter could not honour:
//!   `-32700` for a line that is not JSON, `-32600` for something that is not a
//!   JSON-RPC request (including a batch array — MCP does not use them),
//!   `-32601` for an unknown method, `-32602` for invalid tool arguments, and
//!   `-32603` when the engine session has ended.
//! * A **`tools/call` result with `isError: true`** answers a request the engine
//!   understood and refused. That is MCP's explicit instruction: a tool-level
//!   error reported as a protocol error is invisible to the model, so it cannot
//!   correct itself. The result carries the protocol's own refusal document,
//!   byte for byte, as both text and structured content.
//!
//! # Version negotiation
//!
//! The four published MCP revisions are accepted and echoed back; anything else
//! is answered with the latest we implement, and the client decides whether it
//! can proceed. Only the method set above is served, so the revision does not
//! change any payload except what a client chooses to read.

use serde_json::{Map, Value, json};

use rustim_scene::command::{parse_command, reply_line};
use rustim_scene::{Command, CommandError, CommandOutcome};

use crate::tools;

/// JSON-RPC 2.0 error codes the envelope uses.
pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;

/// The newest MCP revision this adapter implements.
pub const LATEST_PROTOCOL_VERSION: &str = "2025-11-25";

/// Every MCP revision this adapter knows how to answer.
pub const SUPPORTED_PROTOCOL_VERSIONS: [&str; 4] =
    ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];

/// The `serverInfo.name` this adapter reports.
pub const SERVER_NAME: &str = "rustim-mcp";

const INSTRUCTIONS: &str = concat!(
    "rustim is a real-time 2D mathematical animation engine. Build and animate scenes with ",
    "rustim_batch: each command is a rustim protocol v1 object with an `action` (add, set, ",
    "remove, play, camera, narrate, pause, resume, seek, tempo, checkpoint, undo, stop, motion, ",
    "events). Call rustim_capabilities first for the accepted shape names, animation kinds, ",
    "properties and rate curves — they are the engine's own vocabulary, not a guess. New objects ",
    "get ids in the reply's diff.created, and every field that names an object takes either that ",
    "id or the `label` you gave it. The scene runs on a live clock: rustim_batch returns as soon ",
    "as it queues work, while an animation keeps playing. Use rustim_query for the semantic tree ",
    "(names, sizes, pixel bounds, formula structure) and rustim_validate to dry-run a batch. ",
    "Narration queued with the narrate command is timed on the engine's clock; speaking it aloud ",
    "is the assistant's job."
);

/// Why a command could not be applied.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionError {
    /// The engine understood the command and refused it. This is a *tool* error,
    /// not a transport one: the refusal document is the answer.
    Refused(CommandError),
    /// The session thread has ended (closed, or its process is going away).
    Closed,
}

/// Why an `advance` could not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdvanceError {
    Closed,
    /// The session runs an auto clock and advances itself.
    NotManual,
    /// The amount is unusable; the string is the sentence to show.
    Range(String),
}

/// The engine side of the adapter, reduced to the two operations MCP needs.
///
/// The binary implements it over a `rustim_daemon::SessionHandle`; the unit
/// tests implement it over a bare `SceneContext`. Neither the envelope nor the
/// tool catalogue knows which.
pub trait Session {
    fn command(&mut self, command: Command) -> Result<CommandOutcome, SessionError>;
    /// Advance a manual clock, returning the clock the advance reached.
    fn advance(&mut self, seconds: f64) -> Result<f64, AdvanceError>;
}

/// The protocol state machine: one session, one command sequence, one stdio
/// conversation.
pub struct Server<S> {
    session: S,
    seq: u64,
}

impl<S: Session> Server<S> {
    pub fn new(session: S) -> Self {
        Self { session, seq: 0 }
    }

    /// The engine-facing command counter. It numbers protocol replies exactly as
    /// the other transports do, so a reply read over MCP means the same thing as
    /// one read from `--script --reply`.
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// Handle one transport line, returning the line to write back.
    ///
    /// `None` means "write nothing": a blank line, or a notification, which by
    /// JSON-RPC never has a response.
    pub fn handle_line(&mut self, line: &str) -> Option<String> {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }
        let value: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(error) => {
                return Some(
                    RpcError::new(PARSE_ERROR, format!("not JSON: {error}")).line(&Value::Null),
                );
            }
        };
        let Some(message) = value.as_object() else {
            return Some(
                RpcError::new(
                    INVALID_REQUEST,
                    "an MCP message is one JSON object; JSON-RPC batches are not part of MCP",
                )
                .line(&Value::Null),
            );
        };
        let id = match message.get("id") {
            None => None,
            Some(Value::Null) => Some(Value::Null),
            Some(value @ (Value::String(_) | Value::Number(_))) => Some(value.clone()),
            Some(_) => {
                return Some(
                    RpcError::new(INVALID_REQUEST, "`id` must be a string, a number, or null")
                        .line(&Value::Null),
                );
            }
        };
        if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Some(
                RpcError::new(INVALID_REQUEST, "`jsonrpc` must be \"2.0\"")
                    .line(id.as_ref().unwrap_or(&Value::Null)),
            );
        }
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            // A response (we never send requests, so it is unsolicited) or a
            // malformed notification. Only an id-bearing message can be answered.
            return id.map(|id| {
                RpcError::new(
                    INVALID_REQUEST,
                    "a message without `method` is not a request",
                )
                .line(&id)
            });
        };
        if method.starts_with("notifications/") {
            // `notifications/initialized`, `notifications/cancelled`, a client's
            // list-changed notice: no reply, ever, even if a confused client
            // gave it an id.
            return None;
        }
        let params = message.get("params");
        let outcome = self.dispatch(method, params);
        let Some(id) = id else {
            // A request-shaped method sent as a notification executes (that is
            // what a notification is) and is not answered.
            return None;
        };
        Some(match outcome {
            Ok(result) => result_line(&id, result),
            Err(error) => error.line(&id),
        })
    }

    fn dispatch(&mut self, method: &str, params: Option<&Value>) -> Result<Value, RpcError> {
        match method {
            "initialize" => self.initialize(params),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(tools::list_result()),
            "tools/call" => self.call_tool(params),
            other => Err(RpcError::new(
                METHOD_NOT_FOUND,
                format!("unknown method `{other}`"),
            )),
        }
    }

    fn initialize(&mut self, params: Option<&Value>) -> Result<Value, RpcError> {
        let Some(params) = params.and_then(Value::as_object) else {
            return Err(invalid_params(
                "`initialize` needs an object of params with `protocolVersion`",
            ));
        };
        let Some(requested) = params.get("protocolVersion").and_then(Value::as_str) else {
            return Err(invalid_params(
                "`initialize.params.protocolVersion` must be a string",
            ));
        };
        // Echo a revision we implement; otherwise name the newest, as the
        // protocol requires — the client decides whether it can continue.
        let version = if SUPPORTED_PROTOCOL_VERSIONS.contains(&requested) {
            requested
        } else {
            LATEST_PROTOCOL_VERSION
        };
        Ok(json!({
            "protocolVersion": version,
            "capabilities": {"tools": {}},
            "serverInfo": {
                "name": SERVER_NAME,
                "version": env!("CARGO_PKG_VERSION"),
            },
            "instructions": INSTRUCTIONS,
        }))
    }

    fn call_tool(&mut self, params: Option<&Value>) -> Result<Value, RpcError> {
        let Some(params) = params.and_then(Value::as_object) else {
            return Err(invalid_params(
                "`tools/call` needs an object of params with `name`",
            ));
        };
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            return Err(invalid_params("`tools/call.params.name` must be a string"));
        };
        let arguments = match params.get("arguments") {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(arguments)) => arguments.clone(),
            Some(_) => {
                return Err(invalid_params(
                    "`tools/call.params.arguments` must be an object",
                ));
            }
        };
        let call = tools::translate(name, &arguments).map_err(invalid_params)?;
        match call {
            tools::Call::Capabilities => self.run(json!({"action": "hello", "protocol": 1})),
            tools::Call::Batch(commands) => {
                self.run(json!({"action": "batch", "commands": commands}))
            }
            tools::Call::Validate(commands) => {
                self.run(json!({"action": "validate", "commands": commands}))
            }
            tools::Call::Query(target) => {
                let mut command = json!({"action": "query"});
                if let Some(target) = target
                    && let Some(object) = command.as_object_mut()
                {
                    object.insert("target".to_owned(), target);
                }
                self.run(command)
            }
            tools::Call::Advance(seconds) => self.advance(seconds),
        }
    }

    /// Apply one protocol command and wrap its reply as a tool result.
    fn run(&mut self, command: Value) -> Result<Value, RpcError> {
        let text = command.to_string();
        let Some(parsed) = parse_command(&text) else {
            return Err(invalid_params("the tool arguments produced no command"));
        };
        self.seq += 1;
        let (reply, is_error) = match parsed {
            Ok(command) => match self.session.command(command) {
                Ok(outcome) => (reply_line(self.seq, Ok(outcome)), false),
                Err(SessionError::Refused(error)) => (reply_line(self.seq, Err(error)), true),
                Err(SessionError::Closed) => {
                    return Err(RpcError::new(
                        INTERNAL_ERROR,
                        "the engine session has ended",
                    ));
                }
            },
            // The engine could not read the command the tool arguments built.
            // That is still the engine's answer, and it is a value: a model that
            // sent an unknown `action` should see `unparseable`, not a transport
            // failure that says nothing about what to fix.
            Err(error) => (reply_line(self.seq, Err(error)), true),
        };
        let value: Value = serde_json::from_str(&reply).map_err(|_| {
            RpcError::new(INTERNAL_ERROR, "the engine's reply is not a JSON document")
        })?;
        Ok(tool_result(value, is_error))
    }

    fn advance(&mut self, seconds: f64) -> Result<Value, RpcError> {
        match self.session.advance(seconds) {
            Ok(clock) => Ok(tool_result(
                json!({"advanced": true, "seconds": seconds, "clock": clock}),
                false,
            )),
            Err(AdvanceError::Closed) => Err(RpcError::new(
                INTERNAL_ERROR,
                "the engine session has ended",
            )),
            Err(AdvanceError::NotManual) => Ok(tool_result(
                json!({
                    "advanced": false,
                    "error": "not_manual",
                    "detail": concat!(
                        "this session runs an auto clock and advances itself; start ",
                        "rustim-mcp with --clock manual to drive it"
                    ),
                }),
                true,
            )),
            Err(AdvanceError::Range(detail)) => Ok(tool_result(
                json!({"advanced": false, "error": "out_of_range", "detail": detail}),
                true,
            )),
        }
    }
}

/// One `CallToolResult`.
///
/// The reply document is carried twice on purpose: as text (what every client
/// shows the model) and as `structuredContent` (what a client with a schema can
/// read). They are the same JSON, stringified once.
fn tool_result(value: Value, is_error: bool) -> Value {
    let text = value.to_string();
    let mut result = json!({
        "content": [{"type": "text", "text": text}],
        "isError": is_error,
    });
    if value.is_object()
        && let Some(object) = result.as_object_mut()
    {
        object.insert("structuredContent".to_owned(), value);
    }
    result
}

fn result_line(id: &Value, result: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
}

/// A JSON-RPC error: the adapter could not honour the request at all.
struct RpcError {
    code: i64,
    message: String,
}

impl RpcError {
    fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn line(&self, id: &Value) -> String {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": self.code, "message": self.message},
        })
        .to_string()
    }
}

fn invalid_params(message: impl Into<String>) -> RpcError {
    RpcError::new(INVALID_PARAMS, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    use rustim_anim::Motion;
    use rustim_math::Viewport;
    use rustim_scene::SceneContext;

    /// A session that is a bare `SceneContext` on this thread — the same engine
    /// the binary runs, minus the thread and the clock driver.
    struct Local {
        ctx: SceneContext,
    }

    impl Local {
        fn new() -> Self {
            Self {
                ctx: SceneContext::new(Viewport::default(), Motion::Full),
            }
        }
    }

    impl Session for Local {
        fn command(&mut self, command: Command) -> Result<CommandOutcome, SessionError> {
            self.ctx.handle(&command).map_err(SessionError::Refused)
        }

        fn advance(&mut self, seconds: f64) -> Result<f64, AdvanceError> {
            let steps = (seconds * 60.0).round() as u64;
            for _ in 0..steps {
                self.ctx.advance(1.0 / 60.0);
            }
            Ok(self.ctx.clock())
        }
    }

    fn server() -> Server<Local> {
        Server::new(Local::new())
    }

    fn request(server: &mut Server<Local>, method: &str, params: Value) -> Value {
        let line = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let reply = server
            .handle_line(&line.to_string())
            .expect("a request is answered");
        serde_json::from_str(&reply).expect("a JSON reply")
    }

    fn call(server: &mut Server<Local>, name: &str, arguments: Value) -> Value {
        request(
            server,
            "tools/call",
            json!({"name": name, "arguments": arguments}),
        )
    }

    #[test]
    fn initialize_negotiates_and_names_the_tools() {
        let mut server = server();
        let reply = request(
            &mut server,
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "0"},
            }),
        );
        assert_eq!(reply["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(reply["result"]["serverInfo"]["name"], SERVER_NAME);
        assert!(reply["result"]["capabilities"]["tools"].is_object());
        assert!(
            reply["result"]["instructions"]
                .as_str()
                .expect("instructions")
                .contains("rustim_batch")
        );

        // A revision we do not know gets the newest we implement.
        let reply = request(
            &mut server,
            "initialize",
            json!({"protocolVersion": "1999-01-01"}),
        );
        assert_eq!(reply["result"]["protocolVersion"], LATEST_PROTOCOL_VERSION);

        let listed = request(&mut server, "tools/list", json!({}));
        let names: Vec<&str> = listed["result"]["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .map(|tool| tool["name"].as_str().expect("a name"))
            .collect();
        assert_eq!(names, tools::NAMES);
    }

    #[test]
    fn a_batch_builds_an_object_and_a_query_reads_it_back() {
        let mut server = server();
        let reply = call(
            &mut server,
            "rustim_batch",
            json!({"commands": [
                {"action": "add", "type": "circle", "params": {"radius": 1.0, "label": "圆"}},
                {"action": "set", "target": "圆", "property": {"name": "opacity", "value": 0.5}},
            ]}),
        );
        assert_eq!(reply["result"]["isError"], false);
        let structured = &reply["result"]["structuredContent"];
        let created = structured["diff"]["created"].as_array().expect("created");
        assert_eq!(created.len(), 1);
        // The text content is the same document, not a second rendering of it.
        let text = reply["result"]["content"][0]["text"]
            .as_str()
            .expect("text");
        assert_eq!(
            serde_json::from_str::<Value>(text).expect("JSON"),
            *structured
        );
        // The net diff counts a node once: an object created and then modified
        // inside the same batch is `created`, not `modified` — the tree as it
        // stands afterwards is what the diff describes.
        assert_eq!(structured["diff"]["modified"], json!([]));

        let reply = call(&mut server, "rustim_query", json!({"target": "圆"}));
        assert_eq!(reply["result"]["isError"], false);
        let tree = &reply["result"]["structuredContent"]["tree"];
        assert_eq!(tree["root"], created[0]);
        let nodes = tree["nodes"].as_array().expect("nodes");
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0]["name"], "圆");
        assert!(nodes[0]["bounds"].is_object());
    }

    #[test]
    fn an_engine_refusal_is_a_tool_error_and_the_session_survives() {
        let mut server = server();
        let reply = call(
            &mut server,
            "rustim_batch",
            json!({"commands": [
                {"action": "set", "target": "nope", "property": {"name": "opacity", "value": 0.5}},
            ]}),
        );
        assert_eq!(reply["result"]["isError"], true);
        let refusal = &reply["result"]["structuredContent"];
        assert_eq!(refusal["error"]["error"], "in_batch");
        assert_eq!(refusal["error"]["index"], 0);
        assert_eq!(refusal["error"]["cause"]["error"], "no_such_label");
        assert_eq!(refusal["error"]["cause"]["label"], "nope");

        // A refusal is not a broken pipe: the next call is answered normally.
        let reply = request(&mut server, "ping", json!({}));
        assert_eq!(reply["result"], json!({}));
    }

    #[test]
    fn validate_changes_nothing_and_then_the_same_batch_applies() {
        let mut server = server();
        let commands = json!([
            {"action": "add", "type": "square", "params": {"width": 1.0, "label": "方"}},
        ]);
        let reply = call(
            &mut server,
            "rustim_validate",
            json!({"commands": commands}),
        );
        assert_eq!(reply["result"]["isError"], false);
        assert_eq!(
            reply["result"]["structuredContent"]["diff"]["created"],
            json!([])
        );

        let reply = call(&mut server, "rustim_query", json!({}));
        let nodes = reply["result"]["structuredContent"]["tree"]["nodes"]
            .as_array()
            .expect("nodes");
        assert!(!nodes.iter().any(|node| node["name"] == "方"));

        let reply = call(&mut server, "rustim_batch", json!({"commands": commands}));
        assert_eq!(reply["result"]["isError"], false);
        let reply = call(&mut server, "rustim_query", json!({}));
        let nodes = reply["result"]["structuredContent"]["tree"]["nodes"]
            .as_array()
            .expect("nodes");
        assert!(nodes.iter().any(|node| node["name"] == "方"));
    }

    #[test]
    fn invalid_arguments_are_protocol_errors_and_execute_nothing() {
        let mut server = server();
        let reply = call(&mut server, "rustim_nope", json!({}));
        assert_eq!(reply["error"]["code"], INVALID_PARAMS);
        assert!(
            reply["error"]["message"]
                .as_str()
                .expect("a message")
                .contains("rustim_batch")
        );

        let reply = call(&mut server, "rustim_batch", json!({"commands": "add"}));
        assert_eq!(reply["error"]["code"], INVALID_PARAMS);

        let reply = call(&mut server, "rustim_query", json!({"target": 1.5}));
        assert_eq!(reply["error"]["code"], INVALID_PARAMS);

        let reply = call(&mut server, "rustim_advance", json!({"seconds": -1.0}));
        assert_eq!(reply["error"]["code"], INVALID_PARAMS);

        // The session is untouched by any of them.
        assert_eq!(server.seq(), 0);
        let reply = call(&mut server, "rustim_query", json!({}));
        let nodes = reply["result"]["structuredContent"]["tree"]["nodes"]
            .as_array()
            .expect("nodes");
        assert_eq!(nodes.len(), 1, "only the root");
    }

    #[test]
    fn the_envelope_refuses_batches_bad_json_and_unknown_methods() {
        let mut server = server();
        let reply = server.handle_line("[]").expect("answered");
        assert_eq!(
            serde_json::from_str::<Value>(&reply).expect("JSON")["error"]["code"],
            INVALID_REQUEST
        );
        let reply = server.handle_line("{oops").expect("answered");
        assert_eq!(
            serde_json::from_str::<Value>(&reply).expect("JSON")["error"]["code"],
            PARSE_ERROR
        );
        let reply = request(&mut server, "resources/list", json!({}));
        assert_eq!(reply["error"]["code"], METHOD_NOT_FOUND);
        // Blank lines and notifications produce nothing.
        assert_eq!(server.handle_line("   "), None);
        assert_eq!(
            server.handle_line(
                &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string()
            ),
            None
        );
    }

    #[test]
    fn advance_reports_the_clock_it_reached() {
        let mut server = server();
        let reply = call(&mut server, "rustim_advance", json!({"seconds": 0.5}));
        assert_eq!(reply["result"]["isError"], false);
        let structured = &reply["result"]["structuredContent"];
        assert_eq!(structured["advanced"], true);
        // Thirty steps of 1/60 are not exactly 0.5 in f64 — the daemon advances
        // on the same grid the frame loop uses, so the clock is that sum and
        // not the argument.
        let clock = structured["clock"].as_f64().expect("a number");
        assert!((clock - 0.5).abs() < 1e-9, "{clock}");
    }
}
