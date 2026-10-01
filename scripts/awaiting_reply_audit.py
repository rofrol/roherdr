#!/usr/bin/env python3
"""Audit how often agents forget `herdr agent awaiting-reply`.

An agent that ends a turn needing the user's answer is asked to run
`herdr agent awaiting-reply` as the last command of the turn, so herdr marks
its tab with `?`. This reads Claude Code and Pi session transcripts (JSONL),
splits them into turns, and counts, per model:

- turns whose final message looks like a question (a bilingual heuristic),
- turns in which the command was run (anywhere, and as the last tool call),
- misses: looks like a question but nothing was reported,
- false reports: reported but the final message does not look like a question,
- ordering violations: reported, but not as the last tool call of the turn.

The heuristic is a screen for review, not ground truth: the misses are listed
with the tail of the message so they can be judged by eye or by a model. Read
only; nothing is sent anywhere.

    scripts/awaiting_reply_audit.py ~/.claude/projects/*/*.jsonl \
        ~/.pi/agent/sessions/*/*.jsonl [--examples 10] [--json]
"""

import argparse
import collections
import json
import math
import re
import sys

COMMAND = "herdr agent awaiting-reply"

# Phrases that ask the user for a decision even without a question mark.
ASK_PHRASES = re.compile(
    r"\b("
    r"let me know|tell me|which (one|option|variant)|should i|shall i|"
    r"do you want|would you like|want me to|how would you like|"
    r"czy mam|czy chcesz|daj (mi )?zna[ćc]|powiedz|który wariant|którą opcję|"
    r"co wybierasz|jak wolisz|zainstalować|wypchnąć|zrobić"
    r")\b",
    re.IGNORECASE,
)
COURTESY = re.compile(
    r"(anything else|something else|coś jeszcze|czy mogę jeszcze w czymś pomóc"
    r"|let me know if you (need|have) anything)",
    re.IGNORECASE,
)


def strip_code(text):
    """Text without fenced code, inline code and quoted lines."""
    text = re.sub(r"```.*?```", " ", text, flags=re.DOTALL)
    text = re.sub(r"`[^`\n]*`", " ", text)
    lines = [line for line in text.splitlines() if not line.lstrip().startswith(">")]
    return "\n".join(lines)


def last_paragraph(text):
    paragraphs = [p.strip() for p in re.split(r"\n\s*\n", strip_code(text)) if p.strip()]
    return paragraphs[-1] if paragraphs else ""


def looks_like_question(text):
    """True when the final paragraph asks the user to decide or answer."""
    paragraph = last_paragraph(text)
    if not paragraph or COURTESY.search(paragraph):
        return False
    tail = paragraph[-400:]
    if re.search(r"\?[\s\)\"'»”*_]*$", tail):
        return True
    # A question mark in the last two sentences plus an ask phrase.
    sentences = re.split(r"(?<=[.!?])\s+", tail)
    last = " ".join(sentences[-2:])
    return "?" in last and bool(ASK_PHRASES.search(last))


def is_report(command):
    return command.strip() == COMMAND


class Turn:
    def __init__(self, model):
        self.model = model
        self.started = ""  # ISO timestamp of the user prompt
        self.final_text = ""
        self.commands = []  # shell commands in order
        self.last_tool_is_report = False
        self.tool_calls = 0

    def reported(self):
        return any(is_report(c) for c in self.commands)

    def reported_last(self):
        return self.last_tool_is_report


def text_of(content):
    if isinstance(content, str):
        return content
    parts = []
    for block in content or []:
        if isinstance(block, dict) and block.get("type") == "text":
            parts.append(block.get("text", ""))
    return "\n".join(parts)


INTERRUPT = ("[Request interrupted", "<system-reminder>", "Caveat:")


def claude_turns(lines):
    turns, current = [], None
    seen = set()
    for line in lines:
        try:
            entry = json.loads(line)
        except ValueError:
            continue
        if entry.get("isSidechain") or entry.get("isMeta"):
            continue
        uuid = entry.get("uuid")
        if uuid:
            if uuid in seen:
                continue
            seen.add(uuid)
        message = entry.get("message") or {}
        kind = entry.get("type")
        if kind == "user":
            content = message.get("content")
            if isinstance(content, list) and any(
                isinstance(b, dict) and b.get("type") == "tool_result" for b in content
            ):
                continue
            text = text_of(content).strip()
            if not text or text.startswith(INTERRUPT):
                continue
            current = Turn(None)
            current.started = str(entry.get("timestamp") or "")
            turns.append(current)
        elif kind == "assistant" and current is not None:
            current.model = message.get("model") or current.model
            for block in message.get("content") or []:
                if not isinstance(block, dict):
                    continue
                if block.get("type") == "tool_use":
                    current.tool_calls += 1
                    command = ""
                    if block.get("name") == "Bash":
                        command = (block.get("input") or {}).get("command", "")
                        current.commands.append(command)
                    current.last_tool_is_report = is_report(command)
                elif block.get("type") == "text" and block.get("text", "").strip():
                    current.final_text = block["text"]
    return [t for t in turns if t.final_text and t.model and t.model != "<synthetic>"]


def pi_turns(lines):
    turns, current, model = [], None, None
    for line in lines:
        try:
            entry = json.loads(line)
        except ValueError:
            continue
        if entry.get("type") == "model_change":
            model = "%s/%s" % (entry.get("provider"), entry.get("modelId"))
            continue
        if entry.get("type") != "message":
            continue
        message = entry.get("message") or {}
        role = message.get("role")
        if role == "user":
            text = text_of(message.get("content")).strip()
            if not text or text.startswith(INTERRUPT):
                continue
            current = Turn(model)
            current.started = str(entry.get("timestamp") or "")
            turns.append(current)
        elif role == "assistant" and current is not None:
            if message.get("provider") and message.get("model"):
                current.model = "%s/%s" % (message["provider"], message["model"])
            if message.get("stopReason") in ("error", "aborted"):
                current.final_text = ""
                continue
            for block in message.get("content") or []:
                if not isinstance(block, dict):
                    continue
                if block.get("type") in ("toolCall", "tool_use"):
                    current.tool_calls += 1
                    args = block.get("arguments") or block.get("input") or {}
                    command = ""
                    if block.get("name") in ("bash", "Bash"):
                        command = args.get("command", "") if isinstance(args, dict) else ""
                        current.commands.append(command)
                    current.last_tool_is_report = is_report(command)
                elif block.get("type") == "text" and block.get("text", "").strip():
                    current.final_text = block["text"]
    return [t for t in turns if t.final_text and t.model]


def load_turns(path):
    with open(path, encoding="utf-8", errors="replace") as handle:
        lines = handle.readlines()
    head = lines[0] if lines else ""
    if '"type": "session"' in head or '"type":"session"' in head:
        return pi_turns(lines)
    return claude_turns(lines)


def wilson(successes, total, z=1.96):
    if total == 0:
        return (0.0, 0.0)
    p = successes / total
    denom = 1 + z * z / total
    centre = p + z * z / (2 * total)
    margin = z * math.sqrt(p * (1 - p) / total + z * z / (4 * total * total))
    return ((centre - margin) / denom, (centre + margin) / denom)


def audit(paths, since=""):
    stats = collections.defaultdict(lambda: collections.Counter())
    misses = collections.defaultdict(list)
    for path in paths:
        try:
            turns = load_turns(path)
        except OSError:
            continue
        for turn in turns:
            if since and turn.started < since:
                continue
            question = looks_like_question(turn.final_text)
            reported = turn.reported()
            s = stats[turn.model]
            s["turns"] += 1
            s["question_like"] += question
            s["reported"] += reported
            s["reported_last"] += turn.reported_last()
            if question and not reported:
                s["missed"] += 1
                misses[turn.model].append(last_paragraph(turn.final_text)[-160:])
            if reported and not question:
                s["false_report"] += 1
            if reported and not turn.reported_last():
                s["order_violation"] += 1
    return stats, misses


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("paths", nargs="+")
    parser.add_argument("--examples", type=int, default=5, help="misses to print per model")
    parser.add_argument("--json", action="store_true")
    parser.add_argument(
        "--since",
        default="",
        help="only turns started at or after this ISO time, e.g. 2026-10-01T12:40 "
        "(skip sessions that never saw the instruction)",
    )
    parser.add_argument(
        "--min-turns", type=int, default=1, help="hide models with fewer turns"
    )
    parser.add_argument(
        "--false-reports", type=int, default=0, help="false reports to print per model"
    )
    args = parser.parse_args(argv)
    stats, misses = audit(args.paths, args.since)
    if args.json:
        print(json.dumps({m: dict(s) for m, s in stats.items()}, indent=2, sort_keys=True))
        return 0
    for model, s in sorted(stats.items(), key=lambda kv: -kv[1]["turns"]):
        if s["turns"] < args.min_turns:
            continue
        q = s["question_like"]
        lo, hi = wilson(s["missed"], q)
        print(
            "%-40s turns=%-4d question_like=%-3d reported=%-3d missed=%-3d "
            "(%.0f%%, 95%% CI %.0f-%.0f%%) false_report=%-3d order_violation=%d"
            % (
                model,
                s["turns"],
                q,
                s["reported"],
                s["missed"],
                100 * s["missed"] / q if q else 0,
                100 * lo,
                100 * hi,
                s["false_report"],
                s["order_violation"],
            )
        )
        for tail in misses[model][: args.examples]:
            print("    miss: ...%s" % tail.replace("\n", " "))
    return 0


if __name__ == "__main__":
    sys.exit(main())
