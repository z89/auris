# aurisd

aurisd is a rust daemon that speaks the Apple Accessory Protocol (AAP) over
the L2CAP control channel on PSM 0x1001. it reports battery per bud and case,
in-ear state, noise control, adaptive level, conversational awareness and
device metadata, writes typed settings and verifies them against the
accessory's own report. it runs as an unprivileged systemd user service, with no
root and no patched bluez.

this is the `daemon` half of [auris](../README.md). the bar plugin lives one
directory up. this file is the reference for the daemon, the `auris` CLI, the
config file, the state file and the control socket.

## build and install

needs rust 1.85 or newer. run from this directory.

```sh
cargo install --path .
mkdir -p ~/.config/systemd/user
sed 's#/usr/bin/aurisd#%h/.cargo/bin/aurisd#' dist/aurisd.service > ~/.config/systemd/user/aurisd.service
systemctl --user daemon-reload
systemctl --user enable --now aurisd
```

pair and connect the AirPods as usual. the daemon opens its own link next to
the audio one and starts writing state.

## CLI reference

global options, accepted by every subcommand.

| option | meaning |
|---|---|
| `--runtime-dir <PATH>` | override the runtime directory (default `$XDG_RUNTIME_DIR/aurisd`) |
| `-h`, `--help` | print help |
| `-V`, `--version` | print version (top level only) |

| command | arguments | effect |
|---|---|---|
| `auris status` | `[--json]` | summary of the current state. `--json` prints the raw `state.json` object |
| `auris noise` | `anc`, `transparency`, `adaptive` or `off` | set noise control |
| `auris ca` | `on` or `off` | set conversational awareness |
| `auris adaptive` | integer 0 to 100 | set the adaptive transparency level |
| `auris reconnect` | none | drop and redial the AAP link. one guarded attempt, needs an existing local bluetooth connection |
| `auris connect-once` | none | ask bluez once to connect the pinned paired device. never retries, may take audio from another host |
| `auris handoff` | `on`, `off` or `status` | switch Apple multi-host switching and persist `[handoff] enabled`, or show its state |
| `auris take-over` | none | take the AirPods from another Apple host now |
| `auris yield` | none | give the AirPods to another Apple host now |
| `auris rename` | `<NAME>` | rename the accessory. quote names with spaces |
| `auris setting` | `<KEY> <JSON_VALUE>` | write a typed setting (keys below) |

```sh
auris status --json
auris noise anc
auris rename 'Desk AirPods'
auris setting microphone '"left"'
auris setting personalized_volume true
auris setting listening_mode_cycle '["anc","transparency"]'
```

- when the advert scan cannot run (`autoconnect.scan` is `le_disabled`,
  `adapter_off` or `failed`), `auris status` ends with one line saying why.
- `take-over` and `yield` need an open AAP link and act regardless of
  `take_over_on_play` or `[handoff] enabled`.

### setting keys

values are JSON. enums need quotes, booleans do not, mode cycles are arrays.

| key | JSON value |
|---|---|
| `microphone` | `"auto"`, `"left"`, `"right"` |
| `press_speed` | `"default"`, `"slower"`, `"slowest"` |
| `hold_duration` | `"default"`, `"shorter"`, `"shortest"` |
| `listening_mode_cycle` | non-duplicate array of `"off"`, `"anc"`, `"transparency"`, `"adaptive"` |
| `call_controls` | `"mute_once_hangup_twice"` or `"hangup_once_mute_twice"` |
| `personalized_volume` | `true` or `false` |

these six plus rename are software-implemented only for product id `201B` and
need an open AAP link. the CLI validates them before opening the daemon
socket.

### what a setting write or rename reports

`auris setting` and `auris rename` subscribe after the write, wait up to 10s
for `settings_status` to leave `verifying`, then print one line. rename is
tracked under the key `name`. the write went out in every case.

| `settings_status` | meaning | CLI line | exit |
|---|---|---|---|
| `verifying` | written, readback running | none yet | |
| `confirmed` | a fresh dump reported the requested value | `confirmed by AirPods` | 0 |
| `mismatch` | a fresh dump reported something else, now in `settings` (or `device.name`) | `AirPods kept <value>` | 1 |
| `unreported` | readback ran, the model never reports the key (microphone and listening-mode cycle on AirPods 4 (ANC)) | `applied; AirPods 4 (ANC) never reports this setting back` | 0 |
| `unverified` | no readback completed within 10s | `sent but not verified: <reason>` | 1 |
| absent | `settings_api` below 2, the daemon predates the readback contract | `sent; waiting for AirPods to report the setting` | 0 |

the readback reopens the AAP link once, 1500ms after the last write, because
the accessory dumps its settings only at link open. `verify_reopen` in
`state.json` marks that window. only reports from the new link count, and no
write is replayed. the design is in
[verify by reopen](../docs/PROTOCOL_EVIDENCE.md#verify-by-reopen-design).

a rename is read back from the accessory's own metadata, pushed after every
AAP handshake, not from bluez, which re-reads the remote name only at link
open and can be minutes stale. a different reported name is a `mismatch`.
the daemon prefers the metadata name over bluez `Name` or `Alias` and never
writes `Alias`. Apple hosts keep the name in their own pairing record and
never re-read it, so a rename shows on linux and other non-Apple hosts while
an iPhone or Mac keeps its own name.

### exit codes

| code | meaning |
|---|---|
| 0 | the daemon took the command |
| 1 | the daemon refused, or local input validation failed |
| 2 | the daemon is not running |

## daemon flags

`aurisd` takes flags only.

| flag | meaning |
|---|---|
| `--runtime-dir <PATH>` | override the runtime directory (default `$XDG_RUNTIME_DIR/aurisd`) |
| `--device <BD_ADDR>` | pin to one accessory instead of auto-detecting |
| `--dump-schema` | print an example `state.json` and exit |
| `-h`, `--help` | print help |
| `-V`, `--version` | print version |

| variable | effect |
|---|---|
| `RUST_LOG=aurisd=debug` | log identifiers and lengths it does not model. `trace` adds packet framing. raw AAP payloads and metadata strings are never logged |
| `AURISD_FEATURES=alt` | pin the second set-features variant instead of probing |
| `AURISD_FEATURES=ff` | pin the `0xff` selector. same dump as the default `d7` on AirPods 4 (ANC), meant for firmware that answers neither |
| `AURISD_RAW_SOCKET=1` | use a plain libc socket instead of bluer's |

## config reference

`~/.config/aurisd/config.toml` (or `$XDG_CONFIG_HOME/aurisd/config.toml`).
every key is optional and a missing file is fine. an invalid file stops
startup. it never silently drops the pinned device and falls back to another
paired accessory. the example shows every key at its default except `device`.

```toml
device = "AC:DE:48:00:11:22"
primary_bud = "auto"

[ble]
enabled = false
# key_file = "/absolute/private/path/keys.json"
scan_seconds = 8
interval_seconds = 30
freshness_seconds = 75

[handoff]
enabled = false
take_over_on_play = true
# bt_name = "Mac"
rejoin_after_eviction = true

[ear]
auto_pause = true
auto_resume = true
pause_on_one_of_two = true

[autoconnect]
enabled = true
min_rssi = -90
settle_seconds = 3
absence_seconds = 15
fallback_first_seconds = 20
fallback_interval_seconds = 45
fallback_minutes = 10
```

| key | default | effect |
|---|---|---|
| `device` | none | pin one BD_ADDR instead of auto-detecting the paired accessory |
| `primary_bud` | `"auto"` | which bud the primary byte of an ear detection packet describes, `"auto"`, `"left"` or `"right"`. the accessory never names it and it moves as buds go in and out, so `"auto"` resolves it from the battery packet |
| `[ble] enabled` | `false` | opt-in bounded BLE battery observation. needs an absolute `key_file` |
| `[ble] key_file` | none | private JSON file with `address`, `irk` and `encryption_key` |
| `[ble] scan_seconds` | `8` | discovery per interval, 2 to 30 |
| `[ble] interval_seconds` | `30` | between window starts, at least `scan_seconds + 5`, at most 300 |
| `[ble] freshness_seconds` | `75` | age after which a BLE cell is historical, at least `interval_seconds + scan_seconds`, at most 600 |
| `[handoff] enabled` | `false` | yield to other Apple hosts and take over on local playback. `auris handoff on` sets it |
| `[handoff] take_over_on_play` | `true` | take the AirPods back when a local player starts |
| `[handoff] bt_name` | none (effective `"Mac"`) | name announced to other Apple hosts, 1 to 32 bytes, no control characters. unusable values fall back to `"Mac"` |
| `[handoff] rejoin_after_eviction` | `true` | reconnect once when an Apple host's fresh link made the AirPods drop this host. needs `enabled` |
| `[ear] auto_pause` | `true` | pause local players when a bud leaves an ear |
| `[ear] auto_resume` | `true` | resume the player auris paused once the bud is back, if nothing else touched playback |
| `[ear] pause_on_one_of_two` | `true` | pause when one of two in-ear buds leaves, as macOS does. `false` waits for the last bud, for one-bud listeners who swap sides |
| `[autoconnect] enabled` | `true` | scan for the BLE proximity advert and page once per presence episode |
| `[autoconnect] min_rssi` | `-90` | ignore weaker adverts, in dBm, to keep another room out |
| `[autoconnect] settle_seconds` | `3` | wait after the first advert before paging, so a nearer preferred host wins |
| `[autoconnect] absence_seconds` | `15` | silence this long ends a presence episode. the next advert is a new case opening |
| `[autoconnect] fallback_first_seconds` | `20` | wait after a remote or timed-out disconnect before the first page sent without an advert |
| `[autoconnect] fallback_interval_seconds` | `45` | wait between later pages without an advert |
| `[autoconnect] fallback_minutes` | `10` | stop paging without an advert this long after the disconnect. `0` leaves only the advert trigger |

`[ear]` is on by default because the 0x0006 report exists for it and other
platforms already use it.

handoff needs bluez `DeviceID` set to Apple and a re-pair. see the top-level
readme and
[Apple multi-host switching](../docs/BATTERY_AND_HANDOFF.md#apple-multi-host-switching).

bluez never pages a classic device on its own, so without `[autoconnect]` a
Mac listening for the same advert wins the race. the advert scan needs LE on
the adapter. with `ControllerMode = bredr` in `/etc/bluetooth/main.conf` it
cannot start, aurisd logs one warning and waits for the adapter to change
(bluez reads `ControllerMode` only at start). the page fallback keeps working
for its first 10min after a drop. retry timing per fault is under
[autoconnect](#autoconnect).

### BLE safety

BLE needs provisioned identity keys and a pinned paired device. no keys are
requested or imported automatically, discovery is off by default, and invalid
BLE configuration fails closed. read
[battery observation and handoff safety](../docs/BATTERY_AND_HANDOFF.md)
before enabling it.

## state and socket

### state file

`$XDG_RUNTIME_DIR/aurisd/state.json`, replaced atomically on every change.
`aurisd --dump-schema` prints an example.

```json
{
  "schema": 2,
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
  "settings_api": 3,
  "settings": {
    "microphone": "left",
    "press_speed": "default",
    "hold_duration": null,
    "listening_mode_cycle": ["anc", "transparency"],
    "call_controls": null,
    "personalized_volume": true
  },
  "settings_report_seq": { "microphone": 1, "press_speed": 1, "listening_mode_cycle": 1, "personalized_volume": 1 },
  "settings_requested": { "press_speed": "default" },
  "settings_status": { "press_speed": "confirmed" },
  "settings_verify": "idle",
  "verify_reopen": false,
  "link": { "status": "reconnecting", "reason": "bud_switch", "attempt": 1, "since": "2026-09-16T02:01:47Z" },
  "autoconnect": { "scan": "idle" }
}
```

#### daemon and battery

- `daemon.source` is `aap` while the control link is up, else `ble` if a
  current BLE battery observation exists, else `none`. BLE observation never
  implies connected audio.
- battery cells follow the
  [freshness rules](../docs/BATTERY_AND_HANDOFF.md). only reports refresh a
  cell. `battery.stale` means no cell is fresh. nullable
  `last_known_charging` is the last reported charging state, so check
  `fresh` and `present` before treating it as live.
- the buds relay the case only while it holds them. out of the case
  `case.present` is false, `level` keeps the last reading and `last_seen`
  says when. no case-expiry timer exists, battery packets mark components
  absent. the AAP idle watchdog (300s, then a 15s probe grace) detects a
  silent control link, not a case timeout. the firmware's case-reporting
  window is unmeasured.
- a missing `fresh` in old snapshots defaults to false. the plugin still
  reads the legacy global stale flag.
- levels are cached in `~/.cache/aurisd` (0600, bound to the paired address)
  and restored at startup only for an explicitly pinned matching device.

#### ear and device

- `ear` is `in`, `out`, `case` or `unknown`. the BLE observer adds battery
  only, never lid or ear state.
- 0x0006 names a primary and a secondary bud, never left and right. the role
  moves, and the bud that is out and working is primary. `primary_bud = "auto"`
  maps it through the battery packet, which names its component, so a lone
  bud lands on its real side. with both buds out it falls back to left-first.
  pin `"left"` or `"right"` to fix it.
- `serial` and `firmware` come from the AirPods and are `null` until sent.

#### link

`link` tells a UI whether a missing link is healing, so it can keep the device
on screen through the 6s a rejoin takes.

| `link.status` | when |
|---|---|
| `connected` | whenever `device.connected` is true |
| `reconnecting` | a rejoin or auto-connect sequence is scheduled or connecting |
| `disconnected` | otherwise |

| `link.reason` | meaning |
|---|---|
| `bud_switch` | the `0x0C` primary bud address report changed in the 15s before the drop, so the buds moved their radio link and cut this host |
| `taken_over` | an Apple host took the AirPods |
| `link_lost` | a supervision timeout |
| `auto_connect` | the proximity pager is bringing them back after the case opened |

`link.reason` is `null` unless `reconnecting`. `link.attempt` counts
`Device1.Connect` calls in the current sequence, `0` while it only waits out
its delay. `link.since` is the sequence start, RFC3339 UTC with a `Z`, `null`
with no sequence. both reset when the link returns.

#### autoconnect

`autoconnect.scan` says whether the advert scan can run. the object is
additive, so older readers can ignore it. a fault stays until the next scan
runs.

| value | meaning |
|---|---|
| `idle` | no scan running and nothing wrong last time, for example connected or `enabled = false` |
| `scanning` | the LE discovery scan is running |
| `le_disabled` | LE is off on the adapter, usually `ControllerMode = bredr`. waits for the adapter to change, rechecks every 5min |
| `adapter_off` | adapter unpowered or missing. waits for it to return |
| `failed` | other scan failure, retried with a wait growing from 5s to 5min |

#### settings

- `settings_api` is `3` when rename is verified too (key `name`), `2` with
  the readback fields below, `1` for typed settings without them and `0` for
  old schema-1 documents, which omit the field.
- `settings` holds the last confirmed device-reported value per key. `null`
  means unknown, not off or unsupported.
- `settings_report_seq` (additive) maps keys to report counters. a real repeat
  report increments its key, battery updates do not. counters survive
  control-link loss within the process, reset for a new device or daemon, and
  are not command IDs.
- `settings_requested` is the last value written per key in this daemon's
  lifetime, in the shape `settings` uses. it is what was asked for, never
  what the accessory holds. its `name` is the requested name, reported in
  `device.name`.
- `settings_verify` is `idle`, `scheduled` (waiting out the 1500ms debounce)
  or `reopening` (reopening the AAP link to read back).
- `verify_reopen` is true only during that reopen. `device.connected` and
  `device.aap_link` can go false meanwhile, and a UI should keep showing the
  device as connected.
- `settings_status` is the verdict per requested key, listed under
  [what a setting write or rename reports](#what-a-setting-write-or-rename-reports).

### control socket

`$XDG_RUNTIME_DIR/aurisd/ctl.sock`, one JSON object per line. the CLI is a
thin wrapper over it.

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
{"cmd":"take_over"}
{"cmd":"yield"}
{"cmd":"set_handoff","enabled":true}
{"cmd":"status"}
{"cmd":"subscribe"}
```

replies are `{"ok":true}`, `{"ok":false,"error":"..."}`, or the state object
for `status`. `subscribe` answers with the state object, then one more per
change until the client hangs up, and reads no further requests, so send
commands on a second connection. it exists because `state.json` is replaced
by rename, which file watchers do not reliably follow, and polling is late
and always running.

## works with

anything that speaks AAP. unlisted models show their model id.

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

tested on AirPods 4 (ANC). noise control on models without it is ignored by
the AirPods.

## how it works

AAP runs on an L2CAP channel next to the audio link. PSM 0x1001 is in the
dynamic range on linux, so any user can connect once bluez has the classic
link up, with no capability or vendor id trick.

### opening sequence

each step waits for its answer rather than sleeping.

```
-> 00 00 04 00 01 00 02 00 00 00 00 00 00 00 00 00   handshake
<- 01 00 04 00 ...                                   handshake ack, waited for up to 3 s
-> 04 00 04 00 4d 00 d7 00 00 00 00 00 00 00         set features, 14 bytes
<- 04 00 04 00 2b 00 ...                             features ack, up to 2 s, optional
-> 04 00 04 00 0f 00 ff ff ff ff ff                  request notifications
```

set features must be exactly 14 bytes. a 13-byte write goes through and the
AirPods never send a battery packet. the selector is `d7` or `0e`, and some
firmware answers only one, so the daemon alternates per dial until one yields
a battery packet, then keeps it. on `0e` it subscribes before negotiating
features, as the go daemon below does.

### what the AirPods push

| opcode | what |
|---|---|
| 0x0004 | battery, one entry per component. `02` right, `04` left, `08` case. status `01` charging, `02` discharging, `04` not here |
| 0x0006 | ear detection, primary then secondary. `00` in ear, `01` out, `02` in case. other values (`03` among them) read as unknown |
| 0x0009 | a control setting echoed back. `0d` noise control, `28` conversational awareness, `2e` adaptive level, `24` call controls. the other typed settings appear only in the dump at link open, if at all |
| 0x001D | metadata, nul separated strings. pushed once, cannot be asked for |
| 0x004B | speech ducking while conversational awareness is on. a level, not the on/off state |

anything else is logged at debug and dropped.

### recovery rules

- no battery packet within 10s makes the daemon ask twice more on the same
  socket, then suppress recovery. peer loss, send errors and unanswered idle
  probes also suppress automatic redial.
- another guarded attempt needs a newly observed local disconnected to
  connected transition, or an explicit user command.
- each AAP dial checks current bluez state and cancels when the observed link
  generation changes.
- there is no unattended bluetooth-connect loop. `connect-once` is explicit,
  for a pinned paired device, and may take audio from another host.

these checks reduce races. they do not prove another host is idle or protect
against other applications' or bluez's connection policies.

## related

protocol facts came from these sources. nothing is copied from them.

- [battery observation and handoff safety](../docs/BATTERY_AND_HANDOFF.md),
  opt-in setup, protocol references and limitations
- [protocol evidence](../docs/PROTOCOL_EVIDENCE.md), settings opcodes and the
  readback contract
- [features](../docs/FEATURES.md), implemented versus hardware-verified
  behaviour
- [LibrePods](https://github.com/librepods-org/librepods), the most complete
  AAP reference around
- [airpods-battery](https://github.com/AlwxSin/airpods-battery), a go daemon
  with the alternate opening sequence
- [omarchy-pods](https://github.com/thisisgm/omarchy-pods), the same idea for
  a different shell

## license

MIT
