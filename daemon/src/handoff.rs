//! Apple multi-host switching: the ownership state machine.
//!
//! [`decide`] is pure. It takes the state, one event and a clock reading and
//! returns the side effects; `aap::session` performs them. Behaviour follows
//! LibrePods Android (`AirPodsService.takeOver`, its ownership-loss callbacks)
//! with one Linux-specific addition, the yielded hold: browsers resume on
//! their own when the audio route changes, and a resume must not be mistaken
//! for the user asking for the AirPods back. Reports are always folded into
//! the state so the UI can show where audio is; automatic actions
//! additionally need `[handoff] enabled`.

use std::time::{Duration, Instant};

use bluer::Address;

use crate::{
    aap::codec::{AudioSourceState, PLAYING_APP_UNKNOWN},
    audio_route::RouteMode,
    state::{
        AudioSourceStatus, Handoff, HandoffAudioSource, HandoffDevice, HandoffEvent,
        HandoffEventKind, HandoffOwner,
    },
};

/// A local Paused -> Playing edge must stay Playing this long before it counts
/// as the user asking for the AirPods.
pub const PLAY_DEBOUNCE: Duration = Duration::from_millis(1500);
/// While yielded, a Playing edge this soon after our own pause (or the peer's
/// latest ownership request) is a player resuming itself. A player's
/// self-resume lands well within a second of the pause, while a user press
/// asking to take over can come several seconds later; the window is set
/// between the two so a genuine user press is not held back.
pub const PAUSE_HOLD: Duration = Duration::from_millis(2500);
/// While yielded, a Playing edge this soon after a BlueZ connection, profile
/// or transport change, or an audio-source report naming this host, is a
/// player resuming because its sink changed. The PipeWire profile now
/// follows ownership, so every yield fires `AudioLinkChanged` and arms this
/// hold; without it, a profile switch that happens right after a user press
/// can re-pause the very press it should have honoured, needing a second
/// press to take over. A self-resume after a sink change lands within a
/// second of it, so the window is set below the gap seen for a genuine user
/// press.
pub const AUDIO_CHANGE_HOLD: Duration = Duration::from_millis(2500);
/// Reports for this long after an AAP link opens are the AirPods' opening
/// state dump, even when the settings dump has already ended.
pub const OPENING_WINDOW: Duration = Duration::from_secs(3);
/// The opening window never lasts longer than this, settings dump or not.
pub const OPENING_WINDOW_MAX: Duration = Duration::from_secs(10);
/// A yielded hold survives an idle or local audio-source report only this
/// soon after the latest real yield request.
pub const REQUEST_RECENT: Duration = Duration::from_secs(10);
/// After a take-over with a local player playing, the AirPods' A2DP transport
/// must be pending or active this soon, or the take-over is reported as
/// silent. Nothing is retried: the card profile switch is what starts the
/// stream now.
pub const STREAM_CHECK: Duration = Duration::from_secs(3);
/// After a take-over, `HostStreamingState NO` is held back this long while
/// the stream starts: a player stalled on a silent sink is not a stop.
pub const STREAM_GRACE: Duration = Duration::from_secs(8);
/// Local playback must stay stopped this long before `HostStreamingState NO`
/// is sent; a play within it still counts as wanting the stream.
pub const PLAYBACK_STOPPED: Duration = Duration::from_secs(3);

/// Something the session observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// An AAP socket was connected; the opening sequence starts.
    LinkOpened,
    /// The link's opening settings dump ended (first battery packet or the
    /// readback window closing).
    OpeningSettled,
    /// The AAP socket, or the attempt to open one, ended.
    LinkClosed,
    /// Opcode 0x000E.
    AudioSource {
        /// Host the AirPods route for.
        address: Address,
        /// What that host is doing.
        state: AudioSourceState,
    },
    /// Opcode 0x002E, addresses in wire order.
    ConnectedDevices(Vec<Address>),
    /// Control 0x06 from the AirPods.
    OwnsConnection {
        /// 01 own, 00 not.
        owns: bool,
        /// Part of the opening settings dump: state, not a handoff.
        opening: bool,
    },
    /// Opcode 0x0011 relayed from another host.
    SmartRouting {
        /// Sending host.
        sender: Address,
        /// Body carries `audioRoutingSetOwnershipToFalse`.
        requests_yield: bool,
    },
    /// A local MPRIS reading.
    Playback {
        /// Any player is Playing.
        playing: bool,
        /// Identity of the playing player.
        app: Option<String>,
        /// MPRIS bus name of the playing player.
        player: Option<String>,
    },
    /// BlueZ reported a connection, audio profile or media transport change
    /// for the AirPods.
    AudioLinkChanged,
    /// The AirPods' A2DP `MediaTransport1.State` is `pending` or `active`
    /// (`true`), or idle or absent (`false`).
    A2dpTransport {
        /// Audio is flowing or about to.
        active: bool,
    },
    /// Periodic tick; fires a debounced take-over.
    Tick,
    /// `{"cmd":"take_over"}`.
    TakeOver,
    /// `{"cmd":"yield"}`.
    Yield,
}

/// A side effect for the session to perform, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Control 0x06 with 01 or 00.
    SendOwnership(bool),
    /// 0x0010 media information (capitalised keys, category 301).
    SendMediaInfo {
        /// Host to relay to.
        target: Address,
        /// `HostStreamingState` YES or NO.
        streaming: bool,
        /// `PlayingApp`.
        app: String,
    },
    /// 0x0010 media information for a host that just joined.
    SendNewDeviceMediaInfo {
        /// Host to relay to.
        target: Address,
    },
    /// 0x0010 Hijackv2.
    SendHijack {
        /// Host to relay to.
        target: Address,
    },
    /// 0x0010 newTipi.
    SendNewTipi {
        /// Host to relay to.
        target: Address,
    },
    /// Pause every local MPRIS player that is Playing.
    PauseLocalPlayers,
    /// A local player resumed on its own while yielded: pause it again.
    RepausePlayer {
        /// MPRIS bus name of the player; `None` pauses every Playing player.
        player: Option<String>,
    },
    /// BlueZ Device1.ConnectProfile for A2DP sink.
    ClaimAudio,
    /// Point the local PipeWire card at the AirPods or away from them. Sent
    /// before the ownership write on a take-over, so the sink node exists
    /// again before the player streams, and after the local pause on a yield,
    /// so the transport is released from this side.
    SetAudioRoute(RouteMode),
    /// The A2DP stream stayed idle for [`STREAM_CHECK`] after a take-over:
    /// log a warning naming the transport state. Said once per take-over.
    StreamNotStarted,
    /// Publish `last_event`; the session supplies the timestamp.
    Record {
        /// What happened.
        kind: HandoffEventKind,
        /// Other host involved.
        peer: Option<Address>,
    },
}

/// Everything [`decide`] needs to remember.
#[derive(Debug, Clone)]
pub struct HandoffState {
    /// `[handoff] enabled`.
    pub enabled: bool,
    /// `[handoff] take_over_on_play`.
    pub take_over_on_play: bool,
    /// Local adapter Modalias names Apple.
    pub apple_host_id: Option<bool>,
    /// Local adapter address.
    pub local: Option<Address>,
    last_event: Option<HandoffEvent>,
    link_up: bool,
    owner: HandoffOwner,
    audio_source: Option<(Address, AudioSourceStatus)>,
    devices: Vec<Address>,
    /// This host gave the audio up and has not taken it back on this link.
    released: bool,
    /// Yielded hold: local Playing edges are checked for auto-resume.
    /// Ends on take-over, a confirmed user play, or a link reset.
    held: bool,
    /// Our latest pause, or the peer's latest ownership request, while held.
    hold_from: Option<Instant>,
    /// When the current AAP link opened; `None` without a link.
    opened_at: Option<Instant>,
    /// The current link's opening settings dump has ended.
    opening_settled: bool,
    /// Latest real yield request: relayed ownership-to-false, ownership `00`
    /// outside the opening window, or a live audio-source change to another
    /// host at media or call.
    last_request: Option<Instant>,
    /// Players paused again since `last_request`: one automatic re-pause each.
    repaused: Vec<Option<String>>,
    /// Latest BlueZ audio link change or audio-source report naming this host.
    audio_changed: Option<Instant>,
    /// `None` until the first reading since start or link open.
    playing: Option<bool>,
    playing_app: Option<String>,
    playing_player: Option<String>,
    play_edge: Option<Instant>,
    /// Latest time a local player was seen Playing.
    last_play: Option<Instant>,
    /// Local playback stopped at this time while `HostStreamingState NO` is
    /// still to be decided.
    stopped_at: Option<Instant>,
    /// The A2DP transport is pending or active; `None` until BlueZ reports.
    transport_active: Option<bool>,
    /// Latest take-over, for [`STREAM_GRACE`].
    took_over_at: Option<Instant>,
    /// Warn if the stream has not started by then; cleared once it has.
    stream_check: Option<Instant>,
    /// The take-over said streaming before the transport was active: say it
    /// once more when it is.
    reannounce: bool,
    /// Latest `HostStreamingState` sent on this link.
    announced: Option<bool>,
}

impl HandoffState {
    /// Fresh state: nothing reported, no link.
    pub fn new(enabled: bool, take_over_on_play: bool) -> Self {
        Self {
            enabled,
            take_over_on_play,
            apple_host_id: None,
            local: None,
            last_event: None,
            link_up: false,
            owner: HandoffOwner::Unknown,
            audio_source: None,
            devices: Vec::new(),
            released: false,
            held: false,
            hold_from: None,
            opened_at: None,
            opening_settled: false,
            last_request: None,
            repaused: Vec::new(),
            audio_changed: None,
            playing: None,
            playing_app: None,
            playing_player: None,
            play_edge: None,
            last_play: None,
            stopped_at: None,
            transport_active: None,
            took_over_at: None,
            stream_check: None,
            reannounce: false,
            announced: None,
        }
    }

    /// The AirPods last said this host owns them and Auris has not yielded.
    /// A profile claim in flight stops retrying once this is false.
    pub fn owns_audio(&self) -> bool {
        self.owner == HandoffOwner::Local && !self.released
    }

    fn is_local(&self, address: Address) -> bool {
        self.local == Some(address)
    }

    fn others(&self) -> Vec<Address> {
        self.devices
            .iter()
            .copied()
            .filter(|a| !self.is_local(*a))
            .collect()
    }

    fn active_other_source(&self) -> Option<Address> {
        match self.audio_source {
            Some((a, AudioSourceStatus::Call | AudioSourceStatus::Media)) if !self.is_local(a) => {
                Some(a)
            }
            _ => None,
        }
    }

    fn other_in_call(&self) -> bool {
        matches!(self.audio_source, Some((a, AudioSourceStatus::Call)) if !self.is_local(a))
    }

    /// Another host holds the audio, or this host gave it up.
    fn owned_elsewhere(&self) -> bool {
        self.released || self.owner == HandoffOwner::Other || self.active_other_source().is_some()
    }

    /// A Playing edge now would be a player resuming itself, not the user.
    fn in_hold_window(&self, now: Instant) -> bool {
        let within = |t: Option<Instant>, window: Duration| {
            t.is_some_and(|t| now.saturating_duration_since(t) < window)
        };
        self.held
            && (within(self.hold_from, PAUSE_HOLD) || within(self.audio_changed, AUDIO_CHANGE_HOLD))
    }

    /// Reports now belong to the link's opening state dump: the later of
    /// [`OPENING_WINDOW`] and the settings dump ending, capped at
    /// [`OPENING_WINDOW_MAX`].
    fn in_opening(&self, now: Instant) -> bool {
        self.opened_at.is_some_and(|opened| {
            let age = now.saturating_duration_since(opened);
            age < OPENING_WINDOW_MAX && (age < OPENING_WINDOW || !self.opening_settled)
        })
    }

    fn recent_request(&self, now: Instant) -> bool {
        self.last_request
            .is_some_and(|t| now.saturating_duration_since(t) < REQUEST_RECENT)
    }

    /// A real yield request: restarts every player's re-pause allowance.
    fn note_request(&mut self, now: Instant) {
        self.last_request = Some(now);
        self.repaused.clear();
    }

    /// Already gave the audio up: a new request renews the hold.
    fn renew_hold(&mut self, now: Instant) {
        self.held = true;
        self.hold_from = Some(now);
        self.play_edge = None;
    }

    /// The playing player has not been paused again since the latest request.
    fn may_repause(&self) -> bool {
        !self.repaused.contains(&self.playing_player)
    }

    fn repause(&mut self, now: Instant) -> Vec<Action> {
        self.hold_from = Some(now);
        self.play_edge = None;
        self.repaused.push(self.playing_player.clone());
        vec![Action::RepausePlayer {
            player: self.playing_player.clone(),
        }]
    }

    fn app(&self) -> String {
        self.playing_app
            .clone()
            .unwrap_or_else(|| PLAYING_APP_UNKNOWN.to_owned())
    }

    /// Store the event behind an [`Action::Record`].
    pub fn record(&mut self, kind: HandoffEventKind, peer: Option<Address>, at: String) {
        self.last_event = Some(HandoffEvent {
            kind,
            at,
            peer: peer.map(|p| p.to_string()),
        });
    }

    /// The published state.json object.
    pub fn view(&self) -> Handoff {
        Handoff {
            enabled: self.enabled,
            take_over_on_play: self.take_over_on_play,
            apple_host_id: self.apple_host_id,
            owner: self.owner,
            audio_source: self
                .audio_source
                .map(|(address, state)| HandoffAudioSource {
                    address: address.to_string(),
                    is_local: self.is_local(address),
                    state,
                }),
            devices: self
                .devices
                .iter()
                .map(|a| HandoffDevice {
                    address: a.to_string(),
                    is_local: self.is_local(*a),
                })
                .collect(),
            last_event: self.last_event.clone(),
        }
    }

    fn reset_link(&mut self) {
        self.owner = HandoffOwner::Unknown;
        self.audio_source = None;
        self.devices.clear();
        self.released = false;
        self.held = false;
        self.hold_from = None;
        self.opened_at = None;
        self.opening_settled = false;
        self.last_request = None;
        self.repaused.clear();
        self.playing = None;
        self.play_edge = None;
        self.end_stream_start();
        self.announced = None;
    }

    /// Forget the take-over's stream start: checks, grace and re-announcement.
    fn end_stream_start(&mut self) {
        self.stopped_at = None;
        self.took_over_at = None;
        self.stream_check = None;
        self.reannounce = false;
    }

    /// `HostStreamingState` to every other listed host.
    fn announce(&mut self, streaming: bool) -> Vec<Action> {
        self.announced = Some(streaming);
        let app = self.app();
        self.others()
            .into_iter()
            .map(|target| Action::SendMediaInfo {
                target,
                streaming,
                app: app.clone(),
            })
            .collect()
    }

    fn in_stream_grace(&self, now: Instant) -> bool {
        self.took_over_at
            .is_some_and(|t| now.saturating_duration_since(t) < STREAM_GRACE)
    }

    /// A local player is playing, or stopped too recently to count as stopped.
    fn wants_stream(&self, now: Instant) -> bool {
        self.playing == Some(true)
            || self.play_edge.is_some()
            || self
                .last_play
                .is_some_and(|t| now.saturating_duration_since(t) < PLAYBACK_STOPPED)
    }

    /// Report a take-over whose A2DP stream never started. The take-over has
    /// already put the card back on its A2DP profile, which is what makes the
    /// audio server build a fresh node; if the transport is still idle three
    /// seconds later something outside this daemon is wrong, and saying so
    /// once is all that is useful.
    fn stream_tick(&mut self, now: Instant) -> Vec<Action> {
        let Some(due) = self.stream_check else {
            return Vec::new();
        };
        if now < due {
            return Vec::new();
        }
        self.stream_check = None;
        if !self.link_up
            || !self.owns_audio()
            || self.transport_active != Some(false)
            || !self.wants_stream(now)
        {
            return Vec::new();
        }
        vec![Action::StreamNotStarted]
    }

    /// `HostStreamingState NO` once local playback has stayed stopped for
    /// [`PLAYBACK_STOPPED`], never inside the take-over's [`STREAM_GRACE`].
    fn stop_tick(&mut self, now: Instant) -> Vec<Action> {
        let Some(stopped) = self.stopped_at else {
            return Vec::new();
        };
        if self.playing != Some(false) {
            self.stopped_at = None;
            return Vec::new();
        }
        if now.saturating_duration_since(stopped) < PLAYBACK_STOPPED || self.in_stream_grace(now) {
            return Vec::new();
        }
        self.stopped_at = None;
        if self.enabled && self.link_up && self.owns_audio() && self.announced != Some(false) {
            self.announce(false)
        } else {
            Vec::new()
        }
    }

    /// Give the audio up. Profiles stay connected: see
    /// docs/BATTERY_AND_HANDOFF.md.
    fn yield_steps(
        &mut self,
        now: Instant,
        announce: bool,
        kind: HandoffEventKind,
        peer: Option<Address>,
    ) -> Vec<Action> {
        self.owner = HandoffOwner::Other;
        self.released = true;
        self.note_request(now);
        self.renew_hold(now);
        self.end_stream_start();
        // Pause first, then take the card off the AirPods (which releases the
        // transport from this side), and only then say the audio is theirs.
        let mut actions = vec![
            Action::PauseLocalPlayers,
            Action::SetAudioRoute(RouteMode::Yielded),
        ];
        if announce {
            actions.push(Action::SendOwnership(false));
        }
        actions.push(Action::Record { kind, peer });
        actions
    }

    /// Take the audio. The card profile goes back to A2DP before any AAP
    /// write, so the sink node is there before the player starts streaming.
    /// When BlueZ reports the A2DP transport, a check follows that audio
    /// actually flows.
    fn take_over_steps(&mut self, now: Instant) -> Vec<Action> {
        let others = self.others();
        let peer = others
            .first()
            .copied()
            .or_else(|| self.active_other_source());
        self.owner = HandoffOwner::Local;
        self.released = false;
        self.held = false;
        self.play_edge = None;
        self.stopped_at = None;
        self.took_over_at = Some(now);
        self.stream_check = self.transport_active.map(|_| now + STREAM_CHECK);
        self.reannounce = self.transport_active == Some(false);
        self.announced = Some(true);
        let app = self.app();
        let mut actions = vec![
            Action::SetAudioRoute(RouteMode::Own),
            Action::SendOwnership(true),
        ];
        actions.extend(others.iter().map(|&target| Action::SendMediaInfo {
            target,
            streaming: true,
            app: app.clone(),
        }));
        actions.extend(others.iter().map(|&target| Action::SendHijack { target }));
        actions.push(Action::ClaimAudio);
        actions.push(Action::Record {
            kind: HandoffEventKind::TookOver,
            peer,
        });
        actions
    }
}

/// Fold one event into the state and return what to do about it.
pub fn decide(s: &mut HandoffState, event: Event, now: Instant) -> Vec<Action> {
    match event {
        Event::LinkOpened => {
            s.reset_link();
            s.link_up = true;
            s.opened_at = Some(now);
            Vec::new()
        }
        Event::OpeningSettled => {
            s.opening_settled = true;
            Vec::new()
        }
        Event::LinkClosed => {
            s.reset_link();
            s.link_up = false;
            Vec::new()
        }
        Event::AudioSource { address, state } => {
            let status = match state {
                AudioSourceState::Idle => AudioSourceStatus::Idle,
                AudioSourceState::Call => AudioSourceStatus::Call,
                AudioSourceState::Media => AudioSourceStatus::Media,
                AudioSourceState::Other(_) => return Vec::new(),
            };
            let previous = s.audio_source.replace((address, status));
            if s.local.is_none() {
                return Vec::new();
            }
            // The opening dump is state: it may describe a host that stopped
            // playing long ago.
            let opening = s.in_opening(now);
            if status == AudioSourceStatus::Idle || s.is_local(address) {
                if s.is_local(address) && status != AudioSourceStatus::Idle {
                    s.owner = HandoffOwner::Local;
                    s.audio_changed = Some(now);
                }
                if !s.held || opening {
                    return Vec::new();
                }
                // Nobody has asked for the AirPods lately: the hold is over.
                if !s.recent_request(now) {
                    s.held = false;
                    return Vec::new();
                }
                // Our stream leaked onto the AirPods while yielded. A pending
                // play edge is a user play whose audio just arrived: keep it.
                if s.is_local(address)
                    && status == AudioSourceStatus::Media
                    && s.play_edge.is_none()
                    && s.may_repause()
                {
                    return s.repause(now);
                }
                return Vec::new();
            }
            s.owner = HandoffOwner::Other;
            if opening || previous == Some((address, status)) {
                return Vec::new();
            }
            if !s.enabled {
                return Vec::new();
            }
            if s.released {
                s.note_request(now);
                s.renew_hold(now);
                return Vec::new();
            }
            s.yield_steps(now, true, HandoffEventKind::Yielded, Some(address))
        }
        Event::ConnectedDevices(list) => {
            let joined: Vec<Address> = list
                .iter()
                .copied()
                .filter(|a| !s.devices.contains(a) && !s.is_local(*a))
                .collect();
            s.devices = list;
            if s.in_opening(now)
                || !(s.enabled && s.owner == HandoffOwner::Local && s.local.is_some())
            {
                return Vec::new();
            }
            joined
                .into_iter()
                .flat_map(|target| {
                    [
                        Action::SendNewDeviceMediaInfo { target },
                        Action::SendNewTipi { target },
                    ]
                })
                .collect()
        }
        Event::OwnsConnection { owns: true, .. } => {
            s.owner = HandoffOwner::Local;
            Vec::new()
        }
        Event::OwnsConnection {
            owns: false,
            opening,
        } => {
            s.owner = HandoffOwner::Other;
            if !s.enabled || opening || s.in_opening(now) {
                return Vec::new();
            }
            if s.released {
                s.note_request(now);
                return Vec::new();
            }
            let peer = s.active_other_source();
            s.yield_steps(now, false, HandoffEventKind::Yielded, peer)
        }
        Event::SmartRouting {
            sender,
            requests_yield,
        } => {
            if !requests_yield || s.is_local(sender) {
                return Vec::new();
            }
            s.owner = HandoffOwner::Other;
            if !s.enabled {
                return Vec::new();
            }
            if s.released {
                // Already gave it up. The peer asking again renews the hold,
                // and a local player still Playing is paused again.
                s.note_request(now);
                s.renew_hold(now);
                if s.playing == Some(true) {
                    return s.repause(now);
                }
                return Vec::new();
            }
            s.yield_steps(now, true, HandoffEventKind::YieldRequested, Some(sender))
        }
        Event::Playback {
            playing,
            app,
            player,
        } => {
            let previous = s.playing.replace(playing);
            if app.is_some() {
                s.playing_app = app;
            }
            if player.is_some() {
                s.playing_player = player;
            }
            if playing || previous == Some(true) {
                s.last_play = Some(now);
            }
            match (previous, playing) {
                (Some(false), true) if s.in_hold_window(now) && s.may_repause() => s.repause(now),
                // Outside the windows, or the same player resumed again after
                // its one automatic re-pause: the user's play.
                (Some(false), true) => {
                    s.play_edge = Some(now);
                    s.stopped_at = None;
                    // Hosts were told this one stopped, and audio still flows.
                    if s.enabled
                        && s.link_up
                        && s.owns_audio()
                        && s.transport_active == Some(true)
                        && s.announced == Some(false)
                    {
                        s.announce(true)
                    } else {
                        Vec::new()
                    }
                }
                // Streaming NO waits: see `stop_tick`.
                (Some(true), false) => {
                    s.play_edge = None;
                    s.stopped_at = Some(now);
                    Vec::new()
                }
                // The first reading since start or link open, or no change.
                _ => Vec::new(),
            }
        }
        Event::AudioLinkChanged => {
            s.audio_changed = Some(now);
            Vec::new()
        }
        Event::A2dpTransport { active } => {
            let previous = s.transport_active.replace(active);
            if !active || previous == Some(true) || !s.link_up || !s.owns_audio() {
                return Vec::new();
            }
            s.stream_check = None;
            if s.reannounce {
                // One extra message: the take-over's YES preceded the audio.
                s.reannounce = false;
                return s.announce(true);
            }
            if s.enabled && s.playing == Some(true) && s.announced == Some(false) {
                return s.announce(true);
            }
            Vec::new()
        }
        Event::Tick => {
            let mut actions = s.stream_tick(now);
            actions.extend(s.stop_tick(now));
            let Some(edge) = s.play_edge else {
                return actions;
            };
            if now.saturating_duration_since(edge) < PLAY_DEBOUNCE {
                return actions;
            }
            s.play_edge = None;
            if s.playing != Some(true) {
                return actions;
            }
            // A play that began outside every hold window and lasted the
            // debounce is the user's: stop holding their player paused.
            s.held = false;
            let allowed = s.enabled
                && s.take_over_on_play
                && s.link_up
                && s.local.is_some()
                && !s.other_in_call()
                && s.owned_elsewhere();
            if allowed {
                actions.extend(s.take_over_steps(now));
            }
            actions
        }
        Event::TakeOver => s.take_over_steps(now),
        Event::Yield => s.yield_steps(now, true, HandoffEventKind::Yielded, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aap::codec::smart_routing_requests_yield;

    const SPOTIFY: &str = "org.mpris.MediaPlayer2.spotify";

    fn addr(s: &str) -> Address {
        s.parse().unwrap()
    }

    fn host() -> Address {
        addr("5C:F3:70:0D:0E:0F")
    }

    fn mac() -> Address {
        addr("A4:83:E7:00:11:22")
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Enabled, local adapter known, both hosts listed, and the link opened
    /// a minute before `t0`, so its opening window is over.
    fn linked(t0: Instant) -> HandoffState {
        let opened = t0
            .checked_sub(Duration::from_secs(60))
            .expect("monotonic clock past one minute");
        let mut s = HandoffState::new(true, true);
        s.local = Some(host());
        assert!(decide(&mut s, Event::LinkOpened, opened).is_empty());
        assert!(decide(&mut s, Event::ConnectedDevices(vec![host(), mac()]), opened).is_empty());
        assert!(decide(&mut s, Event::OpeningSettled, opened).is_empty());
        s
    }

    fn play(s: &mut HandoffState, playing: bool, at: Instant) -> Vec<Action> {
        decide(
            s,
            Event::Playback {
                playing,
                app: playing.then(|| "Spotify".to_owned()),
                player: playing.then(|| SPOTIFY.to_owned()),
            },
            at,
        )
    }

    fn source(address: Address, state: AudioSourceState) -> Event {
        Event::AudioSource { address, state }
    }

    fn repause() -> Vec<Action> {
        vec![Action::RepausePlayer {
            player: Some(SPOTIFY.to_owned()),
        }]
    }

    /// This host owns the AirPods and nothing has been yielded.
    fn owning(t0: Instant) -> HandoffState {
        let mut s = linked(t0);
        assert!(decide(
            &mut s,
            Event::OwnsConnection {
                owns: true,
                opening: false,
            },
            t0,
        )
        .is_empty());
        assert!(s.owns_audio());
        s
    }

    /// Ear detection pausing a player looks exactly like any other stop: the
    /// other hosts are told this one stopped streaming, and that is all. It
    /// must not release ownership or start a yield.
    #[test]
    fn a_pause_driven_by_ear_detection_is_not_a_yield() {
        let t0 = Instant::now();
        let mut s = owning(t0);
        assert!(play(&mut s, true, t0).is_empty());
        // The bud leaves the ear; `ear_media` pauses, and the MPRIS reading
        // that follows reaches the handoff machine as a stop.
        assert!(play(&mut s, false, t0 + ms(200)).is_empty());
        let actions = decide(&mut s, Event::Tick, t0 + Duration::from_secs(4));
        assert!(
            actions.iter().all(|a| matches!(
                a,
                Action::SendMediaInfo {
                    streaming: false,
                    ..
                }
            )),
            "{actions:?}"
        );
        assert!(s.owns_audio());
        assert_eq!(s.view().owner, HandoffOwner::Local);
        assert!(s.view().last_event.is_none(), "no handoff happened");
    }

    /// The other half: putting the bud back resumes the player, and that
    /// Playing edge must not be read as the user asking for the AirPods back
    /// when this host already has them. A take-over here would announce,
    /// hijack and re-claim for nothing.
    #[test]
    fn a_resume_driven_by_ear_detection_never_takes_over() {
        let t0 = Instant::now();
        let mut s = owning(t0);
        assert!(play(&mut s, false, t0).is_empty());
        assert!(play(&mut s, true, t0 + ms(10)).is_empty());
        assert!(decide(&mut s, Event::Tick, t0 + PLAY_DEBOUNCE + ms(10)).is_empty());
        assert!(s.owns_audio());
        assert_eq!(s.view().last_event, None);
    }

    fn yield_actions(announce: bool, kind: HandoffEventKind, peer: Option<Address>) -> Vec<Action> {
        let mut v = vec![
            Action::PauseLocalPlayers,
            Action::SetAudioRoute(RouteMode::Yielded),
        ];
        if announce {
            v.push(Action::SendOwnership(false));
        }
        v.push(Action::Record { kind, peer });
        v
    }

    fn take_over_actions() -> Vec<Action> {
        vec![
            Action::SetAudioRoute(RouteMode::Own),
            Action::SendOwnership(true),
            Action::SendMediaInfo {
                target: mac(),
                streaming: true,
                app: "Spotify".to_owned(),
            },
            Action::SendHijack { target: mac() },
            Action::ClaimAudio,
            Action::Record {
                kind: HandoffEventKind::TookOver,
                peer: Some(mac()),
            },
        ]
    }

    #[test]
    fn reports_are_recorded_but_never_acted_on_while_disabled() {
        let t0 = Instant::now();
        let mut s = linked(t0);
        s.enabled = false;
        assert!(decide(&mut s, source(mac(), AudioSourceState::Media), t0).is_empty());
        assert!(decide(
            &mut s,
            Event::OwnsConnection {
                owns: false,
                opening: false
            },
            t0
        )
        .is_empty());
        assert!(decide(
            &mut s,
            Event::SmartRouting {
                sender: mac(),
                requests_yield: true
            },
            t0
        )
        .is_empty());
        let view = s.view();
        assert_eq!(view.owner, HandoffOwner::Other);
        let src = view.audio_source.unwrap();
        assert_eq!(
            (src.address.as_str(), src.is_local, src.state),
            ("A4:83:E7:00:11:22", false, AudioSourceStatus::Media)
        );
        assert!(view.devices[0].is_local && !view.devices[1].is_local);
        assert!(view.last_event.is_none());

        // Nor does a local play edge take over.
        play(&mut s, false, t0);
        play(&mut s, true, t0 + ms(4000));
        assert!(decide(&mut s, Event::Tick, t0 + ms(6000)).is_empty());
    }

    #[test]
    fn another_host_playing_makes_this_host_yield_once() {
        let t0 = Instant::now();
        for state in [AudioSourceState::Media, AudioSourceState::Call] {
            let mut s = linked(t0);
            assert_eq!(
                decide(&mut s, source(mac(), state), t0),
                yield_actions(true, HandoffEventKind::Yielded, Some(mac()))
            );
            assert!(decide(&mut s, source(mac(), state), t0 + ms(100)).is_empty());
            assert_eq!(s.view().owner, HandoffOwner::Other);
            assert!(!s.owns_audio());
        }
    }

    #[test]
    fn local_or_idle_audio_sources_never_yield() {
        let t0 = Instant::now();
        let mut s = linked(t0);
        assert!(decide(&mut s, source(host(), AudioSourceState::Media), t0).is_empty());
        assert_eq!(s.view().owner, HandoffOwner::Local);
        assert!(s.owns_audio());
        assert!(decide(&mut s, source(mac(), AudioSourceState::Idle), t0).is_empty());
        assert_eq!(s.view().owner, HandoffOwner::Local);
        assert!(decide(&mut s, source(mac(), AudioSourceState::Other(9)), t0).is_empty());

        // Without the local adapter address nothing can be called foreign.
        let mut s = HandoffState::new(true, true);
        assert!(decide(&mut s, source(mac(), AudioSourceState::Media), t0).is_empty());
    }

    #[test]
    fn ownership_zero_from_the_airpods_yields_without_echoing_it() {
        let t0 = Instant::now();
        let mut s = linked(t0);
        let ev = Event::OwnsConnection {
            owns: false,
            opening: false,
        };
        assert_eq!(
            decide(&mut s, ev.clone(), t0),
            yield_actions(false, HandoffEventKind::Yielded, None)
        );
        assert!(decide(&mut s, ev, t0).is_empty());
    }

    #[test]
    fn ownership_in_the_opening_dump_is_state_not_a_handoff() {
        let t0 = Instant::now();
        let mut s = linked(t0);
        let ev = Event::OwnsConnection {
            owns: false,
            opening: true,
        };
        assert!(decide(&mut s, ev, t0).is_empty());
        assert_eq!(s.view().owner, HandoffOwner::Other);
        let ev = Event::OwnsConnection {
            owns: true,
            opening: false,
        };
        assert!(decide(&mut s, ev, t0).is_empty());
        assert_eq!(s.view().owner, HandoffOwner::Local);
    }

    #[test]
    fn a_relayed_ownership_request_yields_to_its_sender() {
        let t0 = Instant::now();
        let mut s = linked(t0);
        let asks = Event::SmartRouting {
            sender: mac(),
            requests_yield: true,
        };
        let chatter = Event::SmartRouting {
            sender: mac(),
            requests_yield: false,
        };
        assert!(decide(&mut s, chatter, t0).is_empty());
        assert_eq!(
            decide(&mut s, asks.clone(), t0),
            yield_actions(true, HandoffEventKind::YieldRequested, Some(mac()))
        );
        // A repeat with nothing playing only renews the hold.
        assert!(decide(&mut s, asks.clone(), t0 + ms(2500)).is_empty());
        play(&mut s, false, t0 + ms(2500));
        assert_eq!(play(&mut s, true, t0 + ms(4_000)), repause());
        assert!(decide(&mut s, Event::Tick, t0 + ms(6_000)).is_empty());

        // A repeat while a local player is still Playing pauses it again.
        assert_eq!(decide(&mut s, asks, t0 + ms(12_100)), repause());
    }

    #[test]
    fn a_user_play_after_the_hold_takes_over_after_the_debounce() {
        let t0 = Instant::now();
        let mut s = linked(t0);
        decide(&mut s, source(mac(), AudioSourceState::Media), t0);
        assert!(play(&mut s, false, t0 + ms(3_000)).is_empty());
        assert!(play(&mut s, true, t0 + ms(3_500)).is_empty());
        assert!(decide(&mut s, Event::Tick, t0 + ms(4_999)).is_empty());
        assert_eq!(
            decide(&mut s, Event::Tick, t0 + ms(5_000)),
            take_over_actions()
        );
        assert_eq!(s.view().owner, HandoffOwner::Local);
        assert!(s.owns_audio());
        assert!(decide(&mut s, Event::Tick, t0 + ms(5_500)).is_empty());
    }

    #[test]
    fn the_first_reading_is_never_an_edge() {
        let t0 = Instant::now();
        let mut s = linked(t0);
        decide(&mut s, source(mac(), AudioSourceState::Media), t0);
        assert!(play(&mut s, true, t0 + ms(5000)).is_empty());
        assert!(decide(&mut s, Event::Tick, t0 + ms(7000)).is_empty());

        // A link reopen forgets the reading, so the next one is first again.
        decide(&mut s, Event::LinkOpened, t0 + ms(7000));
        decide(
            &mut s,
            Event::OwnsConnection {
                owns: false,
                opening: true,
            },
            t0,
        );
        assert!(play(&mut s, true, t0 + ms(7000)).is_empty());
        assert!(decide(&mut s, Event::Tick, t0 + ms(9000)).is_empty());
    }

    #[test]
    fn a_resume_within_the_pause_hold_is_paused_again() {
        let t0 = Instant::now();
        let mut s = linked(t0);
        play(&mut s, true, t0);
        decide(&mut s, source(mac(), AudioSourceState::Media), t0);
        play(&mut s, false, t0 + ms(100));
        assert_eq!(play(&mut s, true, t0 + ms(1_200)), repause());
        assert!(decide(&mut s, Event::Tick, t0 + ms(2_000)).is_empty());

        // The same player resuming again before a new request is the user's
        // play, even inside the window our re-pause restarted.
        play(&mut s, false, t0 + ms(2_200));
        assert!(play(&mut s, true, t0 + ms(2_400)).is_empty());
        play(&mut s, false, t0 + ms(2_600));

        // Once well past the hold, the resume takes over after the debounce.
        assert!(play(&mut s, true, t0 + ms(6_000)).is_empty());
        assert!(decide(&mut s, Event::Tick, t0 + ms(7_499)).is_empty());
        assert_eq!(
            decide(&mut s, Event::Tick, t0 + ms(7_500)),
            take_over_actions()
        );
    }

    #[test]
    fn a_press_six_seconds_after_our_pause_takes_over_not_repause() {
        let t0 = Instant::now();
        let mut s = linked(t0);
        play(&mut s, true, t0);
        decide(&mut s, source(mac(), AudioSourceState::Media), t0);
        play(&mut s, false, t0 + ms(100));
        // A real user press six seconds after our own pause, well past the
        // 2.5 s hold and with no audio-route change to extend it.
        assert!(play(&mut s, true, t0 + ms(6_000)).is_empty());
        assert!(decide(&mut s, Event::Tick, t0 + ms(7_499)).is_empty());
        assert_eq!(
            decide(&mut s, Event::Tick, t0 + ms(7_500)),
            take_over_actions()
        );
    }

    #[test]
    fn a_resume_within_two_point_five_seconds_of_an_audio_link_change_is_paused_again() {
        let t0 = Instant::now();
        for (at, blocked) in [(22_499, true), (22_500, false)] {
            let mut s = linked(t0);
            decide(&mut s, source(mac(), AudioSourceState::Media), t0);
            play(&mut s, false, t0);
            assert!(decide(&mut s, Event::AudioLinkChanged, t0 + ms(20_000)).is_empty());
            let actions = play(&mut s, true, t0 + ms(at));
            let tick = decide(&mut s, Event::Tick, t0 + ms(at + 1500));
            if blocked {
                assert_eq!(actions, repause());
                assert!(tick.is_empty());
            } else {
                assert!(actions.is_empty());
                assert_eq!(tick, take_over_actions());
            }
        }

        // Without a yield there is no hold to enforce.
        let mut s = linked(t0);
        play(&mut s, false, t0);
        decide(&mut s, Event::AudioLinkChanged, t0);
        assert!(play(&mut s, true, t0 + ms(100)).is_empty());
    }

    /// The PipeWire profile now follows ownership, so every yield fires
    /// `AudioLinkChanged` at once. A play four seconds after that, well past
    /// the 2.5 s hold, is a genuine user press even when it follows the
    /// profile switch closely: it must take over, not be re-paused.
    #[test]
    fn a_press_four_seconds_after_an_audio_link_change_from_the_yield_takes_over() {
        let t0 = Instant::now();
        let mut s = linked(t0);
        decide(&mut s, source(mac(), AudioSourceState::Media), t0);
        play(&mut s, false, t0);
        assert!(decide(&mut s, Event::AudioLinkChanged, t0 + ms(100)).is_empty());
        assert!(play(&mut s, true, t0 + ms(4_000)).is_empty());
        assert_eq!(
            decide(&mut s, Event::Tick, t0 + ms(5_500)),
            take_over_actions()
        );
    }

    #[test]
    fn this_host_reported_as_media_while_held_pauses_again() {
        let t0 = Instant::now();
        let mut s = linked(t0);
        play(&mut s, true, t0);
        decide(&mut s, source(mac(), AudioSourceState::Media), t0);
        play(&mut s, false, t0 + ms(100));
        assert_eq!(
            decide(
                &mut s,
                source(host(), AudioSourceState::Media),
                t0 + ms(5_000)
            ),
            repause()
        );
        // A local call is not a leaked media stream.
        assert!(decide(
            &mut s,
            source(host(), AudioSourceState::Call),
            t0 + ms(5_100)
        )
        .is_empty());

        // Spotify had its one re-pause, so resuming it again is the user's
        // play. Its audio arriving keeps it, and the play ends the hold.
        assert!(play(&mut s, true, t0 + ms(6_000)).is_empty());
        assert!(decide(
            &mut s,
            source(host(), AudioSourceState::Media),
            t0 + ms(6_800)
        )
        .is_empty());
        assert_eq!(
            decide(&mut s, Event::Tick, t0 + ms(7_500)),
            take_over_actions()
        );
        assert!(decide(
            &mut s,
            source(host(), AudioSourceState::Media),
            t0 + ms(7_600)
        )
        .is_empty());
    }

    #[test]
    fn the_hold_ends_on_an_idle_or_local_source_once_requests_stop() {
        let t0 = Instant::now();
        let idle = source(addr("00:00:00:00:00:00"), AudioSourceState::Idle);
        let local_call = source(host(), AudioSourceState::Call);
        for (report, at, ends) in [
            (idle.clone(), 9_999, false),
            (idle, 10_000, true),
            (local_call.clone(), 9_999, false),
            (local_call, 10_000, true),
        ] {
            let mut s = linked(t0);
            play(&mut s, true, t0);
            decide(&mut s, source(mac(), AudioSourceState::Media), t0);
            play(&mut s, false, t0 + ms(100));
            decide(&mut s, Event::AudioLinkChanged, t0 + ms(9_000));
            assert!(decide(&mut s, report, t0 + ms(at)).is_empty());
            let actions = play(&mut s, true, t0 + ms(10_500));
            let tick = decide(&mut s, Event::Tick, t0 + ms(12_000));
            if ends {
                assert!(actions.is_empty());
                assert_eq!(tick, take_over_actions());
            } else {
                assert_eq!(actions, repause());
                assert!(tick.is_empty());
            }
        }
    }

    #[test]
    fn a_player_is_paused_again_once_per_yield_request() {
        let t0 = Instant::now();
        let yielded = || {
            let mut s = linked(t0);
            play(&mut s, true, t0);
            decide(&mut s, source(mac(), AudioSourceState::Media), t0);
            play(&mut s, false, t0 + ms(100));
            assert_eq!(play(&mut s, true, t0 + ms(500)), repause());
            play(&mut s, false, t0 + ms(520));
            s
        };

        // The user presses play again: a debounced take-over, not a pause.
        let mut s = yielded();
        assert!(play(&mut s, true, t0 + ms(2_500)).is_empty());
        assert!(decide(&mut s, Event::Tick, t0 + ms(3_999)).is_empty());
        assert_eq!(
            decide(&mut s, Event::Tick, t0 + ms(4_000)),
            take_over_actions()
        );

        // A new request restores the allowance.
        let mut s = yielded();
        let asks = Event::SmartRouting {
            sender: mac(),
            requests_yield: true,
        };
        assert!(decide(&mut s, asks, t0 + ms(1_000)).is_empty());
        assert_eq!(play(&mut s, true, t0 + ms(1_500)), repause());
    }

    #[test]
    fn link_open_reports_update_the_snapshot_but_never_yield() {
        let t0 = Instant::now();
        let open = || {
            let mut s = HandoffState::new(true, true);
            s.local = Some(host());
            decide(&mut s, Event::LinkOpened, t0);
            s
        };
        let idle = || source(addr("00:00:00:00:00:00"), AudioSourceState::Idle);
        let not_owner = Event::OwnsConnection {
            owns: false,
            opening: false,
        };

        // The settings dump ended at once; the window still lasts 3 s.
        let mut s = open();
        assert!(decide(&mut s, source(host(), AudioSourceState::Media), t0 + ms(50)).is_empty());
        assert!(decide(
            &mut s,
            Event::ConnectedDevices(vec![host(), mac()]),
            t0 + ms(70)
        )
        .is_empty());
        assert!(decide(&mut s, Event::OpeningSettled, t0 + ms(80)).is_empty());
        assert!(decide(&mut s, source(mac(), AudioSourceState::Media), t0 + ms(90)).is_empty());
        assert!(decide(&mut s, not_owner.clone(), t0 + ms(2_999)).is_empty());
        let view = s.view();
        assert_eq!(view.owner, HandoffOwner::Other);
        let src = view.audio_source.unwrap();
        assert_eq!(
            (src.address.as_str(), src.state),
            ("A4:83:E7:00:11:22", AudioSourceStatus::Media)
        );
        assert_eq!(view.devices.len(), 2);
        assert!(view.last_event.is_none());
        // Afterwards a repeat of the snapshot is not a change, a live one is.
        assert!(decide(
            &mut s,
            source(mac(), AudioSourceState::Media),
            t0 + ms(3_000)
        )
        .is_empty());
        assert!(decide(&mut s, idle(), t0 + ms(4_000)).is_empty());
        assert_eq!(
            decide(
                &mut s,
                source(mac(), AudioSourceState::Media),
                t0 + ms(5_000)
            ),
            yield_actions(true, HandoffEventKind::Yielded, Some(mac()))
        );

        // An unfinished dump keeps the window open past 3 s ...
        let mut s = open();
        assert!(decide(&mut s, not_owner.clone(), t0 + ms(6_000)).is_empty());
        decide(&mut s, Event::OpeningSettled, t0 + ms(6_500));
        assert_eq!(
            decide(&mut s, not_owner, t0 + ms(6_500)),
            yield_actions(false, HandoffEventKind::Yielded, None)
        );

        // ... but never past 10 s.
        let mut s = open();
        assert!(decide(&mut s, idle(), t0 + ms(9_999)).is_empty());
        assert_eq!(
            decide(
                &mut s,
                source(mac(), AudioSourceState::Media),
                t0 + ms(10_000)
            ),
            yield_actions(true, HandoffEventKind::Yielded, Some(mac()))
        );
    }

    #[test]
    fn never_take_over_from_a_host_in_a_call() {
        let t0 = Instant::now();
        let mut s = linked(t0);
        decide(&mut s, source(mac(), AudioSourceState::Call), t0);
        play(&mut s, false, t0);
        assert!(play(&mut s, true, t0 + ms(20_000)).is_empty());
        assert!(decide(&mut s, Event::Tick, t0 + ms(21_500)).is_empty());
        // The user's play still ends the hold: their stream is not paused.
        assert!(decide(
            &mut s,
            source(host(), AudioSourceState::Media),
            t0 + ms(21_600)
        )
        .is_empty());
    }

    #[test]
    fn a_play_that_does_not_last_the_debounce_is_ignored() {
        let t0 = Instant::now();
        let mut s = linked(t0);
        decide(&mut s, source(mac(), AudioSourceState::Media), t0);
        play(&mut s, false, t0 + ms(10_000));
        play(&mut s, true, t0 + ms(10_000));
        play(&mut s, false, t0 + ms(11_000));
        assert!(decide(&mut s, Event::Tick, t0 + ms(11_600)).is_empty());
        // A new edge restarts the debounce from its own time.
        play(&mut s, true, t0 + ms(11_700));
        assert!(decide(&mut s, Event::Tick, t0 + ms(13_100)).is_empty());
        assert_eq!(
            decide(&mut s, Event::Tick, t0 + ms(13_200)),
            take_over_actions()
        );
    }

    #[test]
    fn take_over_needs_the_setting_a_link_and_another_owner() {
        let t0 = Instant::now();
        let edge = |s: &mut HandoffState| {
            play(s, false, t0 + ms(10_000));
            play(s, true, t0 + ms(10_000));
            decide(s, Event::Tick, t0 + ms(12_000))
        };

        let mut s = linked(t0);
        s.take_over_on_play = false;
        decide(&mut s, source(mac(), AudioSourceState::Media), t0);
        assert!(edge(&mut s).is_empty());

        // Nobody else is known to own it.
        let mut s = linked(t0);
        assert!(edge(&mut s).is_empty());

        let mut s = linked(t0);
        decide(&mut s, source(mac(), AudioSourceState::Media), t0);
        decide(&mut s, Event::LinkClosed, t0 + ms(10));
        assert!(edge(&mut s).is_empty());
    }

    fn streaming(on: bool) -> Vec<Action> {
        vec![Action::SendMediaInfo {
            target: mac(),
            streaming: on,
            app: "Spotify".to_owned(),
        }]
    }

    fn idle_transport() -> Event {
        Event::A2dpTransport { active: false }
    }

    fn active_transport() -> Event {
        Event::A2dpTransport { active: true }
    }

    #[test]
    fn stopping_local_playback_while_owning_announces_it_after_a_few_seconds() {
        let t0 = Instant::now();
        let mut s = linked(t0);
        decide(&mut s, source(host(), AudioSourceState::Media), t0);
        play(&mut s, true, t0);
        assert!(play(&mut s, false, t0 + ms(100)).is_empty());
        // A play inside the wait cancels it.
        assert!(play(&mut s, true, t0 + ms(1_000)).is_empty());
        assert!(decide(&mut s, Event::Tick, t0 + ms(3_100)).is_empty());
        assert!(play(&mut s, false, t0 + ms(5_000)).is_empty());
        assert!(decide(&mut s, Event::Tick, t0 + ms(7_999)).is_empty());
        assert_eq!(
            decide(&mut s, Event::Tick, t0 + ms(8_000)),
            streaming(false)
        );
        assert!(decide(&mut s, Event::Tick, t0 + ms(8_250)).is_empty());
        s.enabled = false;
        play(&mut s, true, t0 + ms(9_000));
        play(&mut s, false, t0 + ms(9_100));
        assert!(decide(&mut s, Event::Tick, t0 + ms(13_000)).is_empty());
    }

    /// Yielded to the Mac at `t0`, then a user play takes over at 12.5 s.
    fn taken_over_with_idle_transport(t0: Instant) -> HandoffState {
        let mut s = linked(t0);
        decide(&mut s, idle_transport(), t0);
        decide(&mut s, source(mac(), AudioSourceState::Media), t0);
        play(&mut s, false, t0 + ms(10_500));
        play(&mut s, true, t0 + ms(11_000));
        assert_eq!(
            decide(&mut s, Event::Tick, t0 + ms(12_500)),
            take_over_actions()
        );
        s
    }

    /// The card profile must be back on A2DP before the AirPods are told
    /// this host owns them, and off them only after the local players have
    /// been paused and before ownership is released.
    #[test]
    fn the_audio_route_moves_before_a_take_over_and_after_a_yields_pause() {
        let t0 = Instant::now();
        let mut s = linked(t0);
        play(&mut s, true, t0);
        let took = decide(&mut s, Event::TakeOver, t0 + ms(10));
        assert_eq!(took.first(), Some(&Action::SetAudioRoute(RouteMode::Own)));
        assert_eq!(took.get(1), Some(&Action::SendOwnership(true)));

        let gave = decide(
            &mut s,
            Event::SmartRouting {
                sender: mac(),
                requests_yield: true,
            },
            t0 + ms(1_000),
        );
        assert_eq!(
            gave[..3],
            [
                Action::PauseLocalPlayers,
                Action::SetAudioRoute(RouteMode::Yielded),
                Action::SendOwnership(false),
            ]
        );
    }

    /// When the transport never comes back, the take-over is reported
    /// silent once and never again.
    #[test]
    fn a_take_over_whose_stream_stays_idle_says_so_once() {
        let t0 = Instant::now();
        let mut s = taken_over_with_idle_transport(t0);
        // 1.5 s after the take-over: nothing, the profile switch is still
        // being applied.
        assert!(decide(&mut s, Event::Tick, t0 + ms(14_000)).is_empty());
        assert!(decide(&mut s, Event::Tick, t0 + ms(15_499)).is_empty());
        assert_eq!(
            decide(&mut s, Event::Tick, t0 + ms(15_500)),
            [Action::StreamNotStarted]
        );
        for n in (15_750..40_000).step_by(250) {
            assert!(decide(&mut s, Event::Tick, t0 + ms(n)).is_empty());
        }
    }

    /// The transport going active before the check clears it silently.
    #[test]
    fn a_transport_that_starts_within_the_check_is_never_reported_silent() {
        let t0 = Instant::now();
        let mut s = taken_over_with_idle_transport(t0);
        assert!(decide(&mut s, Event::Tick, t0 + ms(14_000)).is_empty());
        assert_eq!(
            decide(&mut s, active_transport(), t0 + ms(15_000)),
            streaming(true)
        );
        for n in (15_250..40_000).step_by(250) {
            let out = decide(&mut s, Event::Tick, t0 + ms(n));
            assert!(out.is_empty(), "{n}: {out:?}");
        }
    }

    #[test]
    fn an_active_transport_ends_the_check_and_repeats_streaming_once() {
        let t0 = Instant::now();
        let mut s = taken_over_with_idle_transport(t0);
        assert_eq!(
            decide(&mut s, active_transport(), t0 + ms(16_000)),
            streaming(true)
        );
        // A stall inside the grace (until 20.5 s) waits for the grace to end.
        assert!(play(&mut s, false, t0 + ms(16_500)).is_empty());
        assert!(decide(&mut s, Event::Tick, t0 + ms(17_000)).is_empty());
        // Suspend and resume: no second extra message.
        assert!(decide(&mut s, idle_transport(), t0 + ms(18_000)).is_empty());
        assert!(decide(&mut s, active_transport(), t0 + ms(19_000)).is_empty());
        assert!(decide(&mut s, Event::Tick, t0 + ms(20_250)).is_empty());
        assert_eq!(
            decide(&mut s, Event::Tick, t0 + ms(20_500)),
            streaming(false)
        );
        // Playing again while audio still flows says so at once.
        assert_eq!(play(&mut s, true, t0 + ms(24_000)), streaming(true));
        assert!(s.owns_audio());
    }

    #[test]
    fn stopping_again_after_a_play_resumes_sends_no_only_once_the_transport_resumes() {
        let t0 = Instant::now();
        let mut s = taken_over_with_idle_transport(t0);
        decide(&mut s, active_transport(), t0 + ms(13_000));
        decide(
            &mut s,
            source(host(), AudioSourceState::Media),
            t0 + ms(13_100),
        );
        play(&mut s, false, t0 + ms(21_000));
        assert_eq!(
            decide(&mut s, Event::Tick, t0 + ms(24_000)),
            streaming(false)
        );
        // The audio server suspends the idle transport; a play restarts it.
        decide(&mut s, idle_transport(), t0 + ms(26_000));
        assert!(play(&mut s, true, t0 + ms(30_000)).is_empty());
        assert_eq!(
            decide(&mut s, active_transport(), t0 + ms(30_400)),
            streaming(true)
        );
        // No silent-stream warning outside a take-over.
        for n in (30_500..40_000).step_by(250) {
            let out = decide(&mut s, Event::Tick, t0 + ms(n));
            assert!(out.is_empty(), "{n}: {out:?}");
        }
    }

    #[test]
    fn no_silent_stream_warning_without_transport_reports_or_local_playback() {
        let t0 = Instant::now();
        // BlueZ never reported the transport: nothing to judge.
        let mut s = linked(t0);
        play(&mut s, true, t0);
        assert_eq!(
            decide(&mut s, Event::TakeOver, t0 + ms(10)),
            take_over_actions()
        );
        for n in (250..12_000).step_by(250) {
            assert!(decide(&mut s, Event::Tick, t0 + ms(n)).is_empty());
        }
        // Idle transport, but playback stopped well before a manual take-over.
        let mut s = linked(t0);
        decide(&mut s, idle_transport(), t0);
        play(&mut s, true, t0);
        play(&mut s, false, t0 + ms(100));
        decide(&mut s, Event::TakeOver, t0 + ms(5_000));
        for n in (5_250..20_000).step_by(250) {
            let out = decide(&mut s, Event::Tick, t0 + ms(n));
            assert!(!out.contains(&Action::StreamNotStarted), "{n}: {out:?}");
        }
    }

    #[test]
    fn losing_ownership_cancels_the_pending_stream_check() {
        let t0 = Instant::now();
        let mut s = linked(t0);
        decide(&mut s, idle_transport(), t0);
        play(&mut s, true, t0);
        decide(&mut s, Event::TakeOver, t0 + ms(10));
        let asks = Event::SmartRouting {
            sender: mac(),
            requests_yield: true,
        };
        assert_eq!(
            decide(&mut s, asks, t0 + ms(1_000)),
            yield_actions(true, HandoffEventKind::YieldRequested, Some(mac()))
        );
        play(&mut s, false, t0 + ms(1_100));
        for n in (1_250..20_000).step_by(250) {
            assert!(decide(&mut s, Event::Tick, t0 + ms(n)).is_empty());
        }
        assert!(decide(&mut s, active_transport(), t0 + ms(20_000)).is_empty());
    }

    #[test]
    fn a_new_host_is_introduced_only_while_this_host_owns() {
        let t0 = Instant::now();
        let ipad = addr("11:22:33:44:55:66");
        let mut s = linked(t0);
        decide(&mut s, source(host(), AudioSourceState::Media), t0);
        assert_eq!(
            decide(
                &mut s,
                Event::ConnectedDevices(vec![host(), mac(), ipad]),
                t0
            ),
            vec![
                Action::SendNewDeviceMediaInfo { target: ipad },
                Action::SendNewTipi { target: ipad }
            ]
        );
        assert!(decide(
            &mut s,
            Event::ConnectedDevices(vec![host(), mac(), ipad]),
            t0
        )
        .is_empty());

        let mut s = linked(t0);
        decide(&mut s, source(mac(), AudioSourceState::Media), t0);
        assert!(decide(
            &mut s,
            Event::ConnectedDevices(vec![host(), mac(), ipad]),
            t0
        )
        .is_empty());
    }

    #[test]
    fn manual_commands_ignore_the_setting_and_the_window() {
        let t0 = Instant::now();
        let mut s = linked(t0);
        s.enabled = false;
        assert_eq!(
            decide(&mut s, Event::Yield, t0),
            yield_actions(true, HandoffEventKind::Yielded, None)
        );
        play(&mut s, true, t0);
        let actions = decide(&mut s, Event::TakeOver, t0 + ms(10));
        assert_eq!(actions, take_over_actions());
        assert_eq!(s.view().owner, HandoffOwner::Local);
    }

    #[test]
    fn a_recorded_event_is_published_with_its_peer() {
        let mut s = HandoffState::new(false, true);
        s.record(
            HandoffEventKind::TookOver,
            Some(mac()),
            "2026-09-15T10:00:00+10:00".into(),
        );
        let ev = s.view().last_event.unwrap();
        assert_eq!(ev.kind, HandoffEventKind::TookOver);
        assert_eq!(ev.peer.as_deref(), Some("A4:83:E7:00:11:22"));
    }

    /// Times are milliseconds after the link opens, one minute before the
    /// timeline below starts. The Mac takes the stream, the user takes it
    /// back, but the A2DP transport stays idle: the local player stalls,
    /// Auris reports streaming NO, and the Mac's later equal-category
    /// request is allowed because the AirPods are not streaming.
    #[test]
    fn live_test_3_reports_the_idle_stream_and_never_says_stopped_while_playing() {
        const CHROMIUM: &str = "org.mpris.MediaPlayer2.chromium.instance237031";
        let macbook = addr("FC:B2:14:0A:0B:0C");
        let t0 = Instant::now();
        let at = |n: u64| t0 + ms(60_000 + n);
        let playback = |playing: bool| Event::Playback {
            playing,
            app: playing.then(|| "Chromium".to_owned()),
            player: playing.then(|| CHROMIUM.to_owned()),
        };
        let run = |stream_starts: bool| {
            let mut s = HandoffState::new(true, true);
            s.local = Some(host());
            decide(&mut s, Event::LinkOpened, t0);
            decide(&mut s, Event::ConnectedDevices(vec![host(), macbook]), t0);
            decide(&mut s, Event::OpeningSettled, t0);
            decide(&mut s, active_transport(), at(0));
            decide(&mut s, playback(false), at(0));
            let mut timeline = vec![
                (6_436, idle_transport()),
                (6_989, source(macbook, AudioSourceState::Media)),
                (11_805, playback(true)),
                (11_900, playback(false)),
                (16_841, playback(true)),
                (
                    18_502,
                    Event::OwnsConnection {
                        owns: true,
                        opening: false,
                    },
                ),
                (
                    18_825,
                    source(addr("00:00:00:00:00:00"), AudioSourceState::Idle),
                ),
            ];
            if stream_starts {
                // A second before the stream check would fire.
                timeline.push((20_500, active_transport()));
            } else {
                timeline.extend([
                    (29_287, playback(false)),
                    (
                        37_042,
                        Event::SmartRouting {
                            sender: macbook,
                            requests_yield: true,
                        },
                    ),
                ]);
            }
            timeline.extend((0..=40_000).step_by(250).map(|n| (n, Event::Tick)));
            timeline.sort_by_key(|(n, e)| (*n, *e == Event::Tick));
            let mut out = Vec::new();
            for (n, event) in timeline {
                out.extend(decide(&mut s, event, at(n)).into_iter().map(|a| (n, a)));
            }
            (s, out)
        };
        let when = |out: &[(u64, Action)], f: &dyn Fn(&Action) -> bool| {
            out.iter()
                .filter(|(_, a)| f(a))
                .map(|(n, _)| *n)
                .collect::<Vec<_>>()
        };
        let silent = |a: &Action| *a == Action::StreamNotStarted;
        let yes = |a: &Action| {
            matches!(
                a,
                Action::SendMediaInfo {
                    streaming: true,
                    ..
                }
            )
        };
        let no = |a: &Action| {
            matches!(
                a,
                Action::SendMediaInfo {
                    streaming: false,
                    ..
                }
            )
        };
        let took_over = |a: &Action| {
            matches!(
                a,
                Action::Record {
                    kind: HandoffEventKind::TookOver,
                    ..
                }
            )
        };

        let (s, out) = run(true);
        assert_eq!(when(&out, &took_over), [18_500]);
        assert_eq!(when(&out, &yes), [18_500, 20_500]);
        assert!(when(&out, &no).is_empty(), "{out:?}");
        assert!(when(&out, &silent).is_empty());
        assert!(s.owns_audio());

        // As logged: the stream never starts.
        let (s, out) = run(false);
        assert_eq!(when(&out, &silent), [21_500]);
        assert_eq!(when(&out, &yes), [18_500]);
        // Not at 29.287 while the grace and the stop wait run: 3 s later.
        assert_eq!(when(&out, &no), [32_500]);
        assert!(!s.owns_audio());
    }

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// Smart-routing relay bodies from the MacBook.
    const MAC_NOT_PLAYING: &str = "01e54a706c6179696e67417070424e4152686f737453747265616d696e675374617465424e4f496274416464726573735146433a42323a31343a43333a42453a34414662744e616d65434d6163586f74686572446576696365417564696f43617465676f72793064";
    const MAC_NEW_TIPI: &str = "01e54869646c6554696d6508476e65775469706901496274416464726573735146433a42323a31343a43333a42453a34414662744e616d65434d6163506e6561726279417564696f53636f72650e";
    const MAC_SPOTIFY: &str = "01e54a706c6179696e6741707052636f6d2e73706f746966792e636c69656e7452686f737453747265616d696e675374617465424e4f496274416464726573735146433a42323a31343a43333a42453a34414662744e616d65434d6163586f74686572446576696365417564696f43617465676f7279312d01";
    const MAC_CHROMIUM: &str = "01e54a706c6179696e674170705c6f72672e6368726f6d69756d2e4368726f6d69756d2e68656c70657252686f737453747265616d696e675374617465424e4f496274416464726573735146433a42323a31343a43333a42453a34414662744e616d65434d6163586f74686572446576696365417564696f43617465676f727931c900";
    /// Truncated at the log's line limit; the ownership key is complete.
    const MAC_HIJACK: &str = "01e65b536d617274526f7574696e674b657953686f774e65617262795549014a6c6f63616c73636f7265312d0146726561736f6e4848696a61636b763251617564696f526f7574696e6753636f7265a25f617564696f526f7574696e675365744f776e657273686970546f46616c7365014b72656d6f746573636f7265a2";

    /// A ping-pong timeline, replayed. Times are milliseconds after the link
    /// opens, one minute before the timeline starts. Chromium gets re-paused
    /// three times; only the first remains. The second comes 10.2 s after the
    /// latest request, so the source naming this host ends the hold, and the
    /// third is the same player resuming after its one re-pause (a 1.2 s
    /// flicker that takes nothing).
    #[test]
    fn live_ping_pong_timeline_yields_once_and_never_takes_over() {
        const CHROMIUM: &str = "org.mpris.MediaPlayer2.chromium.instance237031";
        let macbook = addr("FC:B2:14:0A:0B:0C");
        let t0 = Instant::now();
        let at = |n: u64| t0 + ms(60_000 + n);
        let relay = |hex: &str| Event::SmartRouting {
            sender: macbook,
            requests_yield: smart_routing_requests_yield(&unhex(hex)),
        };
        let playback = |playing: bool| Event::Playback {
            playing,
            app: playing.then(|| "Chromium".to_owned()),
            player: playing.then(|| CHROMIUM.to_owned()),
        };

        // Before the timeline: Chromium playing and this host took over.
        let mut s = HandoffState::new(true, true);
        s.local = Some(host());
        decide(&mut s, Event::LinkOpened, t0);
        decide(&mut s, Event::ConnectedDevices(vec![host(), macbook]), t0);
        decide(&mut s, Event::OpeningSettled, t0);
        decide(&mut s, playback(true), at(228));
        decide(&mut s, Event::TakeOver, at(937));

        let mut timeline = vec![
            (
                990,
                Event::OwnsConnection {
                    owns: true,
                    opening: false,
                },
            ),
            (1023, relay(MAC_NOT_PLAYING)),
            (1023, relay(MAC_NEW_TIPI)),
            (1990, relay(MAC_HIJACK)),
            (2006, playback(false)),
            (2023, relay(MAC_SPOTIFY)),
            (
                2056,
                Event::OwnsConnection {
                    owns: false,
                    opening: false,
                },
            ),
            (2400, source(macbook, AudioSourceState::Media)),
            (2522, playback(true)),
            (3005, playback(true)),
            (8630, Event::AudioLinkChanged),
            (11_986, relay(MAC_SPOTIFY)),
            (
                12_316,
                source(addr("00:00:00:00:00:00"), AudioSourceState::Idle),
            ),
            (12_630, source(host(), AudioSourceState::Media)),
            (16_064, relay(MAC_NOT_PLAYING)),
            (16_787, playback(false)),
            (17_352, playback(true)),
            (18_590, playback(false)),
            (26_706, relay(MAC_CHROMIUM)),
            (36_663, relay(MAC_NOT_PLAYING)),
        ];
        timeline.extend((1000..=40_000).step_by(250).map(|n| (n, Event::Tick)));
        timeline.sort_by_key(|(n, e)| (*n, *e == Event::Tick));

        let mut out = Vec::new();
        for (n, event) in timeline {
            out.extend(decide(&mut s, event, at(n)).into_iter().map(|a| (n, a)));
        }

        let yields: Vec<_> = out
            .iter()
            .filter(|(_, a)| {
                matches!(a, Action::Record { kind, .. } if *kind != HandoffEventKind::TookOver)
            })
            .collect();
        assert_eq!(
            yields,
            [&(
                1990,
                Action::Record {
                    kind: HandoffEventKind::YieldRequested,
                    peer: Some(macbook)
                }
            )]
        );
        assert!(!out.iter().any(|(_, a)| matches!(
            a,
            Action::ClaimAudio
                | Action::SendHijack { .. }
                | Action::SendOwnership(true)
                | Action::Record {
                    kind: HandoffEventKind::TookOver,
                    ..
                }
        )));
        let pauses: Vec<_> = out
            .iter()
            .filter(|(_, a)| matches!(a, Action::PauseLocalPlayers))
            .map(|(n, _)| *n)
            .collect();
        assert_eq!(pauses, [1990]);
        let repauses: Vec<_> = out
            .iter()
            .filter_map(|(n, a)| match a {
                Action::RepausePlayer { player } => Some((*n, player.as_deref())),
                _ => None,
            })
            .collect();
        assert_eq!(repauses, [(2522, Some(CHROMIUM))]);
        assert_eq!(s.view().owner, HandoffOwner::Local);
        assert!(!s.owns_audio());
    }

    /// A reconnect while the Mac is connected but idle, replayed. Times are
    /// milliseconds after the reconnect. The opening dump names the Mac as
    /// media source; the old rules yielded on it and paused every local
    /// play.
    #[test]
    fn live_reconnect_timeline_never_yields_from_the_opening_dump() {
        const CHROMIUM: &str = "org.mpris.MediaPlayer2.chromium.instance237031";
        let macbook = addr("FC:B2:14:0A:0B:0C");
        // Named by the address report beside the Mac's; its role is unknown.
        let other = addr("BC:80:4E:01:02:04");
        let t0 = Instant::now();
        let playback = |playing: bool| Event::Playback {
            playing,
            app: playing.then(|| "Chromium".to_owned()),
            player: playing.then(|| CHROMIUM.to_owned()),
        };
        let run = |as_logged: bool| {
            let mut s = HandoffState::new(true, true);
            s.local = Some(host());
            let mut timeline = vec![
                (59, Event::LinkOpened),
                (59, playback(false)),
                (129, Event::ConnectedDevices(vec![other, macbook])),
                (139, Event::OpeningSettled),
                (146, source(macbook, AudioSourceState::Media)),
                (212, Event::ConnectedDevices(vec![other, macbook])),
                (388, Event::AudioLinkChanged),
                (449, Event::AudioLinkChanged),
                (776, Event::AudioLinkChanged),
                (
                    3_336,
                    source(addr("00:00:00:00:00:00"), AudioSourceState::Idle),
                ),
                (8_531, playback(true)),
                (8_799, source(host(), AudioSourceState::Media)),
                (8_800, Event::AudioLinkChanged),
            ];
            if as_logged {
                // Readings caused by the old re-pauses, and the second press.
                timeline.extend([
                    (8_557, playback(false)),
                    (10_778, playback(true)),
                    (10_798, playback(false)),
                ]);
            }
            timeline.extend((0..=18_500).step_by(250).map(|n| (n, Event::Tick)));
            timeline.sort_by_key(|(n, e)| (*n, *e == Event::Tick));
            let mut out = Vec::new();
            for (n, event) in timeline {
                out.extend(
                    decide(&mut s, event, t0 + ms(n))
                        .into_iter()
                        .map(|a| (n, a)),
                );
            }
            (s, out)
        };

        // No Yielded, no pause, no re-pause, no take-over. Media information
        // saying this host stopped streaming is fine: it owns the AirPods.
        let unwanted = |out: &[(u64, Action)]| {
            out.iter()
                .filter(|(_, a)| {
                    !matches!(
                        a,
                        Action::SendMediaInfo {
                            streaming: false,
                            ..
                        }
                    )
                })
                .cloned()
                .collect::<Vec<_>>()
        };
        let (s, out) = run(true);
        assert!(unwanted(&out).is_empty(), "{out:?}");
        assert_eq!(s.view().owner, HandoffOwner::Local);

        // Had Chromium kept playing, the source already names this host, so
        // there is nothing to take over and nothing stops it.
        let (s, out) = run(false);
        assert!(out.is_empty(), "{out:?}");
        assert!(s.owns_audio());
    }
}
