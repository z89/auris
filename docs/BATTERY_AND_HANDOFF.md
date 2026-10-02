# battery and handoff

runtime behaviour of aurisd. the reference device is AirPods 4 ANC, product id `201B`. config keys and defaults are listed in the [daemon readme](../daemon/README.md).

## battery telemetry and freshness

battery is tracked per cell (left, right, case). each cell has its own `source`, `fresh`, `present`, `last_seen` and `last_known_charging`, and a source writes only the cells it measured.

- an AAP socket opening does not refresh old readings.
- an absent reading keeps history and sets current charging false (the panel shows a muted bolt and "last seen charging ... ago").
- exact zero is a real level.
- a report never refreshes a cell it did not report.

caches are private and address-bound. startup restores only the pinned device's cache, and ignores (does not delete) unbound caches until new measurements replace them. without a pin, startup never guesses an old cache's owner.

## BLE observation

off by default. the observer resolves the rotating address with the pinned device's identity resolving key (IRK), decrypts the proximity battery block and updates only the cells it measured, filling gaps while no host holds an audio link. it never makes the status bar claim connected audio. it cannot be a default, because the keys exist only on a device the owner controls and linux has no provisioning path.

### keys

auris does not acquire or import the IRK or the AES battery key from another application. `[ble] key_file` names a regular file (not a symlink) owned by the daemon's user, mode `0600` with no group or other access, at most 4096 bytes, containing exactly this (placeholder values).

```json
{
  "address": "AC:DE:48:00:11:22",
  "irk": "<32 hexadecimal digits: AAP-export byte order>",
  "encryption_key": "<32 hexadecimal digits: direct AES key order>"
}
```

- `address` must match the pinned classic address. keys never go in a commit, log or issue.
- IRK matching uses bluetooth's short address hash and is **not authenticated encryption**. an advert never authorises a connection or security decision.
- a wrong encryption key can give plausible numbers. check against AAP readings across bud, case and charging states.
- AES is the [RustCrypto aes crate](https://docs.rs/aes/0.8.4/aes/). byte order follows [LibrePods BLE crypto handling](https://github.com/librepods-org/librepods/blob/53679cc90222e94ade84e66542d97ace2540e626/linux/ble/bleutils.cpp).

### advert layout

one 27-byte Apple manufacturer `0x004c` record, type `0x07`, model bytes `1b 20`, paired format `01`. the last 16 bytes are one AES block. battery bytes 1 to 3 are seven-bit percentages with a charging high bit. primary-side bit `0x20` sets left and right order. case data is used only when the reporting-bud-in-case bit `0x40` is set. other layouts, public coarse percentages, inferred ear and lid states and remote-host "free" flags are ignored. the layout follows the [LibrePods proximity parser](https://github.com/librepods-org/librepods/blob/53679cc90222e94ade84e66542d97ace2540e626/linux/ble/blemanager.cpp) and [encrypted battery layout](https://github.com/librepods-org/librepods/blob/53679cc90222e94ade84e66542d97ace2540e626/linux/battery.hpp). the auris code is written independently.

### precedence and discovery

- unknown `127` values do not reset age.
- a freshly reporting AAP cell wins for 15s so duplicate adverts do not flip the source. then a new exact BLE reading may replace it.
- AAP absence does not erase a fresh BLE reading. losing the AAP link invalidates AAP cells only.
- a BLE reading becomes historical after 75s without a new measurement. **that is auris policy, not the Apple case sleep timeout.**
- discovery is LE only, 8s per 30s by default. it may send scan requests and affect a busy or older adapter, so it is not physically passive.
- a window end releases only this client's scan token, never another application's discovery.
- bluez's cached manufacturer data is ignored. only a new manufacturer-data event refreshes.

see [bluez discovery](https://bluez.readthedocs.io/en/latest/adapter-api/) and [bluer discover_devices](https://docs.rs/bluer/0.17.4/bluer/struct.Adapter.html#method.discover_devices).

## Apple multi-host switching

opt-in with `[handoff] enabled = true` or `auris handoff on`. auris joins the AirPods' own ownership protocol over its AAP channel (L2CAP PSM `0x1001`), as LibrePods does on Android, instead of racing a Mac or iPhone for the audio link.

Apple hosts cooperate only with a host whose Device ID names Apple. the adapter must advertise `DeviceID = bluetooth:004C:0000:0000` and the AirPods must then be paired again. `handoff.apple_host_id` in `state.json` reports the adapter `Modalias`, or `null` when bluez exposes none.

### opcodes

*reversed* addresses are least significant byte first. offsets count after the `04 00 04 00 <opcode LE>` header.

| opcode | direction | payload | auris use |
|---|---|---|---|
| `0x000C` | AirPods to host | address (reversed), 2 unknown bytes | logged only |
| `0x000E` audio source | AirPods to host | address (reversed), `00` idle, `01` call, `02` media | `handoff.audio_source`. another host at `01`/`02` yields |
| `0x002E` connected devices | AirPods to host | 2 unknown bytes, count, per host address (**not** reversed) + 2 info bytes | `handoff.devices`, take-over targets. new hosts get media info + `newTipi` |
| `0x0010` smart routing | host to AirPods | target (reversed), u16 LE length, `01`, OPACK dict | media info, `Hijackv2`, `newTipi` |
| `0x0011` smart routing relay | AirPods to host | sender (reversed), u16 LE length, OPACK body | `audioRoutingSetOwnershipToFalse` yields |
| `0x0009` control `0x06` | both | `01` owns, `00` does not | `handoff.owner`. `00` on an established link yields |

fixtures for `201B` are in the `daemon/src/aap/codec.rs` tests, including one host decoding identically from `0x000E` (reversed) and `0x002E` (forward).

### `0x002E` info bytes

| byte 0 | meaning | seen |
|---|---|---|
| `0x00` | listed, link down | after that host disconnects |
| `0x01` | connecting | a few seconds before its link is up |
| `0x02` | link up | from link up. `0x01` to `0x02` takes about 80ms on reconnect |

hosts stay listed after disconnecting, so only byte 0 says who is connected. the rejoin classifier depends on it. byte 1 (`0x15`/`0x17` for a Mac, `0x01`/`0x03` here) and the second header byte (`00` before an Apple host relays smart routing, `02` after) are unexplained and not read.

### what each host observes

an Apple host claiming compares the audio category score this host published with its own, reads the AirPods' advertised stream state, and sends smart routing that arrives here as an `0x0011` relay with `audioRoutingSetOwnershipToFalse`. this host yields, the AirPods send `06 = 00` and report the new source over `0x000E`, then tear down this host's A2DP stream when the other host routes audio.

this host claiming sends `06 = 01`, media information and `Hijackv2` to every other listed host. the Apple host checks the name and model, gives up ownership, shows a banner naming the taker and routes audio to its speakers (`RouteToSpeaker` in macOS). the AirPods report the source idle, then this host once a stream flows.

with equal category scores the streaming host keeps the AirPods. an owner that starts no stream is hijacked back within about 10s. `btName` `Mac` and `iPhone` pass the name check. ownership and stream are separate. any host starting A2DP takes the audio, so a notification sound here interrupts the owning host, and a yield must release the local audio route too.

a Mac connecting fresh while this host holds the link makes the AirPods drop this host within about 0.5s, with no disconnect from the Mac, which logs `IsHeadphoneEligibleForTipiV2: Skip reason: ConnectedSourceDiffiCloud` (different iCloud account). connecting this host second keeps both, and handoff works both ways. rejoin recovers the first order.

### yield

reports update `handoff` state even with handoff disabled. only acting is gated. each AAP link opens with a state dump that can name a long-idle host, so an opening window runs from link up to the later of 3s and the end of the settings dump (first battery packet or readback timeout), capped at 10s. reports inside it update state but never yield, pause, hold or introduce a host. a relayed ownership-to-false request is live and still counts.

a yield needs a relayed `audioRoutingSetOwnershipToFalse` (`Hijackv2`), a `06 = 00` after the window, or an audio-source report after the window that changes the source to another host at call or media (a repeated snapshot is no change). auris then pauses Playing MPRIS players, releases the audio route, and sends `06 = 00` unless the AirPods sent it first. a further request while yielded renews the hold.

A2DP and HFP stay connected, as on Apple hosts. `DisconnectProfile` does not stick (bluez and the audio server restore the profile about 10s later, moving the stream and flipping the source), and linux has no equivalent of the Android per-device connection policy LibrePods uses short of editing wireplumber policy or blocking the device.

### yielded hold

players restart when a yield removes their sink. after any yield (automatic or `auris yield`) a local Paused to Playing edge is a self-resume when it lands within 2.5s of the auris pause or the peer's latest request, or within 2.5s of a bluez `Connected` change, an audio profile endpoint change, a `MediaTransport1` change or an audio-source report naming this host. the player is re-paused by MPRIS bus name, never taken over. an audio-source report naming this host as media while held also re-pauses, unless a play edge is pending. each player is re-paused once per request. a second resume before a new request is a user press and goes to take-over.

a self-resume lands about 0.5s after a sink change. the audio-change window stays 2.5s because every yield fires an audio-change event, and 5s would swallow a press 4s later.

the hold ends on take-over, a user play, an AAP link reset, or an idle or this-host audio-source report 10s or more after the latest request. a re-pause leaves `last_event` unchanged.

### take-over

needs `enabled`, `take_over_on_play` (default true) and a local MPRIS Paused to Playing edge that stays Playing 1.5s, is not the first reading since start or link open, is outside the hold windows, comes while another host owns the AirPods or auris released its audio, and not while the last audio-source report shows another host in a call. such a play ends the hold even when take-over is refused.

1. restore the pipewire card profile, before any AAP write.
2. send `06 = 01`.
3. send media information with `HostStreamingState YES`, `PlayingApp` from MPRIS `Identity`, `btAddress` of the adapter and `btName` (`Mac` unless `bt_name` is set).
4. send `Hijackv2` to every other listed host.
5. `Device1.ConnectProfile` for A2DP sink. already connected is success. busy (`br-connection-busy`) or in progress retries after 700ms, 1.5s and 3s, then warns. no retry once this host no longer owns.

a Mac's check logs `_shouldAllowRelinquishOwnership _myModel Mac otherTipiName Mac` and answers `YES`, and the same for `iPhone`. no other `btName` has been seen to work. `auris take-over` and `auris yield` run the same steps immediately, need an open AAP link and work with automatic handoff disabled.

### HostStreamingState and stream start

an early or stalled `NO` invites the other host to take the AirPods back.

- take-over sends `YES`. when the A2DP transport goes active, one more media information message sends `YES`, with no second `Hijackv2`.
- `NO` only after local playback stays stopped 3s, never within 8s of a take-over while the stream starts.
- a local play while owning, after a `NO`, sends `YES` once audio flows.

3s after a take-over, while a player is playing or stopped under 3s ago, a `MediaTransport1.State` other than `pending` or `active` logs `A2DP transport still idle 3 s after take-over` with the transport path. it is diagnostic only. cycling A2DP (`DisconnectProfile`, `ConnectProfile`) drops the link, bluez fails to reload the remote SEP (`Unable to load LastUsed: rseid 2 not found`) and wireplumber can settle on hands-free. an MPRIS `Pause` then `Play` 300ms later does not make the audio server re-acquire a transport pipewire never released. both were removed. the audio route fixes it by creating a fresh node.

## reconnect and rejoin classification

### one attempt, then stop

a new local bluetooth connection gets one AAP attempt after 800ms, with paired and `Connected` re-checked just before dialling. a link or identity change invalidates in-flight opening work. peer loss, a failed send or an unanswered watchdog stops automatic recovery. there is no redial loop competing with a phone.

`auris reconnect` retries only the AAP link while bluetooth is connected. `auris connect-once` requests one bluetooth connection to the pinned device with no retry. it can move audio and is a deliberate action, not "connect if others are idle". neither runs from BLE data. queued commands are bound to connection identity and epoch with a 4s deadline, and are rejected if expired or the connection changed. a timeout cannot undo a connect already sent to bluez, so read link state before retrying. a missing config uses defaults. malformed config stops startup rather than dropping a pin.

### auto-connect on case open

`[autoconnect]`, on by default. AirPods leaving the case page only their last host. a Mac pages them itself on their BLE proximity-pairing advert, while bluez never pages a classic device, so without this the Mac wins.

`autoconnect.rs` scans for the same advert (LE only, `duplicate_data` on, `discoverable` off) only while the AirPods are not connected locally. the scan needs LE. with `ControllerMode = bredr` it cannot start, the daemon warns once, rechecks every 5min or on adapter change, reports `autoconnect.scan` `le_disabled` in `state.json`, and only the page fallback connects.

an advert qualifies on company id `0x004C`, proximity-pairing type `0x07`, non-pairing-mode byte `0x01` and the model id little-endian (`1B 20` for `201B`, from the DID when known), at or above `min_rssi` (-90 dBm). weaker adverts are another room and not presence. cached `ManufacturerData` is never read, since bluez replays long-gone devices.

a qualifying advert after `absence_seconds` (15) of silence starts an episode, in practice a case shut and reopened. an episode gets one sequence, a connect after `settle_seconds` (3, so a nearer preferred host can win), then retries at 5s, 15s and 45s (four attempts), each only with an advert in the last 10s.

| disarm | detail |
|---|---|
| connect command or `Local` disconnect | `auris reconnect`, `auris connect-once` or `Device1.Disconnected` reason `Local`, until the next absence. aurisd never calls `Disconnect`, so `Local` is the user, panel or `bluetoothctl` choosing, and it sticks |
| pending rejoin | rejoin owns eviction recovery. a `Remote`/`Timeout` drop starts the absence clock and the page fallback, not an episode (buds out of the case keep advertising) |
| `enabled = false` | no discovery at all |

### page fallback

some dongles give no qualifying advert for a whole case cycle, and an LE-off adapter sees none. while the AirPods are disconnected, the machine is armed, the last disconnect was `Remote`/`Timeout` (or the daemon started disconnected) and no rejoin or episode is in flight, this host pages after `fallback_first_seconds` (20), then every `fallback_interval_seconds` (45), within `fallback_minutes` (10, `0` disables) from the drop.

each page is one `Device1.Connect`, logged `no proximity advert since the disconnect; paging anyway` with `away` and `next`. a timeout waits for the next slot (`fallback page did not take; waiting for the next slot`). it ends on success, on `connected_by_other_means`, or at the budget, leaving only the advert trigger until the next drop. an episode takes priority without moving the fallback clock.

auto-connect, fallback and rejoin share the guarded `bluez::rejoin_connect`, which re-reads adapter power, paired, blocked and connected before one `Device1.Connect`, issued from the supervisor so bluez sees one connect source. logs use target `aurisd::autoconnect`.

### when a rejoin is allowed

every `Device1.Disconnected(reason, message)` logs at INFO. `Device1.Connect` runs only if all of these hold when it arrives.

| # | check | rule |
|---|---|---|
| 1 | enabled | `[handoff] enabled` and `rejoin_after_eviction` (default true) |
| 2 | reason | `org.bluez.Reason.Remote` or `org.bluez.Reason.Timeout`. never `Local` (so never a manual disconnect), `Authentication`, `Suspend`, `Unknown`, others, no signal, or ECONNRESET alone |
| 3 | evidence | the latest `0x002E` of the ended session lists another host and matches a kind below |
| 4 | Apple host | that host matches by OUI or relay |
| 5 | not put away | last ear report not both buds `0x02` (case), lid not known closed |
| 6 | usable | adapter powered, AirPods paired and not `Blocked`. `Trusted` not required |
| 7 | manual quiet | no `auris reconnect` or `auris connect-once` in 10s |
| 8 | rate | a sequence in the last 60s defers the new one, it does not refuse |

| match kind | condition | describes |
|---|---|---|
| `joined` | report at most 1.5s before the signal, info byte `0x01` allowed. never for `Timeout` | an Apple host connecting and pushing this host off |
| `connected` | host byte 0 `0x02`, any age | a host that already held its link evicting this one |
| `link_lost` | `Timeout` and host byte 0 `0x02`, any age | this link died while the Apple host kept its own |

the `joined` window is measured at the signal (the AAP reset comes tens of ms earlier). `connected` and `link_lost` have no age cap because `0x002E` is sent only on change. scoping bounds them instead. only the latest report counts, a report serves one sequence, and a new link session forgets it. `0x000E` (can name an already-connected host) and control `0x06` (no address) are not used.

check 4 matches in two ways.

- **OUI.** first three octets registered to exactly `Apple, Inc.` (trimmed, ASCII case ignored). Macs and iPhones use their public BR/EDR address, which `0x002E` carries. the list comes from `/usr/share/hwdata/oui.txt` (`hwdata`), else `/usr/lib/udev/hwdb.d/20-OUI.hwdb` (systemd), else nine built-in OUIs copied from `oui.txt`, logged once as `Apple OUIs loaded count=... source=...`. a locally administered address (bit `0x02` of octet one) never matches.
- **relay.** the host sent an `0x0011` relay here. up to 8 addresses persist in `$CACHE_DIRECTORY/apple_hosts.json` or `~/.cache/aurisd/apple_hosts.json`. covers Apple hosts behind a non-Apple OUI. not required.

one bud charging in the case while the other is out is a bud swap and never blocks. `Trusted` only governs connections the AirPods start, and `bluetoothctl` or some panels leave it unset. nothing learned is needed for a first eviction. the OUI alone qualifies a host new to these AirPods or this host, AirPods paired a minute ago, and a missing or corrupt cache. the first `0x002E` on a link counts. a different AirPods address clears the report, ear state and eviction loop.

### bud swap

taking a bud out moves the radio link to the other bud, and an idle host's link may not survive. the AirPods can write it off in a report sent over it, then the drop is a `Timeout` while the owner keeps its link (`link_lost`), or sometimes `Remote` (`connected`). this host's own `info=00` is logged as `own_link_reported_down`, never required and never acted on. while bluez says connected, no report or ear change makes auris change state, send a packet or tear anything down.

### rate limit and the connect

one sequence starts per minute. an eviction inside the window is deferred to its end with the same evidence, sequence number and attempt budget, logged `rejoin deferred reason=rate_limited delay=...` (rest of window plus 3s). only the rate limit defers. the 20s loop check runs first.

the connect waits 3s, then re-checks powered, paired, not blocked and disconnected. the wait (ordinary or deferred) is cancelled by `Connected=true` from elsewhere, another `Disconnected`, both buds in the case, the adapter going away (object removed or `Adapter1.Powered=false`, not a watch ending), handoff off, or a connect command. link status reads `reconnecting` meanwhile. the wait and connect belong to the session task and survive a watcher rebuild. busy or in progress retries after 3s and 6s. any other error, a 30s timeout or a third busy stops with a warning. auris never calls `Disconnect`, even for a stuck `br-connection-busy`.

a `Remote`/`Timeout` disconnect within 20s of a successful rejoin logs `eviction loop` and disables rejoin until a connect command or restart. the normal new-link path then reopens AAP, about 7s from drop to usable link.

### device watch

bluez emits `InterfacesRemoved` when a profile interface such as `Battery1` or `MediaControl1` goes at disconnect, and `bluer` cancels the whole per-object subscription. auris treats it as an ended watch, reads `Adapter1.Powered` before calling the adapter gone, and rebuilds 2s later without touching a scheduled rejoin, its evidence or handoff state. treating it as `AdapterGone` would cancel rejoins on a powered adapter.

### logging

decisions log at INFO as `rejoin scheduled`, `rejoin deferred reason=rate_limited delay=...`, `rejoin skipped reason=...` or `rejoin cancelled reason=...`. skips are `no_recent_device_report_with_apple_host` (no report or no Apple host) and `no_apple_host_link_up` (Apple host listed, older than the window, link down).

every `Remote`/`Timeout` disconnect logs `disconnect: last AirPods connected-devices report` at INFO before deciding, even with rejoin off, with `reason`, `last_report_age_ms`, `apple_host_match` (`relay`, `oui`, `none`), `apple_host`, `apple_host_link_up`, `own_link_reported_down` and `listed_hosts`. `rejoin scheduled` adds `apple_host_match` and `match_kind`, `rejoin deferred` those and `delay`. each `0x002E` logs at DEBUG with header and per-address info bytes, as `devices=AC:DE:48:00:11:22 info=02 02, ...`.

## the pipewire audio route

with handoff enabled auris sets the card `bluez_card.<addr>` to `off` whenever this host does not own the AirPods, and back to the A2DP profile in use (normally `a2dp-sink`) when it owns or is taking over. the card comes from `pw-dump` and the profile is set with `pw-cli set-param <id> Profile '{ index: N }'` without `save`, so wireplumber never persists `off`. it runs on control `0x06`, at take-over start before AAP writes, on yield, and (restoring A2DP) when handoff turns off or the daemon stops. a disconnect touches nothing.

a yield runs in order, so playback stops instead of moving sink and ownership is never released over an open transport.

1. pause MPRIS players.
2. drain until none is Playing, at most 600ms.
3. set the profile `off`, releasing the transport from this side.
4. wait for the switch, at most 2s.
5. send the ownership release.

both waits are upper bounds. a node whose transport the AirPods tear down while running goes to error and never recovers, so releasing first means the next take-over gets a fresh node.

```
pw.node: (bluez_output.BC_80_4E_01_02_03.1-37) running -> error (Received error event)
```

the AirPods sink stays the default (priority 1010), so pipewire moves streams back when the node returns. this removes the owning-but-silent failure. behaviour against a call on the other host is not established.

## in-ear media control

ear states per bud are `0x00` in ear, `0x01` out, `0x02` case (case counts as out). a change must hold 700ms, absorbing case-open and primary-switch flaps. by default one of two in-ear buds leaving pauses local players, as macOS does. `pause_on_one_of_two = false` waits for the last bud. `auto_pause` and `auto_resume` gate each half. a resume fires once on the settled return when auris made the pause, the in-ear count is back to at least what was lost, within 5min, with no user play or pause between, and audio is on the AirPods (link up, card not yielded, this host owner).

## limits and evidence

- linux's outbound L2CAP connect can establish an ACL, so the fresh bluez check is a mitigation, not an atomic guarantee.
- bluez `Connected` is **local**. RSSI, availability and silence cannot prove another host is disconnected.
- other software or bluez policy can start a connection.
- auris cannot mute an iPhone or Mac that loses its audio route.
- an adapter power cycle inside the 3s rejoin wait is caught only by its `Disconnected` signal and the pre-connect re-read.
- with an Apple host listed link-up, an eviction cannot be told from another AirPods-initiated drop. the case, lid, rate, manual and loop checks bound it.
- an Apple host on a random or locally administered address misses the OUI rule. a non-Apple OS on Apple hardware matches it.
- Magic Pairing (link keys shared through the iCloud Keychain) is out of reach. linux pairs on its own and joins only through AAP ownership. a Mac or iPhone may still claim the AirPods for a call or by its own heuristics, and auris yields.

see [bluez Device1](https://bluez.readthedocs.io/en/latest/device-api/) and the [linux L2CAP connection path](https://github.com/torvalds/linux/blob/master/net/bluetooth/l2cap_core.c).

| behaviour | basis |
|---|---|
| `0x000E`, `0x002E`, `0x0011` layouts, control `0x06` | LibrePods parsers (`AACPManager.kt`), same names in the apple-wireshark AACP dissector, matched by this device's reports |
| `0x000C` address + 2 bytes | layout per the dissector, bytes unexplained |
| `0x002E` byte 0 | reports correlated with an Apple host's connection log |
| `0x002E` byte 1, second header byte | unexplained, not read |
| `Hijackv2` body | byte-for-byte equal to LibrePods' recorded iPhone encoding |
| `newTipi`, new-device media information | byte-for-byte equal to LibrePods' encoders, `btName` substituted |
| streaming media information | LibrePods' keys and values re-encoded as valid OPACK (LibrePods omits the `btName` key tag, tags `YES` as a two-byte string, zero-pads a fixed buffer). unconfirmed against an Apple host |
| yield triggers and take-over sequence | LibrePods code (`AirPodsService.kt`, `MediaController.kt`), exercised against a Mac |
| 2.5s hold windows, 1.5s debounce, no take-over from a call, profiles kept on yield | auris policy, replayed in `handoff.rs` tests. kept profiles unconfirmed against an Apple host |
| opening window (3s or dump, at most 10s) | auris policy, from a dump that named an idle Mac as media source |
| one re-pause per request, hold end 10s after request | auris policy, so a deliberate play is never blocked |
| `btName = Mac` accepted | confirmed by the Mac's `_shouldAllowRelinquishOwnership` answering `YES` for `Mac` and `iPhone` |
| `btName = Mac` giving a Mac-style banner | speculative |
| `HostStreamingState` timing | auris policy, replayed in `handoff.rs` |
| audio route order (pause, drain, `off`, release) | auris policy. 600ms and 2s are upper bounds |
| media information to every listed host | auris policy. LibrePods sends to the first only |

these are not yet exercised on hardware. `connected` and `link_lost` (derived from observed drops, never fired alone), the OUI path (no first-seen Apple host captured evicting this one), the audio route against a call on the other host or the stalled stream the MPRIS nudge failed on, whether an Apple host keeps the AirPods while this host's A2DP is connected but idle, and whether pipewire idle suspend or resume produces audio-source reports.
