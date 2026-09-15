# changelog

## unreleased — AirPods 4 ANC settings

- in-ear media control (`[ear]`, on by default): a bud leaving an ear pauses
  local MPRIS players, and a bud returning resumes the player auris paused.
  the default pauses as soon as one of two in-ear buds leaves, matching
  macOS; `pause_on_one_of_two = false` narrows it to the last bud leaving,
  for a single-bud listener who swaps sides. a bud going into the case
  counts as leaving the ear. a changed reading settles for 700 ms before
  anything acts, so the accessory's own multi-report flapping cannot pause
  and resume audibly for no reason. a resume fires only within a five-minute
  window of auris's own pause, needs the in-ear count back to what it was,
  and is dropped for good if the user plays or pauses anything themselves in
  the meantime, or if another host owns the AirPods when the bud goes back
  in. `auto_pause` and `auto_resume` can each be turned off independently.

- yielding the AirPods to another host is now ordered: local MPRIS players
  are paused first, then the daemon waits up to 600 ms for playback to
  actually stop, then the PipeWire card profile is released, and only then
  is ownership given up. a player that ignores the pause no longer holds the
  yield up forever; the profile switch goes ahead once the wait expires,
  instead of playback moving to another sink.

- a rename is now verified from the AirPods themselves. the daemon records it
  under the key `name` in `settings_requested` and `settings_status`, reopens
  the AAP link like any other setting readback, and compares the name in the
  accessory's own 0x1D metadata with the one written: `confirmed` when they
  match, `mismatch` with the name the device kept when they do not.
  `device.name` now prefers the metadata name, and a BlueZ `Alias` change
  updates it exactly like a `Name` change; the daemon still never writes
  `Alias` itself. apple hosts keep their own pairing record name and never
  re-read it, so a rename from linux shows on linux and other non-apple
  hosts only

- the yielded pause hold shrank from 10 s to 2.5 s, and the yielded
  audio-change hold shrank from 5 s to 2.5 s. the shorter windows still
  catch a self-resume that lands well inside them, while letting a real
  user press through on one try instead of needing a second press to
  override a re-pause. the PipeWire profile now follows ownership directly,
  so every yield switches the card profile off and fires an audio-link
  changed event at once, arming the hold

- an eviction inside the 60 s rejoin rate limit now waits instead of being
  dropped. when the rate limit is the only blocker, the connect is scheduled
  for the moment the window expires, with the same evidence, sequence number
  and attempt budget, and logged as `rejoin deferred reason=rate_limited
  delay=...`. a connect command, both buds going into the case, the adapter
  going away or handoff being switched off cancel it like any pending
  rejoin, and the 20 s eviction loop still stops a ping-pong outright

- auto-connect pages without an advert when the advert never comes. while
  the airpods are away after a `Remote` or `Timeout` drop, or the daemon
  started disconnected, and nothing has disarmed the machine, aurisd now
  pages once `fallback_first_seconds` (default 20) after the drop and then
  every `fallback_interval_seconds` (default 45) for `fallback_minutes`
  (default 10), after which only the advert trigger remains. each page is a
  single `Device1.Connect` with no retry backoff, a successful connect or a
  link already taken ends it, an advert episode and a pending rejoin both
  keep priority, and `fallback_minutes = 0` turns it off

- state.json publishes a `link` object, and the schema version is now `2`.
  every schema-1 field kept its name and meaning, so a reader that ignores
  unknown keys needs no change. `link.status` is `connected` while
  `device.connected` is true, `reconnecting` from the moment a rejoin or an
  auto-connect sequence exists until the link is back or the sequence gives
  up, and `disconnected` otherwise. `link.reason` names what the reconnect is
  for: `bud_switch` when the `0x0C` primary bud address report moved in the
  15 s before the drop, `taken_over` when an apple host took the airpods,
  `link_lost` for a supervision timeout, `auto_connect` for the proximity
  pager. `link.attempt` counts the `Device1.Connect` calls made in the current
  sequence and is `0` while it is only scheduled; `link.since` is when the
  sequence started, rfc3339 in utc

- auto-connect when the case opens (`[autoconnect]`, on by default). aurisd
  now runs le discovery whenever the airpods are not connected locally, and
  treats an apple `0x004c` advert with type `0x07`, the non-pairing-mode
  byte `0x01` and the watched model id (little-endian `1b 20` for `201B`) at
  `min_rssi` or better as presence. a presence episode starts after 15 s of
  silence and gets one connect sequence: one `Device1.Connect` after a 3 s
  settle, which lets another host take them first if it wants them, then
  retries at 5 s, 15 s and 45 s, at most four, and only while an advert has
  been heard in the last 10 s. a manual `reconnect` or `connect-once`, and
  any disconnect with reason `Local`, disarm it until the airpods go away
  again; a pending rejoin always wins, so a `Remote` or `Timeout` drop still
  belongs to the rejoin rules alone. the supervisor issues the connect
  through the same guarded path rejoin uses, so there is still only one
  source of `Device1.Connect`

- reason `Timeout` now qualifies a rejoin as `match_kind="link_lost"` when
  the latest `0x002E` report of the ended link session shows an apple host
  with its link up, covering a bud swap that moves the airpods' radio link
  to the other bud and times this host's own link out. the airpods marking
  this host's own link down is logged as `own_link_reported_down` and is
  never required, and nothing acts on it: a report that writes this link off
  while bluez still says connected, and every ear-detection change, only
  record evidence. one bud in the case with the other in use never blocks a
  rejoin; both buds in the case still does. a manual (`Local`) disconnect
  still never rejoins, and the 20 s eviction-loop stop now covers `Timeout`
  too, so a bud swap that keeps killing the new link cannot loop

- rejoin also after an apple host that was already connected evicts this
  host. `0x002E` info byte 0 is read as the listed host's link state: `0x00`
  listed but down, `0x01` connecting, `0x02` link up. a drop now qualifies on
  either condition: `match_kind="joined"`, an apple host in a report at most
  1.5 s old, or `match_kind="connected"`, an apple host whose link is up in
  the latest report of the link session that just ended, at any age. the
  report is forgotten when a link session starts, so a stale one never
  qualifies. a report naming an apple host that is listed with its link down
  skips with the new reason `no_apple_host_link_up`, and the remote-drop log
  gained `apple_host_link_up`. every exclusion is unchanged: local or manual
  disconnect, buds in the case, closed lid, unpaired or blocked, rate limit,
  and the 20 s eviction-loop stop

- rejoin after an apple host connects (`[handoff] rejoin_after_eviction`,
  default on with handoff): auris reconnects once, 3 s later, only for a
  BlueZ `Disconnected` reason `Remote` within 1.5 s of a `0x002E` report
  listing an apple host, and never after a local, timeout or manual
  disconnect, with buds in the case, blocked, or in an eviction loop. a host
  is apple when its oui is registered to apple, inc. (hwdata `oui.txt`, else
  the systemd hwdb, else a built-in list) or it once sent a `0x0011` relay,
  so it works the first time, with no learned state; the airpods need to be
  paired, not trusted. the airpods keep a disconnected host in `0x002E`, so a
  newly joined address is not required. every remote drop logs the last
  report's age and whether the host matched by relay, oui or not at all;
  `0x002E` info bytes are logged at debug

- a rejoin could be cancelled by a false `adapter gone`: bluez ends the
  device property watch when any profile interface is removed at
  disconnect, and auris read that as the adapter going away. the stream
  ending is now its own event: the watch rebuilds, the scheduled rejoin, its
  evidence and handoff state are untouched, and `adapter gone` now needs the
  adapter object removed or `Powered=false`

- apple multi-host switching (opt-in, `[handoff] enabled`): decode audio
  source (0x0E), connected devices (0x2E), address report (0x0C),
  smart-routing relay (0x11) and ownership control 0x06 into a new
  always-present `handoff` object in state.json

- yield to another apple host that starts playing or asks for the airpods:
  ownership 00 and pause local MPRIS players; A2DP/HFP stay connected

- stop handoff ping-pong: after a yield, a player that resumes itself
  (within 10 s of auris's own pause, or 5 s of a BlueZ connection/profile/transport
  change or a source report naming this host) is paused again by MPRIS bus
  name instead of triggering a take-over; a source report naming this host
  as media while yielded pauses it too. no DisconnectProfile on yield: the
  audio server reconnects A2DP by itself and moves the playing stream back
  onto the airpods on its own

- stop pausing every local play after the airpods reconnect while another
  host is connected but idle: reports in the link-open window (3 s or the
  settings dump, at most 10 s) now only update `handoff`; yield needs a
  relayed `Hijackv2`, ownership 00 after the window, or a live source change
  to another playing host. each player is paused again at most once per
  request, so pressing play again takes over after the debounce; the hold
  ends on an idle or local source report 10 s after the latest request

- fix taking the airpods back giving no sound: the A2DP transport could stay
  idle after take-over, so playback stalled and the other host's next
  request could win because the airpods were not streaming. after a
  take-over auris now watches `MediaTransport1.State` and warns if it is not
  pending or active within 3 s while a player wants audio, naming the
  transport bluez reports. `NO` waits for 3 s of stopped playback and never
  goes out within 8 s of a take-over; `YES` is sent once more when the
  transport becomes active

- cycling the A2DP profile after a take-over, an early attempt at the fix
  above, is gone: disconnect and connect tore the link down, bluez
  `Connected` flapped, the AAP link reopened, and wireplumber could land on
  the hands-free profile, audible as phone-call quality, with both hosts
  losing the airpods soon after. auris now never disconnects a profile; the
  transport log names the transport path and uuid, and audio landing on a
  non-A2DP transport is logged

- nudging the player over MPRIS (pause, 300 ms, play) after an idle
  take-over, a second attempt at the same fix, is also gone: it changed
  nothing when the transport stayed idle. the 3 s idle warning stays as a
  diagnostic only

- the airpods' audio route now follows ownership instead of whichever host
  last started an A2DP stream. while handoff is enabled, auris sets the
  PipeWire bluez card profile to `off` whenever this host does not own the
  airpods, and back to the A2DP profile that was in use when it owns or is
  taking over, using `pw-dump` to find the card and `pw-cli set-param`
  without the save flag so wireplumber never persists `off` as the user's
  choice. this matches what macOS itself does for a host that does not own
  the airpods (`RouteToSpeaker` in its log): sounds on this host go to the
  laptop speakers while another host owns the airpods, instead of stealing
  the stream. releasing the transport on yield, before giving up ownership,
  also avoids a PipeWire node left running under a transport that is about
  to disappear, and take-over recreates the node instead of reusing a failed
  one. the airpods sink stays the default (priority 1010), so pipewire moves
  streams back on its own once the node returns

- take over on a local MPRIS play edge that stays playing for 1.5 s
  (`take_over_on_play`, default on), never while another host is in a call:
  ownership 01, media information and Hijackv2 to the other hosts, then
  connect A2DP, retrying a busy link at 700 ms, 1.5 s and 3 s while still
  owning; announce new hosts with media information and newTipi

- `auris handoff on|off|status`, `auris take-over`, `auris yield`; control
  requests `take_over`, `yield` and `set_handoff` (persists to config.toml,
  keeping comments)

- `handoff.apple_host_id` reports whether the adapter's Device ID names Apple

- fix first-open setup clicks landing on DMS's invisible dismiss window: its
  native input hole stayed collapsed after setup expanded; keep updates
  enabled on Auris's separate dismiss surface while open and restore DMS's
  original binding on close

- reserve fixed status lanes in every setup row and a fixed feedback footer
  for the whole expanded panel; sending, verifying, unconfirmed, success and
  error text can no longer resize or move panel content

- stay on the bar while the daemon heals the link: a `link.status` of
  `reconnecting` keeps the module visible with its last readings dimmed, a
  faded bar icon, a panel line naming the cause (bud switch, taken over,
  link lost, auto-connect) with the attempt once past the first, and no
  cancellation of a pending setup write; any other disconnect is held for
  two seconds before the module leaves, and a daemon with no `link` object
  behaves as before

- make every battery row permanently reserve its caption line; the brief AAP
  readback reconnect can dim or relabel cells without vertically shifting
  the battery card, setup disclosure or panel

- verify press speed, hold duration, Personalized Volume and call-control
  writes against AirPods 4 ANC reports; restore every tested original value

- when a setting has no immediate echo, reopen only the AAP control link
  once and use its startup report as authoritative readback; show Verifying
  meanwhile

- expose loaded UI revision and redacted command/report lifecycle
  diagnostics; reload success alone is not treated as proof of current UI or
  hardware changes

- smaller model-name title with right-aligned status, preserving header
  height

- mention charging only when reported; idle history shows just "last seen …
  ago"

- immediate valid setup changes, independently tracked requested/reported
  values, no Apply modes button, neutral unselected buttons even on
  primary-coloured DMS button themes, and non-layout-shifting severity
  toasts with countdown bars

- no chevron tooltip; paired bar artwork has a small visible silhouette gap

- preserve valid single-mode reports independently of stricter outgoing
  cycle validation; per-setting report counters distinguish echoes from
  battery pushes

- independent cell freshness/source, no stale-battery revival on AAP
  opening, private identity-bound cache and opt-in bounded BLE battery
  discovery with protected provisioned keys; BLE is not proof of audio
  ownership

- one guarded AAP attempt per local connection, cancellation on observed
  link changes and no automatic redial after ambiguous loss; explicit
  pinned-device connect-once, without unattended auto-connect or system
  audio policy changes

- daemon: settings writes are applied with a real readback. AirPods 4 (ANC)
  stores a write silently and dumps its settings only once per AAP link, so
  the daemon sends one datagram, debounces 1500 ms, reopens only the AAP
  link and compares that link's dump against what was asked. the useless
  set-features "refresh" packet is gone; no write is ever replayed

- daemon: per-setting readback status in state.json (`settings_api` 2):
  `settings_requested`, `settings_status` (verifying/confirmed/mismatch/
  unreported/unverified), `settings_verify` and `verify_reopen`, which keeps
  the UI showing the device as connected while the readback link is
  reopening. `auris setting` waits for the verdict and prints one line,
  failing on a mismatch instead of claiming success

- daemon: `AURISD_FEATURES=ff` selects the alternate `0xff` set-features
  byte as an opt-in; it produces the same dump as the default `0xd7`

- daemon: hex logging of unknown and undecodable AAP packets, the only way
  to identify unmodelled replies without a root HCI trace; payload values of
  recognised settings reports are still never logged

- fixed quick controls for listening mode and conversational awareness, with
  a contextual Adaptive audio adjustment; everyday controls never scroll away

- compact setup rows for device rename, microphone side, press speed, hold
  duration, listening-mode cycle, call controls and personalized volume,
  inside the bottom disclosure alongside device information

- clickable information tooltips replace repeated descriptions; setup
  request feedback stays visible while its controls scroll

- use DMS's fixed-height surface and clip to its animated height for the
  setup reveal; quick controls keep their position, dimensions and pixels

- measure panel width from button text, wrap constrained button rows and
  align card gutters; remove the second DMS hover-tooltip owner from info
  buttons

- regression checks use actual DMS button/hover/tooltip code offscreen,
  including pointer events, Escape, text fit at larger fonts and
  frame-by-frame reveal

- consistent left/right AirPods orientation across paired, single-bud and
  charging states, shared by bar and battery-row artwork; icon spacing is
  unchanged

- charging is panel-only: no charging bolt or green charging tint on the
  bar; charging/in-case buds are excluded from its icon and bud battery
  percentage

- retain nullable historical charging across absent reports and cache
  reloads; muted charging captions distinguish history from live data

- compact 12px charging bolt beside the percentage, without shortening the
  track

- outward-facing paired bar positions without changing single-bud ear
  identity

- chevron-only setup disclosure with connection, firmware and recovery
  details

- explicit settings-loader failure/retry feedback and full-panel headless
  checks

- settings component lives in its own directory so Qt's stale plugin-root
  listing cannot reject it with "File name case mismatch" during
  plugin-only development reloads; retry now clears the failed loader
  before reloading

- typed CLI/socket requests and additive schema-1 settings fields; old
  snapshots remain readable and old daemons are identified by the settings
  API marker

- new settings show only device-reported values, with pending/error/timeout
  feedback instead of treating a successful send as hardware confirmation

- protocol fixtures and CLI mock-socket regression checks

- rename, microphone selection and listening-mode cycling remain
  unconfirmed; no unattended Bluetooth auto-connect, media automation, case
  playback or Find My behavior is enabled

## 0.1.0

first one.

plugin

- bar pill with presence-aware AirPods artwork and the lower of the two bud
  percentages
- panel with left, right and case batteries, ear detection, noise control,
  adaptive slider and conversational awareness
- the panel header keeps connection and noise state inline, while the bar
  icon reflects whether one or both buds are reporting
- compact control spacing gives the listening-mode selector more room and
  keeps conversational awareness visually attached to it
- pill stays hidden while the airpods are away and comes back the moment
  they connect, off a push from the daemon. the panel only opens when it is
  clicked
- socket state stays authoritative after commands, avoiding a stale
  state-file reload that could make a selected listening mode jump back
- last known levels stay on the panel, dimmed, with how long ago they were
  seen
- control center tile
- right click on the pill flips between anc and transparency

daemon

- talks aap to the airpods over an l2cap socket. no root, no vendor id spoof
- battery for each bud and the case, ear detection, noise control,
  conversational awareness, adaptive level, model and firmware
- writes `state.json` atomically and takes commands on a unix socket, with
  the `auris` cli on top
- `subscribe` on that socket streams a snapshot on every change, so a ui
  does not have to poll
- keeps the last known levels across reconnects and restarts
- systemd user unit and an aur pkgbuild
