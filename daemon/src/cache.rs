//! Last-known battery cache.
//!
//! The runtime directory is wiped whenever the daemon stops, so the last case
//! level would be lost on every restart or reboot. This keeps the battery
//! block in `$CACHE_DIRECTORY` (systemd) or `$XDG_CACHE_HOME/aurisd`, and
//! loads it back as stale, absent cells at startup.

use std::fs;
use std::path::{Path, PathBuf};

use crate::state::Battery;

/// Where the cache lives, or `None` when no cache directory can be derived.
pub fn cache_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CACHE_DIRECTORY") {
        return Some(PathBuf::from(dir).join("battery.json"));
    }
    dirs::cache_dir().map(|d| d.join("aurisd").join("battery.json"))
}

/// Read the cache. Every cell comes back `present: false`, not charging,
/// with `stale: true`; only levels and `last_seen` are trusted.
pub fn load(path: &Path) -> Option<Battery> {
    let text = fs::read_to_string(path).ok()?;
    let mut b: Battery = serde_json::from_str(&text).ok()?;
    b.stale = true;
    for cell in [&mut b.left, &mut b.right, &mut b.case] {
        cell.present = false;
        cell.charging = false;
    }
    Some(b)
}

/// Write the cache atomically (tmp file in the same directory, then rename).
pub fn save(path: &Path, battery: &Battery) -> std::io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("cache path has no parent"))?;
    fs::create_dir_all(dir)?;
    let tmp = dir.join(".battery.json.tmp");
    fs::write(&tmp, serde_json::to_vec(battery)?)?;
    fs::rename(&tmp, path)
}

/// True when no level or `last_seen` differs, i.e. nothing worth re-saving.
pub fn same_readings(a: &Battery, b: &Battery) -> bool {
    [(&a.left, &b.left), (&a.right, &b.right), (&a.case, &b.case)]
        .iter()
        .all(|(x, y)| x.level == y.level && x.last_seen == y.last_seen)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Cell;

    #[test]
    fn round_trip_comes_back_absent_and_stale() {
        let dir = std::env::temp_dir().join(format!("aurisd-cache-{}", std::process::id()));
        let path = dir.join("battery.json");
        let b = Battery {
            stale: false,
            left: Cell {
                level: Some(80),
                charging: true,
                present: true,
                last_seen: Some("t1".into()),
            },
            right: Cell::default(),
            case: Cell {
                level: Some(62),
                charging: false,
                present: true,
                last_seen: Some("t2".into()),
            },
        };
        save(&path, &b).unwrap();
        let back = load(&path).unwrap();
        assert!(back.stale);
        assert_eq!(back.case.level, Some(62));
        assert_eq!(back.case.last_seen.as_deref(), Some("t2"));
        assert!(!back.left.present && !back.left.charging);
        assert!(same_readings(&b, &back));
        assert!(!dir.join(".battery.json.tmp").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_file_is_none() {
        assert!(load(Path::new("/nonexistent/aurisd/battery.json")).is_none());
    }
}
