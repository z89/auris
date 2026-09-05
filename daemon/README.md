<p align="center">
  <img src="../assets/auris.svg" width="112" alt="aurisd">
</p>

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
auris reconnect              # drop the link and dial again
```

exit code 0 when the daemon took the command, 1 when it refused (no airpods connected, say), 2 when the daemon is not running.

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
    "left":  { "level": 87, "charging": false, "present": true, "last_seen": "2026-09-05T10:22:31+10:00" },
    "right": { "level": 85, "charging": false, "present": true, "last_seen": "2026-09-05T10:22:31+10:00" },
    "case":  { "level": 62, "charging": true,  "present": true, "last_seen": "2026-09-05T10:22:31+10:00" }
  },
  "ear": { "left": "in", "right": "in" },
  "lid": "unknown",
  "noise_control": "anc",
  "conversational_awareness": true,
  "adaptive_level": 50
}
```

- `source` is `aap` while the link is up and `none` otherwise. `ble` is reserved for a later proximity-advert fallback
- when the airpods go away the file stays, with `connected: false`, `stale: true` and the last numbers
- the buds only relay the case while they sit in it. out of the case, `case.present` goes false, `level` keeps the last reading and `last_seen` says when. the levels are cached in `~/.cache/aurisd` so they survive a restart
- `ear` is `in`, `out`, `case` or `unknown`. `lid` is always `unknown` until the ble fallback exists
- `serial` and `firmware` come from the airpods themselves and are `null` until they send them

commands go over `$XDG_RUNTIME_DIR/aurisd/ctl.sock`, one json object per line. the cli is a thin wrapper, so anything else can talk to it too:

```
{"cmd":"set_noise_control","value":"anc"}
{"cmd":"set_conversational_awareness","value":true}
{"cmd":"set_adaptive_level","value":50}
{"cmd":"reconnect"}
{"cmd":"status"}
```

answers are `{"ok":true}`, `{"ok":false,"error":"..."}`, or the state object for `status`.

## config

none needed. `~/.config/aurisd/config.toml` exists for two things:

```toml
device = "AC:DE:48:00:11:22"  # pin one address instead of picking the airpods bluez knows about
primary_bud = "left"          # which bud the airpods call primary. try "right" if ear detection looks swapped
```

and a few environment variables, mostly for poking at things:

- `RUST_LOG=aurisd=debug` for the packets it does not model, `trace` for every byte
- `AURISD_FEATURES=alt` pins the second set-features variant instead of letting the daemon find one
- `AURISD_RAW_SOCKET=1` uses a plain libc socket instead of bluer's

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
| 0x0006 | ear detection, primary then secondary. `00` in ear, `01` out, `02` in case |
| 0x0009 | a control setting echoed back. `0d` noise control, `28` conversational awareness, `2e` adaptive level |
| 0x001D | metadata, nul separated strings. pushed once, cannot be asked for |
| 0x004B | speech ducking while conversational awareness is on. a level, not the on/off state |

everything else is logged at debug and dropped. if no battery packet arrives within 10 s the daemon asks again twice, then redials. a link that dies while bluez still says connected is redialed with backoff, three times per connection, then every five minutes.

## related

these are where the protocol facts came from. nothing is copied from them.

- [librepods](https://github.com/kavishdevar/librepods), the most complete aap reference around
- [airpods-battery](https://github.com/AlwxSin/airpods-battery), a go daemon with the alternate opening sequence
- [omarchy-pods](https://github.com/thisisgm/omarchy-pods), the same idea for a different shell

## license

mit
