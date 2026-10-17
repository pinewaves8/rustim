//! The conformance suite: the protocol, exercised by a client that is not the
//! host.
//!
//! This file is the executable definition of "any AI can drive rustim". It
//! spawns the real binary (`CARGO_BIN_EXE_rustim-app`) as a child process and
//! speaks to it only through [`rustim_client::StdioClient`] — the same crate an
//! adapter outside this workspace would depend on. Nothing here reaches into
//! `rustim-app`'s internals, so a test that passes cannot be passing because it
//! shares a type or a shortcut with the host.
//!
//! What it pins, in the order `驱动协议v1.md` §7 lists them:
//!
//! 1. `hello` answers with protocol 1 and a non-empty vocabulary of the sizes
//!    the build actually has (11 shapes, 11 animations, 10 properties, 45 rates);
//! 2. every advertised shape and animation is accepted by the binary, and every
//!    advertised rate is accepted on a `play` — the capability list is measured
//!    against the parser rather than trusted;
//! 3. `add` → `set` → `query` agrees on ids, labels and properties;
//! 4. `play` answers with a present-but-empty diff and `stop` is idempotent;
//! 5. a bad command, a bad version and bad JSON are readable refusals, and the
//!    feed continues afterwards (exit code still 0);
//! 6. **two independent clients replaying one log produce the same semantic
//!    tree** — the switch-clients property, in the transport v1 has.
//!
//! Each test owns a distinct PNG path: two tests must never render to the same
//! artifact path (the golden suite learned this the hard way — see `CLAUDE.md`),
//! and cargo runs these in parallel threads.
//!
//! # Why the exit status is part of every test
//!
//! A host cannot use a session whose *process* failed. `--reply` keeps per-line
//! refusals out of the exit code on purpose (they are values, not failures), so
//! a non-zero status here means something the protocol did not model — a panic,
//! or a bad command line in the test itself. `close()` is therefore asserted
//! successful in every test, not only where it is the point.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rustim_client::{
    Capabilities, Command, CommandReply, PROTOCOL_VERSION, StdioClient, parse_command,
};

/// The binary under test, built by cargo for this test run.
const BIN: &str = env!("CARGO_BIN_EXE_rustim-app");

/// Distinguishes artifact paths between tests in this file and between test
/// binaries running in the same directory.
static NEXT: AtomicU64 = AtomicU64::new(0);

/// Where this suite's render targets go. Under `target/`, never committed.
fn artifacts_dir() -> PathBuf {
    let target = std::env::var_os("CARGO_TARGET_DIR").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target"),
        PathBuf::from,
    );
    let dir = target.join("conformance-artifacts");
    std::fs::create_dir_all(&dir).expect("create artifacts dir");
    dir
}

/// A render path no other test can pick.
fn png_path(tag: &str) -> PathBuf {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    artifacts_dir().join(format!(
        "{}-{}-{n}-{tag}.png",
        std::env::consts::OS,
        std::process::id()
    ))
}

/// A client on a headless run of the real binary, with the protocol on its
/// stdio and an empty scene.
fn client(tag: &str) -> (StdioClient, PathBuf) {
    let png = png_path(tag);
    let client = StdioClient::spawn(
        Path::new(BIN),
        &[
            "--script",
            "-",
            "--reply",
            "--png",
            png.to_str().expect("utf-8 path"),
            // The baseline discipline: the still must not depend on the
            // machine's desktop motion preference.
            "--motion",
            "full",
        ],
    )
    .unwrap_or_else(|e| panic!("spawning {BIN}: {e}"));
    (client, png)
}

/// Close the session and assert the process ended cleanly.
fn finish(client: StdioClient, png: &Path) {
    let status = client.close().expect("waiting for the engine");
    assert!(status.success(), "the engine exited with {status}");
    assert!(
        png.exists() && std::fs::metadata(png).unwrap().len() > 0,
        "the run did not write {}",
        png.display()
    );
}

/// Send one JSON line and assert it was accepted (a diff and no error).
fn expect_diff(client: &mut StdioClient, line: &str) -> rustim_scene::command::StateDiff {
    let reply = client
        .send_raw(line)
        .unwrap_or_else(|e| panic!("{line}: {e}"));
    assert!(
        reply.error.is_none(),
        "{line} was refused: {:?}",
        reply.error
    );
    let Some(diff) = reply.diff else {
        panic!(
            "{line} was not answered with a diff (reply seq {}, tree {}, capabilities {})",
            reply.seq,
            reply.tree.is_some(),
            reply.capabilities.is_some()
        );
    };
    diff
}

/// The capabilities a fresh build reports, for the tests that only need them as
/// a starting point.
fn capabilities() -> Capabilities {
    let (mut client, png) = client("caps");
    let caps = client.hello().expect("the handshake");
    finish(client, &png);
    caps
}

#[test]
fn hello_answers_with_the_builds_vocabulary() {
    let (mut client, png) = client("hello");
    let caps = client.hello().expect("the handshake");

    assert_eq!(caps.protocol, PROTOCOL_VERSION);
    assert_eq!(caps.protocol, 1, "protocol v1 is the one this suite pins");
    assert!(
        caps.engine.starts_with("rustim/"),
        "the engine string is opaque but not empty: {:?}",
        caps.engine
    );
    // The sizes are the build's, not a documentation count: a shape or
    // animation added without joining `ALL` fails here before it can ship an
    // incomplete capability list.
    assert_eq!(caps.shapes.len(), 11, "{:?}", caps.shapes);
    assert_eq!(caps.animations.len(), 11, "{:?}", caps.animations);
    assert_eq!(caps.properties.len(), 10, "{:?}", caps.properties);
    assert_eq!(caps.rates.len(), 45, "{:?}", caps.rates);
    assert_eq!(caps.profiles, ["primitive".to_owned()]);
    assert_eq!(caps.limits.max_ticks, 256);

    finish(client, &png);
}

/// The vocabulary is not a description of the parser — it **is** the parser's
/// vocabulary, and the way to know that is to feed every name back in.
#[test]
fn every_advertised_word_is_accepted_by_the_binary() {
    let (mut client, png) = client("vocab");
    let caps = client.hello().expect("the handshake");

    for name in &caps.shapes {
        // The two typeset kinds need their source string: an empty text or
        // formula is refused under `text` (the payload a model most easily
        // sends by accident), so a bare probe would be testing that refusal
        // rather than the vocabulary. Every other kind is self-describing.
        let line = match name.as_str() {
            "text" => r#"{"action":"add","type":"text","params":{"text":"你好"}}"#.to_owned(),
            "formula" => r#"{"action":"add","type":"formula","params":{"text":"x^2"}}"#.to_owned(),
            _ => format!(r#"{{"action":"add","type":"{name}"}}"#),
        };
        let diff = expect_diff(&mut client, &line);
        assert_eq!(diff.created.len(), 1, "{line}");
    }

    // A second node for the morphs, so `transform` has a donor that is not the
    // subject (a node into itself is refused, as it should be).
    let a = expect_diff(
        &mut client,
        r#"{"action":"add","type":"square","params":{"width":1.0}}"#,
    )
    .created[0]
        .raw();
    let b = expect_diff(
        &mut client,
        r#"{"action":"add","type":"square","params":{"width":1.0}}"#,
    )
    .created[0]
        .raw();

    for name in &caps.animations {
        // The capability list says the *kind* is accepted; each kind's required
        // fields are its own, and a probe that omitted one would be testing the
        // refusal path, not the vocabulary. `group`/`succession` name members
        // rather than a target (a target on a group is a refusal), and each
        // parameterised kind has one field without which it is deliberately
        // refused.
        let animation = match name.as_str() {
            "group" | "succession" => {
                format!(r#"{{"type":"{name}","animations":[{{"target":{a},"type":"fade_in"}}]}}"#)
            }
            "move_to" => format!(r#"{{"target":{a},"type":"move_to","to":[1.0,-1.0]}}"#),
            "rotate" => format!(r#"{{"target":{a},"type":"rotate","angle":0.5}}"#),
            "scale" => format!(r#"{{"target":{a},"type":"scale","factor":1.5}}"#),
            "transform" => format!(r#"{{"target":{a},"type":"transform","into":{b}}}"#),
            "animate" => format!(
                r#"{{"target":{a},"type":"animate","property":{{"name":"opacity","value":0.5}}}}"#
            ),
            _ => format!(r#"{{"target":{a},"type":"{name}"}}"#),
        };
        let line = format!(r#"{{"action":"play","animation":{animation}}}"#);
        let diff = expect_diff(&mut client, &line);
        assert!(diff.is_empty(), "play answers with an empty diff: {line}");
        // Drop the beat before the next one, so the script is a sequence of
        // independent probes rather than a timeline.
        expect_diff(&mut client, r#"{"action":"stop"}"#);
    }

    for name in &caps.rates {
        let line = format!(
            r#"{{"action":"play","animation":{{"target":{a},"type":"fade_in","rate":"{name}"}}}}"#
        );
        expect_diff(&mut client, &line);
        expect_diff(&mut client, r#"{"action":"stop"}"#);
    }

    finish(client, &png);
}

/// The three mutating commands agree with each other, and a label is a legal
/// address for all of them.
#[test]
fn add_set_and_query_agree_on_ids_and_labels() {
    let (mut client, png) = client("mutate");

    let created = expect_diff(
        &mut client,
        r#"{"action":"add","type":"circle","params":{"radius":1.5,"label":"圆","color":[100,181,246]}}"#,
    )
    .created;
    assert_eq!(created.len(), 1);
    let id = created[0];

    // Set by label, and the diff names the id — the unambiguous handle a host
    // stores for later commands.
    let modified = expect_diff(
        &mut client,
        r#"{"action":"set","target":"圆","property":{"name":"opacity","value":0.5}}"#,
    )
    .modified;
    assert_eq!(modified, [id]);

    // A query by label returns the same node, and the document's root moves to
    // it: the subtree spelling, not an empty tree.
    let reply = client
        .send_raw(r#"{"action":"query","target":"圆"}"#)
        .expect("query by label");
    let tree = reply.tree.expect("a query is answered with a tree");
    assert_eq!(tree.root, id);
    assert_eq!(tree.nodes.len(), 1);
    assert_eq!(tree.nodes[0].name, "圆");
    assert!(tree.nodes[0].bounds.is_some(), "a circle has bounds");

    // The whole-tree query reaches the same node, and the camera rides along.
    let reply = client
        .send_raw(r#"{"action":"query"}"#)
        .expect("whole-tree query");
    let tree = reply.tree.expect("a query is answered with a tree");
    assert_eq!(tree.get(id).map(|n| n.name.as_str()), Some("圆"));
    assert_eq!(tree.camera.zoom, 1.0);

    // Rename, then the old address is a *label* refusal, not a missing node.
    let modified = expect_diff(
        &mut client,
        r#"{"action":"set","target":"圆","property":{"name":"label","value":"圆圈"}}"#,
    )
    .modified;
    assert_eq!(modified, [id]);
    let reply = client
        .send_raw(r#"{"action":"query","target":"圆"}"#)
        .expect("a refusal is still a reply");
    let error = reply.error.expect("the old label no longer resolves");
    assert!(format!("{error}").contains("圆"), "{error}");
    let reply = client
        .send_raw(r#"{"action":"query","target":"圆圈"}"#)
        .expect("the new label resolves");
    assert_eq!(reply.tree.expect("a tree").root, id);

    finish(client, &png);
}

/// A beat is queued, not applied — and stopping twice is not an error.
#[test]
fn play_is_an_empty_diff_and_stop_is_idempotent() {
    let (mut client, png) = client("playstop");

    let id = expect_diff(
        &mut client,
        r#"{"action":"add","type":"square","params":{"width":2.0}}"#,
    )
    .created[0]
        .raw();

    let played = expect_diff(
        &mut client,
        &format!(
            r#"{{"action":"play","animation":{{"target":{id},"type":"fade_in","duration":1.0}}}}"#
        ),
    );
    assert!(
        played.is_empty(),
        "queueing a beat does not change the tree"
    );

    // `stop` interrupts; it does not finish anything, so the square is still
    // there. The empty diff is the same present-and-empty shape `play` uses.
    for _ in 0..2 {
        let stopped = expect_diff(&mut client, r#"{"action":"stop"}"#);
        assert!(stopped.is_empty());
    }

    // The interrupt left the object in place rather than running a `finish`
    // that might have removed it.
    let reply = client.send_raw(r#"{"action":"query"}"#).expect("query");
    let tree = reply.tree.expect("a tree");
    assert_eq!(tree.nodes.len(), 2, "root + the square");

    finish(client, &png);
}

/// A refusal is a value: it names what was wrong, it does not stop the feed, and
/// it does not fail the run.
#[test]
fn refusals_are_readable_and_the_feed_continues() {
    let (mut client, png) = client("refuse");

    let reply = client
        .send_raw(
            r#"{"action":"set","target":"没有这个","property":{"name":"opacity","value":0.5}}"#,
        )
        .expect("a refusal is a reply");
    let error = reply.error.expect("an unknown label is refused");
    assert!(
        matches!(error, rustim_client::CommandError::NoSuchLabel { .. }),
        "{error:?}"
    );

    let reply = client
        .send_raw(r#"{"action":"add","type":"hexagon"}"#)
        .expect("a parse refusal is a reply");
    assert!(
        matches!(
            reply.error,
            Some(rustim_client::CommandError::Unparseable { .. })
        ),
        "{:?}",
        reply.error
    );

    let reply = client
        .send_raw(r#"{"action":"hello","protocol":2}"#)
        .expect("a version refusal is a reply");
    let error = reply.error.expect("a foreign version is refused");
    let text = format!("{error}");
    assert!(text.contains("protocol"), "{text}");
    assert!(text.contains("2") && text.contains("1"), "{text}");

    // The feed continues: a good command after three refusals still runs.
    let diff = expect_diff(
        &mut client,
        r#"{"action":"add","type":"circle","params":{"radius":1.0}}"#,
    );
    assert_eq!(diff.created.len(), 1);

    // And the process still ends cleanly — refusals are not run failures.
    finish(client, &png);
}

/// Run one session log through a fresh process, returning the replies and the
/// tree the session ended on.
fn replay(tag: &str) -> (Vec<CommandReply>, rustim_scene::a11y::SemanticTree) {
    let (mut client, png) = client(tag);
    let mut replies = Vec::new();
    for line in LOG {
        replies.push(
            client
                .send_raw(line)
                .unwrap_or_else(|e| panic!("{line}: {e}")),
        );
    }
    let tree = client
        .send_raw(r#"{"action":"query"}"#)
        .expect("query")
        .tree
        .expect("a tree");
    finish(client, &png);
    (replies, tree)
}

/// A session that uses every part of the state the protocol can carry: creation,
/// styling, a label rename, a camera write, a queued beat, an interrupt and a
/// narration. Written as JSON lines because that is what a real log is.
const LOG: &[&str] = &[
    r#"{"action":"hello","protocol":1}"#,
    r#"{"action":"add","type":"right_triangle","params":{"width":3.0,"height":4.0,"label":"三角形","color":[255,255,255]}}"#,
    r#"{"action":"add","type":"square","params":{"width":2.0,"label":"方块","color":[100,181,246]}}"#,
    r#"{"action":"set","target":"方块","property":{"name":"opacity","value":0.5}}"#,
    r#"{"action":"set","target":"方块","property":{"name":"rotation","value":0.25}}"#,
    r#"{"action":"set","target":"方块","property":{"name":"label","value":"蓝方块"}}"#,
    r#"{"action":"camera","center":[0.5,-0.25],"zoom":1.5}"#,
    r#"{"action":"play","animation":{"target":"三角形","type":"fade_in","duration":1.0,"rate":"ease_in_out_sine"}}"#,
    r#"{"action":"stop"}"#,
    r#"{"action":"narrate","text":"两条直角边是 3 和 4","at":0.0}"#,
    r#"{"action":"query","target":"三角形"}"#,
];

/// **The switch-a-driver acceptance, in the transport v1 has.** A command log is
/// the session; two independent processes fed the same log must be
/// indistinguishable — same review of every reply, same semantic tree. If this
/// holds, a host can hand a session to a different AI by replaying its log,
/// without the engine knowing either of them exists.
#[test]
fn two_independent_clients_replay_the_same_log_to_the_same_tree() {
    let (first, tree_a) = replay("replay-a");
    let (second, tree_b) = replay("replay-b");

    // Same number of replies, same presence per reply, same errors. The whole
    // reply values are compared, which includes the ids the engine handed out —
    // a fresh process must issue the same ids for the same log, or a log is not
    // a replayable session.
    assert_eq!(first, second, "the two sessions replied differently");
    assert_eq!(tree_a, tree_b, "the two sessions ended on different trees");

    // The log did what it says: the triangle exists, the square was renamed and
    // its opacity recorded through the query's bounds/serialisation.
    let triangle = tree_a
        .nodes
        .iter()
        .find(|n| n.name == "三角形")
        .expect("the triangle");
    assert!(triangle.bounds.is_some());
    assert!(
        tree_a.nodes.iter().any(|n| n.name == "蓝方块"),
        "the rename reached the tree"
    );
    assert!(
        !tree_a.nodes.iter().any(|n| n.name == "方块"),
        "the old label is gone"
    );
}

/// A blank command line is a *skip* with no reply, and the sequence number
/// still counts it — the one framing rule a client can get wrong without seeing
/// an error. Exercised through the real binary, because that is where the rule
/// lives.
#[test]
fn a_blank_line_is_skipped_and_counted() {
    let (mut client, png) = client("blank");

    // Line 1: a skip. Line 2: a command, whose reply must say seq 2.
    client.write_raw("").expect("a blank line");
    let reply = client
        .send_raw(r#"{"action":"add","type":"square"}"#)
        .expect("the command after a blank line");
    assert_eq!(reply.seq, 2);

    // The parser agrees about what a blank line is, without a process involved.
    assert!(parse_command("   ").is_none());
    assert!(matches!(
        parse_command("{}"),
        Some(Err(rustim_client::CommandError::Unparseable { .. }))
    ));

    // A typed command and a raw line are the same wire.
    let reply = client
        .send(&Command::Add {
            kind: rustim_mobject::shapes::ShapeKind::Circle,
            params: Default::default(),
            children: None,
        })
        .expect("a typed command");
    assert_eq!(reply.diff.expect("a diff").created.len(), 1);

    finish(client, &png);
}

/// `capabilities()` itself is exercised by every test above; this keeps a direct
/// reference so the helper is not compiled only under `cfg(test)`-like luck.
#[test]
fn capabilities_are_stable_across_sessions() {
    assert_eq!(capabilities(), capabilities());
}
