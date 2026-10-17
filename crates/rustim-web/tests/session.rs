//! The browser host's own frame, checked natively.
//!
//! `Session` is target independent on purpose, and this file is the reason:
//! "the browser runs the Pythagorean scene" is asserted by comparing the
//! session's pixels — through the same `compare_png_files` the golden suite
//! uses — against the baseline the desktop binary produced. The rest of the
//! browser claims (a real canvas, a real accessibility tree) need Chrome, and
//! live in `tools/web-check.mjs`; nothing here pretends to cover them.

use std::path::{Path, PathBuf};

use rustim_export::{Tolerance, compare_png_files};
use rustim_web::Session;
use rustim_web::aria;
use rustim_web::session::{STILL_SECONDS, scene_names};

const WIDTH: u16 = 1280;
const HEIGHT: u16 = 720;

/// The same per-platform, per-arch path the golden suite reads.
fn baseline_path(scene: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../rustim-app/tests/goldens")
        .join(scene)
        .join(format!(
            "{}-{}.png",
            std::env::consts::OS,
            std::env::consts::ARCH
        ))
}

/// Where renders go: under `target/`, never committed.
fn artifacts_dir() -> PathBuf {
    let target = std::env::var_os("CARGO_TARGET_DIR").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"),
        PathBuf::from,
    );
    target.join("web-artifacts")
}

fn settled(scene: &str) -> Session {
    let mut session = Session::new(WIDTH, HEIGHT, scene).expect("the scene builds");
    session.advance(STILL_SECONDS);
    assert!(
        session.render_error().is_none(),
        "rasterisation failed: {:?}",
        session.render_error()
    );
    session
}

/// The claim the whole crate is for: the canvas frame is the desktop golden,
/// pixel for pixel.
#[test]
fn the_browser_frame_is_the_desktop_golden() {
    let session = settled("pythagorean");
    let baseline = baseline_path("pythagorean");
    if !baseline.exists() {
        // The macOS and Windows baselines are bootstrapped from a CI artifact
        // (see `CLAUDE.md`, "Golden images"). Failing here would duplicate the
        // golden suite's job on the platforms it deliberately does not treat
        // differently; the determinism test below still runs everywhere.
        eprintln!(
            "no baseline at {}; only the desktop golden suite owes one",
            baseline.display()
        );
        return;
    }

    let dir = artifacts_dir();
    std::fs::create_dir_all(&dir).expect("create artifacts dir");
    let shot = dir.join("web-pythagorean.png");
    // Straight alpha, which is what a PNG (and the canvas) holds — see
    // `rustim_render::straight_rgba`.
    let straight = rustim_render::straight_rgba(session.pixels());
    rustim_export::write_rgba_png(&straight, WIDTH.into(), HEIGHT.into(), &shot)
        .expect("the frame is exactly one RGBA buffer");

    let tolerance = Tolerance::default();
    let diff = compare_png_files(&shot, &baseline, &tolerance).expect("both files are one frame");
    assert_eq!(
        diff.max,
        0,
        "the browser frame and the desktop golden disagree: {}",
        diff.report(&tolerance)
    );
    assert!(diff.diff_bbox.is_none());
}

/// Two sessions, two runs, the same bytes — no wall clock, no shared state.
#[test]
fn two_runs_agree_byte_for_byte() {
    let a = settled("pythagorean");
    let b = settled("pythagorean");
    assert_eq!(a.pixels(), b.pixels());
    assert_eq!(a.frames(), b.frames());
}

/// The semantic tree the DOM layer projects names the figure, and the ARIA
/// projection keeps the roles, the tree shape and the measured bounds.
#[test]
fn the_semantic_tree_names_the_figure() {
    let session = settled("pythagorean");
    let tree = session.semantic();
    assert_eq!(tree.nodes.len(), 5, "one root and four mobjects");

    let names: Vec<&str> = tree.nodes.iter().map(|n| n.name.as_str()).collect();
    for expected in ["直角三角形", "a 的正方形", "b 的正方形", "c 的正方形"] {
        assert!(
            names.contains(&expected),
            "{expected} missing from {names:?}"
        );
    }
    // The description is measured, never declared. The figure is fitted to 6
    // units tall, so the leg of 3 is drawn 1.8 long and the square's area is
    // 3.24 — a declaration would still say 9, which is exactly why the number
    // here is the scaled one.
    let square = tree
        .nodes
        .iter()
        .find(|n| n.name == "a 的正方形")
        .expect("the square is in the tree");
    assert!(
        square.description.contains("面积 3.24"),
        "{}",
        square.description
    );

    let aria = aria::project(&tree);
    assert_eq!(aria.len(), tree.nodes.len());
    assert_eq!(aria[0].role, "group", "the root is the group");
    assert_eq!(aria[0].children.len(), 4, "the root names every mobject");
    assert!(aria[0].name.is_empty());

    for node in &aria[1..] {
        assert_eq!(node.role, "img", "every figure is a picture");
        assert!(!node.hidden, "a settled frame has nothing hidden");
        assert!(node.bounds.is_some(), "a figure has measured bounds");
        assert!(node.mathml.is_none(), "nothing here is a formula");
        assert!(!node.description.contains("位置"), "{}", node.description);
    }
}

/// `apply` is the browser's command bridge, and it answers exactly as the
/// desktop feed does — because it calls the same two functions.
#[test]
fn apply_answers_like_the_desktop_feed() {
    let mut session = Session::new(WIDTH, HEIGHT, "pythagorean").expect("the scene builds");

    let reply = session
        .apply(r#"{"action":"add","type":"circle","params":{"radius":1.0,"label":"圆"}}"#)
        .expect("a command is not a blank line");
    let reply: serde_json::Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(reply["seq"], 1);
    let id = reply["diff"]["created"][0]
        .as_u64()
        .expect("the add reports the id it created");
    assert_eq!(session.semantic().nodes.len(), 6);

    // A line that is not a command is a refusal, and the tree is untouched.
    let refused: serde_json::Value =
        serde_json::from_str(&session.apply("{").expect("broken JSON is still something")).unwrap();
    assert_eq!(refused["seq"], 2);
    assert!(refused.get("error").is_some());
    assert!(refused.get("diff").is_none());
    assert_eq!(session.semantic().nodes.len(), 6);

    // `play` queues a beat: a present-but-empty diff.
    let play = format!(
        r#"{{"action":"play","animation":{{"target":{id},"type":"fade_in","duration":0.5}}}}"#
    );
    let queued: serde_json::Value = serde_json::from_str(&session.apply(&play).unwrap()).unwrap();
    assert_eq!(queued["seq"], 3);
    assert_eq!(queued["diff"]["created"].as_array().unwrap().len(), 0);
    assert_eq!(queued["diff"]["modified"].as_array().unwrap().len(), 0);
    assert_eq!(queued["diff"]["removed"].as_array().unwrap().len(), 0);

    // A blank physical line is skipped and consumes a line number.
    assert!(session.apply("   ").is_none());
    let after: serde_json::Value = serde_json::from_str(
        &session
            .apply(r#"{"action":"add","type":"square","params":{"width":1.0}}"#)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(after["seq"], 5);
}

/// A query over the browser's bridge is answered with the semantic tree and no
/// diff — the same reply the desktop feed writes, because it is the same two
/// functions.
#[test]
fn a_query_answers_with_the_tree_and_no_diff() {
    let mut session = Session::new(WIDTH, HEIGHT, "pythagorean").expect("the scene builds");

    let whole: serde_json::Value =
        serde_json::from_str(&session.apply(r#"{"action":"query"}"#).unwrap()).unwrap();
    assert_eq!(whole["seq"], 1);
    assert!(whole.get("diff").is_none() && whole.get("error").is_none());
    let nodes = whole["tree"]["nodes"].as_array().expect("a document");
    assert_eq!(nodes.len(), 5, "the root and the four figures");
    assert!(
        nodes.iter().any(|n| n["name"] == "直角三角形"),
        "the query sees the same names the DOM layer does"
    );
    // `live` is present and null, exactly as it is in `--inspect`: the
    // announcement text belongs to the host's accessibility layer, and the
    // kernel has none to offer.
    assert!(whole["tree"]["live"].is_null());

    // A targeted query keeps the node and its descendants; a figure has none,
    // so its subtree is itself, rooted at its own id.
    let id = nodes[1]["id"].as_u64().expect("an id is a number");
    let subtree: serde_json::Value = serde_json::from_str(
        &session
            .apply(&format!(r#"{{"action":"query","target":{id}}}"#))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(subtree["tree"]["root"].as_u64(), Some(id));
    assert_eq!(subtree["tree"]["nodes"].as_array().unwrap().len(), 1);

    // And a node that is not there is a refusal, not an empty document.
    let refused: serde_json::Value =
        serde_json::from_str(&session.apply(r#"{"action":"query","target":999}"#).unwrap())
            .unwrap();
    assert!(refused.get("tree").is_none());
    assert_eq!(refused["error"]["error"], "no_such_node");
}

/// The formula page's scene, under the feature that creates it.
///
/// Typst is the one dependency this crate carries optionally, so a build
/// without the feature must not claim the scene — and a build with it must
/// hand the DOM layer a real `<math>` element, not a flattened sentence.
#[cfg(feature = "formula")]
#[test]
fn the_formula_scene_carries_its_mathml() {
    use rustim_scene::a11y::SemanticRole;

    assert!(scene_names().contains(&"formula"));
    let session = Session::new(WIDTH, HEIGHT, "formula").expect("the formula compiles");
    let tree = session.semantic();

    let formula = tree
        .nodes
        .iter()
        .find(|n| n.name == "公式")
        .expect("the formula is labelled");
    assert_eq!(formula.role, SemanticRole::Math);
    let mathml = formula.mathml.as_deref().expect("the structure was kept");
    assert!(mathml.starts_with("<math>"), "{mathml}");
    // Only structure: the geometry has no way to say "a fraction" or "a
    // superscript", which is why the second compile exists at all.
    assert!(mathml.contains("<mfrac>"), "{mathml}");
    assert!(mathml.contains("<msup>"), "{mathml}");

    // The projection keeps the string unchanged; the browser is what turns it
    // into elements.
    let aria = aria::project(&tree);
    let node = aria
        .iter()
        .find(|n| n.name == "公式")
        .expect("the formula is projected");
    assert_eq!(node.role, "math");
    assert_eq!(node.mathml.as_deref(), Some(mathml));
}

/// A build knows exactly the scenes it can construct, and says so otherwise.
#[test]
fn an_unknown_scene_is_a_value_not_a_panic() {
    assert!(scene_names().contains(&"pythagorean"));
    let err = match Session::new(WIDTH, HEIGHT, "no-such-scene") {
        Ok(_) => panic!("an unknown scene must not build"),
        Err(e) => e,
    };
    assert!(err.contains("no-such-scene"), "{err}");
    assert!(err.contains("pythagorean"), "{err}");
    assert!(Session::new(0, HEIGHT, "pythagorean").is_err());
}
