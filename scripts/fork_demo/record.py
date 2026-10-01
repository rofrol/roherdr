"""Record the fork demo from a real Ghostty window driven by real input.

Called by record.sh, which prepares an isolated herdr server and session first.
Opens a Ghostty window in the top-right corner of the screen that runs
proxy.py (the herdr client plus a parsed copy of its screen), moves and clicks
the real mouse there with Quartz events, captures that part of the screen with
ffmpeg, and adds a caption bar afterwards.
"""

import argparse
import json
import os
import re
import socket
import subprocess
import time

import AppKit
import Quartz
from PIL import Image, ImageDraw, ImageFont

WINDOW_TITLE = "herdr demo"
PADDING = 8
CAPTION_H = 80
CAPTION_BG = (40, 44, 52)
KEY_RETURN, KEY_ESCAPE = 36, 53


def caption_font():
    path = os.environ.get("HERDR_DEMO_FONT") or os.path.expanduser(
        "~/Library/Fonts/JetBrainsMono[wght].ttf"
    )
    if os.path.exists(path):
        font = ImageFont.truetype(path, 30)
        try:
            font.set_variation_by_axes([600])
        except (OSError, ValueError):
            pass
        return font
    return ImageFont.truetype("/System/Library/Fonts/Menlo.ttc", 30)


def ghostty_config(path, x, command):
    with open(path, "w") as config:
        config.write(
            "\n".join(
                [
                    "theme = Atom One Light",
                    "font-size = 14",
                    "window-width = 104",
                    "window-height = 30",
                    f"window-padding-x = {PADDING}",
                    f"window-padding-y = {PADDING}",
                    "window-padding-balance = false",
                    f"window-position-x = {x}",
                    "window-position-y = 0",
                    # In the config, not `-e`: Ghostty asks before running a
                    # command it was given on the command line through `open`.
                    "command = direct:" + " ".join(command),
                    "macos-titlebar-style = hidden",
                    f"title = {WINDOW_TITLE}",
                    "confirm-close-surface = false",
                    "quit-after-last-window-closed = true",
                    "window-save-state = never",
                    "mouse-hide-while-typing = false",
                    "",
                ]
            )
        )


def screen_device():
    """The avfoundation index of the main screen."""
    listing = subprocess.run(
        ["ffmpeg", "-hide_banner", "-f", "avfoundation", "-list_devices", "true", "-i", ""],
        capture_output=True, text=True,
    ).stderr
    found = re.search(r"\[(\d+)\] Capture screen 0", listing)
    if not found:
        raise SystemExit("no screen capture device:\n" + listing)
    return found.group(1)


def windows(owner):
    info = Quartz.CGWindowListCopyWindowInfo(
        Quartz.kCGWindowListOptionOnScreenOnly, Quartz.kCGNullWindowID
    )
    return [w for w in info if w.get("kCGWindowOwnerName") == owner]


def post_mouse(kind, point, button=Quartz.kCGMouseButtonLeft):
    event = Quartz.CGEventCreateMouseEvent(None, kind, point, button)
    Quartz.CGEventPost(Quartz.kCGHIDEventTap, event)


class Recorder:
    def __init__(self, args):
        self.args = args
        self.captions = []
        self.overlays = []
        self.capture = None
        self.t0 = None
        self.scale = AppKit.NSScreen.mainScreen().backingScaleFactor()

    # --- the herdr CLI and the client's screen -------------------------------

    def cli(self, *args):
        env = {k: v for k, v in os.environ.items() if not k.startswith("HERDR_")}
        env["HERDR_SOCKET_PATH"] = self.args.socket
        done = subprocess.run([self.args.herdr, *args], env=env, capture_output=True, check=True)
        return json.loads(done.stdout or "null")

    def screen(self):
        with socket.socket(socket.AF_UNIX) as conn:
            conn.connect(self.args.control)
            conn.sendall(b"screen\n")
            return json.loads(conn.makefile().readline())

    @property
    def display(self):
        return self.screen()["display"]

    def find(self, predicate, seconds=5):
        end = time.time() + seconds
        while True:
            display = self.display
            for y, line in enumerate(display):
                found = predicate(y, line)
                if found is not None:
                    return found
            if time.time() > end:
                raise SystemExit("demo target not found; screen:\n" + "\n".join(display))
            time.sleep(0.2)

    def wait_for_usage(self, seconds):
        """Wait until no usage footer row is still loading (agy /quota is slow)."""
        end = time.time() + seconds
        while time.time() < end:
            rows = [line[:24] for line in self.display]
            if any(r.startswith(" AN ") for r in rows) and not any("…" in r for r in rows):
                return
            time.sleep(1)

    # --- the Ghostty window ---------------------------------------------------

    def launch(self):
        """Open the window at the top-left to learn its width, then again in
        the top-right corner, where macOS shows notification banners."""
        self.open_window(0)
        width = self.window()["kCGWindowBounds"]["Width"]
        self.close()
        while self.window() or os.path.exists(self.args.control):
            if os.path.exists(self.args.control) and not self.window():
                os.unlink(self.args.control)
            time.sleep(0.2)
        screen_width = AppKit.NSScreen.mainScreen().frame().size.width
        self.open_window(int(screen_width - width))

    def open_window(self, x):
        config = os.path.join(self.args.work, "ghostty.conf")
        proxy = os.path.join(os.path.dirname(os.path.abspath(__file__)), "proxy.py")
        # A Ghostty started by `open` has launchd's environment: pass on PATH
        # (for terminal-notifier) and the demo's config and state directories.
        # Ghostty splits `command` on spaces, so PATH entries with one are left out.
        path = ":".join(d for d in os.environ["PATH"].split(":") if " " not in d)
        env = [f"PATH={path}"] + [
            f"{name}={os.environ[name]}" for name in ("XDG_CONFIG_HOME", "XDG_STATE_HOME")
        ]
        ghostty_config(config, x, [
            "/usr/bin/env", *env, self.args.python, proxy, "--herdr", self.args.herdr, "--socket", self.args.socket,
            "--control", self.args.control, "--vt-lib", self.args.vt_lib,
        ])
        subprocess.run(
            ["open", "-na", "Ghostty", "--args", "--config-default-files=false",
             f"--config-file={config}"],
            check=True,
        )
        end = time.time() + 60
        while not (os.path.exists(self.args.control) and self.window()):
            if time.time() > end:
                if self.window():
                    self.pid = self.window()["kCGWindowOwnerPID"]
                raise SystemExit(
                    f"the demo Ghostty window did not open (control socket: "
                    f"{os.path.exists(self.args.control)}, Ghostty windows: "
                    f"{[w.get('kCGWindowName') for w in windows('Ghostty')]})"
                )
            time.sleep(0.2)
        time.sleep(1)
        bounds = self.window()["kCGWindowBounds"]
        self.pid = self.window()["kCGWindowOwnerPID"]
        self.origin = (bounds["X"], bounds["Y"])
        self.size = (bounds["Width"], bounds["Height"])
        geometry = self.screen()
        self.cell = (
            geometry["xpixel"] // geometry["cols"] / self.scale,
            geometry["ypixel"] // geometry["rows"] / self.scale,
        )
        self.activate()

    def window(self):
        found = [w for w in windows("Ghostty") if w.get("kCGWindowName") == WINDOW_TITLE]
        return found[0] if found else None

    def activate(self):
        app = AppKit.NSRunningApplication.runningApplicationWithProcessIdentifier_(self.pid)
        app.activateWithOptions_(AppKit.NSApplicationActivateIgnoringOtherApps)
        time.sleep(0.3)

    def close(self):
        if self.pid:
            try:
                os.kill(self.pid, 15)
            except ProcessLookupError:
                pass

    # --- real input -----------------------------------------------------------

    def point(self, cell_y, cell_x):
        return (
            self.origin[0] + PADDING + (cell_x + 0.5) * self.cell[0],
            self.origin[1] + PADDING + (cell_y + 0.5) * self.cell[1],
        )

    def move(self, target, seconds=0.5):
        """Glide the pointer to a screen point, the way a hand would."""
        start = Quartz.CGEventGetLocation(Quartz.CGEventCreate(None))
        steps = max(1, int(seconds * 60))
        for i in range(1, steps + 1):
            t = i / steps
            t = t * t * (3 - 2 * t)
            post_mouse(Quartz.kCGEventMouseMoved,
                       (start.x + (target[0] - start.x) * t, start.y + (target[1] - start.y) * t))
            time.sleep(seconds / steps)

    def move_to(self, cell_y, cell_x, seconds=0.5):
        self.move(self.point(cell_y, cell_x), seconds)

    def click(self, middle=False):
        where = Quartz.CGEventGetLocation(Quartz.CGEventCreate(None))
        if middle:
            post_mouse(Quartz.kCGEventOtherMouseDown, where, Quartz.kCGMouseButtonCenter)
            time.sleep(0.06)
            post_mouse(Quartz.kCGEventOtherMouseUp, where, Quartz.kCGMouseButtonCenter)
        else:
            post_mouse(Quartz.kCGEventLeftMouseDown, where)
            time.sleep(0.06)
            post_mouse(Quartz.kCGEventLeftMouseUp, where)

    def key(self, code):
        for down in (True, False):
            Quartz.CGEventPost(Quartz.kCGHIDEventTap, Quartz.CGEventCreateKeyboardEvent(None, code, down))
            time.sleep(0.03)

    def confirm_if_asked(self):
        time.sleep(0.8)
        if any("confirm" in line for line in self.display):
            time.sleep(1.2)
            self.key(KEY_RETURN)

    # --- capture and captions -------------------------------------------------

    def start_capture(self):
        s = self.scale
        x, y = int(self.origin[0] * s) // 2 * 2, int(self.origin[1] * s) // 2 * 2
        w, h = int(self.size[0] * s) // 2 * 2, int(self.size[1] * s) // 2 * 2
        self.raw = os.path.join(self.args.work, "raw.mov")
        self.capture = subprocess.Popen(
            ["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-f", "avfoundation",
             "-use_wallclock_as_timestamps", "1",
             "-capture_cursor", "1", "-capture_mouse_clicks", "1", "-framerate", "30",
             "-pixel_format", "bgr0", "-i", f"{screen_device()}:none",
             "-vf", f"crop={w}:{h}:{x}:{y}", "-c:v", "h264_videotoolbox", "-b:v", "24M",
             "-copyts", "-fps_mode", "vfr", self.raw],
            stdin=subprocess.PIPE,
        )
        # avfoundation needs a moment before the first frame arrives.
        time.sleep(1.5)

    def caption(self, text):
        """Show `text` from now on; times are wall clock, matched to the
        capture's wall-clock timestamps in encode()."""
        now = time.time()
        if self.captions:
            self.captions[-1][1] = now
        self.captions.append([now, None, text])

    def card_center(self):
        """Screen point at the middle of the drawn notification card."""
        width_px = int(self.size[0] * self.scale) // 2 * 2
        cx = width_px - NOTE_MARGIN - NOTE_W / 2
        cy = NOTE_MARGIN + NOTE_H / 2
        return (self.origin[0] + cx / self.scale, self.origin[1] + cy / self.scale)

    def stop_capture(self):
        self.captions[-1][1] = time.time()
        time.sleep(0.5)
        self.capture.communicate(b"q")

    def capture_start(self):
        """Wall-clock time of the first captured frame (kept by -copyts)."""
        probe = subprocess.run(
            ["ffprobe", "-v", "error", "-show_entries", "format=start_time", "-of", "csv=p=0", self.raw],
            capture_output=True, text=True, check=True,
        )
        return float(probe.stdout.strip())

    def encode(self, out):
        """Add a caption bar below the capture and encode the final MP4."""
        width = int(self.size[0] * self.scale) // 2 * 2
        t0 = self.capture_start()
        font = caption_font()
        inputs, overlays = [], []
        chain = f"[0:v]setpts=PTS-STARTPTS,pad=iw:ih+{CAPTION_H}:0:0:color=#{bytes(CAPTION_BG).hex()}[v0]"
        for i, (start, end, text) in enumerate(self.captions):
            png = os.path.join(self.args.work, f"caption-{i:02d}.png")
            img = Image.new("RGBA", (width, CAPTION_H), CAPTION_BG + (255,))
            draw = ImageDraw.Draw(img)
            text_w = draw.textlength(text, font=font)
            draw.text(((width - text_w) / 2, 20), text, font=font, fill=(250, 250, 250))
            img.save(png)
            inputs += ["-i", png]
            overlays.append(
                f"[v{i}][{i + 1}:v]overlay=0:H-{CAPTION_H}"
                f":enable='between(t,{start - t0:.2f},{end - t0:.2f})'[v{i + 1}]"
            )
        last = len(self.captions)
        for j, (start, end, card) in enumerate(self.overlays):
            png = os.path.join(self.args.work, f"card-{j:02d}.png")
            card.save(png)
            inputs += ["-i", png]
            overlays.append(
                f"[v{last}][{last + 1}:v]overlay={width - NOTE_MARGIN - NOTE_W}:{NOTE_MARGIN}"
                f":enable='between(t,{start - t0:.2f},{end - t0:.2f})'[v{last + 1}]"
            )
            last += 1
        graph = ";".join([chain, *overlays])
        subprocess.run(
            ["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-i", self.raw, *inputs,
             "-filter_complex", graph, "-map", f"[v{last}]",
             "-r", "30", "-c:v", "libx264", "-crf", "20", "-pix_fmt", "yuv420p", "-movflags", "+faststart", out],
            check=True,
        )


NOTE_W, NOTE_H, NOTE_MARGIN = 660, 148, 24


def notification_card(title, subtitle, body):
    """The macOS banner, drawn onto the video: macOS hides notifications while
    the screen is recorded. Sizes are in capture pixels (2x)."""
    card = Image.new("RGBA", (NOTE_W + 8, NOTE_H + 10), (0, 0, 0, 0))
    draw = ImageDraw.Draw(card)
    draw.rounded_rectangle([4, 6, NOTE_W + 4, NOTE_H + 6], radius=26, fill=(0, 0, 0, 40))
    draw.rounded_rectangle([0, 0, NOTE_W, NOTE_H], radius=26, fill=(238, 238, 242, 255),
                           outline=(205, 205, 210, 255), width=2)
    draw.rounded_rectangle([26, 32, 90, 96], radius=16, fill=(40, 44, 52, 255))
    small, bold = note_fonts()
    draw.text((38, 44), ">_", font=bold, fill=(250, 250, 250))
    draw.text((112, 20), title, font=bold, fill=(30, 30, 30))
    draw.text((112, 56), subtitle, font=small, fill=(60, 60, 60))
    draw.text((112, 92), body, font=small, fill=(60, 60, 60))
    return card


def note_fonts():
    path = "/System/Library/Fonts/SFNS.ttf"
    if not os.path.exists(path):
        return caption_font(), caption_font()
    small = ImageFont.truetype(path, 26)
    bold = ImageFont.truetype(path, 26)
    try:
        bold.set_variation_by_axes([700])
    except (OSError, ValueError, AttributeError):
        pass
    return small, bold


def scenes(rec, args):
    rec.caption("herdr fork: usage widget, middle-click close, notifications, job tabs, oracle stats")
    time.sleep(2.4)

    rec.caption("Click the usage footer to see limits and reset times")
    # Any provider row with numbers: one that failed to load shows `!` instead.
    fy, fx = rec.find(lambda y, line: (y, 5) if line[:4] in (" AN ", " OA ", " GO ") and "%" in line[:24] else None)
    rec.move_to(fy, fx)
    time.sleep(0.4)
    rec.click()
    time.sleep(3.4)
    rec.key(KEY_ESCAPE)
    time.sleep(0.8)

    rec.caption("Middle-click a tab to close it")
    ty, tx = rec.find(lambda y, line: (y, line.index("logs") + 1) if y == 0 and "logs" in line else None)
    rec.move_to(ty, tx)
    time.sleep(0.5)
    rec.click(middle=True)
    rec.confirm_if_asked()
    time.sleep(1.6)

    rec.caption("Middle-click a space to close it")
    sy, sx = rec.find(lambda y, line: (y, line.index("notes") + 1) if "notes" in line[:24] else None)
    rec.move_to(sy, sx)
    time.sleep(0.5)
    rec.click(middle=True)
    rec.confirm_if_asked()
    time.sleep(1.6)

    rec.caption("An agent finishes in another tab and macOS shows a notification")
    rec.cli("pane", "report-agent", "--source", "demo", "--agent", "claude", "--state", "idle", args.agent_pane)
    time.sleep(1.0)
    shown = time.time()
    time.sleep(2.2)
    rec.caption("Click it to jump straight to that agent's tab")
    rec.move(rec.card_center(), 0.7)
    time.sleep(0.6)
    # The card is drawn, so there is nothing to click: run what a click on the
    # real banner runs. A real click here would land on herdr's tab bar.
    rec.cli("agent", "focus", args.agent_pane)
    rec.overlays.append((shown, time.time(), notification_card(
        "claude finished", "herdr · agent", "Fix the login bug")))
    time.sleep(0.4)
    rec.move_to(14, 70, 0.5)
    time.sleep(2.4)

    # herdr-job does this for a long command: a child tab of the agent's tab
    # whose status says how the job is going.
    rec.caption("A long job runs in a child tab; the space row counts it while the agent is idle")
    for label, status in (("build", "running"), ("tests", "failed")):
        tab = rec.cli("tab", "create", "--workspace", args.workspace, "--label", label, "--no-focus")
        tab_id = tab["result"]["tab"]["tab_id"]
        rec.cli("tab", "parent", tab_id, args.agent_tab)
        rec.cli("tab", "status", tab_id, status)
        time.sleep(2.2)
    jy, jx = rec.find(lambda y, line: (y, line.index("⧖")) if "herdr" in line[:24] and "⧖" in line[:24] else None)
    # Point at the counter from below: the arrow would cover it otherwise.
    rec.move_to(jy + 1, jx)
    time.sleep(2.8)

    rec.caption("The herdr menu opens oracle stats: which second-opinion models helped")
    my, mx = rec.find(lambda y, line: (y, line.index("menu") + 1) if "menu" in line[:26] else None)
    rec.move_to(my, mx)
    time.sleep(0.4)
    rec.click()
    oy, ox = rec.find(lambda y, line: (y, line.index("oracle stats") + 2) if "oracle stats" in line else None)
    rec.move_to(oy, ox)
    time.sleep(0.4)
    rec.click()
    time.sleep(4.2)
    rec.caption("The background dims behind the popup; a click outside closes it")
    rec.move_to(1, 3, 0.7)
    time.sleep(0.8)
    rec.click()
    time.sleep(1.8)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--herdr", required=True)
    parser.add_argument("--socket", required=True)
    parser.add_argument("--work", required=True)
    parser.add_argument("--vt-lib", required=True, help="libghostty-vt shared library")
    parser.add_argument("--python", required=True, help="python for proxy.py inside Ghostty")
    parser.add_argument("--agent-pane", required=True)
    parser.add_argument("--agent-tab", required=True)
    parser.add_argument("--workspace", required=True)
    parser.add_argument("--out", required=True)
    args = parser.parse_args()
    args.control = os.path.join(args.work, "control.sock")

    rec = Recorder(args)
    rec.pid = None
    try:
        rec.launch()
        rec.wait_for_usage(90)
        # macOS does not repaint a window that was covered; give it a moment
        # in front before the capture starts.
        rec.activate()
        time.sleep(2)
        rec.move_to(12, 60, 0.3)
        rec.start_capture()
        scenes(rec, args)
        rec.stop_capture()
    finally:
        if rec.capture and rec.capture.poll() is None:
            rec.capture.communicate(b"q")
        rec.close()
    rec.encode(args.out)
    print(f"wrote {args.out}")


if __name__ == "__main__":
    main()
