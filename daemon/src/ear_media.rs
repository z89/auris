//! In-ear detection drives the local media players.
//!
//! [`decide`] is pure. It takes the state, one event, what the rest of the
//! daemon knows right now and a clock reading, and returns the side effects;
//! `aap::session` performs them over MPRIS. This is the Linux side of what
//! macOS and LibrePods do with the 0x0006 ear-detection report: taking a bud
//! out stops the music, putting it back starts it again.
//!
//! # The rule
//!
//! The default is **pause when any bud that was in an ear leaves it**, which
//! is what macOS does: with both buds in, removing one of the two pauses.
//! Setting `[ear] pause_on_one_of_two = false` narrows it to the last bud
//! leaving, for people who habitually listen with one bud and swap sides.
//! A bud going into the case counts as leaving the ear, so closing the case
//! pauses too.
//!
//! A resume is far more intrusive than a pause: sound starts in a room where
//! the user may not expect it. So a resume needs every one of these:
//!
//! * auris performed the pause itself. A player the user paused is left alone.
//! * The ear state came back to at least the in-ear count that was lost, so
//!   the bud that was taken out is back in.
//! * No more than [`RESUME_WINDOW`] has passed since that pause. A bud put
//!   back an hour later is a fresh listening session, not a resumed one.
//! * The user did not touch playback meanwhile. A play or a pause of their
//!   own during the window cancels the pending resume for good.
//! * Local audio is still going to the AirPods: this host owns them and the
//!   card has not been taken off them for a yield. While another host owns
//!   the AirPods, auris never starts a local player.
//!
//! The resume fires at most once, on the settled return to in-ear. If a gate
//! fails at that moment the pending resume is dropped rather than held: a
//! resume that arrives later than the gesture that asked for it is worse than
//! no resume at all.

use std::time::{Duration, Instant};

use crate::{
    config::EarConfig,
    state::{Ear, EarState},
};

/// A changed ear reading must hold this long before it is acted on.
///
/// The accessory flaps: opening the case, a bud role swap, or a bud reseated
/// while putting it on can produce two or three 0x0006 reports a few hundred
/// milliseconds apart. Acting on the first one pauses and resumes audibly for
/// no reason. This is longer than those bursts and short enough that a
/// deliberate removal still feels immediate.
pub const SETTLE: Duration = Duration::from_millis(700);

/// A pause made because a bud left the ear can only be undone this soon after
/// it. Five minutes covers taking a bud out to answer someone; past that, the
/// user has moved on and a sudden resume would be a surprise.
pub const RESUME_WINDOW: Duration = Duration::from_secs(5 * 60);

/// Something the session observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A resolved ear reading, left and right, as published in the snapshot.
    Ear(Ear),
    /// A local MPRIS reading.
    Playback {
        /// Any player is Playing.
        playing: bool,
        /// MPRIS bus name of the playing player.
        player: Option<String>,
    },
    /// The AAP link ended: there is no live ear state any more.
    LinkClosed,
    /// Periodic tick; fires the settle timer and expires a pending resume.
    Tick,
}

/// A side effect for the session to perform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Pause every local MPRIS player that is Playing.
    PauseLocalPlayers,
    /// Resume the player auris paused when the bud left the ear.
    ResumePlayer {
        /// MPRIS bus name; `None` when it was never learned, in which case
        /// nothing is resumed.
        player: Option<String>,
    },
}

/// What the rest of the daemon knows when an event is folded in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Context {
    /// Local audio is going to the AirPods: the link is up, this host owns
    /// them as far as handoff knows, and the card has not been taken off them
    /// for a yield.
    pub audio_on_airpods: bool,
}

/// A pause this module performed, waiting to be undone.
#[derive(Debug, Clone)]
struct Paused {
    /// When the pause was sent.
    at: Instant,
    /// In-ear count just before it; the resume needs that many back.
    count: u8,
    /// The player that was playing.
    player: Option<String>,
    /// The pause has not been seen in a playback reading yet, so the next
    /// stop is ours rather than the user's.
    confirming: bool,
}

/// Everything [`decide`] needs to remember.
#[derive(Debug, Clone)]
pub struct EarMediaState {
    cfg: EarConfig,
    /// Latest settled in-ear count; `None` until the first usable reading.
    settled: Option<u8>,
    /// A reading that differs from `settled`, and when it first arrived.
    pending: Option<(u8, Instant)>,
    /// Latest local playback reading.
    playing: Option<bool>,
    /// Bus name of the player last seen playing.
    player: Option<String>,
    /// An outstanding auris pause.
    paused: Option<Paused>,
}

impl EarMediaState {
    /// Fresh state: no reading, nothing paused.
    pub fn new(cfg: EarConfig) -> Self {
        Self {
            cfg,
            settled: None,
            pending: None,
            playing: None,
            player: None,
            paused: None,
        }
    }

    /// The configuration in force.
    pub fn config(&self) -> EarConfig {
        self.cfg
    }

    /// A resume is waiting for the bud to go back in.
    pub fn resume_pending(&self) -> bool {
        self.paused.is_some()
    }

    /// Buds reported as in an ear. `None` when the reading says nothing at
    /// all, which is the state after a link reset; it must not read as "both
    /// buds were removed" and pause the user's music.
    fn in_ear(ear: Ear) -> Option<u8> {
        if ear.left == EarState::Unknown && ear.right == EarState::Unknown {
            return None;
        }
        Some(u8::from(ear.left == EarState::In) + u8::from(ear.right == EarState::In))
    }

    /// Fewer buds in ears than before.
    fn removed(&mut self, previous: u8, count: u8, ctx: Context, now: Instant) -> Vec<Action> {
        if !self.cfg.auto_pause {
            return Vec::new();
        }
        // The narrow rule waits for the last bud; the default pauses as soon
        // as one of two leaves, like macOS.
        if !self.cfg.pause_on_one_of_two && count > 0 {
            return Vec::new();
        }
        // Nothing to pause, or the audio is not ours to act on.
        if self.playing != Some(true) || !ctx.audio_on_airpods {
            return Vec::new();
        }
        self.paused = Some(Paused {
            at: now,
            count: previous,
            player: self.player.clone(),
            confirming: true,
        });
        vec![Action::PauseLocalPlayers]
    }

    /// More buds in ears than before. Consumes the pending resume either way:
    /// this is the one moment it can fire.
    fn returned(&mut self, count: u8, ctx: Context, now: Instant) -> Vec<Action> {
        let Some(paused) = self.paused.take() else {
            return Vec::new();
        };
        if count < paused.count {
            // Only part way back: one of two buds, when two were in.
            self.paused = Some(paused);
            return Vec::new();
        }
        if !self.cfg.auto_resume
            || !ctx.audio_on_airpods
            || now.saturating_duration_since(paused.at) >= RESUME_WINDOW
            || self.playing == Some(true)
        {
            return Vec::new();
        }
        vec![Action::ResumePlayer {
            player: paused.player,
        }]
    }

    /// Fire the settle timer and expire a pending resume.
    fn tick(&mut self, ctx: Context, now: Instant) -> Vec<Action> {
        if self
            .paused
            .as_ref()
            .is_some_and(|p| now.saturating_duration_since(p.at) >= RESUME_WINDOW)
        {
            self.paused = None;
        }
        let Some((count, since)) = self.pending else {
            return Vec::new();
        };
        if now.saturating_duration_since(since) < SETTLE {
            return Vec::new();
        }
        self.pending = None;
        // The first usable reading is a baseline, not a change: connecting
        // with the buds already out of the ears must not pause anything.
        let Some(previous) = self.settled.replace(count) else {
            return Vec::new();
        };
        if count < previous {
            self.removed(previous, count, ctx, now)
        } else {
            self.returned(count, ctx, now)
        }
    }

    /// A playback edge the user may have caused.
    fn playback(&mut self, playing: bool, player: Option<String>) {
        if player.is_some() {
            self.player = player;
        }
        let previous = self.playing.replace(playing);
        if previous == Some(playing) {
            return;
        }
        let Some(paused) = self.paused.as_mut() else {
            return;
        };
        if playing {
            // The user started a player themselves while the bud is out.
            // They are in control; there is nothing left to put back.
            self.paused = None;
        } else if paused.confirming {
            // Our own pause landing.
            paused.confirming = false;
        } else {
            // A pause of the user's own during the window.
            self.paused = None;
        }
    }
}

/// Fold one event into the state and return what to do about it.
pub fn decide(s: &mut EarMediaState, event: Event, ctx: Context, now: Instant) -> Vec<Action> {
    match event {
        Event::Ear(ear) => {
            let Some(count) = EarMediaState::in_ear(ear) else {
                return Vec::new();
            };
            if s.settled == Some(count) {
                // Back to where it was: the flap never happened.
                s.pending = None;
                return Vec::new();
            }
            if s.pending.map(|(c, _)| c) != Some(count) {
                s.pending = Some((count, now));
            }
            // The settle time has to pass first; a tick fires it.
            s.tick(ctx, now)
        }
        Event::Playback { playing, player } => {
            s.playback(playing, player);
            Vec::new()
        }
        Event::LinkClosed => {
            // No live ear state without a link. A pending resume survives:
            // it is bounded by `RESUME_WINDOW`, so a link that comes back
            // later than that can never resume anything.
            s.settled = None;
            s.pending = None;
            s.playing = None;
            Vec::new()
        }
        Event::Tick => s.tick(ctx, now),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPOTIFY: &str = "org.mpris.MediaPlayer2.spotify";

    fn ear(left: EarState, right: EarState) -> Ear {
        Ear { left, right }
    }

    fn both_in() -> Ear {
        ear(EarState::In, EarState::In)
    }

    fn one_out() -> Ear {
        ear(EarState::In, EarState::Out)
    }

    fn both_out() -> Ear {
        ear(EarState::Out, EarState::Out)
    }

    fn on() -> Context {
        Context {
            audio_on_airpods: true,
        }
    }

    fn resumed() -> Vec<Action> {
        vec![Action::ResumePlayer {
            player: Some(SPOTIFY.to_owned()),
        }]
    }

    fn play(s: &mut EarMediaState, playing: bool, at: Instant) {
        let actions = decide(
            s,
            Event::Playback {
                playing,
                player: playing.then(|| SPOTIFY.to_owned()),
            },
            on(),
            at,
        );
        assert!(actions.is_empty(), "playback never acts on its own");
    }

    /// Report `reading` and let it settle, returning what it decided.
    fn report(s: &mut EarMediaState, reading: Ear, ctx: Context, at: Instant) -> Vec<Action> {
        let early = decide(s, Event::Ear(reading), ctx, at);
        assert!(early.is_empty(), "nothing acts before the settle time");
        decide(s, Event::Tick, ctx, at + SETTLE)
    }

    /// Playing, both buds in and settled, at `t0`.
    fn listening(t0: Instant) -> EarMediaState {
        let mut s = EarMediaState::new(EarConfig::default());
        play(&mut s, true, t0);
        assert!(report(&mut s, both_in(), on(), t0).is_empty(), "baseline");
        s
    }

    #[test]
    fn removing_one_of_two_buds_pauses_and_putting_it_back_resumes() {
        let t0 = Instant::now();
        let mut s = listening(t0);
        let out = t0 + Duration::from_secs(10);
        assert_eq!(
            report(&mut s, one_out(), on(), out),
            [Action::PauseLocalPlayers]
        );
        assert!(s.resume_pending());
        // The pause lands as a reading of our own making.
        play(&mut s, false, out + SETTLE);
        assert!(
            s.resume_pending(),
            "our own pause does not cancel the resume"
        );
        let back = out + Duration::from_secs(20);
        assert_eq!(report(&mut s, both_in(), on(), back), resumed());
        assert!(!s.resume_pending());
    }

    #[test]
    fn a_single_bud_listener_pauses_when_the_last_bud_leaves() {
        let t0 = Instant::now();
        let mut s = EarMediaState::new(EarConfig::default());
        play(&mut s, true, t0);
        assert!(report(&mut s, one_out(), on(), t0).is_empty(), "baseline");
        let out = t0 + Duration::from_secs(5);
        assert_eq!(
            report(&mut s, both_out(), on(), out),
            [Action::PauseLocalPlayers]
        );
        play(&mut s, false, out + SETTLE);
        assert_eq!(
            report(&mut s, one_out(), on(), out + Duration::from_secs(9)),
            resumed()
        );
    }

    #[test]
    fn the_narrow_rule_waits_for_the_last_bud() {
        let t0 = Instant::now();
        let mut s = EarMediaState::new(EarConfig {
            pause_on_one_of_two: false,
            ..EarConfig::default()
        });
        play(&mut s, true, t0);
        assert!(report(&mut s, both_in(), on(), t0).is_empty());
        assert!(
            report(&mut s, one_out(), on(), t0 + Duration::from_secs(3)).is_empty(),
            "one of two is not a pause under the narrow rule"
        );
        assert_eq!(
            report(&mut s, both_out(), on(), t0 + Duration::from_secs(6)),
            [Action::PauseLocalPlayers]
        );
    }

    #[test]
    fn a_bud_going_into_the_case_counts_as_leaving_the_ear() {
        let t0 = Instant::now();
        let mut s = listening(t0);
        assert_eq!(
            report(
                &mut s,
                ear(EarState::Case, EarState::Case),
                on(),
                t0 + Duration::from_secs(2)
            ),
            [Action::PauseLocalPlayers]
        );
    }

    #[test]
    fn a_flap_shorter_than_the_settle_time_is_ignored() {
        let t0 = Instant::now();
        let mut s = listening(t0);
        let flap = t0 + Duration::from_secs(4);
        assert!(decide(&mut s, Event::Ear(one_out()), on(), flap).is_empty());
        assert!(decide(&mut s, Event::Tick, on(), flap + SETTLE / 2).is_empty());
        // Back in before it settled: nothing happened at all.
        assert!(decide(&mut s, Event::Ear(both_in()), on(), flap + SETTLE / 2).is_empty());
        assert!(decide(&mut s, Event::Tick, on(), flap + SETTLE * 3).is_empty());
        assert!(!s.resume_pending());
    }

    #[test]
    fn a_reading_that_keeps_changing_restarts_the_settle_time() {
        let t0 = Instant::now();
        let mut s = listening(t0);
        let a = t0 + Duration::from_secs(4);
        assert!(decide(&mut s, Event::Ear(one_out()), on(), a).is_empty());
        // A different reading before the first settled starts the clock again.
        assert!(decide(&mut s, Event::Ear(both_out()), on(), a + SETTLE / 2).is_empty());
        assert!(decide(&mut s, Event::Tick, on(), a + SETTLE).is_empty());
        assert_eq!(
            decide(&mut s, Event::Tick, on(), a + SETTLE / 2 + SETTLE),
            [Action::PauseLocalPlayers]
        );
    }

    #[test]
    fn nothing_playing_means_nothing_to_pause_and_nothing_to_resume() {
        let t0 = Instant::now();
        let mut s = EarMediaState::new(EarConfig::default());
        play(&mut s, false, t0);
        assert!(report(&mut s, both_in(), on(), t0).is_empty());
        assert!(report(&mut s, one_out(), on(), t0 + Duration::from_secs(3)).is_empty());
        assert!(!s.resume_pending());
        assert!(report(&mut s, both_in(), on(), t0 + Duration::from_secs(9)).is_empty());
    }

    #[test]
    fn a_user_pause_during_the_window_cancels_the_resume() {
        let t0 = Instant::now();
        let mut s = listening(t0);
        let out = t0 + Duration::from_secs(10);
        assert_eq!(
            report(&mut s, one_out(), on(), out),
            [Action::PauseLocalPlayers]
        );
        play(&mut s, false, out + SETTLE);
        // The user plays something themselves, then stops it.
        play(&mut s, true, out + Duration::from_secs(5));
        assert!(!s.resume_pending(), "the user is in control now");
        play(&mut s, false, out + Duration::from_secs(20));
        assert!(report(&mut s, both_in(), on(), out + Duration::from_secs(30)).is_empty());
    }

    #[test]
    fn a_pause_the_user_made_first_is_not_ours_to_undo() {
        let t0 = Instant::now();
        let mut s = listening(t0);
        let out = t0 + Duration::from_secs(10);
        assert_eq!(
            report(&mut s, one_out(), on(), out),
            [Action::PauseLocalPlayers]
        );
        play(&mut s, false, out + SETTLE);
        // A second player stops later: not our pause, and not a cancel of it.
        play(&mut s, true, out + Duration::from_secs(4));
        play(&mut s, false, out + Duration::from_secs(6));
        assert!(!s.resume_pending());
    }

    #[test]
    fn a_resume_after_the_window_never_happens() {
        let t0 = Instant::now();
        let mut s = listening(t0);
        assert_eq!(
            report(&mut s, one_out(), on(), t0),
            [Action::PauseLocalPlayers]
        );
        play(&mut s, false, t0 + SETTLE);
        let late = t0 + RESUME_WINDOW + Duration::from_secs(1);
        assert!(report(&mut s, both_in(), on(), late).is_empty());
        assert!(!s.resume_pending());
    }

    #[test]
    fn a_link_that_comes_back_inside_the_window_still_resumes() {
        let t0 = Instant::now();
        let mut s = listening(t0);
        assert_eq!(
            report(&mut s, one_out(), on(), t0),
            [Action::PauseLocalPlayers]
        );
        play(&mut s, false, t0 + SETTLE);
        assert!(decide(&mut s, Event::LinkClosed, on(), t0 + Duration::from_secs(5)).is_empty());
        assert!(s.resume_pending());
        let back = t0 + Duration::from_secs(30);
        // The first reading of the new link is a baseline again.
        assert!(report(&mut s, one_out(), on(), back).is_empty());
        assert_eq!(
            report(&mut s, both_in(), on(), back + Duration::from_secs(2)),
            resumed()
        );
    }

    #[test]
    fn a_link_that_comes_back_after_the_window_never_resumes() {
        let t0 = Instant::now();
        let mut s = listening(t0);
        assert_eq!(
            report(&mut s, one_out(), on(), t0),
            [Action::PauseLocalPlayers]
        );
        play(&mut s, false, t0 + SETTLE);
        assert!(decide(&mut s, Event::LinkClosed, on(), t0 + Duration::from_secs(5)).is_empty());
        let late = t0 + RESUME_WINDOW + Duration::from_secs(30);
        // The tick that crosses the window drops it, before any ear report.
        assert!(decide(&mut s, Event::Tick, on(), late).is_empty());
        assert!(!s.resume_pending());
        assert!(report(&mut s, both_in(), on(), late).is_empty());
    }

    #[test]
    fn an_all_unknown_reading_is_not_two_buds_removed() {
        let t0 = Instant::now();
        let mut s = listening(t0);
        let blank = ear(EarState::Unknown, EarState::Unknown);
        assert!(decide(&mut s, Event::Ear(blank), on(), t0).is_empty());
        assert!(decide(&mut s, Event::Tick, on(), t0 + SETTLE * 2).is_empty());
        assert!(!s.resume_pending());
    }

    #[test]
    fn another_host_owning_the_airpods_blocks_both_halves() {
        let t0 = Instant::now();
        let mut s = listening(t0);
        let yielded = Context {
            audio_on_airpods: false,
        };
        assert!(
            report(&mut s, one_out(), yielded, t0).is_empty(),
            "the other host's audio is not ours to pause"
        );
        assert!(!s.resume_pending());

        // Pause while owning, then lose ownership before the bud goes back.
        let mut s = listening(t0);
        assert_eq!(
            report(&mut s, one_out(), on(), t0),
            [Action::PauseLocalPlayers]
        );
        play(&mut s, false, t0 + SETTLE);
        let back = t0 + Duration::from_secs(20);
        assert!(
            report(&mut s, both_in(), yielded, back).is_empty(),
            "auris never starts a player while another host owns the AirPods"
        );
        assert!(
            !s.resume_pending(),
            "the missed resume is dropped, not held"
        );
    }

    #[test]
    fn the_feature_switches_off() {
        let t0 = Instant::now();
        let mut s = EarMediaState::new(EarConfig {
            auto_pause: false,
            ..EarConfig::default()
        });
        play(&mut s, true, t0);
        assert!(report(&mut s, both_in(), on(), t0).is_empty());
        assert!(report(&mut s, one_out(), on(), t0 + Duration::from_secs(2)).is_empty());
        assert!(!s.resume_pending());

        let mut s = EarMediaState::new(EarConfig {
            auto_resume: false,
            ..EarConfig::default()
        });
        assert!(s.config().auto_pause, "only the resume half is off");
        play(&mut s, true, t0);
        assert!(report(&mut s, both_in(), on(), t0).is_empty());
        assert_eq!(
            report(&mut s, one_out(), on(), t0 + Duration::from_secs(2)),
            [Action::PauseLocalPlayers]
        );
        play(&mut s, false, t0 + Duration::from_secs(3));
        assert!(report(&mut s, both_in(), on(), t0 + Duration::from_secs(9)).is_empty());
    }

    #[test]
    fn a_player_never_seen_playing_is_not_resumed_blindly() {
        let t0 = Instant::now();
        let mut s = EarMediaState::new(EarConfig::default());
        // Playing, but no bus name was ever reported.
        assert!(decide(
            &mut s,
            Event::Playback {
                playing: true,
                player: None
            },
            on(),
            t0
        )
        .is_empty());
        assert!(report(&mut s, both_in(), on(), t0).is_empty());
        assert_eq!(
            report(&mut s, one_out(), on(), t0),
            [Action::PauseLocalPlayers]
        );
        decide(
            &mut s,
            Event::Playback {
                playing: false,
                player: None,
            },
            on(),
            t0 + SETTLE,
        );
        assert_eq!(
            report(&mut s, both_in(), on(), t0 + Duration::from_secs(5)),
            [Action::ResumePlayer { player: None }]
        );
    }
}
