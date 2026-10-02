# research

feasibility research for features auris does not ship. implemented
behaviour is documented in [BATTERY_AND_HANDOFF.md](BATTERY_AND_HANDOFF.md)
and [PROTOCOL_EVIDENCE.md](PROTOCOL_EVIDENCE.md).

## per-bud control

target is AirPods 4 ANC (model `0x201B`, H2 chip, optical in-ear sensing).
Pro-only features and older reverse engineering do not automatically
transfer ([Apple specifications][apple-specs]).

| behaviour | assessment | next step |
|---|---|---|
| media in the left or right bud only | feasible as host PCM processing, physical mapping unverified | reversible virtual sink, test every bud-presence transition |
| both stereo channels in one bud | feasible downmix, loses separation | bounded-gain mono mix in the chosen channel |
| ANC with one bud worn | documented by Apple ([listening settings][apple-listening]), known setting `0x1B` | model-gated permission with honest readback and hardware tests |
| left ANC with right transparency | no public command | research only |
| power off one out-of-case bud | no public command | research only |
| one bud in its closed case | Apple's physical shutdown and charging | not a remote feature |

the conclusion is that host-side media routing to one ear is the credible
addition, and that it is a linux audio feature, not an AirPods power
command. one-bud ANC is a real setting with its own permission in the
[LibrePods catalog][lp-commands] next to a global listening mode, but it
does not allow different modes per side. absence from public code does not
prove no firmware path exists, but it is not enough to advertise or build
one.

### side addressing

| candidate | finding | verdict |
|---|---|---|
| listening mode `0x0D`, one-bud ANC `0x1B`, ear detection `0x0A` | no left or right destination. stem-hold config carries ordered right and left values, so some settings are side-specific, but there is no universal side selector ([commands][lp-commands], [linux packets][lp-packets]) | no per-side mode mixing |
| MagicPodsCore | one global mode ([setter][mp-anc], [watcher][mp-anc-watch]). listing the [model id][mp-model] does not prove every command works on it. its [one-AirPod capability][mp-one-capability] gets no notification after the change, so a local value is not confirmed readback | corroborates a single global mode |
| ANC, reconnect through the other bud, transparency | the API addresses the logical accessory, so the second global write replaces the first (inference from packet structure, not an exhaustive firmware search) | ruled out as a product feature |
| battery codes `0x02` right, `0x04` left, `0x08` case | classify battery report entries ([types][mp-battery], [parser][mp-battery-watch]), not command addresses | ruled out |
| primary or active role | often follows removal order, not a fixed side ([H2 notes][mp-h2]). label physical sides separately and repeat with the other bud removed first | ruled out |
| transparency customisation | per-side values under a global enabled state ([LibrePods][lp-transparency]), documented for AirPods Pro only ([Apple][apple-pro-transparency]), nothing shows AirPods 4 ANC support or simultaneous per-side modes | inconclusive, best lead for differential capture |

### ear detection, case and power

power, connection, routing and sensor state are separate. Apple's ear
detection controls playback and routing, audio continues with it off, and
the microphone preference picks a side without turning the other bud off
([AirPods settings][apple-settings]). the LibrePods
[ear-detection override][lp-ear] used in its [linux integration][lp-main]
changes application behaviour only. the conclusion is that overriding ear
state is ruled out as power control and is valid only as a labelled
host-side presentation override.

AirPods shut down and charge in a closed case ([charging guide][apple-charging]).
that implies no wireless shutdown, and charging depends on case power, so
placement is not confirmed charging. no published fixed "60 seconds after
closing" telemetry timeout exists in these sources. a timeout is a
per-model, per-firmware measurement, and a bud vanishing from reports does
not date its power-down. the conclusion is that there is no evidence of a
remote per-bud shutdown command.

a real power feature needs independent evidence of a reduced power state,
the other bud still working, a deterministic wake, and repetition after
primary-role changes. stale battery data, a closed socket or a stopped
stream proves none of these. without them an experimental UI says "media
muted", not "AirPod off".

### media routing on linux

host audio controls are not device commands. bluez
[MediaTransport volume][bluez-transport] is a scalar `0-127`, and
pulseaudio [per-channel volume][pa-volume] set through [`pactl`][pactl]
implies no bud-targeted AACP command. setting sink balance is a diagnostic
at most. it shares state with other volume tools, interacts with hardware
volume, and [balance need not be reversible][pa-ui], so resetting it may
not restore the original gains.

the proposed path is an opt-in, session-scoped, auris-owned virtual stereo
sink fed by selected streams and output to the exact AirPods A2DP node,
built from pipewire [loopback][pw-loopback] (remap, target selection) and
[filter-chain][pw-filter] (gain mixing). it exposes mapped `FL` and `FR`,
accepts only a verified stereo A2DP target at first, and must set and
verify the no-fallback and lifecycle properties from the wireplumber
[linking policy][wp-linking] so a disconnect never falls back to speakers.

| mode | out left | out right | trade-off |
|---|---|---|---|
| stereo | `L` | `R` | none |
| left, original | `L` | `0` | loses right-only content |
| right, original | `0` | `R` | loses left-only content |
| left, mixed | `0.5L + 0.5R` | `0` | no stereo image |
| right, mixed | `0` | `0.5L + 0.5R` | no stereo image |

these are DSP choices, not protocol findings. the average never exceeds the
inputs' peak, while `0.707L + 0.707R` without headroom clips correlated
full-scale material by about 3dB. opposite-phase content cancels in any
mono sum. mode changes ramp gains over a short tested interval.

a zeroed channel leaves the radio, microphones, optical sensor, ANC and
transparency running. device chimes, local audio and streams routed
straight to the physical sink bypass the filter. AirPods may change mono
and stereo handling when a bud is removed or cased, so a correct buffer
proves only the filter output, not the rendering transducer, and Apple's
[one-sided audio fix][apple-balance] does not guarantee linux channel
identity. unknown layouts disable the feature with an explanation.

the graph is runtime only, never a permanent config edit.

- tag only its own nodes and links, with a session generation id.
- resolve targets by stable device identity, profile and channel map
  ([pipewire properties][pw-properties]), never persisted object ids.
- route selected streams only. taking the default output is a separate
  choice.
- restore a route only if it still matches what auris installed, so user
  changes win. never delete other clients' nodes or force an old default.
- on target loss leave owned output unlinked and silent.
- when a microphone app leaves stereo A2DP ([bluetooth][wp-bluetooth],
  [profile settings][wp-settings]), suspend side routing, revalidate on
  return, never force a profile or interrupt a call.

the verification gate starts with unit tests for every matrix row,
non-target silence, maximum correlated input, opposite phase, ramps and
invalid channel maps, then a disposable pipewire environment or mock graph
proving target selection, ownership and cleanup before any live session.

| dimension | cases |
|---|---|
| buds | both worn, both out, left alone, right alone, each side cased, opposite removal order |
| input | left-only tone, right-only tone, identical channels, opposite phase, stereo media |
| profiles | each stereo A2DP codec, HFP and back, unknown channel map |
| lifecycle | connect, disconnect, target recreation, role change, helper crash, repeated toggle |
| user actions | volume or default change, manual reroute, new streams, an app pinned to a bypass route |

acceptance measures filter and physical bud output with low-level test
signals and needs no audible non-target media, no speaker fallback, no
stolen routing changes and no stranded streams, all at once. the UI claims
only validated profiles and layouts.

### further reverse engineering

[MagicPairing][magicpairing] and [ToothPicker][toothpicker] predate H2 and
serve as method references only, and research on vulnerabilities of that era is no basis for a daily-use feature. the open leads (per-side transparency
customisation, mixed modes, per-bud power) need these steps in order.

1. **baseline.** model, firmware, battery, stock settings, profile, codec,
   first-removed bud, and a known recovery procedure.
2. **stock transitions.** each bud alone and both, case open and closed,
   ear detection on and off, both removal orders. audible behaviour is
   recorded apart from telemetry.
3. **passive capture.** host HCI and AACP traffic during native controls.
   inter-bud traffic may be invisible, so a missing packet proves nothing.
4. **known baselines.** global listening mode and one-bud ANC traces, with
   primary and secondary kept apart from physical sides.
5. **a real lead.** a byte that resembles a battery side code does not
   justify a write.
6. **offline safeguards.** parser fixtures, exact length and value checks,
   model and firmware gates, finite timeouts, explicit restoration.
7. **measured effect.** acoustic evidence at each bud for mixed modes,
   independent power evidence plus reliable wake for power-off, repeated
   across connection and removal order with the other bud unaffected.

out of scope are opcode brute-forcing, replaying unknown init or
calibration writes, spoofing charging contacts or magnets, and treating a
crash as control. discovery without a recovery path is not a feature, and without a side selector this work stays parked rather than mutating a global command.

### development order

1. **one-bud ANC permission.** validate `0x1B` on target firmware, separate
   requested from confirmed, place it in one-time setup, keep ANC on the
   existing quick control, no left and right ANC switches.
2. **per-ear media routing.** opt-in until the channel and lifecycle tests
   pass, then a stereo, left, right quick control.
3. **passive differential AACP research.** only with a defined question
   and repeatable baseline, independent of item 2.
4. **per-bud power and mixed mode.** not scheduled until a mechanism passes
   the gate above.

## Mac helper

linux renames the AirPods with top-level AAP opcode `0x1A` and the
accessory accepts it ([PROTOCOL_EVIDENCE.md](PROTOCOL_EVIDENCE.md)), but
Apple hosts keep the old name. they show the name in their own pairing
record, synced by iCloud Keychain, and never re-read the advertised name
after pairing. a fix has to run on the Apple side.

### platform limits

| item | finding | verdict |
|---|---|---|
| Secure Enclave keys | class and token-bound keys never leave the chip | impossible |
| iCloud Keychain synchronizable items | absent from `security dump-keychain`, Apple's access groups are entitlement-gated even with a prompt | hard wall |
| Keychain secret values | `security dump-keychain -d` needs a GUI password prompt | needs a person, yields nothing useful |
| Keychain metadata | `security dump-keychain` without `-d` works over ssh. `BluetoothGlobal` entries are the Mac's Continuity identity roots, none for the AirPods | reachable, no secrets |
| pairing plist | `/Library/Preferences/com.apple.bluetooth.plist` is settings only on macOS 26, no device cache plist, names and link keys live in the iCloud-synced store of bluetoothd | not editable |
| Find My store | `~/Library/Group Containers/group.com.apple.icloud.searchpartyuseragent/Library/Storage/` (`OwnedBeacons/*.record`, naming and product records, `BeaconEstimatedLocation/<uuid>/`, `CachedUnifiedBeacons.data`, `CloudStorage.db`) is readable over ssh but ciphertext keyed by iCloud Keychain | unreadable headlessly |
| macless-haystack style fetch | needs self-generated beacon keys, a paired accessory uses a locked per-accessory key | not applicable |
| Apple ID sign-in from linux | reproducible, but the trust circle needs Secure Enclave attestation, so no keychain-synced records arrive | useless here |

the conclusion is that nothing reaches these items from linux. a Mac
already in the trust circle has to do the operation and report back.

### session boundary

TCC denies bluetooth to ssh-started processes for any user, logging
`kTCCServiceBluetoothAlways denied, Policy disallows prompt` against
`sshd`, and bluetooth APIs return zero paired devices. a LaunchAgent in the
console user's GUI domain needs no root and sees the real devices through
IOBluetooth.

```
launchctl bootstrap gui/501 <plist>
```

`launchctl asuser` needs root, unavailable without a password on this Mac.
bluetooth and Keychain work therefore runs in a GUI-session LaunchAgent,
never a bare ssh command or a LaunchDaemon (no user Keychain or iCloud
session).

### rename paths

| method | shown name changes | syncs | survives updates | needs | result |
|---|---|---|---|---|---|
| `blueutil` (homebrew) | no | no | n/a | brew | no rename verb |
| System Settings via `osascript` and System Events | yes | yes | low, SwiftUI layout changes each release | one-time Accessibility grant, unlocked session | reachable (`UI elements enabled` true), drives the screen |
| IOBluetooth `setName:` in a GUI-domain Swift helper | reads work, writes do not persist without a bluetooth grant | `setName:` and `setDisplayName:` respond | medium | bluetooth grant, live run loop | without the grant calls hang on the XPC reply and briefly wedge `pairedDevices()` until bluetoothd recovers |
| edit pairing plist, signal bluetoothd | no | no | n/a | root | dead on macOS 26 |
| AAP `0x1A` over the Mac's L2CAP | no | accessory only | n/a | raw L2CAP | invisible to other Apple hosts |

nothing renames headlessly today. `setName:` from a signed helper with a
bluetooth grant is the programmatic path, unproven with the grant.
System Settings automation works but takes the screen, so it is a
supervised fallback only.

### permission tiers

nothing needs `sudo`, and the Mac has no passwordless root.

| tier | items |
|---|---|
| once at the keyboard | install the helper as a per-user LaunchAgent in `~/Library/LaunchAgents` (no prompt). grant bluetooth in Privacy and Security, the only mandatory step, kept only with a stable signing identity since ad-hoc builds can lose it on rebuild. then status, the Mac's AirPods view (name, connection, battery) and a proven rename run headless, and a rename syncs to iCloud |
| every operation | Mac awake and online or requests queue. a rename needs the AirPods on the Mac, which auris arranges by releasing and reclaiming the link |
| a person each time, or unavailable | System Settings automation (screen, fails when locked, breaks on layout change). Find My (unlock per read, access group unreadable anyway, only the Find My app works). Keychain secret export (prompt per run, unusable). spatial personalization and hearing features (iPhone only) |

### process model and protocol

1. `mac-agent-helper`, a signed Swift app bundle binary launched by
   `~/Library/LaunchAgents/com.auris.mac-agent.plist` with `RunAtLoad` and
   `KeepAlive`, runs in the Aqua session, respawns after sleep or logout
   and listens on a unix socket in the user's home.
2. `mac-agent-relay`, run over ssh, statelessly forwards one JSON request
   from stdin to the socket and the response to stdout. ssh stays a dumb
   pipe.

the wire format is newline-delimited JSON, one request per response, with
`v`, `id`, `op`, optional `args` and `timeout_ms`.

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

- error codes are `unreachable`, `mac_asleep`, `icloud_not_signed_in`,
  `airpods_not_on_mac`, `tcc_denied`, `automation_path_broken`, `timeout`,
  `verify_failed`.
- `v` versions the protocol apart from the binary.
- results are cached by `id`, so a retry repeats no side effect.
- each write is read back as `confirmed` or `mismatch`, the existing auris
  settings vocabulary.
- capabilities report the macOS build, and a build change marks operations
  degraded.

### daemon side and scope

a `mac_agent` module reads host, relay command and timeouts from
`[mac_agent]`, spawns ssh with the request on stdin under the control
socket's deadline handling, and caches a periodic capabilities probe as
agent health. new control requests sync a rename and query agent status. a
`mac_agent` snapshot field and entries such as `mac.name` in
`settings_requested` and `settings_status` reuse the panel's pending
states. an unreachable Mac queues and retries on the next reachability
tick, an absent accessory defers until it reappears, and wake-on-LAN needs
explicit opt-in.

the first slice is the bundle, transport, capabilities and verified rename,
gated on a supervised test of the bluetooth-grant path. firmware-update
triggering (report only), an auto-switching toggle and audio sharing are
deferred. Find My and spatial personalization stay blocked. hearing test,
hearing aid and hearing protection are AirPods Pro 2 features that AirPods
4 lack on any host. personalized spatial audio is an iPhone ear-scan
profile, useless on linux (no Apple spatializer), that only helps the Mac's own playback. an iPhone cannot proxy (no ssh,
background helper or AirPods Shortcuts action without a jailbreak).
LibrePods, OpenPods, AirStatus and MagicPods all talk AAP directly, and none
uses a companion Mac.

## spatial audio

the host renders it, and the AirPods supply only the primary bud's motion
stream. Apple hosts give AirPods 4 spatial stereo, Dolby Atmos,
screen-anchored head tracking, a personalized profile and positioned call
voices. LibrePods marks head-tracked spatial audio unimplemented on Android
and linux, and no third party has shipped it.

a linux version reads the stream (top-level opcode `0x17`, see
[PROTOCOL_EVIDENCE.md](PROTOCOL_EVIDENCE.md)), runs a pipewire binaural
convolver with an open HRTF set and rotates the field from head data. that
is head-tracked stereo for any app, without Atmos or a personal profile (a
one-time listening test can pick the closest public HRTF). head data must
reach the renderer within a few tens of milliseconds or the field lags. no
machine learning is involved.

## motion gestures

the host recognizes gestures. Apple's nod-to-answer runs on the iPhone and
LibrePods detects nod and shake on Android from the same stream. AirPods 4
ANC have no touch surface, only the stem force sensor, so no tap area or
swipe. the stream is orientation and acceleration from one bud at tens of
samples per second. pitch and roll are gravity-referenced and stable, yaw
drifts with no compass, so turns are relative over a second or two. it
costs battery and runs only while a gesture feature is active.

| tier | gestures |
|---|---|
| reliable | nod, shake, tilt left or right, look up and hold, look down and hold, double nod, double shake |
| plausible | slow head swipe for track change, tilt-and-hold volume ramp, turn direction to pick one of two targets, stem press then nod |
| record your own | user repeats a motion five to ten times, live motion is matched to the traces |
| not realistic | subtle motion, absolute pointing, anything surviving walking, chewing or talking without confirmation |
| experimental | firm housing double-tap as an impulse pair (the original AirPods detected double-tap by accelerometer). a single tap looks like adjusting the bud |

stem presses are more direct. single, double and triple presses arrive as
standard media commands linux can remap, and press-and-hold is
configurable over AAP.

classifier choice, in delivery order.

1. **rule-based detectors** for the fixed set. one-axis angular velocity
   threshold, sign reversal in a window, repetition count, per-user
   sliders. no training data, explainable.
2. **dynamic time warping** for recorded gestures, nearest neighbour
   against the user's samples in microseconds with no training. a neural
   network would need hundreds of samples per gesture per person and would
   not beat it.
3. **a small learned false-positive gate**, the only place learning pays,
   since everyday motion (a second monitor, chewing, walking) cannot be
   enumerated by rule. it needs a labelled corpus of gesture and everyday
   traces first.

every action is opt-in with an arming rule, such as only while media plays
or after a stem press. no GPU, cloud or training pipeline is needed. the
head gesture and spatial audio entries in
[FEATURES.md](FEATURES.md#feature-matrix) are the entry points.

## sources

LibrePods links are pinned to `53679cc90222e94ade84e66542d97ace2540e626`
and MagicPodsCore links to `11422817ed7080ac6ec8f02c84dfefa8d1f773dd`.
linux audio docs describe API capability, not hardware testing.

- Apple. [specifications][apple-specs], [listening settings][apple-listening], [AirPods settings][apple-settings], [charging][apple-charging], [Pro listening modes][apple-pro-transparency], [one-sided audio][apple-balance].
- LibrePods. [command catalog][lp-commands], [linux packets][lp-packets], [ear detection][lp-ear], [linux main][lp-main], [transparency][lp-transparency].
- MagicPodsCore. [model ids][mp-model], [ANC setter][mp-anc], [ANC watcher][mp-anc-watch], [one-bud ANC][mp-one-capability], [battery types][mp-battery], [battery parser][mp-battery-watch]. MagicPods [H2 notes][mp-h2].
- linux audio. bluez [MediaTransport][bluez-transport], pulseaudio [volume API][pa-volume] and [volume UI guidance][pa-ui], [`pactl`][pactl], pipewire [loopback][pw-loopback], [filter-chain][pw-filter] and [properties][pw-properties], wireplumber [linking][wp-linking], [bluetooth][wp-bluetooth] and [settings][wp-settings].
- papers. Heinze, Classen and Rohrbach, [MagicPairing][magicpairing]. Heinze et al., [ToothPicker, WOOT][toothpicker].

[apple-specs]: https://support.apple.com/en-au/121204
[apple-listening]: https://support.apple.com/en-gb/guide/airpods/dev6d977ff21/web
[apple-settings]: https://support.apple.com/en-au/108764
[apple-charging]: https://support.apple.com/en-gb/guide/airpods/devde25a4bbe/26/web/26
[apple-pro-transparency]: https://support.apple.com/guide/airpods/switch-between-listening-modes-dev9812f5cc3/web
[apple-balance]: https://support.apple.com/en-la/100494
[lp-commands]: https://github.com/librepods-org/librepods/blob/53679cc90222e94ade84e66542d97ace2540e626/docs/control_commands.md
[lp-packets]: https://github.com/librepods-org/librepods/blob/53679cc90222e94ade84e66542d97ace2540e626/linux/airpods_packets.h
[lp-ear]: https://github.com/librepods-org/librepods/blob/53679cc90222e94ade84e66542d97ace2540e626/linux/eardetection.hpp
[lp-main]: https://github.com/librepods-org/librepods/blob/53679cc90222e94ade84e66542d97ace2540e626/linux/main.cpp
[lp-transparency]: https://github.com/librepods-org/librepods/blob/53679cc90222e94ade84e66542d97ace2540e626/android/app/src/main/java/me/kavishdevar/librepods/data/Transparency.kt
[mp-model]: https://github.com/steam3d/MagicPodsCore/blob/11422817ed7080ac6ec8f02c84dfefa8d1f773dd/src/sdk/aap/enums/AapModelIds.h
[mp-anc]: https://github.com/steam3d/MagicPodsCore/blob/11422817ed7080ac6ec8f02c84dfefa8d1f773dd/src/sdk/aap/setters/AapSetAnc.cpp
[mp-anc-watch]: https://github.com/steam3d/MagicPodsCore/blob/11422817ed7080ac6ec8f02c84dfefa8d1f773dd/src/sdk/aap/watchers/AapAncWatcher.cpp
[mp-one-capability]: https://github.com/steam3d/MagicPodsCore/blob/11422817ed7080ac6ec8f02c84dfefa8d1f773dd/src/device/capabilities/aap/AapNoiseCancellationOneAirPodModeCapability.cpp
[mp-battery]: https://github.com/steam3d/MagicPodsCore/blob/11422817ed7080ac6ec8f02c84dfefa8d1f773dd/src/sdk/aap/enums/AapBatteryType.h
[mp-battery-watch]: https://github.com/steam3d/MagicPodsCore/blob/11422817ed7080ac6ec8f02c84dfefa8d1f773dd/src/sdk/aap/watchers/AapBatteryWatcher.cpp
[mp-h2]: https://help.magicpods.app/help-h2-chip/
[bluez-transport]: https://github.com/bluez/bluez/blob/master/doc/org.bluez.MediaTransport.rst#properties
[pactl]: https://man.archlinux.org/man/pactl.1.en#COMMANDS
[pa-volume]: https://freedesktop.org/software/pulseaudio/doxygen/volume_8h.html
[pa-ui]: https://wiki.freedesktop.org/www/Software/PulseAudio/Documentation/Developer/Clients/WritingVolumeControlUIs/#multichannel-volumes
[pw-loopback]: https://docs.pipewire.org/page_module_loopback.html
[pw-filter]: https://docs.pipewire.org/page_module_filter_chain.html
[pw-properties]: https://docs.pipewire.org/page_man_pipewire-props_7.html
[wp-linking]: https://pipewire.pages.freedesktop.org/wireplumber/policies/linking.html#stream-node-linking-properties
[wp-bluetooth]: https://pipewire.pages.freedesktop.org/wireplumber/daemon/configuration/bluetooth.html
[wp-settings]: https://pipewire.pages.freedesktop.org/wireplumber/daemon/configuration/settings.html
[magicpairing]: https://arxiv.org/abs/2005.07255
[toothpicker]: https://www.usenix.org/conference/woot20/presentation/heinze
