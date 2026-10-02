# feature comparison

the baseline is the [LibrePods](https://github.com/librepods-org/librepods) root
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

for the auris column, ❓ is used in its literal sense. the command is
implemented and the buds accept the write, but nothing has come back to
confirm the setting took effect.

## the matrix

| Feature | LibrePods (linux) | auris | note |
|---|---|---|---|
| Changing Listening Mode | ✅ | ✅ | off, anc, transparency, adaptive |
| Ear detection | ✅ | ✅ | live left and right state in daemon, CLI and UI |
| Battery status | ✅ | ✅ | each bud and case, charging, freshness per cell, last-known cache |
| Renaming AirPods | ✅ | ✅ | auris reads the accessory's own metadata name back |
| Loud Sound Reduction | 🔴 | 🔴 | neither side has it on linux |
| Head Gestures | ⛔ | ⛔ | no linux path, same reason LibrePods gives |
| Conversational Awareness | ✅ | ✅ | control plus the 0x004B event |
| Automatically connect to AirPods | ✅ | ✅ | auris pages over BLE on case opening, on by default, needs LE on the adapter |
| Hearing Aid | 🔴 | 🔴 | neither side has it on linux |
| Transparency Mode customization | 🔴 | 🔴 | auris has adaptive strength only, not the full set |
| Multi-device connectivity (Bluetooth Multipoint; 2 devices only) | ⚪ | ⚪ | auris runs two live hosts with ownership handoff, after an Apple `DeviceID` in bluez and one re-pair |
| Other accessibility configs | 🔴 | see rows below | LibrePods ships these on Android only |
| ... Press speed | 🔴 | ✅ | auris only on linux, physical timing not manually accepted yet |
| ... Press and Hold duration | 🔴 | ✅ | auris only on linux, physical timing not manually accepted yet |
| ... Noise Cancellation with single AirPod | 🔴 | 🔴 | no verified public command, see `PER_BUD_CONTROL_RESEARCH.md` |
| ... Volume control on swipe | 🔴 | 🔴 | neither side has it on linux |
| ... Volume swipe speed | 🔴 | 🔴 | neither side has it on linux |
| Other general configs | 🔴 | see rows below | LibrePods ships these on Android only |
| ... Press and Hold to cycle between listening modes / invoke digital assistant (invoking digital assistant needs a recent firmware) | 🔴 | ❓ | auris sends the cycle, no device report confirms it |
| ... Configure call controls | 🔴 | ✅ | auris only on linux, call behaviour not manually accepted yet |
| ... Personalized volume | 🔴 | ✅ | auris only on linux, audio effect not manually accepted yet |
| ... Loud Sound Reduction (needs VendorID spoofing) | 🔴 | 🔴 | neither side has it on linux |
| ... Microphone side | 🔴 | ❓ | auris sends it, no device report confirms it |
| ... Pause media when falling asleep (needs a recent firmware) | 🔴 | 🔴 | neither side has it on linux |
| ... Enable `Off listening mode` to switch to `Off` | 🔴 | ✅ | off is one of the four selectable modes |
| Head-tracked Spatial Audio | ❓ | 🔴 | needs audio-stack work well beyond this plugin |
| Heart Rate Monitoring | ⛔ | ⛔ | AirPods 4 (ANC) has no heart rate sensor |
| Find My | ❓ | 🔴 | needs protocol and security work beyond this plugin |
| High quality two-way audio | 🔴 | 🔴 | neither side has it on linux |
| Auto play/pause on ear detection | ✅ | ✅ | auris pauses local players when a bud leaves the ear and resumes them when it goes back in |
| Seamless handoff between hosts | ✅ | ✅ | verified against a Mac |

## what auris has beyond the LibrePods list

LibrePods does not list these, so its status for them is not known from the
README.

- adaptive noise level, the ambient adjustment slider
- automatic take-over from a Mac on a local play edge, with the Mac banner honoured and a configurable `bt_name`
- take-over is live on hardware, and never runs while the other host is in a call
- yield to another Apple host gives up ownership and pauses local MPRIS players, while A2DP and HFP stay up
- the yield holds, so a self-resuming browser is not read as the user asking for the buds back
- automatic rejoin after an Apple host evicts this host uses one guarded reconnect, live end to end in about seven seconds
- automatic rejoin after a bud role switch times the link out, where `Timeout` qualifies when the buds still show an Apple host with its link up
- automatic rejoin after link loss, on the same guarded path, with an eviction-loop stop
- pipewire card profile follows AAP ownership, with profile `off` while another host owns the buds, A2DP back on take-over, and never saved as the user's choice
- `link` status published in `state.json` (schema 2) takes the values `connected`, `reconnecting` with a reason and attempt count, and `disconnected`
- the bar widget stays visible during a heal, showing "Attempting to reconnect", with last readings dimmed, the cause named and pending setup writes not cancelled
- the panel closes itself once the AirPods are really gone, after a two second hold on any non-healing disconnect
- opt-in BLE battery telemetry bound to a provisioned IRK, identity matched and bounded, with keys never acquired automatically
- native DMS bar pill, popout and settings panel
- rootless operation, with no patched bluez and no spoofed vendor id for the base features
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

handoff, rejoin and the audio-route follow were each captured working against
a Mac. the live timelines were replayed in unit tests. auto-connect on case
open and the in-ear pause and resume are confirmed working on AirPods 4 (ANC).

hearing aid, Find My, spatial audio, heart rate and high-quality two-way audio
each need protocol, security or audio-stack work well beyond this plugin. none of
them is a roadmap commitment.
