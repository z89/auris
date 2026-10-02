<h1 align="center">auris</h1>

<p align="center">
  <img src="https://img.shields.io/badge/rust-1.85%2B-8fd3ff?style=flat-square&labelColor=1b1a20" alt="rust 1.85+">
  <img src="https://img.shields.io/badge/aap-l2cap%200x1001-8fd3ff?style=flat-square&labelColor=1b1a20" alt="AAP on L2CAP PSM 0x1001">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-8fd3ff?style=flat-square&labelColor=1b1a20" alt="license MIT"></a>
</p>

auris brings the AirPods features of an Apple device to linux. it shows the battery for each bud and the case, switches noise control and changes the other AirPods settings, including press speed, hold duration, call controls and personalized volume. taking a bud out pauses playback and putting it back resumes it, and opening the case is enough to connect. it runs as a rootless user service on stock bluez and pipewire, with no kernel patch, no forked bluetooth stack and nothing running as root.

the `aurisd` daemon speaks the Apple Accessory Protocol (AAP) over the same control channel Apple's own devices use, so the AirPods treat this machine as another Apple host. when a Mac, iPhone or iPad takes the AirPods over, auris pauses and steps aside, and pressing play here brings the audio back without dropping the connection. each setting counts as applied only once the AirPods report it back, and the link recovers by itself after another host evicts it or the connection drops. a command line tool, a JSON state file and an optional [DankMaterialShell](https://github.com/AvengeMedia/DankMaterialShell) (DMS) panel all read the same live state.

auris is its own project rather than part of [LibrePods](https://github.com/librepods-org/librepods), which mapped much of the same protocol and also covers Android. on linux LibrePods is a desktop app with a tray icon. its state lives inside that app, a setting counts as applied as soon as it is sent, playing media is found by checking every player twice a second, and a missing audio profile is fixed by restarting wireplumber. auris runs as a background service that other tools can read, waits for the AirPods to confirm each change, follows players as they change and never restarts wireplumber. those differences go deeper than a patch, and keeping auris MIT licensed means it builds on the protocol findings without using any of LibrePods' GPL code.

> tested on arch linux with bluez 5.87, pipewire 1.6.8 and wireplumber, against AirPods 4 (ANC), model id `0x201B`, with a Mac on macOS Tahoe 26.6.2 as the second host. other AirPods models, other bluez versions and other distributions are untested.

## ✨ highlights

- 🔄 **multi-host handoff** gives the AirPods up when a Mac, iPhone or iPad claims them and takes them back when something plays here, with the connection left up.
- 👂 **in-ear control** pauses local players when a bud comes out and resumes them when it goes back in.
- 🩹 **a self-healing link** recovers once from an eviction, a bud role switch or a dropped link, and never redials a disconnect you made yourself.
- 📦 **auto-connect on case open** follows the case's BLE advert, with a paging fallback for adapters that miss it.
- ✅ **confirmed settings** are only reported as applied once the AirPods report the new value. microphone side and the listening-mode cycle get no report on this firmware, so the CLI and the panel label them unconfirmed rather than failed.
- 🧰 **a CLI and plain JSON** expose every control from a shell, with `state.json` and the control socket carrying the full snapshot for other tools.
- 🎛️ **a live panel** for DMS reads the same socket and updates as the daemon pushes changes.

## 📦 install

`aurisd` builds from the `daemon` directory with rust 1.85 or newer and runs as a systemd user service.

```sh
cargo install --path daemon
mkdir -p ~/.config/systemd/user
sed 's#/usr/bin/aurisd#%h/.cargo/bin/aurisd#' daemon/dist/aurisd.service > ~/.config/systemd/user/aurisd.service
systemctl --user daemon-reload
systemctl --user enable --now aurisd
```

that is the whole install. `auris status` should print the accessory within a few seconds of the AirPods connecting. the [daemon readme](daemon/README.md) documents every command, flag and config key. the [live panel](#-live-panel) is optional and installs separately.

## 📊 feature matrix

rows are checked against [LibrePods' latest README](https://github.com/librepods-org/librepods/blob/main/README.md).
the annotated version is in [features](docs/FEATURES.md).

| symbol | meaning |
| ------ | ------- |
| ✅ | implemented and works well |
| ⚪ | needs VendorID spoofing, use at your own risk |
| 🔴 | not implemented yet, planned |
| ⛔ | will not be implemented |
| ❓ | unknown |

| feature | LibrePods (linux) | auris | note |
|---|---|---|---|
| Changing Listening Mode | ✅ | ✅ | off, ANC, transparency and adaptive |
| Ear detection | ✅ | ✅ | shows whether each bud is in an ear, in the daemon, CLI and panel |
| Battery status | ✅ | ✅ | each bud and the case, with charging state. old readings are kept and marked stale |
| Renaming AirPods | ✅ | ✅ | the new name is read back from the AirPods to confirm it |
| Loud Sound Reduction | 🔴 | 🔴 | neither project has it on linux |
| Head Gestures | ⛔ | ⛔ | not possible on linux, for the same reason LibrePods gives |
| Conversational Awareness | ✅ | ✅ | turns it on and off, and reports when the AirPods lower the volume while you speak |
| Automatically connect to AirPods | ✅ | ✅ | connects when the case opens, on by default, needs LE enabled on the bluetooth adapter |
| Hearing Aid | 🔴 | 🔴 | neither project has it on linux |
| Transparency Mode customization | 🔴 | 🔴 | only the adaptive strength can be set, not the full set of transparency options |
| Multi-device connectivity (Bluetooth Multipoint; 2 devices only) | ⚪ | ⚪ | both hosts stay connected and pass the AirPods back and forth, after setting an Apple `DeviceID` in bluez and pairing again |
| Other accessibility configs | 🔴 | see rows below | LibrePods has these on Android only |
| ... Press speed | 🔴 | ✅ | only auris has it on linux. the AirPods confirm the new setting, but nobody has checked yet that presses feel different |
| ... Press and Hold duration | 🔴 | ✅ | only auris has it on linux. the AirPods confirm the new setting, but nobody has checked yet that holds feel different |
| ... Noise Cancellation with single AirPod | 🔴 | 🔴 | no known command turns it on, see [per-bud control](docs/RESEARCH.md#per-bud-control) |
| ... Volume control on swipe | 🔴 | 🔴 | neither project has it on linux |
| ... Volume swipe speed | 🔴 | 🔴 | neither project has it on linux |
| Other general configs | 🔴 | see rows below | LibrePods has these on Android only |
| ... Press and Hold to cycle between listening modes / invoke digital assistant (invoking digital assistant needs a recent firmware) | 🔴 | ❓ | auris sends the setting, but the AirPods never confirm it |
| ... Configure call controls | 🔴 | ✅ | only auris has it on linux. the AirPods confirm the new setting, but nobody has checked yet that calls behave differently |
| ... Personalized volume | 🔴 | ✅ | only auris has it on linux. the AirPods confirm the new setting, but nobody has checked yet that you can hear the difference |
| ... Loud Sound Reduction (needs VendorID spoofing) | 🔴 | 🔴 | neither project has it on linux |
| ... Microphone side | 🔴 | ❓ | auris sends the setting, but the AirPods never confirm it |
| ... Pause media when falling asleep (needs a recent firmware) | 🔴 | 🔴 | neither project has it on linux |
| ... Enable `Off listening mode` to switch to `Off` | 🔴 | ✅ | off is one of the four modes you can pick |
| Head-tracked Spatial Audio | ❓ | 🔴 | needs work in the linux audio stack, outside this project |
| Heart Rate Monitoring | ⛔ | ⛔ | AirPods 4 (ANC) has no heart rate sensor |
| Find My | ❓ | 🔴 | needs protocol and security work outside this project |
| High quality two-way audio | 🔴 | 🔴 | neither project has it on linux |
| Auto play/pause on ear detection | ✅ | ✅ | taking a bud out pauses local players and putting it back in resumes them |
| Seamless handoff between hosts | ✅ | ✅ | the AirPods pass between hosts the same way they do between Apple devices, tested against a Mac |

## 🔄 multi-host handoff

handoff is opt-in and works with any Mac, iPhone or iPad paired to the same AirPods. Apple devices only pass the AirPods to a host that reports Apple's bluetooth vendor id, so bluez needs that id set once and the AirPods need pairing again.

<p align="center"><img src="docs/img/handoff.svg" alt="handoff flow" width="1000"></p>

```sh
sudo sed -i 's/^#ReconnectAttempts=7/ReconnectAttempts=0/' /etc/bluetooth/main.conf
sudo sed -i '/^\[General\]/a DeviceID = bluetooth:004C:0000:0000' /etc/bluetooth/main.conf
sudo systemctl restart bluetooth
bluetoothctl remove <airpods address>   # then pair again
auris handoff on
```

when another device takes the AirPods, auris pauses playback, lets go of the audio and then hands them over. pressing play here takes them back, unless the other device is on a call. the bluetooth connection stays up throughout, so a switch never needs a reconnect.

`auris handoff status` shows which device has the AirPods, and `auris take-over` and `auris yield` switch by hand. the protocol is described in [Apple multi-host switching](docs/BATTERY_AND_HANDOFF.md#apple-multi-host-switching).

## 👂 in-ear control

taking a bud out pauses whatever is playing, and putting it back in resumes it. a pause you made yourself is never undone, and nothing resumes while another device has the AirPods.

`auto_pause`, `auto_resume` and `pause_on_one_of_two` under `[ear]` in `~/.config/aurisd/config.toml` switch each part off. the last one makes a single bud out enough to pause, as macOS does. all three are on by default, and the [daemon readme](daemon/README.md) covers every key, including `take_over_on_play` and `bt_name` for handoff.

## 🎛️ live panel

the panel is an optional DMS plugin. it reads the daemon's live state, so it updates the moment the AirPods report a change.

```sh
git clone https://github.com/z89/auris ~/.config/DankMaterialShell/plugins/auris
dms ipc call plugins enable auris
```

add `auris` to a bar under settings, bar, widgets. no shell restart is needed.

the bar pill shows the battery level, leaving out any bud that is charging. left click opens the panel and right click toggles ANC and transparency. the panel holds the battery for each bud and the case, the listening modes, conversational awareness and every AirPods setting. readings that stop updating stay visible and are marked stale, and the pill stays on the bar while the AirPods reconnect. what the pill shows and its battery warning levels are set under settings, plugins, auris.

## 🖥️ cli reference

`auris` talks to the daemon over the control socket and exits non-zero when the daemon is unreachable or the AirPods reject a command.

| command | arguments | what it does |
|---|---|---|
| `auris status` | `--json` | current snapshot, as a summary or the raw `state.json` object |
| `auris noise` | `anc`, `transparency`, `adaptive`, `off` | set the listening mode |
| `auris ca` | `on`, `off` | conversational awareness |
| `auris adaptive` | `0`-`100` | adaptive transparency strength |
| `auris setting` | `<key> <json-value>` | any typed setting, for example `auris setting microphone '"left"'` |
| `auris rename` | `<name>` | rename the AirPods, then confirm from their metadata |
| `auris handoff` | `on`, `off`, `status` | multi-host switching |
| `auris take-over` | | claim the AirPods from another Apple host now |
| `auris yield` | | give them up to another Apple host now |
| `auris reconnect` | | drop and re-establish the AAP control link only |
| `auris connect-once` | | ask bluez once to connect the pinned device, never retried automatically |

`--runtime-dir <PATH>` overrides `$XDG_RUNTIME_DIR/aurisd` for both `auris` and `aurisd`. the daemon also takes `--device <BD_ADDR>` to pin one accessory and `--dump-schema` to print an example `state.json`. everything else lives in `~/.config/aurisd/config.toml`, documented key by key in the [daemon readme](daemon/README.md).

## ✅ requirements

- linux with systemd user sessions, bluez 5 and pipewire.
- an adapter with LE enabled, for auto-connect on case open. bluez with `ControllerMode = bredr` turns LE off and leaves only the page fallback.
- rust 1.85 or newer to build.
- DMS 1.6 or newer, for the optional panel.

## 🩺 troubleshooting

| symptom | fix |
|---|---|
| CLI or panel says the daemon is missing | check `systemctl --user status aurisd`. cargo installs to `~/.cargo/bin`, and the plugin also looks in `~/.local/bin` |
| AirPods connected but nothing reports | give the daemon a few seconds after the link comes up, then read `journalctl --user -u aurisd` |
| handoff does nothing | check the `DeviceID` line in `/etc/bluetooth/main.conf` and that the AirPods were paired again after adding it |
| no connect when the case opens | if `auris status` says LE is off, bluez runs with `ControllerMode = bredr`. set it to `dual` (or remove it) and run `sudo systemctl restart bluetooth`. aurisd restarts the scan by itself. while LE is off only the page fallback connects, and only in the first 10min after a drop |

on a MediaTek MT7925 with bluez 5.87, pairing hung in dual mode and completed with `ControllerMode = bredr`. use `bredr` only while pairing, then switch back to `dual` so case-open auto-connect works. dual mode connects normally on that card once paired, and the advert trigger was checked there.

## 🏗️ architecture

<p align="center"><img src="docs/img/architecture.svg" alt="architecture" width="820"></p>

the AirPods give every host two links. bluez and pipewire handle the audio profiles, and `aurisd` opens the AAP control channel on L2CAP PSM 0x1001 beside them, completes the Apple handshake and subscribes to notifications. everything the CLI and the panel show comes from those notifications, never from guesses about the audio stream.

| module | job |
|---|---|
| aap session | owns the L2CAP socket, handshake and subscription, and parses battery, ear, metadata and control echoes |
| handoff | combines MPRIS playback edges with the AirPods' ownership messages to decide when to yield and when to claim |
| autoconnect | matches the case's BLE advert, asks bluez to connect and reports the scan state in `state.json` |
| rejoin | classifies a lost link from bluetooth events and reconnects when another host or a bud role switch caused it |
| settings | writes a control, waits for the AirPods' report and marks the value confirmed or mismatched |
| ear media | turns in-ear changes into pause and resume for local players |
| store | folds every update into one snapshot, writes `state.json` atomically and pushes changes to socket subscribers |

clients hold no bluetooth code. they read the socket or the state file and send commands back over the socket, so `auris status` and the panel cannot disagree.

## 📚 documentation

- [daemon readme](daemon/README.md) covers every command, flag, config key, the state file and the socket.
- [battery and handoff](docs/BATTERY_AND_HANDOFF.md) covers telemetry, the ownership protocol, reconnects and the audio route.
- [protocol evidence](docs/PROTOCOL_EVIDENCE.md) covers opcodes, control ids and what each report proves.
- [features](docs/FEATURES.md) holds the annotated matrix and the status of every feature.
- [research](docs/RESEARCH.md) covers per-bud control, a Mac-side helper, spatial audio and motion gestures.

## 📄 license

MIT
