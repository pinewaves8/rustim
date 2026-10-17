//! Where encoded frames go: a pluggable sink.
//!
//! The engine renders frames; turning them into a file is a separate question
//! with separate failure modes, and the two sinks here are the two honest
//! answers:
//!
//! - [`FfmpegSink`] pipes raw RGBA to an `ffmpeg` binary. It is what produces
//!   an `.mp4` on a machine that has one, and it deliberately resolves nothing:
//!   the caller names the program, and a missing binary is the caller's report
//!   (the app says so and points at the other sink).
//! - [`PngSequenceSink`] writes `frame-00000.png`, `frame-00001.png`, … with the
//!   `png` encoder this crate already depends on. It needs no external program,
//!   so it works on every platform the engine targets, and its files are a
//!   byte-identical artifact: the frames a scene produces are deterministic,
//!   while an mp4's bytes also depend on the encoder's build and version.
//!
//! A sink takes **raw RGBA8 rows**, top to bottom, tightly packed. The engine's
//! frames are opaque (every scene fills the background first), so premultiplied
//! and straight alpha agree; [`PngSequenceSink`] still un-premultiplies, because
//! a PNG of a transparent frame should be right and the cost is skipped
//! entirely when the frame is opaque.
//!
//! # Why `finish` is a method and not `Drop`
//!
//! A dropped `FfmpegSink` must close the pipe so the encoder flushes and exits
//! — that happens in `Drop` too — but only an explicit `finish` can *report*
//! whether the encoder accepted the stream. A run that wrote a truncated file
//! because `ffmpeg` rejected the last frame should not look like a success to
//! the process that asked for it.

use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};

/// A destination for rendered frames.
pub trait VideoSink {
    /// Write one frame. `rgba` must be exactly `width * height * 4` bytes.
    fn write_frame(&mut self, rgba: &[u8], width: u32, height: u32) -> io::Result<()>;

    /// Close the destination and wait for it to finish.
    ///
    /// Returns the encoder's own failure if it had one, so a truncated file is
    /// never reported as a successful export.
    fn finish(&mut self) -> io::Result<()>;
}

/// Pipe raw RGBA frames to an `ffmpeg` binary.
///
/// `program` is a path or a bare name (the OS's normal search applies); the
/// app resolves `ffmpeg` on `PATH` before this so that "not installed" is one
/// clear line rather than a spawn error halfway through the render.
pub struct FfmpegSink {
    child: Child,
    stdin: Option<ChildStdin>,
    width: u32,
    height: u32,
    out: PathBuf,
}

impl FfmpegSink {
    /// Start an encoder writing `out`.
    ///
    /// `fps` is the rate the caller is *feeding* frames at, which is the
    /// scene's virtual frame rate, not the wall clock's — the export loop is
    /// deterministic and faster than real time.
    pub fn spawn(
        program: impl AsRef<OsStr>,
        out: impl AsRef<Path>,
        width: u32,
        height: u32,
        fps: u32,
    ) -> io::Result<Self> {
        if width == 0 || height == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a video needs a non-zero frame size",
            ));
        }
        if fps == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a video needs a non-zero frame rate",
            ));
        }
        // yuv420p requires even dimensions. Refusing here is better than
        // letting the encoder fail on the first frame, after the caller has
        // already rendered it.
        if !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("h.264 with yuv420p needs even dimensions, not {width}x{height}"),
            ));
        }
        let out = out.as_ref().to_path_buf();
        let mut child = Command::new(program)
            .args(ffmpeg_args(&out, width, height, fps))
            .stdin(Stdio::piped())
            // Keep stdout free for a host's `--reply` stream.
            .stdout(Stdio::null())
            // ffmpeg's errors are the only clue if it rejects a frame, so its
            // stderr joins ours; `-loglevel error` keeps the banner out.
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdin = child.stdin.take();
        Ok(Self {
            child,
            stdin,
            width,
            height,
            out,
        })
    }

    /// The file being written.
    pub fn path(&self) -> &Path {
        &self.out
    }
}

/// The exact argument vector, as data.
///
/// A pure function so the argument list can be asserted without an encoder
/// installed — the same reason `rustim_app::speech::command_for` is pure.
pub fn ffmpeg_args(out: &Path, width: u32, height: u32, fps: u32) -> Vec<OsString> {
    [
        "-y".to_owned(),
        "-loglevel".to_owned(),
        "error".to_owned(),
        "-f".to_owned(),
        "rawvideo".to_owned(),
        "-pixel_format".to_owned(),
        "rgba".to_owned(),
        "-video_size".to_owned(),
        format!("{width}x{height}"),
        "-framerate".to_owned(),
        fps.to_string(),
        "-i".to_owned(),
        "pipe:0".to_owned(),
        "-an".to_owned(),
        "-c:v".to_owned(),
        "libx264".to_owned(),
        "-preset".to_owned(),
        "medium".to_owned(),
        "-crf".to_owned(),
        "18".to_owned(),
        "-pix_fmt".to_owned(),
        "yuv420p".to_owned(),
    ]
    .into_iter()
    .map(OsString::from)
    .chain([out.as_os_str().to_owned()])
    .collect()
}

impl VideoSink for FfmpegSink {
    fn write_frame(&mut self, rgba: &[u8], width: u32, height: u32) -> io::Result<()> {
        if (width, height) != (self.width, self.height) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "frame is {width}x{height}, the encoder was started for {}x{}",
                    self.width, self.height
                ),
            ));
        }
        let expected = width as usize * height as usize * 4;
        if rgba.len() != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "frame is {} bytes, expected {expected} for {width}x{height} RGBA",
                    rgba.len()
                ),
            ));
        }
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| io::Error::other("the encoder's input is already closed"))?;
        stdin.write_all(rgba)?;
        Ok(())
    }

    fn finish(&mut self) -> io::Result<()> {
        // Closing stdin is what tells `ffmpeg` the stream ended; waiting
        // without closing would hang.
        let _ = self.stdin.take();
        let status = self.child.wait()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "ffmpeg exited with {status} while writing {}",
                self.out.display()
            )))
        }
    }
}

impl Drop for FfmpegSink {
    fn drop(&mut self) {
        // A `finish` already waited; this covers the error and early-return
        // paths. Closing the pipe first lets ffmpeg write what it has rather
        // than being killed.
        let _ = self.stdin.take();
        let _ = self.child.wait();
    }
}

/// Write numbered PNG frames into a directory.
///
/// `frame-00000.png`, zero based, five digits — the spelling every frame
/// sequence tool expects, and a name that sorts lexicographically for as many
/// frames as anyone will export.
pub struct PngSequenceSink {
    dir: PathBuf,
    next: u32,
    width: u32,
    height: u32,
}

impl PngSequenceSink {
    /// Create `dir` (and parents) and start writing at frame zero.
    pub fn new(dir: impl AsRef<Path>, width: u32, height: u32) -> io::Result<Self> {
        if width == 0 || height == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a frame sequence needs a non-zero frame size",
            ));
        }
        std::fs::create_dir_all(&dir)?;
        Ok(Self {
            dir: dir.as_ref().to_path_buf(),
            next: 0,
            width,
            height,
        })
    }

    /// The directory frames are written into.
    pub fn path(&self) -> &Path {
        &self.dir
    }

    /// The path frame `n` will be written to.
    pub fn frame_path(&self, n: u32) -> PathBuf {
        self.dir.join(format!("frame-{n:05}.png"))
    }
}

impl VideoSink for PngSequenceSink {
    fn write_frame(&mut self, rgba: &[u8], width: u32, height: u32) -> io::Result<()> {
        if (width, height) != (self.width, self.height) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "frame is {width}x{height}, the sink was made for {}x{}",
                    self.width, self.height
                ),
            ));
        }
        let straight = crate::straight_rgba(rgba);
        crate::write_rgba_png(&straight, width, height, self.frame_path(self.next))?;
        self.next = self.next.saturating_add(1);
        Ok(())
    }

    fn finish(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rustim-video-{}-{tag}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn the_ffmpeg_arguments_say_what_kind_of_input_they_are() {
        let args = ffmpeg_args(Path::new("out.mp4"), 1280, 720, 60);
        let argv: Vec<String> = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(&argv[..4], ["-y", "-loglevel", "error", "-f"]);
        assert_eq!(argv[4], "rawvideo");
        assert!(argv.contains(&"rgba".to_owned()));
        assert!(argv.contains(&"1280x720".to_owned()));
        assert!(argv.contains(&"60".to_owned()));
        assert!(argv.contains(&"pipe:0".to_owned()));
        assert!(argv.contains(&"yuv420p".to_owned()));
        assert_eq!(argv.last().unwrap(), "out.mp4");
    }

    #[test]
    fn odd_dimensions_are_refused_before_a_frame_is_rendered() {
        let dir = unique_dir("odd");
        let err = FfmpegSink::spawn("definitely-not-ffmpeg", dir.join("x.mp4"), 1279, 720, 60)
            .err()
            .expect("an odd width cannot be encoded as yuv420p");
        assert!(err.to_string().contains("even dimensions"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_sequence_numbers_its_frames_from_zero() {
        let dir = unique_dir("sequence");
        let mut sink = PngSequenceSink::new(&dir, 4, 3).unwrap();
        assert_eq!(sink.frame_path(0), dir.join("frame-00000.png"));
        assert_eq!(sink.frame_path(41), dir.join("frame-00041.png"));

        // Two frames: red, then blue.
        let red = [255, 0, 0, 255].repeat(12);
        let blue = [0, 0, 255, 255].repeat(12);
        sink.write_frame(&red, 4, 3).unwrap();
        sink.write_frame(&blue, 4, 3).unwrap();
        sink.finish().unwrap();

        for n in 0..2 {
            let bytes = std::fs::read(sink.frame_path(n)).unwrap();
            assert_eq!(&bytes[0..8], b"\x89PNG\r\n\x1a\n");
        }
        // And the second frame is where the counter says it is.
        let bytes = std::fs::read(sink.frame_path(1)).unwrap();
        assert!(bytes.len() > 8);
        assert!(!dir.join("frame-00002.png").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_frame_of_the_wrong_size_is_refused() {
        let dir = unique_dir("size");
        let mut sink = PngSequenceSink::new(&dir, 4, 3).unwrap();
        let short = [0u8; 11 * 4];
        assert!(sink.write_frame(&short, 4, 3).is_err());
        assert!(sink.write_frame(&short, 5, 3).is_err());
        // Neither refusal wrote a file.
        assert!(!sink.frame_path(0).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_zero_sized_sink_is_refused() {
        let dir = unique_dir("zero");
        assert!(PngSequenceSink::new(&dir, 0, 3).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
