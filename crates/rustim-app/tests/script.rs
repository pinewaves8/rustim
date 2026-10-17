//! The command feed, end to end, through the real binary.
//!
//! `--script` is what turns rustim from "seven compiled-in scenes" into something
//! a host drives: a session is a list of JSON commands, and the same list replays
//! to the same frames. That claim is only worth anything if it is made against
//! the binary a user runs — argument parsing, the headless path, the reply
//! stream, and the file it writes — so every test here spawns
//! `CARGO_BIN_EXE_rustim-app`, exactly as the golden suite does.
//!
//! # Why there is no new golden baseline
//!
//! The obvious way to pin "a script replays deterministically" is a third
//! baseline image. It would also demand a macOS and a Windows copy of that image
//! before either platform could go green, which is a cost this step has no
//! reason to charge. Determinism is instead asserted the strong way: run the same
//! script twice and require the two frames to be **byte-for-byte identical**
//! (`max == 0`, no differing pixels at all) — stricter than any golden tolerance,
//! and it needs one platform's worth of pixels none of which are checked in.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rustim_export::{Tolerance, compare_png_files};
use rustim_scene::command::CommandReply;

/// The binary under test, built by cargo for this test run.
const BIN: &str = env!("CARGO_BIN_EXE_rustim-app");

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The example session: every action the protocol has, and the ids the feed's
/// own replies handed out.
fn session() -> PathBuf {
    manifest_dir().join("tests/fixtures/script/session.jsonl")
}

/// Where this suite's renders and scratch scripts go.
///
/// Under `target/`, so nothing here is ever committed: the only images in this
/// repository are the ones a human decided on, and the golden suite owns those.
fn artifacts_dir() -> PathBuf {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map_or_else(|| manifest_dir().join("../../target"), PathBuf::from);
    let dir = target.join("script-artifacts");
    std::fs::create_dir_all(&dir).expect("create artifacts dir");
    dir
}

fn out_path(name: &str) -> PathBuf {
    artifacts_dir().join(format!("{}-{}.png", name, std::env::consts::OS))
}

/// Write a scratch script once, for the tests that need a script the fixture
/// deliberately is not (one with a refusal in it).
fn scratch(name: &str, body: &str) -> PathBuf {
    let path = artifacts_dir().join(format!("{name}.jsonl"));
    std::fs::write(&path, body).expect("write scratch script");
    path
}

struct Ran {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
}

impl Ran {
    fn ok(&self, what: &str) -> &Self {
        assert!(
            self.status.success(),
            "`{what}` failed with {}:\n{}{}",
            self.status,
            self.stdout,
            self.stderr,
        );
        self
    }

    fn replies(&self) -> Vec<CommandReply> {
        self.stdout
            .lines()
            .map(|l| {
                serde_json::from_str(l).unwrap_or_else(|e| {
                    panic!("stdout carries something that is not a reply: {l:?} ({e})")
                })
            })
            .collect()
    }
}

fn run(args: &[&str]) -> Ran {
    let output = Command::new(BIN)
        .args(args)
        .output()
        .expect("run rustim-app");
    Ran {
        status: output.status,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// Every physical line number in the fixture that carries a command, 1-based.
///
/// Read out of the file rather than restated here, so the fixture may grow a
/// blank line — which it deliberately has one of, to pin that a blank line
/// occupies a sequence number and produces no reply — without this test needing
/// an edit.
fn expected_seqs(path: &Path) -> Vec<u64> {
    std::fs::read_to_string(path)
        .expect("read the script")
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, _)| i as u64 + 1)
        .collect()
}

/// A script's commands reach the engine, and `--inspect` shows them.
///
/// The tree is the *consumer-facing* view of what a script built — the same
/// projection a screen reader gets — so this is the assertion that says the
/// lines became objects rather than merely that the process exited zero.
#[test]
fn a_script_names_the_objects_the_engine_reports() {
    let ran = run(&["--script", session().to_str().unwrap(), "--inspect"]);
    ran.ok("--script … --inspect");

    let tree: serde_json::Value =
        serde_json::from_str(&ran.stdout).expect("--inspect must emit JSON");
    let nodes = tree["nodes"].as_array().expect("nodes must be an array");
    assert_eq!(
        nodes.len(),
        3,
        "the root and the two objects the session leaves standing, in:\n{}",
        serde_json::to_string_pretty(&tree).unwrap_or_default(),
    );

    let names: Vec<&str> = nodes
        .iter()
        .filter_map(|n| n["name"].as_str())
        .filter(|n| !n.is_empty())
        .collect();
    // The arrow is added and then removed by the session, so its label is absent
    // — which is also the assertion that `remove` reached the tree.
    assert_eq!(names, ["圆", "方"]);
}

/// **The acceptance criterion**: the same script replays to the same frame.
///
/// Stricter than a golden: not "within tolerance", but no differing pixel at
/// all. A script is applied at virtual time zero and the still is ten seconds of
/// virtual time later, so every beat in it has settled — any difference between
/// two runs would be an id that drifted, a `HashMap` iteration order reaching
/// the draw list, or an animation reading the wall clock.
#[test]
fn replaying_a_script_twice_gives_the_same_frame() {
    let script = session();
    let a = out_path("replay-a");
    let b = out_path("replay-b");
    for out in [&a, &b] {
        run(&[
            "--script",
            script.to_str().unwrap(),
            "--png",
            out.to_str().unwrap(),
        ])
        .ok("--script … --png");
    }

    let tolerance = Tolerance::default();
    let diff = compare_png_files(&a, &b, &tolerance)
        .unwrap_or_else(|e| panic!("comparing {} against {}: {e}", a.display(), b.display()));
    assert_eq!(
        diff.max,
        0,
        "a script replayed twice produced two different frames.\n{}",
        diff.report(&tolerance),
    );
    assert!(
        diff.diff_bbox.is_none(),
        "no pixel may differ at all, not merely none by more than the tolerance"
    );
}

/// One reply per command, in the order they were sent, each saying what changed.
///
/// The `seq` assertion is the one that pins the rule a human debugging a log
/// depends on: sequence numbers are **physical** line numbers, so a blank line
/// consumes one and produces no reply.
#[test]
fn the_reply_stream_names_each_command_and_what_it_changed() {
    let script = session();
    let ran = run(&[
        "--script",
        script.to_str().unwrap(),
        "--reply",
        "--png",
        out_path("replies").to_str().unwrap(),
    ]);
    ran.ok("--script … --reply --png");

    let replies = ran.replies();
    assert_eq!(
        replies.iter().map(|r| r.seq).collect::<Vec<_>>(),
        expected_seqs(&script),
        "one reply per non-blank line, numbered by the line it came from",
    );
    assert!(
        replies.iter().all(|r| r.error.is_none()),
        "the fixture is a session that works: {:?}",
        replies
            .iter()
            .filter_map(|r| r.error.as_ref())
            .collect::<Vec<_>>(),
    );

    // `add` is where the host learns the id it must use from then on — without
    // this the rest of a session could not be written.
    let created: Vec<usize> = replies
        .iter()
        .map(|r| {
            r.diff
                .as_ref()
                .expect("no error means a diff")
                .created
                .len()
        })
        .collect();
    assert_eq!(created, [1, 1, 0, 1, 0, 0, 0, 0, 0, 0]);

    // A `play` queues a beat, which changes nothing in the tree, and says so
    // with an empty diff rather than with no diff: the reply is present and its
    // emptiness is the honest answer. The five plays are the only empty ones —
    // `set` and `remove` both name what they touched.
    let plays = replies.iter().filter(|r| {
        r.diff
            .as_ref()
            .is_some_and(|d| d.created.is_empty() && d.modified.is_empty() && d.removed.is_empty())
    });
    assert_eq!(
        plays.count(),
        5,
        "one reply per play, empty because a beat is not a change"
    );
}

/// A refused command is answered, and the session continues.
///
/// This is the whole posture of the command layer — a refusal is a value, never a
/// panic — carried up to the transport: a model that gets one line wrong loses
/// that line and not the session.
#[test]
fn a_refused_command_is_reported_and_the_script_continues() {
    let script = scratch(
        "refusal",
        concat!(
            r#"{"action":"add","type":"square","params":{"label":"一"}}"#,
            "\n",
            // No such node: refused by the tree.
            r#"{"action":"set","target":999,"property":{"name":"opacity","value":0.5}}"#,
            "\n",
            r#"{"action":"add","type":"circle","params":{"label":"二"}}"#,
            "\n",
        ),
    );
    let ran = run(&[
        "--script",
        script.to_str().unwrap(),
        "--reply",
        "--png",
        out_path("refusal").to_str().unwrap(),
    ]);
    // Exit zero: a refused line is data, not a process failure.
    ran.ok("--script with a refusal in it");

    let replies = ran.replies();
    assert_eq!(replies.len(), 3, "every line is answered, refused or not");
    assert!(replies[0].diff.is_some() && replies[0].error.is_none());
    let refusal = replies[1].error.as_ref().expect("the refusal");
    let wire = serde_json::to_value(refusal).expect("a refusal serialises");
    assert_eq!(wire["error"], "no_such_node");
    // The message a human reads names the id the way the reply does — the opaque
    // integer, not `NodeId(…v…)`. Both sides are read by whoever is debugging the
    // session, and they have to be comparable.
    let target = wire["target"].as_u64().expect("the refusal names the id");
    assert_eq!(refusal.to_string(), format!("no such node: {target}"));
    assert!(
        replies[2].diff.is_some(),
        "the line after a refusal is applied normally"
    );

    // And the human-facing summary, on stderr so it is not part of the protocol.
    assert!(
        ran.stderr.contains("1 of 3 commands refused"),
        "stderr should say what happened:\n{}",
        ran.stderr,
    );
    assert!(
        !ran.stdout.contains("refused"),
        "the summary must not land in the reply stream:\n{}",
        ran.stdout,
    );
}

/// A line that is not a command at all is refused the same way.
///
/// The distinction a host needs is "the engine declined what I asked" versus "the
/// engine could not read what I sent", and both arrive in the same vocabulary —
/// as `CommandError`, one of which the application constructs.
#[test]
fn an_unparseable_line_is_reported_and_the_script_continues() {
    let script = scratch(
        "unparseable",
        concat!(
            "this is not JSON at all\n",
            r#"{"action":"add","type":"circle","params":{"label":"圆"}}"#,
            "\n",
        ),
    );
    let ran = run(&[
        "--script",
        script.to_str().unwrap(),
        "--reply",
        "--png",
        out_path("unparseable").to_str().unwrap(),
    ]);
    ran.ok("--script with a malformed line in it");

    let replies = ran.replies();
    let error = replies[0].error.as_ref().expect("the malformed line");
    assert_eq!(
        serde_json::to_value(error).unwrap()["error"],
        "unparseable",
        "a parse failure is its own error kind, not a generic refusal: {error:?}",
    );
    assert!(
        error.to_string().len() > "not a command: ".len(),
        "the host is told why, not just that: {error}"
    );
    assert!(replies[1].diff.is_some(), "the next line still runs");
}

/// **The end-to-end proof that a host can drive a running engine.**
///
/// A host cannot write a session in advance: it has to add an object, learn the
/// id the engine gave it, and animate *that*. So the two lines cannot be sent
/// together, and this test sends the second only after reading the id out of the
/// first reply — which also makes it the only test that can see a missing flush.
/// A reply left in a buffer until the process exits passes every other test here
/// and hangs this one.
#[test]
fn a_host_learns_the_id_it_just_created_and_animates_it() {
    let out = out_path("handshake");
    let child = Command::new(BIN)
        .args(["--script", "-", "--reply", "--png", out.to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn rustim-app");

    // A child that never answers would hang the test run for as long as CI is
    // willing to wait, and the hang would say nothing about why. The watchdog
    // kills it, which closes the pipe and turns the hang into a failed read.
    let child = Arc::new(Mutex::new(child));
    let done = Arc::new(AtomicBool::new(false));
    let watchdog = {
        let child = Arc::clone(&child);
        let done = Arc::clone(&done);
        std::thread::spawn(move || {
            for _ in 0..300 {
                if done.load(Ordering::SeqCst) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            if !done.load(Ordering::SeqCst) {
                let _ = child.lock().expect("child lock").kill();
            }
        })
    };

    let mut stdin = child.lock().unwrap().stdin.take().expect("piped stdin");
    let mut stdout = BufReader::new(child.lock().unwrap().stdout.take().expect("piped stdout"));

    let mut ask = |line: &str| -> CommandReply {
        writeln!(stdin, "{line}").expect("write a command");
        stdin.flush().expect("flush a command");
        let mut reply = String::new();
        let n = stdout.read_line(&mut reply).expect("read a reply");
        assert!(
            n > 0,
            "the engine closed the pipe without answering — was the reply flushed?"
        );
        serde_json::from_str(&reply).expect("a reply is one JSON object")
    };

    let added = ask(r#"{"action":"add","type":"circle","params":{"radius":2.0,"label":"圆"}}"#);
    let id = added
        .diff
        .as_ref()
        .expect("an add succeeds")
        .created
        .first()
        .copied()
        .expect("the host learns the new id, which is the whole point");

    let spec = format!(
        r#"{{"action":"play","animation":{{"target":{},"type":"fade_in","duration":1.5}}}}"#,
        id.raw()
    );
    let played = ask(&spec);
    assert!(played.error.is_none(), "animating the new id: {played:?}");
    assert!(
        played.diff.as_ref().is_some_and(|d| d.is_empty()),
        "queueing a beat changes nothing in the tree, and says so: {played:?}",
    );

    // EOF is how a session ends; the run then settles and writes its frame.
    drop(stdin);
    done.store(true, Ordering::SeqCst);

    let status = child.lock().unwrap().wait().expect("wait for the child");
    watchdog.join().expect("join the watchdog");
    assert!(status.success(), "the session ended with {status}");
    assert!(
        out.exists(),
        "the frame was written after the session ended"
    );
}

/// A script layers onto a named scene rather than replacing it.
///
/// The two are orthogonal, and the cheapest proof is an empty script: the frame
/// must be the scene's own, byte for byte. The second half — a non-empty script
/// *does* change it — is there so the first half cannot pass by the script being
/// ignored entirely.
#[test]
fn a_script_applies_on_top_of_a_named_scene() {
    let bare = out_path("rotate-bare");
    let empty = out_path("rotate-empty");
    run(&["--scene", "rotate", "--png", bare.to_str().unwrap()]).ok("--scene rotate --png");
    run(&[
        "--scene",
        "rotate",
        "--script",
        "/dev/null",
        "--png",
        empty.to_str().unwrap(),
    ])
    .ok("--scene rotate --script /dev/null --png");

    let tolerance = Tolerance::default();
    let same = compare_png_files(&bare, &empty, &tolerance).expect("compare");
    assert!(
        same.diff_bbox.is_none() && same.max == 0,
        "an empty script moved the scene's own frame — the feed is supposed to be \
         orthogonal to it.\n{}",
        same.report(&tolerance),
    );

    let layered = out_path("rotate-layered");
    let extra = scratch(
        "on-top",
        concat!(
            r#"{"action":"add","type":"square","params":{"width":1.0,"label":"加的"}}"#,
            "\n",
        ),
    );
    run(&[
        "--scene",
        "rotate",
        "--script",
        extra.to_str().unwrap(),
        "--png",
        layered.to_str().unwrap(),
    ])
    .ok("--scene rotate --script … --png");

    let different = compare_png_files(&bare, &layered, &tolerance).expect("compare");
    assert!(
        different.max > 0,
        "a script added a square and the frame did not change:\n{}",
        different.report(&tolerance),
    );
}

/// A script path that cannot be read is an `exit(2)`, not a silent no-op.
///
/// The one failure here that is not a per-line refusal: there is no line to
/// refuse, and a host that asked for a session and got none must hear about it.
#[test]
fn a_missing_script_file_is_an_error() {
    let ran = run(&[
        "--script",
        "/nonexistent/session.jsonl",
        "--png",
        "/dev/null",
    ]);
    assert_eq!(ran.status.code(), Some(2), "got {}", ran.status);
    assert!(
        ran.stderr.contains("/nonexistent/session.jsonl"),
        "the message must name the path:\n{}",
        ran.stderr,
    );
}

/// `--reply` with no script, and `--reply` with `--inspect`, are both refusals.
///
/// They are the same refusal read from two directions: a reply exists per
/// command, and stdout can belong to one stream only. Answering either with
/// silence would look exactly like an engine that refuses everything.
#[test]
fn the_two_ways_to_misuse_reply_are_refused() {
    let no_script = run(&["--reply", "--png", "/dev/null"]);
    assert_eq!(no_script.status.code(), Some(2), "got {}", no_script.status);
    assert!(
        no_script.stderr.contains("--script"),
        "{}",
        no_script.stderr
    );

    let both = run(&["--script", "/dev/null", "--reply", "--inspect"]);
    assert_eq!(both.status.code(), Some(2), "got {}", both.status);
    assert!(both.stderr.contains("--inspect"), "{}", both.stderr);
}
