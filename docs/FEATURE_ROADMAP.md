# AirPods 4 ANC feature roadmap

the target hardware is AirPods 4 with active noise cancellation, product id
`201B`. entries are grouped by verification state rather than by delivery
order, into implemented and verified on hardware, implemented but unconfirmed
by any device report, blocked by a named technical constraint, and
deliberately out of scope.

## implemented and verified on hardware

| feature | verification |
|---|---|
| press speed | value change accepted, confirmed by immediate and startup readback |
| hold duration | value change accepted, confirmed by immediate and startup readback |
| call controls | value change accepted, confirmed by immediate and startup readback |
| personalized volume | value change accepted, confirmed by immediate and startup readback |
| multi-host handoff | yield on another host's claim and the take-over on a local play edge were exercised against a Mac on macOS 26.6.2, and the captured timelines are replayed in `handoff.rs` tests. it is opt-in under `[handoff] enabled` and needs an Apple `DeviceID` in bluez plus one re-pair. keeping both profiles connected on yield is not confirmed against an Apple host, and a yield during a call on the other host is not exercised |
| rename | the rename is sent, then read back from the accessory's own `0x1D` metadata on a reopened link and reported as `confirmed` or `mismatch`. confirmed working on AirPods 4 (ANC) in daily use. Apple hosts never show the new name, see the blocked row below |
| in-ear media control | pause and resume of local MPRIS players on in-ear transitions, on by default under `[ear]`, with a 700ms settle delay, a five-minute resume window and an ownership gate. the bud-out pause and bud-in resume are confirmed working on AirPods 4 (ANC) in daily use |
| auto-connect on case open | on by default under `[autoconnect]`. a qualifying proximity advert starts one bounded connect sequence, and a slow page fallback covers adapters that miss the advert. a case-open connect is confirmed working on AirPods 4 (ANC) in daily use |

## implemented but unconfirmed by any device report

| feature | status |
|---|---|
| settings foundation | typed commands, optional snapshot fields, and the unknown/readback/pending distinction are implemented. state.json is schema 2 and old schema-1 snapshots still deserialize |
| microphone side selection | automatic/left/right selection is implemented. the accessory sends no confirming report on this firmware, and physical one-bud behavior is unconfirmed |
| listening-mode cycle | implemented against a validated set of supported modes. the accessory sends no confirming report on this firmware. stem-triggered cycling and changes made by another host are unconfirmed |
| identity-matched BLE battery telemetry | implemented and opt-in under `[ble]`, off by default. it does not acquire or provision BLE keys automatically. auto-connect is a separate path that matches the proximity advert by model id and needs no keys |
| guarded one-attempt AAP recovery | implemented. one AAP attempt per local connection, with no automatic AAP redial after an ambiguous loss |
| rejoin after eviction, bud switch or link loss | implemented and on by default once handoff is enabled. the eviction rules are replayed in `rejoin.rs` tests from drops captured against a Mac. the `connected` and `link_lost` rules have not fired on hardware on their own |
| ordered yield | local playback is paused and drained, then the card profile is released, before ownership is handed over, so playback stops instead of moving to another sink. the yield itself is exercised under multi-host handoff above. the drain and release order is auris policy with bounded waits and is not separately confirmed on hardware |
| per-key requested/report tracking, non-shifting toasts, per-cell battery freshness | implemented in the settings/readback and telemetry layer |

unattended bluetooth connection is on by default through auto-connect, and
through rejoin once handoff is enabled. both page through one guarded
`Device1.Connect` path, a pending rejoin outranks auto-connect, and a manual
connect command or a `Local` disconnect disarms auto-connect until the
AirPods go away again.
see [battery and handoff safety](BATTERY_AND_HANDOFF.md) for limitations.
per-bud power control and simultaneous mixed listening modes have no
verified public command and are not covered by any entry in this section.
see [per-bud control research](PER_BUD_CONTROL_RESEARCH.md).

## blocked by a named technical constraint

| feature | blocking detail |
|---|---|
| rename visibility on Apple hosts | Apple hosts read the device name from their own iCloud-synced pairing record, never from the accessory's advertised name. see [Mac agent research](MAC_AGENT_AND_GESTURES_RESEARCH.md) |
| stem assistant / custom actions | requires confirmed hardware event delivery and per-user binding isolation that are not yet built |
| pause on sleep | firmware command and device-reported event for this behavior are not yet confirmed on AirPods 4 |
| case charging sounds | whether the command applies to this case, and whether its state is readable, is not yet confirmed |
| head gestures | requires a confirmed motion/event transport, coordinate system, and calibration, plus a labelled-trace classifier. see [Mac agent research](MAC_AGENT_AND_GESTURES_RESEARCH.md) |
| case locating sound | reachability, command, cancellation, and bounded duration are not yet confirmed |
| head-tracked spatial audio | requires a proven timestamped motion stream, drift handling, and a renderer latency budget of a few tens of milliseconds. see [Mac agent research](MAC_AGENT_AND_GESTURES_RESEARCH.md) |
| high-quality simultaneous mic/playback | microphone transport and format are not yet proven. it needs isolation, resampling, and clock synchronization |
| Find My network / left-behind alerts | beacon decryption keys are iCloud Keychain items gated by Secure Enclave attestation. they are unreadable from linux or from a headless Mac session. see [Mac agent research](MAC_AGENT_AND_GESTURES_RESEARCH.md) |
| Mac-agent rename via IOBluetooth `setName:` | requires a bluetooth permission grant to a signed helper. it is unproven with the grant in place |
| Mac Keychain secret export | raises a GUI password prompt on every run. no headless path exists |
| Find My location read from a Mac agent | requires an iCloud Keychain unlock per read. Apple's access group stays unreadable even after unlock |

## deliberately out of scope

| feature | reason |
|---|---|
| hearing test, hearing aid mode, hearing protection | AirPods Pro 2 features, absent from AirPods 4 firmware |
| heart rate sensing | not present on this model |
| stem volume swipes | not present on this model |
| iPhone as a proxy for Apple-side operations | no ssh, no background helper, and no Shortcuts action for AirPods settings on iOS without a jailbreak, which is not a dependency this project carries |
| personalized spatial-audio profile without an iPhone | the profile is produced by an iPhone ear scan. there is no linux equivalent |
| pairing changes, audio-profile changes, VendorID spoofing, system-service restarts during settings work | excluded from the settings feature set as a safety rule. multi-host handoff is separate. it sets the pipewire card profile without saving it, and relies on an Apple `DeviceID` the user sets in bluez |
| reusing GPL LibrePods implementation code | keeps auris under MIT licensing. protocol facts are used and the implementation is independent |
| `sudo` or passwordless root for the Mac agent | not required by the design. the helper runs in the user's GUI session instead |

## product and engineering rules

### truthfulness of state

- distinguish software implementation, device-reported state, and
  hardware-tested behavior.
- a successful socket write is not proof that the AirPods applied a
  setting.
- null means unknown. it does not mean off or unsupported.
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
- [feature comparison](FEATURE_COMPARISON.md) for the annotated LibrePods matrix
