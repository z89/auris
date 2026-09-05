//! The line-delimited JSON control protocol, shared by the daemon and the CLI.
//!
//! One JSON object per line in each direction. Frozen in `CONTRACT.md`.

use serde::{Deserialize, Serialize};

use crate::state::{NoiseControlMode, Snapshot};

/// A request from `auris` to the daemon.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    /// `{"cmd":"set_noise_control","value":"anc"}`
    SetNoiseControl {
        /// Desired mode.
        value: NoiseControlMode,
    },
    /// `{"cmd":"set_conversational_awareness","value":true}`
    SetConversationalAwareness {
        /// On or off.
        value: bool,
    },
    /// `{"cmd":"set_adaptive_level","value":50}`
    SetAdaptiveLevel {
        /// 0-100.
        value: u8,
    },
    /// `{"cmd":"reconnect"}`
    Reconnect,
    /// `{"cmd":"status"}` — the reply is the state.json object itself.
    Status,
}

/// A reply from the daemon. `status` answers with the snapshot; everything
/// else answers `{"ok":true}` or `{"ok":false,"error":"..."}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum Response {
    /// The state.json document, verbatim.
    Status(Box<Snapshot>),
    /// Acknowledgement or failure.
    Ack {
        /// Whether the command succeeded.
        ok: bool,
        /// Human-readable reason when `ok` is false.
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

impl Response {
    /// `{"ok":true}`
    pub fn ok() -> Self {
        Self::Ack {
            ok: true,
            error: None,
        }
    }

    /// `{"ok":false,"error":"..."}`
    pub fn error(msg: impl Into<String>) -> Self {
        Self::Ack {
            ok: false,
            error: Some(msg.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_match_the_contract_wire_form() {
        let cases: [(Request, &str); 5] = [
            (
                Request::SetNoiseControl {
                    value: NoiseControlMode::Anc,
                },
                r#"{"cmd":"set_noise_control","value":"anc"}"#,
            ),
            (
                Request::SetConversationalAwareness { value: true },
                r#"{"cmd":"set_conversational_awareness","value":true}"#,
            ),
            (
                Request::SetAdaptiveLevel { value: 50 },
                r#"{"cmd":"set_adaptive_level","value":50}"#,
            ),
            (Request::Reconnect, r#"{"cmd":"reconnect"}"#),
            (Request::Status, r#"{"cmd":"status"}"#),
        ];
        for (req, wire) in cases {
            assert_eq!(serde_json::to_string(&req).unwrap(), wire);
            assert_eq!(serde_json::from_str::<Request>(wire).unwrap(), req);
        }
    }

    #[test]
    fn responses_match_the_contract_wire_form() {
        assert_eq!(
            serde_json::to_string(&Response::ok()).unwrap(),
            r#"{"ok":true}"#
        );
        assert_eq!(
            serde_json::to_string(&Response::error("not connected")).unwrap(),
            r#"{"ok":false,"error":"not connected"}"#
        );
    }

    #[test]
    fn status_response_round_trips_as_the_snapshot_itself() {
        let snap = Snapshot::example();
        let wire = serde_json::to_string(&Response::Status(Box::new(snap.clone()))).unwrap();
        let value: serde_json::Value = serde_json::from_str(&wire).unwrap();
        assert_eq!(
            value["schema"], 1,
            "status must be the bare state.json object"
        );
        match serde_json::from_str::<Response>(&wire).unwrap() {
            Response::Status(s) => assert_eq!(*s, snap),
            other => panic!("expected status, got {other:?}"),
        }
    }

    #[test]
    fn ack_is_not_mistaken_for_a_snapshot() {
        match serde_json::from_str::<Response>(r#"{"ok":false,"error":"boom"}"#).unwrap() {
            Response::Ack { ok, error } => {
                assert!(!ok);
                assert_eq!(error.as_deref(), Some("boom"));
            }
            other => panic!("expected ack, got {other:?}"),
        }
    }
}
