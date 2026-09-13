//! Last-known battery cache.
//!
//! The runtime directory is wiped whenever the daemon stops, so the last case
//! level would be lost on every restart or reboot. This keeps the battery
//! block in `$CACHE_DIRECTORY` (systemd) or `$XDG_CACHE_HOME/aurisd`, and
//! loads it back as stale, absent cells at startup.

use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use crate::state::Battery;

#[derive(serde::Serialize, serde::Deserialize)]
struct CachedBattery {
    address: String,
    battery: Battery,
}

/// Where the cache lives, or `None` when no cache directory can be derived.
pub fn cache_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CACHE_DIRECTORY") {
        return Some(PathBuf::from(dir).join("battery.json"));
    }
    dirs::cache_dir().map(|d| d.join("aurisd").join("battery.json"))
}

/// Read the cache. Every cell comes back `present: false`, not charging,
/// with `stale: true`; levels, `last_seen`, and historical charging are kept.
pub fn load(path: &Path, address: &str) -> Option<Battery> {
    let text = fs::read_to_string(path).ok()?;
    let cached: CachedBattery = serde_json::from_str(&text).ok()?;
    // Legacy, unbound caches cannot safely be attributed to this accessory.
    if address.is_empty() || cached.address != address {
        return None;
    }
    let mut b = cached.battery;
    b.stale = true;
    for cell in [&mut b.left, &mut b.right, &mut b.case] {
        cell.present = false;
        cell.fresh = false;
        cell.charging = false;
    }
    Some(b)
}

/// Write the cache atomically (tmp file in the same directory, then rename).
pub fn save(path: &Path, address: &str, battery: &Battery) -> std::io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("cache path has no parent"))?;
    fs::create_dir_all(dir)?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(std::io::Error::other)?
        .as_nanos();
    let tmp = dir.join(format!(".battery-{}-{nonce}.tmp", std::process::id()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    let result = (|| {
        file.write_all(&serde_json::to_vec(&CachedBattery {
            address: address.to_owned(),
            battery: battery.clone(),
        })?)?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// True when no retained reading differs, i.e. nothing worth re-saving.
pub fn same_readings(a: &Battery, b: &Battery) -> bool {
    [(&a.left, &b.left), (&a.right, &b.right), (&a.case, &b.case)]
        .iter()
        .all(|(x, y)| {
            x.level == y.level
                && x.last_seen == y.last_seen
                && x.last_known_charging == y.last_known_charging
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Cell;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn round_trip_comes_back_absent_and_stale() {
        let dir = std::env::temp_dir().join(format!("aurisd-cache-{}", std::process::id()));
        let path = dir.join("battery.json");
        let b = Battery {
            stale: false,
            left: Cell {
                level: Some(80),
                charging: true,
                last_known_charging: Some(true),
                present: true,
                last_seen: Some("t1".into()),
                ..Cell::default()
            },
            right: Cell::default(),
            case: Cell {
                level: Some(62),
                charging: false,
                last_known_charging: Some(false),
                present: true,
                last_seen: Some("t2".into()),
                ..Cell::default()
            },
        };
        save(&path, "AC:DE:48:00:11:22", &b).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let back = load(&path, "AC:DE:48:00:11:22").unwrap();
        assert!(load(&path, "AC:DE:48:00:11:23").is_none());
        assert!(back.stale);
        assert_eq!(back.case.level, Some(62));
        assert_eq!(back.case.last_seen.as_deref(), Some("t2"));
        assert!(!back.left.present && !back.left.charging);
        assert_eq!(back.left.last_known_charging, Some(true));
        assert_eq!(back.case.last_known_charging, Some(false));
        assert!(same_readings(&b, &back));
        assert!(!dir.join(".battery.json.tmp").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_file_is_none() {
        assert!(load(Path::new("/nonexistent/aurisd/battery.json"), "").is_none());
    }

    #[test]
    fn unbound_legacy_cache_is_not_attributed_to_a_device() {
        let dir = std::env::temp_dir().join(format!("aurisd-old-cache-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("battery.json");
        fs::write(
            &path,
            r#"{"stale":false,"left":{"level":80,"charging":true,"present":true,"last_seen":"t1"},"right":{"level":null,"charging":false,"present":false,"last_seen":null},"case":{"level":62,"charging":true,"present":true,"last_seen":"t2"}}"#,
        )
        .unwrap();

        assert!(load(&path, "AC:DE:48:00:11:22").is_none());

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cache_equality_includes_history_but_not_live_presence() {
        let mut original = Battery::default();
        original.left.level = Some(80);
        original.left.last_seen = Some("t1".into());
        original.left.last_known_charging = Some(true);

        let mut only_live_state_changed = original.clone();
        only_live_state_changed.left.present = true;
        only_live_state_changed.left.charging = true;
        only_live_state_changed.stale = false;
        assert!(same_readings(&original, &only_live_state_changed));

        let mut history_changed = original.clone();
        history_changed.left.last_known_charging = Some(false);
        assert!(!same_readings(&original, &history_changed));
    }
}
