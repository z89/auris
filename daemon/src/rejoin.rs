//! Rejoin after an Apple host evicts this host.
//!
//! AirPods 4 drop the Linux link within about half a second when a Mac that is
//! not on this host's iCloud account connects. When Linux connects second
//! instead, the AirPods keep both hosts and handoff works, so one reconnect
//! after that eviction restores the shared state.
//!
//! The AirPods keep a recently disconnected host in the 0x002E list (a host
//! can be listed from the first report after its Bluetooth comes back on),
//! so list membership never changes at an eviction. Two kinds of evidence
//! make up for that, and either one qualifies a `Remote` drop:
//!
//! * `joined`: a 0x002E report arrived within [`EVICTION_WINDOW`] of the drop
//!   and lists an Apple host. This is the Mac connecting and pushing this
//!   host off; its report lands 0.2 to 0.5 s before the drop, while the Mac's
//!   own info byte can still read `0x01` (connecting).
//! * `connected`: the latest report of the link session that just ended lists
//!   an Apple host whose link is up (info byte 0 is `0x02`, see
//!   [`crate::aap::codec::ConnectedDevice::is_link_up`]), at any age. This
//!   covers a drop where the Mac had owned the AirPods the whole time, the
//!   newest report was tens of seconds old, and the buds dropped this host
//!   anyway. The age cannot be bounded here because the AirPods send 0x002E
//!   only when something changes.
//!
//! A third condition qualifies a `Timeout` drop, which is a link supervision
//! timeout and never a manual action:
//!
//! * `link_lost`: the reason is `Timeout` and the latest report of the link
//!   session that just ended lists an Apple host whose link is up. This
//!   covers a bud swap while the Mac owns the AirPods: the wearer takes one
//!   bud out, the buds move the radio link to the other bud, a report marks
//!   this host's own entry `0x00` while still arriving over that link, and
//!   shortly after the link dies of a supervision timeout while the Mac's
//!   link survives. The bud swap drops only the host that is not streaming,
//!   so the Apple host keeps the buds and this host has to connect again.
//!
//! A `Timeout` drop needs the link-up evidence; the `joined` window does not
//! apply to it. The AirPods reporting this host's own link as down in the
//! latest report is supporting evidence, logged as `own_link_reported_down`,
//! and never required: the report can be missing or already stale when the
//! timeout lands.
//!
//! The report is forgotten when a link session starts, so a report from an
//! earlier session can never be the evidence for a later drop.
//!
//! A listed host counts as an Apple host when its OUI is registered to
//! Apple, Inc. in the IEEE registry ([`AppleOuis`]) or when it has sent this
//! host a smart routing relay. Macs and iPhones use their public BR/EDR
//! address for classic Bluetooth, so the OUI works the first time a host is
//! seen, with no learned state.
//!
//! Two evictions in quick succession are common when the Mac takes the
//! AirPods, gives them back and takes them again. [`RATE_LIMIT`] keeps this
//! host from paging more than once a minute, but the second eviction still
//! has to be answered: dropping it as `rate_limited` outright leaves the
//! AirPods on the other host. The connect is therefore deferred to the end
//! of the rate-limit window instead, with the
//! same evidence and the same sequence bookkeeping, and cancelled by
//! anything that cancels an ordinary pending rejoin. [`LOOP_WINDOW`] is
//! untouched: a second eviction within 20 s of a successful rejoin is a
//! ping-pong with the other host and still stops rejoining altogether.
//!
//! [`decide`] is pure, like [`crate::handoff::decide`]: it folds one
//! observation into [`RejoinState`] and returns the side effects for
//! `aap::session`. A rejoin needs positive evidence for every condition;
//! anything ambiguous, and every local or manual disconnect, means no
//! reconnect.

use std::{
    collections::HashSet,
    fs,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use bluer::Address;

/// For the `joined` condition the latest 0x002E report must be at most this
/// old at the `Disconnected` signal and list an Apple host. Live: about 0.5 s
/// at 21:17 and 265 ms at 21:47 (205 ms before the AAP reset, which preceded
/// the signal by 60 ms). The `connected` condition has no age bound.
pub const EVICTION_WINDOW: Duration = Duration::from_millis(1500);
/// Wait after the drop before the first connect, so the new host settles.
pub const REJOIN_DELAY: Duration = Duration::from_secs(3);
/// An explicit connect or reconnect command this recent blocks a rejoin.
pub const MANUAL_QUIET: Duration = Duration::from_secs(10);
/// Waits before the second and third connect after a busy refusal.
pub const BUSY_RETRY: [Duration; 2] = [Duration::from_secs(3), Duration::from_secs(6)];
/// `Device1.Connect` calls per rejoin sequence, busy retries included.
pub const MAX_ATTEMPTS: u8 = 3;
/// At most one rejoin sequence starts in this long. An eviction inside the
/// window is not dropped: its connect waits for the window to expire.
pub const RATE_LIMIT: Duration = Duration::from_secs(60);
/// Evicted again this soon after a successful rejoin: stop rejoining.
pub const LOOP_WINDOW: Duration = Duration::from_secs(20);
/// Relay-learned Apple hosts kept on disk.
pub const KNOWN_HOSTS_CAP: usize = 8;

/// First argument of BlueZ `Device1.Disconnected(ss)`, as documented in
/// `org.bluez.Device(5)` for BlueZ 5.87.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    /// `org.bluez.Reason.Unknown`.
    Unknown,
    /// `org.bluez.Reason.Timeout`: link supervision timeout (out of range).
    Timeout,
    /// `org.bluez.Reason.Local`: this host ended it (panel, bluetoothctl, auris).
    Local,
    /// `org.bluez.Reason.Remote`: the AirPods ended it.
    Remote,
    /// `org.bluez.Reason.Authentication`.
    Authentication,
    /// `org.bluez.Reason.Suspend`: this host ended it for suspend.
    Suspend,
    /// Any other name.
    Other(String),
}

impl Reason {
    /// Parse the reason name from the signal.
    pub fn from_bluez(name: &str) -> Self {
        match name {
            "org.bluez.Reason.Unknown" => Self::Unknown,
            "org.bluez.Reason.Timeout" => Self::Timeout,
            "org.bluez.Reason.Local" => Self::Local,
            "org.bluez.Reason.Remote" => Self::Remote,
            "org.bluez.Reason.Authentication" => Self::Authentication,
            "org.bluez.Reason.Suspend" => Self::Suspend,
            other => Self::Other(other.to_owned()),
        }
    }

    fn skip_reason(&self) -> &'static str {
        match self {
            Self::Unknown => "reason_unknown",
            Self::Timeout => "reason_timeout",
            Self::Local => "reason_local",
            Self::Remote => "reason_remote",
            Self::Authentication => "reason_authentication",
            Self::Suspend => "reason_suspend",
            Self::Other(_) => "reason_unrecognised",
        }
    }
}

/// Adapter and device state read when the `Disconnected` signal arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkFacts {
    /// Adapter `Powered`.
    pub powered: bool,
    /// Device `Paired`. `Trusted` is not required: pairing through
    /// `bluetoothctl` or some panels leaves it unset, and a paired device
    /// accepts an outgoing `Device1.Connect` without it.
    pub paired: bool,
    /// Device `Blocked`.
    pub blocked: bool,
}

/// How one `Device1.Connect` attempt ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// BlueZ connected the device.
    Connected,
    /// Refused as in progress or busy (`br-connection-busy`).
    Busy,
    /// Any other error, or a timeout; never retried.
    Failed(String),
    /// The fresh re-check before calling Connect failed; Connect was not called.
    NotReady(&'static str),
}

/// One host of a 0x002E connected-devices report, as [`decide`] needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListedHost {
    /// Host address, in wire (display) order.
    pub address: Address,
    /// Info byte 0 says this host's link to the accessory is up.
    pub link_up: bool,
}

impl ListedHost {
    /// A listed host with a known link state.
    pub fn new(address: Address, link_up: bool) -> Self {
        Self { address, link_up }
    }
}

/// Something the session observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Opcode 0x002E, hosts in wire order.
    ConnectedDevices(Vec<ListedHost>),
    /// BlueZ `Connected` went false to true: a new link session begins, and
    /// no earlier 0x002E report describes it.
    LinkSessionStarted,
    /// Opcode 0x0011 relayed from another host: it is an Apple host.
    Relay {
        /// Sending host.
        sender: Address,
    },
    /// Ear detection report.
    Ear {
        /// Both buds reported in the case.
        both_in_case: bool,
    },
    /// BlueZ `Device1.Disconnected` for the AirPods.
    Disconnected {
        /// Parsed reason name.
        reason: Reason,
        /// Adapter and device state at the signal; `None` when unreadable.
        facts: Option<LinkFacts>,
        /// The case lid is known closed.
        lid_closed: bool,
    },
    /// BlueZ `Connected` for the AirPods.
    Connected(bool),
    /// The adapter or bluetoothd went away.
    AdapterGone,
    /// The watched adapter or device changed.
    IdentityChanged,
    /// An explicit `reconnect` or `connect-once` command.
    ManualCommand,
    /// `[handoff] enabled` changed.
    SetEnabled(bool),
    /// Periodic tick; fires a due connect.
    Tick,
    /// A connect started by [`Action::Connect`] finished.
    ConnectFinished {
        /// Sequence the connect belonged to.
        seq: u64,
        /// Result.
        outcome: Outcome,
    },
}

/// A side effect or decision for the session to perform or log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// The relay-learned Apple host set changed: save it.
    Persist(Vec<Address>),
    /// A rejoin sequence starts; the first connect is due after [`REJOIN_DELAY`].
    Scheduled {
        /// The Apple host the 0x002E report blames for the drop.
        host: Address,
        /// How that host was recognised as Apple.
        matched: HostMatch,
        /// Which qualifying condition matched.
        kind: MatchKind,
    },
    /// A qualifying eviction held back only by [`RATE_LIMIT`]: the connect
    /// is scheduled for when the window expires instead of being dropped.
    Deferred {
        /// The Apple host the 0x002E report blames for the drop.
        host: Address,
        /// How that host was recognised as Apple.
        matched: HostMatch,
        /// Which qualifying condition matched.
        kind: MatchKind,
        /// Wait before the connect: the rest of the rate-limit window plus
        /// [`REJOIN_DELAY`].
        delay: Duration,
    },
    /// A disconnect that does not qualify.
    Skipped(&'static str),
    /// A scheduled rejoin was abandoned before or during a connect.
    Cancelled(&'static str),
    /// Re-check the adapter and device, then call `Device1.Connect` once.
    Connect {
        /// Sequence to report back in [`Event::ConnectFinished`].
        seq: u64,
        /// 1-based attempt number.
        attempt: u8,
    },
    /// A busy connect will be tried again.
    Retry {
        /// Attempt number of the next connect.
        attempt: u8,
        /// Delay before it.
        wait: Duration,
    },
    /// The rejoin connected; AAP recovery follows the new local link.
    Rejoined,
    /// The rejoin stopped on an error (warn).
    GaveUp(String),
    /// Evicted again soon after a rejoin: rejoin is off (warn).
    EvictionLoop,
}

#[derive(Debug, Clone, Copy)]
struct Pending {
    seq: u64,
    attempt: u8,
    due: Instant,
    in_flight: bool,
    /// The condition that qualified the drop. Published as the reconnect
    /// reason, so it has to outlive the [`Action::Scheduled`] that logged it.
    kind: MatchKind,
}

/// Everything [`decide`] remembers.
#[derive(Debug, Clone)]
pub struct RejoinState {
    /// `[handoff] enabled`.
    pub handoff_enabled: bool,
    /// `[handoff] rejoin_after_eviction`.
    pub allowed: bool,
    /// Local adapter address.
    pub local: Option<Address>,
    known: Vec<Address>,
    ouis: Arc<AppleOuis>,
    /// Latest 0x002E report of the current link session and when it arrived.
    /// Kept across the AAP reset, which arrives before the `Disconnected`
    /// signal, and cleared by [`Event::LinkSessionStarted`] so a report from
    /// an earlier session is never evidence for a later drop.
    last_report: Option<(Instant, Vec<ListedHost>)>,
    both_in_case: bool,
    last_manual: Option<Instant>,
    last_sequence: Option<Instant>,
    last_success: Option<Instant>,
    looped: bool,
    pending: Option<Pending>,
    seq: u64,
}

impl RejoinState {
    /// Fresh state with the persisted relay-learned Apple hosts and the Apple
    /// OUI set. An empty `known` list is normal: the OUI set alone recognises
    /// a Mac or iPhone seen for the first time.
    pub fn new(
        handoff_enabled: bool,
        allowed: bool,
        known: Vec<Address>,
        ouis: Arc<AppleOuis>,
    ) -> Self {
        let mut known = known;
        known.dedup();
        let excess = known.len().saturating_sub(KNOWN_HOSTS_CAP);
        known.drain(..excess);
        Self {
            handoff_enabled,
            allowed,
            local: None,
            known,
            ouis,
            last_report: None,
            both_in_case: false,
            last_manual: None,
            last_sequence: None,
            last_success: None,
            looped: false,
            pending: None,
            seq: 0,
        }
    }

    /// Relay-learned Apple hosts, oldest first.
    pub fn known(&self) -> &[Address] {
        &self.known
    }

    /// A rejoin is waiting or connecting.
    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Sequence number of the rejoin that is waiting or connecting. It
    /// changes for every new sequence, so a reader can tell one from the
    /// next even when they follow each other immediately.
    pub fn pending_seq(&self) -> Option<u64> {
        self.pending.map(|p| p.seq)
    }

    /// `Device1.Connect` calls started in the pending sequence: `0` while it
    /// is only scheduled, `1` while the first connect is in flight or after
    /// it was refused as busy, and so on.
    pub fn pending_attempt(&self) -> u8 {
        self.pending
            .map_or(0, |p| p.attempt - 1 + u8::from(p.in_flight))
    }

    /// Which condition qualified the pending sequence.
    pub fn pending_kind(&self) -> Option<MatchKind> {
        self.pending.map(|p| p.kind)
    }

    /// How `address` is recognised as an Apple host: a relay sender first,
    /// else an Apple OUI. This host never matches.
    pub fn match_host(&self, address: Address) -> Option<HostMatch> {
        if self.local == Some(address) {
            None
        } else if self.known.contains(&address) {
            Some(HostMatch::Relay)
        } else if self.ouis.is_apple(address) {
            Some(HostMatch::Oui)
        } else {
            None
        }
    }

    /// What the latest 0x002E report of this link session says; `None`
    /// without a report. The first report on a link counts: no baseline is
    /// needed.
    pub fn device_report_evidence(&self, now: Instant) -> Option<ReportEvidence> {
        let (at, list) = self.last_report.as_ref()?;
        let pick = |only_up: bool| {
            list.iter()
                .rev()
                .filter(|h| !only_up || h.link_up)
                .find_map(|h| self.match_host(h.address).map(|m| (h.address, m)))
        };
        let own_link_down = self
            .local
            .is_some_and(|local| list.iter().any(|h| h.address == local && !h.link_up));
        Some(ReportEvidence {
            age: now.saturating_duration_since(*at),
            apple_host: pick(false),
            apple_host_link_up: pick(true),
            own_link_down,
        })
    }

    /// Hosts other than this one in the latest 0x002E report, wire order.
    pub fn last_report_others(&self) -> Vec<Address> {
        self.last_report
            .as_ref()
            .map(|(_, list)| {
                list.iter()
                    .map(|h| h.address)
                    .filter(|a| self.local != Some(*a))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn cancel(&mut self, why: &'static str) -> Vec<Action> {
        match self.pending.take() {
            Some(_) => vec![Action::Cancelled(why)],
            None => Vec::new(),
        }
    }

    /// The evicting Apple host and the condition that matched, or why this
    /// disconnect does not qualify.
    fn qualify(
        &self,
        reason: &Reason,
        facts: Option<LinkFacts>,
        lid_closed: bool,
        now: Instant,
    ) -> Result<(Address, HostMatch, MatchKind, Duration), &'static str> {
        let within = |at: Option<Instant>, window: Duration| {
            at.is_some_and(|t| now.saturating_duration_since(t) < window)
        };
        if !self.handoff_enabled {
            return Err("handoff_disabled");
        }
        if !self.allowed {
            return Err("rejoin_after_eviction_off");
        }
        // `Remote` is the AirPods dropping this host; `Timeout` is the link
        // dying on its own. Neither can be a manual disconnect, which BlueZ
        // reports as `Local`.
        if !matches!(reason, Reason::Remote | Reason::Timeout) {
            return Err(reason.skip_reason());
        }
        if self.looped {
            return Err("eviction_loop");
        }
        // The rate limit delays a sequence; it never drops one. An eviction
        // inside the window is real, and forgetting it would leave the
        // AirPods on the other host until the wearer noticed. Every other
        // check still refuses outright: only time is missing here, and time
        // passes.
        let defer = self
            .last_sequence
            .and_then(|t| RATE_LIMIT.checked_sub(now.saturating_duration_since(t)))
            .filter(|left| !left.is_zero())
            .unwrap_or_default();
        if within(self.last_manual, MANUAL_QUIET) {
            return Err("manual_command");
        }
        // Any of the three conditions qualifies. "joined" is a report that
        // landed with the drop; "connected" is a report of this session, at
        // any age, listing an Apple host whose link is up; "link_lost" is the
        // same report evidence after a supervision timeout. A timeout never
        // takes the "joined" window: a report that arrived with the drop says
        // nothing about who kept the AirPods, only the link-up byte does.
        let timeout = *reason == Reason::Timeout;
        let evidence = self.device_report_evidence(now);
        let host = match evidence {
            Some(ReportEvidence {
                age,
                apple_host: Some(host),
                ..
            }) if !timeout && age <= EVICTION_WINDOW => (host.0, host.1, MatchKind::Joined),
            Some(ReportEvidence {
                apple_host_link_up: Some(host),
                ..
            }) => (
                host.0,
                host.1,
                if timeout {
                    MatchKind::LinkLost
                } else {
                    MatchKind::Connected
                },
            ),
            Some(ReportEvidence {
                apple_host: Some(_),
                ..
            }) => return Err("no_apple_host_link_up"),
            _ => return Err("no_recent_device_report_with_apple_host"),
        };
        if self.both_in_case {
            return Err("buds_in_case");
        }
        if lid_closed {
            return Err("case_lid_closed");
        }
        let Some(facts) = facts else {
            return Err("device_state_unreadable");
        };
        if !facts.powered {
            return Err("adapter_off");
        }
        if !facts.paired {
            return Err("not_paired");
        }
        if facts.blocked {
            return Err("blocked");
        }
        Ok((host.0, host.1, host.2, defer))
    }
}

/// Fold one event into the state and return the side effects, in order.
pub fn decide(s: &mut RejoinState, event: Event, now: Instant) -> Vec<Action> {
    match event {
        Event::ConnectedDevices(list) => {
            s.last_report = Some((now, list));
            Vec::new()
        }
        Event::Relay { sender } => {
            if s.local == Some(sender) || s.known.contains(&sender) {
                return Vec::new();
            }
            s.known.push(sender);
            if s.known.len() > KNOWN_HOSTS_CAP {
                s.known.remove(0);
            }
            vec![Action::Persist(s.known.clone())]
        }
        Event::Ear { both_in_case } => {
            s.both_in_case = both_in_case;
            // A deferred connect can wait a minute, long enough for the
            // wearer to put the AirPods away in the meantime.
            if both_in_case {
                s.cancel("buds_in_case")
            } else {
                Vec::new()
            }
        }
        Event::Disconnected {
            reason,
            facts,
            lid_closed,
        } => {
            // Any disconnect this rejoin did not cause ends it.
            let mut out = s.cancel("disconnected_again");
            // A drop this soon after a rejoin stops the rejoining, whether
            // the AirPods pushed this host off again or the link died again.
            if s.handoff_enabled
                && s.allowed
                && matches!(reason, Reason::Remote | Reason::Timeout)
                && !s.looped
                && s.last_success
                    .is_some_and(|t| now.saturating_duration_since(t) < LOOP_WINDOW)
            {
                s.looped = true;
                out.push(Action::EvictionLoop);
                return out;
            }
            match s.qualify(&reason, facts, lid_closed, now) {
                Err(why) => out.push(Action::Skipped(why)),
                Ok((host, matched, kind, defer)) => {
                    s.seq += 1;
                    s.last_sequence = Some(now);
                    // One report is evidence for one sequence only.
                    s.last_report = None;
                    let delay = defer + REJOIN_DELAY;
                    s.pending = Some(Pending {
                        seq: s.seq,
                        attempt: 1,
                        due: now + delay,
                        in_flight: false,
                        kind,
                    });
                    out.push(if defer.is_zero() {
                        Action::Scheduled {
                            host,
                            matched,
                            kind,
                        }
                    } else {
                        Action::Deferred {
                            host,
                            matched,
                            kind,
                            delay,
                        }
                    });
                }
            }
            out
        }
        Event::LinkSessionStarted => {
            // Nothing observed before this link describes it.
            s.last_report = None;
            Vec::new()
        }
        Event::Connected(true) => match s.pending {
            // Most likely this rejoin's own connect; its result follows.
            Some(p) if p.in_flight => Vec::new(),
            Some(_) => s.cancel("connected_by_other_means"),
            None => Vec::new(),
        },
        // The drop itself; the Disconnected signal already decided.
        Event::Connected(false) => Vec::new(),
        Event::AdapterGone => s.cancel("adapter_gone"),
        Event::IdentityChanged => {
            // Nothing about the old AirPods applies to the new ones: a first
            // eviction of a different device must not look like a loop.
            s.last_report = None;
            s.both_in_case = false;
            s.last_success = None;
            s.looped = false;
            s.cancel("identity_changed")
        }
        Event::ManualCommand => {
            s.last_manual = Some(now);
            s.looped = false;
            s.cancel("manual_command")
        }
        Event::SetEnabled(enabled) => {
            s.handoff_enabled = enabled;
            if enabled {
                Vec::new()
            } else {
                s.cancel("handoff_disabled")
            }
        }
        Event::Tick => {
            let Some(p) = s.pending.as_mut() else {
                return Vec::new();
            };
            if p.in_flight || now < p.due {
                return Vec::new();
            }
            if !(s.handoff_enabled && s.allowed) {
                return s.cancel("handoff_disabled");
            }
            p.in_flight = true;
            vec![Action::Connect {
                seq: p.seq,
                attempt: p.attempt,
            }]
        }
        Event::ConnectFinished { seq, outcome } => {
            let Some(p) = s.pending.as_mut().filter(|p| p.seq == seq && p.in_flight) else {
                return Vec::new();
            };
            match outcome {
                Outcome::Connected => {
                    s.pending = None;
                    s.last_success = Some(now);
                    vec![Action::Rejoined]
                }
                Outcome::Busy if p.attempt < MAX_ATTEMPTS => {
                    let wait = BUSY_RETRY[usize::from(p.attempt - 1)];
                    p.attempt += 1;
                    p.due = now + wait;
                    p.in_flight = false;
                    vec![Action::Retry {
                        attempt: p.attempt,
                        wait,
                    }]
                }
                Outcome::Busy => {
                    s.pending = None;
                    vec![Action::GaveUp(format!(
                        "still busy after {MAX_ATTEMPTS} connects (br-connection-busy); \
                         not disconnecting to clear it"
                    ))]
                }
                Outcome::Failed(error) => {
                    s.pending = None;
                    vec![Action::GaveUp(error)]
                }
                Outcome::NotReady(why) => {
                    s.pending = None;
                    vec![Action::Cancelled(why)]
                }
            }
        }
    }
}

/// IEEE OUI registry from the `hwdata` package, tried first.
pub const HWDATA_OUI_PATH: &str = "/usr/share/hwdata/oui.txt";
/// systemd hwdb OUI source, tried when `hwdata` gives no Apple OUI.
pub const HWDB_OUI_PATH: &str = "/usr/lib/udev/hwdb.d/20-OUI.hwdb";
/// Registered organisation that marks an Apple OUI, compared after trimming
/// and ignoring ASCII case. Nothing else matches ("Apple, Inc" without the
/// full stop, "Apple Computer", "Applied Materials").
pub const APPLE_VENDOR: &str = "Apple, Inc.";
/// Used only when neither registry file gives an Apple OUI. Each entry is
/// copied from `/usr/share/hwdata/oui.txt`, where it reads "Apple, Inc.".
pub const BUILT_IN_APPLE_OUIS: [[u8; 3]; 9] = [
    [0xFC, 0xB2, 0x14],
    [0x00, 0x03, 0x93],
    [0x00, 0x05, 0x02],
    [0x3C, 0x22, 0xFB],
    [0x58, 0xAD, 0x12],
    [0x60, 0xFD, 0xA6],
    [0xA4, 0x83, 0xE7],
    [0xAC, 0xBC, 0x32],
    [0xF0, 0xEE, 0x7A],
];

/// Bit 1 of the first octet: a locally administered MAC-48, never a
/// registered public address.
const LOCALLY_ADMINISTERED: u8 = 0x02;

/// Where the Apple OUI set came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OuiSource {
    /// [`HWDATA_OUI_PATH`].
    Hwdata,
    /// [`HWDB_OUI_PATH`].
    Hwdb,
    /// [`BUILT_IN_APPLE_OUIS`].
    BuiltIn,
}

impl OuiSource {
    /// Name for logs: the file path, or `built-in`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hwdata => HWDATA_OUI_PATH,
            Self::Hwdb => HWDB_OUI_PATH,
            Self::BuiltIn => "built-in",
        }
    }
}

/// What the latest 0x002E report of the current link session says about the
/// Apple hosts on it. Returned by [`RejoinState::device_report_evidence`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReportEvidence {
    /// How long ago the report arrived.
    pub age: Duration,
    /// Apple host it lists, last in wire order, never this host.
    pub apple_host: Option<(Address, HostMatch)>,
    /// The same, restricted to hosts whose link is up (info byte 0 `0x02`).
    pub apple_host_link_up: Option<(Address, HostMatch)>,
    /// The report lists this host's own address with info byte 0 `0x00`: the
    /// AirPods no longer count this link as up, even while it still carries
    /// their reports. Evidence only, never a reason to act.
    pub own_link_down: bool,
}

/// Which condition qualified a `Remote` drop for a rejoin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchKind {
    /// An Apple host appeared in a report within [`EVICTION_WINDOW`] of the
    /// drop: it connected and pushed this host off.
    Joined,
    /// The latest report of the ended link session lists an Apple host whose
    /// link is up, at any age: it already owned the AirPods.
    Connected,
    /// The link died of a supervision timeout while the latest report of the
    /// ended session listed an Apple host whose link is up: a bud swap or a
    /// range loss took this host's link and left the Apple host's in place.
    LinkLost,
}

impl MatchKind {
    /// Name for logs.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Joined => "joined",
            Self::Connected => "connected",
            Self::LinkLost => "link_lost",
        }
    }
}

/// How a listed host was recognised as an Apple host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostMatch {
    /// It sent this host a smart routing relay (0x0011).
    Relay,
    /// Its OUI is registered to Apple, Inc.
    Oui,
}

impl HostMatch {
    /// Name for logs.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Relay => "relay",
            Self::Oui => "oui",
        }
    }
}

/// Apple OUIs, built once at daemon start and shared read-only.
#[derive(Debug, Clone)]
pub struct AppleOuis {
    set: HashSet<[u8; 3]>,
    source: OuiSource,
}

impl Default for AppleOuis {
    fn default() -> Self {
        Self::built_in()
    }
}

impl AppleOuis {
    /// Read [`HWDATA_OUI_PATH`], else [`HWDB_OUI_PATH`], else the built-in
    /// list. A missing, unreadable or Apple-free file falls through.
    pub fn load() -> Self {
        Self::load_from(Path::new(HWDATA_OUI_PATH), Path::new(HWDB_OUI_PATH))
    }

    /// [`Self::load`] with explicit paths.
    pub fn load_from(hwdata: &Path, hwdb: &Path) -> Self {
        let read = |path: &Path| {
            fs::read(path)
                .ok()
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        };
        if let Some(set) = read(hwdata)
            .map(|t| parse_oui_txt(&t))
            .filter(|s| !s.is_empty())
        {
            return Self {
                set,
                source: OuiSource::Hwdata,
            };
        }
        if let Some(set) = read(hwdb)
            .map(|t| parse_oui_hwdb(&t))
            .filter(|s| !s.is_empty())
        {
            return Self {
                set,
                source: OuiSource::Hwdb,
            };
        }
        Self::built_in()
    }

    /// Only [`BUILT_IN_APPLE_OUIS`].
    pub fn built_in() -> Self {
        Self {
            set: BUILT_IN_APPLE_OUIS.into_iter().collect(),
            source: OuiSource::BuiltIn,
        }
    }

    /// Where the set came from.
    pub fn source(&self) -> OuiSource {
        self.source
    }

    /// Number of Apple OUIs.
    pub fn len(&self) -> usize {
        self.set.len()
    }

    /// No Apple OUI at all.
    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }

    /// The canonical (display order) address has an Apple OUI and is not
    /// locally administered.
    pub fn is_apple(&self, address: Address) -> bool {
        address[0] & LOCALLY_ADMINISTERED == 0
            && self.set.contains(&[address[0], address[1], address[2]])
    }
}

fn is_apple_vendor(vendor: &str) -> bool {
    vendor.trim().eq_ignore_ascii_case(APPLE_VENDOR)
}

/// Exactly six hex digits to three octets.
fn hex_oui(digits: &str) -> Option<[u8; 3]> {
    if digits.len() != 6 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let octet = |i: usize| u8::from_str_radix(&digits[i..i + 2], 16).ok();
    Some([octet(0)?, octet(2)?, octet(4)?])
}

/// Apple OUIs from `hwdata` oui.txt: lines like
/// `FC-B2-14   (hex)\t\tApple, Inc.`. Other lines, including the
/// `(base 16)` duplicates, and malformed prefixes are ignored.
pub fn parse_oui_txt(text: &str) -> HashSet<[u8; 3]> {
    text.lines()
        .filter_map(|line| {
            let (prefix, vendor) = line.split_once("(hex)")?;
            let prefix = prefix.trim();
            let b = prefix.as_bytes();
            if b.len() != 8 || b[2] != b'-' || b[5] != b'-' {
                return None;
            }
            let oui = hex_oui(&prefix.replace('-', ""))?;
            is_apple_vendor(vendor).then_some(oui)
        })
        .collect()
}

/// Apple OUIs from the systemd hwdb source: a record of `OUI:FCB214*` match
/// lines then indented ` ID_OUI_FROM_DATABASE=Apple, Inc.`. Longer
/// (MA-M, MA-S) prefixes are not whole OUIs and are ignored.
pub fn parse_oui_hwdb(text: &str) -> HashSet<[u8; 3]> {
    let mut set = HashSet::new();
    let mut matches: Vec<[u8; 3]> = Vec::new();
    let mut in_properties = false;
    for line in text.lines() {
        if let Some(pattern) = line.strip_prefix("OUI:") {
            if in_properties {
                matches.clear();
                in_properties = false;
            }
            if let Some(oui) = pattern.trim_end().strip_suffix('*').and_then(hex_oui) {
                matches.push(oui);
            }
        } else if !line.trim().is_empty() && line.starts_with(char::is_whitespace) {
            in_properties = true;
            let property = line.trim_start();
            if let Some(vendor) = property.strip_prefix("ID_OUI_FROM_DATABASE=") {
                if is_apple_vendor(vendor) {
                    set.extend(matches.iter().copied());
                }
            }
        } else {
            // Blank line, comment or anything unexpected ends the record.
            matches.clear();
            in_properties = false;
        }
    }
    set
}

#[derive(serde::Serialize, serde::Deserialize)]
struct KnownHosts {
    apple_hosts: Vec<String>,
}

/// `$CACHE_DIRECTORY/apple_hosts.json` (systemd), else
/// `$XDG_CACHE_HOME/aurisd/apple_hosts.json`, next to the battery cache.
pub fn known_hosts_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CACHE_DIRECTORY") {
        return Some(PathBuf::from(dir).join("apple_hosts.json"));
    }
    dirs::cache_dir().map(|d| d.join("aurisd").join("apple_hosts.json"))
}

/// Read the relay-learned Apple hosts, oldest first. Missing or malformed
/// files and unparsable entries give nothing; the OUI set still recognises
/// Apple hosts without them.
pub fn load_known_hosts(path: &Path) -> Vec<Address> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(file) = serde_json::from_str::<KnownHosts>(&text) else {
        return Vec::new();
    };
    let mut hosts: Vec<Address> = Vec::new();
    for address in file.apple_hosts.iter().filter_map(|a| a.parse().ok()) {
        if !hosts.contains(&address) {
            hosts.push(address);
        }
    }
    let excess = hosts.len().saturating_sub(KNOWN_HOSTS_CAP);
    hosts.drain(..excess);
    hosts
}

/// Write the known Apple hosts atomically (tmp file, then rename), at most
/// [`KNOWN_HOSTS_CAP`] of the newest.
pub fn save_known_hosts(path: &Path, hosts: &[Address]) -> std::io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("known hosts path has no parent"))?;
    fs::create_dir_all(dir)?;
    let start = hosts.len().saturating_sub(KNOWN_HOSTS_CAP);
    let body = serde_json::to_vec(&KnownHosts {
        apple_hosts: hosts[start..].iter().map(Address::to_string).collect(),
    })?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(std::io::Error::other)?
        .as_nanos();
    let tmp = dir.join(format!(".apple_hosts-{}-{nonce}.tmp", std::process::id()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    let result = (|| {
        file.write_all(&body)?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(s: &str) -> Address {
        s.parse().unwrap()
    }

    fn host() -> Address {
        addr("5C:F3:70:0D:0E:0F")
    }

    /// The user's Mac.
    fn mac() -> Address {
        addr("FC:B2:14:0A:0B:0C")
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    const READY: LinkFacts = LinkFacts {
        powered: true,
        paired: true,
        blocked: false,
    };

    /// The built-in Apple OUIs, which include the Mac's FC:B2:14.
    fn ouis() -> Arc<AppleOuis> {
        Arc::new(AppleOuis::built_in())
    }

    /// No Apple OUI at all: only relays recognise a host.
    fn no_ouis() -> Arc<AppleOuis> {
        Arc::new(AppleOuis {
            set: HashSet::new(),
            source: OuiSource::BuiltIn,
        })
    }

    /// A host whose OUI (28-6F-B9) oui.txt registers to Nokia Shanghai Bell.
    fn nokia() -> Address {
        addr("28:6F:B9:00:11:22")
    }

    /// Listed with info byte 0 = 0x02: its link to the AirPods is up.
    fn up(address: Address) -> ListedHost {
        ListedHost::new(address, true)
    }

    /// Listed with info byte 0 = 0x00 or 0x01: listed, link not up.
    fn down(address: Address) -> ListedHost {
        ListedHost::new(address, false)
    }

    /// The evidence a report of `age` gives for one Apple host.
    fn evidence(
        age: Duration,
        apple_host: Option<(Address, HostMatch)>,
        link_up: bool,
    ) -> Option<ReportEvidence> {
        Some(ReportEvidence {
            age,
            apple_host,
            apple_host_link_up: apple_host.filter(|_| link_up),
            own_link_down: false,
        })
    }

    fn remote() -> Event {
        drop_with(Reason::Remote)
    }

    fn drop_with(reason: Reason) -> Event {
        Event::Disconnected {
            reason,
            facts: Some(READY),
            lid_closed: false,
        }
    }

    /// Enabled, the Mac known from an earlier relay, and a long-standing link
    /// whose last 0x002E report lists only this host.
    fn linked(t0: Instant) -> RejoinState {
        let mut s = RejoinState::new(true, true, vec![mac()], ouis());
        s.local = Some(host());
        assert!(decide(&mut s, Event::ConnectedDevices(vec![up(host())]), t0).is_empty());
        s
    }

    /// The 21:17 sequence up to the drop: a count 2 report listing the Mac,
    /// ECONNRESET about 500 ms later, BlueZ Connected=false.
    fn evicted(s: &mut RejoinState, at: Instant) {
        assert!(decide(
            s,
            Event::ConnectedDevices(vec![up(host()), down(mac())]),
            at
        )
        .is_empty());
        assert!(decide(s, Event::Connected(false), at + ms(520)).is_empty());
    }

    /// Feed events in order and collect every action.
    fn run(s: &mut RejoinState, events: Vec<(Event, Instant)>) -> Vec<Action> {
        events
            .into_iter()
            .flat_map(|(event, at)| decide(s, event, at))
            .collect()
    }

    fn scheduled(actions: &[Action]) -> usize {
        actions
            .iter()
            .filter(|a| matches!(a, Action::Scheduled { .. }))
            .count()
    }

    /// From the drop on: a duplicate Connected=false, the wait, one connect
    /// that succeeds. Returns every action.
    fn finish(s: &mut RejoinState, dropped: Instant) -> Vec<Action> {
        let mut out = decide(s, Event::Connected(false), dropped + ms(10));
        out.extend(decide(s, Event::Tick, dropped + ms(2900)));
        let seq = fire(s, dropped + REJOIN_DELAY);
        out.extend(decide(s, Event::Tick, dropped + ms(3250)));
        // This rejoin's own Connected=true does not cancel it.
        out.extend(decide(s, Event::Connected(true), dropped + ms(3600)));
        let ok = Event::ConnectFinished {
            seq,
            outcome: Outcome::Connected,
        };
        assert_eq!(decide(s, ok, dropped + ms(3700)), [Action::Rejoined]);
        assert!(!s.is_pending());
        out
    }

    fn schedule(s: &mut RejoinState, at: Instant) {
        evicted(s, at);
        assert_eq!(
            decide(s, remote(), at + ms(540)),
            [Action::Scheduled {
                host: mac(),
                matched: HostMatch::Relay,
                kind: MatchKind::Joined
            }]
        );
    }

    /// Tick to the first connect and return its sequence.
    fn fire(s: &mut RejoinState, at: Instant) -> u64 {
        match decide(s, Event::Tick, at).as_slice() {
            [Action::Connect { seq, .. }] => *seq,
            other => panic!("expected a connect, got {other:?}"),
        }
    }

    #[test]
    fn reason_names_match_bluez_5_87() {
        assert_eq!(
            Reason::from_bluez("org.bluez.Reason.Remote"),
            Reason::Remote
        );
        assert_eq!(Reason::from_bluez("org.bluez.Reason.Local"), Reason::Local);
        assert_eq!(
            Reason::from_bluez("org.bluez.Reason.Timeout"),
            Reason::Timeout
        );
        assert_eq!(
            Reason::from_bluez("org.bluez.Reason.Authentication"),
            Reason::Authentication
        );
        assert_eq!(
            Reason::from_bluez("org.bluez.Reason.Suspend"),
            Reason::Suspend
        );
        assert_eq!(
            Reason::from_bluez("org.bluez.Reason.Unknown"),
            Reason::Unknown
        );
        assert_eq!(
            Reason::from_bluez("Remote"),
            Reason::Other("Remote".to_owned())
        );
    }

    #[test]
    fn replay_of_the_21_17_fresh_connect_schedules_exactly_one_rejoin() {
        // Header 01 00: the Mac connects fresh and is listed, then the reset
        // about 0.5 s later.
        let t0 = Instant::now();
        let mut s = RejoinState::new(true, true, Vec::new(), ouis());
        s.local = Some(host());
        // Learned from a smart routing relay earlier in the session.
        assert_eq!(
            decide(&mut s, Event::Relay { sender: mac() }, t0),
            [Action::Persist(vec![mac()])]
        );
        assert!(decide(&mut s, Event::Relay { sender: mac() }, t0).is_empty());
        assert!(decide(&mut s, Event::ConnectedDevices(vec![up(host())]), t0).is_empty());
        let report = t0 + Duration::from_secs(30);
        let dropped = report + ms(560);
        let mut actions = run(
            &mut s,
            vec![
                (
                    Event::ConnectedDevices(vec![up(host()), down(mac())]),
                    report,
                ),
                (Event::Connected(false), report + ms(540)),
            ],
        );
        assert_eq!(
            s.device_report_evidence(dropped),
            evidence(ms(560), Some((mac(), HostMatch::Relay)), false)
        );
        actions.extend(decide(&mut s, remote(), dropped));
        assert_eq!(
            actions,
            [Action::Scheduled {
                host: mac(),
                matched: HostMatch::Relay,
                kind: MatchKind::Joined
            }]
        );
        actions.extend(finish(&mut s, dropped));
        assert_eq!(scheduled(&actions), 1);
    }

    #[test]
    fn replay_of_the_21_47_eviction_with_the_mac_already_listed_schedules_exactly_one_rejoin() {
        // Times below are relative to the BlueZ Connected event.
        let t0 = Instant::now();
        let at = |secs: u64, millis: u64| t0 + Duration::from_secs(secs) + ms(millis);
        let mut s = RejoinState::new(true, true, Vec::new(), ouis());
        s.local = Some(host());
        let both = || Event::ConnectedDevices(vec![up(host()), down(mac())]);
        let dropped = at(27, 107);
        let mut actions = run(
            &mut s,
            vec![
                // Header 01 00: the Mac is listed although it was off.
                (both(), at(2, 745)),
                // Relays from the Mac, then header 01 02.
                (Event::Relay { sender: mac() }, at(12, 765)),
                (both(), at(12, 765)),
                (both(), at(17, 0)),
                (both(), at(17, 100)),
                (both(), at(17, 200)),
                // Header 01 02, 205 ms before the drop.
                (both(), at(26, 842)),
                (Event::Connected(false), dropped),
            ],
        );
        assert_eq!(actions, [Action::Persist(vec![mac()])]);
        assert_eq!(
            s.device_report_evidence(dropped),
            evidence(ms(265), Some((mac(), HostMatch::Relay)), false)
        );
        actions.extend(decide(&mut s, remote(), dropped));
        assert_eq!(
            actions,
            [
                Action::Persist(vec![mac()]),
                Action::Scheduled {
                    host: mac(),
                    matched: HostMatch::Relay,
                    kind: MatchKind::Joined
                }
            ]
        );
        actions.extend(finish(&mut s, dropped));
        assert_eq!(scheduled(&actions), 1);
    }

    #[test]
    fn replay_of_the_13_26_47_drop_by_a_mac_that_already_owned_the_airpods() {
        // The Mac had been connected the whole time and the newest 0x002E
        // report was 56.261 s old, so the "joined" window cannot see it.
        // Its info byte 0 was 0x02 (link up), which is the evidence that
        // qualifies the drop.
        let t0 = Instant::now();
        let mut s = RejoinState::new(true, true, vec![mac()], ouis());
        s.local = Some(host());
        assert!(decide(&mut s, Event::LinkSessionStarted, t0).is_empty());
        let report = t0 + Duration::from_secs(10);
        let dropped = report + ms(56_261);
        // Both hosts up, exactly as logged.
        assert!(decide(
            &mut s,
            Event::ConnectedDevices(vec![up(mac()), up(host())]),
            report
        )
        .is_empty());
        assert_eq!(
            s.device_report_evidence(dropped),
            evidence(ms(56_261), Some((mac(), HostMatch::Relay)), true)
        );
        let mut actions = decide(&mut s, remote(), dropped);
        assert_eq!(
            actions,
            [Action::Scheduled {
                host: mac(),
                matched: HostMatch::Relay,
                kind: MatchKind::Connected
            }]
        );
        actions.extend(finish(&mut s, dropped));
        assert_eq!(scheduled(&actions), 1);
    }

    #[test]
    fn a_report_from_the_previous_link_session_is_not_evidence() {
        let t0 = Instant::now();
        let mut s = RejoinState::new(true, true, vec![mac()], ouis());
        s.local = Some(host());
        assert!(decide(
            &mut s,
            Event::ConnectedDevices(vec![up(mac()), up(host())]),
            t0
        )
        .is_empty());
        // BlueZ Connected false -> true: this link has no report of its own.
        assert!(decide(&mut s, Event::LinkSessionStarted, t0 + ms(500)).is_empty());
        assert_eq!(s.device_report_evidence(t0 + ms(600)), None);
        assert_eq!(
            decide(&mut s, remote(), t0 + ms(600)),
            [Action::Skipped("no_recent_device_report_with_apple_host")]
        );
        assert!(!s.is_pending());
    }

    #[test]
    fn a_listed_but_disconnected_mac_does_not_rejoin() {
        // The Mac logged Conn Disconnected 164 ms earlier and is still
        // listed, with info 00 15. Listed is not connected.
        let t0 = Instant::now();
        let mut s = RejoinState::new(true, true, vec![mac()], ouis());
        s.local = Some(host());
        assert!(decide(
            &mut s,
            Event::ConnectedDevices(vec![down(mac()), up(host())]),
            t0
        )
        .is_empty());
        let dropped = t0 + Duration::from_secs(40);
        assert_eq!(
            s.device_report_evidence(dropped),
            evidence(
                Duration::from_secs(40),
                Some((mac(), HostMatch::Relay)),
                false
            )
        );
        assert_eq!(
            decide(&mut s, remote(), dropped),
            [Action::Skipped("no_apple_host_link_up")]
        );
        assert!(!s.is_pending());
    }

    #[test]
    fn an_old_report_with_a_connected_mac_still_obeys_every_exclusion() {
        // The "connected" condition adds no exception: the same report that
        // otherwise qualifies a rejoin is refused for a local reason, for
        // both buds in the case, for a closed lid, and just after a manual
        // command.
        let t0 = Instant::now();
        let dropped = t0 + Duration::from_secs(56);
        let session = |ear: bool| {
            let mut s = RejoinState::new(true, true, vec![mac()], ouis());
            s.local = Some(host());
            decide(
                &mut s,
                Event::ConnectedDevices(vec![up(mac()), up(host())]),
                t0,
            );
            if ear {
                decide(&mut s, Event::Ear { both_in_case: true }, t0 + ms(10));
            }
            s
        };

        let mut s = session(false);
        assert_eq!(
            decide(&mut s, drop_with(Reason::Local), dropped),
            [Action::Skipped("reason_local")]
        );

        let mut s = session(true);
        assert_eq!(
            decide(&mut s, remote(), dropped),
            [Action::Skipped("buds_in_case")]
        );

        let mut s = session(false);
        let lid = Event::Disconnected {
            reason: Reason::Remote,
            facts: Some(READY),
            lid_closed: true,
        };
        assert_eq!(
            decide(&mut s, lid, dropped),
            [Action::Skipped("case_lid_closed")]
        );

        let mut s = session(false);
        decide(&mut s, Event::ManualCommand, dropped - ms(9_000));
        assert_eq!(
            decide(&mut s, remote(), dropped),
            [Action::Skipped("manual_command")]
        );
        assert!(!s.is_pending());
    }

    #[test]
    fn a_second_eviction_soon_after_a_connected_match_still_stops_the_loop() {
        let t0 = Instant::now();
        let mut s = RejoinState::new(true, true, vec![mac()], ouis());
        s.local = Some(host());
        decide(
            &mut s,
            Event::ConnectedDevices(vec![up(mac()), up(host())]),
            t0,
        );
        let dropped = t0 + Duration::from_secs(56);
        assert_eq!(scheduled(&decide(&mut s, remote(), dropped)), 1);
        // finish() asserts the Action::Rejoined at dropped + 3700 ms.
        finish(&mut s, dropped);
        // The new link lists the Mac again, and the AirPods drop this host
        // again within LOOP_WINDOW.
        let rejoined = dropped + ms(3_700);
        decide(&mut s, Event::LinkSessionStarted, rejoined);
        decide(
            &mut s,
            Event::ConnectedDevices(vec![up(mac()), up(host())]),
            rejoined + ms(50),
        );
        assert_eq!(
            decide(&mut s, remote(), rejoined + LOOP_WINDOW - ms(1)),
            [Action::EvictionLoop]
        );
        assert!(!s.is_pending());
    }

    #[test]
    fn local_and_other_reasons_never_rejoin() {
        for (reason, why) in [
            (Reason::Local, "reason_local"),
            (Reason::Authentication, "reason_authentication"),
            (Reason::Suspend, "reason_suspend"),
            (Reason::Unknown, "reason_unknown"),
            (Reason::Other("x".into()), "reason_unrecognised"),
        ] {
            let t0 = Instant::now();
            let mut s = linked(t0);
            evicted(&mut s, t0);
            assert_eq!(
                decide(&mut s, drop_with(reason), t0 + ms(600)),
                [Action::Skipped(why)]
            );
            assert!(!s.is_pending());
        }
    }

    /// The state while the Mac owns the AirPods: both hosts are listed with
    /// their link up, and this host is idle.
    fn yielded_to_the_mac(t0: Instant) -> RejoinState {
        let mut s = RejoinState::new(true, true, Vec::new(), ouis());
        s.local = Some(host());
        decide(&mut s, Event::LinkSessionStarted, t0);
        assert!(decide(
            &mut s,
            Event::ConnectedDevices(vec![up(mac()), up(host())]),
            t0
        )
        .is_empty());
        s
    }

    /// A bud comes out, then a report marks this host's own entry 0x00 with
    /// the Mac still 0x02, then the link times out.
    fn bud_swap(s: &mut RejoinState, t0: Instant) -> Instant {
        assert!(decide(
            s,
            Event::Ear {
                both_in_case: false
            },
            t0 + ms(79_000)
        )
        .is_empty());
        assert!(decide(
            s,
            Event::ConnectedDevices(vec![up(mac()), down(host())]),
            t0 + ms(89_000)
        )
        .is_empty());
        t0 + ms(109_000)
    }

    #[test]
    fn a_bud_swap_that_times_out_this_host_rejoins() {
        // The Mac owned the AirPods and this host was idle. The wearer took
        // the right bud out, the buds moved the radio link to the other bud,
        // a report marked this host's own entry 0x00 while it still arrived
        // over that link, and then the link died of a supervision timeout.
        // The Mac's link survived, so this host has to connect again.
        let t0 = Instant::now();
        let mut s = yielded_to_the_mac(t0);
        let dropped = bud_swap(&mut s, t0);
        let evidence = s.device_report_evidence(dropped).expect("a report");
        assert_eq!(evidence.age, ms(20_000));
        assert_eq!(evidence.apple_host_link_up.map(|(h, _)| h), Some(mac()));
        // Supporting evidence, logged and never required.
        assert!(evidence.own_link_down);
        assert!(decide(&mut s, Event::Connected(false), dropped).is_empty());
        assert_eq!(
            decide(&mut s, drop_with(Reason::Timeout), dropped + ms(20)),
            [Action::Scheduled {
                host: mac(),
                matched: HostMatch::Oui,
                kind: MatchKind::LinkLost
            }]
        );
        assert!(s.is_pending());
    }

    #[test]
    fn a_timeout_rejoins_without_the_own_link_down_hint() {
        // The 0x002E report can be older than the bud swap, so this host's
        // own entry still reads 0x02. The Apple host's link-up byte alone
        // qualifies the timeout.
        let t0 = Instant::now();
        let mut s = yielded_to_the_mac(t0);
        let dropped = t0 + Duration::from_secs(120);
        assert!(
            !s.device_report_evidence(dropped)
                .expect("a report")
                .own_link_down
        );
        assert_eq!(
            decide(&mut s, drop_with(Reason::Timeout), dropped),
            [Action::Scheduled {
                host: mac(),
                matched: HostMatch::Oui,
                kind: MatchKind::LinkLost
            }]
        );
    }

    #[test]
    fn a_timeout_without_an_apple_host_link_up_does_not_rejoin() {
        // The Mac is listed but its link is not up: nothing says an Apple
        // host kept the AirPods, so a timeout is just a timeout.
        let t0 = Instant::now();
        let mut s = linked(t0);
        evicted(&mut s, t0);
        assert_eq!(
            decide(&mut s, drop_with(Reason::Timeout), t0 + ms(600)),
            [Action::Skipped("no_apple_host_link_up")]
        );
        assert!(!s.is_pending());

        // The "joined" window never qualifies a timeout on its own.
        let mut s = linked(t0);
        assert!(decide(
            &mut s,
            Event::ConnectedDevices(vec![up(host()), down(mac())]),
            t0 + ms(300)
        )
        .is_empty());
        assert_eq!(
            decide(&mut s, drop_with(Reason::Timeout), t0 + ms(400)),
            [Action::Skipped("no_apple_host_link_up")]
        );

        // No Apple host in the report at all.
        let mut s = linked(t0);
        assert_eq!(
            decide(&mut s, drop_with(Reason::Timeout), t0 + ms(600)),
            [Action::Skipped("no_recent_device_report_with_apple_host")]
        );
        assert!(!s.is_pending());
    }

    #[test]
    fn a_timeout_with_both_buds_in_the_case_does_not_rejoin() {
        // Both buds in the case is the wearer putting the AirPods away; the
        // link dies of a timeout there every time.
        let t0 = Instant::now();
        let mut s = yielded_to_the_mac(t0);
        assert!(decide(&mut s, Event::Ear { both_in_case: true }, t0 + ms(10)).is_empty());
        assert_eq!(
            decide(
                &mut s,
                drop_with(Reason::Timeout),
                t0 + Duration::from_secs(30)
            ),
            [Action::Skipped("buds_in_case")]
        );
        assert!(!s.is_pending());
    }

    #[test]
    fn one_bud_in_the_case_never_blocks_a_timeout_rejoin() {
        // A single bud in the case, charging, with the other one in the ear
        // is the bud swap itself: the report says one bud in the case and one
        // out, and that must not read as "put away".
        let t0 = Instant::now();
        let mut s = yielded_to_the_mac(t0);
        // Both buds in the case first, then one taken back out: the state
        // must follow the newest report, not stay at "in case".
        assert!(decide(&mut s, Event::Ear { both_in_case: true }, t0 + ms(10)).is_empty());
        assert!(decide(
            &mut s,
            Event::Ear {
                both_in_case: false
            },
            t0 + ms(20)
        )
        .is_empty());
        let dropped = bud_swap(&mut s, t0);
        assert_eq!(
            scheduled(&decide(&mut s, drop_with(Reason::Timeout), dropped)),
            1
        );
    }

    #[test]
    fn a_manual_disconnect_and_a_local_reason_never_rejoin_after_a_bud_swap() {
        // The user's rule: an explicit command, and anything this host ended,
        // never reconnects, whatever the report says.
        let t0 = Instant::now();

        let mut s = yielded_to_the_mac(t0);
        let dropped = bud_swap(&mut s, t0);
        assert!(decide(&mut s, Event::ManualCommand, dropped - ms(1_000)).is_empty());
        assert_eq!(
            decide(&mut s, drop_with(Reason::Timeout), dropped),
            [Action::Skipped("manual_command")]
        );
        assert!(!s.is_pending());

        let mut s = yielded_to_the_mac(t0);
        let dropped = bud_swap(&mut s, t0);
        assert_eq!(
            decide(&mut s, drop_with(Reason::Local), dropped),
            [Action::Skipped("reason_local")]
        );
        assert!(!s.is_pending());
    }

    #[test]
    fn a_timeout_soon_after_a_rejoin_stops_the_loop() {
        // A bud swap that keeps killing the new link must not loop.
        let t0 = Instant::now();
        let mut s = yielded_to_the_mac(t0);
        let dropped = bud_swap(&mut s, t0);
        assert_eq!(
            scheduled(&decide(&mut s, drop_with(Reason::Timeout), dropped)),
            1
        );
        finish(&mut s, dropped);
        let rejoined = dropped + ms(3_700);
        decide(&mut s, Event::LinkSessionStarted, rejoined);
        assert!(decide(
            &mut s,
            Event::ConnectedDevices(vec![up(mac()), down(host())]),
            rejoined + ms(50)
        )
        .is_empty());
        assert_eq!(
            decide(
                &mut s,
                drop_with(Reason::Timeout),
                rejoined + LOOP_WINDOW - ms(1)
            ),
            [Action::EvictionLoop]
        );
        assert!(!s.is_pending());
        // And it stays off until a manual command.
        assert_eq!(
            decide(
                &mut s,
                drop_with(Reason::Timeout),
                rejoined + LOOP_WINDOW + Duration::from_secs(300)
            ),
            [Action::Skipped("eviction_loop")]
        );
    }

    #[test]
    fn no_disconnected_signal_means_no_rejoin() {
        // ECONNRESET and Connected=false alone: nothing to act on, ever.
        let t0 = Instant::now();
        let mut s = linked(t0);
        evicted(&mut s, t0);
        for n in 1..40 {
            assert!(decide(&mut s, Event::Tick, t0 + ms(250 * n)).is_empty());
        }
        assert!(!s.is_pending());
    }

    #[test]
    fn remote_without_a_recent_device_report_does_not_rejoin() {
        const WHY: &str = "no_recent_device_report_with_apple_host";
        let t0 = Instant::now();

        // No 0x002E report at all.
        let mut s = RejoinState::new(true, true, vec![mac()], ouis());
        s.local = Some(host());
        assert_eq!(s.device_report_evidence(t0), None);
        assert_eq!(decide(&mut s, remote(), t0), [Action::Skipped(WHY)]);

        // A report exactly at the window still counts; older does not, while
        // the Mac's link is not up.
        let mut s = linked(t0);
        evicted(&mut s, t0);
        assert_eq!(
            decide(&mut s, remote(), t0 + EVICTION_WINDOW + ms(1)),
            [Action::Skipped("no_apple_host_link_up")]
        );
        let mut s = linked(t0);
        evicted(&mut s, t0);
        assert_eq!(
            decide(&mut s, remote(), t0 + EVICTION_WINDOW),
            [Action::Scheduled {
                host: mac(),
                matched: HostMatch::Relay,
                kind: MatchKind::Joined
            }]
        );

        // The latest report decides: the Mac gone from it means no evidence.
        let mut s = linked(t0);
        evicted(&mut s, t0);
        decide(
            &mut s,
            Event::ConnectedDevices(vec![up(host())]),
            t0 + ms(100),
        );
        assert_eq!(
            decide(&mut s, remote(), t0 + ms(600)),
            [Action::Skipped(WHY)]
        );

        // A new adapter or device forgets the report.
        let mut s = linked(t0);
        evicted(&mut s, t0);
        decide(&mut s, Event::IdentityChanged, t0 + ms(100));
        assert_eq!(
            decide(&mut s, remote(), t0 + ms(600)),
            [Action::Skipped(WHY)]
        );
    }

    #[test]
    fn a_report_without_an_apple_host_does_not_rejoin() {
        const WHY: &str = "no_recent_device_report_with_apple_host";
        let t0 = Instant::now();
        // The Mac listed but neither relaying nor with an Apple OUI.
        let mut s = RejoinState::new(true, true, Vec::new(), no_ouis());
        s.local = Some(host());
        evicted(&mut s, t0);
        assert_eq!(
            s.device_report_evidence(t0 + ms(600)),
            evidence(ms(600), None, false)
        );
        assert_eq!(
            decide(&mut s, remote(), t0 + ms(600)),
            [Action::Skipped(WHY)]
        );
        // This host relaying to itself teaches nothing.
        assert!(decide(&mut s, Event::Relay { sender: host() }, t0).is_empty());
        assert!(s.known().is_empty());

        // Only this host listed, even with an Apple OUI and a relay.
        let apple_local = addr("A4:83:E7:00:11:22");
        let mut s = RejoinState::new(true, true, vec![apple_local], ouis());
        s.local = Some(apple_local);
        decide(&mut s, Event::ConnectedDevices(vec![up(apple_local)]), t0);
        assert_eq!(s.match_host(apple_local), None);
        assert_eq!(
            decide(&mut s, remote(), t0 + ms(600)),
            [Action::Skipped(WHY)]
        );
    }

    #[test]
    fn a_listed_host_with_a_non_apple_oui_does_not_rejoin() {
        const WHY: &str = "no_recent_device_report_with_apple_host";
        let t0 = Instant::now();
        let mut s = RejoinState::new(true, true, Vec::new(), ouis());
        s.local = Some(host());
        decide(
            &mut s,
            Event::ConnectedDevices(vec![up(host()), up(nokia())]),
            t0,
        );
        assert_eq!(
            s.device_report_evidence(t0 + ms(205)),
            evidence(ms(205), None, false)
        );
        assert_eq!(s.last_report_others(), [nokia()]);
        assert_eq!(
            decide(&mut s, remote(), t0 + ms(205)),
            [Action::Skipped(WHY)]
        );
        assert!(!s.is_pending());
    }

    #[test]
    fn first_time_replay_with_no_learned_state_schedules_exactly_one_rejoin() {
        // Fresh pairing: no known-hosts file, a fresh session, the first
        // 0x002E report lists the Mac, the Remote drop 205 ms later, and the
        // device is paired but was never marked trusted.
        let t0 = Instant::now();
        let known = load_known_hosts(Path::new("/nonexistent/aurisd/apple_hosts.json"));
        assert!(known.is_empty());
        let mut s = RejoinState::new(true, true, known, ouis());
        assert!(decide(&mut s, Event::IdentityChanged, t0).is_empty());
        s.local = Some(host());
        let report = t0 + ms(40);
        let dropped = report + ms(205);
        let mut actions = run(
            &mut s,
            vec![
                (
                    Event::ConnectedDevices(vec![up(host()), down(mac())]),
                    report,
                ),
                (Event::Connected(false), report + ms(150)),
            ],
        );
        assert!(actions.is_empty());
        assert_eq!(
            s.device_report_evidence(dropped),
            evidence(ms(205), Some((mac(), HostMatch::Oui)), false)
        );
        let untrusted = Event::Disconnected {
            reason: Reason::Remote,
            facts: Some(LinkFacts {
                powered: true,
                paired: true,
                blocked: false,
            }),
            lid_closed: false,
        };
        actions.extend(decide(&mut s, untrusted, dropped));
        assert_eq!(
            actions,
            [Action::Scheduled {
                host: mac(),
                matched: HostMatch::Oui,
                kind: MatchKind::Joined
            }]
        );
        actions.extend(finish(&mut s, dropped));
        assert_eq!(scheduled(&actions), 1);
        assert!(s.known().is_empty());
    }

    #[test]
    fn oui_txt_parsing_matches_only_apple_inc() {
        let text = "OUI/MA-L                                                    Organization\n\
            company_id                                                  Organization\n\
            \n\
            FC-B2-14   (hex)\t\tApple, Inc.\n\
            FCB214     (base 16)\t\tApple, Inc.\n\
            \t\t\t\tOne Apple Park Way\n\
            \n\
            00-03-93   (hex)\t\t  apple, inc.  \n\
            28-6F-B9   (hex)\t\tNokia Shanghai Bell Co., Ltd.\n\
            00-00-01   (hex)\t\tApple Computer\n\
            00-00-02   (hex)\t\tApplied Materials, Inc.\n\
            00-00-03   (hex)\t\tApple, Inc\n\
            00-00-04   (hex)\t\tApple, Inc. Europe\n\
            FC-B2-1    (hex)\t\tApple, Inc.\n\
            GG-00-05   (hex)\t\tApple, Inc.\n\
            FCB215     (hex)\t\tApple, Inc.\n\
            FC:B2:16   (hex)\t\tApple, Inc.\n\
            +C-B2-17   (hex)\t\tApple, Inc.\n\
            A4-83-E7   (hex)\n\
            58AD12     (base 16)\t\tApple, Inc.\n";
        let set = parse_oui_txt(text);
        assert_eq!(set, HashSet::from([[0xFC, 0xB2, 0x14], [0x00, 0x03, 0x93]]));
    }

    #[test]
    fn hwdb_parsing_matches_only_whole_apple_ouis() {
        let text = "# This file is part of systemd.\n\
            \n\
            OUI:FCB214*\n ID_OUI_FROM_DATABASE=Apple, Inc.\n\
            \n\
            OUI:000393*\nOUI:000502*\n ID_OUI_FROM_DATABASE=Apple, Inc.\n\
            \n\
            OUI:286FB9*\n ID_OUI_FROM_DATABASE=Nokia Shanghai Bell Co., Ltd.\n\
            \n\
            OUI:70B3D5123*\n ID_OUI_FROM_DATABASE=Apple, Inc.\n\
            \n\
            OUI:FCB21*\n ID_OUI_FROM_DATABASE=Apple, Inc.\n\
            OUI:ZZB214*\n ID_OUI_FROM_DATABASE=Apple, Inc.\n\
            OUI:000001*\n ID_OUI_FROM_DATABASE=Apple Computer\n\
            OUI:000002*\n ID_OUI_FROM_DATABASE=Applied Materials, Inc.\n\
            OUI:000003*\n\
            \n ID_OUI_FROM_DATABASE=Apple, Inc.\n\
            OUI:000004*\n ID_OUI_FROM_DATABASE=Nokia\nOUI:000005*\n\
            ID_OUI_FROM_DATABASE=Apple, Inc.\n\
            OUI:3C22FB*  \n  ID_OUI_FROM_DATABASE=APPLE, INC.\r\n";
        let set = parse_oui_hwdb(text);
        assert_eq!(
            set,
            HashSet::from([
                [0xFC, 0xB2, 0x14],
                [0x00, 0x03, 0x93],
                [0x00, 0x05, 0x02],
                [0x3C, 0x22, 0xFB],
            ])
        );
    }

    #[test]
    fn oui_sources_fall_back_in_order() {
        let dir = std::env::temp_dir().join(format!(
            "aurisd-oui-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let hwdata = dir.join("oui.txt");
        let hwdb = dir.join("20-OUI.hwdb");

        let none = AppleOuis::load_from(&hwdata, &hwdb);
        assert_eq!(none.source(), OuiSource::BuiltIn);
        assert_eq!(none.len(), BUILT_IN_APPLE_OUIS.len());
        assert!(none.is_apple(mac()));

        fs::write(&hwdb, "OUI:A483E7*\n ID_OUI_FROM_DATABASE=Apple, Inc.\n").unwrap();
        // An oui.txt without any Apple entry falls through to the hwdb.
        fs::write(&hwdata, b"28-6F-B9   (hex)\t\tNokia\n\xff\xfe\n").unwrap();
        let from_hwdb = AppleOuis::load_from(&hwdata, &hwdb);
        assert_eq!(from_hwdb.source(), OuiSource::Hwdb);
        assert!(from_hwdb.is_apple(addr("A4:83:E7:00:11:22")));
        assert!(!from_hwdb.is_apple(mac()));

        // Invalid UTF-8 elsewhere in the file does not hide valid lines.
        fs::write(&hwdata, b"\xff\nFC-B2-14   (hex)\t\tApple, Inc.\n").unwrap();
        let from_hwdata = AppleOuis::load_from(&hwdata, &hwdb);
        assert_eq!(from_hwdata.source(), OuiSource::Hwdata);
        assert_eq!(from_hwdata.len(), 1);
        assert!(from_hwdata.is_apple(mac()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_installed_registry_agrees_with_the_built_in_list() {
        // Only where hwdata is installed, as on the development machine.
        if !Path::new(HWDATA_OUI_PATH).exists() {
            return;
        }
        let loaded = AppleOuis::load();
        assert_eq!(loaded.source(), OuiSource::Hwdata);
        assert!(loaded.len() > 1000, "{} Apple OUIs", loaded.len());
        for oui in BUILT_IN_APPLE_OUIS {
            assert!(loaded.set.contains(&oui), "{oui:02X?} not Apple in oui.txt");
        }
        assert!(loaded.is_apple(mac()));
        assert!(!loaded.is_apple(nokia()));
        assert!(!loaded.is_apple(host()));
    }

    #[test]
    fn a_locally_administered_address_never_matches() {
        // Even if a registry line claimed such a prefix for Apple.
        let ouis = AppleOuis {
            set: HashSet::from([[0xFE, 0xB2, 0x14], [0xFC, 0xB2, 0x14]]),
            source: OuiSource::BuiltIn,
        };
        assert!(ouis.is_apple(mac()));
        assert!(!ouis.is_apple(addr("FE:B2:14:0A:0B:0C")));
        let s = RejoinState::new(true, true, Vec::new(), Arc::new(ouis));
        assert_eq!(s.match_host(addr("FE:B2:14:0A:0B:0C")), None);
        assert_eq!(s.match_host(mac()), Some(HostMatch::Oui));
    }

    #[test]
    fn buds_in_the_case_or_a_closed_lid_do_not_rejoin() {
        let t0 = Instant::now();
        let mut s = linked(t0);
        decide(&mut s, Event::Ear { both_in_case: true }, t0);
        evicted(&mut s, t0);
        assert_eq!(
            decide(&mut s, remote(), t0 + ms(600)),
            [Action::Skipped("buds_in_case")]
        );

        let mut s = linked(t0);
        decide(&mut s, Event::Ear { both_in_case: true }, t0);
        decide(
            &mut s,
            Event::Ear {
                both_in_case: false,
            },
            t0 + ms(1),
        );
        evicted(&mut s, t0);
        let lid = Event::Disconnected {
            reason: Reason::Remote,
            facts: Some(READY),
            lid_closed: true,
        };
        assert_eq!(
            decide(&mut s, lid, t0 + ms(600)),
            [Action::Skipped("case_lid_closed")]
        );
    }

    #[test]
    fn handoff_disabled_or_the_toggle_off_does_not_rejoin() {
        let t0 = Instant::now();
        let mut s = linked(t0);
        s.handoff_enabled = false;
        evicted(&mut s, t0);
        assert_eq!(
            decide(&mut s, remote(), t0 + ms(600)),
            [Action::Skipped("handoff_disabled")]
        );
        let mut s = linked(t0);
        s.allowed = false;
        evicted(&mut s, t0);
        assert_eq!(
            decide(&mut s, remote(), t0 + ms(600)),
            [Action::Skipped("rejoin_after_eviction_off")]
        );
    }

    #[test]
    fn adapter_and_device_state_must_allow_a_connect() {
        let t0 = Instant::now();
        let cases = [
            (None, "device_state_unreadable"),
            (
                Some(LinkFacts {
                    powered: false,
                    ..READY
                }),
                "adapter_off",
            ),
            (
                Some(LinkFacts {
                    paired: false,
                    ..READY
                }),
                "not_paired",
            ),
            (
                Some(LinkFacts {
                    blocked: true,
                    ..READY
                }),
                "blocked",
            ),
        ];
        for (facts, why) in cases {
            let mut s = linked(t0);
            evicted(&mut s, t0);
            let ev = Event::Disconnected {
                reason: Reason::Remote,
                facts,
                lid_closed: false,
            };
            assert_eq!(decide(&mut s, ev, t0 + ms(600)), [Action::Skipped(why)]);
        }
    }

    #[test]
    fn a_manual_command_before_or_during_the_wait_blocks_the_rejoin() {
        let t0 = Instant::now();
        let mut s = linked(t0);
        schedule(&mut s, t0);
        assert_eq!(
            decide(&mut s, Event::ManualCommand, t0 + ms(1500)),
            [Action::Cancelled("manual_command")]
        );
        assert!(decide(&mut s, Event::Tick, t0 + ms(4000)).is_empty());

        // A connect command 10 s or less before the drop.
        let t1 = t0 + Duration::from_secs(120);
        let mut s = linked(t1);
        decide(&mut s, Event::ManualCommand, t1);
        evicted(&mut s, t1 + Duration::from_secs(9));
        assert_eq!(
            decide(&mut s, remote(), t1 + ms(9600)),
            [Action::Skipped("manual_command")]
        );
    }

    #[test]
    fn connected_by_other_means_or_another_disconnect_cancels_the_wait() {
        let t0 = Instant::now();
        let mut s = linked(t0);
        schedule(&mut s, t0);
        assert_eq!(
            decide(&mut s, Event::Connected(true), t0 + ms(2000)),
            [Action::Cancelled("connected_by_other_means")]
        );
        assert!(decide(&mut s, Event::Tick, t0 + ms(4000)).is_empty());

        let mut s = linked(t0);
        schedule(&mut s, t0);
        assert_eq!(
            decide(&mut s, drop_with(Reason::Local), t0 + ms(2000)),
            [
                Action::Cancelled("disconnected_again"),
                Action::Skipped("reason_local")
            ]
        );

        for (event, why) in [
            (Event::AdapterGone, "adapter_gone"),
            (Event::SetEnabled(false), "handoff_disabled"),
            (Event::IdentityChanged, "identity_changed"),
        ] {
            let mut s = linked(t0);
            schedule(&mut s, t0);
            assert_eq!(
                decide(&mut s, event, t0 + ms(1000)),
                [Action::Cancelled(why)]
            );
            assert!(!s.is_pending());
        }
    }

    #[test]
    fn busy_connects_retry_at_most_three_attempts_in_all() {
        let t0 = Instant::now();
        let mut s = linked(t0);
        schedule(&mut s, t0);
        let mut at = t0 + ms(540) + REJOIN_DELAY;
        let busy = |seq| Event::ConnectFinished {
            seq,
            outcome: Outcome::Busy,
        };
        let seq = fire(&mut s, at);
        assert_eq!(
            decide(&mut s, busy(seq), at + ms(100)),
            [Action::Retry {
                attempt: 2,
                wait: BUSY_RETRY[0]
            }]
        );
        at += ms(100);
        assert!(decide(&mut s, Event::Tick, at + ms(2900)).is_empty());
        at += BUSY_RETRY[0];
        assert_eq!(
            decide(&mut s, Event::Tick, at),
            [Action::Connect { seq, attempt: 2 }]
        );
        assert_eq!(
            decide(&mut s, busy(seq), at),
            [Action::Retry {
                attempt: 3,
                wait: BUSY_RETRY[1]
            }]
        );
        at += BUSY_RETRY[1];
        assert_eq!(
            decide(&mut s, Event::Tick, at),
            [Action::Connect { seq, attempt: 3 }]
        );
        let out = decide(&mut s, busy(seq), at);
        assert!(matches!(out.as_slice(), [Action::GaveUp(e)] if e.contains("busy")));
        assert!(!s.is_pending());
        assert!(decide(&mut s, Event::Tick, at + Duration::from_secs(30)).is_empty());
    }

    #[test]
    fn any_other_connect_error_stops_and_stale_results_are_ignored() {
        let t0 = Instant::now();
        let mut s = linked(t0);
        schedule(&mut s, t0);
        let seq = fire(&mut s, t0 + ms(3540));
        let stale = Event::ConnectFinished {
            seq: seq + 7,
            outcome: Outcome::Connected,
        };
        assert!(decide(&mut s, stale, t0 + ms(3600)).is_empty());
        let failed = Event::ConnectFinished {
            seq,
            outcome: Outcome::Failed("br-connection-refused".into()),
        };
        assert_eq!(
            decide(&mut s, failed, t0 + ms(3700)),
            [Action::GaveUp("br-connection-refused".into())]
        );

        let mut s = linked(t0);
        schedule(&mut s, t0);
        let seq = fire(&mut s, t0 + ms(3540));
        let not_ready = Event::ConnectFinished {
            seq,
            outcome: Outcome::NotReady("connected_by_other_means"),
        };
        assert_eq!(
            decide(&mut s, not_ready, t0 + ms(3700)),
            [Action::Cancelled("connected_by_other_means")]
        );
    }

    /// A rejoin at `t0`, then a second qualifying eviction 45 s into the
    /// rate-limit window: the 16:58 to 16:59 field sequence. Returns the
    /// state, the rate-limit anchor and the moment of the second eviction.
    fn deferred(t0: Instant) -> (RejoinState, Instant, Instant) {
        let mut s = linked(t0);
        schedule(&mut s, t0);
        let anchor = t0 + ms(540);
        let seq = fire(&mut s, anchor + REJOIN_DELAY);
        let rejoined = anchor + REJOIN_DELAY + ms(300);
        assert_eq!(
            decide(
                &mut s,
                Event::ConnectFinished {
                    seq,
                    outcome: Outcome::Connected
                },
                rejoined
            ),
            [Action::Rejoined]
        );
        decide(&mut s, Event::LinkSessionStarted, rejoined);
        decide(
            &mut s,
            Event::ConnectedDevices(vec![up(mac()), up(host())]),
            rejoined + ms(50),
        );
        (s, anchor, anchor + Duration::from_secs(45))
    }

    /// The expected deferral of an eviction at `at`, anchored at `anchor`.
    fn deferral(anchor: Instant, at: Instant) -> Action {
        Action::Deferred {
            host: mac(),
            matched: HostMatch::Relay,
            kind: MatchKind::Connected,
            delay: RATE_LIMIT - at.duration_since(anchor) + REJOIN_DELAY,
        }
    }

    #[test]
    fn an_eviction_inside_the_rate_limit_window_waits_for_it_and_connects() {
        // A second eviction inside the rate-limit window used to be dropped
        // as rate limited, leaving the AirPods on the other host. It waits
        // instead.
        let t0 = Instant::now();
        let (mut s, anchor, again) = deferred(t0);
        assert_eq!(decide(&mut s, remote(), again), [deferral(anchor, again)]);
        assert!(s.is_pending());
        assert_eq!(s.pending_kind(), Some(MatchKind::Connected));
        // Nothing before the window closes, and not until the settle wait
        // after it either.
        assert!(decide(&mut s, Event::Tick, anchor + RATE_LIMIT).is_empty());
        assert_eq!(s.pending_attempt(), 0);
        let due = anchor + RATE_LIMIT + REJOIN_DELAY;
        let seq = fire(&mut s, due);
        assert_eq!(s.pending_seq(), Some(seq));
        assert_eq!(s.pending_attempt(), 1);
        assert_eq!(
            decide(
                &mut s,
                Event::ConnectFinished {
                    seq,
                    outcome: Outcome::Connected
                },
                due + ms(400)
            ),
            [Action::Rejoined]
        );
        assert!(!s.is_pending());
    }

    #[test]
    fn a_deferred_rejoin_is_cancelled_like_any_other() {
        let t0 = Instant::now();
        for (event, why) in [
            (Event::ManualCommand, "manual_command"),
            (Event::Ear { both_in_case: true }, "buds_in_case"),
            (Event::AdapterGone, "adapter_gone"),
            (Event::SetEnabled(false), "handoff_disabled"),
        ] {
            let (mut s, anchor, again) = deferred(t0);
            assert_eq!(decide(&mut s, remote(), again), [deferral(anchor, again)]);
            assert_eq!(
                decide(&mut s, event, again + Duration::from_secs(1)),
                [Action::Cancelled(why)],
                "{why}"
            );
            assert!(!s.is_pending(), "{why}");
            // The wait passes and nothing pages.
            assert!(decide(
                &mut s,
                Event::Tick,
                anchor + RATE_LIMIT + REJOIN_DELAY + Duration::from_secs(5)
            )
            .is_empty());
        }
    }

    #[test]
    fn a_ping_pong_eviction_still_stops_rejoining_instead_of_deferring() {
        // Inside both windows the loop rule wins: a second eviction within
        // 20 s of a rejoin is the other host taking them straight back, and
        // waiting a minute to page again would only continue the fight.
        let t0 = Instant::now();
        let mut s = linked(t0);
        schedule(&mut s, t0);
        let anchor = t0 + ms(540);
        let seq = fire(&mut s, anchor + REJOIN_DELAY);
        let rejoined = anchor + REJOIN_DELAY + ms(300);
        decide(
            &mut s,
            Event::ConnectFinished {
                seq,
                outcome: Outcome::Connected,
            },
            rejoined,
        );
        decide(&mut s, Event::LinkSessionStarted, rejoined);
        decide(
            &mut s,
            Event::ConnectedDevices(vec![up(mac()), up(host())]),
            rejoined + ms(50),
        );
        assert_eq!(
            decide(&mut s, remote(), rejoined + LOOP_WINDOW - ms(1)),
            [Action::EvictionLoop]
        );
        assert!(!s.is_pending());
        assert!(s.looped);
        assert_eq!(
            decide(&mut s, remote(), rejoined + Duration::from_secs(300)),
            [Action::Skipped("eviction_loop")]
        );
    }

    #[test]
    fn one_sequence_per_minute_and_an_eviction_loop_stops_rejoining() {
        let t0 = Instant::now();
        let mut s = linked(t0);
        schedule(&mut s, t0);
        // schedule() anchored the rate-limit window here.
        let anchor = t0 + ms(540);
        decide(&mut s, Event::Connected(true), t0 + ms(1000));
        // A second qualifying eviction 30 s later is inside the minute. It
        // is not dropped: the connect waits for the window to expire.
        let t1 = t0 + Duration::from_secs(30);
        decide(&mut s, Event::ConnectedDevices(vec![up(host())]), t1);
        evicted(&mut s, t1 + ms(100));
        let second = t1 + ms(600);
        let left = RATE_LIMIT - second.duration_since(anchor);
        assert_eq!(
            decide(&mut s, remote(), second),
            [Action::Deferred {
                host: mac(),
                matched: HostMatch::Relay,
                kind: MatchKind::Joined,
                delay: left + REJOIN_DELAY,
            }]
        );
        assert!(s.is_pending());
        // Nothing fires until the window has passed and the usual settle
        // wait after it.
        assert!(decide(&mut s, Event::Tick, anchor + RATE_LIMIT).is_empty());
        let due = second + left + REJOIN_DELAY;
        let seq = fire(&mut s, due);
        let ok = Event::ConnectFinished {
            seq,
            outcome: Outcome::Connected,
        };
        assert_eq!(decide(&mut s, ok, due + ms(400)), [Action::Rejoined]);
        // Evicted again 15 s after the rejoin: loop, stop.
        let t3 = due + ms(400) + Duration::from_secs(15);
        assert_eq!(decide(&mut s, remote(), t3), [Action::EvictionLoop]);
        let t4 = t3 + Duration::from_secs(120);
        decide(&mut s, Event::ConnectedDevices(vec![up(host())]), t4);
        evicted(&mut s, t4);
        assert_eq!(
            decide(&mut s, remote(), t4 + ms(600)),
            [Action::Skipped("eviction_loop")]
        );
        // A manual connect re-enables it, after its quiet period.
        decide(&mut s, Event::ManualCommand, t4 + Duration::from_secs(1));
        let t5 = t4 + Duration::from_secs(20);
        decide(&mut s, Event::ConnectedDevices(vec![up(host())]), t5);
        schedule(&mut s, t5);
    }

    #[test]
    fn different_airpods_are_not_held_back_by_the_old_eviction_loop() {
        let t0 = Instant::now();
        let mut s = linked(t0);
        schedule(&mut s, t0);
        let seq = fire(&mut s, t0 + ms(3540));
        let ok = Event::ConnectFinished {
            seq,
            outcome: Outcome::Connected,
        };
        decide(&mut s, ok, t0 + ms(4000));
        assert_eq!(
            decide(&mut s, remote(), t0 + Duration::from_secs(10)),
            [Action::EvictionLoop]
        );
        // New AirPods address, first report, first eviction after the minute.
        decide(&mut s, Event::IdentityChanged, t0 + Duration::from_secs(30));
        let t1 = t0 + Duration::from_secs(61);
        decide(&mut s, Event::ConnectedDevices(vec![up(host())]), t1);
        schedule(&mut s, t1);
    }

    #[test]
    fn an_eviction_more_than_twenty_seconds_after_a_rejoin_is_not_a_loop() {
        let t0 = Instant::now();
        let mut s = linked(t0);
        schedule(&mut s, t0);
        let seq = fire(&mut s, t0 + ms(3540));
        let ok = Event::ConnectFinished {
            seq,
            outcome: Outcome::Connected,
        };
        decide(&mut s, ok, t0 + ms(4000));
        // The Mac is still on the AirPods when they drop this host again.
        decide(&mut s, Event::LinkSessionStarted, t0 + ms(4100));
        decide(
            &mut s,
            Event::ConnectedDevices(vec![up(mac()), up(host())]),
            t0 + ms(4200),
        );
        let later = t0 + Duration::from_secs(25);
        assert_eq!(
            decide(&mut s, drop_with(Reason::Remote), later),
            [Action::Deferred {
                host: mac(),
                matched: HostMatch::Relay,
                kind: MatchKind::Connected,
                delay: RATE_LIMIT - later.duration_since(t0 + ms(540)) + REJOIN_DELAY,
            }]
        );
        assert!(!s.looped);
    }

    #[test]
    fn known_hosts_round_trip_and_cap() {
        let dir = std::env::temp_dir().join(format!(
            "aurisd-rejoin-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join("apple_hosts.json");
        assert!(load_known_hosts(&path).is_empty());

        let hosts: Vec<Address> = (0..10u8)
            .map(|i| Address::new([0xFC, 0xB2, 0x14, 0x0A, 0x0B, i]))
            .collect();
        let mut s = RejoinState::new(true, true, load_known_hosts(&path), ouis());
        s.local = Some(host());
        let mut last = Vec::new();
        for h in &hosts {
            match decide(&mut s, Event::Relay { sender: *h }, Instant::now()).as_slice() {
                [Action::Persist(list)] => last = list.clone(),
                other => panic!("expected persist, got {other:?}"),
            }
        }
        assert_eq!(last.len(), KNOWN_HOSTS_CAP);
        assert_eq!(last, hosts[2..]);
        save_known_hosts(&path, &last).unwrap();
        assert_eq!(load_known_hosts(&path), hosts[2..]);

        // An oversized or partly invalid file loads the newest valid entries.
        save_known_hosts(&path, &hosts).unwrap();
        assert_eq!(load_known_hosts(&path), hosts[2..]);
        fs::write(
            &path,
            r#"{"apple_hosts":["nope","FC:B2:14:0A:0B:0C","FC:B2:14:0A:0B:0C"]}"#,
        )
        .unwrap();
        assert_eq!(load_known_hosts(&path), [mac()]);
        fs::write(&path, "not json").unwrap();
        assert!(load_known_hosts(&path).is_empty());
        assert_eq!(
            RejoinState::new(true, true, hosts.clone(), ouis()).known(),
            &hosts[2..]
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_pending_view_reports_the_sequence_kind_and_connects_started() {
        let t0 = Instant::now();
        let mut s = linked(t0);
        assert_eq!(s.pending_seq(), None);
        assert_eq!(s.pending_attempt(), 0);
        assert_eq!(s.pending_kind(), None);

        schedule(&mut s, t0 + ms(1000));
        let seq = s.pending_seq().expect("a pending sequence");
        assert_eq!(s.pending_kind(), Some(MatchKind::Joined));
        assert_eq!(s.pending_attempt(), 0, "scheduled, not connecting");

        let fired = fire(&mut s, t0 + ms(1000) + REJOIN_DELAY + ms(600));
        assert_eq!(fired, seq);
        assert_eq!(s.pending_attempt(), 1);

        // A busy refusal keeps the sequence and counts the connect it made.
        let busy = Event::ConnectFinished {
            seq,
            outcome: Outcome::Busy,
        };
        assert!(matches!(
            decide(&mut s, busy, t0 + ms(5000)).as_slice(),
            [Action::Retry { attempt: 2, .. }]
        ));
        assert_eq!(s.pending_attempt(), 1, "the second connect has not started");
        assert_eq!(s.pending_seq(), Some(seq), "still the same sequence");

        fire(&mut s, t0 + ms(9000));
        assert_eq!(s.pending_attempt(), 2);

        // Giving up ends the sequence, and with it the reconnecting view.
        let failed = Event::ConnectFinished {
            seq,
            outcome: Outcome::Failed("br-connection-refused".to_owned()),
        };
        assert!(matches!(
            decide(&mut s, failed, t0 + ms(9500)).as_slice(),
            [Action::GaveUp(_)]
        ));
        assert_eq!(s.pending_seq(), None);
        assert_eq!(s.pending_attempt(), 0);
        assert_eq!(s.pending_kind(), None);
    }

    #[test]
    fn a_timeout_drop_keeps_its_link_lost_kind_on_the_pending_sequence() {
        let t0 = Instant::now();
        let mut s = linked(t0);
        assert!(decide(
            &mut s,
            Event::ConnectedDevices(vec![up(host()), up(mac())]),
            t0 + ms(500)
        )
        .is_empty());
        assert!(decide(&mut s, Event::Connected(false), t0 + ms(30_000)).is_empty());
        assert_eq!(
            decide(&mut s, drop_with(Reason::Timeout), t0 + ms(30_100)),
            [Action::Scheduled {
                host: mac(),
                matched: HostMatch::Relay,
                kind: MatchKind::LinkLost
            }]
        );
        assert_eq!(s.pending_kind(), Some(MatchKind::LinkLost));
    }
}
