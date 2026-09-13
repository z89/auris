//! Opt-in proximity telemetry, never an audio connection policy.
//!
//! The protocol layout is documented in docs/BATTERY_AND_HANDOFF.md. Only
//! AirPods 4 ANC adverts resolved using the pinned accessory's IRK are used.
//! Discovery may transmit LE scan requests; it is not physically passive.

use std::{
    collections::HashSet,
    fs::OpenOptions,
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use aes::{
    cipher::{Block, BlockDecrypt, BlockEncrypt, KeyInit},
    Aes128,
};
use anyhow::{bail, Context};
use bluer::{
    AdapterEvent, Address, DeviceEvent, DeviceProperty, DiscoveryFilter, DiscoveryTransport,
};
use futures_util::{stream::SelectAll, StreamExt};
use serde::Deserialize;
use tracing::warn;

use crate::{
    aap::codec::{BatteryComponent, BatteryEntry},
    store::{Store, Update},
};

/// Optional bounded LE discovery. Disabled without explicit configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BleConfig {
    /// Enable the observer (requires a pinned paired device and keys).
    pub enabled: bool,
    /// Private JSON file with address, irk and encryption_key fields.
    pub key_file: Option<PathBuf>,
    /// Seconds of discovery in each interval.
    pub scan_seconds: u64,
    /// Seconds between the beginnings of discovery windows.
    pub interval_seconds: u64,
    /// Age beyond which an observed BLE cell is displayed as historical.
    pub freshness_seconds: u64,
}

impl Default for BleConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            key_file: None,
            scan_seconds: 8,
            interval_seconds: 30,
            freshness_seconds: 75,
        }
    }
}

impl BleConfig {
    /// Bound radio use and ensure the expiry policy spans a scan interval.
    pub fn validate(&self) -> anyhow::Result<()> {
        if !(2..=30).contains(&self.scan_seconds)
            || !(7..=300).contains(&self.interval_seconds)
            || !(9..=600).contains(&self.freshness_seconds)
            || self.interval_seconds.saturating_sub(self.scan_seconds) < 5
            || self.freshness_seconds.saturating_sub(self.interval_seconds) < self.scan_seconds
        {
            bail!("BLE requires scan_seconds 2–30, interval_seconds at least scan+5 and at most 300, freshness_seconds at least interval+scan and at most 600");
        }
        if self.enabled && self.key_file.as_ref().is_none_or(|p| !p.is_absolute()) {
            bail!("enabled BLE telemetry requires an absolute key_file path");
        }
        Ok(())
    }
}

// Intentionally neither Debug nor Serialize: secrets never enter diagnostics.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyDocument {
    address: String,
    irk: String,
    encryption_key: String,
}

struct Keys {
    irk: [u8; 16],
    encryption: [u8; 16],
}

fn key_bytes(value: &str) -> anyhow::Result<[u8; 16]> {
    if value.len() != 32 || !value.is_ascii() {
        bail!("BLE keys must each contain 32 hexadecimal digits");
    }
    let mut bytes = [0; 16];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[i * 2..i * 2 + 2], 16)
            .map_err(|_| anyhow::anyhow!("BLE key contains a non-hexadecimal digit"))?;
    }
    Ok(bytes)
}

fn load_keys(path: &std::path::Path, address: Address) -> anyhow::Result<Keys> {
    // Open first, then validate that very inode. O_NONBLOCK also prevents a
    // malicious FIFO from blocking before the regular-file check.
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .context("cannot open private BLE key file")?;
    let metadata = file.metadata()?;
    // SAFETY: geteuid reads the current process identity and cannot fail.
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
        || metadata.len() > 4096
    {
        bail!("BLE key file must be a regular file owned by this user, private (0600), and at most 4096 bytes");
    }
    let mut body = Vec::new();
    file.take(4097).read_to_end(&mut body)?;
    if body.len() > 4096 {
        bail!("BLE key file exceeds 4096 bytes");
    }
    let doc: KeyDocument = serde_json::from_slice(&body)
        .map_err(|_| anyhow::anyhow!("invalid BLE key file schema"))?;
    if doc.address.parse::<Address>().ok() != Some(address) {
        bail!("BLE key identity differs from the pinned device");
    }
    Ok(Keys {
        irk: key_bytes(&doc.irk)?,
        encryption: key_bytes(&doc.encryption_key)?,
    })
}

/// Bluetooth ah, using the little-endian IRK byte order exported by AAP.
/// An RPA matches an identity, but its short hash is not packet authentication.
fn resolves(address: Address, irk: &[u8; 16]) -> bool {
    let bytes = address.0; // Human/display order: prand then hash.
    if bytes[0] & 0xc0 != 0x40 {
        return false;
    }
    let mut key = *irk;
    key.reverse();
    let cipher = Aes128::new(&key.into());
    let mut block = Block::<Aes128>::default();
    block[13..].copy_from_slice(&bytes[..3]);
    cipher.encrypt_block(&mut block);
    block[13..] == bytes[3..]
}

fn cell(component: BatteryComponent, raw: u8) -> Option<BatteryEntry> {
    let level = raw & 0x7f;
    (level <= 100).then_some(BatteryEntry {
        component,
        level: Some(level),
        charging: raw & 0x80 != 0,
        present: true,
    })
}

fn decode(data: &[u8], key: &[u8; 16]) -> Option<Vec<BatteryEntry>> {
    // One 0x07 proximity message: 11 clear bytes plus one encrypted block.
    // Reject extra/short records and pairing-mode layouts instead of guessing.
    if data.len() != 27
        || data[0] != 7
        || usize::from(data[1]) != data.len() - 2
        || data[2] != 1
        || data[3..5] != [0x1b, 0x20]
    {
        return None;
    }
    let mut block = Block::<Aes128>::default();
    block.copy_from_slice(&data[11..]);
    // One CBC block with a zero IV is exactly AES block decryption.
    Aes128::new(key.into()).decrypt_block(&mut block);
    if block[1..=3]
        .iter()
        .any(|v| (v & 0x7f) > 100 && (v & 0x7f) != 127)
    {
        return None;
    }
    let primary_left = data[5] & 0x20 != 0;
    let mut entries = Vec::with_capacity(3);
    if let Some(e) = cell(
        BatteryComponent::Left,
        block[if primary_left { 1 } else { 2 }],
    ) {
        entries.push(e);
    }
    if let Some(e) = cell(
        BatteryComponent::Right,
        block[if primary_left { 2 } else { 1 }],
    ) {
        entries.push(e);
    }
    // Case telemetry is carried when a reporting bud is in the case. Do not
    // revive the encrypted cached case byte in other configurations.
    if data[5] & 0x40 != 0 {
        if let Some(e) = cell(BatteryComponent::Case, block[3]) {
            entries.push(e);
        }
    }
    (!entries.is_empty()).then_some(entries)
}

fn observe_change(store: &Store, identity: Address, keys: &Keys, event: DeviceEvent) {
    if let DeviceEvent::PropertyChanged(DeviceProperty::ManufacturerData(data)) = event {
        if let Some(entries) = data
            .get(&0x004c)
            .and_then(|data| decode(data, &keys.encryption))
        {
            store.apply(Update::BleBattery {
                address: identity.to_string(),
                entries,
            });
        }
    }
}

async fn scan(store: &Store, address: Address, keys: &Keys, seconds: u64) -> anyhow::Result<()> {
    let session = bluer::Session::new().await?;
    let adapter = session.default_adapter().await?;
    if !adapter.device(address)?.is_paired().await? {
        bail!("pinned device is not paired; BLE observation disabled for this window");
    }
    adapter
        .set_discovery_filter(DiscoveryFilter {
            transport: DiscoveryTransport::Le,
            duplicate_data: true,
            discoverable: false,
            ..Default::default()
        })
        .await?;
    let discovery = adapter.discover_devices().await?;
    tokio::pin!(discovery);
    let mut watched = HashSet::new();
    let mut changes = SelectAll::new();
    let deadline = tokio::time::sleep(Duration::from_secs(seconds));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            biased;
            _ = &mut deadline => break,
            event = discovery.next() => match event {
                Some(AdapterEvent::DeviceAdded(rpa)) if watched.len() < 16 && resolves(rpa, &keys.irk) && watched.insert(rpa) => {
                    // Do NOT read cached ManufacturerData on discovery. BlueZ
                    // also emits known, out-of-range devices at scan startup.
                    let dev = adapter.device(rpa)?;
                    let events = dev.events().await?;
                    changes.push(events.boxed());
                }
                None => break,
                _ => {}
            },
            Some(event) = changes.next(), if !changes.is_empty() => observe_change(store, address, keys, event),
        }
    }
    // Dropping the discovery stream releases only this client's discovery
    // token, not other apps' scans. Session teardown also clears our filter.
    Ok(())
}

/// Run opt-in discovery and independent expiry. Invalid setup fails closed.
pub async fn run(store: Arc<Store>, config: BleConfig, pinned: Option<Address>) {
    if !config.enabled {
        return;
    }
    let setup = (|| -> anyhow::Result<_> {
        config.validate()?;
        let address = pinned.context("BLE telemetry requires an explicitly pinned device")?;
        let keys = load_keys(
            config.key_file.as_deref().context("missing BLE key file")?,
            address,
        )?;
        Ok((address, keys))
    })();
    let (address, keys) = match setup {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "BLE telemetry not started");
            return;
        }
    };
    let observer = async {
        loop {
            let start = tokio::time::Instant::now();
            // Bound D-Bus setup as well as discovery. No retry inside a window.
            match tokio::time::timeout(
                Duration::from_secs(config.scan_seconds + 5),
                scan(&store, address, &keys, config.scan_seconds),
            )
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(e)) => warn!(error = %e, "BLE observation window failed"),
                Err(_) => warn!("BLE observation window timed out"),
            }
            tokio::time::sleep_until(start + Duration::from_secs(config.interval_seconds)).await;
        }
    };
    let expiry = async {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            store.apply(Update::ExpireBle {
                before: (chrono::Utc::now()
                    - chrono::Duration::seconds(config.freshness_seconds as i64))
                .to_rfc3339(),
            });
        }
    };
    tokio::join!(observer, expiry);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn advert(status: u8, values: [u8; 3]) -> Vec<u8> {
        // Synthetic protocol fixture, not a captured/hardware-verified packet.
        let mut out = vec![7, 25, 1, 0x1b, 0x20, status, 0, 0, 0, 0, 0];
        let mut block = Block::<Aes128>::default();
        block[1..4].copy_from_slice(&values);
        Aes128::new(&[0; 16].into()).encrypt_block(&mut block);
        out.extend_from_slice(&block);
        out
    }

    #[test]
    fn bluetooth_spec_ah_vector_and_wrong_identity() {
        // Bluetooth Core ah sample: IRK ec0234...7d9b, prand 708194,
        // hash 0dfbaa. AAP exports the IRK in reverse byte order.
        let irk = key_bytes("9b7d390aa610103405adc857a33402ec").unwrap();
        assert!(resolves("70:81:94:0D:FB:AA".parse().unwrap(), &irk));
        assert!(!resolves("70:81:94:0D:FB:AB".parse().unwrap(), &irk));
        assert!(!resolves("F0:81:94:0D:FB:AA".parse().unwrap(), &irk));
    }

    #[test]
    fn exact_zero_charging_and_primary_side_mapping() {
        let entries = decode(&advert(0x60, [0x80, 99, 0x80 | 45]), &[0; 16]).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].component, BatteryComponent::Left);
        assert_eq!(entries[0].level, Some(0));
        assert!(entries[0].charging);
        assert_eq!(entries[2].level, Some(45));
        let reversed = decode(&advert(0x40, [10, 80, 33]), &[0; 16]).unwrap();
        assert_eq!(reversed[0].level, Some(80));
        assert_eq!(reversed[1].level, Some(10));
    }

    #[test]
    fn unknown_is_not_a_fresh_reading_and_case_requires_reporting_bud() {
        let entries = decode(&advert(0x20, [30, 127, 90]), &[0; 16]).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].component, BatteryComponent::Left);
        assert!(decode(&advert(0x60, [127, 255, 127]), &[0; 16]).is_none());
    }

    #[test]
    fn malformed_wrong_model_pairing_and_invalid_percent_are_rejected() {
        let valid = advert(0x60, [30, 50, 90]);
        for len in 0..valid.len() {
            assert!(decode(&valid[..len], &[0; 16]).is_none());
        }
        for (index, value) in [(0, 8), (1, 24), (2, 0), (3, 0x14)] {
            let mut bad = valid.clone();
            bad[index] = value;
            assert!(decode(&bad, &[0; 16]).is_none());
        }
        assert!(decode(&advert(0x60, [101, 50, 90]), &[0; 16]).is_none());
    }

    #[test]
    fn configuration_is_opt_in_and_bounded() {
        let mut config = BleConfig::default();
        assert!(!config.enabled);
        assert!(config.validate().is_ok());
        config.enabled = true;
        assert!(config.validate().is_err());
        config.key_file = Some("/private/keys.json".into());
        assert!(config.validate().is_ok());
        config.scan_seconds = 300;
        assert!(config.validate().is_err());
        config.scan_seconds = u64::MAX;
        config.interval_seconds = u64::MAX;
        config.freshness_seconds = u64::MAX;
        assert!(config.validate().is_err());
    }

    #[test]
    fn only_new_manufacturer_events_refresh_cells_not_other_device_properties() {
        let address: Address = "AC:DE:48:00:11:22".parse().unwrap();
        let store = Store::new(
            crate::state::Snapshot::initial(&address.to_string()),
            crate::config::PrimaryBud::Auto,
        );
        let keys = Keys {
            irk: [0; 16],
            encryption: [0; 16],
        };
        observe_change(
            &store,
            address,
            &keys,
            DeviceEvent::PropertyChanged(DeviceProperty::Name("AirPods".into())),
        );
        assert!(store.snapshot().battery.stale);
        let manufacturer =
            std::collections::HashMap::from([(0x004c, advert(0x60, [60, 127, 0x80 | 80]))]);
        observe_change(
            &store,
            address,
            &keys,
            DeviceEvent::PropertyChanged(DeviceProperty::ManufacturerData(manufacturer)),
        );
        assert!(store.snapshot().battery.case.fresh);
        assert!(!store.snapshot().battery.right.fresh);
        store.apply(Update::ExpireBle {
            before: "2999-01-01T00:00:00Z".into(),
        });
        observe_change(
            &store,
            address,
            &keys,
            DeviceEvent::PropertyChanged(DeviceProperty::Name("AirPods".into())),
        );
        assert!(!store.snapshot().battery.case.fresh);
        assert_eq!(
            store.snapshot().battery.case.last_known_charging,
            Some(true)
        );
    }

    #[test]
    fn private_keys_reject_wrong_identity_permissions_symlinks_and_schema() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let directory =
            std::env::temp_dir().join(format!("auris-ble-keys-test-{}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("keys.json");
        let address = "AC:DE:48:00:11:22".parse().unwrap();
        let body = r#"{"address":"AC:DE:48:00:11:22","irk":"00000000000000000000000000000000","encryption_key":"00000000000000000000000000000000"}"#;
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(load_keys(&path, address).is_ok());
        assert!(load_keys(&path, "AC:DE:48:00:11:23".parse().unwrap()).is_err());
        let link = directory.join("link.json");
        symlink(&path, &link).unwrap();
        assert!(load_keys(&link, address).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load_keys(&path, address).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(&path, "bad secret content").unwrap();
        assert_eq!(
            load_keys(&path, address).err().unwrap().to_string(),
            "invalid BLE key file schema"
        );
        std::fs::write(&path, vec![b'x'; 4097]).unwrap();
        assert!(load_keys(&path, address).is_err());
        std::fs::remove_file(link).unwrap();
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }
}
