//! The `aurisd` daemon.

use std::{path::PathBuf, sync::Arc};

use anyhow::Context;
use aurisd::{
    aap::session::{SessionConfig, Supervisor},
    bluez,
    config::{self, Config},
    ctl_server,
    state::Snapshot,
    store::{Store, Update},
    writer,
};
use clap::Parser;
use tokio::sync::mpsc;
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

    let cfg = Config::load().unwrap_or_else(|e| {
        warn!(error = %e, "ignoring unreadable config file");
        Config::default()
    });

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
    if let Some(battery) = cache_path.as_deref().and_then(aurisd::cache::load) {
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
    tokio::spawn(
        Supervisor::new(
            Arc::clone(&store),
            SessionConfig::from_env(),
            link_rx,
            cmd_rx,
        )
        .run(),
    );

    wait_for_shutdown().await;
    info!("shutting down");

    // The debounced writer may never get another turn, so publish the closing
    // state synchronously: the link is gone and the batteries are stale.
    // `device.connected` keeps whatever BlueZ last said.
    store.apply(Update::AapLink(false));
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
