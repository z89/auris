//! The single source of truth for the published snapshot.
//!
//! Everything that learns something applies an [`Update`]; the writer task
//! watches for changes. Updates that change nothing do not notify, so an
//! accessory that repeats the same battery packet every few seconds does not
//! cause a file write.

use std::sync::Arc;

use tokio::sync::watch;

use crate::{
    aap::codec::{BatteryComponent, BatteryEntry, Metadata},
    config::PrimaryBud,
    models,
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
}

/// Holds the current [`Snapshot`] and broadcasts changes.
#[derive(Debug)]
pub struct Store {
    tx: watch::Sender<Snapshot>,
    primary_bud: PrimaryBud,
}

impl Store {
    /// Create a store seeded with `initial`.
    pub fn new(initial: Snapshot, primary_bud: PrimaryBud) -> Arc<Self> {
        Arc::new(Self {
            tx: watch::Sender::new(initial),
            primary_bud,
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

    /// Apply an update, notifying watchers only if something actually changed.
    pub fn apply(&self, update: Update) {
        self.tx.send_if_modified(|s| {
            let before = s.clone();
            mutate(s, update, self.primary_bud);
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
        cell.level = e.level;
        cell.charging = e.charging;
        cell.present = true;
        cell.last_seen = Some(crate::now_rfc3339());
    } else {
        // The buds only relay the case level while they sit in it. Keep the
        // last reading so the panel can show it dimmed with its age.
        cell.charging = false;
        cell.present = false;
    }
}

fn mutate(s: &mut Snapshot, update: Update, primary_bud: PrimaryBud) {
    match update {
        Update::Identity {
            address,
            name,
            model_id,
        } => {
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
                s.daemon.source = Source::None;
                s.battery.stale = true;
                s.ear = crate::state::Ear::default();
            }
        }
        Update::AapLink(up) => {
            s.device.aap_link = up;
            if up {
                s.daemon.source = Source::Aap;
                s.battery.stale = false;
            } else {
                s.daemon.source = Source::None;
                s.battery.stale = true;
            }
        }
        Update::Battery(entries) => {
            s.battery.stale = false;
            for e in &entries {
                match e.component {
                    BatteryComponent::Left => apply_cell(&mut s.battery.left, e),
                    BatteryComponent::Right => apply_cell(&mut s.battery.right, e),
                    BatteryComponent::Case => apply_cell(&mut s.battery.case, e),
                    BatteryComponent::Other(_) => {}
                }
            }
        }
        Update::Ear { primary, secondary } => match primary_bud {
            PrimaryBud::Left => {
                s.ear.left = primary;
                s.ear.right = secondary;
            }
            PrimaryBud::Right => {
                s.ear.right = primary;
                s.ear.left = secondary;
            }
        },
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            case.last_seen, seen,
            "last_seen is not bumped by an absent report"
        );
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
}
