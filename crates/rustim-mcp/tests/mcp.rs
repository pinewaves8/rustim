//! The MCP adapter over a real stdio pipe.
//!
//! Every test launches the binary, writes JSON-RPC lines into its stdin and
//! reads its stdout — the same thing an MCP client does. Nothing calls the
//! library directly, so what is asserted is the transport: the newline framing,
//! the handshake, the tool catalogue, and which failures are protocol errors
//! versus tool results. The difference matters to a model, and only the wire
//! shows it.
//!
//! The sessions run on a manual clock wherever a test does not need time to
//! pass, so the frames are deterministic. The reader runs on its own thread
//! with a bounded wait, so a server that stops answering fails a test rather
//! than hanging it.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// A launched server and the two pipes a client owns.
struct Client {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    next_id: u64,
}

impl Client {
    fn start(args: &[&str]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_rustim-mcp"))
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn rustim-mcp");
        let stdin = child.stdin.take().expect("a piped stdin");
        let stdout = child.stdout.take().expect("a piped stdout");
        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            // The sender is dropped when stdout closes, which turns a server
            // that exited into a disconnected channel instead of a hang.
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { return };
                if sender.send(line).is_err() {
                    return;
                }
            }
        });
        Self {
            child,
            stdin: Some(stdin),
            lines,
            next_id: 0,
        }
    }

    fn send(&mut self, value: &Value) {
        let stdin = self.stdin.as_mut().expect("stdin is open");
        writeln!(stdin, "{value}").expect("write a line");
        stdin.flush().expect("flush the line");
    }

    fn send_raw(&mut self, text: &str) {
        let stdin = self.stdin.as_mut().expect("stdin is open");
        writeln!(stdin, "{text}").expect("write a line");
        stdin.flush().expect("flush the line");
    }

    fn next_json(&self) -> Value {
        let line = self
            .lines
            .recv_timeout(Duration::from_secs(20))
            .expect("a reply within twenty seconds");
        serde_json::from_str(&line).expect("a JSON reply")
    }

    /// One request, with the id invented here, answered by id.
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let frame = self.next_json();
        assert_eq!(frame["id"], json!(id), "{frame}");
        frame
    }

    fn initialize(&mut self) -> Value {
        self.request(
            "initialize",
            json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "0"},
            }),
        )["result"]
            .clone()
    }

    fn call(&mut self, name: &str, arguments: Value) -> Value {
        self.request("tools/call", json!({"name": name, "arguments": arguments}))["result"].clone()
    }

    /// A tool call that must have succeeded, returning its structured document.
    fn structured(&mut self, name: &str, arguments: Value) -> Value {
        let result = self.call(name, arguments);
        assert_eq!(result["isError"], false, "{result}");
        result["structuredContent"].clone()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // EOF is how an MCP client stops a server; give it a moment to exit,
        // then make it.
        drop(self.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Ok(None) | Err(_) => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    return;
                }
            }
        }
    }
}

fn started(args: &[&str]) -> Client {
    let mut client = Client::start(args);
    client.initialize();
    client
}

#[test]
fn initialize_negotiates_and_the_catalogue_names_five_tools() {
    let mut client = Client::start(&["--clock", "manual"]);
    let result = client.initialize();
    assert_eq!(result["protocolVersion"], "2025-11-25");
    assert_eq!(result["serverInfo"]["name"], "rustim-mcp");
    assert_eq!(result["capabilities"]["tools"], json!({}));
    assert!(
        result["instructions"]
            .as_str()
            .is_some_and(|text| text.contains("rustim_batch"))
    );

    // A revision we do not implement is answered with the newest one, as the
    // protocol requires.
    let result =
        client.request("initialize", json!({"protocolVersion": "1999-01-01"}))["result"].clone();
    assert_eq!(result["protocolVersion"], "2025-11-25");

    let listed = client.request("tools/list", json!({}))["result"].clone();
    let names: Vec<&str> = listed["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .map(|tool| tool["name"].as_str().expect("a name"))
        .collect();
    assert_eq!(
        names,
        [
            "rustim_capabilities",
            "rustim_batch",
            "rustim_validate",
            "rustim_query",
            "rustim_advance"
        ]
    );
    for tool in listed["tools"].as_array().expect("tools") {
        assert_eq!(tool["inputSchema"]["type"], "object", "{tool}");
        assert!(tool["annotations"].is_object(), "{tool}");
    }
}

#[test]
fn capabilities_are_the_engines_own_hello_reply() {
    let mut client = started(&["--clock", "manual"]);
    let reply = client.structured("rustim_capabilities", json!({}));
    assert_eq!(reply["capabilities"]["protocol"], 1);
    let shapes = reply["capabilities"]["shapes"]
        .as_array()
        .expect("shapes")
        .iter()
        .map(|shape| shape.as_str().expect("a name"))
        .collect::<Vec<_>>();
    for expected in ["circle", "square", "text", "formula", "axes", "plot"] {
        assert!(shapes.contains(&expected), "missing {expected}: {shapes:?}");
    }
    let actions = reply["capabilities"]["actions"]
        .as_array()
        .expect("actions")
        .iter()
        .map(|action| action.as_str().expect("a name"))
        .collect::<Vec<_>>();
    for expected in [
        "batch", "validate", "query", "pause", "resume", "seek", "tempo", "events",
    ] {
        assert!(actions.contains(&expected), "missing {expected}");
    }
}

#[test]
fn a_batch_builds_a_scene_and_a_query_reads_it_back() {
    let mut client = started(&["--clock", "manual"]);
    let reply = client.structured(
        "rustim_batch",
        json!({"commands": [
            {"action": "add", "type": "circle", "params": {"radius": 1.0, "label": "圆"}},
            {"action": "play", "animation": {"target": "圆", "type": "fade_in", "duration": 0.5}},
        ]}),
    );
    let created = reply["diff"]["created"].as_array().expect("created");
    assert_eq!(created.len(), 1);
    let id = created[0].clone();
    // `play` queues a beat; the diff still describes the tree, and nothing else
    // was created or removed.
    assert_eq!(reply["diff"]["removed"], json!([]));

    let reply = client.structured("rustim_query", json!({"target": "圆"}));
    let tree = &reply["tree"];
    assert_eq!(tree["root"], id);
    let nodes = tree["nodes"].as_array().expect("nodes");
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0]["name"], "圆");
    assert!(nodes[0]["bounds"].is_object());

    // The text half is the same document, not a second rendering of it.
    let result = client.call("rustim_query", json!({"target": "圆"}));
    let text = result["content"][0]["text"].as_str().expect("text");
    assert_eq!(
        serde_json::from_str::<Value>(text).expect("JSON"),
        result["structuredContent"]
    );
}

#[test]
fn an_engine_refusal_is_a_tool_result_and_the_process_keeps_going() {
    let mut client = started(&["--clock", "manual"]);
    let result = client.call(
        "rustim_batch",
        json!({"commands": [
            {"action": "set", "target": "nope", "property": {"name": "opacity", "value": 0.5}},
        ]}),
    );
    assert_eq!(result["isError"], true, "{result}");
    let refusal = &result["structuredContent"];
    assert_eq!(refusal["error"]["error"], "in_batch");
    assert_eq!(refusal["error"]["index"], 0);
    assert_eq!(refusal["error"]["cause"]["error"], "no_such_label");

    // A refusal is not a broken transport: the next request is answered.
    let reply = client.request("ping", json!({}));
    assert_eq!(reply["result"], json!({}));
}

#[test]
fn validate_changes_nothing_and_the_same_batch_then_applies() {
    let mut client = started(&["--clock", "manual"]);
    let commands = json!([
        {"action": "add", "type": "square", "params": {"width": 1.0, "label": "方"}},
    ]);
    let reply = client.structured("rustim_validate", json!({"commands": commands}));
    assert_eq!(
        reply["diff"],
        json!({"created": [], "modified": [], "removed": []})
    );

    let tree = client.structured("rustim_query", json!({}));
    assert!(
        !tree["tree"]["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .any(|node| node["name"] == "方"),
        "validate must not create anything"
    );

    client.structured("rustim_batch", json!({"commands": commands}));
    let tree = client.structured("rustim_query", json!({}));
    assert!(
        tree["tree"]["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .any(|node| node["name"] == "方")
    );
}

#[test]
fn advance_moves_a_manual_clock_and_refuses_an_auto_one() {
    let mut manual = started(&["--clock", "manual"]);
    let reply = manual.structured("rustim_advance", json!({"seconds": 0.5}));
    assert_eq!(reply["advanced"], true);
    let clock = reply["clock"].as_f64().expect("a clock");
    assert!((clock - 0.5).abs() < 1e-9, "{clock}");

    let mut auto = started(&[]);
    let result = auto.call("rustim_advance", json!({"seconds": 0.5}));
    assert_eq!(result["isError"], true, "{result}");
    assert_eq!(result["structuredContent"]["advanced"], false);
    assert_eq!(result["structuredContent"]["error"], "not_manual");
    assert!(
        result["structuredContent"]["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("--clock manual"))
    );
}

#[test]
fn malformed_arguments_are_protocol_errors() {
    let mut client = started(&["--clock", "manual"]);
    let frame = client.request(
        "tools/call",
        json!({"name": "rustim_nope", "arguments": {}}),
    );
    assert_eq!(frame["error"]["code"], -32602, "{frame}");

    let frame = client.request(
        "tools/call",
        json!({"name": "rustim_batch", "arguments": {"commands": "add"}}),
    );
    assert_eq!(frame["error"]["code"], -32602);

    let frame = client.request(
        "tools/call",
        json!({"name": "rustim_advance", "arguments": {"seconds": -1.0}}),
    );
    assert_eq!(frame["error"]["code"], -32602);

    let frame = client.request(
        "tools/call",
        json!({"name": "rustim_query", "arguments": {"target": 1.5}}),
    );
    assert_eq!(frame["error"]["code"], -32602);

    // None of that reached the session.
    let tree = client.structured("rustim_query", json!({}));
    assert_eq!(tree["tree"]["nodes"].as_array().expect("nodes").len(), 1);
}

#[test]
fn notifications_are_never_answered() {
    let mut client = started(&["--clock", "manual"]);
    client.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    // If the server answered the notification, this would receive that reply
    // instead of the ping's.
    let reply = client.request("ping", json!({}));
    assert_eq!(reply["result"], json!({}));

    // Blank lines are transport noise, not messages: the next line out is the
    // ping's, not a reply to the blank.
    client.send_raw("");
    client.send_raw("");
    client
        .send_raw(&json!({"jsonrpc": "2.0", "id": 99, "method": "ping", "params": {}}).to_string());
    let frame = client.next_json();
    assert_eq!(frame["id"], json!(99));
}

#[test]
fn a_formula_and_cjk_text_typeset_over_the_pipe() {
    let mut client = started(&["--clock", "manual", "--scene", "blank"]);
    let reply = client.structured(
        "rustim_batch",
        json!({"commands": [
            {"action": "add", "type": "text",
             "params": {"text": "两条直角边是 3 和 4", "label": "说明"}},
            {"action": "add", "type": "formula",
             "params": {"text": "x^2 + frac(1, 2)", "label": "公式"}},
        ]}),
    );
    // A formula is one addressable root plus one mobject per glyph — every
    // symbol is a real object with a `NodeId`, which is the whole point of the
    // Typst route. So `created` holds the text root, the formula root and the
    // formula's glyph children, not two entries.
    let created = reply["diff"]["created"].as_array().expect("created");
    assert!(created.len() >= 2, "{reply}");

    let tree = client.structured("rustim_query", json!({}));
    let nodes = tree["tree"]["nodes"].as_array().expect("nodes");
    let text = nodes
        .iter()
        .find(|node| node["name"] == "说明")
        .expect("the text node");
    assert!(text["bounds"].is_object(), "a shaped text node has bounds");
    let formula = nodes
        .iter()
        .find(|node| node["name"] == "公式")
        .expect("the formula node");
    assert_eq!(formula["role"], "math");
    assert!(
        formula["mathml"]
            .as_str()
            .is_some_and(|mathml| mathml.starts_with("<math")),
        "{formula}"
    );
}

#[test]
fn no_typeset_reports_the_honest_profile_and_refuses_text() {
    let mut client = started(&["--clock", "manual", "--no-typeset"]);
    let reply = client.structured("rustim_capabilities", json!({}));
    let shapes = reply["capabilities"]["shapes"]
        .as_array()
        .expect("shapes")
        .iter()
        .map(|shape| shape.as_str().expect("a name"))
        .collect::<Vec<_>>();
    for absent in ["text", "formula"] {
        assert!(!shapes.contains(&absent), "{absent} promised: {shapes:?}");
    }

    let result = client.call(
        "rustim_batch",
        json!({"commands": [
            {"action": "add", "type": "text", "params": {"text": "hi", "label": "t"}},
        ]}),
    );
    assert_eq!(result["isError"], true, "{result}");
    assert_eq!(
        result["structuredContent"]["error"]["cause"]["error"],
        "unsupported"
    );
    assert_eq!(
        result["structuredContent"]["error"]["cause"]["feature"],
        "text"
    );
}

#[test]
fn the_scene_flag_is_honoured() {
    let mut client = started(&["--clock", "manual", "--scene", "pythagorean"]);
    let tree = client.structured("rustim_query", json!({}));
    let named = tree["tree"]["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .filter(|node| {
            ["直角三角形", "a 的正方形", "b 的正方形", "c 的正方形"]
                .iter()
                .any(|name| node["name"] == *name)
        })
        .count();
    assert_eq!(named, 4, "{tree}");
}
