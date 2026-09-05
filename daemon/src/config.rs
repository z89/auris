//! XDG paths and the optional `~/.config/aurisd/config.toml`.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Which physical bud the accessory calls "primary" in an ear-detection
/// packet. v0.1 assumes left; the contract's 0x0006 payload does not say.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PrimaryBud {
    /// Primary byte describes the left bud.
    #[default]
    Left,
    /// Primary byte describes the right bud.
    Right,
}

/// Contents of `config.toml`. Every field is optional; a missing file is fine.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// BD_ADDR to pin to, instead of auto-detecting.
    pub device: Option<String>,
    /// Which bud the primary byte of an ear-detection packet describes.
    pub primary_bud: PrimaryBud,
}

impl Config {
    /// Load the config file, or return defaults if it is missing. A malformed
    /// file is reported to the caller rather than silently ignored.
    pub fn load() -> anyhow::Result<Self> {
        let Some(path) = config_path() else {
            return Ok(Self::default());
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => Ok(toml::from_str(&text)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(anyhow::Error::new(e).context(format!("reading {}", path.display()))),
        }
    }
}

/// `$XDG_CONFIG_HOME/aurisd/config.toml`, else `$HOME/.config/...`.
pub fn config_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return Some(Path::new(&dir).join("aurisd/config.toml"));
    }
    let home = std::env::var_os("HOME").filter(|v| !v.is_empty())?;
    Some(Path::new(&home).join(".config/aurisd/config.toml"))
}

/// Runtime directory: the `--runtime-dir` override if given, else
/// `$XDG_RUNTIME_DIR/aurisd`, else `/run/user/<uid>/aurisd`.
///
/// There is deliberately no `/tmp` fallback. The DMS plugin only ever looks at
/// `$XDG_RUNTIME_DIR/aurisd/state.json`, so a daemon that quietly relocated
/// itself under `/tmp` would look alive while the widget stayed empty. If
/// `/run/user/<uid>` is missing, `ensure_runtime_dir` fails at startup instead.
pub fn runtime_dir(override_dir: Option<&Path>) -> PathBuf {
    if let Some(d) = override_dir {
        return d.to_path_buf();
    }
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        return Path::new(&dir).join("aurisd");
    }
    // SAFETY: getuid() is always safe; it cannot fail and touches no memory.
    let uid = unsafe { libc::getuid() };
    PathBuf::from(format!("/run/user/{uid}/aurisd"))
}

/// Create the runtime directory (mode 0700), refusing to invent its parent.
///
/// Called once at daemon startup. A missing parent means there is no XDG
/// runtime directory for this user at all (no logind session, or a service
/// started without one), which is a configuration fault worth reporting
/// plainly rather than working around.
pub fn ensure_runtime_dir(dir: &Path) -> anyhow::Result<()> {
    if let Some(parent) = dir.parent() {
        if !parent.is_dir() {
            anyhow::bail!(
                "{} does not exist, so {} cannot be created. Set XDG_RUNTIME_DIR, \
                 or start aurisd from a logind user session that provides one.",
                parent.display(),
                dir.display()
            );
        }
    }
    std::fs::create_dir_all(dir)
        .map_err(|e| anyhow::Error::new(e).context(format!("creating {}", dir.display())))?;
    std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))
        .map_err(|e| anyhow::Error::new(e).context(format!("chmod 0700 {}", dir.display())))?;
    Ok(())
}

/// Path of the state file inside a runtime directory.
pub fn state_path(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join("state.json")
}

/// Path of the control socket inside a runtime directory.
pub fn socket_path(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join("ctl.sock")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_wins() {
        let p = runtime_dir(Some(Path::new("/tmp/whatever")));
        assert_eq!(p, PathBuf::from("/tmp/whatever"));
        assert_eq!(state_path(&p), PathBuf::from("/tmp/whatever/state.json"));
        assert_eq!(socket_path(&p), PathBuf::from("/tmp/whatever/ctl.sock"));
    }

    #[test]
    fn ensure_runtime_dir_rejects_a_missing_parent() {
        let e = ensure_runtime_dir(Path::new("/nonexistent-aurisd-test/aurisd"))
            .expect_err("a missing parent must be an error");
        assert!(e.to_string().contains("/nonexistent-aurisd-test"), "{e}");
    }

    #[test]
    fn parses_a_full_config() {
        let cfg: Config =
            toml::from_str("device = \"AC:DE:48:00:11:22\"\nprimary_bud = \"right\"\n").unwrap();
        assert_eq!(cfg.device.as_deref(), Some("AC:DE:48:00:11:22"));
        assert_eq!(cfg.primary_bud, PrimaryBud::Right);
    }

    #[test]
    fn empty_config_is_default() {
        let cfg: Config = toml::from_str("").unwrap();
        assert!(cfg.device.is_none());
        assert_eq!(cfg.primary_bud, PrimaryBud::Left);
    }
}
