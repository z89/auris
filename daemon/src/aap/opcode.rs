//! Wire constants for the Apple Accessory Protocol, transcribed from the frozen
//! contract in `docs/plans/CONTRACT.md`. Constants only: no code is copied from
//! any prior-art project.

/// Every AAP message from/to the accessory is prefixed with these four bytes,
/// followed by a little-endian u16 opcode.
pub const PREFIX: [u8; 4] = [0x04, 0x00, 0x04, 0x00];

/// The accessory answers the handshake with this prefix instead.
pub const ACK_PREFIX: [u8; 4] = [0x01, 0x00, 0x04, 0x00];

/// Length of prefix + opcode. Shorter packets are rejected.
pub const HEADER_LEN: usize = 6;

// ---------------------------------------------------------------------------
// Opcodes
// ---------------------------------------------------------------------------

/// Battery notification (accessory -> host).
pub const OP_BATTERY: u16 = 0x0004;
/// In-ear detection notification (accessory -> host).
pub const OP_EAR_DETECTION: u16 = 0x0006;
/// Control write, and the accessory's echo of the resulting state.
pub const OP_CONTROL: u16 = 0x0009;
/// Subscribe to notifications (host -> accessory).
pub const OP_REQUEST_NOTIFICATIONS: u16 = 0x000F;
/// Device metadata: NUL-separated strings. The accessory pushes this
/// unsolicited after the handshake; it cannot be requested.
pub const OP_METADATA: u16 = 0x001D;
/// Feature-negotiation acknowledgement (accessory -> host).
pub const OP_FEATURES_ACK: u16 = 0x002B;
/// Conversational-awareness speech event (accessory -> host). Sent while the
/// feature is ON and the wearer talks; carries a volume-duck level, not the
/// on/off state. State lives on control id 0x28.
pub const OP_CONV_AWARENESS_EVENT: u16 = 0x004B;
/// Feature negotiation (host -> accessory).
pub const OP_SET_FEATURES: u16 = 0x004D;

// ---------------------------------------------------------------------------
// Control identifiers (byte 6 of an OP_CONTROL packet)
// ---------------------------------------------------------------------------

/// Noise control: 01 off, 02 anc, 03 transparency, 04 adaptive.
pub const CTL_NOISE_CONTROL: u8 = 0x0D;
/// Conversational awareness: 01 on, 02 off.
pub const CTL_CONV_AWARENESS: u8 = 0x28;
/// Adaptive transparency level: 0-100.
pub const CTL_ADAPTIVE_LEVEL: u8 = 0x2E;

// ---------------------------------------------------------------------------
// Static packets
// ---------------------------------------------------------------------------

/// Opening handshake (host -> accessory), sent first on a fresh socket.
pub const HANDSHAKE: [u8; 16] = [
    0x00, 0x00, 0x04, 0x00, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// Set-features, default variant (14 bytes, `0xd7`): header, opcode, `0xd7`,
/// then seven zero bytes. Every known implementation sends 14 bytes here; a
/// 13-byte write is silently ignored and the accessory never starts
/// notifying.
pub const SET_FEATURES_D7: [u8; 14] = [
    0x04, 0x00, 0x04, 0x00, 0x4d, 0x00, 0xd7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// Set-features, alternate variant (14 bytes, `0x0e`), used by AlwxSin's Go
/// daemon and reported working for product 0x201B among others. Forced with
/// `AURISD_FEATURES=alt`; otherwise reached by automatic fallback.
pub const SET_FEATURES_ALT: [u8; 14] = [
    0x04, 0x00, 0x04, 0x00, 0x4d, 0x00, 0x0e, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// Subscribe to every notification class.
pub const REQUEST_NOTIFICATIONS: [u8; 11] = [
    0x04, 0x00, 0x04, 0x00, 0x0f, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff,
];

/// The narrower first subscribe AlwxSin's daemon sends before the full one,
/// used only on the alternate ordering.
pub const REQUEST_NOTIFICATIONS_ALT: [u8; 10] =
    [0x04, 0x00, 0x04, 0x00, 0x0f, 0x00, 0xff, 0xff, 0xef, 0xff];

/// Bytes of an [`OP_METADATA`] payload to skip before the NUL-separated
/// strings begin. The strings start at frame offset 11: the 5-byte prefix
/// `04 00 04 00 1d`, then six bytes that are not part of any string, of
/// which one is consumed by the 6-byte header framing used here.
pub const METADATA_SKIP: usize = 5;

/// L2CAP PSM the accessory listens on for AAP.
pub const AAP_PSM: u16 = 0x1001;
