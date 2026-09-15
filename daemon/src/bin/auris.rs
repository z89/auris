//! `auris`: the control CLI the plugin shells out to.
//!
//! Exit codes: 0 success, 1 the daemon returned an error, 2 the daemon is not
//! reachable.

use std::{
    io::{BufRead, BufReader, IsTerminal, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::ExitCode,
    time::{Duration, Instant},
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
/// Longest wait for a setting readback. The daemon gives up at the same point,
/// so whichever expires first the answer is the same.
const VERIFY_WAIT: Duration = Duration::from_secs(10);
/// How long a blocking read may sit before the deadline is rechecked. The
/// daemon pushes a snapshot per change, so this is not a poll interval.
const VERIFY_POLL: Duration = Duration::from_millis(500);

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
    /// Apple multi-host switching: turn it on or off, or show its state.
    Handoff {
        /// on, off or status.
        action: HandoffArg,
    },
    /// Take the AirPods over from another Apple host now.
    TakeOver,
    /// Give the AirPods up to another Apple host now.
    Yield,
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
enum HandoffArg {
    On,
    Off,
    Status,
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

    let Parsed {
        request,
        wants_json,
        handoff_view,
        notice,
        verify_key,
    } = match request_from_command(cli.command) {
        Ok(parsed) => parsed,
        Err(e) => return fail(e, EXIT_DAEMON_ERROR),
    };

    let response = match talk(&path, &request) {
        Ok(r) => r,
        Err(e) => return fail(format_args!("{} ({})", e, path.display()), EXIT_UNREACHABLE),
    };

    match response {
        Response::Ack { ok: true, .. } => {
            if let Some(key) = verify_key {
                // The write is stored silently; only the daemon's readback can
                // say what the accessory actually kept.
                return await_setting(&path, &key);
            }
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
            } else if handoff_view {
                print!("{}", handoff_summary(&snap));
            } else {
                print!("{}", summary(&snap));
            }
            ExitCode::SUCCESS
        }
    }
}

/// One command, fully validated, plus what to do with a successful answer.
struct Parsed {
    request: Request,
    wants_json: bool,
    /// Print only the handoff section of a status reply.
    handoff_view: bool,
    notice: Option<&'static str>,
    /// Setting key whose readback must be waited for.
    verify_key: Option<String>,
}

impl Parsed {
    fn plain(request: Request, wants_json: bool) -> Self {
        Self {
            request,
            wants_json,
            handoff_view: false,
            notice: None,
            verify_key: None,
        }
    }
}

/// Convert CLI input into a fully validated request before opening the socket.
fn request_from_command(command: Cmd) -> Result<Parsed, String> {
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
        Cmd::Handoff {
            action: HandoffArg::Status,
        } => {
            return Ok(Parsed {
                handoff_view: true,
                ..Parsed::plain(Request::Status, false)
            });
        }
        Cmd::Handoff { action } => (
            Request::SetHandoff {
                enabled: matches!(action, HandoffArg::On),
            },
            false,
        ),
        Cmd::TakeOver => (Request::TakeOver, false),
        Cmd::Yield => (Request::Yield, false),
        Cmd::Rename { name } => {
            settings::validate_name(&name)?;
            return Ok(Parsed {
                request: Request::Rename { name },
                wants_json: false,
                handoff_view: false,
                notice: None,
                // The accessory pushes its own metadata after every handshake,
                // so the daemon's readback reopen can confirm the new name
                // without waiting for BlueZ to re-read it on some later
                // connection.
                verify_key: Some(aurisd::state::NAME_KEY.to_owned()),
            });
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
            let verify_key = setting.key().to_owned();
            return Ok(Parsed {
                request: Request::SetSetting { setting },
                wants_json: false,
                handoff_view: false,
                notice: None,
                verify_key: Some(verify_key),
            });
        }
        Cmd::Status { json } => (Request::Status, json),
    };
    Ok(Parsed::plain(parsed.0, parsed.1))
}

/// What the daemon's readback concluded about one setting write.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// A fresh dump reported exactly what was written.
    Confirmed,
    /// A fresh dump reported something else; the accessory won.
    Kept(String),
    /// The readback ran, but this model never reports the key at all.
    Unreported,
    /// No readback could be completed. The write itself still went out.
    Unverified(String),
    /// The daemon predates the readback contract.
    Legacy,
}

impl Outcome {
    /// The single line to print, and the process exit code with it.
    fn line(&self) -> (String, u8) {
        match self {
            Self::Confirmed => ("confirmed by AirPods".to_owned(), 0),
            Self::Kept(value) => (format!("AirPods kept {value}"), EXIT_DAEMON_ERROR),
            Self::Unreported => (
                "applied; AirPods 4 (ANC) never reports this setting back".to_owned(),
                0,
            ),
            Self::Unverified(reason) => (
                format!("sent but not verified: {reason}"),
                EXIT_DAEMON_ERROR,
            ),
            Self::Legacy => (
                "sent; waiting for AirPods to report the setting".to_owned(),
                0,
            ),
        }
    }
}

/// Render a reported setting value the way a person would say it.
fn reported_value(snapshot: &Snapshot, key: &str) -> String {
    if key == aurisd::state::NAME_KEY {
        return if snapshot.device.name.is_empty() {
            "a different name".to_owned()
        } else {
            snapshot.device.name.clone()
        };
    }
    let settings = serde_json::to_value(&snapshot.settings).unwrap_or_default();
    match settings.get(key) {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Null) | None => "a different value".to_owned(),
        Some(other) => other.to_string(),
    }
}

/// Why a readback could not conclude, in the words of the published state.
fn unverified_reason(snapshot: &Snapshot) -> String {
    if !snapshot.device.connected {
        "the AirPods disconnected".to_owned()
    } else if !snapshot.device.aap_link {
        "the AAP link could not be reopened".to_owned()
    } else {
        "the AirPods did not report the setting back in time".to_owned()
    }
}

/// Read one snapshot. `None` means the readback is still running.
fn outcome_for(snapshot: &Snapshot, key: &str) -> Option<Outcome> {
    if snapshot.settings_api < 2 {
        return Some(Outcome::Legacy);
    }
    match snapshot.settings_status.get(key).map(String::as_str) {
        None | Some("verifying") => None,
        Some("confirmed") => Some(Outcome::Confirmed),
        Some("mismatch") => Some(Outcome::Kept(reported_value(snapshot, key))),
        Some("unreported") => Some(Outcome::Unreported),
        Some("unverified") => Some(Outcome::Unverified(unverified_reason(snapshot))),
        Some(other) => Some(Outcome::Unverified(format!(
            "the daemon reported an unknown status {other}"
        ))),
    }
}

fn emit(outcome: &Outcome) -> ExitCode {
    let (line, code) = outcome.line();
    println!("{line}");
    if code != 0 {
        eprintln!("{line}");
    }
    ExitCode::from(code)
}

/// Watch the daemon until its readback settles this key, or time runs out.
fn await_setting(path: &Path, key: &str) -> ExitCode {
    let stream = match subscribe(path) {
        Ok(stream) => stream,
        Err(e) => {
            return emit(&Outcome::Unverified(format!(
                "could not watch the daemon: {e}"
            )));
        }
    };
    let deadline = Instant::now() + VERIFY_WAIT;
    let mut reader = BufReader::new(stream);
    let mut announced = false;
    loop {
        if Instant::now() >= deadline {
            return emit(&Outcome::Unverified(
                "the AirPods did not report the setting back in time".to_owned(),
            ));
        }
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => {
                return emit(&Outcome::Unverified(
                    "the daemon closed the subscription".to_owned(),
                ));
            }
            Ok(_) => {
                let Ok(snapshot) = serde_json::from_str::<Snapshot>(&line) else {
                    continue;
                };
                if let Some(outcome) = outcome_for(&snapshot, key) {
                    return emit(&outcome);
                }
                if !announced && std::io::stderr().is_terminal() {
                    eprintln!("verifying (reopening the AAP link for readback)…");
                    announced = true;
                }
            }
            // A read timeout is the normal quiet case: nothing changed yet.
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(e) => {
                return emit(&Outcome::Unverified(format!(
                    "lost the daemon subscription: {e}"
                )));
            }
        }
    }
}

/// Open a second connection in subscribe mode. The first one carried the
/// write; a subscriber never gets to send another request.
fn subscribe(path: &Path) -> std::io::Result<UnixStream> {
    let stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(VERIFY_POLL))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let mut line = serde_json::to_vec(&Request::Subscribe)?;
    line.push(b'\n');
    (&stream).write_all(&line)?;
    (&stream).flush()?;
    Ok(stream)
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

/// The contract spelling of a serde enum value, for display.
fn wire_name(value: impl serde::Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn handoff_summary(s: &Snapshot) -> String {
    let h = &s.handoff;
    let yes_no = |b: bool| if b { "yes" } else { "no" };
    let place = |is_local: bool| if is_local { "this host" } else { "other host" };
    let mut out = format!(
        "handoff: {} (take over on play: {})\n",
        if h.enabled { "on" } else { "off" },
        yes_no(h.take_over_on_play)
    );
    out.push_str(&format!(
        "apple device id: {}\n",
        match h.apple_host_id {
            Some(true) => "yes",
            Some(false) => "no (set DeviceID = bluetooth:004C:0000:0000 and re-pair)",
            None => "unknown",
        }
    ));
    out.push_str(&format!("owner: {}\n", wire_name(h.owner)));
    match &h.audio_source {
        Some(src) => out.push_str(&format!(
            "audio source: {} ({}) {}\n",
            src.address,
            place(src.is_local),
            wire_name(src.state)
        )),
        None => out.push_str("audio source: unknown\n"),
    }
    let devices: Vec<String> = h
        .devices
        .iter()
        .map(|d| format!("{} ({})", d.address, place(d.is_local)))
        .collect();
    out.push_str(&format!(
        "devices: {}\n",
        if devices.is_empty() {
            "none".to_owned()
        } else {
            devices.join(", ")
        }
    ));
    match &h.last_event {
        Some(ev) => out.push_str(&format!(
            "last event: {} at {}{}\n",
            wire_name(ev.kind),
            ev.at,
            ev.peer
                .as_deref()
                .map(|p| format!(" (peer {p})"))
                .unwrap_or_default()
        )),
        None => out.push_str("last event: none\n"),
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
        request_from_command(command(args)).map(|parsed| parsed.request)
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
    fn handoff_commands_parse_to_their_requests() {
        assert_eq!(
            request(&["auris", "handoff", "on"]),
            Ok(Request::SetHandoff { enabled: true })
        );
        assert_eq!(
            request(&["auris", "handoff", "off"]),
            Ok(Request::SetHandoff { enabled: false })
        );
        assert_eq!(request(&["auris", "take-over"]), Ok(Request::TakeOver));
        assert_eq!(request(&["auris", "yield"]), Ok(Request::Yield));
        let parsed = request_from_command(command(&["auris", "handoff", "status"])).unwrap();
        assert_eq!(parsed.request, Request::Status);
        assert!(parsed.handoff_view && !parsed.wants_json);
        let text = handoff_summary(&Snapshot::example());
        assert!(
            text.starts_with("handoff: on (take over on play: yes)"),
            "{text}"
        );
        assert!(text.contains("owner: other"), "{text}");
        assert!(text.contains("last event: yielded"), "{text}");
    }

    fn snapshot_with(status: &str, key: &str) -> Snapshot {
        let mut snapshot = Snapshot::initial("AC:DE:48:00:11:22");
        snapshot.device.connected = true;
        snapshot.device.aap_link = true;
        snapshot
            .settings_status
            .insert(key.to_owned(), status.to_owned());
        snapshot
    }

    #[test]
    fn a_setting_write_or_a_rename_waits_for_a_readback() {
        let parsed =
            request_from_command(command(&["auris", "setting", "press_speed", "\"slower\""]))
                .unwrap();
        assert_eq!(parsed.verify_key.as_deref(), Some("press_speed"));
        assert!(parsed.notice.is_none());

        let renamed = request_from_command(command(&["auris", "rename", "Pods"])).unwrap();
        assert_eq!(renamed.verify_key.as_deref(), Some("name"));
        assert!(renamed.notice.is_none());

        for args in [vec!["auris", "noise", "anc"], vec!["auris", "status"]] {
            assert!(request_from_command(command(&args))
                .unwrap()
                .verify_key
                .is_none());
        }
    }

    #[test]
    fn a_verifying_snapshot_is_never_an_answer() {
        assert_eq!(
            outcome_for(&snapshot_with("verifying", "press_speed"), "press_speed"),
            None
        );
        // Another key settling says nothing about this one.
        let mut snapshot = snapshot_with("verifying", "press_speed");
        snapshot
            .settings_status
            .insert("microphone".to_owned(), "confirmed".to_owned());
        assert_eq!(outcome_for(&snapshot, "press_speed"), None);
        assert_eq!(outcome_for(&Snapshot::initial(""), "press_speed"), None);
    }

    #[test]
    fn each_status_prints_one_line_with_the_right_exit_code() {
        let confirmed = outcome_for(&snapshot_with("confirmed", "press_speed"), "press_speed");
        assert_eq!(
            confirmed.unwrap().line(),
            ("confirmed by AirPods".into(), 0)
        );

        let mut mismatch = snapshot_with("mismatch", "press_speed");
        mismatch.settings.press_speed = Some(aurisd::settings::PressSpeed::Default);
        assert_eq!(
            outcome_for(&mismatch, "press_speed").unwrap().line(),
            ("AirPods kept default".to_owned(), 1)
        );

        let unreported = outcome_for(&snapshot_with("unreported", "microphone"), "microphone");
        assert_eq!(
            unreported.unwrap().line(),
            (
                "applied; AirPods 4 (ANC) never reports this setting back".to_owned(),
                0
            )
        );

        let mut unverified = snapshot_with("unverified", "press_speed");
        unverified.device.aap_link = false;
        let (line, code) = outcome_for(&unverified, "press_speed").unwrap().line();
        assert_eq!(code, 1);
        assert_eq!(
            line,
            "sent but not verified: the AAP link could not be reopened"
        );

        unverified.device.connected = false;
        let (line, _) = outcome_for(&unverified, "press_speed").unwrap().line();
        assert_eq!(line, "sent but not verified: the AirPods disconnected");
    }

    #[test]
    fn a_daemon_without_the_readback_contract_is_not_an_error() {
        let mut old = snapshot_with("confirmed", "press_speed");
        old.settings_api = 1;
        assert_eq!(
            outcome_for(&old, "press_speed").unwrap().line(),
            (
                "sent; waiting for AirPods to report the setting".to_owned(),
                0
            )
        );
    }

    #[test]
    fn a_boolean_or_list_value_is_still_one_readable_line() {
        let mut snapshot = snapshot_with("mismatch", "personalized_volume");
        snapshot.settings.personalized_volume = Some(false);
        assert_eq!(
            outcome_for(&snapshot, "personalized_volume")
                .unwrap()
                .line(),
            ("AirPods kept false".to_owned(), 1)
        );

        let mut absent = snapshot_with("mismatch", "microphone");
        absent.settings.microphone = None;
        assert_eq!(
            outcome_for(&absent, "microphone").unwrap().line(),
            ("AirPods kept a different value".to_owned(), 1)
        );
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
