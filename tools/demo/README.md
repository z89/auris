# auris demo capture

`record.py` produces the README demo. it stages a reproducible DankMaterialShell (DMS) bar, records a lossless crop and logs the pointer and click timeline for post-processing. while it runs it changes the live bar, wallpaper, theme, AirPods connection and AirPods controls, and it restores the captured state on exit. run no mode until those visible and bluetooth changes are acceptable, and every calibration, dry run and recording needs its own desktop approval.

## before a take

- finish the separately approved daemon and plugin deployment and the hardware checks.
- recalibrate against the condensed settings panel and the tighter bar icons. old coordinates are not acceptance evidence.
- include one instant setup change with its feedback, then collapse setup before the listening-mode and theme sequence.
- show real charging and history transitions. synthetic live values are not allowed.
- do not enable BLE discovery or unattended connection automation for a take. BLE needs provisioned keys and radio validation.

## required programs

`dms`, `auris`, `bluetoothctl`, `ydotool` with a running `ydotoold`, `grim` and imagemagick's `magick` for calibration, `wf-recorder` for the take, and `ffmpeg` and `gifski` for composition once `compose.py` exists.

## wallpapers

| order | wallpaper | file |
|---|---|---|
| 1 | snowy mountains | `9fza8w50wdd91.png` |
| 2 | cosmic blue and purple | `cosmic_art-wallpaper-5120x1440.jpg` |
| 3 | warm lofi scene | `lofi-girl.jpg` |
| 4 | neon magenta | `neon-paint-5120x1440.jpg` |
| 5 | cyan and orange Shanghai | `shanghai-at-night-5120x1440.jpg` |

the first is staged before recording. the other four and one dark to light mode change run from a single clock, and each is written to `events.json` after DMS returns.

## workflow

run from the repository root.

```sh
python3 tools/demo/record.py --calibrate
python3 tools/demo/record.py --dry
python3 tools/demo/record.py
```

| mode | what it does |
|---|---|
| `--calibrate` | writes `/tmp/auris-demo/calib-grid.png`. update the geometry block at the top of `record.py`, including the adaptive slider target, and set `GEOMETRY_CALIBRATED = True` only once every target is measured on the final UI |
| `--dry` | runs the real connection and control sequence without a recorder. a click that does not produce its expected daemon state aborts the run |
| no flag | writes `/tmp/auris-demo/raw.mkv` and `events.json`. post-processing uses the marks to trim, draw the cursor and click feedback, and produce a compact README GIF and an H.264 MP4 |
