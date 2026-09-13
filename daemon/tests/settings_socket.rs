//! Exercise the real CLI against a private mock socket, never a live daemon.

use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixListener,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicUsize, Ordering},
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

fn exchange(args: &[&str], reply: Value) -> (Output, Value) {
    let runtime = Runtime::new();
    let listener = UnixListener::bind(runtime.0.join("ctl.sock")).unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "CLI did not connect to mock");
                    thread::sleep(Duration::from_millis(10));
                }
                Err(e) => panic!("accept failed: {e}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut wire = String::new();
        BufReader::new(&stream).read_line(&mut wire).unwrap();
        assert!(wire.ends_with('\n'));
        let request: Value = serde_json::from_str(&wire).unwrap();
        writeln!(stream, "{reply}").unwrap();
        request
    });
    let output = runtime.cli(args);
    let request = server.join().unwrap();
    (output, request)
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
        let (output, request) = exchange(&["setting", key, &input], json!({"ok": true}));
        assert!(output.status.success(), "{output:?}");
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
fn real_cli_preserves_unicode_and_shell_characters_in_name_as_data() {
    let name = "Archie's 🎧 $(whoami) `date`";
    let (output, request) = exchange(&["rename", name], json!({"ok": true}));
    assert!(output.status.success(), "{output:?}");
    assert_eq!(request, json!({"cmd": "rename", "name": name}));
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
    old.as_object_mut().unwrap().remove("settings");
    old.as_object_mut().unwrap().remove("settings_api");
    let (output, request) = exchange(&["status", "--json"], old);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(request, json!({"cmd": "status"}));
    let state: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(state["schema"], 1);
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
