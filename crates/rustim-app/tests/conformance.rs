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
//!    the build actually has (31 actions, 12 shapes, 11 animations, 10
//!    properties, 45 rates);
//! 2. every advertised shape and animation is accepted by the binary, and every
//!    advertised rate is accepted on a `play` — the capability list is measured
//!    against the parser rather than trusted;
//! 3. `add` → `set` → `query` agrees on ids, labels and properties;
//! 4. `play` answers with a present-but-empty diff and `stop` is idempotent;
//! 5. a bad command, a bad version and bad JSON are readable refusals, and the
//!    feed continues afterwards (exit code still 0);
//! 6. **two independent clients replaying one log produce the same semantic
//!    tree** — the switch-clients property, in the transport v1 has.
//! 7. a command list is one command: `batch` answers with its net diff,
//!    `validate` with an empty one, and a refusal in the middle names the
//!    member without stopping the feed.
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
    assert_eq!(caps.shapes.len(), 12, "{:?}", caps.shapes);
    assert_eq!(caps.animations.len(), 11, "{:?}", caps.animations);
    assert_eq!(caps.properties.len(), 10, "{:?}", caps.properties);
    assert_eq!(caps.rates.len(), 45, "{:?}", caps.rates);
    assert_eq!(caps.actions.len(), 31, "{:?}", caps.actions);
    assert_eq!(
        caps.profiles,
        ["primitive".to_owned(), "teaching".to_owned()],
        "`teaching` is advertised because the build accepts command lists"
    );
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
        // rather than the vocabulary. A polygon needs its vertices for the same
        // reason — the list *is* the geometry. Every other kind is
        // self-describing.
        let line = match name.as_str() {
            "text" => r#"{"action":"add","type":"text","params":{"text":"你好"}}"#.to_owned(),
            "formula" => r#"{"action":"add","type":"formula","params":{"text":"x^2"}}"#.to_owned(),
            "polygon" => {
                r#"{"action":"add","type":"polygon","params":{"points":[[0.0,0.0],[1.0,0.0],[0.0,1.0]]}}"#
                    .to_owned()
            }
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

/// A command list is answered like any other command: one reply, the **net**
/// change for a `batch`, an empty diff for a `validate` — and a refusal in the
/// middle stops at that member and says which one, while the feed continues.
#[test]
fn batch_and_validate_answer_through_the_wire() {
    let (mut client, png) = client("batch");

    // A dry run answers "nothing changed" and really changes nothing: the
    // query afterwards cannot see the circle it checked. The second member
    // names the first member's label, which is the property that makes a dry
    // run worth sending an interdependent plan to.
    let dry = expect_diff(
        &mut client,
        r#"{"action":"validate","commands":[{"action":"add","type":"circle","params":{"label":"预演"}},{"action":"set","target":"预演","property":{"name":"opacity","value":0.5}}]}"#,
    );
    assert!(dry.is_empty(), "a dry run changes nothing: {dry:?}");
    let reply = client
        .send_raw(r#"{"action":"query","target":"预演"}"#)
        .expect("a refusal is a reply");
    assert!(reply.error.is_some(), "the dry run created nothing");

    // The real list reports the tree *before* against *after*: a node created
    // and then written is one `created` and no `modified`, and a node created
    // and removed inside the list is in neither half.
    let diff = expect_diff(
        &mut client,
        r#"{"action":"batch","commands":[{"action":"add","type":"circle","params":{"radius":1.0,"label":"圆"}},{"action":"set","target":"圆","property":{"name":"opacity","value":0.5}},{"action":"add","type":"square","params":{"label":"临时"}},{"action":"remove","target":"临时"}]}"#,
    );
    assert_eq!(diff.created.len(), 1, "{diff:?}");
    assert!(
        diff.modified.is_empty() && diff.removed.is_empty(),
        "the net change is one creation: {diff:?}"
    );

    // A refusal names the member's position, and the prefix stays applied —
    // the documented half-application, not a rollback.
    let reply = client
        .send_raw(
            r#"{"action":"batch","commands":[{"action":"add","type":"square","params":{"label":"前缀"}},{"action":"remove","target":"没有这个"}]}"#,
        )
        .expect("a refusal is a reply");
    match reply.error.expect("the second member is refused") {
        rustim_client::CommandError::InBatch { index, cause } => {
            assert_eq!(index, 1);
            assert!(
                matches!(*cause, rustim_client::CommandError::NoSuchLabel { .. }),
                "{cause:?}"
            );
        }
        other => panic!("the refusal has to name the member: {other:?}"),
    }
    let prefix = expect_diff(
        &mut client,
        r#"{"action":"set","target":"前缀","property":{"name":"opacity","value":0.25}}"#,
    );
    assert_eq!(prefix.modified.len(), 1, "the prefix was applied");

    // A member whose answer is not a diff is refused as a *shape* error, at
    // its own position, before anything runs.
    let reply = client
        .send_raw(r#"{"action":"batch","commands":[{"action":"query"}]}"#)
        .expect("a refusal is a reply");
    let error = reply.error.expect("a query cannot live in a list");
    assert!(format!("{error}").contains("commands[0]"), "{error}");

    finish(client, &png);
}

/// The teaching tier's layout actions are ordinary commands: they run through
/// the same wire, inside a `batch`, and the placement they produce is visible
/// in the tree's measured bounds — which is the only way to prove the action
/// reached the real protocol rather than only the crate that defines it.
#[test]
fn layout_actions_place_objects_through_the_wire() {
    let (mut client, png) = client("layout");

    // Three squares of different sizes, created and arranged in one round trip
    // — the shape a teaching client actually sends.
    let diff = expect_diff(
        &mut client,
        r#"{"action":"batch","commands":[{"action":"add","type":"square","params":{"width":1.0,"label":"甲"}},{"action":"add","type":"square","params":{"width":2.0,"label":"乙"}},{"action":"add","type":"square","params":{"width":0.5,"label":"丙"}},{"action":"arrange","targets":["甲","乙","丙"],"axis":"row","buff":0.5}]}"#,
    );
    // `created` is the net change of the list: the three adds. The arrangement
    // moved nodes the same list created, so the net diff does not repeat them
    // in `modified`.
    assert_eq!(diff.created.len(), 3, "{diff:?}");
    assert!(diff.modified.is_empty(), "{diff:?}");

    let query = |client: &mut StdioClient, label: &str| {
        client
            .send_raw(&format!(r#"{{"action":"query","target":"{label}"}}"#))
            .expect("a query is a reply")
            .tree
            .expect("a tree")
    };
    let (a, b, c) = (
        query(&mut client, "甲"),
        query(&mut client, "乙"),
        query(&mut client, "丙"),
    );
    // A query reports **pixel** bounds, so the world-space `buff` has to be
    // converted through the snapshot's own camera rather than assumed to be
    // 90 px/unit — the test is about the placement, not the projection.
    let px_per_unit = f64::from(a.camera.frame[1]) / a.camera.visible[1];
    let gap = 0.5 * px_per_unit;
    let (ba, bb, bc) = (
        a.nodes[0].bounds.expect("a measurable node"),
        b.nodes[0].bounds.expect("a measurable node"),
        c.nodes[0].bounds.expect("a measurable node"),
    );
    assert!((bb.x0 - ba.x1 - gap).abs() < 1e-6, "{ba:?} {bb:?}");
    assert!((bc.x0 - bb.x1 - gap).abs() < 1e-6, "{bb:?} {bc:?}");
    assert!(
        (ba.center().y - bb.center().y).abs() < 1e-6,
        "a row is a row: {ba:?} {bb:?}"
    );

    // `next_to` defaults its gap (Manim's `SMALL_BUFF`) when the field is
    // absent, and reports the id it moved.
    let moved = expect_diff(
        &mut client,
        r#"{"action":"next_to","target":"甲","reference":"乙","direction":"left"}"#,
    )
    .modified;
    assert_eq!(moved.len(), 1, "one object moved");

    // `fit` is the third action, and its refusals are the protocol's: a plain
    // payload error naming the field, not a transport failure.
    let reply = client
        .send_raw(r#"{"action":"fit","targets":["甲"],"width":0.0,"height":1.0}"#)
        .expect("a refusal is a reply");
    let error = reply.error.expect("a zero extent is refused");
    assert!(format!("{error}").contains("width"), "{error}");

    finish(client, &png);
}

/// The teaching tier's attention actions are ordinary commands: `point_at`
/// creates a node and answers with its id (the handle a client needs to fade or
/// remove the arrow later), and `highlight` queues a beat and answers with an
/// empty diff — the same distinction `play` has with `add`.
#[test]
fn attention_commands_work_through_the_wire() {
    let (mut client, png) = client("attention");

    expect_diff(
        &mut client,
        r#"{"action":"add","type":"square","params":{"width":2.0,"label":"三角形"}}"#,
    );
    expect_diff(
        &mut client,
        r#"{"action":"add","type":"square","params":{"width":1.0,"position":[3.0,0.0],"label":"旁物"}}"#,
    );

    let created = expect_diff(
        &mut client,
        r#"{"action":"point_at","target":"三角形","direction":"down","buff":0.2,"label":"看这里"}"#,
    )
    .created;
    assert_eq!(created.len(), 1, "one arrow was created");
    let arrow = created[0];

    // The created arrow is addressable by the id the reply named, and by the
    // label it carries — both are the protocol's normal spellings.
    let tree = client
        .send_raw(&format!(r#"{{"action":"query","target":{}}}"#, arrow.raw()))
        .expect("a query is a reply")
        .tree
        .expect("a tree");
    assert_eq!(tree.root, arrow);
    assert_eq!(tree.nodes[0].name, "看这里");
    assert!(
        tree.nodes[0].bounds.is_some(),
        "a placed arrow has measured bounds"
    );

    // A callout is two pictures from one measurement, and the label is always
    // first: `created[0]` is the words, `created[1]` its arrow. The text is
    // typeset by the host, so this also proves the real binary's typesetter
    // reaches the teaching tier.
    let created = expect_diff(
        &mut client,
        r#"{"action":"annotate","target":"三角形","text":"面积 4","label":"注释","color":[100,181,246]}"#,
    )
    .created;
    assert_eq!(created.len(), 2, "a label and its arrow");
    let label = created[0];
    let tree = client
        .send_raw(&format!(r#"{{"action":"query","target":{}}}"#, label.raw()))
        .expect("a query is a reply")
        .tree
        .expect("a tree");
    assert_eq!(tree.nodes[0].name, "注释", "the label names the text only");
    assert!(
        tree.nodes[0].bounds.is_some(),
        "the placed label has measured bounds"
    );

    // A highlight is a beat: the tree does not move until the frames run, so
    // the diff is present and empty, like `play`'s.
    let diff = expect_diff(
        &mut client,
        r#"{"action":"highlight","target":"三角形","dim":0.5}"#,
    );
    assert!(diff.is_empty(), "{diff:?}");

    // And its refusals are the protocol's, not transport failures.
    let error = client
        .send_raw(r#"{"action":"highlight","target":"三角形","dim":2.0}"#)
        .expect("a refusal is a reply")
        .error
        .expect("dim above 1 is refused");
    assert!(format!("{error}").contains("dim"), "{error}");

    finish(client, &png);
}

/// The first `explain.*` template is still an ordinary command on the wire: it
/// answers with the group first and addressable pieces after it, and the group
/// declares the union of their boxes so the whole explanation can be placed,
/// focused or queried as one object. The caption is typeset by the host, which
/// is why this also proves the real binary's typesetter reaches the templates.
#[test]
fn the_equation_template_builds_one_addressable_group() {
    let (mut client, png) = client("equation");

    let created = expect_diff(
        &mut client,
        r#"{"action":"explain.equation","formula":"a^2 + b^2 = c^2","position":[0.5,0.5],"caption":"勾股定理","label":"定理"}"#,
    )
    .created;
    assert_eq!(created.len(), 3, "the group, the formula root, the caption");

    let group = created[0];
    let tree = client
        .send_raw(&format!(r#"{{"action":"query","target":{}}}"#, group.raw()))
        .expect("a query is a reply")
        .tree
        .expect("a tree");
    assert_eq!(tree.root, group);
    assert_eq!(tree.nodes[0].name, "定理", "the label names the group");
    assert!(
        tree.nodes[0].bounds.is_some(),
        "the group declares the union of its pieces"
    );
    assert!(
        tree.nodes.iter().any(|node| node.name == "公式"),
        "the formula keeps its own name under the group: {:?}",
        tree.nodes.iter().map(|n| &n.name).collect::<Vec<_>>()
    );

    // A caption that says nothing is refused by field, and the group does not
    // survive the refusal.
    let error = client
        .send_raw(r#"{"action":"explain.equation","formula":"x^2","caption":"   ","label":"空的"}"#)
        .expect("a refusal is a reply")
        .error
        .expect("a blank caption is refused");
    assert!(format!("{error}").contains("caption"), "{error}");
    let tree = client
        .send_raw(r#"{"action":"query"}"#)
        .expect("a query is a reply")
        .tree
        .expect("a tree");
    assert!(
        !tree.nodes.iter().any(|node| node.name == "空的"),
        "a refused template left its group behind"
    );

    finish(client, &png);
}

/// The second `explain.*` template has the same shape on the wire, and two
/// properties of its own that only the real binary can prove: the tick labels
/// are typeset by the host's typesetter (they are nodes, not pixels), and a
/// curve that leaves the frame is several addressable pieces rather than one
/// broken polyline.
#[test]
fn the_plot_template_groups_axes_a_curve_and_ticks() {
    let (mut client, png) = client("plot");

    let created = expect_diff(
        &mut client,
        r#"{"action":"explain.plot","coefficients":[0.0,0.0,1.0],"position":[0.5,0.5],"caption":"y = x²","label":"图像"}"#,
    )
    .created;
    // The group, the axes, one curve piece, eight x labels (-4..4 without 0),
    // six y labels (-3..3 without 0), and the caption.
    assert_eq!(created.len(), 18, "{:?}", created);

    let group = created[0];
    let tree = client
        .send_raw(&format!(r#"{{"action":"query","target":{}}}"#, group.raw()))
        .expect("a query is a reply")
        .tree
        .expect("a tree");
    assert_eq!(tree.root, group);
    assert_eq!(tree.nodes[0].name, "图像", "the label names the group");
    assert!(
        tree.nodes[0].bounds.is_some(),
        "the group declares the union of its pieces"
    );
    // The caption is the last piece and carries no label of its own, so it is
    // addressable by the id the diff reported and announces as a text node.
    let caption = *created.last().unwrap();
    let tree = client
        .send_raw(&format!(
            r#"{{"action":"query","target":{}}}"#,
            caption.raw()
        ))
        .expect("a query is a reply")
        .tree
        .expect("a tree");
    assert_eq!(tree.root, caption);
    assert_eq!(tree.nodes[0].name, "文本");
    assert!(tree.nodes[0].bounds.is_some(), "the caption has a box");

    let tree = client
        .send_raw(&format!(r#"{{"action":"query","target":{}}}"#, group.raw()))
        .expect("a query is a reply")
        .tree
        .expect("a tree");
    assert!(
        tree.nodes.iter().any(|node| node.name == "4"),
        "the tick labels were typeset: {:?}",
        tree.nodes.iter().map(|n| &n.name).collect::<Vec<_>>()
    );

    // A curve that leaves the band and comes back is two plot pieces, not one
    // polyline with a chord across the gap: x² ∈ [0.5, 3] on two disjoint
    // intervals, so the group holds the group, the axes, two curves, eight x
    // labels and three y labels (1, 2, 3).
    let created = expect_diff(
        &mut client,
        r#"{"action":"explain.plot","coefficients":[0.0,0.0,1.0],"y_range":[0.5,3.0],"label":"双段"}"#,
    )
    .created;
    assert_eq!(created.len(), 15, "{:?}", created);

    // An empty coefficient list is refused by field, and the group does not
    // survive the refusal.
    let error = client
        .send_raw(r#"{"action":"explain.plot","coefficients":[],"label":"空的"}"#)
        .expect("a refusal is a reply")
        .error
        .expect("an empty polynomial is refused");
    assert!(format!("{error}").contains("coefficients"), "{error}");
    let tree = client
        .send_raw(r#"{"action":"query"}"#)
        .expect("a query is a reply")
        .tree
        .expect("a tree");
    assert!(
        !tree.nodes.iter().any(|node| node.name == "空的"),
        "a refused template left its group behind"
    );

    finish(client, &png);
}

/// The third `explain.*` template: a figure and everything a lesson needs
/// around it. The pieces that only the real binary can prove are that the side
/// and vertex labels are typeset by the host's shaper (they are nodes, not
/// pixels), and that the right-angle marker is placed rather than left to the
/// caller.
#[test]
fn the_geometry_template_groups_a_figure_its_labels_and_a_marker() {
    let (mut client, png) = client("geometry");

    let created = expect_diff(
        &mut client,
        r#"{"action":"explain.geometry","vertices":[[0.0,0.0],[3.0,0.0],[0.0,4.0]],"side_labels":["a","b","c"],"vertex_labels":["A","B","C"],"caption":"直角三角形","label":"三角形"}"#,
    )
    .created;
    // The group, the figure, one marker (the right angle at the origin), three
    // side labels, three vertex labels and the caption.
    assert_eq!(created.len(), 10, "{:?}", created);

    let group = created[0];
    let tree = client
        .send_raw(&format!(r#"{{"action":"query","target":{}}}"#, group.raw()))
        .expect("a query is a reply")
        .tree
        .expect("a tree");
    assert_eq!(tree.root, group);
    assert_eq!(tree.nodes[0].name, "三角形", "the label names the group");
    assert!(
        tree.nodes[0].bounds.is_some(),
        "the group declares the union of its pieces"
    );
    let names: Vec<&str> = tree.nodes.iter().map(|node| node.name.as_str()).collect();
    assert!(
        names.contains(&"图形"),
        "the figure keeps its own name: {names:?}"
    );
    assert!(
        names.contains(&"直角标记"),
        "the right angle was marked: {names:?}"
    );
    assert!(
        names.contains(&"a") && names.contains(&"A"),
        "the side and vertex labels were typeset: {names:?}"
    );

    // A figure with no area is refused by field, and the group does not survive
    // the refusal.
    let error = client
        .send_raw(
            r#"{"action":"explain.geometry","vertices":[[0.0,0.0],[1.0,1.0],[2.0,2.0]],"label":"空的"}"#,
        )
        .expect("a refusal is a reply")
        .error
        .expect("a collinear figure is refused");
    assert!(format!("{error}").contains("vertices"), "{error}");
    let tree = client
        .send_raw(r#"{"action":"query"}"#)
        .expect("a query is a reply")
        .tree
        .expect("a tree");
    assert!(
        !tree.nodes.iter().any(|node| node.name == "空的"),
        "a refused template left its group behind"
    );

    finish(client, &png);
}

/// The fourth `explain.*` template: a matrix whose cells are real, named nodes.
///
/// The pieces only the real binary can prove are that the host's shaper draws
/// every entry (a cell is a glyph run, not a picture), that two identical cells
/// are an `ambiguous_label` rather than a silent pick, and that a refusal by
/// field leaves nothing behind.
#[test]
fn the_matrix_template_groups_its_cells_and_addresses_each_by_its_text() {
    let (mut client, png) = client("matrix");

    // The group, two square brackets, four cells, two column labels, two row
    // labels and a caption.
    let created = expect_diff(
        &mut client,
        r#"{"action":"explain.matrix","rows":[[1.0,2.0],[1.0,4.0]],"column_labels":["列一","列二"],"row_labels":["甲","乙"],"caption":"行列式","label":"矩阵"}"#,
    )
    .created;
    assert_eq!(created.len(), 12, "{:?}", created);

    let group = created[0];
    let tree = client
        .send_raw(&format!(r#"{{"action":"query","target":{}}}"#, group.raw()))
        .expect("a query is a reply")
        .tree
        .expect("a tree");
    assert_eq!(tree.root, group);
    assert_eq!(tree.nodes[0].name, "矩阵", "the label names the group");
    assert!(
        tree.nodes[0].bounds.is_some(),
        "the group declares the union of its pieces"
    );
    let names: Vec<&str> = tree.nodes.iter().map(|node| node.name.as_str()).collect();
    for name in [
        "左括号",
        "右括号",
        "1",
        "2",
        "4",
        "列一",
        "列二",
        "甲",
        "乙",
    ] {
        assert!(names.contains(&name), "missing {name}: {names:?}");
    }

    // A duplicate cell is not a key: addressing "1" names two nodes in draw
    // order instead of picking one.
    let error = client
        .send_raw(r#"{"action":"set","target":"1","property":{"name":"opacity","value":0.5}}"#)
        .expect("a refusal is a reply")
        .error;
    assert!(
        matches!(
            error,
            Some(rustim_client::CommandError::AmbiguousLabel { .. })
        ),
        "{error:?}"
    );

    // Ragged rows are refused under `rows`, and the refused group does not
    // survive.
    let error = client
        .send_raw(r#"{"action":"explain.matrix","rows":[[1.0],[2.0,3.0]],"label":"残缺的矩阵"}"#)
        .expect("a refusal is a reply")
        .error
        .expect("a ragged matrix is refused");
    assert!(format!("{error}").contains("rows"), "{error}");
    let tree = client
        .send_raw(r#"{"action":"query"}"#)
        .expect("a query is a reply")
        .tree
        .expect("a tree");
    assert!(
        !tree.nodes.iter().any(|node| node.name == "残缺的矩阵"),
        "a refused template left its group behind"
    );

    finish(client, &png);
}

/// The fifth `explain.*` template: a bar chart whose bars are named objects.
///
/// The pieces only the real binary can prove are that the host's shaper draws
/// both the category labels and the value axis's numbers, that a bar is
/// addressable by its category, that a negative value hangs below the baseline
/// without needing a second command, and that a window without zero is refused
/// rather than drawn with a truncated baseline.
#[test]
fn the_chart_template_addresses_each_bar_by_its_category() {
    let (mut client, png) = client("chart");

    // Group, two axes, three bars, three value labels, four value-axis ticks
    // (-4, -2, 0, 2 on the automatic window and step) and three category
    // labels, plus the caption.
    let created = expect_diff(
        &mut client,
        r#"{"action":"explain.chart","categories":["一","二","三"],"values":[3.0,-5.0,2.0],"caption":"各季度销量","label":"柱状图"}"#,
    )
    .created;
    assert_eq!(created.len(), 17, "{:?}", created);

    let group = created[0];
    let tree = client
        .send_raw(&format!(r#"{{"action":"query","target":{}}}"#, group.raw()))
        .expect("a query is a reply")
        .tree
        .expect("a tree");
    assert_eq!(tree.root, group);
    assert_eq!(tree.nodes[0].name, "柱状图", "the label names the group");
    assert!(
        tree.nodes[0].bounds.is_some(),
        "the group declares the union of its pieces"
    );
    let names: Vec<&str> = tree.nodes.iter().map(|node| node.name.as_str()).collect();
    for name in ["数值轴", "类别轴", "一", "二", "三", "-5"] {
        assert!(names.contains(&name), "missing {name}: {names:?}");
    }

    // A bar is addressable by its category, and the label resolves to the bar
    // rather than to the category label underneath it (which carries none).
    let diff = expect_diff(
        &mut client,
        r#"{"action":"set","target":"二","property":{"name":"opacity","value":0.5}}"#,
    );
    assert_eq!(diff.modified, vec![created[4]], "the second bar");

    // A window that excludes zero is refused under `value_range`, and the
    // refused group does not survive.
    let error = client
        .send_raw(
            r#"{"action":"explain.chart","categories":["a"],"values":[3.0],"value_range":[1.0,5.0],"label":"错图"}"#,
        )
        .expect("a refusal is a reply")
        .error
        .expect("a truncated baseline is refused");
    assert!(format!("{error}").contains("value_range"), "{error}");
    let tree = client
        .send_raw(r#"{"action":"query"}"#)
        .expect("a query is a reply")
        .tree
        .expect("a tree");
    assert!(
        !tree.nodes.iter().any(|node| node.name == "错图"),
        "a refused template left its group behind"
    );

    finish(client, &png);
}

/// The sixth `explain.*` template: one frame of an algorithm's state, with
/// numbered cells and named pointers.
///
/// The pieces only the real binary can prove are that the host's shaper draws
/// each cell's value *and* its zero-based index, that a cell and a pointer are
/// each addressable by name, that two pointers over one column stack rather
/// than collide, and that an out-of-range pointer is refused rather than drawn
/// pointing at nothing.
#[test]
fn the_algorithm_template_addresses_cells_and_pointers_by_name() {
    let (mut client, png) = client("algorithm");

    // Group, four cells each with a value and an index (12), two pointers each
    // with a caret and a name (4), and the caption.
    let created = expect_diff(
        &mut client,
        r#"{"action":"explain.algorithm","values":[5.0,3.0,8.0,1.0],"pointers":[["i",1],["j",1]],"highlight":[1],"caption":"比较中","label":"数组"}"#,
    )
    .created;
    assert_eq!(created.len(), 18, "{:?}", created);

    let group = created[0];
    let tree = client
        .send_raw(&format!(r#"{{"action":"query","target":{}}}"#, group.raw()))
        .expect("a query is a reply")
        .tree
        .expect("a tree");
    assert_eq!(tree.root, group);
    assert_eq!(tree.nodes[0].name, "数组", "the label names the group");
    assert!(
        tree.nodes[0].bounds.is_some(),
        "the group declares the union of its pieces"
    );
    let names: Vec<&str> = tree.nodes.iter().map(|node| node.name.as_str()).collect();
    for name in ["格0", "格1", "格2", "格3", "5", "3", "8", "1", "i", "j"] {
        assert!(names.contains(&name), "missing {name}: {names:?}");
    }

    // A cell is addressable by its own number, and a pointer by its name — two
    // different nodes, so neither lookup is ambiguous.
    let diff = expect_diff(
        &mut client,
        r#"{"action":"set","target":"格2","property":{"name":"opacity","value":0.5}}"#,
    );
    assert_eq!(diff.modified, vec![created[7]], "the third cell");
    let diff = expect_diff(
        &mut client,
        r#"{"action":"set","target":"i","property":{"name":"opacity","value":0.5}}"#,
    );
    assert_eq!(diff.modified, vec![created[14]], "the first pointer's name");

    // A pointer naming a cell that does not exist is refused under `pointers`,
    // and the refused group does not survive.
    let error = client
        .send_raw(
            r#"{"action":"explain.algorithm","values":[1.0],"pointers":[["i",4]],"label":"错帧"}"#,
        )
        .expect("a refusal is a reply")
        .error
        .expect("a pointer into nothing is refused");
    assert!(format!("{error}").contains("pointers"), "{error}");
    let tree = client
        .send_raw(r#"{"action":"query"}"#)
        .expect("a query is a reply")
        .tree
        .expect("a tree");
    assert!(
        !tree.nodes.iter().any(|node| node.name == "错帧"),
        "a refused template left its group behind"
    );

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

/// The playback controls move the clock, not the tree: each answers with a
/// present-but-empty diff, and an impossible value is a refusal naming its field.
#[test]
fn playback_controls_answer_with_empty_diffs() {
    let (mut client, png) = client("playback");

    for line in [
        r#"{"action":"pause"}"#,
        r#"{"action":"resume"}"#,
        r#"{"action":"pause"}"#,
        r#"{"action":"seek","to":1.0}"#,
        r#"{"action":"resume"}"#,
        r#"{"action":"tempo","factor":2.0}"#,
        r#"{"action":"tempo","factor":1.0}"#,
    ] {
        assert!(expect_diff(&mut client, line).is_empty(), "{line}");
    }

    // A backward seek needs a checkpoint, and zero tempo is `pause` misspelt.
    // Both are refused under the field that carried the bad value.
    for (line, field) in [
        (r#"{"action":"seek","to":0.5}"#, "to"),
        (r#"{"action":"tempo","factor":0.0}"#, "factor"),
    ] {
        let reply = client.send_raw(line).expect("a refusal is a reply");
        let error = reply.error.expect("an impossible value is refused");
        let text = format!("{error}");
        assert!(text.contains(field), "{line}: {text}");
    }

    // The feed continues, and the process still ends cleanly.
    let diff = expect_diff(
        &mut client,
        r#"{"action":"add","type":"circle","params":{"radius":1.0}}"#,
    );
    assert_eq!(diff.created.len(), 1);
    finish(client, &png);
}

/// A checkpoint is a mark (empty diff) and an undo is the one host-answered
/// command whose diff names nodes. With the stack empty the answer is a state
/// refusal, not a bad-field one, and the feed continues.
#[test]
fn checkpoint_and_undo_answer_with_the_change_they_made() {
    let (mut client, png) = client("checkpoint");

    let circle = expect_diff(
        &mut client,
        r#"{"action":"add","type":"circle","params":{"radius":1.0,"label":"圆"}}"#,
    );
    assert_eq!(circle.created.len(), 1);
    assert!(
        expect_diff(&mut client, r#"{"action":"checkpoint"}"#).is_empty(),
        "a mark changes nothing in the tree"
    );

    let square = expect_diff(
        &mut client,
        r#"{"action":"add","type":"square","params":{"width":1.0}}"#,
    );
    assert_eq!(square.created.len(), 1);

    let undone = expect_diff(&mut client, r#"{"action":"undo"}"#);
    assert_eq!(
        undone.removed, square.created,
        "the undo must name the square"
    );
    assert!(undone.created.is_empty() && undone.modified.is_empty());

    let reply = client
        .send_raw(r#"{"action":"undo"}"#)
        .expect("a refusal is a reply");
    let error = reply.error.expect("the stack holds one mark");
    assert_eq!(format!("{error}"), "no checkpoint to restore");
    assert!(reply.diff.is_none(), "a refusal carries no diff");

    // The restored tree still answers for the object it kept, and the feed is
    // unharmed.
    let kept = expect_diff(
        &mut client,
        r#"{"action":"set","target":"圆","property":{"name":"opacity","value":0.5}}"#,
    );
    assert_eq!(kept.modified.len(), 1);
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
