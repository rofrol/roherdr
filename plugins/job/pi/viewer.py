#!/usr/bin/env python3
"""User-opened, plain-text view of one saved Pi tool call; never executes it."""
import argparse
import json
import os
from pathlib import Path
import re
import runpy

MAX_LINE = 8 * 1024 * 1024
ANSI = re.compile(r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b\[[0-?]*[ -/]*[@-~]|\x1b[@-_]")
CONTROL = re.compile(r"[\x00-\x08\x0b-\x1f\x7f\u202a-\u202e\u2066-\u2069]")


def safe_text(value):
    return CONTROL.sub("", ANSI.sub("", str(value))).replace("\r", "")


def selected_payload(entry, call_id):
    message = entry.get("message")
    if not isinstance(message, dict):
        return None
    if message.get("role") == "assistant":
        content = message.get("content")
        if not isinstance(content, list):
            return None
        for block in content:
            if isinstance(block, dict) and block.get("type") == "toolCall" and block.get("id") == call_id:
                progress = []
                for part in content:
                    if not isinstance(part, dict):
                        continue
                    if part.get("type") == "thinking" and not part.get("redacted") and isinstance(part.get("thinking"), str):
                        progress.append("Provider-exposed thinking:\n" + part["thinking"])
                progress.append("Tool input: " + str(block.get("name", "tool")) + "\n" + json.dumps(block.get("arguments"), ensure_ascii=False, indent=2))
                return False, "\n\n".join(progress)
    if message.get("role") == "toolResult" and message.get("toolCallId") == call_id:
        parts = []
        content = message.get("content")
        for block in content if isinstance(content, list) else []:
            if isinstance(block, dict) and block.get("type") == "text":
                parts.append(str(block.get("text", "")))
            elif isinstance(block, dict) and block.get("type") == "image":
                parts.append("[Image retained in the original Pi transcript]")
        label = "Tool failed" if message.get("isError") else "Tool result"
        return True, label + "\n" + "\n".join(parts)
    return None


def file_watch(session, stream):
    """Herdr-job's kqueue/inotify watch on the session file and its directory."""
    job = runpy.run_path(str(Path(__file__).resolve().parent.parent / "herdr-job"))
    watch = job["make_dir_watch"](session.parent)
    watch.watch_file(stream.fileno())
    return watch, job["TextSourceError"]


def follow(session, call_id, output):
    """Shows the call's records as Pi saves them. Waits on file events for
    the next record, never on a pause or a deadline: it ends with the call's
    result, when the session file is removed or replaced, or when the user
    closes the viewer's job."""
    output.write("Pi transcript detail viewer (not the tool executor).\n"
                 "Only this call is shown. Original data stays in the saved session.\n"
                 "Waiting for saved tool records; live partial output may not be persisted yet.\n\n")
    output.flush()
    with session.open(encoding="utf-8") as stream:
        identity = os.fstat(stream.fileno()).st_ino
        watch, source_error = file_watch(session, stream)
        try:
            return show_records(session, call_id, output, stream, identity, watch, source_error)
        finally:
            watch.close()


def show_records(session, call_id, output, stream, identity, watch, source_error):
    while True:
        offset = stream.tell()
        line = stream.readline(MAX_LINE + 1)
        if not line or not line.endswith("\n"):
            if len(line) > MAX_LINE:
                while line and not line.endswith("\n"):
                    line = stream.readline(MAX_LINE + 1)
                output.write("[Oversized session record omitted; expand it in Pi instead.]\n")
                output.flush()
                continue
            stream.seek(offset)
            try:
                if os.stat(session).st_ino != identity:
                    raise FileNotFoundError
            except FileNotFoundError:
                output.write("The session file was removed or replaced; expand the tool in Pi instead.\n")
                output.flush()
                return 1
            try:
                watch.wait()  # Pi appends the next record
            except source_error as err:
                output.write(f"Cannot follow the session file ({err}); expand the tool in Pi instead.\n")
                output.flush()
                return 1
            continue
        try:
            entry = json.loads(line)
        except ValueError:
            continue
        if not isinstance(entry, dict):
            continue
        payload = selected_payload(entry, call_id)
        if payload is not None:
            done, text = payload
            output.write(safe_text(text) + "\n\n")
            output.flush()
            if done:
                output.write("Detail view complete. Use the job footer to return to Pi.\n")
                output.flush()
                return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--session", type=Path, required=True)
    parser.add_argument("--call", required=True)
    args = parser.parse_args()
    path = os.environ.get("HERDR_JOB_TTY")
    if not path:
        parser.error("A Herdr job terminal is required")
    # Never duplicate transcript payload into the job log or stdout.
    with open(path, "w", encoding="utf-8") as terminal:
        if not os.isatty(terminal.fileno()):
            parser.error("The detail target must be a terminal")
        return follow(args.session, args.call, terminal)


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except KeyboardInterrupt:
        raise SystemExit(130)
