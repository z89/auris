//! BLE proximity-triggered auto-connect.
//!
//! When AirPods leave their case they page the host they last used. A Mac in
//! the room does not wait for that: macOS also listens for the AirPods'
//! proximity-pairing advert and pages them itself, so it wins the race
//! whenever the AirPods' own page goes elsewhere. BlueZ never pages a classic
//! device on its own, so this host stays disconnected until the AirPods
//! happen to choose it.
//!
//! This module watches for the same advert and pages once per case opening.
//! Two rules keep it from becoming a policy of its own:
//!
//! * One presence episode gets at most one connect sequence. An episode ends
//!   only after [`AutoconnectConfig::absence_seconds`] without a qualifying
//!   advert, which in practice means the AirPods went back in the case.
//! * A manual command, or a local disconnect this daemon did not cause,
//!   disarms the machine until the next absence. Putting the AirPods on
//!   another host, or telling this host to let go, has to stick.
//!
//! The advert is not always there to see. On the BCM20702A0 dongle a case
//! cycle can produce no qualifying advert at all, so nothing pages and the
//! AirPods stay on the other host. A bounded page
//! fallback covers that: while the AirPods are away after a remote or
//! timed-out drop and the machine is still armed, this host pages once
//! [`AutoconnectConfig::fallback_first_seconds`] after the drop and then
//! every [`AutoconnectConfig::fallback_interval_seconds`], for at most
//! [`AutoconnectConfig::fallback_minutes`]. After that only the advert
//! trigger remains. The fallback never runs while an advert episode, a
//! rejoin or another page is in flight, so the advert path keeps priority
//! and there is still only one page at a time.
//!
//! The decision logic in [`decide`] is pure; the scanner in [`run`] only
//! reports sightings and the supervisor in `aap::session` owns every connect,
//! which is also where `rejoin` lives. Only one place ever pages the device.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};

use bluer::{
    AdapterEvent, Address, DeviceEvent, DeviceProperty, DiscoveryFilter, DiscoveryTransport,
};
use futures_util::{stream::SelectAll, StreamExt};
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};

use crate::{
    config::AutoconnectConfig,
    rejoin::{Outcome, Reason},
    store::Store,
};

/// Waits before the second, third and fourth connect of an episode.
pub const RETRY_BACKOFF: [Duration; 3] = [
    Duration::from_secs(5),
    Duration::from_secs(15),
    Duration::from_secs(45),
];

/// Connects allowed in one presence episode.
pub const MAX_ATTEMPTS: u8 = 4;

/// A retry needs an advert at least this fresh. Without one the AirPods are
/// gone again and further pages would only be noise.
pub const ADVERT_FRESH: Duration = Duration::from_secs(10);

/// Apple's Bluetooth company identifier.
pub const APPLE_COMPANY_ID: u16 = 0x004c;

/// AirPods 4 (ANC), used when the accessory's DID product id is not known yet.
pub const DEFAULT_MODEL_ID: u16 = 0x201b;

/// Devices whose property stream the scanner follows at once. LE traffic in a
/// flat is busy; a cap keeps one crowded room from costing unbounded D-Bus
/// subscriptions. Removed devices give their slot back.
pub const WATCHED_CAP: usize = 64;

/// Sleep after a failed scan setup, so a missing adapter cannot spin.
const SCAN_RETRY: Duration = Duration::from_secs(5);

/// One qualifying advert, as the scanner saw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sighting {
    /// Resolvable private address the advert came from. It changes roughly
    /// every fifteen minutes and is only used to key the last RSSI.
    pub address: Address,
    /// Last RSSI reported for that address, if BlueZ has given one yet.
    pub rssi: Option<i16>,
}

/// The two model bytes of an Apple proximity-pairing advert, little-endian,
/// derived from the uppercase hex product id (`"201B"`). Anything
/// unparseable falls back to [`DEFAULT_MODEL_ID`].
pub fn model_bytes(model_id: &str) -> [u8; 2] {
    u16::from_str_radix(model_id.trim(), 16)
        .unwrap_or(DEFAULT_MODEL_ID)
        .to_le_bytes()
}

/// Is this Apple manufacturer-data blob a proximity-pairing advert from the
/// watched model, sent outside pairing mode?
///
/// Byte 0 is the proximity-pairing type, byte 2 is the pairing-mode flag
/// (`0x01` means not in pairing mode, so the accessory is paired and simply
/// out of its case), bytes 3 and 4 are the model id. Everything after that is
/// the encrypted status block, which this module never reads.
pub fn qualifies(data: &[u8], model: [u8; 2]) -> bool {
    data.len() >= 5 && data[0] == 0x07 && data[2] == 0x01 && data[3..5] == model
}

/// Something the supervisor observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A qualifying advert from the watched model.
    Seen {
        /// RSSI of the advertising address, if known.
        rssi: Option<i16>,
    },
    /// Periodic tick; fires a due connect.
    Tick,
    /// BlueZ reports the classic link up.
    LocalConnected,
    /// BlueZ reports the classic link down.
    LocalDisconnected {
        /// Parsed `Device1.Disconnected` reason, or [`Reason::Unknown`] when
        /// only the `Connected` property moved.
        reason: Reason,
    },
    /// An explicit `reconnect` or `connect-once` command.
    ManualCommand,
    /// A connect started by [`Action::Connect`] finished.
    ConnectFinished {
        /// Sequence the connect belonged to.
        seq: u64,
        /// Result.
        outcome: Outcome,
    },
}

/// A side effect or decision for the supervisor to perform or log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// A presence episode began; the first connect is due after the settle.
    EpisodeStarted {
        /// RSSI of the advert that started it.
        rssi: Option<i16>,
        /// Wait before the first connect.
        delay: Duration,
    },
    /// An advert that could have started an episode did not (info).
    Skipped(&'static str),
    /// Re-check the adapter and device, then call `Device1.Connect` once.
    Connect {
        /// Sequence to report back in [`Event::ConnectFinished`].
        seq: u64,
        /// 1-based attempt number.
        attempt: u8,
    },
    /// The connect failed and will be tried again.
    Retry {
        /// Attempt number of the next connect.
        attempt: u8,
        /// Delay before it.
        wait: Duration,
    },
    /// The auto-connect got the link.
    Connected,
    /// A scheduled connect was abandoned.
    Cancelled(&'static str),
    /// The episode ran out of attempts or hit an error (warn).
    GaveUp(String),
    /// Auto-connect is off until the next absence (info).
    Disarmed(&'static str),
    /// An absence was observed; auto-connect is armed again (info).
    Rearmed,
    /// No advert arrived, so this host pages anyway. [`Action::Connect`]
    /// follows immediately.
    FallbackPage {
        /// How long the AirPods have been away.
        away: Duration,
        /// Wait before the next fallback page, if the budget allows one.
        next: Duration,
    },
    /// A fallback page did not get the link; the next slot still comes (info).
    FallbackFailed(String),
}

#[derive(Debug, Clone, Copy)]
struct Pending {
    seq: u64,
    attempt: u8,
    due: Instant,
    in_flight: bool,
    /// Started by the page fallback rather than by an advert: one page per
    /// slot, no retry backoff.
    fallback: bool,
}

/// Everything [`decide`] remembers.
#[derive(Debug, Clone)]
pub struct AutoConnectState {
    /// `[autoconnect] enabled`.
    pub enabled: bool,
    /// `[autoconnect] min_rssi`.
    pub min_rssi: i16,
    /// A rejoin connect is scheduled or in flight. The rejoin module owns
    /// eviction handling; this one never pages over it.
    pub rejoin_pending: bool,
    settle: Duration,
    absence: Duration,
    connected: bool,
    disarmed: bool,
    /// Last qualifying advert, or the last link change. A link change anchors
    /// the absence clock so that a drop the user or another host caused is
    /// not read as a case opening.
    last_seen: Option<Instant>,
    /// An episode has already had its connect sequence.
    episode: bool,
    pending: Option<Pending>,
    seq: u64,
    fallback_first: Duration,
    fallback_interval: Duration,
    fallback_budget: Duration,
    /// When the current away period began, or `None` when the page fallback
    /// does not apply to it. Only a remote or timed-out drop, or a daemon
    /// that started disconnected, sets it.
    fallback_from: Option<Instant>,
    /// Earliest next fallback page; `None` means the first one is still due
    /// at `fallback_from + fallback_first`.
    fallback_next: Option<Instant>,
    /// The state was built with no link and has not seen a clock yet.
    fallback_start: bool,
}

impl AutoConnectState {
    /// Fresh state from configuration. Nothing is armed until the first
    /// qualifying advert arrives.
    pub fn new(config: &AutoconnectConfig) -> Self {
        Self {
            enabled: config.enabled,
            min_rssi: config.min_rssi,
            rejoin_pending: false,
            settle: Duration::from_secs(config.settle_seconds),
            absence: Duration::from_secs(config.absence_seconds),
            connected: false,
            disarmed: false,
            last_seen: None,
            episode: false,
            pending: None,
            seq: 0,
            fallback_first: Duration::from_secs(config.fallback_first_seconds),
            fallback_interval: Duration::from_secs(config.fallback_interval_seconds),
            fallback_budget: Duration::from_secs(config.fallback_minutes * 60),
            fallback_from: None,
            fallback_next: None,
            fallback_start: true,
        }
    }

    /// Should the scanner be running right now? Discovery costs radio time
    /// and only matters while the AirPods are not already here.
    pub fn should_scan(&self) -> bool {
        self.enabled && !self.connected
    }

    /// A connect is scheduled or in flight.
    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Sequence number of the connect that is scheduled or in flight.
    pub fn pending_seq(&self) -> Option<u64> {
        self.pending.map(|p| p.seq)
    }

    /// `Device1.Connect` calls started in the pending episode; `0` while it
    /// is only scheduled.
    pub fn pending_attempt(&self) -> u8 {
        self.pending
            .map_or(0, |p| p.attempt - 1 + u8::from(p.in_flight))
    }

    /// Auto-connect is off until the next absence.
    pub fn is_disarmed(&self) -> bool {
        self.disarmed
    }

    fn cancel(&mut self, why: &'static str) -> Vec<Action> {
        match self.pending.take() {
            Some(_) => vec![Action::Cancelled(why)],
            None => Vec::new(),
        }
    }

    /// The AirPods are here, or somebody else has them: no more pages
    /// without an advert until the next qualifying drop.
    fn end_fallback(&mut self) {
        self.fallback_from = None;
        self.fallback_next = None;
    }

    fn disarm(&mut self, why: &'static str) -> Vec<Action> {
        let mut out = self.cancel(why);
        if !self.disarmed {
            self.disarmed = true;
            out.push(Action::Disarmed(why));
        }
        out
    }
}

/// Fold one event into the state and return what to do about it.
///
/// Pure: no clock, no D-Bus, no logging. `now` is the caller's clock.
pub fn decide(s: &mut AutoConnectState, event: Event, now: Instant) -> Vec<Action> {
    // A daemon that starts with the AirPods already away has no disconnect to
    // anchor the fallback on. The first event with a clock is that anchor.
    if std::mem::take(&mut s.fallback_start) && !s.connected {
        s.fallback_from = Some(now);
    }
    match event {
        Event::Seen { rssi } => {
            if !s.enabled {
                return Vec::new();
            }
            // A weak advert is another room, or the case across the desk. It
            // is not evidence of presence, so it does not even count against
            // the absence clock.
            if rssi.is_some_and(|r| r < s.min_rssi) {
                return vec![Action::Skipped("rssi_below_min")];
            }
            let absent = s
                .last_seen
                .is_none_or(|t| now.saturating_duration_since(t) >= s.absence);
            s.last_seen = Some(now);
            // A sequence already running owns this episode. Sparse adverts
            // must not restart it and hand it a fresh attempt budget.
            if s.pending.is_some() || !absent {
                return Vec::new();
            }
            let mut out = Vec::new();
            s.episode = false;
            if s.disarmed {
                s.disarmed = false;
                out.push(Action::Rearmed);
            }
            if s.connected {
                out.push(Action::Skipped("already_connected"));
                return out;
            }
            if s.rejoin_pending {
                out.push(Action::Skipped("rejoin_pending"));
                return out;
            }
            s.seq += 1;
            s.episode = true;
            s.pending = Some(Pending {
                seq: s.seq,
                attempt: 1,
                due: now + s.settle,
                in_flight: false,
                fallback: false,
            });
            out.push(Action::EpisodeStarted {
                rssi,
                delay: s.settle,
            });
            out
        }
        Event::Tick => {
            let Some(p) = s.pending.as_mut() else {
                return fallback_page(s, now);
            };
            if p.in_flight || now < p.due {
                return Vec::new();
            }
            if !s.enabled {
                return s.cancel("disabled");
            }
            if s.connected {
                return s.cancel("already_connected");
            }
            if s.rejoin_pending {
                return s.cancel("rejoin_pending");
            }
            // The settle wait is the user's choice, so the first connect is
            // not held to the freshness rule; a retry is.
            if p.attempt > 1
                && s.last_seen
                    .is_none_or(|t| now.saturating_duration_since(t) > ADVERT_FRESH)
            {
                return s.cancel("stale_adverts");
            }
            p.in_flight = true;
            vec![Action::Connect {
                seq: p.seq,
                attempt: p.attempt,
            }]
        }
        Event::LocalConnected => {
            s.connected = true;
            // Whatever brought the link up, the next advert must not be read
            // as a case opening.
            s.last_seen = Some(now);
            s.end_fallback();
            match s.pending.take() {
                Some(p) if p.in_flight => vec![Action::Connected],
                Some(_) => vec![Action::Cancelled("already_connected")],
                None => Vec::new(),
            }
        }
        Event::LocalDisconnected { reason } => {
            s.connected = false;
            // The disconnect starts the absence clock. AirPods still out of
            // the case keep advertising, so an advert arriving within the
            // absence window proves this was not a case opening, and nothing
            // happens. That is what keeps a remote or timed-out drop quiet.
            s.last_seen = Some(now);
            let mut out = s.cancel("disconnected");
            // Only a drop this host did not ask for earns a page without an
            // advert. `Unknown` is the bare `Connected` property change that
            // follows the signal, so it leaves an armed window alone rather
            // than arming one of its own.
            match reason {
                Reason::Remote | Reason::Timeout => {
                    s.fallback_from = Some(now);
                    s.fallback_next = None;
                }
                Reason::Unknown => {}
                _ => s.end_fallback(),
            }
            // `Local` means this host ended the link. aurisd never calls
            // Device1.Disconnect, so it was the panel, bluetoothctl or the
            // user: a decision to override, not to undo.
            if reason == Reason::Local {
                out.extend(s.disarm("local_disconnect"));
            }
            out
        }
        Event::ManualCommand => s.disarm("manual_command"),
        Event::ConnectFinished { seq, outcome } => {
            let Some(p) = s.pending.as_mut().filter(|p| p.seq == seq && p.in_flight) else {
                return Vec::new();
            };
            // A fallback page is one page for one slot: it never retries,
            // because the cadence already decides when to try again.
            if p.fallback {
                s.pending = None;
                return match outcome {
                    Outcome::Connected => {
                        s.connected = true;
                        s.end_fallback();
                        vec![Action::Connected]
                    }
                    Outcome::NotReady(why @ ("connected_by_other_means" | "superseded")) => {
                        if why == "connected_by_other_means" {
                            s.end_fallback();
                        }
                        vec![Action::Cancelled(why)]
                    }
                    outcome => vec![Action::FallbackFailed(describe(&outcome))],
                };
            }
            match outcome {
                Outcome::Connected => {
                    s.pending = None;
                    s.connected = true;
                    vec![Action::Connected]
                }
                // Somebody else already has the link, or the sequence was
                // superseded: not a failure to retry around.
                Outcome::NotReady(why @ ("connected_by_other_means" | "superseded")) => {
                    s.pending = None;
                    vec![Action::Cancelled(why)]
                }
                _ if p.attempt < MAX_ATTEMPTS => {
                    let wait = RETRY_BACKOFF[usize::from(p.attempt - 1)];
                    p.attempt += 1;
                    p.due = now + wait;
                    p.in_flight = false;
                    vec![Action::Retry {
                        attempt: p.attempt,
                        wait,
                    }]
                }
                outcome => {
                    s.pending = None;
                    vec![Action::GaveUp(format!(
                        "{MAX_ATTEMPTS} connects did not take: {}",
                        describe(&outcome)
                    ))]
                }
            }
        }
    }
}

/// Page without an advert, if this away period still has a slot for it.
///
/// Every condition is positive: the feature is on, the fallback has a budget,
/// the AirPods are away after a drop this host did not ask for, the machine
/// is armed, no rejoin or advert episode owns the link, and the slot is due
/// inside the budget. Anything else means waiting for the next tick.
fn fallback_page(s: &mut AutoConnectState, now: Instant) -> Vec<Action> {
    if !s.enabled || s.connected || s.disarmed || s.rejoin_pending || s.fallback_budget.is_zero() {
        return Vec::new();
    }
    let Some(from) = s.fallback_from else {
        return Vec::new();
    };
    let away = now.saturating_duration_since(from);
    if away > s.fallback_budget {
        return Vec::new();
    }
    let due = s.fallback_next.unwrap_or(from + s.fallback_first);
    if now < due {
        return Vec::new();
    }
    s.seq += 1;
    s.fallback_next = Some(now + s.fallback_interval);
    s.pending = Some(Pending {
        seq: s.seq,
        attempt: 1,
        due: now,
        in_flight: true,
        fallback: true,
    });
    vec![
        Action::FallbackPage {
            away,
            next: s.fallback_interval,
        },
        Action::Connect {
            seq: s.seq,
            attempt: 1,
        },
    ]
}

fn describe(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Connected => "connected".to_owned(),
        Outcome::Busy => "br-connection-busy".to_owned(),
        Outcome::Failed(e) => e.clone(),
        Outcome::NotReady(why) => (*why).to_owned(),
    }
}

/// Run LE discovery whenever the supervisor asks for it, reporting qualifying
/// adverts on `seen_tx`. Never scans while the feature is off.
pub async fn run(
    store: Arc<Store>,
    config: AutoconnectConfig,
    seen_tx: mpsc::Sender<Sighting>,
    mut scan_rx: watch::Receiver<bool>,
) {
    if !config.enabled {
        info!(
            target: "aurisd::autoconnect",
            "auto-connect disabled; no proximity scanning"
        );
        return;
    }
    loop {
        if !*scan_rx.borrow_and_update() {
            if scan_rx.changed().await.is_err() {
                return;
            }
            continue;
        }
        if let Err(e) = scan(&store, &config, &seen_tx, &mut scan_rx).await {
            warn!(target: "aurisd::autoconnect", error = %e, "proximity scan failed");
            tokio::time::sleep(SCAN_RETRY).await;
        }
    }
}

async fn scan(
    store: &Store,
    config: &AutoconnectConfig,
    seen_tx: &mpsc::Sender<Sighting>,
    scan_rx: &mut watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let model = model_bytes(&store.snapshot().device.model_id);
    let session = bluer::Session::new().await?;
    let adapter = session.default_adapter().await?;
    adapter
        .set_discovery_filter(DiscoveryFilter {
            transport: DiscoveryTransport::Le,
            duplicate_data: true,
            discoverable: false,
            ..Default::default()
        })
        .await?;
    let discovery = adapter.discover_devices().await?;
    tokio::pin!(discovery);
    info!(
        target: "aurisd::autoconnect",
        model = %format!("{:02X}{:02X}", model[1], model[0]),
        min_rssi = config.min_rssi,
        "watching for the proximity advert"
    );
    let mut watched: HashSet<Address> = HashSet::new();
    let mut rssi: HashMap<Address, i16> = HashMap::new();
    let mut changes = SelectAll::new();
    loop {
        tokio::select! {
            biased;
            changed = scan_rx.changed() => {
                if changed.is_err() || !*scan_rx.borrow() {
                    break;
                }
            }
            event = discovery.next() => match event {
                Some(AdapterEvent::DeviceAdded(rpa))
                    if watched.len() < WATCHED_CAP && watched.insert(rpa) =>
                {
                    // Do NOT read cached ManufacturerData on discovery. BlueZ
                    // replays known, long-gone devices at scan startup, and a
                    // stale advert would page an accessory that is not here.
                    let dev = adapter.device(rpa)?;
                    let events = dev.events().await?;
                    changes.push(events.map(move |e| (rpa, e)).boxed());
                }
                Some(AdapterEvent::DeviceRemoved(rpa)) => {
                    watched.remove(&rpa);
                    rssi.remove(&rpa);
                }
                None => break,
                _ => {}
            },
            Some((rpa, event)) = changes.next(), if !changes.is_empty() => {
                match event {
                    // RSSI arrives as its own property change, usually before
                    // the advert data, so it is kept per advertising address.
                    DeviceEvent::PropertyChanged(DeviceProperty::Rssi(value)) => {
                        rssi.insert(rpa, value);
                    }
                    DeviceEvent::PropertyChanged(DeviceProperty::ManufacturerData(data)) => {
                        if !data.get(&APPLE_COMPANY_ID).is_some_and(|d| qualifies(d, model)) {
                            continue;
                        }
                        let seen = rssi.get(&rpa).copied();
                        // An unknown RSSI is accepted: the advert itself is
                        // the evidence, and BlueZ does not always fill it in.
                        if seen.is_some_and(|v| v < config.min_rssi) {
                            debug!(
                                target: "aurisd::autoconnect",
                                rssi = ?seen,
                                "proximity advert below min_rssi"
                            );
                            continue;
                        }
                        let _ = seen_tx.try_send(Sighting { address: rpa, rssi: seen });
                    }
                    _ => {}
                }
            }
        }
    }
    // Dropping the discovery stream releases only this client's discovery
    // token, not other apps' scans.
    info!(target: "aurisd::autoconnect", "proximity scanning stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> AutoConnectState {
        AutoConnectState::new(&AutoconnectConfig::default())
    }

    fn seen(s: &mut AutoConnectState, now: Instant) -> Vec<Action> {
        decide(s, Event::Seen { rssi: Some(-50) }, now)
    }

    fn connect_at(s: &mut AutoConnectState, at: Instant) -> Vec<Action> {
        decide(s, Event::Tick, at)
    }

    fn finished(s: &mut AutoConnectState, seq: u64, outcome: Outcome, now: Instant) -> Vec<Action> {
        decide(s, Event::ConnectFinished { seq, outcome }, now)
    }

    #[test]
    fn first_sight_connects_after_the_settle_wait() {
        let t0 = Instant::now();
        let mut s = state();
        let out = seen(&mut s, t0);
        assert_eq!(
            out,
            vec![Action::EpisodeStarted {
                rssi: Some(-50),
                delay: Duration::from_secs(3)
            }]
        );
        // Not yet: the Mac gets its chance first.
        assert!(connect_at(&mut s, t0 + Duration::from_secs(2)).is_empty());
        assert_eq!(
            connect_at(&mut s, t0 + Duration::from_secs(3)),
            vec![Action::Connect { seq: 1, attempt: 1 }]
        );
        // In flight: a tick does not start a second one.
        assert!(connect_at(&mut s, t0 + Duration::from_secs(4)).is_empty());
        assert_eq!(
            finished(&mut s, 1, Outcome::Connected, t0 + Duration::from_secs(5)),
            vec![Action::Connected]
        );
        assert!(!s.is_pending());
        assert!(!s.should_scan());
    }

    #[test]
    fn a_second_advert_in_the_same_episode_does_not_connect_again() {
        let t0 = Instant::now();
        let mut s = state();
        seen(&mut s, t0);
        connect_at(&mut s, t0 + Duration::from_secs(3));
        finished(&mut s, 1, Outcome::Connected, t0 + Duration::from_secs(4));
        decide(&mut s, Event::LocalConnected, t0 + Duration::from_secs(4));
        decide(
            &mut s,
            Event::LocalDisconnected {
                reason: Reason::Remote,
            },
            t0 + Duration::from_secs(5),
        );
        // Still the same outing: adverts keep arriving, nothing happens.
        for n in 6..20 {
            assert!(seen(&mut s, t0 + Duration::from_secs(n)).is_empty(), "{n}");
            assert!(connect_at(&mut s, t0 + Duration::from_secs(n)).is_empty());
        }
        assert!(!s.is_pending());
    }

    #[test]
    fn an_absence_starts_a_new_episode() {
        let t0 = Instant::now();
        let mut s = state();
        seen(&mut s, t0);
        connect_at(&mut s, t0 + Duration::from_secs(3));
        finished(&mut s, 1, Outcome::Connected, t0 + Duration::from_secs(4));
        decide(&mut s, Event::LocalConnected, t0 + Duration::from_secs(4));
        let back_in_case = t0 + Duration::from_secs(60);
        decide(
            &mut s,
            Event::LocalDisconnected {
                reason: Reason::Timeout,
            },
            back_in_case,
        );
        let reopened = back_in_case + Duration::from_secs(20);
        assert_eq!(
            seen(&mut s, reopened),
            vec![Action::EpisodeStarted {
                rssi: Some(-50),
                delay: Duration::from_secs(3)
            }]
        );
        assert_eq!(
            connect_at(&mut s, reopened + Duration::from_secs(3)),
            vec![Action::Connect { seq: 2, attempt: 1 }]
        );
    }

    #[test]
    fn a_manual_command_disarms_until_an_absence() {
        let t0 = Instant::now();
        let mut s = state();
        seen(&mut s, t0);
        assert_eq!(
            decide(&mut s, Event::ManualCommand, t0 + Duration::from_secs(1)),
            vec![
                Action::Cancelled("manual_command"),
                Action::Disarmed("manual_command")
            ]
        );
        assert!(s.is_disarmed());
        // The user put them on the Mac; adverts keep coming and are ignored.
        for n in 2..14 {
            assert!(seen(&mut s, t0 + Duration::from_secs(n)).is_empty(), "{n}");
        }
        assert!(connect_at(&mut s, t0 + Duration::from_secs(30)).is_empty());
        let after_absence = t0 + Duration::from_secs(13 + 15);
        let out = seen(&mut s, after_absence);
        assert_eq!(out[0], Action::Rearmed);
        assert!(matches!(out[1], Action::EpisodeStarted { .. }));
        assert!(!s.is_disarmed());
    }

    #[test]
    fn a_local_disconnect_disarms_but_a_remote_one_does_not() {
        let t0 = Instant::now();
        let mut s = state();
        decide(&mut s, Event::LocalConnected, t0);
        let out = decide(
            &mut s,
            Event::LocalDisconnected {
                reason: Reason::Local,
            },
            t0 + Duration::from_secs(1),
        );
        assert_eq!(out, vec![Action::Disarmed("local_disconnect")]);
        assert!(seen(&mut s, t0 + Duration::from_secs(2)).is_empty());

        let mut s = state();
        decide(&mut s, Event::LocalConnected, t0);
        assert!(decide(
            &mut s,
            Event::LocalDisconnected {
                reason: Reason::Remote,
            },
            t0 + Duration::from_secs(1),
        )
        .is_empty());
        assert!(!s.is_disarmed());
        // Still nothing while the adverts continue: no absence, no episode.
        assert!(seen(&mut s, t0 + Duration::from_secs(2)).is_empty());
        assert!(matches!(
            seen(&mut s, t0 + Duration::from_secs(40)).as_slice(),
            [Action::EpisodeStarted { .. }]
        ));
    }

    #[test]
    fn a_pending_rejoin_owns_the_link() {
        let t0 = Instant::now();
        let mut s = state();
        s.rejoin_pending = true;
        assert_eq!(seen(&mut s, t0), vec![Action::Skipped("rejoin_pending")]);
        assert!(!s.is_pending());
        // A rejoin that starts after the episode did cancels it too.
        let mut s = state();
        seen(&mut s, t0);
        s.rejoin_pending = true;
        assert_eq!(
            connect_at(&mut s, t0 + Duration::from_secs(3)),
            vec![Action::Cancelled("rejoin_pending")]
        );
    }

    #[test]
    fn failures_back_off_and_stop_after_four_attempts() {
        let t0 = Instant::now();
        let mut s = state();
        seen(&mut s, t0);
        let mut at = t0 + Duration::from_secs(3);
        for (attempt, wait) in [(1u8, 5u64), (2, 15), (3, 45)] {
            assert_eq!(
                connect_at(&mut s, at),
                vec![Action::Connect { seq: 1, attempt }],
                "attempt {attempt}"
            );
            assert_eq!(
                finished(&mut s, 1, Outcome::Busy, at),
                vec![Action::Retry {
                    attempt: attempt + 1,
                    wait: Duration::from_secs(wait)
                }]
            );
            at += Duration::from_secs(wait);
            // Adverts keep the retry alive.
            seen(&mut s, at);
        }
        assert_eq!(
            connect_at(&mut s, at),
            vec![Action::Connect { seq: 1, attempt: 4 }]
        );
        let out = finished(
            &mut s,
            1,
            Outcome::Failed("br-connection-refused".into()),
            at,
        );
        assert!(
            matches!(&out[..], [Action::GaveUp(e)] if e.contains("br-connection-refused")),
            "{out:?}"
        );
        assert!(!s.is_pending());
        // Quiet until the next absence.
        assert!(seen(&mut s, at + Duration::from_secs(1)).is_empty());
    }

    #[test]
    fn a_retry_stops_once_the_adverts_go_stale() {
        let t0 = Instant::now();
        let mut s = state();
        seen(&mut s, t0);
        let first = t0 + Duration::from_secs(3);
        assert_eq!(
            connect_at(&mut s, first),
            vec![Action::Connect { seq: 1, attempt: 1 }]
        );
        finished(&mut s, 1, Outcome::Busy, first);
        // The second attempt still goes: the advert is eight seconds old.
        let second = first + Duration::from_secs(5);
        assert_eq!(
            connect_at(&mut s, second),
            vec![Action::Connect { seq: 1, attempt: 2 }]
        );
        finished(&mut s, 1, Outcome::Busy, second);
        // They went back in the case: nothing heard for more than
        // ADVERT_FRESH, so the third attempt would be paging an empty room.
        assert_eq!(
            connect_at(&mut s, second + Duration::from_secs(15)),
            vec![Action::Cancelled("stale_adverts")]
        );
        assert!(!s.is_pending());
    }

    #[test]
    fn a_connect_that_finds_the_link_taken_does_not_retry() {
        let t0 = Instant::now();
        let mut s = state();
        seen(&mut s, t0);
        connect_at(&mut s, t0 + Duration::from_secs(3));
        assert_eq!(
            finished(
                &mut s,
                1,
                Outcome::NotReady("connected_by_other_means"),
                t0 + Duration::from_secs(4)
            ),
            vec![Action::Cancelled("connected_by_other_means")]
        );
        assert!(!s.is_pending());
        // A stale result from an older sequence is ignored.
        assert!(finished(&mut s, 1, Outcome::Connected, t0).is_empty());
    }

    #[test]
    fn disabled_does_nothing_and_never_scans() {
        let t0 = Instant::now();
        let mut s = AutoConnectState::new(&AutoconnectConfig {
            enabled: false,
            ..AutoconnectConfig::default()
        });
        assert!(!s.should_scan());
        assert!(seen(&mut s, t0).is_empty());
        assert!(connect_at(&mut s, t0 + Duration::from_secs(10)).is_empty());
        assert!(!s.is_pending());
    }

    #[test]
    fn a_weak_advert_is_not_presence() {
        let t0 = Instant::now();
        let mut s = state();
        assert_eq!(
            decide(&mut s, Event::Seen { rssi: Some(-95) }, t0),
            vec![Action::Skipped("rssi_below_min")]
        );
        assert!(!s.is_pending());
        // It did not reset the absence clock either.
        assert!(matches!(
            decide(
                &mut s,
                Event::Seen { rssi: None },
                t0 + Duration::from_secs(1)
            )
            .as_slice(),
            [Action::EpisodeStarted { rssi: None, .. }]
        ));
    }

    #[test]
    fn scanning_follows_the_local_link() {
        let t0 = Instant::now();
        let mut s = state();
        assert!(s.should_scan());
        decide(&mut s, Event::LocalConnected, t0);
        assert!(!s.should_scan());
        decide(
            &mut s,
            Event::LocalDisconnected {
                reason: Reason::Timeout,
            },
            t0 + Duration::from_secs(1),
        );
        assert!(s.should_scan());
    }

    fn gone(s: &mut AutoConnectState, at: Instant, reason: Reason) -> Vec<Action> {
        decide(s, Event::LocalDisconnected { reason }, at)
    }

    /// Connected at `t0`, dropped by the AirPods a second later. Returns the
    /// moment of the drop, which is where the fallback clock starts.
    fn dropped(s: &mut AutoConnectState, t0: Instant) -> Instant {
        decide(s, Event::LocalConnected, t0);
        let at = t0 + Duration::from_secs(1);
        assert!(gone(s, at, Reason::Remote).is_empty());
        // The bare `Connected` property change that follows the signal must
        // not disturb the window the signal just opened.
        assert!(gone(s, at + ms(5), Reason::Unknown).is_empty());
        at
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn page_timed_out(s: &mut AutoConnectState, seq: u64, at: Instant) -> Vec<Action> {
        finished(
            s,
            seq,
            Outcome::Failed("br-connection-page-timeout".to_owned()),
            at,
        )
    }

    #[test]
    fn no_advert_after_a_remote_drop_pages_on_the_fallback_cadence() {
        // A case cycle where the adapter never reports a qualifying advert
        // means nothing ever pages on the advert path.
        let t0 = Instant::now();
        let mut s = state();
        let away = dropped(&mut s, t0);
        assert!(connect_at(&mut s, away + secs(19)).is_empty());
        assert_eq!(
            connect_at(&mut s, away + secs(20)),
            vec![
                Action::FallbackPage {
                    away: secs(20),
                    next: secs(45),
                },
                Action::Connect { seq: 1, attempt: 1 },
            ]
        );
        // One page per slot.
        assert!(connect_at(&mut s, away + secs(21)).is_empty());
        // A page timeout is not a failure to back off from: the cadence
        // already says when to try again.
        let out = page_timed_out(&mut s, 1, away + secs(25));
        assert!(
            matches!(&out[..], [Action::FallbackFailed(e)] if e.contains("page-timeout")),
            "{out:?}"
        );
        assert!(!s.is_pending());
        assert!(connect_at(&mut s, away + secs(64)).is_empty());
        assert!(matches!(
            connect_at(&mut s, away + secs(65)).as_slice(),
            [
                Action::FallbackPage { .. },
                Action::Connect { seq: 2, attempt: 1 }
            ]
        ));
        page_timed_out(&mut s, 2, away + secs(70));

        // And it stops at the budget: ten minutes after the drop.
        let mut seq = 2;
        let mut pages = 0;
        for n in (75..=900).step_by(5) {
            let at = away + secs(n);
            let out = connect_at(&mut s, at);
            if !out.is_empty() {
                assert!(n <= 600, "paged {n} s after the drop, past the budget");
                pages += 1;
                seq += 1;
                page_timed_out(&mut s, seq, at);
            }
        }
        assert_eq!(pages, 11, "one page every 45 s to the ten-minute budget");
    }

    #[test]
    fn a_manual_command_suppresses_the_fallback_page() {
        let t0 = Instant::now();
        let mut s = state();
        let away = dropped(&mut s, t0);
        decide(&mut s, Event::ManualCommand, away + secs(2));
        for n in [20u64, 65, 110] {
            assert!(connect_at(&mut s, away + secs(n)).is_empty(), "{n}");
        }
        assert!(!s.is_pending());
    }

    #[test]
    fn a_local_disconnect_suppresses_the_fallback_page() {
        let t0 = Instant::now();
        let mut s = state();
        decide(&mut s, Event::LocalConnected, t0);
        let away = t0 + secs(1);
        assert_eq!(
            gone(&mut s, away, Reason::Local),
            vec![Action::Disarmed("local_disconnect")]
        );
        assert!(gone(&mut s, away + ms(5), Reason::Unknown).is_empty());
        for n in [20u64, 65, 110] {
            assert!(connect_at(&mut s, away + secs(n)).is_empty(), "{n}");
        }
        assert!(!s.is_pending());
    }

    #[test]
    fn an_advert_episode_keeps_priority_and_the_fallback_clock_stands() {
        let t0 = Instant::now();
        let mut s = state();
        let away = dropped(&mut s, t0);
        // An advert two seconds before the first fallback slot: the episode
        // owns the link, and the slot passes without a second page.
        assert!(matches!(
            seen(&mut s, away + secs(18)).as_slice(),
            [Action::EpisodeStarted { .. }]
        ));
        assert!(connect_at(&mut s, away + secs(20)).is_empty());
        let mut at = away + secs(21);
        assert_eq!(
            connect_at(&mut s, at),
            vec![Action::Connect { seq: 1, attempt: 1 }]
        );
        for (attempt, wait) in [(1u8, 5u64), (2, 15), (3, 45)] {
            finished(&mut s, 1, Outcome::Busy, at);
            at += secs(wait);
            seen(&mut s, at);
            assert_eq!(
                connect_at(&mut s, at),
                vec![Action::Connect {
                    seq: 1,
                    attempt: attempt + 1
                }],
                "attempt {attempt}"
            );
        }
        let out = finished(&mut s, 1, Outcome::Busy, at);
        assert!(matches!(&out[..], [Action::GaveUp(_)]), "{out:?}");
        // The episode changed nothing about the fallback: its first slot is
        // long due, so the next tick pages.
        assert!(matches!(
            connect_at(&mut s, at + secs(1)).as_slice(),
            [
                Action::FallbackPage { .. },
                Action::Connect { seq: 2, attempt: 1 }
            ]
        ));
    }

    #[test]
    fn a_zero_fallback_budget_leaves_only_the_advert_trigger() {
        let t0 = Instant::now();
        let mut s = AutoConnectState::new(&AutoconnectConfig {
            fallback_minutes: 0,
            ..AutoconnectConfig::default()
        });
        let away = dropped(&mut s, t0);
        for n in [20u64, 65, 110, 300] {
            assert!(connect_at(&mut s, away + secs(n)).is_empty(), "{n}");
        }
        assert!(matches!(
            seen(&mut s, away + secs(400)).as_slice(),
            [Action::EpisodeStarted { .. }]
        ));
    }

    #[test]
    fn a_daemon_that_starts_away_pages_too_and_stops_when_somebody_else_wins() {
        let t0 = Instant::now();
        let mut s = state();
        // No disconnect was ever seen: the first tick anchors the window.
        assert!(connect_at(&mut s, t0).is_empty());
        // A rejoin owns eviction recovery; the fallback never pages over it.
        s.rejoin_pending = true;
        assert!(connect_at(&mut s, t0 + secs(20)).is_empty());
        s.rejoin_pending = false;
        assert!(matches!(
            connect_at(&mut s, t0 + secs(21)).as_slice(),
            [Action::FallbackPage { .. }, Action::Connect { seq: 1, .. }]
        ));
        assert_eq!(s.pending_attempt(), 1);
        assert_eq!(
            finished(
                &mut s,
                1,
                Outcome::NotReady("connected_by_other_means"),
                t0 + secs(22)
            ),
            vec![Action::Cancelled("connected_by_other_means")]
        );
        // Somebody else has them: no more pages until the next drop.
        for n in [66u64, 111, 300] {
            assert!(connect_at(&mut s, t0 + secs(n)).is_empty(), "{n}");
        }
    }

    #[test]
    fn only_a_paired_airpods_4_advert_out_of_the_case_qualifies() {
        assert_eq!(model_bytes("201B"), [0x1b, 0x20]);
        assert_eq!(model_bytes("2019"), [0x19, 0x20]);
        assert_eq!(model_bytes(""), DEFAULT_MODEL_ID.to_le_bytes());
        assert_eq!(model_bytes("nonsense"), DEFAULT_MODEL_ID.to_le_bytes());
        let model = model_bytes("201B");
        let good = [0x07u8, 0x19, 0x01, 0x1b, 0x20, 0x60, 0, 0];
        assert!(qualifies(&good, model));
        for (index, value) in [(0, 0x10), (2, 0x00), (3, 0x0e), (4, 0x00)] {
            let mut bad = good;
            bad[index] = value;
            assert!(!qualifies(&bad, model), "byte {index}");
        }
        for len in 0..5 {
            assert!(!qualifies(&good[..len], model));
        }
        // A different model's advert is somebody else's AirPods.
        assert!(!qualifies(&good, model_bytes("2014")));
    }

    #[test]
    fn the_pending_view_counts_connects_started_in_the_episode() {
        let t0 = Instant::now();
        let mut s = state();
        assert_eq!(s.pending_seq(), None);
        assert_eq!(s.pending_attempt(), 0);

        seen(&mut s, t0);
        assert_eq!(s.pending_seq(), Some(1));
        assert_eq!(s.pending_attempt(), 0, "scheduled, not connecting");

        connect_at(&mut s, t0 + Duration::from_secs(3));
        assert_eq!(s.pending_attempt(), 1);

        finished(
            &mut s,
            1,
            Outcome::Failed("br-connection-page-timeout".to_owned()),
            t0 + Duration::from_secs(4),
        );
        // The retry is waiting, so no second connect has started yet.
        assert_eq!(s.pending_attempt(), 1);
        assert_eq!(s.pending_seq(), Some(1));

        seen(&mut s, t0 + Duration::from_secs(5));
        connect_at(&mut s, t0 + Duration::from_secs(10));
        assert_eq!(s.pending_attempt(), 2);
    }
}
