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
    settings::{self, SettingCommand},
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
#[command(
    version,
    about = "control aurisd: AirPods status and settings",
    long_about = None,
    after_help = "Settings use JSON values, for example:\n  auris setting microphone '\"left\"'\n  auris setting personalized_volume true\n  auris setting listening_mode_cycle '[\"anc\",\"transparency\"]'\n\nconnect-once is explicit and can transfer audio away from another host; it is never triggered automatically."
)]
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
    /// Ask BlueZ once to connect the pinned paired device. This may transfer
    /// audio from another host; it never retries automatically.
    ConnectOnce,
    /// Rename the AirPods accessory.
    Rename {
        /// New accessory name (quote names containing spaces).
        name: String,
    },
    /// Set a typed AirPods setting using a JSON value.
    Setting {
        /// Setting key, such as microphone or personalized_volume.
        key: String,
        /// JSON string, boolean, or array appropriate for the key.
        #[arg(value_name = "JSON_VALUE")]
        value: String,
    },
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

    let (request, wants_json, notice) = match request_from_command(cli.command) {
        Ok(parsed) => parsed,
        Err(e) => return fail(e, EXIT_DAEMON_ERROR),
    };

    let response = match talk(&path, &request) {
        Ok(r) => r,
        Err(e) => return fail(format_args!("{} ({})", e, path.display()), EXIT_UNREACHABLE),
    };

    match response {
        Response::Ack { ok: true, .. } => {
            if let Some(message) = notice {
                println!("{message}");
            }
            ExitCode::SUCCESS
        }
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

/// Convert CLI input into a fully validated request before opening the socket.
fn request_from_command(command: Cmd) -> Result<(Request, bool, Option<&'static str>), String> {
    let parsed = match command {
        Cmd::Noise { mode } => (Request::SetNoiseControl { value: mode.into() }, false),
        Cmd::Ca { state } => (
            Request::SetConversationalAwareness {
                value: matches!(state, OnOff::On),
            },
            false,
        ),
        Cmd::Adaptive { level } => (Request::SetAdaptiveLevel { value: level }, false),
        Cmd::Reconnect => (Request::Reconnect, false),
        Cmd::ConnectOnce => (Request::ConnectOnce, false),
        Cmd::Rename { name } => {
            settings::validate_name(&name)?;
            return Ok((
                Request::Rename { name },
                false,
                Some("sent; AirPods metadata may not refresh until reconnect"),
            ));
        }
        Cmd::Setting { key, value } => {
            let value: serde_json::Value = serde_json::from_str(&value)
                .map_err(|e| format!("invalid JSON value for {key}: {e}"))?;
            let setting: SettingCommand = serde_json::from_value(serde_json::json!({
                "key": key,
                "value": value,
            }))
            .map_err(|e| format!("invalid setting: {e}"))?;
            setting.validate()?;
            return Ok((
                Request::SetSetting { setting },
                false,
                Some("sent; waiting for AirPods to report the setting"),
            ));
        }
        Cmd::Status { json } => (Request::Status, json),
    };
    Ok((parsed.0, parsed.1, None))
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
    if s.settings_api == 0 {
        out.push_str("settings: unavailable (older daemon)\n");
    } else {
        let settings = serde_json::to_value(&s.settings).unwrap_or_default();
        let object = settings.as_object();
        out.push_str("settings:");
        for key in [
            "microphone",
            "press_speed",
            "hold_duration",
            "listening_mode_cycle",
            "call_controls",
            "personalized_volume",
        ] {
            let value = object
                .and_then(|values| values.get(key))
                .filter(|value| !value.is_null())
                .map(ToString::to_string)
                .unwrap_or_else(|| "unknown".to_owned());
            out.push_str(&format!("\n  {key}: {value}"));
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(args: &[&str]) -> Cmd {
        Cli::try_parse_from(args).unwrap().command
    }

    fn request(args: &[&str]) -> Result<Request, String> {
        request_from_command(command(args)).map(|(request, _, _)| request)
    }

    #[test]
    fn rename_validates_utf8_bytes_and_controls_before_transport() {
        let at_limit = format!("{}a", "é".repeat(127));
        assert!(matches!(
            request(&["auris", "rename", &at_limit]),
            Ok(Request::Rename { name }) if name == at_limit
        ));

        let too_long = "é".repeat(128);
        assert!(request(&["auris", "rename", &too_long]).is_err());
        assert!(request(&["auris", "rename", "   "]).is_err());
        assert!(request(&["auris", "rename", "bad\nname"]).is_err());

        let shell_like = "$(touch /tmp/auris-must-not-run)";
        assert!(matches!(
            request(&["auris", "rename", shell_like]),
            Ok(Request::Rename { name }) if name == shell_like
        ));
    }

    #[test]
    fn setting_accepts_typed_json_values() {
        for args in [
            ["auris", "setting", "microphone", "\"left\""],
            ["auris", "setting", "press_speed", "\"slower\""],
            ["auris", "setting", "hold_duration", "\"shortest\""],
            [
                "auris",
                "setting",
                "call_controls",
                "\"mute_once_hangup_twice\"",
            ],
            ["auris", "setting", "personalized_volume", "true"],
            [
                "auris",
                "setting",
                "listening_mode_cycle",
                "[\"anc\",\"transparency\"]",
            ],
        ] {
            assert!(matches!(request(&args), Ok(Request::SetSetting { .. })));
        }
    }

    #[test]
    fn setting_rejects_malformed_or_wrong_json_types() {
        for args in [
            ["auris", "setting", "microphone", "left"],
            ["auris", "setting", "microphone", "\"middle\""],
            ["auris", "setting", "personalized_volume", "\"true\""],
            ["auris", "setting", "listening_mode_cycle", "\"anc\""],
            ["auris", "setting", "listening_mode_cycle", "[]"],
            [
                "auris",
                "setting",
                "listening_mode_cycle",
                "[\"anc\",\"anc\"]",
            ],
            ["auris", "setting", "not_a_setting", "true"],
        ] {
            assert!(request(&args).is_err(), "accepted {args:?}");
        }
    }

    #[test]
    fn existing_commands_still_parse() {
        assert!(matches!(
            request(&["auris", "noise", "anc"]),
            Ok(Request::SetNoiseControl {
                value: NoiseControlMode::Anc
            })
        ));
        assert!(matches!(request(&["auris", "status"]), Ok(Request::Status)));
        assert!(matches!(
            request(&["auris", "connect-once"]),
            Ok(Request::ConnectOnce)
        ));
    }

    #[test]
    fn setting_request_uses_the_flattened_wire_shape() {
        let request = request(&["auris", "setting", "microphone", "\"right\""]).unwrap();
        assert_eq!(
            serde_json::to_string(&request).unwrap(),
            r#"{"cmd":"set_setting","key":"microphone","value":"right"}"#
        );
    }
}
