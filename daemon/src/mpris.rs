//! Local media players over MPRIS on the session bus.
//!
//! Read-only apart from `Pause` and `Play`. The handoff logic asks for a pause
//! when another Apple host takes the AirPods, and again when a player resumes
//! on its own while that host still owns them; ear detection asks for a pause
//! when a bud leaves the ear and for a play when it goes back in. Uses the
//! same `dbus` stack bluer links.

use std::{sync::Arc, time::Duration};

use dbus::{
    message::MatchRule,
    nonblock::{stdintf::org_freedesktop_dbus::Properties, Proxy, SyncConnection},
};
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

const PLAYER_PREFIX: &str = "org.mpris.MediaPlayer2.";
/// playerctld mirrors whichever player is active; reading it double counts.
const PLAYERCTLD: &str = "org.mpris.MediaPlayer2.playerctld";
const PLAYER_PATH: &str = "/org/mpris/MediaPlayer2";
const ROOT_IFACE: &str = "org.mpris.MediaPlayer2";
const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";
const CALL_TIMEOUT: Duration = Duration::from_secs(2);
/// Signals drive updates; this only catches a missed one.
const RECONCILE: Duration = Duration::from_secs(10);
/// Wait before reconnecting to a session bus that failed or is absent.
const RETRY: Duration = Duration::from_secs(30);

/// Aggregate playback state of every local player.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaybackReading {
    /// At least one player reports `Playing`.
    pub playing: bool,
    /// `Identity` of the first playing player.
    pub app: Option<String>,
    /// Bus name of the first playing player, for pausing exactly that one.
    pub player: Option<String>,
}

/// Requests from the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MprisCommand {
    /// Pause every player that is currently `Playing`.
    PausePlaying,
    /// Pause one player by bus name. Chromium resumes itself when its audio
    /// sink changes, so the handoff hold pauses it again.
    Pause(String),
    /// Resume one player by bus name, after ear detection paused it. Only a
    /// player that is still `Paused` is started.
    Play(String),
}

/// Watch players forever, sending a reading whenever the aggregate changes.
pub async fn run(tx: mpsc::Sender<PlaybackReading>, mut commands: mpsc::Receiver<MprisCommand>) {
    loop {
        match once(&tx, &mut commands).await {
            Ok(()) if tx.is_closed() => return,
            Ok(()) => debug!("MPRIS watcher stopped; reconnecting"),
            Err(e) => debug!(error = %e, "MPRIS session bus unavailable"),
        }
        let sleep = tokio::time::sleep(RETRY);
        tokio::pin!(sleep);
        // Keep draining commands so a pause request is not left queued.
        loop {
            tokio::select! {
                _ = &mut sleep => break,
                cmd = commands.recv() => match cmd {
                    Some(_) => warn!("cannot pause local players: no session bus"),
                    None => return,
                },
            }
        }
    }
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn once(
    tx: &mpsc::Sender<PlaybackReading>,
    commands: &mut mpsc::Receiver<MprisCommand>,
) -> Result<(), dbus::Error> {
    let (resource, conn) = dbus_tokio::connection::new_session_sync()?;
    let (lost_tx, mut lost_rx) = tokio::sync::oneshot::channel::<()>();
    let _io = AbortOnDrop(tokio::spawn(async move {
        let err = resource.await;
        debug!(error = %err, "MPRIS session bus connection lost");
        let _ = lost_tx.send(());
    }));

    let changed = MatchRule::new_signal("org.freedesktop.DBus.Properties", "PropertiesChanged")
        .with_path(PLAYER_PATH);
    let (_changed_match, mut changed) = conn.add_match(changed).await?.msg_stream();
    let owners = MatchRule::new_signal("org.freedesktop.DBus", "NameOwnerChanged")
        .with_sender("org.freedesktop.DBus");
    let (_owners_match, mut owners) = conn.add_match(owners).await?.msg_stream();

    let mut reconcile = tokio::time::interval(RECONCILE);
    reconcile.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last: Option<PlaybackReading> = None;
    loop {
        let refresh = tokio::select! {
            _ = &mut lost_rx => return Ok(()),
            _ = reconcile.tick() => true,
            msg = changed.next() => match msg {
                Some(_) => true,
                None => return Ok(()),
            },
            msg = owners.next() => match msg {
                Some(m) => m.get1::<&str>().is_some_and(|n| n.starts_with(PLAYER_PREFIX)),
                None => return Ok(()),
            },
            cmd = commands.recv() => match cmd {
                Some(MprisCommand::PausePlaying) => {
                    pause_playing(&conn).await;
                    true
                }
                Some(MprisCommand::Pause(name)) => {
                    pause_player(&conn, name).await;
                    true
                }
                Some(MprisCommand::Play(name)) => {
                    play_player(&conn, name).await;
                    true
                }
                None => return Ok(()),
            },
        };
        if !refresh {
            continue;
        }
        let reading = read(&conn).await?;
        if last.as_ref() != Some(&reading) {
            debug!(playing = reading.playing, "local playback changed");
            last = Some(reading.clone());
            if tx.send(reading).await.is_err() {
                return Ok(());
            }
        }
    }
}

/// Every player and its `PlaybackStatus`, skipping players that do not answer.
async fn players(
    conn: &Arc<SyncConnection>,
) -> Result<Vec<(Proxy<'static, Arc<SyncConnection>>, String)>, dbus::Error> {
    let bus = Proxy::new(
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        CALL_TIMEOUT,
        Arc::clone(conn),
    );
    let (names,): (Vec<String>,) = bus
        .method_call("org.freedesktop.DBus", "ListNames", ())
        .await?;
    let mut out = Vec::new();
    for name in names
        .into_iter()
        .filter(|n| n.starts_with(PLAYER_PREFIX) && n != PLAYERCTLD)
    {
        let proxy = Proxy::new(name, PLAYER_PATH, CALL_TIMEOUT, Arc::clone(conn));
        if let Ok(status) = proxy.get::<String>(PLAYER_IFACE, "PlaybackStatus").await {
            out.push((proxy, status));
        }
    }
    Ok(out)
}

async fn read(conn: &Arc<SyncConnection>) -> Result<PlaybackReading, dbus::Error> {
    for (proxy, status) in players(conn).await? {
        if status == "Playing" {
            let app = proxy.get::<String>(ROOT_IFACE, "Identity").await.ok();
            return Ok(PlaybackReading {
                playing: true,
                app,
                player: Some(proxy.destination.to_string()),
            });
        }
    }
    Ok(PlaybackReading {
        playing: false,
        app: None,
        player: None,
    })
}

async fn pause_playing(conn: &Arc<SyncConnection>) {
    let players = match players(conn).await {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "cannot list local players to pause");
            return;
        }
    };
    for (proxy, status) in players {
        if status != "Playing" {
            continue;
        }
        let result: Result<(), dbus::Error> = proxy.method_call(PLAYER_IFACE, "Pause", ()).await;
        match result {
            Ok(()) => info!(player = %proxy.destination, "paused local player for another host"),
            Err(e) => warn!(player = %proxy.destination, error = %e, "pause failed"),
        }
    }
}

/// Pause one player. MPRIS `Pause` on a paused player is a no-op.
async fn pause_player(conn: &Arc<SyncConnection>, name: String) {
    let proxy = Proxy::new(name, PLAYER_PATH, CALL_TIMEOUT, Arc::clone(conn));
    let result: Result<(), dbus::Error> = proxy.method_call(PLAYER_IFACE, "Pause", ()).await;
    match result {
        Ok(()) => {
            info!(player = %proxy.destination, "paused a local player that resumed while yielded")
        }
        Err(e) => warn!(player = %proxy.destination, error = %e, "pause failed"),
    }
}

/// Resume one player, but only while it is still `Paused`.
///
/// A player the user stopped, closed and reopened, or left playing something
/// else must not be started by auris: the ear-detection resume is only ever
/// meant to undo auris's own pause. `Play` rather than `PlayPause`, which
/// would pause a player that is already going.
async fn play_player(conn: &Arc<SyncConnection>, name: String) {
    let proxy = Proxy::new(name, PLAYER_PATH, CALL_TIMEOUT, Arc::clone(conn));
    match proxy.get::<String>(PLAYER_IFACE, "PlaybackStatus").await {
        Ok(status) if status == "Paused" => {}
        Ok(status) => {
            debug!(player = %proxy.destination, %status, "not resuming: the player is not paused");
            return;
        }
        Err(e) => {
            debug!(player = %proxy.destination, error = %e, "not resuming: the player is gone");
            return;
        }
    }
    let result: Result<(), dbus::Error> = proxy.method_call(PLAYER_IFACE, "Play", ()).await;
    match result {
        Ok(()) => info!(player = %proxy.destination, "resumed a local player: the bud is back in"),
        Err(e) => warn!(player = %proxy.destination, error = %e, "resume failed"),
    }
}
