//! The PipeWire card profile of the AirPods follows AAP ownership.
//!
//! Two hosts can hold an ACL link to the same AirPods, and whichever one
//! starts an A2DP stream gets the audio: the ownership handshake does not stop
//! it. Left alone, PipeWire here keeps the `bluez_output.*` sink alive while
//! another host owns them, so a desktop notification sound steals the stream
//! from that host, and a transport torn down underneath a running node leaves
//! the node in `error` for good.
//!
//! So the card profile is set to `off` while another host owns the AirPods:
//! the sink node disappears and nothing local can reach them. Setting `off`
//! also releases the transport from this side, which is what keeps the node
//! out of `error`.
//!
//! A yield is therefore ordered, not fired all at once. Taking the sink away
//! from a playing stream is not a way to stop it: the audio server moves the
//! stream to whatever sink is left, so the music simply carries on out of the
//! laptop speakers, and on a host with no other sink the node is left broken
//! instead. Local playback has to stop first.
//!
//! 1. Pause every local MPRIS player.
//! 2. Wait up to [`DRAIN_TIMEOUT`] for the playback reading to say nothing is
//!    playing. A player that ignores the pause does not get to hold the yield
//!    up for ever: the yield proceeds when the window ends.
//! 3. Switch the card profile to `off`, which releases the transport.
//! 4. Only then tell the AirPods this host no longer owns them.
//!
//! Taking over runs the other way: the A2DP profile goes back before any AAP
//! write, the node is recreated, and as the highest-priority configured
//! default sink it takes the streams back.
//!
//! `pw-cli set-param` is deliberate, and deliberately without a `save` field:
//! `wpctl set-profile` saves, which would persist `off` in
//! `~/.local/state/wireplumber/default-profile` as the user's own choice.

use std::{
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use serde_json::Value;
use tokio::{process::Command, sync::Notify, task::JoinHandle};
use tracing::{debug, info, warn};

/// Wait between retries while the card is missing or the switch failed.
const RETRY_GAP: Duration = Duration::from_millis(500);
/// Give up retrying after this long: the AirPods' card appears within about
/// two seconds of a connection, so anything longer is a real failure.
const RETRY_WINDOW: Duration = Duration::from_secs(15);
/// A `pw-dump` or `pw-cli` call that takes longer than this has hung.
const TOOL_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the shutdown restore may take before the daemon exits anyway.
const RESTORE_TIMEOUT: Duration = Duration::from_secs(3);
/// How long a yield waits for local playback to stop after the pause request
/// before switching the card off anyway. An MPRIS pause and the stream stop
/// that follows it are a couple of D-Bus round trips; longer than this means
/// the player is not going to stop, and a yield that waits for it would leave
/// the other host without audio.
pub const DRAIN_TIMEOUT: Duration = Duration::from_millis(600);
/// How long a yield waits for the card to actually reach `off` before letting
/// the ownership release go out behind it. Bounded so a stuck `pw-cli` cannot
/// stall the session loop.
pub const YIELD_ROUTE_WAIT: Duration = Duration::from_secs(2);
/// The `off` profile, present on every card.
const OFF: &str = "off";
/// Every high-fidelity playback profile starts with this; a `headset-*`
/// profile is never chosen, it would put the AirPods on the call codec.
const A2DP: &str = "a2dp-sink";

/// Whether a yield may take the card off the AirPods yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drain {
    /// Nothing is playing locally: switch the card off now.
    Done,
    /// A player is still going and the window has not run out.
    Waiting,
    /// The window ran out with a player still going: switch off anyway,
    /// because the other host is waiting for the AirPods.
    Expired,
}

/// Decide the drain step from the latest playback reading and how long the
/// yield has waited since asking the players to pause.
pub fn drain_state(playing: bool, waited: Duration) -> Drain {
    if !playing {
        return Drain::Done;
    }
    if waited >= DRAIN_TIMEOUT {
        return Drain::Expired;
    }
    Drain::Waiting
}

/// Where the local audio route should point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteMode {
    /// This host owns the AirPods: the A2DP profile is on.
    Own,
    /// Another host owns them: the card is `off`.
    Yielded,
}

impl RouteMode {
    fn as_str(self) -> &'static str {
        match self {
            RouteMode::Own => "own",
            RouteMode::Yielded => "yielded",
        }
    }
}

/// One entry of the card's `EnumProfile`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileInfo {
    /// `Profile` index to pass to `pw-cli set-param`.
    pub index: u32,
    /// `off`, `a2dp-sink`, `headset-head-unit` and so on.
    pub name: String,
    /// The profile can be selected right now.
    pub available: bool,
}

/// The AirPods' bluez card as `pw-dump` describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardInfo {
    /// PipeWire global id, the first argument of `pw-cli set-param`.
    pub id: u32,
    /// Index of the profile in use.
    pub current: u32,
    /// Everything the card offers.
    pub profiles: Vec<ProfileInfo>,
}

impl CardInfo {
    fn name_of(&self, index: u32) -> &str {
        self.profiles
            .iter()
            .find(|p| p.index == index)
            .map_or("?", |p| p.name.as_str())
    }

    fn is_a2dp(&self, index: u32) -> bool {
        self.name_of(index).starts_with(A2DP)
    }
}

/// What [`apply`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applied {
    /// The card already had the wanted profile.
    NoChange,
    /// The profile was switched.
    Changed {
        /// Profile index before the switch.
        from: u32,
        /// Profile index now in use.
        to: u32,
        /// Name of `from`.
        from_name: String,
        /// Name of `to`.
        to_name: String,
        /// The A2DP profile just left, to restore on the next take-over;
        /// `None` when the previous profile was not an A2DP one.
        remembered_a2dp: Option<u32>,
    },
}

/// Why [`apply`] could not do it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteError {
    /// No bluez card for this address: the AirPods are not connected, or
    /// PipeWire has not created it yet.
    NoCard,
    /// `pw-dump` or `pw-cli` is not installed.
    ToolMissing,
    /// Anything else, with the tool's own words.
    Failed(String),
}

impl std::fmt::Display for RouteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RouteError::NoCard => f.write_str("no PipeWire bluez card for the AirPods"),
            RouteError::ToolMissing => f.write_str("pw-dump or pw-cli is not installed"),
            RouteError::Failed(e) => f.write_str(e),
        }
    }
}

/// `bluez_card.BC_80_4E_01_02_03` for `BC:80:4E:01:02:03`.
fn card_name(addr: &str) -> String {
    format!(
        "bluez_card.{}",
        addr.replace([':', '-'], "_").to_uppercase()
    )
}

/// PipeWire writes `available` as `"yes"`, `"no"`, `"unknown"` or a bool,
/// depending on the version. Anything but an explicit no counts as available.
fn available_of(v: Option<&Value>) -> bool {
    match v {
        None => true,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.eq_ignore_ascii_case("no"),
        Some(_) => true,
    }
}

fn profile_of(v: &Value) -> Option<ProfileInfo> {
    let index = u32::try_from(v.get("index")?.as_u64()?).ok()?;
    Some(ProfileInfo {
        index,
        name: v.get("name")?.as_str()?.to_owned(),
        available: available_of(v.get("available")),
    })
}

/// Find the AirPods' card in a `pw-dump` document. Objects without the fields
/// are skipped, so a partial dump or a future node shape cannot panic.
pub fn parse_card(pw_dump_json: &str, addr: &str) -> Option<CardInfo> {
    let wanted = card_name(addr);
    let doc: Value = serde_json::from_str(pw_dump_json).ok()?;
    doc.as_array()?
        .iter()
        .find_map(|object| card_of(object, &wanted))
}

/// One `pw-dump` object as the AirPods' card, or `None` for anything else.
fn card_of(object: &Value, wanted: &str) -> Option<CardInfo> {
    if object.get("type").and_then(Value::as_str) != Some("PipeWire:Interface:Device") {
        return None;
    }
    let info = object.get("info")?;
    let name = info
        .get("props")
        .and_then(|p| p.get("device.name"))
        .and_then(Value::as_str)?;
    if name != wanted {
        return None;
    }
    let id = u32::try_from(object.get("id").and_then(Value::as_u64)?).ok()?;
    let params = info.get("params")?;
    let current = params
        .get("Profile")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(|p| p.get("index"))
        .and_then(Value::as_u64)
        .and_then(|i| u32::try_from(i).ok())?;
    let profiles = params
        .get("EnumProfile")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(profile_of).collect())
        .unwrap_or_default();
    Some(CardInfo {
        id,
        current,
        profiles,
    })
}

/// The profile index `mode` wants, or `None` when the card offers nothing
/// suitable.
///
/// While yielded that is `off`. While owning it is the A2DP profile already in
/// use if there is one, then `remembered_a2dp` (so the codec the user was on
/// comes back without an AVDTP reconfigure), then plain `a2dp-sink`, then the
/// first available `a2dp-sink*`. A `headset-*` profile is never chosen.
pub fn pick_profile(card: &CardInfo, mode: RouteMode, remembered_a2dp: Option<u32>) -> Option<u32> {
    let usable = |p: &&ProfileInfo| p.available;
    match mode {
        RouteMode::Yielded => card
            .profiles
            .iter()
            .find(|p| p.name == OFF)
            .map(|p| p.index),
        RouteMode::Own => {
            if card.is_a2dp(card.current) {
                return Some(card.current);
            }
            let remembered = remembered_a2dp.and_then(|index| {
                card.profiles
                    .iter()
                    .find(|p| p.index == index && p.name.starts_with(A2DP))
                    .filter(usable)
                    .map(|p| p.index)
            });
            remembered
                .or_else(|| {
                    card.profiles
                        .iter()
                        .find(|p| p.name == A2DP)
                        .filter(usable)
                        .map(|p| p.index)
                })
                .or_else(|| {
                    card.profiles
                        .iter()
                        .find(|p| p.name.starts_with(A2DP) && p.available)
                        .map(|p| p.index)
                })
        }
    }
}

async fn run_tool(program: &str, args: &[String]) -> Result<Vec<u8>, RouteError> {
    let run = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .output();
    let output = match tokio::time::timeout(TOOL_TIMEOUT, run).await {
        Err(_) => return Err(RouteError::Failed(format!("{program} timed out"))),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(RouteError::ToolMissing)
        }
        Ok(Err(e)) => {
            return Err(RouteError::Failed(format!(
                "{program} failed to start: {e}"
            )))
        }
        Ok(Ok(output)) => output,
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(RouteError::Failed(format!(
            "{program} exited {}: {}",
            output.status,
            stderr.trim()
        )));
    }
    Ok(output.stdout)
}

/// Read the card and, if it is on the wrong profile, switch it.
pub async fn apply(
    mode: RouteMode,
    addr: &str,
    remembered_a2dp: Option<u32>,
) -> Result<Applied, RouteError> {
    let dump = run_tool("pw-dump", &[]).await?;
    let dump = String::from_utf8_lossy(&dump);
    let card = parse_card(&dump, addr).ok_or(RouteError::NoCard)?;
    let target = pick_profile(&card, mode, remembered_a2dp).ok_or_else(|| {
        RouteError::Failed(format!(
            "the bluez card offers no {} profile",
            match mode {
                RouteMode::Own => A2DP,
                RouteMode::Yielded => OFF,
            }
        ))
    })?;
    if target == card.current {
        return Ok(Applied::NoChange);
    }
    let from = card.current;
    // Leaving an A2DP profile: remember which one, so the take-over restores
    // the same codec instead of renegotiating AVDTP.
    let remembered = card.is_a2dp(from).then_some(from);
    run_tool(
        "pw-cli",
        &[
            "set-param".to_owned(),
            card.id.to_string(),
            "Profile".to_owned(),
            format!("{{ index: {target} }}"),
        ],
    )
    .await?;
    Ok(Applied::Changed {
        from,
        to: target,
        from_name: card.name_of(from).to_owned(),
        to_name: card.name_of(target).to_owned(),
        remembered_a2dp: remembered,
    })
}

/// An outstanding `off`, so the daemon can put the profile back on its way
/// out. Process-wide because the shutdown path in `main` does not own the
/// [`Router`].
static OUTSTANDING: Mutex<Option<(String, Option<u32>)>> = Mutex::new(None);

fn note_yielded(addr: &str, remembered: Option<u32>) {
    if let Ok(mut held) = OUTSTANDING.lock() {
        *held = Some((addr.to_owned(), remembered));
    }
}

fn clear_yielded() {
    if let Ok(mut held) = OUTSTANDING.lock() {
        *held = None;
    }
}

/// Put the A2DP profile back if the daemon is exiting while the card is `off`.
/// Without this a user who stops `aurisd` while the Mac owns the AirPods would
/// find a card stuck at `off` and no way to hear anything.
pub async fn restore_on_exit() {
    let Some((addr, remembered)) = OUTSTANDING.lock().ok().and_then(|held| held.clone()) else {
        return;
    };
    match tokio::time::timeout(RESTORE_TIMEOUT, apply(RouteMode::Own, &addr, remembered)).await {
        Ok(Ok(Applied::Changed { to_name, .. })) => {
            info!(profile = %to_name, reason = "shutdown", "AirPods audio route restored");
        }
        Ok(Ok(Applied::NoChange)) => {}
        Ok(Err(e)) => warn!(error = %e, "could not restore the AirPods audio route on shutdown"),
        Err(_) => warn!("restoring the AirPods audio route on shutdown timed out"),
    }
    clear_yielded();
}

/// Drives the card profile towards the mode the handoff logic asked for.
///
/// One task at a time: a new [`Router::set`] aborts the previous one, so a
/// take-over that follows a yield within the retry window cannot be overtaken
/// by the yield's retries. The generation counter names the task in the log.
#[derive(Default)]
pub struct Router {
    mode: Option<RouteMode>,
    remembered: Arc<Mutex<Option<u32>>>,
    settled: Arc<AtomicBool>,
    /// Woken when a switch task finishes, however it finished, so a yield can
    /// wait for the card to be off without polling.
    finished: Arc<Notify>,
    generation: u64,
    task: Option<JoinHandle<()>>,
}

impl Router {
    /// No mode wanted yet, nothing running.
    pub fn new() -> Self {
        Self::default()
    }

    /// The mode the latest [`Router::set`] asked for.
    pub fn mode(&self) -> Option<RouteMode> {
        self.mode
    }

    /// Tasks started so far, for tests and logs.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The A2DP profile a yield switched away from.
    pub fn remembered_a2dp(&self) -> Option<u32> {
        self.remembered.lock().ok().and_then(|r| *r)
    }

    /// Ask for `mode`. Returns at once; the switch runs in a task that retries
    /// every [`RETRY_GAP`] for up to [`RETRY_WINDOW`], because the card can
    /// appear a second or two after the AirPods connect.
    pub fn set(&mut self, addr: &str, mode: RouteMode, reason: &'static str) {
        if self.mode == Some(mode) && self.settled.load(Ordering::Relaxed) {
            debug!(
                mode = mode.as_str(),
                reason, "AirPods audio route unchanged"
            );
            return;
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
        self.mode = Some(mode);
        self.generation = self.generation.wrapping_add(1);
        self.settled.store(false, Ordering::Relaxed);
        let generation = self.generation;
        let settled = Arc::clone(&self.settled);
        let remembered = Arc::clone(&self.remembered);
        let addr = addr.to_owned();
        let finished = Arc::clone(&self.finished);
        self.task = Some(tokio::spawn(async move {
            drive(addr, mode, reason, generation, settled, remembered).await;
            finished.notify_waiters();
        }));
    }

    /// Wait until the latest [`Router::set`] has finished, or `timeout`
    /// passes, and report whether the card reached the wanted profile.
    ///
    /// The yield uses this so the ownership release cannot go out before the
    /// transport has been released from this side. A switch that failed, or
    /// one aborted by a newer request, never becomes settled; the timeout is
    /// what stops a yield waiting on it.
    pub async fn wait_settled(&self, timeout: Duration) -> bool {
        // Registered before the check, so a task that finishes in between
        // still wakes this.
        let waiter = self.finished.notified();
        if self.settled.load(Ordering::Relaxed) {
            return true;
        }
        let _ = tokio::time::timeout(timeout, waiter).await;
        self.settled.load(Ordering::Relaxed)
    }

    /// Stop trying. Used when the AirPods disconnect: the card is gone, and
    /// the profile it was on says nothing about the next connection.
    pub fn cancel(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        self.mode = None;
        self.settled.store(false, Ordering::Relaxed);
        // Nothing is coming: release anything waiting for this switch.
        self.finished.notify_waiters();
        if let Ok(mut held) = self.remembered.lock() {
            *held = None;
        }
        clear_yielded();
    }
}

impl Drop for Router {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// One `set`: retry until the card exists and the switch lands.
async fn drive(
    addr: String,
    mode: RouteMode,
    reason: &'static str,
    generation: u64,
    settled: Arc<AtomicBool>,
    remembered: Arc<Mutex<Option<u32>>>,
) {
    let deadline = tokio::time::Instant::now() + RETRY_WINDOW;
    loop {
        let want = remembered.lock().ok().and_then(|r| *r);
        match apply(mode, &addr, want).await {
            Ok(Applied::NoChange) => {
                debug!(
                    mode = mode.as_str(),
                    reason, generation, "AirPods audio route already set"
                );
                if mode == RouteMode::Yielded {
                    note_yielded(&addr, want);
                } else {
                    clear_yielded();
                }
                settled.store(true, Ordering::Relaxed);
                return;
            }
            Ok(Applied::Changed {
                from_name,
                to_name,
                remembered_a2dp,
                ..
            }) => {
                if let (Some(index), Ok(mut held)) = (remembered_a2dp, remembered.lock()) {
                    *held = Some(index);
                }
                info!(
                    profile = %to_name,
                    reason,
                    from = %from_name,
                    generation,
                    "AirPods audio route"
                );
                if mode == RouteMode::Yielded {
                    note_yielded(&addr, remembered_a2dp.or(want));
                } else {
                    clear_yielded();
                }
                settled.store(true, Ordering::Relaxed);
                return;
            }
            Err(RouteError::ToolMissing) => {
                warn!(
                    mode = mode.as_str(),
                    reason, "pw-dump or pw-cli is missing; the AirPods audio route is not managed"
                );
                return;
            }
            Err(e) => {
                if tokio::time::Instant::now() >= deadline {
                    warn!(
                        mode = mode.as_str(),
                        reason,
                        error = %e,
                        "could not set the AirPods audio route"
                    );
                    return;
                }
                debug!(mode = mode.as_str(), reason, error = %e, "retrying the AirPods audio route");
                tokio::time::sleep(RETRY_GAP).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADDR: &str = "BC:80:4E:01:02:03";
    /// No card anywhere has this address, so a router test can never reach a
    /// real `pw-cli set-param`: every attempt stops at `NoCard`.
    const NOWHERE: &str = "AA:BB:CC:DD:EE:FF";

    /// A trimmed `pw-dump`: one node to ignore, one foreign card, then the
    /// AirPods' card with the profiles this machine offers.
    const DUMP: &str = r#"[
      { "id": 61, "type": "PipeWire:Interface:Node",
        "info": { "props": { "device.name": "bluez_card.BC_80_4E_01_02_03" } } },
      { "id": 44, "type": "PipeWire:Interface:Device",
        "info": { "props": { "device.name": "alsa_card.pci-0000_c1_00.6" },
                  "params": { "Profile": [ { "index": 2, "name": "output:analog-stereo" } ],
                              "EnumProfile": [ { "index": 0, "name": "off", "available": "yes" } ] } } },
      { "id": 9, "type": "PipeWire:Interface:Device" },
      { "id": 143, "type": "PipeWire:Interface:Device",
        "info": { "props": { "device.name": "bluez_card.BC_80_4E_01_02_03" },
                  "params": {
                    "Profile": [ { "index": 131076, "name": "a2dp-sink", "available": "yes", "save": false } ],
                    "EnumProfile": [
                      { "index": 0, "name": "off", "available": "yes" },
                      { "index": 131073, "name": "a2dp-sink-sbc", "available": "yes" },
                      { "index": 131074, "name": "a2dp-sink-sbc_xq", "available": true },
                      { "index": 131076, "name": "a2dp-sink", "available": "yes" },
                      { "name": "broken-no-index" },
                      { "index": 196864, "name": "headset-head-unit-cvsd", "available": "yes" },
                      { "index": 196865, "name": "headset-head-unit", "available": "yes" }
                    ] } } }
    ]"#;

    fn card() -> CardInfo {
        parse_card(DUMP, ADDR).expect("the AirPods card is in the fixture")
    }

    #[test]
    fn the_bluez_card_is_found_by_address_and_nodes_are_ignored() {
        let card = card();
        assert_eq!(card.id, 143);
        assert_eq!(card.current, 131076);
        // The entry without an index is dropped, the rest are kept in order.
        assert_eq!(
            card.profiles.iter().map(|p| p.index).collect::<Vec<_>>(),
            [0, 131073, 131074, 131076, 196864, 196865]
        );
        assert!(card.profiles.iter().all(|p| p.available));
        assert_eq!(card.name_of(0), "off");
    }

    #[test]
    fn an_address_with_no_card_and_a_broken_dump_are_both_none() {
        assert_eq!(parse_card(DUMP, "AA:BB:CC:DD:EE:FF"), None);
        assert_eq!(parse_card("not json", ADDR), None);
        assert_eq!(parse_card("{}", ADDR), None);
        assert_eq!(parse_card("[]", ADDR), None);
        // Underscores and lower case name the same card.
        assert!(parse_card(DUMP, "bc_80_4e_01_02_03").is_some());
    }

    #[test]
    fn yielding_picks_off_whatever_is_remembered() {
        assert_eq!(pick_profile(&card(), RouteMode::Yielded, None), Some(0));
        assert_eq!(
            pick_profile(&card(), RouteMode::Yielded, Some(131073)),
            Some(0)
        );
        let mut no_off = card();
        no_off.profiles.retain(|p| p.name != OFF);
        assert_eq!(pick_profile(&no_off, RouteMode::Yielded, None), None);
    }

    #[test]
    fn owning_keeps_the_a2dp_profile_already_in_use() {
        // Already on a codec-specific A2DP profile: no reconfigure.
        let mut on_sbc = card();
        on_sbc.current = 131073;
        assert_eq!(pick_profile(&on_sbc, RouteMode::Own, None), Some(131073));
        assert_eq!(
            pick_profile(&on_sbc, RouteMode::Own, Some(131076)),
            Some(131073)
        );
    }

    #[test]
    fn owning_from_off_restores_the_remembered_profile_then_falls_back() {
        let mut off = card();
        off.current = 0;
        assert_eq!(
            pick_profile(&off, RouteMode::Own, Some(131074)),
            Some(131074)
        );
        // Remembered but gone, or unavailable: the exact a2dp-sink profile.
        assert_eq!(pick_profile(&off, RouteMode::Own, Some(999)), Some(131076));
        let mut stale = off.clone();
        stale.profiles[1].available = false;
        assert_eq!(
            pick_profile(&stale, RouteMode::Own, Some(131073)),
            Some(131076)
        );
        assert_eq!(pick_profile(&off, RouteMode::Own, None), Some(131076));
        // A headset profile is never chosen, even as the last resort.
        let mut headset_only = off.clone();
        headset_only.profiles.retain(|p| !p.name.starts_with(A2DP));
        assert_eq!(
            pick_profile(&headset_only, RouteMode::Own, Some(131076)),
            None
        );
        // No plain a2dp-sink: the first available codec profile.
        let mut codecs = off.clone();
        codecs.profiles.retain(|p| p.name != A2DP);
        assert_eq!(pick_profile(&codecs, RouteMode::Own, None), Some(131073));
        codecs.profiles[1].available = false;
        assert_eq!(pick_profile(&codecs, RouteMode::Own, None), Some(131074));
    }

    #[test]
    fn available_reads_both_spellings() {
        assert!(available_of(None));
        assert!(available_of(Some(&Value::Bool(true))));
        assert!(!available_of(Some(&Value::Bool(false))));
        assert!(available_of(Some(&Value::String("yes".into()))));
        assert!(!available_of(Some(&Value::String("no".into()))));
        assert!(available_of(Some(&Value::String("unknown".into()))));
    }

    /// The router never shells out here: with no `pw-dump` in a test
    /// environment the task simply fails, so only its bookkeeping is checked.
    #[tokio::test]
    async fn each_set_starts_one_generation_and_cancel_forgets_everything() {
        let mut router = Router::new();
        assert_eq!(router.mode(), None);
        router.set(NOWHERE, RouteMode::Yielded, "test");
        assert_eq!(router.mode(), Some(RouteMode::Yielded));
        assert_eq!(router.generation(), 1);
        // A repeat of an unsettled mode restarts the task; the abort of the
        // previous one must not leave the router without a task.
        router.set(NOWHERE, RouteMode::Yielded, "test");
        assert_eq!(router.generation(), 2);
        router.set(NOWHERE, RouteMode::Own, "test");
        assert_eq!(router.generation(), 3);
        assert_eq!(router.mode(), Some(RouteMode::Own));
        router.cancel();
        assert_eq!(router.mode(), None);
        assert_eq!(router.remembered_a2dp(), None);
        assert_eq!(router.generation(), 3);
    }

    #[test]
    fn the_drain_stops_waiting_when_playback_stops_or_the_window_ends() {
        assert_eq!(drain_state(false, Duration::ZERO), Drain::Done);
        // Still playing, window open: wait.
        assert_eq!(drain_state(true, Duration::ZERO), Drain::Waiting);
        assert_eq!(
            drain_state(true, DRAIN_TIMEOUT - Duration::from_millis(1)),
            Drain::Waiting
        );
        // A player that ignores the pause never blocks the yield.
        assert_eq!(drain_state(true, DRAIN_TIMEOUT), Drain::Expired);
        assert_eq!(drain_state(true, Duration::from_secs(60)), Drain::Expired);
        // A stop always wins, however long it took.
        assert_eq!(drain_state(false, Duration::from_secs(60)), Drain::Done);
    }

    #[tokio::test]
    async fn a_yield_gives_up_waiting_for_a_card_that_never_appears() {
        let mut router = Router::new();
        router.set(NOWHERE, RouteMode::Yielded, "test");
        // No such card, so the switch cannot settle: the yield proceeds.
        assert!(!router.wait_settled(Duration::from_millis(50)).await);
        router.settled.store(true, Ordering::Relaxed);
        assert!(router.wait_settled(Duration::from_millis(50)).await);
        // A cancelled switch releases the waiter instead of stalling it.
        router.cancel();
        assert!(!router.wait_settled(Duration::from_millis(50)).await);
    }

    #[tokio::test]
    async fn a_settled_mode_is_not_re_applied() {
        let mut router = Router::new();
        router.set(NOWHERE, RouteMode::Own, "test");
        router.settled.store(true, Ordering::Relaxed);
        router.set(NOWHERE, RouteMode::Own, "test");
        assert_eq!(router.generation(), 1);
        router.set(NOWHERE, RouteMode::Yielded, "test");
        assert_eq!(router.generation(), 2);
    }
}
