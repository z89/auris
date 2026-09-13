//! Typed AirPods settings shared by the control protocol, AAP codec, and
//! published snapshot.

use serde::{Deserialize, Serialize};

use crate::state::NoiseControlMode;

/// Which AirPod supplies microphone audio.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MicrophoneMode {
    /// Let the AirPods choose a microphone automatically.
    Auto,
    /// Always use the left AirPod.
    Left,
    /// Always use the right AirPod.
    Right,
}

/// Time allowed between presses of the stem.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PressSpeed {
    /// Firmware default.
    Default,
    /// Accept a slower double press.
    Slower,
    /// Accept the slowest double press.
    Slowest,
}

/// Time the stem must be held.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HoldDuration {
    /// Firmware default.
    Default,
    /// Trigger after a shorter hold.
    Shorter,
    /// Trigger after the shortest hold.
    Shortest,
}

/// Assignment of single and double stem presses during a call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallControls {
    /// Single press mutes; double press hangs up.
    MuteOnceHangupTwice,
    /// Single press hangs up; double press mutes.
    HangupOnceMuteTwice,
}

/// One setting write accepted by the daemon control socket.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "key", content = "value", rename_all = "snake_case")]
pub enum SettingCommand {
    /// Select the microphone side.
    Microphone(MicrophoneMode),
    /// Change stem press speed.
    PressSpeed(PressSpeed),
    /// Change press-and-hold duration.
    HoldDuration(HoldDuration),
    /// Modes included in the press-and-hold listening-mode cycle.
    ListeningModeCycle(Vec<NoiseControlMode>),
    /// Assign mute and hang-up to single/double presses.
    CallControls(CallControls),
    /// Enable or disable Personalized Volume.
    PersonalizedVolume(bool),
}

impl SettingCommand {
    /// Stable state.json key for report sequencing and UI correlation.
    pub const fn key(&self) -> &'static str {
        match self {
            Self::Microphone(_) => "microphone",
            Self::PressSpeed(_) => "press_speed",
            Self::HoldDuration(_) => "hold_duration",
            Self::ListeningModeCycle(_) => "listening_mode_cycle",
            Self::CallControls(_) => "call_controls",
            Self::PersonalizedVolume(_) => "personalized_volume",
        }
    }

    /// Validate invariants not expressible through serde enum decoding.
    pub fn validate(&self) -> Result<(), String> {
        if let Self::ListeningModeCycle(modes) = self {
            if !(2..=4).contains(&modes.len()) {
                return Err("listening mode cycle must contain between 2 and 4 modes".into());
            }
            for (index, mode) in modes.iter().enumerate() {
                if modes[..index].contains(mode) {
                    return Err("listening mode cycle must not contain duplicate modes".into());
                }
            }
        }
        Ok(())
    }
}

/// Settings confirmed by control-state notifications from the accessory.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeviceSettings {
    /// Confirmed microphone selection.
    pub microphone: Option<MicrophoneMode>,
    /// Confirmed stem press speed.
    pub press_speed: Option<PressSpeed>,
    /// Confirmed press-and-hold duration.
    pub hold_duration: Option<HoldDuration>,
    /// Confirmed listening-mode cycle.
    pub listening_mode_cycle: Option<Vec<NoiseControlMode>>,
    /// Confirmed call-control assignment.
    pub call_controls: Option<CallControls>,
    /// Confirmed Personalized Volume state.
    pub personalized_volume: Option<bool>,
}

/// Validate a Bluetooth name before constructing an AAP rename packet.
pub fn validate_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("name must not be blank".into());
    }
    if name.len() > u8::MAX as usize {
        return Err("name must be at most 255 UTF-8 bytes".into());
    }
    if name.chars().any(char::is_control) {
        return Err("name must not contain control characters".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycle_requires_distinct_modes() {
        assert!(SettingCommand::ListeningModeCycle(vec![
            NoiseControlMode::Anc,
            NoiseControlMode::Transparency,
        ])
        .validate()
        .is_ok());
        assert!(
            SettingCommand::ListeningModeCycle(vec![NoiseControlMode::Anc])
                .validate()
                .is_err()
        );
        assert!(SettingCommand::ListeningModeCycle(vec![
            NoiseControlMode::Anc,
            NoiseControlMode::Anc,
        ])
        .validate()
        .is_err());
    }

    #[test]
    fn names_are_bounded_by_utf8_bytes() {
        assert!(validate_name("Auris Pods").is_ok());
        assert!(validate_name("   ").is_err());
        assert!(validate_name("Pods\n").is_err());
        assert!(validate_name(&"a".repeat(255)).is_ok());
        assert!(validate_name(&"a".repeat(256)).is_err());
        assert!(validate_name(&"é".repeat(127)).is_ok());
        assert!(validate_name(&"é".repeat(128)).is_err());
    }
}
