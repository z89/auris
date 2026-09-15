//! `Snapshot` is the frozen state.json schema v2. The field names and the
//! string values of every enum here are part of the contract with the plugin;
//! `serde_round_trip_matches_contract_keys` guards them.

use serde::{Deserialize, Serialize};

use crate::settings::{CallControls, DeviceSettings, HoldDuration, MicrophoneMode, PressSpeed};

/// state.json schema version. Bumped to 2 by the additive `link` object;
/// every schema-1 field kept its name and meaning.
pub const SCHEMA_VERSION: u32 = 2;

/// Daemon version reported in `daemon.version`.
pub const DAEMON_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Where the currently reported data came from.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Apple Accessory Protocol over an L2CAP link.
    Aap,
    /// Identity-matched BLE proximity adverts (opt-in discovery).
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
    /// Source of the most recent retained measurement, not audio ownership.
    #[serde(default)]
    pub source: Source,
    /// This cell has a current observation. A link opening alone is not one.
    #[serde(default)]
    pub fresh: bool,
    /// 0-100, or `null` when never seen. When `present` is false this is the
    /// last level the component reported, so a case that dropped out of range
    /// can still be shown dimmed.
    pub level: Option<u8>,
    /// Whether the cell is charging. Always false when not present.
    pub charging: bool,
    /// Charging state from the last live reading, including for absent cells.
    /// `null` means no live charging state has ever been observed.
    #[serde(default)]
    pub last_known_charging: Option<bool>,
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

/// Current value of [`Snapshot::settings_api`]. Version 2 added the
/// requested/status/verify fields that describe a settings readback. Version 3
/// put the accessory name under [`NAME_KEY`] in those same maps, so a rename is
/// verified and reported exactly like any other setting.
pub const SETTINGS_API: u32 = 3;

/// Key used for the accessory name inside `settings_requested` and
/// `settings_status`. It is not a [`crate::settings::SettingCommand`] key: the
/// requested value is the name that was written, and the reported value is
/// `device.name` as the accessory itself gave it in its 0x1D metadata.
pub const NAME_KEY: &str = "name";

/// `settings_status` value: written, readback in progress.
pub const STATUS_VERIFYING: &str = "verifying";
/// `settings_status` value: a fresh dump reported the requested value.
pub const STATUS_CONFIRMED: &str = "confirmed";
/// `settings_status` value: a fresh dump reported a different value.
pub const STATUS_MISMATCH: &str = "mismatch";
/// `settings_status` value: readback ran, but this model never reports the key.
pub const STATUS_UNREPORTED: &str = "unreported";
/// `settings_status` value: the readback could not run.
pub const STATUS_UNVERIFIED: &str = "unverified";

/// `settings_verify` value: no readback is pending.
pub const VERIFY_IDLE: &str = "idle";
/// `settings_verify` value: a write is waiting out the debounce.
pub const VERIFY_SCHEDULED: &str = "scheduled";
/// `settings_verify` value: the AAP link is being reopened for readback.
pub const VERIFY_REOPENING: &str = "reopening";

fn verify_idle() -> String {
    VERIFY_IDLE.to_owned()
}

/// Who the AirPods say owns their audio connection.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HandoffOwner {
    /// This host.
    Local,
    /// Another host (Mac, iPhone, iPad).
    Other,
    /// Not reported since the AAP link opened.
    #[default]
    Unknown,
}

/// What the host in an audio-source report is doing.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AudioSourceStatus {
    /// Nothing routed.
    Idle,
    /// A call.
    Call,
    /// Media playback.
    Media,
}

/// The host the AirPods currently route audio for.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct HandoffAudioSource {
    /// `AA:BB:CC:DD:EE:FF`.
    pub address: String,
    /// Whether it is this machine's adapter.
    pub is_local: bool,
    /// Idle, call or media.
    pub state: AudioSourceStatus,
}

/// One host in the AirPods' connected-devices list.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct HandoffDevice {
    /// `AA:BB:CC:DD:EE:FF`.
    pub address: String,
    /// Whether it is this machine's adapter.
    pub is_local: bool,
}

/// Kind of the most recent handoff action.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HandoffEventKind {
    /// Audio released because another host is playing, or on `yield`.
    Yielded,
    /// Audio claimed on local playback or `take_over`.
    TookOver,
    /// Audio released because another host asked for it over smart routing.
    YieldRequested,
}

/// The most recent handoff action.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct HandoffEvent {
    /// What happened.
    pub kind: HandoffEventKind,
    /// RFC3339 timestamp.
    pub at: String,
    /// The other host involved, when known.
    pub peer: Option<String>,
}

/// Apple multi-host switching state. Always present in state.json.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct Handoff {
    /// `[handoff] enabled`: automatic yield and take-over.
    pub enabled: bool,
    /// `[handoff] take_over_on_play`.
    pub take_over_on_play: bool,
    /// Local adapter Modalias names Apple (`bluetooth:v004C...`); `null` when
    /// unreadable.
    pub apple_host_id: Option<bool>,
    /// Ownership as last reported or claimed.
    pub owner: HandoffOwner,
    /// Last audio-source report, `null` before one arrives on this link.
    pub audio_source: Option<HandoffAudioSource>,
    /// Last connected-devices report.
    pub devices: Vec<HandoffDevice>,
    /// Most recent yield or take-over.
    pub last_event: Option<HandoffEvent>,
}

impl Default for Handoff {
    fn default() -> Self {
        Self {
            enabled: false,
            take_over_on_play: true,
            apple_host_id: None,
            owner: HandoffOwner::Unknown,
            audio_source: None,
            devices: Vec::new(),
            last_event: None,
        }
    }
}

/// Whether the classic link is up, coming back, or gone.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LinkStatus {
    /// `device.connected` is true.
    Connected,
    /// A rejoin or auto-connect sequence is waiting or connecting. The UI
    /// should keep the device on screen: the link is expected back.
    Reconnecting,
    /// No link and nothing trying to get one back.
    #[default]
    Disconnected,
}

/// Why a reconnect sequence is running. `null` unless
/// [`LinkStatus::Reconnecting`].
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LinkReason {
    /// The primary bud role moved shortly before the drop: the AirPods cut
    /// this host while swapping which bud carries the radio link.
    BudSwitch,
    /// An Apple host took the AirPods over.
    TakenOver,
    /// A link supervision timeout took this host's link.
    LinkLost,
    /// Proximity auto-connect is bringing the AirPods back.
    AutoConnect,
}

/// Link health as the widget needs it: a self-healing reconnect must not look
/// like a real disconnect. Always present in state.json.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(default)]
pub struct Link {
    /// Connected, reconnecting or disconnected.
    pub status: LinkStatus,
    /// Why the reconnect is running; `null` in the other two statuses.
    pub reason: Option<LinkReason>,
    /// `Device1.Connect` calls started in the current sequence. `0` while the
    /// sequence is only scheduled.
    pub attempt: u32,
    /// When the current sequence started, RFC3339 in UTC; `null` without one.
    pub since: Option<String>,
}

/// The whole state.json document.
///
/// `Eq` is deliberately absent: `settings_requested` holds arbitrary JSON.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
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
    /// Version of the additive settings API. Missing in older snapshots.
    #[serde(default)]
    pub settings_api: u32,
    /// Accessory-confirmed settings. Values are unknown until an AAP echo.
    #[serde(default)]
    pub settings: DeviceSettings,
    /// Per-setting accessory report counters. Increment even for an unchanged
    /// value, so unrelated battery snapshots cannot be mistaken for an echo.
    #[serde(default)]
    pub settings_report_seq: std::collections::BTreeMap<String, u64>,
    /// Last value written per key in this daemon lifetime, in the same JSON
    /// shape `settings` uses for that key. Survives link loss.
    #[serde(default)]
    pub settings_requested: std::collections::BTreeMap<String, serde_json::Value>,
    /// Readback outcome per requested key: `verifying`, `confirmed`,
    /// `mismatch`, `unreported` or `unverified`.
    #[serde(default)]
    pub settings_status: std::collections::BTreeMap<String, String>,
    /// Global readback phase: `idle`, `scheduled` or `reopening`.
    #[serde(default = "verify_idle")]
    pub settings_verify: String,
    /// True while the AAP link is being reopened purely to read settings back.
    /// `connected`/`aap_link` may drop during that window; the UI keeps showing
    /// the device as connected.
    #[serde(default)]
    pub verify_reopen: bool,
    /// Apple multi-host switching. Missing in older snapshots.
    #[serde(default)]
    pub handoff: Handoff,
    /// Link health, including self-healing reconnects. Missing in schema-1
    /// snapshots, where it reads as `disconnected`.
    #[serde(default)]
    pub link: Link,
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
            settings_api: SETTINGS_API,
            settings: DeviceSettings::default(),
            settings_report_seq: Default::default(),
            settings_requested: Default::default(),
            settings_status: Default::default(),
            settings_verify: verify_idle(),
            verify_reopen: false,
            handoff: Handoff::default(),
            link: Link::default(),
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
                    source: Source::Aap,
                    fresh: true,
                    level: Some(87),
                    charging: false,
                    last_known_charging: Some(false),
                    present: true,
                    last_seen: Some(now.clone()),
                },
                right: Cell {
                    source: Source::Aap,
                    fresh: true,
                    level: Some(85),
                    charging: false,
                    last_known_charging: Some(false),
                    present: true,
                    last_seen: Some(now.clone()),
                },
                case: Cell {
                    source: Source::Aap,
                    fresh: true,
                    level: Some(62),
                    charging: true,
                    last_known_charging: Some(true),
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
            settings_api: SETTINGS_API,
            settings: DeviceSettings {
                microphone: Some(MicrophoneMode::Auto),
                press_speed: Some(PressSpeed::Default),
                hold_duration: Some(HoldDuration::Default),
                listening_mode_cycle: Some(vec![
                    NoiseControlMode::Anc,
                    NoiseControlMode::Transparency,
                    NoiseControlMode::Adaptive,
                ]),
                call_controls: Some(CallControls::MuteOnceHangupTwice),
                personalized_volume: Some(true),
            },
            settings_report_seq: Default::default(),
            settings_requested: std::collections::BTreeMap::from([(
                "press_speed".to_owned(),
                serde_json::json!("default"),
            )]),
            settings_status: std::collections::BTreeMap::from([(
                "press_speed".to_owned(),
                STATUS_CONFIRMED.to_owned(),
            )]),
            settings_verify: verify_idle(),
            verify_reopen: false,
            handoff: Handoff {
                enabled: true,
                take_over_on_play: true,
                apple_host_id: Some(true),
                owner: HandoffOwner::Other,
                audio_source: Some(HandoffAudioSource {
                    address: "A4:83:E7:00:11:22".to_owned(),
                    is_local: false,
                    state: AudioSourceStatus::Media,
                }),
                devices: vec![
                    HandoffDevice {
                        address: "5C:F3:70:0D:0E:0F".to_owned(),
                        is_local: true,
                    },
                    HandoffDevice {
                        address: "A4:83:E7:00:11:22".to_owned(),
                        is_local: false,
                    },
                ],
                last_event: Some(HandoffEvent {
                    kind: HandoffEventKind::Yielded,
                    at: crate::now_rfc3339(),
                    peer: Some("A4:83:E7:00:11:22".to_owned()),
                }),
            },
            link: Link {
                status: LinkStatus::Connected,
                reason: None,
                attempt: 0,
                since: None,
            },
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
                "handoff",
                "lid",
                "link",
                "noise_control",
                "schema",
                "settings",
                "settings_api",
                "settings_report_seq",
                "settings_requested",
                "settings_status",
                "settings_verify",
                "updated_at",
                "verify_reopen",
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
        assert_eq!(
            cell_keys,
            [
                "charging",
                "fresh",
                "last_known_charging",
                "last_seen",
                "level",
                "present",
                "source"
            ]
        );

        let mut ear_keys: Vec<&str> = obj["ear"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        ear_keys.sort_unstable();
        assert_eq!(ear_keys, ["left", "right"]);

        assert_eq!(json["schema"], 2);
        assert_eq!(json["daemon"]["source"], "aap");
        assert_eq!(json["device"]["model_id"], "201B");
        assert_eq!(json["battery"]["left"]["level"], 87);
        assert_eq!(json["battery"]["left"]["last_known_charging"], false);
        assert_eq!(json["ear"]["left"], "in");
        assert_eq!(json["ear"]["right"], "out");
        assert_eq!(json["noise_control"], "anc");
        assert_eq!(json["lid"], "unknown");
        assert_eq!(json["settings_api"], 3);
        assert_eq!(json["settings"]["microphone"], "auto");
        assert_eq!(json["settings_requested"]["press_speed"], "default");
        assert_eq!(json["settings_status"]["press_speed"], "confirmed");
        assert_eq!(json["settings_verify"], "idle");
        assert_eq!(json["verify_reopen"], false);
        assert_eq!(json["settings"]["call_controls"], "mute_once_hangup_twice");
        assert_eq!(json["link"]["status"], "connected");
        assert!(json["link"]["reason"].is_null());

        let back: Snapshot = serde_json::from_value(json).unwrap();
        assert_eq!(back, snap);
    }

    #[test]
    fn null_values_serialise_as_json_null() {
        let snap = Snapshot::initial("AC:DE:48:00:11:22");
        let json = serde_json::to_value(&snap).unwrap();
        assert!(json["battery"]["left"]["level"].is_null());
        assert!(json["battery"]["left"]["last_known_charging"].is_null());
        assert!(json["device"]["model"].is_null());
        assert!(json["conversational_awareness"].is_null());
        assert!(json["adaptive_level"].is_null());
        assert_eq!(json["settings_api"], 3);
        assert!(json["settings"]["microphone"].is_null());
        assert_eq!(json["settings_verify"], "idle");
        assert_eq!(json["verify_reopen"], false);
        assert!(json["settings_requested"].as_object().unwrap().is_empty());
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

    #[test]
    fn old_snapshot_defaults_the_additive_settings_fields() {
        let mut json = serde_json::to_value(Snapshot::example()).unwrap();
        let obj = json.as_object_mut().unwrap();
        obj.remove("settings_api");
        obj.remove("settings");
        obj.remove("settings_requested");
        obj.remove("settings_status");
        obj.remove("settings_verify");
        obj.remove("verify_reopen");

        let snapshot: Snapshot = serde_json::from_value(json).unwrap();
        assert_eq!(snapshot.settings_api, 0);
        assert_eq!(snapshot.settings, DeviceSettings::default());
        assert!(snapshot.settings_requested.is_empty());
        assert!(snapshot.settings_status.is_empty());
        assert_eq!(snapshot.settings_verify, VERIFY_IDLE);
        assert!(!snapshot.verify_reopen);
    }

    #[test]
    fn old_snapshot_defaults_missing_charging_history() {
        let mut json = serde_json::to_value(Snapshot::example()).unwrap();
        for name in ["left", "right", "case"] {
            json["battery"][name]
                .as_object_mut()
                .unwrap()
                .remove("last_known_charging");
        }

        let snapshot: Snapshot = serde_json::from_value(json).unwrap();
        assert_eq!(snapshot.battery.left.last_known_charging, None);
        assert_eq!(snapshot.battery.right.last_known_charging, None);
        assert_eq!(snapshot.battery.case.last_known_charging, None);
    }

    #[test]
    fn partial_settings_object_defaults_missing_fields() {
        let mut json = serde_json::to_value(Snapshot::example()).unwrap();
        json["settings"] = serde_json::json!({ "microphone": "left" });

        let snapshot: Snapshot = serde_json::from_value(json).unwrap();
        assert_eq!(snapshot.settings.microphone, Some(MicrophoneMode::Left));
        assert_eq!(snapshot.settings.press_speed, None);
        assert_eq!(snapshot.settings.call_controls, None);
    }
}

#[cfg(test)]
mod handoff_contract_tests {
    use super::*;

    #[test]
    fn handoff_object_matches_the_frozen_contract() {
        let json = serde_json::to_value(Snapshot::example()).unwrap();
        let h = &json["handoff"];
        let mut keys: Vec<&str> = h.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "apple_host_id",
                "audio_source",
                "devices",
                "enabled",
                "last_event",
                "owner",
                "take_over_on_play"
            ]
        );
        assert_eq!(h["owner"], "other");
        assert_eq!(h["audio_source"]["state"], "media");
        assert_eq!(h["audio_source"]["is_local"], false);
        assert_eq!(h["devices"][0]["is_local"], true);
        assert_eq!(h["last_event"]["kind"], "yielded");
        for (kind, wire) in [
            (HandoffEventKind::TookOver, "took_over"),
            (HandoffEventKind::YieldRequested, "yield_requested"),
        ] {
            assert_eq!(serde_json::to_value(kind).unwrap(), wire);
        }
    }

    #[test]
    fn default_handoff_is_present_with_nulls() {
        let json = serde_json::to_value(Snapshot::initial("")).unwrap();
        assert_eq!(
            json["handoff"],
            serde_json::json!({
                "enabled": false,
                "take_over_on_play": true,
                "apple_host_id": null,
                "owner": "unknown",
                "audio_source": null,
                "devices": [],
                "last_event": null
            })
        );
    }

    #[test]
    fn old_snapshot_without_handoff_still_loads() {
        let mut json = serde_json::to_value(Snapshot::initial("")).unwrap();
        json.as_object_mut().unwrap().remove("handoff");
        let snap: Snapshot = serde_json::from_value(json).unwrap();
        assert_eq!(snap.handoff, Handoff::default());
    }
}

#[cfg(test)]
mod link_contract_tests {
    use super::*;

    #[test]
    fn link_object_matches_the_frozen_contract() {
        let json = serde_json::to_value(Snapshot::example()).unwrap();
        let link = &json["link"];
        let mut keys: Vec<&str> = link
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, ["attempt", "reason", "since", "status"]);
        assert_eq!(link["status"], "connected");
        assert!(link["reason"].is_null());
        assert_eq!(link["attempt"], 0);
        assert!(link["since"].is_null());
    }

    #[test]
    fn link_enum_spellings_are_the_contract() {
        for (status, wire) in [
            (LinkStatus::Connected, "connected"),
            (LinkStatus::Reconnecting, "reconnecting"),
            (LinkStatus::Disconnected, "disconnected"),
        ] {
            assert_eq!(serde_json::to_value(status).unwrap(), wire);
        }
        for (reason, wire) in [
            (LinkReason::BudSwitch, "bud_switch"),
            (LinkReason::TakenOver, "taken_over"),
            (LinkReason::LinkLost, "link_lost"),
            (LinkReason::AutoConnect, "auto_connect"),
        ] {
            assert_eq!(serde_json::to_value(reason).unwrap(), wire);
        }
    }

    #[test]
    fn a_reconnecting_link_serialises_every_field() {
        let mut snap = Snapshot::initial("AC:DE:48:00:11:22");
        snap.link = Link {
            status: LinkStatus::Reconnecting,
            reason: Some(LinkReason::BudSwitch),
            attempt: 1,
            since: Some("2026-09-16T02:01:47Z".to_owned()),
        };
        let json = serde_json::to_value(&snap).unwrap();
        assert_eq!(json["link"]["status"], "reconnecting");
        assert_eq!(json["link"]["reason"], "bud_switch");
        assert_eq!(json["link"]["attempt"], 1);
        assert_eq!(json["link"]["since"], "2026-09-16T02:01:47Z");
        let back: Snapshot = serde_json::from_value(json).unwrap();
        assert_eq!(back.link, snap.link);
    }

    #[test]
    fn a_schema_one_snapshot_without_link_still_loads() {
        let mut json = serde_json::to_value(Snapshot::initial("")).unwrap();
        let obj = json.as_object_mut().unwrap();
        obj.remove("link");
        obj.insert("schema".to_owned(), serde_json::json!(1));
        let snap: Snapshot = serde_json::from_value(json).unwrap();
        assert_eq!(snap.link, Link::default());
        assert_eq!(snap.link.status, LinkStatus::Disconnected);
    }
}
