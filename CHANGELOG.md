# changelog

## 0.1.0

first one.

plugin

- bar pill with a headphones icon and the lower of the two bud percentages
- panel with left, right and case batteries, ear detection, noise control, adaptive slider and conversational awareness
- pill stays hidden while the airpods are away and comes back on its own when they connect, with the panel open for a few seconds
- last known levels stay on the panel, dimmed, with how long ago they were seen
- control center tile
- right click on the pill flips between anc and transparency

daemon

- talks aap to the airpods over an l2cap socket. no root, no vendor id spoof
- battery for each bud and the case, ear detection, noise control, conversational awareness, adaptive level, model and firmware
- writes `state.json` atomically and takes commands on a unix socket, with the `auris` cli on top
- keeps the last known levels across reconnects and restarts
- systemd user unit and an aur pkgbuild
