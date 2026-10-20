#!/usr/bin/env python3
"""Ask MiMo-V2.6-Pro over the user's xiaomi token plan (`xiaomi-token-plan-ams`),
the route the coordinator session itself runs on. Single-shot: the prompt text
is the only argument, the answer goes to stdout, and the call is logged for
consult-stats (whose `log` prints "[consult id: ...]" on stderr, which
consult.py round parses).

Why not OpenRouter: the user asked (2026-10-20, "zmien ze do konsultacji jest
uzywany mimo z planu ams") that consultations use mimo from the ams token
plan, the same subscription the session runs on. The paid `xiaomi` route
answers 402 (insufficient balance) and OpenRouter is a separate balance with
its own secret vet.

Usage: ask_mimo_ams.py "PROMPT"
"""
import argparse
import os
import subprocess
import sys
import time
from pathlib import Path

CONSULT_DIR = Path(__file__).resolve().parent.parent / "consult-stats"  # the skills live side by side
CONSULT = CONSULT_DIR / "consult.py"
# External deadline: a bound on one external LLM API call, the same kind of
# bound ask_deepseek.py carries (420 s there); it never waits for an internal
# event. A hung call must not hang a whole consult round.
SECONDS = 900


def log_call(status, seconds, prompt_chars, answer_chars, error_text=""):
    """Record the call for consult-stats; never let logging fail the consultation."""
    try:
        args = [sys.executable, str(CONSULT), "log", "--skill", "mimo",
                "--model", "mimo-v2.6-pro-via-token-plan-ams", "--status", status,
                "--seconds", str(int(seconds)), "--prompt-chars", str(prompt_chars),
                "--answer-chars", str(answer_chars)]
        if error_text:
            args += ["--error-text-file", "-"]
            subprocess.run(args, input=error_text, text=True, timeout=30)
        else:
            subprocess.run(args, timeout=30)
    except Exception:
        pass


def main():
    p = argparse.ArgumentParser()
    p.add_argument("prompt", nargs="*")
    p.add_argument("--show-reasoning", action="store_true")
    a = p.parse_args()
    prompt = " ".join(a.prompt)
    if not prompt.strip():
        sys.exit("ask_mimo_ams.py: empty prompt")

    argv = ["pi", "--print", "--no-session", "--thinking", "high",
            "--provider", "xiaomi-token-plan-ams", "--model", "mimo-v2.6-pro", prompt]
    start = time.monotonic()
    try:
        proc = subprocess.run(argv, capture_output=True, text=True,
                              stdin=subprocess.DEVNULL, timeout=SECONDS)
    except subprocess.TimeoutExpired:
        log_call("error", time.monotonic() - start, len(prompt), 0, "timeout after %d s" % SECONDS)
        sys.exit("ask_mimo_ams.py: no answer within %d s" % SECONDS)
    except OSError as e:
        log_call("error", time.monotonic() - start, len(prompt), 0, str(e))
        sys.exit(f"ask_mimo_ams.py: {e}")

    answer = proc.stdout
    if proc.returncode != 0 or not answer.strip():
        log_call("error", time.monotonic() - start, len(prompt), len(answer),
                 proc.stderr[-4000:] or f"exit {proc.returncode}, empty answer")
        sys.exit(proc.stderr.strip() or f"ask_mimo_ams.py: pi exit {proc.returncode}, empty answer")
    log_call("ok", time.monotonic() - start, len(prompt), len(answer))
    print(answer.rstrip())


if __name__ == "__main__":
    main()
