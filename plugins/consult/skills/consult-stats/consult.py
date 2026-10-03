#!/usr/bin/env python3
"""Log consultations (gpt, gemini, deepseek skills), rate them after triage, show stats per model.

  consult.py log --skill S --model M --status ok|error [--effort E] [--mode M] [--seconds N] [--prompt-chars N]
                [--answer-chars N] [--usage JSON] [--usage-raw JSON]
                [--model-version V] [--fingerprint F]                      (round id from $CONSULT_ROUND)
                [--error-kind K | --error-text-file PATH|-]                (errors only; the text is classified, never stored)
  consult.py new-round                                                     # prints a round id for CONSULT_ROUND
  consult.py rate ID useful|partial|useless [--findings N] [--accepted N] [--unique N] [--note TEXT]
  consult.py self (--round R | --calls ID,ID) --model ID [--effort E] [--findings N] [--accepted N] [--refuted N] [--unique N] [--missed N] [--note TEXT]
  consult.py stats [--days N] [--pairs] [--all] [--by-alias] [--width N]
  consult.py stats --vs A B [--since ROUND] [--rounds N] [--by-alias] [--width N]   # head-to-head over shared rounds
  consult.py recent [-n N] [--width N]

--width N fits the output to N columns for a pager (the plugin's popups): a wider table is split into bands that each
repeat the name column, prose is reflowed. Without it the output is unchanged, one line per row, for scripts and agents.

Data: $CONSULT_LOG or ~/.local/state/consult/log.jsonl (one JSON object per line; ratings are separate lines).
Usage is normalized by the ask_* scripts: input includes cached, output includes reasoning (both are subsets).
"""
import argparse, json, math, os, random, re, statistics, sys, textwrap, time, uuid
from collections import Counter, defaultdict
from pathlib import Path

LOG = Path(os.environ.get("CONSULT_LOG", Path.home() / ".local/state/consult/log.jsonl"))
VERDICTS = {"useful": 1.0, "partial": 0.5, "useless": 0.0}
USAGE_KEYS = ("input", "cached", "output", "reasoning")
PAIR_REF = {"gpt": "gpt-6-astra"}
MIN_WIDTH = 40  # below this the name column (at least 34 wide) leaves no room for a number  # reference model for --pairs, when it is in the group
# Why a call failed. `limit` covers both rate limits and exhausted plans (the vendors' texts do not separate them
# reliably). Calls logged before error kinds existed have none and show as `unknown`; `other` means a reason was
# seen but not recognized.
ERROR_KINDS = ("limit", "auth", "model", "timeout", "empty", "server", "network", "other")
# Checked in order, on the wrapper's error text (stderr tail, provider error message). The text itself is never
# logged: stderr and provider bodies can echo the prompt or credentials.
ERROR_PATTERNS = (
    ("auth", r"\b(?:HTTP|status|code)[ :=]*40[13]\b|\bunauthori[sz]ed\b|\binvalid (?:api )?key\b|\blog ?in\b"
             r"|\btoken (?:expired|invalid)\b|\bno [\w-]+ (?:token|key|accountId)\b|\bnot logged in\b"),
    ("model", r"\bunrecognized_model\b|\b(?:invalid|unknown) model\b|\bnot a valid model\b"
              r"|\bmodel [^\n]{0,80}\b(?:is )?not (?:supported|recogni[sz]ed|found)\b"),
    ("limit", r"\b(?:HTTP|status|code)[ :=]*(?:429|402)\b|\btoo many requests\b|\busage limit\b|\brate[ -]?limit"
              r"|\bquota\b|\bresource[_ ]exhausted\b|\binsufficient (?:balance|credits?)\b"),
    ("server", r"\b(?:HTTP|status|code)[ :=]*5\d\d\b|\boverloaded\b|\binternal server error\b|\bbad gateway\b"
               r"|\bservice unavailable\b"),
    ("timeout", r"\btimed? ?out\b|\btimeout\b|\bdeadline\b|\bno answer within\b"),
    ("network", r"\bconnection (?:refused|reset|error|aborted)\b|\bname resolution\b|\bnodename nor servname\b"
                r"|\bnetwork is unreachable\b|\bURLError\b|\bSSL\b"),
    ("empty", r"\bempty answer\b|\bno (?:successful )?answer\b"),
)


def append(rec):
    LOG.parent.mkdir(parents=True, exist_ok=True)
    with LOG.open("a") as f:
        f.write(json.dumps(rec, ensure_ascii=False) + "\n")


def by_version(calls):
    """Name calls by the model version the provider reported, when known: an alias like `deepseek-flash` moves to newer
    models over time, and merging them would mix two models' stats. Calls logged before versions were recorded keep
    their alias (version unknown); `stats --by-alias` merges everything under the alias instead."""
    return {i: {**c, "model": c.get("model_version") or c["model"]} for i, c in calls.items()}


def load():
    calls, ratings, rounds = {}, {}, {}
    if LOG.exists():
        for line in LOG.read_text().splitlines():
            try:
                r = json.loads(line)
            except ValueError:
                continue
            if r.get("type") == "call":
                calls[r["id"]] = r
            elif r.get("type") == "rating":
                ratings[r["id"]] = r  # the last rating of an id wins
            elif r.get("type") == "self":
                rounds[r.get("round") or r["calls"]] = r  # re-logging the same round replaces it
    return calls, ratings, rounds


def classify_error(text):
    """The error kind for a wrapper's error text: the first matching ERROR_PATTERNS entry, else `other`."""
    for kind, pattern in ERROR_PATTERNS:
        if re.search(pattern, text, re.IGNORECASE):
            return kind
    return "other"


def label(c):
    """skill/model, plus @effort when set explicitly (gemini has it in the model id, deepseek has none), -r for repo mode.
    Old calls without effort and new ones without -e (@default) share a row."""
    effort = c.get("effort")
    return (f'{c["skill"]}/{c["model"]}' + (f"@{effort}" if effort and effort != "default" else "")
            + (" -r" if c.get("mode") == "repo" else ""))


def out_tokens(c):
    return (c.get("usage") or {}).get("output")


def name_width(labels, floor=34):
    """Width of a name column: the longest label (at least `floor`, the old fixed width), so a long name does not
    push the other columns out of line. Labels are never cut: they are the rows' keys."""
    return max([floor, *map(len, labels)])


def table(header, rows, align, width=None, groups=None):
    """Lines of a plain-text table, rule included: each column as wide as its widest cell or header (the name
    column at least `name_width`'s floor), aligned per `align` ('<' or '>' per column); no trailing spaces.

    With `width`, a table wider than that is split into bands, blank-line separated, each a whole table with the
    name column, the header and every row in the same order, so no number loses its label when a pager scrolls.
    `groups` lists how many of the columns after the name belong together (default: each alone); a band takes
    whole groups in order and splits a group only when it does not fit next to the name column alone. When some
    column does not fit next to the name even alone, the table stays whole for the pager to wrap: one band per
    column would be no easier to read, and labels are never cut."""
    widths = [max([len(h), *(len(r[i]) for r in rows)]) for i, h in enumerate(header)]
    widths[0] = name_width([header[0], *(r[0] for r in rows)])

    def line(cells, cols):
        assert len(cells) == len(header), (cells, header)
        return " ".join(f"{cells[i]:{align[i]}{widths[i]}}" for i in cols).rstrip()

    def block(cols):
        return [line(header, cols), "-" * (sum(widths[i] + 1 for i in cols) - 1), *(line(r, cols) for r in rows)]

    def fits(cols):
        return sum(widths[i] + 1 for i in [0, *cols]) - 1 <= width

    cols = list(range(1, len(header)))
    if not width or fits(cols) or not all(fits([c]) for c in cols):
        return block([0, *cols])
    sizes = groups or [1] * len(cols)
    assert sum(sizes) == len(cols), (sizes, header)
    bands, band, start = [], [], 0
    for n in sizes:
        group, start = cols[start:start + n], start + n
        for part in [group] if fits(group) else [[c] for c in group]:
            if band and not fits(band + part):
                bands.append(band)
                band = []
            band += part
    bands.append(band)
    lines = []
    for band in bands:
        lines += [""] * bool(lines) + block([0, *band])
    return lines


def say(text, width=None, items=False):
    """Print prose. With `width` it is reflowed to fit: as one paragraph, or with `items` line by line, each line
    an item whose continuation is indented. Words are never broken, so a longer word overflows. Leading blank
    lines stay, they separate sections."""
    if not width:
        print(text)
        return
    body = text.lstrip("\n")
    lead = text[:len(text) - len(body)]
    wrap = dict(width=width, break_long_words=False, break_on_hyphens=False)
    if items:
        lines = [part for item in body.split("\n") for part in textwrap.wrap(item, subsequent_indent="  ", **wrap) or [""]]
    else:
        lines = textwrap.wrap(" ".join(body.split("\n")), **wrap)
    print(lead + "\n".join(lines))


def strip_via(model):
    """`xiaomi/mimo-v2.6-pro via Xiaomi` -> `xiaomi/mimo-v2.6-pro`: the provider repeats the model's author. Any other
    provider (`via DeepInfra`, `via unknown-provider`) is information and stays."""
    served, sep, provider = model.rpartition(" via ")
    if sep and "/" in served and provider.strip().casefold() == served.split("/", 1)[0].casefold():
        return served
    return model


def display_names(calls):
    """Row label -> the name shown, without a redundant ` via <author>`. Only the display changes: the label stays the
    row's key, so a model logged without a version is not merged into its versioned row. Labels whose short names
    would coincide keep their full form."""
    short = {label(c): label({**c, "model": strip_via(c["model"])}) for c in calls}
    clashes = Counter(short.values())
    return {k: v if clashes[v] == 1 else k for k, v in short.items()}


def date_range(first, last, now=None):
    """`MM-DD..MM-DD` of two timestamps in local time, `MM-DD` for one day; both ends get the year when either is
    outside the current year, so a December..January range stays readable."""
    year = time.localtime(now).tm_year
    a, b = time.localtime(first), time.localtime(last)
    fmt = "%m-%d" if a.tm_year == b.tm_year == year else "%Y-%m-%d"
    a, b = time.strftime(fmt, a), time.strftime(fmt, b)
    return a if a == b else f"{a}..{b}"


def p50(xs):
    return statistics.median(xs) if xs else None


def p90(xs, min_n=10):
    """Nearest-rank 90th percentile; None under `min_n` values, where it would just be the maximum."""
    if len(xs) < min_n:
        return None
    xs = sorted(xs)
    return xs[math.ceil(0.9 * len(xs)) - 1]


def secs(x):
    return "-" if x is None else f"{x:.0f}"


def ktok(n):
    return "-" if n is None else f"{n / 1000:.1f}k" if n >= 1000 else str(round(n))


def cmd_log(a):
    cid = uuid.uuid4().hex[:8]
    rec = {"type": "call", "id": cid, "ts": int(time.time()), "skill": a.skill, "model": a.model,
           "effort": a.effort, "mode": a.mode, "status": a.status, "seconds": a.seconds,
           "prompt_chars": a.prompt_chars, "answer_chars": a.answer_chars, "cwd": os.getcwd()}
    if os.environ.get("CONSULT_ROUND"):
        rec["round"] = os.environ["CONSULT_ROUND"]
    for key, val in (("model_version", a.model_version), ("fingerprint", a.fingerprint)):
        if val:
            rec[key] = val
    if a.status == "error":
        kind = a.error_kind
        if not kind and a.error_text_file:
            try:
                text = sys.stdin.read() if a.error_text_file == "-" else Path(a.error_text_file).read_text(errors="replace")
            except OSError:
                text = ""
            kind = classify_error(text[-4000:]) if text.strip() else None
        if kind:
            rec["error_kind"] = kind
    for key, raw in (("usage", a.usage), ("usage_raw", a.usage_raw)):
        try:
            val = json.loads(raw) if raw else None
        except ValueError:
            val = None  # a broken usage blob must not lose the call
        if key == "usage" and isinstance(val, dict):
            val = {k: int(val[k]) for k in USAGE_KEYS if isinstance(val.get(k), (int, float))} or None
        if val:
            rec[key] = val
    append(rec)
    print(f"[consult id: {cid}]", file=sys.stderr)


def cmd_new_round(a):
    print(time.strftime("%Y%m%d-%H%M%S-") + uuid.uuid4().hex[:4])


def check_counts(a, keys):
    vals = {k: getattr(a, k) for k in keys}
    if any(v is not None and v < 0 for v in vals.values()):
        sys.exit("Counts must not be negative")
    f, acc, u = vals.get("findings"), vals.get("accepted"), vals.get("unique")
    if f is not None and acc is not None and acc > f:
        sys.exit(f"accepted ({acc}) > findings ({f})")
    if acc is not None and u is not None and u > acc:
        sys.exit(f"unique ({u}) > accepted ({acc})")


def cmd_rate(a):
    calls, _, _ = load()
    if a.id not in calls:
        sys.exit(f"Unknown id: {a.id} (see: consult.py recent)")
    check_counts(a, ("findings", "accepted", "unique"))
    append({"type": "rating", "id": a.id, "ts": int(time.time()), "verdict": a.verdict,
            "findings": a.findings, "accepted": a.accepted, "unique": a.unique, "note": a.note})


def coordinator(a):
    """Which agent coordinates (claude-code, pi), its model id and reasoning effort (each with where it came from)
    and CLI version, as known when the entry is logged. pi exposes its model and effort to its bash tool; Claude Code
    only its effort, so there the model comes from --model. Anything not known is `unknown`, never a guessed default."""
    env = os.environ
    if env.get("CLAUDECODE"):
        agent = "claude-code"
    elif env.get("PI_MODEL") or env.get("PI_SESSION_ID"):
        agent = "pi"
    else:
        agent = "unknown"

    def pick(flag, *names):
        if flag:
            return flag, "flag"
        for name in names:
            if env.get(name):
                return env[name], name
        return "unknown", "unknown"

    model, model_source = pick(a.model, "PI_MODEL")
    effort, effort_source = pick(a.effort, "CLAUDE_EFFORT", "PI_REASONING_LEVEL")
    execpath = env.get("CLAUDE_CODE_EXECPATH")  # e.g. ~/.local/share/claude/versions/2.1.283
    cli = f"claude-code {Path(execpath).name}" if execpath else agent
    return {"agent": agent, "model": model, "model_source": model_source, "effort": effort,
            "effort_source": effort_source, "cli": cli}


def coordinator_label(rd):
    """agent/model@effort; entries from before 2026-09-27 have no agent, older ones only a family name like `claude`."""
    model = rd.get("model") or "claude"
    effort = rd.get("effort")
    agent = rd.get("agent")
    label = f"{agent}/{model}" if agent and agent != "unknown" else model
    return f"{label}@{effort}" if effort and effort != "unknown" else label


def cmd_self(a):
    calls, _, _ = load()
    if a.round:
        ids = sorted(i for i, c in calls.items() if c.get("round") == a.round)
        if not ids:
            sys.exit(f"No calls in round {a.round}")
    else:
        ids = sorted({i.strip() for i in a.calls.split(",") if i.strip()})
        unknown = [i for i in ids if i not in calls]
        if not ids or unknown:
            sys.exit(f"Unknown id: {unknown or '(none)'} (see: consult.py recent)")
    check_counts(a, ("findings", "accepted", "unique"))
    rec = {"type": "self", "calls": ",".join(ids), "ts": int(time.time()), **coordinator(a),
           "findings": a.findings, "accepted": a.accepted, "refuted": a.refuted,
           "unique": a.unique, "missed": a.missed, "note": a.note}
    if a.round:
        rec["round"] = a.round
    append(rec)


def bootstrap_ci(rounds, stat, n=2000, seed=0):
    """95% percentile interval of `stat` over `n` resamples of whole rounds; seeded, so a rerun prints the same."""
    rng = random.Random(seed)
    vals = sorted(stat([rng.choice(rounds) for _ in rounds]) for _ in range(n))
    return vals[int(0.025 * n)], vals[int(0.975 * n) - 1]


def print_vs(calls, ratings, a_pat, b_pat, since=None, limit=None, width=None):
    """Head-to-head of two models over the rounds where both answered and were rated (same prompt). A pattern matches
    a row label (`skill/model...`) by substring, case-insensitive. Numbers only: a trial's pass rule is judged by
    the reader, not encoded here."""
    sides = (a_pat, b_pat)
    by_round = defaultdict(lambda: ([], []))
    for c in calls.values():
        if c["status"] != "ok" or not c.get("round") or (since and c["round"] <= since):  # a failed try has no answer
            continue
        for k, pat in enumerate(sides):
            if pat.casefold() in label(c).casefold():
                by_round[c["round"]][k].append(c)
    shared, only, unrated, ambiguous = [], [0, 0], 0, 0
    for rd in sorted(by_round):  # round ids start with their date and time, so this is chronological
        ca, cb = by_round[rd]
        if len(ca) > 1 or len(cb) > 1 or (ca and cb and ca[0]["id"] == cb[0]["id"]):
            ambiguous += 1
        elif not (ca and cb):
            only[0 if ca else 1] += 1
        elif all(ratings.get(c["id"], {}).get(k) is not None for c in (ca[0], cb[0]) for k in ("findings", "accepted")):
            shared.append((rd, ca[0], cb[0]))
        else:
            unrated += 1
    later = len(shared) - limit if limit and len(shared) > limit else 0
    if limit:
        shared = shared[:limit]
    names = display_names(c for _, *pair in shared for c in pair)
    say(f"{a_pat} vs {b_pat}: {len(shared)} shared rated rounds" + (f" ({shared[0][0]} .. {shared[-1][0]})" if shared else "")
        + (f"; {later} later rounds left out by --rounds" if later else ""), width, items=True)
    say(f"left out: {only[0]} rounds with only {a_pat}, {only[1]} with only {b_pat}, {unrated} not fully rated, "
        f"{ambiguous} where a pattern matched two calls", width, items=True)
    if not shared:
        return
    rows = []
    for k in (0, 1):
        cs = [pair[k] for _, *pair in shared]
        rs = [ratings[c["id"]] for c in cs]
        find, acc = sum(r["findings"] for r in rs), sum(r["accepted"] for r in rs)
        outs = [out_tokens(c) for c in cs if out_tokens(c) is not None]
        rows.append([" / ".join(sorted({names.get(label(c), label(c)) for c in cs})), str(len(cs)), str(find), str(acc),
                     f"{find - acc} ({100 * (find - acc) / find:.0f}%)" if find else "-",
                     f'{sum(r.get("unique") or 0 for r in rs) / len(rs):.2f}', f"{acc / len(rs):.2f}",
                     ktok(sum(outs) / len(outs)) if outs else "-",
                     secs(p50([c["seconds"] for c in cs if c.get("seconds") is not None]))])
    header = ["model", "rounds", "findings", "accepted", "rejected", "uniq/call", "acc/call", "out/call", "p50 s"]
    print("\n".join(table(header, rows, "<>>>>>>>>", width, groups=[1, 3, 2, 2])))

    def uniq_diff(rs):
        return sum((ratings[x["id"]].get("unique") or 0) - (ratings[y["id"]].get("unique") or 0) for _, x, y in rs) / len(rs)

    def rejected_diff(rs):  # percentage points; a side without findings counts as 0% rejected
        share = [sum(ratings[p[k]["id"]]["findings"] - ratings[p[k]["id"]]["accepted"] for p in rs)
                 / max(1, sum(ratings[p[k]["id"]]["findings"] for p in rs)) for k in (1, 2)]
        return 100 * (share[0] - share[1])
    d = [(ratings[x["id"]].get("unique") or 0) - (ratings[y["id"]].get("unique") or 0) for _, x, y in shared]
    lo, hi = bootstrap_ci(shared, uniq_diff)
    say(f"paired uniq/call {a_pat} - {b_pat}: {uniq_diff(shared):+.2f} (95% CI {lo:+.2f}..{hi:+.2f}), "
        f"rounds W/T/L {sum(x > 0 for x in d)}/{sum(x == 0 for x in d)}/{sum(x < 0 for x in d)}", width, items=True)
    lo, hi = bootstrap_ci(shared, rejected_diff)
    say(f"rejected share {a_pat} - {b_pat}: {rejected_diff(shared):+.1f} points (95% CI {lo:+.1f}..{hi:+.1f})",
        width, items=True)
    say("CI: percentile bootstrap over whole rounds (2000 resamples, seed 0); descriptive with this few rounds.", width)


def cmd_stats(a):
    calls, ratings, rounds = load()
    since = time.time() - a.days * 86400 if a.days else 0
    calls = {i: c for i, c in calls.items() if c["ts"] >= since}
    if not a.by_alias:
        calls = by_version(calls)
    if a.vs:
        print_vs(calls, ratings, *a.vs, since=a.since, limit=a.rounds, width=a.width)
        return
    rows = defaultdict(lambda: defaultdict(float))
    lat = defaultdict(list)  # seconds of ok calls, per row
    kinds = defaultdict(Counter)  # error kinds, per row
    for c in calls.values():
        s = rows[label(c)]
        s["calls"] += 1
        s["first"] = min(s["first"] or c["ts"], c["ts"])
        s["last"] = max(s["last"], c["ts"])
        s["errors"] += c["status"] != "ok"
        if c["status"] != "ok":
            kinds[label(c)][c.get("error_kind") or "unknown"] += 1
        if c["status"] == "ok" and c.get("seconds") is not None:
            lat[label(c)].append(c["seconds"])
        if c["status"] == "ok" and out_tokens(c) is not None:
            s["used"] += 1
            s["out"] += out_tokens(c)
        r = ratings.get(c["id"])
        if r:
            s["rated"] += 1
            s["score"] += VERDICTS[r["verdict"]]
            # Missing counts stay missing: acc/find only over ratings that have both.
            if r.get("findings") is not None and r.get("accepted") is not None:
                s["findings"] += r["findings"]
                s["accepted"] += r["accepted"]
            s["unique"] += r.get("unique") or 0
    if not a.all:  # high effort is disabled in ask_gpt.sh; its rows are history
        rows = {k: s for k, s in rows.items() if "@high" not in k}
    if not rows:
        print("No data.")
        return
    extra = a.all or a.pairs
    names = display_names(calls.values())
    header, groups = ["skill/model", "call dates", "uniq/call", "wrong", "rated", "err"], [5]
    if extra:
        header += ["score", "acc/find", "unique", "lat n", "p50 s", "p90 s", "out/call"]
        groups += [3, 3, 1]  # rating, latency, tokens
    lines = []
    # Anecdotal rows (under 5 rated calls) go last, so a lucky 2/2 does not top the table.
    for k, s in sorted(rows.items(), key=lambda kv: (kv[1]["rated"] < 5,
                                                     -(kv[1]["unique"] / kv[1]["rated"] if kv[1]["rated"] else -1),
                                                     -kv[1]["rated"])):
        uniq = f'{s["unique"] / s["rated"]:.2f}' if s["rated"] else "-"
        wrong = f'{1 - s["accepted"] / s["findings"]:.0%}' if s["findings"] else "-"
        rated = f'{int(s["rated"])}/{int(s["calls"])}'
        err = f'{s["errors"] / s["calls"]:.0%}'
        line = [names.get(k, k), date_range(s["first"], s["last"]), uniq, wrong, rated, err]
        if extra:
            score = f'{s["score"] / s["rated"]:.2f}' if s["rated"] else "-"
            acc = f'{int(s["accepted"])}/{int(s["findings"])}' if s["findings"] else "-"
            out = ktok(s["out"] / s["used"]) if s["used"] else "-"
            line += [score, acc, str(int(s["unique"])), str(len(lat[k])), secs(p50(lat[k])), secs(p90(lat[k])), out]
        lines.append(line)
    print("\n".join(table(header, lines, "<<" + ">" * (len(header) - 2), a.width, groups)))
    say("\ncall dates: first..last day (local time) of the row's calls in the window, failed and unrated ones included;\n"
          "not continuous activity. Rows from different periods were rated against different companions.\n"
          "uniq/call: accepted findings nobody else (Claude, other models) had, per rated call — depends on who else was asked;\n"
          "wrong: share of findings rejected on verification (not necessarily false; also irrelevant or unverifiable), pooled\n"
          "over rated calls; rated: rated/all calls, unrated ones are left out; err: calls that failed (no answer), not wrong answers.\n"
          "Rows under 5 rated calls are anecdotal and sorted last. --all adds @high history, score, speed, tokens and the coordinator table.\n"
          "Models are named by the version the provider reported; a row named by an alias (deepseek/deepseek-flash) holds calls\n"
          "logged before versions were recorded, of unknown version. --by-alias merges them. A provider that only repeats\n"
          "the model's author (`via Xiaomi` on xiaomi/...) is not shown.", a.width)
    if extra:
        say("score: useful=1, partial=0.5, useless=0; out/call: mean output tokens incl. reasoning, over ok calls with usage.\n"
              "lat n, p50 s, p90 s: wrapper wall-clock seconds of ok calls (median; nearest-rank p90, shown from 10 calls on).\n"
              "Failed calls are left out of the latency columns; their time counts in the rounds table (--all).", a.width)
    if a.pairs:
        print_pairs(calls, ratings, names, a.width)
    if not a.all:
        return
    print_errors(kinds, rows, names, a.width)
    print_rounds(calls, since, names, a.width)
    selves = defaultdict(lambda: defaultdict(float))
    for rd in rounds.values():
        if rd["ts"] < since:
            continue
        s = selves[coordinator_label(rd)]
        s["rounds"] += 1
        for k in ("findings", "accepted", "refuted", "unique", "missed"):
            s[k] += rd.get(k) or 0
    if selves:
        lines = []
        for k, s in sorted(selves.items()):
            acc = f'{int(s["accepted"])}/{int(s["findings"])}' if s["findings"] else "-"
            known = s["accepted"] + s["missed"]
            recall = f'{s["accepted"] / known:.2f}' if known else "-"
            lines.append([k, str(int(s["rounds"])), acc, *(str(int(s[x])) for x in ("refuted", "unique", "missed")),
                          recall])
        header = ["coordinator", "rounds", "acc/find", "refuted", "unique", "missed", "recall"]
        print("\n" + "\n".join(table(header, lines, "<>>>>>>", a.width)))
        say("\nrefuted: Claude's own claims disproved (by a consulted model or verification); missed: accepted findings of consulted models "
              "Claude did not have; recall: accepted / (accepted + missed), i.e. against findings anyone discovered.", a.width)


def print_errors(kinds, rows, names, width=None):
    """Failed calls per row, by kind; `unknown` holds calls logged before kinds were recorded."""
    kinds = {k: c for k, c in kinds.items() if k in rows}
    if not kinds:
        return
    order = {k: i for i, k in enumerate((*ERROR_KINDS, "unknown"))}
    lines = []
    for k, c in sorted(kinds.items(), key=lambda kv: -sum(kv[1].values())):
        parts = ", ".join(f"{n} {kind}" for kind, n in sorted(c.items(), key=lambda kv: order.get(kv[0], 99)))
        lines.append([names.get(k, k), str(sum(c.values())), parts])
    print("\n" + "\n".join(table(["errors by kind", "n", "kinds"], lines, "<><", width)))


def round_finishes(calls, since=0):
    """Rounds with at least two models, as {round: {label: call}}. A model asked twice in a round (a retry) counts
    once, by its last call. Rounds that started before `since` are left out whole, so --days never cuts one."""
    rounds = defaultdict(dict)
    for c in sorted(calls.values(), key=lambda c: c["ts"]):
        if c.get("round"):
            rounds[c["round"]][label(c)] = c
    def start(members):
        return min(c["ts"] - (c.get("seconds") or 0) for c in members.values())
    return {r: m for r, m in rounds.items() if len(m) >= 2 and start(m) >= since}


def round_table(calls, since=0):
    """Per model: rounds joined, how often it finished last (alone), how often that last call had failed, and the
    finish gap: how much later it finished than the next model, i.e. how long the round waited for it alone.
    Finish times are the logged end times (`ts`), so a model launched late is not blamed for the others' time."""
    stats = defaultdict(lambda: {"rounds": 0, "last": 0, "last_err": 0, "gaps": []})
    for members in round_finishes(calls, since).values():
        for k in members:
            stats[k]["rounds"] += 1
        ends = sorted(members.items(), key=lambda kv: kv[1]["ts"], reverse=True)
        (k, c), second = ends[0], ends[1][1]
        gap = c["ts"] - second["ts"]
        if gap > 0:  # a tie has no single straggler
            stats[k]["last"] += 1
            stats[k]["last_err"] += c["status"] != "ok"
            stats[k]["gaps"].append(gap)
    return stats


def print_rounds(calls, since, names, width=None):
    stats = round_table(calls, since)
    if not stats:
        return
    lines = []
    # Under 5 rounds is anecdotal and goes last, like the main table.
    for k, s in sorted(stats.items(), key=lambda kv: (kv[1]["rounds"] < 5, -sum(kv[1]["gaps"]))):
        total = sum(s["gaps"])
        lines.append([names.get(k, k), str(s["rounds"]), f'{s["last"]}/{s["rounds"]}', str(s["last_err"]),
                      secs(p50(s["gaps"])), f"{total / 60:.0f} min" if total >= 600 else f"{total} s"])
    header = ["rounds (2+ models)", "rounds", "last", "failed", "gap p50", "gap sum"]
    print("\n" + "\n".join(table(header, lines, "<>>>>>", width)))
    say("last: rounds it finished last, alone; failed: of those, how many ended in an error (time spent waiting for a\n"
          "failure); gap: seconds between its end and the next model's end in those rounds (by logged end time), so\n"
          "gap sum is how long rounds waited for it alone. Rounds joined differ between models: compare last/rounds.", width)


def print_pairs(calls, ratings, names, width=None):
    """Token efficiency per model, and paired token ratios within rounds (same prompt, mode and effort)."""
    usable = [c for c in calls.values() if c["status"] == "ok" and out_tokens(c)]
    eff = defaultdict(lambda: defaultdict(float))
    for c in usable:
        r = ratings.get(c["id"])
        if r and r.get("accepted") is not None:
            s = eff[label(c)]
            s["n"] += 1
            s["out"] += out_tokens(c)
            s["accepted"] += r["accepted"]
            s["score"] += VERDICTS[r["verdict"]]
    lines = [[names.get(k, k), str(int(s["n"])), ktok(s["out"]), f'{1e6 * s["accepted"] / s["out"]:.1f}',
              f'{1e5 * s["score"] / s["out"]:.2f}'] for k, s in sorted(eff.items())]
    header = ["efficiency (rated, with usage)", "n", "out tok", "acc/1M out", "score/100k"]
    print("\n" + "\n".join(table(header, lines, "<>>>>", width)))
    if not eff:
        print("(no rated calls with token usage yet)")
    say("sums over calls, not means of per-call ratios; n<5 is anecdotal.", width)

    # Pair each model with its vendor's reference: PAIR_REF, else the model in most shared rounds of that
    # skill/mode/effort (ties: alphabetical).
    groups = defaultdict(lambda: defaultdict(dict))  # (skill, mode, effort) -> round -> model -> call
    for c in usable:
        if c.get("round"):
            groups[(c["skill"], c.get("mode") or "", c.get("effort") or "")][c["round"]][c["model"]] = c
    lines = []
    for (skill, mode, effort), by_round in sorted(groups.items()):
        seen = Counter(m for models in by_round.values() if len(models) > 1 for m in models)
        if not seen:
            continue
        ref = PAIR_REF.get(skill) if PAIR_REF.get(skill) in seen else max(sorted(seen), key=seen.get)
        for model in sorted(seen):
            if model == ref:
                continue
            logs, wtl = [], [0, 0, 0]
            for models in by_round.values():
                if model in models and ref in models:
                    m, r = models[model], models[ref]
                    logs.append(math.log(out_tokens(m) / out_tokens(r)))
                    rm, rr = ratings.get(m["id"]), ratings.get(r["id"])
                    if rm and rr:
                        d = VERDICTS[rm["verdict"]] - VERDICTS[rr["verdict"]]
                        wtl[0 if d > 0 else 1 if d == 0 else 2] += 1
            if logs:
                geo = math.exp(sum(logs) / len(logs))
                spread = f"{math.exp(min(logs)):.2f}–{math.exp(max(logs)):.2f}" if len(logs) > 1 else ""
                cfg = "/".join(x for x in (effort, "-r" if mode == "repo" else "") if x)
                lines.append(f'{skill} {model} vs {ref}{" [" + cfg + "]" if cfg else ""}: {len(logs)} rounds · '
                             f'out tokens {geo:.2f}× {spread} · score W/T/L {wtl[0]}/{wtl[1]}/{wtl[2]}')
    say("\npaired within rounds (geometric mean of per-round output-token ratios)", width)
    say("\n".join(lines) if lines else "(no rounds with token usage yet; set CONSULT_ROUND for parallel calls)", width,
        items=True)


def cmd_recent(a):
    calls, ratings, rounds = load()
    calls = by_version(calls)
    latest = sorted(calls.values(), key=lambda c: c["ts"])[-a.n:]
    names = display_names(latest)
    w = name_width(names.values())
    for c in latest:
        r = ratings.get(c["id"])
        rated = f'{r["verdict"]} {r.get("accepted") or 0}/{r.get("findings") or 0} u{r.get("unique") or 0}' if r else "unrated"
        when = time.strftime("%Y-%m-%d %H:%M", time.localtime(c["ts"]))
        status = c["status"] if c["status"] == "ok" else f'{c["status"]}:{c.get("error_kind") or "unknown"}'
        say(f'{c["id"]}  {when}  {names[label(c)]:{w}} {status:13}  {ktok(out_tokens(c)):>6}  {rated}  '
            f'{Path(c.get("cwd") or "").name}', a.width, items=True)
    covered = {i for rd in rounds.values() for i in rd["calls"].split(",")}
    todo = sorted(i for i in calls if i not in covered and i in ratings)
    if todo:
        say(f"\nrated calls without a coordinator (self) entry: {', '.join(todo)}", a.width, items=True)


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    l = sub.add_parser("log")
    l.add_argument("--skill", required=True); l.add_argument("--model", required=True)
    l.add_argument("--status", required=True, choices=["ok", "error"]); l.add_argument("--mode", default="")
    l.add_argument("--effort", default="")
    for k in ("--seconds", "--prompt-chars", "--answer-chars"):
        l.add_argument(k, type=int)
    l.add_argument("--usage", default="", help='normalized JSON: {"input","cached","output","reasoning"}')
    l.add_argument("--usage-raw", default="", help="the provider's usage object, kept for later")
    l.add_argument("--model-version", default="", help="the model the provider says serves --model (when it is an alias)")
    l.add_argument("--fingerprint", default="", help="the provider's backend fingerprint (e.g. system_fingerprint)")
    e = l.add_mutually_exclusive_group()
    e.add_argument("--error-kind", choices=ERROR_KINDS, help="why the call failed, when the wrapper knows it")
    e.add_argument("--error-text-file", help="the wrapper's error text (- = stdin), classified into an error kind; "
                                            "the text is not stored")
    sub.add_parser("new-round")
    r = sub.add_parser("rate")
    r.add_argument("id"); r.add_argument("verdict", choices=list(VERDICTS))
    for k in ("--findings", "--accepted", "--unique"):
        r.add_argument(k, type=int)
    r.add_argument("--note", default="")
    c = sub.add_parser("self")
    g = c.add_mutually_exclusive_group(required=True)
    g.add_argument("--calls"); g.add_argument("--round")
    c.add_argument("--model", help="your exact model id, e.g. claude-opus-5-5 (default: $PI_MODEL, else unknown)")
    c.add_argument("--effort", help="your reasoning effort (default: $CLAUDE_EFFORT or $PI_REASONING_LEVEL, else unknown)")
    for k in ("--findings", "--accepted", "--refuted", "--unique", "--missed"):
        c.add_argument(k, type=int)
    c.add_argument("--note", default="")
    s = sub.add_parser("stats"); s.add_argument("--days", type=int)
    s.add_argument("--pairs", action="store_true", help="token efficiency and paired within-round comparisons")
    s.add_argument("--all", action="store_true", help="all columns, @high history and the coordinator table")
    s.add_argument("--by-alias", action="store_true", help="group by the requested model, merging its versions")
    s.add_argument("--vs", nargs=2, metavar=("A", "B"),
                   help="head-to-head over rounds where both answered and were rated; A, B match row labels by substring")
    s.add_argument("--since", metavar="ROUND", help="with --vs: only rounds after this round id")
    s.add_argument("--rounds", type=int, metavar="N", help="with --vs: only the first N shared rounds")
    n = sub.add_parser("recent"); n.add_argument("-n", type=int, default=20)
    for q in (s, n):
        q.add_argument("--width", type=int, metavar="N", help=f"fit the output to N columns (at least {MIN_WIDTH}): wide "
                       "tables split into bands that repeat the name column, prose is reflowed; for a pager")
    a = p.parse_args()
    if getattr(a, "width", None) is not None and a.width < MIN_WIDTH:
        p.error(f"--width must be at least {MIN_WIDTH}")
    if a.cmd == "log" and a.status == "ok" and (a.error_kind or a.error_text_file):
        p.error("--error-kind/--error-text-file only go with --status error")
    {"log": cmd_log, "new-round": cmd_new_round, "rate": cmd_rate, "self": cmd_self, "stats": cmd_stats,
     "recent": cmd_recent}[a.cmd](a)


if __name__ == "__main__":
    main()
