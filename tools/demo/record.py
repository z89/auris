#!/usr/bin/env python3
"""Record the auris demo.

Stages the DMS bar down to a single auris widget, drives the pointer through a
fixed choreography while the shell theme changes underneath, and writes a frame
accurate event log so the cursor can be drawn back on in post.

  record.py --calibrate    stage the bar, open the panel, dump a measuring grid
  record.py --dry          run the choreography with no recorder attached
  record.py                the real take

Everything it touches (bar layout, wallpaper, theme mode, Bluetooth connection,
noise mode, adaptive level and conversational awareness) is snapshotted first
and restored on the way out, including on Ctrl-C.
"""

import atexit
import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import threading
import time
from pathlib import Path

HOME = Path.home()
RUNTIME = Path(os.environ.get("XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}"))
SETTINGS = HOME / ".config/DankMaterialShell/settings.json"
STATE = RUNTIME / "aurisd/state.json"

OUT = Path(os.environ.get("AURIS_DEMO_OUT", "/tmp/auris-demo"))
WALLPAPERS = [
    HOME / "Pictures/wallpapers/9fza8w50wdd91.png",
    HOME / "Pictures/wallpapers/cosmic_art-wallpaper-5120x1440.jpg",
    HOME / "Pictures/wallpapers/lofi-girl.jpg",
    HOME / "Pictures/wallpapers/neon-paint-5120x1440.jpg",
    HOME / "Pictures/wallpapers/shanghai-at-night-5120x1440.jpg",
]

# The snowy mountains are staged before recording. These four changes then
# move through cool saturated, warm, neon and cyan/orange palettes. Light mode
# enters over the neon wallpaper so both DMS theme modes appear in the take.
THEME_EVENTS = [
    (5.0, "wallpaper", WALLPAPERS[1]),
    (7.5, "wallpaper", WALLPAPERS[2]),
    (10.0, "wallpaper", WALLPAPERS[3]),
    (11.0, "theme", "light"),
    (12.5, "wallpaper", WALLPAPERS[4]),
]

# --- geometry -------------------------------------------------------------
# Screen coords, valid only while the bar is staged (auris alone, centred).
# Re-measure with --calibrate if the panel layout changes.
CROP = (2300, 0, 520, 560)          # x, y, w, h handed to wf-recorder
PILL = (2560, 24)

# These are from the previous panel revision. The approved header and spacing
# redesign deliberately invalidates them; calibration must set the new points.
GEOMETRY_CALIBRATED = False
NOISE_OFF = (2430, 340)
NOISE_ANC = (2490, 340)
NOISE_TRANS = (2585, 340)
NOISE_ADAPTIVE = (2690, 340)
ADAPTIVE_LEVEL = (2630, 430)
CA_TOGGLE = (2715, 528)
PARK = (2610, 200)                  # somewhere harmless to leave the pointer

FPS = 30
RATE = 1 / 120.0                    # pointer update interval


# --- plumbing -------------------------------------------------------------

def hypr_socket():
    sig = os.environ.get("HYPRLAND_INSTANCE_SIGNATURE")
    base = RUNTIME / "hypr"
    if not sig:
        cands = sorted(base.glob("*/.socket.sock"), key=lambda p: p.stat().st_mtime)
        if not cands:
            sys.exit("no hyprland instance socket found")
        return str(cands[-1])
    return str(base / sig / ".socket.sock")


HYPR_SOCK = hypr_socket()


def hypr(cmd):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.connect(HYPR_SOCK)
    s.sendall(cmd.encode())
    s.recv(256)
    s.close()


def dms(*args):
    return subprocess.run(["dms", "ipc", "call", *args],
                          capture_output=True, text=True).stdout.strip()


def ydotool(*args):
    env = dict(os.environ)
    env.setdefault("YDOTOOL_SOCKET", str(RUNTIME / ".ydotool_socket"))
    subprocess.run(["ydotool", *args], env=env, check=True,
                   capture_output=True)


def read_state():
    try:
        return json.loads(STATE.read_text())
    except Exception:
        return {}


def wait_state(label, predicate, timeout=5.0):
    """Wait for an observable daemon state or fail the take."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        state = read_state()
        if predicate(state):
            return state
        if LOG.t0 is not None:
            LOG.point(*_pos)
        time.sleep(0.05)
    raise RuntimeError(f"timed out waiting for {label}")


def bluetooth(action, address):
    result = subprocess.run(
        ["bluetoothctl", action, address], capture_output=True, text=True
    )
    if result.returncode != 0 or "successful" not in result.stdout.lower():
        detail = (result.stdout + result.stderr).strip()
        raise RuntimeError(f"bluetoothctl {action} failed: {detail}")


def preflight(mode):
    required = {"dms", "auris", "ydotool", "bluetoothctl"}
    if mode == "record":
        required.update({"wf-recorder", "paplay"})
    if mode == "calibrate":
        required.update({"grim", "magick"})
    missing = sorted(name for name in required if shutil.which(name) is None)
    if missing:
        sys.exit("missing tools: " + ", ".join(missing))

    if not SETTINGS.is_file():
        sys.exit(f"DMS settings not found: {SETTINGS}")
    try:
        settings = json.loads(SETTINGS.read_text())
        if not settings.get("barConfigs"):
            raise ValueError("barConfigs is empty")
    except Exception as exc:
        sys.exit(f"DMS settings are not usable: {exc}")

    missing_wallpapers = [str(path) for path in WALLPAPERS if not path.is_file()]
    if missing_wallpapers:
        sys.exit("missing wallpapers: " + ", ".join(missing_wallpapers))

    OUT.mkdir(parents=True, exist_ok=True)
    if shutil.disk_usage(OUT).free < 2 * 1024**3:
        sys.exit(f"less than 2 GiB is free for the lossless capture in {OUT}")

    state = read_state()
    if state.get("schema") not in (1, 2):
        sys.exit("aurisd state is missing or has an unsupported schema")

    ydotool_socket = Path(os.environ.get("YDOTOOL_SOCKET", RUNTIME / ".ydotool_socket"))
    if not ydotool_socket.exists():
        sys.exit(f"ydotoold is not running at {ydotool_socket}")

    if mode != "calibrate" and not GEOMETRY_CALIBRATED:
        sys.exit("demo coordinates need calibration; run --calibrate and update the geometry block")


# --- event log ------------------------------------------------------------

class Log:
    """Pointer track and click markers, timestamped against recorder start."""

    def __init__(self):
        self.t0 = None
        self.track = []
        self.clicks = []
        self.marks = {}

    def start(self):
        self.t0 = time.monotonic()

    def now(self):
        return time.monotonic() - self.t0

    def point(self, x, y):
        self.track.append((round(self.now(), 4), x, y))

    def click(self, x, y, button="left"):
        self.clicks.append({"t": round(self.now(), 4), "x": x, "y": y,
                            "button": button})

    def mark(self, name):
        self.marks[name] = round(self.now(), 4)

    def dump(self, path):
        path.write_text(json.dumps({
            "fps": FPS,
            "crop": CROP,
            "track": self.track,
            "clicks": self.clicks,
            "marks": self.marks,
        }))


LOG = Log()


# --- pointer --------------------------------------------------------------

def ease(t):
    return 4 * t * t * t if t < 0.5 else 1 - pow(-2 * t + 2, 3) / 2


def warp(x, y):
    # Hyprland 0.56 routes dispatchers through Lua; this form is an absolute
    # warp, so it sidesteps the pointer acceleration that makes ydotool's
    # relative and absolute moves land short of where you asked.
    hypr(f"dispatch hl.dsp.cursor.move({{x={int(x)}, y={int(y)}}})")
    LOG.point(int(x), int(y))


def glide(to, dur, frm=None):
    """Move the pointer along an eased path so it reads as a hand, not a jump."""
    global _pos
    frm = frm or _pos
    steps = max(2, int(dur / RATE))
    begin = time.monotonic()
    for i in range(steps + 1):
        f = ease(i / steps)
        warp(frm[0] + (to[0] - frm[0]) * f, frm[1] + (to[1] - frm[1]) * f)
        target = begin + (i + 1) * RATE
        slack = target - time.monotonic()
        if slack > 0:
            time.sleep(slack)
    _pos = to


def hold(dur):
    """Idle, still logging position so the drawn cursor does not vanish."""
    end = time.monotonic() + dur
    while time.monotonic() < end:
        LOG.point(*_pos)
        time.sleep(RATE * 4)


def click(button="left"):
    code = "0xC0" if button == "left" else "0xC1"
    LOG.click(_pos[0], _pos[1], button)
    ydotool("click", code)


_pos = (2900, 420)


# --- snapshot and restore -------------------------------------------------

class Restore:
    def __init__(self):
        self.done = False
        self.settings = None
        self.wallpaper = None
        self.mode = None
        self.noise = None
        self.ca = None
        self.adaptive = None
        self.address = None
        self.was_connected = False

    def snapshot(self):
        OUT.mkdir(parents=True, exist_ok=True)
        self.settings = SETTINGS.read_bytes()
        (OUT / "settings.backup.json").write_bytes(self.settings)
        self.wallpaper = dms("wallpaper", "get")
        self.mode = dms("theme", "getMode")
        st = read_state()
        device = st.get("device", {})
        self.address = device.get("address")
        self.was_connected = device.get("connected") is True
        self.noise = st.get("noise_control")
        self.ca = st.get("conversational_awareness")
        self.adaptive = st.get("adaptive_level")
        print(f"snapshot: wallpaper={Path(self.wallpaper).name} mode={self.mode} "
              f"noise={self.noise} ca={self.ca}")

    def __call__(self, *_):
        if self.done:
            return
        self.done = True
        print("\nrestoring...")
        if self.settings:
            SETTINGS.write_bytes(self.settings)
        if self.mode in ("dark", "light"):
            dms("theme", self.mode)
        if self.wallpaper and Path(self.wallpaper).exists():
            dms("wallpaper", "set", self.wallpaper)
        if self.address and self.was_connected and not read_state().get("device", {}).get("connected"):
            try:
                bluetooth("connect", self.address)
                wait_state(
                    "the original Bluetooth connection",
                    lambda s: s.get("device", {}).get("connected") is True,
                    timeout=10,
                )
            except Exception as exc:
                print(f"warning: could not restore Bluetooth connection: {exc}")
        if self.noise:
            subprocess.run(["auris", "noise", self.noise], capture_output=True)
        if self.ca is not None:
            subprocess.run(["auris", "ca", "on" if self.ca else "off"],
                           capture_output=True)
        if self.adaptive is not None:
            subprocess.run(["auris", "adaptive", str(self.adaptive)],
                           capture_output=True)
        print("restored")


RESTORE = Restore()


def stage_bar():
    """auris alone, dead centre, nothing else on the strip."""
    s = json.loads(SETTINGS.read_text())
    bar = s["barConfigs"][0]
    bar["leftWidgets"] = []
    bar["centerWidgets"] = ["auris"]
    bar["rightWidgets"] = []
    SETTINGS.write_text(json.dumps(s, indent=2))
    time.sleep(1.5)


def stage_look():
    """Start every take from the same snowy, dark colour treatment."""
    dms("theme", "dark")
    dms("wallpaper", "set", str(WALLPAPERS[0]))
    time.sleep(1.5)


def reset_controls():
    """Put every control demonstrated by the take at a known baseline."""
    for args in (("noise", "off"), ("ca", "off"), ("adaptive", "50")):
        result = subprocess.run(["auris", *args], capture_output=True, text=True)
        if result.returncode != 0:
            raise RuntimeError(f"could not reset {' '.join(args)}: {result.stdout.strip()}")


# --- choreography ---------------------------------------------------------

def themes():
    """Run the ordered wallpaper/theme track from one scheduler thread."""
    def run():
        for at, kind, value in THEME_EVENTS:
            slack = at - LOG.now()
            if slack > 0:
                time.sleep(slack)
            if kind == "wallpaper":
                dms("wallpaper", "set", str(value))
                LOG.mark("wallpaper_" + Path(value).stem)
            else:
                dms("theme", str(value))
                LOG.mark("theme_" + str(value))

    threading.Thread(target=run, name="auris-demo-theme", daemon=True).start()


def select_noise(point, mode, travel, settle):
    glide(point, travel)
    hold(0.12)
    click()
    wait_state(
        f"noise mode {mode}",
        lambda s: s.get("noise_control") == mode,
    )
    LOG.mark("noise_" + mode)
    hold(settle)


def choreograph(address, wait_for_charge=True):
    if LOG.t0 is None:
        LOG.start()
    themes()

    hold(0.4)
    bluetooth("connect", address)
    wait_state(
        "AirPods connection",
        lambda s: s.get("device", {}).get("connected") is True,
        timeout=10,
    )
    LOG.mark("airpods_connected")
    wait_state(
        "both AirPods reporting",
        lambda s: all(
            s.get("battery", {}).get(side, {}).get("present") is True
            for side in ("left", "right")
        ),
        timeout=10,
    )
    LOG.mark("both_buds_visible")

    glide(PILL, 1.0)                     # cursor enters frame, heads for the pill
    hold(0.3)
    click()                              # panel opens
    LOG.mark("panel_open")
    hold(1.4)                            # rows fill in, wallpaper flips underneath

    select_noise(NOISE_OFF, "off", 0.45, 0.35)
    select_noise(NOISE_ANC, "anc", 0.35, 0.45)
    select_noise(NOISE_TRANS, "transparency", 0.4, 0.5)
    select_noise(NOISE_ADAPTIVE, "adaptive", 0.4, 0.55)

    glide(ADAPTIVE_LEVEL, 0.4)
    hold(0.12)
    click()
    wait_state(
        "adaptive strength change",
        lambda s: s.get("adaptive_level") not in (None, 50),
    )
    LOG.mark("adaptive_level")
    hold(0.45)

    glide(CA_TOGGLE, 0.55); hold(0.15); click(); hold(0.75)
    wait_state(
        "conversational awareness on",
        lambda s: s.get("conversational_awareness") is True,
    )
    LOG.mark("ca_on")
    click()
    wait_state(
        "conversational awareness off",
        lambda s: s.get("conversational_awareness") is False,
    )
    LOG.mark("ca_off")
    hold(0.5)

    glide(PARK, 0.5)
    LOG.mark("await_charge")

    if wait_for_charge:
        subprocess.Popen(["paplay", "/usr/share/sounds/freedesktop/stereo/message.oga"],
                         stderr=subprocess.DEVNULL)
        print("\n>>> drop the right bud into the case now <<<\n")
        deadline = time.monotonic() + 120
        while time.monotonic() < deadline:
            b = read_state().get("battery", {})
            if any(b.get(k, {}).get("charging") for k in ("left", "right", "case")):
                break
            LOG.point(*_pos)
            time.sleep(RATE * 4)
        else:
            print("timed out waiting for a charging bud")
    LOG.mark("charging")
    hold(2.5)
    LOG.mark("end")


# --- modes ----------------------------------------------------------------

def calibrate():
    preflight("calibrate")
    RESTORE.snapshot()
    atexit.register(RESTORE)
    signal.signal(signal.SIGINT, lambda *a: sys.exit(1))
    stage_bar()
    stage_look()
    LOG.start()
    glide(PILL, 0.4)
    time.sleep(0.3)
    click()
    time.sleep(1.2)
    x, y, w, h = CROP
    raw = OUT / "calib-raw.png"
    grid = OUT / "calib-grid.png"
    subprocess.run(["grim", "-g", f"{x},{y} {w}x{h}", str(raw)], check=True)
    draw = ["magick", str(raw)]
    for gx in range(0, w + 1, 20):
        draw += ["-stroke", "red" if gx % 100 == 0 else "rgba(255,0,0,0.25)",
                 "-strokewidth", "1", "-draw", f"line {gx},0 {gx},{h}"]
    for gy in range(0, h + 1, 20):
        draw += ["-stroke", "red" if gy % 100 == 0 else "rgba(255,0,0,0.25)",
                 "-strokewidth", "1", "-draw", f"line 0,{gy} {w},{gy}"]
    draw += ["-stroke", "none", "-fill", "yellow", "-pointsize", "13"]
    for gx in range(0, w + 1, 100):
        draw += ["-draw", f"text {gx + 2},12 '{x + gx}'"]
    for gy in range(100, h + 1, 100):
        draw += ["-draw", f"text 2,{gy - 3} '{gy}'"]
    draw.append(str(grid))
    subprocess.run(draw, check=True)
    print(f"grid written to {grid}  (labels are SCREEN x, crop-relative y)")
    time.sleep(0.3)


def record():
    preflight("record")
    RESTORE.snapshot()
    atexit.register(RESTORE)
    signal.signal(signal.SIGINT, lambda *a: sys.exit(1))
    signal.signal(signal.SIGTERM, lambda *a: sys.exit(1))

    st = read_state()
    if not st.get("device", {}).get("connected"):
        sys.exit("airpods are not connected")
    b = st.get("battery", {})
    missing = [k for k in ("left", "right") if not b.get(k, {}).get("present")]
    if missing:
        sys.exit(f"not reporting: {', '.join(missing)} - take both buds out of the case")

    reset_controls()
    stage_bar()
    stage_look()
    address = st.get("device", {}).get("address")
    if not address:
        sys.exit("airpods address is missing from daemon state")
    bluetooth("disconnect", address)
    wait_state(
        "AirPods disconnection before the take",
        lambda s: s.get("device", {}).get("connected") is False,
        timeout=10,
    )

    x, y, w, h = CROP
    raw = OUT / "raw.mkv"
    raw.unlink(missing_ok=True)
    # Timestamp the event track against recorder launch, including its warm-up,
    # rather than resetting the clock when the first pointer move begins.
    LOG.start()
    rec = subprocess.Popen(
        ["wf-recorder", "-g", f"{x},{y} {w}x{h}", "-r", str(FPS),
         "-c", "ffv1", "-x", "bgr0", "-f", str(raw)],
        stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    time.sleep(2.0)
    if rec.poll() is not None:
        sys.exit(f"wf-recorder died: {rec.stderr.read().decode()[:400]}")

    try:
        choreograph(address)
    finally:
        time.sleep(0.4)
        rec.send_signal(signal.SIGINT)
        rec.wait(timeout=20)

    LOG.dump(OUT / "events.json")
    print(f"\nwrote {raw} and {OUT / 'events.json'}")


if __name__ == "__main__":
    if "--calibrate" in sys.argv:
        calibrate()
    elif "--dry" in sys.argv:
        preflight("dry")
        RESTORE.snapshot()
        atexit.register(RESTORE)
        st = read_state()
        address = st.get("device", {}).get("address")
        if not address or not st.get("device", {}).get("connected"):
            sys.exit("airpods must be connected before a dry run")
        reset_controls()
        stage_bar()
        stage_look()
        bluetooth("disconnect", address)
        wait_state(
            "AirPods disconnection before the dry run",
            lambda s: s.get("device", {}).get("connected") is False,
            timeout=10,
        )
        choreograph(address, wait_for_charge=False)
        LOG.dump(OUT / "events-dry.json")
    else:
        record()
