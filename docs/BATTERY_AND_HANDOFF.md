# Battery observation and handoff safety

Target: AirPods 4 ANC (`201B`). This implementation is offline-tested; radio,
firmware effects and phone/Mac handoffs require a separately approved hardware
session. No unattended Bluetooth connection policy is enabled.

## What changed

Each battery cell has its own `source`, `fresh`, `present`, `last_seen` and
`last_known_charging`. An AAP socket opening does not refresh old readings.
An absent reading preserves history; current charging becomes false, while the
panel can retain its muted green bolt and “last seen charging … ago” caption.
Exact zero is a real level, not unknown. A report cannot refresh an unreported
cell just because another cell changed.

The optional BLE observer resolves rotating addresses with the pinned device's
IRK, decrypts that device's proximity battery block, and updates only measured
cells. Unknown `127` values do not reset age. A freshly reporting AAP cell wins
for 15 seconds to avoid alternating sources on duplicate adverts; otherwise a
new exact BLE reading may replace it. AAP absence does not erase a fresh BLE
reading. Losing the AAP link invalidates AAP cells, not independently observed
BLE cells. Nearby BLE readings never make the bar claim connected audio.

The supported layout is one 27-byte Apple manufacturer `0x004c`, type `0x07`
record, model bytes `1b 20`, paired format `01`. The last 16 bytes are one AES
block; battery bytes 1–3 use seven-bit percentages and a charging high bit.
Primary-side bit `0x20` determines left/right order. Case data is used only when
the reporting-bud-in-case bit `0x40` is set. Other layouts are ignored. Public
coarse percentages, inferred ear/lid states and remote-host “free” flags are not
used. This follows the [LibrePods proximity parser](https://github.com/librepods-org/librepods/blob/53679cc90222e94ade84e66542d97ace2540e626/linux/ble/blemanager.cpp)
and [encrypted battery layout](https://github.com/librepods-org/librepods/blob/53679cc90222e94ade84e66542d97ace2540e626/linux/battery.hpp);
Auris's implementation is independently written, not copied.

## Opt-in setup prerequisites

Do not enable this until the correct keys have been provisioned and the radio
test is approved. Auris does not yet acquire or import keys from another app.
No user credentials were read or changed during development.

In `~/.config/aurisd/config.toml`, pin the owned, paired classic address:

```toml
device = "AC:DE:48:00:11:22" # example only: replace with your device

[ble]
enabled = false # enable only after key provisioning and radio-test approval
key_file = "/absolute/private/path/keys.json"
scan_seconds = 8
interval_seconds = 30
freshness_seconds = 75
```

The key file is a regular file owned by the daemon's user, mode `0600` (no group
or other access), at most 4096 bytes, and not a symlink. It contains exactly:

```json
{
  "address": "AC:DE:48:00:11:22",
  "irk": "<32 hexadecimal digits: AAP-export byte order>",
  "encryption_key": "<32 hexadecimal digits: direct AES key order>"
}
```

These are placeholders, not valid keys. Never commit or paste actual keys into
logs/issues. The classic identity must match the pinned address. IRK matching
uses Bluetooth's short address hash, **not authenticated encryption**; adverts
must not authorize connections or security decisions. Wrong encryption keys
can produce plausible numbers, so compare actual observations with AAP/Apple
readings before relying on them. AES uses the [RustCrypto AES crate](https://docs.rs/aes/0.8.4/aes/),
not a custom cipher implementation. The byte-order reference is
[LibrePods BLE crypto handling](https://github.com/librepods-org/librepods/blob/53679cc90222e94ade84e66542d97ace2540e626/linux/ble/bleutils.cpp).

Discovery is LE-only for eight seconds per thirty-second window by default.
It may transmit scan requests and affect a busy/older adapter; it is not a
promise of physically passive scanning. Only this client's scan token is
released when a window ends; Auris does not stop other apps' discovery. Initial
BlueZ cached manufacturer data is ignored: subsequent manufacturer-data events
are required to refresh observations. BLE readings become historical after
75 seconds without a new cell measurement. **That is Auris's freshness policy,
not Apple's case sleep timeout.** Silent hardware cannot provide fresh data.
[BlueZ discovery semantics](https://bluez.readthedocs.io/en/latest/adapter-api/)
and the [bluer discovery API](https://docs.rs/bluer/0.17.4/bluer/struct.Adapter.html#method.discover_devices)
describe shared discovery and initially known devices.

Battery caches are now private and address-bound. Startup restores only a
matching pinned device's cache. Old unbound caches are ignored, not deleted;
new measurements replace them. Without pinning, retained runtime history still
works, but startup does not guess which device owned an old cache.

## Conservative connection behavior

On a new observed local Bluetooth connection, Auris makes one AAP attempt after
an 800 ms settle delay, checking current paired/Connected state immediately
before dialing. Observed link/identity changes invalidate in-flight opening
work. Peer loss, failed sends and unanswered watchdogs stop automatic recovery;
there is no exponential redial loop that can repeatedly compete with a phone.

`auris reconnect` explicitly retries only the settings/telemetry link while the
device is already connected locally. `auris connect-once` explicitly requests
one Bluetooth connection to the pinned paired device, with no automatic retry.
It can transfer audio: it is an intentional user action, not “connect only if
all other hosts are idle.” Neither command runs during tests or from BLE data.

Queued control commands are bound to the connection identity/epoch and have a
four-second daemon deadline. Expired commands or commands whose connection has
changed are rejected before action. A timeout cannot undo a Bluetooth connection
request already sent to BlueZ; check the actual link state before retrying.
Missing/malformed configuration is distinguished: a missing file uses defaults,
but malformed configuration stops startup rather than silently dropping a pin.

Linux's outbound L2CAP connect can establish an ACL, so a fresh BlueZ check is
a mitigation, not an atomic ownership guarantee. BlueZ's `Connected` describes
the **local** connection; RSSI, availability and silence cannot prove another
host is disconnected. Other software or BlueZ policy can also initiate a
connection. No system Bluetooth policy or audio routing was changed. Auris
cannot mute an iPhone/Mac when those devices lose their audio route.
See [BlueZ Device1](https://bluez.readthedocs.io/en/latest/device-api/) and the
[Linux L2CAP connection path](https://github.com/torvalds/linux/blob/master/net/bluetooth/l2cap_core.c).

## Hardware acceptance gate

With explicit per-session approval, first record normal AAP-only behavior, then
test BLE observation with each bud out, each bud in an open/closed case, both in
the case, case power connected/disconnected, case full/empty, and role reversal.
Compare percentages, charging, source and timestamps; a changed value must come
from a new measured cell, not a cached BlueZ property. Check radio/audio stability
with bounded discovery before considering a shorter interval.

For handoff, use non-sensitive test audio at low volume: desktop→iPhone,
desktop→Mac, deliberately disconnected buds, out-of-range return, peer reset,
adapter disappearance and daemon restart. Correlate HCI/D-Bus logs with Auris's
attempt/suppression messages. Stop on any unexpected connection takeover; do not
enable unattended auto-connect based only on offline tests. Keep any desktop
speaker-fallback protection a separate opt-in audio-policy change.
