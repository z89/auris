# AirPods 4 ANC feature roadmap

target hardware: AirPods 4 with active noise cancellation, product id
`201B`. entries are grouped by verification state rather than by delivery
order: implemented and device-verified, implemented but unconfirmed by any
device report, blocked by a named technical constraint, and deliberately
out of scope.

## implemented and verified against the accessory's own reports

| feature | verification |
|---|---|
| press speed | value change accepted, confirmed by immediate and startup readback |
| hold duration | value change accepted, confirmed by immediate and startup readback |
| call controls | value change accepted, confirmed by immediate and startup readback |
| personalized volume | value change accepted, confirmed by immediate and startup readback |

## implemented but unconfirmed by any device report

| feature | status |
|---|---|
| settings foundation | typed commands, optional snapshot fields, and the unknown/readback/pending distinction are implemented; old schema-1 snapshots still deserialize |
| rename | accessory rename command is implemented and sent; device-side persistence and cross-host effect are unconfirmed |
| microphone side selection | automatic/left/right selection is implemented; physical one-bud behavior is unconfirmed |
| listening-mode cycle | implemented against a validated set of supported modes; stem-triggered cycling and changes made by another host are unconfirmed |
| identity-matched BLE observation | implemented, opt-in; does not acquire or provision BLE keys automatically |
| guarded one-attempt AAP recovery | implemented |
| in-ear media control | pause and resume of local MPRIS players on in-ear transitions is implemented, with a settle delay, a bounded resume window and an ownership gate; the physical bud-out and bud-in path is unconfirmed |
| ordered yield | local playback is drained and the card profile released before ownership is handed over, so playback stops instead of moving to another sink; unconfirmed against a second host |
| per-key requested/report tracking, non-shifting toasts, per-cell battery freshness | implemented in the settings/readback and telemetry layer |

unattended Bluetooth connection remains disabled regardless of the above;
see [battery and handoff safety](BATTERY_AND_HANDOFF.md) for limitations.
per-bud power control and simultaneous mixed listening modes have no
verified public command and are not covered by any entry in this section.
see [per-bud control research](PER_BUD_CONTROL_RESEARCH.md).

## blocked by a named technical constraint

| feature | blocking detail |
|---|---|
| rename visibility on Apple hosts | Apple hosts read the device name from their own iCloud-synced pairing record, never from the accessory's advertised name; see [Mac agent research](MAC_AGENT_AND_GESTURES_RESEARCH.md) |
| automatic media pause/resume | requires an MPRIS ownership model and debounced ear-event rules that are not yet built |
| automatic Bluetooth connection | requires a bounded-backoff BlueZ connection policy that is not yet built |
| stem assistant / custom actions | requires confirmed hardware event delivery and per-user binding isolation that are not yet built |
| pause on sleep | firmware command and device-reported event for this behavior are not yet confirmed on AirPods 4 |
| case charging sounds | whether the command applies to this case, and whether its state is readable, is not yet confirmed |
| head gestures | requires a confirmed motion/event transport, coordinate system, and calibration, plus a labelled-trace classifier; see [Mac agent research](MAC_AGENT_AND_GESTURES_RESEARCH.md) |
| case locating sound | reachability, command, cancellation, and bounded duration are not yet confirmed |
| multipoint/handoff | transport, identity, and ownership model are not yet researched; would require a separate privileged-spoofing design and a two-device test matrix |
| head-tracked spatial audio | requires a proven timestamped motion stream, drift handling, and a renderer latency budget of a few tens of milliseconds; see [Mac agent research](MAC_AGENT_AND_GESTURES_RESEARCH.md) |
| high-quality simultaneous mic/playback | microphone transport and format are not yet proven; needs isolation, resampling, and clock synchronization |
| Find My network / left-behind alerts | beacon decryption keys are iCloud Keychain items gated by Secure Enclave attestation; unreadable from Linux or from a headless Mac session; see [Mac agent research](MAC_AGENT_AND_GESTURES_RESEARCH.md) |
| Mac-agent rename via IOBluetooth `setName:` | requires a Bluetooth permission grant to a signed helper; unproven with the grant in place |
| Mac Keychain secret export | raises a GUI password prompt on every run; no headless path exists |
| Find My location read from a Mac agent | requires an iCloud Keychain unlock per read; Apple's access group stays unreadable even after unlock |

## deliberately out of scope

| feature | reason |
|---|---|
| hearing test, hearing aid mode, hearing protection | AirPods Pro 2 features; absent from AirPods 4 firmware |
| heart rate sensing | not present on this model |
| stem volume swipes | not present on this model |
| iPhone as a proxy for Apple-side operations | no ssh, no background helper, and no Shortcuts action for AirPods settings on iOS without a jailbreak, which is not a dependency this project carries |
| personalized spatial-audio profile without an iPhone | the profile is produced by an iPhone ear scan; there is no Linux equivalent |
| pairing changes, audio-profile changes, VendorID spoofing, system-service restarts during settings work | excluded from the settings feature set as a safety rule |
| reusing GPL LibrePods implementation code | keeps auris under MIT licensing; protocol facts are used, the implementation is independent |
| `sudo` or passwordless root for the Mac agent | not required by the design; the helper runs in the user's GUI session instead |

## product and engineering rules

### truthfulness of state

- distinguish software implementation, device-reported state, and
  hardware-tested behavior.
- a successful socket write is not proof that the AirPods applied a
  setting.
- null means unknown; it does not mean off or unsupported.
- do not infer full support from a command identifier, a model number, or
  an optimistic local state update.

### setting mechanics

- use typed settings, strict ranges, and enum validation.
- use confirmed readback, visible pending/error states, and finite
  confirmation timeouts.
- clear volatile device settings on disconnect or device change.
- never silently replay stale preferences onto another connection.

### compatibility

- keep existing CLI commands, schema-1 readers, battery caching, and push
  subscriptions working.
- add optional fields with defaults rather than renaming existing fields.
- document every new public command and state field.

## references

- [LibrePods feature availability](https://github.com/librepods-org/librepods#feature-availability)
- [LibrePods AACP manager](https://github.com/librepods-org/librepods/blob/main/android/app/src/main/java/me/kavishdevar/librepods/bluetooth/AACPManager.kt)
- [Apple AirPods 4 ANC specifications](https://support.apple.com/en-au/121204)
- [Apple adaptive audio controls](https://support.apple.com/en-gb/104979)
- [feature comparison](FEATURE_COMPARISON.md) for the pre-expansion baseline
