//! The single source of truth for the published snapshot.
//!
//! Everything that learns something applies an [`Update`]; the writer task
//! watches for changes. Updates that change nothing do not notify, so an
//! accessory that repeats the same battery packet every few seconds does not
//! cause a file write.

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};

use tokio::sync::watch;

use crate::{
    aap::codec::{BatteryComponent, BatteryEntry, Metadata},
    config::PrimaryBud,
    models,
    settings::{DeviceSettings, SettingCommand},
    state::{Cell, EarState, NoiseControl, NoiseControlMode, Snapshot, Source},
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
    /// The last 0x0006 payload exactly as it came off the wire. Which bud the
    /// primary byte describes depends on which buds are out of the case, and
    /// that can arrive after the ear packet, so the resolution is redone on
    /// every battery update from these raw bytes.
    last_ear: Mutex<Option<(EarState, EarState)>>,
}

impl Store {
    /// Create a store seeded with `initial`.
    pub fn new(initial: Snapshot, primary_bud: PrimaryBud) -> Arc<Self> {
        Arc::new(Self {
            tx: watch::Sender::new(initial),
            primary_bud,
            connection_epoch: AtomicU64::new(0),
            last_ear: Mutex::new(None),
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
        let mut last_ear = self
            .last_ear
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.tx.send_if_modified(|s| {
            let before = s.clone();
            // An unsuccessful tentative opening may never have set aap_link
            // true. Its teardown still invalidates commands queued behind it.
            let link_ended = matches!(&update, Update::AapLink(false));
            mutate(s, update, self.primary_bud, &mut last_ear);
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

fn mutate(
    s: &mut Snapshot,
    update: Update,
    primary_bud: PrimaryBud,
    last_ear: &mut Option<(EarState, EarState)>,
) {
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
                s.battery = crate::state::Battery {
                    stale: true,
                    ..Default::default()
                };
                s.ear = crate::state::Ear::default();
                s.lid = crate::state::Lid::Unknown;
                *last_ear = None;
            }
            if changed_address {
                // No property from the previous physical device may satisfy a
                // model gate or look like confirmation for the new one.
                s.device.name.clear();
                s.device.model_id.clear();
                s.device.model = None;
                s.device.firmware = None;
                s.device.serial = None;
            }
            s.device.address = address;
            if let Some(n) = name {
                s.device.name = n;
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
                *last_ear = None;
            }
        }
        Update::AapLink(up) => {
            s.device.aap_link = up;
            if up {
                s.daemon.source = Source::Aap;
            } else {
                invalidate_aap(s);
                s.settings = DeviceSettings::default();
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
            if let Some((primary, secondary)) = *last_ear {
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
            *last_ear = Some((primary, secondary));
            assign_ear(s, primary, secondary, primary_bud);
        }
        Update::Metadata(md) => {
            if let Some(n) = md.name {
                s.device.name = n;
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
            apply_setting(&mut s.settings, setting);
        }
    }
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
}
