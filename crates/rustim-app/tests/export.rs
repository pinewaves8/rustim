//! Video export end to end, through the real binary.
//!
//! The sink is a seam (`rustim_export::VideoSink`) and the interesting claims are
//! about the *loop* around it: the frames it writes are the frames the still
//! path builds, the timeline really advances, and a subtitle cue is timed by the
//! canvas's virtual clock rather than by how long the machine took to draw.
//! None of that can be checked without running `CARGO_BIN_EXE_rustim-app` —
//! argument parsing, the export branch, the files on disk — so this suite spawns
//! it, exactly as the golden and feed suites do.
//!
//! # Why the frame counts here are small
//!
//! `write_rgba_png` goes through `flate2`, which is *unoptimised* in a debug
//! build: one 1280x720 frame costs about 0.6 s there, against a few milliseconds
//! under `--release`. A 600-frame sequence export therefore takes six minutes in
//! the test profile and a few seconds as a user runs it. The tests that need a
//! real sequence stay at a few dozen frames, and the one full-length claim —
//! "the last frame is the still" — is made on a scene that does not animate, so
//! one frame is the same claim as six hundred.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use rustim_export::{Tolerance, compare_png_files};

/// The binary under test, built by cargo for this test run.
const BIN: &str = env!("CARGO_BIN_EXE_rustim-app");

/// The frame size the binary exports at, from `main.rs`.
const WIDTH: u32 = 1280;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Where this suite's exports go: under `target/`, so nothing is ever committed.
fn dir(tag: &str) -> PathBuf {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map_or_else(|| manifest_dir().join("../../target"), PathBuf::from);
    let dir = target
        .join("export-artifacts")
        .join(format!("{tag}-{:?}", std::thread::current().id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create artifacts dir");
    dir
}

fn run(args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .output()
        .expect("run rustim-app")
}

fn run_with_stdin(args: &[&str], stdin: &str) -> Output {
    use std::io::Write as _;
    use std::process::Stdio;
    let mut child = Command::new(BIN)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn rustim-app");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(stdin.as_bytes())
        .expect("write the script");
    child.wait_with_output().expect("wait for rustim-app")
}

fn frame(dir: &Path, n: u32) -> PathBuf {
    dir.join(format!("frame-{n:05}.png"))
}

/// A one-frame export of a *static* scene is the still.
///
/// `primitives` declares no animation, so its frame at one sixtieth of a second
/// and its frame at ten seconds are the same image — which makes one exported
/// frame exactly the claim "the export loop builds the frame the still path
/// builds", with the same renderer, the same projection and the same draw walk.
#[test]
fn an_exported_frame_is_the_still() {
    let d = dir("still");
    let seq = d.join("seq");
    let still = d.join("still.png");

    let exported = run(&[
        "--scene",
        "primitives",
        "--motion",
        "full",
        "--frames",
        "1",
        "--video-png",
        seq.to_str().unwrap(),
    ]);
    assert!(
        exported.status.success(),
        "--video-png failed:\n{}",
        String::from_utf8_lossy(&exported.stderr)
    );
    assert_eq!(std::fs::read_dir(&seq).unwrap().count(), 1);

    let shot = run(&[
        "--scene",
        "primitives",
        "--motion",
        "full",
        "--png",
        still.to_str().unwrap(),
    ]);
    assert!(
        shot.status.success(),
        "--png failed:\n{}",
        String::from_utf8_lossy(&shot.stderr)
    );

    let diff = compare_png_files(&frame(&seq, 0), &still, &Tolerance::default())
        .expect("both files are the same size");
    assert_eq!(
        diff.max,
        0,
        "an exported frame and the still disagree: {}",
        diff.report(&Tolerance::default())
    );
    assert!(diff.diff_bbox.is_none());
    let _ = std::fs::remove_dir_all(&d);
}

/// The loop walks the timeline rather than repeating one image, and the walk is
/// deterministic: the same command twice gives the same bytes.
#[test]
fn an_export_walks_the_timeline_and_repeats() {
    let d = dir("walk");
    let first = d.join("first");
    let second = d.join("second");

    for out in [&first, &second] {
        let exported = run(&[
            "--scene",
            "rotate",
            "--motion",
            "full",
            "--frames",
            "12",
            "--video-png",
            out.to_str().unwrap(),
        ]);
        assert!(
            exported.status.success(),
            "--video-png failed:\n{}",
            String::from_utf8_lossy(&exported.stderr)
        );
        assert_eq!(std::fs::read_dir(out).unwrap().count(), 12);
    }

    // The opening fade-in is still running at frame 11, so the timeline has
    // moved. A sink that wrote the same frame twelve times would fail here.
    let moving = compare_png_files(&frame(&first, 0), &frame(&first, 11), &Tolerance::default())
        .expect("both frames are the same size");
    assert!(
        moving.max > 0,
        "twelve frames of an animating scene are all identical"
    );

    // And no wall clock leaked into the pixels: a byte-for-byte match, not a
    // golden tolerance.
    let repeat = compare_png_files(
        &frame(&first, 11),
        &frame(&second, 11),
        &Tolerance::default(),
    )
    .expect("both frames are the same size");
    assert_eq!(repeat.max, 0, "two runs of one export disagree");
    let _ = std::fs::remove_dir_all(&d);
}

/// A subtitle cue is timed by the virtual clock, and a new line ends the one it
/// replaces.
///
/// Three lines at 0.0, 0.1 and 0.2 over half a second of timeline: the first two
/// are cut where the next begins (that is when the band changes text), and the
/// last runs to the end of the file rather than past it.
///
/// The sink is whichever one this machine can offer — `ffmpeg` when it is
/// installed, a PNG sequence otherwise — because the cue track is built by the
/// loop and must not depend on where the frames went.
#[test]
fn the_srt_times_the_narration_on_virtual_time() {
    let d = dir("srt");
    let srt = d.join("track.srt");
    let seq = d.join("seq");
    let mp4 = d.join("out.mp4");
    let script = concat!(
        r#"{"action":"narrate","text":"甲"}"#,
        "\n",
        r#"{"action":"narrate","text":"乙","at":0.1}"#,
        "\n",
        r#"{"action":"narrate","text":"丙","at":0.2}"#,
        "\n",
    );

    let use_ffmpeg = rustim_app::speech::find("ffmpeg").is_some();
    let sink = if use_ffmpeg {
        mp4.to_str().unwrap()
    } else {
        seq.to_str().unwrap()
    };
    let mode = if use_ffmpeg { "--video" } else { "--video-png" };

    let exported = run_with_stdin(
        &[
            "--script",
            "-",
            "--scene",
            "primitives",
            "--motion",
            "full",
            "--frames",
            "30",
            mode,
            sink,
            "--srt",
            srt.to_str().unwrap(),
        ],
        script,
    );
    assert!(
        exported.status.success(),
        "the export failed:\n{}",
        String::from_utf8_lossy(&exported.stderr)
    );

    assert_eq!(
        std::fs::read_to_string(&srt).expect("the srt was written"),
        concat!(
            "1\n00:00:00,000 --> 00:00:00,100\n甲\n\n",
            "2\n00:00:00,100 --> 00:00:00,200\n乙\n\n",
            "3\n00:00:00,200 --> 00:00:00,500\n丙\n\n",
        )
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// With `ffmpeg` on `PATH`, `--video` produces a file and says so.
///
/// The frame count is three: this asserts the pipe, not the picture — whether
/// the encoder accepted the stream is `FfmpegSink::finish`'s question, and a
/// truncated file is reported as a failure by the binary rather than by this
/// test. Skipped only when there is no encoder to call, which is the normal
/// state of a stock macOS or Windows.
#[test]
fn an_mp4_is_written_when_ffmpeg_is_on_the_path() {
    if rustim_app::speech::find("ffmpeg").is_none() {
        eprintln!("no `ffmpeg` on PATH; --video-png covers the same loop");
        return;
    }
    let d = dir("mp4");
    let out = d.join("out.mp4");
    let exported = run(&[
        "--scene",
        "primitives",
        "--motion",
        "full",
        "--frames",
        "3",
        "--video",
        out.to_str().unwrap(),
    ]);
    assert!(
        exported.status.success(),
        "--video failed:\n{}",
        String::from_utf8_lossy(&exported.stderr)
    );
    let bytes = std::fs::read(&out).expect("the mp4 was written");
    assert!(bytes.len() > 64, "the mp4 is {} bytes", bytes.len());
    assert!(
        String::from_utf8_lossy(&exported.stdout).contains("3 frames at 60 fps"),
        "{}",
        String::from_utf8_lossy(&exported.stdout)
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// A directory a longer run already filled is reported, not cleaned: the frames
/// past this run's end would otherwise make the directory read as a longer
/// sequence than the command produced.
#[test]
fn a_stale_frame_is_reported_and_not_deleted() {
    let d = dir("stale");
    let seq = d.join("seq");
    std::fs::create_dir_all(&seq).unwrap();
    let stale = frame(&seq, 999);
    std::fs::write(&stale, b"left over").unwrap();

    let exported = run(&[
        "--scene",
        "primitives",
        "--motion",
        "full",
        "--frames",
        "1",
        "--video-png",
        seq.to_str().unwrap(),
    ]);
    assert!(exported.status.success());
    let stderr = String::from_utf8_lossy(&exported.stderr);
    assert!(stderr.contains("already holds 1 frame-*.png"), "{stderr}");
    assert!(stale.exists(), "the note must not clean the directory");
    let _ = std::fs::remove_dir_all(&d);
}

/// No `ffmpeg` is one clear line and exit 2, before a single frame is rendered.
///
/// The empty `PATH` is the whole simulation: the binary resolves the program
/// itself rather than letting `spawn` fail, so this is the same lookup a machine
/// without one performs.
#[test]
fn a_missing_ffmpeg_is_a_refusal_that_names_the_fallback() {
    let d = dir("no-ffmpeg");
    let empty = d.join("empty-path");
    std::fs::create_dir_all(&empty).unwrap();
    let out = d.join("out.mp4");
    let exported = Command::new(BIN)
        .args([
            "--scene",
            "primitives",
            "--frames",
            "1",
            "--video",
            out.to_str().unwrap(),
        ])
        .env("PATH", &empty)
        .output()
        .expect("run rustim-app");
    assert_eq!(exported.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&exported.stderr);
    assert!(stderr.contains("no `ffmpeg` on PATH"), "{stderr}");
    assert!(stderr.contains("--video-png"), "{stderr}");
    assert!(!out.exists());
    let _ = std::fs::remove_dir_all(&d);
}

/// The ways an export cannot be asked for are values, not a silent precedence
/// rule — and they are checked before anything is rendered.
#[test]
fn contradictory_export_flags_are_refused() {
    let d = dir("flags");
    let seq = d.join("seq");
    let still = d.join("still.png");
    let mp4 = d.join("out.mp4");
    let srt = d.join("track.srt");
    let s = [seq.to_str().unwrap(), still.to_str().unwrap()];

    let cases: [(&[&str], &str); 5] = [
        (
            &["--video", mp4.to_str().unwrap(), "--video-png", s[0]],
            "--video and --video-png are mutually exclusive",
        ),
        (&["--video", mp4.to_str().unwrap(), "--png", s[1]], "--png"),
        (
            &["--srt", srt.to_str().unwrap(), "--scene", "pythagorean"],
            "--srt needs",
        ),
        (
            &["--video", mp4.to_str().unwrap(), "--frames", "0"],
            "--frames 0",
        ),
        (&["--video"], "takes a path"),
    ];
    for (args, needle) in cases {
        let out = run(args);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{args:?} should have been refused, got {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.stdout.is_empty(),
            "{args:?} wrote to stdout: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(needle), "{args:?}: {stderr}");
    }
    assert!(!seq.exists() && !mp4.exists() && !srt.exists());
    let _ = std::fs::remove_dir_all(&d);
}

/// The band is drawn into the exported frames, and it is the caption's own
/// band: the three shapes of `primitives` sit above it.
///
/// This is the one assertion that the export path and the windowed path append
/// the caption at the same place — without `--captions` the two exports differ
/// only in the bottom band.
#[test]
fn captions_are_drawn_into_the_exported_frames() {
    let d = dir("captions");
    let plain = d.join("plain");
    let banded = d.join("banded");
    for (out, extra) in [(&plain, None), (&banded, Some("--captions"))] {
        let mut args = vec![
            "--scene",
            "primitives",
            "--motion",
            "full",
            "--frames",
            "2",
            "--video-png",
            out.to_str().unwrap(),
        ];
        if let Some(flag) = extra {
            args.push(flag);
        }
        let exported = run(&args);
        assert!(
            exported.status.success(),
            "--video-png failed:\n{}",
            String::from_utf8_lossy(&exported.stderr)
        );
    }

    // The content box legitimately grows: the caption *is* new content.
    let tolerance = Tolerance {
        bbox_must_match: false,
        ..Tolerance::default()
    };
    let diff = compare_png_files(&frame(&plain, 1), &frame(&banded, 1), &tolerance)
        .expect("both frames are the same size");
    let bbox = diff.diff_bbox.expect("the band changed some pixels");
    assert!(
        bbox.y0 >= 720 - 200,
        "the caption's pixels start at y={}, above the bottom band: {}",
        bbox.y0,
        diff.report(&tolerance)
    );
    assert!(
        bbox.y1 < 720 && bbox.x1 < WIDTH,
        "the band runs outside the frame: {}",
        diff.report(&tolerance)
    );
    let _ = std::fs::remove_dir_all(&d);
}
