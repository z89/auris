# changelog

## unreleased — AirPods 4 ANC settings

- major fix: prevent first-open setup clicks landing on DMS's invisible dismiss window:
  its native input hole stayed collapsed after setup expanded; keep updates
  enabled on Auris's separate dismiss surface while open and restore DMS's
  original binding on close; verified by native tracing, a threaded regression
  and live first-open, repeated expansion, scrolling, idle and reopen checks
- reserve fixed status lanes in every setup row and a fixed feedback footer for
  the whole expanded panel; sending, verifying, unconfirmed, success and error
  text can no longer resize or move panel content
- make every battery row permanently reserve its caption line; the brief AAP
  readback reconnect can dim or relabel cells without vertically shifting the
  battery card, setup disclosure or panel
- verify press speed, hold duration, Personalized Volume and call-control writes
  against AirPods 4 ANC reports; restore every tested original value
- when a setting has no immediate echo, reopen only the AAP control link once
  and use its startup report as authoritative readback; show Verifying meanwhile
- expose loaded UI revision and redacted command/report lifecycle diagnostics;
  reload success alone is not treated as proof of current UI or hardware changes
- smaller model-name title with right-aligned status, preserving header height
- mention charging only when reported; idle history shows just “last seen … ago”
- immediate valid setup changes, independently tracked requested/reported values,
  no Apply modes button, neutral unselected buttons even on primary-coloured DMS
  button themes, and non-layout-shifting severity toasts with countdown bars
- no chevron tooltip; paired bar artwork has a small visible silhouette gap
- preserve valid single-mode reports independently of stricter outgoing cycle
  validation; per-setting report counters distinguish echoes from battery pushes
- independent cell freshness/source, no stale-battery revival on AAP opening,
  private identity-bound cache and opt-in bounded BLE battery discovery with
  protected provisioned keys; BLE is not proof of audio ownership
- one guarded AAP attempt per local connection, cancellation on observed link
  changes and no automatic redial after ambiguous loss; explicit pinned-device
  connect-once, without unattended auto-connect or system audio policy changes

- fixed quick controls for listening mode and conversational awareness, with
  a contextual Adaptive audio adjustment; everyday controls never scroll away
- compact setup rows for device rename, microphone side, press speed, hold
  duration, listening-mode cycle, call controls and personalized volume, inside
  the bottom disclosure alongside device information
- clickable information tooltips replace repeated descriptions; setup request
  feedback stays visible while its controls scroll
- use DMS's fixed-height surface and clip to its animated height for the setup
  reveal; quick controls keep their position, dimensions and pixels
- measure panel width from button text, wrap constrained button rows and align
  card gutters; remove the second DMS hover-tooltip owner from info buttons
- regression checks use actual DMS button/hover/tooltip code offscreen, including
  pointer events, Escape, text fit at larger fonts and frame-by-frame reveal
- consistent left/right AirPods orientation across paired, single-bud and charging
  states, shared by bar and battery-row artwork; icon spacing is unchanged
- charging is panel-only: no charging bolt or green charging tint on the bar;
  charging/in-case buds are excluded from its icon and bud battery percentage
- retain nullable historical charging across absent reports and cache reloads;
  muted charging captions distinguish history from live data
- compact 12px charging bolt beside the percentage, without shortening the track
- outward-facing paired bar positions without changing single-bud ear identity
- chevron-only setup disclosure with connection, firmware and recovery details
- explicit settings-loader failure/retry feedback and full-panel headless checks
- settings component lives in its own directory so Qt's stale plugin-root
  listing cannot reject it with "File name case mismatch" during plugin-only
  development reloads; retry now clears the failed loader before reloading
- typed CLI/socket requests and additive schema-1 settings fields; old snapshots
  remain readable and old daemons are identified by the settings API marker
- new settings show only device-reported values, with pending/error/timeout
  feedback instead of treating a successful send as hardware confirmation
- protocol fixtures and CLI mock-socket regression checks
- rename, microphone selection and listening-mode cycling remain unconfirmed;
  no unattended Bluetooth auto-connect,
  media automation, case playback or Find My behavior is enabled

## 0.1.0

first one.

plugin

- bar pill with presence-aware AirPods artwork and the lower of the two bud percentages
- panel with left, right and case batteries, ear detection, noise control, adaptive slider and conversational awareness
- the panel header keeps connection and noise state inline, while the bar icon reflects whether one or both buds are reporting
- compact control spacing gives the listening-mode selector more room and keeps conversational awareness visually attached to it
- pill stays hidden while the airpods are away and comes back the moment they connect, off a push from the daemon. the panel only opens when it is clicked
- socket state stays authoritative after commands, avoiding a stale state-file reload that could make a selected listening mode jump back
- last known levels stay on the panel, dimmed, with how long ago they were seen
- control center tile
- right click on the pill flips between anc and transparency

daemon

- talks aap to the airpods over an l2cap socket. no root, no vendor id spoof
- battery for each bud and the case, ear detection, noise control, conversational awareness, adaptive level, model and firmware
- writes `state.json` atomically and takes commands on a unix socket, with the `auris` cli on top
- `subscribe` on that socket streams a snapshot on every change, so a ui does not have to poll
- keeps the last known levels across reconnects and restarts
- systemd user unit and an aur pkgbuild
