# feature comparison

baseline: the [LibrePods](https://github.com/librepods-org/librepods) root
`README.md` at commit `b5a3eae`.

auris is deliberately narrower. it is a rootless AirPods daemon, CLI and native
DankMaterialShell widget for AirPods 4 (ANC). it is not a general AirPods
configuration application.

there is no Android column here. the auris column uses LibrePods' own symbols, so
both columns read alike. the note says where a symbol alone would overstate or
understate what was checked on hardware.

## legend

copied from the LibrePods root README.

| Symbol | Meaning |
| ------ | ------- |
| ✅ | Implemented and works well |
| ⚪ | Needs VendorID spoofing; use at your own risk |
| 🔴 | Not implemented yet; planned |
| ⛔ | Will not be implemented |
| ❓ | Unknown |

for the auris column, ❓ is used in its literal sense: the command is
implemented and the buds accept the write, but nothing has come back to
confirm the setting took effect.

## the matrix

| Feature | LibrePods (Linux) | auris | note |
|---|---|---|---|
| Changing Listening Mode | ✅ | ✅ | off, anc, transparency, adaptive |
| Ear detection | ✅ | ✅ | live left and right state in daemon, cli and ui |
| Battery status | ✅ | ✅ | each bud and case, charging, freshness per cell, last-known cache |
| Renaming AirPods | ✅ | ✅ | auris reads the accessory's own metadata name back |
| Loud Sound Reduction | 🔴 | 🔴 | neither side has it on Linux |
| Head Gestures | ⛔ | ⛔ | no Linux path, same reason LibrePods gives |
| Conversational Awareness | ✅ | ✅ | control plus the 0x004B event |
| Automatically connect to AirPods | ✅ | ✅ | auris pages over ble on case opening, on by default |
| Hearing Aid | 🔴 | 🔴 | neither side has it on Linux |
| Transparency Mode customization | 🔴 | 🔴 | auris has adaptive strength only, not the full set |
| Multi-device connectivity (Bluetooth Multipoint; 2 devices only) | ⚪ | ⚪ | auris runs two live hosts with ownership handoff, after an Apple `DeviceID` in BlueZ and one re-pair |
| Other accessibility configs | 🔴 | see rows below | LibrePods ships these on android only |
| ... Press speed | 🔴 | ✅ | auris only on Linux, physical timing not manually accepted yet |
| ... Press and Hold duration | 🔴 | ✅ | auris only on Linux, physical timing not manually accepted yet |
| ... Noise Cancellation with single AirPod | 🔴 | 🔴 | no verified public command, see `PER_BUD_CONTROL_RESEARCH.md` |
| ... Volume control on swipe | 🔴 | 🔴 | neither side has it on Linux |
| ... Volume swipe speed | 🔴 | 🔴 | neither side has it on Linux |
| Other general configs | 🔴 | see rows below | LibrePods ships these on android only |
| ... Press and Hold to cycle between listening modes / invoke digital assistant (invoking digital assistant needs a recent firmware) | 🔴 | ❓ | auris sends the cycle, no device report confirms it |
| ... Configure call controls | 🔴 | ✅ | auris only on Linux, call behaviour not manually accepted yet |
| ... Personalized volume | 🔴 | ✅ | auris only on Linux, audio effect not manually accepted yet |
| ... Loud Sound Reduction (needs VendorID spoofing) | 🔴 | 🔴 | neither side has it on Linux |
| ... Microphone side | 🔴 | ❓ | auris sends it, no device report confirms it |
| ... Pause media when falling asleep (needs a recent firmware) | 🔴 | 🔴 | neither side has it on Linux |
| ... Enable `Off listening mode` to switch to `Off` | 🔴 | ✅ | off is one of the four selectable modes |
| Head-tracked Spatial Audio | ❓ | 🔴 | needs audio-stack work well beyond this plugin |
| Heart Rate Monitoring | ⛔ | ⛔ | AirPods 4 (ANC) has no heart rate sensor |
| Find My | ❓ | 🔴 | needs protocol and security work beyond this plugin |
| High quality two-way audio | 🔴 | 🔴 | neither side has it on Linux |
| Auto play/pause on ear detection | ✅ | ✅ | auris pauses local players when a bud leaves the ear and resumes them when it goes back in |
| Seamless handoff between hosts | ✅ | ✅ | verified against a Mac |

## what auris has beyond the LibrePods list

LibrePods does not list these, so its status for them is simply not known from
the README.

- adaptive noise level, the ambient adjustment slider
- automatic take-over from a Mac on a local play edge, with the Mac banner honoured and a configurable `bt_name`
- take-over is live on hardware, and never runs while the other host is in a call
- yield to another Apple host: ownership is given up and local MPRIS players pause, while A2DP and HFP stay up
- the yield holds, so a self-resuming browser is not read as the user asking for the buds back
- automatic rejoin after an Apple host evicts this host: one guarded reconnect, live end to end in about seven seconds
- automatic rejoin after a bud role switch times the link out, where `Timeout` qualifies when the buds still show an Apple host with its link up
- automatic rejoin after link loss, on the same guarded path, with an eviction-loop stop
- PipeWire card profile follows AAP ownership: profile `off` while another host owns the buds, a2dp back on take-over, never saved as the user's choice
- `link` status published in `state.json` (schema 2): `connected`, `reconnecting` with a reason and attempt count, `disconnected`
- the bar widget stays visible during a heal, showing "Attempting to reconnect", with last readings dimmed, the cause named and pending setup writes not cancelled
- the panel closes itself once the AirPods are really gone, after a two second hold on any non-healing disconnect
- opt-in BLE battery telemetry bound to a provisioned IRK: identity matched and bounded, and keys are never acquired automatically
- native DankMaterialShell bar pill, popout and settings panel
- rootless operation, with no patched BlueZ and no spoofed vendor id for the base features
- line-delimited JSON control socket, CLI and atomic state file for other tools
- connected-devices and ownership decoding surfaced to the UI (0x000E, 0x0010, 0x0011, 0x002E, control 0x06)

## reading the auris column honestly

a sent packet is not a verified feature.

press speed, hold duration, call controls and Personalized Volume were changed,
reported back by AirPods 4 (ANC) and restored to their original values. the write
path is therefore proven. the experiential effect still needs a manual check.

a rename is confirmed. the daemon reads the accessory's own metadata name back
after the rename.

microphone side and the listening-mode cycle produce no device report. that is
why they carry ❓.

handoff, rejoin, auto-connect and the audio-route follow were each captured
working against a Mac. the live timelines were replayed in unit tests.

hearing aid, Find My, spatial audio, heart rate and high-quality two-way audio
each need protocol, security or audio-stack work well beyond this plugin. none of
them is a roadmap commitment.
