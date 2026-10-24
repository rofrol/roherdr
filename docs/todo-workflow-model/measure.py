#!/usr/bin/env python3
"""Coordinator and worker cost of the TODO pipeline.

Usage: python3 measure.py [--since YYYY-MM-DD] [--until YYYY-MM-DD]
                          [--records FILE] [--workers-only]
Run from the repository root. Reads herdr's worker run records
(`herdr worker records --repo .`, or FILE holding that command's JSON) and
the Claude Code transcripts of this repository's sessions (at list prices);
prints one JSON object. Each worker run's cost is the one its agent
reported; a run whose cost is unknown is counted, never priced at 0. The
denominator is the exact number of items `herdr todo run` landed in the
window (`items_landed`). --workers-only leaves out the transcripts.
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

def ms(t):
    return None if t is None else int(t * 1000)

def records(lo, hi, path):
    """`herdr worker records`' reply: from FILE, else from the CLI."""
    if path:
        text = open(path).read()
    else:
        args = ["herdr", "worker", "records", "--repo", "."]
        if lo is not None:
            args += ["--since", str(ms(lo))]
        if hi is not None:
            args += ["--until", str(ms(hi))]
        text = subprocess.run(args, capture_output=True, text=True, check=True).stdout
    return json.loads(text)["result"]

def workers(result):
    """Per item: its worker runs, their known cost, how many have none, and
    whether it landed; and every run's known cost."""
    items = {}; costs = []; unknown = 0
    landed = {l["item"] for l in result.get("landed", [])}
    for r in result.get("records", []):
        cost = r.get("cost_usd")
        if cost is None:
            unknown += 1
        else:
            costs.append(cost)
        if not r.get("item"):
            continue
        it = items.setdefault(r["item"], {"worker_runs": 0, "worker_cost_usd": 0.0,
                                          "worker_runs_cost_unknown": 0,
                                          "landed": r["item"] in landed})
        it["worker_runs"] += 1
        if cost is None:
            it["worker_runs_cost_unknown"] += 1
        else:
            it["worker_cost_usd"] += cost
    return items, costs, unknown

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
    ap.add_argument("--records"); ap.add_argument("--workers-only", action="store_true")
    a = ap.parse_args()
    lo, hi = ts(a.since), ts(a.until)
    result = records(lo, hi, a.records)
    items, w, unknown = workers(result)
    landed = result.get("items_landed")
    per = [it["worker_runs"] for it in items.values()]
    out = {
        "worker_runs": len(w) + unknown,
        "worker_runs_cost_unknown": unknown,
        "worker_cost_median": round(statistics.median(w), 2) if w else None,
        "worker_cost_mean": round(statistics.mean(w), 2) if w else None,
        "worker_runs_per_item_mean": round(statistics.mean(per), 2) if per else None,
        "items_landed": landed,
        "worker_cost_per_landed_item": round(sum(w) / landed, 2) if landed else None,
        "items": items,
    }
    if not a.workers_only:
        c, started, calls, ctx = coordinators(lo, hi)
        out.update({
            "coordinator_items_started": started,
            "coordinator_cost": round(c, 2),
            "coordinator_cost_per_item": round(c / started, 2) if started else None,
            "coordinator_calls_per_item": round(calls / started, 1) if started else None,
            "coordinator_mean_context": int(statistics.mean(ctx)) if ctx else None,
            "coordinator_cost_per_landed_item": round(c / landed, 2) if landed else None,
        })
    print(json.dumps(out, indent=2))

if __name__ == "__main__":
    main()
