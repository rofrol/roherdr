#!/usr/bin/env python3
"""Consult Claude via Claude Code, with usage logging and Herdr-job visibility."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time

CONSULT_DIR = Path(__file__).resolve().parent.parent / "consult-stats"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("prompt", nargs="*")
    parser.add_argument("-m", "--model", default=os.environ.get("CLAUDE_CONSULT_MODEL", "claude-sonnet-5-5"))
    parser.add_argument("-f", "--file", action="append", default=[], help="attach a file; - reads stdin")
    parser.add_argument("-t", "--timeout", type=int, default=int(os.environ.get("CLAUDE_CONSULT_TIMEOUT", "420")))
    args = parser.parse_args()
    if args.timeout <= 0:
        parser.error("Timeout must be positive")
    if not os.environ.get("CONSULT_IN_JOB") and os.environ.get("HERDR_SOCKET_PATH") and shutil.which("herdr-job"):
        wrapper = CONSULT_DIR / "in_herdr_job.sh"
        os.execv(str(wrapper), [str(wrapper), f"claude {args.model}", str(Path(__file__).resolve()), *sys.argv[1:]])
    prompt = " ".join(args.prompt)
    for name in args.file:
        if name == "-":
            saved = os.environ.get("CONSULT_STDIN")
            text = Path(saved).read_text() if saved else sys.stdin.read()
        else:
            text = Path(name).read_text()
        prompt += f"\n\n--- {'stdin' if name == '-' else name} ---\n{text}"
    if not prompt.strip():
        parser.error("Empty prompt")
    start = time.monotonic()
    status, answer, usage, version = "error", "", {}, ""
    rc = 1
    try:
        # No repository context, tools, MCP servers, or persisted child session.
        with tempfile.TemporaryDirectory(prefix="consult-claude-") as cwd:
            result = subprocess.run([
                "claude", "-p", "--model", args.model, "--tools", "",
                "--strict-mcp-config", "--mcp-config", '{"mcpServers":{}}',
                "--no-session-persistence", "--output-format", "json",
            ], input=prompt, text=True, capture_output=True, cwd=cwd, timeout=args.timeout)
        if result.stderr:
            print(result.stderr, end="", file=sys.stderr)
        rc = result.returncode
        payload = json.loads(result.stdout)
        usage = payload.get("usage") or {}
        models = payload.get("modelUsage") or {}
        if len(models) == 1:
            key = next(iter(models))
            version = models[key].get("canonicalModel") or key
        answer = payload.get("result") or ""
        if rc == 0 and not payload.get("is_error") and payload.get("subtype") == "success" and answer:
            status = "ok"
            print(answer)
        else:
            rc = rc or 1
            print(answer or "Claude returned no successful answer", file=sys.stderr)
    except (OSError, subprocess.SubprocessError, ValueError) as error:
        print(f"Claude consultation failed: {error}", file=sys.stderr)
        rc = 1
    finally:
        log = [str(CONSULT_DIR / "consult.py"), "log", "--skill", "claude", "--model", args.model,
               "--status", status, "--seconds", str(int(time.monotonic() - start)),
               "--prompt-chars", str(len(prompt)), "--answer-chars", str(len(answer))]
        if version:
            log += ["--model-version", version]
        if usage:
            cached = usage.get("cache_read_input_tokens", 0)
            normalized = {"input": usage.get("input_tokens", 0) + usage.get("cache_creation_input_tokens", 0) + cached,
                          "cached": cached, "output": usage.get("output_tokens"),
                          "reasoning": (usage.get("output_tokens_details") or {}).get("thinking_tokens")}
            log += ["--usage", json.dumps(normalized), "--usage-raw", json.dumps(usage)]
        try:
            subprocess.run(log, timeout=10, check=False)
        except (OSError, subprocess.SubprocessError):
            pass  # Logging must not change the consultation outcome.
    return rc


if __name__ == "__main__":
    sys.exit(main())
