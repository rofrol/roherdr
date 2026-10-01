"""Drive a real herdr client in a PTY and render the fork demo frames.

Called by record.sh, which prepares an isolated server and session first.
Writes numbered PNG frames and an ffmpeg concat list with their durations.
"""

import argparse
import fcntl
import os
import pty
import select
import struct
import subprocess
import termios
import time

import pyte
from PIL import ImageDraw, ImageFont, Image

COLS, ROWS = 104, 30
CH = 21
PAD = 14
CAPTION_H = 40
DEFAULT_FG = (56, 58, 66)
DEFAULT_BG = (250, 250, 250)
NAMED = {
    "black": (56, 58, 66), "red": (228, 86, 73), "green": (80, 161, 79),
    "brown": (193, 132, 1), "yellow": (193, 132, 1), "blue": (64, 120, 242),
    "magenta": (166, 38, 164), "cyan": (1, 132, 188), "white": (160, 161, 167),
}
BOX = "─│┌┐└┘├┤┬┴┼"


def load_font(size, weight=None):
    """JetBrains Mono when installed (variable weight), otherwise Menlo."""
    path = os.environ.get("HERDR_DEMO_FONT") or os.path.expanduser(
        "~/Library/Fonts/JetBrainsMono[wght].ttf"
    )
    if os.path.exists(path):
        font = ImageFont.truetype(path, size)
        if weight:
            try:
                font.set_variation_by_axes([weight])
            except (OSError, ValueError):
                pass
        return font
    return ImageFont.truetype("/System/Library/Fonts/Menlo.ttc", size, index=1 if weight else 0)


FONT = load_font(15)
BOLD = load_font(15, 700)
CAPTION = load_font(16, 600)
SMALL = load_font(12)
SMALL_BOLD = load_font(12, 700)
CW = round(FONT.getlength("M"))


def color(value, default):
    if value in ("default", None):
        return default
    if len(value) == 6:
        try:
            return tuple(int(value[i:i + 2], 16) for i in (0, 2, 4))
        except ValueError:
            pass
    return NAMED.get(value.replace("bright", ""), default)


class Screen(pyte.Screen):
    # pyte rejects some private DSR/DA queries; the demo does not need replies.
    def report_device_status(self, *args, **kwargs):
        pass

    def report_device_attributes(self, *args, **kwargs):
        pass


def render(screen, caption=""):
    width = COLS * CW + 2 * PAD
    img = Image.new("RGB", (width, ROWS * CH + 2 * PAD + CAPTION_H), DEFAULT_BG)
    draw = ImageDraw.Draw(img)
    for y in range(ROWS):
        row = screen.buffer[y]
        for x in range(COLS):
            ch = row[x]
            fg = color(ch.fg, DEFAULT_FG)
            bg = color(ch.bg, DEFAULT_BG)
            if ch.reverse:
                fg, bg = bg, fg
            px, py = PAD + x * CW, PAD + y * CH
            if bg != DEFAULT_BG:
                draw.rectangle([px, py, px + CW - 1, py + CH - 1], fill=bg)
            if not ch.data.strip():
                continue
            if ch.data in BOX:
                # Draw box lines so borders connect regardless of font metrics.
                mx, my = px + CW // 2, py + CH // 2
                left, right = (px, my, mx, my), (mx, my, px + CW, my)
                up, down = (mx, py, mx, my), (mx, my, mx, py + CH)
                parts = {
                    "─": [left, right], "│": [up, down], "┌": [right, down],
                    "┐": [left, down], "└": [up, right], "┘": [up, left],
                    "├": [up, down, right], "┤": [up, down, left],
                    "┬": [left, right, down], "┴": [left, right, up],
                    "┼": [left, right, up, down],
                }[ch.data]
                for line in parts:
                    draw.line(line, fill=fg, width=1)
            elif ch.data in "█░":
                fill = fg if ch.data == "█" else tuple((a + b * 2) // 3 for a, b in zip(fg, bg))
                draw.rectangle([px, py + 4, px + CW - 1, py + CH - 5], fill=fill)
            else:
                draw.text((px, py + 2), ch.data, font=BOLD if ch.bold else FONT, fill=fg)
    top = ROWS * CH + 2 * PAD
    draw.rectangle([0, top, width, top + CAPTION_H], fill=(40, 44, 52))
    if caption:
        text_w = draw.textlength(caption, font=CAPTION)
        draw.text(((width - text_w) / 2, top + 10), caption, font=CAPTION, fill=(250, 250, 250))
    return img


def pointer(img, cell_y, cell_x, label=None):
    draw = ImageDraw.Draw(img)
    tx, ty = PAD + cell_x * CW + CW // 2, PAD + cell_y * CH + CH // 2
    arrow = [(tx, ty), (tx, ty + 18), (tx + 5, ty + 13), (tx + 9, ty + 21),
             (tx + 12, ty + 19), (tx + 8, ty + 12), (tx + 14, ty + 12)]
    draw.polygon(arrow, fill=(20, 20, 20), outline=(255, 255, 255))
    if label:
        w = draw.textlength(label, font=SMALL_BOLD) + 12
        box = [tx + 16, ty + 16, tx + 16 + w, ty + 34]
        draw.rounded_rectangle(box, radius=5, fill=(64, 120, 242))
        draw.text((box[0] + 6, box[1] + 2), label, font=SMALL_BOLD, fill=(255, 255, 255))
    return img


NOTE_W, NOTE_H = 330, 74


def notification(img, title, subtitle, body):
    """Illustrate the macOS banner, which is drawn outside the terminal."""
    draw = ImageDraw.Draw(img)
    x1 = img.width - PAD - NOTE_W - 6
    y1 = PAD + 30
    draw.rounded_rectangle([x1 + 2, y1 + 3, x1 + NOTE_W + 2, y1 + NOTE_H + 3], radius=12, fill=(210, 210, 214))
    draw.rounded_rectangle([x1, y1, x1 + NOTE_W, y1 + NOTE_H], radius=12, fill=(236, 236, 240), outline=(200, 200, 206))
    draw.rounded_rectangle([x1 + 12, y1 + 14, x1 + 44, y1 + 46], radius=8, fill=(40, 44, 52))
    draw.text((x1 + 18, y1 + 20), ">_", font=SMALL_BOLD, fill=(250, 250, 250))
    draw.text((x1 + 56, y1 + 10), title, font=SMALL_BOLD, fill=(30, 30, 30))
    draw.text((x1 + 56, y1 + 28), subtitle, font=SMALL, fill=(60, 60, 60))
    draw.text((x1 + 56, y1 + 46), body, font=SMALL, fill=(60, 60, 60))
    return x1 + NOTE_W // 2, y1 + NOTE_H // 2


class Recorder:
    def __init__(self, herdr, socket_path, out_dir):
        self.herdr = herdr
        self.socket_path = socket_path
        self.out_dir = out_dir
        self.frames = []
        env = {k: v for k, v in os.environ.items() if not k.startswith("HERDR_")}
        env.update(HERDR_SOCKET_PATH=socket_path, TERM="xterm-256color", COLORTERM="truecolor")
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            os.execve(herdr, [herdr], env)
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
        self.screen = Screen(COLS, ROWS)
        self.stream = pyte.ByteStream(self.screen)

    def cli(self, *args):
        env = {k: v for k, v in os.environ.items() if not k.startswith("HERDR_")}
        env["HERDR_SOCKET_PATH"] = self.socket_path
        subprocess.run([self.herdr, *args], env=env, capture_output=True, check=True)

    def pump(self, seconds):
        end = time.time() + seconds
        while time.time() < end:
            ready, _, _ = select.select([self.fd], [], [], 0.05)
            if ready:
                try:
                    self.stream.feed(os.read(self.fd, 65536))
                except OSError:
                    return

    def add(self, img, hold_ms):
        name = f"frame-{len(self.frames):02d}.png"
        img.save(os.path.join(self.out_dir, name))
        self.frames.append((name, hold_ms))

    def shot(self, caption, hold_ms):
        self.add(render(self.screen, caption), hold_ms)

    def mouse(self, y, x, button):
        """Press and release an SGR mouse button (0 left, 1 middle) at a cell."""
        os.write(self.fd, f"\x1b[<{button};{x + 1};{y + 1}M".encode())
        self.pump(0.05)
        os.write(self.fd, f"\x1b[<{button};{x + 1};{y + 1}m".encode())

    def key(self, data):
        os.write(self.fd, data)

    def find(self, predicate):
        for y, line in enumerate(self.screen.display):
            found = predicate(y, line)
            if found is not None:
                return found
        raise SystemExit("demo target not found; screen:\n" + "\n".join(self.screen.display))

    def confirm_if_asked(self, caption):
        if any("confirm" in line for line in self.screen.display):
            self.shot(caption, 1300)
            self.key(b"\r")
            self.pump(1.0)

    def finish(self):
        os.kill(self.pid, 9)
        # ffmpeg's concat demuxer ignores the last duration, so repeat the final frame.
        with open(os.path.join(self.out_dir, "frames.txt"), "w") as listing:
            for name, hold_ms in self.frames:
                listing.write(f"file '{name}'\nduration {hold_ms / 1000}\n")
            listing.write(f"file '{self.frames[-1][0]}'\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--herdr", required=True)
    parser.add_argument("--socket", required=True)
    parser.add_argument("--out-dir", required=True)
    parser.add_argument("--agent-pane", required=True)
    args = parser.parse_args()

    rec = Recorder(args.herdr, args.socket, args.out_dir)
    rec.pump(6)
    rec.shot("herdr fork: usage widget, middle-click close, clickable notifications", 2200)

    cap = "Click the usage footer to see limits and reset times"
    fy, fx = rec.find(lambda y, line: (y, 1) if line.startswith(" AN ") and "%" in line[:24] else None)
    rec.add(pointer(render(rec.screen, cap), fy, fx + 4), 1100)
    rec.mouse(fy, fx, 0)
    rec.pump(1.5)
    rec.shot(cap, 3200)
    rec.key(b"\x1b")
    rec.pump(0.8)

    cap = "Middle-click a tab to close it"
    ty, tx = rec.find(lambda y, line: (y, line.index("logs") + 1) if y == 0 and "logs" in line else None)
    rec.add(pointer(render(rec.screen, cap), ty, tx, "middle click"), 1400)
    rec.mouse(ty, tx, 1)
    rec.pump(1.0)
    rec.confirm_if_asked(cap)
    rec.shot(cap, 1500)

    cap = "Middle-click a space to close it"
    sy, sx = rec.find(lambda y, line: (y, line.index("notes") + 1) if "notes" in line[:24] else None)
    rec.add(pointer(render(rec.screen, cap), sy, sx, "middle click"), 1400)
    rec.mouse(sy, sx, 1)
    rec.pump(1.0)
    rec.confirm_if_asked(cap)
    rec.shot(cap, 1500)

    cap = "An agent finishes in another tab and macOS shows a notification"
    rec.cli("pane", "report-agent", "--source", "demo", "--agent", "claude", "--state", "idle", args.agent_pane)
    rec.pump(1.5)
    img = render(rec.screen, cap)
    notification(img, "claude finished", "herdr · agent", "Fix the login bug")
    rec.add(img, 2200)
    cap = "Click it to jump straight to that agent's tab"
    img = render(rec.screen, cap)
    nx, ny = notification(img, "claude finished", "herdr · agent", "Fix the login bug")
    rec.add(pointer(img, (ny - PAD) // CH, (nx - PAD) // CW), 1300)
    # A notification click runs exactly this against the session socket.
    rec.cli("agent", "focus", args.agent_pane)
    rec.pump(1.5)
    rec.shot(cap, 3200)
    rec.finish()
    print(f"recorded {len(rec.frames)} frames")


if __name__ == "__main__":
    main()
