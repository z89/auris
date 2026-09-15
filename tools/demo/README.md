# auris demo capture

`record.py` produces the README demo. it does three things:

- stages a reproducible DMS bar
- records a lossless crop
- logs the pointer and click timeline for post-processing

while it runs it changes the live bar, wallpaper, theme, AirPods connection and
AirPods controls. it restores the captured state on exit.

do not run any mode unless you are ready for those visible and Bluetooth changes.

## pre-record gate

gate before any take:

- finish the separately approved daemon/plugin deployment first.
- finish the hardware checks first.
- recalibrate against the condensed settings panel and the tighter bar icons.
  old coordinates are not acceptance evidence.
- include one instant setup change and its contextual feedback, then collapse
  setup before the main listening-mode/theme sequence.
- show real charging/history transitions; synthetic live values are not
  allowed.
- do not enable BLE discovery or unattended connection automation merely for a
  take. BLE requires provisioned keys and radio validation.

every calibration, dry run and recording still needs its own desktop approval.

## required programs

- `dms`, `auris`, `bluetoothctl`, `ydotool` and a running `ydotoold`
- `grim` and ImageMagick's `magick` for calibration
- `wf-recorder` for the real take
- `ffmpeg` and `gifski` for composition once `compose.py` is added

## wallpaper sequence

the take uses five local wallpapers in this order:

1. snowy mountains (`9fza8w50wdd91.png`)
2. cosmic blue/purple (`cosmic_art-wallpaper-5120x1440.jpg`)
3. warm lofi scene (`lofi-girl.jpg`)
4. neon magenta (`neon-paint-5120x1440.jpg`)
5. cyan/orange Shanghai (`shanghai-at-night-5120x1440.jpg`)

the first is staged before recording. the other four run from a single clock,
along with one dark-to-light mode change. each operation is written into
`events.json` after DMS returns.

## workflow

from the repository root:

```sh
python3 tools/demo/record.py --calibrate
python3 tools/demo/record.py --dry
python3 tools/demo/record.py
```

### calibrate

writes `/tmp/auris-demo/calib-grid.png`. update the geometry block at the top
of `record.py`, including the adaptive slider target. set
`GEOMETRY_CALIBRATED = True` only after every target has been measured on the
final UI.

### dry run

performs the real connection and control choreography without a recorder.
every click must produce its expected daemon state or the run aborts.

### real take

writes `/tmp/auris-demo/raw.mkv` and `events.json`. post-processing will use
the marks to trim the take, draw the cursor and click feedback, then produce a
compact README GIF and an H.264 MP4.
