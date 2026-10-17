//! SubRip (`.srt`) cues: the narration track as a file.
//!
//! A scripted lecture already has a narration timeline — `Command::Narrate`
//! lines with a virtual due time — and a video export walks that timeline frame
//! by frame. This module turns the same track into subtitles, so the words are
//! available to a player, an editor, or a translator without a second timing
//! model.
//!
//! The format is deliberately the smallest thing that players accept:
//!
//! ```text
//! 1
//! 00:00:00,000 --> 00:00:03,000
//! 先看这个三角形
//!
//! ```
//!
//! Times are seconds of **virtual** time, converted to the comma-decimal
//! `HH:MM:SS,mmm` spelling SubRip requires (not the dot many tools also take).
//! A cue whose end is not after its start is dropped rather than written as an
//! empty interval: it would be invisible in every player and it is always a
//! caller arithmetic error, not something a viewer should be asked to fix.

use std::fmt::Write as _;
use std::path::Path;

/// One subtitle line.
#[derive(Debug, Clone, PartialEq)]
pub struct Cue {
    /// Start, in seconds of the exported timeline.
    pub start: f64,
    /// End, in the same clock. Must be after `start` to be written.
    pub end: f64,
    pub text: String,
}

/// Render a track as a SubRip document.
///
/// Cues are written in the order given — the caller's timeline order — and the
/// document ends with the blank line the format expects after the last cue.
pub fn format(cues: &[Cue]) -> String {
    let mut out = String::new();
    let mut written = 0usize;
    for cue in cues {
        let text = cue.text.trim_end();
        if cue.end <= cue.start || text.is_empty() {
            continue;
        }
        written += 1;
        // `write!` on a String cannot fail; the result is discarded rather than
        // unwrapped so the function has no panic path.
        let _ = write!(
            out,
            "{written}\n{} --> {}\n{text}\n\n",
            timestamp(cue.start),
            timestamp(cue.end)
        );
    }
    out
}

/// Write [`format`]'s document to `path`.
pub fn write(path: impl AsRef<Path>, cues: &[Cue]) -> std::io::Result<()> {
    std::fs::write(path, format(cues))
}

/// `HH:MM:SS,mmm`, rounded to the nearest millisecond.
///
/// Negative times clamp to zero: a cue cannot begin before the video does, and
/// `-00:00:01` is not a timestamp any parser accepts.
pub fn timestamp(seconds: f64) -> String {
    let ms = if seconds.is_finite() && seconds > 0.0 {
        (seconds * 1000.0).round() as u64
    } else {
        0
    };
    format!(
        "{:02}:{:02}:{:02},{:03}",
        ms / 3_600_000,
        (ms / 60_000) % 60,
        (ms / 1000) % 60,
        ms % 1000
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_track_renders_as_numbered_cues() {
        let cues = [
            Cue {
                start: 0.0,
                end: 3.0,
                text: "先看这个三角形".to_owned(),
            },
            Cue {
                start: 2.0,
                end: 5.25,
                text: "两条直角边是 3 和 4".to_owned(),
            },
        ];
        assert_eq!(
            format(&cues),
            concat!(
                "1\n00:00:00,000 --> 00:00:03,000\n先看这个三角形\n\n",
                "2\n00:00:02,000 --> 00:00:05,250\n两条直角边是 3 和 4\n\n",
            )
        );
    }

    #[test]
    fn timestamps_round_and_carry() {
        assert_eq!(timestamp(1.2345), "00:00:01,235");
        assert_eq!(timestamp(59.9996), "00:01:00,000");
        assert_eq!(timestamp(3661.5), "01:01:01,500");
        assert_eq!(timestamp(-1.0), "00:00:00,000");
        assert_eq!(timestamp(f64::NAN), "00:00:00,000");
    }

    #[test]
    fn empty_or_inverted_cues_are_dropped_and_numbering_stays_dense() {
        let cues = [
            Cue {
                start: 0.0,
                end: 0.0,
                text: "zero length".to_owned(),
            },
            Cue {
                start: 1.0,
                end: 2.0,
                text: "  ".to_owned(),
            },
            Cue {
                start: 3.0,
                end: 2.0,
                text: "backwards".to_owned(),
            },
            Cue {
                start: 4.0,
                end: 5.0,
                text: "kept".to_owned(),
            },
        ];
        let doc = format(&cues);
        assert!(doc.starts_with("1\n"), "{doc}");
        assert!(doc.contains("kept"));
        assert!(!doc.contains("zero length"));
        assert!(!doc.contains("backwards"));
        assert_eq!(doc.matches("-->").count(), 1);
    }

    #[test]
    fn an_empty_track_is_an_empty_file() {
        assert_eq!(format(&[]), "");
    }

    #[test]
    fn writing_a_file_says_the_same_thing_as_formatting_it() {
        let dir = std::env::temp_dir().join(format!("rustim-srt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("track.srt");
        let cues = [Cue {
            start: 0.5,
            end: 1.5,
            text: "hello".to_owned(),
        }];
        write(&path, &cues).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), format(&cues));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
