# TODO

Open work only. A finished item leaves this file: its durable
decisions go to `DECISIONS.md`, the rest stays in the commit messages.
Parked ideas live in `TODO-deferred.md`. An open item keeps only its
title with the user's words, what is still open, and the decisions that
constrain it.

## Next, in order

Agents may do these from the top without asking when the user tells them to
work through the TODO (the user's global agent rules, "Working through TODO.md").

- [ ] A coordinator stops between items without being asked (user, [t-xe6hpo4z]
  2026-10-07: "why aren't you delegating anything? ... explain why; we want
  to improve the process, not have you start working now and forget"). Twice
  the herdr coordinator answered the user's mid-turn questions, wrote a
  status report and ended its turn with "I will delegate the next item when
  you say continue", although the items were approved. Causes: (1) the rule
  says questions are answered, not queued, but not that the coordinator
  goes on afterwards, and a report reads as a natural end of turn; (2) a
  Claude coordinator only acts inside a turn: once the turn ends, nothing
  starts the next item or reviews a finished worker until the user writes.
  Candidates: a rule line ("answering or queuing never ends the turn; end
  it only when 'Next, in order' is empty or every item waits on the user");
  waiting for a worker with a background wait that wakes the session when
  it ends (done since 2026-10-07 in this session); a Stop hook that blocks a
  coordinator-role tab's stop once while "Next, in order" has items and no
  worker runs; herdr flagging an idle `♛` tab with open items.
  Consult round `20261007-024331-bde5` (sol, MiMo; the user: "how would I
  know what works? ask the models, do tests"). Both: the rule line is the
  direct fix but only probabilistic, and must say what legitimately ends a
  turn (cancellation, a decision whose answer changes the diff, failed
  checks, missing credentials), with the blocking question written into
  "Needs a decision" so a silent stop is checkable; "a mid-turn question
  does not revoke approval" (sol). The background wait fixes a different
  failure (nothing reviews a finished worker), not this one: here no worker
  ran (both). The Stop hook is the only deterministic backstop, but one nudge
  only (`stop_hook_active`) and it costs a turn per false positive: ship it
  only with its predicate unit-tested on synthetic hook input to zero false
  positives (both). An idle-`♛` flag is observability, not prevention.
  Bigger alternatives: a durable item state machine outside the model (sol)
  or a shell driver that runs the queue and calls Claude per item to review
  (MiMo).
  Test plan, cheapest first: (1) an offline audit of coordinator
  transcripts that labels each turn end (finished, blocked on the user,
  abandoned with items left and no worker), the baseline; (2) the rule
  wording, then the audit on later transcripts; (3) the Stop hook with its
  predicate tested on the audit's cases; (4) headless runs on a throwaway
  repo with three trivial items and a stub worker, with "what is happening?"
  injected mid-turn, many runs (costs Claude usage; ask first); (5) a field
  metric: abandoned turn ends per 100 coordinator turn ends, before and after.
  Step 1 done 2026-10-07 (`78167f03`, `scripts/coordinator_turn_audit.py`):
  baseline over this machine's transcripts, 3 coordinator sessions, 36 turn
  ends: asked 1, waiting 32, abandoned 2 (both in the herdr coordinator,
  the known case), other 1, so 5.6 abandoned per 100. Small sample; rerun
  after the rule change.
  Step 2 done 2026-10-07 (dotfiles `45c6466`, approved by the user in the
  worker's pane after the auto-mode classifier blocked a worker): a
  mid-work question does not withdraw the approval; a turn ends only when
  every item waits on the user, Next is empty, the user says stop, or a
  background wait on a running worker will wake the session.
  Step 3 done 2026-10-07 (`e238621c`, integration reinstalled with the
  user's consent): the Stop hook blocks a `coordinator`-role tab's stop once
  when its final text waits for a go-ahead (the audit's `ABANDON`
  expression, parity-tested) and no background task of the session runs;
  fails open. Left: step 4 (headless runs, ask first) and step 5 (rerun the
  audit after a few days of coordinator work and compare with 5.6 per 100).

Three gaps found in https://spznrf.dev/blog/the-fellowship-of-the-pane
(2026-10-03: a user runs five Pi agents in five visible Herdr panes, a main
session that writes all code and delegates to documenter, reviewer, qa and
ops-recon through `herdr agent prompt`, one of them on a remote host).
Consulted sol and MiMo twice (2026-10-06, rounds `20261006-023800-a481`
and `20261006-030215-b8ca`); both put the first two at the top.

  Decided by the user 2026-10-07: run step 4 with 5 headless runs (after
  asking what it tests: whether a coordinator asked "what is happening?"
  mid-work answers and goes on, or still ends its turn waiting for a
  go-ahead, with the new rule line and the Stop hook in place).
  Step 4 done 2026-10-07 (`scripts/coordinator_trial.sh`, report
  `docs/coordinator-trial-2026-10-07.md`): 5/5 runs finished all three
  items, 21 turn ends, 0 abandoned; the question always landed mid-turn and
  the coordinator answered and went on; the Stop hook never had to block.
  Not shown: a question while the coordinator idles on a background wait,
  and the Stop hook's nudge itself. Left: step 5 (the audit over real
  coordinator transcripts after a few days, compare with 5.6 per 100); a
  variant of the trial that asks while the coordinator idles.
- [ ] Event-driven worker waits, no timers (user, 2026-10-07: "a deadline of [t-osip4upq]
  about 30 minutes? too much? why any asynchronous workaround at all? make
  a TODO with the models to fix this and do it next"). Supersedes the
  polling/deadline design below (worker `w-worker-end`'s commit is not taken).
  Round `20261007-175803-0ec5` (sol, MiMo, DeepSeek), agreeing: a turn end
  is an event, a task end is a verdict. The coordinator starts the worker
  with `agent.prompt_turn` (or prompt + request id) and blocks on that
  request's end: Stop, StopFailure, interrupt, the agent process's exit
  (track the agent's process, not the pane's shell), or a server restart
  reported as such; then reads the transcript once and classifies: done
  (WORKER-DONE with its sha on the branch), blocked, awaiting input (a
  question: escalate), failed, crashed, tracking lost. No timer decides
  anything: no deadline, no idle debounce, no resend after N seconds; at
  most a human-facing "overdue" notice for a real agreed deadline.
  herdr needs: `agent.wait_turn <request id> [since <cursor>]` with an
  atomic check-and-subscribe, a terminal reason enum, process-exit events,
  and request ids that survive a server restart (or an explicit error);
  check that Esc-interrupt ends the turn (MiMo). Startup prompts need an
  acknowledged, idempotent delivery instead of "resend after 15 s".
  Done 2026-10-07 by a worker (`feat: wait for a prompt's turn by event`):
  `agent.wait_turn` (finished, failed with StopFailure's error, interrupted,
  exited, unknown_request), `agent.prompt_tracked`, `herdr agent
  wait-turn`, `herdr-job wait-agent <pane> --request <id>` reading the
  transcript once; screen polling and the deadline are gone. Gap: Claude
  sends no Stop on Esc, so an interrupted turn shows only when the next
  turn starts; a coordinator waiting on a worker the user interrupted waits
  until then. Left: acknowledged startup prompt delivery.
  Bug at first use (2026-10-07): the Keychain worker finished normally
  (WORKER-DONE at 16:43 UTC) but `wait_turn` returned `interrupted`. The
  inference "another turn started before this one reported its end" seems
  to fire when a background task's notification starts a turn inside the
  same prompt's work; check the hook order for task notifications and
  derive `interrupted` only from an explicit signal.
  Acknowledged startup prompt delivery done 2026-10-08 (`feat: agent
  prompts confirm that the agent accepted them`, installed). Left: the
  false `interrupted` above.
  Fixed 2026-10-08 by headless worker `w21` (`fix: wait_turn reports
  interrupted only on an explicit signal`): `interrupted` only from Esc or
  Ctrl-C sent through `agent.send_keys` or the agent's own interrupted
  report; a turn started by a background-task notification is a
  continuation (the Claude hook marks it); ambiguous order gives `unknown`.
  Left, decided by the coordinator: confirm live that Claude sends
  `UserPromptSubmit` for a background-task notification mid-turn; the
  user's own Esc in the pane now gives `unknown` (better than a false
  `interrupted`); an explicit signal for it could come from the
  transcript's `[Request interrupted by user` marker, verified live first.

- [ ] Why the "added delay is a bug signal" rule did not hold (user, [t-v65dxbip]
  2026-10-07: "is that rule somewhere in CLAUDE.md? where? why didn't you
  apply it? ask the models"). It is in `~/.claude/CLAUDE.md` ("Added delay
  is a bug signal") and Rule 10 of `~/personal_projects/agents.md/AGENTS.md`.
  The coordinator wrote a 120 s "debounce" ("a debounce, not a delay"),
  delegated a 30-minute deadline and a 2-minute idle debounce, polled a
  transcript every 5 s and resent prompts after 15 s. Round
  `20261007-180041-55f7` (sol, MiMo, DeepSeek), agreeing: it rationalised (its
  own scripts felt like "tooling" outside the rule; relabelling; the
  exception list is the escape hatch; delegation dilutes; consults suggest
  timers; queue pressure). Fixes: every wait names a positive observable
  condition and its producer (a required `awaits:` field), negative
  conditions ("idle for N", "no marker for N") banned; a hook that flags
  time constants in the agent's own commands, scripts and worker tasks,
  outside the agent's edit scope; worker tasks state "terminates when";
  the rule says it covers coordinator scripts and delegated tasks and that
  a label does not qualify a delay. Verdicts on today's: 120 s and 2 min
  debounces not allowed; 30 min deadline not allowed (not external); 15 s
  blind resend not allowed (needs ack + idempotency); 5 s transcript poll
  borderline (external polling of an authoritative file, but the event
  exists). Rule text is the user's file: a worker, with his approval.
  Rule text done 2026-10-07 (dotfiles `c348422`, approved by the user):
  the rule covers own commands, scripts and delegated tasks; a label does
  not qualify a delay; every wait names its positive event and producer; no
  negative conditions; retries need an acknowledgement and an idempotent
  action. Left: the hook that flags time constants.
  Hook done 2026-10-08: prepared by headless worker `w22` as files (no
  edits under ~), shown to the user with its risks, installed after his
  yes (dotfiles `be4a352 claude: hook flags time constants without a delay
  reason`): a PreToolUse hook on Bash/Write/Edit/MultiEdit denies waits and
  give-up times (`sleep N`, `--timeout`, `Duration::from_secs(N)`,
  `Instant::now() +`, `setTimeout(`, deadline/debounce wording in code and
  agent instruction files) without a `delay: <reason>` marker; 28 tests.
  Known gap: headless workers start with `disableAllHooks`, so it does not
  reach them.
  First block in use (2026-10-08): it denied the coordinator's `sleep 0`
  (a no-op, a habit, not a wait); harmless but a false positive: `sleep 0`
  and `timeout 0` could pass, or the denial could say to drop the no-op.

- [ ] A runaway wait loop exhausted the Mac's PTYs (user, 2026-10-07: "about [t-ul4yd4ll]
  400 tabs, opened by the bussiness-ideas coordinator when it looped on
  retrying its wait for a worker; it closed them and removed the loop. Ask
  the models; add it to TODO as next"). `openpty: Device not configured`
  (`kern.tty.ptmx_max` 511, ~527 `/dev/ttys*` in use): herdr could not open
  tabs anywhere. Each retry went through `herdr-job run`, which opens a tab
  per job, and kept failed job tabs open. Consult the default set on
  guards: a cap on running/failed job tabs per owner pane, a rate limit on
  `herdr-job run` and tab creation, PTY headroom checks, retries that reuse
  one job instead of starting a new one, and how herdr reports PTY
  exhaustion.
  Round `20261007-131635-9d1d` (sol, MiMo), agreeing:
  - A tab per wait attempt is the bug: one wait job that reconnects inside
    (backoff 1 s to 30 s, give up after ~15 min of transport failures),
    distinguishing a failed worker from a lost connection.
  - herdr-job caps checked before any PTY is allocated, atomically, counting
    running and failed jobs and attributing nested jobs to the original
    owner: per owner pane sol 16 / MiMo 4 running, failed tabs kept 8 / 4
    (the oldest closed, its log kept, never the focused one); global 64 / 32;
    launches ~3-6 per minute per owner; refuse a job name that failed 3
    times in 10 minutes. MiMo: reuse a running/failed job of the same name
    unless `--new-attempt`, and keep the counters on disk (a restart is what
    triggered the retries).
  - herdr core: refuse a spawn below ~64 free PTYs with a clear error naming
    the counts (MiMo: `EHERDR_PTY_EXHAUSTED`), warn at 80%, a tab cap per
    workspace (64) and global (128, sol).
  - Agent rule: one wait command per wait, never a retry loop around
    `herdr-job run`; on wait infrastructure errors check status once and
    report; every loop bounded in iterations and time.
  - Test: set the owner cap to 3, launch three failing jobs, the fourth is
    refused before a PTY is opened, also under concurrent launches.
  Part 1 done 2026-10-07 (`02796a77`, live at once: herdr-job runs from
  this checkout): caps before the job's tab exists under one lock (16
  running+failed per owner pane, 64 globally, 8 failed tabs kept per owner,
  nested jobs count for the original owner), a job name that failed 3 times
  in 10 minutes is refused unless `--force` (counters on disk), and
  `herdr-job wait-agent <pane> [--until …] [--worker-line]` waits in one job,
  retrying transport errors (`server_not_running`, `server_unavailable`)
  from 1 s to 30 s for up to 15 minutes. Left: herdr core's PTY headroom
  check and clear error; the agent rule in the global instructions (the
  rule text is the user's file: a worker, with his approval).
  Bug found at first use: `wait-agent --worker-line` reads `agent read
  --source recent --lines 200`, which herdr refuses while the agent works
  (`agent_not_idle`: alternate-screen history needs scrolling while idle),
  so the wait exits 2 at once; read `--source detection` (the visible
  screen) instead, and treat `agent_not_idle` as retry, with a test.
  Fixed 2026-10-07 by a worker (`fix(job): wait-agent reads the visible
  screen`); the coordinator waits with `herdr-job wait-agent <pane>
  --worker-line` from now on.
  Part 2 brought in 2026-10-07 (`5de0c204`, not pushed or installed yet):
  `pty_exhausted` below 64 free PTYs and `server.pty_usage`. `just check`
  fails on `windows-lint`: `count_macos_pty_slave_names` and a method
  `invalidate` are dead code on Windows; a worker gates them with cfg
  (AGENTS.md: platform code compile-gated) before push and install.

- [ ] Ideas from Omarchy's "agent account" (user, 2026-10-07: "add all of [t-gtzcciut]
  it to the TODO as 4, ask the models"; omacom/omarchy PRs 13770 and 13992:
  several Claude/Codex/Grok logins, new sessions move to the account with
  the most headroom near 95%, limit meters with reset times). Round
  `20261007-132403-eedb` (sol, MiMo): take the provider choice by headroom,
  skip the meter panel and launch tiles (footer and `usage.read` exist),
  never migrate a running session (a `Limited` agent gets the explicit
  handoff), no credential copying. Round `20261007-134745-f39d` on this
  plan; slices, each shippable alone:
  1. Bug, verified in Claude Code 2.1.292's binary: its macOS Keychain item
     is `Claude Code-credentials` only without `CLAUDE_CONFIG_DIR`; with it,
     the name gets `-` + the first 8 hex chars of sha256 of the
     NFC-normalized dir (`CLAUDE_SECURESTORAGE_CONFIG_DIR` overrides the dir;
     set but empty means the default name). `src/usage/claude.rs` honours
     the variable for `.credentials.json` but always reads the default
     Keychain item: another account's limits. Tests: unset, empty and set
     variables, override precedence, NFC, a golden name taken from a real
     Keychain item (MiMo: a test of our own reimplementation proves
     nothing); never fall back to the default item when the variable is set.
     Done 2026-10-07 (`a4c5885b`, installed): golden names captured from
     Claude 2.1.292 through a `security` shim (no per-dir item exists on
     this Mac yet), NFC through CoreFoundation, no fallback when set.
  2. `usage.read` gains optional facts per provider: which account it read
     (MiMo: wrong-account data must be visible), observed time, stale and
     failed-poll state (stale is unknown, never 0% or 100%). Define per
     window, not one "tightest %": 5h, weekly and Codex's windows are not
     comparable, and the tightest window's reset is not when the provider
     frees up (sol).
  3. The coordinator picks Claude or pi/sol for a new worker from
     `usage.read` (both models: a coordinator rule or script first, not a
     server capability that freezes an unstable policy in the API).
     Decided (rounds above; the coordinator decided the values, the user
     can change them): percentages are "used"; a new worker goes to pi/sol
     when any Claude window is at 90% or more, and back to Claude only when
     every Claude window is below 80% or has reset (MiMo proposed 90/50;
     80 because weekly windows fall only at their reset); the hysteresis
     state lives in the coordinator's session and starts from Claude after
     a restart; stale or failed usage data counts as unknown and keeps the
     current choice, with the data age in the worker task; when both are at
     90% or more, the item waits for the earlier reset (the user's rule:
     "When no worker can run (limits), the item waits"); an explicit kind
     from the user always wins. pi and Codex keep separate
     logins (`~/.pi/agent/auth.json`, `~/.codex/auth.json`); both are the
     same account today, so Codex's limits stand for pi's.
  4. Named accounts (`--account <id>` as an execution profile: config dir,
     settings, hooks) go to "Needs a decision" only when the user has a
     second account per provider, together with the subscription terms on
     several accounts (both models: all of them can be suspended at once).

- [ ] Compare the T3 Code approach: agents through their SDKs instead of a PTY [t-fjbflpho]
  per agent (user, 2026-10-07: "we ran out of pseudo-terminals today;
  analyse whether T3 Code's approach with an SDK is better here; ask the
  models"; third in the queue). Today each agent, shell and herdr-job runs
  in its own PTY; ~400 runaway job tabs plus normal use exhausted macOS's
  511. Consult the default set: what an SDK-driven agent (Claude Agent SDK,
  Codex app-server) would gain and lose here (the user's terminal UIs,
  `/remote-control`, hooks, resume, screen detection), a hybrid (SDK for
  workers only), and the PTY budget in numbers.
  Measured 2026-10-07 13:23: 108 of 511 PTYs in use with 65 agents in 29
  spaces. Round `20261007-132332-a080` (sol, MiMo), agreeing: the incident
  was missing admission control, not a reason to leave PTYs; 511 is ample
  with the guards (about 85-250 PTYs for this use); an SDK runaway would be
  worse (no visible tabs). SDK workers gain structured turns, reliable
  prompt delivery and no screen flicker, and lose the agent's own TUI,
  slash commands and possibly `/remote-control`. Unverified and decisive:
  whether the Claude Agent SDK runs on a subscription login or needs an API
  key (sol: Anthropic restricts third-party use of subscription OAuth; MiMo:
  it works); check the current docs first. Recommendation: finish the
  guards; measure how often a worker's TUI is viewed and how often typed
  prompts fail; only then prototype one SDK worker backend behind an
  `AgentDriver`-like seam.
  Again 2026-10-07 (user: "something always needs fixing with the
  coordinator and the workers; I think that is why T3 Code uses the agents'
  SDKs; analyse with the models, read the source"). Facts: the Agent SDK
  runs the Claude Code binary; its docs say "Unless previously approved,
  Anthropic does not allow third party developers to offer claude.ai login
  or rate limits for their products, including agents built on the Claude
  Agent SDK" (code.claude.com/docs/en/agent-sdk/overview), and point other
  languages to `claude -p --output-format json`. T3 Code (MIT, commit
  611132c1) drives Claude through the SDK with `pathToClaudeCodeExecutable`
  on the user's local login (it labels `claudemax20xsubscription`): turn end
  = the `result` message, interrupted from `terminal_reason`
  (`aborted_tools`/`aborted_streaming`), limits from `api_error_status`
  429/529, questions and approvals through `canUseTool`, `query.interrupt()`,
  readiness from `system/init`; its Claude adapter is 8016 lines (held
  frames until the prompt echo, a deadlock when interrupting the raw
  generator), Pi and Cursor 3000+ each.
  Rounds `20261007-183147-d819` and `20261007-183420-2140` (sol, MiMo, DeepSeek):
  an event stream instead of a TUI removes today's failures 3 (state
  flicker), 4 (stale screen), 5 (Esc without Stop: interrupt is a control
  request with a reason) and the PTY per worker (6), and makes 1 (lost first
  prompt: gate on `system/init`) and 2 (trust dialog) startup errors rather
  than silent hangs; 7 (permission classifier) and 8 (tab ids) stay ours.
  New costs: no native TUI for a worker (watch through a log view, take over
  with `claude --resume <session>` in a tab, never two writers), questions
  and approvals need our own answering path, pipe backpressure and a
  journal of events, the user's CLI version churn. All three: hybrid,
  headless workers, TUI coordinators and agents the user talks to. Split on
  the transport: SDK sidecar (sol, DeepSeek: it absorbs the undocumented
  `control_request` protocol for `can_use_tool`/`interrupt`) vs plain
  `claude -p --input-format stream-json --output-format stream-json` now
  (MiMo: the only SDK-specific win is `canUseTool`). Policy risk either way:
  the auth note targets SDK products; the user's own CLI in print mode is
  the stronger case; verify before building.
  The user chose plain `claude -p` stream-json for an experiment (not the
  SDK, not staying with TUI workers).
  Experiment done 2026-10-07 (`scripts/headless_worker_trial.py`, report
  `docs/headless-worker-trial-2026-10-07.md`, observed on Claude Code
  2.1.292 with the user's login): failures 1-5 gone (a prompt sent before
  `system/init` is buffered and echoed with `--replay-user-messages`; no
  trust dialog in `-p`; the turn ends at `result`; interrupt is a
  `control_request` acknowledged at once, ending in `aborted_tools`);
  `--permission-prompt-tool stdio` turns approvals and `AskUserQuestion`
  into `can_use_tool` control requests the host answers; the CLI needs no
  PTY; a `rate_limit_event` with 5-hour and 7-day use comes every turn;
  workers inherit the user's global hooks and CLAUDE.md; a SIGKILLed
  worker leaves tool processes; `--resume` in a tab asks for trust once.
  Adoption, round `20261007-185247-aeba` (sol, MiMo, DeepSeek), decided from
  the agreement: a failure-focused trial first; tab-less worker rows with
  state from the event journal and a log view; takeover = interrupt, wait
  for `aborted_tools`, end the process group, confirm exit, then `claude
  --resume` in a tab; no Stop-hook injection for workers; `rate_limit_event`
  pauses delegation at high use.
  Decided by the user 2026-10-07 (menu): approvals allowed by default inside
  the worker's worktree, every decision logged (realpath checks for file
  tools); AskUserQuestion to the user's `?` list; the supervisor in herdr's
  server (Rust); workers load the global CLAUDE.md plus a worker contract,
  global hooks off; Bash: a strict list per repository (one command, no
  chaining, pipes or redirection; git without push/config/-c, just check,
  cargo test) runs without asking, every other command goes to the user.
  Trial 2 done 2026-10-07 (report section "Trial 2"): `disableAllHooks`
  keeps CLAUDE.md and the login; interrupt and SIGTERM leave no orphans,
  SIGKILL does (tools start their own sessions: record and end them); a
  20-request approval storm stays in order; a second `--resume` of a
  running session is not refused and forks the transcript; three workers at
  once stay apart. Decided after round `20261007-191044-2920` (all three
  agreeing): never SIGKILL on a timer (report "still alive" after SIGTERM,
  the user force-stops); the user accepts the trust dialog on takeover
  (herdr neither writes `~/.claude.json` nor answers the dialog); a running
  worker's session shows "running: resuming it forks the transcript", and
  herdr's own takeover ends the worker first.
  Next: the server-side supervisor in slices, one worker each:
  1. herdr's server starts, journals and ends headless Claude workers
     (`worker.start`, state from events, `worker.wait/interrupt/stop/kill`,
     a CLI), approvals limited to file tools inside the worktree for now.
     Done 2026-10-07 (`feat: a server-side supervisor for headless Claude
     workers (slice 1)`): `worker.*` API and `herdr worker` CLI, a journal
     per worker, state from events, `worker.wait` woken by state changes,
     realpath file-tool policy, stop/kill with recorded tool sessions,
     `lost` after a server restart. Review finding for slice 2:
     `worker.stop` reports "still alive" from a check right after SIGTERM,
     a race; report the group's exit from the exit event instead.
  2. The approval policy (realpath file tools, the strict Bash list) and
     questions to the user's `?` list, answered as `control_response`.
     Done 2026-10-07 (`feat: approvals and questions for headless workers
     (slice 2)`): the policy with `.herdr/worker-allow.toml`, questions in
     `worker.status` and the `?` list, `herdr worker answer`, `worker.stop`
     returns at once and reports the exit from the event.
  3. The sidebar: a tab-less worker row, a log view, takeover; the
     coordinator's `/todo` flow starts workers through `worker.start`.
     Done 2026-10-07 (`feat: headless workers in the sidebar, their log and
     takeover (slice 3)`): worker lines under their space (task name, state,
     question), a following log popup, right-click takeover (interrupt,
     result, stop, exit by events, then `claude --resume` in a tab; refused
     while a question is pending), `herdr worker start --name --workspace
     --prompt`, a paragraph in AGENTS.md. Left: the coordinator's `/todo`
     skill and rule switch to `herdr worker start` (rule text: a worker with
     the user's approval), and a first real coordinator run with headless
     workers.
     Decided by the user 2026-10-07: switch after one trial run: the
     coordinator hands one real item to `herdr worker start`; if it goes
     through, a worker changes the rule and skill (approved in its pane).
     Trial run 2026-10-07: worker `w1` ("header arrows", the `‹ ›` item).
     Its first question was a Bash heredoc writing a code draft to `/tmp`
     (asked because of `&&`); the user allowed it. For the rule switch: the
     worker contract should tell workers to edit with Edit/Write inside the
     worktree and read with Read/Grep, so such drafts do not need the user.
     Result: `w1` finished the item (`fix: the header's back/forward
     arrows keep their place`, checked and installed); it asked 9
     questions, all harmless, each waiting until something woke the
     coordinator, and tried a `Co-Authored-By` trailer once. Verdict: the
     transport works; switching the rule waits for the reliability plan's
     steps 1-2 (fewer questions, the wake on questions) and the commit
     rule item.

- [ ] Atomicity fixes from the review (`docs/atomicity-review-2026-10-07.md`, [t-5dqymmsm]
  user 2026-10-07: "it must be like a database transaction"). 15 findings
  verified by the worker, the critical one also by the coordinator. Until
  fix 3 lands: no install while a headless worker runs (finding 1: the new
  server marks every running worker `lost` at start and worker pipes are
  not handed over, so an install ends them all). In order, one worker
  each, the doc's "Fixes, in order" has the details:
  1. Answers name their question: `request_id` required (or refused when
     several are pending), the `?` list shows it; a gone id is refused
     with what happened to it (finding 2: the oldest question was
     answered, possibly one the user never saw).
     Done 2026-10-07 (`fix: answers to a worker's question must name the
     question`, `fix: regenerate the api schema for worker answers`,
     checked and installed; by headless worker `w2`, whose hand-edited
     schema the check caught).
  2. Takeover claims recorded in every state and released on failure; no
     prompt or answer during a takeover (findings 3, 8: two clicks on an
     exited worker open two tabs on one session).
     Done 2026-10-08 by headless worker `w4` (`fix: worker takeovers are
     claimed once and released on failure`, no questions). Open point
     decided after round `20261008-023754-9eb8` (sol, MiMo, DeepSeek): after a
     restart with `takeover_unfinished`, "no pane shows the session" does
     not prove no tab was opened (the session id appears only after Claude
     starts). Follow-up: the takeover tab is created carrying a unique
     takeover id (env and title) recorded in the claim; recovery looks for
     that id and for a process running `claude --resume <session>`; if
     found, herdr adopts that pane (journals `takeover_tab_opened`);
     otherwise a retry needs an explicit `--force` that states the
     duplicate risk; retries serialized by the claim.
  3. A live handoff does not kill or mislabel workers: first refuse or
     postpone it while a worker runs and mark `lost` only when no server
     owns the journal (a lock per journal); a later `exited` replaces
     `lost` on replay. Then a design choice for the user (hand worker pipes
     over like PTY fds, or a small per-worker broker that outlives the
     server).
     First step done 2026-10-08 by headless worker `w5` (`fix: a live
     handoff no longer kills or mislabels headless workers`, `fix: stop
     idle workers before a handoff by their exit events, without a
     deadline`): the server refuses a handoff while any worker process
     lives (`--force` sends SIGTERM and goes on); `herdr_live.sh install`
     stops idle workers itself (`worker stop`, `worker wait --exit`) and
     refuses when one is in a turn; a lock per journal so a new server
     marks `lost` only unowned journals; a later `exited` replaces `lost`;
     a worker that finished its turn shows that state with "ended by a
     server restart". The coordinator sent back a 10 s server-side
     deadline (a timer deciding the outcome, freezing the main loop).
     Decided by the coordinator: `herdr update --handoff` keeps the
     refusal with its instruction (no own stopping); the script's
     `python3` use is fine. The first install of this change is not yet
     protected (the old binary asks for the handoff).
  4. The coordinator's stopgap wait script: wake on `question`, `lost`,
     `exited`; open and seek the journal before reading the status; take
     the path from `worker status` (finding 4: it can miss a question and
     hangs after a handoff).
     Done 2026-10-07 by the coordinator itself in its scratchpad (not a
     repository file; the rule says a worker should have done it): the
     script now opens the journal, waits on kqueue for writes, rereads the
     status on each, and takes the path from `worker status`.
  5. `worker kill` checks process identity (start time) before killing a
     recorded tool session (finding 5: a reused session id can be a herdr
     pane's shell).
     Done 2026-10-08 by headless worker `w6` (`fix: worker kill checks
     process identity before signalling`, no questions): sessions recorded
     with the leader's start token; only matching leaders and members not
     older than them are signalled; old entries without a token are
     skipped and reported; a lost or exited worker needs `--force`.
  6. Install the build that was checked: build and install under one
     clean-tree lock, and `herdr_live.sh install` refuses a binary whose
     build label is not the expected one (finding 11).
     Done 2026-10-08 by headless worker `w7` (`fix: build and install the
     checked tree under one lock`, no questions): `clean-tree --then`,
     `just clean-install <paths>`, `herdr_live.sh install --expect-build`,
     one snapshot of HEAD and the paths; first used for its own install.
  7. herdr-job's locks follow the command (`pass_fds`), no `lost` verdict
     after 60 s, `run --key` (findings 10, 13).
     Done 2026-10-08 by headless worker `w8` (`fix: herdr-job locks
     follow the command and jobs dedupe by key`, no questions). Decided by
     the coordinator under the event rule: a daemon that inherits the lock
     keeps the slot after its job ends (documented; our builds leave none;
     `herdr-job slots` names the holder); a job whose `pane run` failed
     stays `pending` until its tab is closed (not `lost` by a timer). Left:
     the orphan tab when the process dies between `tab create` and
     `write_meta`.
  8. The `/todo` claim under an exclusive lock; cherry-pick only after
     `git merge-tree` shows it applies (finding 14).
     Deferred by the coordinator 2026-10-08 to after the reliability
     steps: it changes the `/todo` skill in the user's dotfiles (outside
     this repository; a headless worker cannot write there, and edits under
     `~/.claude` need the user's approval in a worker's pane); lowest
     severity.
  Folded into the reliability plan: a sequence on every journal record and
  prompt/interrupt bound to a turn (step 2, findings 7, 12); answers
  journaled as intent, sent, settled, idempotent by request id, and
  `worker.start` with a client key (step 3, findings 6, 7); journal write
  failures visible as `degraded`, lossy replay, numbers reserved (finding
  9); the doc's extra fault-injection cases (step 7).
  Found 2026-10-08: a headless worker that finished its turn keeps its
  process waiting for the next prompt, so the folder slot stays busy until
  the coordinator runs `herdr worker stop` and `herdr worker wait --exit`;
  the refusal should say so, or a finished worker in a slot should be
  stopped once the coordinator has taken its commit.
  Found 2026-10-08: the worker folder's `target/` grew to 14 GB in a day
  (each `cargo test` leaves a hashed binary) and blocked a check at
  `just guard`; `just sweep` acts only above 25 GiB per tree, so it freed
  nothing; the user approved `cargo clean` there. The folder slot should
  run the sweep with a smaller limit before each worker (or `just guard`
  should count the slot), so it cannot fill the disk.
  Fixed 2026-10-08 in the same commit: `target_sweep.py slot` keeps the
  slot's `target/` under 10 GiB before each start, removes it when the
  disk stays under the guard threshold, else refuses the start.

- [ ] Headless workers ask the user almost never (user, 2026-10-07, after [t-u4znflk7]
  two approval questions from worker `w1` for a heredoc draft in `/tmp`:
  "why do you ask me about such trivia? allow. It was supposed to be
  without me, or as little as possible. Ask the models"). Until this lands
  the coordinator answers such harmless requests itself (user: "allow").
  Round `20261007-204900-9b47` (sol, MiMo, DeepSeek), all three: drop the shell
  syntax list (risk lies in effects, not in `&&` or heredocs; a parsed
  deny list is spoofable) for a boundary: Claude Code's Bash sandbox
  (seatbelt: writes only to the worktree and a per-worker temp dir, reads
  of credential paths such as `~/.ssh`, `~/.aws`, `.env` blocked, network
  denied or limited to an allow list) plus `--permission-mode auto` (the
  CLI lists it; verify it works together with `--permission-prompt-tool
  stdio` in `-p`); herdr logs every decision. Reaching the user: only what
  crosses the boundary: network or package installs, writes outside the
  worktree and temp dir, credential reads, `git push`/remote/config/hooks,
  `sudo`, ssh. A classifier denial is not forwarded at once: the worker
  tries another way or reports blocked (sol). Questions batched, the worker
  not blocked where it can go on (DeepSeek). Also: worktrees share the
  repository's `.git`, so the boundary must protect it (sol); verify the
  sandbox covers subprocesses and reads (MiMo). File tools keep the
  realpath rule. Trial 3 done 2026-10-07 (`test: headless workers in the
  sandbox and auto mode`, report sections T3-1..T3-6): Claude Code's Bash
  sandbox stopped every boundary-crossing Bash probe (writes outside,
  `~/.ssh`, `.env`, network, subprocesses, a push to a remote outside);
  auto mode alone is no boundary (it allowed all of them when the prompt
  asked, and lets the Write tool write outside the worktree with no
  request); a realistic task under sandbox + auto asked 0 questions; the
  `attribution` setting removes the co-author trailer; a model refusal can
  end with `result/success` and exit code 1. Decided by the coordinator
  from round `20261007-215458-0718` (sol, MiMo, DeepSeek agreeing, against
  the worker's second and fourth recommendations): manual mode + the
  sandbox (`failIfUnavailable`, `allowUnsandboxedCommands: false`, no
  network, credential reads denied), file tools by herdr's realpath rule
  with the worker's temp dir as a second root, herdr answering in-root
  requests itself; keep `disableAllHooks` and the user's settings until a
  hook is really needed; no network (sol preferred a tested allow list:
  try an online `cargo build` first and revisit); no extra write paths or
  the herdr socket for workers: the coordinator runs `just check` after
  bringing a commit in (a socket to herdr's control plane would undo the
  sandbox, sol); a post-turn check that only the worker's branch moved,
  after testing whether `git branch -f` from a worktree works at all;
  workers use their absolute per-worker temp dir, never `$TMPDIR` (shared
  `/tmp/claude-<uid>`, observed clobbering), and the issue is reported
  upstream; a turn's outcome is judged by `result.subtype`, the exit code,
  `permission_denials` and refusal events, not `result/success` alone.
  Next: apply this in `src/workers/` (args, per-worker settings with
  `attribution` off, the temp dir, the policy, the outcome check), in
  progress 2026-10-07 (worker `w-wsandbox`); the user asked: "let the
  models review the implementation this worker made afterwards": a consult
  round on its diff before it is brought in. Round `20261007-221707-16f1` on
  `5f9f349c` (sol, MiMo, DeepSeek), checked by the coordinator: real:
  `.env` denied only at the worktree root for Bash and only for Read/Edit
  in the deny rules; Glob/Grep patterns unchecked (only `path`); the temp
  dir made by remove-then-`create_dir_all` with default permissions;
  replay deleting temp dirs of workers a live handoff may still run;
  credential env vars and paths (`~/.git-credentials`, `~/.netrc`, ...)
  not covered; a non-zero exit or refusal not shown as a failed turn in
  every order. False: "no manual permission mode" (it is there). Sent back
  to the worker as one fix commit; known limit to document: file tools are
  not sandboxed, so a symlink race between herdr's check and a write
  remains (Bash cannot write outside; the coordinator reviews the diff).
  Done 2026-10-07 (`feat: headless workers run in claude code's bash
  sandbox`, `fix: harden the headless worker sandbox settings and
  policy`, checked and installed): `.env` files denied at any depth (the
  sandbox's `denyRead` takes globs on 2.1.293), more credential paths,
  credential env vars removed, Glob/Grep patterns checked, a private
  exclusive 0700 temp dir, temp dirs removed only after the process is
  gone, failed turns on a non-zero exit or refusal; `Write(...)` deny rules
  are ineffective in 2.1.293, `Edit(...)` covers writes (Observed by the
  worker). A real smoke run asked 0 questions. Left: an online `cargo
  build` test (network decision), the post-turn branch check, an upstream
  report on the shared sandbox `$TMPDIR`; then
  the policy change in
  `src/workers/policy.rs`. Gap found 2026-10-07: `herdr worker wait` ends
  only at the turn's end or exit, so a coordinator is not woken by a
  worker's question; add a wait that also ends on a new question (the
  `can_use_tool` event), so whatever still needs an answer reaches the
  coordinator first and the user only when the coordinator cannot decide.

- [ ] A history of finished TODO items to look through afterwards (user, [t-jlw2bqrz]
  2026-10-07, next: "some dropdown list under the coordinator with the TODO
  text, what was done and what conclusions, whether new TODO entries were
  made after it finished. Store it somewhere in SQL? Ask the models. I want
  to look through it after the fact. So it would be like a ticket system
  that could even get a web interface later? Ask the models.").
  Round `20261007-204300-9a56` (sol, MiMo, DeepSeek), agreeing: not a ticket
  system and no web app now: an audit log of finished items. TODO.md stays
  the open work; an append-only `.herdr/history.jsonl` committed in the
  repository holds finished items (SQLite only later as a rebuildable index,
  never the truth: a per-repo database diverges from clones and discarded
  worktrees; git alone cannot show aborted or no-commit items, the user's
  decisions or follow-ups). Each item needs a stable id minted when it is
  claimed (DeepSeek: a ULID, and a `Todo-Item: <id>` commit trailer, which
  cherry-pick keeps). Events: `claimed` written before a worker starts, then
  `done`/`aborted`/`blocked` with the item text at claim and at the end,
  commits, workers and their journals, review findings, the user's
  decisions, consult round ids (references into consult-stats), follow-up
  items created (written at close: not derivable later), checks, timings.
  A reconcile step at coordinator start reports claims without an end and
  TODO deletions without a record, loudly, never dropped. Smallest slice:
  the ledger and the dropdown under the coordinator; then `herdr history
  --html`, one static page, for review after the fact; GitHub issues or
  Datasette only as exports. Overlaps the fresh-coordinator item (verified
  writes, reconcilable records): build them together.
  Diverged: who writes it (sol, MiMo: herdr's server, the coordinator only
  proposes; DeepSeek: the coordinator, every TODO edit its own commit).
  Decided by the user 2026-10-07 (menu): herdr's server writes the records
  through a `herdr history` call that checks the commits and the TODO
  change first; each item gets a short id at the end of its first line
  (`[t-...]`, minted when claimed, also a `Todo-Item:` commit trailer); the
  history is kept outside the repository, under herdr's state dir per
  repository, not committed (so it is private and local to this machine).

- [ ] Show how many pseudo-terminals are in use, e.g. `108/511` (user, [t-e3wrhvox]
  2026-10-07: "show somewhere how many pseudo-terminals are used out of how
  many for the current terminal, now Ghostty, e.g. 450/500; ask the models";
  fourth in the queue). The macOS limit `kern.tty.ptmx_max` is system-wide,
  not per terminal app; herdr can count its own panes' PTYs and the system
  total (`/dev/ttys*`). Ask the models where (footer, header, only past a
  threshold) and how often to sample.
  Round `20261007-132613-27ed` (sol, MiMo), agreeing: always on in the
  footer, dim, e.g. `PTY 65 · sys ~108/511` (herdr's exact count first, it is
  the actionable one; the system figure marked approximate), amber at 70%,
  red at 90%, one toast at 80% and 90% with hysteresis, an `openpty` failure
  always warns; herdr's count updates on pane create/close, the system
  estimate every 10-15 s (slower when low), never per frame. Clicking opens
  herdr's PTY users (job tabs, idle shells) sorted, with close actions that
  confirm before killing a running job. Caveat (MiMo, matches the 527 > 511
  seen in the incident): counting `/dev/ttys*` may not track live
  allocations; find an accurate source (`lsof /dev/ptmx`, sysctl) first.

- [ ] Usage summed per workspace. The author asked every session for its [t-kjpuc4zm]
  `/session` accounting by hand and had an agent record the total. The
  fork's usage module has the numbers per agent. Risk: totals that disagree
  with the provider's bill, and resumed sessions counted twice (sol).
  Decided by the user 2026-10-06: CLI first (`herdr usage --workspace` or
  similar), resumed sessions counted once by session id, labelled an
  estimate, not the bill; no sidebar total yet.

- [ ] Bug (user, 2026-10-03, screenshot): "I closed the tab with the job, [t-xieekngb]
  but it did not close the job." The explicit close is fixed (`DECISIONS.md`,
  "Closing a parent's last pane"); a parent whose shell exits by itself
  keeps its jobs on purpose, but nothing shows it.
  Decided by the user 2026-10-06: when a parent exits on its own, show the
  "Parent <name> exited; N jobs kept running" notice and a `was <name>`
  mark on the orphaned rows (sol, DeepSeek, MiMo); no new "Close tab, keep
  jobs" button.

- [ ] The tab state does not show that something runs in the background [t-fyd7ts3f]
  (user, 2026-10-06, screenshot: "the tab state doesn't show that something
  is running in the background"). Space `music-mpd`: the tab showed idle
  while Claude waited for a finite `musicdb update` (about 2 minutes), its
  footer saying `1 shell still running`. The 2026-10-03 decision left a
  shell alone idle on purpose (`DECISIONS.md`, "Claude background tasks and
  working state"); this revisits it.
  Decided by the user 2026-10-06: a `bg:N` observed-count badge on the
  tab, dev servers included; a `herdr-job wait` counts as work; no
  separate awaiting-background flag.
  - Shape: an optional runtime field (pane background task counts, parsed
    from the footer below the prompt box) in the JSON API, drawn by the TUI
    on the tab line; never a new `AgentStatus` variant (append-closed in
    frozen codecs). Report unknown (no footer seen, agents such as Codex)
    apart from an observed zero; show the count next to every state; notify
    only the final done, never on shell exit (sol). A bare count never makes
    a space busy (bubble, header).
  - Glyph: not `◑` (herdr-job jobs; it promises a tab, a log and an exit
    code), not `⧖`, not `○` (the idle glyph). Dimming must be carried by
    text, not colour (16-colour themes, `NO_COLOR`). Consult calls
    `557d6c84`, `56fe2bac`, `f2132f2c`, `9c834783`.

- [ ] Consult cost per model and the coordinator's extra spend (user, [t-4hgsm43b]
  2026-10-03: "how much money/tokens a model used on a consult, and how much
  more the coordinator burned by asking it"). Today every call logs normalized
  usage, but no money, and the coordinator's own tokens are not logged at all.
  Round `20261003-013157-b88d` (Sol, DeepSeek, MiMo, Space Bunny agree):
  - Money only where money exists: a versioned, dated price table (input,
    cached input, output; reasoning billed as output, never twice). Subscription
    models (GPT, Claude, Gemini) show tokens and "included in subscription",
    never a made-up price; an API-list-price equivalent only as a separately
    labelled column. A free preview model is `$0` for now, not for good.
  - Coordinator: log the Claude Code session id and the round's start and end
    (`new-round` to the last `rate`/`self`), then sum that window's per-message
    usage from the session transcript, cache reads apart, per round, not split
    per model. Do not add the answers again (already in the tool-result
    input). `answer_chars` is only a fallback proxy: it misses reasoning.
  - The true "how much more" needs a few matched tasks with and without a
    consult; a one-off audit, not a stats column.
  Decided by the user 2026-10-06: approved as proposed: a dated price
  table with `$` only for DeepSeek and OpenRouter, and the coordinator's
  usage summed per round from the transcript, labelled "consult-
  associated".

- [ ] Naming: `ask_*` scripts versus the `consult` plugin and `consult.py` [t-xl4wcb52]
  (user, 2026-10-03: "do we need to unify ask in one place and consult in
  another?"). `consult` names the bundle and the stats, `ask_*` are the
  per-vendor adapters; renaming would split the log keys (`skill` field).
  Decided by the user 2026-10-06: keep the names; add one README line
  explaining them.

- [ ] Consult stats default view: mixed rows, too much data, and why `astra [t-puirz3p4]
  -r` ranks above `astra` (user, 2026-10-03: "astra -r better than astra, why?
  how do you rate these models now? The table is mixed up, deepseek is third;
  maybe show last week as the first table. Very much data; is it needed? ask
  the models"). Round `20261003-124630-aaf5`; findings in `DECISIONS.md`
  ("Consult roster").
  - The mix-up: the default table pools all time and sorts by uniq/call, but
    unique depends on who else was asked; `--days 7` alone still shows the
    09-26..09-28 rows.
  - Plan: current configurations first (the default set and running trials,
    in configured order), last 7 days with the dates printed; retired models
    and rows under 5 rated calls collapse into one footer line; rows from
    another coordinator marked or split. Keep: rated/calls, uniq/call,
    rejected share, err, p50. Cut from the default: call dates, the 8-line
    legend (two lines plus `--legend`), anecdotal rows. All of it stays
    behind `--all`.
  Decided by the user 2026-10-06: default to the current set over the last
  7 days; merge the unknown-version DeepSeek alias row into V4.1.

- [ ] Consult stats by lineup (user, 2026-10-03: "shouldn't consult stats [t-xxmtseue]
  show which models were tested together, e.g. sol ds mimo, and now a new
  stage sol mimo? ask the models"). Unique per call only compares models
  asked beside the same companions; the log has 32 distinct lineups. Round
  `20261003-130240-6583`. Plan:
  - `new-round` records the requested lineup (`--models sol,mimo`, the
    consult skill passes the default set), because dates cannot assign
    stages (the MiMo and Space Bunny trials ran inside the sol+ds period).
    Older rounds get a lineup derived from their calls, marked derived.
  - `stats --lineups`: one block per lineup with dates, coordinator, rounds,
    full rounds; per model calls ok/failed, findings, accepted, rejected,
    unique per answered call, p50. Lineups under 5 rounds fold into one line.
  - Default `stats`: the current lineup's block first; no ranking across
    lineups.
  - Kept apart, each with a count so nothing is silently dropped: one-model
    asks, rounds where a companion failed (its outage inflates the other's
    unique), rounds run by another coordinator, rounds with an extra model
    asked on request.
  Decided by the user 2026-10-06: approved: `new-round --models`, `stats
  --lineups`, the current lineup first by default; no named stages (the
  consult skill records set changes).

- [ ] "Consult: models" menu with checkboxes (user, 2026-10-03: "a simple [t-hdb3644s]
  menu: which models are used for consultation now, a checkbox to enable or
  disable, its rank, uniqueness, error rate, and maybe how much the
  coordinator's token cost increases"). Narrows the deferred settings >
  consults page and the auto-consult toggle. Round `20261003-145404-dae6`.
  Decided by the user 2026-10-06: unblocked; the native modal gets its
  data from `consult.py ... --json` through the server, so the statistics
  logic stays in one place.
  - A native herdr modal in Rust (user, 2026-10-03: "a script? I want it in
    Rust"), in the existing dialog style, mouse-first, replacing the menu's
    **consult stats** item. New advertised API methods with neutral names
    (e.g. `consult.models.list`, `consult.models.set`), so it works against a
    remote server; an older server disables only this item. Rows `[x] model
    | uniq/call (n) | wrong% | err% | rated/calls | last used`; a toggle
    shows only after the server confirms it is persisted.
  - State: one global file `~/.local/state/consult/models.json`, written
    atomically. `consult.py models` prints the enabled set and is the single
    source: the skill's default when the file is missing, an empty list means
    consulting is off, a malformed file is an error, not a silent default.
    The consult skill runs it at each round. An explicit request ("ask
    DeepSeek") bypasses the checkbox but never the self-consultation rule or
    a missing key.
  - `new-round` records the enabled set and whether the round was automatic
    or explicitly requested (feeds the lineup item above).
  - No rank column: one number moves when another row is toggled. Metrics
    from rounds of the actual lineup, with n shown, hidden under 5 rated
    calls; `stats --vs` stays the comparison.
  - Coordinator cost, stage 1: the answer tokens each round injects
    (`answer_chars`), labelled a lower bound; the full number waits for the
    cost item above. Subscription models show "included", never `$0`.
  - Later: a `doctor` mark for an enabled model without a key or CLI.

- [ ] Consult stats per model over time, to spot a silently "nerfed" model [t-62w3fpsp]
  (user, 2026-10-03: "what if we showed stats for a model over time? we could
  detect a nerfed model. How to display those graphs then? ask the models").
  `model_version` exists for DeepSeek, MiMo and Claude, never for the GPT
  models (Codex does not report it). Round `20261003-023646-4385`.
  Decided by the user 2026-10-06: only the smallest first step now: a
  weekly list of `model_version`/fingerprint per model; the full
  `consult.py trend` waits. When it comes: a drift report, not a "nerf
  detector" (no composite score, no alerts); the paired difference against
  a reference model over shared rounds (three-model rounds tell which side
  moved); equal-n blocks with CIs and "insufficient n" below the minimum, a
  rule fixed in advance; text in the `page-consult` popup, no kitty-graphics
  PNG (`less -R` strips it).

- [ ] No `?` on a tab that ended with a question (user, 2026-10-01, screenshot [t-goai2j6u]
  of this very session: the tab showed the idle green ring after a turn that
  ended "Install this build, push the commits, or fix the flaky test first?").
  The Claude Code Stop-hook check is done (`DECISIONS.md`, "Stop-hook check
  for unreported questions"). Open: Pi (no `Stop` equivalent found; it needs
  an `agent_end` extension), V3 inference (the hook marks the pane itself,
  `source = inferred`, per-turn generation, cleared on `UserPromptSubmit`,
  typing and the next tool use), an LLM judge for the audit (rubric: "does
  the final message ask the user for a decision or an answer before work can
  continue", two judges, blind to the report status). Thresholds: V3 only
  when the inferred precision's lower bound is above 98-99% and V2 is not
  enough.
  Decided by the user 2026-10-06: investigate the pi part with a consult
  round (should pi get an `agent_end` nudge although it cannot block a
  stop, or does this close with V3?). If the models agree, do what they
  agree on; if not, move the item back under Needs a decision with their
  findings.

- [ ] Consider adding a subtle gradient in the empty space between the job [t-azup3ly4]
  indicators and the next tab in the sidebar (screenshot, 2026-09-29 23:53).
  Show several visual variants in the terminal before choosing one; generate
  the demos with Python, as Claude did previously.
  Decided by the user 2026-10-06: the agent makes demos of a few variants
  for the user to view in a tab, then moves the item back under Needs a
  decision with the variants named, so the user picks one with a click.

- [ ] Remove the agents panel; fold agents into spaces. The spaces list [t-kuaojupm]
  already shows vertical tabs with job squares (`DECISIONS.md`, "Vertical
  tabs and job squares"); the old panel is only hidden by
  `ui.sidebar.show_agents_panel = false`.
  Decided by the user 2026-10-06: remove the panel and
  `show_agents_panel`; the attention counts (`◉1 ●1`: blocked, done and
  unseen; clicking switches to prio) move to the sidebar header; raise the
  white-on-accent contrast (`#4078F2`, 3.9:1) to at least 4.5:1.
  - Also open: space drag and drop in the multi-machine sidebar still works
    from the drawn spaces only.

- [ ] Dragging a space does not show where it will land (screenshot [t-llnkot6j]
  2026-09-26, dragging `herdr`). The live reorder is done (`DECISIONS.md`,
  "Dragging spaces"). Open:
  - A collapsed space does not show that it is the focused one (screenshot
    2026-09-29). A collapsed space is one line: no branch line, its git
    status on the name line without the branch (`► herdr ↑2 ⧖ 1 !3`), and
    the focused collapsed space's name line gets the focused active tab's
    solid accent fill. Consulted (GPT-6 Astra, DeepSeek): fill only the
    focused collapsed space; the collapsed line gets its own configurable
    token list (default `workspace, git_status, tab_jobs`); the triangle,
    `+`, grip and counts need readable colours on the accent; hover must
    differ from the focus fill; a collapsed worktree parent whose child is
    focused gets the fill plus a "focus inside" cue, and its git status must
    not pass off one child's as the group's. Git status before the job
    counts; truncate the name first, then drop the git status, never the
    counts. Worktree children keep no branch.
  - Not done: the row-by-row slide animation, a per-job menu on a square
    (open, close), the drag in the multi-machine sidebar (keeps the old
    look), the priority view (no reordering, a hint to switch). A move is
    sent by ids (`move X before Y`); if another client changed the order or
    the anchor vanished, cancel with a notice.
  Decided by the user 2026-10-06: the agent makes demos of the collapsed
  one-line space and the "focus inside" cue for worktree parents, then
  moves the item back under Needs a decision with the variants named, so
  the user picks with a click.

- [ ] "Restart agents…": restart agent CLIs (Claude, pi) after they update, [t-wbgh3nrb]
  resuming their sessions, e.g. when Claude reports that a new version is
  available. Should herdr tell the instances to restart once they finish
  their work? The manual restart exists (`DECISIONS.md`, "Restart agents").
  Decided by the user 2026-10-06: when an update is detected, only mark
  the agents "restart pending"; the user restarts them from the menu; no
  automatic restart.
  - Version: record `claude --version` when the pane starts, compare with
    the binary on disk (mtime only as a hint).
  - Still to do: version detection and the restart-pending mark, a
    preview/picker (how many idle / working / blocked; current agent,
    selected agents or the workspace, showing which support resume), pi's
    draft check. Restart only an idle pane: not blocked, no draft, no
    subagents (`SubagentStop` hook), no jobs; one at a time.

- [ ] Review queue for agent commits, plus `herdr diff`. When an agent's turn [t-nwuo24w7]
  ends with new commits, list them as "to review" until I acknowledge them.
  Decided by the user 2026-10-06: start with lazygit in a popup on the
  tab's repository, no new code beyond that; a herdr-native list only if
  that falls short. For that list (GPT-6 Astra, DeepSeek, 2026-09-27): the
  unit is the commit (uncommitted changes in the shared checkout cannot be
  attributed); at turn start record the session id and HEAD, at turn end
  find new commits carrying `Claude-Session: <id>`; show each commit's own
  patch, never `git diff first^..last` (other agents' commits fall in the
  range); only an explicit acknowledgement clears "to review". Codex and pi
  need an equivalent of the trailer.

- [ ] Does MiMo earn its slot in the default consult set? (user, 2026-10-06, [t-cdr6gn32]
  after a consult round on GLM and Kimi, `20261006-221442-cf9e`, where both
  models advised checking this before adding any model.) From consult-stats,
  compare MiMo with Sol over shared rounds (`consult.py stats --vs`):
  accepted unique findings per call, dismissed share, errors, latency. Then
  propose keep, replace or drop, with the numbers, under Needs a decision.

- [ ] Pin a tab: pinned tabs are marked with a pin icon (or similar) in [t-grbntule]
  the tab bar and stay at its start, before the unpinned tabs, like
  pinned tabs in Chrome or Firefox.
  Decided by the user 2026-10-06: like pinned spaces: pinned tabs come
  first in their space's vertical tab list; server-owned state; Pin/Unpin
  in the tab menu plus a keybinding; a 1-cell glyph in a fixed column; no
  drag across the pinned boundary.

- [ ] Pin a space, like a pinned tab: a pin icon on the space row, and [t-rqpbuado]
  pinned spaces stay at the top of the spaces list. Consulted (GPT-6 Astra,
  DeepSeek, 2026-09-28), both agreed on:
  - Pinned first in every sort mode (manual, name, prio); the sort and its
    direction apply inside each tier.
  - A 1-cell narrow glyph (ASCII `*` or `▪`), not 📌 (double width, emoji)
    and no nerd-font requirement; in a fixed leading column, so names do
    not shift when a space gets pinned. The icon is only an indicator.
  - Server-owned session state (like the manual order), in the JSON API;
    the sort mode stays client-only. In the multi-machine sidebar pins apply
    per server.
  - Pin/Unpin in the space's context menu, plus a keybinding; no drag to
    pin. Worktree families are pinned whole; a child's menu says "Pin
    family". Unpinning keeps the underlying manual order.
  - Cost to weigh: in prio an idle pinned space sits above an unpinned
    blocked one; urgent unpinned agents need another cue (the header
    attention counts).
  Decided by the user 2026-10-06: dragging a pinned space out of the
  pinned group is refused, and a separator line divides pinned from
  unpinned spaces; unpin from the menu. Pinned spaces come first in every
  sort mode, as consulted.

- [ ] Audit whether colours and symbols are consistent across the UI [t-atqpgouz]
  (sidebar, mobile layout, tabs, toasts, job statuses `⧖ ✓ !`, state dots).
  Consulted (GPT-6 Astra, DeepSeek, 2026-09-28): one colour meaning
  different things in different contexts is not automatically a conflict;
  check it without colour, in light and dark themes and narrow layouts.
  - Audit (2026-09-28), conflicts by severity:
    1. `Done` is teal in `status_color` (`src/client/shell.rs`) but blue in
       the mobile summary (`mobile.rs`) and finished toasts
       (`notifications.rs`); in most themes blue equals `accent`.
    2. The Dots style draws working, blocked, done and waiting-on-job all
       as `●`: colour alone tells them apart.
    3. Blocked has three glyphs: `●` (Dots), `×` (Symbols), `◉` (mobile);
       other red problems use `!`.
    4. `◐` is both agent working and endpoint connecting, both yellow.
    5. Green is both agent idle and job succeeded; `✓` is also agent done
       (Symbols style, teal).
    6. Theme collisions: in `Palette::terminal()` mauve equals overlay0
       (waiting-on-job looks unknown) and peach equals yellow; in Dracula
       blue equals teal.
    7. Mauve means waiting-on-job, focused branch, resize mode and help keys.
    8. Unknown reads "unknown" in `status_text` but "idle" in the agent
       sidebar and mobile.
  - Duplicated mappings: status text (`shell.rs`, `agent_sidebar.rs`,
    `mobile.rs`), glyph and colour overrides in `mobile.rs`, job glyphs and
    colours in `tab_groups.rs`, `tabs.rs` and `ui/sidebar.rs`, toast colours
    twice in `notifications.rs`. Model to follow:
    `endpoint_status_presentation` (`endpoints.rs`) returns glyph, label
    and colour together. Next: one `status_style` module per domain (agent,
    job, endpoint, notification) and semantic palette roles.
  Decided by the user 2026-10-06: the agent makes demos of a few palette-
  role variants (e.g. Done teal vs blue) for the user to view in a tab,
  then moves the item back under Needs a decision with the variants named,
  so the user picks with a click; the legend item waits for that choice.

- [ ] No view of how much memory and CPU spaces, tabs and jobs use (user, [t-xgvnwfyp]
  2026-10-03). Round `20261003-163010-4ae0` (sol, MiMo).
  Decided by the user 2026-10-06: start with a `herdr top` prototype
  computed from `ps` and each pane's process tree, with no new API
  contract (space > tab > pane totals, process count, CPU, memory, sample
  age; `--sort cpu|mem`, `--json`, `--watch`); the native sampler and
  `resources.snapshot` wait. Constraints for both:
  - One process enumeration per tick for the whole machine, one parent
    graph, each `(pid, start time)` assigned once; never one walk per pane.
  - Attribution: the pane's PTY child tree plus processes still holding its
    controlling tty; daemons that escaped (cargo build server,
    rust-analyzer, docker) go into a `shared / unattributed` row, never onto
    a pane. herdr's own server and clients get their own row. Jobs are tabs,
    not a separate bucket.
  - CPU: per-process deltas before summing, labelled "% of one core". Not
    `ps cputime` on Linux (whole seconds).
  - Memory: macOS `phys_footprint` labelled "footprint", Linux RSS labelled
    "RSS" (the `ps` prototype: an estimate); never mix metrics in one total,
    never call a sum "memory freed by closing this space".
  - The native sampler later: server-owned, runs only while subscribed,
    pushes `resources.sampled`, measures its own cost.
  - Tests: aggregation over a synthetic process graph (reparenting, pid
    reuse, a shared daemon, tty holders).

- [ ] Track which buttons the user never clicks, to drop them from roherdr [t-4frwhqjr]
  (user, 2026-10-07: "it would be useful to somehow track which buttons I
  never click at all, so maybe I can throw them out of roherdr? like
  telemetry? ask the models"). Consult the default set first (local-only
  counters vs. anything sent out, where to store them, how to show the
  never-clicked list), then propose the design.

- [ ] A standing self-improvement process over agent sessions (user, [t-avp4qzo2]
  2026-10-07: "today's analysis of all Claude sessions was good, that most of
  the time is waiting for my decision. Also include pi in the analysis, and
  maybe other agents when I use them. A standing self-improvement process,
  but that's probably a separate TODO. Ask the models"). Find today's
  analysis and its script first, add pi's session files
  (`~/.pi/agent/sessions/`) and a per-agent reader so other agents can join,
  then consult the default set on making it a recurring process (how often,
  what it reports, where findings go).

- [ ] No tab line is lit for the focused tab of a collapsed worktree space [t-j543tp6e]
  (user, 2026-10-07, three screenshots: "why does this session have no
  highlighted tab? probably opened by Claude as a todo-worker; ask the
  models"). The focused pane was `worker: shuffle prev`, a worktree space a
  coordinator created (`herdr worktree create --no-focus`) nested under
  `rormpc-tools`; its name line shows `▶` (collapsed), so no tab line and no
  highlight anywhere in the sidebar marks where the user is. Check how
  collapse is chosen for API-created worktree spaces and what a collapsed
  space should show when it holds the focused tab; consult the default set.

- [ ] Navigation history survives a client restart (user, 2026-10-07: "the [t-77zz4rei]
  navigation history is cleared after a client restart, I can't go back").
  The header's back/forward (`focus_history.rs`) lives in client memory, so
  every reattach, and every install's live handoff, empties it.

- [ ] A coordinator waiting on a busy worker looks idle (user, 2026-10-07, [t-tmgddenp]
  screenshot: "this circle is grey, it looks as if the coordinator is not
  working"). Its only running job is `herdr agent wait <worker pane>`: no
  output and no CPU, so after 5 minutes herdr-job reports it `--activity
  idle` and the coordinator's state shows the grey still ring (`◌`, `z` once
  the uncommitted idle-mark change lands), while the worker it waits on
  works (`◐` next to `⚒`). A wait is idle by design; its liveness is the
  awaited target's. Options: herdr-job never marks a wait job (`agent wait`,
  `watch --pid`, `pane wait-output`) idle, or reports the awaited agent's
  state instead of its own CPU and output.

- [ ] Coordinators present "Needs a decision" questions as clickable [t-vs3664vc]
  options (user via the try-roguix coordinator, 2026-10-07: it moved four
  items there and only mentioned them; "fix the process so a coordinator
  that records questions also presents them as clickable options at once or
  at a defined point; ask the models"; the user: "does the collector have to
  be in herdr? earlier you also said something had to be in herdr and the
  /todo skill was enough"). The herdr coordinator did the same with three
  questions on 2026-10-07. Consult rounds `20261007-025516-0631` and
  `20261007-025602-9f40` (sol, MiMo). Both: no herdr collector needed now;
  TODO.md is already the durable state; a script plus a `/decisions` skill
  first, herdr only after lost updates or unseen questions are observed.
  - Each question carries its options in the TODO line (`Options: a | b |
    c`); a skill never invents choices (MiMo); a short id or hash lets the
    asker re-read and skip a question changed meanwhile (both).
  - Timing in the `/todo` skill: right after delegating an item (the worker
    runs meanwhile), ask the new questions with the multiple-choice tool, 4
    per call; if unanswered or on the phone, plain text plus `herdr agent
    awaiting-reply "N decisions"`, which already shows in the `?` list, so no
    herdr badge is needed (MiMo wanted a badge for visibility).
  - Answers are written back as "Decided by the user <date>: ..." and the
    item moves to "Next, in order" only on an explicit approval; writers
    re-read before writing and commit `TODO.md` by path (both). Coordinators
    re-read `TODO.md` before each item, never from memory (MiMo).
  - Done 2026-10-07 (user chose it from a menu): the global rule and the
    `/todo` skill (dotfiles `9b851dd`) require `Options:` on each question,
    ask new ones right after starting the next worker, fall back to plain
    text plus `awaiting-reply`, write answers back, re-read `TODO.md`; the
    false "herdr collects them" is gone. Left: the cross-repository script
    plus `/decisions` skill, and the test below.
  - (sol, before:) The global rule's "herdr collects them from there" was not
    true and had to go. Fix the coordinator stopping between items first, or
    stalls get blamed on the wrong change (both, round 1).
  - Test: a fixture repo with one trivial item and two questions, no user
    present: the item still lands and both questions are asked once; an
    answer written back is not asked again after a restart.
  - Second rormpc report (2026-10-07, forwarded by the user; "fix the
    process, consult the models"): its coordinator piled up 13 questions in
    "Needs a decision" and never asked them; with "Next, in order" down to
    one item it waited on a worker and reported, so work would have
    stopped; only after the user asked "is nothing left? why don't you hand
    it to workers?" did it ask 4 in a menu, which unblocked 3 items at once.
    Expected: ask as soon as they are added, at the latest while a worker
    runs, so "Next" never runs dry while answerable questions wait; record
    answers and queue the work. The rule of `9b851dd` covers only new
    questions, not a backlog of old ones without `Options:`.
  - Other languages (user, 2026-10-07: "what if I also start in Odin? other
    rules than for Rust"): how workers build (worktree or not, a shared
    cache) depends on the repository; it belongs in each repository's
    AGENTS.md, the global rule stays language-neutral.

- [ ] Do the consult popups need `less`? (user, 2026-10-03: "less used in [t-74laujmv]
  consult stats? we have Rust. ask the models"). `page-consult` pages
  `consult.py` output with `less -R`; a popup is a real PTY pane
  (`spawn_popup_command`, `src/app/popup.rs`). Round `20261003-022724-693a`
  (Sol, DeepSeek, MiMo), unanimous: keep `less` for now. Rejected: a herdr
  pager subcommand (rebuilds `less` inside a PTY), rewriting `consult.py
  stats` in Rust inside herdr.
  - First step, a spike: a temporary popup with `command = ["seq", "1",
    "300"]`. Does it keep scrollback, scroll with the wheel, start at the
    top, get SIGWINCH on resize? If yes, drop `less` from `page-consult`
    (print, then wait for Enter; no `/` search). If popups do not scroll,
    that is a herdr defect worth fixing on its own.
  - Later, only if several plugins want it: a manifest text popup whose
    command's stdout herdr renders itself (a new pane type).
  - Known limit either way: tables are fitted to the width at launch.
  Triage 2026-10-06 (manual): The first step is a live popup spike (scrollback, wheel scroll, start at the top, SIGWINCH) that needs you watching the real UI; a popup steals focus.
  Decided by the user 2026-10-07 ("I don't know. A popup spike?"): do the
  spike; a worker prepares the test popup, the user watches it scroll.

- [ ] Phone notifications when I am away from the Mac (agent blocked, [t-p76rjpzl]
  agent done, herdr-job finished).
  - Consulted models (DeepSeek, GPT-6 Astra, GPT-6 Sol, 2026-09-26): a
    Telegram bot sending to my private chat (`chat_id`); Instagram rejected
    (no API for it). Bot chats are not end-to-end encrypted. ntfy or
    Pushover as alternatives.
  - A plugin, not core: agent blocked/done comes as
    `pane.agent_status_changed` through plugin `[[events]]` hooks; herdr-job
    completion has no event, so herdr-job's `notify()` calls the plugin's
    sender. Bot token and `chat_id` in the plugin config, never in payloads.
    Transitions only; dedup per pane and approval request, coalesce bursts,
    drop a stale alert.
  - Away: read it from the OS (macOS `ioreg -c IOHIDSystem` `HIDIdleTime`,
    Linux logind's `IdleHint` or `xprintidle`) plus a manual away/mute
    action writing a state file; the API knows nothing about idle clients.
  - Content: the toast text (`claude finished` plus `workspace · 1 · tab`):
    no paths, prompts or agent output. Later: approve/deny buttons answered
    through the herdr socket, accepting callbacks only from my own user id.
  Triage 2026-10-06 (manual): The spike is done; give a bot token and `chat_id` so sending can be tested.
  Decided by the user 2026-10-07: an Android app, ntfy (no account or
  token: a topic the user subscribes to in the ntfy app). The plugin design
  above stands, with ntfy instead of the Telegram bot; Telegram rejected for
  now.

- [ ] `scripts/fork_demo/README.md` still says "oracle stats" where the menu [t-6aparioe]
  item is "consult stats" (left over from the dropped README animations).

- [ ] Force-quitting a quit Ghostty killed ~19 Claude agents in herdr panes, [t-upz2ppt4]
  and their `?` marks did not come back after `claude --resume` (user,
  2026-10-03, screenshots of job-seeker and email-assistant showing "Resume
  this session with:"). At 15:17:35 loginwindow opened the Force Quit panel
  for Ghostty "because it still has background processes"; at 15:17:39-40
  ~19 `claude` processes exited gracefully at once, while their zsh shells,
  the herdr server and ~12 other claude survived. Auto-resume of such agents
  is done (`DECISIONS.md`, "Auto-resume of agents stopped by a signal").
  - Cause: the herdr server, every pane shell and agent sit in the resource
    coalition of the Ghostty that started the server
    (`proc_pidinfo(PROC_PIDCOALITIONINFO)`); live handoff inherits it. Why
    ~12 agents in the same coalition survived is unexplained.
  - Not done: reproduce with disposable agents (quit Ghostty, force-quit
    its entry, record the signal in a wrapper, `sudo launchctl procinfo` on
    the server, shells and agents before and after); the fix waits for it.
  - Fix, if the reproduction confirms it (sol, MiMo, `20261003-153704-1042`):
    start the macOS server as a per-user LaunchAgent (own coalition):
    `launchctl bootstrap gui/$UID <plist>`, then `kickstart` (never `-k`),
    wait for the socket; `RunAtLoad=false`, `ProcessType=Interactive`, no
    plain `KeepAlive=true`. Costs: send the client's `PATH`,
    `SSH_AUTH_SOCK` and the like per new pane; TCC grants move to herdr
    (sign with a stable self-signed identity); `bootstrap` fails over SSH,
    keep direct spawn as fallback; start the handoff successor through
    launchd too, or exec in place; existing panes stay in the old
    coalition. Rejected: `responsibility_spawnattrs_setdisclaim`,
    `posix_spawnattr_setcoalition_np`, `launchctl submit`.
  - Gaps of the auto-resume: Windows (the PowerShell hook does not report
    the stop), SIGKILL (no hook runs), and the limit and the `?` are not
    saved across a server restart (persist the report keyed by session id,
    restore only when that session resumes with no user prompt after the
    question). Consider a "the agent exited, resume?" hint on panes whose
    agent died.
  - Until then, the user's side: do not Force Quit a "Ghostty" entry that
    shows up after Ghostty has quit; Cmd+Q is enough.
  Triage 2026-10-06 (manual): Auto-resume is done (b2f3adb9, 3a8e7f66); reproduce by quitting and force-quitting Ghostty with `sudo launchctl procinfo`; the LaunchAgent fix waits for that.
  Decided by the user 2026-10-07: reproduce first; a worker prepares the
  script with disposable agents, the user quits and force-quits Ghostty once.

- [ ] An `×` that clears the spaces filter field (user, 2026-10-07: "in the [t-qrtnlvgf]
  filter for searching tabs and spaces, add some x to clear the field").

- [ ] The space lines are hard to read (user, 2026-10-07, screenshot of the [t-nssjdkde]
  sidebar: "this can hardly be read; propose something, ask the models, show
  nice visualisations, in the browser?"). Every space name line carries the
  branch, `↑N`, job counts and three buttons (`❏ T A`); nested worktree
  spaces cut their names to a few letters ("worker: re…", "try-roguix AX…").
  Consult the default set, then show variants as browser mockups and let
  the user choose.
  Consult round `20261007-043713-4b75` (sol, MiMo); mockups (artifact):
  https://claude.ai/artifact/N1pThPneSMEGcsq5XQhctp. Variants: A buttons only
  on hover, B one `⋯` per space (MiMo), C a quiet list with one action bar at
  the bottom (sol), D two-line space headings. Both: counts once, on the
  line that owns them; worker lines show the role mark and branch, never a
  cut name.
  The user rejected all four (2026-10-07): "none. Make it so a space always
  has a coordinator, and clicking the space title opens its tab. Do not nest
  worktrees. Clean the UI of unneeded things. Consult the models, maybe even
  Astra."
  Round `20261007-044327-2f1f` (sol, Astra, MiMo), agreeing: "always a
  coordinator" means a permanent destination, not a running process: the
  title click opens the coordinator's tab without starting Claude (no
  quota, no trust prompt); the agent starts on an explicit Start TODO;
  `❏ T A` leave the rows (menus, coordinator); branch and worktree path go
  to the worker's own view. Split: workers as rows under the space named by
  their task (Astra, MiMo) or only "↳ N worker" (sol); role marks and
  header totals removed (sol, Astra) or kept (MiMo); idle tabs folded (MiMo)
  or never hidden (sol, Astra); spaces without a TODO get no coordinator
  (MiMo) or an overview with "set up coordinator" (sol, Astra). Mockups E
  and F added to the artifact.
  Decided by the user 2026-10-07 (menu, mockup E): a space's title is its
  coordinator; workers are lines under their space, not nested spaces.
  Slices, one worker each:
  1. Clicking a space title focuses its coordinator tab (role
     `coordinator` in that space); with none, it starts one at once (the
     user chose that over a tab that waits for Start TODO), through the
     same launcher as the T button.
  2. Worker worktrees are no longer drawn as nested spaces: each shows as a
     line under the space it came from, named by its task (the worker
     agent's task title, else the tab label), with branch and worktree path
     only in its own view (tooltip).
  3. Clean the rows: no `❏ T A` buttons (new tab and agent launch move to the
     space's right-click menu), no role marks (`♛ ⚒`), no idle `○`, no row
     backgrounds except the selection; the header keeps only `?` and `!`.
  4. A space without `TODO.md` (`~`, scratch dirs): its title opens an
     overview offering "Set up coordinator" (create a TODO.md, then start
     one), not a coordinator.

- [ ] DeepSeek back in the default consult set (user, 2026-10-07: "add [t-sjngd5gy]
  DeepSeek to the consultations; it is fast, and since I have Claude Max 20
  the tokens Claude spends reading its answer do not hurt as much"). The
  `consult` skill (`plugins/consult/skills/consult/SKILL.md`) names sol +
  MiMo as the default set and says DeepSeek left it on 2026-10-03; make it
  sol + MiMo + DeepSeek. The coordinator asks DeepSeek in its rounds from
  now on.

- [ ] What the usage footer can take from Magpie (user, 2026-10-07: "what of [t-wdfmvf66]
  this for our usage widget, https://usemagpie.ai/? work out a TODO with
  the models if needed"). Magpie is a local MIT gateway agents route
  through: per-account used % and resets, tokens, cache hits and cost per
  agent and model, failover between accounts at limits. Round
  `20261007-180654-3509` (sol, MiMo, DeepSeek), agreeing: take decision
  support, not its accounting; herdr must not become a gateway.
  1. Near-limit alerts from the provider usage endpoints we already read:
     one notice when a window crosses a threshold, with its reset time,
     only for providers with running agents (all three).
  2. A pace-based forecast ("~3%/h, likely out in 30-60 min, resets 17:20"),
     coarse and labelled an estimate, suppressed when evidence is weak (all
     three; MiMo and DeepSeek rank it first).
  3. Next-worker hints: "Anthropic 95%, resets in 48 m: start the next
     worker with pi/GPT", which a coordinator can act on (sol, MiMo; MiMo:
     a one-key re-route of the next queued task).
  Skip: exact cost per agent/model and cache-hit rate (only reliable
  through a gateway; flat subscriptions make dollars noise), automatic
  failover. Later and labelled estimated: per-agent burn from local
  transcripts, Claude and Codex first (DeepSeek), which could feed the
  forecast's pace (MiMo).

- [ ] A fresh coordinator per item instead of one long-lived session (user, [t-o6hf6tr3]
  2026-10-07: "can't it be compacted or cleared now and then? ask the
  models"; decided by the user 2026-10-07 from the menu: a fresh coordinator
  per item, state only in files, herdr owns the waits, a thin chat session
  stays for talking with the user). Slices, one worker each:
  1. Verified writes: a small tool the coordinator uses for TODO.md and
     DECISIONS.md edits that fails loudly when the anchor is missing or the
     text did not land (the 2026-10-07 lost edits), and the rule that a
     preference said only in chat goes to DECISIONS.md before the turn ends.
  2. Records a new coordinator can reconcile: each delegated item in
     TODO.md carries its worker id, worktree, branch, base SHA and the event
     waited for; open menus listed in TODO.md.
  3. The per-item coordinator: herdr starts a headless coordinator for the
     top item (its startup prompt rereads TODO.md, DECISIONS.md, `git log`,
     `herdr worker list`, `herdr-job list`), which ends after committing the
     item; the next starts on that event. The chat session only answers and
     queues. The `/todo` skill and the rule change accordingly.
  Background: the user asked 2026-10-07 ("can't it be compacted or cleared now
  and then? ask the models"). Measured on 2026-10-07: the coordinator made
  734 calls averaging ~530k tokens of context; cache reads (388M x 0.1 =
  39M) are most of its 44M weighted cost, against 24M for all 26 workers.
  Round `20261007-203033-73ed`: DeepSeek and MiMo a fresh process per item,
  sol `/clear` now and a fresh process later; all three reject `/compact`
  (a lossy summary, the session already lost track of failed TODO edits
  after one). Needed first under any choice: TODO/DECISIONS writes read
  back and verified (that bug), chat-only preferences written to
  DECISIONS.md before the turn ends, worker/job records a new coordinator
  can reconcile (worker id, worktree, branch, base SHA, the event waited
  for), pending menus in TODO.md.

- [ ] A headless worker's line in the sidebar cannot be clicked (user, [t-3bsem3en]
  2026-10-07, with a screenshot of the `?` list showing `worker w1 ·
  header-arrows` and its Bash question: "a headless worker's entry cannot
  be clicked; ask the models").
  Checked: the `?` row of a worker question has no pane, tab or space
  (`worker_question_row` in `src/client/shell/notification_log.rs`), so a
  click only closes the list. Round `20261007-204431-83a8` (sol, DeepSeek; MiMo
  gave an empty answer), agreeing: a click opens a dialog in herdr's modal
  style bound to the request id (not the worker): worker name and task, the
  full command (monospace, newlines kept, scrollable, never truncated: an
  approval must not rest on a preview) or the question with its options as
  buttons; Allow once / Deny (optional deny message) / View log; no default
  action on Enter. If the request is answered elsewhere or the worker exits
  while it is open, the dialog shows that and disables its buttons; the
  server rejects stale request ids. Several pending requests: "1 of 3", the
  next opens after the answer. No "allow this pattern" for now (both: a
  single command does not show a safe pattern). Keyboard works too.

- [ ] Clicking outside the popup does not close it, only Escape does (user, [t-aonxduu7]
  2026-10-07, with a screenshot of a headless worker's log popup titled
  "popup": "clicking outside the modal does not close it, only escape
  works; ask the models").
  Round `20261007-204532-f8ba` (sol, MiMo, DeepSeek): sol and DeepSeek: popups
  herdr opens itself (the worker log) close on an outside click; popups of
  custom commands (an editor may hold unsaved work) do not, and get an `[x]`
  in the border (DeepSeek: decide by who opened it, not by guessing the
  content; MiMo: ask first when the process still runs). All three: find
  out who owns Escape first (if herdr takes it, vim in a popup cannot leave
  insert mode; likely the log viewer exits on Escape itself), and title the
  popup by its content, e.g. `worker w1 · header arrows · log`, not
  "popup". Chosen by the coordinator from the agreement: sol's and
  DeepSeek's rule.

- [ ] One wait over all of a coordinator's workers (user, 2026-10-07: [t-cguvhgwu]
  "waiting for several workers at once was deferred: so work out a new TODO
  entry with the models"). Builds on "A coordinator is woken by its
  worker's question". Round `20261007-205522-d711` (sol, MiMo, DeepSeek), all
  three: not `worker.wait --any` or an id list (membership races: workers
  started, exiting or re-owned during a wait) but a per-coordinator inbox:
  1. `worker.events --owner <coordinator> --after <cursor> --wait`: blocks
     while empty, returns every pending event in order as a bounded batch
     with `next_cursor` (two questions at once arrive together; returning
     only the first lets one unanswered question starve the rest). Events:
     question, turn end, exit, joined, left, re-owned. `worker.wait` stays
     as a shorthand over it.
  2. One server-wide monotonic sequence filtered by owner, with the
     server's incarnation (`epoch, seq`); kept across live handoff; a
     cursor the server can no longer serve gets `resync_required` and a
     snapshot, never a silent skip.
  3. Reading is not resolving (sol): pending questions are durable state;
     a reconnecting or new coordinator gets a snapshot of its workers'
     states and pending questions first, even when their events are older
     than its cursor.
  4. Handing over, as a transaction, not "hop siup" (user, 2026-10-07: "the
     old coordinator's answers are rejected: what? why is there no handoff?
     It must be like a database transaction"). Round `20261007-210018-c710`
     (sol, MiMo, DeepSeek): workers belong to the item: a coordinator
     finishes and reviews its item's workers before it ends, so normally
     only the item is handed over, not live workers (sol, MiMo). When a
     worker must go on (a crash, a long task), `coordinator.handoff {note}`
     is one server transaction, serialized with answers: it stores the note
     (what was being done, decisions said only in chat, each worker's
     purpose and what to check in review, answers given and why), moves
     ownership and the epoch, and captures worker states, pending questions
     and the cursor; an answer lands either before it (and is in the
     record) or after it (and goes to the new owner). herdr's TODO runner,
     not the old coordinator, picks and starts the successor, which reads
     and acknowledges the package before acting; the old one exits after
     the commit. Workers keep running meanwhile; their questions stay
     queued. Fencing only for a crashed, hung or stale old coordinator: its
     late actions are refused with `ownership_transferred` naming the
     successor, and its late answers are kept and passed to the successor
     as information, never applied and never dropped.
  5. One background job per coordinator (`herdr-job run -- herdr worker
     events --owner ... --wait`), not one per worker; events arriving while
     the coordinator's turn is busy wait in the inbox; after a wake the
     coordinator drains the batch and re-arms from its last cursor.
  Tests: two simultaneous questions in one batch; an event between drain
  and re-arm; a worker started and one exiting during a wait; live handoff
  and restart keep the sequence; re-owning during a wait and a stale answer
  from the old owner; a wake while the coordinator's turn is busy.

- [ ] A headless worker's line shows how long it has been working and which [t-jfo7bcvb]
  TODO task it got (user, 2026-10-08: "I don't see how long a given worker
  has been working, nor which task from the TODO it got; ask the models").
  Round `20261008-031505-ffde` (sol, MiMo, DeepSeek), agreeing, chosen by the
  coordinator:
  - the line: a per-state glyph (with a text state in the tooltip, not
    colour alone), the task title (not the coordinator's nickname,
    truncated with `…`) and the age right-aligned (`<1m`, `12m`, `1h05`,
    `2d03h`), counted from the worker's start (it includes waiting; the
    current turn's time goes to the tooltip and the log);
  - the age refreshes with the sidebar's existing animation/minute
    redraw, computed from the server's timestamps at render, no per-row
    timer (render stays cheap);
  - the tooltip: full TODO title, item id, worker name, state, branch,
    folder slot, turns, current turn's time, last activity;
  - the log popup header: the same, then the full assigned task text
    (collapsible) above the log, and the takeover action visible there
    (DeepSeek: right-click alone is undiscoverable);
  - `worker.start --item <title or id>` separate from `--name`, stored as
    the worker's item; without it, the prompt's first line (recorded as
    inferred); the coordinator passes the TODO item title, and the id once
    items have `[t-...]` ids;
  - ended workers: their lines may go (item "Headless worker lines after
    their work is done") but stay reachable in a history view (sol,
    DeepSeek), which the item history can provide.

- [ ] Record coordinators in the server's SQLite (user, 2026-10-08: "is it [t-ikxxc5ca]
  written to SQL that there is now a coordinator with id X that started
  coordinating at T? ask the models"). Today: no; workers store only
  `owner_pane`/`owner_session`; being a coordinator is a tab role flag.
  Round `20261008-104011-d7f1` (sol, MiMo, DeepSeek), agreeing, chosen by the
  coordinator:
  - a coordination tenure with its own id (`c-...`), never the pane or
    Claude session id: those are bindings that change on resume or a move
    to another pane (a `coordinator_bindings` table: pane, session, from,
    to); a resume keeps the tenure, a handoff starts a new one;
  - events `coordinator_started` / `coordinator_ended` (with the reason) /
    later `handoff` in the same events log, the `coordinators` table their
    projection in the same transaction (repo, current item, started_at,
    ended_at, end reason, epoch);
  - one active coordinator per repository (the TODO rule), enforced by a
    partial unique index on the repo where `ended_at IS NULL`;
  - the server is the source of truth: setting the tab role coordinator
    calls `coordinator.start`, the crown is drawn from the table, ending it
    calls `coordinator.end`; `workers.owner_coordinator_id` points at the
    tenure, the pane/session stay for routing and escalation; obligations
    key on the tenure;
  - a crashed coordinator: closed on the owner events herdr already has
    (pane closed, agent exited) and re-evaluated at server start
    (`end_reason = orphaned`), no heartbeat reaper (DeepSeek's, dismissed
    under the user's events-only decision of 2026-10-08);
  - first slice: the table, the bindings, the two events, start/end calls
    wired to the tab role, the unique index, `owner_coordinator_id`;
    handoff epochs with the handoff item, item history later.
  First slice done 2026-10-08 by `w31` (`feat: coordinator tenures, one per
  repository`, verified, installed); the first tenure is `c-6xve3ubo`
  (the herdr coordinator's tab). Left: obligations keyed on the tenure,
  the current item, handoff epochs, resume keeping the tenure.

- [ ] A deterministic coordinator driver: `herdr todo run` (user, 2026-10-08, [t-s3oaxcki]
  after the coordinator passed a whole label where `--expect-build` takes
  an id: "can't a program restrict the options instead of the agent
  choosing well once and badly once? I dislike the non-determinism in the
  coordinator's work; ask the models"). Round `20261008-181228-d7a1` (sol, MiMo, DeepSeek),
  agreeing: the coordinator chooses intent, the driver owns execution.
  - `herdr todo run <item-id> --task <file> --message <subject> --paths
    <globs> --check <registered check>`: a run persisted in herdr's SQLite
    (run id, item, step, attempt, base sha, worker, branch, command ids,
    last seq, error), an immutable task snapshot, typed argv (never shell
    strings), checks from a registered list; every side effect recorded as
    intent before and result after, reconciled after a crash against
    commit ids, worker ids and remote refs.
  - Steps: preflight (TODO structure, repo state, disk, slot) → start
    worker → attention (policy-covered questions answered and acked; others
    become a question event) → review (base-relative diff and the task:
    the model approves or asks for changes) → stop and confirm exit →
    verify → cherry-pick → `just clean-install` → TODO update with
    `todo_edit` → push → cleanup. A failed verify or rejected review asks
    the model for the next attempt's task text (attempts capped, then
    escalate); a conflict or failed check stops as a blocked event; never
    a silent continue or auto-rebase.
  - The coordinator waits once on the driver's event stream (`herdr todo
    wait`), answers with `herdr todo resume <run> --action ...` naming the
    event id; stale answers refused. No job-id grepping, labels, quoting or
    sleeps in the coordinator's hands.
  - Kept to the model: task text, questions outside policy, the diff's
    intent, retry text, exception approval.
  - A PreToolUse allowlist for the coordinator: `herdr todo ...`, read-only
    git and file reads, `todo_edit`; exceptions through an audited
    `--override --reason` step, never a general shell escape.
  - First slice: preflight → start → attention → ack → stop → verify
    (→ cherry-pick), persisted, resumable, one wait; clean-install, TODO,
    push and the allowlist next.
  First slice done 2026-10-08 by `w38` (`feat: herdr todo run drives an
  item from preflight to cherry-pick`, verified after the coordinator ended
  8 `yes` load processes the worker left: `verify`'s process check caught
  them). Decided by the coordinator for the next slice, from earlier
  decisions: `herdr todo resume` sends the caller's environment again (never
  stored: it may hold credentials); without it a resumed check is
  `unavailable`; a per-run lock held by the server driving it, so after a
  live handoff the new server drives the run only once the old one let go
  (the cherry-pick race w38 named); a worker ignoring SIGTERM gives a
  "still alive" event to the coordinator, no kill on a timer; `todo.*`
  stays local like `worker.*`. Also: AGENTS.md's flaky-test stress recipe
  starts `yes` loads with `&` and relies on `pkill yes`; make the loads end
  with the command (a trap or one process group killed at exit).
  Pattern seen three times on 2026-10-08 (prompt/kill, then the driver's
  test helpers): unix-only test helpers dead on Windows fail
  `just windows-lint` only at the coordinator's clean-install. Register a
  `windows-lint` check (`python3 scripts/windows_cross.py lint`) in
  `.herdr/checks.toml` and pass it with `--check` for changes under `src/`,
  so `verify` catches it before the cherry-pick.
  Second slice done 2026-10-08 through the driver itself (run `r-5wjbvf3m`,
  the first item done by `herdr todo run` end to end: attempt 1 rejected in
  review for a 5 s timer and a missing commit subject in the worker's task;
  attempt 2 approved, verified, cherry-picked as `be9b9a7b`; installed).
  Next slice: clean-install, TODO update, push, cleanup in the driver, then
  the coordinator's allowlist hook.
  Slice 3 open point (run `r-7yjpuc43`, decided by the coordinator): the
  install's live handoff starts a server without the coordinator's
  environment (never stored), so every herdr run would stop at
  `push_failed` until `retry-push`. Next: the old server hands the runs'
  in-memory environments to the new one inside the live handoff payload
  (like the PTY fds), never on disk.
  Run `r-5gsvuqm6` (2026-10-08): the first run of an item the second time blocked
  at start on a branch-name collision; fixed in this run (branches carry the
  run id). Also found: `herdr todo resume --action abort` on a blocked run
  (`r-cese4nxv`) changed nothing; a blocked run must be abortable.
  Run `r-5gsvuqm6` went end to end through the driver (verify, cherry-pick,
  install, TODO note, push) on 2026-10-08; its push needed one
  `retry-push` because the old binary did the install (the next run carries
  the environment). Found: `herdr todo wait` dies with `EmptyResponse` when
  the install's live handoff replaces the server; it should reconnect to
  the new server and keep waiting on the same run.

- [ ] Extract the coordination layer into its own crate (user, 2026-10-08: [t-ydd2vlwe]
  "the coordinator/worker code, the whole control, could be extracted as a
  new package; much less code than all of herdr; then various TUIs, GUIs
  could connect to it; analyse with the models"). Facts: `src/workers/` is
  ~17.5k lines (with ~5k of tests) of herdr's ~345k; it uses herdr's
  platform layer at 60 places (process groups, sessions, start tokens), API
  schema types and thread helpers; herdr reaches into it from API handlers,
  the coordinator tab role, takeover tabs, the live handoff and the client
  snapshot. Analysis, round `20261008-182824-1186` (sol, MiMo, DeepSeek), agreeing:
  - the boundary is real but not yet clean: generic orchestration (store,
    events, receipts, outbox, tenures, runs, broker, policy, verify, folder
    slots, item ids/titles, `todo run`) vs multiplexer integration (PTY
    takeover, coordinator as a tab role, sidebar, Items popup, live
    handoff); coordinator authority must not depend on tab identity;
  - a workspace crate first (e.g. `orchestrator-core`), compiler-enforced
    to import nothing from herdr, linked into herdr's server (still the
    only daemon and API host); platform needs behind ports the crate owns
    (`ProcessSpawner`, `SessionRegistry`, `StartToken`, clock, files),
    herdr implementing them, fakes in tests; decide whether Windows is in
    scope for those ports (MiMo);
  - a separate daemon only when a second client must run without herdr
    (a third long-lived process next to the server and the brokers would
    multiply recovery paths now);
  - a versioned protocol for TUI/GUI/web clients: commands with receipts,
    events with sequence numbers, replay and snapshots, capabilities (e.g.
    takeover); version the envelope, not a two-day-old schema; a web client
    needs an authenticated gateway, not the local socket;
  - when: after the driver's first slice lands (churn), together with the
    planned split of `src/workers/mod.rs` [t-ofobwsra].
  Approved by the user 2026-10-08 ("add it to the TODO"): after the
  driver's first slice [t-s3oaxcki].
  User 2026-10-08: "one could even design the protocol and make an
  implementation in Odin etc.": so the protocol is specified language-
  neutrally (a written spec plus JSON schemas for commands, events and
  receipts, and a conformance test suite any implementation runs against
  a socket), not defined by the Rust types; the Rust crate is the reference
  implementation, and a second one (Odin or another language) is possible
  once the spec and the conformance tests exist.

- [ ] Waiting for a worker without shell state (user, 2026-10-08, after the [t-khw7lira]
  coordinator's `${SEQ:+--after $SEQ}` became one argument in zsh and the
  wait failed at once: "how is it armed? ask the models"). Round
  `20261008-104136-8de5` (sol, MiMo, DeepSeek): a shell-care rule alone is a
  wish; make it mechanical:
  1. `herdr worker wait <id> --attention` without `--after` starts after
     the caller's acknowledged seq when the caller owns the worker (the
     server keeps `acked_seq` since step 4), so the coordinator's loop is:
     wait, handle, `herdr worker ack <id> <seq>`, the same wait again; no
     variable in the shell (chosen by the coordinator: it reuses what the
     server already stores, instead of a second cursor file as DeepSeek
     proposed). User 2026-10-08: "what is --after for? ask the models";
     round `20261008-104542-e5d4`: the wait is level-triggered (nothing that
     happened before it began is missed), so a finished turn would wake it
     again for ever without a cursor; the cursor is needed, the flag is not
     for an owner (sol, DeepSeek): the default is the owner's acked seq,
     and an ack means handled, not seen, so a crash before handling wakes
     it again; non-owners do not inherit the owner's cursor; `--after`
     stays for recovery, tests and observers. MiMo's "clear the state when
     the coordinator acts" was dismissed: it can hide a question or exit
     that arrives at the same time (DeepSeek). The seq is durable (SQLite
     AUTOINCREMENT), so an old cursor stays valid.
  2. `--after=<seq>` accepted as one token; a stray argument names itself
     in the usage error (MiMo).
  3. `herdr-job run` prints the job id as its first stdout line (everything
     else to stderr), so nobody finds a job with `herdr-job list | grep`,
     which can pick the wrong one when several run (MiMo, sol).
  4. AGENTS.md: pass arguments literally; never build options with
     `${X:+...}` or unquoted expansions (zsh does not split them); use
     arrays for lists; treat a failed wait as a failure, not as the worker
     needing attention.
  The per-coordinator inbox (planned) replaces the two-layer wait later;
  no `--then-wait` glue until then (MiMo, sol).

- [ ] Compare the current headless worker implementation with T3 Code again, [t-ofobwsra]
  with the models (user, 2026-10-08: "in spare time, maybe give the models
  the current implementation to analyse again and let them compare it with
  t3code"). A worker reads `src/workers/` (store, wait, receipts, outbox,
  obligations, escalation, sandbox, folder slot, handoff) and
  `vendor/t3code` (orchestration-v2, the Claude and Codex adapters),
  briefs sol, MiMo and DeepSeek with code excerpts, verifies their claims
  and writes a report (`docs/headless-workers-vs-t3code-<date>.md`): what
  T3 does better, what we do better, gaps, and follow-up items in order.
  User 2026-10-08: "fragments? maybe modularise so a model can get a whole
  module? ask the models"; "so check modules one by one, without context
  rot?". Round `20261008-105941-b7f4` (sol, MiMo, DeepSeek), agreeing, so this
  item becomes two steps:
  1. Split `src/workers/mod.rs` (4038 lines; the god object is
     `WorkerSupervisor`, ~50 methods) into cohesive modules of 400-900
     lines, behaviour unchanged, tests green, re-exports keeping callers:
     `status.rs` first (Status, questions, `apply` returning effects
     instead of touching live processes or the registry: pure, the
     highest review value), then `journal.rs`, `live.rs`/`process.rs`,
     `registry.rs`, `types.rs`/`error.rs`, then `supervisor/` by verb
     (lifecycle, commands/questions, takeover, views) and the tests next to
     their modules. Mechanical moves only, one module per commit.
  2. Review module by module, each in a fresh request (no accumulated
     context): the whole module, its tests, and its neighbours'
     signatures with their contracts (invariants, lock order, transaction
     boundaries, failure semantics) plus a generated outline; T3 Code's
     matching part as its interface and the relevant whole file where it
     fits; per-model budget about 50-60k tokens of input (MiMo). Then one
     separate integration review of cross-module paths (start, answer,
     takeover, handoff), where atomicity bugs live (sol).

- [ ] The TODO as a tracker, like Jira or GitHub Issues (user, 2026-10-08: [t-dklsfhg4]
  "maybe it's time for the TODO to be in an SQL database? ask the models";
  "like jira or github issues"). Rounds `20261008-115256-4ae4` and `20261008-115345-2e9e` (sol, MiMo,
  DeepSeek): a private database as the source of truth would strand the
  ~10 repos using the global TODO rules, agents without herdr and git
  review; GitHub Issues in the fork is the closest to Jira (search,
  per-item history and threads, commit links, a board) but public, rate
  limited, and `gh` in this checkout resolves to upstream
  `herdrdev/herdr` by default (checked 2026-10-08), so every call must pin
  `--repo rofrol/herdr`. Decided by the user 2026-10-08, in order:
  1. stable ids `[t-...]` on every item in TODO.md (`todo_edit.py` mints
     them for new items and adds them to the 71 open ones, keeping the
     `?` + `Options:` question shape herdr parses);
  2. a read-only SQLite index in herdr's state dir (`herdr todo sync`: repo,
     id, section, order, title, content hash; no write-back), used by the
     item history and worker links;
  3. a pilot: about 5 items as GitHub Issues in the fork, `gh` pinned to
     `rofrol/herdr` (and a guard that refuses upstream), to feel the
     friction before any migration.
  User 2026-10-08: "there is Codeberg, based on Forgejo, so we could use
  Forgejo or copy something from its architecture; but for now a local SQL
  database is probably enough for us? ask the models". Round `20261008-115719-8c5f` (sol,
  DeepSeek; MiMo gave an empty answer): yes, local SQLite is enough; do not
  self-host Forgejo now (a service, auth, UI to maintain for one user);
  copy its concepts, not its schema: a stable global id apart from a
  per-repo number, title and Markdown body, state, timestamps and a
  version for stale-update detection, append-only comments with author,
  labels, external references (later Forgejo/GitHub ids as mappings),
  explicit order, and the decision questions stored as raw text plus
  parsed fields; defer milestones, boards, timeline, permissions,
  notifications, attachments. One authority per item: an item moved into
  the tracker is no longer edited in Markdown. Step 3 changes: the pilot
  of about 5 items runs in a local tracker in herdr's SQLite (`herdr todo`
  commands, transactional writes), with versioned JSON export/import and a
  Markdown export, instead of GitHub Issues.
  Reference source (user, 2026-10-08): a Forgejo checkout at
  `~/personal_projects/vendor/forgejo` (adfdb1a532); the tracker's model
  is read from `models/issues/` (issue.go, issue_index.go for the per-repo
  number, comment.go, content_history.go for edit history, issue_label.go,
  dependency.go for blocks/blocked-by, issue_xref.go for references),
  copying concepts, not code: Forgejo is GPL-3.0, herdr is Apache-2.0,
  so copied code would force the GPL onto herdr.
  Fossil (user, 2026-10-08: "fossil keeps issues in the repo"): its
  tickets are append-only change artifacts stored and synced with the
  repository, the current ticket table derived from them; the same
  event-and-projection shape as herdr's worker store, and a model for
  keeping tracker items with the repository rather than in a private
  database (git-bug is the git analogue).

- [ ] Headless worker lines in the sidebar after their work is done [t-nulti42o]
  (proposed by the coordinator 2026-10-08 from the user's screenshot of the
  herdr space: "header arrows lost", "answers name their q… lost",
  "vendored mktemp us… exited", "takeover claimed o… exited", each with
  the same green circle):
  - `lost` is wrong for a worker that had finished its turn and only
    waited for a next prompt when an install restarted the server: show
    it as finished (ended by a server restart), not lost (relates to the
    atomicity review's finding 1);
  - the state glyph should differ by state (working, waiting for you,
    finished, failed, exited, lost), as tab lines do;
  - ended workers stay listed for ever: drop a line once its worker has
    exited and the coordinator took its commit (or after the user opens
    its log once), with the log still reachable via `herdr worker log`;
  - a worker that finished its turn keeps its process and its folder slot
    until `herdr worker stop`: decide whether the coordinator stops it
    when it brings the commit in.
  User 2026-10-08, with a screenshot of 19 worker lines under the
  coordinator: "fold idle agents does not work on the tabs the coordinator
  opened; why are they not closed at all? ask the models". Round `20261008-120458-71a7`
  (sol, MiMo, DeepSeek), agreeing, chosen by the coordinator: a worker line
  goes away once the worker is exited or finished, acked by its owner, and
  has no open question or unacked event (not tied to the commit reaching
  master: a cherry-pick can fail); acking a finished turn stops the worker
  and frees its folder slot (an ack means handled); the ⊟ fold button folds
  quiet worker lines with the quiet tabs, never one with a question or an
  unacked event; ended workers collapse into one line per owner tab ("18
  ended workers ▸", expanding on click); `herdr worker list --all`, the log
  and the item history keep them reachable; per-state glyphs instead of
  one green circle.

- [ ] DeepSeek joins the default consult round as a third model (user, [t-evfsfkkc]
  2026-10-09, relayed by the try-roguix coordinator: "add deepseek as an
  additional one to those two models" ... "in the skill"). In
  `plugins/consult/skills/consult/SKILL.md`, the default set becomes sol +
  MiMo + DeepSeek (`python3 "$D/../deepseek/ask_deepseek.py"`), replacing
  "DeepSeek left the default set on 2026-10-03"; keep the history line and
  the self-consultation pairs consistent (a MiMo or GPT coordinator still
  never asks itself). Check the installed copy under `~/.claude/skills`
  follows the plugin.

- [ ] Ideas from Delta (delta.dev, the Zed team) for item history and the [t-sjjnjbxn]
  review queue (user, 2026-10-09: "analyse with the models, also deepseek";
  consult round 20261009-114040-88b8, sol + MiMo + DeepSeek). Delta pairs
  each agent conversation with its checkout ("threads" instead of PRs) and
  anchors comments to fine-grained deltas (DeltaDB, a CRDT layer over git).
  Copy concepts only (license not stated). Agreed by all three: the
  CRDT/delta layer is a trap for one user, one Mac and one worker slot; it
  matters only once two writers edit the same checkout at the same time.
  Worth taking, cheapest form:
  - Code to conversation: the driver adds `Herdr-Item: t-...` and
    `Herdr-Run: r-...` trailers when it lands the commit (the worker's
    commit stays subject-only, as `verify` requires), and the store records
    the worker and landed shas; `herdr worker runs --commit <sha>` (or
    `herdr blame`) finds the run, its transcript and verdict. Trailers survive
    cherry-pick and the upstream rebase; git notes do not, so not notes. A
    run id is provenance of a commit, not a line-level anchor.
  - Review shows the run's transcript, task, diff and check results together,
    and an approval is bound to the exact commit and base (a new commit
    invalidates it).
  - `retry` carries the previous attempt automatically: its commit (to start
    from) and the coordinator's review text, instead of the coordinator
    writing "cherry-pick <sha> first" into every retry task.
  Dismissed: verify on the landed tree (already done: `just clean-install`
  runs `just check` on master with the cherry-pick before installing and
  pushing). Consider (MiMo): the crate extraction before a second consumer
  exists may be premature.
  Decided by the user 2026-10-09: approved, with this data design.
  Today `runs` keeps one row per run and overwrites `worker_id`, `branch`
  and `task` on `retry`, so attempts survive only as `events` plus `workers`
  rows. Design:
  1. `attempts(run_id, attempt, worker_id, branch, base, task, from_attempt,
     from_commit, commit, review_event, review_decision, review_text,
     verification, PRIMARY KEY (run_id, attempt))`. On `retry` the driver
     itself cherry-picks `from_commit` onto the new branch before starting
     the worker and appends the previous attempt's `review_text` to the task.
  2. `approve` records `(commit, base)`; before the cherry-pick the driver
     compares the branch tip with the approved commit and raises a new
     `review` instead of landing a different one.
  3. `landings(landed_sha PRIMARY KEY, run_id, attempt, item, worker_commit,
     ts)`, plus `Herdr-Item: t-...` and `Herdr-Run: r-.../N` trailers added
     by the driver to the landed commit (the worker's commit stays
     subject-only). `herdr worker runs --commit <sha>` looks in `landings`,
     then falls back to the trailers in `git log` (shas change on the
     upstream rebase, trailers stay).
  4. The review view joins `attempts`, `events`, the worker's transcript and
     `git diff base..commit`; no new table.
  Out of scope: per-change identities and line anchors (DeltaDB).
  Line anchors and per-edit provenance, researched 2026-10-09 (user asked;
  consult round 20261009-115028-45b9 with sol, MiMo, DeepSeek, plus a web
  survey with licenses checked by `gh api`): no open-source project does all
  of DeltaDB (it is not released; Zed's `text` anchor crate is
  GPL-3.0-or-later, concepts only). Closest: git-ai (Apache-2.0, 2.8k stars,
  active) keeps agent line ranges in `refs/notes/ai`, rewrites them on
  rebase/amend/cherry-pick and links transcripts; jj change ids (Apache-2.0)
  and Gerrit Change-Id are commit-level; Loro/Automerge/Yrs (MIT, Rust) give
  exact CRDT anchors but every edit must be fed into them (agents write files
  directly, so it becomes diff-ingestion anyway); Radicle (Apache-2.0) and
  GitHub anchor review comments to revision + lines and mark them outdated.
  If line anchors are wanted later, the cheap design all three models
  converged on: anchor = (run id, `git patch-id --stable` of the commit, path,
  byte range, preimage and context hashes), remapped by exact preimage search,
  then rename-aware diffs, then `blame -M -C`, reported as exact, ambiguous
  or orphaned (never moved silently). Per-edit provenance: snapshot the
  worktree per tool call and diff (Edit/Write strings miss Bash/sed edits,
  formatters and whole-file writes); evaluate git-ai's notes format before
  inventing one. Not scheduled: commit-level trailers above come first.
  Would jj help? (user, 2026-10-09: "would jj help here? ask the models";
  round 20261009-115641-7377, sol + MiMo + DeepSeek; jj 0.45.1 is installed
  here). All three: not in the shared checkout and not in the driver. jj
  snapshots the whole working copy, so it would absorb other sessions'
  uncommitted files into a change and break the allowed-paths rule; it
  snapshots only when a jj command runs (no watcher), so per-tool-call
  history still needs an explicit call per tool call; `jj op restore` undoes
  jj's own state, not an install, a push or other sessions' git work;
  workers (and Claude) know git and would bypass it; change ids do not
  survive our git cherry-pick or the upstream rebase, so trailers + SQLite
  stay the source of truth; jj-lib is pre-1.0, so the CLI at most. Where it
  helps: one change id with `jj evolog` across retry/amend attempts, and
  first-class conflicts when jj does the fork-sync rebase. Smallest
  experiment, if ever: only in the worker slot (a jj workspace seeded from
  master), the driver and landing stay git; measure whether `evolog`
  replaces the planned `attempts` table and whether jj calls after each tool
  call give usable boundaries. Not scheduled; the `attempts` table and
  explicit worktree snapshots come first.

## Proposed

Items agents add. Not approved until the user moves them up.

- [ ] While coordinating, which wins: "ask the models" (consult now, in this [t-hgs7p6b4]
  turn) or "a new request is queued, everything else goes to a worker"?
  Report from the email-assistant coordinator (2026-10-07): the user said
  "do todo: ... ask the models", it ran the consult at once before writing
  the TODO entry; the user asked why the coordinator works itself. The herdr
  coordinator ran its consults itself all day too. The rules do not say.
- [ ] `herdr agent wait <worker> --until ... --timeout 3600000` inside [t-jnema5dg]
  herdr-job failed with `Error: Custom { kind: Other, error: EmptyResponse }`
  after 4-5 minutes, four times, while the workers kept running (reported
  by the email-assistant coordinator, 2026-10-07). Long waits must survive;
  find where the socket returns an empty response (a server-side timeout?).

- [ ] herdr: `agent start` reports ready before Claude accepts typed input, [t-gp6n6qbx]
  and `agent prompt` returns `agent_prompted` without knowing the prompt
  arrived (2026-10-07: the bussiness-ideas coordinator's first prompt was
  lost; workers' prompts too until the coordinator resent them). A
  structured `awaiting_user_action` state from `agent start` instead of
  `agent_not_ready` for a startup prompt (sol, round
  `20261007-040046-b1c7`). The launcher (dotfiles `140fc7f`) works around
  both: it waits up to 10 minutes for the user to answer Claude's trust
  prompt and resends the first prompt once unless the agent turns working.

- [ ] Agents name tabs by ids the user cannot see (user, 2026-10-07, [t-7a2ext64]
  screenshot of the `?` list: "how do I know which tab that is?" for
  "approve the edit in tab w4:t6Z"). The sidebar shows space names and tab
  labels, never `w4:t6Z`. A coordinator (and any agent) should name a tab
  as the user sees it, space plus label (or the worker's role mark), and a
  worker that needs the user should be the one marked, so its own tab shows
  `?` instead of the coordinator's. Options for herdr: a `herdr tab focus`
  link in the ask, or a click on the `↳` line that jumps to a tab named in
  it.

- [ ] A working Claude agent's detected state flickers to done/idle. 2026-10-07: [t-evgrqenz]
  `herdr agent wait <worker> --until done --until blocked` returned twice
  while the worker kept working (its screen showed "Thundering…" with a
  running shell, `agent get` said working right after), so a coordinator
  waiting on the state alone reviews a worker that is not finished. Capture
  the detection screen at the flicker (`herdr agent explain --json`) to find
  which rule matches between tool calls. The coordinator's workaround: wait
  for the `WORKER-` line, debounce done/idle for 120 s.

- [ ] A live handoff breaks other sessions' waits. 2026-10-07: each [t-bojbiegs]
  `scripts/herdr_live.sh install` restarts the server, and the try-roguix
  coordinator's `herdr pane wait-output` on its worker failed with
  `server_unavailable` ("server is shutting down"); it then wrapped the wait in
  a retry loop of its own. CLI waits (`pane wait-output`, `agent wait`) could
  reconnect across a handoff instead of failing.

- [ ] Coordinator gaps reported by the rormpc coordinator (todo-rormpc, [t-e5tslo5h]
  2026-10-07, forwarded by the user). After the user answered two "Needs a
  decision" questions with "do it" (release rormpc-tools, install rormpc, add
  config lines), the coordinator bumped the version, committed and tagged
  itself; the user asked why the coordinator worked itself. The global rule
  "Working through TODO.md" does not say: (1) whether an answer to a "Needs a
  decision" question is a new request to queue first or an approval to act
  now; (2) who does release steps workers may not do (push, tag, install,
  editing the user's config after an install): the coordinator, a worker
  with that permission, or the user; (3) a rule changed mid-session reaches
  a running coordinator only partly (it had already done one item itself).
  Also seen: `herdr agent prompt` right after `herdr agent start` in a fresh
  worktree was swallowed by Claude's folder-trust dialog (`agent_prompted`
  returned, the prompt never arrived) until `herdr worktree create` got
  `--trust-repository`; `herdr pane wait-output` has a short default timeout,
  so a herdr-job waiting for a worker's last line failed early.
  The herdr coordinator showed gap (2) too (user, 2026-10-07: "why do you
  make changes yourself when you are the coordinator?"): after the rule
  change it still edited itself the global rule and `/todo` skill (about 15
  lines), a code fix (the role mark's gap, with tests, build and install), a
  flaky test, AGENTS.md notes and the launcher, reading "few-line fixes" as
  covering them. The rule needs a line saying what the coordinator does
  itself (TODO.md and DECISIONS.md edits, triage, answering, integrating
  workers' commits, installs and pushes it is approved for) and that
  everything else, code, tests and rule text included, goes to a worker,
  however small.
  Done 2026-10-07 (user: "plug this gap"), by a worker: dotfiles `a24546d`
  lists what the coordinator does itself and sends everything else to a
  worker, also after a "do it" answer (gaps 1 and 2 above). Left: gap 3 and
  the two herdr observations.
  Also (user, 2026-10-07): a worker needs its own worktree for code
  changes; for documentation and rule text in files nobody else is editing,
  the shared checkout with a commit by path is enough. Seen the same day: a
  dotfiles worktree had to be made by hand (`herdr worktree create` does not
  see the home repository, whose git dir lives elsewhere), and Claude
  stopped in it on the folder-trust prompt.
  Build cost of a worktree per worker (user: "won't the worktree slow down
  building the Rust code? ask the models"; round `20261007-030604-5a6b`,
  sol and MiMo). Measured 2026-10-07: one worker's cold `target/` reached
  5.2 GB, clean-check's is 10 GB, 25 GiB were free (95% used) against the
  15 GiB guard, so disk binds first (MiMo). Both: one shared worker
  `CARGO_TARGET_DIR` (workers run one at a time), clean-check kept separate
  as the correctness gate, shared-checkout work rejected for code. MiMo:
  Cargo fingerprints path dependencies by absolute path, so a new worktree
  path rebuilds `crates/ghostty-vt` (Zig) and `vendor/portable-pty`
  (verified: both are path deps; Zig's own `~/.cache/zig` may soften it).
  My addition: one persistent worker worktree at a fixed path, reset to
  `master` per item like clean-check, keeps paths stable. Fallback (MiMo):
  APFS clones (`cp -Rc`) of a warm target. Measure first: `cargo build
  --timings` cold, `du` of `target/{debug,incremental,nextest}`, `cargo
  check -v` after switching worktrees (dirty units, did build.rs rerun).

- [ ] `workspace list` shows a space's repository only when the space is in a [t-uckv7wa5]
  worktree family (its `worktree` object), so tools read `.worktree` as "the
  space's repo" and fail for plain git spaces (2026-10-07: the todo launcher
  found no repo for job-seeker). Expose the cached `git_space()` on every
  space as its own object (`repo_root`, `repo_name`, explicit null for a
  non-git space) and document `worktree` as worktree-family membership only
  (consult round `20261007-015141-b75b`, MiMo).

- [ ] `herdr tab rename <tab> ""` leaves an empty custom label instead of [t-dxhugzpt]
  clearing it, so the tab shows nothing rather than its automatic name (the
  agent's task or the terminal title); there is no way to return to the
  automatic name (2026-10-07: the todo-worker launcher's `--label TODO`
  hid the coordinator's task, and clearing it left a blank row).
  `handle_tab_rename` calls `set_custom_name(Some(label))`; an empty or
  whitespace-only label should clear it (`None`), in the TUI rename too.

- [ ] `herdr-job clean-tree` should refuse a path that matches nothing. [t-bd7wsdn6]
  2026-10-07: from zsh, `clean-tree $PATHS -- just check` with `PATHS="a b"`
  passed one path with spaces (zsh does not split words); the tree got
  "0 changed paths" and the check ran on bare `HEAD` without a warning.

- [ ] Replace job pinning and folding with an explicit pin (user, [t-5rsujy34]
  2026-10-06: "I don't like this pinning of herdr jobs and the whole
  folding logic. Throw it out."). Remove the automatic attachment of job
  rows to their parent and the fold/unfold logic for job squares (see
  `DECISIONS.md`: "Unfolding job squares near the bottom", "Auto-unfolded
  job squares", "Fold state of job squares across client restart",
  "Folded job squares and focus"). Add only: right click on a job, "Pin"
  in its context menu. A pinned job stays visible the way it is shown now,
  perhaps with an icon that acts as an unpin button, and "Unpin" in the
  same context menu. Before starting, record in `DECISIONS.md` which
  earlier fold decisions this supersedes.

- [ ] Flatten worktree spaces instead of nesting them under the creator [t-g7igxlgs]
  tab (user, 2026-10-06: "why is a worktree tab indented and with
  different logic? Could it be flattened and just marked as a worktree?"
  Example: this session created the worktree space "pi: tab create" for a
  delegated pi agent, and that space's agent starts job tabs of its own).
  This reverses "Worktree space nested under its creator tab" in
  `DECISIONS.md` (user, 2026-10-03/04); the user decides. Consulted Sol
  and MiMo: both recommend flattening. A nested space is the only row
  type with its own header (no name bar, no git details, branch label),
  family drag/pin, a fold toggle and job counts aggregated into the
  parent; space -> tab -> worktree -> tab -> job is deep and narrows the
  mouse targets. Proposal: a worktree space is an ordinary top-level row
  (same header, git details, job counts, collapse, independent drag/pin)
  with a worktree glyph and a secondary `from herdr/7` reference that
  jumps to the creator tab (or reads as history when it is gone);
  `creator_tab` stays as provenance, not hierarchy. A UI-created space
  still lands right after the active space's family, which then is just
  its worktrees. Rejected: nesting as a config option (two navigation
  models, double layout and tests). Note: a nested space already shows
  its own running/failed job counts (`workspace_rows` gets `tab_jobs`);
  the parent's counts include a folded worktree's jobs, which flattening
  removes.
  Screenshot (2026-10-06): tab `Koordynacja pi: tab…` with its job child
  `wait: pi tab create` and the parent's job count, then `└ ▼ pi: tab
  create` (worktree space, `□ A` on the right) and its pi tab `π - You are
  working i…`: three levels of indent for one delegation, and the
  worktree's own line shows no job count until its agent starts a job.

- [ ] Header `?` list: tell the kinds of waiting apart and use the row's [t-dp7pa7y5]
  agent, not the tab (user, 2026-10-06: "now you cannot tell which tab is
  a plain `?` question and which is a question with a choice"; agreed to
  the mockup). Rows draw `tab_state_icon`, the tab's dominant state, so in
  a split tab a pi asking a plain question shows the `×` of a Claude
  blocked on a choice next to it. Proposal (Sol, 2026-10-06): the icon of
  the row's own agent, with `?` awaiting reply, `×` blocked on an
  approval or choice (existing glyph, `src/client/shell.rs:206`) and `!`
  limited, plus the text detail so colour is not required. The sidebar
  tab line keeps the aggregate icon. Separate from the icon column bug
  fixed on branch `pi/list-icons` (`(.., icon)` bound `detail` after
  7f55e2c5).

- [ ] Every header button looks like a button (user, 2026-10-06: "all [t-6o6ow3qe]
  buttons in the top bar should be like buttons: a space on the left and
  right, a highlight on hover"). Consulted Sol (read the code) and MiMo.
  - Padding: one space each side, both cells part of the button's paint
    and click rectangle; always reserved, never only on hover (the row
    would jitter). Agent indicators already reserve two cells
    (`sidebar.rs:496`); sort has a glyph-only hitbox (`space_sort.rs:121`),
    so this is partly a consistency fix. Rejected: one space shared by
    two neighbours (MiMo; Sol: a cell cannot belong predictably to two
    buttons).
  - Styles: hover a subtle neutral background; an open list keeps its
    accent tint and glyph colour (`DECISIONS.md`, header sort button);
    open and hovered a slightly stronger tint. Not bold alone (asking
    counts are already bold). Check indexed-colour themes, where the open
    style falls back to solid accent (`sidebar.rs:1910`).
  - Hover: clear it on focus loss (`input.rs:256`), overlay change and
    re-layout; repaint only when the hovered button changes
    (`endpoint_navigation.rs:91`); without motion reporting only hover is
    lost. Pointer exit without an event cannot be detected.
  - Overflow, when the padded buttons do not fit: shorten the sort key to
    its first letter (`⇅ m↑` for manual, `n` name, `p` prior…; user,
    2026-10-07: the full name shows on click, in the sort menu), then to
    `⇅` alone, then move disabled navigation, fold and zero-count history into
    an overflow menu, a second row as last resort. Bug found on the way:
    `sidebar.rs:482` silently skips a non-zero indicator when it does not
    fit, against "never hide a non-zero indicator".

- [ ] Workers follow the repository's commit rules mechanically (proposed by [t-ti3yszn6]
  the coordinator 2026-10-07 after the user asked why worker `w1` tried to
  commit with a `Co-Authored-By` line that AGENTS.md forbids: "did it not
  know or ignore it? ask the models", then: "Claude Code already reads
  AGENTS.md, github.com/anthropics/claude-code/tree/main/mods/agents-md; ask
  the models"). Corrected finding: the coordinator first said it did not
  know (it never opened AGENTS.md); the user was right: Claude Code 2.1.293
  loads AGENTS.md as project instructions when there is no CLAUDE.md (the
  built-in `agents-md` plugin), and a session launched exactly like a
  worker (`disableAllHooks` included) confirms it has AGENTS.md. So it knew
  and did not apply "no AI co-author lines" against Claude Code's
  attribution reminder, which sits next to the commit and names CLAUDE.md
  and memory, not AGENTS.md, as overriding it (DeepSeek). Round
  `20261007-211912-b075` (sol, MiMo, DeepSeek), agreeing: not a knowledge
  problem; enforce it: (1) turn commit attribution off in the worker's
  settings (verify the setting's name and effect under `--settings` in
  this version); (2) the task gives the exact full commit message and says
  "no body, no trailers"; (3) the coordinator checks the resulting
  commit's message (`git log --format=%B`) before cherry-picking and
  rejects a violation (a `commit-msg` hook is bypassable with
  `--no-verify`). Keep one copy of each rule (MiMo): no CLAUDE.md
  restating AGENTS.md.
  Hooks (user, 2026-10-07: "so maybe the worker should have hooks enabled?
  ask the models"). Round `20261007-212023-f5fe` (sol, MiMo, DeepSeek), all
  three: not the user's global hooks (they target interactive panes: the
  awaiting-reply reminder would land in worker prompts, the coordinator
  stop-check and `uncommitted-notes.sh` would block workers' stops in a
  shared checkout; making each hook worker-aware rots as hooks change), but
  herdr's own worker hook set: the user's settings left out with
  `--setting-sources project,local` (Claude Code 2.1.293 has it; verify the
  user's CLAUDE.md and login stay) instead of `disableAllHooks` (which would
  also turn off herdr's hooks), and herdr's hooks given with `--settings`.
  Candidates: a Stop hook that checks the worker's own new commits
  (message rules, no trailers, the task's subject) and its worktree, and
  refuses to stop with the reason, with a way out after a refusal so it
  cannot loop (sol, MiMo); a PreToolUse Bash check on `git commit` only as
  a hint (bypassable by `git -C`, `-F`, scripts). Needed because
  auto-mode calls never reach `can_use_tool`. The coordinator's check of
  the commits before cherry-picking stays the gate.

- [ ] Hard boundaries instead of text rules (user, 2026-10-08: "constantly [t-6mbbnkor]
  baby-sitting the models through rules in AGENTS.md etc.; where are the
  hard boundaries? ask the models"). Round `20261008-133828-f6d6` (sol, MiMo, DeepSeek):
  the coordinator, holding push and install rights, is the riskiest actor;
  a boundary is hard only if the actor cannot remove it (a hook in `.git`
  is not; a GitHub ruleset or a credential the agent lacks is). Their
  five, in order: protect the fork's history (a ruleset against
  force-push and deletion, which conflicts with the rebase-and-force-push
  fork sync unless agents get a separate credential without bypass);
  remove upstream write paths; install only through `just clean-install`
  (a PreToolUse deny for anything else); restrict the coordinator's writes
  to TODO.md/DECISIONS.md through the verified tool (PreToolUse); CI for
  commit messages (no co-author lines) and `unwrap()` in production code.
  Guidance stays guidance for consult rounds, language and "do not touch
  others' hunks". Decided by the user 2026-10-08: the upstream cut-off
  now; done the same day by the coordinator in this checkout's
  `.git/config` (a sandboxed worker cannot write it): `upstream`'s push
  URL is `DISABLED: ...` (a push fails) and `gh repo set-default
  rofrol/roherdr` (gh defaulted to herdrdev/herdr before). Not chosen now,
  kept here: a `gh` wrapper refusing other repositories (soft: anyone may
  open issues upstream, and PATH wrappers are bypassable), the fork
  ruleset, the install and push allowlist for the coordinator, its write
  scope, and the CI checks.

## Needs a decision

Moved here in the 2026-10-06 triage: each item's last line states what the
user needs to decide or do.

- [ ] Should the coordinator's Stop hook block on state, not wording (a [t-5njp4uab]
  coordinator tab, no background task of the session running, no question
  asked this turn, and "Next, in order" not empty)?
  Options: block on state, once per turn, with `herdr todo run` as the main path (Recommended) | keep wording-only and rely on `herdr todo run` | no hook change
  Checked: the 2026-10-09 audit (`docs/coordinator-audit-2026-10-09.md`)
  found 3 silent stops the wording missed; an earlier consult rejected a
  state rule for its false-alarm cost (a coordinator that correctly waits on
  the user with "Next" non-empty would be blocked once).

### Decide

- [ ] A legend explaining the UI's dots and symbols (agent state dots, [t-m2eyheg2]
  job counts like `!2` / `⧖ 1` / `✓3`, git tokens `↑4` `±7`, endpoint
  states, sort buttons, the grip, footer provider codes). A status legend
  for agent and job states exists (`DECISIONS.md`, "Status legend",
  `src/client/shell/status_legend.rs`); the other domains are open.
  - Consulted models (GPT-6 Astra, DeepSeek, 2026-09-28): a full modal,
    never a bare `?` (it belongs to the agent's terminal); hover tooltips
    only as a supplement; no first-run hint.
  - Content per domain (agent, job, git, endpoint, controls, usage):
    glyph, label, one-line meaning and a swatch in the theme's actually
    rendered colour, never a colour name. Explain the counts by example
    (`!2` two failed jobs, `↑4` four commits ahead); define what `±7`
    counts (files or lines); never describe planned glyphs as current.
  - Generated from the per-domain `status_style` mapping of the audit item,
    with a test that every state variant has an entry. Unknown states from
    older remote servers show as "unknown status". DeepSeek: an "on screen
    now" filter; a warning when the theme gives two states the same colour.
  - Order: DeepSeek after the consolidation and the shape redesign; Astra
    together with the consolidation (my preference: a generated legend
    follows the redesign for free).
  Triage 2026-10-06 (depends): Generated from the per-domain `status_style` mapping of the item above (colours and symbols audit); the ordering also needs your confirmation.
  Decided by the user 2026-10-07: after the colours and symbols audit,
  generated from its style map; waits for that audit.

### Needs you to act or watch
