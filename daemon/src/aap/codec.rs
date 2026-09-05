//! Pure AAP encode/decode over `&[u8]`. This module never sees a socket, and
//! `session.rs` never parses bytes.
//!
//! Framing is SEQPACKET: one `recv()` is exactly one message, so there is no
//! re-framing and no length prefix to honour. Unknown opcodes decode to
//! [`Packet::Unknown`] and are never an error.

use super::opcode as op;
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlState {
    /// Noise control mode.
    NoiseControl(NoiseControlMode),
    /// Conversational awareness on/off.
    ConversationalAwareness(bool),
    /// Adaptive transparency level 0-100.
    AdaptiveLevel(u8),
    /// A control identifier this version does not model.
    Other {
        /// Control identifier byte.
        id: u8,
        /// Raw value byte.
        value: u8,
    },
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
            Ok(Packet::Control(decode_control(payload[0], payload[1])))
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
        _ => Ok(Packet::Unknown {
            opcode,
            payload: payload.to_vec(),
        }),
    }
}

fn decode_control(id: u8, value: u8) -> ControlState {
    match id {
        op::CTL_NOISE_CONTROL => match NoiseControlMode::from_wire(value) {
            Some(m) => ControlState::NoiseControl(m),
            None => ControlState::Other { id, value },
        },
        op::CTL_CONV_AWARENESS => ControlState::ConversationalAwareness(value == 0x01),
        op::CTL_ADAPTIVE_LEVEL => ControlState::AdaptiveLevel(value.min(100)),
        _ => ControlState::Other { id, value },
    }
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

fn encode_control(id: u8, value: u8) -> Vec<u8> {
    let mut v = Vec::with_capacity(11);
    v.extend_from_slice(&op::PREFIX);
    v.extend_from_slice(&op::OP_CONTROL.to_le_bytes());
    v.push(id);
    v.push(value);
    v.extend_from_slice(&[0x00, 0x00, 0x00]);
    v
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
}
