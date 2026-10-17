//! A formula's structure, end to end: Typst compiles the scene's formulas, the
//! MathML rides on each root mobject, and `--inspect` prints it.
//!
//! The unit tests already pin each hop — `formula.rs` that a compiled formula
//! has a `<math>` element, `a11y.rs` that a mobject's field reaches the semantic
//! node. What they cannot see is the *pairing through the real binary*: the
//! scene's own `FormulaCompiler`, its font paths, and the JSON a host reads. A
//! change that stopped setting the field in `FormulaScene::build`, or that
//! dropped it somewhere in `--inspect`'s projection, would leave both halves
//! green.
//!
//! Nothing here asserts the exact MathML: that string is Typst's, it moves when
//! the pinned Typst version moves, and the claim this file makes is about the
//! survival of structure, not its spelling.

use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_rustim-app");

fn inspect(scene: &str) -> serde_json::Value {
    let out = Command::new(BIN)
        .args(["--scene", scene, "--inspect"])
        .stdin(Stdio::null())
        .output()
        .expect("the binary under test is built");
    assert!(
        out.status.success(),
        "--inspect failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("--inspect emits JSON")
}

#[test]
fn the_formula_scene_reports_its_structure() {
    let tree = inspect("formula");
    let nodes = tree["nodes"].as_array().expect("nodes is a list");

    let math: Vec<&serde_json::Value> = nodes.iter().filter(|n| n["role"] == "math").collect();
    assert!(
        math.len() >= 4,
        "the formula scene declares five formulas, found {}",
        math.len()
    );

    for node in &math {
        let mathml = node["mathml"]
            .as_str()
            .unwrap_or_else(|| panic!("a math node without structure: {node}"));
        assert!(mathml.starts_with("<math>"), "{mathml}");
        assert!(mathml.ends_with("</math>"), "{mathml}");
        // The element is the structure, not a document: no stylesheet, no body.
        assert!(!mathml.contains("<style"), "{mathml}");
        // And it is beside the spoken fields, not inside them.
        assert!(!node["description"].as_str().unwrap().contains('<'));
    }

    // The scene deliberately contains a fraction and a radical, so the check
    // that the *structure* arrived rather than an echo of the glyphs is
    // available on this scene: those two cannot be written as a flat list.
    let all: String = math
        .iter()
        .map(|n| n["mathml"].as_str().unwrap_or_default())
        .collect::<Vec<_>>()
        .join("");
    assert!(all.contains("<mfrac>"), "no fraction in {all}");
    assert!(all.contains("<msqrt>"), "no radical in {all}");
    assert!(all.contains("<msubsup>"), "no sum with limits in {all}");

    // Every other node is structureless, and the field is absent from the JSON
    // rather than null — the wire's rule for absent parameters.
    for node in nodes.iter().filter(|n| n["role"] != "math") {
        assert!(node.get("mathml").is_none(), "{node}");
    }
}
