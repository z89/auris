<h1 align="center">aurisd</h1>

<p align="center">airpods battery and controls for linux</p>

<p align="center">
  <a href="https://github.com/z89/auris/stargazers"><img src="https://img.shields.io/github/stars/z89/auris?style=flat-square&color=8fd3ff&labelColor=1b1a20" alt="stars"></a>
  <a href="https://github.com/z89/auris/commits/main"><img src="https://img.shields.io/github/last-commit/z89/auris?style=flat-square&color=8fd3ff&labelColor=1b1a20" alt="last commit"></a>
  <a href="../LICENSE"><img src="https://img.shields.io/github/license/z89/auris?style=flat-square&color=8fd3ff&labelColor=1b1a20" alt="license"></a>
  <a href="https://aur.archlinux.org/packages/aurisd-git"><img src="https://img.shields.io/badge/aur-aurisd--git-8fd3ff?style=flat-square&labelColor=1b1a20" alt="aur"></a>
</p>

a small daemon that talks to airpods the way a mac does, and a cli to go with it. battery for each bud and the case, ear detection, noise control, conversational awareness. no root, no patched bluez, no pretending to be an apple device.

this is the `daemon` half of [auris](../README.md). the bar plugin lives one directory up.

## install

```sh
yay -S aurisd-git
systemctl --user enable --now aurisd
```

or from source, with rust 1.85 or newer, from this directory:

```sh
cargo install --path .
mkdir -p ~/.config/systemd/user
sed 's#/usr/bin/aurisd#%h/.cargo/bin/aurisd#' dist/aurisd.service > ~/.config/systemd/user/aurisd.service
systemctl --user daemon-reload
systemctl --user enable --now aurisd
```

pair and connect the airpods as usual. the daemon notices, opens its own link next to the audio one, and starts writing.

## use

```sh
auris status                 # what the daemon knows right now
auris status --json          # the same, as the state file
auris noise anc              # off, anc, transparency, adaptive
auris adaptive 60            # adaptive transparency level, 0 to 100
auris ca on                  # conversational awareness, on or off
auris reconnect              # one guarded control-link attempt; requires local Bluetooth connection
auris connect-once           # explicit pinned-device Bluetooth attempt; may transfer audio
auris rename 'Desk AirPods'  # rename the accessory
auris setting microphone '"left"'
auris setting personalized_volume true
auris setting listening_mode_cycle '["anc","transparency"]'
```

`setting` values are JSON: enum values need quotes, booleans do not, and mode
cycles are arrays. accepted keys and values are:

| key | JSON value |
|---|---|
| `microphone` | `"auto"`, `"left"`, `"right"` |
| `press_speed` | `"default"`, `"slower"`, `"slowest"` |
| `hold_duration` | `"default"`, `"shorter"`, `"shortest"` |
| `listening_mode_cycle` | a non-duplicate array of `"off"`, `"anc"`, `"transparency"`, `"adaptive"` |
| `call_controls` | `"mute_once_hangup_twice"` or `"hangup_once_mute_twice"` |
| `personalized_volume` | `true` or `false` |

the six typed settings above plus rename are the seven new controls. they are
currently software-implemented only for product id `201B`, and require an open
AAP link. the cli validates them before opening the daemon socket. an `ok` reply
means the command was sent, not that the hardware applied it; wait for the
AirPods to report the setting. none of these new controls has yet been
hardware-verified on the target firmware.

rename is also only an accepted write. the metadata name may not refresh until
the AirPods reconnect, and aurisd does not infer readback from the requested
name.

exit code 0 when the daemon took the command, 1 when it refused or local input
validation failed, 2 when the daemon is not running.

## what it writes

`$XDG_RUNTIME_DIR/aurisd/state.json`, replaced atomically on every change. read it, watch it, whatever you like. `aurisd --dump-schema` prints an example:

```json
{
  "schema": 1,
  "updated_at": "2026-09-05T10:22:31+10:00",
  "daemon": { "version": "0.1.0", "source": "aap" },
  "device": {
    "address": "AC:DE:48:00:11:22",
    "name": "AirPods",
    "model_id": "201B",
    "model": "AirPods 4 (ANC)",
    "firmware": "7B21",
    "serial": null,
    "connected": true,
    "aap_link": true
  },
  "battery": {
    "stale": false,
    "left":  { "source": "aap", "fresh": true, "level": 87, "charging": false, "last_known_charging": false, "present": true, "last_seen": "2026-09-05T10:22:31+10:00" },
    "right": { "source": "aap", "fresh": true, "level": 85, "charging": false, "last_known_charging": false, "present": true, "last_seen": "2026-09-05T10:22:31+10:00" },
    "case":  { "source": "aap", "fresh": true, "level": 62, "charging": true,  "last_known_charging": true,  "present": true, "last_seen": "2026-09-05T10:22:31+10:00" }
  },
  "ear": { "left": "in", "right": "in" },
  "lid": "unknown",
  "noise_control": "anc",
  "conversational_awareness": true,
  "adaptive_level": 50,
  "settings_api": 1,
  "settings": {
    "microphone": "left",
    "press_speed": "default",
    "hold_duration": null,
    "listening_mode_cycle": ["anc", "transparency"],
    "call_controls": null,
    "personalized_volume": true
  },
  "settings_report_seq": { "microphone": 1, "press_speed": 1, "listening_mode_cycle": 1, "personalized_volume": 1 }
}
```

- `daemon.source` is `aap` while the control link is up, otherwise `ble` if a current BLE battery observation exists, otherwise `none`. BLE observation never implies connected audio
- each battery cell adds `source` and `fresh`. opening an AAP link does not refresh old cells; only reports do. `battery.stale` means no cell is fresh
- the buds only relay the case while they sit in it. out of the case, `case.present` goes false, `level` keeps the last reading and `last_seen` says when. the levels are cached in `~/.cache/aurisd` so they survive a restart
- each cell's nullable `last_known_charging` retains its last reported charging state alongside `level` and `last_seen`. check per-cell `fresh` and `present` before treating it as live. old snapshots default missing `fresh` to false in the daemon; the plugin supports older snapshots with the legacy global stale flag
- battery caches are private (0600) and bound to the paired address. startup restores them only for an explicitly pinned matching device. old unbound cache files are ignored, not deleted; new reports replace them
- there is no fixed case-expiry timer: battery packets mark components absent. the separate AAP idle watchdog waits 300 seconds, then allows a 15-second probe grace; that detects a silent control link, not a case timeout. the firmware's case-reporting window has not been measured here
- `ear` is `in`, `out`, `case` or `unknown`. the current BLE observer only adds battery observations; it does not guess lid or ear state
- 0x0006 names a primary and a secondary bud, never a left and a right, and the role moves: whichever bud is out and working is the primary one. `primary_bud = "auto"` resolves it from the battery packet, which does name its component, so a bud on its own is mapped to the side it is actually on. with both buds out there is nothing to go on and it falls back to left-first; pin `"left"` or `"right"` if you want it fixed
- `serial` and `firmware` come from the airpods themselves and are `null` until they send them
- `settings_api` is `1` when typed settings are available. old schema-1 documents omit it and deserialize as `0`
- each value under `settings` is last confirmed device-reported state. `null` means unknown; it does not mean off or unsupported. the top-level schema remains `1`
- additive `settings_report_seq` maps setting keys to report counters. an actual repeat report increments its key; unrelated battery updates do not. counters persist across control-link loss in the process, reset for a new device or daemon, and are not command IDs

commands go over `$XDG_RUNTIME_DIR/aurisd/ctl.sock`, one json object per line. the cli is a thin wrapper, so anything else can talk to it too:

```
{"cmd":"set_noise_control","value":"anc"}
{"cmd":"set_conversational_awareness","value":true}
{"cmd":"set_adaptive_level","value":50}
{"cmd":"rename","name":"Desk AirPods"}
{"cmd":"set_setting","key":"microphone","value":"left"}
{"cmd":"set_setting","key":"press_speed","value":"slower"}
{"cmd":"set_setting","key":"hold_duration","value":"shorter"}
{"cmd":"set_setting","key":"listening_mode_cycle","value":["anc","transparency"]}
{"cmd":"set_setting","key":"call_controls","value":"mute_once_hangup_twice"}
{"cmd":"set_setting","key":"personalized_volume","value":true}
{"cmd":"reconnect"}
{"cmd":"connect_once"}
{"cmd":"status"}
{"cmd":"subscribe"}
```

answers are `{"ok":true}`, `{"ok":false,"error":"..."}`, or the state object for `status`.

`subscribe` answers with the state object and then sends another one every time
anything changes, one per line, until you hang up. that connection stops reading
requests, so send commands on a second one. it exists so a ui does not have to
poll `state.json`: the file is replaced by rename, which file watchers do not
reliably follow, and the alternative is a timer that is both late and always
running.

## config

defaults need no configuration. `~/.config/aurisd/config.toml` can pin identity and opt into BLE telemetry:

```toml
device = "AC:DE:48:00:11:22"  # pin one address instead of picking the airpods bluez knows about
primary_bud = "auto"          # which bud the airpods call primary. "auto", "left" or "right"

[ble]
enabled = false              # opt-in only; see the BLE setup/evidence guide
# key_file = "/absolute/private/path/keys.json"
scan_seconds = 8
interval_seconds = 30
freshness_seconds = 75
```

and a few environment variables, mostly for poking at things:

- `RUST_LOG=aurisd=debug` for identifiers/lengths it does not model; `trace` for packet framing diagnostics. raw AAP payloads and AAP metadata strings are not logged
- `AURISD_FEATURES=alt` pins the second set-features variant instead of letting the daemon find one
- `AURISD_RAW_SOCKET=1` uses a plain libc socket instead of bluer's

BLE requires provisioned identity keys and a pinned paired device. See
[battery observation and handoff safety](../docs/BATTERY_AND_HANDOFF.md) before
enabling it. No keys are automatically requested or imported, and discovery is
disabled by default. Invalid BLE configuration fails closed.
Malformed main configuration now stops startup rather than silently discarding
the pinned device and falling back to a different paired accessory.

## works with

anything that speaks aap. names are known for these, anything else shows as its model id:

| id | model |
|---|---|
| 2002 | AirPods |
| 200F | AirPods 2 |
| 2013 | AirPods 3 |
| 2019 | AirPods 4 |
| 201B | AirPods 4 (ANC) |
| 200E | AirPods Pro |
| 2014 | AirPods Pro 2 |
| 2024 | AirPods Pro 2 (USB-C) |
| 200A | AirPods Max |
| 201F | AirPods Max (USB-C) |

tested on airpods 4 (anc). noise control on models without it does nothing, the airpods just ignore the command.

## how it works

macos and ios talk to airpods over aap, apple's accessory protocol, on an l2cap channel next to the audio link. the channel is psm 0x1001, which on linux is in the dynamic range, so any user can connect to it once bluez has the classic link up. no capability, no vendor id trick.

the opening sequence waits for answers rather than sleeping:

```
-> 00 00 04 00 01 00 02 00 00 00 00 00 00 00 00 00   handshake
<- 01 00 04 00 ...                                   handshake ack, waited for up to 3 s
-> 04 00 04 00 4d 00 d7 00 00 00 00 00 00 00         set features, 14 bytes
<- 04 00 04 00 2b 00 ...                             features ack, up to 2 s, optional
-> 04 00 04 00 0f 00 ff ff ff ff ff                  request notifications
```

set features has to be exactly 14 bytes. a 13 byte write goes through and the airpods quietly never send a battery packet. there are two known selector bytes, `d7` and `0e`, and some firmware only answers one of them, so the daemon alternates per dial until one produces a battery packet and then sticks with it. on the `0e` variant it also subscribes before negotiating features, which is what the go daemon below does.

after that the airpods push what they have:

| opcode | what |
|---|---|
| 0x0004 | battery, one entry per component. `02` right, `04` left, `08` case. status `01` charging, `02` discharging, `04` not here |
| 0x0006 | ear detection, primary then secondary. `00` in ear, `01` out, `02` in case. other values seen in the wild, `03` among them, read as unknown |
| 0x0009 | a control setting echoed back. `0d` noise control, `28` conversational awareness, `2e` adaptive level |
| 0x001D | metadata, nul separated strings. pushed once, cannot be asked for |
| 0x004B | speech ducking while conversational awareness is on. a level, not the on/off state |

everything else is logged at debug and dropped. if no battery packet arrives
within 10 s the daemon asks again twice on the existing socket, then suppresses
recovery. peer loss, send errors and unanswered idle probes also suppress
automatic redial. a new observed local disconnected→connected transition or an
explicit user command is required for another guarded attempt. each AAP dial
checks current BlueZ state and cancels when the observed link generation changes.
there is no unattended Bluetooth-connect loop. `connect-once` is a separate,
explicit command for a pinned paired device; it may take audio from another host.
these checks reduce races, not prove that another host is idle or guarantee
protection from other applications' or BlueZ's connection policies.

## related

these are where the protocol facts came from. nothing is copied from them.

- [battery observation and handoff safety](../docs/BATTERY_AND_HANDOFF.md), including opt-in setup, protocol references and limitations
- [feature comparison](../docs/FEATURE_COMPARISON.md), including the distinction between implemented and hardware-verified behavior
- [librepods](https://github.com/kavishdevar/librepods), the most complete aap reference around
- [airpods-battery](https://github.com/AlwxSin/airpods-battery), a go daemon with the alternate opening sequence
- [omarchy-pods](https://github.com/thisisgm/omarchy-pods), the same idea for a different shell

## license

mit
