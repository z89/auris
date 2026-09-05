//! Debounced atomic writer for state.json.
//!
//! Atomic means: write a temp file in the *same* directory, fsync it, then
//! rename over the target. A reader using Quickshell's `FileView` therefore
//! never sees a half-written document.

use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

use tokio::sync::watch;
use tracing::{debug, warn};

use crate::state::Snapshot;

/// How long to coalesce a burst of updates before writing.
pub const DEBOUNCE: Duration = Duration::from_millis(100);

/// Write `snap` to `path` atomically.
pub fn write_atomic(path: &Path, snap: &Snapshot) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let tmp: PathBuf = dir.join(format!(
        ".{}.tmp",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("state.json")
    ));

    let body = serde_json::to_vec_pretty(snap)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    {
        let mut f: File = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(&body)?;
        f.write_all(b"\n")?;
        f.sync_all()?;
    }
    match fs::rename(&tmp, path) {
        Ok(()) => {
            // Durably record the rename itself. On tmpfs (the usual runtime
            // dir) this is a no-op and may even fail; either way it is not
            // worth failing the write over.
            if let Ok(d) = File::open(dir) {
                let _ = d.sync_all();
            }
            Ok(())
        }
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Watch the store and keep state.json up to date. Returns when every sender
/// has been dropped.
pub async fn run(mut rx: watch::Receiver<Snapshot>, path: PathBuf, cache: Option<PathBuf>) {
    let mut cached: Option<crate::state::Battery> = None;
    while rx.changed().await.is_ok() {
        // Coalesce whatever else arrives inside the debounce window.
        tokio::time::sleep(DEBOUNCE).await;
        let snap = rx.borrow_and_update().clone();
        match write_atomic(&path, &snap) {
            Ok(()) => debug!(path = %path.display(), "wrote state.json"),
            Err(e) => warn!(path = %path.display(), error = %e, "failed to write state.json"),
        }
        let Some(cache_path) = &cache else { continue };
        if cached
            .as_ref()
            .is_some_and(|c| crate::cache::same_readings(c, &snap.battery))
        {
            continue;
        }
        match crate::cache::save(cache_path, &snap.battery) {
            Ok(()) => cached = Some(snap.battery.clone()),
            Err(e) => {
                warn!(path = %cache_path.display(), error = %e, "failed to write battery cache")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_produces_parseable_json() {
        let dir = std::env::temp_dir().join(format!("aurisd-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        write_atomic(&path, &Snapshot::example()).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["schema"], 1);
        assert!(
            !dir.join(".state.json.tmp").exists(),
            "temp file must be renamed away"
        );
        fs::remove_dir_all(&dir).unwrap();
    }
}
