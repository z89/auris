//! Pure AAP encode/decode over `&[u8]`. This module never sees a socket, and
//! `session.rs` never parses bytes.
//!
//! Framing is SEQPACKET: one `recv()` is exactly one message, so there is no
//! re-framing and no length prefix to honour. Unknown opcodes decode to
//! [`Packet::Unknown`] and are never an error.

use bluer::Address;

use super::opcode as op;
use crate::settings::{
    validate_name, CallControls, HoldDuration, MicrophoneMode, PressSpeed, SettingCommand,
};
use crate::state::{EarState, NoiseControlMode};

/// The only way decoding can fail: the packet is shorter than its own contents
/// claim. Anything else decodes to [`Packet::Unknown`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    /// Packet is shorter than the header, or shorter than its declared payload.
    #[error("packet truncated")]
    Truncated,
}

/// Which physical cell a battery entry describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatteryComponent {
    /// Right bud.
    Right,
    /// Left bud.
    Left,
    /// Charging case.
    Case,
    /// Something else this firmware reports.
    Other(u8),
}

impl BatteryComponent {
    const fn from_wire(v: u8) -> Self {
        match v {
            0x02 => Self::Right,
            0x04 => Self::Left,
            0x08 => Self::Case,
            other => Self::Other(other),
        }
    }
}

/// One decoded battery entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatteryEntry {
    /// Which cell.
    pub component: BatteryComponent,
    /// Exact 0-100 level, `None` when the component is disconnected or the
    /// firmware reported an out-of-range value.
    pub level: Option<u8>,
    /// Charging (status 0x01).
    pub charging: bool,
    /// Reporting at all (status != 0x04).
    pub present: bool,
}

/// Metadata strings from opcode 0x001D, positionally assigned and tolerant of
/// a short or over-long list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Metadata {
    /// Bluetooth name.
    pub name: Option<String>,
    /// Model number as the accessory reports it, e.g. `A3056`. Kept raw; any
    /// mapping to a marketing name happens above this layer.
    pub model: Option<String>,
    /// Manufacturer.
    pub manufacturer: Option<String>,
    /// Serial.
    pub serial: Option<String>,
    /// Firmware revision.
    pub firmware: Option<String>,
}

/// A control value echoed back by the accessory (opcode 0x0009).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlState {
    /// Noise control mode.
    NoiseControl(NoiseControlMode),
    /// Conversational awareness on/off.
    ConversationalAwareness(bool),
    /// Adaptive transparency level 0-100.
    AdaptiveLevel(u8),
    /// A typed setting confirmed by the accessory.
    Setting(SettingCommand),
    /// Control 0x06: whether this host owns the audio connection.
    OwnsConnection(bool),
    /// A control identifier this version does not model.
    Other {
        /// Control identifier byte.
        id: u8,
        /// Raw value byte.
        value: u8,
    },
}

/// Status byte of an audio-source report (opcode 0x000E).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioSourceState {
    /// 0x00: nothing is routed.
    Idle,
    /// 0x01: a call.
    Call,
    /// 0x02: media playback.
    Media,
    /// Any other value, kept raw.
    Other(u8),
}

impl AudioSourceState {
    const fn from_wire(v: u8) -> Self {
        match v {
            0x00 => Self::Idle,
            0x01 => Self::Call,
            0x02 => Self::Media,
            other => Self::Other(other),
        }
    }
}

/// Value of [`ConnectedDevice::info`] byte 0 for a host whose link to the
/// accessory is up. See [`ConnectedDevice::is_link_up`] for the evidence.
pub const LINK_UP: u8 = 0x02;

/// One host listed in a connected-devices report (opcode 0x002E).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectedDevice {
    /// Host address. Unlike 0x000C/0x000E this is sent in display order.
    pub address: Address,
    /// Two trailing bytes per host. Byte 0 is the host's link state, see
    /// [`ConnectedDevice::is_link_up`]. Byte 1 is not established.
    pub info: [u8; 2],
}

impl ConnectedDevice {
    /// Whether this host's link to the accessory is up right now.
    ///
    /// The AirPods keep a recently disconnected host in the 0x002E list, so
    /// being listed says nothing. Info byte 0 does: it lines up with the
    /// other side's own connection log (`TipiTableEvent ... Conn
    /// Connected|Disconnected`), byte for byte:
    ///
    /// * `0x00` listed, link down. The reports carrying `00 15` then `00 05`
    ///   for a host follow shortly after that host logs `Conn Disconnected`,
    ///   both while that host is away.
    /// * `0x01` connecting. `01 05` appears a few seconds before that host's
    ///   `Conn Connected`; `01 01` for this host appears while it is
    ///   reconnecting, turning into `02 ..` within about 80 ms.
    /// * `0x02` link up. `02 17` (or similar) appears within a few
    ///   milliseconds of the other side's `Conn Connected`.
    ///
    /// Byte 1 varies independently (`0x15`/`0x17` for the Mac, `0x01`/`0x03`
    /// for this host) and is left alone.
    pub fn is_link_up(&self) -> bool {
        self.info[0] == LINK_UP
    }
}

/// A decoded AAP message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Packet {
    /// Reply to the opening handshake (`01 00 04 00 ...`).
    HandshakeAck,
    /// Acknowledgement of set-features (`04 00 04 00 2b 00 ...`).
    FeaturesAck,
    /// Battery notification.
    Battery(Vec<BatteryEntry>),
    /// In-ear detection, in wire order: primary bud then secondary.
    EarDetection {
        /// Primary bud.
        primary: EarState,
        /// Secondary bud.
        secondary: EarState,
    },
    /// Control state echo.
    Control(ControlState),
    /// Device metadata.
    Metadata(Metadata),
    /// Conversational awareness speech event (opcode 0x004B): the duck level
    /// the buds applied because the wearer is talking. Low values mean speech
    /// started, high values (>= 0x06) mean it ended. Not the on/off state.
    ConversationalAwarenessLevel(u8),
    /// Address report (opcode 0x000C): an address and two unexplained bytes.
    AddressReport {
        /// Address, already un-reversed.
        address: Address,
        /// Trailing bytes, kept raw.
        extra: [u8; 2],
    },
    /// Audio source (opcode 0x000E): which host the accessory routes for.
    AudioSource {
        /// Host address, already un-reversed.
        address: Address,
        /// What that host is doing.
        state: AudioSourceState,
    },
    /// Connected devices (opcode 0x002E): every host linked to the accessory.
    ConnectedDevices {
        /// Two leading bytes, kept raw.
        header: [u8; 2],
        /// Hosts in wire order.
        devices: Vec<ConnectedDevice>,
    },
    /// Smart-routing message another host sent via the accessory (0x0011).
    SmartRouting {
        /// Sending host, already un-reversed.
        sender: Address,
        /// OPACK body, clipped to its declared length.
        body: Vec<u8>,
    },
    /// Anything else. Logged at debug and ignored; never an error.
    Unknown {
        /// Little-endian opcode as read from bytes 4..6.
        opcode: u16,
        /// Everything after the header.
        payload: Vec<u8>,
    },
}

const fn ear_from_wire(v: u8) -> EarState {
    match v {
        0x00 => EarState::In,
        0x01 => EarState::Out,
        0x02 => EarState::Case,
        _ => EarState::Unknown,
    }
}

/// Decode one SEQPACKET datagram.
pub fn decode(buf: &[u8]) -> Result<Packet, DecodeError> {
    if buf.len() < op::HEADER_LEN {
        return Err(DecodeError::Truncated);
    }
    if buf[..4] == op::ACK_PREFIX {
        return Ok(Packet::HandshakeAck);
    }
    let opcode = u16::from_le_bytes([buf[4], buf[5]]);
    let payload = &buf[op::HEADER_LEN..];
    if buf[..4] != op::PREFIX {
        return Ok(Packet::Unknown {
            opcode,
            payload: payload.to_vec(),
        });
    }

    match opcode {
        op::OP_BATTERY => decode_battery(payload).map(Packet::Battery),
        op::OP_EAR_DETECTION => {
            if payload.len() < 2 {
                return Err(DecodeError::Truncated);
            }
            Ok(Packet::EarDetection {
                primary: ear_from_wire(payload[0]),
                secondary: ear_from_wire(payload[1]),
            })
        }
        op::OP_CONTROL => {
            if payload.len() < 2 {
                return Err(DecodeError::Truncated);
            }
            decode_control(payload).map(Packet::Control)
        }
        op::OP_METADATA => Ok(Packet::Metadata(decode_metadata(payload))),
        op::OP_FEATURES_ACK => Ok(Packet::FeaturesAck),
        op::OP_CONV_AWARENESS_EVENT => {
            if payload.is_empty() {
                return Err(DecodeError::Truncated);
            }
            // Observed layout: 02 00 [level]. Take the last byte so a
            // shorter variant still yields something sensible.
            Ok(Packet::ConversationalAwarenessLevel(
                payload[payload.len() - 1],
            ))
        }
        op::OP_ADDRESS_REPORT => {
            if payload.len() < 8 {
                return Err(DecodeError::Truncated);
            }
            Ok(Packet::AddressReport {
                address: address_reversed(&payload[..6]),
                extra: [payload[6], payload[7]],
            })
        }
        op::OP_AUDIO_SOURCE => {
            if payload.len() < 7 {
                return Err(DecodeError::Truncated);
            }
            Ok(Packet::AudioSource {
                address: address_reversed(&payload[..6]),
                state: AudioSourceState::from_wire(payload[6]),
            })
        }
        op::OP_CONNECTED_DEVICES => decode_connected_devices(payload),
        op::OP_SMART_ROUTING_RELAY => {
            if payload.len() < 8 {
                return Err(DecodeError::Truncated);
            }
            let declared = usize::from(u16::from_le_bytes([payload[6], payload[7]]));
            let body = &payload[8..];
            Ok(Packet::SmartRouting {
                sender: address_reversed(&payload[..6]),
                body: body[..declared.min(body.len())].to_vec(),
            })
        }
        _ => Ok(Packet::Unknown {
            opcode,
            payload: payload.to_vec(),
        }),
    }
}

/// Address bytes as sent in 0x000C/0x000E/0x0010/0x0011: least significant first.
fn address_reversed(b: &[u8]) -> Address {
    Address::new([b[5], b[4], b[3], b[2], b[1], b[0]])
}

fn decode_connected_devices(payload: &[u8]) -> Result<Packet, DecodeError> {
    if payload.len() < 3 {
        return Err(DecodeError::Truncated);
    }
    let count = usize::from(payload[2]);
    // Tolerate a short list the way LibrePods does: keep what is complete.
    let devices = payload[3..]
        .chunks_exact(8)
        .take(count)
        .map(|c| ConnectedDevice {
            address: Address::new([c[0], c[1], c[2], c[3], c[4], c[5]]),
            info: [c[6], c[7]],
        })
        .collect();
    Ok(Packet::ConnectedDevices {
        header: [payload[0], payload[1]],
        devices,
    })
}

fn decode_control(payload: &[u8]) -> Result<ControlState, DecodeError> {
    let id = payload[0];
    let value = payload[1];
    let typed_setting = matches!(
        id,
        op::CTL_MICROPHONE
            | op::CTL_PRESS_SPEED
            | op::CTL_HOLD_DURATION
            | op::CTL_LISTENING_MODE_CYCLE
            | op::CTL_CALL_CONTROLS
            | op::CTL_PERSONALIZED_VOLUME
    );
    if typed_setting && payload.len() < 5 {
        return Err(DecodeError::Truncated);
    }
    if typed_setting && payload.len() != 5 {
        return Ok(ControlState::Other { id, value });
    }
    let scalar_padding_is_zero = !typed_setting || payload[2..5] == [0x00; 3];
    match id {
        op::CTL_NOISE_CONTROL => match NoiseControlMode::from_wire(value) {
            Some(m) => Ok(ControlState::NoiseControl(m)),
            None => Ok(ControlState::Other { id, value }),
        },
        op::CTL_CONV_AWARENESS => Ok(ControlState::ConversationalAwareness(value == 0x01)),
        op::CTL_ADAPTIVE_LEVEL => Ok(ControlState::AdaptiveLevel(value.min(100))),
        op::CTL_OWNS_CONNECTION => Ok(match value {
            0x00 => ControlState::OwnsConnection(false),
            0x01 => ControlState::OwnsConnection(true),
            _ => ControlState::Other { id, value },
        }),
        op::CTL_MICROPHONE if scalar_padding_is_zero => Ok(match value {
            0x00 => ControlState::Setting(SettingCommand::Microphone(MicrophoneMode::Auto)),
            0x01 => ControlState::Setting(SettingCommand::Microphone(MicrophoneMode::Right)),
            0x02 => ControlState::Setting(SettingCommand::Microphone(MicrophoneMode::Left)),
            _ => ControlState::Other { id, value },
        }),
        op::CTL_PRESS_SPEED if scalar_padding_is_zero => Ok(match value {
            0x00 => ControlState::Setting(SettingCommand::PressSpeed(PressSpeed::Default)),
            0x01 => ControlState::Setting(SettingCommand::PressSpeed(PressSpeed::Slower)),
            0x02 => ControlState::Setting(SettingCommand::PressSpeed(PressSpeed::Slowest)),
            _ => ControlState::Other { id, value },
        }),
        op::CTL_HOLD_DURATION if scalar_padding_is_zero => Ok(match value {
            0x00 => ControlState::Setting(SettingCommand::HoldDuration(HoldDuration::Default)),
            0x01 => ControlState::Setting(SettingCommand::HoldDuration(HoldDuration::Shorter)),
            0x02 => ControlState::Setting(SettingCommand::HoldDuration(HoldDuration::Shortest)),
            _ => ControlState::Other { id, value },
        }),
        op::CTL_LISTENING_MODE_CYCLE if scalar_padding_is_zero => {
            let modes = listening_modes_from_mask(value);
            let setting = SettingCommand::ListeningModeCycle(modes);
            // A report describes firmware state, not a proposed write. In
            // particular, keep a single-bit report even though our command
            // API requires at least two modes for a useful stem cycle.
            if value != 0 && value & !0x0f == 0 {
                Ok(ControlState::Setting(setting))
            } else {
                Ok(ControlState::Other { id, value })
            }
        }
        op::CTL_CALL_CONTROLS => {
            // Unlike scalar controls this setting uses two meaningful bytes,
            // followed by the two zero padding bytes of the fixed frame.
            Ok(match &payload[1..5] {
                [0x00, 0x03, 0x00, 0x00] => ControlState::Setting(SettingCommand::CallControls(
                    CallControls::MuteOnceHangupTwice,
                )),
                [0x00, 0x02, 0x00, 0x00] => ControlState::Setting(SettingCommand::CallControls(
                    CallControls::HangupOnceMuteTwice,
                )),
                _ => ControlState::Other { id, value },
            })
        }
        op::CTL_PERSONALIZED_VOLUME if scalar_padding_is_zero => Ok(match value {
            0x01 => ControlState::Setting(SettingCommand::PersonalizedVolume(true)),
            0x02 => ControlState::Setting(SettingCommand::PersonalizedVolume(false)),
            _ => ControlState::Other { id, value },
        }),
        _ => Ok(ControlState::Other { id, value }),
    }
}

fn listening_modes_from_mask(mask: u8) -> Vec<NoiseControlMode> {
    [
        (0x01, NoiseControlMode::Off),
        (0x02, NoiseControlMode::Anc),
        (0x04, NoiseControlMode::Transparency),
        (0x08, NoiseControlMode::Adaptive),
    ]
    .into_iter()
    .filter_map(|(bit, mode)| (mask & bit != 0).then_some(mode))
    .collect()
}

fn decode_battery(payload: &[u8]) -> Result<Vec<BatteryEntry>, DecodeError> {
    let count = *payload.first().ok_or(DecodeError::Truncated)? as usize;
    let body = &payload[1..];
    if body.len() < count * 5 {
        return Err(DecodeError::Truncated);
    }
    let mut out = Vec::with_capacity(count);
    for chunk in body.chunks_exact(5).take(count) {
        // [component] 01 [level] [status] 01
        let component = BatteryComponent::from_wire(chunk[0]);
        let level = chunk[2];
        let status = chunk[3];
        let present = status != 0x04;
        out.push(BatteryEntry {
            component,
            level: if present && level <= 100 {
                Some(level)
            } else {
                None
            },
            charging: status == 0x01,
            present,
        });
    }
    Ok(out)
}

/// Decode opcode 0x001D.
///
/// The strings begin at frame offset 11, i.e. [`op::METADATA_SKIP`] bytes into
/// the payload under the 6-byte header framing used here. Those leading bytes
/// are not text and contain NULs of their own; splitting from offset 0 is what
/// produced the bogus `"\u{2}"` name.
///
/// The full string list, in wire order, is: name, model number, manufacturer,
/// serial, version1, version2, hardware revision, updater app, left serial,
/// right serial, version. Only the first five are modelled.
fn decode_metadata(payload: &[u8]) -> Metadata {
    let Some(payload) = payload.get(op::METADATA_SKIP..) else {
        return Metadata::default();
    };
    let mut fields: Vec<String> = payload
        .split(|b| *b == 0)
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();
    while fields.last().is_some_and(String::is_empty) {
        fields.pop();
    }
    let mut it = fields
        .into_iter()
        .map(|s| if s.is_empty() { None } else { Some(s) });
    Metadata {
        name: it.next().flatten(),
        model: it.next().flatten(),
        manufacturer: it.next().flatten(),
        serial: it.next().flatten(),
        firmware: it.next().flatten(),
    }
}

// ---------------------------------------------------------------------------
// Encoders
// ---------------------------------------------------------------------------

/// Which set-features packet to send. Both forms are 14 bytes. The `0xd7`
/// form is the default; the `0x0e` form is what AlwxSin's daemon sends and is
/// reached either by `AURISD_FEATURES=alt` or by automatic fallback when a
/// session produced no battery packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FeaturesVariant {
    /// Default form: `04 00 04 00 4d 00 d7 00 00 00 00 00 00 00`.
    #[default]
    D7,
    /// Alternate form: `04 00 04 00 4d 00 0e 00 00 00 00 00 00 00`.
    Alt,
    /// Full form iOS sends: `04 00 04 00 4d 00 ff 00 00 00 00 00 00 00`.
    Ff,
}

/// The opening handshake.
pub fn encode_handshake() -> Vec<u8> {
    op::HANDSHAKE.to_vec()
}

/// Feature negotiation, in the requested variant.
pub fn encode_set_features(variant: FeaturesVariant) -> Vec<u8> {
    match variant {
        FeaturesVariant::D7 => op::SET_FEATURES_D7.to_vec(),
        FeaturesVariant::Alt => op::SET_FEATURES_ALT.to_vec(),
        FeaturesVariant::Ff => op::SET_FEATURES_FF.to_vec(),
    }
}

/// Subscribe to notifications.
pub fn encode_request_notifications() -> Vec<u8> {
    op::REQUEST_NOTIFICATIONS.to_vec()
}

/// The narrower first subscribe sent ahead of the full one on the alternate
/// ordering.
pub fn encode_request_notifications_alt() -> Vec<u8> {
    op::REQUEST_NOTIFICATIONS_ALT.to_vec()
}

fn encode_control_data(id: u8, data: &[u8]) -> Vec<u8> {
    debug_assert!(data.len() <= 4);
    let mut v = Vec::with_capacity(11);
    v.extend_from_slice(&op::PREFIX);
    v.extend_from_slice(&op::OP_CONTROL.to_le_bytes());
    v.push(id);
    v.extend_from_slice(data);
    v.resize(11, 0x00);
    v
}

fn encode_control(id: u8, value: u8) -> Vec<u8> {
    encode_control_data(id, &[value])
}

/// Claim (`true`) or give up (`false`) the accessory's audio connection.
pub fn encode_owns_connection(owns: bool) -> Vec<u8> {
    encode_control(op::CTL_OWNS_CONNECTION, u8::from(owns))
}

// ---------------------------------------------------------------------------
// Smart routing (opcode 0x0010). Bodies are OPACK dictionaries preceded by a
// constant 0x01. Key names and values follow LibrePods' AACPManager.kt; the
// Hijackv2 body is checked against the iPhone capture quoted in its history.
// ---------------------------------------------------------------------------

/// `PlayingApp` value LibrePods sends when no app is known.
pub const PLAYING_APP_UNKNOWN: &str = "NA";
/// `btName` announced to other hosts unless configured (LibrePods PR #202).
pub const DEFAULT_BT_NAME: &str = "Mac";
/// Longest string that fits the one-byte OPACK tag `0x40 + length`.
pub const OPACK_SHORT_STRING: usize = 32;
/// Key whose presence in a relayed 0x0011 body asks this host to yield.
const OWNERSHIP_TO_FALSE: &[u8] = b"audioRoutingSetOwnershipToFalse";
const OPACK_TRUE: u8 = 0x01;

fn opack_str(out: &mut Vec<u8>, s: &str) {
    let mut end = s.len().min(OPACK_SHORT_STRING);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    // end <= 32, so the tag stays inside 0x40..=0x60.
    out.push(0x40 + end as u8);
    out.extend_from_slice(&s.as_bytes()[..end]);
}

fn opack_uint(out: &mut Vec<u8>, v: u16) {
    match v {
        0..=39 => out.push(0x08 + v as u8),
        40..=127 => out.extend_from_slice(&[0x30, v as u8]),
        _ => {
            out.push(0x31);
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
}

/// Start a body holding an OPACK dictionary of `entries` pairs.
fn opack_body(entries: u8) -> Vec<u8> {
    vec![0x01, 0xE0 + entries]
}

/// Frame an OPACK body as a 0x0010 message the accessory relays to `target`.
pub fn encode_smart_routing(target: Address, body: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(op::HEADER_LEN + 8 + body.len());
    v.extend_from_slice(&op::PREFIX);
    v.extend_from_slice(&op::OP_SMART_ROUTING.to_le_bytes());
    let mut mac = target.0;
    mac.reverse();
    v.extend_from_slice(&mac);
    let len = u16::try_from(body.len()).unwrap_or(u16::MAX);
    v.extend_from_slice(&len.to_le_bytes());
    v.extend_from_slice(body);
    v
}

/// Ask `target` to give up the audio route (`reason = Hijackv2`).
pub fn encode_hijack_v2(target: Address) -> Vec<u8> {
    let mut b = opack_body(5);
    opack_str(&mut b, "localscore");
    opack_uint(&mut b, 100);
    opack_str(&mut b, "reason");
    opack_str(&mut b, "Hijackv2");
    opack_str(&mut b, "audioRoutingScore");
    opack_uint(&mut b, 301);
    opack_str(&mut b, "audioRoutingSetOwnershipToFalse");
    b.push(OPACK_TRUE);
    opack_str(&mut b, "remotescore");
    // Back-reference to the sixth object, the score 301.
    b.push(0xA5);
    encode_smart_routing(target, &b)
}

/// Tell `target` what this host is playing and whether it is streaming.
pub fn encode_media_info(
    target: Address,
    local: Address,
    playing_app: &str,
    streaming: bool,
    bt_name: &str,
) -> Vec<u8> {
    let mut b = opack_body(5);
    opack_str(&mut b, "PlayingApp");
    opack_str(&mut b, playing_app);
    opack_str(&mut b, "HostStreamingState");
    opack_str(&mut b, if streaming { "YES" } else { "NO" });
    opack_str(&mut b, "btAddress");
    opack_str(&mut b, &local.to_string());
    opack_str(&mut b, "btName");
    opack_str(&mut b, bt_name);
    opack_str(&mut b, "otherDeviceAudioCategory");
    opack_uint(&mut b, 301);
    encode_smart_routing(target, &b)
}

/// Media information for a host that just joined (idle, not streaming).
pub fn encode_media_info_new_device(target: Address, local: Address, bt_name: &str) -> Vec<u8> {
    let mut b = opack_body(5);
    opack_str(&mut b, "playingApp");
    opack_str(&mut b, PLAYING_APP_UNKNOWN);
    opack_str(&mut b, "hostStreamingState");
    opack_str(&mut b, "NO");
    opack_str(&mut b, "btAddress");
    opack_str(&mut b, &local.to_string());
    opack_str(&mut b, "btName");
    opack_str(&mut b, bt_name);
    opack_str(&mut b, "otherDeviceAudioCategory");
    opack_uint(&mut b, 100);
    encode_smart_routing(target, &b)
}

/// Introduce this host to a host that just joined (`newTipi`).
pub fn encode_new_tipi(target: Address, local: Address, bt_name: &str) -> Vec<u8> {
    let mut b = opack_body(5);
    opack_str(&mut b, "idleTime");
    opack_uint(&mut b, 0);
    opack_str(&mut b, "newTipi");
    b.push(OPACK_TRUE);
    opack_str(&mut b, "btAddress");
    opack_str(&mut b, &local.to_string());
    opack_str(&mut b, "btName");
    opack_str(&mut b, bt_name);
    opack_str(&mut b, "nearbyAudioScore");
    opack_uint(&mut b, 6);
    encode_smart_routing(target, &b)
}

/// Whether a relayed smart-routing body asks this host to give up ownership.
/// A substring match, as LibrePods does; the OPACK body is not parsed.
pub fn smart_routing_requests_yield(body: &[u8]) -> bool {
    body.windows(OWNERSHIP_TO_FALSE.len())
        .any(|w| w == OWNERSHIP_TO_FALSE)
}

/// Set the noise control mode.
pub fn encode_set_noise_control(mode: NoiseControlMode) -> Vec<u8> {
    encode_control(op::CTL_NOISE_CONTROL, mode.to_wire())
}

/// Turn conversational awareness on or off.
pub fn encode_set_conversational_awareness(on: bool) -> Vec<u8> {
    encode_control(op::CTL_CONV_AWARENESS, if on { 0x01 } else { 0x02 })
}

/// Set the adaptive transparency level; the value is clamped to 0-100.
pub fn encode_set_adaptive_level(level: u8) -> Vec<u8> {
    encode_control(op::CTL_ADAPTIVE_LEVEL, level.min(100))
}

/// Encode one typed setting as a fixed-width control command.
pub fn encode_set_setting(setting: &SettingCommand) -> Result<Vec<u8>, String> {
    setting.validate()?;
    let packet = match setting {
        SettingCommand::Microphone(mode) => encode_control(
            op::CTL_MICROPHONE,
            match mode {
                MicrophoneMode::Auto => 0x00,
                MicrophoneMode::Right => 0x01,
                MicrophoneMode::Left => 0x02,
            },
        ),
        SettingCommand::PressSpeed(speed) => encode_control(
            op::CTL_PRESS_SPEED,
            match speed {
                PressSpeed::Default => 0x00,
                PressSpeed::Slower => 0x01,
                PressSpeed::Slowest => 0x02,
            },
        ),
        SettingCommand::HoldDuration(duration) => encode_control(
            op::CTL_HOLD_DURATION,
            match duration {
                HoldDuration::Default => 0x00,
                HoldDuration::Shorter => 0x01,
                HoldDuration::Shortest => 0x02,
            },
        ),
        SettingCommand::ListeningModeCycle(modes) => {
            let mask = modes.iter().fold(0, |mask, mode| {
                mask | match mode {
                    NoiseControlMode::Off => 0x01,
                    NoiseControlMode::Anc => 0x02,
                    NoiseControlMode::Transparency => 0x04,
                    NoiseControlMode::Adaptive => 0x08,
                }
            });
            encode_control(op::CTL_LISTENING_MODE_CYCLE, mask)
        }
        SettingCommand::CallControls(controls) => encode_control_data(
            op::CTL_CALL_CONTROLS,
            match controls {
                CallControls::MuteOnceHangupTwice => &[0x00, 0x03],
                CallControls::HangupOnceMuteTwice => &[0x00, 0x02],
            },
        ),
        SettingCommand::PersonalizedVolume(on) => {
            encode_control(op::CTL_PERSONALIZED_VOLUME, if *on { 0x01 } else { 0x02 })
        }
    };
    Ok(packet)
}

/// Encode a rename request. Success only means the packet was accepted for
/// sending; the name remains unconfirmed until later metadata arrives.
pub fn encode_rename(name: &str) -> Result<Vec<u8>, String> {
    validate_name(name)?;
    let bytes = name.as_bytes();
    let mut packet = Vec::with_capacity(9 + bytes.len());
    packet.extend_from_slice(&op::PREFIX);
    packet.extend_from_slice(&op::OP_RENAME.to_le_bytes());
    packet.extend_from_slice(&[0x01, bytes.len() as u8, 0x00]);
    packet.extend_from_slice(bytes);
    Ok(packet)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn battery(pkt: Packet) -> Vec<BatteryEntry> {
        match pkt {
            Packet::Battery(v) => v,
            other => panic!("expected battery, got {other:?}"),
        }
    }

    /// 1
    #[test]
    fn battery_three_components() {
        let bytes = [
            0x04, 0x00, 0x04, 0x00, 0x04, 0x00, 0x03, 0x04, 0x01, 0x57, 0x02, 0x01, 0x02, 0x01,
            0x55, 0x02, 0x01, 0x08, 0x01, 0x3e, 0x01, 0x01,
        ];
        let entries = battery(decode(&bytes).unwrap());
        assert_eq!(entries.len(), 3);
        assert_eq!(
            entries[0],
            BatteryEntry {
                component: BatteryComponent::Left,
                level: Some(87),
                charging: false,
                present: true
            }
        );
        assert_eq!(
            entries[1],
            BatteryEntry {
                component: BatteryComponent::Right,
                level: Some(85),
                charging: false,
                present: true
            }
        );
        assert_eq!(
            entries[2],
            BatteryEntry {
                component: BatteryComponent::Case,
                level: Some(62),
                charging: true,
                present: true
            }
        );
    }

    /// 2
    #[test]
    fn battery_disconnected_case() {
        let bytes = [
            0x04, 0x00, 0x04, 0x00, 0x04, 0x00, 0x02, 0x04, 0x01, 0x57, 0x02, 0x01, 0x08, 0x01,
            0x00, 0x04, 0x01,
        ];
        let entries = battery(decode(&bytes).unwrap());
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].component, BatteryComponent::Left);
        assert_eq!(entries[0].level, Some(87));
        assert_eq!(entries[1].component, BatteryComponent::Case);
        assert_eq!(entries[1].level, None);
        assert!(!entries[1].present);
        assert!(!entries[1].charging);
    }

    /// 3
    #[test]
    fn battery_truncated() {
        // Declares three components, carries one.
        let bytes = [
            0x04, 0x00, 0x04, 0x00, 0x04, 0x00, 0x03, 0x04, 0x01, 0x57, 0x02, 0x01,
        ];
        assert_eq!(decode(&bytes), Err(DecodeError::Truncated));
    }

    /// 4
    #[test]
    fn ear_in_out() {
        let bytes = [0x04, 0x00, 0x04, 0x00, 0x06, 0x00, 0x00, 0x01];
        assert_eq!(
            decode(&bytes).unwrap(),
            Packet::EarDetection {
                primary: EarState::In,
                secondary: EarState::Out
            }
        );
    }

    /// 5
    #[test]
    fn ear_both_in_case() {
        let bytes = [0x04, 0x00, 0x04, 0x00, 0x06, 0x00, 0x02, 0x02];
        assert_eq!(
            decode(&bytes).unwrap(),
            Packet::EarDetection {
                primary: EarState::Case,
                secondary: EarState::Case
            }
        );
    }

    /// 6
    #[test]
    fn noise_control_decode() {
        let bytes = [
            0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x0d, 0x02, 0x00, 0x00, 0x00,
        ];
        assert_eq!(
            decode(&bytes).unwrap(),
            Packet::Control(ControlState::NoiseControl(NoiseControlMode::Anc))
        );
    }

    /// 7
    #[test]
    fn noise_control_encode_roundtrip() {
        for (mode, byte) in [
            (NoiseControlMode::Off, 0x01u8),
            (NoiseControlMode::Anc, 0x02),
            (NoiseControlMode::Transparency, 0x03),
            (NoiseControlMode::Adaptive, 0x04),
        ] {
            let pkt = encode_set_noise_control(mode);
            assert_eq!(
                pkt,
                vec![0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x0d, byte, 0x00, 0x00, 0x00]
            );
            assert_eq!(
                decode(&pkt).unwrap(),
                Packet::Control(ControlState::NoiseControl(mode))
            );
        }
    }

    /// 8
    #[test]
    fn conv_awareness_encode() {
        assert_eq!(
            encode_set_conversational_awareness(true),
            vec![0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x28, 0x01, 0x00, 0x00, 0x00]
        );
        assert_eq!(
            encode_set_conversational_awareness(false),
            vec![0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x28, 0x02, 0x00, 0x00, 0x00]
        );
        assert_eq!(
            decode(&encode_set_conversational_awareness(true)).unwrap(),
            Packet::Control(ControlState::ConversationalAwareness(true))
        );
    }

    /// 9
    #[test]
    fn adaptive_level_encode() {
        assert_eq!(
            encode_set_adaptive_level(50),
            vec![0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x2e, 0x32, 0x00, 0x00, 0x00]
        );
        assert_eq!(encode_set_adaptive_level(0)[7], 0x00);
        assert_eq!(encode_set_adaptive_level(100)[7], 100);
        assert_eq!(encode_set_adaptive_level(255)[7], 100, "clamps to 100");
    }

    /// 10
    #[test]
    fn metadata_decode() {
        // Header, then five bytes that are not text: the strings start at
        // frame offset 11.
        let mut bytes = vec![0x04, 0x00, 0x04, 0x00, 0x1d, 0x00];
        bytes.extend_from_slice(&[0x01, 0x00, 0x00, 0x00, 0x02]);
        bytes.extend_from_slice(b"AirPods\x00A3056\x00Apple Inc.\x00H4H200\x007B21\x00");
        let Packet::Metadata(md) = decode(&bytes).unwrap() else {
            panic!("expected metadata")
        };
        assert_eq!(md.name.as_deref(), Some("AirPods"));
        assert_eq!(md.model.as_deref(), Some("A3056"));
        assert_eq!(md.manufacturer.as_deref(), Some("Apple Inc."));
        assert_eq!(md.serial.as_deref(), Some("H4H200"));
        assert_eq!(md.firmware.as_deref(), Some("7B21"));
    }

    /// 10b
    #[test]
    fn metadata_tail_strings_are_ignored_not_fatal() {
        let mut bytes = vec![0x04, 0x00, 0x04, 0x00, 0x1d, 0x00];
        bytes.extend_from_slice(&[0x01, 0x00, 0x00, 0x00, 0x02]);
        bytes.extend_from_slice(
            b"AirPods\x00A3056\x00Apple Inc.\x00H4H200\x007B21\x007B21\x001.0\x00updater\x00LSER\x00RSER\x007B21\x00",
        );
        let Packet::Metadata(md) = decode(&bytes).unwrap() else {
            panic!("expected metadata")
        };
        assert_eq!(md.name.as_deref(), Some("AirPods"));
        assert_eq!(md.firmware.as_deref(), Some("7B21"));
    }

    /// 10c
    #[test]
    fn metadata_shorter_than_the_skip_is_empty_not_garbage() {
        let bytes = [0x04, 0x00, 0x04, 0x00, 0x1d, 0x00, 0x01, 0x00];
        assert_eq!(
            decode(&bytes).unwrap(),
            Packet::Metadata(Metadata::default())
        );
    }

    /// 11
    #[test]
    fn conv_awareness_event_is_a_level_not_a_state() {
        let bytes = [0x04, 0x00, 0x04, 0x00, 0x4b, 0x00, 0x02, 0x00, 0x08];
        assert_eq!(
            decode(&bytes).unwrap(),
            Packet::ConversationalAwarenessLevel(0x08)
        );
    }

    /// 12
    #[test]
    fn unknown_opcode_is_not_error() {
        let bytes = [0x04, 0x00, 0x04, 0x00, 0xff, 0x7f, 0x00];
        assert_eq!(
            decode(&bytes).unwrap(),
            Packet::Unknown {
                opcode: 0x7fff,
                payload: vec![0x00]
            }
        );
    }

    /// 13
    #[test]
    fn short_packet_rejected() {
        assert_eq!(decode(&[0x04, 0x00, 0x04]), Err(DecodeError::Truncated));
        assert_eq!(decode(&[]), Err(DecodeError::Truncated));
    }

    /// 14
    #[test]
    fn static_packets_match_contract() {
        assert_eq!(
            encode_handshake(),
            vec![
                0x00, 0x00, 0x04, 0x00, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00
            ]
        );
        assert_eq!(
            encode_set_features(FeaturesVariant::D7),
            vec![
                0x04, 0x00, 0x04, 0x00, 0x4d, 0x00, 0xd7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00
            ]
        );
        assert_eq!(
            encode_set_features(FeaturesVariant::Alt),
            vec![
                0x04, 0x00, 0x04, 0x00, 0x4d, 0x00, 0x0e, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00
            ]
        );
        assert_eq!(
            encode_request_notifications(),
            vec![0x04, 0x00, 0x04, 0x00, 0x0f, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff]
        );
        assert_eq!(
            encode_request_notifications_alt(),
            vec![0x04, 0x00, 0x04, 0x00, 0x0f, 0x00, 0xff, 0xff, 0xef, 0xff]
        );
    }

    /// 15
    #[test]
    fn both_features_variants_are_fourteen_bytes() {
        for v in [FeaturesVariant::D7, FeaturesVariant::Alt] {
            let pkt = encode_set_features(v);
            assert_eq!(pkt.len(), 14, "{v:?} must be 14 bytes, not 13");
            // Header, opcode, selector, then seven zero bytes.
            assert_eq!(&pkt[..6], &[0x04, 0x00, 0x04, 0x00, 0x4d, 0x00]);
            assert_eq!(&pkt[7..], &[0u8; 7]);
        }
        assert_eq!(encode_set_features(FeaturesVariant::D7)[6], 0xd7);
        assert_eq!(encode_set_features(FeaturesVariant::Alt)[6], 0x0e);
    }

    /// 16
    #[test]
    fn handshake_ack_recognised() {
        let bytes = [0x01, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00];
        assert_eq!(decode(&bytes).unwrap(), Packet::HandshakeAck);
        // Any frame on the ack prefix is an ack, whatever follows it.
        let other = [0x01, 0x00, 0x04, 0x00, 0x1d, 0x00, 0xff];
        assert_eq!(decode(&other).unwrap(), Packet::HandshakeAck);
        // The ordinary prefix is not one.
        let not_ack = [0x04, 0x00, 0x04, 0x00, 0x01, 0x00, 0x00];
        assert_ne!(decode(&not_ack).unwrap(), Packet::HandshakeAck);
    }

    /// 17
    #[test]
    fn features_ack_is_distinct_from_unknown() {
        let bytes = [0x04, 0x00, 0x04, 0x00, 0x2b, 0x00, 0x00, 0x00, 0x00, 0x00];
        assert_eq!(decode(&bytes).unwrap(), Packet::FeaturesAck);
        // Payload-free is still an ack; a neighbouring opcode is not.
        assert_eq!(decode(&bytes[..6]).unwrap(), Packet::FeaturesAck);
        assert!(matches!(
            decode(&[0x04, 0x00, 0x04, 0x00, 0x2c, 0x00]).unwrap(),
            Packet::Unknown { opcode: 0x002c, .. }
        ));
    }

    #[test]
    fn typed_setting_packets_match_the_wire_contract_and_round_trip() {
        let cases = [
            (
                SettingCommand::Microphone(MicrophoneMode::Auto),
                vec![0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x01, 0x00, 0, 0, 0],
            ),
            (
                SettingCommand::Microphone(MicrophoneMode::Right),
                vec![0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x01, 0x01, 0, 0, 0],
            ),
            (
                SettingCommand::Microphone(MicrophoneMode::Left),
                vec![0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x01, 0x02, 0, 0, 0],
            ),
            (
                SettingCommand::PressSpeed(PressSpeed::Default),
                vec![0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x17, 0x00, 0, 0, 0],
            ),
            (
                SettingCommand::PressSpeed(PressSpeed::Slower),
                vec![0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x17, 0x01, 0, 0, 0],
            ),
            (
                SettingCommand::PressSpeed(PressSpeed::Slowest),
                vec![0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x17, 0x02, 0, 0, 0],
            ),
            (
                SettingCommand::HoldDuration(HoldDuration::Default),
                vec![0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x18, 0x00, 0, 0, 0],
            ),
            (
                SettingCommand::HoldDuration(HoldDuration::Shorter),
                vec![0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x18, 0x01, 0, 0, 0],
            ),
            (
                SettingCommand::HoldDuration(HoldDuration::Shortest),
                vec![0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x18, 0x02, 0, 0, 0],
            ),
            (
                SettingCommand::ListeningModeCycle(vec![
                    NoiseControlMode::Off,
                    NoiseControlMode::Anc,
                    NoiseControlMode::Transparency,
                    NoiseControlMode::Adaptive,
                ]),
                vec![0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x1a, 0x0f, 0, 0, 0],
            ),
            (
                SettingCommand::CallControls(CallControls::MuteOnceHangupTwice),
                vec![0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x24, 0x00, 0x03, 0, 0],
            ),
            (
                SettingCommand::CallControls(CallControls::HangupOnceMuteTwice),
                vec![0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x24, 0x00, 0x02, 0, 0],
            ),
            (
                SettingCommand::PersonalizedVolume(true),
                vec![0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x26, 0x01, 0, 0, 0],
            ),
            (
                SettingCommand::PersonalizedVolume(false),
                vec![0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x26, 0x02, 0, 0, 0],
            ),
        ];

        for (setting, bytes) in cases {
            assert_eq!(encode_set_setting(&setting).unwrap(), bytes);
            assert_eq!(
                decode(&bytes).unwrap(),
                Packet::Control(ControlState::Setting(setting))
            );
        }
    }

    #[test]
    fn malformed_setting_echoes_never_become_confirmed_settings() {
        let short_call = [0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x24, 0x00, 0x03];
        assert_eq!(decode(&short_call), Err(DecodeError::Truncated));

        for bytes in [
            [0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x24, 0x00, 0x03, 1, 0],
            [0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x01, 0x00, 1, 0, 0],
            [0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x1a, 0x10, 0, 0, 0],
            [0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x26, 0x00, 0, 0, 0],
        ] {
            assert!(matches!(
                decode(&bytes).unwrap(),
                Packet::Control(ControlState::Other { .. })
            ));
        }

        let short_scalar = [0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x17, 0x01];
        assert_eq!(decode(&short_scalar), Err(DecodeError::Truncated));
    }

    #[test]
    fn rename_packet_uses_utf8_byte_length() {
        assert_eq!(
            encode_rename("Auré").unwrap(),
            vec![
                0x04, 0x00, 0x04, 0x00, 0x1a, 0x00, 0x01, 0x05, 0x00, b'A', b'u', b'r', 0xc3, 0xa9,
            ]
        );
        assert!(encode_rename("bad\nname").is_err());
        assert!(encode_rename(&"x".repeat(256)).is_err());
    }

    #[test]
    fn single_mode_report_is_preserved_but_not_accepted_as_a_write() {
        let report = [4, 0, 4, 0, 9, 0, 0x1a, 2, 0, 0, 0];
        let setting = SettingCommand::ListeningModeCycle(vec![NoiseControlMode::Anc]);
        assert_eq!(
            decode(&report).unwrap(),
            Packet::Control(ControlState::Setting(setting.clone()))
        );
        assert!(encode_set_setting(&setting).is_err());
    }
}

#[cfg(test)]
mod handoff_tests {
    use super::*;

    const HOST: &str = "5C:F3:70:0D:0E:0F";
    const AIRPODS: &str = "BC:80:4E:01:02:03";

    fn addr(s: &str) -> Address {
        s.parse().unwrap()
    }

    fn hex(s: &str) -> Vec<u8> {
        let s: String = s.split_whitespace().collect();
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// Frame a payload captured from the user's AirPods (hex after the header).
    fn frame(opcode: u8, payload: &str) -> Vec<u8> {
        let mut v = vec![0x04, 0x00, 0x04, 0x00, opcode, 0x00];
        v.extend(hex(payload));
        v
    }

    fn cat(parts: &[&[u8]]) -> Vec<u8> {
        parts.concat()
    }

    #[test]
    fn real_address_reports_decode_with_reversed_addresses() {
        assert_eq!(
            decode(&frame(0x0c, "03 02 01 4e 80 bc 00 02")).unwrap(),
            Packet::AddressReport {
                address: addr(AIRPODS),
                extra: [0x00, 0x02]
            }
        );
        assert_eq!(
            decode(&frame(0x0c, "0f 0e 0d 70 f3 5c 01 01")).unwrap(),
            Packet::AddressReport {
                address: addr(HOST),
                extra: [0x01, 0x01]
            }
        );
        assert_eq!(
            decode(&frame(0x0c, "0f 0e 0d 70 f3 5c 01")),
            Err(DecodeError::Truncated)
        );
    }

    #[test]
    fn real_audio_source_reports_decode() {
        assert_eq!(
            decode(&frame(0x0e, "0f 0e 0d 70 f3 5c 00")).unwrap(),
            Packet::AudioSource {
                address: addr(HOST),
                state: AudioSourceState::Idle
            }
        );
        assert_eq!(
            decode(&frame(0x0e, "0f 0e 0d 70 f3 5c 02")).unwrap(),
            Packet::AudioSource {
                address: addr(HOST),
                state: AudioSourceState::Media
            }
        );
        assert!(matches!(
            decode(&frame(0x0e, "0f 0e 0d 70 f3 5c 01")).unwrap(),
            Packet::AudioSource {
                state: AudioSourceState::Call,
                ..
            }
        ));
        assert_eq!(
            decode(&frame(0x0e, "0f 0e 0d 70 f3 5c")),
            Err(DecodeError::Truncated)
        );
    }

    #[test]
    fn real_connected_devices_reports_decode_in_wire_order() {
        for (payload, header) in [
            ("01 00 01 5c f3 70 0d 0e 0f 02 02", [0x01, 0x00]),
            ("01 02 01 5c f3 70 0d 0e 0f 02 02", [0x01, 0x02]),
        ] {
            assert_eq!(
                decode(&frame(0x2e, payload)).unwrap(),
                Packet::ConnectedDevices {
                    header,
                    devices: vec![ConnectedDevice {
                        address: addr(HOST),
                        info: [0x02, 0x02]
                    }]
                }
            );
        }
    }

    #[test]
    fn the_same_host_reads_identically_from_reversed_and_forward_layouts() {
        let Packet::AudioSource { address: a, .. } =
            decode(&frame(0x0e, "0f 0e 0d 70 f3 5c 02")).unwrap()
        else {
            panic!("not an audio source");
        };
        let Packet::ConnectedDevices { devices, .. } =
            decode(&frame(0x2e, "01 00 01 5c f3 70 0d 0e 0f 02 02")).unwrap()
        else {
            panic!("not a device list");
        };
        assert_eq!(a, devices[0].address);
        assert_eq!(a.to_string(), HOST);
    }

    #[test]
    fn info_byte_0_reads_the_listed_host_link_state() {
        // Reports with the Mac FC:B2:14:0A:0B:0C first, this host second,
        // each lined up against the Mac's own connection log.
        for (payload, mac_up, host_up) in [
            // Shortly after the Mac logs Conn Disconnected.
            (
                "01 01 02 fc b2 14 0a 0b 0c 00 15 5c f3 70 0d 0e 0f 02 03",
                false,
                true,
            ),
            // The Mac connecting again.
            (
                "01 01 02 fc b2 14 0a 0b 0c 01 05 5c f3 70 0d 0e 0f 02 03",
                false,
                true,
            ),
            // Just before the Mac logs Conn Connected.
            (
                "01 00 02 fc b2 14 0a 0b 0c 02 17 5c f3 70 0d 0e 0f 01 01",
                true,
                false,
            ),
            // Both links up, the state right before a drop.
            (
                "01 02 02 fc b2 14 0a 0b 0c 02 15 5c f3 70 0d 0e 0f 02 03",
                true,
                true,
            ),
        ] {
            let Packet::ConnectedDevices { devices, .. } = decode(&frame(0x2e, payload)).unwrap()
            else {
                panic!("not a device list");
            };
            assert_eq!(devices[0].is_link_up(), mac_up, "{payload}");
            assert_eq!(devices[1].is_link_up(), host_up, "{payload}");
        }
    }

    #[test]
    fn a_short_device_list_keeps_the_complete_entries() {
        let pkt = decode(&frame(0x2e, "01 00 02 5c f3 70 0d 0e 0f 02 02 aa bb")).unwrap();
        let Packet::ConnectedDevices { devices, .. } = pkt else {
            panic!("not a device list");
        };
        assert_eq!(devices.len(), 1);
        assert_eq!(decode(&frame(0x2e, "01 00")), Err(DecodeError::Truncated));
    }

    #[test]
    fn ownership_control_matches_librepods_both_ways() {
        // docs/control_commands.md: 0x06 owns connection, 01 own, 00 not.
        let own = hex("04 00 04 00 09 00 06 01 00 00 00");
        let not = hex("04 00 04 00 09 00 06 00 00 00 00");
        assert_eq!(encode_owns_connection(true), own);
        assert_eq!(encode_owns_connection(false), not);
        assert_eq!(
            decode(&own).unwrap(),
            Packet::Control(ControlState::OwnsConnection(true))
        );
        assert_eq!(
            decode(&not).unwrap(),
            Packet::Control(ControlState::OwnsConnection(false))
        );
    }

    #[test]
    fn hijack_matches_the_capture_in_librepods_history() {
        let target = addr("AA:BB:CC:DD:EE:FF");
        let captured = hex(
            "620001E54A6C6F63616C73636F7265306446726561736F6E4848696A61636B763251617564696F\
             526F7574696E6753636F7265312D015F617564696F526F7574696E675365744F776E6572736869\
             70546F46616C7365014B72656D6F746573636F7265A5",
        );
        let expected = cat(&[
            &[0x04, 0x00, 0x04, 0x00, 0x10, 0x00],
            &[0xFF, 0xEE, 0xDD, 0xCC, 0xBB, 0xAA],
            &captured,
        ]);
        assert_eq!(encode_hijack_v2(target), expected);
        assert_eq!(expected.len(), 6 + 106, "LibrePods allocates 106 bytes");
    }

    #[test]
    fn new_tipi_matches_librepods_create_add_tipi_device_packet() {
        let target = addr("AA:BB:CC:DD:EE:FF");
        let local = addr(HOST);
        let expected = cat(&[
            &[0x04, 0x00, 0x04, 0x00, 0x10, 0x00],
            &[0xFF, 0xEE, 0xDD, 0xCC, 0xBB, 0xAA],
            &[0x52, 0x00],
            &[0x01, 0xE5],
            &[0x48],
            b"idleTime",
            &[0x08, 0x47],
            b"newTipi",
            &[0x01, 0x49],
            b"btAddress",
            &[0x51],
            HOST.as_bytes(),
            &[0x46],
            b"btName",
            &[0x47],
            b"Android",
            &[0x50],
            b"nearbyAudioScore",
            &[0x0E],
        ]);
        assert_eq!(encode_new_tipi(target, local, "Android"), expected);
        assert_eq!(expected.len(), 6 + 90);
    }

    #[test]
    fn new_device_media_info_matches_librepods() {
        let target = addr("AA:BB:CC:DD:EE:FF");
        let expected = cat(&[
            &[0x04, 0x00, 0x04, 0x00, 0x10, 0x00],
            &[0xFF, 0xEE, 0xDD, 0xCC, 0xBB, 0xAA],
            &[0x6C, 0x00],
            &[0x01, 0xE5, 0x4A],
            b"playingApp",
            &[0x42],
            b"NA",
            &[0x52],
            b"hostStreamingState",
            &[0x42],
            b"NO",
            &[0x49],
            b"btAddress",
            &[0x51],
            HOST.as_bytes(),
            &[0x46],
            b"btName",
            &[0x47],
            b"Android",
            &[0x58],
            b"otherDevice",
            b"AudioCategory",
            &[0x30, 0x64],
        ]);
        assert_eq!(
            encode_media_info_new_device(target, addr(HOST), "Android"),
            expected
        );
        assert_eq!(expected.len(), 6 + 116);
    }

    /// LibrePods' createMediaInformationPacket, with its two OPACK slips
    /// corrected: the `btName` key lacks its 0x46 tag and "YES" is tagged as a
    /// two-byte string (0x42). It also zero-pads to a fixed buffer; auris sends
    /// the exact body length instead.
    #[test]
    fn streaming_media_info_matches_librepods_with_opack_tags_corrected() {
        let target = addr("AA:BB:CC:DD:EE:FF");
        let body = cat(&[
            &[0x01, 0xE5, 0x4A],
            b"PlayingApp",
            &[0x56],
            b"com.google.ios.youtube",
            &[0x52],
            b"HostStreamingState",
            &[0x43],
            b"YES",
            &[0x49],
            b"btAddress",
            &[0x51],
            HOST.as_bytes(),
            &[0x46],
            b"btName",
            &[0x43],
            b"Mac",
            &[0x58],
            b"otherDevice",
            b"AudioCategory",
            &[0x31, 0x2D, 0x01],
        ]);
        let expected = cat(&[
            &[0x04, 0x00, 0x04, 0x00, 0x10, 0x00],
            &[0xFF, 0xEE, 0xDD, 0xCC, 0xBB, 0xAA],
            &(body.len() as u16).to_le_bytes(),
            &body,
        ]);
        assert_eq!(
            encode_media_info(
                target,
                addr(HOST),
                "com.google.ios.youtube",
                true,
                DEFAULT_BT_NAME
            ),
            expected
        );
        let stopped = encode_media_info(target, addr(HOST), "NA", false, "Mac");
        assert!(stopped.windows(3).any(|w| w == [0x42, b'N', b'O']));
    }

    #[test]
    fn long_strings_are_cut_at_a_character_boundary() {
        let mut out = Vec::new();
        opack_str(&mut out, &"é".repeat(20));
        assert_eq!(out[0], 0x40 + 32);
        assert_eq!(out.len(), 33);
        assert!(std::str::from_utf8(&out[1..]).is_ok());
    }

    #[test]
    fn relayed_ownership_request_decodes_with_its_sender() {
        let mut relayed = encode_hijack_v2(addr(HOST));
        relayed[4] = 0x11;
        // The relay carries the sender where the request carried the target.
        relayed[6..12].copy_from_slice(&[0x01, 0x02, 0x03, 0x04, 0x05, 0x06]);
        let Packet::SmartRouting { sender, body } = decode(&relayed).unwrap() else {
            panic!("not smart routing");
        };
        assert_eq!(sender, addr("06:05:04:03:02:01"));
        assert!(smart_routing_requests_yield(&body));
        assert!(!smart_routing_requests_yield(
            b"\x01\xe1\x46reason\x48Hijackv2"
        ));
        assert_eq!(decode(&relayed[..13]), Err(DecodeError::Truncated));
    }
}
