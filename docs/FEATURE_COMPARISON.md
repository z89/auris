# feature comparison

comparison with [LibrePods](https://github.com/librepods-org/librepods), as of
2026-09-13. this uses LibrePods' current root feature table as the baseline. its
Linux README marks the detailed application description as the old version and
points to the root table for the rewrite.

auris is intentionally narrower: a rootless AirPods daemon, CLI and native DMS
widget rather than a general AirPods configuration application.

Development update: all seven controls have software implementations for AirPods
4 ANC. The target buds accepted and persisted changed values for press speed, hold
duration, call controls and Personalized Volume; every original value was restored.
Their real-world gesture/call/audio effects still need manual acceptance. Rename,
microphone selection and listening-mode cycling remain unverified.

| feature | LibrePods on Linux | auris |
|---|---|---|
| listening modes | yes | yes: off, anc, transparency and adaptive |
| ear detection | yes | yes: live left/right state in the daemon and UI |
| automatic media pause/resume | available in the older Linux application | no; auris reports ear state but does not control players |
| battery status | yes | yes: each bud and case, live/last-known charging, presence and cache |
| rename AirPods | yes | 🟡 software implemented; no confirming metadata yet |
| conversational awareness | yes | yes |
| automatic Bluetooth connection | yes | unattended connection remains off; one guarded AAP attempt on a local link, recovery suppression after ambiguous loss, and an explicit pinned-device `connect-once` command |
| adaptive level | not listed as a separate Linux feature | yes |
| loud sound reduction | planned | no |
| head gestures | will not be implemented on Linux | no |
| hearing-aid controls | planned in the current feature table | no |
| transparency customization | planned | no; adaptive strength is not the full accessibility control set |
| multipoint/vendor-id handoff | vendor-ID spoofing required | no; auris deliberately does not spoof an Apple vendor ID |
| accessibility configuration | planned | ☑️ press speed and hold duration changes accepted, reported and persisted on AirPods 4 ANC; physical timing still needs manual acceptance |
| general AirPods configuration | planned | ☑️ call mapping and Personalized Volume changes accepted, reported and persisted; 🟡 microphone side and listening-mode cycle have no device report yet |
| head-tracked spatial audio | unknown | no |
| heart-rate monitoring | will not be implemented on Linux | no |
| Find My | unknown | no |
| high-quality two-way audio | planned | no |

## where auris differs

- native DankMaterialShell bar pill, popout and control-center integration
- the bar widget appears and disappears from daemon pushes as the AirPods connect
- rootless AAP connection without a patched BlueZ or Apple vendor-ID spoofing
- small Rust daemon with a line-delimited JSON control socket and CLI
- atomic JSON state file for other tools, plus a streaming subscription
- last-known battery readings survive disconnects and daemon restarts

The comparison is not a roadmap commitment. Hearing-aid controls, Find My,
spatial audio, multipoint and high-quality microphone transport each require
substantial protocol, security or audio-stack work beyond the current plugin.

Status here is deliberately strict: ✅ means end-to-end behavior verified; ☑️
means the target AirPods accepted, reported and persisted the configuration but
its experiential effect still needs a manual check; 🟡 means implemented without
device confirmation. A sent packet alone earns no status.

Automatic media pause/resume, Bluetooth auto-connect, custom stem actions,
sleep-triggered pause and case sounds remain planned; telemetry or a command
identifier is not a completed implementation of those behaviors.
