//! Exercise the real CLI against a private mock socket, never a live daemon.

use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixListener,
    path::PathBuf,
    process::{Command, Output},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use serde_json::{json, Value};

static SEQUENCE: AtomicUsize = AtomicUsize::new(0);

struct Runtime(PathBuf);

impl Runtime {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "auris-cli-settings-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn cli(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_auris"))
            .arg("--runtime-dir")
            .arg(&self.0)
            .args(args)
            .output()
            .unwrap()
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        // Only this test's explicitly created private directory is removed.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Serve one command connection, plus any follow-up `subscribe` connection the
/// CLI opens to watch a settings readback. Returns the command request.
fn exchange(args: &[&str], reply: Value) -> (Output, Value) {
    exchange_watching(args, reply, &[]).0
}

/// As [`exchange`], and feed `snapshots` to the subscriber, one line each.
/// The second return value is how many subscribe connections were served.
fn exchange_watching(args: &[&str], reply: Value, snapshots: &[Value]) -> ((Output, Value), usize) {
    let runtime = Runtime::new();
    let listener = UnixListener::bind(runtime.0.join("ctl.sock")).unwrap();
    listener.set_nonblocking(true).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let snapshots: Vec<Value> = snapshots.to_vec();
    let server = {
        let stop = Arc::clone(&stop);
        thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(8);
            let mut request: Option<Value> = None;
            let mut subscriptions = 0usize;
            while !stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(3)))
                            .unwrap();
                        let mut wire = String::new();
                        BufReader::new(&stream).read_line(&mut wire).unwrap();
                        assert!(wire.ends_with('\n'));
                        let asked: Value = serde_json::from_str(&wire).unwrap();
                        if asked["cmd"] == "subscribe" {
                            subscriptions += 1;
                            for snapshot in &snapshots {
                                writeln!(stream, "{snapshot}").unwrap();
                            }
                        } else {
                            writeln!(stream, "{reply}").unwrap();
                            request.get_or_insert(asked);
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "CLI did not connect to mock");
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(e) => panic!("accept failed: {e}"),
                }
            }
            (request, subscriptions)
        })
    };
    let output = runtime.cli(args);
    stop.store(true, Ordering::Relaxed);
    // Unblock the accept loop's sleep-poll with one throwaway connection.
    let _ = std::os::unix::net::UnixStream::connect(runtime.0.join("ctl.sock"));
    let (request, subscriptions) = server.join().unwrap();
    (
        (output, request.expect("no command request was served")),
        subscriptions,
    )
}

/// A published snapshot whose readback has settled `key` as `status`.
fn snapshot_with(key: &str, status: &str, reported: Value) -> Value {
    let mut snapshot = serde_json::to_value(aurisd::state::Snapshot::example()).unwrap();
    snapshot["settings_status"] = json!({ key: status });
    snapshot["settings_requested"] = json!({ key: reported });
    snapshot["settings_verify"] = json!("idle");
    snapshot
}

#[test]
fn real_cli_transmits_typed_settings_without_changing_existing_wire_commands() {
    for (key, value) in [
        ("microphone", json!("left")),
        ("press_speed", json!("slower")),
        ("hold_duration", json!("shortest")),
        ("listening_mode_cycle", json!(["anc", "transparency"])),
        ("call_controls", json!("mute_once_hangup_twice")),
        ("personalized_volume", json!(true)),
    ] {
        let input = value.to_string();
        let confirmed = snapshot_with(key, "confirmed", value.clone());
        let ((output, request), subscriptions) =
            exchange_watching(&["setting", key, &input], json!({"ok": true}), &[confirmed]);
        assert!(output.status.success(), "{output:?}");
        assert_eq!(subscriptions, 1, "the CLI must watch its own write");
        assert!(String::from_utf8_lossy(&output.stdout).contains("confirmed by AirPods"));
        assert_eq!(
            request,
            json!({"cmd": "set_setting", "key": key, "value": value})
        );
    }
    let (output, request) = exchange(&["noise", "anc"], json!({"ok": true}));
    assert!(output.status.success(), "{output:?}");
    assert_eq!(request, json!({"cmd": "set_noise_control", "value": "anc"}));
}

#[test]
fn a_setting_the_accessory_overrode_is_reported_as_a_failure() {
    let mut mismatch = snapshot_with("press_speed", "mismatch", json!("slowest"));
    mismatch["settings"]["press_speed"] = json!("default");
    let ((output, _), _) = exchange_watching(
        &["setting", "press_speed", "\"slowest\""],
        json!({"ok": true}),
        &[
            snapshot_with("press_speed", "verifying", json!("slowest")),
            mismatch,
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stdout).contains("AirPods kept default"));
}

#[test]
fn a_setting_this_model_never_reports_is_still_a_success() {
    let ((output, _), _) = exchange_watching(
        &["setting", "microphone", "\"left\""],
        json!({"ok": true}),
        &[snapshot_with("microphone", "unreported", json!("left"))],
    );
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("never reports this setting back"));
}

#[test]
fn a_readback_that_never_ran_does_not_claim_success() {
    let mut unverified = snapshot_with("press_speed", "unverified", json!("slower"));
    unverified["device"]["aap_link"] = json!(false);
    let ((output, _), _) = exchange_watching(
        &["setting", "press_speed", "\"slower\""],
        json!({"ok": true}),
        &[unverified],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stdout).contains("sent but not verified"));
}

#[test]
fn real_cli_preserves_unicode_and_shell_characters_in_name_as_data() {
    let name = "Ada's 🎧 $(whoami) `date`";
    let mut confirmed = snapshot_with("name", "confirmed", json!(name));
    confirmed["device"]["name"] = json!(name);
    let ((output, request), subscriptions) =
        exchange_watching(&["rename", name], json!({"ok": true}), &[confirmed]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(subscriptions, 1, "the CLI must watch its own rename");
    assert!(String::from_utf8_lossy(&output.stdout).contains("confirmed by AirPods"));
    assert_eq!(request, json!({"cmd": "rename", "name": name}));
}

#[test]
fn a_rename_the_accessory_did_not_take_names_what_it_kept() {
    let mut mismatch = snapshot_with("name", "mismatch", json!("Auris Pods"));
    mismatch["device"]["name"] = json!("Airpods00000000");
    let ((output, _), _) = exchange_watching(
        &["rename", "Auris Pods"],
        json!({"ok": true}),
        &[
            snapshot_with("name", "verifying", json!("Auris Pods")),
            mismatch,
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stdout).contains("AirPods kept Airpods00000000"));
}

#[test]
fn real_cli_surfaces_daemon_rejection_without_claiming_success() {
    let (output, _) = exchange(
        &["setting", "microphone", "\"left\""],
        json!({"ok": false, "error": "device is not connected: no AAP link is open"}),
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stdout).contains("no AAP link"));
}

#[test]
fn real_cli_can_read_an_old_schema_one_snapshot() {
    let mut old = serde_json::to_value(aurisd::state::Snapshot::example()).unwrap();
    for field in [
        "settings",
        "settings_api",
        "settings_requested",
        "settings_status",
        "settings_verify",
        "verify_reopen",
    ] {
        old.as_object_mut().unwrap().remove(field);
    }
    let (output, request) = exchange(&["status", "--json"], old);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(request, json!({"cmd": "status"}));
    let state: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(state["schema"], 2);
    assert_eq!(state["settings_api"], 0);
    assert!(state["settings"]["microphone"].is_null());
}

#[test]
fn invalid_settings_are_rejected_before_opening_any_socket() {
    let runtime = Runtime::new();
    for args in [
        vec!["rename", "   "],
        vec!["rename", "line\nbreak"],
        vec!["setting", "microphone", "\"speaker\""],
        vec!["setting", "personalized_volume", "\"true\""],
        vec!["setting", "listening_mode_cycle", "[\"anc\"]"],
        vec!["setting", "listening_mode_cycle", "[\"anc\",\"anc\"]"],
        vec!["setting", "microphone", "not-json"],
        vec!["setting", "invented", "true"],
    ] {
        let output = runtime.cli(&args);
        assert!(
            !output.status.success(),
            "invalid arguments accepted: {args:?}"
        );
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !text.contains("ctl.sock"),
            "invalid input attempted a socket connection: {args:?}: {text}"
        );
    }
}

#[tokio::test]
async fn raw_socket_requests_cannot_bypass_supervisor_validation() {
    use aurisd::{
        aap::session::{SessionConfig, Supervisor},
        config::PrimaryBud,
        ctl_server,
        state::Snapshot,
        store::Store,
    };
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    // No BlueZ watcher, link events, or AAP socket exists in this test. Valid
    // input therefore reaches the missing-link gate; invalid input must fail
    // validation earlier, even when the client bypasses the CLI entirely.
    let store = Store::new(Snapshot::example(), PrimaryBud::Auto);
    let before = store.snapshot();
    let (_link_tx, link_rx) = tokio::sync::mpsc::channel(4);
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel(4);
    let supervisor = Supervisor::new(store.clone(), SessionConfig::default(), link_rx, cmd_rx);
    let supervisor_task = tokio::spawn(supervisor.run());
    let (client, server) = tokio::net::UnixStream::pair().unwrap();
    let server_task = tokio::spawn(ctl_server::handle(server, store.clone(), cmd_tx));
    let (read, mut write) = client.into_split();
    let mut reader = tokio::io::BufReader::new(read);

    for (request, expected) in [
        (
            json!({"cmd":"set_setting","key":"listening_mode_cycle","value":["anc"]}),
            "between 2 and 4",
        ),
        (
            json!({"cmd":"set_setting","key":"listening_mode_cycle","value":["anc","anc"]}),
            "duplicate",
        ),
        (json!({"cmd":"rename","name":"  "}), "blank"),
        (json!({"cmd":"rename","name":"bad\nname"}), "control"),
        (json!({"cmd":"rename","name":"x".repeat(256)}), "255"),
        (
            json!({"cmd":"set_setting","key":"microphone","value":"left"}),
            "no AAP link",
        ),
    ] {
        write
            .write_all(format!("{request}\n").as_bytes())
            .await
            .unwrap();
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(2), reader.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["ok"], false, "{reply}");
        assert!(
            reply["error"].as_str().unwrap().contains(expected),
            "{reply}"
        );
        assert_eq!(
            store.snapshot(),
            before,
            "rejected write changed confirmed state"
        );
    }

    server_task.abort();
    supervisor_task.abort();
    let _ = server_task.await;
    let _ = supervisor_task.await;
}

#[test]
fn real_cli_sends_handoff_requests() {
    for (args, wire) in [
        (
            &["handoff", "on"][..],
            json!({"cmd": "set_handoff", "enabled": true}),
        ),
        (
            &["handoff", "off"][..],
            json!({"cmd": "set_handoff", "enabled": false}),
        ),
        (&["take-over"][..], json!({"cmd": "take_over"})),
        (&["yield"][..], json!({"cmd": "yield"})),
    ] {
        let (output, request) = exchange(args, json!({"ok": true}));
        assert!(output.status.success(), "{args:?}: {output:?}");
        assert_eq!(request, wire);
    }

    let snapshot = serde_json::to_value(aurisd::state::Snapshot::example()).unwrap();
    let (output, request) = exchange(&["handoff", "status"], snapshot);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(request, json!({"cmd": "status"}));
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("handoff: on"), "{text}");
    assert!(text.contains("owner: other"), "{text}");

    let (output, _) = exchange(
        &["take-over"],
        json!({"ok": false, "error": "device is not connected: no AAP link is open"}),
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("no AAP link"));
}

#[tokio::test]
async fn raw_socket_handoff_requests_reach_the_supervisor() {
    use aurisd::{
        aap::session::{HandoffOptions, SessionConfig, Supervisor},
        config::{Config, PrimaryBud},
        ctl_server,
        state::Snapshot,
        store::Store,
    };
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    let runtime = Runtime::new();
    let config_path = runtime.0.join("aurisd").join("config.toml");
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(&config_path, "# mine\nprimary_bud = \"right\"\n").unwrap();

    // No AAP link and no BlueZ: manual handoff must refuse, and the default
    // options forbid profile calls and MPRIS entirely.
    let store = Store::new(Snapshot::initial(""), PrimaryBud::Auto);
    let (_link_tx, link_rx) = tokio::sync::mpsc::channel(4);
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel(4);
    let supervisor = Supervisor::new(store.clone(), SessionConfig::default(), link_rx, cmd_rx)
        .with_handoff(HandoffOptions {
            config_path: Some(config_path.clone()),
            ..HandoffOptions::default()
        });
    let supervisor_task = tokio::spawn(supervisor.run());
    let (client, server) = tokio::net::UnixStream::pair().unwrap();
    let server_task = tokio::spawn(ctl_server::handle(server, store.clone(), cmd_tx));
    let (read, mut write) = client.into_split();
    let mut reader = tokio::io::BufReader::new(read);

    let mut ask = async |request: Value| -> Value {
        write
            .write_all(format!("{request}\n").as_bytes())
            .await
            .unwrap();
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(2), reader.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        serde_json::from_str(&line).unwrap()
    };

    for cmd in ["take_over", "yield"] {
        let reply = ask(json!({"cmd": cmd})).await;
        assert_eq!(reply["ok"], false, "{reply}");
        assert!(
            reply["error"].as_str().unwrap().contains("no AAP link"),
            "{reply}"
        );
    }
    assert!(!store.snapshot().handoff.enabled);

    let reply = ask(json!({"cmd": "set_handoff", "enabled": true})).await;
    assert_eq!(reply, json!({"ok": true}));
    assert!(store.snapshot().handoff.enabled);
    let text = std::fs::read_to_string(&config_path).unwrap();
    assert!(text.contains("# mine"), "{text}");
    let cfg: Config = toml::from_str(&text).unwrap();
    assert_eq!(cfg.primary_bud, PrimaryBud::Right);
    assert!(cfg.handoff.enabled);

    let status = ask(json!({"cmd": "status"})).await;
    assert_eq!(status["handoff"]["enabled"], true, "{status}");
    assert_eq!(status["handoff"]["owner"], "unknown", "{status}");

    let reply = ask(json!({"cmd": "set_handoff", "enabled": false})).await;
    assert_eq!(reply, json!({"ok": true}));
    assert!(!store.snapshot().handoff.enabled);

    let reply = ask(json!({"cmd": "set_handoff", "enabled": "yes"})).await;
    assert_eq!(reply["ok"], false, "{reply}");

    server_task.abort();
    supervisor_task.abort();
    let _ = server_task.await;
    let _ = supervisor_task.await;
}
