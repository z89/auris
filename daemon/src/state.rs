//! `Snapshot` is the frozen state.json schema v1. The field names and the
//! string values of every enum here are part of the contract with the plugin;
//! `serde_round_trip_matches_contract_keys` guards them.

use serde::{Deserialize, Serialize};

/// state.json schema version.
pub const SCHEMA_VERSION: u32 = 1;

/// Daemon version reported in `daemon.version`.
pub const DAEMON_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Where the currently reported data came from.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Apple Accessory Protocol over an L2CAP link.
    Aap,
    /// Passive BLE proximity-pairing adverts (v0.2).
    Ble,
    /// Nothing is connected; values are last-known.
    #[default]
    None,
}

/// In-ear detection state of one bud.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EarState {
    /// In the ear.
    In,
    /// Out of the ear.
    Out,
    /// In the charging case.
    Case,
    /// Not reported yet.
    #[default]
    Unknown,
}

/// Case lid state. Only ever known from BLE adverts.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Lid {
    /// Lid open.
    Open,
    /// Lid closed.
    Closed,
    /// Not reported.
    #[default]
    Unknown,
}

/// Noise control state as published in state.json.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NoiseControl {
    /// Off.
    Off,
    /// Active noise cancellation.
    Anc,
    /// Transparency.
    Transparency,
    /// Adaptive.
    Adaptive,
    /// Not reported yet.
    #[default]
    Unknown,
}

/// Noise control mode as *commanded*. Same wire values minus `unknown`.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NoiseControlMode {
    /// Off.
    Off,
    /// Active noise cancellation.
    Anc,
    /// Transparency.
    Transparency,
    /// Adaptive.
    Adaptive,
}

impl NoiseControlMode {
    /// Wire byte for the 0x0D control identifier.
    pub const fn to_wire(self) -> u8 {
        match self {
            Self::Off => 0x01,
            Self::Anc => 0x02,
            Self::Transparency => 0x03,
            Self::Adaptive => 0x04,
        }
    }

    /// Parse the wire byte; unknown values yield `None`.
    pub const fn from_wire(v: u8) -> Option<Self> {
        match v {
            0x01 => Some(Self::Off),
            0x02 => Some(Self::Anc),
            0x03 => Some(Self::Transparency),
            0x04 => Some(Self::Adaptive),
            _ => None,
        }
    }
}

impl From<NoiseControlMode> for NoiseControl {
    fn from(m: NoiseControlMode) -> Self {
        match m {
            NoiseControlMode::Off => Self::Off,
            NoiseControlMode::Anc => Self::Anc,
            NoiseControlMode::Transparency => Self::Transparency,
            NoiseControlMode::Adaptive => Self::Adaptive,
        }
    }
}

/// `daemon` object.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Daemon {
    /// Daemon version string.
    pub version: String,
    /// Data source currently in use.
    pub source: Source,
}

impl Default for Daemon {
    fn default() -> Self {
        Self {
            version: DAEMON_VERSION.to_owned(),
            source: Source::None,
        }
    }
}

/// `device` object.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct DeviceInfo {
    /// BD_ADDR in `XX:XX:XX:XX:XX:XX` form, empty until a device is picked.
    pub address: String,
    /// Bluetooth name.
    pub name: String,
    /// Uppercase hex product id from the DID modalias or a BLE advert.
    pub model_id: String,
    /// Human model name from the lookup table, `null` when unrecognised.
    pub model: Option<String>,
    /// Firmware string from the metadata packet.
    pub firmware: Option<String>,
    /// Serial from the metadata packet.
    pub serial: Option<String>,
    /// BlueZ `Connected` for the classic link.
    pub connected: bool,
    /// Whether the AAP socket is currently open.
    pub aap_link: bool,
}

/// One battery cell.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Cell {
    /// 0-100, or `null` when never seen. When `present` is false this is the
    /// last level the component reported, so a case that dropped out of range
    /// can still be shown dimmed.
    pub level: Option<u8>,
    /// Whether the cell is charging. Always false when not present.
    pub charging: bool,
    /// Whether the component is reporting right now.
    pub present: bool,
    /// RFC3339 time of the last live reading, `null` if never seen.
    #[serde(default)]
    pub last_seen: Option<String>,
}

/// `battery` object.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Battery {
    /// Values are last-known rather than live.
    pub stale: bool,
    /// Left bud.
    pub left: Cell,
    /// Right bud.
    pub right: Cell,
    /// Charging case.
    pub case: Cell,
}

/// `ear` object.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ear {
    /// Left bud.
    pub left: EarState,
    /// Right bud.
    pub right: EarState,
}

/// The whole state.json document.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// Schema version, always [`SCHEMA_VERSION`].
    pub schema: u32,
    /// RFC3339 timestamp with local offset.
    pub updated_at: String,
    /// Daemon metadata.
    pub daemon: Daemon,
    /// Device identity and link state.
    pub device: DeviceInfo,
    /// Battery levels.
    pub battery: Battery,
    /// In-ear detection.
    pub ear: Ear,
    /// Case lid (BLE only).
    pub lid: Lid,
    /// Current noise control mode.
    pub noise_control: NoiseControl,
    /// Conversational awareness, `null` when unknown.
    pub conversational_awareness: Option<bool>,
    /// Adaptive transparency level, `null` when unknown.
    pub adaptive_level: Option<u8>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            schema: SCHEMA_VERSION,
            updated_at: crate::now_rfc3339(),
            daemon: Daemon::default(),
            device: DeviceInfo::default(),
            battery: Battery {
                stale: true,
                ..Battery::default()
            },
            ear: Ear::default(),
            lid: Lid::Unknown,
            noise_control: NoiseControl::Unknown,
            conversational_awareness: None,
            adaptive_level: None,
        }
    }
}

impl Snapshot {
    /// The document written at startup, before anything is known: the plugin
    /// must always find a readable file.
    pub fn initial(address: &str) -> Self {
        Self {
            device: DeviceInfo {
                address: address.to_owned(),
                ..DeviceInfo::default()
            },
            ..Self::default()
        }
    }

    /// A fully populated example, printed by `aurisd --dump-schema`.
    pub fn example() -> Self {
        let now = crate::now_rfc3339();
        Self {
            schema: SCHEMA_VERSION,
            updated_at: crate::now_rfc3339(),
            daemon: Daemon {
                version: DAEMON_VERSION.to_owned(),
                source: Source::Aap,
            },
            device: DeviceInfo {
                address: "AC:DE:48:00:11:22".to_owned(),
                name: "AirPods".to_owned(),
                model_id: "201B".to_owned(),
                model: Some("AirPods 4 (ANC)".to_owned()),
                firmware: Some("7B21".to_owned()),
                serial: Some("H4H200".to_owned()),
                connected: true,
                aap_link: true,
            },
            battery: Battery {
                stale: false,
                left: Cell {
                    level: Some(87),
                    charging: false,
                    present: true,
                    last_seen: Some(now.clone()),
                },
                right: Cell {
                    level: Some(85),
                    charging: false,
                    present: true,
                    last_seen: Some(now.clone()),
                },
                case: Cell {
                    level: Some(62),
                    charging: true,
                    present: true,
                    last_seen: Some(now),
                },
            },
            ear: Ear {
                left: EarState::In,
                right: EarState::Out,
            },
            lid: Lid::Unknown,
            noise_control: NoiseControl::Anc,
            conversational_awareness: Some(false),
            adaptive_level: Some(50),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Guards every key name and enum spelling in the frozen schema.
    #[test]
    fn serde_round_trip_matches_contract_keys() {
        let snap = Snapshot::example();
        let json = serde_json::to_value(&snap).unwrap();

        let obj = json.as_object().unwrap();
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "adaptive_level",
                "battery",
                "conversational_awareness",
                "daemon",
                "device",
                "ear",
                "lid",
                "noise_control",
                "schema",
                "updated_at",
            ]
        );

        let mut daemon_keys: Vec<&str> = obj["daemon"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        daemon_keys.sort_unstable();
        assert_eq!(daemon_keys, ["source", "version"]);

        let mut device_keys: Vec<&str> = obj["device"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        device_keys.sort_unstable();
        assert_eq!(
            device_keys,
            [
                "aap_link",
                "address",
                "connected",
                "firmware",
                "model",
                "model_id",
                "name",
                "serial"
            ]
        );

        let mut battery_keys: Vec<&str> = obj["battery"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        battery_keys.sort_unstable();
        assert_eq!(battery_keys, ["case", "left", "right", "stale"]);

        let mut cell_keys: Vec<&str> = obj["battery"]["left"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        cell_keys.sort_unstable();
        assert_eq!(cell_keys, ["charging", "last_seen", "level", "present"]);

        let mut ear_keys: Vec<&str> = obj["ear"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        ear_keys.sort_unstable();
        assert_eq!(ear_keys, ["left", "right"]);

        assert_eq!(json["schema"], 1);
        assert_eq!(json["daemon"]["source"], "aap");
        assert_eq!(json["device"]["model_id"], "201B");
        assert_eq!(json["battery"]["left"]["level"], 87);
        assert_eq!(json["ear"]["left"], "in");
        assert_eq!(json["ear"]["right"], "out");
        assert_eq!(json["noise_control"], "anc");
        assert_eq!(json["lid"], "unknown");

        let back: Snapshot = serde_json::from_value(json).unwrap();
        assert_eq!(back, snap);
    }

    #[test]
    fn null_values_serialise_as_json_null() {
        let snap = Snapshot::initial("AC:DE:48:00:11:22");
        let json = serde_json::to_value(&snap).unwrap();
        assert!(json["battery"]["left"]["level"].is_null());
        assert!(json["device"]["model"].is_null());
        assert!(json["conversational_awareness"].is_null());
        assert!(json["adaptive_level"].is_null());
        assert_eq!(json["battery"]["stale"], true);
        assert_eq!(json["device"]["connected"], false);
        assert_eq!(json["daemon"]["source"], "none");
    }

    #[test]
    fn noise_control_wire_values() {
        assert_eq!(NoiseControlMode::Off.to_wire(), 0x01);
        assert_eq!(NoiseControlMode::Anc.to_wire(), 0x02);
        assert_eq!(NoiseControlMode::Transparency.to_wire(), 0x03);
        assert_eq!(NoiseControlMode::Adaptive.to_wire(), 0x04);
        assert_eq!(
            NoiseControlMode::from_wire(0x02),
            Some(NoiseControlMode::Anc)
        );
        assert_eq!(NoiseControlMode::from_wire(0x09), None);
    }
}
