#!/usr/bin/env python3
"""Ask an OpenRouter model (default: the free cloaked `stealth/space-bunny-alpha`) for a second opinion.

For READ-ONLY consultation only, never a default consultant. The model's provider is anonymous and RETAINS the
prompt and completion (OpenRouter Stealth Model Terms: not used for training, but logged by the unnamed lab), so
treat everything sent as seen by a third party. Private code is allowed by the user's choice; secrets never are.

Safety is layered, because the real risk is not this script but the calling agent pasting file content into the
prompt (see the consult on 2026-10-02). This script cannot read the repository on its own (there is no `-r` mode):
  * the prompt is whatever you pass as arguments;
  * a file's contents are attached only with an explicit `-f PATH` AND the `--allow-files` flag;
  * every `-f` path is checked fail-closed (inside the cwd, no symlink component, tracked-and-not-gitignored,
    not on a secrets denylist, not binary, within a size cap);
  * the whole assembled prompt is scanned for secret-shaped strings and the send is HARD-REFUSED on a match,
    with no override, so a human must sanitise and resend;
  * the exact payload (sha256, size, attached files) is printed before sending so a human watching can see it;
  * only byte counts are logged to consult-stats, never the prompt or the answer.
None of this is a substitute for not sending secrets: an agent with filesystem access can still paste text.
"""
import argparse, fnmatch, hashlib, json, os, re, signal, subprocess, sys, time
import urllib.request, urllib.error, urllib.parse
from pathlib import Path

CONSULT_DIR = Path(__file__).resolve().parent.parent / "consult-stats"  # the skills live side by side
CONSULT = CONSULT_DIR / "consult.py"
DEFAULT_BASE_URL = "https://openrouter.ai/api/v1"
DEFAULT_MODEL = "stealth/space-bunny-alpha"
# Short names for named (non-cloaked) models on trial, each pinned to one provider so the code goes to a known party.
MODEL_ALIASES = {
    "mimo": ("xiaomi/mimo-v2.6-pro", "Xiaomi"),
    "mimo-flash": ("xiaomi/mimo-v2.6-flash", "Xiaomi"),
}

# A file whose name matches any of these is never attached, even with --allow-files.
DENY_GLOBS = [
    ".env", ".env.*", "*.env", "*.key", "*.pem", "*.p12", "*.pfx", "*.jks", "*.keystore",
    "id_rsa", "id_dsa", "id_ecdsa", "id_ed25519", "*.ppk",
    "*secret*", "*secrets*", "*credential*", "*credentials*", "*.pyc",
    ".netrc", ".npmrc", ".pgpass", ".htpasswd", "auth.json", "*.tfstate", "*.tfvars",
]
# Secret-shaped strings in the assembled prompt; a match hard-refuses the send.
SECRET_PATTERNS = {
    "private-key-block": re.compile(r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----"),
    "aws-access-key": re.compile(r"\bAKIA[0-9A-Z]{16}\b"),
    "jwt": re.compile(r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}"),
    "github-token": re.compile(r"\b(ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{20,}\b|\bgithub_pat_[A-Za-z0-9_]{20,}\b"),
    "slack-token": re.compile(r"\bxox[baprs]-[A-Za-z0-9-]{10,}\b"),
    "secret-assignment": re.compile(
        r"(?i)\b(secret|token|password|passwd|api[_-]?key|access[_-]?key|private[_-]?key|client[_-]?secret|bearer)\b"
        r"\s*[:=]\s*['\"]?\S{6,}"),
    "upper-env-assignment": re.compile(r"\b[A-Z][A-Z0-9_]{2,}=\S{8,}"),
    "url-with-credentials": re.compile(r"://[^/\s:@]+:[^/\s@]+@"),
}
MAX_FILE_BYTES = 64 * 1024
MAX_TOTAL_BYTES = 256 * 1024


class Deadline(Exception):
    pass


def bearer():
    # A pre-fetched token in the environment lets the sandboxed run (ask_openrouter.sh) avoid reading pi's auth.json, so the
    # sandbox can deny every secret path including ~/.pi. Fall back to pi when the env var is absent.
    env_tok = os.environ.get("OPENROUTER_BEARER")
    if env_tok:
        return env_tok.strip()
    try:
        out = subprocess.run(["pi", "auth", "print-bearer-token", "--provider", "openrouter", "--min-expiry", "15m"],
                             capture_output=True, text=True, timeout=30)
    except (OSError, subprocess.SubprocessError) as e:
        sys.exit(f"Could not run pi to get an OpenRouter token: {e!r}")
    if out.returncode != 0 or not out.stdout.strip():
        sys.exit("No OpenRouter token in pi; log in to pi (/login) and authorise OpenRouter.")
    return out.stdout.strip()


def reject(msg):
    sys.exit(f"ask_openrouter: refused — {msg}")


def check_file(path):
    """Fail-closed vetting of one -f path. Returns the text to attach or exits."""
    cwd = Path.cwd().resolve()
    p = Path(path)
    # No symlink anywhere in the path: resolve each component and compare against a non-following stat.
    try:
        real = p.resolve(strict=True)
    except OSError as e:
        reject(f"cannot resolve {path!r}: {e}")
    cur = p if p.is_absolute() else cwd / p
    for parent in [cur, *cur.parents]:
        if parent == cwd or parent == cwd.parent:
            break
        if parent.is_symlink():
            reject(f"{path!r} has a symlink component ({parent}); refused")
    if cwd != real and cwd not in real.parents:
        reject(f"{path!r} resolves to {real}, outside the current directory {cwd}")
    name = real.name
    if any(fnmatch.fnmatch(name, g) or fnmatch.fnmatch(str(real), g) for g in DENY_GLOBS):
        reject(f"{path!r} matches the secrets denylist")
    # Must be a tracked, non-ignored file in a git repo (fail closed when git can't tell).
    try:
        ignored = subprocess.run(["git", "check-ignore", "--no-index", "-q", str(real)],
                                 cwd=real.parent, capture_output=True, timeout=10)
    except (OSError, subprocess.SubprocessError) as e:
        reject(f"could not run git check-ignore for {path!r} ({e}); refusing to guess")
    if ignored.returncode == 0:
        reject(f"{path!r} is gitignored; refused (sanitise and paste it into the prompt if it is safe)")
    if ignored.returncode not in (0, 1):  # 128 = not a git repo
        reject(f"{path!r} is not inside a git repository; refused")
    data = real.read_bytes()
    if b"\0" in data[:8192]:
        reject(f"{path!r} looks binary; refused")
    if len(data) > MAX_FILE_BYTES:
        reject(f"{path!r} is {len(data)} bytes, over the {MAX_FILE_BYTES}-byte per-file cap")
    return data.decode(errors="replace")


def scan_secrets(text):
    hits = sorted({name for name, rx in SECRET_PATTERNS.items() if rx.search(text)})
    if hits:
        reject("the prompt contains secret-shaped strings (" + ", ".join(hits) + "); "
               "this skill never sends them to the anonymous provider. Sanitise or use synthetic values and resend. "
               "There is no override.")


def log_call(model, status, seconds, prompt_chars, answer_chars, usage=None, version="", fingerprint=""):
    args = [*(["--model-version", version] if version else []), *(["--fingerprint", fingerprint] if fingerprint else [])]
    if usage:
        norm = {"input": usage.get("prompt_tokens"), "output": usage.get("completion_tokens"),
                "reasoning": (usage.get("completion_tokens_details") or {}).get("reasoning_tokens")}
        args += ["--usage", json.dumps(norm), "--usage-raw", json.dumps(usage)]
    try:
        subprocess.run([str(CONSULT), "log", "--skill", "openrouter", "--model", model, "--status", status,
                        "--seconds", str(int(seconds)), "--prompt-chars", str(prompt_chars),
                        "--answer-chars", str(answer_chars), *args], timeout=10)
    except (OSError, subprocess.SubprocessError):
        pass


def run_in_herdr_job(model):
    import shutil
    if os.environ.get("CONSULT_IN_JOB") or not os.environ.get("HERDR_SOCKET_PATH") or not shutil.which("herdr-job"):
        return
    wrap = CONSULT_DIR / "in_herdr_job.sh"
    label = "openrouter " + model.split("/")[-1]
    os.execv(str(wrap), [str(wrap), label, str(Path(__file__).resolve()), *sys.argv[1:]])


def generation_identity(base_url, tok, gen_id):
    """OpenRouter's post-hoc metadata for one generation: who actually served it. Empty when it cannot be read."""
    req = urllib.request.Request(base_url + "/generation?id=" + urllib.parse.quote(gen_id),
                                 headers={"Authorization": f"Bearer {tok}"})
    try:
        with urllib.request.urlopen(req, timeout=15) as r:
            d = (json.load(r) or {}).get("data") or {}
    except (OSError, ValueError):
        return {}
    return {k: d.get(k) for k in ("model", "provider_name", "origin", "upstream_inference_provider") if d.get(k)}


def resolve_model(model, provider):
    """Expand a MODEL_ALIASES short name; an explicit --provider wins over the alias's pinned one."""
    slug, pinned = MODEL_ALIASES.get(model, (model, ""))
    return slug, provider or pinned


def main():
    p = argparse.ArgumentParser()
    p.add_argument("prompt", nargs="*")
    p.add_argument("-m", "--model", default=os.environ.get("OPENROUTER_MODEL", DEFAULT_MODEL))
    p.add_argument("--provider", default=os.environ.get("OPENROUTER_PROVIDER", ""),
                   help="serve only through this OpenRouter provider (e.g. Xiaomi), no fallbacks")
    p.add_argument("-f", "--file", action="append", default=[], help="attach a file (needs --allow-files; - = stdin)")
    p.add_argument("--allow-files", action="store_true", help="permit -f attachments (off by default)")
    p.add_argument("-s", "--system", default="You are a senior engineer giving a candid, concrete second opinion. "
                   "Point out mistakes, risks and counter-examples; say plainly when you are unsure or need to see code.")
    p.add_argument("-t", "--timeout", type=int, default=int(os.environ.get("OPENROUTER_TIMEOUT", 420)))
    a = p.parse_args()
    run_in_herdr_job(a.model)
    a.model, a.provider = resolve_model(a.model, a.provider)

    prompt = " ".join(a.prompt)
    if a.file and not a.allow_files:
        reject("-f was given without --allow-files; attachments are off by default for this skill")
    total = len(prompt.encode())
    attached = []
    for f in a.file:
        if f == "-":
            text = (Path(os.environ["CONSULT_STDIN"]).read_text(errors="replace")
                    if os.environ.get("CONSULT_STDIN") else sys.stdin.read())
            label = "stdin"
        else:
            text = check_file(f)
            label = f
        total += len(text.encode())
        if total > MAX_TOTAL_BYTES:
            reject(f"attachments exceed the {MAX_TOTAL_BYTES}-byte total cap")
        attached.append((label, len(text)))
        prompt += f"\n\n--- {label} ---\n{text}"
    if not prompt.strip():
        sys.exit("Empty prompt")

    scan_secrets(prompt)

    base_url = os.environ.get("OPENROUTER_BASE_URL", DEFAULT_BASE_URL)
    digest = hashlib.sha256(prompt.encode()).hexdigest()[:12]
    print(f"ask_openrouter: sending to {a.model} via {base_url}"
          + (f", provider pinned to {a.provider}" if a.provider else ""), file=sys.stderr)
    print(f"  payload: {len(prompt)} chars, sha256:{digest}"
          + ("; files: " + ", ".join(f"{n} ({c}c)" for n, c in attached) if attached else "; prompt only"),
          file=sys.stderr)
    if a.model.startswith("stealth/"):
        print("  NOTE: the provider is anonymous and retains what is sent; never send secrets.",
              file=sys.stderr)
    else:
        print(f"  NOTE: the prompt goes to {a.provider or 'whichever provider OpenRouter routes to'}; "
              "its retention/training terms apply. No secrets or private code without the user's consent.",
              file=sys.stderr)

    tok = bearer()
    body = {"model": a.model, "stream": False, "usage": {"include": True},
            "provider": {"allow_fallbacks": False, **({"only": [a.provider]} if a.provider else {})},
            "messages": [{"role": "system", "content": a.system}, {"role": "user", "content": prompt}]}
    req = urllib.request.Request(base_url + "/chat/completions", data=json.dumps(body).encode(),
                                 headers={"Authorization": f"Bearer {tok}", "Content-Type": "application/json",
                                          "X-Title": "herdr-consult"})

    def on_alarm(*_):
        raise Deadline
    signal.signal(signal.SIGALRM, on_alarm)
    signal.alarm(a.timeout)
    start = time.monotonic()
    try:
        with urllib.request.urlopen(req, timeout=min(120, a.timeout)) as r:
            resp = json.load(r)
    except Deadline:
        log_call(a.model, "error", time.monotonic() - start, len(prompt), 0)
        sys.exit(f"\n[ask_openrouter: no answer within {a.timeout}s]")
    except urllib.error.HTTPError as e:
        log_call(a.model, "error", time.monotonic() - start, len(prompt), 0)
        sys.exit(f"HTTP {e.code}: {e.read().decode(errors='replace')[:500]}")
    except (urllib.error.URLError, TimeoutError, OSError, ValueError) as e:
        log_call(a.model, "error", time.monotonic() - start, len(prompt), 0)
        sys.exit(f"ask_openrouter: request failed: {e}")
    finally:
        signal.alarm(0)

    choices = resp.get("choices") or []
    answer = (choices[0].get("message", {}).get("content") if choices else "") or ""
    gen_id = resp.get("id") or ""
    served = resp.get("model") or ""
    usage = resp.get("usage")
    ident = generation_identity(base_url, tok, gen_id) if gen_id else {}
    # /generation answers 404 for pi's OAuth token (seen 2026-10-02), but the completion itself names its provider.
    if resp.get("provider") and not ident.get("provider_name"):
        ident["provider_name"] = resp["provider"]
    served_name = ident.get("model") or served
    provider = ident.get("provider_name") or ident.get("upstream_inference_provider") or "unknown-provider"
    version = f"{served_name} via {provider}" if served_name else ""
    log_call(a.model, "ok" if answer else "error", time.monotonic() - start, len(prompt), len(answer),
             usage, version, gen_id)

    # Identity check: a cloaked slug that comes back as a different, named model (or an unverifiable provider) is a
    # finding, not a success — surface it so a silent swap cannot pass unnoticed.
    if a.provider and ident.get("provider_name") and ident["provider_name"] != a.provider:
        print(f"\n[ask_openrouter: pinned provider {a.provider}, but {ident['provider_name']} served it]",
              file=sys.stderr)
    if served_name and not served_name.startswith(a.model):
        print(f"\n[ask_openrouter: requested {a.model}, served {served_name} via {provider}; "
              f"verify this is still the model you meant]", file=sys.stderr)
    elif not ident:
        print(f"\n[ask_openrouter: could not verify which provider/model served {a.model} "
              f"(generation metadata unavailable); treat the identity as unverified]", file=sys.stderr)

    print(answer)
    if not answer:
        sys.exit("ask_openrouter: empty answer")


if __name__ == "__main__":
    main()
