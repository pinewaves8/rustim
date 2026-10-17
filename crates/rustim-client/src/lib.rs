//! A synchronous client for rustim's stdio command protocol.
//!
//! This crate is the *client half* of [`rustim_scene::command`], and nothing
//! else. It turns a [`Command`] into one JSON line, reads the one reply line
//! back, and checks the two framing rules the protocol states — one reply per
//! command line, exactly one of `diff` / `tree` / `report` / `capabilities` /
//! `error` present — so every consumer gets the same conformance check rather than each
//! reimplementing it.
//!
//! **Why it exists.** The AI layer is not in this repository (see
//! `贾维斯集成方案.md`): an assistant drives the engine across a process
//! boundary, and the tests that prove an arbitrary client can drive it need to
//! *be* such a client. The conformance suite in `rustim-app` uses this crate
//! against the real binary; an external adapter can use it against any host
//! that speaks the protocol. It contains no engine logic, no rendering and no
//! platform code, and it is not one of the wasm kernel crates — it spawns a
//! process, which is a host's job.
//!
//! ```no_run
//! use rustim_client::StdioClient;
//! use std::path::Path;
//!
//! let mut client = StdioClient::spawn(
//!     Path::new("rustim-app"),
//!     &["--script", "-", "--reply", "--png", "/tmp/out.png"],
//! )?;
//! let caps = client.hello()?;
//! assert_eq!(caps.protocol, 1);
//! client.close()?;
//! # Ok::<(), std::io::Error>(())
//! ```
//!
//! The transport writes one line and flushes before reading, so the engine can
//! answer line by line instead of buffering the whole script. A session's
//! lifetime is the process's lifetime; switching clients mid-session is
//! "replay the command log into a fresh process", which is a property of the
//! protocol, not of this type.

use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command as Process, ExitStatus, Stdio};

pub use rustim_scene::check::CheckReport;
pub use rustim_scene::command::{
    Capabilities, Command, CommandError, CommandOutcome, CommandReply, PROTOCOL_VERSION,
    parse_command, reply_line,
};

/// Read exactly one reply line.
///
/// Blank lines are skipped (the engine never writes one — a blank *command*
/// line is a skip and produces no reply — so a blank here means a transport in
/// between inserted a newline, which is worth tolerating rather than
/// mis-parsing). End of stream before a reply is [`io::ErrorKind::UnexpectedEof`]:
/// the engine exited or closed its stdout, and a caller that treated that as
/// "no reply" would silently lose the command it sent.
///
/// The reply's presence rule is checked here rather than left to the caller.
/// `CommandReply` can *parse* a document carrying two halves or none, because
/// it is a plain data type; the protocol says exactly one is present, and this
/// is the function every client goes through.
pub fn read_reply<R: BufRead>(reader: &mut R) -> io::Result<CommandReply> {
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the engine closed the stream before replying",
            ));
        }
        let text = line.trim();
        if text.is_empty() {
            continue;
        }
        let reply: CommandReply = serde_json::from_str(text).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("not a reply ({e}): {text}"),
            )
        })?;
        let present = [
            reply.diff.is_some(),
            reply.tree.is_some(),
            reply.report.is_some(),
            reply.capabilities.is_some(),
            reply.error.is_some(),
        ]
        .into_iter()
        .filter(|present| *present)
        .count();
        if present != 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "a reply must carry exactly one of diff/tree/report/capabilities/error: {text}"
                ),
            ));
        }
        return Ok(reply);
    }
}

/// A rustim process driven over its stdin/stdout.
///
/// The child's **stdout is the protocol**; its stderr is inherited, because the
/// engine routes every human-facing line there and a driver that swallowed it
/// would hide the reason a run failed.
///
/// Commands are counted the way the engine counts them: `seq` is the 1-based
/// *physical* line number, blank lines included, so [`write_raw`] (which sends a
/// line without waiting for a reply) advances that count even when the line is
/// blank. [`send_raw`] refuses a blank line instead of silently desynchronising
/// the conversation — a caller that wants the skip is asking for [`write_raw`].
///
/// [`write_raw`]: StdioClient::write_raw
/// [`send_raw`]: StdioClient::send_raw
pub struct StdioClient {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    /// Physical lines written so far, which is the `seq` the next reply must
    /// carry.
    lines: u64,
}

impl StdioClient {
    /// Spawn `program` with `args`, connecting the protocol to its stdio.
    ///
    /// The caller owns the command line: this type does not know that
    /// `--script - --reply` is what makes a rustim binary answer, and should
    /// not — another host may spell it differently.
    pub fn spawn(program: &Path, args: &[&str]) -> io::Result<Self> {
        let mut child = Process::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("the child has no stdin pipe"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("the child has no stdout pipe"))?;
        Ok(Self {
            child,
            stdin: Some(stdin),
            stdout: BufReader::new(stdout),
            lines: 0,
        })
    }

    /// Write one line and flush it, **without** reading a reply.
    ///
    /// For a blank line this is the only correct call — it is a skip, and the
    /// engine answers it with nothing. For a command it is the "fire and
    /// forget" half of the protocol, useful to a caller that wants to pipeline
    /// several commands before reading any reply.
    pub fn write_raw(&mut self, line: &str) -> io::Result<()> {
        let stdin = self.stdin.as_mut().ok_or_else(|| {
            io::Error::new(io::ErrorKind::BrokenPipe, "the client is already closed")
        })?;
        writeln!(stdin, "{line}")?;
        stdin.flush()?;
        self.lines += 1;
        Ok(())
    }

    /// Send one command line and read its reply.
    ///
    /// A blank line is refused — it has no reply, and this method's contract is
    /// one in, one out. The reply's `seq` must be the line this client just
    /// wrote; a mismatch means the two sides disagree about how many lines were
    /// sent, and continuing would attribute every later reply to the wrong
    /// command.
    pub fn send_raw(&mut self, line: &str) -> io::Result<CommandReply> {
        if line.trim().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a blank line is a skip and has no reply; use write_raw",
            ));
        }
        self.write_raw(line)?;
        let expected = self.lines;
        let reply = read_reply(&mut self.stdout)?;
        if reply.seq != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("reply seq {} does not match line {expected}", reply.seq),
            ));
        }
        Ok(reply)
    }

    /// Send a typed command and read its reply.
    ///
    /// Serialisation can fail only for a value JSON cannot carry — a non-finite
    /// float in a Rust-constructed `Command` — which is why this returns an
    /// error rather than unwrapping the way [`reply_line`] does.
    pub fn send(&mut self, command: &Command) -> io::Result<CommandReply> {
        let line = serde_json::to_string(command)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
        self.send_raw(&line)
    }

    /// The handshake: the build's capabilities, or an error explaining why not.
    ///
    /// A convenience over [`send`](Self::send) so a driver's first line is one
    /// call. A refusal (a foreign protocol version) and a reply that is not a
    /// greeting both become [`io::ErrorKind::InvalidData`] carrying the
    /// engine's own words — the caller is not in a position to do anything but
    /// report them.
    pub fn hello(&mut self) -> io::Result<Capabilities> {
        let reply = self.send(&Command::Hello {
            protocol: Some(PROTOCOL_VERSION),
        })?;
        if let Some(error) = reply.error {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("hello refused: {error}"),
            ));
        }
        reply.capabilities.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "hello was answered without capabilities",
            )
        })
    }

    /// Close the protocol stream and wait for the process.
    ///
    /// Dropping the write end is what tells a `--script -` host that the script
    /// has ended; the wait makes the exit status — and therefore a failed run —
    /// visible instead of leaving a zombie. Calling it twice is not possible:
    /// it consumes `self`.
    pub fn close(mut self) -> io::Result<ExitStatus> {
        drop(self.stdin.take());
        self.child.wait()
    }
}

impl Drop for StdioClient {
    /// A client dropped without [`close`](Self::close) still closes the stream —
    /// otherwise a test that panicked would leave the child reading stdin
    /// forever — but the exit status is not checked, because `Drop` has no way
    /// to report one. That is the whole reason `close` is a method.
    fn drop(&mut self) {
        drop(self.stdin.take());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn reply_json(seq: u64) -> String {
        format!(r#"{{"seq":{seq},"diff":{{"created":[],"modified":[],"removed":[]}}}}"#)
    }

    #[test]
    fn one_reply_is_read_from_one_line() {
        let mut input = Cursor::new(format!("{}\n", reply_json(1)));
        let reply = read_reply(&mut input).unwrap();
        assert_eq!(reply.seq, 1);
        assert!(reply.diff.is_some());
    }

    /// A blank line before a reply is tolerated rather than parsed as one. The
    /// engine never writes it; a transport that adds newlines must not turn
    /// every following reply into a parse error.
    #[test]
    fn blank_lines_before_a_reply_are_skipped() {
        let mut input = Cursor::new(format!("\n  \n{}\n", reply_json(2)));
        assert_eq!(read_reply(&mut input).unwrap().seq, 2);
    }

    #[test]
    fn eof_before_a_reply_is_an_error_not_an_empty_one() {
        let mut input = Cursor::new(String::new());
        let error = read_reply(&mut input).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn bad_json_says_what_was_read() {
        let mut input = Cursor::new("not json\n");
        let error = read_reply(&mut input).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("not json"));
    }

    /// The presence rule is enforced by the reader, not left to each caller.
    /// Both failures are the same mistake — a document that cannot be
    /// classified — and both name the offending line.
    #[test]
    fn a_reply_must_carry_exactly_one_half() {
        let mut none = Cursor::new("{\"seq\":1}\n");
        assert_eq!(
            read_reply(&mut none).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );

        let both = r#"{"seq":1,"diff":{"created":[],"modified":[],"removed":[]},"error":{"error":"unparseable","detail":"x"}}"#;
        let mut both = Cursor::new(format!("{both}\n"));
        assert_eq!(
            read_reply(&mut both).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    /// A greeting is a legal reply and reaches the caller as capabilities.
    #[test]
    fn a_greeting_is_read_as_capabilities() {
        let caps = Capabilities::current();
        let line = serde_json::to_string(&CommandReply::greeted(1, caps.clone())).unwrap();
        let mut input = Cursor::new(format!("{line}\n"));
        let reply = read_reply(&mut input).unwrap();
        assert_eq!(reply.capabilities, Some(caps));
    }

    /// A lint is the fifth half, and an empty report is a legal answer: the
    /// reader counts the *field*, not how much is in it.
    #[test]
    fn a_check_is_read_as_a_report() {
        let line =
            serde_json::to_string(&CommandReply::checked(1, CheckReport::default())).unwrap();
        let mut input = Cursor::new(format!("{line}\n"));
        let reply = read_reply(&mut input).unwrap();
        assert_eq!(reply.report, Some(CheckReport::default()));
        assert!(reply.diff.is_none() && reply.tree.is_none());
    }
}
