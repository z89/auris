# Mac agent and motion gesture feasibility

this document records what a helper running in a Mac's own user session can
and cannot reach on behalf of auris, and what motion gestures are feasible
from the AirPods 4 sensor stream. it covers hard platform limits, a proposed
helper architecture, the AirPods features such a helper could and could not
deliver, and gesture recognition design.

## why Linux renames do not propagate to Apple hosts

Linux renames the AirPods over AAP opcode 0x1A and the accessory accepts it.
every Apple host still shows the old name.

Apple hosts display the name stored in their own pairing record. iCloud
Keychain syncs that record between Apple devices. an Apple host never
re-reads the accessory's advertised name after initial pairing.

this means a Linux-side rename cannot make Apple hosts show the new name.
any fix has to run on the Apple side of the sync boundary.

## hard platform limits

| item | finding | verdict |
|---|---|---|
| Secure Enclave keys | class keys and any token-bound key never leave the chip; no software path reads them | physically impossible |
| iCloud Keychain synchronizable items | not enumerated by `security dump-keychain` at all; Apple's own access groups are entitlement-gated, so third-party code cannot read them even with a user prompt | hard wall |
| Keychain secret values | `security dump-keychain -d` raises a GUI password prompt; no headless path exists | needs a person at the keyboard, and yields nothing a helper needs |
| Keychain attribute metadata | `security dump-keychain` without `-d` works over ssh; entries under service `BluetoothGlobal` are the Mac's own Continuity identity roots, none labelled for the AirPods | reachable, but carries no secrets |
| Bluetooth pairing plist | `/Library/Preferences/com.apple.bluetooth.plist` holds only settings on macOS 26; the legacy device cache plist does not exist on this version. names and link keys live in bluetoothd's iCloud-synced store | not editable |
| Find My location store | `~/Library/Group Containers/group.com.apple.icloud.searchpartyuseragent/Library/Storage/` holds `OwnedBeacons/*.record`, naming and product records, `BeaconEstimatedLocation/<uuid>/`, `CachedUnifiedBeacons.data`, and `CloudStorage.db`. files are readable over ssh but every record is ciphertext; the decryption keys are iCloud Keychain items | unreadable headlessly |
| independent Find My fetch (macless-haystack style) | works only for self-generated beacon keys; an Apple-paired accessory uses a per-accessory private beacon key that is the same locked item above | not applicable to a paired accessory |
| direct Apple ID sign-in from Linux | a login flow is reproducible outside Apple hardware, but the trust circle requires Secure Enclave attestation, so a non-Apple host never receives keychain-synced records even when signed in | possible, but useless for this purpose |

conclusion: no amount of Keychain duplication or Secure Enclave replication
reaches these items from Linux. the durable route is to let a Mac already
inside the trust circle perform the operation and report the result back.

## what a helper in the user's Mac session can reach

### the ssh session boundary

a process started by ssh is denied Bluetooth by the privacy system
regardless of user. the responsible binary is `sshd`; the TCC log line reads
`kTCCServiceBluetoothAlways denied, Policy disallows prompt`. every
Bluetooth API called from a bare ssh shell returns zero paired devices.

a LaunchAgent bootstrapped into the console user's GUI domain does not carry
this restriction:

```
launchctl bootstrap gui/501 <plist>
```

this needs no root. inside that domain, IOBluetooth sees the real paired
devices. `launchctl asuser` was evaluated as an alternative; it requires
root, which is not available without a password on this machine.

this means any operation that needs Bluetooth or Keychain access must run
inside a GUI-session LaunchAgent, never as a bare ssh command and never as a
LaunchDaemon (root context, no user Keychain, no iCloud session).

### path comparison for a Mac-side rename

| method | changes the shown name | writes accessory and syncs iCloud | survives macOS updates | prerequisites | result |
|---|---|---|---|---|---|
| `blueutil` (Homebrew) | no | no | n/a | brew | no rename verb exists; ruled out |
| System Settings UI automation via `osascript` and System Events | yes, Apple's own path | yes | low; System Settings is SwiftUI and is re-laid-out across releases | one-time Accessibility grant, unlocked GUI session | reachable (`UI elements enabled` reports true); drives the visible screen |
| IOBluetooth `setName:` from a Swift helper in the GUI domain | reads succeed; the write does not persist without a Bluetooth grant | route exists (`setName:` and `setDisplayName:` respond) | medium | GUI domain, Bluetooth permission for the helper, a live run loop | calls hang waiting for the XPC reply without the grant and briefly wedge `pairedDevices()`; bluetoothd recovers on its own each time |
| edit the pairing plist and signal bluetoothd | no | no | n/a | root | dead on macOS 26: no device cache exists in any plist |
| AAP 0x1A from the Mac's own L2CAP socket | no | accessory only | n/a | raw L2CAP | same invisibility to other Apple hosts as the Linux rename |

nothing renames headlessly today. the programmatic path is `setName:` from a
signed helper holding a Bluetooth grant; it is unproven with the grant in
place. the guaranteed path is System Settings automation, which is fragile
and takes over the visible screen, so it can only be a supervised fallback,
not a background operation.

## permission model

nothing in this design needs `sudo`. passwordless root does not exist on the
Mac and is not required by any operation below.

### one-time at the keyboard, then headless

- installing and updating the helper as a per-user LaunchAgent under
  `~/Library/LaunchAgents`, which is user-writable. no prompt.
- granting Bluetooth to the helper under Privacy and Security. this is the
  only mandatory keyboard step. the grant persists only if the helper is
  signed with a stable identity; an ad-hoc build can lose the grant on every
  rebuild, so signing is part of the design.
- after the grant: helper status, the Mac's view of the AirPods (name,
  connected state, battery), and the `setName:` rename once proven. a
  successful rename syncs to iCloud automatically.

### conditions on every operation, regardless of permissions

- the Mac must be awake and on the network; requests queue otherwise.
- a rename needs the AirPods connected to the Mac at that moment. auris can
  release the link briefly and reclaim it, which keeps this automatic
  rather than manual.

### requires a person every time, or is unavailable

- System Settings automation: no prompt per run after the Accessibility
  grant, but it drives the visible screen, fails on a locked screen, and
  breaks when the layout changes across releases.
- Find My location: requires a Keychain unlock per read, and Apple's access
  group is unreadable even after unlock. treat as unavailable; the Find My
  app is the only route.
- any Keychain secret export: a password prompt per run, and the result is
  not usable regardless.
- spatial-audio personalization and hearing features: these require an
  iPhone and are out of a Mac helper's reach entirely.

## proposed helper architecture

### process model

two components run on the Mac:

1. `mac-agent-helper`: a signed Swift binary in an app bundle, launched by
   `~/Library/LaunchAgents/com.auris.mac-agent.plist` with `RunAtLoad` and
   `KeepAlive`. it runs inside the Aqua session, re-spawns after sleep or
   logout, and listens on a unix socket under the user's home directory.
2. `mac-agent-relay`: a stateless forwarder invoked over ssh. it shuttles
   one JSON request from stdin to the socket and one response back to
   stdout.

ssh remains a dumb pipe; every operation that needs the GUI session lives in
the helper, not in the relay.

### transport and wire protocol

newline-delimited JSON, one request to one response. every request carries
`v`, `id`, `op`, an optional `args` object, and `timeout_ms`.

```
-> {"v":1,"id":"a1b2","op":"rename","args":{"name":"my AirPods"},"timeout_ms":15000}
<- {"v":1,"id":"a1b2","ok":true,"op":"rename","status":"confirmed",
    "result":{"name":"my AirPods","readback":"my AirPods","synced":true}}

-> {"v":1,"id":"e5","op":"capabilities"}
<- {"v":1,"id":"e5","ok":true,"op":"capabilities",
    "result":{"os_build":"<macos-build>","ops":{"rename":"ok","get_location":"unavailable"}}}

<- {"v":1,"id":"a1b2","ok":false,"op":"rename","status":"unverified",
    "error":{"code":"airpods_not_on_mac","message":"accessory not connected to this Mac"}}
```

### error vocabulary

`unreachable`, `mac_asleep`, `icloud_not_signed_in`, `airpods_not_on_mac`,
`tcc_denied`, `automation_path_broken`, `timeout`, `verify_failed`.

### versioning and verification

- the `v` field versions the wire protocol independently of the helper
  binary version.
- the helper caches a small result set keyed by `id`, so a retried request
  returns the cached result rather than repeating a side effect.
- every write is followed by a read-back that reports `confirmed` or
  `mismatch`. that vocabulary matches the existing settings verification
  used elsewhere in auris, so the panel needs no new states for it.
- the capabilities probe reports the current macOS build. operations are
  marked degraded when the build changes, since automation paths in
  particular are known to break across releases.

### daemon-side integration

- a `mac_agent` module holds host, relay command, and timeouts from a
  `[mac_agent]` config section.
- an async call spawns ssh with the request on stdin, under the same
  deadline handling as the control socket.
- a periodic capabilities probe is cached as agent health.
- new control requests sync a rename to Apple hosts and query agent status.
- a `mac_agent` snapshot field, and namespaced entries such as `mac.name` in
  the existing `settings_requested` and `settings_status` maps, let the
  panel's pending-state machinery apply unchanged.

### resilience rules

- an unreachable Mac queues the write and retries on the next reachability
  tick.
- an absent accessory defers the operation until it reappears on the Mac.
- no wake-on-LAN without explicit opt-in.

### scope

a first slice covers the helper bundle, transport, capabilities, and the
rename operation with verification, gated on a supervised test of the
Bluetooth-grant path. firmware-update triggering (report-only), an
automatic-switching toggle, and audio sharing are deferred. Find My and
spatial personalization remain blocked for the platform reasons in
sections 2 and 6.

### related prior art

LibrePods, OpenPods, AirStatus, and MagicPods speak AAP to the accessory
directly; none of them drives a companion Mac. using the user's own Mac, in
session, as an authenticated execution surface for operations Linux cannot
structurally reach is a different approach from all four.

## features a Mac helper cannot reach regardless

- hearing test, hearing aid mode, and hearing protection are AirPods Pro 2
  features. AirPods 4 lack the hardware for them on any host.
- personalized spatial audio is a host-side rendering profile produced by an
  iPhone ear scan. Linux has no Apple spatializer, so the profile has no use
  there; a scan improves only the Mac's own playback.
- an iPhone cannot act as a proxy. there is no ssh, no background helper
  process, and no Shortcuts action for AirPods settings on iOS without a
  jailbreak, which is not a dependency this project carries.

## spatial audio on Linux

spatial audio is rendered by the host. the AirPods contribute only the
motion stream from the primary bud's inertial sensor.

on Apple hosts, AirPods 4 receive spatialized stereo, Dolby Atmos playback,
head tracking anchored to the screen, a personalized profile, and positioned
voices on calls. LibrePods marks head-tracked spatial audio as unimplemented
on both Android and Linux; no third party has shipped it.

a Linux implementation would read the motion stream over AAP (opcode 0x17
traffic; see PROTOCOL_EVIDENCE.md), run a binaural convolver in PipeWire
with an open HRTF set, and rotate the field using the head data. that yields
head-tracked spatialized stereo for any Linux application, but there is no
Atmos content path and no personalized profile; the closest substitute is
picking the best-fitting public HRTF through a one-time listening test.

the latency budget is the hard constraint: head data must reach the
renderer within a few tens of milliseconds or the field visibly lags the
head. no machine learning is involved anywhere in this pipeline.

## motion gestures

### where recognition happens

the AirPods themselves do not recognize gestures; the host does. Apple's
nod-to-answer is a classifier running on the iPhone. LibrePods implements
its own nod and shake detector on Android from the same motion stream.
AirPods 4 ANC have no touch surface; the stem force sensor is the only
physical input, so there is no tap area and no swipe gesture available.

### what the stream gives

orientation and acceleration at tens of samples per second, from one bud.
pitch and roll are gravity-referenced and stable. yaw has no compass
reference and drifts, so turns are usable only as relative motion over a
second or two, not as an absolute heading. enabling the stream costs
battery, so it should run only while a gesture feature is active.

### gesture tiers

- reliable: nod, shake, tilt left or right, look up and hold, look down and
  hold, double nod, double shake.
- plausible: a slow head swipe for track change, tilt-and-hold as a volume
  ramp, turn direction to choose between two targets, and chords such as a
  stem press followed by a nod.
- record-your-own: the user performs a motion five to ten times, and live
  motion is matched against the stored traces.
- not realistic: subtle motions, absolute pointing, and anything that must
  survive walking, chewing, or talking without a confirmation step.
- experimental: the original AirPods detected a double-tap purely from the
  accelerometer, so a firm double-tap on the AirPods 4 housing could be
  recognized as an impulse pair in the same stream. a single tap is
  indistinguishable from a bud being adjusted by hand.

### stem presses as the more direct path

the firmware sends single, double, and triple presses as standard media
commands, and Linux can already intercept and remap them. press-and-hold is
already configurable over AAP, making the stem the more direct gesture
surface compared to motion detection.

### classifier choice

- rule-based detectors cover the fixed gesture set: angular velocity over a
  threshold on one axis, a sign reversal inside a window, and a repetition
  count, tunable per user with a few sliders. no training data is needed
  and the logic stays explainable.
- template matching with dynamic time warping covers recorded gestures,
  using nearest-neighbour comparison against the user's own samples. this
  costs microseconds and needs no training step; a neural network would
  need hundreds of samples per gesture per person and would not outperform
  it at this scale.
- a small learned gate for false-positive rejection is the one place a
  learned classifier earns its place, because everyday motion (a second
  monitor, chewing, walking) is too varied to enumerate by rule. it needs a
  labelled corpus of gesture-versus-everyday traces before it can be built.

### delivery order

1. rule-based detectors with per-user thresholds.
2. dynamic time warping for custom, recorded gestures.
3. the learned false-positive gate once labelled traces exist.

every gesture action stays opt-in with an arming rule, for example only
while media plays or only after a stem press. none of this requires a GPU,
a cloud service, or a training pipeline.

the roadmap entries for head gestures and head-tracked spatial audio in
[feature roadmap](FEATURE_ROADMAP.md) are the implementation entry
points for this section.
