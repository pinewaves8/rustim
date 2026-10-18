//! The daemon over real sockets: a real listener, real clients, real frames.
//!
//! Every test here dials a loopback WebSocket and speaks JSON — the same thing
//! an assistant across a process boundary does. Nothing calls the library's
//! internals directly, so what is asserted is the wire, not an in-process
//! shortcut: the framing, the refusal vocabulary, the attach/reconnect rules and
//! the event fan-out.
//!
//! The tests are deliberately narrow about time. A manual session never ticks,
//! so most of them are deterministic. The one auto-clock test waits for an
//! event, which is the only thing in this crate that needs a clock at all; it
//! bounds the wait with a timeout so a regression fails rather than hangs.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use rustim_daemon::{Hub, HubOptions, serve};

type Client = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Start a daemon on an ephemeral loopback port and return its URL.
async fn start(typeset: bool) -> String {
    let hub = Arc::new(
        Hub::new(HubOptions {
            typeset,
            ..HubOptions::default()
        })
        .expect("the defaults name a real scene"),
    );
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let address = listener
        .local_addr()
        .expect("a bound listener has an address");
    tokio::spawn(serve(listener, hub));
    format!("ws://{address}")
}

async fn connect(url: &str) -> Client {
    connect_async(url).await.expect("connect").0
}

async fn send(client: &mut Client, value: Value) {
    client
        .send(Message::text(value.to_string()))
        .await
        .expect("send");
}

/// The next JSON frame, skipping the WebSocket control frames a client never
/// has to act on. Bounded so a server that stops answering fails a test.
async fn next_json(client: &mut Client) -> Value {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), client.next())
            .await
            .expect("a frame within five seconds")
            .expect("the stream is open")
            .expect("a frame, not an error");
        match message {
            Message::Text(text) => {
                return serde_json::from_str(text.as_str()).expect("a JSON frame");
            }
            Message::Close(_) => panic!("the daemon closed the connection"),
            _ => continue,
        }
    }
}

/// Send one request and return the frame that answers *that* id.
///
/// Events interleave, so matching on `id` is not tidiness — it is the property
/// the envelope exists for.
async fn call(client: &mut Client, id: u64, method: &str, params: Value) -> Value {
    send(
        client,
        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
    )
    .await;
    loop {
        let frame = next_json(client).await;
        if frame.get("id") == Some(&json!(id)) {
            return frame;
        }
        assert!(
            frame.get("method").is_some(),
            "a frame that answers nothing and is not an event: {frame}"
        );
    }
}

/// The successful body of a call, or a panic quoting the transport error.
fn result(frame: &Value) -> &Value {
    assert!(
        frame.get("error").is_none(),
        "unexpected transport error: {frame}"
    );
    frame
        .get("result")
        .unwrap_or_else(|| panic!("no result: {frame}"))
}

/// The transport error code of a failed call.
fn code(frame: &Value) -> i64 {
    frame["error"]["code"]
        .as_i64()
        .unwrap_or_else(|| panic!("no transport error: {frame}"))
}

/// The protocol's reply half of a result: exactly one of the six keys.
fn half(result: &Value) -> &'static str {
    ["diff", "tree", "report", "plan", "capabilities", "error"]
        .into_iter()
        .find(|key| result.get(*key).is_some())
        .unwrap_or_else(|| panic!("a reply carries one of six halves: {result}"))
}

async fn attach(client: &mut Client, id: u64, params: Value) -> Value {
    let frame = call(client, id, "attach", params).await;
    result(&frame).clone()
}

#[tokio::test]
async fn a_session_is_created_and_driven_through_the_protocol() {
    let url = start(false).await;
    let mut client = connect(&url).await;

    let attached = attach(&mut client, 1, json!({"scene": "blank", "clock": "manual"})).await;
    assert_eq!(attached["session"], "s1");
    assert_eq!(attached["created"], true);
    assert_eq!(attached["scene"], "blank");
    assert_eq!(attached["clock"], "manual");

    let added = call(
        &mut client,
        2,
        "add",
        json!({"type": "circle", "params": {"radius": 1.0, "label": "圆"}}),
    )
    .await;
    let added = result(&added);
    assert_eq!(half(added), "diff");
    assert_eq!(added["seq"], 1);
    let circle = added["diff"]["created"][0].clone();
    assert!(circle.is_number(), "{added}");

    // The label spelling addresses the same node, and the reply names ids.
    let changed = call(
        &mut client,
        3,
        "set",
        json!({"target": "圆", "property": {"name": "opacity", "value": 0.5}}),
    )
    .await;
    let changed = result(&changed);
    assert_eq!(changed["seq"], 2);
    assert_eq!(changed["diff"]["modified"][0], circle);

    let queried = call(&mut client, 4, "query", json!({})).await;
    let queried = result(&queried);
    assert_eq!(half(queried), "tree");
    assert!(queried["tree"]["root"].is_number());
    assert_eq!(queried["tree"]["nodes"].as_array().expect("nodes").len(), 2);

    let removed = call(&mut client, 5, "remove", json!({"target": circle})).await;
    let removed = result(&removed);
    assert_eq!(removed["diff"]["removed"][0], circle);
    let queried = call(&mut client, 6, "query", json!({})).await;
    assert_eq!(
        result(&queried)["tree"]["nodes"]
            .as_array()
            .expect("nodes")
            .len(),
        1,
        "only the root remains"
    );
}

#[tokio::test]
async fn an_action_needs_an_attachment() {
    let url = start(false).await;
    let mut client = connect(&url).await;
    let frame = call(&mut client, 1, "add", json!({"type": "circle"})).await;
    assert_eq!(code(&frame), -32001);

    // A daemon method that needs one says so too.
    let frame = call(&mut client, 2, "advance", json!({"seconds": 1.0})).await;
    assert_eq!(code(&frame), -32001);
}

#[tokio::test]
async fn a_reconnect_joins_the_live_session_with_its_objects() {
    let url = start(false).await;
    let mut first = connect(&url).await;
    let attached = attach(&mut first, 1, json!({"scene": "blank", "clock": "manual"})).await;
    assert_eq!(attached["session"], "s1");
    call(
        &mut first,
        2,
        "add",
        json!({"type": "square", "params": {"width": 1.0}}),
    )
    .await;
    drop(first);

    let mut second = connect(&url).await;
    let attached = attach(&mut second, 1, json!({"session": "s1"})).await;
    assert_eq!(attached["created"], false);
    assert_eq!(attached["session"], "s1");
    let queried = call(&mut second, 2, "query", json!({})).await;
    assert_eq!(
        result(&queried)["tree"]["nodes"]
            .as_array()
            .expect("nodes")
            .len(),
        2,
        "the reconnect sees the square the dropped socket created"
    );

    // Stating a different scene is refused, not silently ignored.
    let frame = call(
        &mut second,
        3,
        "attach",
        json!({"session": "s1", "scene": "pythagorean"}),
    )
    .await;
    assert_eq!(code(&frame), -32602);

    let frame = call(&mut second, 4, "attach", json!({"session": "s404"})).await;
    assert_eq!(code(&frame), -32002);
}

#[tokio::test]
async fn a_kernel_refusal_stays_in_the_protocol_vocabulary() {
    let url = start(false).await;
    let mut client = connect(&url).await;
    attach(&mut client, 1, json!({"clock": "manual"})).await;
    let frame = call(
        &mut client,
        2,
        "set",
        json!({"target": 999, "property": {"name": "opacity", "value": 0.5}}),
    )
    .await;
    let reply = result(&frame);
    assert_eq!(half(reply), "error");
    // The protocol's own refusal document, unchanged: `error.error` is the kind.
    assert_eq!(reply["error"]["error"], "no_such_node");
    // The id comes back in the engine's own spelling: `from_ffi` normalises the
    // generation bits, so a client that sent `999` is answered with `999 | 1<<32`
    // — the same id, written the way the tree holds it (see CLAUDE.md).
    assert_eq!(reply["error"]["target"], 4294967296u64 + 999);
    assert_eq!(reply["seq"], 1);

    // The connection is healthy; the refusal was one exchange, not a failure.
    let frame = call(&mut client, 3, "ping", json!({})).await;
    assert_eq!(result(&frame)["pong"], true);
}

#[tokio::test]
async fn transport_errors_are_json_rpc_errors_and_the_connection_survives() {
    let url = start(false).await;
    let mut client = connect(&url).await;

    send(&mut client, json!({"this is": "not a request"})).await;
    let frame = next_json(&mut client).await;
    assert_eq!(code(&frame), -32600);
    assert!(frame["id"].is_null());

    client.send(Message::text("{not json")).await.expect("send");
    let frame = next_json(&mut client).await;
    assert_eq!(code(&frame), -32700);

    let frame = call(&mut client, 2, "explode", json!({})).await;
    assert_eq!(code(&frame), -32601);

    client
        .send(Message::binary(b"{}".to_vec()))
        .await
        .expect("send");
    let frame = next_json(&mut client).await;
    assert_eq!(code(&frame), -32600);

    let frame = call(&mut client, 3, "ping", json!({})).await;
    assert_eq!(
        result(&frame)["pong"],
        true,
        "still speaking after all four"
    );
}

#[tokio::test]
async fn a_manual_session_advances_on_request_and_an_auto_one_refuses() {
    let url = start(false).await;
    let mut client = connect(&url).await;
    attach(&mut client, 1, json!({"clock": "manual"})).await;

    let frame = call(&mut client, 2, "advance", json!({"seconds": 0.5})).await;
    let clock = result(&frame)["clock"].as_f64().expect("a clock");
    assert!((clock - 0.5).abs() < 1e-9, "{clock}");

    for bad in [json!({"seconds": -1.0}), json!({"seconds": 1e9}), json!({})] {
        let frame = call(&mut client, 3, "advance", bad.clone()).await;
        assert_eq!(code(&frame), -32602, "{bad}");
    }

    let auto = attach(&mut client, 4, json!({"scene": "blank"})).await;
    assert_eq!(auto["clock"], "auto");
    assert_eq!(auto["session"], "s2");
    let frame = call(&mut client, 5, "advance", json!({"seconds": 1.0})).await;
    assert_eq!(code(&frame), -32003);
}

#[tokio::test]
async fn events_reach_every_client_attached_to_the_session() {
    let url = start(false).await;
    let mut driver = connect(&url).await;
    let attached = attach(&mut driver, 1, json!({"scene": "blank"})).await;
    assert_eq!(attached["clock"], "auto");

    // A second client joins before anything is queued, so it cannot miss the
    // beat. Attaching alone receives events: the subscription is session state.
    let mut display = connect(&url).await;
    attach(&mut display, 1, json!({"session": "s1"})).await;

    let subscribed = call(&mut driver, 2, "events", json!({"enabled": true})).await;
    assert_eq!(half(result(&subscribed)), "diff");
    call(
        &mut driver,
        3,
        "add",
        json!({"type": "circle", "params": {"radius": 1.0, "label": "c"}}),
    )
    .await;
    call(
        &mut driver,
        4,
        "play",
        json!({"animation": {"target": "c", "type": "fade_in", "duration": 0.2}}),
    )
    .await;

    for (name, client) in [("driver", &mut driver), ("display", &mut display)] {
        let started = wait_for_event(client, "beat_started").await;
        assert_eq!(started["params"]["beat"], 1, "{name}");
        assert_eq!(started["params"]["members"], 1, "{name}");
        let done = wait_for_event(client, "animation_done").await;
        assert_eq!(done["params"]["beat"], 1, "{name}");
    }
}

/// Read frames until an event of `kind` arrives.
async fn wait_for_event(client: &mut Client, kind: &str) -> Value {
    loop {
        let frame = next_json(client).await;
        if frame.get("method") == Some(&json!("event")) {
            assert!(
                frame.get("id").is_none(),
                "an event answers nothing: {frame}"
            );
            if frame["params"]["kind"] == kind {
                return frame;
            }
        }
    }
}

#[tokio::test]
async fn close_ends_the_session_for_every_attached_client() {
    let url = start(false).await;
    let mut first = connect(&url).await;
    attach(&mut first, 1, json!({"clock": "manual"})).await;
    let mut second = connect(&url).await;
    attach(&mut second, 1, json!({"session": "s1"})).await;

    let frame = call(&mut first, 2, "close", json!({})).await;
    assert_eq!(result(&frame)["closed"], true);
    assert_eq!(result(&frame)["session"], "s1");

    // The second connection still holds the handle it attached to; `ping` is
    // how it learns the handle is dead (`alive: false`) rather than being told
    // by a push it never asked for. The answer is deterministic from the
    // instant `close` returns — the handle records the close itself rather than
    // inferring it from the session thread having exited — which is why this
    // assertion is not racy against the scheduler.
    let frame = call(&mut second, 2, "ping", json!({})).await;
    assert_eq!(result(&frame)["session"], "s1");
    assert_eq!(result(&frame)["alive"], false, "{frame}");

    let frame = call(&mut second, 3, "add", json!({"type": "circle"})).await;
    assert_eq!(
        code(&frame),
        -32002,
        "the first use of the dead handle says so and releases it"
    );

    let frame = call(&mut second, 4, "attach", json!({"session": "s1"})).await;
    assert_eq!(code(&frame), -32002);

    // Closing what is not there is an answer, not an error: idempotent.
    let frame = call(&mut second, 5, "close", json!({"session": "s1"})).await;
    assert_eq!(result(&frame)["closed"], false);
}

#[tokio::test]
async fn a_notification_is_applied_and_never_answered() {
    let url = start(false).await;
    let mut client = connect(&url).await;
    attach(&mut client, 1, json!({"clock": "manual"})).await;

    // No `id`: an add that must still happen.
    send(
        &mut client,
        json!({"jsonrpc": "2.0", "method": "add", "params": {"type": "circle"}}),
    )
    .await;
    // The very next frame is the ping's, so the notification got no reply.
    let frame = call(&mut client, 2, "ping", json!({})).await;
    assert_eq!(result(&frame)["pong"], true);

    let queried = call(&mut client, 3, "query", json!({})).await;
    assert_eq!(
        result(&queried)["tree"]["nodes"]
            .as_array()
            .expect("nodes")
            .len(),
        2
    );
    // The notification consumed the first protocol reply number.
    assert_eq!(result(&queried)["seq"], 2);
}

#[tokio::test]
async fn two_sessions_do_not_share_a_tree() {
    let url = start(false).await;
    let mut client = connect(&url).await;
    attach(&mut client, 1, json!({"clock": "manual"})).await;
    call(&mut client, 2, "add", json!({"type": "circle"})).await;

    let second = attach(&mut client, 3, json!({"scene": "blank", "clock": "manual"})).await;
    assert_eq!(second["session"], "s2");
    let queried = call(&mut client, 4, "query", json!({})).await;
    assert_eq!(
        result(&queried)["tree"]["nodes"]
            .as_array()
            .expect("nodes")
            .len(),
        1,
        "s2 is empty"
    );

    let back = attach(&mut client, 5, json!({"session": "s1"})).await;
    assert_eq!(back["session"], "s1");
    let queried = call(&mut client, 6, "query", json!({})).await;
    assert_eq!(
        result(&queried)["tree"]["nodes"]
            .as_array()
            .expect("nodes")
            .len(),
        2,
        "s1 kept its circle"
    );
}

#[tokio::test]
async fn hello_reports_exactly_what_the_build_can_typeset() {
    let without = start(false).await;
    let mut client = connect(&without).await;
    attach(&mut client, 1, json!({"clock": "manual"})).await;
    let frame = call(&mut client, 2, "hello", json!({})).await;
    let capabilities = result(&frame);
    assert_eq!(half(capabilities), "capabilities");
    let shapes = capabilities["capabilities"]["shapes"]
        .as_array()
        .expect("shapes");
    assert!(!shapes.iter().any(|s| s == "text"), "{capabilities}");

    let with = start(true).await;
    let mut client = connect(&with).await;
    attach(&mut client, 1, json!({"clock": "manual"})).await;
    let frame = call(&mut client, 2, "hello", json!({})).await;
    let shapes = result(&frame)["capabilities"]["shapes"]
        .as_array()
        .expect("shapes")
        .clone();
    assert!(shapes.iter().any(|s| s == "text"), "{shapes:?}");
    assert!(shapes.iter().any(|s| s == "formula"), "{shapes:?}");
}

#[tokio::test]
async fn a_text_object_typesets_over_the_socket() {
    let url = start(true).await;
    let mut client = connect(&url).await;
    attach(&mut client, 1, json!({"clock": "manual"})).await;
    let frame = call(
        &mut client,
        2,
        "add",
        json!({"type": "text", "params": {"text": "两条直角边是 3 和 4", "label": "说明"}}),
    )
    .await;
    let reply = result(&frame);
    assert_eq!(half(reply), "diff", "{reply}");
    let added = reply["diff"]["created"][0].clone();

    let frame = call(&mut client, 3, "query", json!({"target": "说明"})).await;
    let tree = &result(&frame)["tree"];
    assert_eq!(tree["nodes"][0]["id"], added);
    assert_eq!(tree["nodes"][0]["name"], "说明");
    assert!(tree["nodes"][0]["bounds"].is_object(), "{tree}");
}
