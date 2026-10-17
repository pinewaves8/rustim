//! The JSON-RPC 2.0 envelope the daemon speaks over its WebSocket.
//!
//! # Why an envelope at all
//!
//! `rustim_scene::command` already owns the *payload*: a command is one JSON
//! object with an `"action"`, a reply is one with `"seq"` and exactly one of
//! `diff` / `tree` / `report` / `capabilities` / `error`. The stdio feed prepends nothing
//! because a pipe is one ordered line stream and every line is a command. A
//! socket is different in three ways that the envelope has to answer:
//!
//! * **Correlation.** A browser can have several requests in flight; a reply
//!   line alone does not say which one it answers. JSON-RPC's `id` does, and it
//!   is echoed as-is (a number or a string).
//! * **Framing.** There is no line-oriented transport; a message is one
//!   WebSocket text frame, and the document is the frame.
//! * **A second channel.** Events arrive unsolicited, interleaved with replies.
//!   JSON-RPC has notifications for exactly that, and its `method` field is the
//!   discriminant a client reads without parsing a body.
//!
//! # The method vocabulary is the protocol's
//!
//! Every action in [`rustim_scene::command::ACTIONS`] (`add`, `play`, `set`,
//! `query`, `events`, …) is a JSON-RPC method here, with **the command's own
//! fields as `params`** and no `"action"` member — the method *is* the action.
//! The daemon turns `{"method":"add","params":{…}}` into
//! `{"action":"add",…}` and hands it to the same
//! [`SceneContext::handle`](rustim_scene::SceneContext::handle) the feed uses,
//! so the two transports cannot drift into two dialects of the same command.
//! A nested `batch` command is the one exception by design: its `commands`
//! array carries ordinary protocol command documents, each with its own
//! `"action"`, exactly as the protocol defines them.
//!
//! Five methods are the daemon's own, because they are about *sessions* rather
//! than about a scene: `attach`, `detach`, `close`, `ping`, `advance`. They are
//! not protocol actions and are not claimed by [`ACTIONS`]; a client discovers
//! them from the daemon's `--help` and its documentation, not from `hello`.
//!
//! # Two kinds of failure, two places for them
//!
//! A **kernel refusal** ("no such node", "opacity out of range", a command that
//! does not parse) is a successful RPC call whose result is the protocol's own
//! reply document: `{"jsonrpc":"2.0","id":1,"result":{"seq":3,"error":{…}}}`.
//! The client asked a question the engine understood and answered "no"; that is
//! the protocol's vocabulary, and translating it into a JSON-RPC `error` would
//! throw away the `kind`, the property name and the candidate ids. JSON-RPC
//! `error` is reserved for the transport: malformed JSON, an unknown method,
//! wrong-shaped params, not being attached to anything. The split is the same
//! one the stdio feed makes between "refused" (one exit-code-zero line) and
//! "unreadable" — and keeping it identical is what lets a client written
//! against the feed move to the socket with only its framing changed.

use serde_json::{Value, json};

/// The request was not JSON at all.
pub const PARSE_ERROR: i64 = -32700;
/// JSON, but not a JSON-RPC request (missing `jsonrpc`, a non-string method, …).
pub const INVALID_REQUEST: i64 = -32600;
/// No such method: neither a protocol action nor a daemon method.
pub const METHOD_NOT_FOUND: i64 = -32601;
/// The params are not an object, or a daemon method found them unusable.
pub const INVALID_PARAMS: i64 = -32602;
/// A bug on our side — the request was well-formed and we could not answer.
pub const INTERNAL_ERROR: i64 = -32603;

/// A command was sent before `attach`.
pub const NOT_ATTACHED: i64 = -32001;
/// The named session does not exist (never created, or already closed).
pub const NO_SUCH_SESSION: i64 = -32002;
/// `advance` on a session that advances itself.
pub const NOT_MANUAL: i64 = -32003;

/// The daemon's own methods, beside the protocol's actions.
///
/// Kept as one list so `--help`, the dispatcher and the tests name the same
/// five; a method that is neither here nor in the protocol's [`ACTIONS`] is
/// [`METHOD_NOT_FOUND`]. `ping` deliberately shares its name with nothing in
/// the protocol — a WebSocket ping frame is a different thing, and the daemon
/// answers both.
///
/// [`ACTIONS`]: rustim_scene::command::ACTIONS
pub const DAEMON_METHODS: &[&str] = &["advance", "attach", "close", "detach", "ping"];

/// A transport-level failure, ready to be written as a JSON-RPC `error`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// The error object alone, without the envelope.
    pub fn body(&self) -> Value {
        json!({ "code": self.code, "message": self.message })
    }

    /// The full message. `id` is `None` only when the request's id could not be
    /// read (a parse error), and JSON-RPC requires `null` in that position.
    pub fn message(&self, id: Option<&Value>) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": id.cloned().unwrap_or(Value::Null),
            "error": self.body(),
        })
    }
}

/// One parsed request. A request without `id` is a notification: it is applied
/// and answered with nothing.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub id: Option<Value>,
    pub method: String,
    pub params: Value,
}

/// Parse one WebSocket text frame as a JSON-RPC request.
///
/// The strictness here is deliberate and limited: `jsonrpc` must be exactly the
/// string `"2.0"` (the one field every 2.0 client sends), `method` a string, and
/// `params` — if present — an object rather than the array form the spec also
/// allows. The protocol's params are named fields, so an array could only be
/// ambiguous; refusing it says so instead of guessing an order.
pub fn parse(text: &str) -> Result<Request, RpcError> {
    let value: Value = serde_json::from_str(text)
        .map_err(|e| RpcError::new(PARSE_ERROR, format!("not JSON: {e}")))?;
    let Some(object) = value.as_object() else {
        return Err(RpcError::new(
            INVALID_REQUEST,
            "a request must be a JSON object",
        ));
    };
    match object.get("jsonrpc") {
        Some(Value::String(version)) if version == "2.0" => {}
        Some(_) => {
            return Err(RpcError::new(
                INVALID_REQUEST,
                "`jsonrpc` must be the string \"2.0\"",
            ));
        }
        None => {
            return Err(RpcError::new(
                INVALID_REQUEST,
                "`jsonrpc` is required and must be \"2.0\"",
            ));
        }
    }
    let Some(Value::String(method)) = object.get("method") else {
        return Err(RpcError::new(
            INVALID_REQUEST,
            "`method` is required and must be a string",
        ));
    };
    let id = match object.get("id") {
        None | Some(Value::Null) => None,
        Some(Value::Number(_) | Value::String(_)) => object.get("id").cloned(),
        Some(_) => {
            return Err(RpcError::new(
                INVALID_REQUEST,
                "`id` must be a number or a string",
            ));
        }
    };
    let params = match object.get("params") {
        None => Value::Object(serde_json::Map::new()),
        Some(params @ Value::Object(_)) => params.clone(),
        Some(_) => {
            return Err(RpcError::new(
                INVALID_PARAMS,
                "`params` must be a JSON object; the protocol's fields are named",
            ));
        }
    };
    Ok(Request {
        id,
        method: method.clone(),
        params,
    })
}

/// Whether `method` names a protocol action.
pub fn is_action(method: &str) -> bool {
    rustim_scene::command::ACTIONS.contains(&method)
}

/// The protocol command document a method and its params spell.
///
/// `params` must be an object (the parser guarantees it), so this is the exact
/// inversion of the stdio reader's `{"action": …, …}` shape. It returns text
/// because [`rustim_scene::command::parse_command`] is the one parser: going
/// through it means a daemon client gets the *same* `Unparseable` refusal, under
/// the same property name, as a script line.
pub fn command_text(method: &str, params: &Value) -> String {
    let mut object = params.as_object().cloned().unwrap_or_default();
    object.insert("action".to_owned(), Value::String(method.to_owned()));
    Value::Object(object).to_string()
}

/// A successful response, echoing `id`. `id` is `None` only for a request whose
/// id could not be read, in which case the caller should use [`RpcError`].
pub fn result_message(id: &Value, result: Value) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string()
}

/// An unsolicited event, as a JSON-RPC notification.
///
/// The method is `"event"` and the params are the [`Event`] itself, tagged by
/// `kind` — so the same event value the kernel serialises for the stdio feed
/// crosses both transports with no second schema. There is no `id`: an event
/// answers nothing.
///
/// [`Event`]: rustim_scene::Event
pub fn event_message(event: &rustim_scene::Event) -> String {
    json!({ "jsonrpc": "2.0", "method": "event", "params": event }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_well_formed_request_parses() {
        let request =
            parse(r#"{"jsonrpc":"2.0","id":7,"method":"add","params":{"type":"circle"}}"#)
                .expect("valid");
        assert_eq!(request.id, Some(json!(7)));
        assert_eq!(request.method, "add");
        assert_eq!(request.params, json!({"type":"circle"}));
    }

    #[test]
    fn a_missing_id_is_a_notification_and_a_missing_params_is_empty() {
        let request = parse(r#"{"jsonrpc":"2.0","method":"ping"}"#).expect("valid");
        assert_eq!(request.id, None);
        assert_eq!(request.params, json!({}));
    }

    #[test]
    fn string_ids_survive_verbatim() {
        let request = parse(r#"{"jsonrpc":"2.0","id":"a-1","method":"ping"}"#).expect("valid");
        assert_eq!(request.id, Some(json!("a-1")));
    }

    #[test]
    fn the_three_parse_refusals_say_which_rule_broke() {
        let cases = [
            ("not json", PARSE_ERROR),
            (r#"[1,2,3]"#, INVALID_REQUEST),
            (r#"{"id":1,"method":"ping"}"#, INVALID_REQUEST),
            (
                r#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#,
                INVALID_REQUEST,
            ),
            (r#"{"jsonrpc":"2.0","id":1,"method":3}"#, INVALID_REQUEST),
            (
                r#"{"jsonrpc":"2.0","id":{},"method":"ping"}"#,
                INVALID_REQUEST,
            ),
            (
                r#"{"jsonrpc":"2.0","id":1,"method":"ping","params":[1]}"#,
                INVALID_PARAMS,
            ),
        ];
        for (text, code) in cases {
            let error = parse(text).expect_err(text);
            assert_eq!(error.code, code, "{text}: {error:?}");
        }
    }

    #[test]
    fn an_action_becomes_the_command_document_it_names() {
        let text = command_text(
            "set",
            &json!({"target": 7, "property": {"name":"opacity","value":0.5}}),
        );
        let parsed: Value = serde_json::from_str(&text).expect("json");
        assert_eq!(parsed["action"], "set");
        assert_eq!(parsed["target"], 7);
        assert_eq!(parsed["property"]["name"], "opacity");
    }

    #[test]
    fn the_method_vocabulary_is_the_protocols_plus_five() {
        assert!(is_action("add"));
        assert!(is_action("events"));
        for method in DAEMON_METHODS {
            assert!(!is_action(method), "{method} must not shadow an action");
        }
        assert!(!is_action("attach"));
        assert!(!is_action("nope"));
    }

    #[test]
    fn an_error_names_the_request_it_answers_and_null_when_it_cannot() {
        let error = RpcError::new(METHOD_NOT_FOUND, "no");
        assert_eq!(error.message(Some(&json!(3)))["id"], 3);
        assert!(error.message(None)["id"].is_null());
        assert_eq!(error.message(None)["error"]["code"], METHOD_NOT_FOUND);
    }
}
