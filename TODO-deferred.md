# Deferred

Parked ideas, moved out of `TODO.md` so it stays small. Move an item
back to `TODO.md` when it becomes next.

- [ ] Which roherdr parts could be plugins, which need core, and which are
  worth a PR upstream (user, 2026-10-07: "think it over with the agents:
  what can be split out of roherdr as a plugin and what not, where a PR
  upstream would be needed; or maybe it no longer makes sense, because the
  plugin is too limiting and we are racing ahead like mad?").
  Consult round `20261007-035612-31de` (sol, MiMo), agreeing:
  - Core for good: the sidebar and TUI work (vertical tabs, squares, role
    marks, T/A buttons, ask line, footers, legend, filter/sort/drag) and
    new API verbs and server semantics (handoff, reply and limit reports,
    tab.create_* variants, reopen, name resolution). Plugins today can only
    orchestrate existing verbs; already plugins: job, consult, restart,
    relaunch, pi-title.
  - The plugin-API extensions that would unlock most: declarative UI slots
    (tab badges, sidebar rows/buttons, a footer strip with bound actions)
    and namespaced, server-persisted entity metadata with lifecycle events;
    API method registration is core work in a plugin costume (MiMo).
  - Upstream: plan for permanent divergence; file verified reproducible bugs
    in upstream-owned code (each fix upstream deletes rebase work); no
    feature PRs or approval chase (their rules wall them out).
  - Rebase cost grows with overlap in upstream-owned files, not commit
    count: rebase often, keep fork code behind narrow seams, enable `git
    rerere`, count conflicts per rebase; a rebase over a day is the signal to
    redesign seams (MiMo).
  - Policy: extract later and only when the API preserves the behaviour; a
    feature graduates to a plugin after ~4 weeks stable with no new UI slot
    or verb (MiMo). The user decides whether to adopt it.
  Parked by the user 2026-10-07 ("I only wanted to think it over, not do
  it"): nothing adopted; the outcome above is for when rebases start to
  hurt.

- [ ] Awaiting reply for agents other than Claude and pi, the same way as
  their integrations (user, 2026-09-28): each integration that can add
  session context (a session-start hook, an extension, a plugin) injects the
  same instruction, and where the agent has a command allowlist the install
  adds `herdr agent awaiting-reply` to it. Open: antigravity_cli, codex,
  copilot, cursor, devin, droid, grok, hermes, kilo, kimi, letta,
  mastracode, omp, opencode, qodercli, qwen; check per agent what it
  offers, bump each changed integration's version once, try each live.
  Pi's extension is not verified in a live Pi session yet (`DECISIONS.md`,
  "Awaiting-reply for pi").
  Triage 2026-10-06 (manual): Pi is done but not checked live; each of the 16 other integrations needs a live check in that agent with your accounts.
  Parked by the user 2026-10-07: only the agents he uses (Claude, pi) for
  now; pi still needs a live check.

- [ ] Follow-ups of "show what an agent asks": a "needs me" filter, a
  header count for limited agents, outcomes (parked by the user 2026-10-07:
  "none for now").
  Checked 2026-10-07: the `?` list and (since `e0bfbce8`) a `↳` line under
  asking tab lines show the ask. Waiting since the user's 2026-10-06
  decision: a "needs me" filter in the planned sidebar filter bar, never
  hiding rows by default (hiding breaks positional `Alt-1…9`, focus and job
  child tabs); a separate header count for limited agents, and limits for
  Codex and pi; outcomes ("ready for review"), which sol shows apart from
  actions and MiMo would not (stale within a day; round
  `20261006-021127-0eb7`), and if they come only until the tab is viewed,
  as facts herdr reads itself (branch, `+N -M`), not agent prose.

- [ ] Ideas from pstack-t3 (user, 2026-10-06; https://github.com/creedants/pstack-t3,
  a 3-day-old port of Lauren Tan's pstack to T3 Code's orchestrator; not
  installed: its orchestration only runs inside T3 Code, and 55 skill
  descriptions cost about 3k tokens per session). Consulted sol and MiMo
  (round of 2026-10-06, both: skip the install, borrow these):
  - Not the full landing queue: it needs a worktree per writer, against the
    fork's "work on master in the shared checkout" rule (user, 2026-10-06:
    keep the rule). Take only what fixes the real incident (2026-10-02: a
    bare `git commit` swept another session's staged hunks):
    - A commit lock: `flock` on a file under `.git/`, so one session commits
      at a time, in a script that does what AGENTS.md describes (a patch of
      only its own hunks, a temporary index) and refuses a commit without
      paths.
    - Light path claims: a session announces the files it edits; another
      session gets a warning before editing the same file, not a refusal.
    - Notes from pstack's `land.py`: `flock` is released when the process
      dies, so no stale lock; after a rebase compare `HEAD^{tree}` with the
      reviewed tree (MiMo); claims do not stop an agent that bypasses them
      (sol). The full queue with worktrees stays for long or risky work,
      where AGENTS.md already asks for a worktree.
    - The Fellowship post (see the three gaps near the top) avoids
      concurrent writers by role instead: only the main session writes
      code, the reviewer never edits, qa works in throwaway worktrees. A
      cheaper first step is that rule in `AGENTS.md`, though it lives only
      in prose.
  - [x] Machine-wide slots for builds and tests: extend `herdr-job` (and next to
    `just guard`) with N slots plus an exclusive mode for benchmarks, so
    several sessions do not thrash one `target/` or skew measurements.
    Done 2026-10-06 (round `20261006-030357-bd7d`, sol and MiMo):
    `herdr-job slot [--exclusive] -- CMD`, `run --slot/--exclusive`,
    `herdr-job slots`; the `just` build, test and clippy lines take a slot,
    benchmarks every slot. One slot by default (both: cargo and nextest each
    use every core, so two slots let two full-machine loads run); MiMo: cargo's
    `target/` lock does not cover it, nextest runs tests after releasing it;
    an exclusive request inside a slot fails at once (it would wait for its
    own ancestor); no gate lock (moot with one slot). Not done: re-run
    `just guard` after a long slot wait (sol), and the CPU and output idle
    detector still cannot tell a job blocked on cargo's lock (MiMo).
  - [x] Structured dispositions in `consult`: classify each finding Act on /
    Consider / Noted / Dismissed (pstack's `$interrogate`), with evidence and
    whether it was verified, next to the existing per-call ratings.
    Done 2026-10-06 as counts, not per-finding records (sol and MiMo: records
    keep the same judgment and cost much more bookkeeping):
    `rate --act --consider --noted --dismissed` (all four, adding up to
    `--findings`; a rejected finding is dismissed), `act/call` in
    `stats --all`, and the `consult` skill reports to the user in the four
    buckets. Later, if wanted: link `act` findings to the commits that
    landed them (MiMo).
  Decided by the user 2026-10-06: parked: one swept-hunk incident in 276
  commits, and AGENTS.md now lets only one session work through the TODO
  at a time.

- [ ] Orchestration direction (user, 2026-10-06: "analyse how to do this
  orchestration best ... is there a point in using the Claude SDK etc., how
  does T3 Code do it?"). Read T3 Code `4df84a7d`; consulted sol and MiMo
  (round `20261006-033911-ff97`). Decision pending with the user.
  - T3 Code has no PTY for agents: Claude through `@anthropic-ai/claude-agent-sdk`
    (spawns the user's `claude`, uses the subscription login, `canUseTool`
    for approvals and questions, `rate_limit_event` for a Limited state
    with auto-resume), `codex app-server`, `pi --mode rpc`, ACP. One local
    HTTP MCP server with per-session tokens (`delegate_task` async|wait,
    `task_status`, `task_cancel` depth-first); a finished child wakes the
    parent with an injected message; children get a brief, not history; no
    agent concurrency cap. Its gap: a child stuck on a permission request
    looks idle.
  - Terms: Anthropic's Agent SDK docs forbid unapproved third-party products
    from offering claude.ai login; on 2026-06-15 Anthropic paused moving SDK
    and `claude -p` use to separate credits, so both still draw on the
    subscription. Wrapping `claude -p` instead of the SDK is no loophole
    (sol). An interactive `claude` in a PTY is plain terminal use.
  - Both models: stay PTY-first; no SDK in herdr. Headless only for bounded
    child tasks nobody watches (`claude -p --output-format stream-json`,
    `codex exec --json`, `pi --mode rpc`), one adapter proven before the
    next, and one state record fed by both screen detection and stream or
    hook events (MiMo), so hybrid does not double the state machine.
  - Order: (1) a truthful task state: idle is not done; awaiting permission,
    awaiting answer, limited, failed, with question text and reset time from
    hooks where available (user, 2026-10-06: "looks ok"); (2) clean
    validation, not worktrees per child (see below); (3) a child-task primitive: parent link, brief,
    completion that wakes the parent at a safe input boundary (never typed
    into a permission dialog), subtree cancel, recursion bounds; (4)
    handoff; (5) MCP only as a thin facade over the API.
  - Worktrees for children? (user, 2026-10-06: "what do we need them for;
    if they slow things down, is manual handoff not better?"; round
    `20261006-035159-d484`, sol and MiMo agree): no, the shared master stays.
    Measured: one swept-hunk incident in 276 commits over 8 days, but every
    `just check` and install builds whatever another session left half done
    in `src/` (it was the case while asking). Worktree checks: median 3.0
    min against 1.9 (7 runs, 7.8 cold). So:
    - Build and test from a clean tree: a reusable detached worktree at the
      `master` SHA plus only this session's own patch (the fix is not
      committed before the user tries it), sharing `CARGO_TARGET_DIR` with
      the main checkout under one lock; install from there. Measure two
      builds sharing the target first (cargo rebuilds local crates per
      source path).
    - Handoff has nothing to do with worktrees (same task, one after the
      other): automate only the pointer (session id, transcript, task,
      SHA); ownership moves once the first agent stops writing.
    - Children: read-only ones (review, research) in the shared checkout,
      reviews of a committed snapshot; writing ones sequential on master
      with path claims; a worktree only when two writers really run at
      once or the work is long or exploratory, as AGENTS.md already says.
    - A pre-commit hook that refuses a commit without paths (MiMo); note
      `git commit -- <path>` also takes others' unstaged edits in that file.
  - Order and praise (user, 2026-10-06: "would these changes make people
    praise roherdr like T3 Code?"; round `20261006-035823-bbe8`, sol and
    MiMo): the praise is less supervision, not looks: "who needs me now",
    ranked by how long they have waited, one click to the question. So:
    - [x] First the clean build tree, time-boxed to an evening: one persistent
      check worktree (not a fresh one per run: cold Rust builds) reset to
      the `master` SHA, plus a patch of explicitly named paths of this
      session (`git diff` of the shared checkout carries other sessions'
      edits, so it cannot be the input), then the existing checks there.
      Done 2026-10-06: `herdr-job clean-tree [PATHS] -- CMD` (any repository;
      first as `scripts/clean_tree.py`, generalized the same day at the
      user's request, with a rule in the global Claude and pi instructions:
      use it when `git status` shows changes that are not yours),
      `just clean-check <paths>`, `just clean-release <paths>`, AGENTS.md
      install flow. Own `target/`;
      a shared one is not measured (cargo keys local and vendored path
      crates by source path, so they may rebuild on every switch): cold `just clean-check` 6.0 min and 3.6 GB, warm 1.8 min.
    - Then the task state as an attention inbox: waiting agents ranked by
      when they started waiting, the question text inline, jump to it;
      mark hook-confirmed states apart from screen-inferred ones; never
      call silence "done".
    - Limited: only an agent stopped by a limit gets the state (not an
      account that is nearly used up); say which limit (rate, credits,
      context full: different remedies); `limited · resets 14:32` as the
      second line and in the header counts next to `?N`; no implied
      auto-resume. Herdr already reads the reset times (`src/usage/`).
    - Beyond features (user decides): a demo with six agents, two needing
      the user and one limited, solved without hunting; README positioning
      "run your real agent CLIs, find every agent waiting on you", one
      install path; both models call the name "roherdr" hard to say and
      search; MiMo: signed releases, since a one-person fork that replaces
      its server binary live reads as a supply-chain risk.
    - Child tasks, MCP: deferred until supervision is trustworthy.
  - OptMem and OptChat (user, 2026-10-06; github.com/VictorTaelin/OptMem, no
    license; rounds `20261006-042739-ce28`, `20261006-044634-0c2b`, sol and
    MiMo): do not adopt it for our agents. One global log mixes projects into
    mushy summaries, "age" counts later notes rather than time, agents
    compress inline (about one compression per note) and every session pays
    about 8k tokens at wake, and `forget` never erases raw notes. Project
    lessons stay in AGENTS.md. John Ash ran the same tree for two years and
    dropped it: errors stack up and temporal reasoning is weak.
    - If herdr ever keeps an event log (handoff, child briefs): provenance
      first (who, when, pane, transcript link), validity times for
      decisions that can be revoked, summaries last, per task and off the
      agent's turn (like activegraph.ai's replay and explain, Apache 2.0,
      as an idea, not a dependency). After the attention inbox.
      CorpusMap (arXiv 2609.37226, preprint) measured it: summary layers
      (LLM wiki, topic trees) often lose to the raw corpus, while entity pages
      that link to untouched documents beat it with 34-57% fewer tokens, for a
      plain find/grep agent.
    - Child context (step 3): a brief by default, not inherited history
      (Taelin's spawn-by-inherit assumes a single writer). A child that
      continues the same work may get a native fork (`claude --resume <id>
      --fork-session`, `codex fork <id>`), always by explicit id and never
      resumed in place or via `--last`. Writing children re-read only the
      files they edit; reviewers get acceptance criteria and the diff but
      not the parent's diagnosis. Completion returns changed paths, tests,
      blockers and what remains; cancel never blindly reverts. Defer a
      read-files ledger with hashes; a short list of relevant files in the
      brief is enough.
  - Not to build: a chat GUI, a universal conversation schema, a scheduler
    or quotas, auto-approval, auto-merge, default auto-resume after a limit.
  - Slots: freeze them (6 min of overlap in 14 days); what contends is the
    shared checkout and the subscription limits, not CPU. Measure harm
    (failed or slowed runs), not overlap.
  Decided by the user 2026-10-06: steps 1 and 2 are done (truthful state,
  inbox and limits in 7f55e2c5; clean-tree) and handoff is decided in
  TODO.md; child tasks (step 3) and the rest wait until they are missed in
  practice.

- [ ] Build line (bottom left of the sidebar): hover shows the full commit
  message, click opens a modal with the full commit info (full hash,
  subject, body, author, date, dirty flag, version and channel), scrollable,
  Esc closes.
  - The data does not exist yet: `HERDR_GIT_COMMIT_LINE` holds only
    `<short hash> <subject>`. Embed structured commit metadata at build
    time (handle builds without git); never ask the git repo of the
    current space, which is another project.
  - Consulted models (GPT-6 Astra, DeepSeek, 2026-09-28): the client and
    the server builds can differ after a live handoff, so the modal shows
    both, labelled "server" and "client", and marks a mismatch. Server
    details come from a new advertised build-info method (the snapshot's
    `build_commit` stays as is); an old server shows "details unavailable",
    never the client's data in its place. Astra: the tooltip shows the
    subject only, the body belongs in the modal.
  - Done 2026-09-29: hovering the build line shows its whole commit line
    (hash and subject) in a tooltip, and both builds when the client's
    differs (`server <line> · client <hash>`). Still open: the modal and
    the build metadata it needs.
  Decided by the user 2026-10-06: parked: the hover with hash and subject
  is enough; the modal and its build-info API method come back when it
  falls short.

- [ ] Run the untrusted/cloaked OpenRouter consult (`ask-bunny`, Space Bunny /
  MiMo) so a secret can never reach the logging provider (user, 2026-10-02).
  - Done so far (committed `1d71dabf`): `ask-bunny` runs `ask_openrouter.py`
    under macOS `sandbox-exec` (`bunny.sb`, `allow default` + `deny file-read*`
    of `~/.pi`, `~/.ssh`, `~/.config`, `~/personal_projects`, `*.env`/`*.key`/
    `credentials`/`auth.json`), token passed via `OPENROUTER_BEARER` from
    outside the sandbox, clean cwd `/tmp/bunny`. Verified all those reads return
    `PermissionError`. This removes the path where the model/script reads a
    secret itself.
  - NOT closed by the sandbox: the orchestrating agent can paste a secret into
    the prompt (`ask-bunny "$(cat ~/.env)"` — the `cat` runs in the caller's
    shell, before the sandbox).
  - Decision (web research + consulted GPT Astra `117cec6b` and DeepSeek
    `223dab7f`, 2026-10-02, agree): the only real close without a human
    approval gate (which the user rejected) is capability split — the component
    that talks to the model has no secrets, the component with secrets has no
    channel to it. Egress DLP / secret-value registry / canary tokens are a
    sensor, not a wall (base64, split, paraphrase, "key starts with sk-, 40
    chars" bypass them); keep only as defense-in-depth.
  - Build options (not built; both need a system change, so ask first):
    - A separate macOS user `agentuser` (`dscl`), Bunny consults run as
      `sudo -u agentuser` against a cleaned/secret-free workspace, so the
      primary user's `~/.env`/Keychain/`~/.pi` are unreadable at the kernel
      level — nothing to paste. Secret-needing work stays with the primary user
      behind a narrow broker (unix socket, whitelisted operations) that returns
      a result, not the value.
    - Or run the whole agent in a Linux VM (Colima/Lima) with no home mount,
      only a cleaned repo copy; credentials outside the VM via the broker.
  - Irreducible (both models, explicit): while an agent can both read a secret
    and send arbitrary text to the untrusted model, there is no 100% guarantee
    without a human gate or removing its read access. So the chosen direction is
    removing read access (B), not policy-only (C).
  - Sources: specstory agent-secrets; dev.to "never see your API keys"; DZone
    "4 ways agents exfiltrate secrets"; github.com/fabriziosalmi/aidlp.
  Decided by the user 2026-10-06: parked: Space Bunny is dropped and MiMo
  runs under sandbox-exec; a separate macOS user comes back with the next
  untrusted model.

- [ ] Add a model-selection review workflow for the consult/ask skills.
  - Use official model announcements, CLI release notes and authentication /
    subscription availability first. Terminal-Bench and SWE-bench Verified /
    Pro are candidate sources, not automatic rankings for a read-only
    consultation task. Record benchmark version, date, model snapshot and
    harness/agent settings; do not compare unlike evaluation setups.
  - Treat these user-supplied links as unverified leads, not evidence that
    Opus is better than Sonnet:
    https://www.reddit.com/r/Anthropic/comments/1wso4lj/silly_question_if_sonnet_opus_55_is_better_than/
    https://x.com/BalegaNorbert/status/2102451570608853211
  - Additional sources read in the browser on 2026-10-01, including their
    attached images (claims not independently reproduced):
    https://x.com/BalegaNorbert/status/2102280368909111497 compares dated
    MiMo V2.6 Command Code/OpenCode promotions, including 72-hour / one-week
    windows. Track plan, provider, expiry, actual quotas, overage and normal
    non-promotional pricing; an offer multiplier is not a quality score.
    https://x.com/BalegaNorbert/status/2102055662087786534 claims Qwen 27B
    reproduces an earlier proprietary frontier about six months later.
    Its chart attributes scores to Artificial Analysis Intelligence Index
    v4.3, with current re-evaluations plotted against original release dates
    and roughly 4-bit models in the single-24GB class. Verify the primary
    model pages, index methodology, model/version and deployment details.
    Neither score differences nor parameter counts establish the post's
    "1000x" claim or parity for coding consultations.
  - Also read on 2026-10-01:
    https://www.reddit.com/r/singularity/comments/1wspt5z/gpt6_sol_vs_sonnet_55_at_the_same_cost_per_task/
    The author plots claimed Artificial Analysis scores against API cost per
    task at different effort settings: Sol is claimed more efficient at
    overlapping budgets, Sonnet has a higher maximum-effort ceiling. The
    post separately cites Terminal-Bench 4.0 scores; those are not the same
    metric as the composite Intelligence Index. Verify primary data and
    token accounting (including reasoning/cache) before adopting conclusions.
    Equal token prices do not imply equal task costs, and effort labels are
    not comparable across providers. API dollars/task do not establish
    subscription quota consumption. Do not transfer GPT-6 Sol results to
    GPT-6.1 Sol without matching the exact model snapshot. User comments
    and unverified scores are leads, not grounds for switching defaults.
  - Keep quality, total cost and delivery route separate. Tag CLI subscription,
    hosted API and local weights distinctly; provider wrappers can alter
    harnesses, privacy terms and quotas. For local candidates record hardware,
    quantization, memory/context headroom, latency and throughput; local
    serving is not cost-free merely because there is no API invoice.
    Evaluate read-only consultations separately from tool-using coding
    agents. No purchases, default switches or new provider integration based
    solely on these posts. Verify offers again at decision time.
  - Consulted DeepSeek and Gemini (low/medium/high), 2026-10-01: distinguish
    temporary promotion value from quality; verify primary benchmark data
    and local consultation usefulness, with delivery/privacy constraints.
    Do not treat a screenshot, composite chart or marketing multiplier as
    a reproducible result.
  - Before switching a skill default, verify the exact model through its
    subscribed CLI and run a small representative local evaluation. Compare
    accepted/unique findings, incorrect advice, latency and quota consumption
    using consult-stats. Record the decision and a rollback path; do not
    auto-switch defaults based on leaderboard or social-media claims.
  - Consulted DeepSeek on 2026-10-01: prioritise primary sources, exact model
    identities and local usefulness; preserve explicit selection and report
    unavailable models without silent fallback. No scheduled polling or
    paid benchmark/model calls until the workflow is designed and approved.
  - New leads (user, 2026-10-02), folded in as unverified leads, not grounds to
    switch a default:
    - SuperGrok's "160x more in the subscription than in tokens"
      (https://x.com/PawelHuryn/status/2105703147184239042): a cost/access
      ratio, not a quality signal. It compares a flat subscription's
      theoretical token ceiling with marginal API price and ignores rate
      limits/fair-use, that a sub may be a loss-leader, and that real
      consumption sits far below the cap. "How many tokens do I get" in Claude
      Max 5x vs 20x vs a GPT sub is throughput (how many consultations), not
      competence; tokens of different models are not one unit of useful work.
    - Artificial Analysis AA-Omniscience
      (https://x.com/ArtificialAnlys/status/2105392625788637299): Gemini 4 Argon
      15% hallucination (lowest among models scoring 45+ on the Intelligence
      Index), vs GPT-6 Astra 51% and GPT-6.1 Sol 54% at max effort. A
      general-knowledge hallucination benchmark, not reasoning over an unknown
      codebase. Low hallucination suggests better uncertainty calibration (more
      "I don't know / show me the file", fewer confident false positives),
      genuinely useful for a devil's advocate, but it does not transfer the
      percentages to code review, and a cautious model can also miss more real
      bugs. One recent third-party score is a lead, not a default switch.
    - Consulted GPT Astra (9cf5c878) and DeepSeek (c883180b) 2026-10-02 as
      devil's advocates (both agree): neither argument measures quality. The
      deciding metric stays per-consult verifiable value-add — accepted/unique
      findings, plus false-positives-per-accepted, finding severity, cost per
      accepted finding, and calibration (does it admit "I don't know" and ask
      for evidence) — measured by blind A/B on the same unknown repo with the
      same prompt, and by also scoring misses on cases with known bugs, never a
      leaderboard or a subscription multiplier.
  - More leads (user, 2026-10-02):
    - TerminalBench 4.0 cost/task (https://artificialanalysis.ai/evaluations/
      terminalbench-4-0): user cited Grok 4.7 (xhigh) $14.6, GPT-6.1 Sol (max)
      $1.82, Claude Opus 5.5 (high, with fallback) $5.12. More relevant than
      AA-Omniscience (agentic coding, not trivia) but still not our role:
      TerminalBench is a tool-using agent that solves tasks, we run a read-only
      second opinion. Cost without the paired score is half the picture — on the
      page's score chart the top is Claude Sonnet 5.5 (max, fallback) 63.6%,
      then Opus 5.5 59.6% (Sol's score not surfaced in the fetch), so "cheapest"
      is not "best". Effort labels (xhigh/max/high) are not comparable across
      providers, "with fallback" means the figure is not pure Opus, and API
      $/task is not our subscription-quota consumption (consults bill to the CLI
      subscription).
    - "Space Bunny Alpha", free now on OpenRouter
      (https://openrouter.ai/rankings#leaderboard-table), guessed to be
      MiniMax-M3.1 (https://www.reddit.com/r/SillyTavernAI/comments/1wo8csn/
      comment/pbmo076/): a cloaked model. "Free" is a promo / data-collection
      phase, not a quality score; the identity is a Reddit guess, so it fails
      this item's "exact model identity" rule and can be swapped under us
      (consult-stats could not log the real version). Privacy red flag: consults
      send code, and `-r` repo mode sends the whole checkout including untracked
      files, to an unknown provider with unknown retention (MiniMax is a China
      lab, like DeepSeek). Worth an A/B only through a route that pins the exact
      model id, and only after deciding what code it may see; never the default,
      never for `-r` with secrets.
  Decided by the user 2026-10-06: parked: models are chosen through
  consult-stats and trial rounds; this workflow comes back at the next
  change of the default set.

- [ ] Add easily accessible advisor checkboxes in Herdr so it injects
  `Consult with <selected agents>` into coding-agent requests. Let the user
  select advisors (for example DeepSeek) and disable the instruction easily.
  Consulted DeepSeek 2026-09-30: start with a per-pane/session picker opened
  from a visible `Advisors` control, showing the selected advisors. Inject
  only on an explicit user send, preserve the user's text, preview the added
  instruction and avoid duplicates; do not trigger background consultations.
  Verify each CLI's supported injection path; use a visible, copyable prefix
  rather than silent PTY keystrokes when safe injection is unavailable.
  Decide scope, persistence, timing (every prompt or first turn), advisor
  identity/invocation and multi-client ownership before implementation.
  Make remote-provider privacy and cost implications explicit. These are
  recommendations, not an approved UI design or implementation.
  Decided by the user 2026-10-06: parked: scope, privacy and cost of
  remote providers are open; back when needed.

- [ ] Update check for the fork (deferred 2026-10-02, the user: not announced yet, so
  probably not needed; DeepSeek and GPT agree: defer). Today `herdr_live.sh` (backup,
  rollback) is the update path of the only user, and the updater is off for fork builds.
  Trigger to do it: the first outside user relying on the published binaries, or the
  public announcement. Then in two steps: (1) notify only: compare `(0.9.3, revision)`
  from the embedded `ROHERDR_VERSION` with the newest `roherdr-v*` release of
  `rofrol/roherdr`, show "newer release available" and the download command, nothing
  replaced; local builds (hash instead of a number) do not check. (2) Only when several
  binary users need it, after the upstream rebase: download `roherdr-<os>-<arch>`, verify
  `SHA256SUMS`, stage the file and swap it after the process exits, with a tested rollback;
  if the fork gets a Homebrew tap, leave upgrades to Homebrew instead. Not before the
  upstream rebase (rebase debt). Done: nothing.
  Decided by the user 2026-10-06: parked: the fork is not announced and
  nobody else uses its binaries; back to TODO.md when it is announced.

- [ ] Compact job presentation for the agents the user runs: Pi, Claude Code,
  others (asked 2026-10-01). Today only Pi has it: the Pi activity extension
  (`plugins/job/pi`) folds every tool call (bash, read, edit, write,
  codemode) into one row, whatever the model, and a job's row is a Ctrl+click
  link to the job tab. Claude Code has no tool-rendering API: its Bash result
  is collapsed by Claude Code itself (Ctrl+O expands), but the model still
  receives the whole output, and `herdr-job wait` used to stream the job log
  into it. Other agents (codex, cursor, gemini, opencode, ...) are not used.
  - Consulted DeepSeek, Opus, GPT and Gemini 2026-10-01: all rank the same
    first: make `herdr-job wait` compact at the source, which helps every
    agent without per-agent code, then advice in the agent instructions; a
    PreToolUse hook rewriting `wait` is a brittle fallback; PostToolUse
    cannot change what the UI shows; Monitor is for sparse state changes, not
    log tails; do not build per-agent renderers for unused agents.
  - Done 2026-10-01 (committed, not installed): `herdr-job wait <id>` prints
    one start line (with `herdr tab focus <tab>`), nothing while the job
    runs, and the unchanged final line `herdr-job <id> (<name>): <state>,
    exit <code>`; a failure adds the last 40 lines (at most 8 KiB, escapes
    removed) before it. `--stream` (or `HERDR_JOB_WAIT_STREAM=1`) restores
    the old whole-log streaming; `--quiet` prints only the final line. Exit
    codes are unchanged. Tests: `WaitTests` in `plugins/job/test_herdr_job.py`.
  - Open: tell Claude Code and Pi to prefer the compact `wait` and to read the
    log path only on failure (the user's global instructions already say to
    wait with `herdr-job wait`); a Claude Code `PreToolUse` hook is optional;
    an OSC 8 job link in the Claude output was not added (Claude Code may not
    pass it through); per-agent rows for codex, cursor, gemini and opencode
    stay deferred until the user runs one.
  Moved from TODO.md on 2026-10-06: `herdr-job wait` is compact by default;
  the rest waits until the user runs these other agents.

- [ ] Attach image preview for JPEG/HEIC: the panel shows PNG only (read
  from the file's header; the terminal decodes it). `qlmanage -t -s 512 -o
  <tmp dir> <file>` makes a small PNG of any format without a window or a
  new dependency (MiMo, 2026-10-06), but takes ~100-300 ms, so it needs a
  background run that wakes the client loop and drops results for a row no
  longer highlighted. Parked: screenshots on macOS are PNG.

- [ ] REPL in a sibling pane as an agent tool (idea from the HN thread
  "Why Common Lisp is now the best programming language", 2026-10-06,
  item 49973598; the strongest report is a Pi extension that gives the agent
  a Clojure nREPL eval tool, clj-reload and named subsystem restarts).
  Parked: the user works mostly in Rust (Herdr, rmpc) and rarely in Python,
  so a live image buys little; Rust's feedback lever is build latency, which
  the warm clean-tree build already covers.
  - Revisit only after 3 real occasions within a week where rebuilding
    process state (fixtures, imports, services) dominated an iteration.
    Then try a skill recipe for one language before any API.
  - Pitfalls found when consulting sol and MiMo (checked against
    `herdr pane wait-output --help`): `wait-output` searches existing output,
    so a sentinel matches its own echoed input; build the marker at runtime
    and match a whole output line. A sentinel does not prove the output is
    complete (scrolled away, skipped by an exception, late background
    output). A timeout means "completion unknown": never retry side effects.
    Autoreload leaves stale state (decorators, re-exports, class
    attributes): verify in a fresh process. The REPL pane must be owned by
    the agent; prompt detection is less reliable than sentinels.
  - If an API is ever needed: a server-owned output cursor (capture armed
    before sending, explicit truncation, timeout and pane-exit states), not a
    `pane eval` that promises language-neutral evaluation.

- [ ] Live handoff can garble a primary-screen pane (user, 2026-10-02,
  screenshot 10 s after installing `bcf83e77`; rare, fix only if it happens
  again): in a Claude Code pane the caret sat one row below the prompt, a
  few columns right, and blinked fast; one earlier output line read
  `[1Brestarcuruchamiałby...`, a literal `[1B` with every space gone.
  - Cause found in the code: the PTY reader is paused at any byte, not at
    a parser ground state, and the parser's pending sequence is not handed
    over, so the new parser printed `[1B` without its ESC. Primary-screen
    handoff sends only `initial_history_ansi` (text, no cursor position,
    pending wrap, DECSCUSR style or DECTCEM visibility); only the alternate
    screen carries cursor state. Claude Code's relative cursor moves then
    start from the end of the replayed history.
  - Not explained: the lost spaces (overwritten from the wrong origin, or a
    lossy history replay) and the fast blink (DECSCUSR has no speed; maybe
    `?25h/l` toggling at the wrong place or the detached client's stale
    cursor).
  - Consulted sol, DeepSeek and Space Bunny (MiMo timed out). Plan: a
    differential test (the same bytes parsed straight through vs. handed off
    at every byte of short CSI and UTF-8 fixtures, including a reader's
    read-ahead buffer); cut only at ground state or hand over the pending
    bytes; carry primary cursor position, pending wrap, style and
    visibility; compare old and new grids right after replay, before the
    child writes or the nudge (a same-size SIGWINCH may not redraw on
    macOS).

- [ ] Consult stats: pair the coordinator with Opus at a lower effort
  (`claude -p --model <same id> --effort low`, fresh context without project
  instructions or tools) to measure what effort buys.
  - Method (consulted 2026-09-26): same prompt against a fresh-context call
    at the coordinator's effort; the self entry is only a baseline (full
    context, rates itself). Pilot 10 rounds, conclude after 20-30, rate blind
    where practical, "unique" only relative to that round's roster. It uses
    the same subscription, so log failures, never drop them.

- [ ] Usage widget: include minutes in reset countdowns (e.g. `2h 15m`,
  `45m`, `<1m`), not just whole hours. For weekly limits above 24h, show
  days plus remaining hours (`34h` → `1d 10h`), not just whole days.
  - Consulted DeepSeek 2026-10-01: share the footer/modal countdown policy:
    >=24h days + hours, >=1h hours + minutes, >=1m minutes, positive <1m
    `<1m`, expired `now`. Floor units and omit zero secondary units;
    `24h 30m` therefore shows `1d`. Check footer width and boundary tests
    (23h 59m, 24h, 34h, 48h). Presentation only; no provider/API changes.
  - Countdown format done 2026-10-01 (committed, not installed): the
    footer shows `1d10h`, `2h15m`, `45m`, `<1m`, `now` without inner spaces so
    a cell keeps five columns (cells start at columns 4 and 14); the modal
    shows `1d 10h`, `2h 15m`. Boundary tests cover 59s, 24h, 24h30m, 34h,
    48h and 23h59m. The two bullets below (reset entitlement research,
    redeemable resets, credits) are still open.
  - [ ] Investigate whether Anthropic offers a reset entitlement comparable
    to the user's ChatGPT Plus `Full reset (Weekly + 5 hr)` observation, and
    whether it could explain successful Sonnet calls at weekly 100%.
    This is a hypothesis, not an established Anthropic feature or cause.
    Distinguish scheduled renewal, a manually redeemed reset, model-specific
    allowance, delayed/aggregate telemetry and paid usage credits.
    Record the exact model, plan/auth mode, timestamps, displayed buckets,
    rejection/reset text and any actual redemption or billing evidence.
    Use official Claude/Claude Code subscription docs and account usage /
    billing UI, not API Console limits as proof of subscription semantics.
    Do not redeem anything, enable paid overage or expose credentials.
    Consulted DeepSeek and Gemini (low/medium/high) on 2026-10-01: successful
    calls alone cannot identify the mechanism; none verified an Anthropic
    reset grant. Gemini's API headers/Console suggestions are not evidence
    for Claude Code subscription quotas. Claude consultation was deferred
    after the actual weekly-limit rejection until 02:00 Europe/Warsaw.
  Also show how many redeemable quota resets are available, their types /
  scope, and when each expires; keep these separate from automatic limit
  renewals. Show a compact count in the footer and details in the usage modal.
  - User example: ChatGPT Plus shows `Full reset (Weekly + 5 hr)` and
    `Expires October 5` at https://chatgpt.com/settings/usage?tab=overview.
    This is a user observation, not a verified entitlement for every Plus
    account; do not invent the expiration year, time or timezone.
  - Consulted DeepSeek: unknown or unavailable reset data is not zero;
    count only available grants, not used or expired ones. Keep the footer
    and modal consistent, indicate stale data, and show an exact expiration
    with timezone when the source provides it.
  - First verify an authenticated, supported source for grant data; retrieval
    remains blocked until then. Do not invent endpoints or scrape browser
    credentials. Read-only display: redeeming a reset is out of scope.
  - Also distinguish subscription allowance from paid overage / usage
    credits. User example from Anthropic: `Turn on usage credits to keep
    using Claude if you hit a plan limit.` Settings page:
    https://claude.ai/new#settings/usage. Clearly indicate in the footer
    and usage modal when current usage is billed to credits rather than
    included in the subscription, with balance / spend when available.
    Distinguish credits disabled, enabled as a fallback, and actually in
    use; enabling credits alone does not prove paid usage. Require verified
    data for the same account; otherwise show unknown, never infer billing
    solely from an exhausted plan limit. Keep subscription overage separate
    from Anthropic Console API billing. Display only; do not enable credits
    or change billing settings.
  - [ ] Remind me to redeem a reset before it expires (user, 2026-10-06:
    "clicking reset when it gets close to expired": an unused `Full reset`
    is wasted when it expires). Herdr never redeems it and has no verified
    source for the grant, so the first slice is a manually entered expiry
    with an optional link to the usage page, labelled unverified ("check
    your reset offer", it may already be used), dismissable and markable as
    redeemed. A date without time or timezone reminds conservatively on the
    day before, never at an invented midnight; a date without a year is
    not guessed (the `Expires October 5` example had passed by 2026-10-06).
    Consulted sol and MiMo (round `20261006-144141-fb4f`): one nudge when a
    limit is actually hit while a recorded grant is unexpired (when the
    reset pays off); a ~24 h lead-time nudge too, which MiMo would send only
    while a limit is hit (otherwise it nags a user under quota); dedupe to
    one nudge per grant per day. Take an API field if one appears; never
    parse the settings page.

- [ ] Usage modal (click the footer) / settings: checkboxes choosing which
  providers the usage footer shows. Also token-based usage?
  - Consulted models (DeepSeek, GPT-6 Astra, GPT-6 Luna, 2026-09-26): the
    checkboxes, yes. Tokens answer a different question ("what did this
    cost?") than the footer ("can I keep going?"): if ever, a separate usage
    details view, not the footer. Transcript scraping is brittle (resumed
    sessions, retries and cache tokens double-count), so only with a concrete
    need; split input, output and cache, and label estimates.
  - Deferred (consulted 2026-09-26): premature with few providers; built-in
    settings widgets are enough, no plugin settings framework needed.
  - Revised 2026-09-28: put the checkboxes in the usage modal that opens when
    I click the footer, not in settings. Also show usage of my other
    workspaces: I have extra workspaces in OpenAI (platform projects) and
    Anthropic (Console workspaces). Maybe per API key too.
  - Consulted models (GPT-6 Astra, DeepSeek, 2026-09-28): three different
    things, keep them apart: footer visibility, subscription allowance, API
    spend.
    - Checkboxes mean "show in footer", not "poll": hiding a row must not
      stop polling (`[usage].<provider>` stays the poll switch). It is TUI
      presentation state: persist it on the client side, never in
      `usage.read`. The modal keeps listing hidden rows so they can be turned
      back on, and tells apart hidden, polling off, no credentials and
      refresh failed.
    - Workspaces only through the admin APIs (`sk-ant-admin…`, `sk-admin…`),
      and those give spend and tokens, never a remaining balance or budget:
      Anthropic `GET /v1/organizations/cost_report` (group by `workspace_id`)
      and `usage_report/messages` (by `workspace_id`, `api_key_id`, `model`);
      OpenAI `GET /v1/organization/costs` (by `project_id`, `line_item`) and
      `usage/completions` (by `project_id`, `api_key_id`, `model`). Verify
      against the docs before building; neither model could fetch them.
    - Per API key: tokens only; cost per key would be an estimate from
      prices (cache, batch, price changes). Cut for now.
    - Workspace spend goes in a separate "API spend" section of the modal
      (month to date, currency, scope, when observed), never as footer rows:
      the footer stays a compact allowance strip. Label it "Anthropic API
      spend", distinct from the Claude subscription row. Cost reports lag by
      hours: own slow refresh, not the allowance poller.
    - Admin keys read org-wide billing: opt-in, own env var or auth file
      entry, server side only, never in API responses, logs or the shared
      `usage-cache.json`.
    - Order: footer checkboxes in the modal; then API spend per workspace
      (ties in with the OpenAI API row above); per-key usage only on real
      need.

- [ ] herdr > menu > settings > consults: an "enabled" checkbox column per
  model (which models get consulted), and next to it the consult stats
  columns. Then drop the separate "consult stats" menu item.
  - Consulted models (DeepSeek, GPT-6 Astra, GPT-6 Luna, 2026-09-26): consult is
    a plugin, so core needs a declarative plugin settings page (a versioned
    schema of tables, checkbox fields, loading/error states, actions proxied
    over the endpoint); core renders standard widgets, no plugin-drawn TUI and
    no consult-specific code in core. Keep latency/tokens from crushing the
    checkbox and model name (a detail view per model).
  - Deferred: one consumer does not justify a plugin settings framework yet.

- [ ] herdr > menu > settings: the consult skills (gpt, gemini, deepseek,
  consult-stats) get their own settings section, like Integrations (agent
  hooks) but a separate item: per agent (Claude Code, pi) whether the skills
  are installed, with an install button. Today only the "Consult: install
  skills" popup runs `plugins/consult/install-skills`.
  - Consulted models (GPT-6 Astra, DeepSeek, 2026-09-28): both advise against
    a core section for now (one plugin, four coupled skills); the missing
    value is status, not placement. Start in the plugin: the popup shows the
    state per agent and installs or repairs.
  - If it goes into core, make it generic, not consult-specific: plugins
    declare skills in `herdr-plugin.toml`, the server advertises new methods
    (`skill.list`, `skill.install`), and agents are data-driven strings;
    never new `IntegrationTarget` variants (frozen enum). Installing is not
    enabling: per-model checkboxes and stats stay on the consults page above.
  - One row per bundle × agent (the four skills go together: the scripts find
    `consult-stats` as a sibling), expandable to skills. States: linked,
    missing, broken link, conflict (a real directory at the destination),
    mixed; warnings: possibly shadowed by `~/.agents/skills`, plugin
    installed as a copy (links into it break on update). Install/repair
    never overwrites foreign files; uninstall (later) removes only links
    that point into the plugin. Agents without a skills directory are
    "unsupported", not "missing".
  - The server writes into its own host's home: with remote endpoints show
    which host is affected. Windows symlinks need their own handling.
  - Smallest stage: read-only status plus bundle install/repair in the
    popup; a settings entry, if any, only opens that popup.

- [ ] A "consult models" checkbox in herdr's bottom bar (on/off), or instead
  checkboxes next to the models to consult (GPT Astra, DeepSeek, Gemini), so
  I choose in the UI whether and whom agents consult, instead of the rule in
  the agent's memory ("before design decisions consult GPT Astra +
  DeepSeek").
  - Open: how the state reaches a running agent (a state file the consult
    skills read, plus a hook such as Claude's `UserPromptSubmit` injecting
    "consult: on, models: astra, deepseek" so the agent knows before it
    decides); scope (global, per workspace or per agent pane); core footer
    or the consult plugin (plugins cannot draw widgets today). Overlaps the
    per-model "enabled" column on the deferred settings > consults page.
  - Consulted models (GPT-6 Astra, DeepSeek, 2026-09-28): both agree. A
    checkbox promises more control than herdr has: it cannot force a
    running agent to consult, and "off" must beat the rule in the agent's
    memory. Per workspace (consult policy follows the project; global leaks
    into unrelated work, per pane gets lost when panes restart). One toggle
    first, the model list in the same state file; per-model checkboxes mix
    "whether" with "whom" (fallbacks, missing keys). In the consult plugin
    (menu action plus popup), not the core footer, which would give one
    plugin privileged UI. Delivery: a workspace state file as the source
    of truth, a prompt hook injecting `[herdr consult policy] enabled=…
    models=…` every turn, and the consult scripts re-reading it before
    sending, so "off" is enforced, not advisory. pi has no such hook: say
    so. Astra: label it "Auto-consult" and decide whether my explicit
    "consult X" bypasses off; show which providers get the code.
  - Smallest stage: that state file, a plugin menu toggle, the Claude hook
    and the dispatch-time check, logging policy against actual consults.
    Kill it if agents ignore it; a footer checkbox only if I flip it often.
  - Inject every turn, only for questions, or only on change? Consulted
    models (GPT-6 Astra, DeepSeek, 2026-09-28), both: not only for
    questions (the hook sees my prompt, not the agent's decision point;
    "implement X" hits design choices mid-turn, a classifier adds latency
    and misses). Not every turn either (repetition primes over-consulting).
    Inject at session start (startup, resume, clear, compact) and on
    `UserPromptSubmit` only when the policy's generation counter differs
    from the one last injected into that session (per session, not per
    workspace; a `PreCompact` dirty flag forces reinjection). Inject even
    when the policy matches the default, and replace the memory rule with
    "follow the herdr consult policy", so there is one source of truth.
    The scripts re-check the state file right before sending; a missing or
    broken file means "auto off" with a clear reason, not a silent "on".
    Subagents may never see the line: the script prints the policy on its
    first call. Explicit "consult X" bypasses auto-off and the model list
    (a separate hard "no external consult" switch, if ever needed, would
    not be bypassed); scripts take an `--explicit` flag, logged. Astra: one
    shared dispatch layer for all consult scripts instead of a brittle Bash
    `PreToolUse` matcher.
  - Idea: when a checked model's limit is exhausted, grey its checkbox out
    and leave it out of the injected policy, so the agent does not try it.
    herdr already has the signals in the usage footer (`usage.read`, cached
    in `usage-cache.json`): Codex rate-limit windows, DeepSeek balance
    (`is_available`), Gemini weekly quotas from `agy -p /quota`.
  - Consulted models (GPT-6 Astra, DeepSeek, 2026-09-28): both: an
    availability hint, not a hard gate yet. A false "exhausted" silently
    drops a working model for hours, which is worse than one failed call;
    unknown means available. Keep the checkbox as my intent and show
    availability as a separate badge with the reason, when it was observed
    and the reset hint; a checked-but-grey box reads as "on but not on".
    Never gate GPT on `usage.read`: the footer reads Codex's own ChatGPT
    login, the gpt skill uses pi's `openai-codex` token, possibly another
    account. Better signal: the consult scripts' own classified failures
    (provider, model, credential hash, hard quota vs rate limit vs auth vs
    outage, timestamp) logged by consult-stats. DeepSeek `is_available =
    false` is trustworthy for a hard zero; Gemini's quota only when `agy`
    uses the same account and the group covers the model. Reset times are
    hints ("try again tomorrow" cleared in two hours): at reset go back to
    "unknown", allow one try, re-block with a bounded TTL; a "retry now"
    action.
  - Stages: classify and log failures in consult-stats; show the badge; the
    scripts fail fast only on a same-credential hard quota failure within
    the TTL (unless I ask explicitly); drop models from the hook only if the
    data shows agents wasting turns on exhausted ones.
  - Sort the model checkboxes by the consult-stats ranking (`consult.py
    stats`: accepted unique findings per rated call, e.g. DeepSeek 1.57,
    Astra 1.56, Gemini 0.57 on 2026-09-28), with the number next to each
    model; models under 5 rated calls go last, as in `stats`. The ranking
    depends on which models were asked together, so it is a hint, not a
    verdict. Do not reorder while the pointer is over the list (as in the
    agents' `prio` sort).

- [ ] Shared checkout awareness: agents in one checkout do not know about
  each other. On 2026-09-28 another session started editing `src/` minutes
  after this one checked `git status`; only commits by explicit path kept
  the two fixes apart. The policy ("ask whether to use a worktree when the
  checkout has code changes that are not yours") stays in `AGENTS.md`;
  herdr would add the facts only it knows and, when the checkout is shared
  or has code changes, tell the agent to ask me whether to create a
  worktree before it edits code (never create one on its own).
  - Consulted models (GPT-6 Astra, DeepSeek, 2026-09-28), both: herdr gives
    facts, the repo gives the rule (what counts as code vs notes, whether
    to ask, warn or require a worktree). Never claim whose changes they
    are: after resume, `/clear` or compaction an agent can take its own
    edits for foreign ones, so say "ownership unknown".
  - Timing: a line at session start only when the checkout is already
    shared or has code changes (it goes stale in minutes); the real check
    before the session's first file edit (`PreToolUse` on `Edit`/`Write`,
    fresh state); not every prompt (noise). DeepSeek: optionally warn at
    commit when it includes files this session did not edit.
  - Limits: advisory, not a lock. Edit hooks miss shell edits (`sed`,
    scripts); hookless agents only get the CLI. Separate from the
    auto-consult injection above, which refreshes on a policy generation,
    not on checkout state. A worktree per agent is the real isolation but
    costs a cold `target/`.
  - Smallest stage, in the fork (plugin, opt-in), not upstream by default:
    `herdr checkout status` (other agent panes in the same git root and
    worktree, uncommitted code files, when observed), then the first-edit
    hook, e.g. "2 other agents share this checkout (panes 3, 7); 4
    uncommitted code files, ownership unknown. Ask the user whether to
    create a worktree before editing code."
  - Idea: a herdr setting for the worktree policy. Consulted models (GPT-6
    Astra, DeepSeek, 2026-09-28), both:
    - Per repo, keyed by the common git dir so linked worktrees share it;
      a global value only as the default; not per workspace (a UI
      grouping, not a checkout). Modes: `shared` (default), `ask`,
      `always` (a new worktree per new agent session). Not "never ask": it
      names the prompt, not where the agent works. DeepSeek: if the
      setting contradicts `AGENTS.md`, herdr says it overrides the repo
      rule, never silently.
    - `ask` triggers on another live agent in the same checkout OR
      uncommitted changes (tracked, staged or untracked; do not classify
      code vs notes). The 2026-09-28 race began from a clean `git status`,
      so "has changes" alone misses it.
    - herdr asks in its TUI and creates the worktree before the agent
      starts (pane cwd = worktree); an agent that moves itself later
      leaves its session and relative paths in the old checkout. The hook
      text then carries only facts. Serialize herdr's occupancy check so
      two launches cannot race.
    - `always` costs a cold `target/` per worktree: a shared
      `CARGO_TARGET_DIR` contends on cargo's lock and rebuilds on
      differing flags, sccache skips linking. Every fix must land on
      current `master` before `scripts/herdr_live.sh install`; never merge
      or install automatically.
  - Missed by both: herdr usually does not launch the agent (I type
    `claude` in a shell pane; herdr detects it after it starts) and cannot
    move a running agent's cwd. A launch-time prompt only works when herdr
    starts the agent (pane command, relaunch, `herdr worktree create`).
    Otherwise: a notification when a second agent is detected in the same
    checkout ("agent in pane 3 shares this checkout") with an action that
    creates a worktree and restarts the agent there, plus the first-edit
    hook as the fallback.
  - Stages: `herdr checkout status` without any setting; then a per-repo
    `shared`/`ask` setting (launch prompt, detection notification);
    `always` only once branch naming, resuming a session in its worktree
    and cleanup of finished worktrees are reliable. The setting makes me
    choose between instant visibility on `master` and isolation; it does
    not reconcile them.

- [ ] Workspace recipes (tmuxp-like): a TOML file under
  `~/.config/herdr/recipes/` naming a root, panes, splits and commands.
  - Consulted models (GPT-6 Astra, DeepSeek, 2026-09-27): a plugin built on
    the `dev-layout-bootstrap` example (`ogulcancelik/herdr-plugin-examples`),
    not core. Apply reconciles: create missing panes, leave running
    processes and hand-made panes alone, re-apply is a no-op, removal only
    with an explicit `--prune` (panes may hold uncommitted agent work).
  - Deferred: sessions already survive server restarts with 48 snapshots;
    recipes only help on a new machine or a fresh checkout. Build it when I
    notice rebuilding the same layout by hand.

- [ ] Explain the consult/ask naming mismatch: the plugin (`plugins/consult`,
  `local.consult`) and the stats skill (`consult-stats`, `consult.py`) say
  "consult", but the scripts inside the skills say "ask" (`ask_gpt.sh`,
  `ask_gemini.sh`, `ask_deepseek.py`). Decide whether it is deliberate (the
  verb an agent runs vs. the feature name) or should be unified, and on
  which name; consult the agents (DeepSeek, GPT-6 Astra) before renaming.

- [ ] The notification history does not say what the agent asks (user,
  2026-10-01, screenshot `Screenshot 2026-10-01 at 14.19.39.png`): rows read
  `14:18 claude needs attention · email-assistant · 2 …` and `14:19 claude
  finished · email-assistant · 2 · 3`. The `2` is the workspace's position and
  the `3` an auto tab number, neither means anything to the user, and the task
  and the request never appear (the 56-column panel cuts the rest).
  - How it is built today: title `<agent> needs attention|finished`, body (the
    context) `<workspace> · <position> · <tab label if the workspace has
    several tabs>` (`notification_context`, `src/app/actions.rs`); the record
    has agent, workspace/tab/pane ids, kind and time but no task. macOS system
    notifications already use the agent's terminal title as their message.
  - Consulted DeepSeek, Opus 5.5, GPT sol 6.1 and Gemini high 2026-10-01 (all
    four agree): row `time ? task · request` (`14:18 ? Fix IMAP retry loop ·
    Allow Bash: npm test`, `14:19 ✓ Fix IMAP retry loop`); add two optional
    fields to `NotificationRecord`, `task` (the terminal title captured when
    the notification fires, not the current one; treat `zsh`, a bare path or
    the agent name as none) and `request` (the agent's own message with the
    blocked state: Claude Code's notification hook message, Pi's reported
    message), both sanitised (no escapes or control characters, one line,
    about 80 and 160 characters) and optional so old clients ignore them
    (frozen generation-1 contract); no screen scraping (fragile, can copy
    secrets); drop the workspace position and auto tab numbers; keep one line
    per row (15 rows) and show the highlighted row's full task, request,
    agent, workspace and a meaningful tab label in a detail footer; truncate
    the workspace first, keep the request visible; fall back to `Input
    needed; open pane` when there is no request, never invent one.
  - Tests: old JSON without the fields; sanitising; useless titles; 56 / 40 /
    20 columns with CJK and emoji; every kind; `task` is a snapshot at
    notification time; auto tab numbers hidden; a click still jumps to the
    right pane; the 100-entry bound.
  - Done 2026-10-01: nothing yet (scoping the change in the server first).

- [ ] Too many notifications while the agent keeps working (user, 2026-10-01,
  screenshot of the history: `14:19 claude finished` and `14:19 claude needs
  attention` for the same pane): "what do I need this notification for, if
  the agent keeps working anyway?" For one Claude Code pane the history shows
  finished / needs attention / finished / needs attention x2 / finished within
  two minutes.
  - Cause (from the code, not reproduced): the in-app toast path waits
    `ui.toast.delay_seconds` (default 1 s) and notifies only if the pane is
    still in the same state (`pending_agent_notifications`), but the path that
    feeds client shells, the history list, system notifications and sounds
    (`forward_semantic_agent_transition`, `src/server/headless/notifications.rs`)
    sends and records at the transition itself, with no delay and no same-state
    check, and does not dedupe repeats.
  - Consulted DeepSeek, Opus 5.5, GPT sol 6.1 and Gemini high 2026-10-01; they
    agree on: one eligibility policy before every channel (history, shell,
    system notification, sound, toast), one pending notification per pane that
    any state change replaces and a return to `working` cancels; a settle delay
    (needs attention 3-5 s, awaiting reply 1-2 s, idle/turn ended 10-30 s);
    dedupe a repeated needs-attention for the pane until the user interacts or
    the agent worked for about 5 s; suppress when the pane is visible and the
    client focused (not merely the active tab); history keeps only delivered
    notifications (superseded ones, if kept at all, in a separate diagnostic
    log); name the kinds honestly: "Reply needed" (awaiting reply), "Needs
    approval" (blocked), "Turn ended" (idle after a turn), and keep "Finished"
    for a trustworthy end (process exit, explicit completion); positively named
    options (`attention_delay_seconds`, `reply_delay_seconds`,
    `idle_delay_seconds`, `notify_while_viewing`, ...); the existing
    `ui.toast.delay_seconds` stays as an alias.
  - Tests (fake clock, a pure `NotificationPolicy` state machine): the user's
    14:17-14:19 sequence yields at most one notification; blocked then working
    after 1 s yields none and no history row; a blocked state held 4 s yields
    one; a repeat without interaction yields none; interaction rearms; awaiting
    reply is immediate; focus and visibility suppress; panes are independent;
    toast, shell and history get the same stream.
  - Done 2026-10-01 (committed, not installed; `just check` passes): the
    delay now applies to the semantic path as well: the client-shell
    notification and the history row are sent when the delay has run out and
    the pane is still in the same state (`forward_agent_notification_delivery`),
    so `blocked` then `working` within the delay leaves no notification and no
    history row. One wait for every kind, `delay_seconds`, default 3 (upstream
    1); `delay_seconds = 0` is still instant. (A separate
    `finished_delay_seconds` was added and removed the same day: the user did
    not understand two numbers, and the models say to have one internal
    stability check.) Test:
    `a_notification_the_agent_undoes_within_the_delay_reaches_nobody_...`. Not
    done from the list: dedupe of a repeated needs-attention until the user
    interacts, suppression by pane visibility plus client focus (still by
    active tab), honest kind names ("Reply needed", "Needs approval", "Turn
    ended"), the diagnostic log.
  - The user's question (2026-10-01): "I do not understand the 3 s / 10 s logic.
    Is it that something needs my attention, or that something finished but
    does not need me?" Consulted DeepSeek, Opus, GPT and Gemini: they agree on
    a model of three words: Needs you (a permission/approval prompt, an
    awaiting-reply question or a failure: one category, alert and keep the row
    until answered), Done (a turn or task ended without a question: quiet,
    history and a dot, no system notification or sound while the user is at the
    computer), Error (immediate). A turn that ends with a question is only
    Needs you, never a Finished/Needs-attention pair. Merge blocked and
    awaiting-reply. One internal ~3 s stability check, not a setting (if
    shown: "Only alert if it still waits for me after 3 seconds"). History:
    one row per agent episode, updated in place; a "Needs me" filter on top.
    Open decision for the user: should "finished" alert at all?
    Related: the entry above about what the history rows say.

- [ ] A sound but no notification for an approval prompt (user, 2026-10-01,
  screenshot): a Claude Code permission prompt ("This command requires
  approval", 1 Yes / 2 Yes and don't ask again / 3 No) appeared in the tab
  that was displayed; the sound played, nothing was shown.
  - Cause (client notification policy, `src/client/shell/notification_policy.rs`,
    read in the code and confirmed by the server's history, which recorded
    `needs_attention` for that pane at 14:43:56 and 14:44:58): with
    `delivery = "system"` and the Herdr window focused, `herdr_toast` is
    true, and a target tab that is the focused tab counts as "looking at it"
    (`target_active`): neither Herdr's toast (`herdr_toast && !target_active`)
    nor the system one (`!suppress_external`) is shown, while the needs-attention
    sound still plays (only Finished sounds are suppressed). By design, but the
    result is a phantom ping.
  - Consulted DeepSeek, Opus 5.5, GPT sol 6.1 and Gemini high 2026-10-01: all
    four say sound without a visible cue is wrong; focused tab is not focused
    pane is not a user looking; suppress the system toast (it would cover the
    window) but show a compact in-window cue: a Herdr toast or chip ("Claude
    needs approval · tab/pane", click to go there) plus a persistent mark on the
    sidebar row until answered; one alert per episode, no repeated toast or
    chime for a repeated needs-attention. Disagreement on whether recent input
    in that pane (15-30 s) should suppress the toast: DeepSeek, Opus and
    Gemini yes, GPT no (input is not proof it was seen).
  - The user's answer (2026-10-01): the toast need not appear when the tab
    is the active one, but the notification should appear at the top of the
    notification list (the `✉` dropdown). I tried a Herdr toast for the active
    tab and took it out again. What the list does today: the server records
    the `needs_attention` (14:43:56 and 14:44:58 are in `notification.list`),
    the dropdown fetches the list when it opens and shows the newest first, but
    the `✉` badge counts only tabs that are not shown (`notification_log_
    received`), so a question in the displayed tab raises no count, and
    nothing marks the row. Open: make the row stand out (an unread mark and
    the badge count until the prompt is answered or the tab's state changes),
    and say in the row what it is (see the next entry).
  - Not done: the persistent row mark in the sidebar, the input-recency rule,
    deduping repeats per episode.

- [ ] History rows for a repeated notification from one session (user,
  2026-10-01, screenshot: `14:42`, `14:39`, `14:38 claude finished · ~ · 7 · 4`):
  "I cannot see WHAT this claude finished", and "shouldn't a new notification
  from the same session clear the previous ones?"
  - Today the client already replaces a pending, queued or visible toast of
    the same pane when a new one arrives (`receive_notification`), but the
    server's history list keeps every record.
  - Consulted DeepSeek, Opus, GPT and Gemini 2026-10-01: merge, do not append:
    key by pane; a new unread event of the same kind replaces the older unread
    row (newest time, a counter `x3`, first time kept); a seen row stays as it
    was and the new event starts a fresh unread row; a different kind stays
    separate (a finished must never hide an unanswered needs-attention);
    needs-attention that was answered is marked resolved/greyed; the unread
    badge counts tabs with unread rows, not rows. Row text: `time mark task ·
    agent · cwd@branch · duration x3` from a snapshot taken when the event
    fires (terminal title, treating generic titles like `claude` as none), with
    the fallback agent summary, then `basename(cwd)@branch`, then cwd (`~`); no
    workspace position or auto tab number. Closed pane: keep the row, note it,
    click opens the tab or says so. Memory only. Tests as in the two entries
    above.
  - The user's rule (2026-10-01): in the displayed tab, seeing the agent's
    question, no toast is needed; but a sound from an inactive tab without a
    list entry is wrong. Consulted again: every sound, toast or system
    notification must have a matching list entry (one-way: an entry may exist
    without an alert); a needs-attention in the active tab is recorded too
    and counts as read at once (no badge); a finished in the active, focused
    tab makes no alert and so no entry. With the delayed path the entry is
    created exactly when the alert is delivered (`forward_agent_notification_
    delivery`), which keeps that invariant; the installed build still records
    at the state change.
  - Done 2026-10-01 (committed, not installed): `NotificationRecord` has two
    optional fields, `task` (the pane's cleaned terminal title when it fired;
    none for a shell or agent name, a path or an empty title; one line, 80
    characters) and `repeats`; a pane's newest entry of the same kind gives way
    to a new one that counts it (`x3`), a different kind stays separate; rows
    read `✓ Fix the login test · claude · herdr x3` (`?` for needs attention),
    without the workspace position or the tab number
    (`notification_row_text`); the generated API schema is updated. Not done:
    the `request` field (the agent's own message), the detail footer for the
    highlighted row, marking a row read or resolved in place, the badge rule for
    merged rows, a closed-pane note.
  - Done 2026-10-01 (second part): the list shows the highlighted row's whole
    text, wrapped to at most 3 lines, under a rule below the rows
    (`wrap_detail` in overlays.rs).
  - Done 2026-10-01 (third part): `NotificationRecord.request` (optional): for
    a needs-attention entry the server takes the agent's own hook-report
    message (`hook_authority.message`, the approval prompt or question),
    cleaned to one line of at most 300 characters; the row text appends it as
    `— "..."` and the footer shows it whole. A finished entry carries none, and
    a pane whose agent reported no message shows no request (agents that only
    block on screen text have none; a detection-based fallback would need the
    screen text and is not done). Still not done: marking a row read or
    resolved in place, the badge rule for merged rows, a closed-pane note.

- [ ] "What do I do with this update?" (user, 2026-10-01, screenshot): Claude
  Code shows `✓ Update installed · Restart to update` under its input box.
  - What it is: Claude Code updated its own binary in the background; the
    running session keeps the old version until it is restarted. Nothing is
    lost by waiting (the old version runs on); restarting picks up fixes and
    features, costs the process state (background tasks, an unsent draft) and
    a warm prompt cache, and the conversation comes back with `claude
    --resume <id>`.
  - What to do now (consulted DeepSeek, Opus, GPT and Gemini 2026-10-01, all
    agree): finish the current exchange, then, with the agent idle and the
    input box empty, run the `restart` plugin's menu action "Restart agent in
    this pane" (or "Restart idle agents in this workspace" for several); it
    sends SIGTERM, waits for the shell and runs the same command line again
    with `--resume <id>`, skipping working or blocked agents and a pane with
    unsent text. It is not urgent.
  - What Herdr should do (this extends the "Restart agents..." entry): detect
    the pending update from the version, not the screen text: record `claude
    --version` when the pane starts and compare it with the binary on disk
    (re-check when the file's mtime or the symlink target changes); use the
    "Update installed" text only as a hint to verify; no Claude hook reports an
    update. Default: a quiet "update pending" mark on the pane and its sidebar
    row plus a one-click restart and a preview/picker for a workspace; cleared
    only after the new version is confirmed. Automatic restart of idle agents
    is opt-in, never the default. Batches: one pane at a time, re-check
    "idle, empty input, not focused, no key in the last ~5 s" just before each
    SIGTERM, keep the exact argv, cwd and environment, replace conflicting
    resume arguments, never replay a prompt, stop the batch on the first
    failure and keep the pane, its scrollback and the session id; if resume
    fails, never start a fresh session silently.
  - Tests: version bump and mtime or symlink change set the mark, the same
    version does not; busy, blocked, draft and typing panes are skipped; the
    state changing between the check and the SIGTERM; flags survive; an
    unknown session id is an error, not a new session; a 10-pane batch runs
    in order and stops at the first failure.
  - Done: nothing yet (the plugin's stage 1 exists).

- [ ] Live "working" and "asking" indicators next to the notification button
  (user, 2026-10-01: "at the top next to the notification icon add a working
  icon and a count; I can click and a list opens. Same for those that ask.
  Do it next."). Consulted DeepSeek, Opus and GPT (all agree on the core):
  - Header, right to left: `✉n` (unchanged), `?n` (needs you), `◐n` (working,
    the existing animated half circle); counts derived from the snapshot the
    client already has, no background request; keep the sort/filter buttons,
    shorten the "Spaces" title first, then hide the working count.
    Open question between the models: hide a zero count (Opus, DeepSeek dims)
    or keep it dimmed for stable hit targets (GPT, DeepSeek).
  - Counts: distinct `pane_id`; asking = `Blocked || awaiting_reply`; working =
    `Working` and not asking (attention wins); herdr-job tabs are not agents,
    so they stay out (maybe a separate indicator later); the focused pane
    counts (looking at it is not an answer) but shows dimmed in the list.
  - Dropdown (one at a time, like the envelope list): one row per agent, task
    (terminal title) first, then `space / tab · agent`; an asking row says
    approval or reply; sorted by time in the state only when a reliable
    timestamp exists (else workspace/tab/pane order); about 8-10 rows, then
    scroll; the highlighted row shows its whole text below; Enter or click
    jumps to the pane's tab; Esc or an outside click closes.
  - Relation to others: the envelope is history, these are the live queues, so
    an overlap is fine; opening them does not mark notifications read; the
    hidden Agents panel stays the full inventory.
  - Tests: counts from snapshot fixtures (0, 1, many, Blocked plus awaiting
    on one pane counted once), layout at 32 and 44 columns, a row vanishing
    live while the list is open (highlight follows the pane id), jump target,
    no request sent.
  - Done 2026-10-01: `?n` (asking) and `◐n` (working, the animated glyph)
    beside `✉` in the spaces header; counts from the snapshot (distinct
    panes, attention wins over working, a zero count is hidden); a click opens
    the notification list's dropdown with the agents (task, agent, space,
    "approval" or "reply"), Enter or a click jumps to the pane (tab as a
    fallback), no request is sent. They sit right of the sort buttons and
    only when there is room: at the default 32 columns with `manual name ↑
    prio ↓` only the asking one fits (both from about 36). Not done: the
    time in the state (the snapshot has no timestamp), dimming the focused
    pane's row, the multi-machine sidebar header, narrowing the sort buttons
    to make room.

- [ ] Rebuild the fork's history as functional commits (user, 2026-10-01:
  "add a functional split of the git history to the todo"). Today `master` is
  19 chronological block commits plus fixes on `upstream/master`; the 315
  original commits are in the tag `archive/pre-sync-20261001`. Consulted
  DeepSeek, Opus and GPT; the plan they agree on:
  - Freeze: tag the compacted tip `final`, record `base` (the upstream commit
    under the blocks, `git merge-base HEAD upstream/master` at that time),
    rebuild on a disposable branch from `base`, never on newer upstream.
  - Inventory: `git diff --name-status base final`; labels from the archive
    subjects (`git log --format=%s archive/...`), co-change clusters from
    `git log --name-only`; an LLM proposes a feature for each hunk of the hot
    files (`state.rs`, `mouse.rs`, `sidebar.rs`, 35-44 commits each), a human
    reviews every assignment; order the features topologically (a symbol's
    definer comes first; merge cycles).
  - Manifest `split/features.toml`: feature, deps, exclusive path globs, hunk
    markers (function or struct regexes), archive SHAs. A script
    (`split/split.py`) parses `git diff -U0 base final`, assigns every hunk,
    emits `NN-feature.patch`, and fails on an unassigned or doubly assigned
    hunk or a failing `git apply --check`.
  - Mechanics: `git switch -c functional base`; per feature `git checkout
    final -- <exclusive files>` (handle deletions), `git apply --cached` for
    the shared hunks (or `git add -p` with final content in the worktree),
    commit; hand-fix hunks that compile only with a later feature. The other
    way, `git rebase -i` over the original 315, works only if most commits are
    feature-pure: trial once with `rerere` on and drop it when more than ~10%
    of the picks conflict in the hot files (expected here).
  - Verify: `git diff --exit-code final HEAD` empty; `git rebase --exec
    'cargo check --all-targets' base`; `just windows-lint` per commit if
    affordable; `just check` at the tip; test each snapshot in a clean
    worktree (unstaged final content hides missing dependencies).
  - Effort and stopping rule: estimates range from 1-2 days (Opus) to 40-80
    hours (DeepSeek) to 3-10 days (GPT). Timebox two days, reassess after two
    hard features, stop when more than ~15 hunks need rewriting to compile or
    the effort exceeds the value; fallback: extract a single feature on demand
    when it goes upstream (branch from `upstream/master`, take its files and
    hunks from `final`, `git add -p`).
  - Afterwards: one commit per feature, fixes as `git commit --fixup=<sha>`
    and `rebase -i --autosquash` before a sync, weekly `git rebase
    upstream/master` with `rerere` on, tag before every rewrite, keep the
    manifest as the feature index.
  - Done: nothing yet (recommended: not before a feature goes upstream).

- [ ] A reconnect can deliver half of a typed line to a remote pane (found
  2026-10-06 through the flaky test above). Dropping keys while the endpoint
  is offline is intended; cutting one stdin read in two is not: the
  offline decision should be made once per read. Coalescing the chunks in
  the reader is not enough on its own, because the `StdinInput` handler
  checks each chunk for the image-paste key and file drops. Upstream code:
  consider reporting it upstream instead of diverging.
  - Fix (`e1402580`, 2026-10-06; 120 of 120 under load on the original
    test, which failed 2 of 40 before; installed): the reader
    sends the events of one read as one `StdinBatch`, and the loop drains
    it through a queue before `select!`, so each key still goes through its
    own handler in order (sol, round `20261006-014726-fde7`: handling the
    special keys first and the rest in one call would reorder input).
  - Not fixed, by decision (user, 2026-10-06, after sol and MiMo, round
    `20261006-020529-3310`): a line split by the kernel across two reads,
    and frames queued to a dying SSH bridge, can still arrive cut. An acked
    sequence protocol cannot give exactly-once keystrokes (a lost ack
    resends `y` as `yy`, seq and payload are two messages, drops must
    advance the seq), and queueing input while offline lands stale keys at
    a new prompt; 800-1500 lines for little. Documented instead (round
    `20261006-020858-b22a`): a "Keys typed as a connection drops" item in
    `connecting-machines.mdx` and a comment at the drop site in
    `finish_client_shell_input`.
  - Upstream report: not filed. Upstream `3d9d2b18` has the same code. In
    300 stress runs there, 3 failed at "remote reconnect N must restore
    visible input", but upstream's assertion prints no screen and waits only
    8 s, so a cut line is not shown; the 120 runs with a screen dump hit no
    such failure (one unrelated `local-online` timeout). The upstream rules
    allow an issue only for a bug reproduced on the reported version, so
    file it only after capturing the `>` screen there.

- [ ] Durable-execution and agent-orchestration projects to test or study
  later (user, 2026-10-10: "add the found projects to the TODO for later").
  Found by a web survey (HN, GitHub; repos verified with gh on
  2026-10-10) and deferred from the first hands-on test set because they
  share a recovery contract with a chosen project (round
  20261010-121933-62f8). Each joins a test only for a specific unanswered
  question:
  - replay of a history/journal: Restate (journal, durable promises, Rust
    core), Obelisk (obeli-sk/obelisk, Rust + SQLite + WASM, AGPL-3.0),
    Inngest (step memoization, event-resumed waits), microsoft/duroxide
    (MIT, Rust + SQLite, event-sourced replay), Resonate (durable
    promises, Rust server);
  - checkpoints without replay: LangGraph (SQLite checkpointer,
    `interrupt()`), Trigora (trigora-dev/trigora, continuation
    checkpointing, its own recovery benchmark);
  - transactional database steps: microsoft/pg_durable (step graphs inside
    Postgres), OpenWorkflow (openworkflowdev/openworkflow, SQLite or
    Postgres, workers coordinate through the database), durare (Rust,
    DBOS-compatible library);
  - agent-session tools close to herdr: mjolnir (BrokkAi/mjolnir,
    GPL-3.0, Rust, journals and checkpoints for Claude Code and Codex),
    coven (OpenCoven/coven, Rust, PTY sessions in SQLite), acpus
    (kelvinschen/acpus, `runs resume` and `runs retry` for ACP agents),
    Smithers (smithersai/smithers, SQLite agent workflows);
  - lower priority: julep (agents on Temporal), Rivet (actors with
    SQLite), everruns, kassette, Duron, iopsystems/durable, underway,
    Dapr Agents, Vercel Workflow, Trigger.dev, LittleHorse, Hatchet,
    Absurd, vibe-kanban and mini-swe-agent (dropped from the first test:
    no durable resume).
