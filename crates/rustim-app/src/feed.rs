//! The command feed: a stream of JSON commands applied to a running engine.
//!
//! This is the transport half of "an AI drives the engine" (`需求探讨.txt`): the
//! *protocol* — the command values, their refusals, and the replies — lives in
//! `rustim_scene::command`, while reading a file or a pipe and writing back to
//! one lives here, because this crate is the one allowed to do I/O.
//!
//! # One apply point
//!
//! [`apply_all`] is the only place a line of the feed becomes a mutation. The
//! windowed loop calls it with whatever arrived since the last frame and the
//! headless paths call it with the whole script; both therefore agree by
//! construction about what applying a command means, and both get the
//! [`Scene::on_command`] callback and the reply stream for free. It is also why
//! this file can be unit-tested with no window at all: give it a scene, a
//! context and a `Vec<u8>` to write the replies into.
//!
//! # Two sources, two rhythms
//!
//! A **file** is read whole and applied back to back at virtual time zero, so a
//! `play` in it queues a beat and the beats run in the order they were written.
//! That is what makes a session log replayable: the same file, applied the same
//! way, produces the same frames.
//!
//! **stdin** is read as it arrives, because that is what an interactive host
//! needs. Headless it is read a line at a time, applied, and answered before the
//! next line is read — so a host on the other end of a pipe can send `add`, read
//! the id back, and send an animation naming it. Windowed it needs a thread
//! ([`spawn_stdin`]), because the event loop cannot block; the lines come back
//! through the proxy as [`AppEvent::Command`].
//!
//! # Replying
//!
//! A reply is one JSON object per command, on stdout. **Every line is flushed
//! before the next command is read**, which is not an optimisation: the host is
//! on the other end of a pipe waiting for it, and a reply left in the buffer
//! until the process exits is a host that waits forever. Nothing here can make
//! that structural — `stdout` on a pipe is block-buffered by the C library
//! underneath — so the flush is stated here and asserted by
//! `a_host_learns_the_id_it_just_created_and_animates_it`.
//!
//! And the stream is **only** replies: with `--reply` every informational line
//! the binary prints — `wrote out.png (1280x720)`, the frame-rate line — moves to
//! stderr (`main::note`). A host parsing the stream cannot skip a line it did not
//! expect, and a stray one whose presence depended on which other flags were
//! passed would be a bug that hides until someone adds a flag.

use std::io::{self, BufRead, Write};

use rustim_scene::command::{AnimationKind, Command, CommandError, CommandReply, StateDiff};
use rustim_scene::{Scene, SceneContext};

use crate::AppEvent;

/// The line a `--script -` feed reads, and the file path `--script` names.
pub const STDIN: &str = "-";

/// What a feed did, so a session can say so in one line at the end.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub applied: u64,
    pub refused: u64,
}

impl Counts {
    pub fn total(&self) -> u64 {
        self.applied + self.refused
    }

    /// The closing line, or `None` when there is nothing worth saying.
    ///
    /// A clean session says nothing: a script that did what it said is the
    /// expected case, and a line per run would be noise in a log. A refusal is
    /// the one thing a human needs told, because the reply stream may be going
    /// somewhere else entirely (or nowhere).
    pub fn report(&self) -> Option<String> {
        match self.refused {
            0 => None,
            n => Some(format!("feed: {} of {} commands refused", n, self.total())),
        }
    }
}

/// One line of the feed, as the parser sees it.
///
/// `None` means the line carried nothing — blank or whitespace — which is a
/// *skip*, not a refusal: a session log with paragraph breaks in it is still a
/// well-formed session log. Everything else is either a command or the reason
/// there is not one, and the reason is a [`CommandError`] rather than a panic,
/// because the text comes from a language model.
pub fn parse_command(line: &str) -> Option<Result<Command, CommandError>> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    Some(
        serde_json::from_str(line).map_err(|e| CommandError::Unparseable {
            detail: e.to_string(),
        }),
    )
}

/// One reply line, without its newline.
pub fn reply_line(seq: u64, outcome: Result<StateDiff, CommandError>) -> String {
    let reply = match outcome {
        Ok(diff) => CommandReply::applied(seq, diff),
        Err(error) => CommandReply::refused(seq, error),
    };
    // A reply is built from types that already round-trip, so this cannot fail
    // for a reason the caller could act on; a `Result` here would only push the
    // same `unwrap` up a level.
    serde_json::to_string(&reply).expect("a reply is serialisable by construction")
}

/// Apply every line of the feed, in order, and answer each one.
///
/// The single apply point — see the module doc. `reply` is where the answers go;
/// `None` runs the feed silently, which is what a `--script` without `--reply`
/// asks for.
///
/// A refused command does **not** stop the feed. That is the whole posture of
/// the command layer ("refusals are values, never panics") carried one level up:
/// a model that sends one bad line should get that line back with a reason and
/// have the rest of its script run, not lose the session.
pub fn apply_all(
    scene: &mut dyn Scene,
    ctx: &mut SceneContext,
    lines: impl IntoIterator<Item = (u64, String)>,
    mut reply: Option<&mut dyn Write>,
) -> Counts {
    let mut counts = Counts::default();
    for (seq, line) in lines {
        let Some(parsed) = parse_command(&line) else {
            continue;
        };
        let outcome = parsed.and_then(|cmd| match ctx.handle(&cmd) {
            Ok(diff) => {
                // The hook the trait has carried since it was written and that
                // nothing had ever called. A scene that wants to react to what a
                // host did to it — relabel, re-fit, script the next beat — has
                // exactly here to do it.
                scene.on_command(&diff, ctx);
                Ok(diff)
            }
            Err(e) => Err(e),
        });

        match outcome {
            Ok(_) => counts.applied += 1,
            Err(_) => counts.refused += 1,
        }

        if let Some(w) = reply.as_mut() {
            let written = writeln!(w, "{}", reply_line(seq, outcome)).and_then(|()| w.flush());
            if written.is_err() {
                // The host's end of the pipe is gone. Stop answering rather than
                // repeating the same failure for every remaining line; the feed
                // itself keeps applying, because the scene is now the only
                // consumer left and it is still a valid one.
                reply = None;
            }
        }
    }
    counts
}

/// Every line of a script, numbered from 1.
///
/// A file is read whole — a session log is small and reading it up front is what
/// makes the replay deterministic — while [`STDIN`] yields lines as they arrive,
/// so a host can interleave sending and reading.
///
/// `seq` is the **physical** line number, blank lines included. A human
/// debugging a log counts that way, and the reply stream is read by humans too.
pub fn lines_from(path: &str) -> io::Result<Lines> {
    if path == STDIN {
        return Ok(Lines::new(Box::new(io::stdin().lock().lines())));
    }
    let text = std::fs::read_to_string(path)?;
    let owned: Vec<io::Result<String>> = text.lines().map(|l| Ok(l.to_owned())).collect();
    Ok(Lines::new(Box::new(owned.into_iter())))
}

/// A numbered, non-failing view of a stream of lines.
///
/// The reader's errors are reported once and then end the feed: there is no way
/// to continue reading a stream that failed, and a feed that stopped is
/// distinguishable from one that never started by whether anything was applied.
pub struct Lines {
    inner: Box<dyn Iterator<Item = io::Result<String>>>,
    seq: u64,
}

impl Lines {
    fn new(inner: Box<dyn Iterator<Item = io::Result<String>>>) -> Self {
        Self { inner, seq: 0 }
    }
}

impl Iterator for Lines {
    type Item = (u64, String);

    fn next(&mut self) -> Option<(u64, String)> {
        let line = match self.inner.next()? {
            Ok(line) => line,
            Err(e) => {
                eprintln!("feed: {e}");
                return None;
            }
        };
        self.seq += 1;
        Some((self.seq, line))
    }
}

/// Read stdin on its own thread, handing each line to the event loop.
///
/// The windowed half of the interactive feed: winit's loop cannot block, so a
/// blocking read has to happen elsewhere and arrive as an event. It is the
/// second variant [`AppEvent`] was declared to make room for.
///
/// The thread ends at EOF, and also when the loop has gone away (`send_event`
/// fails) — neither is an error, because a host closing its end of the pipe is
/// how a session ends, not a fault.
pub fn spawn_stdin(proxy: winit::event_loop::EventLoopProxy<AppEvent>) {
    std::thread::spawn(move || {
        // `enumerate` is the physical line number here for the same reason it is
        // in `Lines::next`: a reader error ends the feed, so nothing after one is
        // counted.
        for (i, line) in io::stdin().lock().lines().enumerate() {
            let Ok(line) = line else { break };
            let seq = i as u64 + 1;
            if proxy.send_event(AppEvent::Command { seq, line }).is_err() {
                break;
            }
        }
    });
}

/// The animation kinds a session log can name, for `--help`.
///
/// Read off the enum rather than written out by hand, so a new kind cannot be
/// documented in `--help` and missing from the protocol (or the reverse).
pub fn animation_kinds() -> String {
    let names = [
        AnimationKind::FadeIn,
        AnimationKind::FadeOut,
        AnimationKind::MoveTo,
        AnimationKind::Rotate,
        AnimationKind::Scale,
        AnimationKind::Create,
        AnimationKind::Uncreate,
        AnimationKind::Group,
        AnimationKind::Succession,
        AnimationKind::Transform,
    ];
    names
        .iter()
        .map(|k| {
            serde_json::to_string(k)
                .unwrap_or_default()
                .trim_matches('"')
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scene that remembers what it was told, so the hook can be tested
    /// without a window and without a renderer.
    #[derive(Default)]
    struct Recorder {
        seen: Vec<StateDiff>,
    }

    impl Scene for Recorder {
        fn construct(&mut self, _ctx: &mut SceneContext) {}

        fn on_command(&mut self, diff: &StateDiff, _ctx: &mut SceneContext) {
            self.seen.push(diff.clone());
        }
    }

    fn context() -> SceneContext {
        SceneContext::new(rustim_math::Viewport::default(), rustim_anim::Motion::Full)
    }

    fn feed(
        scene: &mut dyn Scene,
        ctx: &mut SceneContext,
        lines: &[&str],
    ) -> (Counts, Vec<CommandReply>) {
        let numbered = lines
            .iter()
            .enumerate()
            .map(|(i, l)| (i as u64 + 1, (*l).to_owned()))
            .collect::<Vec<_>>();
        let mut out = Vec::new();
        let counts = apply_all(scene, ctx, numbered, Some(&mut out));
        let replies = String::from_utf8(out)
            .expect("replies are utf-8")
            .lines()
            .map(|l| serde_json::from_str(l).expect("every reply line is one JSON object"))
            .collect();
        (counts, replies)
    }

    #[test]
    fn a_blank_line_is_skipped_and_is_not_a_refusal() {
        assert!(parse_command("").is_none());
        assert!(parse_command("   \t ").is_none());

        let mut scene = Recorder::default();
        let mut ctx = context();
        let (counts, replies) = feed(
            &mut scene,
            &mut ctx,
            &["", "  ", r#"{"action":"add","type":"square"}"#],
        );
        assert_eq!(
            counts,
            Counts {
                applied: 1,
                refused: 0
            }
        );
        // The blank lines occupy sequence numbers but produce no reply at all.
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].seq, 3);
    }

    #[test]
    fn a_malformed_line_is_a_refusal_with_a_reason_and_not_a_panic() {
        let mut scene = Recorder::default();
        let mut ctx = context();
        let (counts, replies) = feed(&mut scene, &mut ctx, &["{\"action\": \"add\","]);
        assert_eq!(
            counts,
            Counts {
                applied: 0,
                refused: 1
            }
        );
        match replies[0].error.as_ref().expect("a refusal") {
            CommandError::Unparseable { detail } => {
                assert!(!detail.is_empty(), "the host is told why, not just that");
            }
            other => panic!("expected Unparseable, got {other:?}"),
        }
        assert!(replies[0].diff.is_none());
    }

    #[test]
    fn a_successful_command_replies_with_its_id_and_an_empty_diff_for_a_beat() {
        let mut scene = Recorder::default();
        let mut ctx = context();
        let (counts, replies) = feed(
            &mut scene,
            &mut ctx,
            &[
                r#"{"action":"add","type":"circle","params":{"radius":1.5,"label":"圆"}}"#,
                r#"{"action":"play","animation":{"target":0,"type":"fade_in"}}"#,
            ],
        );
        // The second command names node 0, which does not exist — the point here
        // is only that both lines got a reply and the tally matches.
        assert_eq!(counts.applied, 1);
        assert_eq!(counts.refused, 1);

        let created = replies[0]
            .diff
            .as_ref()
            .expect("an add diffs")
            .created
            .clone();
        assert_eq!(created.len(), 1, "the host learns exactly one new id");
        let label = ctx.tree.get(created[0]).unwrap().label.clone();
        assert_eq!(label.as_deref(), Some("圆"));
    }

    /// `focus` is the camera field that needs the tree — it names an object and
    /// frames its bounding box — so the feed is the place it is exercised end to
    /// end: `add`, read the id back, then `camera focus` that id. That is exactly
    /// the round trip an interactive host performs, and the reason `add` reports
    /// the id it created.
    #[test]
    fn a_focus_command_in_a_feed_frames_the_shape_it_names() {
        let mut scene = Recorder::default();
        let mut ctx = context();
        let (_, replies) = feed(
            &mut scene,
            &mut ctx,
            &[r#"{"action":"add","type":"square","params":{"width":2.0,"height":2.0}}"#],
        );
        let id = replies[0].diff.as_ref().expect("an add diffs").created[0];

        let line = format!(r#"{{"action":"camera","focus":{}}}"#, id.raw());
        let (counts, replies) = feed(&mut scene, &mut ctx, &[line.as_str()]);
        assert_eq!(counts.applied, 1);
        assert!(
            replies[0].diff.as_ref().expect("a reply").is_empty(),
            "the view is not a node, so nothing in the tree changed"
        );
        // A 2x2 box at 1280x720: the height is the tighter axis, so the fit is
        // 8 / (1.2 * 2). The host did not have to send a zoom to get it.
        assert_eq!(ctx.camera().center, (0.0, 0.0));
        assert!(
            (ctx.camera().zoom - rustim_math::Camera::WORLD_HEIGHT / (1.2 * 2.0)).abs() < 1e-12,
            "{}",
            ctx.camera().zoom
        );
        // And `camera` is a command, not an animation kind: `--help` must keep
        // saying so.
        assert!(!animation_kinds().contains("camera"));
    }

    /// The hook `Scene::on_command` exists for — and this is the only test that
    /// can see it, because the app's frame loop is the other caller and that
    /// needs a window.
    #[test]
    fn the_scene_hears_about_every_command_that_reached_the_tree() {
        let mut scene = Recorder::default();
        let mut ctx = context();
        let (_, _) = feed(
            &mut scene,
            &mut ctx,
            &[
                r#"{"action":"add","type":"square","params":{"label":"a"}}"#,
                // Refused: no such node. The scene must not hear about it.
                r#"{"action":"set","target":999,"property":{"name":"opacity","value":0.5}}"#,
                r#"{"action":"add","type":"square","params":{"label":"b"}}"#,
            ],
        );
        assert_eq!(scene.seen.len(), 2, "one callback per applied command");
        assert_eq!(scene.seen[0].created.len(), 1);
        assert_eq!(scene.seen[1].created.len(), 1);
        assert_ne!(scene.seen[0].created[0], scene.seen[1].created[0]);
    }

    /// The reply stream survives a host that stops reading.
    #[test]
    fn a_write_failure_stops_the_answers_but_not_the_feed() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "gone"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let mut scene = Recorder::default();
        let mut ctx = context();
        let lines: Vec<(u64, String)> = (1..=3)
            .map(|i| {
                (
                    i,
                    r#"{"action":"add","type":"circle","params":{"radius":1.0}}"#.to_owned(),
                )
            })
            .collect();
        let counts = apply_all(&mut scene, &mut ctx, lines, Some(&mut Broken));
        assert_eq!(counts.applied, 3, "the scene still got all three");
        assert_eq!(scene.seen.len(), 3);
    }

    #[test]
    fn the_closing_line_appears_only_when_something_was_refused() {
        assert!(
            Counts {
                applied: 3,
                refused: 0
            }
            .report()
            .is_none()
        );
        assert_eq!(
            Counts {
                applied: 2,
                refused: 1
            }
            .report()
            .unwrap(),
            "feed: 1 of 3 commands refused"
        );
    }

    /// `--help` and the protocol must not drift: every animation kind the wire
    /// accepts is named there.
    #[test]
    fn the_documented_animation_kinds_are_the_ones_the_wire_takes() {
        assert_eq!(
            animation_kinds(),
            "fade_in, fade_out, move_to, rotate, scale, create, uncreate, group, succession, transform"
        );
        for name in animation_kinds().split(", ") {
            let json = format!(r#"{{"action":"play","animation":{{"target":0,"type":"{name}"}}}}"#);
            let ok = serde_json::from_str::<Command>(&json);
            // `rotate`/`scale`/`move_to` need their parameter, so those three are
            // expected to be refused for *that* — not for an unknown kind, which
            // would be a parse error rather than a refusal.
            match ok {
                Ok(Command::Play { animation }) => {
                    assert_eq!(animation.kind, kind_named(name), "{name}")
                }
                Ok(other) => panic!("{name} parsed as {other:?}"),
                Err(e) => assert!(
                    e.to_string().contains("unknown variant"),
                    "{name} is documented but the wire rejects the *kind*: {e}"
                ),
            }
        }
    }

    fn kind_named(name: &str) -> AnimationKind {
        match name {
            "fade_in" => AnimationKind::FadeIn,
            "fade_out" => AnimationKind::FadeOut,
            "move_to" => AnimationKind::MoveTo,
            "rotate" => AnimationKind::Rotate,
            "create" => AnimationKind::Create,
            "uncreate" => AnimationKind::Uncreate,
            "group" => AnimationKind::Group,
            "succession" => AnimationKind::Succession,
            "transform" => AnimationKind::Transform,
            _ => AnimationKind::Scale,
        }
    }
}
