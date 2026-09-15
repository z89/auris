//! The `aurisd` daemon.

use std::{path::PathBuf, sync::Arc};

use anyhow::Context;
use aurisd::{
    aap::session::{AutoConnectOptions, HandoffOptions, SessionConfig, Supervisor},
    bluez,
    config::{self, Config},
    ctl_server,
    state::Snapshot,
    store::{Store, Update},
    writer,
};
use clap::Parser;
use tokio::sync::{mpsc, watch};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

/// AirPods AAP daemon: publishes state.json and serves the control socket.
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Override the runtime directory (default `$XDG_RUNTIME_DIR/aurisd`).
    #[arg(long, value_name = "PATH")]
    runtime_dir: Option<PathBuf>,

    /// Pin to one accessory instead of auto-detecting.
    #[arg(long, value_name = "BD_ADDR")]
    device: Option<String>,

    /// Print an example state.json and exit.
    #[arg(long)]
    dump_schema: bool,
}

/// Control-command queue depth. Clicks are rare; a small queue is plenty.
const CMD_QUEUE: usize = 16;
/// Link-event queue depth.
const LINK_QUEUE: usize = 16;
/// Playback-reading and pause-request queue depth.
const PLAYBACK_QUEUE: usize = 8;
/// Proximity-sighting queue depth. Adverts repeat, so dropping one when the
/// supervisor is busy costs nothing.
const SIGHTING_QUEUE: usize = 8;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    if args.dump_schema {
        println!("{}", serde_json::to_string_pretty(&Snapshot::example())?);
        return Ok(());
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    // Dropping malformed configuration could silently discard a pinned
    // identity or safety policy. Missing files still load safe defaults.
    let cfg = Config::load().context("loading aurisd configuration")?;
    cfg.ble.validate().context("validating BLE configuration")?;

    let pinned_str = args.device.or(cfg.device);
    let pinned = match pinned_str.as_deref() {
        Some(s) => Some(
            s.parse::<bluer::Address>()
                .map_err(|e| anyhow::anyhow!("{e}"))?,
        ),
        None => None,
    };

    let runtime_dir = config::runtime_dir(args.runtime_dir.as_deref());
    config::ensure_runtime_dir(&runtime_dir)?;
    let state_path = config::state_path(&runtime_dir);
    let socket_path = config::socket_path(&runtime_dir);

    // Write a usable file before anything else: the plugin must never find the
    // path missing, even when no accessory has ever connected.
    let mut initial = Snapshot::initial(&pinned.map(|a| a.to_string()).unwrap_or_default());
    let cache_path = aurisd::cache::cache_path();
    if let Some(battery) = cache_path
        .as_deref()
        .and_then(|path| aurisd::cache::load(path, &initial.device.address))
    {
        info!(case = ?battery.case.level, "restored last known battery levels");
        initial.battery = battery;
    }
    writer::write_atomic(&state_path, &initial)
        .with_context(|| format!("writing {}", state_path.display()))?;
    info!(path = %state_path.display(), "publishing state");

    let store = Store::new(initial, cfg.primary_bud);
    let listener = ctl_server::bind(&socket_path)?;
    info!(path = %socket_path.display(), "control socket ready");

    let (cmd_tx, cmd_rx) = mpsc::channel(CMD_QUEUE);
    let (link_tx, link_rx) = mpsc::channel(LINK_QUEUE);

    tokio::spawn(writer::run(
        store.subscribe(),
        state_path.clone(),
        cache_path,
    ));
    tokio::spawn(ctl_server::serve(listener, Arc::clone(&store), cmd_tx));
    tokio::spawn(bluez::run(link_tx, pinned));
    tokio::spawn(aurisd::ble::run(Arc::clone(&store), cfg.ble, pinned));
    // Read once; shared read-only by the rejoin decision.
    let apple_ouis = Arc::new(aurisd::rejoin::AppleOuis::load());
    info!(
        count = apple_ouis.len(),
        source = apple_ouis.source().as_str(),
        "Apple OUIs loaded"
    );
    let (playback_tx, playback_rx) = mpsc::channel(PLAYBACK_QUEUE);
    let (mpris_tx, mpris_rx) = mpsc::channel(PLAYBACK_QUEUE);
    tokio::spawn(aurisd::mpris::run(playback_tx, mpris_rx));
    // Proximity auto-connect: the scanner only reports adverts, and the
    // supervisor decides, so BlueZ never sees two sources of Device1.Connect.
    let (seen_tx, seen_rx) = mpsc::channel(SIGHTING_QUEUE);
    let (scan_tx, scan_rx) = watch::channel(false);
    tokio::spawn(aurisd::autoconnect::run(
        Arc::clone(&store),
        cfg.autoconnect.clone(),
        seen_tx,
        scan_rx,
    ));
    tokio::spawn(
        Supervisor::new(
            Arc::clone(&store),
            SessionConfig::from_env(),
            link_rx,
            cmd_rx,
        )
        .with_handoff(HandoffOptions {
            config: cfg.handoff,
            config_path: config::config_path(),
            playback_rx: Some(playback_rx),
            mpris_tx: Some(mpris_tx),
            control_audio_profiles: true,
            known_hosts_path: aurisd::rejoin::known_hosts_path(),
            apple_ouis: Some(apple_ouis),
            control_link: true,
        })
        .with_autoconnect(AutoConnectOptions {
            config: cfg.autoconnect,
            seen_rx,
            scan_tx,
            control_link: true,
        })
        .with_ear_media(cfg.ear)
        .run(),
    );

    wait_for_shutdown().await;
    info!("shutting down");

    // A yield sets the AirPods' PipeWire card to `off`. Nothing else would
    // put it back once this process is gone, and the user would find a silent
    // pair of AirPods.
    aurisd::audio_route::restore_on_exit().await;

    // The debounced writer may never get another turn, so publish the closing
    // state synchronously: the link is gone and the batteries are stale.
    // `device.connected` keeps whatever BlueZ last said.
    store.apply(Update::AapLink(false));
    // No observer survives process exit. Retain BLE readings as history, not
    // live observations in the final state file.
    store.apply(Update::ExpireBle {
        before: "9999-12-31T23:59:59Z".into(),
    });
    let final_snapshot = store.snapshot();
    if let Err(e) = writer::write_atomic(&state_path, &final_snapshot) {
        warn!(path = %state_path.display(), error = %e, "failed to write final state.json");
    }
    ctl_server::unbind(&socket_path);
    Ok(())
}

async fn wait_for_shutdown() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "cannot listen for SIGTERM");
            let _ = tokio::signal::ctrl_c().await;
            return;
        }
    };
    tokio::select! {
        _ = term.recv() => info!("SIGTERM"),
        r = tokio::signal::ctrl_c() => {
            if let Err(e) = r {
                warn!(error = %e, "ctrl_c handler failed");
            } else {
                info!("SIGINT");
            }
        }
    }
}
