//! `rustim-mcp`: the binary half — flags, one embedded session, and the stdio
//! loop.
//!
//! Everything an MCP client can say is in the library ([`rustim_mcp::mcp`]);
//! this file is about the deployment: which scene the process starts, which
//! clock it runs, whether it links the text shaper, and how big the viewport is.
//! Those are the *session's* properties, and they are defaults in exactly the
//! daemon's sense — the tool surface cannot change them, because a model that
//! wants a different stage should say so with `rustim_batch`, not with a flag it
//! cannot see.
//!
//! # One session, one process, one pipe
//!
//! An MCP server is launched by its client and lives as long as that client's
//! pipe: the client initialises once and calls tools for the rest of the
//! conversation. So the session is created at startup, kept for the process's
//! lifetime, and closed when standard input reaches EOF. There is no `attach`
//! tool — the thing MCP gives you instead of one is the process itself.
//!
//! # The session thread
//!
//! `SceneContext` is not `Send`, so [`rustim_daemon::Hub`] owns it on a
//! dedicated thread and hands out an async [`SessionHandle`]. This binary embeds
//! that machinery rather than restating it: the same clock, the same request
//! queue, the same typesetter, and the same guarantee that an MCP tool call and
//! a WebSocket method end in one `SceneContext::handle`.
//!
//! Blocking stdio is driven from a current-thread runtime that does nothing but
//! wait on those channels — the session thread has the runtime that matters.

use std::io::{BufRead, Write};
use std::process::ExitCode;
use std::sync::Arc;

use rustim_daemon::AdvanceError as DaemonAdvanceError;
use rustim_daemon::{
    AttachSpec, ClockMode, CommandFailure, DEFAULT_HEIGHT, DEFAULT_WIDTH, Hub, HubOptions,
    SessionHandle,
};
use rustim_mcp::mcp::{AdvanceError, Server, Session, SessionError};
use rustim_scene::command::MotionMode;
use rustim_scene::{Command, CommandOutcome};

struct Options {
    scene: String,
    clock: ClockMode,
    typeset: bool,
    width: u16,
    height: u16,
    motion: MotionMode,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            scene: "blank".to_owned(),
            clock: ClockMode::Auto,
            typeset: true,
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
            motion: MotionMode::Full,
        }
    }
}

enum Parsed {
    Options(Options),
    Help,
}

fn parse(args: impl Iterator<Item = String>) -> Result<Parsed, String> {
    let mut options = Options::default();
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--scene" => {
                options.scene = args
                    .next()
                    .ok_or_else(|| "`--scene` needs a scene name".to_owned())?;
            }
            "--clock" => {
                options.clock = match args
                    .next()
                    .ok_or_else(|| "`--clock` needs `auto` or `manual`".to_owned())?
                    .as_str()
                {
                    "auto" => ClockMode::Auto,
                    "manual" => ClockMode::Manual,
                    other => {
                        return Err(format!("`--clock {other}`: expected `auto` or `manual`"));
                    }
                };
            }
            "--motion" => {
                options.motion = match args
                    .next()
                    .ok_or_else(|| "`--motion` needs `full` or `reduced`".to_owned())?
                    .as_str()
                {
                    "full" => MotionMode::Full,
                    "reduced" => MotionMode::Reduced,
                    other => {
                        return Err(format!("`--motion {other}`: expected `full` or `reduced`"));
                    }
                };
            }
            "--width" => options.width = dimension(&mut args, "--width")?,
            "--height" => options.height = dimension(&mut args, "--height")?,
            "--no-typeset" => options.typeset = false,
            "--help" | "-h" => return Ok(Parsed::Help),
            other => return Err(format!("unrecognised argument `{other}`")),
        }
    }
    Ok(Parsed::Options(options))
}

fn dimension(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<u16, String> {
    let value = args
        .next()
        .ok_or_else(|| format!("`{flag}` needs a pixel count"))?;
    match value.parse::<u16>() {
        Ok(0) | Err(_) => Err(format!(
            "`{flag} {value}`: expected a positive 16-bit pixel count"
        )),
        Ok(pixels) => Ok(pixels),
    }
}

fn help() -> String {
    format!(
        "\
rustim-mcp — drive a rustim scene through the Model Context Protocol

Usage: rustim-mcp [options]

Speaks newline-delimited JSON-RPC 2.0 on stdin/stdout; every line is one MCP
message, and stdout carries nothing else. An MCP client launches this binary
and talks to it for the life of the pipe.

Options:
  --scene NAME    scene the session starts with: {} (default blank)
  --clock MODE    auto | manual (default auto). A manual clock moves only when
                  `rustim_advance` says how far; an auto clock advances in
                  real time, so queued animations keep playing.
  --motion MODE   full | reduced (default full). Reduced writes each beat's end
                  state instead of interpolating it.
  --width N       viewport width in pixels (default {DEFAULT_WIDTH})
  --height N      viewport height in pixels (default {DEFAULT_HEIGHT})
  --no-typeset    do not install the text/formula provider; `rustim_capabilities`
                  then reports no `text` and no `formula` shape
  --help, -h      print this and exit

Tools: rustim_capabilities, rustim_batch, rustim_validate, rustim_query,
rustim_advance. Every command document is a 驱动协议v1 command and is passed
through untouched; see 驱动协议v1.md and 使用说明.md.
",
        rustim_daemon::scene_names().join(", ")
    )
}

/// The embedded session, behind the trait the envelope knows.
struct HubSession {
    runtime: tokio::runtime::Runtime,
    handle: Arc<SessionHandle>,
}

impl Session for HubSession {
    fn command(&mut self, command: Command) -> Result<CommandOutcome, SessionError> {
        let reply = self.runtime.block_on(self.handle.command(command));
        reply.map_err(|failure| match failure {
            CommandFailure::Closed => SessionError::Closed,
            CommandFailure::Refused(error) => SessionError::Refused(error),
        })
    }

    fn advance(&mut self, seconds: f64) -> Result<f64, AdvanceError> {
        self.runtime
            .block_on(self.handle.advance(seconds))
            .map_err(|error| match error {
                DaemonAdvanceError::Closed => AdvanceError::Closed,
                DaemonAdvanceError::NotManual => AdvanceError::NotManual,
                DaemonAdvanceError::Range(detail) => AdvanceError::Range(detail),
            })?;
        // The session updates its clock *before* it answers the advance, so a
        // snapshot taken now is the clock this call reached — reading it after
        // the reply is what the contract promises, not a race.
        Ok(self.handle.clock_now())
    }
}

fn main() -> ExitCode {
    let options = match parse(std::env::args().skip(1)) {
        Ok(Parsed::Options(options)) => options,
        Ok(Parsed::Help) => {
            print!("{}", help());
            return ExitCode::SUCCESS;
        }
        Err(detail) => {
            eprintln!("rustim-mcp: {detail}");
            eprint!("{}", help());
            return ExitCode::from(2);
        }
    };

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("rustim-mcp: could not start the session runtime: {error}");
            return ExitCode::from(2);
        }
    };
    let handle = match runtime.block_on(attach(&options)) {
        Ok(handle) => handle,
        Err(detail) => {
            eprintln!("rustim-mcp: {detail}");
            return ExitCode::from(2);
        }
    };
    eprintln!(
        "rustim-mcp: scene `{}`, clock {}, {}x{}, typesetting {}; speaking MCP {} on stdio",
        handle.scene(),
        handle.clock().name(),
        handle.width(),
        handle.height(),
        if options.typeset { "on" } else { "off" },
        rustim_mcp::LATEST_PROTOCOL_VERSION,
    );

    let mut server = Server::new(HubSession {
        runtime,
        handle: handle.clone(),
    });
    serve_stdio(&mut server);
    exit_reason(&handle)
}

async fn attach(options: &Options) -> Result<Arc<SessionHandle>, String> {
    let hub = Hub::new(HubOptions {
        scene: options.scene.clone(),
        clock: options.clock,
        typeset: options.typeset,
    })?;
    let spec = AttachSpec {
        session: None,
        scene: Some(options.scene.clone()),
        clock: Some(options.clock),
        width: Some(options.width),
        height: Some(options.height),
        motion: Some(options.motion),
    };
    hub.attach(spec)
        .await
        .map(|(handle, _)| handle)
        .map_err(|error| match error {
            rustim_daemon::AttachError::Invalid(detail)
            | rustim_daemon::AttachError::NoSuchSession(detail)
            | rustim_daemon::AttachError::Internal(detail) => detail,
        })
}

/// The transport loop: one message in, at most one message out.
fn serve_stdio(server: &mut Server<HubSession>) {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else {
            break;
        };
        let Some(reply) = server.handle_line(&line) else {
            continue;
        };
        // A write error means the client's pipe is gone; there is no one left
        // to tell, so the loop ends the way EOF does.
        if writeln!(stdout, "{reply}").is_err() || stdout.flush().is_err() {
            break;
        }
    }
}

/// How the run ended, after the pipe closed.
///
/// EOF is how an MCP client stops a server — it closes the pipe, and the
/// process exits. A session that died before the pipe did is reported, because
/// that is a fact an operator needs and the client cannot see it: its next call
/// would just be answered by a closed channel.
fn exit_reason(handle: &SessionHandle) -> ExitCode {
    if handle.alive() {
        ExitCode::SUCCESS
    } else {
        eprintln!("rustim-mcp: the session thread ended before stdin closed");
        ExitCode::from(1)
    }
}
