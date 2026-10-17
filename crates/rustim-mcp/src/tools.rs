//! The tool catalogue, and the translation from MCP arguments to protocol
//! commands.
//!
//! The rule is **few and thick**: a real-time model chooses better between a
//! handful of verbs than between nineteen, and every primitive the protocol has
//! is already reachable inside `rustim_batch`'s `commands` list. So this module
//! adds no vocabulary of its own — it names the five shapes a director actually
//! uses, and hands the command documents through untouched.
//!
//! Only *shape* errors are caught here (a missing `commands`, a `target` that is
//! neither an integer nor a string). Whether `{"action":"add","type":"hexagon"}`
//! is legal is the engine's answer, and it must stay there: a second validator
//! would drift from the first, and the model would get two different refusals
//! for the same mistake depending on which transport it used.

use serde_json::{Map, Value, json};

/// The tool names, in the order `tools/list` returns them.
pub const NAMES: [&str; 5] = [
    "rustim_capabilities",
    "rustim_batch",
    "rustim_validate",
    "rustim_query",
    "rustim_advance",
];

/// What one `tools/call` asks the server to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Call {
    /// The protocol's `hello`.
    Capabilities,
    /// Apply these protocol commands in order.
    Batch(Vec<Value>),
    /// Dry-run these protocol commands against a copy.
    Validate(Vec<Value>),
    /// Read the tree, optionally rooted at one node (an id or a label).
    Query(Option<Value>),
    /// Move a manual clock.
    Advance(f64),
}

/// The `tools/list` result document.
pub fn list_result() -> Value {
    json!({
        "tools": [
            capabilities_tool(),
            batch_tool(),
            validate_tool(),
            query_tool(),
            advance_tool(),
        ],
    })
}

/// Turn a tool name and its arguments into a [`Call`].
///
/// `Err` is an *invalid-arguments* message: the caller maps it to a JSON-RPC
/// `-32602`. An engine refusal cannot happen here — nothing is executed.
pub fn translate(name: &str, arguments: &Map<String, Value>) -> Result<Call, String> {
    match name {
        "rustim_capabilities" => Ok(Call::Capabilities),
        "rustim_batch" => Ok(Call::Batch(commands(arguments)?)),
        "rustim_validate" => Ok(Call::Validate(commands(arguments)?)),
        "rustim_query" => Ok(Call::Query(target(arguments)?)),
        "rustim_advance" => Ok(Call::Advance(seconds(arguments)?)),
        other => Err(format!(
            "unknown tool `{other}`; this server offers: {}",
            NAMES.join(", ")
        )),
    }
}

fn capabilities_tool() -> Value {
    json!({
        "name": "rustim_capabilities",
        "title": "rustim capabilities",
        "description": concat!(
            "Report what this rustim build can build and do, as the protocol's `hello` reply: ",
            "protocol version, action verbs, shape names, animation kinds, settable properties, ",
            "rate curves, capability profiles and limits. Call it once before composing commands — ",
            "it is the authoritative vocabulary, so do not guess a shape or animation name."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {},
            "additionalProperties": false
        },
        "annotations": {"readOnlyHint": true, "openWorldHint": false}
    })
}

fn batch_tool() -> Value {
    json!({
        "name": "rustim_batch",
        "title": "Build and animate the scene",
        "description": concat!(
            "Apply an ordered list of rustim protocol v1 commands and return the net change. ",
            "Each command is a JSON object with an `action` (add, set, remove, play, camera, ",
            "narrate, pause, resume, seek, tempo, checkpoint, undo, stop, motion, events). ",
            "Commands see one another, so a later one may address an object by the label an ",
            "earlier one created. Example: [{\"action\":\"add\",\"type\":\"circle\",",
            "\"params\":{\"radius\":1,\"label\":\"圆\"}},{\"action\":\"play\",\"animation\":",
            "{\"target\":\"圆\",\"type\":\"fade_in\",\"duration\":0.6}}]. The result carries the ",
            "diff: `created` lists the ids of new objects (the only way to learn them), plus ",
            "`modified` and `removed`. Not atomic: if member i is refused the reply is an ",
            "`in_batch` error naming i, and members before it have already applied. Returns as ",
            "soon as the commands are applied; a queued animation plays on the session's clock."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "commands": {
                    "type": "array",
                    "description": concat!(
                        "Protocol v1 commands in order, each an object with an `action` and that ",
                        "action's fields. hello, query, batch and validate are not members; use ",
                        "their own tools."
                    ),
                    "items": {"type": "object"}
                }
            },
            "required": ["commands"],
            "additionalProperties": false
        },
        "annotations": {
            "readOnlyHint": false,
            "destructiveHint": true,
            "idempotentHint": false,
            "openWorldHint": false
        }
    })
}

fn validate_tool() -> Value {
    json!({
        "name": "rustim_validate",
        "title": "Dry-run a command list",
        "description": concat!(
            "Run a `commands` list exactly as `rustim_batch` would, against a copy of the scene, ",
            "and change nothing. Use it when a batch is expensive or destructive: same commands, ",
            "same order, same engine, and the same refusal it would meet. A successful result is ",
            "an empty diff."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "commands": {
                    "type": "array",
                    "description": "The same list `rustim_batch` takes.",
                    "items": {"type": "object"}
                }
            },
            "required": ["commands"],
            "additionalProperties": false
        },
        "annotations": {
            "readOnlyHint": true,
            "idempotentHint": true,
            "openWorldHint": false
        }
    })
}

fn query_tool() -> Value {
    json!({
        "name": "rustim_query",
        "title": "Read the semantic tree",
        "description": concat!(
            "Read the scene's semantic tree: each node's id, role, name, description, pixel ",
            "bounds, hidden flag and children, the camera view the bounds were projected with, ",
            "and a formula's MathML. Pass `target` (an object id or the label it was created ",
            "with) to read one subtree instead of the whole scene. This is how to learn ids and ",
            "labels after a session has run."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "target": {
                    "description": "Read only this node's subtree; omit for the whole scene.",
                    "oneOf": [
                        {"type": "integer", "minimum": 0, "description": "the object's id"},
                        {"type": "string", "description": "the label the object was created with"}
                    ]
                }
            },
            "additionalProperties": false
        },
        "annotations": {"readOnlyHint": true, "openWorldHint": false}
    })
}

fn advance_tool() -> Value {
    json!({
        "name": "rustim_advance",
        "title": "Step a manual clock",
        "description": concat!(
            "Advance a manual-clock session by `seconds` of virtual time and return the clock it ",
            "reached. Only a session started with `--clock manual` accepts it; the default auto ",
            "clock advances in real time and answers `not_manual`. Use manual mode when the ",
            "caller must control exactly which frame the scene is on."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "seconds": {
                    "type": "number",
                    "minimum": 0,
                    "maximum": 600,
                    "description": "virtual seconds to advance (at most 600)"
                }
            },
            "required": ["seconds"],
            "additionalProperties": false
        },
        "annotations": {
            "readOnlyHint": false,
            "destructiveHint": false,
            "idempotentHint": false,
            "openWorldHint": false
        }
    })
}

fn commands(arguments: &Map<String, Value>) -> Result<Vec<Value>, String> {
    let Some(value) = arguments.get("commands") else {
        return Err("`commands` is required: an array of protocol v1 command objects".to_owned());
    };
    let Some(items) = value.as_array() else {
        return Err("`commands` must be an array of command objects".to_owned());
    };
    for (index, item) in items.iter().enumerate() {
        if !item.is_object() {
            return Err(format!(
                "`commands[{index}]` must be an object with an `action`"
            ));
        }
    }
    // An empty list is passed through: `batch` refuses it with the engine's own
    // `out_of_range`, which says more than "invalid arguments" does.
    Ok(items.clone())
}

fn target(arguments: &Map<String, Value>) -> Result<Option<Value>, String> {
    match arguments.get("target") {
        None => Ok(None),
        Some(value @ Value::String(_)) => Ok(Some(value.clone())),
        // `Target` is an id or a label, and an id is an *integer*. A float or a
        // negative number is not a target the engine could ever resolve, so it
        // is an argument error here rather than a `no_such_node` from a guess.
        Some(Value::Number(number)) if number.as_u64().is_some() => {
            Ok(Some(Value::Number(number.clone())))
        }
        Some(Value::Number(_)) => {
            Err("`target` must be a non-negative integer id or a string label".to_owned())
        }
        Some(_) => Err("`target` must be an integer id or a string label".to_owned()),
    }
}

fn seconds(arguments: &Map<String, Value>) -> Result<f64, String> {
    let Some(value) = arguments.get("seconds") else {
        return Err("`seconds` is required: a finite number of virtual seconds".to_owned());
    };
    let Some(number) = value.as_f64() else {
        return Err("`seconds` must be a number".to_owned());
    };
    if !number.is_finite() {
        return Err("`seconds` must be finite".to_owned());
    }
    if number < 0.0 {
        return Err("`seconds` must be non-negative".to_owned());
    }
    if number > rustim_daemon::MAX_ADVANCE_SECONDS {
        return Err(format!(
            "`seconds` must be at most {}",
            rustim_daemon::MAX_ADVANCE_SECONDS
        ));
    }
    Ok(number)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(value: Value) -> Map<String, Value> {
        value.as_object().cloned().expect("an object")
    }

    #[test]
    fn the_catalogue_names_five_tools_with_object_schemas() {
        let listed = list_result();
        let tools = listed["tools"].as_array().expect("tools");
        let names: Vec<&str> = tools
            .iter()
            .map(|tool| tool["name"].as_str().expect("a name"))
            .collect();
        assert_eq!(names, NAMES);
        for tool in tools {
            assert_eq!(tool["inputSchema"]["type"], "object", "{tool}");
            assert!(
                tool["description"]
                    .as_str()
                    .is_some_and(|text| text.len() > 40),
                "{tool}"
            );
        }
    }

    #[test]
    fn a_batch_is_translated_with_its_commands_untouched() {
        let commands = vec![
            json!({"action":"add","type":"circle","params":{"label":"圆"}}),
            json!({"action":"play","animation":{"target":"圆","type":"fade_in"}}),
        ];
        let call =
            translate("rustim_batch", &arguments(json!({"commands": commands}))).expect("valid");
        assert_eq!(call, Call::Batch(commands));
    }

    #[test]
    fn malformed_arguments_say_which_argument_is_wrong() {
        assert_eq!(
            translate("rustim_batch", &Map::new()),
            Err("`commands` is required: an array of protocol v1 command objects".to_owned())
        );
        assert_eq!(
            translate("rustim_batch", &arguments(json!({"commands": "add"}))),
            Err("`commands` must be an array of command objects".to_owned())
        );
        assert_eq!(
            translate("rustim_batch", &arguments(json!({"commands": [1]}))),
            Err("`commands[0]` must be an object with an `action`".to_owned())
        );
        assert_eq!(
            translate("rustim_query", &arguments(json!({"target": 1.5}))),
            Err("`target` must be a non-negative integer id or a string label".to_owned())
        );
        assert_eq!(
            translate("rustim_query", &arguments(json!({"target": true}))),
            Err("`target` must be an integer id or a string label".to_owned())
        );
        assert_eq!(
            translate("rustim_advance", &arguments(json!({"seconds": -1.0}))),
            Err("`seconds` must be non-negative".to_owned())
        );
        assert_eq!(
            translate("rustim_advance", &arguments(json!({"seconds": 601.0}))),
            Err(format!(
                "`seconds` must be at most {}",
                rustim_daemon::MAX_ADVANCE_SECONDS
            ))
        );
    }

    #[test]
    fn a_query_target_keeps_the_spelling_it_arrived_in() {
        for target in [json!(42), json!("圆")] {
            let call = translate(
                "rustim_query",
                &arguments(json!({"target": target.clone()})),
            )
            .expect("valid");
            assert_eq!(call, Call::Query(Some(target)));
        }
        assert_eq!(
            translate("rustim_query", &Map::new()),
            Ok(Call::Query(None))
        );
    }

    #[test]
    fn an_unknown_tool_lists_the_known_ones() {
        let error = translate("rustim_typo", &Map::new()).unwrap_err();
        for name in NAMES {
            assert!(error.contains(name), "{error}");
        }
    }
}
