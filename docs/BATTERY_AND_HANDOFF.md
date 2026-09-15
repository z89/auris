# battery and handoff

aurisd reads AirPods battery telemetry over the Apple Accessory Protocol (AAP)
and, optionally, from encrypted Bluetooth LE adverts. it also takes part in the
AirPods' own multi-host ownership protocol, which moves the buds between this
Linux host, a Mac and an iPhone. the reference device is AirPods 4 ANC, product
id `201B`.

## battery telemetry and freshness

battery state is tracked per cell: left bud, right bud and case. each cell
carries its own `source`, `fresh`, `present`, `last_seen` and
`last_known_charging`.

the freshness rules are:

- an AAP socket opening does not refresh old readings.
- an absent reading preserves history. current charging becomes false. the panel
  can still show its muted green bolt and a "last seen charging ... ago"
  caption.
- exact zero is a real level, not unknown.
- a report cannot refresh an unreported cell just because another cell changed.

battery caches are private and address-bound.

- startup restores only a matching pinned device's cache.
- old unbound caches are ignored, not deleted. new measurements replace them.
- without a pinned address, retained runtime history still works. startup simply
  does not guess which device owned an old cache.

per-cell tracking is what lets the interface separate a level that is current
from one that is only remembered. it also lets a second source be added without
changing any display rule, because a source writes only the cells it measured.

## BLE observation and its key requirement

the BLE observer is optional and off by default. it resolves the AirPods'
rotating addresses with the pinned device's identity resolving key, decrypts
that device's proximity battery block, and updates only the cells it measured.
it fills battery gaps while no audio link exists. a nearby BLE reading never
makes the status bar claim connected audio.

### the key requirement

observation needs two secrets held by the device owner: the IRK that resolves
the rotating address, and the AES key that decrypts the battery block. auris
does not acquire or import either from another application.

config lives in `~/.config/aurisd/config.toml`, with the owned, paired classic
address pinned:

```toml
device = "AC:DE:48:00:11:22" # example only: replace with your device

[ble]
enabled = false
key_file = "/absolute/private/path/keys.json"
scan_seconds = 8
interval_seconds = 30
freshness_seconds = 75
```

the key file must be a regular file owned by the daemon's user, mode `0600` with
no group or other access, at most 4096 bytes, and not a symlink. it contains
exactly:

```json
{
  "address": "AC:DE:48:00:11:22",
  "irk": "<32 hexadecimal digits: AAP-export byte order>",
  "encryption_key": "<32 hexadecimal digits: direct AES key order>"
}
```

those two values are placeholders, not valid keys. keys belong in that file
alone, never in a commit, a log or an issue.

constraints on the keys:

- the classic identity must match the pinned address.
- IRK matching uses Bluetooth's short address hash. it is **not authenticated
  encryption**. an advert must never authorise a connection or a security
  decision.
- a wrong encryption key can produce plausible numbers. observed percentages
  should be compared against AAP readings across bud, case and charging states
  before the output is trusted.
- AES comes from the [RustCrypto aes crate](https://docs.rs/aes/0.8.4/aes/), not
  from a local cipher implementation.
- byte order follows [LibrePods BLE crypto
  handling](https://github.com/librepods-org/librepods/blob/53679cc90222e94ade84e66542d97ace2540e626/linux/ble/bleutils.cpp).

### supported advert layout

one 27-byte Apple manufacturer `0x004c` record, type `0x07`, model bytes
`1b 20`, paired format `01`.

- the last 16 bytes are one AES block.
- battery bytes 1-3 use seven-bit percentages with a charging high bit.
- primary-side bit `0x20` determines left and right order.
- case data is used only when the reporting-bud-in-case bit `0x40` is set.

other layouts are ignored. public coarse percentages, inferred ear and lid
states, and remote-host "free" flags are not used.

the layout follows the [LibrePods proximity
parser](https://github.com/librepods-org/librepods/blob/53679cc90222e94ade84e66542d97ace2540e626/linux/ble/blemanager.cpp)
and its [encrypted battery
layout](https://github.com/librepods-org/librepods/blob/53679cc90222e94ade84e66542d97ace2540e626/linux/battery.hpp).
the auris implementation is written independently.

### source precedence

- unknown `127` values do not reset age.
- a freshly reporting AAP cell wins for 15 seconds. that stops the source
  alternating on duplicate adverts. afterwards a new exact BLE reading may
  replace it.
- AAP absence does not erase a fresh BLE reading.
- losing the AAP link invalidates AAP cells. it does not invalidate
  independently observed BLE cells.

### discovery behaviour

- discovery is LE-only, for eight seconds per thirty-second window by default.
- it may transmit scan requests and affect a busy or older adapter. physically
  passive scanning is not promised.
- only this client's scan token is released when a window ends. auris does not
  stop another application's discovery.
- BlueZ's cached manufacturer data is ignored. a subsequent manufacturer-data
  event is required to refresh an observation.
- a BLE reading becomes historical after 75 seconds without a new cell
  measurement. **that is the auris freshness policy, not the Apple case sleep
  timeout.** silent hardware cannot provide fresh data.

[BlueZ discovery semantics](https://bluez.readthedocs.io/en/latest/adapter-api/)
and the [bluer discovery
API](https://docs.rs/bluer/0.17.4/bluer/struct.Adapter.html#method.discover_devices)
describe shared discovery and initially known devices.

the observer extends battery reporting into the periods when no host holds an
audio link. it cannot become a default, because the keys it needs exist only on
a device the owner already controls, and no provisioning path for them exists on
Linux.

## Apple multi-host switching

opt-in: `[handoff] enabled = true`, or `auris handoff on`.

auris takes part in the AirPods' own ownership protocol over the AAP channel it
already holds, L2CAP PSM `0x1001`. that is the mechanism LibrePods uses on
Android. auris does not race a Mac or an iPhone for the audio link.

Apple hosts cooperate only with a host whose Bluetooth Device ID names Apple.
the adapter must advertise `DeviceID = bluetooth:004C:0000:0000`, and the
AirPods must be paired again afterwards. `handoff.apple_host_id` in state.json
reports the adapter's `Modalias`, and is `null` when BlueZ exposes none.

### opcodes

addresses marked *reversed* are sent least significant byte first. payload
offsets are counted after the six-byte `04 00 04 00 <opcode LE>` header.

| opcode | direction | payload | auris use |
|---|---|---|---|
| `0x000C` | AirPods to host | address (reversed), 2 unknown bytes | decoded and logged only |
| `0x000E` audio source | AirPods to host | address (reversed), status `00` idle / `01` call / `02` media | `handoff.audio_source`; another host at `01` or `02` triggers a yield |
| `0x002E` connected devices | AirPods to host | 2 unknown bytes, count, then per host address (**not** reversed) + 2 info bytes | `handoff.devices`; take-over targets; new hosts get media info + `newTipi` |
| `0x0010` smart routing | host to AirPods | target address (reversed), u16 LE body length, `01`, OPACK dictionary | media info, `Hijackv2`, `newTipi` |
| `0x0011` smart routing relay | AirPods to host | sender address (reversed), u16 LE length, OPACK body | `audioRoutingSetOwnershipToFalse` in the body triggers a yield |
| `0x0009` control id `0x06` | both | `01` owns connection, `00` does not | `handoff.owner`; `00` on an established link triggers a yield |

decoder fixtures for AirPods 4 ANC (`201B`) live in `daemon/src/aap/codec.rs`
tests. they include the byte-order check that the same host decodes identically
from `0x000E` (reversed) and `0x002E` (forward).

### the `0x002E` info bytes

each listed host carries two info bytes after its address. byte 0 is that host's
own link state to the AirPods.

| byte 0 | meaning |
|---|---|
| `0x00` | listed, link down |
| `0x01` | connecting |
| `0x02` | link up |

a host stays on the list for a while after it disconnects, so membership of the
list does not mean connected. byte 0 is the part that says which. it reads
`0x00` on the reports that follow a remote host's disconnect. it reads `0x01`
for a few seconds before that host's link comes up, and `0x02` from the moment
it is up. a host reconnecting moves from `0x01` to `0x02` within about 80 ms.

byte 1 varies independently, `0x15` or `0x17` for a Mac and `0x01` or `0x03` for
this host. its meaning is unknown and auris does not read it. the second byte of
the report header is likewise unexplained: it reads `00` before an Apple host
has relayed a smart-routing message and `02` afterwards.

byte 0 is the evidence the rejoin classifier uses to tell "another host holds
the AirPods" from "another host is merely remembered". without it, a stale list
entry would qualify every disconnect.

## what each host observes during a switch

the two sides of a switch see different halves of the same event, in a fixed
order.

a claim by another Apple host:

1. the other host compares the audio category score this host published against
   its own, and reads the AirPods' advertised stream state.
2. it sends a smart-routing message that reaches this host as an `0x0011` relay
   containing `audioRoutingSetOwnershipToFalse`.
3. this host yields.
4. the AirPods send control `06 = 00` toward this host and report the new audio
   source over `0x000E`.
5. the other host routes its audio to the AirPods, and the AirPods tear down
   this host's A2DP stream.

a claim by this host:

1. this host sends `06 = 01`, then media information, then `Hijackv2` to every
   other listed host.
2. the Apple host runs an ownership check on the name and model in that media
   information.
3. it gives up ownership, shows a banner naming the taking host, and routes its
   own audio to its speakers. macOS calls that step `RouteToSpeaker`.
4. the AirPods report the audio source as idle, then as this host once a stream
   flows.

the acceptance rules on the Apple side are visible in its own behaviour:

- it compares the audio category score both hosts published. with the scores
  equal, the host that is actually streaming keeps the AirPods.
- it reads the AirPods' advertised stream state. an owner that has not started a
  stream is hijacked back within about ten seconds.
- it checks the other host's reported name and model before relinquishing.
  `btName` values `Mac` and `iPhone` are accepted.

ownership and the audio stream are separate mechanisms. any host that starts an
A2DP stream takes the AirPods' audio, whether it owns them or not. a short
notification sound played here while another host owns the AirPods acquires the
local transport and interrupts that host's playback. yielding therefore has to
release the local audio route as well as ownership.

connection order also matters. a Mac connecting fresh to AirPods that already
hold this host's link makes the AirPods drop that link within about half a
second. the Mac sends no disconnect of its own; it records
`IsHeadphoneEligibleForTipiV2: Skip reason: ConnectedSourceDiffiCloud`, because
the Linux host is not on the same iCloud account. connecting this host second
keeps both hosts, and handoff then works in both directions. the rejoin path
below exists to restore the first case.

## how auris yields

reports are decoded into `handoff` state even while handoff is disabled. only
acting on them is gated.

### the opening window

every AAP link opens with a state dump. that dump can name a host which stopped
playing long ago, so it must not trigger anything.

- audio-source, ownership and connected-device reports update `handoff` during
  the window and do nothing else.
- the window runs from link up until the later of 3 s and the end of the
  settings dump, which is the first battery packet or the readback timeout. it
  is capped at 10 s.
- reports inside it never yield, pause, hold or introduce a host.
- a relayed ownership-to-false request is a live message and still counts.

### what triggers a yield

a yield needs a real request. any of these qualify:

- a relayed smart-routing message carrying `audioRoutingSetOwnershipToFalse`
  (`Hijackv2`).
- control `06 = 00` from the AirPods after the opening window.
- an audio-source report after the window that changes the source to another
  host at call or media. a repeat of the same snapshot is not a change.

on a yield auris pauses local MPRIS players that are Playing, releases the audio
route, and only then sends `06 = 00`, unless the AirPods sent it first. the full
order is under the PipeWire audio route below. the A2DP and HFP profiles stay
connected. the AirPods route by ownership, and a paused player sends nothing. a
further request while already yielded renews the hold.

profiles are left connected deliberately. `DisconnectProfile` on yield does not
stick: the profile returns about ten seconds later from the BlueZ and audio
server side, and its return moves the stream back and flips the reported source.
LibrePods on Android holds A2DP down with a per-device connection policy. Linux
has no equivalent auris could set, short of editing the user's WirePlumber
policy or blocking the device outright. Apple hosts do not disconnect either.
ownership plus a paused player is the whole mechanism.

### the yielded hold

after any yield, automatic or from `auris yield`, a local Paused to Playing edge
counts as a player resuming itself when it lands:

- within 2.5 s of the auris pause, or of the peer's latest request, or
- within 2.5 s of a BlueZ `Connected` change, an audio profile endpoint change,
  a `MediaTransport1` change, or an audio-source report naming this host.

such a player is paused again by its MPRIS bus name. auris does not take it
over. an audio-source report naming this host as media while held also pauses
the local player, unless a play edge is still pending.

each player is paused again at most once per request. the same player resuming
again before a new request is a user pressing play, and goes to the debounced
take-over path.

both windows are 2.5 s. the audio-change window cannot be wider, because the
PipeWire profile follows ownership: every yield switches the card profile off
and fires an audio-change event at once. a 5 s window there swallows a user
press four seconds after a yield. a self-resume after a sink change lands about
0.5 s after it, so 2.5 s covers the case it is there for.

the hold ends on take-over, on a user play, or when the AAP link resets. it also
ends on an audio-source report that is idle or names this host, when that report
arrives 10 s or more after the latest request. a re-pause leaves `last_event`
unchanged.

the hold exists because players restart themselves when their sink disappears,
and a yield always removes their sink. it is bounded on both sides so that a
deliberate press is never mistaken for that restart.

## how auris takes over

take-over needs `enabled` and `take_over_on_play`, default true. it needs a
local MPRIS Paused to Playing edge that:

- stays Playing for 1.5 s
- is not the first reading since start or link open
- is outside the hold windows
- happens while another host owns the AirPods, or while auris released its audio
- does not happen while the last audio-source report shows another host in a
  call

such a play ends the hold even when take-over is not allowed.

the sequence:

1. restore the PipeWire BlueZ card profile, before any AAP write.
2. send `06 = 01`.
3. send media information: `HostStreamingState YES`, `PlayingApp` from the
   player's MPRIS `Identity`, `btAddress` of the local adapter, and `btName`
   `Mac` unless `bt_name` is set.
4. send `Hijackv2` to every other listed host.
5. call `Device1.ConnectProfile` for A2DP sink. already connected counts as
   success.

a connect refused as busy (`br-connection-busy`) or in progress is retried after
700 ms, 1.5 s and 3 s, then abandoned with a warning. no retry runs once this
host no longer owns the AirPods.

`btName` defaults to `Mac` because an Apple host's ownership check is known to
accept it. the check reads
`_shouldAllowRelinquishOwnership _myModel Mac otherTipiName Mac` and answers
`YES`, and it answers the same for `iPhone`. no other value has been observed to
work, so a Linux-looking name may be refused.

### HostStreamingState

- the take-over sends `YES`.
- when the A2DP transport then becomes active, auris sends `YES` once more, as
  one extra media information message with no second `Hijackv2`.
- `NO` is sent only after local playback has stayed stopped for 3 s. it is never
  sent within 8 s of a take-over while the stream is still starting.
- a local play while owning, after a `NO`, sends `YES` again once audio flows.

an early or stalled `NO` invites the other host to take the AirPods straight
back, which is what these three rules prevent.

### stream start after take-over

auris watches `MediaTransport1.State` for the AirPods' A2DP transport. the check
runs 3 s after a take-over, while a local player is playing or stopped less than
3 s ago. if the state is neither `pending` nor `active`, auris logs
`A2DP transport still idle 3 s after take-over`, naming the transport BlueZ
reports.

that warning is a diagnostic and takes no action. two remedies were tried in its
place and both are gone:

- cycling the A2DP profile, `DisconnectProfile` then `ConnectProfile`, removes
  the transport and takes the whole link with it. BlueZ then fails to reload the
  remote SEP (`Unable to load LastUsed: rseid 2 not found`), and WirePlumber can
  settle on the hands-free profile, which is audible as a drop in quality.
- nudging the player over MPRIS, `Pause` then `Play` 300 ms later, touches no
  profile and churns no link. it does not make the audio server re-acquire a
  transport that PipeWire never released.

the remedy that works is the audio route below, which creates a fresh PipeWire
node instead of trying to wake a failed one.

`auris take-over` and `auris yield` run the same steps at once. both need an
open AAP link, and both work while automatic handoff is disabled.

## reconnect and rejoin classification

### one attempt, then stop

on a newly observed local Bluetooth connection, auris makes one AAP attempt
after an 800 ms settle delay. it re-checks paired and `Connected` state
immediately before dialling. an observed link or identity change invalidates
in-flight opening work.

peer loss, a failed send and an unanswered watchdog each stop automatic
recovery. there is no exponential redial loop that can repeatedly compete with a
phone.

### the two explicit commands

- `auris reconnect` retries only the settings and telemetry link, while the
  device is already connected locally.
- `auris connect-once` requests one Bluetooth connection to the pinned paired
  device, with no automatic retry. it can transfer audio. it is an intentional
  user action, not "connect only if all other hosts are idle".

neither command runs from BLE data.

queued control commands are bound to the connection identity and epoch, with a
four-second daemon deadline. an expired command is rejected before action, as is
one whose connection has changed. a timeout cannot undo a Bluetooth connection
request already sent to BlueZ, so the actual link state has to be read before a
retry.

missing and malformed configuration are distinguished. a missing file uses
defaults. malformed configuration stops startup rather than silently dropping a
pin.

### auto-connect on case open

`[autoconnect]`, on by default.

AirPods leaving their case page the host they last used, and only that host. a
Mac in the room does not wait to be paged. it listens for the AirPods' own BLE
proximity-pairing advert and pages them itself, so it wins whenever the AirPods
choose someone else. BlueZ never pages a classic device on its own, which is why
this host can sit disconnected for minutes while the Mac holds them.

`autoconnect.rs` watches for that same advert and pages once per case opening.
discovery is LE only, with `duplicate_data` on and `discoverable` off. it runs
only while the AirPods are not connected locally. a local connection stops it,
and a disconnect starts it again.

an advert qualifies when all of these hold:

- Apple's company id `0x004C` carries proximity-pairing type `0x07`.
- the non-pairing-mode byte is `0x01`.
- the watched accessory's model id is present in little-endian order. for the
  AirPods 4 ANC product id `201B` that is `1B 20`, taken from the DID when
  known.

cached `ManufacturerData` is never read on discovery, for the same reason the
battery observer ignores it: BlueZ replays devices that are long gone.

a presence episode begins with a qualifying advert that follows at least
`absence_seconds` (default 15) of silence. in practice that means the case was
shut and opened again. one episode gets at most one connect sequence:

- a first `Device1.Connect` after `settle_seconds` (default 3). the delay lets a
  nearer host the user prefers take the link first.
- then retries at 5 s, 15 s and 45 s, up to four attempts.
- only while an advert has been heard in the last 10 s.

adverts weaker than `min_rssi` (default -90 dBm) are treated as another room.
they do not even count as presence.

three things disarm the trigger, each logged with its reason:

- `auris reconnect`, `auris connect-once`, or any `Device1.Disconnected` with
  reason `Local`, until the next absence. aurisd never calls `Disconnect`
  itself, so a `Local` reason is the panel, `bluetoothctl` or the user deciding
  where the AirPods belong, and that decision has to stick.
- a pending rejoin. the rejoin state machine owns eviction recovery, and this
  path never pages over it. a `Remote` or `Timeout` drop therefore starts no
  episode of its own. the drop starts the absence clock, and AirPods still out
  of the case keep advertising, so no episode begins until they go away. the
  drop does start the page fallback.
- `enabled = false`, which also means no discovery at all.

### page fallback when no advert arrives

the advert trigger is not reliable on every adapter. some dongles produce no
qualifying advert for a whole case cycle, and nothing pages. this host therefore
pages on a slow cadence when all of these hold:

- the AirPods are disconnected locally.
- the machine is still armed: no connect command and no `Local` disconnect since
  the last absence.
- the last disconnect was `Remote` or `Timeout`, or the daemon started
  disconnected.
- no rejoin and no advert episode is in flight.

| setting | default | meaning |
|---|---|---|
| `fallback_first_seconds` | 20 | delay from the drop to the first page |
| `fallback_interval_seconds` | 45 | gap between later pages |
| `fallback_minutes` | 10 | total budget measured from the drop |

after the budget, only the advert trigger remains until the next drop.
`fallback_minutes = 0` turns the fallback off.

each fallback page is one `Device1.Connect`, with no retry backoff. a page
timeout waits for the next slot, logged as
`fallback page did not take; waiting for the next slot`. the fallback for an
away period ends on a connect that succeeds, or on one that finds the link
already taken (`connected_by_other_means`). the pages are logged as
`no proximity advert since the disconnect; paging anyway`, with `away` and
`next`.

an advert episode always takes priority and does not move the fallback clock. a
failed episode therefore leaves the slow cadence to carry on inside the budget.

both paths use the same guarded `bluez::rejoin_connect` as the rejoin path
below. it re-reads adapter power, paired, blocked and connected state before
calling `Device1.Connect` once. it is issued from the supervisor, so BlueZ never
sees two sources of connect requests. logs are under the `aurisd::autoconnect`
target.

### when a rejoin is allowed

BlueZ `Device1.Disconnected(reason, message)` is logged at INFO on every
disconnect. auris calls `Device1.Connect` only when all eight of the following
hold as that signal arrives.

**1. enabled.** `[handoff] enabled` and `rejoin_after_eviction`, default true.

**2. the reason qualifies.** only `org.bluez.Reason.Remote`, the AirPods
dropping this host, or `org.bluez.Reason.Timeout`, the link supervision timer
expiring.

there is no rejoin for `Local` (panel, `bluetoothctl`, auris), `Authentication`,
`Suspend`, `Unknown`, any other name, or no signal at all. a manual disconnect
is always `Local`, so it can never reach a rejoin. ECONNRESET alone never
counts.

**3. the evidence report qualifies.** the latest `0x002E` report of the link
session that just ended lists a host other than this one, and one of three
conditions holds.

| match kind | condition | what it describes |
|---|---|---|
| `joined` | the report arrived at most 1.5 s before the `Disconnected` signal | an Apple host connecting and pushing this host off. it matches even when that host's info byte reads `0x01` |
| `connected` | the listed Apple host's info byte 0 is `0x02`, at any report age | an Apple host that already held its own link and evicted this host anyway |
| `link_lost` | the reason is `Timeout` and the listed Apple host's info byte 0 is `0x02`, at any report age | this host's link died while the Apple host kept its own |

the window for `joined` is measured at the signal, because that is when the
decision is made and the signal is required anyway. the AAP reset arrives tens
of milliseconds earlier.

`connected` and `link_lost` have no age cap. the AirPods send `0x002E` only on a
change, so the newest report can be a minute old while nothing has changed.
session scoping is what bounds them instead. the `joined` window never qualifies
a `Timeout`: a report that arrives with the drop says nothing about who kept the
AirPods, and only the link-up byte does.

scoping rules:

- only the latest report counts. a later report without that host is no
  evidence.
- a report is used for one rejoin sequence at most.
- a report is forgotten when a new link session starts, so a report from an
  earlier session never qualifies a later drop.
- audio-source and ownership reports are not used. `0x000E` can name a host that
  was already connected, and control `0x06` carries no address.

`rejoin scheduled` logs which condition matched, as `match_kind="joined"`,
`match_kind="connected"` or `match_kind="link_lost"`.

**4. that listed host is an Apple host.** recognised either way:

- **OUI.** the first three octets of its address are registered to exactly
  `Apple, Inc.`, trimmed and ASCII case ignored. Macs and iPhones use their
  public BR/EDR address for classic Bluetooth, and the `0x002E` list carries it.
  at start the daemon reads `/usr/share/hwdata/oui.txt` (package `hwdata`), else
  `/usr/lib/udev/hwdb.d/20-OUI.hwdb` (systemd), else a built-in list of nine
  Apple OUIs copied from `oui.txt`. it logs
  `Apple OUIs loaded count=... source=...` once. a locally administered address,
  bit `0x02` of the first octet, never matches.
- **relay.** the host has sent this one a smart-routing relay (`0x0011`). up to
  8 addresses persist in `$CACHE_DIRECTORY/apple_hosts.json`, or
  `~/.cache/aurisd/apple_hosts.json`. this covers an Apple host behind a
  non-Apple OUI. it is not required.

**5. the buds are not put away.** the last ear report did not show both buds in
the case, and the lid is not known closed.

both buds in the case means put away. one bud in the case charging, while the
other is in an ear or merely out of it, is a bud swap and never blocks a rejoin.
the ear report carries a state per bud: `0x00` in ear, `0x01` out, `0x02` case.
only `0x02` for both counts.

**6. the adapter and device are usable.** the adapter is powered, and the
AirPods are paired and not blocked.

`Trusted` is not required. pairing through `bluetoothctl` or some panels leaves
it unset, and it only governs whether BlueZ accepts connections the AirPods
start. auris starts this connect itself, to a device the user paired. `Blocked`
still means never.

**7. no recent manual command.** no `auris reconnect` or `auris connect-once` in
the last 10 s.

**8. no eviction loop.** a rejoin sequence started in the last 60 s does not
refuse the new one. it delays it, per the rate limit below.

nothing learned is needed for the first eviction. all of these qualify on the
OUI alone:

- a Mac or iPhone that has never connected to these AirPods.
- a Mac or iPhone that has never talked to this host.
- AirPods paired a minute ago.
- a missing or corrupt `apple_hosts.json`.
- a missing cache directory.

the first `0x002E` report on a link counts, and there is no baseline. a
different AirPods address clears the old report, the ear state and any eviction
loop from the previous device. the 60 s rate limit and the 10 s manual quiet
period still apply.

### the bud swap case

taking a bud out moves the AirPods' radio link from the primary bud to the other
one. an idle host's link does not always survive that move. the AirPods can
write that host's own link off in a report which still arrives over that link.
the drop then lands as a supervision `Timeout`, while the owning host keeps its
link. the same swap sometimes lands as `Remote` instead. `link_lost` covers the
first shape, `connected` the second.

this host's own `info=00` in such a report is recorded as
`own_link_reported_down` and logged. it is never required, because the report
can predate the swap or be missing altogether. nothing acts on it.

a report that marks this host's link down while BlueZ still says connected
changes no state, sends no packet and tears nothing down. the same holds for
every ear-detection change. BlueZ owns the link, and a link that still carries
reports is not auris's to recycle.

### the rate limit waits, it does not drop

at most one rejoin sequence starts per minute. a qualifying eviction inside that
window is scheduled for the moment the window expires, rather than dropped. it
keeps the same evidence, sequence number and attempt budget. it is logged as
`rejoin deferred reason=rate_limited delay=...`, where the delay is the rest of
the window plus the usual 3 s wait.

only the rate limit defers. every other check refuses outright.

a deferred connect is cancelled by exactly what cancels an ordinary one: a
connect command, both buds going into the case, the adapter going away, or
handoff being switched off. while it waits, the published link status stays
`reconnecting`, as it does for any pending sequence.

the 20 s eviction loop is checked first and is unaffected. a second eviction
within 20 s of a successful rejoin stops rejoining altogether, rather than
waiting a minute to page again.

### the connect itself

the connect waits 3 s, then re-reads BlueZ: powered, paired, not blocked, still
disconnected.

the wait is cancelled by:

- `Connected=true` from anything else
- another `Disconnected` signal
- the adapter going away
- handoff being switched off
- a connect command

"the adapter going away" means the adapter object removed, or
`Adapter1.Powered=false`. a D-Bus watch that merely ended is not evidence of it
and cancels nothing.

the wait and the `Device1.Connect` call belong to the session task, not to the
BlueZ watcher, so they survive a watcher rebuild. a connect refused as busy or
in progress is tried again after 3 s and then 6 s, three calls in all. any other
error, a 30 s timeout, or a third busy refusal stops with a warning. auris never
calls `Disconnect`, not even to clear a stuck `br-connection-busy`.

a `Remote` or `Timeout` disconnect within 20 s of a successful rejoin logs
`eviction loop` and turns rejoin off, until a connect command or a daemon
restart. a bud swap that keeps killing the new link therefore cannot loop.

after the connect, the normal new-local-link path reopens AAP. the whole
sequence from the drop to a usable link takes about seven seconds: 3 s of wait,
then the page and profile setup.

### the device watch ends at every disconnect

BlueZ emits `InterfacesRemoved` for the device object when a profile interface
such as `Battery1` or `MediaControl1` goes away at disconnect. `bluer` cancels
the whole per-object subscription on that signal, even though `Device1` and the
adapter remain.

auris therefore reports the stream ending as a watch that ended, and reads
`Adapter1.Powered` before claiming the adapter is gone. it rebuilds the watch
2 s later, without touching a scheduled rejoin, the evidence behind it, or
handoff state. treating that signal as `AdapterGone` cancels a rejoin on a
powered adapter, and leaves the AirPods on the other host until the user acts.

### logging

every decision is logged at INFO as one of `rejoin scheduled`,
`rejoin deferred reason=rate_limited delay=...`, `rejoin skipped reason=...` or
`rejoin cancelled reason=...`.

skip reasons:

- with no report, or a report naming no Apple host:
  `no_recent_device_report_with_apple_host`.
- a report that names an Apple host but is older than the window and shows that
  host's link down: `no_apple_host_link_up`.

every `Remote` and `Timeout` disconnect also logs, at INFO and before the
decision, `disconnect: last AirPods connected-devices report`. it carries
`reason`, `last_report_age_ms`, `apple_host_match` (`relay`, `oui` or `none`),
`apple_host`, `apple_host_link_up`, `own_link_reported_down` and `listed_hosts`,
every other host in that report. it logs even when rejoin is off.

`rejoin scheduled` also carries `apple_host_match` and `match_kind`.
`rejoin deferred` carries both of those and its `delay`. each `0x002E` report is
logged at DEBUG with its header, and every address with its two info bytes:
`devices=AC:DE:48:00:11:22 info=02 02, ...`.

## the PipeWire audio route

while handoff is enabled, auris owns the PipeWire BlueZ card profile,
`bluez_card.<addr>`:

- `off` whenever this host does not own the AirPods
- back to the A2DP profile that was in use, normally `a2dp-sink`, when this host
  owns them or is taking over

the card is found with `pw-dump`. the profile is set with
`pw-cli set-param <id> Profile '{ index: N }'`, with no `save` field.
WirePlumber therefore never persists `off` as the user's chosen profile.

the change runs at four points:

- on an AirPods ownership report, control `0x06`
- at the start of a take-over, before the AAP writes
- on a yield, in the ordered sequence below
- once more, restoring A2DP, when handoff is turned off or the daemon stops

a disconnect touches nothing, because the card disappears with the link.

### the ordered yield sequence

a yield runs five steps in order:

1. pause local MPRIS players.
2. drain: wait until no player reports Playing. the wait is bounded by a 600 ms
   timeout, after which the sequence proceeds anyway.
3. set the card profile to `off`. that releases the A2DP transport from this
   side.
4. wait for the profile switch to land, bounded by a 2 s timeout.
5. send the ownership release on the AAP link.

the order is what the sequence is for. local playback stops rather than being
re-routed to another sink, and ownership is never released while a transport is
still open underneath this host.

both waits are upper bounds. a quiet graph reaches step 5 as soon as the pause
and the profile switch have taken effect.

releasing the transport from this side also keeps the local node healthy. when
the AirPods tear down the transport under a running PipeWire node, that node
goes to error:

```
pw.node: (bluez_output.BC_80_4E_01_02_03.1-37) running -> error (Received error event)
```

the node never recovers on its own. closing the transport before ownership is
released avoids the error entirely, and a fresh node is created on the next
take-over instead of a failed one being reused.

the AirPods sink stays the configured default sink, priority 1010, so PipeWire
moves streams back to it on its own once the node returns.

routing by ownership keeps the local audio graph in agreement with who holds the
AirPods, and removes the failure where this host owns them but stays silent. its
behaviour against a call on the other host is not established.

## in-ear media control

the accessory's ear report carries a state per bud: `0x00` in ear, `0x01` out,
`0x02` case. a bud entering the case counts as leaving an ear. auris acts on
those reports over MPRIS.

a changed reading must hold for 700 ms before anything is sent. that absorbs the
flap bursts a case opening or a primary bud role switch produces.

**pause.** a bud leaving an ear pauses local players. the default rule pauses as
soon as one of two in-ear buds leaves, which is what macOS does.
`pause_on_one_of_two = false` narrows the rule to the last bud, so playback
continues while either bud is still in an ear.

**resume.** a resume needs every one of these:

- auris made the pause.
- the in-ear count is back to at least the count that was lost.
- the return lands within a 5 minute window.
- no user play or pause happened in between.
- audio is actually on the AirPods: the link is up, the card is not yielded, and
  handoff reports this host as owner.

it fires at most once, on the settled return.

config lives under `[ear]`:

| key | default | effect |
|---|---|---|
| `auto_pause` | true | pause local players when a bud leaves an ear |
| `auto_resume` | true | resume after a qualifying return |
| `pause_on_one_of_two` | true | pause on the first of two buds leaving, rather than the last |

the resume conditions are what keep the feature from fighting the user or the
other host. a pause auris did not make is never undone, and a resume never
starts audio that would land on a host which does not own the AirPods.

## safety limits and failure modes

### what the guards cannot promise

- Linux's outbound L2CAP connect can establish an ACL, so a fresh BlueZ check is
  a mitigation, not an atomic ownership guarantee.
- BlueZ's `Connected` describes the **local** connection. RSSI, availability and
  silence cannot prove another host is disconnected.
- other software, or BlueZ policy, can also start a connection.
- auris cannot mute an iPhone or a Mac when those devices lose their audio
  route.
- an adapter power cycle inside the 3 s rejoin wait is caught only through its
  own `Disconnected` signal and the re-read before connecting.
- the rejoin rules cannot tell an eviction from any other AirPods-initiated drop
  while an Apple host is listed with its link up. the case, lid, rate-limit,
  manual-command and eviction-loop checks are what bound that.
- an Apple host using a random or locally administered address does not match
  the OUI rule. a non-Apple host on Apple hardware, such as another Linux
  machine on a Mac, does match it.

see [BlueZ Device1](https://bluez.readthedocs.io/en/latest/device-api/) and the
[Linux L2CAP connection
path](https://github.com/torvalds/linux/blob/master/net/bluetooth/l2cap_core.c).

### magic pairing

Magic Pairing proper is out of reach on Linux. it is the mechanism where every
device on one Apple ID shares the AirPods link keys through iCloud, so the
AirPods connect to a Mac without pairing. those keys live in the iCloud
Keychain, and Linux has no way to obtain or publish them.

the Linux host is paired on its own, and joins the handoff only through the AAP
ownership messages above. an Apple host keeps its own policy either way. a Mac
or an iPhone may still claim the AirPods for a call, or when its own heuristics
decide to, and auris yields when that happens.

### confidence in each part

| behaviour | basis |
|---|---|
| `0x000E`, `0x002E`, `0x0011` layouts; control `0x06` meaning | cross-checked against LibrePods parsers (`AACPManager.kt`), named the same way by the apple-wireshark AACP dissector, and matched by this device's own reports |
| `0x000C` address + 2 bytes | layout confirmed against the dissector; the two bytes are unexplained |
| `0x002E` info byte 0 meanings | read from reports correlated with an Apple host's own connection log |
| `0x002E` info byte 1, second header byte | unexplained; not read |
| `Hijackv2` body | byte-for-byte equal to LibrePods' recorded iPhone encoding |
| `newTipi` and new-device media information | byte-for-byte equal to LibrePods' encoders, with `btName` substituted |
| streaming media information | LibrePods' key and value sequence, re-encoded as valid OPACK. LibrePods omits the `btName` key tag, tags `YES` as a two-byte string and zero-pads to a fixed buffer. unconfirmed against an Apple host |
| yield on another host's audio source, ownership `00` or a relayed request; take-over sequence | confirmed in LibrePods code (`AirPodsService.kt`, `MediaController.kt`), and exercised against a Mac |
| hold windows (2.5 s and 2.5 s), 1.5 s debounce, no take-over from a call, profiles kept connected on yield | auris policy, replayed in `handoff.rs` tests. keeping profiles connected is not confirmed against an Apple host |
| opening window (3 s or the settings dump, at most 10 s) | auris policy, from an opening dump that named an idle Mac as media source |
| one re-pause per player per request; hold ending 10 s after the latest request | auris policy: a deliberate local play is never blocked |
| `btName = Mac` accepted by a Mac's ownership check | confirmed: `_shouldAllowRelinquishOwnership _myModel Mac otherTipiName Mac` answers `YES`, as does `otherTipiName iPhone` |
| `btName = Mac` producing a Mac-style banner on Apple hosts | speculative |
| `HostStreamingState` timing rules | auris policy, replayed in `handoff.rs` |
| audio route following ownership; pause, drain, profile `off`, then ownership release | auris policy. the 600 ms drain and the 2 s profile-switch wait are upper bounds |
| media information sent to every other listed host | auris policy. LibrePods sends to the first host only |

### not yet exercised

- the `connected` and `link_lost` rejoin rules are derived from observed drops.
  neither has fired on hardware on its own.
- the OUI path has not fired on hardware. an Apple host seen for the first time
  has not been captured evicting this one.
- the audio route change has not been exercised against a call on the other
  host, nor against the stalled-stream case the MPRIS nudge failed on.
- whether an Apple host keeps the AirPods while this host's A2DP stays connected
  but idle is not established.
- whether PipeWire's idle transport suspend or resume produces new audio-source
  reports is not established.
