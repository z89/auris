//! The single source of truth for the published snapshot.
//!
//! Everything that learns something applies an [`Update`]; the writer task
//! watches for changes. Updates that change nothing do not notify, so an
//! accessory that repeats the same battery packet every few seconds does not
//! cause a file write.

use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

use tokio::sync::watch;

use crate::{
    aap::codec::{BatteryComponent, BatteryEntry, Metadata},
    config::PrimaryBud,
    models,
    settings::{DeviceSettings, SettingCommand},
    state::{
        Cell, EarState, Link, LinkReason, LinkStatus, NoiseControl, NoiseControlMode, Snapshot,
        Source, NAME_KEY, STATUS_CONFIRMED, STATUS_MISMATCH, STATUS_UNREPORTED, STATUS_UNVERIFIED,
        STATUS_VERIFYING, VERIFY_IDLE, VERIFY_REOPENING, VERIFY_SCHEDULED,
    },
};

/// A fact learned about the accessory.
#[derive(Debug, Clone)]
pub enum Update {
    /// A device was selected or its identity was refreshed.
    Identity {
        /// BD_ADDR.
        address: String,
        /// Bluetooth name, if BlueZ has one.
        name: Option<String>,
        /// Uppercase hex product id, if the modalias had one.
        model_id: Option<String>,
    },
    /// BlueZ `Connected` changed for the classic link.
    AclConnected(bool),
    /// The AAP socket opened or closed.
    AapLink(bool),
    /// Battery notification.
    Battery(Vec<BatteryEntry>),
    /// Exact battery observations from an identity-verified BLE advert.
    BleBattery {
        /// Paired classic identity to which the private advert was resolved.
        address: String,
        /// Only measured cells; unknown/sentinel values are omitted.
        entries: Vec<BatteryEntry>,
    },
    /// Expire BLE observations independently of the classic/AAP link.
    ExpireBle {
        /// RFC3339 cutoff; readings older than this become historical.
        before: String,
    },
    /// In-ear detection, in wire order.
    Ear {
        /// Primary bud.
        primary: EarState,
        /// Secondary bud.
        secondary: EarState,
    },
    /// Metadata strings.
    Metadata(Metadata),
    /// Noise control mode, from the accessory's echo.
    NoiseControl(NoiseControlMode),
    /// Conversational awareness state.
    ConversationalAwareness(bool),
    /// Adaptive transparency level.
    AdaptiveLevel(u8),
    /// A typed setting confirmed by an accessory control-state echo.
    Setting(SettingCommand),
    /// A setting datagram left the daemon. The accessory never echoes one, so
    /// this only records what was asked for and arms the readback.
    SettingRequested(SettingCommand),
    /// A rename datagram left the daemon. Recorded under
    /// [`crate::state::NAME_KEY`] so the readback judges it like a setting:
    /// the accessory's own 0x1D metadata on the reopened link is the answer.
    RenameRequested(String),
    /// The AAP link is being dropped and reopened purely to read settings back.
    VerifyReopening,
    /// The readback window on the current link has closed: compare every
    /// pending request against what this link reported.
    VerifyCompleted,
    /// The readback could not run, or did not finish inside its budget.
    VerifyFailed,
    /// Replace the published Apple multi-host switching view.
    Handoff(crate::state::Handoff),
    /// The reconnect sequence the session is running, or `None` when it is
    /// running none. The published `link.status` also depends on
    /// `device.connected`, which the store already holds, so the two can
    /// arrive in either order.
    Link(Option<LinkActivity>),
}

/// A reconnect sequence in flight, as the session sees it. Internal to the
/// daemon: [`crate::state::Link`] is the published shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkActivity {
    /// Why the sequence is running.
    pub reason: LinkReason,
    /// `Device1.Connect` calls started so far; `0` while only scheduled.
    pub attempt: u32,
    /// When the sequence started, RFC3339 in UTC.
    pub since: String,
}

/// Bookkeeping the store needs but the published document must not carry.
#[derive(Debug, Default)]
struct Aux {
    /// The last 0x0006 payload exactly as it came off the wire. Which bud the
    /// primary byte describes depends on which buds are out of the case, and
    /// that can arrive after the ear packet, so the resolution is redone on
    /// every battery update from these raw bytes.
    last_ear: Option<(EarState, EarState)>,
    /// The reconnect sequence the session last reported, if any. The
    /// published document carries the resolved `link` object instead, so a
    /// later `AclConnected` can turn `reconnecting` into `connected` without
    /// the session saying anything.
    link: Option<LinkActivity>,
    /// How often each setting key was reported since the current AAP link
    /// opened. The lifetime counters in the snapshot cannot answer "did the
    /// *fresh* link report this", which is the whole readback question.
    link_reports: BTreeMap<String, u64>,
    /// The name the accessory last gave in its own 0x1D metadata. It outranks
    /// the BlueZ name, which BlueZ only re-reads when a link opens and which
    /// can therefore be many minutes out of date after a rename.
    metadata_name: Option<String>,
}

/// The selected identity and link generation at which a control command was
/// accepted. It is intentionally internal to the daemon control queue rather
/// than part of state.json's frozen wire contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionContext {
    pub address: String,
    pub epoch: u64,
}

/// Holds the current [`Snapshot`] and broadcasts changes.
#[derive(Debug)]
pub struct Store {
    tx: watch::Sender<Snapshot>,
    primary_bud: PrimaryBud,
    /// Bumped whenever a command could otherwise cross a device or link
    /// boundary while waiting behind an opening sequence.
    connection_epoch: AtomicU64,
    /// Non-published per-link bookkeeping.
    aux: Mutex<Aux>,
}

impl Store {
    /// Create a store seeded with `initial`.
    pub fn new(initial: Snapshot, primary_bud: PrimaryBud) -> Arc<Self> {
        Arc::new(Self {
            tx: watch::Sender::new(initial),
            primary_bud,
            connection_epoch: AtomicU64::new(0),
            aux: Mutex::new(Aux::default()),
        })
    }

    /// Subscribe to snapshot changes.
    pub fn subscribe(&self) -> watch::Receiver<Snapshot> {
        self.tx.subscribe()
    }

    /// Current snapshot.
    pub fn snapshot(&self) -> Snapshot {
        self.tx.borrow().clone()
    }

    /// Capture the queue-safety context for a command before it is enqueued.
    pub fn connection_context(&self) -> ConnectionContext {
        // Hold the same read guard across both reads; apply increments the
        // epoch under its write guard, including same-address reconnects.
        let snapshot = self.tx.borrow();
        let epoch = self.connection_epoch.load(Ordering::Acquire);
        ConnectionContext {
            address: snapshot.device.address.clone(),
            epoch,
        }
    }

    /// Apply an update, notifying watchers only if something actually changed.
    pub fn apply(&self, update: Update) {
        let mut aux = self
            .aux
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.tx.send_if_modified(|s| {
            let before = s.clone();
            // An unsuccessful tentative opening may never have set aap_link
            // true. Its teardown still invalidates commands queued behind it.
            let link_ended = matches!(&update, Update::AapLink(false));
            mutate(s, update, self.primary_bud, &mut aux);
            if link_ended
                || s.device.address != before.device.address
                || s.device.model_id != before.device.model_id
                || s.device.connected != before.device.connected
                || s.device.aap_link != before.device.aap_link
            {
                self.connection_epoch.fetch_add(1, Ordering::AcqRel);
            }
            if *s == before {
                false
            } else {
                s.updated_at = crate::now_rfc3339();
                true
            }
        });
    }
}

fn apply_cell(cell: &mut Cell, e: &BatteryEntry) {
    if e.present {
        cell.source = Source::Aap;
        cell.fresh = true;
        cell.level = e.level;
        cell.charging = e.charging;
        cell.last_known_charging = Some(e.charging);
        cell.present = true;
        cell.last_seen = Some(crate::now_rfc3339());
    } else if cell.source != Source::Ble || !cell.fresh {
        // The buds only relay the case level while they sit in it. Keep the
        // last reading so the panel can show it dimmed with its age.
        cell.charging = false;
        cell.present = false;
        cell.fresh = false;
    }
}

fn invalidate_aap(s: &mut Snapshot) {
    for cell in [
        &mut s.battery.left,
        &mut s.battery.right,
        &mut s.battery.case,
    ] {
        if cell.source != Source::Ble {
            cell.fresh = false;
            cell.present = false;
            cell.charging = false;
        }
    }
}

fn recent_aap(cell: &Cell) -> bool {
    cell.fresh
        && cell.source == Source::Aap
        && cell
            .last_seen
            .as_deref()
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .is_some_and(|t| {
                (chrono::Utc::now() - t.with_timezone(&chrono::Utc)).num_seconds() < 15
            })
}

/// Decide which side the primary byte of a 0x0006 payload describes, and write
/// both buds. The wire says "primary" and "secondary", never which bud that is,
/// and the role is not fixed to a side: the bud that is out and working becomes
/// primary, so it moves when you put one away. Battery packets do carry an
/// explicit component byte, so when exactly one bud is out of the case it must
/// be the one the primary byte describes. With both out, or both away, there is
/// nothing to go on and the original left-first assumption stands.
fn assign_ear(s: &mut Snapshot, primary: EarState, secondary: EarState, primary_bud: PrimaryBud) {
    let primary_is_left = match primary_bud {
        PrimaryBud::Left => true,
        PrimaryBud::Right => false,
        PrimaryBud::Auto => match (s.battery.left.present, s.battery.right.present) {
            (true, false) => true,
            (false, true) => false,
            _ => true,
        },
    };
    if primary_is_left {
        s.ear.left = primary;
        s.ear.right = secondary;
    } else {
        s.ear.right = primary;
        s.ear.left = secondary;
    }
}

fn apply_setting(settings: &mut DeviceSettings, setting: SettingCommand) {
    match setting {
        SettingCommand::Microphone(value) => settings.microphone = Some(value),
        SettingCommand::PressSpeed(value) => settings.press_speed = Some(value),
        SettingCommand::HoldDuration(value) => settings.hold_duration = Some(value),
        SettingCommand::ListeningModeCycle(value) => {
            settings.listening_mode_cycle = Some(value);
        }
        SettingCommand::CallControls(value) => settings.call_controls = Some(value),
        SettingCommand::PersonalizedVolume(value) => {
            settings.personalized_volume = Some(value);
        }
    }
}

/// Close the readback window: judge every request the current link could have
/// answered. Only keys this link actually reported can be judged; a key the
/// model never reports stays `unreported` until some later link reports it.
fn finish_verification(
    s: &mut Snapshot,
    link_reports: &BTreeMap<String, u64>,
    metadata_name: Option<&str>,
) {
    let mut reported = serde_json::to_value(&s.settings).unwrap_or(serde_json::Value::Null);
    // The name is not part of `settings`; the accessory reports it in its own
    // metadata packet. Fold it in so one loop judges every pending key.
    if let (Some(object), Some(name)) = (reported.as_object_mut(), metadata_name) {
        object.insert(
            NAME_KEY.to_owned(),
            serde_json::Value::String(name.to_owned()),
        );
    }
    // Taken out to judge each key against `settings_requested` without holding
    // two borrows of the snapshot; put back unconditionally below.
    let mut statuses = std::mem::take(&mut s.settings_status);
    for (key, status) in &mut statuses {
        let pending = status == STATUS_VERIFYING;
        // A key this model never reports, or one a dropped link could not
        // answer, is still eligible: a later dump can settle it.
        let recheck = status == STATUS_UNREPORTED || status == STATUS_UNVERIFIED;
        if !pending && !recheck {
            continue;
        }
        let Some(want) = s.settings_requested.get(key) else {
            if pending {
                *status = STATUS_UNVERIFIED.to_owned();
            }
            continue;
        };
        let fresh = link_reports
            .contains_key(key)
            .then(|| reported.get(key))
            .flatten()
            .filter(|value| !value.is_null());
        match fresh {
            Some(value) => {
                *status = if value == want {
                    STATUS_CONFIRMED
                } else {
                    STATUS_MISMATCH
                }
                .to_owned();
            }
            None if pending => *status = STATUS_UNREPORTED.to_owned(),
            None => {}
        }
    }
    s.settings_status = statuses;
    s.settings_verify = VERIFY_IDLE.to_owned();
    s.verify_reopen = false;
}

/// Settle a pending rename against the name the accessory just reported.
/// `device.name` already holds that name, which is what the UI and the CLI
/// show as the value the accessory kept.
fn judge_name(s: &mut Snapshot) {
    let Some(status) = s.settings_status.get_mut(NAME_KEY) else {
        return;
    };
    if status != STATUS_VERIFYING && status != STATUS_UNREPORTED && status != STATUS_UNVERIFIED {
        return;
    }
    let matches = s
        .settings_requested
        .get(NAME_KEY)
        .and_then(serde_json::Value::as_str)
        .is_some_and(|want| want == s.device.name);
    *status = if matches {
        STATUS_CONFIRMED
    } else {
        STATUS_MISMATCH
    }
    .to_owned();
}

/// Give up on the pending readback without touching what the accessory did
/// report. The write itself was sent; only its confirmation is lost.
fn abandon_verification(s: &mut Snapshot) {
    for status in s.settings_status.values_mut() {
        if status == STATUS_VERIFYING {
            *status = STATUS_UNVERIFIED.to_owned();
        }
    }
    s.settings_verify = VERIFY_IDLE.to_owned();
    s.verify_reopen = false;
}

/// A link ended. A reopen this daemon asked for to read settings back is not
/// evidence of anything; any other loss ends the readback.
fn link_lost(s: &mut Snapshot, aux: &mut Aux) {
    aux.link_reports.clear();
    if !s.verify_reopen {
        abandon_verification(s);
    }
}

fn mutate(s: &mut Snapshot, update: Update, primary_bud: PrimaryBud, aux: &mut Aux) {
    match update {
        Update::Identity {
            address,
            name,
            model_id,
        } => {
            let changed_address = !s.device.address.is_empty() && s.device.address != address;
            let changed_device = changed_address
                || model_id
                    .as_ref()
                    .is_some_and(|id| !s.device.model_id.is_empty() && s.device.model_id != *id);
            if changed_device {
                s.settings = DeviceSettings::default();
                s.settings_report_seq.clear();
                s.settings_requested.clear();
                s.settings_status.clear();
                s.settings_verify = VERIFY_IDLE.to_owned();
                s.verify_reopen = false;
                aux.link_reports.clear();
                s.battery = crate::state::Battery {
                    stale: true,
                    ..Default::default()
                };
                s.ear = crate::state::Ear::default();
                s.lid = crate::state::Lid::Unknown;
                aux.last_ear = None;
            }
            if changed_address {
                // No property from the previous physical device may satisfy a
                // model gate or look like confirmation for the new one.
                s.device.name.clear();
                aux.metadata_name = None;
                s.device.model_id.clear();
                s.device.model = None;
                s.device.firmware = None;
                s.device.serial = None;
            }
            s.device.address = address;
            if let Some(n) = name {
                // BlueZ `Name`/`Alias` is a fallback only: it is read once per
                // connection and never reflects a rename until the next one.
                if aux.metadata_name.is_none() {
                    s.device.name = n;
                }
            }
            if let Some(id) = model_id {
                s.device.model = models::model_name(&id).map(ToOwned::to_owned);
                s.device.model_id = id;
            }
        }
        Update::AclConnected(connected) => {
            s.device.connected = connected;
            if !connected {
                // Keep the last battery values, dimmed, per the contract.
                s.device.aap_link = false;
                invalidate_aap(s);
                s.ear = crate::state::Ear::default();
                s.settings = DeviceSettings::default();
                link_lost(s, aux);
                aux.last_ear = None;
            }
        }
        Update::AapLink(up) => {
            s.device.aap_link = up;
            if up {
                s.daemon.source = Source::Aap;
                // `link_reports` is cleared when a link ends, not here: the
                // opening dump arrives before the socket is promoted.
            } else {
                invalidate_aap(s);
                s.settings = DeviceSettings::default();
                link_lost(s, aux);
            }
        }
        Update::Battery(entries) => {
            for e in &entries {
                match e.component {
                    BatteryComponent::Left => apply_cell(&mut s.battery.left, e),
                    BatteryComponent::Right => apply_cell(&mut s.battery.right, e),
                    BatteryComponent::Case => apply_cell(&mut s.battery.case, e),
                    BatteryComponent::Other(_) => {}
                }
            }
            // Presence is what resolves primary/secondary onto left/right, and
            // it can arrive after the ear packet that needs it. Redo the last
            // resolution now that we know which buds are out.
            if let Some((primary, secondary)) = aux.last_ear {
                assign_ear(s, primary, secondary, primary_bud);
            }
        }
        Update::BleBattery { address, entries } => {
            if s.device.address != address {
                return;
            }
            for e in entries {
                let cell = match e.component {
                    BatteryComponent::Left => &mut s.battery.left,
                    BatteryComponent::Right => &mut s.battery.right,
                    BatteryComponent::Case => &mut s.battery.case,
                    BatteryComponent::Other(_) => continue,
                };
                // BLE's encrypted percent is exact, but do not alternate
                // sources on every duplicate while AAP is actively reporting.
                if !e.present || e.level.is_none() || recent_aap(cell) {
                    continue;
                }
                apply_cell(cell, &e);
                cell.source = Source::Ble;
            }
        }
        Update::ExpireBle { before } => {
            let Ok(cutoff) = chrono::DateTime::parse_from_rfc3339(&before) else {
                return;
            };
            for cell in [
                &mut s.battery.left,
                &mut s.battery.right,
                &mut s.battery.case,
            ] {
                if cell.source == Source::Ble
                    && cell.fresh
                    && cell
                        .last_seen
                        .as_deref()
                        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                        .is_none_or(|t| t < cutoff)
                {
                    cell.fresh = false;
                    cell.present = false;
                    cell.charging = false;
                }
            }
        }
        Update::Ear { primary, secondary } => {
            aux.last_ear = Some((primary, secondary));
            assign_ear(s, primary, secondary, primary_bud);
        }
        Update::Metadata(md) => {
            if let Some(n) = md.name {
                // The device's own word on its name. Counted as a report for
                // this link so the readback can tell a fresh answer from a
                // name left over from an earlier one.
                aux.metadata_name = Some(n.clone());
                let counter = aux.link_reports.entry(NAME_KEY.to_owned()).or_default();
                *counter = counter.saturating_add(1);
                s.device.name = n;
                // Judged here rather than only at the end of the readback
                // window: metadata and the first battery packet arrive within
                // milliseconds of each other and either order is legal.
                judge_name(s);
            }
            if md.serial.is_some() {
                s.device.serial = md.serial;
            }
            if md.firmware.is_some() {
                s.device.firmware = md.firmware;
            }
            if s.device.model.is_none() {
                s.device.model = md.model;
            }
        }
        Update::NoiseControl(mode) => s.noise_control = NoiseControl::from(mode),
        Update::ConversationalAwareness(on) => s.conversational_awareness = Some(on),
        Update::AdaptiveLevel(level) => s.adaptive_level = Some(level.min(100)),
        Update::Setting(setting) => {
            let counter = s
                .settings_report_seq
                .entry(setting.key().to_owned())
                .or_default();
            *counter = counter.saturating_add(1);
            let link_counter = aux
                .link_reports
                .entry(setting.key().to_owned())
                .or_default();
            *link_counter = link_counter.saturating_add(1);
            apply_setting(&mut s.settings, setting);
        }
        Update::SettingRequested(setting) => {
            let key = setting.key().to_owned();
            s.settings_requested
                .insert(key.clone(), setting.value_json());
            s.settings_status.insert(key, STATUS_VERIFYING.to_owned());
            s.settings_verify = VERIFY_SCHEDULED.to_owned();
        }
        Update::RenameRequested(name) => {
            s.settings_requested
                .insert(NAME_KEY.to_owned(), serde_json::Value::String(name));
            s.settings_status
                .insert(NAME_KEY.to_owned(), STATUS_VERIFYING.to_owned());
            s.settings_verify = VERIFY_SCHEDULED.to_owned();
        }
        Update::VerifyReopening => {
            s.settings_verify = VERIFY_REOPENING.to_owned();
            s.verify_reopen = true;
        }
        Update::VerifyCompleted => {
            finish_verification(s, &aux.link_reports, aux.metadata_name.as_deref())
        }
        Update::VerifyFailed => abandon_verification(s),
        Update::Handoff(handoff) => s.handoff = handoff,
        Update::Link(activity) => aux.link = activity,
    }
    s.link = resolve_link(s.device.connected, aux.link.as_ref());
    s.battery.stale = ![&s.battery.left, &s.battery.right, &s.battery.case]
        .iter()
        .any(|c| c.fresh);
    s.daemon.source = if s.device.aap_link {
        Source::Aap
    } else if [&s.battery.left, &s.battery.right, &s.battery.case]
        .iter()
        .any(|c| c.fresh && c.source == Source::Ble)
    {
        Source::Ble
    } else {
        Source::None
    };
}

/// Fold the link facts into the published object. A live link always wins:
/// a sequence that has just succeeded may not have been retired yet, and the
/// widget must never see `reconnecting` while the AirPods are connected.
fn resolve_link(connected: bool, activity: Option<&LinkActivity>) -> Link {
    if connected {
        return Link {
            status: LinkStatus::Connected,
            reason: None,
            attempt: 0,
            since: None,
        };
    }
    match activity {
        Some(a) => Link {
            status: LinkStatus::Reconnecting,
            reason: Some(a.reason),
            attempt: a.attempt,
            since: Some(a.since.clone()),
        },
        None => Link::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_epoch_advances_only_for_identity_and_link_transitions() {
        let store = Store::new(Snapshot::initial("AA:BB:CC:DD:EE:01"), PrimaryBud::Auto);
        let start = store.connection_context();
        store.apply(Update::Battery(vec![entry(
            BatteryComponent::Left,
            20,
            false,
        )]));
        assert_eq!(store.connection_context(), start);

        store.apply(Update::AclConnected(true));
        let acl = store.connection_context();
        assert!(acl.epoch > start.epoch);
        assert_eq!(acl.address, start.address);

        store.apply(Update::AclConnected(true));
        assert_eq!(
            store.connection_context(),
            acl,
            "a no-op must not expire commands"
        );
        store.apply(Update::Identity {
            address: acl.address.clone(),
            name: Some("New display name".into()),
            model_id: None,
        });
        assert_eq!(
            store.connection_context(),
            acl,
            "display-name updates are not connection handoffs"
        );
        store.apply(Update::AapLink(false));
        assert!(
            store.connection_context().epoch > acl.epoch,
            "tentative opening failure invalidates queued actions even before promotion"
        );
        store.apply(Update::Identity {
            address: "AA:BB:CC:DD:EE:02".into(),
            name: None,
            model_id: None,
        });
        let next = store.connection_context();
        assert!(next.epoch > acl.epoch);
        assert_eq!(next.address, "AA:BB:CC:DD:EE:02");
    }

    #[test]
    fn opening_aap_does_not_revive_cached_or_disconnected_cells() {
        let store = Store::new(Snapshot::default(), PrimaryBud::Auto);
        store.apply(Update::Battery(vec![entry(
            BatteryComponent::Left,
            42,
            true,
        )]));
        store.apply(Update::AapLink(false));
        store.apply(Update::AapLink(true));
        let snapshot = store.snapshot();
        assert!(snapshot.battery.stale);
        assert!(!snapshot.battery.left.fresh);
        assert_eq!(snapshot.battery.left.level, Some(42));
        assert_eq!(snapshot.battery.left.last_known_charging, Some(true));
        store.apply(Update::Battery(vec![entry(
            BatteryComponent::Right,
            60,
            false,
        )]));
        assert!(store.snapshot().battery.right.fresh);
        assert!(!store.snapshot().battery.left.fresh);
    }

    #[test]
    fn ble_observations_are_identity_bound_independent_and_expire_to_charging_history() {
        let address = "AC:DE:48:00:11:22";
        let store = Store::new(Snapshot::initial(address), PrimaryBud::Auto);
        let update = |address: &str| Update::BleBattery {
            address: address.into(),
            entries: vec![entry(BatteryComponent::Case, 0, true)],
        };
        store.apply(update("AC:DE:48:00:11:23"));
        assert_eq!(store.snapshot().battery.case.level, None);
        store.apply(update(address));
        store.apply(Update::AclConnected(false));
        store.apply(Update::AapLink(false));
        let observed = store.snapshot();
        assert_eq!(observed.daemon.source, Source::Ble);
        assert!(!observed.device.connected);
        assert!(observed.battery.case.fresh && observed.battery.case.charging);
        assert_eq!(observed.battery.case.level, Some(0));
        store.apply(Update::ExpireBle {
            before: "2999-01-01T00:00:00Z".into(),
        });
        let expired = store.snapshot();
        assert!(expired.battery.stale);
        assert!(!expired.battery.case.fresh && !expired.battery.case.present);
        assert!(!expired.battery.case.charging);
        assert_eq!(expired.battery.case.last_known_charging, Some(true));
        assert_eq!(
            expired.battery.case.last_seen,
            observed.battery.case.last_seen
        );
    }

    #[test]
    fn recent_exact_aap_wins_but_case_absence_does_not_erase_new_ble() {
        let address = "AC:DE:48:00:11:22";
        let store = Store::new(Snapshot::initial(address), PrimaryBud::Auto);
        store.apply(Update::Battery(vec![entry(
            BatteryComponent::Left,
            52,
            false,
        )]));
        store.apply(Update::BleBattery {
            address: address.into(),
            entries: vec![
                entry(BatteryComponent::Left, 50, true),
                entry(BatteryComponent::Case, 80, true),
            ],
        });
        let snapshot = store.snapshot();
        assert_eq!(snapshot.battery.left.level, Some(52));
        assert_eq!(snapshot.battery.left.source, Source::Aap);
        assert_eq!(snapshot.battery.case.source, Source::Ble);
        store.apply(Update::Battery(vec![BatteryEntry {
            component: BatteryComponent::Case,
            level: None,
            charging: false,
            present: false,
        }]));
        assert!(store.snapshot().battery.case.fresh);
        store.apply(Update::Identity {
            address: "AC:DE:48:00:11:23".into(),
            name: None,
            model_id: None,
        });
        assert_eq!(store.snapshot().battery.case.level, None);
        assert!(store.snapshot().battery.stale);
    }

    #[test]
    fn identical_setting_reports_advance_but_battery_and_link_events_do_not() {
        let store = Store::new(Snapshot::default(), PrimaryBud::Auto);
        let report = Update::Setting(SettingCommand::Microphone(
            crate::settings::MicrophoneMode::Auto,
        ));
        store.apply(report.clone());
        store.apply(Update::Battery(vec![entry(
            BatteryComponent::Left,
            60,
            false,
        )]));
        store.apply(Update::AapLink(false));
        assert_eq!(store.snapshot().settings_report_seq["microphone"], 1);
        store.apply(report);
        assert_eq!(store.snapshot().settings_report_seq["microphone"], 2);
    }

    fn entry(component: BatteryComponent, level: u8, charging: bool) -> BatteryEntry {
        BatteryEntry {
            component,
            level: Some(level),
            charging,
            present: true,
        }
    }

    #[test]
    fn absent_cell_keeps_last_known_level() {
        let store = Store::new(Snapshot::initial(""), PrimaryBud::Left);
        store.apply(Update::Battery(vec![entry(
            BatteryComponent::Case,
            62,
            true,
        )]));
        let seen = store.snapshot().battery.case.last_seen.clone();
        assert!(seen.is_some());
        store.apply(Update::Battery(vec![BatteryEntry {
            component: BatteryComponent::Case,
            level: None,
            charging: false,
            present: false,
        }]));
        let case = store.snapshot().battery.case;
        assert_eq!(case.level, Some(62), "level survives the case dropping out");
        assert!(!case.present && !case.charging);
        assert_eq!(
            case.last_known_charging,
            Some(true),
            "historical charging survives the case dropping out"
        );
        assert_eq!(
            case.last_seen, seen,
            "last_seen is not bumped by an absent report"
        );
    }

    #[test]
    fn live_reports_replace_charging_history_independently_per_cell() {
        let store = Store::new(Snapshot::initial(""), PrimaryBud::Left);
        assert_eq!(store.snapshot().battery.case.last_known_charging, None);

        store.apply(Update::Battery(vec![
            entry(BatteryComponent::Left, 100, true),
            entry(BatteryComponent::Right, 75, false),
            entry(BatteryComponent::Case, 100, true),
        ]));
        let battery = store.snapshot().battery;
        assert_eq!(battery.left.last_known_charging, Some(true));
        assert_eq!(battery.right.last_known_charging, Some(false));
        assert_eq!(battery.case.last_known_charging, Some(true));

        store.apply(Update::Battery(vec![entry(
            BatteryComponent::Case,
            99,
            false,
        )]));
        let battery = store.snapshot().battery;
        assert_eq!(battery.case.last_known_charging, Some(false));
        assert_eq!(battery.left.last_known_charging, Some(true));
        assert_eq!(battery.right.last_known_charging, Some(false));
    }

    #[test]
    fn battery_updates_clear_stale_and_disconnect_keeps_values() {
        let store = Store::new(Snapshot::initial(""), PrimaryBud::Left);
        store.apply(Update::AclConnected(true));
        store.apply(Update::AapLink(true));
        store.apply(Update::Battery(vec![
            entry(BatteryComponent::Left, 87, false),
            entry(BatteryComponent::Case, 62, true),
        ]));
        let s = store.snapshot();
        assert!(!s.battery.stale);
        assert_eq!(s.battery.left.level, Some(87));
        assert!(s.battery.case.charging);
        assert_eq!(s.daemon.source, Source::Aap);

        store.apply(Update::AclConnected(false));
        let s = store.snapshot();
        assert!(s.battery.stale, "values are kept but marked stale");
        assert_eq!(s.battery.left.level, Some(87));
        assert!(!s.device.connected);
        assert!(!s.device.aap_link);
        assert_eq!(s.daemon.source, Source::None);
    }

    #[test]
    fn primary_bud_config_swaps_ear_mapping() {
        for (bud, expect_left) in [
            (PrimaryBud::Left, EarState::In),
            (PrimaryBud::Right, EarState::Out),
        ] {
            let store = Store::new(Snapshot::initial(""), bud);
            store.apply(Update::Ear {
                primary: EarState::In,
                secondary: EarState::Out,
            });
            assert_eq!(store.snapshot().ear.left, expect_left);
        }
    }

    fn absent(component: BatteryComponent) -> BatteryEntry {
        BatteryEntry {
            component,
            level: None,
            charging: false,
            present: false,
        }
    }

    #[test]
    fn auto_primary_follows_whichever_bud_is_out_of_the_case() {
        // The reported bug: right bud in the ear, left in the case. The primary
        // byte describes the right bud, but a left-pinned mapping put "in" on
        // the left and left the right reading Unknown, so a live bud rendered
        // dimmed with an "unknown" caption.
        let store = Store::new(Snapshot::initial(""), PrimaryBud::Auto);
        store.apply(Update::Battery(vec![
            entry(BatteryComponent::Right, 60, false),
            absent(BatteryComponent::Left),
        ]));
        store.apply(Update::Ear {
            primary: EarState::In,
            secondary: EarState::Unknown,
        });
        let ear = store.snapshot().ear;
        assert_eq!(ear.right, EarState::In, "the bud that is out is primary");
        assert_eq!(ear.left, EarState::Unknown);

        // Mirror case: left out, right away.
        let store = Store::new(Snapshot::initial(""), PrimaryBud::Auto);
        store.apply(Update::Battery(vec![
            entry(BatteryComponent::Left, 60, false),
            absent(BatteryComponent::Right),
        ]));
        store.apply(Update::Ear {
            primary: EarState::In,
            secondary: EarState::Unknown,
        });
        assert_eq!(store.snapshot().ear.left, EarState::In);
    }

    #[test]
    fn auto_primary_is_redone_when_presence_arrives_after_the_ear_packet() {
        // On connect the 0x0006 packet can beat the first battery packet, so
        // the resolution has to be redone rather than fixed at arrival.
        let store = Store::new(Snapshot::initial(""), PrimaryBud::Auto);
        store.apply(Update::Ear {
            primary: EarState::In,
            secondary: EarState::Unknown,
        });
        assert_eq!(
            store.snapshot().ear.left,
            EarState::In,
            "with no presence yet, the old left-first assumption stands"
        );
        store.apply(Update::Battery(vec![
            entry(BatteryComponent::Right, 60, false),
            absent(BatteryComponent::Left),
        ]));
        let ear = store.snapshot().ear;
        assert_eq!(ear.right, EarState::In, "corrected once presence is known");
        assert_eq!(ear.left, EarState::Unknown);
    }

    #[test]
    fn auto_primary_does_not_survive_a_disconnect() {
        let store = Store::new(Snapshot::initial(""), PrimaryBud::Auto);
        store.apply(Update::Battery(vec![
            entry(BatteryComponent::Right, 60, false),
            absent(BatteryComponent::Left),
        ]));
        store.apply(Update::Ear {
            primary: EarState::In,
            secondary: EarState::Unknown,
        });
        store.apply(Update::AclConnected(false));
        assert_eq!(store.snapshot().ear.right, EarState::Unknown);
        // A battery packet after the drop must not resurrect the old reading.
        store.apply(Update::Battery(vec![entry(
            BatteryComponent::Right,
            60,
            false,
        )]));
        assert_eq!(store.snapshot().ear.right, EarState::Unknown);
    }

    #[test]
    fn identity_fills_model_name() {
        let store = Store::new(Snapshot::initial(""), PrimaryBud::Left);
        store.apply(Update::Identity {
            address: "AC:DE:48:00:11:22".into(),
            name: Some("AirPods".into()),
            model_id: Some("201B".into()),
        });
        let s = store.snapshot();
        assert_eq!(s.device.model.as_deref(), Some("AirPods 4 (ANC)"));
        assert_eq!(s.device.model_id, "201B");
    }

    #[test]
    fn redundant_update_does_not_notify() {
        let store = Store::new(Snapshot::initial(""), PrimaryBud::Left);
        let mut rx = store.subscribe();
        store.apply(Update::AclConnected(true));
        assert!(rx.has_changed().unwrap());
        rx.borrow_and_update();
        store.apply(Update::AclConnected(true));
        assert!(
            !rx.has_changed().unwrap(),
            "no-op update must not wake the writer"
        );
    }

    #[test]
    fn settings_change_only_when_a_confirmed_update_is_applied() {
        use crate::settings::{MicrophoneMode, SettingCommand};

        let store = Store::new(Snapshot::initial(""), PrimaryBud::Left);
        assert_eq!(store.snapshot().settings.microphone, None);
        store.apply(Update::Setting(SettingCommand::Microphone(
            MicrophoneMode::Right,
        )));
        assert_eq!(
            store.snapshot().settings.microphone,
            Some(MicrophoneMode::Right)
        );
    }

    #[test]
    fn settings_clear_on_aap_loss_and_identity_change() {
        use crate::settings::{MicrophoneMode, SettingCommand};

        let store = Store::new(Snapshot::initial("AA:BB:CC:DD:EE:01"), PrimaryBud::Left);
        store.apply(Update::Setting(SettingCommand::Microphone(
            MicrophoneMode::Left,
        )));
        store.apply(Update::AapLink(false));
        assert_eq!(store.snapshot().settings, DeviceSettings::default());

        store.apply(Update::Identity {
            address: "AA:BB:CC:DD:EE:01".into(),
            name: Some("Old Pods".into()),
            model_id: Some("201B".into()),
        });
        store.apply(Update::Metadata(Metadata {
            name: Some("Old Pods".into()),
            model: Some("A3056".into()),
            manufacturer: Some("Apple Inc.".into()),
            serial: Some("OLD-SERIAL".into()),
            firmware: Some("7B21".into()),
        }));
        store.apply(Update::Setting(SettingCommand::Microphone(
            MicrophoneMode::Right,
        )));
        store.apply(Update::Identity {
            address: "AA:BB:CC:DD:EE:02".into(),
            name: None,
            model_id: None,
        });
        let snapshot = store.snapshot();
        assert_eq!(snapshot.settings, DeviceSettings::default());
        assert_eq!(snapshot.device.model_id, "");
        assert_eq!(snapshot.device.model, None);
        assert_eq!(snapshot.device.firmware, None);
        assert_eq!(snapshot.device.serial, None);
    }

    use crate::settings::{CallControls, HoldDuration, MicrophoneMode, PressSpeed, SettingCommand};

    /// Bring a store to "link open, one setting written, reopen under way".
    fn store_with_pending_write(command: SettingCommand) -> Arc<Store> {
        let store = Store::new(Snapshot::initial("AA:BB:CC:DD:EE:01"), PrimaryBud::Auto);
        store.apply(Update::AclConnected(true));
        store.apply(Update::AapLink(true));
        store.apply(Update::SettingRequested(command));
        store
    }

    fn status(store: &Store, key: &str) -> String {
        store
            .snapshot()
            .settings_status
            .get(key)
            .cloned()
            .unwrap_or_default()
    }

    /// Bring a store to "link open, rename written, reopen under way".
    fn store_with_pending_rename(name: &str) -> Arc<Store> {
        let store = Store::new(Snapshot::initial("AA:BB:CC:DD:EE:01"), PrimaryBud::Auto);
        store.apply(Update::AclConnected(true));
        store.apply(Update::AapLink(true));
        store.apply(Update::RenameRequested(name.to_owned()));
        store
    }

    fn metadata_named(name: &str) -> Metadata {
        Metadata {
            name: Some(name.to_owned()),
            ..Metadata::default()
        }
    }

    #[test]
    fn a_rename_the_accessory_reports_back_is_confirmed() {
        let store = store_with_pending_rename("Airpods00000000");
        let pending = store.snapshot();
        assert_eq!(pending.settings_status[NAME_KEY], STATUS_VERIFYING);
        assert_eq!(pending.settings_requested[NAME_KEY], "Airpods00000000");
        assert_eq!(pending.settings_verify, VERIFY_SCHEDULED);

        store.apply(Update::VerifyReopening);
        store.apply(Update::AapLink(false));
        assert_eq!(status(&store, NAME_KEY), STATUS_VERIFYING);

        store.apply(Update::AapLink(true));
        store.apply(Update::Metadata(metadata_named("Airpods00000000")));
        store.apply(Update::VerifyCompleted);
        let settled = store.snapshot();
        assert_eq!(settled.settings_status[NAME_KEY], STATUS_CONFIRMED);
        assert_eq!(settled.device.name, "Airpods00000000");
        assert_eq!(settled.settings_verify, VERIFY_IDLE);
    }

    #[test]
    fn a_rename_the_accessory_did_not_take_reports_the_name_it_kept() {
        let store = store_with_pending_rename("Auris Pods");
        store.apply(Update::VerifyReopening);
        store.apply(Update::AapLink(false));
        store.apply(Update::AapLink(true));
        store.apply(Update::Metadata(metadata_named("Old Pods")));
        store.apply(Update::VerifyCompleted);
        let settled = store.snapshot();
        assert_eq!(settled.settings_status[NAME_KEY], STATUS_MISMATCH);
        // The name the device reported is what the UI and the CLI show.
        assert_eq!(settled.device.name, "Old Pods");
    }

    #[test]
    fn a_rename_no_metadata_answered_is_not_claimed_as_success() {
        let store = store_with_pending_rename("Auris Pods");
        store.apply(Update::VerifyReopening);
        store.apply(Update::AapLink(false));
        store.apply(Update::AapLink(true));
        store.apply(Update::VerifyCompleted);
        assert_eq!(status(&store, NAME_KEY), STATUS_UNREPORTED);

        // A later link that does report the name still settles it.
        store.apply(Update::Metadata(metadata_named("Auris Pods")));
        assert_eq!(status(&store, NAME_KEY), STATUS_CONFIRMED);
    }

    #[test]
    fn the_accessory_name_outranks_the_bluez_name() {
        let store = Store::new(Snapshot::initial("AA:BB:CC:DD:EE:01"), PrimaryBud::Auto);
        // BlueZ `Name` or `Alias`, whichever changed, is the fallback.
        store.apply(Update::Identity {
            address: "AA:BB:CC:DD:EE:01".into(),
            name: Some("Old Pods".into()),
            model_id: None,
        });
        assert_eq!(store.snapshot().device.name, "Old Pods");

        store.apply(Update::Metadata(metadata_named("Airpods00000000")));
        assert_eq!(store.snapshot().device.name, "Airpods00000000");

        // BlueZ only re-reads the remote name when a link opens, so a stale
        // one must never overwrite the device's own word.
        store.apply(Update::Identity {
            address: "AA:BB:CC:DD:EE:01".into(),
            name: Some("Old Pods".into()),
            model_id: None,
        });
        assert_eq!(store.snapshot().device.name, "Airpods00000000");
    }

    #[test]
    fn a_write_is_verifying_until_a_fresh_dump_reports_it() {
        let store = store_with_pending_write(SettingCommand::PressSpeed(PressSpeed::Slower));
        let pending = store.snapshot();
        assert_eq!(pending.settings_status["press_speed"], STATUS_VERIFYING);
        assert_eq!(pending.settings_requested["press_speed"], "slower");
        assert_eq!(pending.settings_verify, VERIFY_SCHEDULED);
        assert!(!pending.verify_reopen);

        // The reopen this daemon asks for is not evidence of a lost readback.
        store.apply(Update::VerifyReopening);
        store.apply(Update::AapLink(false));
        let reopening = store.snapshot();
        assert!(reopening.verify_reopen);
        assert_eq!(reopening.settings_verify, VERIFY_REOPENING);
        assert_eq!(reopening.settings_status["press_speed"], STATUS_VERIFYING);
        assert_eq!(reopening.settings_requested["press_speed"], "slower");

        store.apply(Update::AapLink(true));
        store.apply(Update::Setting(SettingCommand::PressSpeed(
            PressSpeed::Slower,
        )));
        store.apply(Update::VerifyCompleted);
        let done = store.snapshot();
        assert_eq!(done.settings_status["press_speed"], STATUS_CONFIRMED);
        assert_eq!(done.settings_verify, VERIFY_IDLE);
        assert!(!done.verify_reopen);
        assert_eq!(done.settings.press_speed, Some(PressSpeed::Slower));
    }

    #[test]
    fn a_different_reported_value_is_a_mismatch_and_the_device_wins() {
        let store = store_with_pending_write(SettingCommand::PressSpeed(PressSpeed::Slowest));
        store.apply(Update::VerifyReopening);
        store.apply(Update::AapLink(false));
        store.apply(Update::AapLink(true));
        store.apply(Update::Setting(SettingCommand::PressSpeed(
            PressSpeed::Default,
        )));
        store.apply(Update::VerifyCompleted);
        let snapshot = store.snapshot();
        assert_eq!(snapshot.settings_status["press_speed"], STATUS_MISMATCH);
        assert_eq!(snapshot.settings.press_speed, Some(PressSpeed::Default));
        assert_eq!(snapshot.settings_requested["press_speed"], "slowest");
    }

    #[test]
    fn keys_this_model_never_reports_end_as_unreported() {
        // Microphone and the listening-mode cycle are stored by AirPods 4
        // (ANC) but are absent from every dump it sends.
        let store = store_with_pending_write(SettingCommand::Microphone(MicrophoneMode::Left));
        store.apply(Update::VerifyReopening);
        store.apply(Update::AapLink(false));
        store.apply(Update::AapLink(true));
        store.apply(Update::Setting(SettingCommand::PressSpeed(
            PressSpeed::Slower,
        )));
        store.apply(Update::VerifyCompleted);
        assert_eq!(status(&store, "microphone"), STATUS_UNREPORTED);
        assert_eq!(store.snapshot().settings_verify, VERIFY_IDLE);
    }

    #[test]
    fn a_report_from_an_earlier_link_cannot_confirm_a_later_write() {
        let store = Store::new(Snapshot::initial("AA:BB:CC:DD:EE:01"), PrimaryBud::Auto);
        store.apply(Update::AclConnected(true));
        store.apply(Update::AapLink(true));
        store.apply(Update::Setting(SettingCommand::PressSpeed(
            PressSpeed::Slower,
        )));
        store.apply(Update::SettingRequested(SettingCommand::PressSpeed(
            PressSpeed::Slower,
        )));
        store.apply(Update::VerifyReopening);
        store.apply(Update::AapLink(false));
        store.apply(Update::AapLink(true));
        // The lifetime counter is still non-zero, but this link said nothing.
        store.apply(Update::VerifyCompleted);
        assert_eq!(store.snapshot().settings_report_seq["press_speed"], 1);
        assert_eq!(status(&store, "press_speed"), STATUS_UNREPORTED);
    }

    #[test]
    fn a_link_loss_outside_the_reopen_leaves_the_write_unverified() {
        let store = store_with_pending_write(SettingCommand::PersonalizedVolume(true));
        store.apply(Update::AclConnected(false));
        let snapshot = store.snapshot();
        assert_eq!(
            snapshot.settings_status["personalized_volume"],
            STATUS_UNVERIFIED
        );
        assert_eq!(snapshot.settings_requested["personalized_volume"], true);
        assert_eq!(snapshot.settings_verify, VERIFY_IDLE);
        assert!(!snapshot.verify_reopen);
    }

    #[test]
    fn an_expired_readback_budget_leaves_the_write_unverified() {
        let store = store_with_pending_write(SettingCommand::HoldDuration(HoldDuration::Shorter));
        store.apply(Update::VerifyReopening);
        store.apply(Update::VerifyFailed);
        let snapshot = store.snapshot();
        assert_eq!(snapshot.settings_status["hold_duration"], STATUS_UNVERIFIED);
        assert_eq!(snapshot.settings_verify, VERIFY_IDLE);
        assert!(!snapshot.verify_reopen);
    }

    #[test]
    fn a_later_natural_dump_settles_an_unverified_write() {
        let store = store_with_pending_write(SettingCommand::HoldDuration(HoldDuration::Shortest));
        store.apply(Update::AapLink(false));
        assert_eq!(status(&store, "hold_duration"), STATUS_UNVERIFIED);

        store.apply(Update::AapLink(true));
        store.apply(Update::Setting(SettingCommand::HoldDuration(
            HoldDuration::Shortest,
        )));
        store.apply(Update::VerifyCompleted);
        assert_eq!(status(&store, "hold_duration"), STATUS_CONFIRMED);

        // A dump that says nothing about the key cannot undo that verdict.
        store.apply(Update::AapLink(false));
        store.apply(Update::AapLink(true));
        store.apply(Update::VerifyCompleted);
        assert_eq!(status(&store, "hold_duration"), STATUS_CONFIRMED);
    }

    #[test]
    fn an_unreported_key_is_rechecked_by_every_later_dump() {
        let store = store_with_pending_write(SettingCommand::CallControls(
            CallControls::HangupOnceMuteTwice,
        ));
        store.apply(Update::VerifyCompleted);
        assert_eq!(status(&store, "call_controls"), STATUS_UNREPORTED);

        store.apply(Update::AapLink(false));
        store.apply(Update::AapLink(true));
        store.apply(Update::Setting(SettingCommand::CallControls(
            CallControls::MuteOnceHangupTwice,
        )));
        store.apply(Update::VerifyCompleted);
        assert_eq!(status(&store, "call_controls"), STATUS_MISMATCH);
    }

    #[test]
    fn a_completed_readback_with_nothing_pending_changes_nothing() {
        let store = Store::new(Snapshot::initial("AA:BB:CC:DD:EE:01"), PrimaryBud::Auto);
        store.apply(Update::AapLink(true));
        let before = store.snapshot();
        store.apply(Update::VerifyCompleted);
        let after = store.snapshot();
        assert_eq!(before.updated_at, after.updated_at);
        assert!(after.settings_status.is_empty());
    }

    #[test]
    fn another_device_drops_every_request_from_the_previous_one() {
        let store = store_with_pending_write(SettingCommand::PressSpeed(PressSpeed::Slower));
        store.apply(Update::Identity {
            address: "AA:BB:CC:DD:EE:02".into(),
            name: None,
            model_id: None,
        });
        let snapshot = store.snapshot();
        assert!(snapshot.settings_requested.is_empty());
        assert!(snapshot.settings_status.is_empty());
        assert_eq!(snapshot.settings_verify, VERIFY_IDLE);
    }
}

#[cfg(test)]
mod link_tests {
    use super::*;

    fn reconnecting(reason: LinkReason, attempt: u32) -> LinkActivity {
        LinkActivity {
            reason,
            attempt,
            since: "2026-09-16T02:01:47Z".to_owned(),
        }
    }

    #[test]
    fn a_scheduled_rejoin_publishes_reconnecting_and_link_up_clears_it() {
        let store = Store::new(Snapshot::initial("AA:BB:CC:DD:EE:01"), PrimaryBud::Auto);
        assert_eq!(store.snapshot().link.status, LinkStatus::Disconnected);

        store.apply(Update::Link(Some(reconnecting(LinkReason::TakenOver, 0))));
        let link = store.snapshot().link;
        assert_eq!(link.status, LinkStatus::Reconnecting);
        assert_eq!(link.reason, Some(LinkReason::TakenOver));
        assert_eq!(link.attempt, 0);
        assert_eq!(link.since.as_deref(), Some("2026-09-16T02:01:47Z"));

        // The connect starts: only the attempt counter moves.
        store.apply(Update::Link(Some(reconnecting(LinkReason::TakenOver, 1))));
        assert_eq!(store.snapshot().link.attempt, 1);

        // BlueZ reports the link before the session retires the sequence.
        store.apply(Update::AclConnected(true));
        let link = store.snapshot().link;
        assert_eq!(link.status, LinkStatus::Connected);
        assert!(link.reason.is_none());
        assert!(link.since.is_none());
        assert_eq!(link.attempt, 0);
    }

    #[test]
    fn giving_up_leaves_a_plain_disconnected_link() {
        let store = Store::new(Snapshot::initial("AA:BB:CC:DD:EE:01"), PrimaryBud::Auto);
        store.apply(Update::AclConnected(true));
        store.apply(Update::AclConnected(false));
        store.apply(Update::Link(Some(reconnecting(LinkReason::LinkLost, 3))));
        assert_eq!(store.snapshot().link.status, LinkStatus::Reconnecting);
        store.apply(Update::Link(None));
        assert_eq!(store.snapshot().link, Link::default());
    }

    #[test]
    fn a_connected_link_never_reports_a_reconnect() {
        let store = Store::new(Snapshot::initial("AA:BB:CC:DD:EE:01"), PrimaryBud::Auto);
        store.apply(Update::AclConnected(true));
        store.apply(Update::Link(Some(reconnecting(LinkReason::AutoConnect, 2))));
        assert_eq!(store.snapshot().link.status, LinkStatus::Connected);
        // The sequence survives in the bookkeeping: the drop republishes it.
        store.apply(Update::AclConnected(false));
        let link = store.snapshot().link;
        assert_eq!(link.status, LinkStatus::Reconnecting);
        assert_eq!(link.reason, Some(LinkReason::AutoConnect));
        assert_eq!(link.attempt, 2);
    }
}
