//! `rustim-daemon`: the binary half — flags, the listener, and one line telling
//! an operator what it is listening on.
//!
//! Everything a client can do is in the library; everything here is about a
//! deployment: which address, which scene a new session gets, whether the
//! process links the text shaper, and which clock a session runs. Those four are
//! defaults for `attach`, not policies — a client that wants another scene or a
//! manual clock says so in its own `attach`, within what this build can offer.

use std::process::ExitCode;
use std::sync::Arc;

use rustim_daemon::session::ClockMode;
use rustim_daemon::{Hub, HubOptions, scene_names, serve};
use tokio::net::TcpListener;

const DEFAULT_LISTEN: &str = "127.0.0.1:7300";

struct Options {
    listen: String,
    scene: String,
    clock: ClockMode,
    typeset: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            listen: DEFAULT_LISTEN.to_owned(),
            scene: "blank".to_owned(),
            clock: ClockMode::Auto,
            typeset: true,
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
            "--listen" => {
                options.listen = args
                    .next()
                    .ok_or_else(|| "`--listen` needs an address".to_owned())?;
            }
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
            "--no-typeset" => options.typeset = false,
            "--help" | "-h" => return Ok(Parsed::Help),
            other => return Err(format!("unrecognised argument `{other}`")),
        }
    }
    Ok(Parsed::Options(options))
}

fn help() -> String {
    format!(
        "\
rustim-daemon — drive a rustim scene over a WebSocket

Usage: rustim-daemon [options]

Options:
  --listen ADDR   address to bind (default {DEFAULT_LISTEN})
  --scene NAME    scene for a session created without one: {}
  --clock MODE    clock for a session created without one: auto | manual
                  (default auto)
  --no-typeset    do not install the text/formula provider; `hello` then
                  omits `text` and `formula` from its shape list
  --help, -h      print this and exit

Every protocol action is a JSON-RPC method (`add`, `play`, `set`, `query`,
`events`, …) with the command's fields as `params`. Five methods are the
daemon's own: attach, detach, close, ping, advance. Events arrive as JSON-RPC
notifications with method \"event\". See 驱动协议v1.md and 使用说明.md.
",
        scene_names().join(", ")
    )
}

#[tokio::main]
async fn main() -> ExitCode {
    let options = match parse(std::env::args().skip(1)) {
        Ok(Parsed::Options(options)) => options,
        Ok(Parsed::Help) => {
            print!("{}", help());
            return ExitCode::SUCCESS;
        }
        Err(detail) => {
            eprintln!("rustim-daemon: {detail}");
            eprint!("{}", help());
            return ExitCode::from(2);
        }
    };

    let hub = match Hub::new(HubOptions {
        scene: options.scene.clone(),
        clock: options.clock,
        typeset: options.typeset,
    }) {
        Ok(hub) => Arc::new(hub),
        Err(detail) => {
            eprintln!("rustim-daemon: {detail}");
            return ExitCode::from(2);
        }
    };
    let listener = match TcpListener::bind(&options.listen).await {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("rustim-daemon: cannot bind {}: {err}", options.listen);
            return ExitCode::from(2);
        }
    };
    let address = listener
        .local_addr()
        .map(|address| address.to_string())
        .unwrap_or_else(|_| options.listen.clone());
    eprintln!("rustim-daemon: listening on ws://{address}");
    eprintln!(
        "  default scene `{}`, clock {}, typesetting {}",
        options.scene,
        options.clock.name(),
        if options.typeset { "on" } else { "off" }
    );

    tokio::select! {
        served = serve(listener, hub) => {
            if let Err(err) = served {
                eprintln!("rustim-daemon: listener stopped: {err}");
                return ExitCode::FAILURE;
            }
        }
        _ = tokio::signal::ctrl_c() => {
            // Dropping the hub drops every session handle, which ends each
            // session thread; an interrupted lesson is not saved anywhere, by
            // design (see the crate docs).
            eprintln!("rustim-daemon: shutting down");
        }
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(args: &[&str]) -> Options {
        match parse(args.iter().map(|s| (*s).to_owned())) {
            Ok(Parsed::Options(options)) => options,
            other => panic!("expected options from {args:?}, got {:?}", other.is_ok()),
        }
    }

    #[test]
    fn the_defaults_bind_loopback_and_stand_on_an_empty_stage() {
        let options = Options::default();
        assert_eq!(options.listen, "127.0.0.1:7300");
        assert_eq!(options.scene, "blank");
        assert_eq!(options.clock, ClockMode::Auto);
        assert!(options.typeset);
    }

    #[test]
    fn the_flags_override_the_defaults() {
        let options = parse_ok(&[
            "--listen",
            "0.0.0.0:9000",
            "--scene",
            "pythagorean",
            "--clock",
            "manual",
            "--no-typeset",
        ]);
        assert_eq!(options.listen, "0.0.0.0:9000");
        assert_eq!(options.scene, "pythagorean");
        assert_eq!(options.clock, ClockMode::Manual);
        assert!(!options.typeset);
    }

    #[test]
    fn a_bad_argument_is_exit_2_material() {
        for args in [
            &["--clock", "sometimes"][..],
            &["--listen"][..],
            &["--scene"][..],
            &["--nope"][..],
        ] {
            assert!(
                parse(args.iter().map(|s| (*s).to_owned())).is_err(),
                "{args:?}"
            );
        }
    }

    #[test]
    fn help_names_every_scene_the_hub_can_build() {
        let text = help();
        for scene in scene_names() {
            assert!(text.contains(scene), "{scene} missing from --help");
        }
        assert!(text.contains("attach"));
    }
}
