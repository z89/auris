//! Socket lifecycle and the connection state machine.
//!
//! This module owns the only AAP socket and never parses bytes itself: every
//! packet goes through [`crate::aap::codec`]. It reacts to BlueZ link events
//! and to control commands, and it never initiates a Bluetooth connection.

use std::{sync::Arc, time::Duration};

use bluer::Address;
use tokio::{
    sync::mpsc,
    time::{Instant, MissedTickBehavior},
};
use tracing::{debug, info, trace, warn};

use crate::{
    aap::{
        codec::{self, ControlState, FeaturesVariant, Packet},
        opcode,
        socket::{self, AapSocket, Link},
    },
    bluez::LinkEvent,
    ctl_proto::{Request, Response},
    ctl_server::Command,
    store::{Store, Update},
};

/// Settle time after `Connected=true` before dialing the PSM.
const SETTLE: Duration = Duration::from_millis(800);
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
/// Backoff ladder in seconds, ±20% jitter, capped at the last entry.
const BACKOFF_SECS: [u64; 7] = [1, 2, 4, 8, 15, 30, 60];
/// A session that lasted this long resets the failure streak.
const SURVIVED: Duration = Duration::from_secs(30);
/// Recycles allowed inside one `Connected` period before slow polling.
const MAX_RECYCLES: u32 = 3;
/// Slow-poll interval once the recycle budget is spent.
const SLOW_POLL: Duration = Duration::from_secs(300);
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
/// Receive buffer; AAP messages are tiny but the L2CAP MTU can be larger.
const RECV_BUF: usize = 2048;

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

/// Deterministic ±20% jitter without pulling in an RNG crate.
fn jitter(base: Duration) -> Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::from(d.subsec_nanos()));
    // 0..=40 -> -20%..=+20%
    let pct = 80 + (nanos % 41);
    Duration::from_millis(base.as_millis() as u64 * pct / 100)
}

enum Wake {
    Link(LinkEvent),
    Cmd(Command),
    Recv(std::io::Result<Vec<u8>>),
    Tick,
}

/// The supervisor task: sole owner of the AAP socket.
pub struct Supervisor {
    store: Arc<Store>,
    cfg: SessionConfig,
    link_rx: mpsc::Receiver<LinkEvent>,
    cmd_rx: mpsc::Receiver<Command>,

    adapter: Option<Address>,
    device: Option<Address>,
    acl: bool,
    sock: Option<Arc<Link>>,

    dial_at: Option<Instant>,
    backoff_idx: usize,
    recycles: u32,
    session_start: Option<Instant>,
    handshake_at: Option<Instant>,
    last_packet: Option<Instant>,
    battery_seen: bool,
    notif_resends: u8,
    /// When the idle probe was sent, while its answer is still outstanding.
    idle_probe_at: Option<Instant>,

    /// Dials made in this `Connected` period; drives variant alternation.
    dial_attempt: u32,
    /// Variant used by the session currently open (or last attempted).
    active_variant: FeaturesVariant,
    /// Variant that has produced a battery packet. Once set, it is used for
    /// the rest of the process: alternation is a search, not a policy.
    locked_variant: Option<FeaturesVariant>,
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

/// Send one packet, failing rather than blocking forever on a stalled peer.
fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

async fn send_timed(sock: &Link, packet: &[u8]) -> std::io::Result<()> {
    trace!(tx = %hex(packet), "AAP send");
    match tokio::time::timeout(SEND_TIMEOUT, sock.send(packet)).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "AAP send timed out after 1500 ms",
        )),
    }
}

impl Supervisor {
    /// Build the supervisor. Nothing happens until [`Self::run`] is awaited.
    pub fn new(
        store: Arc<Store>,
        cfg: SessionConfig,
        link_rx: mpsc::Receiver<LinkEvent>,
        cmd_rx: mpsc::Receiver<Command>,
    ) -> Self {
        Self {
            store,
            cfg,
            link_rx,
            cmd_rx,
            adapter: None,
            device: None,
            acl: false,
            sock: None,
            dial_at: None,
            backoff_idx: 0,
            recycles: 0,
            session_start: None,
            handshake_at: None,
            last_packet: None,
            battery_seen: false,
            notif_resends: 0,
            idle_probe_at: None,
            dial_attempt: 0,
            active_variant: FeaturesVariant::D7,
            locked_variant: None,
        }
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
                r = recv_one(sock) => Wake::Recv(r),
                _ = tick.tick() => Wake::Tick,
            };

            match wake {
                Wake::Link(ev) => self.on_link(ev),
                Wake::Cmd(cmd) => self.on_command(cmd).await,
                Wake::Recv(Ok(bytes)) if bytes.is_empty() => {
                    self.recycle("peer closed the AAP socket").await;
                }
                Wake::Recv(Ok(bytes)) => self.on_packet(&bytes),
                Wake::Recv(Err(e)) => {
                    warn!(error = %e, errno = ?e.raw_os_error(), "AAP recv failed");
                    self.recycle("recv error").await;
                }
                Wake::Tick => self.on_tick().await,
            }
        }
    }

    fn on_link(&mut self, ev: LinkEvent) {
        match ev {
            LinkEvent::Identity {
                adapter,
                address,
                name,
                model_id,
            } => {
                self.adapter = Some(adapter);
                self.device = Some(address);
                self.store.apply(Update::Identity {
                    address: address.to_string(),
                    name,
                    model_id,
                });
            }
            LinkEvent::Connected(true) => {
                self.store.apply(Update::AclConnected(true));
                if !self.acl {
                    self.acl = true;
                    self.recycles = 0;
                    self.backoff_idx = 0;
                    self.dial_attempt = 0;
                    self.dial_at = Some(Instant::now() + SETTLE);
                    info!("device connected; dialing AAP after settle");
                }
            }
            LinkEvent::Connected(false) | LinkEvent::AdapterGone => {
                self.acl = false;
                self.dial_at = None;
                self.drop_socket();
                self.store.apply(Update::AclConnected(false));
            }
        }
    }

    fn drop_socket(&mut self) {
        if self.sock.take().is_some() {
            self.store.apply(Update::AapLink(false));
        }
        self.handshake_at = None;
        self.session_start = None;
        self.battery_seen = false;
        self.notif_resends = 0;
        self.idle_probe_at = None;
    }

    async fn on_command(&mut self, cmd: Command) {
        let Command { request, reply } = cmd;
        let response = match request {
            Request::Status => Response::Status(Box::new(self.store.snapshot())),
            Request::Reconnect => {
                if self.acl {
                    self.drop_socket();
                    self.backoff_idx = 0;
                    self.recycles = 0;
                    self.dial_at = Some(Instant::now());
                    Response::ok()
                } else {
                    Response::error("device is not connected: BlueZ reports no classic link")
                }
            }
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
        };
        let _ = reply.send(response);
    }

    async fn send_control(&mut self, packet: Vec<u8>, optimistic: Update) -> Response {
        let Some(sock) = self.sock.clone() else {
            return Response::error("device is not connected: no AAP link is open");
        };
        match send_timed(&sock, &packet).await {
            Ok(()) => {
                // The accessory echoes the new state on 0x0009, but reflect it
                // straight away so the widget does not lag behind the click.
                self.store.apply(optimistic);
                Response::ok()
            }
            Err(e) => {
                warn!(error = %e, "failed to send control packet");
                self.recycle("control send failed").await;
                Response::error(format!("failed to send command: {e}"))
            }
        }
    }

    fn on_packet(&mut self, bytes: &[u8]) {
        trace!(rx = %hex(bytes), "AAP recv");
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
            }
            Ok(Packet::EarDetection { primary, secondary }) => {
                self.store.apply(Update::Ear { primary, secondary });
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
            Ok(Packet::Control(ControlState::Other { id, value })) => {
                debug!(id, value, "unmodelled control echo");
            }
            Ok(Packet::Metadata(md)) => {
                info!(?md, "device metadata");
                self.store.apply(Update::Metadata(md));
            }
            Ok(Packet::ConversationalAwarenessLevel(level)) => {
                // Speech ducking while the feature is on. Earlier builds took
                // this for the on/off state and switched the toggle off every
                // time the wearer spoke.
                debug!(level, "conversational awareness speech event");
            }
            Ok(Packet::Unknown { opcode, payload }) => {
                debug!(
                    opcode = format_args!("{opcode:#06x}"),
                    len = payload.len(),
                    "unknown AAP packet"
                );
            }
            Err(e) => debug!(error = %e, len = bytes.len(), "undecodable AAP packet"),
        }
    }

    async fn on_tick(&mut self) {
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
                                send_timed(&s, &codec::encode_request_notifications()).await
                            {
                                warn!(error = %e, "resend failed");
                                self.recycle("resend failed").await;
                            }
                        }
                    } else {
                        self.recycle("no battery packet after re-requests").await;
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
                if let Err(e) = send_timed(&s, &codec::encode_request_notifications()).await {
                    warn!(error = %e, "idle probe send failed");
                    self.recycle_idle("idle probe send failed").await;
                }
            }
            IdleAction::Recycle => {
                self.recycle_idle("idle probe went unanswered for 15 s")
                    .await;
            }
        }
    }

    async fn dial(&mut self) {
        // Verify before clearing `dial_at`: without both addresses there is
        // nothing to dial, and dropping the deadline here would strand the
        // machine with acl=true and no socket and no retry armed.
        let (Some(adapter), Some(device)) = (self.adapter, self.device) else {
            debug!("dial deadline reached with no adapter/device address yet");
            self.schedule_backoff();
            return;
        };
        self.dial_at = None;

        debug!(%device, psm = self.cfg.psm, raw = self.cfg.raw_socket, "dialing AAP");
        let link = match tokio::time::timeout(
            CONNECT_TIMEOUT,
            socket::connect(adapter, device, self.cfg.psm, self.cfg.raw_socket),
        )
        .await
        {
            Ok(Ok(link)) => link,
            Ok(Err(e)) => {
                warn!(error = %e, errno = ?e.raw_os_error(), "AAP connect failed");
                self.schedule_backoff();
                return;
            }
            Err(_) => {
                warn!("AAP connect timed out");
                self.schedule_backoff();
                return;
            }
        };

        let sock = Arc::new(link);
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
        if let Err(e) = send_timed(&sock, &codec::encode_handshake()).await {
            warn!(error = %e, "handshake send failed");
            self.schedule_backoff();
            return;
        }
        match self
            .await_ack(&sock, Awaited::Handshake, HANDSHAKE_ACK_WAIT)
            .await
        {
            AckWait::Got => debug!("handshake ack received"),
            AckWait::TimedOut => {
                warn!(?variant, "no handshake ack");
                drop(sock);
                self.recycle("no handshake ack").await;
                return;
            }
            AckWait::Failed(e) => {
                warn!(error = %e, "handshake ack wait failed");
                drop(sock);
                self.schedule_backoff();
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
            if let Err(e) = send_timed(&sock, packet).await {
                warn!(error = %e, step = i, "AAP opening sequence failed");
                drop(sock);
                self.schedule_backoff();
                return;
            }
        }

        // 3. The features ack is advisory: some firmware never sends one, so a
        //    timeout is logged and the sequence continues.
        match self
            .await_ack(&sock, Awaited::Features, FEATURES_ACK_WAIT)
            .await
        {
            AckWait::Got => debug!("features ack received"),
            AckWait::TimedOut => info!("no set-features ack; continuing anyway"),
            AckWait::Failed(e) => {
                warn!(error = %e, "features ack wait failed");
                drop(sock);
                self.schedule_backoff();
                return;
            }
        }

        // 4. On the default order the subscribe comes last.
        if !alt_order {
            if let Err(e) = send_timed(&sock, &codec::encode_request_notifications()).await {
                warn!(error = %e, "request-notifications send failed");
                drop(sock);
                self.schedule_backoff();
                return;
            }
        }

        // Dialing can take seconds; BlueZ may already have reported
        // Connected=false. Drain anything queued before claiming the link.
        while let Ok(ev) = self.link_rx.try_recv() {
            self.on_link(ev);
        }
        if !self.acl {
            warn!("ACL dropped while dialing; discarding the fresh AAP socket");
            drop(sock);
            return;
        }

        info!(%device, ?variant, "AAP link up");
        let now = Instant::now();
        self.sock = Some(sock);
        self.session_start = Some(now);
        // The battery watchdog counts from the last request-notifications,
        // which is the packet that was just sent.
        self.handshake_at = Some(now);
        self.last_packet = Some(now);
        self.store.apply(Update::AapLink(true));
    }

    /// Wait for one opening-sequence acknowledgement, handling every other
    /// frame that arrives meanwhile exactly as the running loop would.
    async fn await_ack(&mut self, sock: &Arc<Link>, want: Awaited, budget: Duration) -> AckWait {
        let deadline = Instant::now() + budget;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return AckWait::TimedOut;
            }
            let mut buf = vec![0u8; RECV_BUF];
            let n = match tokio::time::timeout(left, sock.recv(&mut buf)).await {
                Err(_) => return AckWait::TimedOut,
                Ok(Err(e)) => return AckWait::Failed(e),
                Ok(Ok(n)) => n,
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

    fn schedule_backoff(&mut self) {
        let idx = self.backoff_idx.min(BACKOFF_SECS.len() - 1);
        let delay = jitter(Duration::from_secs(BACKOFF_SECS[idx]));
        self.backoff_idx += 1;
        debug!(?delay, "scheduling AAP redial");
        self.dial_at = Some(Instant::now() + delay);
    }

    async fn recycle(&mut self, reason: &str) {
        self.recycle_inner(reason, true).await;
    }

    /// Recycle without crediting the session for having survived.
    ///
    /// An idle recycle happens long after `SURVIVED`, so the ordinary path
    /// would zero `recycles` every time and the budget would never be spent:
    /// the daemon would flap the link forever instead of falling back to the
    /// slow poll.
    async fn recycle_idle(&mut self, reason: &str) {
        self.recycle_inner(reason, false).await;
    }

    async fn recycle_inner(&mut self, reason: &str, credit_survival: bool) {
        let survived = self.session_start.is_some_and(|t| t.elapsed() >= SURVIVED);
        warn!(reason, survived, "recycling AAP session");
        self.drop_socket();
        if !self.acl {
            return;
        }
        if survived {
            self.backoff_idx = 0;
            if credit_survival {
                self.recycles = 0;
            }
        }
        self.recycles += 1;
        if self.recycles > MAX_RECYCLES {
            warn!("recycle budget spent; slow polling");
            self.dial_at = Some(Instant::now() + SLOW_POLL);
        } else {
            self.schedule_backoff();
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jitter_stays_within_twenty_percent() {
        let base = Duration::from_secs(10);
        for _ in 0..64 {
            let j = jitter(base);
            assert!(j >= Duration::from_secs(8), "{j:?}");
            assert!(j <= Duration::from_secs(12), "{j:?}");
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
    fn idle_threshold_outlives_the_survival_window() {
        // An idle recycle always lands after SURVIVED, which is exactly why it
        // must not credit the session and zero the recycle budget.
        assert!(IDLE_TIMEOUT > SURVIVED);
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
}
