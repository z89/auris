//! XDG paths and the optional `~/.config/aurisd/config.toml`.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Which physical bud the accessory calls "primary" in an ear-detection
/// packet. The 0x0006 payload never says, and it is not a fixed property of the
/// hardware: whichever bud is doing the work becomes primary, so it changes
/// when you put one bud away. Pinning it to a side is therefore wrong half the
/// time, which is why [`PrimaryBud::Auto`] is the default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PrimaryBud {
    /// Work it out from which buds are out of the case. See `store::mutate`.
    #[default]
    Auto,
    /// Primary byte always describes the left bud.
    Left,
    /// Primary byte always describes the right bud.
    Right,
}

/// `[handoff]`: Apple multi-host switching over AAP.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HandoffConfig {
    /// Yield to other Apple hosts and take over on local playback.
    pub enabled: bool,
    /// Take over when a local MPRIS player starts playing.
    pub take_over_on_play: bool,
    /// `btName` announced to other hosts. Default `Mac`.
    pub bt_name: Option<String>,
    /// Reconnect once when an Apple host's fresh connection made the AirPods
    /// drop this host. Needs `enabled`. Default true.
    pub rejoin_after_eviction: bool,
}

impl Default for HandoffConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            take_over_on_play: true,
            bt_name: None,
            rejoin_after_eviction: true,
        }
    }
}

impl HandoffConfig {
    /// The configured `bt_name` if usable (1-32 bytes, no control
    /// characters), otherwise the default.
    pub fn bt_name(&self) -> &str {
        self.bt_name
            .as_deref()
            .filter(|n| {
                !n.trim().is_empty()
                    && n.len() <= crate::aap::codec::OPACK_SHORT_STRING
                    && !n.chars().any(char::is_control)
            })
            .unwrap_or(crate::aap::codec::DEFAULT_BT_NAME)
    }
}

/// `[autoconnect]`: page the AirPods when their BLE proximity advert appears.
///
/// BlueZ never pages a classic device on its own, so a host only gets the
/// AirPods when they happen to page it. A Mac listening for the same adverts
/// wins that race every time. Watching for the advert and paging once per
/// case opening evens it up.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AutoconnectConfig {
    /// Scan for the advert and page once per presence episode.
    pub enabled: bool,
    /// Ignore adverts weaker than this, in dBm. Keeps another room out.
    pub min_rssi: i16,
    /// Wait this long after the first advert before paging, so a nearer host
    /// that the user prefers gets the link first.
    pub settle_seconds: u64,
    /// Silence of at least this long ends a presence episode. The next advert
    /// after it is a new case opening, and re-arms a disarmed state machine.
    pub absence_seconds: u64,
    /// Wait this long after a remote or timed-out disconnect before the first
    /// page sent without an advert. The BCM20702A0 adapter misses the
    /// proximity advert after a case cycle often enough that waiting for it
    /// alone leaves the AirPods on another host.
    pub fallback_first_seconds: u64,
    /// Wait between later pages sent without an advert.
    pub fallback_interval_seconds: u64,
    /// Keep paging without an advert for at most this long after the
    /// disconnect. `0` turns the fallback off and leaves only the advert
    /// trigger.
    pub fallback_minutes: u64,
}

impl Default for AutoconnectConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_rssi: -90,
            settle_seconds: 3,
            absence_seconds: 15,
            fallback_first_seconds: 20,
            fallback_interval_seconds: 45,
            fallback_minutes: 10,
        }
    }
}

/// `[ear]`: in-ear detection drives the local media players.
///
/// On by default, because it is what the hardware is for and what every other
/// platform does with the 0x0006 report. See [`crate::ear_media`] for the
/// exact rule and the conditions a resume has to meet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EarConfig {
    /// Pause local players when a bud leaves an ear.
    pub auto_pause: bool,
    /// Resume the player auris paused, once the bud is back in and nothing
    /// else has touched playback meanwhile.
    pub auto_resume: bool,
    /// Pause as soon as one of two in-ear buds leaves, which is what macOS
    /// does. `false` waits for the last bud, for one-bud listeners who swap
    /// sides while the music plays.
    pub pause_on_one_of_two: bool,
}

impl Default for EarConfig {
    fn default() -> Self {
        Self {
            auto_pause: true,
            auto_resume: true,
            pause_on_one_of_two: true,
        }
    }
}

/// Set `[handoff] enabled` in the config file, keeping every other key and
/// comment. The file is created if missing and replaced by rename. A result
/// that would not load is refused, so this can never break startup.
pub fn set_handoff_enabled(path: &Path, enabled: bool) -> anyhow::Result<()> {
    use anyhow::Context;

    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(anyhow::Error::new(e).context(format!("reading {}", path.display()))),
    };
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .with_context(|| format!("{} is not valid TOML", path.display()))?;
    if !doc.contains_key("handoff") {
        doc.insert("handoff", toml_edit::Item::Table(toml_edit::Table::new()));
    }
    let table = doc
        .get_mut("handoff")
        .and_then(toml_edit::Item::as_table_like_mut)
        .ok_or_else(|| anyhow::anyhow!("handoff in {} is not a table", path.display()))?;
    table.insert("enabled", toml_edit::value(enabled));
    let out = doc.to_string();
    toml::from_str::<Config>(&out)
        .with_context(|| format!("refusing to write {}: it would not load", path.display()))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, out).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

/// Contents of `config.toml`. Every field is optional; a missing file is fine.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// BD_ADDR to pin to, instead of auto-detecting.
    pub device: Option<String>,
    /// Which bud the primary byte of an ear-detection packet describes.
    pub primary_bud: PrimaryBud,
    /// Opt-in bounded BLE battery observation (requires private identity keys).
    pub ble: crate::ble::BleConfig,
    /// Apple multi-host switching.
    pub handoff: HandoffConfig,
    /// In-ear detection driving the local media players.
    pub ear: EarConfig,
    /// BLE proximity-triggered auto-connect.
    pub autoconnect: AutoconnectConfig,
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
    fn autoconnect_defaults_on_and_parses() {
        // A missing section must still arm the feature: this is the whole
        // point of the default, and a silent false would look like a bug.
        let cfg: Config = toml::from_str("device = \"AC:DE:48:00:11:22\"\n").unwrap();
        assert!(cfg.autoconnect.enabled);
        assert_eq!(cfg.autoconnect.min_rssi, -90);
        assert_eq!(cfg.autoconnect.settle_seconds, 3);
        assert_eq!(cfg.autoconnect.absence_seconds, 15);
        let cfg: Config = toml::from_str("[autoconnect]\n").unwrap();
        assert_eq!(cfg.autoconnect, AutoconnectConfig::default());
        let cfg: Config = toml::from_str(
            "[autoconnect]\nenabled = false\nmin_rssi = -55\nsettle_seconds = 8\n\
             absence_seconds = 30\n",
        )
        .unwrap();
        assert!(!cfg.autoconnect.enabled);
        assert_eq!(cfg.autoconnect.min_rssi, -55);
        assert_eq!(cfg.autoconnect.settle_seconds, 8);
        assert_eq!(cfg.autoconnect.absence_seconds, 30);
        assert!(toml::from_str::<Config>("[autoconnect]\nmin_rsi = -55\n").is_err());
    }

    #[test]
    fn ear_defaults_on_and_parses() {
        // A missing section must arm both halves: a silent false would look
        // exactly like ear detection being broken.
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.ear, EarConfig::default());
        assert!(cfg.ear.auto_pause);
        assert!(cfg.ear.auto_resume);
        assert!(cfg.ear.pause_on_one_of_two);
        let cfg: Config = toml::from_str("[ear]\n").unwrap();
        assert_eq!(cfg.ear, EarConfig::default());
        let cfg: Config = toml::from_str(
            "[ear]\nauto_pause = true\nauto_resume = false\npause_on_one_of_two = false\n",
        )
        .unwrap();
        assert!(cfg.ear.auto_pause);
        assert!(!cfg.ear.auto_resume);
        assert!(!cfg.ear.pause_on_one_of_two);
        assert!(toml::from_str::<Config>("[ear]\nauto_puase = true\n").is_err());
    }

    #[test]
    fn empty_config_is_default() {
        let cfg: Config = toml::from_str("").unwrap();
        assert!(cfg.device.is_none());
        assert_eq!(cfg.primary_bud, PrimaryBud::Auto);
    }
}

#[cfg(test)]
mod handoff_config_tests {
    use super::*;

    fn temp_config() -> PathBuf {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir()
            .join(format!("aurisd-handoff-cfg-{}-{n}", std::process::id()))
            .join("config.toml")
    }

    #[test]
    fn handoff_defaults_off_with_take_over_on_play() {
        let cfg: Config = toml::from_str("").unwrap();
        assert!(!cfg.handoff.enabled);
        assert!(cfg.handoff.take_over_on_play);
        assert!(cfg.handoff.rejoin_after_eviction);
        assert_eq!(cfg.handoff.bt_name(), "Mac");
        let cfg: Config = toml::from_str("[handoff]\nrejoin_after_eviction = false\n").unwrap();
        assert!(!cfg.handoff.rejoin_after_eviction);
        let cfg: Config =
            toml::from_str("[handoff]\nenabled = true\nbt_name = \"Linux\"\n").unwrap();
        assert!(cfg.handoff.enabled);
        assert_eq!(cfg.handoff.bt_name(), "Linux");
        let cfg: Config =
            toml::from_str(&format!("[handoff]\nbt_name = \"{}\"\n", "x".repeat(33))).unwrap();
        assert_eq!(cfg.handoff.bt_name(), "Mac");
    }

    #[test]
    fn set_handoff_enabled_preserves_other_keys_and_comments() {
        let path = temp_config();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "# pinned\nprimary_bud = \"left\"\n\n[handoff]\ntake_over_on_play = false\n",
        )
        .unwrap();
        set_handoff_enabled(&path, true).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# pinned"), "{text}");
        let cfg: Config = toml::from_str(&text).unwrap();
        assert_eq!(cfg.primary_bud, PrimaryBud::Left);
        assert!(cfg.handoff.enabled);
        assert!(!cfg.handoff.take_over_on_play);
        set_handoff_enabled(&path, false).unwrap();
        let cfg: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(!cfg.handoff.enabled);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn set_handoff_enabled_creates_a_missing_file_and_refuses_a_broken_one() {
        let path = temp_config();
        set_handoff_enabled(&path, true).unwrap();
        let cfg: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(cfg.handoff.enabled);
        std::fs::write(&path, "handoff = 3\n").unwrap();
        assert!(set_handoff_enabled(&path, true).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "handoff = 3\n");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
