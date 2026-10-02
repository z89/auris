# protocol evidence and verification for advanced settings

observations for AirPods 4 ANC (model id `201B`) speaking AAP over L2CAP PSM
`0x1001`. they are not fixtures shipped with the repository, and auris is
written independently against them. battery, in-ear state, BLE, identity
matching, radio limits and handoff opcodes are in
[battery observation and handoff safety](BATTERY_AND_HANDOFF.md).

## message classes and opcodes

| opcode | meaning | notes |
|---|---|---|
| `0x08` | connection info | re-emitted after a settings write, no setting payload |
| `0x09` | control report or write | control identifier plus payload (table below) |
| `0x0C` | bud address report | changes on a primary role switch |
| `0x0D` | noise mode | echoes on change. of the settings below only call controls (`24`) also echoes |
| `0x0E` | audio source | which host is sending audio to the AirPods. 6-byte host address (byte-reversed), then `00` none, `01` call or `02` media. also re-emitted after a settings write, with no setting payload |
| `0x1A` (top level) | rename | unrelated to control `0x1A` inside `0x09` |
| `0x1D` | device metadata | the accessory's own name, used to verify a rename |
| `0x2e` | connected devices | not a settings report |

the `0x0E` and `0x2e` layouts are in
[Apple multi-host switching](BATTERY_AND_HANDOFF.md#apple-multi-host-switching).
control `0x17` (press speed) travels inside `0x09`. top-level opcode `0x17`
carries sensor and head-tracking traffic in LibrePods and is not a settings
list.

## handshake, subscription and the settings dump

the accessory dumps its settings as `0x09` packets exactly once per L2CAP
link, about 13ms after the set-features ack. re-sending request-notifications
(five-byte or four-byte form), set-features (`0xd7` or `0xff`) or the whole
handshake triple mid-link gives no second dump. reopening the AAP link does,
and a press-speed write followed by `auris reconnect` shows the new value
every time. set-features `0xff` (`FeaturesVariant::Ff`, `AURISD_FEATURES=ff`)
dumps the same as the default `0xd7` and stays opt-in.

reported at link open are `0x17`, `0x18`, `0x24`, `0x26` and the unmodelled
`0x1F` (chime volume), `0x1B`, `0x35` and `0x3E`. microphone (`0x01`) and
listening-mode cycle (`0x1A`) are never reported, at link open, after a
write, or with either features byte. that proves missing readback only, not
rejection or missing hardware. a trace from other firmware is needed to rule
out unusual framing.

## noise control and listening modes

the cycle mask is off `01`, ANC `02`, transparency `04`, adaptive `08`, or a
combination. Apple documents multiple stem-cycle modes. LibrePods refuses to
leave fewer than two, and auris keeps that two-mode write minimum. the UI has
no apply button for the cycle because nothing echoes it. a one-mode mask is
still kept on read, since write validation and report decoding are separate.

noise mode (`0x0D`) echoes immediately. conversational awareness is sent once
after subscribe, not on every change. off permission (`0x34`) and
hold-duration timing need device verification before auris relies on them,
and auris never changes the off permission to make a cycle work.

## settings writes, readback and the verification contract

writes are `04 00 04 00 09 00 ID D1 D2 D3 D4`, unused bytes zero. reports
are `0x09` with the same identifier and are decoded from the full payload,
because call controls cannot be resolved from the first byte.

| setting | id | values | not yet confirmed |
|---|---|---|---|
| microphone | `01` | auto `00`, right `01`, left `02` | never echoed, the bud needs a live call to confirm |
| press speed | `17` | default `00`, slower `01`, slowest `02` | not echoed, the report arrives only on reopen (checked 2026-10-04), gesture timing unverified |
| hold duration | `18` | default `00`, shorter `01`, shortest `02` | not echoed, the report arrives only on reopen (checked 2026-10-04), threshold and persistence unverified |
| listening-mode cycle | `1A` | bitmask above | never echoed, unsupported modes and stem behaviour unverified |
| call controls | `24` | hang up once/mute twice `00 02`, mute once/hang up twice `00 03` | echoed both ways (checked 2026-10-04), answer press not remapped, unverified on a live call |
| personalised volume | `26` | on `01`, off `02` | not echoed either way, the report arrives only on reopen (checked 2026-10-04), audible effect unverified, not host call-audio ducking |

press-speed writes that produced no control report.

```
04 00 04 00 09 00 17 01 00 00 00   (slower)
04 00 04 00 09 00 17 00 00 00 00   (default)
```

the report counter did not move, and a new notification request did not
help. reopening only the AAP link returned the new value with a newer
per-link counter, and does the same for hold duration and personalised
volume. call controls report changed and restored values immediately. a repeat
run on 2026-10-04 gave the same result for all four. no bluetooth connection or
audio service is restarted.

### verify-by-reopen design

a write is a request plus a readback. auris never sends an invented read
packet and never falls back to success.

1. **write.** records `settings_requested[key]`, sets
   `settings_status[key] = "verifying"` and `settings_verify = "scheduled"`,
   and arms a 1500ms debounce that each new write restarts.
2. **reopen.** sets `settings_verify = "reopening"` and `verify_reopen = true`,
   then drops and redials the AAP socket as `reconnect` does. bluez is never
   asked to reconnect and an unconnected device is never dialled. with no
   link, every verifying key becomes `"unverified"`.
3. **compare.** the window ends at the first battery packet after the opening
   sequence (where the features variant is pinned), or 2000ms after subscribe.
   values are compared with a per-link counter, not `settings_report_seq`.

| outcome | status |
|---|---|
| equal | `"confirmed"` |
| different | `"mismatch"`, `settings` holds the device value |
| never reported | `"unreported"` (expected for microphone and cycle) |
| no verdict 10s after the write | `"unverified"` |

any other link loss keeps `settings_requested` and marks verifying keys
`"unverified"`. every later link open rechecks keys left `"unverified"` or
`"unreported"`. `connected` and `aap_link` drop briefly during the reopen,
so the UI stays connected while `verify_reopen` is true. `settings_api = 3`
adds `name` to `settings_requested` and `settings_status`. CLI output and
exit codes are in the [daemon reference](../daemon/README.md#cli-reference).

### diagnostics

DankMaterialShell (DMS) and daemon logs omit device names and payload values.
a change is traced by these lines in order, and a datagram-send line alone is
not confirmation.

1. DMS `setting command started` (key, local request number)
2. daemon `sending AirPods setting`, then `setting datagram sent`
3. daemon `setting report received`, or a malformed or unmodelled report diagnostic
4. DMS completion, matching report or confirmation timeout

the UI shows `UI revision` and logs `auris: UI loaded revision`. a plugin
reload response does not prove a new root revision loaded.
`tools/QmlReloadTest.cpp` shows a new file-URL query loads modified source in
a plain qt engine while an existing instance keeps its old revision. it does
not prove the live DMS and quickshell reload path.

## device metadata and rename

rename is `04 00 04 00 1A 00 01 LL 00 <UTF-8 name>`, `LL` the byte count. the
name is nonblank, at most 255 bytes, free of control characters, never
trimmed and never executed. a send or a bluez alias change proves nothing.
the rename is confirmed only when `0x1D` metadata on the reopened link
reports it, and `device.name` carries what was reported.

new fields stay unknown until a valid report arrives. a known model plus a
written packet does not prove firmware support, and unknown values are never
coerced to a default or read as off.

## multi-host ownership and smart routing

ownership, opcodes and rejoin rules are in
[Apple multi-host switching](BATTERY_AND_HANDOFF.md#apple-multi-host-switching).

### macOS banner text for a take-over by this host

`BluetoothUIService` renders the banner. its strings are at
`/System/Library/CoreServices/BluetoothUIService.app/Contents/Resources/Localizable.loctable`
on the sealed system volume (arm64 Mac, SIP enabled). `audioaccessoryd` maps
the `btName` in smart-routing media info to a class and builds
`MOVED_TO_<CLASS>`. English strings exist for `MOVED_TO_IPHONE`,
`MOVED_TO_IPAD`, `MOVED_TO_MAC`, `MOVED_TO_WATCH`, `MOVED_TO_APPLETV` and
`MOVED_TO_IPOD`. unknown names fall back to iPhone. `HomePod` has no string,
so the raw key `MOVED_TO_HOMEPOD` shows. the Mac learns the name only when it
recreates its entry for this host, on link establishment.

### primary bud role switch drops a non-Apple host

about 30s after link-up, as a bud is taken out, the `0x0C` report changes to
the other bud's address. about 3s later the AirPods reset this host's link
with `Reason.Remote`, and once it ended as a supervision timeout instead.
Apple hosts survive because their exposed address does not change. the drop
may be specific to the BCM20702A0 dongle's 2012 firmware. LibrePods has no
handover either and reconnects the same way. the rejoin rules cover both
outcomes and the link is back in about 6s.

## later-feature cautions

- `31` (in-case/charging tones) is not a locating sound. Find My case access
  raises separate reachability and authentication questions.
- `35` (sleep detection) targets recent firmware with no published AirPods 4
  minimum and alone does not justify a sleep classifier.
- `16` (hold-action configuration) is not a full stem-event protocol. verify
  event delivery before building linux actions on it.
- `39` (stem configuration) does not document every gesture event.
- ownership commands are not ordinary bluetooth multipoint.
- microphone side does not give the high-quality AACP microphone transport or
  solve audio-profile switching.

media automation follows
[MPRIS player semantics](https://specifications.freedesktop.org/mpris/latest/Player_Interface.html)
with explicit playback ownership. auto-connect follows
[bluez Device1 semantics](https://bluez.readthedocs.io/en/latest/device-api/)
with opt-in, bounded retry and deliberate-disconnect handling. gates and
order are in [features](FEATURES.md).

## primary references

- [Apple stem controls](https://support.apple.com/en-au/108764)
- [LibrePods control catalog](https://github.com/librepods-org/librepods/blob/53679cc/docs/control_commands.md)
- [LibrePods AACP manager](https://github.com/librepods-org/librepods/blob/53679cc/android/app/src/main/java/me/kavishdevar/librepods/bluetooth/AACPManager.kt)
- [LibrePods control repository](https://github.com/librepods-org/librepods/blob/53679cc/android/app/src/main/java/me/kavishdevar/librepods/data/ControlCommandRepository.kt)
- [LibrePods linux command framing](https://github.com/librepods-org/librepods/blob/53679cc/linux/BasicControlCommand.hpp)
- [LibrePods cycle UI](https://github.com/librepods-org/librepods/blob/53679cc/android/app/src/main/java/me/kavishdevar/librepods/presentation/viewmodel/AirPodsViewModel.kt)
