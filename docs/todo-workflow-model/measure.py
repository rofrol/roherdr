#!/usr/bin/env python3
"""Coordinator and worker cost of the TODO pipeline at list prices.

Usage: python3 measure.py [--since YYYY-MM-DD] [--until YYYY-MM-DD]
Run from the repository root. Reads herdr's worker journals and the Claude
Code transcripts of this repository's sessions; prints one JSON object.
"""
import argparse, datetime, glob, json, os, re, statistics, subprocess

# USD per million tokens (list prices, 2026-10-06 table).
PRICES = {
    "opus":   {"in": 4.0, "cw": 5.0,  "cr": 0.20, "out": 20.0},
    "sonnet": {"in": 2.0, "cw": 2.5,  "cr": 0.20, "out": 10.0},
}

def family(model):
    return "sonnet" if "sonnet" in (model or "") else "opus"

def ts(day):
    return datetime.datetime.fromisoformat(day).timestamp() if day else None

def find_result(o):
    if isinstance(o, dict):
        if o.get("type") == "result" and "total_cost_usd" in o:
            return o
        for v in o.values():
            r = find_result(v)
            if r:
                return r
    return None

def workers(lo, hi):
    runs = []
    for f in glob.glob(os.path.expanduser("~/.local/state/herdr/workers/w*.jsonl")):
        m = os.path.getmtime(f)
        if (lo and m < lo) or (hi and m >= hi):
            continue
        cost = 0.0; n = 0
        for line in open(f, errors="ignore"):
            if "total_cost_usd" not in line:
                continue
            try:
                r = find_result(json.loads(line))
            except ValueError:
                continue
            if r:
                n += 1; cost += r.get("total_cost_usd") or 0
        if n:
            runs.append(cost)
    return runs

def coordinators(lo, hi):
    root = subprocess.run(["git", "rev-parse", "--show-toplevel"], capture_output=True,
                          text=True, check=True).stdout.strip()
    proj = os.path.expanduser("~/.claude/projects/" + root.replace("/", "-").replace("_", "-"))
    total = 0.0; items = 0; calls = 0; ctx = []
    for f in glob.glob(proj + "/*.jsonl"):
        m = os.path.getmtime(f)
        if (lo and m < lo) or (hi and m >= hi):
            continue
        text = open(f, errors="ignore").read()
        # A coordinator session starts runs for real item ids.
        n_items = sum(1 for l in text.splitlines()
                      if '"tool_use"' in l and re.search(r"herdr todo run t-[a-z0-9]{8}\b", l))
        if not n_items:
            continue
        seen = set(); cost = 0.0
        for line in text.splitlines():
            if '"usage"' not in line:
                continue
            try:
                j = json.loads(line)
            except ValueError:
                continue
            msg = j.get("message") or {}
            if j.get("type") != "assistant" or msg.get("id") in seen:
                continue
            seen.add(msg.get("id"))
            u = msg.get("usage") or {}; p = PRICES[family(msg.get("model"))]
            i, w, r, o = (u.get(k) or 0 for k in ("input_tokens", "cache_creation_input_tokens",
                                                  "cache_read_input_tokens", "output_tokens"))
            cost += (i * p["in"] + w * p["cw"] + r * p["cr"] + o * p["out"]) / 1e6
            ctx.append(i + w + r)
        total += cost; items += n_items; calls += len(seen)
    return total, items, calls, ctx

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--since"); ap.add_argument("--until")
    a = ap.parse_args()
    lo, hi = ts(a.since), ts(a.until)
    w = workers(lo, hi)
    c, items, calls, ctx = coordinators(lo, hi)
    out = {
        "worker_runs": len(w),
        "worker_cost_median": round(statistics.median(w), 2) if w else None,
        "worker_cost_mean": round(statistics.mean(w), 2) if w else None,
        "coordinator_items_started": items,
        "coordinator_cost": round(c, 2),
        "coordinator_cost_per_item": round(c / items, 2) if items else None,
        "coordinator_calls_per_item": round(calls / items, 1) if items else None,
        "coordinator_mean_context": int(statistics.mean(ctx)) if ctx else None,
    }
    runs = subprocess.run(["herdr", "worker", "runs", "--repo", "."], capture_output=True, text=True)
    try:
        its = [i for i in json.loads(runs.stdout)["result"]["items"] if i.get("item")]
        per = [len(i["runs"]) for i in its]
        out["worker_runs_per_item_mean"] = round(statistics.mean(per), 2) if per else None
    except (ValueError, KeyError):
        out["worker_runs_per_item_mean"] = None
    root = subprocess.run(["git", "rev-parse", "--show-toplevel"], capture_output=True,
                          text=True, check=True).stdout.strip()
    tr = subprocess.run(["herdr", "todo", "runs"], capture_output=True, text=True)
    try:
        done = [r for r in json.loads(tr.stdout)["result"]["runs"]
                if r.get("repo") == root and r.get("status") == "done"
                and (not lo or r["created_ms"] / 1000 >= lo) and (not hi or r["created_ms"] / 1000 < hi)]
        out["todo_runs_done"] = len(done)
        out["coordinator_cost_per_done_run"] = round(c / len(done), 2) if done else None
        out["worker_cost_per_done_run"] = round(sum(w) / len(done), 2) if done else None
    except (ValueError, KeyError):
        out["todo_runs_done"] = None
    print(json.dumps(out, indent=2))

if __name__ == "__main__":
    main()
