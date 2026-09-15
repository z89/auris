//! Socket lifecycle and the connection state machine.
//!
//! This module owns the only AAP socket and never parses bytes itself: every
//! packet goes through [`crate::aap::codec`]. It reacts to BlueZ link events
//! and control commands. L2CAP dialing is guarded because it can cause the
//! kernel to establish an ACL connection.

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use bluer::Address;
use tokio::{
    sync::{mpsc, watch},
    time::{Instant, MissedTickBehavior},
};
use tracing::{debug, info, trace, warn};

use crate::{
    aap::{
        codec::{self, ControlState, FeaturesVariant, Packet},
        opcode,
        socket::{self, AapSocket, Link},
    },
    audio_route::{drain_state, Drain, RouteMode, Router, DRAIN_TIMEOUT, YIELD_ROUTE_WAIT},
    autoconnect::{self, Action as AutoAction, AutoConnectState, Event as AutoEvent, Sighting},
    bluez::{self, LinkEvent},
    config::{self, AutoconnectConfig, EarConfig, HandoffConfig},
    ctl_proto::{Request, Response},
    ctl_server::Command,
    ear_media::{self, Action as EarAction, EarMediaState, Event as EarEvent},
    handoff::{self, Action, Event as HandoffEvent, HandoffState},
    mpris::{MprisCommand, PlaybackReading},
    rejoin::{self, Action as RejoinAction, Event as RejoinEvent, MatchKind, RejoinState},
    state::{EarState, Handoff, Lid, LinkReason, Snapshot},
    store::{ConnectionContext, LinkActivity, Store, Update},
};

/// Settle time after `Connected=true` before dialing the PSM.
const SETTLE: Duration = Duration::from_millis(800);
/// A primary bud address change this recent when the link drops makes the
/// drop a bud role switch. The 0x0C report can move to the other bud a few
/// seconds before the AirPods reset this host's link, and the same switch
/// can instead end as a supervision timeout up to about 20 s later.
const BUD_SWITCH_WINDOW: Duration = Duration::from_secs(15);
/// How long to wait for the accessory to answer the handshake before giving
/// up on the socket. Nothing sent before this ack is acted on.
const HANDSHAKE_ACK_WAIT: Duration = Duration::from_secs(3);
/// How long to wait for the set-features ack. Advisory: some firmware stays
/// silent here, so a timeout only logs and the sequence continues.
const FEATURES_ACK_WAIT: Duration = Duration::from_secs(2);
/// Give up on a `connect()` after this long.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Give up on an L2CAP `send()` after this long; a stalled peer must not
/// freeze the select loop.
const SEND_TIMEOUT: Duration = Duration::from_millis(1500);
/// No battery packet within this long after the handshake is suspicious.
const BATTERY_WATCHDOG: Duration = Duration::from_secs(10);
/// How many times to re-send request-notifications before recycling.
const MAX_NOTIF_RESENDS: u8 = 2;
/// No packet at all for this long makes the link *suspect*. AirPods only
/// speak when something changes, so silence alone proves nothing: the idle
/// watchdog sends a probe first and only recycles if that probe goes
/// unanswered. See [`idle_decision`].
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);
/// How long to wait for any packet after the idle probe before recycling.
const IDLE_PROBE_GRACE: Duration = Duration::from_secs(15);
/// State machine tick.
const TICK: Duration = Duration::from_millis(250);
/// How long after a setting write to wait before reopening the link to read
/// it back. A burst of writes from a slider settles into one reopen.
const VERIFY_DEBOUNCE: Duration = Duration::from_millis(1500);
/// Whole budget for a readback, measured from the write. Past it the write
/// stands but is published as unverified.
const VERIFY_BUDGET: Duration = Duration::from_secs(10);
/// The accessory dumps its settings once per link, about 13 ms after the
/// set-features ack. The dump ends at the first battery packet; this is how
/// long to wait for one before judging the dump complete anyway.
const READBACK_WINDOW: Duration = Duration::from_millis(2000);
/// Receive buffer; AAP messages are tiny but the L2CAP MTU can be larger.
const RECV_BUF: usize = 2048;
/// Longest wait for one BlueZ audio profile connect.
const PROFILE_TIMEOUT: Duration = Duration::from_secs(20);
/// Waits before retrying a take-over profile connect that BlueZ refused as
/// busy (`br-connection-busy`) or in progress; then give up.
const CLAIM_RETRY: [Duration; 3] = [
    Duration::from_millis(700),
    Duration::from_millis(1500),
    Duration::from_secs(3),
];
/// Longest wait for one rejoin `Device1.Connect`. A timeout is not retried:
/// the request may still complete.
const REJOIN_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Knobs read from the environment at startup.
#[derive(Debug, Clone, Copy)]
pub struct SessionConfig {
    /// PSM to dial. Always 0x1001 in practice.
    pub psm: u16,
    /// Set-features variant forced by the environment. `None` leaves the
    /// choice to the automatic fallback in [`pick_variant`].
    pub features: Option<FeaturesVariant>,
    /// Use the raw libc socket instead of bluer's.
    pub raw_socket: bool,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            psm: opcode::AAP_PSM,
            features: None,
            raw_socket: false,
        }
    }
}

impl SessionConfig {
    /// Honour `AURISD_FEATURES=alt` and `AURISD_RAW_SOCKET=1`.
    pub fn from_env() -> Self {
        let features = match std::env::var("AURISD_FEATURES").as_deref() {
            Ok("alt") => Some(FeaturesVariant::Alt),
            Ok("ff") => Some(FeaturesVariant::Ff),
            _ => None,
        };
        let raw_socket = matches!(std::env::var("AURISD_RAW_SOCKET").as_deref(), Ok("1"));
        Self {
            features,
            raw_socket,
            ..Self::default()
        }
    }
}

enum Wake {
    Link(LinkEvent),
    Cmd(Command),
    Playback(Option<PlaybackReading>),
    Recv(std::io::Result<Vec<u8>>),
    Rejoin(u64, rejoin::Outcome),
    Seen(Sighting),
    AutoConnect(u64, rejoin::Outcome),
    Tick,
}

/// Inputs and side-effect permissions for Apple multi-host switching. The
/// default has no MPRIS feed, no config path and no BlueZ profile calls, so a
/// supervisor built for tests can never touch the desktop or the radio.
#[derive(Default)]
pub struct HandoffOptions {
    /// `[handoff]` from config.toml.
    pub config: HandoffConfig,
    /// Where `set_handoff` persists; `None` refuses the request.
    pub config_path: Option<PathBuf>,
    /// Local playback readings.
    pub playback_rx: Option<mpsc::Receiver<PlaybackReading>>,
    /// Pause requests to the MPRIS task.
    pub mpris_tx: Option<mpsc::Sender<MprisCommand>>,
    /// Allow BlueZ Device1.ConnectProfile on take-over.
    pub control_audio_profiles: bool,
    /// Where relay-learned Apple hosts (smart routing relay senders) persist;
    /// `None` keeps them in memory only. Rejoin does not need them.
    pub known_hosts_path: Option<PathBuf>,
    /// Apple OUIs loaded at daemon start; `None` uses the built-in list.
    pub apple_ouis: Option<Arc<rejoin::AppleOuis>>,
    /// Allow BlueZ Device1.Connect to rejoin after an Apple host evicted this
    /// host.
    pub control_link: bool,
}

/// Inputs and side-effect permissions for BLE proximity auto-connect. A
/// supervisor built without them never scans and never pages.
pub struct AutoConnectOptions {
    /// `[autoconnect]` from config.toml.
    pub config: AutoconnectConfig,
    /// Qualifying adverts from the scanner task.
    pub seen_rx: mpsc::Receiver<Sighting>,
    /// Tells the scanner when discovery is wanted.
    pub scan_tx: watch::Sender<bool>,
    /// Allow BlueZ Device1.Connect when an episode comes due.
    pub control_link: bool,
}

/// The supervisor task: sole owner of the AAP socket.
pub struct Supervisor {
    store: Arc<Store>,
    cfg: SessionConfig,
    link_rx: mpsc::Receiver<LinkEvent>,
    cmd_rx: mpsc::Receiver<Command>,

    adapter: Option<Address>,
    device: Option<Address>,
    /// True only when the watcher selected this identity from an explicit pin.
    pinned: bool,
    acl: bool,
    sock: Option<Arc<Link>>,

    dial_at: Option<Instant>,
    dial_kind: Option<DialKind>,
    /// Invalidates a dial or handshake when the selected link changes.
    link_generation: u64,
    /// Ambiguous peer loss can mean handoff to another host. Suppression lasts
    /// until a genuine new local false->true link observation.
    auto_suppressed: bool,
    handshake_at: Option<Instant>,
    last_packet: Option<Instant>,
    battery_seen: bool,
    notif_resends: u8,
    /// When the idle probe was sent, while its answer is still outstanding.
    idle_probe_at: Option<Instant>,

    /// When the debounced verify reopen is due, after a setting write.
    verify_at: Option<Instant>,
    /// Deadline for the whole readback, from the write that started it.
    verify_budget: Option<Instant>,
    /// When the current link's opening settings dump is judged finished, if no
    /// battery packet ends it first.
    readback_at: Option<Instant>,

    /// Dials made in this `Connected` period; drives variant alternation.
    dial_attempt: u32,
    /// Variant used by the session currently open (or last attempted).
    active_variant: FeaturesVariant,
    /// Variant that has produced a battery packet. Once set, it is used for
    /// the rest of the process: alternation is a search, not a policy.
    locked_variant: Option<FeaturesVariant>,

    /// Apple multi-host switching state machine.
    handoff: HandoffState,
    /// Last handoff view sent to the store, to avoid a snapshot clone per tick.
    handoff_published: Handoff,
    /// Side effects decided but not yet performed.
    handoff_actions: Vec<Action>,
    /// Latest media transports BlueZ reported, as `path uuid state`, for the
    /// logs that name why a stream did not start.
    transports: String,
    /// `btName` announced to other hosts.
    bt_name: String,
    config_path: Option<PathBuf>,
    playback_rx: Option<mpsc::Receiver<PlaybackReading>>,
    mpris_tx: Option<mpsc::Sender<MprisCommand>>,
    control_audio_profiles: bool,
    /// Drives the local PipeWire card profile to follow ownership.
    router: Router,
    /// Latest local playback reading, replayed after a link opens.
    last_playback: Option<PlaybackReading>,
    /// In-ear detection driving the local media players.
    ear_media: EarMediaState,
    /// Bumped whenever this host does not own the audio; a profile claim in
    /// flight stops retrying once it changes.
    claim_epoch: Arc<AtomicU64>,

    /// Rejoin after an Apple host evicts this host.
    rejoin: RejoinState,
    rejoin_tx: mpsc::Sender<(u64, rejoin::Outcome)>,
    rejoin_rx: mpsc::Receiver<(u64, rejoin::Outcome)>,
    /// Sequence allowed to call Device1.Connect; 0 after a cancel.
    rejoin_epoch: Arc<AtomicU64>,
    known_hosts_path: Option<PathBuf>,
    control_link: bool,

    /// Page the AirPods when their proximity advert says the case opened.
    autoconnect: AutoConnectState,
    autoconnect_seen_rx: Option<mpsc::Receiver<Sighting>>,
    autoconnect_tx: mpsc::Sender<(u64, rejoin::Outcome)>,
    autoconnect_rx: mpsc::Receiver<(u64, rejoin::Outcome)>,
    /// Sequence allowed to call Device1.Connect; 0 after a cancel.
    autoconnect_epoch: Arc<AtomicU64>,
    autoconnect_scan_tx: Option<watch::Sender<bool>>,
    autoconnect_control_link: bool,

    /// The last two distinct primary bud addresses from 0x0C reports, oldest
    /// first, each with the time it first appeared. Cleared when a new link
    /// session starts: a switch seen on an earlier link says nothing about
    /// this one.
    bud_addresses: Vec<(Address, Instant)>,
    /// The reconnect sequence currently published, with the reason decided
    /// once at the drop and the start time it keeps for its whole life.
    link_sequence: Option<LinkSequence>,
    /// Last value handed to the store, so an unchanged view stays quiet.
    link_published: Option<LinkActivity>,
}

/// A reconnect sequence the supervisor is publishing.
#[derive(Debug, Clone)]
struct LinkSequence {
    /// True for a rejoin, false for proximity auto-connect.
    rejoin: bool,
    /// Sequence number inside that module; it identifies the sequence, so a
    /// new one that starts in the same event gets its own start time.
    seq: u64,
    /// Published reason. Decided when the sequence starts, because the
    /// evidence for it (the bud address history) ages out.
    reason: LinkReason,
    /// RFC3339 UTC start.
    since: String,
}

/// Choose the set-features variant for a dial.
///
/// The environment wins; then a variant already proven on this device; then
/// alternation, so a session that ends without a single battery packet is
/// followed by a dial on the other variant.
const fn pick_variant(
    forced: Option<FeaturesVariant>,
    locked: Option<FeaturesVariant>,
    attempt: u32,
) -> FeaturesVariant {
    match (forced, locked) {
        (Some(v), _) | (None, Some(v)) => v,
        (None, None) => {
            if attempt % 2 == 0 {
                FeaturesVariant::D7
            } else {
                FeaturesVariant::Alt
            }
        }
    }
}

/// Which acknowledgement the opening sequence is waiting for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Awaited {
    /// `01 00 04 00 ...`, the answer to the handshake.
    Handshake,
    /// `04 00 04 00 2b 00 ...`, the answer to set-features.
    Features,
}

/// Result of waiting for one opening-sequence acknowledgement.
enum AckWait {
    /// The expected ack arrived.
    Got,
    /// The budget expired; nothing was wrong with the socket.
    TimedOut,
    /// The socket failed or the peer hung up.
    Failed(std::io::Error),
    /// BlueZ changed identity or link state while the opening sequence ran.
    Cancelled,
}

/// Why the sole permitted dial in the current state was armed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DialKind {
    /// One attempt following an observed local `Connected: false -> true`.
    Automatic,
    /// One attempt explicitly rearmed by a user command.
    Manual,
}

/// What the idle watchdog wants done, given the clock and the two timestamps.
///
/// Split out from [`Supervisor::on_tick`] so the decision can be unit tested
/// without a socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IdleAction {
    /// Nothing to do.
    Nothing,
    /// Silence has gone on too long; poke the accessory.
    Probe,
    /// The probe was not answered; the link is dead.
    Recycle,
}

fn idle_decision(
    now: Instant,
    last_packet: Option<Instant>,
    probe_at: Option<Instant>,
) -> IdleAction {
    match probe_at {
        // A probe is outstanding: anything heard since it went out clears it.
        Some(sent) => {
            if last_packet.is_some_and(|t| t > sent) {
                IdleAction::Nothing
            } else if now.duration_since(sent) >= IDLE_PROBE_GRACE {
                IdleAction::Recycle
            } else {
                IdleAction::Nothing
            }
        }
        None => {
            if last_packet.is_some_and(|t| now.duration_since(t) >= IDLE_TIMEOUT) {
                IdleAction::Probe
            } else {
                IdleAction::Nothing
            }
        }
    }
}

/// The AirPods are put away: both buds report the case. One bud in the case
/// while the other is in an ear or merely out of it is a bud swap or a single
/// bud charging, and the AirPods stay in use, so it must not count.
/// `EarState::Unknown` is not the case either.
fn both_buds_in_case(primary: EarState, secondary: EarState) -> bool {
    primary == EarState::Case && secondary == EarState::Case
}

fn settings_model_error(snapshot: &Snapshot) -> Option<String> {
    match snapshot.device.model_id.as_str() {
        "201B" => None,
        "" => Some(
            "device model is unknown; settings and rename require AirPods 4 (ANC) model 201B"
                .into(),
        ),
        model => Some(format!(
            "unsupported device model {model}; settings and rename require AirPods 4 (ANC) model 201B"
        )),
    }
}

async fn send_timed(sock: &impl AapSocket, packet: &[u8]) -> std::io::Result<()> {
    trace!(len = packet.len(), "AAP send");
    match tokio::time::timeout(SEND_TIMEOUT, sock.send(packet)).await {
        Ok(Ok(sent)) if sent == packet.len() => Ok(()),
        Ok(Ok(_)) => Err(std::io::Error::new(
            std::io::ErrorKind::WriteZero,
            "AAP datagram was not sent in full",
        )),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "AAP send timed out after 1500 ms",
        )),
    }
}

// The accessory stores a setting write silently and never echoes the control
// packet, and nothing sent on the open link makes it repeat its settings dump:
// not set-features (either byte), not request-notifications, not the whole
// handshake triple. Only a fresh L2CAP link dumps again, so exactly one
// datagram goes out here and the readback is a link reopen. Never retried.
async fn send_setting(sock: &impl AapSocket, packet: &[u8]) -> std::io::Result<()> {
    send_timed(sock, packet).await?;
    debug!(
        bytes = packet.len(),
        "AirPods setting datagram sent; readback armed"
    );
    Ok(())
}

impl Supervisor {
    /// Build the supervisor. Nothing happens until [`Self::run`] is awaited.
    pub fn new(
        store: Arc<Store>,
        cfg: SessionConfig,
        link_rx: mpsc::Receiver<LinkEvent>,
        cmd_rx: mpsc::Receiver<Command>,
    ) -> Self {
        let (rejoin_tx, rejoin_rx) = mpsc::channel(4);
        let (autoconnect_tx, autoconnect_rx) = mpsc::channel(4);
        Self {
            store,
            cfg,
            link_rx,
            cmd_rx,
            adapter: None,
            device: None,
            pinned: false,
            acl: false,
            sock: None,
            dial_at: None,
            dial_kind: None,
            link_generation: 0,
            auto_suppressed: false,
            handshake_at: None,
            last_packet: None,
            battery_seen: false,
            notif_resends: 0,
            idle_probe_at: None,
            verify_at: None,
            verify_budget: None,
            readback_at: None,
            dial_attempt: 0,
            active_variant: FeaturesVariant::D7,
            locked_variant: None,
            handoff: HandoffState::new(false, true),
            handoff_published: Handoff::default(),
            handoff_actions: Vec::new(),
            transports: "none".to_owned(),
            bt_name: codec::DEFAULT_BT_NAME.to_owned(),
            config_path: None,
            playback_rx: None,
            mpris_tx: None,
            control_audio_profiles: false,
            router: Router::new(),
            last_playback: None,
            ear_media: EarMediaState::new(EarConfig::default()),
            claim_epoch: Arc::new(AtomicU64::new(0)),
            rejoin: RejoinState::new(false, true, Vec::new(), Arc::default()),
            rejoin_tx,
            rejoin_rx,
            rejoin_epoch: Arc::new(AtomicU64::new(0)),
            known_hosts_path: None,
            control_link: false,
            // Off until `with_autoconnect` says otherwise: a supervisor built
            // for tests must never reach for the radio.
            autoconnect: AutoConnectState::new(&AutoconnectConfig {
                enabled: false,
                ..AutoconnectConfig::default()
            }),
            autoconnect_seen_rx: None,
            autoconnect_tx,
            autoconnect_rx,
            autoconnect_epoch: Arc::new(AtomicU64::new(0)),
            autoconnect_scan_tx: None,
            autoconnect_control_link: false,
            bud_addresses: Vec::new(),
            link_sequence: None,
            link_published: None,
        }
    }

    /// Set the `[ear]` policy. Ear-detection actions only ever reach the
    /// desktop through the MPRIS task, so a supervisor built without one
    /// decides and does nothing.
    pub fn with_ear_media(mut self, config: EarConfig) -> Self {
        self.ear_media = EarMediaState::new(config);
        self
    }

    /// Enable BLE proximity auto-connect. See [`AutoConnectOptions`].
    pub fn with_autoconnect(mut self, options: AutoConnectOptions) -> Self {
        self.autoconnect = AutoConnectState::new(&options.config);
        self.autoconnect_seen_rx = Some(options.seen_rx);
        self.autoconnect_control_link = options.control_link;
        // The scanner starts as soon as this says so; the link state has not
        // been observed yet, and a Connected event stops it again.
        let _ = options.scan_tx.send(self.autoconnect.should_scan());
        self.autoconnect_scan_tx = Some(options.scan_tx);
        self
    }

    /// Enable handoff inputs and side effects. See [`HandoffOptions`].
    pub fn with_handoff(mut self, options: HandoffOptions) -> Self {
        self.handoff.enabled = options.config.enabled;
        self.handoff.take_over_on_play = options.config.take_over_on_play;
        self.bt_name = options.config.bt_name().to_owned();
        self.config_path = options.config_path;
        self.playback_rx = options.playback_rx;
        self.mpris_tx = options.mpris_tx;
        self.control_audio_profiles = options.control_audio_profiles;
        let known = options
            .known_hosts_path
            .as_deref()
            .map(rejoin::load_known_hosts)
            .unwrap_or_default();
        let local = self.rejoin.local;
        self.rejoin = RejoinState::new(
            options.config.enabled,
            options.config.rejoin_after_eviction,
            known,
            options.apple_ouis.unwrap_or_default(),
        );
        self.rejoin.local = local;
        self.known_hosts_path = options.known_hosts_path;
        self.control_link = options.control_link;
        self.publish_handoff();
        self
    }

    /// Drive the state machine until both input channels close.
    pub async fn run(mut self) {
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

        loop {
            let sock = self.sock.clone();
            let wake = tokio::select! {
                biased;
                ev = self.link_rx.recv() => match ev {
                    Some(ev) => Wake::Link(ev),
                    None => return,
                },
                cmd = self.cmd_rx.recv() => match cmd {
                    Some(cmd) => Wake::Cmd(cmd),
                    None => return,
                },
                r = recv_playback(&mut self.playback_rx) => Wake::Playback(r),
                Some((seq, outcome)) = self.rejoin_rx.recv() => Wake::Rejoin(seq, outcome),
                Some((seq, outcome)) = self.autoconnect_rx.recv() => Wake::AutoConnect(seq, outcome),
                Some(sighting) = recv_sighting(&mut self.autoconnect_seen_rx) => Wake::Seen(sighting),
                r = recv_one(sock) => Wake::Recv(r),
                _ = tick.tick() => Wake::Tick,
            };

            match wake {
                Wake::Link(ev) => self.on_link(ev),
                Wake::Cmd(cmd) => self.on_command(cmd).await,
                Wake::Recv(Ok(bytes)) if bytes.is_empty() => {
                    self.suppress_automatic("peer closed the AAP socket");
                }
                Wake::Recv(Ok(bytes)) => self.on_packet(&bytes),
                Wake::Recv(Err(e)) => {
                    warn!(error = %e, errno = ?e.raw_os_error(), "AAP recv failed");
                    self.suppress_automatic("recv error");
                }
                Wake::Playback(Some(reading)) => self.on_playback(reading),
                Wake::Playback(None) => self.playback_rx = None,
                Wake::Rejoin(seq, outcome) => {
                    self.rejoin_event(RejoinEvent::ConnectFinished { seq, outcome });
                }
                Wake::Seen(sighting) => {
                    self.autoconnect_event(AutoEvent::Seen {
                        rssi: sighting.rssi,
                    });
                }
                Wake::AutoConnect(seq, outcome) => {
                    self.autoconnect_event(AutoEvent::ConnectFinished { seq, outcome });
                }
                Wake::Tick => self.on_tick().await,
            }
            let _ = self.flush_handoff().await;
        }
    }

    fn on_link(&mut self, ev: LinkEvent) {
        match ev {
            LinkEvent::Identity {
                adapter,
                address,
                name,
                model_id,
                pinned,
            } => {
                let changed = self.adapter != Some(adapter)
                    || self.device != Some(address)
                    || self.pinned != pinned;
                if changed {
                    // A new identity has no authority inherited from the old
                    // one. In-flight opening work observes this generation.
                    self.link_generation = self.link_generation.wrapping_add(1);
                    self.acl = false;
                    self.pinned = pinned;
                    self.dial_at = None;
                    self.dial_kind = None;
                    self.drop_socket();
                    self.store.apply(Update::AclConnected(false));
                    self.rejoin_event(RejoinEvent::IdentityChanged);
                }
                self.adapter = Some(adapter);
                self.rejoin.local = Some(adapter);
                self.device = Some(address);
                self.handoff.local = Some(adapter);
                self.store.apply(Update::Identity {
                    address: address.to_string(),
                    name,
                    model_id,
                });
            }
            LinkEvent::AdapterAppleId(apple) => {
                self.handoff.apple_host_id = apple;
                self.publish_handoff();
            }
            LinkEvent::AudioTransport => self.handoff_event(HandoffEvent::AudioLinkChanged),
            LinkEvent::A2dpTransport { active, transports } => {
                info!(active, %transports, "A2DP transport streaming state");
                self.transports = transports;
                self.handoff_event(HandoffEvent::A2dpTransport { active });
            }
            LinkEvent::HandsFreeTransport { transports } => {
                info!(%transports, "audio is on the hands-free profile");
            }
            LinkEvent::Disconnected {
                reason,
                message: _,
                facts,
            } => {
                let lid_closed = self.store.snapshot().lid == Lid::Closed;
                let reason = rejoin::Reason::from_bluez(&reason);
                if matches!(reason, rejoin::Reason::Remote | rejoin::Reason::Timeout) {
                    // Logged before deciding, which consumes the report.
                    let evidence = self
                        .rejoin
                        .device_report_evidence(std::time::Instant::now());
                    let age_ms = evidence
                        .map_or_else(|| "none".to_owned(), |e| e.age.as_millis().to_string());
                    let apple_host = evidence.and_then(|e| e.apple_host);
                    let link_up = evidence.and_then(|e| e.apple_host_link_up);
                    info!(
                        reason = ?reason,
                        last_report_age_ms = %age_ms,
                        apple_host_match = apple_host.map_or("none", |(_, m)| m.as_str()),
                        apple_host = ?apple_host.map(|(host, _)| host),
                        apple_host_link_up = ?link_up.map(|(host, _)| host),
                        own_link_reported_down = evidence.is_some_and(|e| e.own_link_down),
                        listed_hosts = ?self.rejoin.last_report_others(),
                        "disconnect: last AirPods connected-devices report"
                    );
                }
                // The card is gone with the device, and the profile it was
                // on says nothing about the next connection.
                self.router.cancel();
                self.autoconnect_event(AutoEvent::LocalDisconnected {
                    reason: reason.clone(),
                });
                self.rejoin_event(RejoinEvent::Disconnected {
                    reason,
                    facts,
                    lid_closed,
                });
            }
            LinkEvent::Connected(true) => {
                self.rejoin_event(RejoinEvent::Connected(true));
                self.autoconnect_event(AutoEvent::LocalConnected);
                self.handoff_event(HandoffEvent::AudioLinkChanged);
                self.store.apply(Update::AclConnected(true));
                if !self.acl {
                    self.acl = true;
                    self.dial_attempt = 0;
                    self.link_generation = self.link_generation.wrapping_add(1);
                    // A real false->true observation begins a new local-link
                    // period. It is the only automatic rearm signal.
                    self.auto_suppressed = false;
                    // A 0x002E report from the previous link says nothing
                    // about this one.
                    self.bud_addresses.clear();
                    self.rejoin_event(RejoinEvent::LinkSessionStarted);
                    self.arm_dial(DialKind::Automatic, SETTLE);
                    info!("device connected locally; one guarded AAP attempt armed");
                }
            }
            LinkEvent::WatchEnded => {
                // Only the D-Bus subscription ended. The link, the adapter and
                // any scheduled rejoin are untouched: the rejoin wait and its
                // Device1.Connect live in this task, not in the watcher, so
                // they survive the rebuild. The rebuilt watcher re-sends
                // Identity and the current Connected state.
                info!("BlueZ device watch ended; the watcher rebuilds it");
            }
            ev @ (LinkEvent::Connected(false) | LinkEvent::AdapterGone) => {
                self.rejoin_event(if matches!(ev, LinkEvent::AdapterGone) {
                    RejoinEvent::AdapterGone
                } else {
                    RejoinEvent::Connected(false)
                });
                // The `Disconnected` signal carries the reason and usually
                // arrives first; this only makes sure the link state and the
                // absence clock follow a bare property change too.
                self.autoconnect_event(AutoEvent::LocalDisconnected {
                    reason: rejoin::Reason::Unknown,
                });
                self.handoff_event(HandoffEvent::AudioLinkChanged);
                let changed = self.acl || self.adapter.is_some();
                self.acl = false;
                self.dial_at = None;
                self.dial_kind = None;
                if changed {
                    self.link_generation = self.link_generation.wrapping_add(1);
                }
                self.drop_socket();
                self.store.apply(Update::AclConnected(false));
            }
        }
    }

    fn drop_socket(&mut self) {
        self.sock.take();
        // Opening packets can arrive before a socket is promoted into `sock`.
        // Always invalidate AAP-derived state when that tentative opening ends.
        self.store.apply(Update::AapLink(false));
        self.handshake_at = None;
        self.battery_seen = false;
        self.notif_resends = 0;
        self.idle_probe_at = None;
        // A link that ended has no dump left to finish.
        self.readback_at = None;
        self.handoff_event(HandoffEvent::LinkClosed);
        self.ear_event(EarEvent::LinkClosed);
        // A yield left the card off. With the AAP link gone there is no
        // handoff logic left to put it back, so a user would find the AirPods
        // silent: restore the A2DP profile while the device is still there.
        if self.router.mode() == Some(RouteMode::Yielded) {
            if self.acl {
                self.route(RouteMode::Own, "AAP link closed while yielded");
            } else {
                self.router.cancel();
            }
        }
    }

    fn arm_dial(&mut self, kind: DialKind, delay: Duration) {
        self.dial_kind = Some(kind);
        self.dial_at = Some(Instant::now() + delay);
    }

    /// Stop unattended recovery for this local-link period. A vanished peer,
    /// failed write, or unanswered watchdog cannot distinguish a reset from a
    /// host handoff, so retrying might reclaim audio from that other host.
    fn suppress_automatic(&mut self, reason: &str) {
        warn!(
            reason,
            "AAP recovery suppressed until a new local link observation or explicit command"
        );
        self.auto_suppressed = true;
        self.dial_at = None;
        self.dial_kind = None;
        self.drop_socket();
    }

    async fn fresh_local_connected(&self) -> Result<bool, String> {
        let (Some(adapter), Some(device)) = (self.adapter, self.device) else {
            return Err("selected Bluetooth device is not known yet".into());
        };
        match tokio::time::timeout(CONNECT_TIMEOUT, bluez::locally_connected(adapter, device)).await
        {
            Ok(Ok(connected)) => Ok(connected),
            Ok(Err(e)) => Err(format!("could not verify BlueZ local Connected state: {e}")),
            Err(_) => Err("timed out verifying BlueZ local Connected state".into()),
        }
    }

    /// Consume a fresh-check result without opening an L2CAP socket. Keeping
    /// this gate separate gives tests a checker seam with no D-Bus dependency.
    fn authorize_dial(
        &mut self,
        kind: DialKind,
        generation: u64,
        fresh: Result<bool, String>,
    ) -> bool {
        match fresh {
            Ok(true) if self.opening_alive(generation) => true,
            Ok(true) => {
                warn!("link changed while verifying local Connected; AAP dial cancelled");
                false
            }
            Ok(false) => {
                warn!(
                    ?kind,
                    "fresh BlueZ check says device is not locally connected"
                );
                if matches!(kind, DialKind::Automatic) {
                    self.suppress_automatic("fresh local Connected check failed");
                }
                false
            }
            Err(e) => {
                warn!(error = %e, ?kind, "cannot verify local BlueZ link before AAP dial");
                if matches!(kind, DialKind::Automatic) {
                    self.suppress_automatic("fresh local Connected check was unavailable");
                }
                false
            }
        }
    }

    /// `reconnect` reopens only an already-local ACL. `connect-once` is the
    /// one explicit exception: it asks BlueZ to connect the pinned paired
    /// accessory once, then waits for the watcher to authorize AAP normally.
    fn command_is_current(
        &self,
        context: &ConnectionContext,
        deadline: Instant,
        reply: &tokio::sync::oneshot::Sender<Response>,
    ) -> Result<(), &'static str> {
        if reply.is_closed() {
            return Err("request client disconnected");
        }
        if Instant::now() >= deadline {
            return Err("request expired before execution");
        }
        if self.store.connection_context() != *context {
            return Err("request context changed before execution");
        }
        Ok(())
    }

    async fn rearm_manual(
        &mut self,
        require_pinned: bool,
        context: &ConnectionContext,
        deadline: Instant,
        reply: &tokio::sync::oneshot::Sender<Response>,
    ) -> Response {
        if require_pinned && !self.pinned {
            return Response::error(
                "connect-once requires aurisd to be started with a pinned device address",
            );
        }
        if require_pinned {
            let (Some(adapter), Some(device)) = (self.adapter, self.device) else {
                return Response::error("selected Bluetooth device is not known yet");
            };
            let paired = match tokio::time::timeout_at(
                deadline,
                bluez::paired_device(adapter, device),
            )
            .await
            {
                Ok(Ok(paired)) => paired,
                Ok(Err(e)) => {
                    return Response::error(format!("could not verify pinned device: {e}"));
                }
                Err(_) => return Response::error("timed out verifying pinned device"),
            };
            self.drain_link_events();
            if let Err(reason) = self.command_is_current(context, deadline, reply) {
                return Response::error(reason);
            }
            let Some(paired) = paired else {
                return Response::error(
                    "pinned device is unavailable or not paired; BlueZ connect was not attempted",
                );
            };
            // No further device lookup between the authority check and this
            // one side effect. A request already issued to BlueZ cannot be
            // recalled by a later timeout; never retry it automatically.
            return match tokio::time::timeout_at(deadline, paired.connect()).await {
                Ok(Ok(())) => Response::ok(),
                Ok(Err(e)) => Response::error(format!("BlueZ connect-once failed: {e}")),
                Err(_) => Response::error(
                    "BlueZ connect-once timed out; it may still complete; aurisd will not retry",
                ),
            };
        }
        match self.fresh_local_connected().await {
            Ok(true) => {
                self.drain_link_events();
                if let Err(reason) = self.command_is_current(context, deadline, reply) {
                    return Response::error(reason);
                }
                self.reopen_local_link();
                Response::ok()
            }
            Ok(false) => Response::error(
                "device is not connected locally; aurisd will not ask BlueZ to connect it",
            ),
            Err(e) => Response::error(e),
        }
    }

    async fn on_command(&mut self, cmd: Command) {
        if let Err(reason) = self.command_is_current(&cmd.context, cmd.deadline, &cmd.reply) {
            if !cmd.reply.is_closed() {
                let _ = cmd.reply.send(Response::error(reason));
            }
            return;
        }
        let Command {
            request,
            reply,
            context,
            deadline,
        } = cmd;
        let response = match request {
            Request::Status => Response::Status(Box::new(self.store.snapshot())),
            // ctl_server takes the connection over before dispatch, so this is
            // unreachable; answer rather than panic if that ever stops holding.
            Request::Subscribe => Response::error("subscribe is served by the control server"),
            Request::Reconnect => {
                self.rejoin_event(RejoinEvent::ManualCommand);
                self.autoconnect_event(AutoEvent::ManualCommand);
                self.rearm_manual(false, &context, deadline, &reply).await
            }
            Request::ConnectOnce => {
                self.rejoin_event(RejoinEvent::ManualCommand);
                self.autoconnect_event(AutoEvent::ManualCommand);
                self.rearm_manual(true, &context, deadline, &reply).await
            }
            Request::TakeOver => self.manual_handoff(HandoffEvent::TakeOver).await,
            Request::Yield => self.manual_handoff(HandoffEvent::Yield).await,
            Request::SetHandoff { enabled } => self.set_handoff(enabled),
            Request::SetNoiseControl { value } => {
                info!(mode = ?value, "setting noise control");
                self.send_control(
                    codec::encode_set_noise_control(value),
                    Update::NoiseControl(value),
                )
                .await
            }
            Request::SetConversationalAwareness { value } => {
                info!(on = value, "setting conversational awareness");
                self.send_control(
                    codec::encode_set_conversational_awareness(value),
                    Update::ConversationalAwareness(value),
                )
                .await
            }
            Request::SetAdaptiveLevel { value } => {
                let value = value.min(100);
                self.send_control(
                    codec::encode_set_adaptive_level(value),
                    Update::AdaptiveLevel(value),
                )
                .await
            }
            Request::SetSetting { setting } => {
                let packet = match codec::encode_set_setting(&setting) {
                    Ok(packet) => packet,
                    Err(error) => {
                        let _ = reply.send(Response::error(error));
                        return;
                    }
                };
                info!(key = setting.key(), "sending AirPods setting");
                self.send_setting_write(setting, packet).await
            }
            Request::Rename { name } => {
                let packet = match codec::encode_rename(&name) {
                    Ok(packet) => packet,
                    Err(error) => {
                        let _ = reply.send(Response::error(error));
                        return;
                    }
                };
                info!(bytes = name.len(), "sending AirPods rename");
                self.send_rename(name, packet).await
            }
        };
        let _ = reply.send(response);
    }

    /// Drop the socket and arm one immediate manual dial. Shared by the
    /// `reconnect` request and by the settings readback, which must reopen the
    /// AAP link exactly the same way rather than through a BlueZ reconnect.
    fn reopen_local_link(&mut self) {
        self.drop_socket();
        self.arm_dial(DialKind::Manual, Duration::ZERO);
    }

    /// Send a rename and arm the same readback a setting write uses. `ok`
    /// means only that the datagram was sent. The accessory never acknowledges
    /// a rename, and BlueZ re-reads the remote name only when a link opens, so
    /// the confirmation is the accessory's own 0x1D metadata on the reopened
    /// link.
    async fn send_rename(&mut self, name: String, packet: Vec<u8>) -> Response {
        let Some(sock) = self.sock.clone() else {
            return Response::error("device is not connected: no AAP link is open");
        };
        if let Some(error) = settings_model_error(&self.store.snapshot()) {
            return Response::error(error);
        }
        match send_timed(sock.as_ref(), &packet).await {
            Ok(()) => {
                let now = Instant::now();
                self.store.apply(Update::RenameRequested(name));
                self.verify_at = Some(now + VERIFY_DEBOUNCE);
                self.verify_budget = Some(now + VERIFY_BUDGET);
                Response::ok()
            }
            Err(e) => {
                warn!(error = %e, "failed to send rename packet");
                self.suppress_automatic("rename send failed");
                Response::error(format!("failed to send command: {e}"))
            }
        }
    }

    /// Send one setting write and arm its readback. The accessory stores the
    /// value silently, so the only confirmation available is the dump on a
    /// fresh link; the debounce lets a burst of writes share one reopen.
    async fn send_setting_write(
        &mut self,
        setting: crate::settings::SettingCommand,
        packet: Vec<u8>,
    ) -> Response {
        let Some(sock) = self.sock.clone() else {
            return Response::error("device is not connected: no AAP link is open");
        };
        if let Some(error) = settings_model_error(&self.store.snapshot()) {
            return Response::error(error);
        }
        match send_setting(sock.as_ref(), &packet).await {
            Ok(()) => {
                let now = Instant::now();
                self.store.apply(Update::SettingRequested(setting));
                self.verify_at = Some(now + VERIFY_DEBOUNCE);
                self.verify_budget = Some(now + VERIFY_BUDGET);
                Response::ok()
            }
            Err(e) => {
                warn!(error = %e, "failed to send setting packet");
                self.suppress_automatic("setting send failed");
                Response::error(format!("failed to send command: {e}"))
            }
        }
    }

    /// End the readback window and publish the comparison.
    fn finish_readback(&mut self) {
        self.readback_at = None;
        self.verify_budget = None;
        self.store.apply(Update::VerifyCompleted);
        // Handoff treats reports before this (and at least 3 s) as state.
        self.handoff_event(HandoffEvent::OpeningSettled);
    }

    fn abandon_readback(&mut self, reason: &str) {
        warn!(
            reason,
            "settings readback abandoned; the write itself stands"
        );
        self.verify_at = None;
        self.verify_budget = None;
        self.store.apply(Update::VerifyFailed);
    }

    /// Timers that must run whether or not a link is currently up.
    async fn tick_verification(&mut self) {
        let now = Instant::now();
        if self.readback_at.is_some_and(|t| now >= t) {
            debug!("settings dump window closed without a battery packet");
            self.finish_readback();
        }
        if self.verify_at.is_some_and(|t| now >= t) {
            self.verify_at = None;
            self.begin_verify_reopen().await;
        }
        if self.verify_budget.is_some_and(|t| now >= t) {
            self.abandon_readback("readback did not finish within 10 s");
        }
    }

    /// Reopen the AAP link for readback only, the same way `reconnect` does.
    async fn begin_verify_reopen(&mut self) {
        if !self.acl && self.sock.is_none() {
            self.abandon_readback("no AAP link to reopen");
            return;
        }
        match self.fresh_local_connected().await {
            Ok(true) => {
                self.drain_link_events();
                if !self.acl {
                    self.abandon_readback("link went away before the readback reopen");
                    return;
                }
                info!("reopening the AAP link to read the new settings back");
                // Published before the socket drops: the link-loss path must
                // not read this deliberate reopen as a failed readback, and
                // the UI keeps showing the device as connected.
                self.store.apply(Update::VerifyReopening);
                self.reopen_local_link();
            }
            Ok(false) => self.abandon_readback("device is no longer connected locally"),
            Err(e) => {
                warn!(error = %e, "cannot verify the local link before a readback reopen");
                self.abandon_readback("could not verify the local BlueZ link");
            }
        }
    }

    async fn send_control(&mut self, packet: Vec<u8>, optimistic: Update) -> Response {
        let Some(sock) = self.sock.clone() else {
            return Response::error("device is not connected: no AAP link is open");
        };
        match send_timed(sock.as_ref(), &packet).await {
            Ok(()) => {
                // The accessory echoes the new state on 0x0009, but reflect it
                // straight away so the widget does not lag behind the click.
                self.store.apply(optimistic);
                Response::ok()
            }
            Err(e) => {
                warn!(error = %e, "failed to send control packet");
                self.suppress_automatic("control send failed");
                Response::error(format!("failed to send command: {e}"))
            }
        }
    }

    fn on_packet(&mut self, bytes: &[u8]) {
        trace!(len = bytes.len(), "AAP recv");
        self.last_packet = Some(Instant::now());
        self.idle_probe_at = None;
        match codec::decode(bytes) {
            Ok(Packet::HandshakeAck) => debug!("handshake acknowledged"),
            Ok(Packet::FeaturesAck) => debug!("set-features acknowledged"),
            Ok(Packet::Battery(entries)) => {
                if self.locked_variant.is_none() {
                    info!(variant = ?self.active_variant, "battery received; pinning set-features variant");
                    self.locked_variant = Some(self.active_variant);
                }
                self.battery_seen = true;
                debug!(?entries, "battery");
                self.store.apply(Update::Battery(entries));
                if self.readback_at.is_some() {
                    debug!("first battery packet ends this link's settings dump");
                    self.finish_readback();
                }
            }
            Ok(Packet::EarDetection { primary, secondary }) => {
                // Never a reason to disconnect or reroute anything: the
                // AirPods handle their own radio link. It does drive the local
                // players, once the store has resolved primary and secondary
                // into left and right.
                self.rejoin_event(RejoinEvent::Ear {
                    both_in_case: both_buds_in_case(primary, secondary),
                });
                self.store.apply(Update::Ear { primary, secondary });
                let ear = self.store.snapshot().ear;
                self.ear_event(EarEvent::Ear(ear));
            }
            Ok(Packet::Control(ControlState::NoiseControl(m))) => {
                self.store.apply(Update::NoiseControl(m));
            }
            Ok(Packet::Control(ControlState::ConversationalAwareness(v))) => {
                self.store.apply(Update::ConversationalAwareness(v));
            }
            Ok(Packet::Control(ControlState::AdaptiveLevel(v))) => {
                self.store.apply(Update::AdaptiveLevel(v));
            }
            Ok(Packet::Control(ControlState::Setting(setting))) => {
                debug!(key = setting.key(), "AirPods setting report received");
                self.store.apply(Update::Setting(setting));
            }
            Ok(Packet::Control(ControlState::OwnsConnection(owns))) => {
                // The opening dump reports ownership as state; only a report
                // on an established link is a handoff.
                let opening = self.sock.is_none() || self.readback_at.is_some();
                info!(owns, opening, "AirPods ownership report");
                self.handoff_event(HandoffEvent::OwnsConnection { owns, opening });
                // The AirPods' own word on who has them, including the
                // opening dump after a connect or rejoin while another host
                // owns them, and a hijack by that host.
                if self.handoff.enabled {
                    if owns {
                        self.route(RouteMode::Own, "AirPods report this host owns them");
                    } else {
                        self.route(RouteMode::Yielded, "AirPods report another host owns them");
                    }
                }
            }
            Ok(Packet::Control(ControlState::Other { id, value: _ })) => {
                // Do not log raw values: settings reports can contain user
                // preferences. The control payload begins after the six-byte
                // AAP header, including any unmodelled trailing bytes.
                debug!(
                    id,
                    len = bytes.len().saturating_sub(6),
                    "unmodelled control echo"
                );
            }
            Ok(Packet::Metadata(md)) => {
                info!(
                    name = md.name.is_some(),
                    model = md.model.is_some(),
                    serial = md.serial.is_some(),
                    firmware = md.firmware.is_some(),
                    "device metadata"
                );
                self.store.apply(Update::Metadata(md));
            }
            Ok(Packet::ConversationalAwarenessLevel(level)) => {
                // Speech ducking while the feature is on. Earlier builds took
                // this for the on/off state and switched the toggle off every
                // time the wearer spoke.
                debug!(level, "conversational awareness speech event");
            }
            Ok(Packet::AddressReport { address, extra }) => {
                debug!(%address, extra = %hex_bytes(&extra), "AirPods address report");
                self.note_address_report(address, extra);
            }
            Ok(Packet::AudioSource { address, state }) => {
                debug!(%address, ?state, "AirPods audio source");
                self.handoff_event(HandoffEvent::AudioSource { address, state });
            }
            Ok(Packet::ConnectedDevices { header, devices }) => {
                let listed = devices
                    .iter()
                    .map(|d| format!("{} info={}", d.address, hex_bytes(&d.info)))
                    .collect::<Vec<_>>()
                    .join(", ");
                debug!(
                    count = devices.len(),
                    header = %hex_bytes(&header),
                    devices = %listed,
                    "AirPods connected devices"
                );
                let listed_hosts: Vec<rejoin::ListedHost> = devices
                    .iter()
                    .map(|d| rejoin::ListedHost::new(d.address, d.is_link_up()))
                    .collect();
                // The AirPods can mark this host's own entry 0x00 while the
                // report still arrives over that link, which happens when a
                // bud swap moves the link. Record it and do nothing: BlueZ
                // owns the link state, and a link that still carries packets
                // is not ours to tear down or to "recover".
                if self.rejoin.local.is_some_and(|local| {
                    listed_hosts
                        .iter()
                        .any(|h| h.address == local && !h.link_up)
                }) {
                    info!(
                        own_link_reported_down = true,
                        "AirPods report this host's link down while it still carries their \
                         reports; recording it as rejoin evidence only"
                    );
                }
                let addresses: Vec<Address> = devices.iter().map(|d| d.address).collect();
                self.rejoin_event(RejoinEvent::ConnectedDevices(listed_hosts));
                self.handoff_event(HandoffEvent::ConnectedDevices(addresses));
            }
            Ok(Packet::SmartRouting { sender, body }) => {
                let requests_yield = codec::smart_routing_requests_yield(&body);
                debug!(
                    %sender,
                    requests_yield,
                    len = body.len(),
                    hex = %hex_bytes(&body),
                    "smart routing relayed from another host"
                );
                self.rejoin_event(RejoinEvent::Relay { sender });
                self.handoff_event(HandoffEvent::SmartRouting {
                    sender,
                    requests_yield,
                });
            }
            Ok(Packet::Unknown { opcode, payload }) => {
                // Payload hex is the only way to identify an unmodelled reply
                // (for example a setting ack) without a root HCI trace.
                debug!(
                    opcode = format_args!("{opcode:#06x}"),
                    len = payload.len(),
                    hex = %hex_bytes(&payload),
                    "unknown AAP packet"
                );
            }
            Err(e) => debug!(
                error = %e,
                len = bytes.len(),
                hex = %hex_bytes(bytes),
                "undecodable AAP packet"
            ),
        }
    }

    async fn on_tick(&mut self) {
        self.handoff_event(HandoffEvent::Tick);
        self.ear_event(EarEvent::Tick);
        self.rejoin_event(RejoinEvent::Tick);
        self.autoconnect_event(AutoEvent::Tick);
        // Readback timers are independent of the link: their whole job is to
        // notice that one never came back.
        self.tick_verification().await;
        if !self.acl {
            return;
        }
        if self.sock.is_none() {
            if self.dial_at.is_some_and(|t| Instant::now() >= t) {
                self.dial().await;
            }
            return;
        }
        let now = Instant::now();
        if !self.battery_seen {
            if let Some(t) = self.handshake_at {
                if now.duration_since(t) >= BATTERY_WATCHDOG {
                    if self.notif_resends < MAX_NOTIF_RESENDS {
                        self.notif_resends += 1;
                        warn!(
                            attempt = self.notif_resends,
                            "no battery packet; re-requesting notifications"
                        );
                        self.handshake_at = Some(now);
                        let sock = self.sock.clone();
                        if let Some(s) = sock {
                            if let Err(e) =
                                send_timed(s.as_ref(), &codec::encode_request_notifications()).await
                            {
                                warn!(error = %e, "resend failed");
                                self.suppress_automatic("notification resend failed");
                            }
                        }
                    } else {
                        self.suppress_automatic("battery watchdog unanswered");
                    }
                    return;
                }
            }
        }
        match idle_decision(now, self.last_packet, self.idle_probe_at) {
            IdleAction::Nothing => {}
            IdleAction::Probe => {
                let Some(s) = self.sock.clone() else { return };
                debug!("no AAP traffic for 300 s; probing with request-notifications");
                self.idle_probe_at = Some(now);
                if let Err(e) = send_timed(s.as_ref(), &codec::encode_request_notifications()).await
                {
                    warn!(error = %e, "idle probe send failed");
                    self.suppress_automatic("idle probe send failed");
                }
            }
            IdleAction::Recycle => {
                self.suppress_automatic("idle probe went unanswered for 15 s");
            }
        }
    }

    async fn dial(&mut self) {
        // Events always win over a deadline. In particular, do not turn a
        // queued `Connected=false` into a kernel L2CAP connect attempt.
        self.drain_link_events();
        if !self.acl {
            return;
        }
        let Some(kind) = self.dial_kind.take() else {
            return;
        };
        let generation = self.link_generation;
        let (Some(adapter), Some(device)) = (self.adapter, self.device) else {
            self.suppress_automatic("dial reached without a selected Bluetooth identity");
            return;
        };
        self.dial_at = None;

        // The watcher is advisory; `Connected` is read again immediately
        // before dialing because L2CAP connect can initiate an ACL in-kernel.
        if !self.authorize_dial(kind, generation, self.fresh_local_connected().await) {
            return;
        }

        debug!(%device, psm = self.cfg.psm, raw = self.cfg.raw_socket, ?kind, "dialing guarded AAP");
        let connect = tokio::time::timeout(
            CONNECT_TIMEOUT,
            socket::connect(adapter, device, self.cfg.psm, self.cfg.raw_socket),
        );
        tokio::pin!(connect);
        let link = loop {
            tokio::select! {
                biased;
                ev = self.link_rx.recv() => match ev {
                    Some(ev) => {
                        self.on_link(ev);
                        if !self.opening_alive(generation) {
                            warn!("link changed while AAP connect was in flight; cancelling");
                            return;
                        }
                    }
                    None => return,
                },
                result = &mut connect => match result {
                    Ok(Ok(link)) => break link,
                    Ok(Err(e)) => {
                        warn!(error = %e, errno = ?e.raw_os_error(), "AAP connect failed");
                        self.suppress_automatic("AAP connect failed");
                        return;
                    }
                    Err(_) => {
                        warn!("AAP connect timed out");
                        self.suppress_automatic("AAP connect timed out");
                        return;
                    }
                },
            }
        };

        let sock = Arc::new(link);
        if !self.opening_alive(generation) {
            warn!("link changed after AAP connect; discarding socket");
            return;
        }
        // Ownership reports can arrive during the opening sequence.
        self.handoff_event(HandoffEvent::LinkOpened);
        if let Some(reading) = self.last_playback.clone() {
            // Prime the reading so a player already playing is not an edge.
            self.handoff_event(HandoffEvent::Playback {
                playing: reading.playing,
                app: reading.app,
                player: reading.player,
            });
        }
        // Cleared before the opening sequence, not after: a battery packet can
        // arrive while we are still waiting on an ack, and that is exactly the
        // evidence the variant fallback needs.
        self.battery_seen = false;
        self.notif_resends = 0;
        let variant = pick_variant(self.cfg.features, self.locked_variant, self.dial_attempt);
        let alt_order = matches!(variant, FeaturesVariant::Alt);
        info!(
            ?variant,
            attempt = self.dial_attempt,
            order = if alt_order {
                "subscribe-then-features"
            } else {
                "features-then-subscribe"
            },
            "AAP opening sequence"
        );
        self.active_variant = variant;
        self.dial_attempt += 1;

        // 1. Handshake, then wait for the accessory to answer it. The
        //    accessory ignores everything sent before it has acked, which is
        //    what the old fixed 250 ms spacing was guessing at.
        if !self.opening_alive(generation) {
            return;
        }
        if let Err(e) = send_timed(sock.as_ref(), &codec::encode_handshake()).await {
            warn!(error = %e, "handshake send failed");
            self.suppress_automatic("handshake send failed");
            return;
        }
        match self
            .await_ack(&*sock, Awaited::Handshake, HANDSHAKE_ACK_WAIT, generation)
            .await
        {
            AckWait::Got => debug!("handshake ack received"),
            AckWait::TimedOut => {
                warn!(?variant, "no handshake ack");
                self.suppress_automatic("no handshake ack");
                return;
            }
            AckWait::Failed(e) => {
                warn!(error = %e, "handshake ack wait failed");
                self.suppress_automatic("handshake ack wait failed");
                return;
            }
            AckWait::Cancelled => {
                warn!("link changed during handshake; AAP opening cancelled");
                return;
            }
        }

        // 2. The alternate variant subscribes before negotiating features,
        //    with a narrower first subscribe; the default order negotiates
        //    first. Nothing else differs.
        let pre: Vec<Vec<u8>> = if alt_order {
            vec![
                codec::encode_request_notifications_alt(),
                codec::encode_request_notifications(),
                codec::encode_set_features(variant),
            ]
        } else {
            vec![codec::encode_set_features(variant)]
        };
        for (i, packet) in pre.iter().enumerate() {
            if !self.opening_alive(generation) {
                return;
            }
            if let Err(e) = send_timed(sock.as_ref(), packet).await {
                warn!(error = %e, step = i, "AAP opening sequence failed");
                self.suppress_automatic("AAP opening sequence send failed");
                return;
            }
        }

        // 3. The features ack is advisory: some firmware never sends one, so a
        //    timeout is logged and the sequence continues.
        match self
            .await_ack(&*sock, Awaited::Features, FEATURES_ACK_WAIT, generation)
            .await
        {
            AckWait::Got => debug!("features ack received"),
            AckWait::TimedOut => info!("no set-features ack; continuing anyway"),
            AckWait::Failed(e) => {
                warn!(error = %e, "features ack wait failed");
                self.suppress_automatic("features ack wait failed");
                return;
            }
            AckWait::Cancelled => {
                warn!("link changed during feature negotiation; AAP opening cancelled");
                return;
            }
        }

        // 4. On the default order the subscribe comes last.
        if !alt_order {
            if !self.opening_alive(generation) {
                return;
            }
            if let Err(e) = send_timed(sock.as_ref(), &codec::encode_request_notifications()).await
            {
                warn!(error = %e, "request-notifications send failed");
                self.suppress_automatic("request-notifications send failed");
                return;
            }
        }

        if !self.opening_alive(generation) {
            warn!("link changed while opening AAP; discarding fresh socket");
            return;
        }

        info!(%device, ?variant, "AAP link up");
        let now = Instant::now();
        self.sock = Some(sock);
        // The battery watchdog counts from the last request-notifications,
        // which is the packet that was just sent.
        self.handshake_at = Some(now);
        self.last_packet = Some(now);
        self.store.apply(Update::AapLink(true));
        // Every link opens with one settings dump, whether or not this daemon
        // asked for it: a natural reconnection can settle an earlier write.
        self.readback_at = Some(now + READBACK_WINDOW);
    }

    /// Wait for one opening-sequence acknowledgement, handling every other
    /// frame that arrives meanwhile exactly as the running loop would.
    async fn await_ack<S: AapSocket + ?Sized>(
        &mut self,
        sock: &S,
        want: Awaited,
        budget: Duration,
        generation: u64,
    ) -> AckWait {
        let deadline = Instant::now() + budget;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return AckWait::TimedOut;
            }
            let mut buf = vec![0u8; RECV_BUF];
            let deadline = tokio::time::sleep(left);
            tokio::pin!(deadline);
            let n = tokio::select! {
                biased;
                ev = self.link_rx.recv() => match ev {
                    Some(ev) => {
                        self.on_link(ev);
                        if !self.opening_alive(generation) {
                            return AckWait::Cancelled;
                        }
                        continue;
                    }
                    None => return AckWait::Cancelled,
                },
                _ = &mut deadline => return AckWait::TimedOut,
                result = sock.recv(&mut buf) => match result {
                    Ok(n) => n,
                    Err(e) => return AckWait::Failed(e),
                },
            };
            if n == 0 {
                return AckWait::Failed(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "peer closed the AAP socket during the opening sequence",
                ));
            }
            buf.truncate(n);
            let hit = matches!(
                (want, codec::decode(&buf)),
                (Awaited::Handshake, Ok(Packet::HandshakeAck))
                    | (Awaited::Features, Ok(Packet::FeaturesAck))
            );
            self.on_packet(&buf);
            if hit {
                return AckWait::Got;
            }
        }
    }

    /// Fold one event into the handoff state and queue its side effects.
    fn handoff_event(&mut self, event: HandoffEvent) {
        let actions = handoff::decide(&mut self.handoff, event, std::time::Instant::now());
        for action in actions {
            match action {
                Action::Record { kind, peer } => {
                    info!(?kind, peer = ?peer.map(|p| p.to_string()), "handoff");
                    self.handoff.record(kind, peer, crate::now_rfc3339());
                }
                Action::RepausePlayer { ref player } => {
                    info!(
                        player = player.as_deref().unwrap_or("all"),
                        "local player resumed while another host owns the AirPods; pausing it again"
                    );
                    self.handoff_actions.push(action);
                }
                Action::StreamNotStarted => {
                    warn!(
                        transports = %self.transports,
                        "A2DP transport still idle 3 s after take-over"
                    );
                }
                other => self.handoff_actions.push(other),
            }
        }
        if !self.handoff.owns_audio() {
            self.claim_epoch.fetch_add(1, Ordering::Relaxed);
        }
        self.publish_handoff();
    }

    fn publish_handoff(&mut self) {
        let view = self.handoff.view();
        if view != self.handoff_published {
            self.handoff_published = view.clone();
            self.store.apply(Update::Handoff(view));
        }
    }

    /// What ear detection needs to know about the rest of the daemon. Local
    /// audio only counts as going to the AirPods while the link is up, the
    /// card has not been taken off them for a yield, and handoff, when it is
    /// on, says this host owns them.
    fn ear_context(&self) -> ear_media::Context {
        ear_media::Context {
            audio_on_airpods: self.acl
                && self.router.mode() != Some(RouteMode::Yielded)
                && (!self.handoff.enabled || self.handoff.owns_audio()),
        }
    }

    /// Fold one event into the ear-media state and perform what it decides.
    ///
    /// Deliberately not routed through `handoff_actions`: a pause made because
    /// a bud left the ear is not a yield and must never be read as one, and a
    /// resume must not look like a take-over request. The only side effects
    /// are MPRIS requests.
    fn ear_event(&mut self, event: EarEvent) {
        let ctx = self.ear_context();
        let actions = ear_media::decide(&mut self.ear_media, event, ctx, std::time::Instant::now());
        for action in actions {
            let cmd = match action {
                EarAction::PauseLocalPlayers => {
                    info!("a bud left the ear; pausing local playback");
                    MprisCommand::PausePlaying
                }
                EarAction::ResumePlayer { player: Some(name) } => {
                    info!(player = %name, "the bud is back in; resuming local playback");
                    MprisCommand::Play(name)
                }
                EarAction::ResumePlayer { player: None } => {
                    debug!("the bud is back in, but the paused player was never named");
                    continue;
                }
            };
            match &self.mpris_tx {
                Some(tx) if tx.try_send(cmd).is_err() => {
                    warn!("could not queue an ear-detection MPRIS request");
                }
                Some(_) => {}
                None => debug!("no MPRIS task; ear detection has nothing to drive"),
            }
        }
    }

    fn on_playback(&mut self, reading: PlaybackReading) {
        debug!(playing = reading.playing, "local playback reading");
        self.last_playback = Some(reading.clone());
        self.ear_event(EarEvent::Playback {
            playing: reading.playing,
            player: reading.player.clone(),
        });
        self.handoff_event(HandoffEvent::Playback {
            playing: reading.playing,
            app: reading.app,
            player: reading.player,
        });
    }

    /// Perform queued handoff side effects. Returns the first failure.
    async fn flush_handoff(&mut self) -> Option<String> {
        if self.handoff_actions.is_empty() {
            return None;
        }
        let actions = std::mem::take(&mut self.handoff_actions);
        let mut failure = None;
        for action in actions {
            let packet = match action {
                Action::SendOwnership(owns) => Some(codec::encode_owns_connection(owns)),
                Action::SendMediaInfo {
                    target,
                    streaming,
                    app,
                } => self.adapter.map(|local| {
                    codec::encode_media_info(target, local, &app, streaming, &self.bt_name)
                }),
                Action::SendNewDeviceMediaInfo { target } => self
                    .adapter
                    .map(|local| codec::encode_media_info_new_device(target, local, &self.bt_name)),
                Action::SendHijack { target } => Some(codec::encode_hijack_v2(target)),
                Action::SendNewTipi { target } => self
                    .adapter
                    .map(|local| codec::encode_new_tipi(target, local, &self.bt_name)),
                Action::PauseLocalPlayers => {
                    match &self.mpris_tx {
                        Some(tx) if tx.try_send(MprisCommand::PausePlaying).is_err() => {
                            warn!("could not queue a local pause");
                        }
                        Some(_) => {}
                        None => debug!("no MPRIS task; local players not paused"),
                    }
                    None
                }
                Action::RepausePlayer { player } => {
                    let cmd = player.map_or(MprisCommand::PausePlaying, MprisCommand::Pause);
                    match &self.mpris_tx {
                        Some(tx) if tx.try_send(cmd).is_err() => {
                            warn!("could not queue a local pause");
                        }
                        Some(_) => {}
                        None => debug!("no MPRIS task; local player not paused"),
                    }
                    None
                }
                Action::ClaimAudio => {
                    self.spawn_audio_claim();
                    None
                }
                Action::SetAudioRoute(RouteMode::Own) => {
                    self.route(RouteMode::Own, "take-over");
                    None
                }
                Action::SetAudioRoute(RouteMode::Yielded) => {
                    self.yield_route().await;
                    None
                }
                Action::StreamNotStarted | Action::Record { .. } => None,
            };
            let Some(packet) = packet else { continue };
            let Some(sock) = self.sock.clone() else {
                failure.get_or_insert_with(|| {
                    "device is not connected: no AAP link is open".to_owned()
                });
                continue;
            };
            if let Err(e) = send_timed(sock.as_ref(), &packet).await {
                warn!(error = %e, "handoff send failed");
                self.suppress_automatic("handoff send failed");
                failure.get_or_insert_with(|| format!("failed to send command: {e}"));
            }
        }
        failure
    }

    /// The audio half of a yield, in order: local playback stops, then the
    /// card comes off the AirPods, and only then does this return, so the
    /// ownership release queued behind it cannot go out before the transport
    /// has been released from this side. See [`crate::audio_route`].
    async fn yield_route(&mut self) {
        self.drain_local_playback().await;
        self.route(RouteMode::Yielded, "yield");
        if self.control_audio_profiles && self.device.is_some() {
            self.router.wait_settled(YIELD_ROUTE_WAIT).await;
        }
    }

    /// Wait for the pause queued ahead of this to actually stop the stream.
    ///
    /// Taking the card off the AirPods under a playing stream does not stop
    /// it: the audio server moves the stream to whatever sink is left, which
    /// is how a yield used to become audible on the laptop speakers. Draining
    /// first is what makes it silent. A player that ignores the pause is not
    /// waited for beyond [`DRAIN_TIMEOUT`]: the other host is waiting.
    async fn drain_local_playback(&mut self) {
        if self.playback_rx.is_none() {
            return;
        }
        let started = Instant::now();
        loop {
            let playing = self.last_playback.as_ref().is_some_and(|r| r.playing);
            match drain_state(playing, started.elapsed()) {
                Drain::Done => return,
                Drain::Expired => {
                    warn!("a local player did not stop; yielding anyway");
                    return;
                }
                Drain::Waiting => {}
            }
            let left = DRAIN_TIMEOUT.saturating_sub(started.elapsed());
            match tokio::time::timeout(left, recv_playback(&mut self.playback_rx)).await {
                Ok(Some(reading)) => self.on_playback(reading),
                Ok(None) => {
                    self.playback_rx = None;
                    return;
                }
                // The next turn of the loop reports the expiry.
                Err(_) => {}
            }
        }
    }

    /// Point the local PipeWire card at the AirPods, or off them. Gated by
    /// the same permission as BlueZ profile calls, so a supervisor built for
    /// tests never touches the desktop's audio.
    fn route(&mut self, mode: RouteMode, reason: &'static str) {
        if !self.control_audio_profiles {
            debug!(
                reason,
                "audio profile control is off; the card profile stands"
            );
            return;
        }
        let Some(device) = self.device else {
            debug!(reason, "no device address yet; the card profile stands");
            return;
        };
        self.router.set(&device.to_string(), mode, reason);
    }

    /// Connect A2DP in the background: ConnectProfile can take seconds and
    /// must not stall the socket loop. A busy link is retried after each
    /// [`CLAIM_RETRY`] wait, but never once ownership has been lost; a newer
    /// claim supersedes one in flight. A profile is never disconnected: see
    /// docs/BATTERY_AND_HANDOFF.md.
    fn spawn_audio_claim(&self) {
        if !self.control_audio_profiles {
            debug!("audio profile control is off for this supervisor");
            return;
        }
        let (Some(adapter), Some(device)) = (self.adapter, self.device) else {
            return;
        };
        if !self.handoff.owns_audio() {
            return;
        }
        let epoch = Arc::clone(&self.claim_epoch);
        let mine = epoch.fetch_add(1, Ordering::Relaxed) + 1;
        tokio::spawn(async move {
            let mut waits = CLAIM_RETRY.iter();
            loop {
                if epoch.load(Ordering::Relaxed) != mine {
                    debug!("ownership lost; audio profile claim abandoned");
                    return;
                }
                match tokio::time::timeout(PROFILE_TIMEOUT, bluez::claim_audio(adapter, device))
                    .await
                {
                    Ok(Ok(())) => {
                        info!("audio profile connected for take-over");
                        return;
                    }
                    Ok(Err(e)) if bluez::profile_busy(&e) => match waits.next() {
                        Some(wait) => {
                            debug!(error = %e, ?wait, "audio profile busy; retrying");
                            tokio::time::sleep(*wait).await;
                        }
                        None => {
                            warn!(error = %e, "audio profile still busy after retries; giving up");
                            return;
                        }
                    },
                    Ok(Err(e)) => {
                        warn!(error = %e, "audio profile connect failed");
                        return;
                    }
                    Err(_) => {
                        warn!("audio profile connect timed out");
                        return;
                    }
                }
            }
        });
    }

    async fn manual_handoff(&mut self, event: HandoffEvent) -> Response {
        if self.sock.is_none() {
            return Response::error("device is not connected: no AAP link is open");
        }
        if self.adapter.is_none() {
            return Response::error("local Bluetooth adapter is not known yet");
        }
        self.handoff_event(event);
        match self.flush_handoff().await {
            None => Response::ok(),
            Some(error) => Response::error(error),
        }
    }

    fn set_handoff(&mut self, enabled: bool) -> Response {
        let Some(path) = self.config_path.clone() else {
            return Response::error("no configuration file path; handoff setting was not changed");
        };
        if let Err(e) = config::set_handoff_enabled(&path, enabled) {
            return Response::error(format!("could not save handoff setting: {e:#}"));
        }
        info!(enabled, "handoff switched");
        self.handoff.enabled = enabled;
        if !enabled && self.router.mode() == Some(RouteMode::Yielded) {
            self.route(RouteMode::Own, "handoff switched off while yielded");
        }
        self.rejoin_event(RejoinEvent::SetEnabled(enabled));
        self.publish_handoff();
        Response::ok()
    }

    /// Fold one observation into the rejoin state and perform its decisions.
    fn rejoin_event(&mut self, event: RejoinEvent) {
        for action in rejoin::decide(&mut self.rejoin, event, std::time::Instant::now()) {
            match action {
                RejoinAction::Persist(hosts) => {
                    info!(
                        count = hosts.len(),
                        "learned an Apple host from a smart routing relay"
                    );
                    if let Some(path) = &self.known_hosts_path {
                        if let Err(e) = rejoin::save_known_hosts(path, &hosts) {
                            warn!(path = %path.display(), error = %e, "could not save known Apple hosts");
                        }
                    }
                }
                RejoinAction::Scheduled {
                    host,
                    matched,
                    kind,
                } => {
                    info!(
                        %host,
                        apple_host_match = matched.as_str(),
                        match_kind = kind.as_str(),
                        delay = ?rejoin::REJOIN_DELAY,
                        "rejoin scheduled"
                    );
                }
                RejoinAction::Deferred {
                    host,
                    matched,
                    kind,
                    delay,
                } => {
                    info!(
                        %host,
                        apple_host_match = matched.as_str(),
                        match_kind = kind.as_str(),
                        reason = "rate_limited",
                        ?delay,
                        "rejoin deferred"
                    );
                }
                RejoinAction::Skipped(reason) => info!(reason = %reason, "rejoin skipped"),
                RejoinAction::Cancelled(reason) => {
                    self.rejoin_epoch.store(0, Ordering::Relaxed);
                    info!(reason = %reason, "rejoin cancelled");
                }
                RejoinAction::Connect { seq, attempt } => self.spawn_rejoin(seq, attempt),
                RejoinAction::Retry { attempt, wait } => {
                    info!(attempt, ?wait, "rejoin connect busy; retrying");
                }
                RejoinAction::Rejoined => {
                    info!("rejoin connected; AAP recovery follows the new local link");
                }
                RejoinAction::GaveUp(error) => warn!(error = %error, "rejoin stopped"),
                RejoinAction::EvictionLoop => warn!(
                    "eviction loop: evicted again within 20 s of a rejoin; \
                     rejoin off until a manual connect or daemon restart"
                ),
            }
        }
        self.publish_link();
    }

    /// Remember the accessory's own address reports so a primary bud role
    /// switch can be told from a take-over. Byte 1 of the trailer is `0x02`
    /// on the accessory's addresses (`00 02` and `01 02` live); this host's
    /// own entry reads `01 01` and is not a bud.
    fn note_address_report(&mut self, address: Address, extra: [u8; 2]) {
        if extra[1] != 0x02 || self.adapter == Some(address) {
            return;
        }
        if self
            .bud_addresses
            .last()
            .is_some_and(|(seen, _)| *seen == address)
        {
            return;
        }
        self.bud_addresses.push((address, Instant::now()));
        if self.bud_addresses.len() > 2 {
            self.bud_addresses.remove(0);
        }
    }

    /// The primary bud address moved inside [`BUD_SWITCH_WINDOW`]: the buds
    /// handed the radio link to the other bud and cut this host doing it.
    fn bud_switched_recently(&self, now: Instant) -> bool {
        match self.bud_addresses.as_slice() {
            [_, (_, at)] => now.saturating_duration_since(*at) <= BUD_SWITCH_WINDOW,
            _ => false,
        }
    }

    /// Why the pending rejoin is running, in the published spelling.
    fn rejoin_reason(&self, now: Instant) -> LinkReason {
        if self.bud_switched_recently(now) {
            LinkReason::BudSwitch
        } else if self.rejoin.pending_kind() == Some(MatchKind::LinkLost) {
            LinkReason::LinkLost
        } else {
            LinkReason::TakenOver
        }
    }

    /// Publish the reconnect sequence, so the panel can tell a self-healing
    /// reconnect from a real disconnect. The store decides the final status:
    /// a live link always outranks a sequence that has not been retired yet.
    fn publish_link(&mut self) {
        let now = Instant::now();
        let current = (self.rejoin.pending_seq().map(|seq| (true, seq)))
            .or_else(|| self.autoconnect.pending_seq().map(|seq| (false, seq)));
        match current {
            Some((rejoin, seq)) => {
                let same = self
                    .link_sequence
                    .as_ref()
                    .is_some_and(|l| l.rejoin == rejoin && l.seq == seq);
                if !same {
                    let reason = if rejoin {
                        self.rejoin_reason(now)
                    } else {
                        LinkReason::AutoConnect
                    };
                    self.link_sequence = Some(LinkSequence {
                        rejoin,
                        seq,
                        reason,
                        since: crate::now_rfc3339_utc(),
                    });
                }
            }
            None => self.link_sequence = None,
        }
        let view = self.link_sequence.as_ref().map(|l| LinkActivity {
            reason: l.reason,
            attempt: u32::from(if l.rejoin {
                self.rejoin.pending_attempt()
            } else {
                self.autoconnect.pending_attempt()
            }),
            since: l.since.clone(),
        });
        if view != self.link_published {
            self.link_published = view.clone();
            self.store.apply(Update::Link(view));
        }
    }

    /// Fold one observation into the auto-connect state, perform its
    /// decisions, and tell the scanner whether discovery is still wanted.
    fn autoconnect_event(&mut self, event: AutoEvent) {
        // Rejoin owns eviction recovery. Reading its state here, rather than
        // duplicating it, keeps one connect policy per link drop.
        self.autoconnect.rejoin_pending = self.rejoin.is_pending();
        for action in autoconnect::decide(&mut self.autoconnect, event, std::time::Instant::now()) {
            match action {
                AutoAction::EpisodeStarted { rssi, delay } => info!(
                    target: "aurisd::autoconnect",
                    rssi = ?rssi,
                    device = ?self.device,
                    ?delay,
                    "proximity advert after an absence; connect scheduled"
                ),
                AutoAction::Skipped(reason) => {
                    info!(target: "aurisd::autoconnect", reason = %reason, "auto-connect skipped");
                }
                AutoAction::Connect { seq, attempt } => self.spawn_autoconnect(seq, attempt),
                AutoAction::Retry { attempt, wait } => info!(
                    target: "aurisd::autoconnect",
                    attempt,
                    ?wait,
                    "auto-connect did not take; retrying"
                ),
                AutoAction::Connected => {
                    info!(target: "aurisd::autoconnect", "auto-connect got the link");
                }
                AutoAction::Cancelled(reason) => {
                    self.autoconnect_epoch.store(0, Ordering::Relaxed);
                    info!(target: "aurisd::autoconnect", reason = %reason, "auto-connect cancelled");
                }
                AutoAction::GaveUp(error) => {
                    self.autoconnect_epoch.store(0, Ordering::Relaxed);
                    warn!(target: "aurisd::autoconnect", error = %error, "auto-connect stopped");
                }
                AutoAction::Disarmed(reason) => info!(
                    target: "aurisd::autoconnect",
                    reason = %reason,
                    "auto-connect disarmed until the AirPods go away again"
                ),
                AutoAction::Rearmed => {
                    info!(target: "aurisd::autoconnect", "auto-connect armed again");
                }
                AutoAction::FallbackPage { away, next } => info!(
                    target: "aurisd::autoconnect",
                    ?away,
                    ?next,
                    device = ?self.device,
                    "no proximity advert since the disconnect; paging anyway"
                ),
                AutoAction::FallbackFailed(error) => info!(
                    target: "aurisd::autoconnect",
                    error = %error,
                    "fallback page did not take; waiting for the next slot"
                ),
            }
        }
        self.publish_link();
        if let Some(tx) = &self.autoconnect_scan_tx {
            let wanted = self.autoconnect.should_scan();
            tx.send_if_modified(|current| {
                let changed = *current != wanted;
                *current = wanted;
                changed
            });
        }
    }

    /// Re-check BlueZ and call Device1.Connect once, off the socket loop. The
    /// result comes back as [`Wake::AutoConnect`]. This is the same call the
    /// rejoin path uses; only one of the two can have a sequence in flight,
    /// because each refuses to start while the other is pending.
    fn spawn_autoconnect(&self, seq: u64, attempt: u8) {
        let tx = self.autoconnect_tx.clone();
        let (Some(adapter), Some(device)) = (self.adapter, self.device) else {
            let _ = tx.try_send((seq, rejoin::Outcome::NotReady("device_unknown")));
            return;
        };
        if !self.autoconnect_control_link {
            debug!("link control is off for this supervisor");
            let _ = tx.try_send((seq, rejoin::Outcome::NotReady("link_control_off")));
            return;
        }
        self.autoconnect_epoch.store(seq, Ordering::Relaxed);
        let epoch = Arc::clone(&self.autoconnect_epoch);
        info!(
            target: "aurisd::autoconnect",
            attempt,
            %device,
            "auto-connect: calling Device1.Connect"
        );
        tokio::spawn(async move {
            let still_wanted = || epoch.load(Ordering::Relaxed) == seq;
            let outcome = match tokio::time::timeout(
                REJOIN_CONNECT_TIMEOUT,
                bluez::rejoin_connect(adapter, device, still_wanted),
            )
            .await
            {
                Ok(outcome) => outcome,
                Err(_) => rejoin::Outcome::Failed(
                    "Device1.Connect timed out; it may still complete".to_owned(),
                ),
            };
            let _ = tx.send((seq, outcome)).await;
        });
    }

    /// Re-check BlueZ and call Device1.Connect once, off the socket loop. The
    /// result comes back as [`Wake::Rejoin`].
    fn spawn_rejoin(&self, seq: u64, attempt: u8) {
        let tx = self.rejoin_tx.clone();
        let (Some(adapter), Some(device)) = (self.adapter, self.device) else {
            let _ = tx.try_send((seq, rejoin::Outcome::NotReady("device_unknown")));
            return;
        };
        if !self.control_link {
            debug!("link control is off for this supervisor");
            let _ = tx.try_send((seq, rejoin::Outcome::NotReady("link_control_off")));
            return;
        }
        self.rejoin_epoch.store(seq, Ordering::Relaxed);
        let epoch = Arc::clone(&self.rejoin_epoch);
        info!(attempt, "rejoin: calling Device1.Connect");
        tokio::spawn(async move {
            let still_wanted = || epoch.load(Ordering::Relaxed) == seq;
            let outcome = match tokio::time::timeout(
                REJOIN_CONNECT_TIMEOUT,
                bluez::rejoin_connect(adapter, device, still_wanted),
            )
            .await
            {
                Ok(outcome) => outcome,
                Err(_) => rejoin::Outcome::Failed(
                    "Device1.Connect timed out; it may still complete".to_owned(),
                ),
            };
            let _ = tx.send((seq, outcome)).await;
        });
    }

    fn drain_link_events(&mut self) {
        while let Ok(ev) = self.link_rx.try_recv() {
            self.on_link(ev);
        }
    }

    fn opening_alive(&mut self, generation: u64) -> bool {
        self.drain_link_events();
        self.acl && self.link_generation == generation
    }
}

/// Receive exactly one datagram, or park forever when there is no socket.
async fn recv_one(sock: Option<Arc<Link>>) -> std::io::Result<Vec<u8>> {
    match sock {
        Some(s) => {
            let mut buf = vec![0u8; RECV_BUF];
            let n = s.recv(&mut buf).await?;
            buf.truncate(n);
            Ok(buf)
        }
        None => std::future::pending().await,
    }
}

/// Next playback reading, or park forever when there is no MPRIS feed.
async fn recv_playback(
    rx: &mut Option<mpsc::Receiver<PlaybackReading>>,
) -> Option<PlaybackReading> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// Wait for a proximity sighting, or forever when no scanner is attached.
async fn recv_sighting(rx: &mut Option<mpsc::Receiver<Sighting>>) -> Option<Sighting> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// Space-separated lowercase hex, for debug logging of raw AAP bytes.
fn hex_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aap::codec::BatteryEntry;

    #[test]
    fn only_both_buds_in_the_case_counts_as_put_away() {
        use EarState::{Case, In, Out, Unknown};
        assert!(both_buds_in_case(Case, Case));
        // A bud swap: one bud back in the case and charging, the other in an
        // ear or out of it. The AirPods are in use, and a rejoin stays open.
        for (primary, secondary) in [
            (Case, In),
            (In, Case),
            (Case, Out),
            (Out, Case),
            (In, Out),
            (Out, In),
            (In, In),
            (Out, Out),
            (Case, Unknown),
            (Unknown, Case),
            (Unknown, Unknown),
        ] {
            assert!(
                !both_buds_in_case(primary, secondary),
                "{primary:?}/{secondary:?}"
            );
        }
    }

    #[test]
    fn an_ear_report_decides_nothing_but_the_in_case_flag() {
        // The wire bytes of an ear-detection report: primary out of ear,
        // secondary in ear. It must not read as the case.
        let packet = codec::decode(&[0x04, 0x00, 0x04, 0x00, 0x06, 0x00, 0x01, 0x00]);
        match packet {
            Ok(Packet::EarDetection { primary, secondary }) => {
                assert_eq!((primary, secondary), (EarState::Out, EarState::In));
                assert!(!both_buds_in_case(primary, secondary));
            }
            other => panic!("expected an ear detection packet, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_setting_write_puts_exactly_one_datagram_on_the_link() {
        struct RecordingSocket {
            sent: std::sync::Mutex<Vec<Vec<u8>>>,
            fail: bool,
            short_write: bool,
        }
        impl AapSocket for RecordingSocket {
            async fn send(&self, packet: &[u8]) -> std::io::Result<usize> {
                self.sent.lock().unwrap().push(packet.to_vec());
                if self.fail {
                    return Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe));
                }
                Ok(packet.len() - usize::from(self.short_write))
            }
            async fn recv(&self, _packet: &mut [u8]) -> std::io::Result<usize> {
                unreachable!("a setting write must not consume reports")
            }
        }
        let packet = codec::encode_set_setting(&crate::settings::SettingCommand::PressSpeed(
            crate::settings::PressSpeed::Slower,
        ))
        .unwrap();
        // No refresh packet exists any more: the accessory ignores one, and a
        // second datagram could only ever risk replaying the write.
        for (fail, short_write, success) in [
            (false, false, true),
            (true, false, false),
            (false, true, false),
        ] {
            let socket = RecordingSocket {
                sent: std::sync::Mutex::new(Vec::new()),
                fail,
                short_write,
            };
            assert_eq!(send_setting(&socket, &packet).await.is_ok(), success);
            let sent = socket.sent.lock().unwrap();
            assert_eq!(sent.len(), 1);
            assert_eq!(sent[0], packet);
        }
    }

    #[tokio::test]
    async fn a_readback_without_a_link_gives_up_instead_of_dialing() {
        use crate::config::PrimaryBud;
        use crate::settings::{PressSpeed, SettingCommand};

        let store = Store::new(Snapshot::initial("AA:BB:CC:DD:EE:01"), PrimaryBud::Auto);
        store.apply(Update::SettingRequested(SettingCommand::PressSpeed(
            PressSpeed::Slower,
        )));
        let (_link_tx, link_rx) = mpsc::channel(1);
        let (_cmd_tx, cmd_rx) = mpsc::channel(1);
        let mut supervisor =
            Supervisor::new(store.clone(), SessionConfig::default(), link_rx, cmd_rx);

        supervisor.verify_at = Some(Instant::now() - Duration::from_millis(1));
        supervisor.verify_budget = Some(Instant::now() + VERIFY_BUDGET);
        supervisor.tick_verification().await;

        let snapshot = store.snapshot();
        assert_eq!(snapshot.settings_status["press_speed"], "unverified");
        assert_eq!(snapshot.settings_verify, "idle");
        assert!(!snapshot.verify_reopen);
        // A readback is never a reason to make BlueZ connect anything.
        assert!(supervisor.dial_at.is_none());
        assert!(supervisor.verify_at.is_none());
        assert!(supervisor.verify_budget.is_none());
    }

    #[tokio::test]
    async fn the_dump_window_closes_on_time_without_a_battery_packet() {
        use crate::config::PrimaryBud;
        use crate::settings::{PressSpeed, SettingCommand};

        let store = Store::new(Snapshot::initial("AA:BB:CC:DD:EE:01"), PrimaryBud::Auto);
        store.apply(Update::AapLink(true));
        store.apply(Update::SettingRequested(SettingCommand::PressSpeed(
            PressSpeed::Slower,
        )));
        store.apply(Update::Setting(SettingCommand::PressSpeed(
            PressSpeed::Slower,
        )));
        let (_link_tx, link_rx) = mpsc::channel(1);
        let (_cmd_tx, cmd_rx) = mpsc::channel(1);
        let mut supervisor =
            Supervisor::new(store.clone(), SessionConfig::default(), link_rx, cmd_rx);

        supervisor.readback_at = Some(Instant::now() - Duration::from_millis(1));
        supervisor.verify_budget = Some(Instant::now() + VERIFY_BUDGET);
        supervisor.tick_verification().await;

        let snapshot = store.snapshot();
        assert_eq!(snapshot.settings_status["press_speed"], "confirmed");
        assert_eq!(snapshot.settings_verify, "idle");
        assert!(supervisor.readback_at.is_none());
        assert!(supervisor.verify_budget.is_none());
    }

    #[test]
    fn readback_timers_fit_the_observed_dump_behaviour() {
        // The debounce must leave room for the reopen, the opening sequence
        // and the dump inside the published 10 s budget.
        assert!(
            VERIFY_DEBOUNCE + HANDSHAKE_ACK_WAIT + FEATURES_ACK_WAIT + READBACK_WINDOW
                < VERIFY_BUDGET
        );
        assert!(VERIFY_DEBOUNCE > TICK);
    }

    fn command(
        store: &Store,
        request: Request,
    ) -> (Command, tokio::sync::oneshot::Receiver<Response>) {
        let (reply, rx) = tokio::sync::oneshot::channel();
        (
            Command {
                request,
                reply,
                context: store.connection_context(),
                deadline: Instant::now() + Duration::from_secs(1),
            },
            rx,
        )
    }

    struct PendingSocket;

    impl AapSocket for PendingSocket {
        async fn send(&self, _buf: &[u8]) -> std::io::Result<usize> {
            std::future::pending().await
        }

        async fn recv(&self, _buf: &mut [u8]) -> std::io::Result<usize> {
            std::future::pending().await
        }
    }

    #[test]
    fn idle_watchdog_probes_before_recycling() {
        let t0 = Instant::now();
        // Fresh traffic: nothing to do.
        assert_eq!(idle_decision(t0, Some(t0), None), IdleAction::Nothing);
        // Silence past the threshold asks for a probe, not a recycle.
        let idle = t0 + IDLE_TIMEOUT;
        assert_eq!(idle_decision(idle, Some(t0), None), IdleAction::Probe);
        // With a probe outstanding, wait out the grace period.
        assert_eq!(
            idle_decision(idle, Some(t0), Some(idle)),
            IdleAction::Nothing
        );
        assert_eq!(
            idle_decision(idle + IDLE_PROBE_GRACE, Some(t0), Some(idle)),
            IdleAction::Recycle
        );
        // A packet after the probe clears the suspicion.
        let answered = idle + Duration::from_secs(1);
        assert_eq!(
            idle_decision(idle + IDLE_PROBE_GRACE, Some(answered), Some(idle)),
            IdleAction::Nothing
        );
    }

    #[test]
    fn local_link_edges_arm_at_most_one_automatic_dial() {
        use crate::config::PrimaryBud;

        let store = Store::new(Snapshot::initial(""), PrimaryBud::Auto);
        let (_link_tx, link_rx) = mpsc::channel(1);
        let (_cmd_tx, cmd_rx) = mpsc::channel(1);
        let mut supervisor = Supervisor::new(store, SessionConfig::default(), link_rx, cmd_rx);

        supervisor.on_link(LinkEvent::Connected(true));
        assert_eq!(supervisor.dial_kind, Some(DialKind::Automatic));
        let first_generation = supervisor.link_generation;

        // Repeated `true` is not a fresh local reconnect and cannot rearm.
        supervisor.suppress_automatic("test ambiguity");
        supervisor.on_link(LinkEvent::Connected(true));
        assert_eq!(supervisor.dial_kind, None);
        assert_eq!(supervisor.link_generation, first_generation);

        supervisor.on_link(LinkEvent::Connected(false));
        supervisor.on_link(LinkEvent::Connected(true));
        assert_eq!(
            supervisor.dial_kind,
            Some(DialKind::Automatic),
            "a new false->true observation starts a new local-link period"
        );
    }

    #[test]
    fn identity_change_cancels_authority_before_a_dial() {
        use crate::config::PrimaryBud;

        let store = Store::new(Snapshot::initial(""), PrimaryBud::Auto);
        let (_link_tx, link_rx) = mpsc::channel(1);
        let (_cmd_tx, cmd_rx) = mpsc::channel(1);
        let mut supervisor = Supervisor::new(store, SessionConfig::default(), link_rx, cmd_rx);
        let adapter: Address = "00:11:22:33:44:55".parse().unwrap();
        let first: Address = "AA:BB:CC:DD:EE:01".parse().unwrap();
        let second: Address = "AA:BB:CC:DD:EE:02".parse().unwrap();

        supervisor.on_link(LinkEvent::Identity {
            adapter,
            address: first,
            name: None,
            model_id: None,
            pinned: false,
        });
        supervisor.on_link(LinkEvent::Connected(true));
        let old_generation = supervisor.link_generation;
        assert_eq!(supervisor.dial_kind, Some(DialKind::Automatic));

        supervisor.on_link(LinkEvent::Identity {
            adapter,
            address: second,
            name: None,
            model_id: None,
            pinned: false,
        });
        assert!(!supervisor.acl);
        assert_eq!(supervisor.dial_kind, None);
        assert_ne!(supervisor.link_generation, old_generation);
    }

    #[test]
    fn fake_checker_and_link_events_cannot_authorize_an_unsafe_dial() {
        use crate::config::PrimaryBud;

        let store = Store::new(Snapshot::initial(""), PrimaryBud::Auto);
        let (_link_tx, link_rx) = mpsc::channel(1);
        let (_cmd_tx, cmd_rx) = mpsc::channel(1);
        let mut supervisor = Supervisor::new(store, SessionConfig::default(), link_rx, cmd_rx);
        supervisor.on_link(LinkEvent::Connected(true));
        let generation = supervisor.link_generation;

        // The checker seam returns false: no dial can start and no retry is
        // left armed in this local-link period.
        let fake_disconnected_checker = || Ok::<bool, ()>(false);
        assert!(!supervisor.authorize_dial(
            DialKind::Automatic,
            generation,
            fake_disconnected_checker().map_err(|_| "fake checker: disconnected".into()),
        ));
        assert!(supervisor.auto_suppressed);
        assert_eq!(supervisor.dial_kind, None);

        // A late disconnect and adapter loss invalidate an in-flight opening
        // generation even if a checker had returned true earlier.
        supervisor.on_link(LinkEvent::Connected(false));
        assert!(!supervisor.opening_alive(generation));
        supervisor.on_link(LinkEvent::AdapterGone);
        assert!(!supervisor.opening_alive(generation));
    }

    #[tokio::test]
    async fn disconnect_cancels_a_real_pending_handshake_wait() {
        use crate::config::PrimaryBud;

        let store = Store::new(Snapshot::initial(""), PrimaryBud::Auto);
        let (link_tx, link_rx) = mpsc::channel(1);
        let (_cmd_tx, cmd_rx) = mpsc::channel(1);
        let mut supervisor = Supervisor::new(store, SessionConfig::default(), link_rx, cmd_rx);
        supervisor.on_link(LinkEvent::Connected(true));
        let generation = supervisor.link_generation;
        link_tx.send(LinkEvent::Connected(false)).await.unwrap();

        let result = supervisor
            .await_ack(
                &PendingSocket,
                Awaited::Handshake,
                Duration::from_secs(1),
                generation,
            )
            .await;
        assert!(matches!(result, AckWait::Cancelled));
        assert!(!supervisor.acl);
    }

    #[test]
    fn tentative_opening_cleanup_invalidates_aap_data_without_a_promoted_socket() {
        use crate::config::PrimaryBud;

        let store = Store::new(Snapshot::initial(""), PrimaryBud::Auto);
        let (_link_tx, link_rx) = mpsc::channel(1);
        let (_cmd_tx, cmd_rx) = mpsc::channel(1);
        let mut supervisor =
            Supervisor::new(store.clone(), SessionConfig::default(), link_rx, cmd_rx);
        store.apply(Update::Battery(vec![BatteryEntry {
            component: codec::BatteryComponent::Left,
            level: Some(40),
            charging: false,
            present: true,
        }]));
        assert!(store.snapshot().battery.left.fresh);
        supervisor.drop_socket();
        assert!(!store.snapshot().battery.left.fresh);
    }

    #[tokio::test]
    async fn queued_context_change_rejects_a_non_status_command_before_action() {
        use crate::config::PrimaryBud;

        let store = Store::new(Snapshot::initial(""), PrimaryBud::Auto);
        let (_link_tx, link_rx) = mpsc::channel(1);
        let (_cmd_tx, cmd_rx) = mpsc::channel(1);
        let mut supervisor =
            Supervisor::new(store.clone(), SessionConfig::default(), link_rx, cmd_rx);
        let (command, reply) = command(
            &store,
            Request::SetNoiseControl {
                value: crate::state::NoiseControlMode::Anc,
            },
        );
        // This is a real non-status command path, but the queue context was
        // captured before the local link changed.
        store.apply(Update::AclConnected(true));
        supervisor.on_command(command).await;
        let response = reply.await.unwrap();
        assert!(matches!(response, Response::Ack { ok: false, .. }));
        assert_eq!(
            store.snapshot().noise_control,
            crate::state::NoiseControl::Unknown
        );
    }

    #[tokio::test]
    async fn expired_and_closed_queued_commands_are_inert() {
        use crate::config::PrimaryBud;

        let store = Store::new(Snapshot::initial(""), PrimaryBud::Auto);
        let (_link_tx, link_rx) = mpsc::channel(1);
        let (_cmd_tx, cmd_rx) = mpsc::channel(1);
        let mut supervisor =
            Supervisor::new(store.clone(), SessionConfig::default(), link_rx, cmd_rx);

        let (mut expired, reply) = command(&store, Request::Reconnect);
        expired.deadline = Instant::now() - Duration::from_millis(1);
        supervisor.on_command(expired).await;
        assert!(matches!(
            reply.await.unwrap(),
            Response::Ack { ok: false, .. }
        ));

        let (closed, reply) = command(&store, Request::ConnectOnce);
        drop(reply);
        let before = store.snapshot();
        supervisor.on_command(closed).await;
        assert_eq!(store.snapshot(), before);
    }

    #[test]
    fn features_variant_from_env_defaults_to_contract_packet() {
        assert_eq!(SessionConfig::default().features, None);
        assert_eq!(SessionConfig::default().psm, 0x1001);
        assert_eq!(pick_variant(None, None, 0), FeaturesVariant::D7);
    }

    #[test]
    fn variant_alternates_until_one_produces_battery() {
        // Even dials take the default variant, odd dials the alternate, so a
        // session that saw no battery is followed by the other variant.
        assert_eq!(pick_variant(None, None, 0), FeaturesVariant::D7);
        assert_eq!(pick_variant(None, None, 1), FeaturesVariant::Alt);
        assert_eq!(pick_variant(None, None, 2), FeaturesVariant::D7);
        assert_eq!(pick_variant(None, None, 3), FeaturesVariant::Alt);
        // A proven variant sticks whatever the attempt number.
        for n in 0..4 {
            assert_eq!(
                pick_variant(None, Some(FeaturesVariant::Alt), n),
                FeaturesVariant::Alt
            );
            assert_eq!(
                pick_variant(None, Some(FeaturesVariant::D7), n),
                FeaturesVariant::D7
            );
        }
        // The environment overrides both.
        assert_eq!(
            pick_variant(Some(FeaturesVariant::Alt), Some(FeaturesVariant::D7), 0),
            FeaturesVariant::Alt
        );
        assert_eq!(
            pick_variant(Some(FeaturesVariant::Alt), None, 2),
            FeaturesVariant::Alt
        );
    }

    #[test]
    fn opening_acks_are_recognised_by_the_codec() {
        // What `await_ack` matches on, without needing a socket.
        assert!(matches!(
            codec::decode(&[0x01, 0x00, 0x04, 0x00, 0x00, 0x00]),
            Ok(Packet::HandshakeAck)
        ));
        assert!(matches!(
            codec::decode(&[0x04, 0x00, 0x04, 0x00, 0x2b, 0x00, 0x00]),
            Ok(Packet::FeaturesAck)
        ));
        // The handshake we send is not itself an ack.
        assert!(!matches!(
            codec::decode(&codec::encode_handshake()),
            Ok(Packet::HandshakeAck | Packet::FeaturesAck)
        ));
    }

    #[test]
    fn ack_budgets_fit_inside_the_battery_watchdog() {
        // Both waits happen before `handshake_at` is armed, so the watchdog
        // must still be the longer window once the link is claimed.
        assert!(HANDSHAKE_ACK_WAIT + FEATURES_ACK_WAIT < BATTERY_WATCHDOG);
        assert!(HANDSHAKE_ACK_WAIT + FEATURES_ACK_WAIT < CONNECT_TIMEOUT + BATTERY_WATCHDOG);
    }

    #[test]
    fn settings_model_gate_uses_the_current_did() {
        let mut snapshot = Snapshot::initial("");
        assert!(settings_model_error(&snapshot)
            .unwrap()
            .contains("model is unknown"));

        snapshot.device.model_id = "200E".into();
        assert!(settings_model_error(&snapshot)
            .unwrap()
            .contains("unsupported device model 200E"));

        snapshot.device.model_id = "201B".into();
        assert_eq!(settings_model_error(&snapshot), None);
    }
}

#[cfg(test)]
mod handoff_session_tests {
    use super::*;
    use crate::{
        config::PrimaryBud,
        state::{HandoffEventKind, HandoffOwner, LinkStatus},
    };

    fn supervisor(store: &Arc<Store>) -> Supervisor {
        let (_link_tx, link_rx) = mpsc::channel(1);
        let (_cmd_tx, cmd_rx) = mpsc::channel(1);
        let mut sup = Supervisor::new(Arc::clone(store), SessionConfig::default(), link_rx, cmd_rx);
        sup.on_link(LinkEvent::Identity {
            adapter: "5C:F3:70:0D:0E:0F".parse().unwrap(),
            address: "BC:80:4E:01:02:03".parse().unwrap(),
            name: None,
            model_id: None,
            pinned: false,
        });
        sup
    }

    const LOCAL_IDLE: [u8; 13] = [
        4, 0, 4, 0, 0x0e, 0, 0x0f, 0x0e, 0x0d, 0x70, 0xf3, 0x5c, 0x00,
    ];
    const DEVICES: [u8; 17] = [
        4, 0, 4, 0, 0x2e, 0, 0x01, 0x00, 0x01, 0x5c, 0xf3, 0x70, 0x0d, 0x0e, 0x0f, 0x02, 0x02,
    ];
    const OTHER_MEDIA: [u8; 13] = [
        4, 0, 4, 0, 0x0e, 0, 0x22, 0x11, 0x00, 0xe7, 0x83, 0xa4, 0x02,
    ];

    /// 0x0011 relay from the user's Mac (FC:B2:14:0A:0B:0C, reversed).
    const MAC_RELAY: [u8; 16] = [
        4, 0, 4, 0, 0x11, 0, 0x0c, 0x0b, 0x0a, 0x14, 0xb2, 0xfc, 2, 0, 0x01, 0xe0,
    ];
    /// 0x002E listing this host and the Mac, header 01 00.
    const DEVICES_WITH_MAC: [u8; 25] = [
        4, 0, 4, 0, 0x2e, 0, 0x01, 0x00, 0x02, 0x5c, 0xf3, 0x70, 0x0d, 0x0e, 0x0f, 0x02, 0x02,
        0xfc, 0xb2, 0x14, 0x0a, 0x0b, 0x0c, 0x02, 0x02,
    ];
    /// The same list with header 01 02, as reported repeatedly up to the drop.
    const DEVICES_WITH_MAC_ROUTED: [u8; 25] = [
        4, 0, 4, 0, 0x2e, 0, 0x01, 0x02, 0x02, 0x5c, 0xf3, 0x70, 0x0d, 0x0e, 0x0f, 0x02, 0x02,
        0xfc, 0xb2, 0x14, 0x0a, 0x0b, 0x0c, 0x02, 0x02,
    ];

    fn handoff_supervisor(store: &Arc<Store>) -> Supervisor {
        supervisor(store).with_handoff(HandoffOptions {
            config: HandoffConfig {
                enabled: true,
                ..HandoffConfig::default()
            },
            ..HandoffOptions::default()
        })
    }

    const READY: Option<rejoin::LinkFacts> = Some(rejoin::LinkFacts {
        powered: true,
        paired: true,
        blocked: false,
    });

    /// AAP reset, Connected=false, then `Disconnected` with `reason`.
    fn drop_link(sup: &mut Supervisor, reason: &str) {
        sup.suppress_automatic("recv error");
        sup.on_link(LinkEvent::Connected(false));
        assert!(!sup.rejoin.is_pending());
        sup.on_link(LinkEvent::Disconnected {
            reason: reason.into(),
            message: String::new(),
            facts: READY,
        });
    }

    #[test]
    fn both_live_evictions_through_the_supervisor_schedule_one_rejoin() {
        let mac: Address = "FC:B2:14:0A:0B:0C".parse().unwrap();

        // The Mac connects fresh (header 01 00), reset follows.
        let store = Store::new(Snapshot::initial(""), PrimaryBud::Auto);
        let mut sup = handoff_supervisor(&store);
        sup.on_packet(&MAC_RELAY);
        assert_eq!(sup.rejoin.known(), [mac]);
        sup.on_packet(&DEVICES);
        sup.on_packet(&DEVICES_WITH_MAC);
        drop_link(&mut sup, "org.bluez.Reason.Remote");
        assert!(sup.rejoin.is_pending());

        // The Mac is listed from the first report, relays, and the 01 02
        // reports repeat until the drop.
        let store = Store::new(Snapshot::initial(""), PrimaryBud::Auto);
        let mut sup = handoff_supervisor(&store);
        sup.on_packet(&DEVICES_WITH_MAC);
        sup.on_packet(&MAC_RELAY);
        for _ in 0..5 {
            sup.on_packet(&DEVICES_WITH_MAC_ROUTED);
        }
        let evidence = sup.rejoin.device_report_evidence(std::time::Instant::now());
        assert!(matches!(
            evidence,
            Some(rejoin::ReportEvidence {
                age,
                apple_host: Some((host, rejoin::HostMatch::Relay)),
                ..
            }) if host == mac && age < rejoin::EVICTION_WINDOW
        ));
        drop_link(&mut sup, "org.bluez.Reason.Remote");
        assert!(sup.rejoin.is_pending());

        // First time: no relay and no known hosts. The Mac's Apple OUI and the
        // first report on the link are enough.
        let store = Store::new(Snapshot::initial(""), PrimaryBud::Auto);
        let mut sup = handoff_supervisor(&store);
        sup.on_packet(&DEVICES_WITH_MAC);
        assert!(sup.rejoin.known().is_empty());
        let evidence = sup.rejoin.device_report_evidence(std::time::Instant::now());
        assert!(matches!(
            evidence,
            Some(rejoin::ReportEvidence {
                apple_host: Some((host, rejoin::HostMatch::Oui)),
                ..
            }) if host == mac
        ));
        drop_link(&mut sup, "org.bluez.Reason.Remote");
        assert!(sup.rejoin.is_pending());

        // The same drop with a local reason never schedules.
        let store = Store::new(Snapshot::initial(""), PrimaryBud::Auto);
        let mut sup = handoff_supervisor(&store);
        sup.on_packet(&MAC_RELAY);
        sup.on_packet(&DEVICES_WITH_MAC_ROUTED);
        drop_link(&mut sup, "org.bluez.Reason.Local");
        assert!(!sup.rejoin.is_pending());
    }

    /// When the Mac takes the AirPods, BlueZ reports `Disconnected` with
    /// reason Remote, and shortly after the device property watch can end
    /// because BlueZ removed a profile interface from the device object.
    /// That stream end used to be reported as `AdapterGone` and cancelled a
    /// rejoin scheduled shortly before it, while the adapter was powered the
    /// whole time. An eviction where the watch survives instead rejoins
    /// normally.
    #[test]
    fn a_watch_that_ends_keeps_the_scheduled_rejoin() {
        let adapter: Address = "00:11:22:33:44:55".parse().unwrap();
        let airpods: Address = "BC:80:4E:01:02:03".parse().unwrap();
        let identity = || LinkEvent::Identity {
            adapter,
            address: airpods,
            name: None,
            model_id: None,
            pinned: false,
        };

        let store = Store::new(Snapshot::initial(""), PrimaryBud::Auto);
        let mut sup = handoff_supervisor(&store);
        sup.on_link(identity());
        sup.on_link(LinkEvent::Connected(true));
        sup.on_packet(&MAC_RELAY);
        sup.on_packet(&DEVICES_WITH_MAC_ROUTED);
        drop_link(&mut sup, "org.bluez.Reason.Remote");
        assert!(sup.rejoin.is_pending(), "the eviction schedules a rejoin");

        // The device event stream ends; the adapter is still there.
        sup.on_link(LinkEvent::WatchEnded);
        assert!(
            sup.rejoin.is_pending(),
            "a watch that ended cancels nothing"
        );

        // The rebuilt watcher re-announces the same device and the state it
        // reads, which is still disconnected.
        sup.on_link(identity());
        sup.on_link(LinkEvent::Connected(false));
        assert!(sup.rejoin.is_pending(), "the rebuilt watch cancels nothing");

        // The wait belongs to this task, not to the watcher: it still fires.
        let due = std::time::Instant::now() + rejoin::REJOIN_DELAY;
        let actions = rejoin::decide(&mut sup.rejoin, rejoin::Event::Tick, due);
        assert!(
            matches!(actions[..], [rejoin::Action::Connect { attempt: 1, .. }]),
            "expected one Device1.Connect, got {actions:?}"
        );
    }

    /// When the Mac has owned the AirPods for a while and the newest 0x002E
    /// report is tens of seconds old, only the Mac's info byte 0 of 0x02
    /// (link up) can qualify the drop.
    #[test]
    fn a_mac_that_already_owned_the_airpods_still_schedules_a_rejoin() {
        let mac: Address = "FC:B2:14:0A:0B:0C".parse().unwrap();
        let store = Store::new(Snapshot::initial(""), PrimaryBud::Auto);
        let mut sup = handoff_supervisor(&store);
        sup.on_link(LinkEvent::Connected(true));
        // The last report before the drop: both links up.
        sup.on_packet(&DEVICES_WITH_MAC_ROUTED);
        let evidence = sup
            .rejoin
            .device_report_evidence(std::time::Instant::now())
            .expect("a report");
        assert_eq!(evidence.apple_host_link_up.map(|(host, _)| host), Some(mac));
        drop_link(&mut sup, "org.bluez.Reason.Remote");
        assert!(sup.rejoin.is_pending());

        // The same report with the Mac listed but its link down (info 00 15,
        // as 164 ms after the Mac disconnected) never qualifies once the
        // eviction window has passed.
        const DEVICES_WITH_MAC_DOWN: [u8; 25] = [
            4, 0, 4, 0, 0x2e, 0, 0x01, 0x02, 0x02, 0x5c, 0xf3, 0x70, 0x0d, 0x0e, 0x0f, 0x02, 0x03,
            0xfc, 0xb2, 0x14, 0x0a, 0x0b, 0x0c, 0x00, 0x15,
        ];
        let store = Store::new(Snapshot::initial(""), PrimaryBud::Auto);
        let mut sup = handoff_supervisor(&store);
        sup.on_link(LinkEvent::Connected(true));
        sup.on_packet(&DEVICES_WITH_MAC_DOWN);
        let evidence = sup
            .rejoin
            .device_report_evidence(std::time::Instant::now())
            .expect("a report");
        assert_eq!(evidence.apple_host_link_up, None);
        assert_eq!(evidence.apple_host.map(|(host, _)| host), Some(mac));
    }

    #[test]
    fn a_new_link_session_forgets_the_previous_reports() {
        let store = Store::new(Snapshot::initial(""), PrimaryBud::Auto);
        let mut sup = handoff_supervisor(&store);
        sup.on_link(LinkEvent::Connected(true));
        sup.on_packet(&DEVICES_WITH_MAC_ROUTED);
        assert!(sup
            .rejoin
            .device_report_evidence(std::time::Instant::now())
            .is_some());
        // The link drops for a local reason and comes back; the old report
        // describes the old session only.
        drop_link(&mut sup, "org.bluez.Reason.Local");
        sup.on_link(LinkEvent::Connected(false));
        sup.on_link(LinkEvent::Connected(true));
        assert_eq!(
            sup.rejoin.device_report_evidence(std::time::Instant::now()),
            None
        );
        assert_eq!(sup.rejoin.last_report_others(), Vec::<Address>::new());
    }

    #[test]
    fn a_powered_off_adapter_still_cancels_the_scheduled_rejoin() {
        let store = Store::new(Snapshot::initial(""), PrimaryBud::Auto);
        let mut sup = handoff_supervisor(&store);
        sup.on_packet(&MAC_RELAY);
        sup.on_packet(&DEVICES_WITH_MAC_ROUTED);
        drop_link(&mut sup, "org.bluez.Reason.Remote");
        assert!(sup.rejoin.is_pending());

        // Real evidence about the adapter: Powered=false or its object gone.
        sup.on_link(LinkEvent::AdapterGone);
        assert!(!sup.rejoin.is_pending());
    }

    #[test]
    fn airpods_reports_are_published_while_handoff_is_disabled() {
        let store = Store::new(Snapshot::initial(""), PrimaryBud::Auto);
        let mut sup = supervisor(&store);
        sup.on_packet(&LOCAL_IDLE);
        sup.on_packet(&DEVICES);
        let h = store.snapshot().handoff;
        let src = h.audio_source.expect("audio source published");
        assert_eq!(src.address, "5C:F3:70:0D:0E:0F");
        assert!(src.is_local);
        assert_eq!(h.devices.len(), 1);
        assert!(h.devices[0].is_local);

        sup.on_packet(&OTHER_MEDIA);
        let h = store.snapshot().handoff;
        assert_eq!(h.owner, HandoffOwner::Other);
        assert!(!h.audio_source.unwrap().is_local);
        assert!(h.last_event.is_none());
        assert!(sup.handoff_actions.is_empty());
    }

    #[test]
    fn an_enabled_supervisor_queues_a_yield_and_publishes_it() {
        let store = Store::new(Snapshot::initial(""), PrimaryBud::Auto);
        let mut sup = supervisor(&store).with_handoff(HandoffOptions {
            config: HandoffConfig {
                enabled: true,
                ..HandoffConfig::default()
            },
            ..HandoffOptions::default()
        });
        sup.on_packet(&OTHER_MEDIA);
        // Pause, then take the card off the AirPods, then release ownership.
        assert_eq!(
            sup.handoff_actions,
            [
                Action::PauseLocalPlayers,
                Action::SetAudioRoute(RouteMode::Yielded),
                Action::SendOwnership(false),
            ]
        );
        let event = store
            .snapshot()
            .handoff
            .last_event
            .expect("event published");
        assert_eq!(event.kind, HandoffEventKind::Yielded);
        assert_eq!(event.peer.as_deref(), Some("A4:83:E7:00:11:22"));
    }

    /// An ear-detection report: primary then secondary, 00 in, 01 out.
    fn ear_packet(primary: u8, secondary: u8) -> [u8; 8] {
        [4, 0, 4, 0, 0x06, 0, primary, secondary]
    }

    /// Report `packet`, wait out the settle time and report it again, so the
    /// settled reading is acted on without waiting for a 250 ms tick.
    async fn settle_ear(sup: &mut Supervisor, packet: [u8; 8]) {
        sup.on_packet(&packet);
        tokio::time::sleep(ear_media::SETTLE + Duration::from_millis(50)).await;
        sup.on_packet(&packet);
    }

    /// A supervisor with an MPRIS channel and a live local link, so
    /// ear-detection requests can be observed without a session bus.
    fn ear_supervisor(store: &Arc<Store>) -> (Supervisor, mpsc::Receiver<MprisCommand>) {
        let (tx, rx) = mpsc::channel(8);
        let mut sup = supervisor(store);
        sup.mpris_tx = Some(tx);
        sup.on_link(LinkEvent::Connected(true));
        (sup, rx)
    }

    /// The whole ear-detection path through the supervisor: a bud out pauses,
    /// the bud back in resumes the same player, and neither is visible to the
    /// handoff machine as ownership changing hands.
    #[tokio::test]
    async fn a_bud_leaving_and_returning_pauses_and_resumes_local_players() {
        const SPOTIFY: &str = "org.mpris.MediaPlayer2.spotify";
        let store = Store::new(Snapshot::initial(""), PrimaryBud::Auto);
        let (mut sup, mut mpris) = ear_supervisor(&store);
        sup.on_playback(PlaybackReading {
            playing: true,
            app: Some("Spotify".to_owned()),
            player: Some(SPOTIFY.to_owned()),
        });

        // Both buds in is the baseline, not a change.
        settle_ear(&mut sup, ear_packet(0x00, 0x00)).await;
        assert!(
            mpris.try_recv().is_err(),
            "a baseline reading pauses nothing"
        );

        settle_ear(&mut sup, ear_packet(0x00, 0x01)).await;
        assert_eq!(mpris.try_recv(), Ok(MprisCommand::PausePlaying));
        // None of this is a handoff: no actions, no owner, no event.
        assert!(sup.handoff_actions.is_empty());
        let published = store.snapshot().handoff;
        assert_eq!(published.owner, HandoffOwner::Unknown);
        assert!(published.last_event.is_none());

        // The pause lands, then the bud goes back in.
        sup.on_playback(PlaybackReading {
            playing: false,
            app: None,
            player: None,
        });
        settle_ear(&mut sup, ear_packet(0x00, 0x00)).await;
        assert_eq!(mpris.try_recv(), Ok(MprisCommand::Play(SPOTIFY.to_owned())));
        assert!(sup.handoff_actions.is_empty());
        assert_eq!(store.snapshot().handoff.owner, HandoffOwner::Unknown);
    }

    /// A yield drains local playback before the card comes off the AirPods,
    /// and never waits for a player that will not stop.
    #[tokio::test]
    async fn a_yield_drains_local_playback_before_switching_the_card_off() {
        let store = Store::new(Snapshot::initial(""), PrimaryBud::Auto);
        let mut sup = supervisor(&store);
        let (tx, rx) = mpsc::channel(4);
        sup.playback_rx = Some(rx);

        // Nothing playing: the yield is not held up at all.
        let started = Instant::now();
        sup.drain_local_playback().await;
        assert!(started.elapsed() < DRAIN_TIMEOUT);

        // A player that stops when asked releases the yield early.
        sup.last_playback = Some(PlaybackReading {
            playing: true,
            app: None,
            player: None,
        });
        tx.send(PlaybackReading {
            playing: false,
            app: None,
            player: None,
        })
        .await
        .expect("the drain is listening");
        let started = Instant::now();
        sup.drain_local_playback().await;
        assert!(started.elapsed() < DRAIN_TIMEOUT);

        // A player that ignores the pause is left behind after the window.
        sup.last_playback = Some(PlaybackReading {
            playing: true,
            app: None,
            player: None,
        });
        let started = Instant::now();
        sup.drain_local_playback().await;
        assert!(started.elapsed() >= DRAIN_TIMEOUT);
    }

    /// 0x0C primary bud address report for `BC:80:4E:01:02:03`, trailer 01 02.
    const BUD_LEFT: [u8; 14] = [
        4, 0, 4, 0, 0x0c, 0, 0x03, 0x02, 0x01, 0x4e, 0x80, 0xbc, 0x01, 0x02,
    ];
    /// The same report after the role moved to the other bud.
    const BUD_RIGHT: [u8; 14] = [
        4, 0, 4, 0, 0x0c, 0, 0x5c, 0xe8, 0xda, 0x4e, 0x80, 0xbc, 0x01, 0x02,
    ];

    /// Pretend the last bud address change happened `ago` before now.
    fn backdate_bud_switch(sup: &mut Supervisor, ago: Duration) {
        let last = sup.bud_addresses.last_mut().expect("a bud address change");
        last.1 = Instant::now() - ago;
    }

    #[test]
    fn a_scheduled_rejoin_publishes_a_reconnecting_link() {
        let store = Store::new(Snapshot::initial(""), PrimaryBud::Auto);
        let mut sup = handoff_supervisor(&store);
        sup.on_link(LinkEvent::Connected(true));
        assert_eq!(store.snapshot().link.status, LinkStatus::Connected);

        sup.on_packet(&DEVICES_WITH_MAC);
        drop_link(&mut sup, "org.bluez.Reason.Remote");
        assert!(sup.rejoin.is_pending());

        let link = store.snapshot().link;
        assert_eq!(link.status, LinkStatus::Reconnecting);
        // No bud address report, so the Mac taking over is the explanation.
        assert_eq!(link.reason, Some(LinkReason::TakenOver));
        assert_eq!(link.attempt, 0, "only scheduled, no connect started yet");
        let since = link.since.expect("a start time");
        assert!(since.ends_with('Z'), "{since}");
        assert!(
            chrono::DateTime::parse_from_rfc3339(&since).is_ok(),
            "{since}"
        );
    }

    #[test]
    fn a_bud_role_switch_just_before_the_drop_reads_as_bud_switch() {
        let store = Store::new(Snapshot::initial(""), PrimaryBud::Auto);
        let mut sup = handoff_supervisor(&store);
        sup.on_packet(&DEVICES_WITH_MAC);
        sup.on_packet(&BUD_LEFT);
        sup.on_packet(&BUD_LEFT);
        assert!(!sup.bud_switched_recently(Instant::now()), "no change yet");
        sup.on_packet(&BUD_RIGHT);
        backdate_bud_switch(&mut sup, Duration::from_secs(5));

        drop_link(&mut sup, "org.bluez.Reason.Remote");
        let link = store.snapshot().link;
        assert_eq!(link.status, LinkStatus::Reconnecting);
        assert_eq!(link.reason, Some(LinkReason::BudSwitch));
    }

    #[test]
    fn an_old_bud_role_switch_is_not_the_reason_for_the_drop() {
        let store = Store::new(Snapshot::initial(""), PrimaryBud::Auto);
        let mut sup = handoff_supervisor(&store);
        sup.on_packet(&DEVICES_WITH_MAC);
        sup.on_packet(&BUD_LEFT);
        sup.on_packet(&BUD_RIGHT);
        backdate_bud_switch(&mut sup, BUD_SWITCH_WINDOW + Duration::from_secs(5));

        drop_link(&mut sup, "org.bluez.Reason.Remote");
        assert_eq!(
            store.snapshot().link.reason,
            Some(LinkReason::TakenOver),
            "a switch older than the window explains nothing"
        );
    }

    #[test]
    fn a_new_link_session_forgets_the_bud_address_history() {
        let store = Store::new(Snapshot::initial(""), PrimaryBud::Auto);
        let mut sup = handoff_supervisor(&store);
        sup.on_packet(&BUD_LEFT);
        sup.on_packet(&BUD_RIGHT);
        assert!(sup.bud_switched_recently(Instant::now()));
        sup.on_link(LinkEvent::Connected(true));
        assert!(sup.bud_addresses.is_empty());
        assert!(!sup.bud_switched_recently(Instant::now()));
    }

    #[test]
    fn a_supervision_timeout_reads_as_link_lost() {
        let store = Store::new(Snapshot::initial(""), PrimaryBud::Auto);
        let mut sup = handoff_supervisor(&store);
        sup.on_packet(&DEVICES_WITH_MAC);
        drop_link(&mut sup, "org.bluez.Reason.Timeout");
        assert_eq!(sup.rejoin.pending_kind(), Some(MatchKind::LinkLost));

        let link = store.snapshot().link;
        assert_eq!(link.status, LinkStatus::Reconnecting);
        assert_eq!(link.reason, Some(LinkReason::LinkLost));
    }

    #[test]
    fn a_cancelled_rejoin_publishes_a_disconnected_link() {
        let store = Store::new(Snapshot::initial(""), PrimaryBud::Auto);
        let mut sup = handoff_supervisor(&store);
        sup.on_packet(&DEVICES_WITH_MAC);
        drop_link(&mut sup, "org.bluez.Reason.Remote");
        assert_eq!(store.snapshot().link.status, LinkStatus::Reconnecting);

        // An explicit reconnect takes the sequence over; nothing is healing.
        sup.rejoin_event(RejoinEvent::ManualCommand);
        assert!(!sup.rejoin.is_pending());
        assert_eq!(store.snapshot().link, crate::state::Link::default());
    }

    #[test]
    fn the_proximity_scanner_path_publishes_auto_connect() {
        let store = Store::new(Snapshot::initial(""), PrimaryBud::Auto);
        let mut sup = supervisor(&store);
        sup.autoconnect = AutoConnectState::new(&AutoconnectConfig {
            enabled: true,
            ..AutoconnectConfig::default()
        });
        sup.autoconnect_event(AutoEvent::Seen { rssi: Some(-45) });
        assert!(sup.autoconnect.is_pending());

        let link = store.snapshot().link;
        assert_eq!(link.status, LinkStatus::Reconnecting);
        assert_eq!(link.reason, Some(LinkReason::AutoConnect));
        assert_eq!(link.attempt, 0);
        assert!(link.since.is_some());

        // The AirPods answer the page: the link is simply connected again.
        sup.on_link(LinkEvent::Connected(true));
        let link = store.snapshot().link;
        assert_eq!(link.status, LinkStatus::Connected);
        assert!(link.reason.is_none());
        assert!(link.since.is_none());
    }

    #[test]
    fn a_rejoin_outranks_a_pending_auto_connect_in_the_published_reason() {
        let store = Store::new(Snapshot::initial(""), PrimaryBud::Auto);
        let mut sup = handoff_supervisor(&store);
        sup.autoconnect = AutoConnectState::new(&AutoconnectConfig {
            enabled: true,
            ..AutoconnectConfig::default()
        });
        sup.autoconnect_event(AutoEvent::Seen { rssi: Some(-45) });
        assert_eq!(store.snapshot().link.reason, Some(LinkReason::AutoConnect));

        sup.on_packet(&DEVICES_WITH_MAC);
        drop_link(&mut sup, "org.bluez.Reason.Remote");
        assert_eq!(store.snapshot().link.reason, Some(LinkReason::TakenOver));
    }
}
