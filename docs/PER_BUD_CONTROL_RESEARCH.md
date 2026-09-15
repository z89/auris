# independent control of AirPods 4 ANC

feasibility assessment of per-bud media routing, per-bud noise control and
per-bud power for AirPods 4 ANC (model `0x201B`, H2 chip with optical
in-ear sensing). these are feasibility assessments, not shipped features.
Pro-only accessibility features and earlier-generation reverse engineering
do not automatically transfer to this hardware; see
[Apple's model-specific specifications][apple-specs].

## summary table

| requested behaviour | assessment | implementation consequence |
|---|---|---|
| media through the left or right bud only | feasible as host PCM processing; physical mapping unverified on this device | prototype a reversible virtual audio sink, then test every bud-presence transition |
| hear both stereo channels through one bud | feasible downmix; loses stereo separation | use a bounded-gain mono mix before placing it in the chosen output channel |
| anc with only one bud worn | documented for AirPods 4 ANC; known protocol setting | add model-gated permission with honest readback status and hardware tests |
| left anc and right transparency simultaneously | no supported public command identified | research only; require evidence of a side-selecting control before experimental writes |
| power off a chosen bud while it remains out/in-ear | no supported public command identified | research only; require independent power and reliable wake measurements |
| put one bud in its closed case | Apple's documented physical shutdown/charging mechanism | not a new remotely controlled auris feature |

the most credible addition is host-side media routing to one ear: a
stereo/left-only/right-only choice, with an option to mix both source
channels into the selected ear. this is a Linux audio-processing feature,
not an AirPods power command, and its physical left/right behaviour still
needs validation on the target buds; that matters most when only one bud
is available.

one-bud noise cancellation is a separate, established Apple feature and a
good candidate for auris. it does not permit simultaneous anc on one side
and transparency on the other. no verified public command was found for
that mixed-mode behaviour, and none was found for powering off an
individual out-of-case bud either. absence from public implementations is
not proof that a hidden firmware path cannot exist, but it is insufficient
evidence to advertise or implement one.
[Apple's AirPods 4 ANC listening settings][apple-listening] describe
one-bud anc explicitly; the [LibrePods command catalog][lp-commands]
exposes a separate permission for it, plus a global listening-mode value.

## what was examined: listening mode and physical-side addressing

### the known commands carry no side

listening mode is control identifier `0x0D`, carrying a scalar mode.
one-bud anc uses `0x1B`. automatic ear-detection behaviour uses `0x0A`.
neither the listening-mode value nor the one-bud permission carries a
left/right destination. by contrast, known stem-hold configuration
contains explicitly ordered right/left values, so some settings are
side-specific in principle; but there is no demonstrated universal side
selector that can be attached to arbitrary commands.
[LibrePods' command definitions][lp-commands] and its
[Linux packet definitions][lp-packets] show these different payload
shapes.

**conclusion: no verified command exists for per-side anc/transparency
mixing.** this rules the feature out until a side-selecting control is
demonstrated.

### independent implementations agree

MagicPodsCore implements a single global mode in its
[anc setter][mp-anc] and [anc watcher][mp-anc-watch]. it recognises
[AirPods 4 ANC's model identifier][mp-model], but recognising the model is
not evidence that every reverse-engineered command works on its firmware.
the same project's [one-AirPod anc capability][mp-one-capability] notes a
notification gap after changing that setting, so a local optimistic value
must not be presented as confirmed hardware readback.

**conclusion: a second independent implementation reaches the same single
global-mode model.** this corroborates the "no side" finding rather than
weakening it.

### the reconnect trick does not work

setting anc, reconnecting through the other bud, then setting transparency
is not a credible independent-mode implementation. the known API controls
the logical accessory as a whole; without evidence to the contrary, a
second global write should be treated as replacing the first, not as
configuring a second side. this is an engineering inference from the
packet structures, not a claim of having exhaustively ruled out every
internal firmware path.

**conclusion: this workaround is ruled out as a product feature**, though
it remains an open question at the firmware level.

### two tempting but invalid shortcuts

battery records identify right, left and case components with codes
`0x02`, `0x04` and `0x08`. these classify entries inside battery reports;
they are not demonstrated command addresses.
[MagicPods' battery types][mp-battery] and
[report parser][mp-battery-watch] show their actual use, and reusing them
as a side selector for a control write would be unfounded.

active/primary role does not mean a permanently fixed physical side.
MagicPods' [H2 behaviour notes][mp-h2] describe an active bud commonly
determined by removal order. any future experiment must label physical
left/right independently of primary/secondary role, and must repeat with
the opposite bud removed first.

**conclusion: both shortcuts are ruled out as evidence for a side
selector.**

### transparency customisation

LibrePods includes left/right values in its
[transparency customisation representation][lp-transparency], which is
useful evidence for targeted accessibility parameters, though the
structure still has a global enabled state. Apple documents that
customisation for AirPods Pro in its
[listening-mode guide][apple-pro-transparency]. neither source establishes
that AirPods 4 ANC supports those parameters, and neither establishes
different simultaneous modes per side.

**conclusion: inconclusive.** this is the most promising open lead for
future differential capture work, not a usable command today.

## what was examined: ear detection, case behaviour and power

### keep the concepts separate

power, connection, media routing and sensor state are distinct concepts.
Apple describes automatic ear detection as controlling playback and
routing, and states that audio can continue with detection disabled. its
microphone preference explicitly selects a side without switching off the
other bud. [Apple's settings documentation][apple-settings] supports these
distinctions.

### overriding ear state is a host behaviour, not a device command

LibrePods' [ear-detection implementation][lp-ear] has an override used by
its [Linux integration][lp-main]. it changes application behaviour; it is
not evidence of an accessory command that falsifies the optical sensor.
editing auris's reported ear state would similarly alter presentation or
future host automation, without establishing control over the bud's power
circuitry.

**conclusion: ruled out as a power-control mechanism.** it remains valid
as a host-side presentation override, clearly labelled as such.

### the case is physical, not remote

Apple states that AirPods shut down and charge in a closed case, in its
[charging guide][apple-charging]. that physical mechanism does not imply
an equivalent wireless shutdown command, and charging also depends on
available case power, so placement alone must not be displayed as
confirmed charging.

no manufacturer-published fixed "60 seconds after closing" telemetry
timeout was identified in these sources. whether a specific timeout
applies is a measurement question for the particular model and firmware,
not a documented Apple constant. a disappearance from auris's reports must
not be used to infer when every radio or processor powered down.

**conclusion: no evidence of a remote per-bud shutdown command; the
closed-case behaviour is physical and out of scope for a protocol
feature.**

### what a real power feature would require

observing silence alone is not sufficient evidence of a power-off. a
credible power feature would need: independent evidence of a reduced power
state, continued operation of the other bud, a deterministic wake
procedure, and successful repetition after primary-role changes. battery
data becoming stale, a disconnected control socket, or a stopped audio
stream satisfies none of those on its own. without those measurements,
even an experimental UI should say "media muted," not "AirPod off."

## recommended path: Linux media routing

### host audio controls are not device commands

Linux can manipulate audio channels before encoding Bluetooth media.
BlueZ's documented [MediaTransport volume][bluez-transport] is a scalar
`0-127`, not a left/right power or amplifier control. PulseAudio's
[multichannel volume representation][pa-volume] stores per-channel values,
and [`pactl` exposes per-channel sink volumes][pactl]. these are host-audio
controls; their existence does not establish a bud-targeted AACP command.

directly setting the AirPods sink balance is a possible short diagnostic
but a poor initial product architecture: it shares state with other volume
tools, requires careful restoration, and can interact with hardware-volume
behaviour. PulseAudio documents that
[balance transformations need not be reversible][pa-ui]; auris should not
assume that setting balance back to zero reconstructs the original channel
gains.

### proposed signal path

use an opt-in, session-scoped auris-owned virtual stereo sink/filter.
selected application streams feed it, and its processed output targets the
exact AirPods A2DP node. PipeWire documents virtual sinks, channel
remapping and target selection in its [loopback module][pw-loopback], and
gain-controlled input mixing in its [filter-chain module][pw-filter].
those are suitable building blocks, with no invented AirPods packets.

the filter should always expose two explicitly mapped channels, `FL` and
`FR`, and should initially accept only a verified stereo A2DP target. it
must not silently fall back to speakers if the buds disconnect.
WirePlumber's [linking policy documentation][wp-linking] describes target
selection and fallback behaviour; the implementation must use and verify
the appropriate no-fallback and lifecycle properties rather than rely on
default policy.

rows are output left and right; columns are input left and right. these
are DSP design choices, not AirPods-protocol findings.

| mode | output left | output right | trade-off |
|---|---|---|---|
| stereo | `L` | `R` | original stereo signal |
| left, original channel | `L` | `0` | loses content exclusive to source right |
| right, original channel | `0` | `R` | loses content exclusive to source left |
| left, both channels mixed | `0.5L + 0.5R` | `0` | both contributions in one ear; no stereo image |
| right, both channels mixed | `0` | `0.5L + 0.5R` | both contributions in one ear; no stereo image |

an arithmetic average is a safe starting point for bounded full-scale
samples: it does not exceed either channel's common peak limit when the
inputs are in range. using `0.707L + 0.707R` without added headroom can
clip correlated full-scale material by approximately 3 dB. opposite-phase
material can cancel in any simple mono sum, so "mix both channels" must
not promise preservation of all audible content. mode changes should ramp
gains over a short, tested interval to avoid clicks.

a zeroed channel does not disable the bud's radio, microphones, optical
sensor, anc processor or ambient transparency sound. device-generated
chimes and other local audio are outside the host media filter.
applications routed directly to the physical sink bypass it. these are
boundaries of the proposed signal path, not implementation bugs to
disguise with a broader "disable AirPod" label.

AirPods may change mono/stereo handling when a bud is removed, cased or
absent. a correct two-channel PCM buffer proves only the filter's output,
not which physical transducer ultimately renders it. Apple's
[one-sided audio troubleshooting][apple-balance] uses channel balance but
does not guarantee Linux channel identity across all those transitions.
unknown or unsupported layouts should disable the feature with a clear
explanation, rather than guess.

this should be a reversible runtime graph, not an edit to permanent system
audio configuration.

- own and tag only its own nodes and links, with a session generation
  identifier.
- resolve targets using stable device identity plus the current profile
  and channel map. do not persist transient PipeWire object IDs. PipeWire's
  [audio and stream properties][pw-properties] describe the explicit
  channel mapping and remix behaviour to configure and inspect.
- route only explicitly selected application streams initially. taking
  over the desktop's default output is a separate user choice with broader
  restoration requirements.
- on disable or failure, restore a route only if it still matches the
  state auris installed; later user changes must win.
- never delete another client's nodes, and never force an old speaker or
  default selection after the user changed it.
- if the target is lost, leave owned output unlinked and silent rather
  than exposing it to an unintended fallback device.

a microphone application may move the connection away from stereo A2DP.
WirePlumber documents [Bluetooth node/profile behaviour][wp-bluetooth] and
[automatic profile switching/settings][wp-settings]. version one should
suspend side-specific routing for mono or unknown layouts, revalidate
after A2DP returns, and must not force a profile switch or interrupt a
call.

### verification required before enabling media routing

unit tests should cover every matrix entry, silence on the non-target
channel, maximum correlated input, opposing-phase input, mode-change
ramps, and invalid or unknown channel maps. integration tests should use a
disposable PipeWire environment or mock graph first, and must prove exact
target selection, ownership and cleanup before any live session.

| dimension | required cases |
|---|---|
| bud configuration | both worn; both out; left alone; right alone; each side cased; opposite first-removal order |
| audio input | left-only tone; right-only tone; identical channels; opposing-phase signal; ordinary stereo media |
| profiles | each available stereo A2DP codec; HFP transition; return to A2DP; unknown channel map |
| device lifecycle | connect/disconnect, target recreation, primary-role changes, helper crash and repeated enable/disable |
| user/application actions | volume/default changes during use; manual rerouting; new streams; an application pinned to a bypass route |

acceptance requires measuring both the filter output and the physical bud
output with low-level test signals, and requires all of: no audible
non-target host media in the tested states, no unexpected speaker
fallback, no stolen user routing changes, and no stranded streams after
cleanup. the UI must explicitly limit its claim to validated profiles and
layouts.

## what continued reverse engineering would need

original [MagicPairing research][magicpairing] and
[ToothPicker research][toothpicker] are useful transport and methodology
references. they predate H2 and AirPods 4 ANC, and do not establish a
modern per-bud power or mixed-mode command; vulnerability research from
that era is not a reliable or appropriate basis for a daily-use feature on
current firmware.

a narrow, evidence-gated experiment on the open leads above (per-side
transparency customisation, mixed anc/transparency, per-bud power) would
need, in order:

1. **baseline.** record exact model, firmware, battery state, available
   stock Apple settings, profile/codec and first-removed bud. establish a
   known normal-recovery procedure first.
2. **observe stock transitions.** compare both buds, each alone,
   open/closed case, ear-detection enabled/disabled and both removal
   orders. record audible behaviour separately from connection, sensor and
   battery telemetry.
3. **capture passively.** collect host-side HCI/AACP traffic while known
   native controls are used. such captures may not expose private
   inter-bud traffic; a missing host packet does not prove absence of
   internal behaviour.
4. **compare against known controls.** use traces of the global
   listening-mode change and the one-bud anc permission as a baseline.
   keep primary/secondary labels separate from physical left/right until
   mapping is demonstrated across role changes.
5. **require a meaningful new lead.** a byte position that merely
   resembles a battery-side code is not sufficient justification for a
   write.
6. **prepare offline safeguards first.** add parser fixtures, exact-length
   and value validation, model/firmware gates, finite timeouts and
   explicit restoration.
7. **measure the claimed effect.** mixed modes need separate acoustic
   evidence at each bud. a power-off claim needs independent power-state
   evidence plus reliable wake. repeat across connection and removal order,
   and check that the other bud is unaffected.

brute-forcing opcodes, replaying unknown initialisation or calibration
writes, spoofing charging contacts or magnets, and interpreting a crash as
useful control are all out of scope: discovery without a reliable recovery
path is not a feature. if no side-selecting mechanism emerges, the
mixed-mode and power work should stay parked as unresolved rather than
repeatedly mutating a global command.

## recommended development order

1. **one-bud anc permission.** the smallest known protocol addition:
   validate `0x1B` on the target firmware, distinguish requested versus
   confirmed state, and put the permission under one-time setup while
   continuing to select anc through the existing quick control. do not add
   misleading left/right anc switches.
2. **per-ear media routing prototype.** a larger audio integration, and a
   credible way to meet the practical "only hear media in one bud"
   requirement. keep it opt-in until the physical channel and lifecycle
   tests pass; a compact stereo/left/right quick control would fit the
   redesigned panel once supported.
3. **passive differential AACP research.** pursue only with a defined
   question, controlled captures and a repeatable baseline; it can run
   independently of the media-filter implementation.
4. **per-bud power and mixed-mode implementation.** do not schedule as a
   deliverable until a real mechanism passes the evidence gate above; its
   scope and completion time cannot responsibly be estimated at present.

this order delivers the tractable experience first. it does not conflate
software silence, sensor policy, microphone selection and hardware power,
and it leaves room for stronger native control if future evidence
supports it.

## sources

LibrePods code links are pinned to commit
`53679cc90222e94ade84e66542d97ace2540e626`. MagicPodsCore links are pinned
to `11422817ed7080ac6ec8f02c84dfefa8d1f773dd`. Linux audio documentation
describes API capability; it is not evidence of device-level hardware
testing.

- Apple: [AirPods 4 technical specifications][apple-specs]; [listening settings][apple-listening]; [AirPods settings][apple-settings]; [charging guide][apple-charging]; [Pro listening/customisation guide][apple-pro-transparency]; [one-sided audio troubleshooting][apple-balance].
- LibrePods contributors: [control command catalog][lp-commands]; [Linux packets][lp-packets]; [ear-detection implementation][lp-ear]; [Linux integration][lp-main]; [transparency data representation][lp-transparency].
- MagicPodsCore contributors: [model identifiers][mp-model]; [anc setter][mp-anc]; [anc watcher][mp-anc-watch]; [one-bud anc capability][mp-one-capability]; [battery types][mp-battery]; [battery parser][mp-battery-watch]. MagicPods: [H2 behaviour notes][mp-h2].
- BlueZ: [MediaTransport API][bluez-transport]. PulseAudio: [volume API][pa-volume]; [multichannel volume UI guidance][pa-ui]; [`pactl` manual, Arch Linux package documentation][pactl].
- PipeWire: [loopback module][pw-loopback]; [filter-chain module][pw-filter]; [stream/audio properties][pw-properties]. WirePlumber: [linking policy][wp-linking]; [Bluetooth configuration][wp-bluetooth]; [runtime settings][wp-settings].
- Heinze, Classen and Rohrbach: [MagicPairing][magicpairing]. Heinze et al.: [ToothPicker, WOOT][toothpicker].

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
