//! `aurisd`: an AirPods daemon for BlueZ.
//!
//! The daemon opens an Apple Accessory Protocol (AAP) L2CAP link to a paired
//! accessory once BlueZ reports it connected, and publishes what it learns to
//! `$XDG_RUNTIME_DIR/aurisd/state.json`. Control commands arrive on
//! `ctl.sock` as line-delimited JSON. Both formats are frozen in
//! `docs/plans/CONTRACT.md`.
//!
//! The daemon never initiates a Bluetooth connection: it only reacts to
//! BlueZ's `Connected` property.

pub mod aap;
pub mod bluez;
pub mod cache;
pub mod config;
pub mod ctl_proto;
pub mod ctl_server;
pub mod models;
pub mod state;
pub mod store;
pub mod writer;

/// Current time as RFC3339 with the local UTC offset, second resolution.
pub fn now_rfc3339() -> String {
    chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

#[cfg(test)]
mod tests {
    #[test]
    fn timestamp_is_rfc3339_with_offset() {
        let ts = super::now_rfc3339();
        assert!(chrono::DateTime::parse_from_rfc3339(&ts).is_ok(), "{ts}");
        assert_eq!(ts.len(), 25, "expected YYYY-MM-DDTHH:MM:SS+HH:MM, got {ts}");
    }
}
