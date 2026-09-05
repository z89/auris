//! `auris`: the control CLI the plugin shells out to.
//!
//! Exit codes: 0 success, 1 the daemon returned an error, 2 the daemon is not
//! reachable.

use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    process::ExitCode,
    time::Duration,
};

use aurisd::{
    config,
    ctl_proto::{Request, Response},
    state::{Cell, NoiseControl, NoiseControlMode, Snapshot, Source},
};
use clap::{Parser, Subcommand, ValueEnum};

/// Daemon returned `{"ok":false,...}`.
const EXIT_DAEMON_ERROR: u8 = 1;
/// Could not talk to the daemon at all.
const EXIT_UNREACHABLE: u8 = 2;
/// The daemon answers in microseconds; this only guards against a wedged one.
const IO_TIMEOUT: Duration = Duration::from_secs(5);

/// Control the aurisd daemon.
#[derive(Parser, Debug)]
#[command(version, about = "control aurisd: noise modes, conversational awareness, status", long_about = None)]
struct Cli {
    /// Override the runtime directory (default `$XDG_RUNTIME_DIR/aurisd`).
    #[arg(long, global = true, value_name = "PATH")]
    runtime_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Set the noise control mode.
    Noise {
        /// One of anc, transparency, adaptive, off.
        mode: NoiseArg,
    },
    /// Turn conversational awareness on or off.
    Ca {
        /// on or off.
        state: OnOff,
    },
    /// Set the adaptive transparency level.
    Adaptive {
        /// 0-100.
        #[arg(value_parser = clap::value_parser!(u8).range(0..=100))]
        level: u8,
    },
    /// Drop and re-establish the AAP link.
    Reconnect,
    /// Show the current state.
    Status {
        /// Print the raw state.json object instead of a summary.
        #[arg(long)]
        json: bool,
    },
}

#[derive(ValueEnum, Clone, Copy, Debug)]
enum NoiseArg {
    Anc,
    Transparency,
    Adaptive,
    Off,
}

impl From<NoiseArg> for NoiseControlMode {
    fn from(a: NoiseArg) -> Self {
        match a {
            NoiseArg::Anc => Self::Anc,
            NoiseArg::Transparency => Self::Transparency,
            NoiseArg::Adaptive => Self::Adaptive,
            NoiseArg::Off => Self::Off,
        }
    }
}

#[derive(ValueEnum, Clone, Copy, Debug)]
enum OnOff {
    On,
    Off,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let dir = config::runtime_dir(cli.runtime_dir.as_deref());
    let path = config::socket_path(&dir);

    let (request, wants_json) = match cli.command {
        Cmd::Noise { mode } => (Request::SetNoiseControl { value: mode.into() }, false),
        Cmd::Ca { state } => (
            Request::SetConversationalAwareness {
                value: matches!(state, OnOff::On),
            },
            false,
        ),
        Cmd::Adaptive { level } => (Request::SetAdaptiveLevel { value: level }, false),
        Cmd::Reconnect => (Request::Reconnect, false),
        Cmd::Status { json } => (Request::Status, json),
    };

    let response = match talk(&path, &request) {
        Ok(r) => r,
        Err(e) => return fail(format_args!("{} ({})", e, path.display()), EXIT_UNREACHABLE),
    };

    match response {
        Response::Ack { ok: true, .. } => ExitCode::SUCCESS,
        Response::Ack { ok: false, error } => fail(
            error.as_deref().unwrap_or("command failed"),
            EXIT_DAEMON_ERROR,
        ),
        Response::Status(snap) => {
            if wants_json {
                match serde_json::to_string_pretty(&*snap) {
                    Ok(s) => println!("{s}"),
                    Err(e) => {
                        return fail(
                            format_args!("could not render state: {e}"),
                            EXIT_DAEMON_ERROR,
                        );
                    }
                }
            } else {
                print!("{}", summary(&snap));
            }
            ExitCode::SUCCESS
        }
    }
}

/// Report a failure on both streams and return its exit code.
///
/// The DMS plugin runs this CLI through Quickshell's `Proc`, which hands only
/// stdout to the QML callback; an error written to stderr alone would surface
/// as an empty toast. One line, both streams, exit code unchanged.
fn fail(msg: impl std::fmt::Display, code: u8) -> ExitCode {
    let text = msg.to_string();
    let line = text.replace('\n', " ");
    let line = line.trim();
    println!("auris: {line}");
    eprintln!("auris: {line}");
    ExitCode::from(code)
}

/// Send one request, read one reply.
fn talk(path: &std::path::Path, request: &Request) -> std::io::Result<Response> {
    let stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;

    let mut line = serde_json::to_vec(request)?;
    line.push(b'\n');
    (&stream).write_all(&line)?;
    (&stream).flush()?;

    let mut reply = String::new();
    let n = BufReader::new(&stream).read_line(&mut reply)?;
    if n == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "daemon closed the connection without replying",
        ));
    }
    Ok(serde_json::from_str(&reply)?)
}

fn cell(label: &str, c: &Cell) -> String {
    match (c.present, c.level) {
        (true, Some(level)) => {
            format!(
                "{label} {level}%{}",
                if c.charging { " (charging)" } else { "" }
            )
        }
        _ => format!("{label} --"),
    }
}

fn noise_label(n: NoiseControl) -> &'static str {
    match n {
        NoiseControl::Off => "off",
        NoiseControl::Anc => "anc",
        NoiseControl::Transparency => "transparency",
        NoiseControl::Adaptive => "adaptive",
        NoiseControl::Unknown => "unknown",
    }
}

fn summary(s: &Snapshot) -> String {
    let name = if s.device.name.is_empty() {
        "AirPods"
    } else {
        s.device.name.as_str()
    };
    let model = s.device.model.as_deref().unwrap_or("unknown model");
    let addr = if s.device.address.is_empty() {
        "no device"
    } else {
        s.device.address.as_str()
    };

    let link = if s.device.connected {
        if s.device.aap_link {
            "connected"
        } else {
            "connected (no AAP link)"
        }
    } else {
        "disconnected"
    };

    let mut out = format!("{name} [{model}] {addr}: {link}\n");
    out.push_str(&format!(
        "battery: {}  {}  {}{}\n",
        cell("L", &s.battery.left),
        cell("R", &s.battery.right),
        cell("case", &s.battery.case),
        if s.battery.stale {
            "  (last known)"
        } else {
            ""
        },
    ));
    out.push_str(&format!(
        "noise: {}   source: {}\n",
        noise_label(s.noise_control),
        match s.daemon.source {
            Source::Aap => "aap",
            Source::Ble => "ble",
            Source::None => "none",
        }
    ));
    out
}
