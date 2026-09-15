<h1 align="center">aurisd</h1>

<p align="center">AirPods battery and controls for Linux</p>

<p align="center">
  <a href="https://github.com/z89/auris/stargazers"><img src="https://img.shields.io/github/stars/z89/auris?style=flat-square&color=8fd3ff&labelColor=1b1a20" alt="stars"></a>
  <a href="https://github.com/z89/auris/commits/main"><img src="https://img.shields.io/github/last-commit/z89/auris?style=flat-square&color=8fd3ff&labelColor=1b1a20" alt="last commit"></a>
  <a href="../LICENSE"><img src="https://img.shields.io/github/license/z89/auris?style=flat-square&color=8fd3ff&labelColor=1b1a20" alt="license"></a>
</p>

aurisd is a Rust daemon that speaks the Apple Accessory Protocol (AAP) over
the L2CAP control channel on PSM 0x1001. it reports battery per bud and case,
in-ear state, noise control, adaptive level, conversational awareness and
device metadata, writes typed settings and verifies them against the
accessory's own report. it runs as an unprivileged systemd user service: no
root, no patched BlueZ.

this is the `daemon` half of [auris](../README.md). the bar plugin lives one
directory up.

this file is the reference for the daemon, the `auris` cli and the config
file: build and install, then the cli, then the daemon's own flags, then
config keys, then the state file and control socket.

## build and install

with Rust 1.85 or newer, from this directory:

```sh
cargo install --path .
mkdir -p ~/.config/systemd/user
sed 's#/usr/bin/aurisd#%h/.cargo/bin/aurisd#' dist/aurisd.service > ~/.config/systemd/user/aurisd.service
systemctl --user daemon-reload
systemctl --user enable --now aurisd
```

pair and connect the AirPods as usual. the daemon notices, opens its own link
next to the audio one, and starts writing.

## CLI reference

every `auris` subcommand accepts the global option:

| option | meaning |
|---|---|
| `--runtime-dir <PATH>` | override the runtime directory (default `$XDG_RUNTIME_DIR/aurisd`) |
| `-h`, `--help` | print help |
| `-V`, `--version` | print version (top level only) |

### `auris status [--json]`

shows the current state. `--json` prints the raw `state.json` object instead
of a summary.

```sh
auris status
auris status --json
```

### `auris noise <MODE>`

sets the noise control mode. `<MODE>` is one of `anc`, `transparency`,
`adaptive`, `off`.

```sh
auris noise anc
```

### `auris ca <STATE>`

turns conversational awareness on or off. `<STATE>` is `on` or `off`.

```sh
auris ca on
```

### `auris adaptive <LEVEL>`

sets the adaptive transparency level. `<LEVEL>` is an integer, 0 to 100.

```sh
auris adaptive 60
```

### `auris reconnect`

drops and re-establishes the AAP link. one guarded attempt; requires an
existing local Bluetooth connection.

```sh
auris reconnect
```

### `auris connect-once`

asks BlueZ once to connect the pinned paired device. it never retries
automatically, and it may transfer audio away from another host.

```sh
auris connect-once
```

### `auris handoff <ACTION>`

controls Apple multi-host switching. `<ACTION>` is `on`, `off` or `status`.

```sh
auris handoff on
auris handoff status
```

### `auris take-over`

takes the AirPods over from another Apple host now, acting immediately on an
open AAP link.

```sh
auris take-over
```

### `auris yield`

gives the AirPods up to another Apple host now, acting immediately on an open
AAP link.

```sh
auris yield
```

### `auris rename <NAME>`

renames the AirPods accessory. quote names containing spaces.

```sh
auris rename 'Desk AirPods'
```

see [rename](#rename) below for how the write is verified.

### `auris setting <KEY> <JSON_VALUE>`

sets a typed AirPods setting using a JSON value. `<KEY>` is a setting key such
as `microphone` or `personalized_volume`. `<JSON_VALUE>` is a JSON string,
boolean, or array appropriate for the key.

```sh
auris setting microphone '"left"'
auris setting personalized_volume true
auris setting listening_mode_cycle '["anc","transparency"]'
```

#### setting keys

`setting` values are JSON. enum values need quotes. booleans do not. mode
cycles are arrays.

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
AAP link. the cli validates them before opening the daemon socket.

#### what a setting write reports

`auris setting` does not stop at "sent". it watches the daemon's readback and
prints one line when the verdict lands, within ten seconds.

| line | exit | meaning |
|---|---|---|
| `confirmed by AirPods` | 0 | a fresh dump reported exactly what was written |
| `AirPods kept <value>` | 1 | the accessory reported something else; that value is in `settings` |
| `applied; AirPods 4 (ANC) never reports this setting back` | 0 | the readback ran; this model never reports the key. microphone and listening-mode cycle only |
| `sent but not verified: <reason>` | 1 | the write went out but no readback could be completed |

the readback drops and reopens the AAP link once, about a second and a half
after the last write. it does that because the accessory dumps its settings
only at link open. `verify_reopen` in `state.json` marks that window.

#### rename

`auris rename` is verified the same way, under the key `name` in
`settings_requested` and `settings_status`.

the accessory pushes its own metadata after every AAP handshake. so the
readback reopen reads the name back from the AirPods themselves. it does not
wait for BlueZ, which re-reads the remote name only when a link opens and can
therefore be many minutes stale.

a reported name that differs from the one written is a `mismatch`.
`device.name` then holds the name the accessory kept.

the daemon prefers that metadata name over the BlueZ `Name` or `Alias`. it
never writes `Alias` itself.

a rename changes the accessory's own name only. Apple hosts keep the name in
their own pairing record and never re-read it from the accessory. so a rename
sent from Linux shows up on Linux and other non-Apple hosts, while an iPhone
or Mac keeps showing whatever it called the AirPods.

### exit codes

these apply to every `auris` subcommand:

| code | meaning |
|---|---|
| 0 | the daemon took the command |
| 1 | the daemon refused, or local input validation failed |
| 2 | the daemon is not running |

## daemon flags

`aurisd` itself takes no subcommands, only flags:

| flag | meaning |
|---|---|
| `--runtime-dir <PATH>` | override the runtime directory (default `$XDG_RUNTIME_DIR/aurisd`) |
| `--device <BD_ADDR>` | pin to one accessory instead of auto-detecting |
| `--dump-schema` | print an example `state.json` and exit |
| `-h`, `--help` | print help |
| `-V`, `--version` | print version |

### environment variables

mostly for poking at things:

- `RUST_LOG=aurisd=debug` for identifiers and lengths it does not model.
  `trace` for packet framing diagnostics. raw AAP payloads and AAP metadata
  strings are not logged
- `AURISD_FEATURES=alt` pins the second set-features variant instead of
  letting the daemon find one
- `AURISD_FEATURES=ff` pins the `0xff` selector byte. on AirPods 4 (ANC) it
  produces the same dump as the default `d7`. it exists for firmware that
  answers neither of the others
- `AURISD_RAW_SOCKET=1` uses a plain libc socket instead of bluer's

## config reference

defaults need no configuration. `~/.config/aurisd/config.toml` (or
`$XDG_CONFIG_HOME/aurisd/config.toml`) can pin identity and opt into extra
behavior. every key is optional; a missing file is fine. an invalid file
stops startup rather than being silently ignored.

```toml
device = "AC:DE:48:00:11:22"  # pin one address instead of picking the AirPods BlueZ knows about
primary_bud = "auto"          # which bud the AirPods call primary. "auto", "left" or "right"

[ble]
enabled = false              # opt-in only; see the BLE setup/evidence guide
# key_file = "/absolute/private/path/keys.json"
scan_seconds = 8
interval_seconds = 30
freshness_seconds = 75

[handoff]
enabled = false              # Apple multi-host switching; `auris handoff on` sets this
take_over_on_play = true     # take the AirPods back when a local player starts
# bt_name = "Mac"            # name announced to other Apple hosts (1-32 bytes)
rejoin_after_eviction = true # reconnect once after an Apple host's fresh link evicts this host

[ear]
auto_pause = true            # pause local players when a bud leaves an ear
auto_resume = true           # resume the player auris paused, once the bud is back in
pause_on_one_of_two = true   # pause as soon as one of two in-ear buds leaves, not just the last one

[autoconnect]
enabled = true                    # page the AirPods when their BLE proximity advert appears
min_rssi = -90                    # ignore adverts weaker than this, in dbm
settle_seconds = 3                # wait this long after the first advert before paging
absence_seconds = 15              # silence at least this long ends a presence episode
fallback_first_seconds = 20       # wait this long after a disconnect before the first page sent without an advert
fallback_interval_seconds = 45    # wait between later pages sent without an advert
fallback_minutes = 10             # keep paging without an advert for at most this long; 0 disables the fallback
```

### `device` and `primary_bud`

- `device` pins one BD_ADDR instead of auto-detecting the paired accessory.
- `primary_bud` sets which bud the accessory's primary byte in an ear
  detection packet describes. the accessory never says which side is
  primary, and it changes as buds go in and out of the ear, so `"auto"`
  resolves it from the battery packet instead of guessing a fixed side.
  default `"auto"`.

### `[ble]`

opt-in bounded BLE battery observation. requires provisioned identity keys.
see [battery observation and handoff safety](../docs/BATTERY_AND_HANDOFF.md)
before enabling it.

| key | default | effect |
|---|---|---|
| `enabled` | `false` | turns the observer on. requires an absolute `key_file` path |
| `key_file` | none | private JSON file with `address`, `irk` and `encryption_key` fields |
| `scan_seconds` | `8` | seconds of discovery in each interval, 2 to 30 |
| `interval_seconds` | `30` | seconds between the beginnings of discovery windows, at least `scan_seconds + 5` and at most 300 |
| `freshness_seconds` | `75` | age beyond which an observed BLE cell is shown as historical, at least `interval_seconds + scan_seconds` and at most 600 |

no keys are automatically requested or imported. discovery is disabled by
default. invalid BLE configuration fails closed.

### `[handoff]`

Apple multi-host switching over AAP.

| key | default | effect |
|---|---|---|
| `enabled` | `false` | yield to other Apple hosts and take over on local playback. `auris handoff on` sets this |
| `take_over_on_play` | `true` | take the AirPods back when a local player starts playing |
| `bt_name` | none (effective `"Mac"`) | name announced to other Apple hosts, 1 to 32 bytes with no control characters. an unusable value falls back to `"Mac"` |
| `rejoin_after_eviction` | `true` | reconnect once when an Apple host's fresh connection made the AirPods drop this host. needs `enabled` |

handoff needs BlueZ's `DeviceID` set to Apple and a re-pair. see the top-level
readme and
[Apple multi-host switching](../docs/BATTERY_AND_HANDOFF.md#apple-multi-host-switching).

`auris take-over` and `auris yield` act immediately on an open AAP link,
independent of `take_over_on_play`.

### `[ear]`

in-ear detection driving the local media players. on by default: it is what
the hardware is for, and what other platforms already do with the 0x0006
report.

| key | default | effect |
|---|---|---|
| `auto_pause` | `true` | pause local players when a bud leaves an ear |
| `auto_resume` | `true` | resume the player auris paused, once the bud is back in and nothing else has touched playback meanwhile |
| `pause_on_one_of_two` | `true` | pause as soon as one of two in-ear buds leaves, matching macOS. `false` waits for the last bud, for one-bud listeners who swap sides while the music plays |

### `[autoconnect]`

pages the AirPods when their BLE proximity advert appears. BlueZ never pages
a classic device on its own, so a host only gets the AirPods when they happen
to page it; a Mac listening for the same advert wins that race otherwise.

| key | default | effect |
|---|---|---|
| `enabled` | `true` | scan for the advert and page once per presence episode |
| `min_rssi` | `-90` | ignore adverts weaker than this, in dBm, to keep another room out |
| `settle_seconds` | `3` | wait this long after the first advert before paging, so a nearer preferred host gets the link first |
| `absence_seconds` | `15` | silence of at least this long ends a presence episode; the next advert after it is a new case opening |
| `fallback_first_seconds` | `20` | wait this long after a remote or timed-out disconnect before the first page sent without an advert |
| `fallback_interval_seconds` | `45` | wait between later pages sent without an advert |
| `fallback_minutes` | `10` | keep paging without an advert for at most this long after the disconnect. `0` turns the fallback off and leaves only the advert trigger |

### BLE safety

BLE requires provisioned identity keys and a pinned paired device. see
[battery observation and handoff safety](../docs/BATTERY_AND_HANDOFF.md)
before enabling it.

malformed main configuration stops startup. it does not silently discard the
pinned device and fall back to a different paired accessory.

## state and socket

### what it writes

`$XDG_RUNTIME_DIR/aurisd/state.json`, replaced atomically on every change.
read it, watch it, whatever you like. `aurisd --dump-schema` prints an
example:

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
  "link": { "status": "reconnecting", "reason": "bud_switch", "attempt": 1, "since": "2026-09-16T02:01:47Z" }
}
```

#### daemon

- `daemon.source` is `aap` while the control link is up. otherwise it is
  `ble` if a current BLE battery observation exists, otherwise `none`.
- BLE observation never implies connected audio.

#### battery

- each battery cell adds `source` and `fresh`. opening an AAP link does not
  refresh old cells; only reports do. `battery.stale` means no cell is fresh.
- the buds only relay the case while they sit in it. out of the case,
  `case.present` goes false, `level` keeps the last reading and `last_seen`
  says when.
- the levels are cached in `~/.cache/aurisd`, so they survive a restart.
- each cell's nullable `last_known_charging` retains its last reported
  charging state alongside `level` and `last_seen`. check per-cell `fresh`
  and `present` before treating it as live.
- old snapshots default missing `fresh` to false in the daemon. the plugin
  supports older snapshots with the legacy global stale flag.

battery cache rules:

- caches are private (0600) and bound to the paired address.
- startup restores them only for an explicitly pinned matching device.
- old unbound cache files are ignored, not deleted. new reports replace them.

there is no fixed case-expiry timer. battery packets mark components absent.

the separate AAP idle watchdog waits 300 seconds, then allows a 15-second
probe grace. that detects a silent control link, not a case timeout. the
firmware's case-reporting window has not been measured here.

#### ear and buds

- `ear` is `in`, `out`, `case` or `unknown`. the current BLE observer only
  adds battery observations. it does not guess lid or ear state.
- 0x0006 names a primary and a secondary bud, never a left and a right. the
  role moves: whichever bud is out and working is the primary one.
- `primary_bud = "auto"` resolves it from the battery packet, which does name
  its component. so a bud on its own is mapped to the side it is actually on.
- with both buds out there is nothing to go on, and it falls back to
  left-first. pin `"left"` or `"right"` if you want it fixed.
- `serial` and `firmware` come from the AirPods themselves. they are `null`
  until the AirPods send them.

#### link

`link` says whether a missing link is healing itself. a ui can then keep the
device on screen instead of dropping it for the six seconds a rejoin takes.

`link.status`:

| value | when |
|---|---|
| `connected` | whenever `device.connected` is true |
| `reconnecting` | while a rejoin or auto-connect sequence is scheduled or connecting |
| `disconnected` | otherwise |

`link.reason` is `null` unless the status is `reconnecting`:

| value | meaning |
|---|---|
| `bud_switch` | the `0x0C` primary bud address report changed in the 15 seconds before the drop, so the buds moved their radio link to the other bud and cut this host doing it |
| `taken_over` | an Apple host took the AirPods |
| `link_lost` | a supervision timeout |
| `auto_connect` | the proximity pager bringing them back after the case opened |

- `link.attempt` counts the `Device1.Connect` calls started in the current
  sequence. it is `0` while the sequence is only waiting out its delay.
- `link.since` is when the sequence started, rfc3339 in utc with a `Z`. it is
  `null` when there is no sequence.
- both reset when the link comes back.

#### settings

`settings_api` values:

| value | meaning |
|---|---|
| `3` | a rename is verified as well, under the key `name` in the maps below |
| `2` | the readback fields below are present |
| `1` | typed settings without them |
| `0` | old schema-1 documents, which omit the field |

- each value under `settings` is last confirmed device-reported state. `null`
  means unknown. it does not mean off or unsupported.
- additive `settings_report_seq` maps setting keys to report counters. an
  actual repeat report increments its key. unrelated battery updates do not.
  counters persist across control-link loss in the process, reset for a new
  device or daemon, and are not command IDs.
- `settings_requested` is the last value written per key in this daemon's
  lifetime, in the same JSON shape `settings` uses for that key. it is what
  was asked for, never what the accessory holds. the key `name` is a
  requested accessory name, and its reported counterpart is `device.name`.

`settings_status` is the readback verdict per requested key. the write itself
went out in every case:

| value | meaning |
|---|---|
| `verifying` | written, readback running |
| `confirmed` | a fresh dump reported the requested value |
| `mismatch` | a fresh dump reported something else, and `settings` holds that value |
| `unreported` | the readback ran and this model never reports the key: microphone and listening-mode cycle on AirPods 4 (ANC) |
| `unverified` | no readback could be completed within 10 s |

`settings_verify` is `idle`, `scheduled` (a write is waiting out the 1500 ms
debounce) or `reopening` (the AAP link is being reopened to read settings
back).

`verify_reopen` is true only while that readback reopen is in progress.
`device.connected` and `device.aap_link` can go false during it. a ui should
keep showing the device as connected while this flag is true.

##### why the readback is a reopen

the accessory stores a setting write silently. it dumps its settings exactly
once per AAP link, about 13 ms after the set-features ack.

nothing sent on an open link makes it dump again. so the readback is a link
reopen:

1. one write datagram
2. a debounce, so a burst of writes shares one reopen
3. a fresh link, whose dump is compared against `settings_requested`

only reports from that new link count. no write is ever replayed. BlueZ is
never asked to reconnect the device.

### control socket

commands go over `$XDG_RUNTIME_DIR/aurisd/ctl.sock`, one JSON object per line.
the cli is a thin wrapper, so anything else can talk to it too:

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

answers are `{"ok":true}`, `{"ok":false,"error":"..."}`, or the state object
for `status`.

`subscribe` answers with the state object. it then sends another one every
time anything changes, one per line, until you hang up.

that connection stops reading requests, so send commands on a second one.

it exists so a ui does not have to poll `state.json`. the file is replaced by
rename, which file watchers do not reliably follow. the alternative is a
timer that is both late and always running.

## works with

anything that speaks AAP. names are known for these. anything else shows as
its model id:

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

tested on AirPods 4 (ANC). noise control on models without it does nothing;
the AirPods just ignore the command.

## how it works

macOS and iOS talk to AirPods over AAP, Apple's Accessory Protocol. it runs
on an L2CAP channel next to the audio link.

the channel is PSM 0x1001. on Linux that is in the dynamic range, so any user
can connect to it once BlueZ has the classic link up. no capability, no
vendor id trick.

### opening sequence

it waits for answers rather than sleeping:

```
-> 00 00 04 00 01 00 02 00 00 00 00 00 00 00 00 00   handshake
<- 01 00 04 00 ...                                   handshake ack, waited for up to 3 s
-> 04 00 04 00 4d 00 d7 00 00 00 00 00 00 00         set features, 14 bytes
<- 04 00 04 00 2b 00 ...                             features ack, up to 2 s, optional
-> 04 00 04 00 0f 00 ff ff ff ff ff                  request notifications
```

set features has to be exactly 14 bytes. a 13 byte write goes through, and the
AirPods quietly never send a battery packet.

there are two known selector bytes, `d7` and `0e`. some firmware only answers
one of them. so the daemon alternates per dial until one produces a battery
packet, then sticks with it.

on the `0e` variant it also subscribes before negotiating features. that is
what the go daemon below does.

### what the AirPods push

| opcode | what |
|---|---|
| 0x0004 | battery, one entry per component. `02` right, `04` left, `08` case. status `01` charging, `02` discharging, `04` not here |
| 0x0006 | ear detection, primary then secondary. `00` in ear, `01` out, `02` in case. other values seen in the wild, `03` among them, read as unknown |
| 0x0009 | a control setting echoed back. `0d` noise control, `28` conversational awareness, `2e` adaptive level |
| 0x001D | metadata, nul separated strings. pushed once, cannot be asked for |
| 0x004B | speech ducking while conversational awareness is on. a level, not the on/off state |

everything else is logged at debug and dropped.

### recovery rules

if no battery packet arrives within 10 s, the daemon asks again twice on the
existing socket, then suppresses recovery.

peer loss, send errors and unanswered idle probes also suppress automatic
redial.

another guarded attempt needs either a new observed local disconnected to
connected transition, or an explicit user command.

each AAP dial checks current BlueZ state. it cancels when the observed link
generation changes.

there is no unattended Bluetooth-connect loop. `connect-once` is a separate,
explicit command for a pinned paired device. it may take audio from another
host.

these checks reduce races. they do not prove that another host is idle. they
do not guarantee protection from other applications' or BlueZ's connection
policies.

## related

these are where the protocol facts came from. nothing is copied from them.

- [battery observation and handoff safety](../docs/BATTERY_AND_HANDOFF.md),
  including opt-in setup, protocol references and limitations
- [feature comparison](../docs/FEATURE_COMPARISON.md), including the
  distinction between implemented and hardware-verified behavior
- [LibrePods](https://github.com/kavishdevar/librepods), the most complete
  AAP reference around
- [airpods-battery](https://github.com/AlwxSin/airpods-battery), a go daemon
  with the alternate opening sequence
- [omarchy-pods](https://github.com/thisisgm/omarchy-pods), the same idea for
  a different shell

## license

mit
