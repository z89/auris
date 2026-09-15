# advanced settings: protocol evidence and verification

protocol observations for AirPods 4 ANC (model id `201B`) speaking AAP over
L2CAP PSM `0x1001`. these are protocol observations, not captured fixtures
shipped with the repository; the implementation in auris is written
independently against them.

## message classes and opcodes

| opcode | meaning | notes |
|---|---|---|
| `0x08` | connection info | re-emitted after a settings write, carries no setting payload |
| `0x09` | accessory control report / write | carries a control identifier plus payload bytes; see the settings table below |
| `0x0C` | bud address report | reports the left/right bud's Bluetooth address; changes on a primary-role switch |
| `0x0D` | noise mode (listening mode) | the only control identifier that reliably echoes on change |
| `0x0E` | connection info | re-emitted after a settings write, carries no setting payload |
| `0x1A` (top-level) | rename | distinct from control identifier `0x1A` (listening-mode cycle) carried inside opcode `0x09` |
| `0x1D` | device metadata | carries the accessory's own name; the basis for verifying a rename |
| `0x2e` | connected-device information | not decoded as a settings report |

control identifier `0x17` (press speed) is carried inside opcode `0x09`.
a separate, unrelated top-level opcode `0x17` carries sensor and
head-tracking traffic in LibrePods; it is not a settings list. the two must
not be conflated when reading a capture.

## handshake, subscription and the settings dump

the accessory dumps its settings as `0x09` packets exactly once per L2CAP
link, about 13 ms after the set-features acknowledgement. re-sending
request-notifications (five-byte or four-byte form), set-features (`0xd7` or
`0xff`), or the full handshake triple mid-link does not produce a second
dump. reopening the AAP L2CAP link does. a press-speed write followed by
`auris reconnect` shows the new value in the fresh dump every time.

the `0xff` set-features byte (`FeaturesVariant::Ff`, `AURISD_FEATURES=ff`)
produces a dump identical to the default `0xd7`. it remains an opt-in
variant and is not the default.

the identifiers reported at link open are `0x17`, `0x18`, `0x24`, `0x26`,
plus the unmodelled `0x1F` (chime volume), `0x1B`, `0x35` and `0x3E`.
microphone (`0x01`) and listening-mode cycle (`0x1A`) are never reported by
this model at link open, after a write, or with either features byte. this
establishes only missing readback for those two identifiers, not rejection
or unsupported hardware; a separately captured device trace against
different firmware would be needed to rule out unusual framing.

battery reports, in-ear sensor state, BLE layout, cryptographic identity
matching, radio limits and hardware gates are documented in
[battery observation and handoff safety](BATTERY_AND_HANDOFF.md).

## noise control and listening modes

listening-mode cycle uses control identifier `0x1A`: off `01`, anc `02`,
transparency `04`, adaptive `08`, and bitmask combinations of those values.
Apple documents selecting multiple stem-cycle listening modes; LibrePods
blocks removing a mode when fewer than two would remain, and auris keeps the
same two-mode write minimum. the apply button for this control is removed
from the UI because there is no confirming echo (see above); an incoming
cycle mask with only one valid mode is still retained on read, since command
validation and report decoding are separate rules.

noise mode (control identifier `0x0D`) is the only setting that echoes
immediately on change. conversational awareness state is sent once after
subscribe, not on every change.

off permission (`0x34`) and hold-duration timing semantics still need
device verification before auris relies on them; auris does not silently
change the separate off permission to make a cycle work.

references: [Apple stem controls](https://support.apple.com/en-au/108764),
[LibrePods cycle UI](https://github.com/librepods-org/librepods/blob/53679cc/android/app/src/main/java/me/kavishdevar/librepods/presentation/viewmodel/AirPodsViewModel.kt).

## settings writes, readback and the verification contract

control writes use `04 00 04 00 09 00 ID D1 D2 D3 D4`. unused bytes are
zero. accessory control reports use opcode `0x09` with the same identifier;
decoding the report requires the full payload, since the call-control
mapping cannot be resolved from the first value byte alone.

| setting | identifier | values / payload | what auris cannot yet confirm |
|---|---|---|---|
| microphone | `01` | auto `00`, right `01`, left `02` | never echoed; confirming the selected bud requires an active call session |
| press speed | `17` | default `00`, slower `01`, slowest `02` | echoed and verified by link reopen; gesture timing at each setting is unverified |
| hold duration | `18` | default `00`, shorter `01`, shortest `02` | echoed and verified by link reopen; the actual hold threshold and its persistence are unverified |
| listening-mode cycle | `1A` | off `01`, anc `02`, transparency `04`, adaptive `08`, combined bitmask | never echoed; unsupported-mode handling and stem behaviour are unverified |
| call controls | `24` | `00 02`: hang up once/mute twice; `00 03`: mute once/hang up twice | echoed both ways; does not remap the initial answer-call press, which is unverified against a live call |
| personalised volume | `26` | on `01`, off `02` | echoed both ways; the audible effect is unverified and must not be conflated with host call-audio volume ducking |

example capture of a press-speed write with no corresponding control
report:

```
04 00 04 00 09 00 17 01 00 00 00   (slower)
04 00 04 00 09 00 17 00 00 00 00   (default)
```

the report counter after the write above stayed unchanged. sending the
existing notification request after a write does not make press speed
report immediately; reopening only the AAP control link does, returning the
new value with a newer per-link counter. the same reopen sequence verifies
hold duration and personalised volume. call controls report both their
changed and restored values immediately, with no link reopen needed. no
Bluetooth connection or audio service is restarted for any of this.

### verify-by-reopen design

auris treats a settings write as a request plus a readback, never as a
single fire-and-forget datagram, and never sends an invented read packet or
a success fallback.

1. **write.** a successful write records `settings_requested[key]`, sets
   `settings_status[key] = "verifying"` and `settings_verify = "scheduled"`,
   and arms a 1500 ms debounce. another write restarts the debounce, so a
   burst of changes costs one reopen.
2. **reopen.** when the debounce fires, auris publishes
   `settings_verify = "reopening"` and `verify_reopen = true`, then reopens
   the AAP link the same way `reconnect` does: drop the socket and dial
   again. it never asks BlueZ to reconnect the device, and never dials a
   device that is not already locally connected; with no link to reopen,
   every verifying key becomes `"unverified"`.
3. **compare.** the readback window ends at the first battery packet after
   the opening sequence, the point where the set-features variant is
   pinned; if none arrives it ends 2000 ms after the subscribe. each
   verifying key is compared against the value reported on that link, using
   a per-link report counter rather than the lifetime `settings_report_seq`.

| outcome | status |
|---|---|
| equal | `"confirmed"` |
| different | `"mismatch"`, and `settings` holds the device's value |
| never reported | `"unreported"` (the expected outcome for microphone and cycle) |
| ten seconds from the write with no verdict | `"unverified"` |

a link loss that is not the verify reopen keeps `settings_requested` and
marks verifying keys `"unverified"`. every later link open runs the same
comparison for keys left `"unverified"` or `"unreported"`, so a natural
reconnection can still settle an old write.

`verify_reopen` exists because `connected` and `aap_link` briefly go false
during a readback reopen; the UI keeps showing the device as connected
while it is true.

the published contract is `settings_api = 3`. it adds the key `name` to
`settings_requested` and `settings_status`. a rename is verified from the
accessory's own `0x1D` metadata on the reopened link, and `device.name`
carries the name it reported.

`auris setting <key> <json>` subscribes after the write, waits up to ten
seconds for the status to leave `"verifying"`, then prints one line:

- `confirmed by AirPods`
- `AirPods kept <device value>` (exit 1)
- `applied; AirPods 4 (ANC) never reports this setting back`
- `sent but not verified: <reason>` (exit 1)

### diagnostics: correlating a setting write across processes

DMS and the daemon log deliberately omit device names and setting payload
values. correlating a setting change end to end means matching, in order:
the DMS log line `setting command started` (with key and local request
number), the daemon lines `sending AirPods setting` and
`setting datagram sent`, the daemon line `setting report received` (or a
malformed/unmodelled report diagnostic), and finally the DMS command
completion, matching report, or confirmation timeout. a datagram-send
message alone is not confirmation.

the UI exposes `UI revision` in its technical disclosure and logs
`auris: UI loaded revision`. a plugin reload response alone does not prove
a new root revision loaded; the diagnostic `tools/QmlReloadTest.cpp`
confirms that a new file-URL query loads modified source in the same plain
Qt engine, while an existing instance retains its old revision, but it does
not prove the live DMS/Quickshell reload path works end to end.

## device metadata and rename

rename uses a different opcode from the settings-write path:
`04 00 04 00 1A 00 01 LL 00 <UTF-8 name>`. it shares the number `1A` with
the listening-mode cycle control identifier carried inside opcode `0x09`,
but the two are unrelated. `LL` is the UTF-8 byte count of the name.

the name must be nonblank, at most 255 bytes, and free of control
characters; auris does not trim it and does not execute it.

a successful send alone does not update the confirmed device name.
metadata is unsolicited and may only refresh on a later connection; a
BlueZ alias change is not proof of an accessory rename. rename is only
considered confirmed once the accessory's own `0x1D` metadata reports the
new name on a reopened link.

more generally: new fields stay unknown until a valid device report
arrives. a recognised model plus a successfully written packet does not
establish firmware support for that field. unknown values must not be
coerced to a default, and must not be mistaken for an explicit off value.

## multi-host ownership and smart routing

### macOS banner text for a take-over by this host

the banner macOS shows when another host takes the AirPods is rendered by
`BluetoothUIService`. its strings live on the sealed system volume at
`/System/Library/CoreServices/BluetoothUIService.app/Contents/Resources/Localizable.loctable`
on an arm64 Mac with SIP enabled.

`audioaccessoryd` maps the `btName` carried in smart-routing media info to
a model class, and builds the key `MOVED_TO_<CLASS>`.

keys with an English string: `MOVED_TO_IPHONE`, `MOVED_TO_IPAD`,
`MOVED_TO_MAC`, `MOVED_TO_WATCH`, `MOVED_TO_APPLETV`, `MOVED_TO_IPOD`.

an unknown name falls back to the iPhone class. `HomePod` is a known class
without a string, so the raw key `MOVED_TO_HOMEPOD` is displayed as-is. the
Mac learns the name only when it recreates its entry for this host, which
happens on link establishment.

### primary bud role switch drops a non-Apple host

when the bud acting as primary switches, the `0x0C` address report changes
from the left bud's address to the right bud's (or vice versa) about 30 s
after link-up, coinciding with a bud being taken out. about 3 s later the
AirPods reset the link to this host with `Reason.Remote`; in another
observed case the same switch instead ended as a supervision timeout.

Apple hosts survive the switch; the address exposed to the host stays the
same, so the switch is likely meant to be transparent there. the
disconnection observed here may be specific to the BCM20702A0 dongle's 2012
firmware rather than to AAP itself. LibrePods has no handover mechanism
either and reconnects the same way. both outcomes are covered by auris's
existing rejoin rules, and the link is back in about 6 s.

## later-feature cautions

- `31` (in-case/charging tones) is not a locating-sound request. Find My
  case access raises separate reachability and authentication questions.
- `35` (sleep detection) is described for recent firmware with no
  published AirPods 4 minimum; no host sleep classifier is justified by
  this identifier alone.
- `16` (hold-action configuration) is not a complete incoming stem-event
  protocol; actual event delivery needs verification before building
  custom Linux actions on top of it.
- `39` (stem configuration) does not by itself document every gesture
  event.
- connection-ownership commands are not proof of ordinary Bluetooth
  multipoint.
- microphone-side preference does not implement the high-quality AACP
  microphone transport, and does not solve audio-profile switching.

for media automation, use
[MPRIS player semantics](https://specifications.freedesktop.org/mpris/latest/Player_Interface.html)
and explicit playback ownership. for auto-connect, use
[BlueZ Device1 semantics](https://bluez.readthedocs.io/en/latest/device-api/)
with opt-in, bounded retry and deliberate-disconnect handling. see the
[roadmap](FEATURE_ROADMAP.md) for acceptance gates and delivery order.

## primary references

- [control catalog](https://github.com/librepods-org/librepods/blob/53679cc/docs/control_commands.md)
- [AACP manager](https://github.com/librepods-org/librepods/blob/53679cc/android/app/src/main/java/me/kavishdevar/librepods/bluetooth/AACPManager.kt)
- [control repository](https://github.com/librepods-org/librepods/blob/53679cc/android/app/src/main/java/me/kavishdevar/librepods/data/ControlCommandRepository.kt)
- [Linux command framing](https://github.com/librepods-org/librepods/blob/53679cc/linux/BasicControlCommand.hpp)
