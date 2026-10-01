"""Run the herdr client inside the demo's Ghostty window and report its screen.

Ghostty runs this as its command. It starts the client in a pseudo-terminal of
the window's size, copies bytes both ways, and parses the client's output with
libghostty-vt. record.py connects to --control and sends one request per
line: `screen` answers with the screen text and the window's cell geometry.
"""

import argparse
import fcntl
import json
import os
import pty
import select
import signal
import socket
import struct
import sys
import termios
import tty

import ghostty_vt


def window_size(fd):
    rows, cols, xpixel, ypixel = struct.unpack("HHHH", fcntl.ioctl(fd, termios.TIOCGWINSZ, b"\0" * 8))
    return rows, cols, xpixel, ypixel


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--herdr", required=True)
    parser.add_argument("--socket", required=True, help="herdr session socket")
    parser.add_argument("--control", required=True, help="unix socket for record.py")
    parser.add_argument("--vt-lib", required=True)
    args = parser.parse_args()

    stdin = sys.stdin.fileno()
    rows, cols, xpixel, ypixel = window_size(stdin)
    screen = ghostty_vt.Screen(args.vt_lib, cols, rows)
    pid, child = pty.fork()
    if pid == 0:
        env = {k: v for k, v in os.environ.items() if not k.startswith("HERDR_")}
        env.update(HERDR_SOCKET_PATH=args.socket, COLORTERM="truecolor")
        os.execve(args.herdr, [args.herdr], env)
    fcntl.ioctl(child, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, xpixel, ypixel))

    def on_resize(*_):
        nonlocal rows, cols, xpixel, ypixel
        rows, cols, xpixel, ypixel = window_size(stdin)
        fcntl.ioctl(child, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, xpixel, ypixel))
        screen.resize(cols, rows)
        os.kill(pid, signal.SIGWINCH)

    signal.signal(signal.SIGWINCH, on_resize)
    control = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    control.bind(args.control)
    control.listen()
    clients = []
    saved = termios.tcgetattr(stdin)
    tty.setraw(stdin)
    try:
        while True:
            try:
                ready, _, _ = select.select([stdin, child, control, *clients], [], [])
            except InterruptedError:
                continue
            if child in ready:
                try:
                    data = os.read(child, 65536)
                except OSError:
                    break
                if not data:
                    break
                os.write(sys.stdout.fileno(), data)
                screen.feed(data)
            if stdin in ready:
                os.write(child, os.read(stdin, 65536))
            if control in ready:
                clients.append(control.accept()[0])
            for client in [c for c in clients if c in ready]:
                request = client.recv(4096)
                if not request:
                    clients.remove(client)
                    client.close()
                    continue
                answer = {
                    "display": screen.display,
                    "cols": cols,
                    "rows": rows,
                    "xpixel": xpixel,
                    "ypixel": ypixel,
                }
                client.sendall(json.dumps(answer).encode() + b"\n")
    finally:
        termios.tcsetattr(stdin, termios.TCSADRAIN, saved)


if __name__ == "__main__":
    main()
