# features

auris is a rootless AirPods daemon, CLI and native DankMaterialShell (DMS) widget for AirPods 4 (ANC), model `0x201B`. it is narrower than a general AirPods configuration application. the baseline for comparison is the [LibrePods](https://github.com/librepods-org/librepods) root `README.md` at commit `b5a3eae`.

## feature matrix

there is no Android column. the auris column uses LibrePods' symbols so both columns read alike, and the note says where a symbol alone would overstate or understate what was checked on hardware. for auris, ❓ is literal. the command is implemented and the buds accept the write, but nothing comes back to confirm the setting took effect.

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
| ... Noise Cancellation with single AirPod | 🔴 | 🔴 | no known command turns it on, see [per-bud control](RESEARCH.md#per-bud-control) |
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

### beyond the LibrePods list

LibrePods does not list these.

- adaptive noise level (the ambient adjustment slider).
- take-over from a Mac on a local play edge, honouring the Mac banner, with a configurable `bt_name`. live on hardware, never while the other host is in a call.
- yield to another Apple host releases ownership and pauses local MPRIS players, A2DP and HFP stay up. the yield holds, so a self-resuming browser does not count as the user reclaiming the buds.
- rejoin through one guarded reconnect after an Apple host evicts this host (live end to end in about 7s), after a bud role switch times the link out (`Timeout` qualifies while the buds still show an Apple host with its link up), and after link loss, with an eviction-loop stop.
- the pipewire card profile follows AAP ownership, `off` while another host owns the buds and A2DP again on take-over, never saved as the user's choice.
- `link` in `state.json` (schema 2) is `connected`, `reconnecting` with reason and attempt count, or `disconnected`.
- during a heal the bar widget stays visible with "Attempting to reconnect", dimmed last readings, the cause named and pending setup writes kept. the panel closes after a 2s hold on any non-healing disconnect.
- opt-in BLE battery telemetry bound to a provisioned IRK, identity matched and bounded, keys never acquired automatically.
- native DMS bar pill, popout and settings panel, rootless, no patched bluez and no spoofed vendor id for the base features.
- line-delimited JSON control socket, CLI and atomic state file.
- connected-devices and ownership decoding in the UI (`0x000E`, `0x0010`, `0x0011`, `0x002E`, control `0x06`).

## status on AirPods 4 (ANC)

a sent packet is not a verified feature. verified means checked on hardware. readback means the AirPods report the new value but nobody checked the physical effect. unconfirmed means sent with no confirming report. implemented means built and tested in software only.

| feature | state | evidence and detail |
|---|---|---|
| multi-host handoff | verified | yield on another host's claim and take-over on a local play edge exercised against a Mac on macOS 26.6.2. handoff, rejoin and the audio-route follow were each captured, and the timelines are replayed in `handoff.rs` tests. opt-in under `[handoff] enabled`, needs an Apple `DeviceID` in bluez and one re-pair. not checked are both profiles staying up on yield to an Apple host, and a yield during a call on the other host |
| rename | verified, owner 2026-10-01 | read back from the accessory's `0x1D` metadata on a reopened link, reported as `confirmed` or `mismatch`. Apple hosts never show the new name |
| in-ear media control | verified, owner 2026-10-01 | pause and resume of local MPRIS players. on by default under `[ear]`, 700ms settle, 5min resume window, ownership gate |
| auto-connect on case open | verified, owner 2026-10-01 | on by default under `[autoconnect]`. a qualifying proximity advert starts one bounded connect sequence if the adapter has LE. a slow page fallback covers missed adverts and LE off. with `ControllerMode = bredr` in bluez the scan cannot start, only the fallback connects, and `state.json` reports `autoconnect.scan` `le_disabled`. advert trigger checked on a MediaTek MT7925 in dual mode after pairing in bredr mode |
| press speed, hold duration, call controls, personalized volume | readback | accepted, confirmed by the reopen readback after each write and again at startup (only call controls also echoes the change at once), restored to the original values. physical or audible effect not checked by hand |
| microphone side | unconfirmed | automatic, left and right. no report on this firmware, one-bud behaviour unchecked |
| listening-mode cycle | unconfirmed | validated against the supported modes. no report on this firmware. stem-triggered cycling and changes from another host unchecked |
| settings foundation | implemented | typed commands, optional snapshot fields, unknown versus readback versus pending, per-key requested and reported tracking, non-shifting toasts, per-cell battery freshness. `state.json` schema 2, schema-1 snapshots still deserialize |
| BLE battery telemetry | implemented, opt-in | `[ble]`, off by default, never acquires or provisions keys. auto-connect matches the proximity advert by model id and needs no keys |
| guarded AAP recovery | implemented | one AAP attempt per local connection, no automatic redial after an ambiguous loss |
| rejoin after eviction, bud switch or link loss | implemented | on by default once handoff is enabled. eviction rules replayed in `rejoin.rs` tests from drops captured against a Mac. the `connected` and `link_lost` rules have not fired on hardware on their own |
| ordered yield | implemented | pause and drain local playback, release the card profile, then hand over ownership, so playback stops instead of moving to another sink. bounded waits. the order is auris policy, not separately confirmed on hardware |
| rename visibility on Apple hosts | blocked | Apple hosts read the name from their iCloud-synced pairing record, never from the accessory. see [Mac helper](RESEARCH.md#mac-helper) |
| stem assistant, custom actions | blocked | needs confirmed hardware event delivery and per-user binding isolation |
| pause on sleep | blocked | firmware command and device event unconfirmed on AirPods 4 |
| case charging sounds | blocked | unconfirmed whether the command applies to this case or its state is readable |
| head gestures | blocked | needs a confirmed motion transport, coordinate system, calibration and a labelled-trace classifier. see [motion gestures](RESEARCH.md#motion-gestures) |
| case locating sound | blocked | reachability, command, cancellation and bounded duration unconfirmed |
| head-tracked spatial audio | blocked | needs a proven timestamped motion stream, drift handling and a renderer latency budget of a few tens of milliseconds. see [spatial audio](RESEARCH.md#spatial-audio) |
| high-quality simultaneous mic and playback | blocked | mic transport and format unproven. needs isolation, resampling and clock sync |
| Find My network, left-behind alerts | blocked | beacon keys are iCloud Keychain items gated by Secure Enclave attestation, unreadable from linux or a headless Mac session. see [Mac helper](RESEARCH.md#mac-helper) |
| Mac helper rename via IOBluetooth `setName:` | blocked | needs a bluetooth permission grant to a signed helper, unproven with the grant |
| Mac Keychain secret export | blocked | GUI password prompt on every run, no headless path |
| Find My location from a Mac helper | blocked | needs an iCloud Keychain unlock per read, and Apple's access group stays unreadable after unlock |
| hearing test, hearing aid mode, hearing protection | out of scope | AirPods Pro 2 features, absent from AirPods 4 firmware |
| heart rate sensing, stem volume swipes | out of scope | not on this model |
| iPhone as a proxy for Apple-side operations | out of scope | iOS has no ssh, background helper or Shortcuts action for AirPods settings without a jailbreak |
| personalized spatial-audio profile without an iPhone | out of scope | made by an iPhone ear scan, no linux equivalent |
| pairing changes, audio-profile changes, VendorID spoofing, system-service restarts during settings work | out of scope | safety rule for settings. handoff is separate, it sets the pipewire card profile without saving it and relies on an Apple `DeviceID` the user sets in bluez |
| reusing GPL LibrePods code | out of scope | keeps auris MIT. protocol facts are used, the implementation is independent |
| `sudo` or passwordless root for the Mac helper | out of scope | the helper runs in the user's GUI session |

unattended connection is on by default through auto-connect, and through rejoin once handoff is enabled. both page through one guarded `Device1.Connect` path and a pending rejoin outranks auto-connect. a manual connect or a `Local` disconnect disarms auto-connect until the AirPods go away. limitations are in [battery and handoff safety](BATTERY_AND_HANDOFF.md).

per-bud power control and mixed per-bud listening modes have no verified public command, see [per-bud control](RESEARCH.md#per-bud-control). none of the blocked or out-of-scope rows is a roadmap commitment.

## rules

- keep implementation, device-reported state and hardware-tested behaviour apart. a socket write does not prove the AirPods applied a setting.
- null means unknown, not off or unsupported. never infer support from a command id, a model number or an optimistic local update.
- settings are typed with strict ranges and enum validation, confirmed readback, visible pending and error states and finite confirmation timeouts.
- clear volatile device settings on disconnect or device change. never replay stale preferences onto another connection.
- keep existing CLI commands, schema-1 readers, battery caching and push subscriptions working. add optional fields with defaults instead of renaming, and document every new public command and state field.

## sources

- [LibrePods](https://github.com/librepods-org/librepods), root `README.md` at commit `b5a3eae`
- [LibrePods feature availability](https://github.com/librepods-org/librepods#feature-availability)
- [LibrePods latest README](https://github.com/librepods-org/librepods/blob/main/README.md)
- [LibrePods AACP manager](https://github.com/librepods-org/librepods/blob/main/android/app/src/main/java/me/kavishdevar/librepods/bluetooth/AACPManager.kt)
- [Apple AirPods 4 ANC specifications](https://support.apple.com/en-au/121204)
- [Apple adaptive audio controls](https://support.apple.com/en-gb/104979)
