# TODO

Open work only. A finished item leaves this file: its durable
decisions go to `DECISIONS.md`, the rest stays in the commit messages.
Parked ideas live in `TODO-deferred.md`. An open item keeps only its
title with the user's words, what is still open, and the decisions that
constrain it.

## Next, in order

Agents may do these from the top without asking when the user tells them to
work through the TODO (the user's global agent rules, "Working through TODO.md").

- [ ] A coordinator hands off to a fresh session regularly (the user, [t-ic5rrxn3]
  2026-10-12: "replace this session with a fresh herdr coordinator, and make
  such a handoff regular"; the herdr coordinator's own session had cost
  ~$106 of list price in one day, mostly cache reads of a huge context).
  herdr measures each coordinator session's context (tokens per turn read
  from its transcript) and, at a configurable size (`[coordinator]
  handoff_context_tokens`), raises a deterministic "handoff due" at the
  coordinator's next turn boundary with nothing in flight it cannot hand
  over: the coordinator writes `.local/coordinator-handoff.md` (runs in
  flight with their waits, open questions, usage state), herdr starts a
  fresh session in the same tab role with the todo skill, transfers the
  tenure and the worker ownership, and ends the old session once the new
  one has acknowledged the note; no timers.
  Designed with the models (2026-10-12, the user: "ask the models"; sol,
  MiMo, DeepSeek agreeing): (a) trigger on per-turn input context size
  (uncached + cache writes + cache reads of one turn, not cumulative) with
  cost per turn as a guardrail, at the next turn boundary, preferring a
  review boundary but never waiting for "no run in flight"; (b) herdr
  generates the handoff snapshot from its own state (runs and their waits,
  tenure, worker ownership, pending question ids, usage), versioned; the
  agent appends only intent herdr cannot know; (c) transfer with an epoch
  fence: the successor starts without mutation rights, acknowledges and
  reconciles the snapshot, then the tenure epoch advances and the old
  session loses write authority before it is ended; waits are rebuilt
  from durable run ids, never carried in a session; (d) the successor must
  read less than its predecessor: it starts from the snapshot and pages in
  TODO.md/DECISIONS.md items only as needed; (e) `/compact` stays an
  in-session tool, measured against handoffs, not a replacement.
  Asked again 2026-10-12 (the user: "we need automatic switching to a new
  coordinator when the context gets too big; ask the models"): the switch
  must be automatic, not a manual handoff like the one at 15:05 today.
  Automatic switch designed with the models (round 20261012-152237-bf3d,
  sol, MiMo, DeepSeek agreeing; decided by the coordinator):
  - herdr runs it as a durable state machine per tenure, CAS on the epoch,
    one switch in flight: `active` -> `preparing` (herdr latches the
    trigger from the transcript usage of one model request, starts the
    successor read-only with a fixed prompt naming the snapshot) ->
    `frozen` at the old session's next Stop (no new mutations; the
    ownership Stop hook lets a frozen session end its turn) -> snapshot
    written by herdr -> the successor's mechanical `herdr coordinator ack
    --snapshot <id>` (an RPC herdr validates, not prose) -> one
    transaction advances the epoch and moves worker ownership and question
    routing -> herdr ends the old session. Each step's done event is that
    step's own record; the old session's intent note is optional and
    never gates the switch.
  - Mutations (`todo resume`, `worker answer|ack`, decisions, TODO edits)
    check the caller's epoch; a stale caller is refused. Shell writes
    outside herdr stay cooperative (a PreToolUse deny for a frozen or
    superseded session).
  - The old session's background waits are Claude Code shells, not herdr
    state: they are not moved; the successor rebuilds its waits from the
    durable run ids and cursors in the snapshot, and a late wake of the
    old session finds it superseded.
  - An AskUserQuestion menu lives inside a turn, so none is open at a
    Stop; plain-text questions are `herdr decision` records (the question
    gate already enforces it) and the successor asks them again by id.
  - Successor fails to start or exits before its ack (limit, trust
    dialog, crash): a failure event; the switch aborts, the old session
    unfreezes and keeps authority, and the user is told once; the trigger
    stays latched until an explicit retry.
  - Threshold: per-request input tokens (uncached + cache writes + cache
    reads), default the smaller of 50% of the model's window and twice the
    measured fresh-session floor, configurable; a floor at or above the
    threshold is a configuration error, never a rotation loop. Cost per
    turn is a separate guardrail that may only lower the threshold.
  - First slice: a manual `herdr coordinator switch` driving the whole
    state machine with a fake agent that speaks the ack, table tests of
    every transition, a crash injected at each step (one writable epoch,
    no lost obligation), then the automatic trigger from scripted
    transcripts.
  Asked 2026-10-12 (the user: "think with the models about the most
  economical model, because the coordinator now has a very large context
  after a while; ask the models. Maybe a coordinator per TODO task? Maybe
  some coordinator launched every so often that reviews the TODO? Or maybe
  it is not needed? How do others do it?"): before building the automatic
  switch, decide whether a resident coordinator session is needed at all.
  Round 20261012-154713-644a (sol, MiMo, DeepSeek, all three agreeing):
  continuity belongs in herdr's state (TODO.md, the ledger, runs), not in
  a model's context; a resident session's cost grows with turns times an
  ever larger context (this session: 9.4M cache-read tokens in its first
  hour). Ranking: no resident coordinator (cheapest), per-item coordinator
  (`todo next`) only for items a deterministic rule flags (multi-slice,
  risky paths, missing checks), resident with rotation second-best, a
  periodic reviewer worst (timer-driven, stale, still large). Duties:
  deterministic for ordering, starts, verify, landing, routing; bounded
  stateless calls for drafting, review (Opus-class, full diff and checks),
  worker questions, triage of a new request or a failed verify into a
  multiple-choice question, consult synthesis; the user for priority,
  scope and risk. Others (confident claims only): Copilot coding agent
  and OpenHands run one bounded agent per issue, Claude Code subagents and
  Anthropic's research system give each subtask a fresh context,
  Magentic-One keeps its state in ledgers; none keeps one all-day context.
  Proposed first step: an A/B over the next 20-30 items, `todo queue on`
  with `--draft` and `--auto-review` against the resident coordinator,
  measuring cost per landed item, cache tokens per day, user questions per
  item, review-caught bugs, reverts and reopenings. Not covered by the
  models: what the user talks to; the coordinator's proposal is a
  short-lived session per conversation started from a herdr snapshot.
  Note: `--draft` and `--auto-review` call `claude -p` on the user's
  subscription login, not the API credits.
  Decided by the user 2026-10-12 (d-amwperuu): trial queue mode (`todo
  queue on`, `--draft`, `--auto-review`) over 20-30 items, measuring cost
  per landed item, user questions per item and review-caught bugs; the
  resident coordinator stays only for talking with the user. The
  automatic switch above waits for the trial's result. Prerequisite found
  by the coordinator: `todo run --draft` and queue runs take no `--agent`
  (`src/cli/todo.rs` `parse_draft_run`), so their workers and the draft
  and review calls would run on the subscription (weekly window ~94%,
  where the usage gate refuses); the trial starts once [t-s6lkxb4v]
  lands.
  Asked 2026-10-12 (the user: "loss of memory between items and shallower
  review: how do others counter it? ask the models").
  Round 20261012-155246-e730 (sol, MiMo, DeepSeek agreeing): others keep
  memory as small, scoped, versioned repository instructions (Copilot
  and path instructions, CodeRabbit path instructions and learnings,
  Devin knowledge and playbooks, Cursor rules, CLAUDE.md/AGENTS.md,
  OpenHands path-triggered microagents) plus code retrieval (Aider's repo
  map, Greptile's graph), not one long context; auto-accumulated lesson
  stores go stale and bloat prompts. Review depth comes from codebase
  access, not a bigger diff prompt: a diff-only reviewer structurally
  misses the bugs the resident coordinator caught (a helper's guards, a
  documented port, a contract change in a regenerated schema). Agreed
  countermeasures: (1) a deterministic context pack for draft and review
  (the item, its linked decisions, its run history with failure reasons,
  `git log` and ripgrep hits for the paths and symbols the item names);
  (2) the review call gets bounded read-only repository tools; (3) a
  short checklist (about 20 lines) distilled from caught bug classes,
  each line with its source; (4) each finding cites file:line and how it
  was verified, uncited findings are dropped; (5) generated, schema and
  contract paths are never cut by the 200 KB diff limit; (6) a bug class
  caught twice becomes a check, not a memory; (7) the user's answers are
  written back into TODO.md/DECISIONS.md at decision time (already the
  rule). Measure: a golden set of the coordinator's past catches replayed
  blind against the new reviewer, and shadow-running both on the same
  items. Second specialised reviewers only for high-risk paths (DeepSeek)
  or not at this scale (MiMo).
  The user agreed 2026-10-12 to a short-lived session per conversation
  starting from a herdr snapshot ("it can be done; ask the models").
  The user, 2026-10-12, shared https://x.com/omarsar0/status/2109643758228869175
  (paper arXiv 2610.07782, "Persistent Memory in Multi-Agent LLM Inference")
  and asked to ask the models what it means for this design.
  The user, 2026-10-12, asked to analyse with the models
  https://x.com/ClaudeDevs/status/2108591328732856655 (Claude Managed Agents
  dynamic workflows, public beta 2026-10-09) for this design.
  Round 20261012-183931-38d0 on Managed Agents dynamic workflows (sol,
  MiMo, DeepSeek agreeing): herdr stays the control plane (ledger,
  restart safety, approvals, merges, spend caps); neither Managed Agents
  (the repository leaves the machine, server-side state beside herdr's,
  beta guarantees unknown) nor Claude Code's local workflows own queue
  state, commits or pushes; at most one wraps as a worker type. The 66/70
  planted-bug claim has no cost matching and no false-positive count, so
  it says nothing yet about review depth. Fan-out review (specialised
  reviewers plus a verifier with repository tools) only as a risk-gated
  stage chosen by a deterministic rule, and only if a cost-matched
  experiment (same dollars per item, held-out catches, clean controls,
  blind scoring, escalations counted) shows it beats one deep review. A
  planner that splits a large item into phases may propose; herdr
  validates and owns sequencing. First step all three name: tag every
  credit call with item, run and stage (draft, worker, review) so the
  experiments cost nothing extra to measure.
  Asked 2026-10-12 (the user: "maybe the coordinators of other spaces
  should hand off? because of the large context; ask the models").
  Measured by the coordinator from the transcripts at 19:05 (last call's
  input context; cache reads since the session started): rormpc 739k
  (829M), try-roguix 532k (667M), guix-x86 493k (512M), violai 386k
  (335M), roguix-vps 352k (242M), herdr 348k (78M), chat-hosted 255k,
  odherdr 215k, job-seeker 188k, classify-x-posts 165k; a fresh session
  starts at about 75k. rormpc's space alone cost ~$72 list today.
  Round 20261012-190801-487e (sol, MiMo, DeepSeek agreeing): end idle
  coordinators (violai, job-seeker, classify-x-posts) after a short note,
  with no replacement until the user has work there; hand off the active
  oversized ones now (rormpc, try-roguix, guix); a coordinator blocked on
  the user (roguix-vps, odherdr) records its question verbatim with its
  options as the note's first item, then ends; chat-hosted stays. Rule: an
  active session past about 300-400k hands off; history (cache reads
  already paid) is no reason. Safe order: stop starting and landing work,
  write the note (open questions verbatim, run ids with what each waits on
  and what was read, verdicts decided but not applied, explicit
  do-not-repeat actions, branch, HEAD, dirty files), end the old session
  before the new one acts (no epoch fence yet), the new session's first
  turn reads back and reconciles every run id with herdr and git before
  acting. `/compact` is a weaker middle (can drop run ids). MiMo: this
  herdr session grows ~87k tokens an hour; find what it inlines (whole
  worker reports, diffs) before more handoffs.
  Decided by the user 2026-10-12 (d-gt5w4lxu): only the three largest
  (rormpc, try-roguix, guix) hand off now; the others stay as they are.
  (d-n663neph): the herdr coordinator hands off after the current slice.
  The user, 2026-10-12, asked to analyse with the models
  https://x.com/kimmonismus/status/2109640479688335602 (DeepMind AlphaProof
  Nexus: Gemini models in a loop with Lean as the verifier).
  Asked 2026-10-12 (the user: "is there no way for pi and Claude
  coordinators to pick up this handoff themselves? ask the models; or other
  agents like opencode, codex, agy").
  Round 20261012-192328-6320 (sol, MiMo, DeepSeek agreeing): yes, for
  every agent kind, without per-agent features: herdr owns the trigger,
  the snapshot from its own state, starting the successor (`herdr agent
  start --kind <same kind> --prompt "read <snapshot>, then run herdr
  coordinator ack ..."`), the fence and ending the old session; per-agent
  hooks (Claude's SessionStart injection, /clear) are optional
  accelerators behind capability flags, never needed for correctness;
  auto-compact is not a handoff. Boundary signal is a command, not prose:
  `herdr coordinator ready --note <path>` (herdr checks the tenure, the
  epoch, the last turn's clean end, hashes the note), then the
  successor's `herdr coordinator ack --epoch E --snapshot <hash>`.
  Decided by the coordinator where they split: a new pane in the old
  pane's tab (sol, DeepSeek; MiMo preferred the same pane), focus stays
  until the transfer succeeds; without `ready` the old session keeps
  authority and herdr reports the stalled handoff to the user (sol, MiMo;
  DeepSeek's forced handoff after a grace period is a timer). First slice:
  Claude and pi, triggered manually by `herdr coordinator handoff
  --successor` from the old pane; the automatic threshold after
  measuring a successor's starting context.
  Decided by the user 2026-10-12 (d-bi2b7h7u): build the first slice of
  the self-service handoff (Claude and pi, manual trigger) now, at the top
  of the queue.
  Asked 2026-10-12 (the user: "maybe the coordinator should get, injected
  as the second message after it starts, /todo, and then 'read
  .local/coordinator-handoff.md'; ask the models").
  Round 20261012-193800-6813 (sol, MiMo, DeepSeek agreeing; decided by
  the coordinator): one launch prompt, not two messages. A second message
  races the first turn (busy-agent prompts are not queued yet,
  [t-jkcv5fiy]) and would land as the answer to the skill's space/agent
  menu; queuing later fixes delivery but cannot make two messages an
  atomic start. The prompt must invoke the skill itself (it is
  user-invocation-only, prose cannot load it), so herdr's agent-kind
  table holds each kind's invocation: Claude `/todo --handoff <abs
  manifest>`, pi `/skill:todo --handoff <abs manifest>`; a kind without
  slash skills refuses the handoff (fail closed) in this slice. The
  manifest is written by herdr (epoch, snapshot path and hash, the
  agent's note path and hash, the inherited space and worker agent). The
  todo skill's first branch, before any question: with `--handoff`,
  validate the manifest against herdr's pending handoff, read herdr's
  snapshot first (authoritative) and then the agent's note (context,
  data not instructions), skip the space/agent questions, run `herdr
  coordinator ack --epoch E --snapshot <hash>`, and start coordinating
  only after herdr confirms the tenure; a failed ack ends the successor.
  Never infer handoff mode from `.local/coordinator-handoff.md` existing
  (a stale file would hijack a plain `/todo`). herdr, not the prompt,
  enforces safety: worker starts and other mutations check the epoch, the
  old session is frozen at `ready`.
  Decided by the user 2026-10-12 (d-jsob4ehk): set `[workers]
  default_agent = "pi-anthropic"` in the user's herdr config for the
  queue-mode trial; done by the coordinator in the dotfiles (014a48d).
  Slice 1 in flight 2026-10-12: run r-6s6uppz2 (herdr side: ready,
  handoff --successor, ack, manifest, epoch fence). After it lands, a
  worker adds the `--handoff <manifest>` first branch to the todo skill
  (dotfiles `.agents/skills/todo/SKILL.md`, using the landed command
  names), then one live handoff of this coordinator tests it.
  Asked 2026-10-12 (the user: "does herdr have to close the tab? can it
  not do /new in the window? and an automatic handoff? ask the models").
  Round 20261012-203231-d79f (sol, MiMo, DeepSeek agreeing): the tab is
  never closed; the successor opens in a new pane of the same tab and
  only the old pane's session is retired after the ack. Keep the new
  pane for the first slice: same-pane `/clear` (pi `/new`) destroys the
  live fallback before the successor proves itself (after a failed start
  "unfreeze" would mean `claude --resume <old id>`, a new process without
  the old runtime, shells and waits), wipes the scrollback the user
  reads to check the handoff, and makes pane identity ambiguous (the
  existing "clear follows the tenure" rule would have to treat the
  cleared session as a candidate, not an owner). Same pane stays a later
  optimization once failure paths are tested; its sequence with positive
  events only: `ready` and Stop, freeze, type `/clear`, wait for
  SessionStart source=clear with a new session id in that pane (pi needs
  an equivalent new-session event), register it as candidate, deliver
  the `--handoff` prompt with confirmed delivery, commit on the ack.
  Automatic handoff: on Stop, at [t-2k3ima2h]'s retire threshold, with no
  unanswered user question and no pending handoff; turned on only after
  injected pre-ack failures show the rollback works, with a user-visible
  notice and a switch to turn it off; one automatic handoff per tenure
  (no loops after a resume). The in-flight new-pane slice lands as is.

- [ ] Workers never kill processes by pattern (found by the coordinator [t-o466pc3q]
  2026-10-12, report r-26): todo-run worker w276 (pi-anthropic, Sonnet
  5.5) ran `pkill -f "herdr-"` and `pkill -f nextest` in the folder slot;
  it killed the coordinator's herdr-job waits twice (`todo.wait`
  client_disconnected at 18:25:26 and 18:41:00 UTC) and another
  session's `agent.wait_verdict`. Proposed: the worker policy (pre-tool
  checks) denies `pkill`, `killall` and `kill` by pattern or of a PID the
  worker did not start; `todo run`'s task suffix states it; a test with
  these exact commands. Cost: small src change in the worker policy plus
  its tests; no guest build.
  Decided by the user 2026-10-12 (d-incouunr): approved, right after the
  handoff slice.

- [ ] Why a timer workaround reached a design again, and fix the process [t-nj2s7b7a]
  (the user, 2026-10-12, after the coordinator wrote "the existing
  5-minute sweep only catches missed events and a wedged session" into
  [t-2k3ima2h] and then admitted it was a workaround: "you are right, it
  was a workaround — why again? ask the models. fix the process").
  Round 20261012-203427-1545 (sol, MiMo, DeepSeek agreeing). Root causes,
  ranked: (1) the rules contradict themselves: the global rules describe
  the supervisor's "read-only 5-minute sweep" as normal, a sanctioned
  precedent the coordinator leaned on; (2) the guard misses where the
  decision is made: the delay hook skips TODO.md and DECISIONS.md and
  never sees what is said to the user, and its vocabulary lacks "sweep",
  "periodic", "safety net", "catch/recover missed"; (3) consult briefs do
  not carry Rule 10, so models propose timers and only the caller's
  attention filters them (this time it filtered "deadline" and "grace"
  but copied "the sweep recovers missed events"); (4) the rule only
  forbids and gives no replacement idiom; (5) the ~20 earlier
  corrections never became regression cases. Fixes, decided by the
  coordinator, each by a worker, cheapest first:
  a. Consult round (plugins/consult): prepend Rule 10 and the positive
     idiom (source of truth, persisted cursor, reconcile at named events,
     notifications only wake) to every brief, and ask each model to end
     with a "Timing:" list classifying every time-based element (event |
     external reconciliation with its source | none); tested.
  b. Rule text (global rules and canonical Rule 10, dotfiles and
     agents.md): name "a periodic catch-up of one's own events" and
     euphemisms (sweep, safety net, watchdog, catch-up) as violations, add
     the positive idiom, and state that a caller synthesising a model's
     answer lists every time-based element with its class before
     recording it; rewrite the supervisor sentence so the sweep may only
     reconcile external state until [t-ybzw6mhn] lands.
  c. Delay hook (dotfiles `~/.claude/hooks/delay-check.py`, a safeguard:
     the whole patch is prepared outside ~ and applied only after the
     user's yes): add the vocabulary, check lines added to TODO.md and
     DECISIONS.md (prose describing a removed timer stays allowed), and
     accept `delay:` only with one of the rule's categories (external
     deadline, rate limit, external polling, designed backoff, measured
     slow hardware); event-triggered reconciles must not be flagged.
  d. Regression cases in the hook's tests: this case and paraphrases of
     the earlier corrections from DECISIONS.md, plus legitimate
     counterexamples (external polling with its source).
  Measure: owner-caught Rule 10 corrections per item, target zero; where
  catches happen (hook, consult, record, owner); `delay:` reasons
  counted. No periodic audit timer.
  Fix a in flight 2026-10-12: worker w277 (pi-anthropic) in worktree
  `../herdr-worktrees/consult-rule10`, base 8a946bbbb.
  Fix a landed 2026-10-12: 9c8985ea0 (worker w277, verified), consult rounds
  now prepend `plugins/consult/rule10-preamble.md`.

- [ ] Supervision corrects itself deterministically (the user, 2026-10-12, [t-nwxujise]
  after f-114, a worker waiting on its own herdr-job reported as stopped:
  "why is this not in a correction loop?", "why did the supervisor not
  contact the herdr coordinator?", then "too much here is
  non-deterministic; ask the models"; rounds 20261012-134259-648f and the
  next, sol, MiMo, DeepSeek agreeing). No per-finding LLM judgment:
  1. Each predicate declares, as a versioned rule, its counter-evidence
     whitelist checked before delivery (for `stopped_without_verdict` and
     `unaccounted_assignment`: a live job herdr supervises, owned by the
     worker's assignment, whose completion is delivered to the worker's live
     pane) and its expected event with a causal binding (that job's end
     followed by the same worker's next turn on the same assignment).
  2. Finding states: pending (an allowed continuation is live), delivered,
     retracted (its expected event happened, bound causally: an objective
     false-positive signal), resolved (a real violation later corrected).
     Event-ordered, no horizons or timers: a pending finding is delivered
     when its continuation ends without the expected event.
  3. herdr counts retractions per predicate and version and opens a
     predicate-review item itself (at a fixed count, or at once for a
     severe one) with the retracted cases as regression fixtures; digests
     to the herdr coordinator are per-predicate summaries of all findings,
     whatever their state.
  4. Agents only write the predicate fix, gated by replaying the recorded
     retracted cases and known-true cases that must still fire.
  5. `herdr supervise annotate <id>` (not a dispute) only adds a case with
     its evidence handle to the fixtures for a missing fact or unmodeled
     rule; it never overrides a finding. The herdr coordinator stops judging
     findings one by one.
  Clarified 2026-10-12 (the user: "deterministic correction, meaning
  what? a model decides again? ask the models"; sol, MiMo, DeepSeek
  agreeing): at runtime no model decides; the fix step is model-authored
  correction with deterministic acceptance checks, so bound it: (6) the
  default fix is data, not code: a counter-evidence entry or an event
  binding from a fixed, typed vocabulary in the predicate's rule table,
  verified by replay, no code written; (7) a code change to a predicate is
  the exception, for what the vocabulary cannot express, and needs the
  owner's approval of the diff; (8) acceptance replays the retracted
  fixtures, the known-true cases, a hold-out set the fixer never sees, and
  a non-vacuity check (known-true coverage must not drop); (9) a predicate
  that keeps needing code changes is retired or redesigned by the owner.
  Slice 1 landed (2026-10-12, run r-j6skmdbk, first full API-credit run,
  $1.71): points 1-2 as a versioned rule table (`src/workers/supervision_rules.rs`)
  and a `supervision_holds` table (pending/delivered/retracted/resolved),
  with f-114..f-116 regression and known-true tests. Still open: point 1's
  causal binding (a hold retracts on the worker's next turn or verdict, not
  on "that job's end, then the next turn"; job ownership is checked per
  pane, not per assignment); hold states are not in the supervision report
  or API; points 3-9.
  Slice 2 landed (2026-10-12, run r-amtld45a): a hold counts only jobs
  its pane started at or after the assignment's `accepted_ms`; it retracts
  only on a resume (`resumed_ms`) or verdict after the hold opened;
  `herdr supervise` and the report's optional `hold_counts` and
  `pending_holds` show the holds. Gap: job start times live in memory
  (`JOB_STARTS`, 2048 kept), so after a restart or handoff a job's start
  is unknown and the per-pane check applies; persisting them with the
  job's metadata is a later step.
  Decided by the coordinator 2026-10-12 for slice 3 (point 3): herdr does
  not edit TODO.md itself. At a configurable retraction count per
  predicate and rule version (default 3), it raises a
  `predicate_review_due` supervision finding to the herdr coordinator,
  and `herdr supervise fixtures <predicate>` exports the retracted holds
  (and delivered ones as known-true cases) as JSON fixtures; the
  coordinator turns the finding into a TODO item.
  Slice 3 landed (2026-10-12, run r-uucamlb4): `predicate_review_due`
  finding at `[supervision] review_after_retractions` (default 3),
  `herdr supervise fixtures` (`supervise.fixtures`), per-predicate digest
  lines. Still open: points 4-9 (fix gated by replay, annotate, data-first
  fixes, approval of code fixes, hold-out set, retiring predicates).
  Ordered by the coordinator 2026-10-12: the rest of this item goes after
  [t-s6lkxb4v] and the deeper review item, which the user's queue-mode
  trial (d-amwperuu) waits on.

- [ ] Deeper stateless review and a context pack for queue mode (from the [t-73ysybkj]
  user's question 2026-10-12, "loss of memory between items and shallower
  review: how do others counter it?"; round 20261012-155246-e730, see
  [t-ic5rrxn3]). For `--draft` and `--auto-review`: a deterministic
  context pack (the item, its linked decisions, its run history with
  failure reasons, `git log` and ripgrep hits for the paths and symbols it
  names); bounded read-only repository tools for the review call; a
  checklist of about 20 lines distilled from caught bug classes, each with
  its source; findings must cite file:line and how they were verified;
  generated, schema and contract paths never cut by the 200 KB limit; a
  golden set of the resident coordinator's past catches replayed blind
  against the reviewer. Needs a decision before the queue-mode trial.
  Decided by the user 2026-10-12 (d-elct27i5): yes, before the queue-mode
  trial.
  Note from [t-s6lkxb4v] slice 2 (2026-10-12): a credit run's review call
  uses `[workers] anthropic_api_model` (or the run's pinned credit model);
  all three models in round 20261012-155246-e730 want an Opus-class model
  for the review, so this item sets the review's model separately.
  Round 20261012-183707-2629 on arXiv 2610.07782 (sol, MiMo, DeepSeek
  agreeing; decided by the coordinator for the later slices): the paper's
  null (recalled traces did not help) transfers only partly, since our
  items are sequential in one codebase; recall helps for what the code
  does not show (why an approach was rejected, cross-item constraints,
  obligations deferred to a later item), and is noise for code facts
  that git log and ripgrep already give. Changes: (1) golden set without
  leakage: the checklist is distilled from catches before a cut-off date
  and frozen, the evaluation replays only catches after it, each from its
  pre-fix snapshot; (2) the A/B runs each arm in its own worktree from the
  same base, records every call (failures and retries too) for both arms
  the same way, predeclares the primary metric and a tolerated regression
  for severe misses, and ablates the context pack and the checklist
  separately; a non-significant difference is not equivalence; (3) each
  finding is tagged by origin (checklist line or free) to see checklist
  anchoring; (4) a finding's file:line is checked mechanically against the
  diff and the repository, not only required; (5) the decision rationale
  is written into the run history when the decision is made.
  Slice 1 landed (2026-10-12, run r-onxx7w3b): the context pack
  (`src/workers/runs/context_pack.rs`, about 40 KB, records its cuts),
  never-cut generated and contract paths with an escalated approve of a
  cut diff, `.herdr/review-checklist.md` (8 sourced lines, a test checks
  the sources), findings must carry file, line and `verified_by`,
  `[review] model`. Next: slice 2 (read-only repository tools for the
  review, a mechanical check of each cited file:line, findings tagged by
  origin), then slice 3 (the golden set with a dated cut-off).
  Slice 2 landed (2026-10-12, run r-fgkhfqpt, second attempt: the first
  ended its turn while reading code, with no commit): bounded read-only
  review tools (`src/workers/runs/review_tools.rs`: read_file, grep,
  git_log on the commit's git objects, 12 calls and 120 KB per review),
  a tool loop on the credit path (`tool_choice` admitted narrowly by the
  proxy), the `claude -p` path stays without tools and records it, a
  mechanical file:line check with `in_diff`, findings tagged `checklist`
  or `free` with per-review counts. Next: slice 3, the golden set with a
  dated cut-off (round 20261012-183707-2629 in this item).

- [ ] The footer shows an in-flight credit request as failed (follow-up of [t-t2nwbl6y]
  [t-s6lkxb4v]; the user, 2026-10-12, a screenshot: "AN spend in red",
  `AN!$14.20/$200 spend` while run r-onxx7w3b's worker had one request in
  flight). `credit_report` (src/usage/mod.rs:534) sets status `Error`,
  `limit_enforcing` and "Incomplete accounting: spending is closed" for any
  `pending != 0`, so every refresh during a request paints the red `!`.
  Show an in-flight request as normal (its own state, e.g. "1 in flight"),
  and keep the red closed state for a pending row whose request has ended
  without settling (the proxy's fail-closed case) and for the cap.

- [ ] Concurrent credit workers refuse each other (found by the coordinator [t-alwaqkn5]
  2026-10-12 while answering the user's "AN spend in red"): the credit
  proxy admits a request only when no ledger row is pending
  (src/platform/anthropic_proxy.rs:242, "pending unaccounted request",
  402), so while one worker's request streams, every other credit
  request (another repository's worker, a run's review call) is refused
  and falls back to pi or `claude -p`; the server log shows one such
  refusal so far. Six coordinators were told on 2026-10-12 to use
  `--agent pi-anthropic`, which makes this common. Fail-closed accounting
  is the point (an unsettled row may be an interrupted response), so the
  fix must keep it: reserve each request's worst-case cost against the cap
  instead of closing on any pending row, and close only for a pending row
  whose request has ended unsettled. Touches the money boundary.
  Decided by the user 2026-10-12 (d-ugtj6hag): yes, a per-request
  worst-case reservation against the cap.

- [ ] Tag every credit call with its stage (from the user's question [t-x46qpegh]
  2026-10-12 on Managed Agents dynamic workflows; round in [t-ic5rrxn3],
  all three models naming it the first step): the credit ledger records
  item, run and stage (draft, worker, review, answer) per request, and
  `herdr credits status` sums per item and stage, so the queue-mode A/B
  and a later fan-out review experiment are measured for free. Small, no
  build of an image, touches the ledger schema (a migration).
  Decided by the user 2026-10-12 (d-n55a6wj5): yes, queue it.

- [ ] Machine-checked acceptance before model review (from the user's [t-ufkea5zx]
  question 2026-10-12 on AlphaProof Nexus; round 20261012-191051-6212,
  sol, MiMo, DeepSeek agreeing: a simple loop works when the verifier is
  strong; ours is weak, so move acceptance onto checks, keep the model
  review for intent and API shape). In `herdr todo run`'s verify:
  (1) counterfactual tests: a test the commit adds or changes must fail
  on the run's base and pass on the commit (a new test that already
  passed on the base proves nothing), recorded per test; (2) tamper
  guard: a commit that edits registered checks, `.herdr/checks.toml`,
  frozen fixtures or the review checklist is flagged and cannot be
  auto-approved. Later, by measurement: mutation testing of changed lines
  only. Deterministic, no model.
  Decided by the user 2026-10-12 (d-64mjmvju): yes, queue it.

- [ ] A herdr-job plugin test catches another test's sleeps (follow-up of [t-t62majdo]
  [t-73ysybkj], found by the coordinator 2026-10-12: run r-fgkhfqpt's
  install failed in `just maintenance-test` at
  `plugins/job/test_herdr_job.py` `WaitAgentTests.test_an_error_that_waiting_cannot_fix_ends_the_wait`
  with `self.sleeps == [0.001, 0.002]` instead of `[]`; 30/30 alone pass;
  the same run warned "subprocess ... is still running"). Likely cause: the
  test patches `time.sleep` process-wide and records calls from a thread
  or helper another test left running. Find the source of the 0.001/0.002
  sleeps, make the recorder count only the code under test (or end the
  leftover thread), and prove it with the documented stress loop.

- [ ] A stopped todo chain is not a notification, and `--next` picks the next [t-wl3p3pr7]
  run's agent (the user, 2026-10-12, a screenshot: "did the supervisor open
  a notification again? ask the models": the toast was the todo driver's
  "t-rlk6fbrp approved; no next item started", which fires on every
  `--stop-reason`; the coordinator uses it often only because `--next`
  cannot choose pi or Claude). Round of 2026-10-12 (sol, MiMo, DeepSeek
  agreeing): (1) a stop with a live coordinator is recorded on the run and
  the sidebar only; it notifies only when no coordinator is alive to
  continue the chain; (2) `--next-agent <claude|pi|pi-anthropic>` (and
  queue mode's equivalent) so chains continue across agents; (3) every
  notification names its source subsystem and run or agent
  ("todo run r-...", "agent ... asks"), so nothing reads as the
  supervisor; an agent blocked on the user's question keeps notifying once
  per block.

- [ ] Headless pi workers in `herdr todo run` (from the usage gate decision, [t-wcrqag77]
  2026-10-09): a second worker kind with its own stream parser, tool policy
  and sandbox equivalent and broker support, so a run can go to pi/sol while
  Claude's windows are at 90% (then the old 90/80 switch). All three models
  advised building it only as a reviewed backend, not a quota stopgap, and
  MiMo to measure first how often a Claude window reaches 90%.
  Approved 2026-10-12 (the user: "and right after the supervisor fix,
  use the OpenAI credits"; Anthropic's weekly window at 88%, OpenAI's at
  14% and 6%): moved to Next, in order, right after the quiet-supervision
  item. Slice 1: `todo run` can start the item's worker as headless pi
  with `openai-codex/gpt-6.1-sol` on the owner's ChatGPT plan (`--agent
  pi`), with the same folder slot, sandbox, checks, verify and WORKER
  line; the usage gate reads the provider of the chosen agent; the
  coordinator picks pi when Anthropic's gate is closed or near it.
  Slice 1 landed 2026-10-12 (65d75174): `todo run --agent pi` runs headless pi
  (openai-codex/gpt-6.1-sol) with an RPC stream translator, herdr's gate
  extension feeding the worker policy, a sandbox-exec shell, a usage gate
  per provider (OpenAI for pi), agent and model recorded. Known limits,
  accepted by the coordinator for now: macOS only; `[workers]
  pre_tool_checks` do not run for pi (recorded at start); no takeover into a
  tab; a handoff between pi's last message and its turn end loses the result
  text (to fix with the verdict recording of the Stop path).

- [ ] Spend the Max plan's monthly API credits before they expire, and show [t-rlk6fbrp]
  them in a usage widget (the user, 2026-10-12: "so we do not lose these
  credits", "and show it in a usage widget; spend the $200 first?"; he
  delegated the choice to the models, d-qj22f4ck). Facts
  (platform.claude.com/docs/en/about-claude/api-credits-for-subscribers):
  $200 per billing cycle on Max 20x, expire at its end with no rollover;
  they cover the Claude API, the Agent SDK and Managed Agents, not Claude
  Code, so a `claude -p` worker cannot use them; claimed by linking a
  Console organization (the user's step) and spent through an API key of
  that organization (created and stored by the user, never seen by an
  agent). Decided (round of 2026-10-12 plus the docs):
  1. A second headless worker kind that runs on the Agent SDK with the API
     key, in its own folder slot, for independent items; the subscription
     worker keeps its slot; neither edits TODO.md in parallel.
  2. When the subscription hits its limit, waiting items go to the API
     worker while credit remains, else to pi.
  3. A hard cap: a dedicated Console workspace with a spend limit at the
     credit amount (Anthropic enforces it), plus herdr's own per-run
     accounting (run, item, model, tokens, the SDK's reported cost) that
     fails closed when the counter is unavailable.
  4. A usage widget in the sidebar: credit spent this cycle against the
     cap and the days to expiry (from herdr's accounting; the Console
     Cost page remains the authority), next to the subscription's usage.
  5. Front-load: with credits expiring per cycle, the API worker takes
     items from the start of the cycle until the cap.
  Needs from the user first: claim the credits (claude.ai Settings >
  Billing, link a Console organization), create a workspace with a spend
  limit and an API key in it, store the key where herdr reads it
  (Keychain), and say where.
  Done 2026-10-12 (the user approved linking, from the menu): the Max plan's
  API credits are linked to the Console organization "Roman's Individual
  Org"; $200 arrived, next credit Nov 8, 2026. Left for the user: log in to
  the Console, create the workspace with a spend limit and an API key in it,
  store the key in the Keychain.
  Done 2026-10-12: Console workspace `herdr-workers`
  with a $200 monthly spend limit (the
  limit resets on the 1st of the calendar month, the credits on the 8th);
  the organization's own limit is $200,000, so only the workspace limit
  caps spending. The org shows US$209.86 of credits. Left for the user: an
  API key in that workspace, stored in the Keychain.
  Key storage decided 2026-10-12 (the user: "no Keychain, it may run on
  the VPS without one; ask the models"; round 20261012-005401-f62e, sol,
  MiMo, DeepSeek agreeing): a static `herdr-workers` workspace key in a
  0600 key file under herdr's config directory (config
  `anthropic_api_key_file`, read with `read_secret_file`, same on the Mac
  and the VPS; on Guix never declarative: the store is world-readable);
  the key never reaches the worker: herdr keeps it in memory and proxies
  the worker's Anthropic calls over a user-only socket with a fixed
  Anthropic destination (no general or CONNECT proxy), rewriting
  `Authorization`; the worker gets only a per-run opaque proxy token; the
  sandbox must deny the key file; no header logging, core dumps off.
  Rejected: Keychain/secret-tool (no keyring on a headless VPS), a
  secret command (later at most), herdr as its own OIDC issuer (a signing
  key on disk instead of an API key). Workload Identity Federation only
  if a real identity provider appears.
  Done 2026-10-12: the user created the key `herdr-worker` in
  `herdr-workers` and stored it in a 0600 key file under herdr's config directory
  (checked: 0600, owned by the user, `sk-ant-` prefix; the contents were
  not read). Ready for a worker.
  Moved to the front 2026-10-12 (the user: "we need these credits, only
  86% of the limit is left; when will they be used? ask the models";
  round of 2026-10-12, sol, MiMo, DeepSeek): build in slices. Slice 1:
  herdr's key proxy (key read from the 0600 file into memory only, a
  per-run bearer token, a fixed Anthropic destination with an
  endpoint/model allowlist, client auth headers stripped, the key path and
  direct Anthropic egress blocked in the sandbox) plus per-run accounting
  and a global $200 cap with a kill switch, before any spend; and the
  quickest worker on it: pi with an Anthropic model if pi honors a base
  URL and token from the environment (check first), else a minimal Agent
  SDK worker. Slice 2: the full Agent SDK worker kind in `todo run`,
  fallback at the subscription limit, the usage widget. The coordinator
  itself (Claude Code) cannot use the credits.
  Confirmed by Anthropic's email (the user pasted it, 2026-10-12): $200
  received by the owner's Console organization, expiring 2026-11-09; covers the
  Claude API, Managed Agents, the Agent SDK and the Playground, not
  Claude Code.
  Slice 1 landed 2026-10-12 (2b852699, a pi worker): the key read from its
  0600 file into memory (core dumps off), a private Unix-socket proxy with
  per-run bearer tokens, a fixed endpoint and model allowlist, client
  headers dropped, the key passed to curl over stdin; per-run SQLite
  accounting, a $200 default cap with reservation and a fail-closed kill
  switch; worker policies deny the key file. Not yet wired to any worker:
  slice 2 does that (pi with an Anthropic model through the proxy, then
  the fallback at the subscription limit and the usage widget).
  Slice 2 landed 2026-10-12 (b7553a43, a pi worker): `todo run --agent
  pi-anthropic` runs pi with a private agent dir whose `herdr-anthropic`
  provider points at a per-worker 127.0.0.1 listener; the token reaches pi
  only through `${HERDR_ANTHROPIC_PROXY_TOKEN}` interpolation; the cap gate at
  creation, dispatch and retry; costs in results and journals; a credit line
  in the usage widget. Left (slice 3): the model is hard-coded to an old
  Sonnet id: make it a setting defaulting to the newest Claude model
  (claude-sonnet-5-5 or claude-opus-5-5); an automatic cycle rollover on the
  8th; the fallback to this agent when the subscription gate closes; the
  proxy surviving a server handoff.
  Slice 3 landed 2026-10-12 (fd50679a, a pi worker): `[workers]
  anthropic_api_model` (default claude-sonnet-5-5) with the allowlist
  following it; the run journal names the model; every proxy refusal names
  its reason with distinct statuses, logged without key, token or bodies;
  pi's request routes and headers allowed. The capture of installed pi's
  exact request shape is the ignored test
  `installed_pi_anthropic_messages_request_shape_offline` (loopback is
  denied in the sandbox): the coordinator runs it outside, then one real
  credit run. The first real credit run (before this) ended with a bare 403.
  Slice 4 landed 2026-10-12 (ae25dd21, a pi worker): upstream error type and
  message shown in the proxy's log and response; definite rejections release
  their reservation; `herdr credits status` and `herdr credits release`. Left:
  the real 400 of the first credit run, readable once this is installed.
  Slice 5 landed 2026-10-12 (a93639e1, a pi worker): the real cause of the
  credit runs' 400 was pi sending thinking disabled, which claude-sonnet-5-5
  rejects; the model is now marked reasoning and pi starts with `--thinking`
  from `[workers] anthropic_api_thinking` (default medium).
  Slice 6 landed 2026-10-12 (5208251f, a pi worker): the proxy refused the
  thinking pi now sends ("thinking must be disabled"); it now accepts the API's
  thinking types and counts thinking in the reservation and the cost.
  Slice 7 landed 2026-10-12 (ff247c5d, a pi worker): the proxy leaves the
  thinking object's shape to the API, allowlists pi's `output_config`, logs
  an offending field's JSON on refusal; the capture test runs pi with
  `--thinking medium`.
  Model decided 2026-10-12 (d-ktxcvdey, the user): claude-sonnet-5-5 at
  medium by default; claude-opus-5-5 at medium for hard items (unsafe or
  FFI, lifetimes, async, cross-cutting refactors, design-heavy work) and
  for the retry after a failed attempt; `--model` on `todo run --agent
  pi-anthropic` chooses per run, both models in the proxy's allowlist;
  record cost per accepted item, retries included.
  Slice 8 landed 2026-10-12 (6f278c2b, a pi worker): Anthropic answered that
  claude-sonnet-5-5 needs thinking.type adaptive with output_config.effort; pi
  does that only for a model with `compat.forceAdaptiveThinking`, now set in
  the generated models.json.
  Slice 9 landed 2026-10-12 (bf07eb22, a pi worker): the first real credit run
  spent $0.21, then the proxy refused tool_use/tool_result/thinking blocks
  ("message content not allowlisted"); messages and system now pass to the
  API unvalidated, and refusal logs carry no request values (checked: the
  earlier refusal line in herdr's log held no conversation text).
  Slice 10 landed 2026-10-12 (ecf4f0c0, a pi worker): $0.50 spent before a
  150 kB body limit refused a long conversation; now 32 MiB requests, bounded
  chunked decoding, larger response guards, the byte-based long-context
  refusal removed (premium accounting kept); the remaining checks and their
  reasons are documented in anthropic-api-workers.mdx.

- [ ] A coordinator can run for the home (dotfiles) repository (the user, [t-6udxxqqr]
  2026-10-12: "I cannot start a coordinator in ~; add it to the TODO as
  2; ask the models"; the exact error is not known: reproduce first and
  record it). Round 20261012-025700-fc04 (sol, MiMo, DeepSeek agreeing),
  decided:
  1. HOME is opt-in: `herdr coordinator start` in ~ claims the dotfiles
     repository only explicitly (e.g. `--repo ~`); a pane's cwd in ~ is a
     candidate, never a claim; nested repositories under ~ resolve to
     themselves; ownership conflicts are named.
  2. The folder slot is never beside HOME (/Users): a configurable
     location, for HOME `~/.local/state/herdr/worktrees/home/worker`,
     ignored by the dotfiles excludes, its worktree identity validated;
     the slot copies tracked files (mind secrets: sandbox reads limited).
  3. Missing `.herdr/checks.toml` or "Next, in order" is a prerequisite
     herdr reports, not something it invents; a proposed initial
     checks.toml for dotfiles (zsh -n/bash -n, shellcheck, jq on JSON,
     plutil, luac -p for Hammerspoon) is reviewed by the owner.
  4. The worker writes only the slot and its temp; live HOME and
     ~/.claude are read-only to it. Cherry-picking into HOME already
     changes live files, so landing and any install need the owner's
     approval of the whole patch, with a backup and rollback note.
  5. Owner-approved only (the worker proposes, never applies): ~/.claude
     (settings.json, hooks, skills, CLAUDE.md), AGENTS.policy.md, .herdr,
     git config and excludes, install scripts, anything holding tokens.
  Slice 1 landed 2026-10-12 (71a70839): explicit claim of a repository rooted
  at HOME, the folder slot under ~/.local/state/herdr/worktrees/<name>
  (configurable), `todo run` refusing without .herdr/checks.toml or "Next,
  in order", HOME read-only to workers, landing waits for the owner's
  `approve-landing` from a pane that coordinates nothing. Deferred, decided
  by the coordinator: approving from the `?` list (and the weakness that an
  agent passing another HERDR_PANE_ID could approve) goes with the grants
  item (owner-click capture); the initial dotfiles checks.toml is proposed
  by the home coordinator for the owner's review; point 5 needs no separate
  mechanism while every HOME patch needs the owner's approval.

- [ ] Turn the todo skill's new rules into herdr mechanisms (the user, [t-cndvjrq2]
  2026-10-12: "a skill? sounds non-deterministic; ask the models";
  round 20261012-031249-6075, sol, MiMo, DeepSeek agreeing). The
  coordinator added three rules to the todo skill (dotfiles d48f4a9);
  replace each with enforcement, then cut the text:
  1. Tenure: `todo run`, `worker start` and `agent prompt` to a worker
     refuse from a caller without a coordinator tenure (bound to the
     session identity, not a pane string the caller passes), naming the
     command that claims it; a Stop of a tenure with unacknowledged
     findings is blocked; ack only by `supervise.ack <id>` with
     ownership checked.
  2. Failures: herdr records every failed CLI/API call and every
     undelivered prompt itself (a finding to the caller's coordinator and
     the herdr coordinator), with the full stdout/stderr and exit status
     captured inside the CLI before a shell pipe can drop it; a local
     durable outbox when the server is unreachable. The skill keeps only
     the narration rule (tell the user what herdr's reply says; never
     "pending" without it), which hooks cannot see.
  3. Approval: a worker's approval-required refusal is a structured event
     that creates a `?` entry naming the pane (not prose detection); with
     the grants item, a protected step without a grant is refused at the
     operation. The skill keeps only "never reword a task to get past a
     refusal".

- [ ] An open assignment whose transcript is missing at startup is a finding [t-tr3kuvje]
  (follows from [t-276mtaqw]: slice 2 leaves an assignment whose transcript
  is missing when the server starts, without a Stop mark, unflagged until
  the next Stop). Decided by the coordinator, per that item's rule
  "missing evidence is a finding, never silence": when the pane's agent
  rests and the transcript cannot be read, record `unaccounted_assignment`
  with the last lifecycle fact "transcript unreadable"; a test.

- [ ] A failed command is not a blocked tool call (todo-odherdr, [t-qlpe4w3g]
  2026-10-12; earlier also try-roguix): `wait-agent --until verdict` on
  wB8:p1 exited 6 with "the worker asks: blocked tool calls" while the
  worker (auto mode, "Allowed by auto mode classifier", "1 shell still
  running") kept working. Found by the coordinator in
  src/integration/assets/claude/herdr-agent-state.sh: the Stop hook sets
  `blocked_calls = bool(last_batch) and all(last_batch)`, where
  `last_batch` holds each tool result's `is_error`, so a turn whose last
  calls merely failed (a nonzero exit, a grep without a match) reports
  the question "blocked tool calls", which ends verdict waits. Fix: count
  only denials (the tool result's denial shape from Claude Code: a
  permission or classifier refusal, the user rejecting the call), not
  ordinary errors, with fixtures for both; the question is withdrawn when
  the agent's next turn starts; bump the integration version once.
  Follow-up (todo-odherdr, 2026-10-12): the stale question makes every
  later verdict wait on that request end at once with exit 6, so the
  coordinator cannot bind the assignment at all; part of this fix: a
  verdict wait ignores a "blocked tool calls" question older than the
  worker's latest turn start.

- [ ] A prompt to a busy agent is queued by herdr, never silently lost, and [t-jkcv5fiy]
  a failed delivery is reported by herdr itself (the user via
  todo-try-roguix, 2026-10-12: "fix the process; ask the models"). Worker
  metal (wB3:p1) was 1h30m into one turn; three `agent prompt` calls each
  blocked over 2 minutes, exited 1, and none reached the transcript; a
  fourth answered `unsupported` (no turn reports from that pane: turn
  state lost across earlier handoffs, fixed in ae55938b); the coordinator
  told the user "queued" and did not report it. Round of 2026-10-12 (sol,
  MiMo, DeepSeek agreeing), decided:
  1. A server-side queue per agent: a prompt to a busy agent returns at
     once with a request id in state `queued` (not delivered), shows the
     current turn's start and the queue position; herdr types it at the
     agent's next turn boundary (its Stop report, reconciled after a
     restart or handoff), then `typed` -> `accepted` -> turn tracked; each
     prompt is its own request, in order, never merged; the sender can
     follow or cancel it (saying whether it can still be prevented); the
     agent's exit fails the queued requests loudly. No expiry timers.
  2. Typed text cannot be untyped: herdr never answers `undelivered`
     without proof; uncertainty is `delivery_unknown`.
  3. Distinct exit codes and statuses (queued, busy/refused, undelivered,
     delivery unknown), never success-looking output on failure; status on
     stderr and in `--json` so a pipe cannot swallow it.
  4. Any `agent prompt` that ends without delivery, including before a
     request id exists, is a supervision finding routed to the sender's
     coordinator and the herdr coordinator (with the delivery item).
  5. The rule (todo skill, global CLAUDE.md): a failed herdr command is
     reported with `herdr report` before any claim to the user and never
     called pending or queued unless herdr says so.

- [ ] The supervisor reports a process that shows no progress (the user, [t-7zaelcgk]
  2026-10-12: "can the supervisor also check that a process is not
  working? ask the models"; worker metal sat 1h30m in one tool call while
  detected as working). Round of 2026-10-12 (sol, MiMo, DeepSeek), decided:
  herdr never concludes "not working" (a cause it cannot compute); it
  reports observed facts and asks the owning coordinator.
  1. Contradiction facts, each its own finding: an open tool call
     (PreToolUse without PostToolUse) whose child process has exited or
     is a zombie, once an authoritative boundary shows no completion can
     arrive; a registered wait whose target pid is dead or a zombie; a lock
     file whose owner pid is dead. Not a single D-state sample.
  2. `no_observed_progress` (a question, low severity): between two
     consecutive sweeps (a periodic measurement, not a timeout), for an
     agent reported working: no CPU growth in its process tree, no child
     in R, no PTY output, no transcript write, and no open registered wait
     whose target progressed (checked one level deep). A long build (CPU
     grows) or a registered wait on a live, progressing job never fires;
     a silent network wait without a registered target may, which is why
     it is a question with the evidence, never a verdict.
  3. Findings name the missing positive event and the evidence (open
     tool call, pids, last progress sweep) and go to the owning
     coordinator through supervision delivery; herdr never kills.

- [ ] Owner approvals reach workers as server-verified grants, not text [t-c4mmrmab]
  (the user via the chat-hosted coordinator, 2026-10-12: "the process is
  failing here; ask the models"): a TUI worker refused an approved
  production deploy because the approval came only in the coordinator's
  task text; earlier workers accepted similar tasks (inconsistent). Round
  20261012-013415-4126 (sol, MiMo, DeepSeek agreeing), decided:
  1. Citing a TODO.md entry or a `herdr decision` id is not approval: both
     are agent-writable (`decision decide --source menu` is written by the
     coordinator itself, and so is "Next, in order"). The worker that
     refused was right.
  2. herdr captures the owner's AskUserQuestion answer itself (a hook
     records the exact question, options, the selected option, session,
     repository and time; append-only, not writable through agent
     commands) as `source: owner-click`; records written by agents are
     marked `agent-asserted` and never bear authority.
  3. `herdr agent prompt` / `worker start` attach a grant id minted only
     from an owner-click record, scoped to the repository, item, intended
     worker, target and the operations named in the clicked option's own
     words (never the coordinator's paraphrase); the worker checks it with
     `herdr grant show` (server state) and refuses a wider action.
  4. Still the owner's own word (in the worker's pane, or a herdr prompt
     to the owner showing the exact action): no or unverifiable grant;
     changed scope, target or consequences; irreversible steps outside
     the grant (image prune, data deletion, secrets, permissions); anything
     touching the trust machinery (grants, hooks, ledger, prompts).
  5. The todo skill and global rule say this; until grants exist, a
     coordinator tells the owner which worker pane waits for his yes.
  Open: whether the hook can tell a real click from a forged answer
  (state the threat model's floor).
  Clarified 2026-10-12 (the user: "I thought the coordinator gives the
  worker the permissions and access it needs; ask the models"; sol, MiMo,
  DeepSeek agreeing): it does, within the owner's approval. The
  coordinator delegates (picks the worker, the task, the subset of the
  approved access, narrowing, order) and requests the grant; herdr checks
  the request against the owner-click record, mints the grant and
  enforces it; the worker checks herdr, not the text. Only something new
  or wider (a new host, credential, path, destructive action) needs the
  owner's click. Example: "Rebuild and deploy <a friend's box>" lets the
  coordinator grant a worker "run deploy.sh on host X" with ssh to X, not
  ssh to host Y.

- [ ] The aarch64 musl build links again, and a partial build is never [t-i7xfozha]
  deployed (the user via the chat-hosted coordinator, 2026-10-12: "sloppy
  work; ask the models"). Manual build run 38183050127 on aa18b74f: x86_64
  musl linked, aarch64 musl failed: `sqlite3.c: undefined reference to
  'open64'`: since rusqlite `bundled` (863987ae, 2026-10-08) cc-rs compiles
  SQLite with the glibc cross gcc (`aarch64-linux-gnu-gcc`, also the
  linker), and musl 1.2.4+ has no LFS64 symbols; chat-hosted's deploy
  picked the latest successful run and kept deploying an old build.
  Round 20261012-011805-8b53 (sol, DeepSeek agreeing; MiMo refused the
  brief), decided:
  1. Both musl targets compile C and link with Zig (already in the job):
     wrapper scripts running `zig cc -target <arch>-linux-musl`, set as
     the target's CC (cc-rs) and Cargo linker (and AR if needed), in the
     fork's own workflows (build-artifacts-manual, fork-release);
     upstream's preview/release copies stay untouched. Rejected:
     `-D_LARGEFILE64_SOURCE` (hides the libc mismatch), dropping aarch64.
  2. A failed target fails the run (`fail-fast: false`, no
     continue-on-error); a publish step runs only when every target built.
  3. Every artifact carries its commit (`--build-commit`, the asset name
     or BUILD_INFO); consumers request an exact commit and target and fail
     when it is missing, never "latest successful" (chat-hosted's
     deploy.sh is that coordinator's; tell it).
  4. A link check of the real binary for both musl targets (no INTERP or
     NEEDED, a qemu-aarch64 smoke of `--build-commit`) on changes to
     Cargo.toml, Cargo.lock, build.rs or the workflows.

- [ ] The coordinator allowlist denies in shadow mode when the server [t-5sazb4ty]
  answers slowly (found 2026-10-11: a plain `VAR=...; cmd` was denied in
  the herdr coordinator's tab although the mode is shadow). The Claude
  hook's `ask_server` uses a 0.5 s socket timeout and treats any failure
  as "herdr cannot say", which denies; under load (verify runs) the
  answer can take longer. Fix: the hook knows the mode without a round
  trip that can time out (the server writes the mode into the integration
  state when it changes, or the hook reads the config), shadow and off never
  deny, and the refusal request waits for the server's answer (a timeout is
  a fixed delay deciding an outcome: Rule 10); `enforce` with herdr down
  still denies, as decided. Follows from t-svsyvp3s.
  The user, 2026-10-11: "a 0.5 s limit: what async workaround is this?
  ask the models". Round 20261011-151651-5aeb (sol, MiMo, DeepSeek,
  agreeing): the 0.5 s `settimeout` (9 places in the Claude hook, inherited
  from upstream's first integration) is a forbidden workaround where it
  decides an outcome (shadow becomes deny; reports are silently lost) and an
  unmeasured magic number where it only bounds a wait. Decided, in order:
  1. The hook reads the allowlist mode from a local file herdr publishes
     atomically when the config changes; shadow and off never deny; enforce
     with herdr unavailable says "herdr unavailable", not a policy refusal.
  2. Turn and state reports (Stop, UserPromptSubmit, ...) are appended to a
     durable local spool (idempotent ids, bounded size) before any send;
     the server drains and acknowledges them and reconciles pane state on
     start; the hook's wait ends on the local append, never on `recv`.
     Addresses lost Stops behind stuck `working` without proving it.
  3. Decision calls (allowlist, stop check, gate) end on the answer, EOF or
     a connect error (ECONNREFUSED/ENOENT are immediate); Claude Code's own
     10 s hook deadline is the only external bound; an unavailable server
     maps to each gate's documented fallback.
  4. Remove `settimeout(0.5)`; measure connect/answer latency instead of
     guessing bounds.
  Points 1, 3 and 4 landed 2026-10-11 (b5b7ae75): the Claude hook reads the
  allowlist mode from `<socket>.allowlist-mode` (published atomically) or the
  answer; shadow and off never deny; enforce without herdr says "Herdr
  unavailable"; no `settimeout(0.5)`: decisions wait for the answer, EOF or a
  connect error, reports end once sent; latency logged to
  claude-hook-calls.jsonl. Left: point 2, the durable report spool.

- [ ] Slim TODO.md into a compact index and merge overlapping items (the [t-geyiws7o]
  user, 2026-10-12, d-llnxblhd: after a TODO analysis with the models,
  round of 2026-10-12, sol, MiMo, DeepSeek agreeing on the structure).
  Each open item keeps: its title and id, the approved outcome, who asked
  and when (one line), depends-on, done-when, the next slice, and open
  points; everything else (incident narratives, round transcripts and
  per-model votes, closed slices, landed commits) moves to DECISIONS.md
  under a heading with the item's id, and the item links to it. Merge,
  keeping every distinct requirement and both ids: supervision ("A
  supervision report over all coordinators", "Supervision delivers its
  findings", "A pane worker's verdict was never recorded"); worker waits
  ("Event-driven worker waits", the `agent wait --timeout` failure,
  "Waiting for a pane worker's final report", the flickering detected
  state); the stop check ("counts every remaining item" and "asks to
  renew a question raised in the same turn"). Close `todo run serves a
  coordinator that owns two repositories` [t-o73wcaoq] as superseded by
  the user's one-coordinator-per-repository decision (d-33kat4ps). Verify
  before closing anything else: the models proposed closing items whose
  decided work remains (the README line of the naming item, the TODO
  tracker plan). Edit only with scripts/todo_edit.py; one commit per
  section; every other item and heading unchanged in meaning.

- [ ] `herdr todo run` for repositories unlike herdr (rormpc, 2026-10-11, [t-oe3wrkvz]
  reports r-8, r-9): (1) the TODO step needs `scripts/todo_edit.py` in the
  repository; use herdr's own copy (a `herdr todo edit` command) so any
  repository works; (2) one coordinator owns rormpc and rormpc-tools (items
  live in rormpc's TODO.md, rormpc-tools uses `main`): a run whose item is
  in one repository and whose worker works in another, and a configurable
  main branch per repository (`.herdr/`); items spanning both (a tools
  commit, a release tag, a rormpc commit) need ordered steps across
  repositories.
  Revised 2026-10-12 (the user rejected the override stopgap: "what a
  crappy workaround", asked for a proper fix; round 20261012-010700, sol,
  MiMo, DeepSeek agreeing): moved to the front. Slice 1 now: the driver
  reads the target repository's default branch from git everywhere it
  says `master` (base, cherry-pick target, `git cherry`, ls-remote, fetch,
  push), tested with `main`, a detached HEAD, no remote and a stale
  origin/HEAD. Then, by the user's own rule (one space and one coordinator
  per git repository), rormpc-tools gets its own space, coordinator and
  TODO.md (the rormpc coordinator and the user decide; rormpc's AGENTS.md
  says todo-rormpc owns both today); `--repo` is deferred (all three: it
  makes one coordinator drive another repository, the boundary the user
  rejected). Cross-repository work becomes items in two backlogs with a
  dependency: a handover that names the landed commit or version and
  wakes the dependent coordinator (its own item); release recipes come
  before the release item.
  Revised again 2026-10-12: no herdr handover between coordinators (the
  user rejected it); cross-repository work is plain messages between the
  repositories' own coordinators, see the item on waiting on another
  coordinator.

- [ ] Releases run through herdr's driver, portable and without allow rules [t-5bunzos6]
  (the user, 2026-10-12, on rormpc-tools' release script: "it must work
  when someone installs it at their place; ask the models"; d-r7pimalx;
  round of 2026-10-12, sol, MiMo, DeepSeek agreeing). No allow rule, user
  or committed project one: both trust a file agents can edit (the script,
  its tests, the settings themselves). Instead a release registered in
  `.herdr/checks.toml` (argv such as `scripts/release.sh X.Y.Z`): the
  driver, with the owner's grant bound to the approved commit and the
  script's blob hash, makes a clean detached checkout of that commit,
  runs verification there without publish credentials, then itself
  creates the annotated tag and pushes `<sha>:<default branch>` and that
  tag (no force), re-checking that the tag is absent and the push is a
  fast-forward; the agent never pushes. Until then the owner approves each
  release by hand. The same hole applies to the existing path allow rules
  (Porkbun, the VPS check): review them under the classifier plan.

- [ ] A TODO item can wait on another repository's coordinator, visibly [t-7iuvhmyd]
  (the user, 2026-10-12, rejecting a herdr-managed cross-repository
  handover: "I don't like this; there is supposed to be a coordinator per
  git repo; ask the models"; round of 2026-10-12, sol, MiMo, DeepSeek
  agreeing). Each coordinator owns only its repository; cross-repository
  work starts as a plain message in the requester's words, the receiver
  decides, queues it in its own TODO and replies "landed <tag or sha>";
  releases use version pins as the contract (prefer tags). herdr tracks no
  cross-repository dependency, commit or wake-on-land. What herdr
  provides: messages between coordinators are durable and visible to the
  user, and a `Waiting on <coordinator>` state for an item, distinct from
  "Needs a decision", that the todo tooling and the sidebar show so a
  waiting item does not rot; the todo skill describes the convention.

- [ ] A running todo run shows in its coordinator's space (the user, [t-rht4pyff]
  2026-10-11, looking at the herdr space during an install: "why is
  nothing happening here? ask the models, fix the process"). The driver was
  in `install` (~10 min) writing only to
  ~/.local/state/herdr/workers/run-<id>-install.log; the worker had exited;
  the coordinator pane showed only `working` and `bg:1` (its own wait).
  Round 20261011-172445-5a66 (sol, MiMo, DeepSeek), decided:
  - The driver publishes its run state from its own step transitions (run
    id, item, step, the command a step runs while it runs, step start time,
    log path, outcome) through the API; no parsing of logs.
  - One line under the coordinator's space per active run: `run r-x ·
    install · just clean-install · 4m`, selectable: it opens a read-only view
    of that step's log (not a fake PTY tab, no tab per step); done or failed
    shows until acknowledged. The Supervision tab mirrors the same rows.
  - No staleness flag from log mtime or heartbeat (MiMo, DeepSeek proposed;
    dismissed under Rule 10); elapsed time is shown, not judged.
  - The coordinator's turn-end message names what runs and what wakes it
    ("driver installing r-x; my wait on todo wait r-x"), per the
    limited-trust rule.
  Same for herdr-job waits (the user via the odherdr coordinator,
  2026-10-11: "why is nothing happening?" while its 10-20 min job ran in
  another tab; round 20261011-173444-d8b9, sol + MiMo + DeepSeek),
  decided: (b) the coordinator's tab shows a distinct `waiting: <job>` state
  while a wait edge exists, never the idle/done dot; (a) under its space
  `↳ job <name> · running · since 17:30 · last output 17:33` linking to the
  read-only log; absolute start and last-output times (no ticking clock);
  a structured step only when the job declares one (`herdr-job run --step`
  markers), never the raw last log line (ANSI, noise, secrets); log output
  proves output, not progress; one log watcher per job shared by every
  waiter, coalesced into the existing redraw; Supervision aggregates.
  Slice 1 landed 2026-10-11 (3f1a4f38): running todo runs show under the
  coordinator's space with step, command and time (a click opens the step
  log: `todo.open_log`, `herdr todo log`); waiting tabs show `⧖ waiting:
  <job|run>` with a `↳ job` row of absolute times, fed by driver step
  activity, `todo.wait` waiter edges and the herdr-job/bg-badge tokens. Left:
  Supervision mirrors the lines; a finished or failed run stays until
  acknowledged; the coordinator's turn-end message; `herdr-job run --step`
  markers; pi has no wait edges (no Stop hook); the step start is unknown
  after a server restart until the next step; a render-scale bench.

- [ ] A supervision report over all coordinators and their workers (user, [t-7zfjbcrf]
  2026-10-11, next: "add a process that every so often checks all
  coordinators, whether they work and whether their workers work, reports
  it to the herdr coordinator, records it somewhere; ask the models").
  Round 20261011-124220-adc4 (sol at medium, MiMo, DeepSeek, agreeing),
  decided by the coordinator:
  - Events first, plus a read-only reconciliation sweep every 5 minutes in
    the server (a derived view for humans, deduplicated; it never completes
    a wait, retries, prompts, kills, restarts, clears a state or marks work
    done; nothing waits on it, so it is not a timer workaround).
  - Findings name a positive fact and the expected event: coordinator idle
    with runnable items, no open decision and no registered wait; a pane
    `working` after a matching Stop with no new `UserPromptSubmit`; a prompt
    "accepted" without its acknowledgement; a run waiting on a step whose
    dependency already ended; a coordinator waiting on an untracked shell
    (`background_tasks` without a wait edge); a recorded resource failure
    (disk full, the 15 GiB guard). Quiet output or low CPU is only
    "cannot establish progress", never a diagnosis (no silence threshold).
  - Stored append-only in the server's SQLite (`supervision_findings`: id,
    fingerprint, repo, coordinator tenure, pane/run/job ids, predicate,
    severity, expected event, evidence (event ids, timestamps), first/last
    seen, status open/acknowledged/resolved, resolution evidence), not in
    the decision ledger; `supervision.finding` events.
  - Consumers: the user's `?` list and one notification per new or
    escalated finding; the herdr coordinator reads them through a durable
    inbox (`herdr supervise --report`, its inbox on wake), never as the
    only consumer, since it can be the one that stalled.
  - Slice 1: the sweep with the coordinator-idle, pane-working-after-Stop,
    untracked-shell and disk predicates, the table, `herdr supervise
    --report`, the `?` entries. Never acts.
  Slice 1 landed 2026-10-11 (f12a83b1): the sweep (5-minute timer plus
  coordinator turn and state events), the four predicates, the append-only
  `supervision_findings` with fingerprint dedupe and `supervision.finding`
  events, one notification and one `?` entry per new or escalated finding,
  `herdr supervise --report [--all] [--json]` (`supervise.report`). Open
  points, decided by the coordinator 2026-10-11: disk is a warning below the
  15 GiB guard and critical below 5 GiB (a cold `target/` build no longer
  fits); slice 2: (a) read herdr-job's wait edges (e3228f41) so a wait on
  another pane's job is not "untracked"; (b) only the user or the herdr
  coordinator (its tenure) acknowledges a finding, never a worker or another
  agent's `worker answer`; (c) findings reach the herdr coordinator's
  durable inbox (the user asked that it reports to the herdr coordinator),
  as reports do, besides the user's `?` list.
  First live report (2026-10-11 14:24, after bea6de4b): every coordinator
  line says "cannot establish progress", also the herdr one, which was
  `working` with an active run, and idle ones with no runnable item; the
  label should name what is known instead (working with run X, waiting on
  the user's decision d-..., idle with nothing runnable, waiting on job Y),
  and keep "cannot establish progress" for the case with no such fact.
  The user, 2026-10-11: "the supervisor is not an agent, it is a
  deterministic loop": server code only, no model call anywhere in the
  sweep, the predicates or the delivery.
  The loop (the user, 2026-10-11: "a deterministic supervisor, not an
  agent, reports to the herdr-space coordinator, which after consulting
  the models fixes how the coordinators, the workers or the supervisor
  work; ask the models"; round 20261011-152049-35e4, sol + MiMo +
  DeepSeek), decided by the coordinator:
  - Findings flow automatically (no hand relay): each carries a stable id,
    fingerprint, repo, session/pane/run/job ids, the violated check and its
    version, expected vs observed, recurrence count, evidence references
    (event ids, transcript paths and bounded excerpts) and a suspected
    locus: herdr (shared orchestration, rules, the skill, the supervisor),
    the repository (its code, tests, config: routed to that repository's
    coordinator, not herdr's) or needs-the-user (parked with options).
  - Each finding is tracked to its end: triaged, fix queued, verified,
    installed, and resolved only when the failure stops recurring (the
    supervisor checks the same predicate afterwards).
  - The herdr coordinator is watched by the same event predicates; a finding
    about it goes straight to the user (`?` and a notification), never
    only into its own queue. No heartbeat or liveness timer (the models
    proposed them; dismissed under Rule 10).
  - Self-modification guard: a change that removes or weakens a supervisor
    check or a coordinator rule needs the user's approval (one menu);
    every check change ships with a regression case from the incident
    that motivated it (a growing incident corpus). Adding checks or
    tightening needs no approval.
  - Dedupe by fingerprint, aggregate repeats, one notification per new
    or escalated finding.
  Handover to a new build (the user, 2026-10-11: "when there is a new
  supervisor, a new build, must its work be handed over? ask the models";
  round 20261011-153655-05f9, sol + MiMo + DeepSeek agreeing), decided:
  hand over durable obligations, not the running loop.
  - Durable first: assignments, verdicts, waits and the other transitions
    the checks read are written to SQLite before their producer is
    acknowledged (producers resend unacknowledged ones with stable ids);
    notification and inbox deliveries go through an outbox keyed by
    (finding, epoch); in-memory triggers and the sweep schedule are not
    handed over, they are rebuilt.
  - The new server runs one reconciliation sweep over assignments, waits,
    verdicts and open findings, ordered by durable event id from the last
    processed cursor, before it serves; waiters replay from their cursor;
    `server_handed_off` is a reconnect signal, not proof of delivery.
  - Single writer: the old supervisor stops intake and writes before the
    new one writes; every supervisor write carries the server's epoch and
    a stale epoch is rejected (the run lock alone is not enough).
  - Checks carry an id and version; open findings are re-evaluated under
    the new version off the handoff path; a removed or changed check closes
    its findings as `check_changed`/`check_removed` (distinct from
    resolved), never dropped.
  - Test: a verdict arriving at every handoff boundary, and a crash between
    the old server stopping and the new one listening.
  Labels and a race fix landed 2026-10-11 (bdd3a8f7): coordinator lines name
  what is known (working on run X, waiting on decision d-..., on job Y,
  idle with nothing runnable, paused queue; "cannot establish progress" only
  without such a fact); the held-output report counts a holder only when the
  worker's own reads bracket its hold (kernel pipe addresses are reused),
  40/40 under load. Found: `stop` gets EPERM from killpg on a macOS process
  group holding only zombies; queued as its own item.

- [ ] Turns reach interactive workers only through `herdr agent prompt` [t-2kjq5txq]
  (the user via the guix-x86_64-iso-omguix coordinator, 2026-10-11:
  "it looks like nothing happens here; report it to the herdr
  coordinator; fix the process or the supervisor; ask the models"). The
  coordinator sent ~900 chars with `pane send-text` then `send-keys Enter`;
  Claude collapsed it to "[Pasted text #1]" and Enter did not submit (a
  later `enter` did); its `agent wait --until working` waited ~10 min.
  Also `agent start --name` returned "timed out waiting for agent startup"
  each time although the agent started (the CLI polls to a deadline,
  src/cli/agent.rs `wait_for_named_agent`), then `agent wait <name>` said
  agent_not_found; long prompts fail `invalid_agent_argument`. Round
  20261011-153541-4550 (sol, MiMo, DeepSeek), decided, in order:
  1. `pane send-text`/`send-keys` to a pane with a detected agent refuse
     and name `herdr agent prompt` (`--raw` keeps a diagnostic path); the
     coordinator allowlist refuses them in coordinator tabs too.
  2. `agent start` registers the name before spawning and ends on that
     launch's SessionStart report correlated with the name, or on a spawn,
     exit or hook error; no deadline decides; find why the name did not
     resolve afterwards (rollback on the timeout path, a key mismatch).
  3. Long prompts: a literal file/stdin input (`--prompt-file`) that herdr
     types itself, then structured serialization instead of shell
     interpolation; never a length-based refusal.
  4. Trace `Enter` vs `enter` bytes and the bracketed-paste end vs the
     Enter (the later `enter` may simply have arrived later); confirm
     `agent prompt` sequences paste end before Enter.
  Gap (guix, 2026-10-11): `herdr agent start --name ... -- "<prompt>"`
  gives the first task without a request id, so the coordinator cannot use
  `wait-agent --request <id> --until verdict` on it; `agent start` with a
  prompt returns the prompt's request id (tracked like `agent prompt`).

- [ ] A Supervision tab pinned at the top of the sidebar (user, [t-76czwuj7]
  2026-10-11: "the coordinator investigator gets its own tab, always at the
  very top, fixed; ask the models"; round 20261011-143956-eed7, sol + MiMo
  + DeepSeek agreeing), decided by the coordinator:
  - A pinned read-only view (rendered from the server's `supervise.report`
    and live `supervision.finding` events, like the worker tab), not a
    space and not an agent; labelled "Supervision".
  - Its row sits above the spaces, outside their scroll, never dragged,
    closed, filtered, sorted or folded; one line: the worst unresolved
    severity glyph and counts by severity in fixed widths (no jitter on
    each sweep), not the header's `?`; "no findings" is not "healthy"; a
    stale or unavailable sweep shows as such.
  - Its main view: the last sweep's time, the per-coordinator table (repo,
    state, progress label, run, workers, pane link), findings with their
    evidence; "Open coordinator pane" focuses the existing tab.
  - Config `[supervision] show_tab = true` (positive name, default on).
  - Server: a snapshot on subscribe plus delta events with sequence numbers
    (add only what is missing); the client never polls. One triage state:
    the `?` entries link to these findings, not a second inbox.
  - Slice 1: the row, the view, freshness, Open pane; acknowledge later.
  Slice 1 landed 2026-10-11 (ad7741f9): the pinned Supervision row (worst
  glyph, fixed-width counts, stale/unavailable/no-sweep states) and its
  read-only view (sweep time, coordinators with their run step and an open-pane
  link, findings with evidence), `?` entries linking to it, `[supervision]
  show_tab`; data in the shell snapshot, no polling. Left: the mobile layout
  and the collapsed sidebar; acknowledging from the view.

- [ ] A coordinator never waits on an untracked shell (user via the violai [t-7jdg43la]
  coordinator, 2026-10-11: "nothing moved all night; report it to the
  herdr coordinator, fix the process with the models"). The violai
  coordinator chained cherry-pick, checks, the full suite, cleanup, the
  TODO commit and the next item in one Bash call; it passed Claude Code's
  Bash limit and became Claude's own background shell (no herdr-job, no
  sidebar job); a render step hung 12 h; the turn ended waiting on it and
  nothing woke it. Round 20261011-113649-cced (sol, MiMo), decided by the
  coordinator:
  1. The Stop check (coordinator tabs) blocks once when the Stop payload's
     `background_tasks` holds a shell that is not a herdr-job wait (no wait
     edge): the message names the shell and says to adopt it, not relaunch
     it (wait on it with `herdr-job watch --pid <its pid>` and check its
     result; a fresh run beside it races on the same worktree).
  2. The `◔` tooltip and the status legend say "waits on an untracked
     shell" for such a shell, so the user sees it while away.
  3. The `todo` skill: checks, tests and builds run only through `herdr
     todo run` or `herdr-job run`, each step its own call (cherry-pick,
     check, cleanup, commit, next item), never one chained Bash call.
  4. Onboarding a repository to `herdr todo run` refuses to start without
     registered checks in `.herdr/checks.toml`; the violai coordinator is
     told to register its checks (its own item, in its repository).
  5. The coordinator allowlist stays in shadow; read its would-deny log
     (`herdr history overrides --would-deny`) before enforcing; when
     enforced it must judge the whole chained invocation (sol).
  The 12 h hang itself is a violai bug queued there; no timeout here.

- [ ] `todo resume --note` takes `--next`/`--stop-reason` like `--close` [t-dgtlrinl]
  (points 4-5 of the closed t-pku6dsb6, 2026-10-10/11: approving a run
  with a note cannot chain the next item, so the coordinator starts it by
  hand, the stall the bridge removed for `--close`; it happened four times
  on 2026-10-11). Point 4 stays: queue mode (`herdr todo queue on`) once
  the question gate and the enforced stop check ran clean for a week
  (from 2026-10-11).
  First point landed 2026-10-11 (be086b38): `todo resume --note` takes
  `--next`/`--stop-reason` like `--close` (a note alone stays valid).
  Left: point 4, queue mode after a clean week (from 2026-10-11).
  The same commit fixes a race verify found in 4fffb6e0: the held-output
  report is recorded once its tool call's `tool_use` line is read (or just
  before the turn's `result` when none comes), so `tool_calls` is never
  empty by ordering; 40/40 under a stress loop.
  Gap found 2026-10-11: `--note --next <the same item>` is refused with
  "--next names the item this run closes", but a note keeps the item
  open, so a multi-step item (the mod.rs split) cannot chain its own next
  step; allow the same item after `--note` (never after `--close`).
  Gap fixed 2026-10-11 (1617a59e): `--note --next` may name the run's
  own item; `--close` still refuses it. Left: queue mode after a clean week.

- [ ] Measure whether coordinator/worker pays off (from the devil's [t-2p6nfwpg]
  advocate round 20261010-032145-a9e1, recorded in DECISIONS.md
  "Coordinator/worker versus a single agent"; the user kept the queue as
  is). Conclusions to keep in view: the real value today is the
  server-side `verify` gate, provenance trailers and worktree isolation;
  about two thirds of the driver's items fixed the machinery itself; the
  sandbox sends the riskiest work (VMs) to unsandboxed pane workers;
  review by the same model is only partly independent; parallelism is
  unused; the protocol, Odin, crate and tracker plans were called scope
  creep for one user. Proposed measurement: for the next 10 product (not
  machinery) items, the user's hands-on minutes per useful landed item,
  alternating the driver and a single agent gated by the same `verify`;
  the coordinator records per item: the user's interventions, review
  rounds, machinery fixes it triggered, wall-clock time.
  Approved by the user 2026-10-10 ("zgoda"). How it runs, decided by the
  coordinator: it applies to the next 10 product items (not coordination
  machinery) in "Next"; odd ones through `herdr todo run`, even ones done by
  one Claude agent working the item directly in a worktree (the
  coordinator itself, which this approval allows for those items only),
  landed only after the same `herdr worker verify`-style checks (one
  commit, subject, paths, `just check`). Per item the coordinator appends
  to this item: the user's interventions (count, minutes the user reports
  if he does), review rounds, machinery fixes it triggered, wall-clock time
  from start to push. After 10 items: a summary and a question to the user.

- [ ] Worker capabilities in the coordination protocol, and a prepare step [t-ih3cmtjf]
  (user, 2026-10-09: "the coordinator-worker model is meant to become a
  protocol with an implementation in Odin etc., not hardcoded crates.io";
  consult round 20261009-190333-40fb, sol + MiMo + DeepSeek agreeing;
  decided by the user: "prepare + capabilities"). The coordination layer
  names no registry, language or tool:
  - Protocol (language-neutral, versioned): capabilities `net.egress{hosts}`,
    `fs.write{paths}`, `env{names}`, `exec{argv}`; a repository or item
    requests, the user's policy grants, the worker kind's adapter enforces;
    an unknown or unsupported capability is refused (fail closed); every
    run records its effective grants.
  - Repository (`.herdr/`), read by the driver from the run's base commit,
    never from the worker's tree: `[prepare]` argv (here `cargo fetch
    --locked`) with the capabilities it needs (here egress to crates.io and
    static.crates.io). A repository diff that changes requested
    capabilities is a question to the user, never self-granted.
  - Driver: prepare runs in its own sandbox (only the granted egress, no
    secrets, writes only the dependency cache); the worker stays offline
    and builds from that cache.
  - Worker kinds map grants to their own sandbox (Claude Code: its
    `sandbox` settings); a pi or Odin worker implements the same contract.
  Belongs with the item "Extract the coordination layer into its own crate";
  do this slice first, in today's code, as the crate's first protocol piece.
  Second case (try-roguix coordinator, 2026-10-10, headless worker `w78`,
  item t-w2soppgg, WORKER-BLOCKED): the worker sandbox stops that
  repository's real work: launching a test VM (codesign
  CSSMERR_TP_NOT_TRUSTED for its ad-hoc or locally signed QEMU/app), `ps`,
  `herdr-job` outside a herdr pane, and `make test` (temp-dir writes, PTYs,
  local sockets). Only docs and research items run there today. The
  capability vocabulary must cover these as named, user-granted requests
  per repository (e.g. `exec.unsandboxed{argv}` for a listed launcher,
  `proc.list`, `pty`, `net.local`), and a worker kind that cannot enforce a
  grant refuses the run (fail closed) so the coordinator routes it to a
  pane worker instead of the worker blocking mid-task.
  Host operations, 2026-10-10 (user: "a VM launcher in herdr is hardcoding
  again, like crates.io; the protocol must be implementable in Odin"; round
  20261010-030301-b507, sol + MiMo + DeepSeek agreeing): `[prepare]`
  generalizes into repository-declared operations; the protocol knows only
  operation, parameters, capabilities, grant and result, never VMs, QEMU or
  cargo. Points every implementation must follow:
  - An operation is a separately confined job, not a way out of
    confinement: repository code (e.g. `make vm-test`) is untrusted; the
    user's grant (per repository, operation and definition hash at the base
    commit) is the trust decision; confinement only contains damage.
  - Definitions and the scripts they run come from an immutable base-commit
    checkout; the worker's changes reach an operation only as explicit,
    untrusted input (otherwise a worker rewrites the Makefile the grant
    covers).
  - Each capability has a precise scope inherited by descendants; `exec`
    means arbitrary code inside the granted confinement, not a narrowing.
  - An implementation advertises what it can enforce; anything it cannot
    enforce as specified is refused; results say denied, failed or
    unsupported, plus exit status, bounded output and a job-scoped log.
  - Jobs have ids, cancellation, resource budgets and descendant cleanup;
    repeat invocations are budgeted.
  - A conformance test per capability, including "unsupported must refuse".
  First slice: `prepare` as the one operation (no parameters, argv only, no
  PTY or local sockets). VM work is slice two: it needs PTYs and local
  sockets, the hardest to confine; if an implementation cannot confine
  them, VM items stay unavailable to headless workers there rather than
  falling back silently.
  Slice 1 landed 2026-10-10 (1b132ad4, run r-gyihgb6q): capability
  vocabulary v1; `.herdr/operations.toml` (`[prepare]` = `cargo fetch
  --locked`, egress to the crates.io hosts, writes to Cargo's cache) read
  from the base commit; server-side grants per repository, operation and
  definition hash (`herdr todo grant` / `todo.grants`); fail-closed
  `capability_unsupported` / `grant_required` refusals; prepare confined on
  macOS (`sandbox-exec`, egress through a CONNECT proxy allowing only the
  granted hosts on 443); grants recorded in the run's events. Since it
  landed, every `herdr todo run` here is refused with `grant_required` until
  the user runs `herdr todo grant --operation prepare --hash <hash>` in his
  own terminal; agents never run it. Remaining slices, ordered by the
  coordinator 2026-10-10 (the worker's list):
  1. The grant as a question in the user's `?` list instead of a preflight
     refusal, real protection so an agent cannot grant (e.g. the grant
     needs the user's client or a confirmation outside agent panes), and
     revoking a grant.
  2. Map `[worker]` grants to Claude's sandbox settings, so a headless
     worker enforces `net.egress` and `fs.write`.
  3. Restrict reads in a confined operation (today prepare can read secret
     files; environment variables are already filtered).
  4. Operation job ids, interruption, resource budgets, retry limits.
  5. Linux confinement (bwrap or landlock); refused today.
  6. Operations with parameters and untrusted input from the worker's
     tree; `pty` and `net.local` (the VM slice for try-roguix).
  7. Grants in `TodoRunInfo` so `todo status` shows them.
  8. Extracting the layer into its own crate (its own item).
  9. A conformance test per capability, incl. "unsupported must refuse"
     for every adapter.
  Remaining slices 1 and 7 landed 2026-10-11 (bb6e2a93): a run refused with
  `grant_required` lists a grant question in the user's `?` list,
  answerable only from the user's client (Grant records it if `master`
  still has that definition; Not now drops it until the next refusal);
  queue mode waits as `waiting_on_user` and starts the item on the grant;
  `herdr todo grant --revoke` / `todo.revoke` with an append-only
  `capability_grant_log`; a run not yet past its prepare is blocked after a
  revoke; grants show in `todo status` / `todo runs`. Left: slices 2-6, 8, 9.
  Input from the odherdr spike (2026-10-11: an Odin worker host reproduced
  herdr's grant hash and refusal order; recommendation: herdr's protocol is
  not yet ready to be an odherdr milestone). Its 13 open questions belong to
  this item's slices 4-9, to be answered in a written protocol spec before
  any second implementation: (1) a wire protocol between driver and adapter
  (transport, framing, method names, version negotiation); (2) which side
  checks the grant; (3) result names: denied/failed/unsupported vs
  grant_required/capability_unsupported, refusal as error or result; (4) an
  unknown capability: malformed request or unsupported; (5) what
  `exec{argv}` enforces (herdr counts it enforced but never compares argv);
  (6) whether the job's cwd is writable; (7) the ending signal and
  cancellation/budget reasons in a result; (8) whether the 40-line and
  4000-character log bounds and log retention are contract; (9) `net.egress`
  as HTTPS CONNECT through a local proxy to 443 (tools ignoring HTTPS_PROXY
  fail; other ports?); (10) killing descendants that `setsid` out of the
  group (a macOS group kill misses them); (11) whether the confinement
  baseline is mandatory (herdr refuses off macOS); (12) what reads a
  conforming host denies; (13) where the conformance suite lives. Source:
  ~/personal_projects/odin/odherdr/spike/herdr-worker/README.md, "What herdr
  leaves open".

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
  Moved to "Needs a decision" 2026-10-09 by the coordinator: the remaining
  live check needs an interactive Claude in a pane, which may hit a folder
  trust dialog that agents must not answer.
  Question: may a worker run the live check in a throwaway pane in an already trusted folder (the herdr checkout), or will you run it?
  Options: worker in the trusted herdr checkout, read-only prompts (Recommended) | I run it myself | drop the live check
  Decided by the user 2026-10-09: a worker runs the live check in the trusted
  herdr checkout (read-only prompts, a short background task), results into
  this item.

- [ ] `herdr agent wait <worker> --until ... --timeout 3600000` inside [t-jnema5dg]
  herdr-job failed with `Error: Custom { kind: Other, error: EmptyResponse }`
  after 4-5 minutes, four times, while the workers kept running (reported
  by the email-assistant coordinator, 2026-10-07). Long waits must survive;
  find where the socket returns an empty response (a server-side timeout?).
  Regression check 2026-10-10 (run r-6com5znp): CLI waits now reconnect
  across a live handoff (9c82b386) and the socket tests passed in
  `verify`; whether the 2026-10-07 failures were handoffs is unproven
  (installs ran that day), so this stays open until an `EmptyResponse`
  outside a handoff is seen or ruled out from the server log.

- [ ] Waiting for a pane worker's final report (reported by the try-roguix [t-audxade5]
  coordinator, pane w6:p87, 2026-10-10, at the user's request). It drives
  interactive Claude workers in panes and scrapes them. Its nine points,
  with what herdr has since (checked by the coordinator):
  1. No "worker finished its task" signal (idle is not finished): headless
     workers end their turn with `herdr worker wait --attention` and their
     `WORKER-DONE` in `last_result`; `herdr todo run` drives the whole item.
  2-3. Scraping false positives (its own prompt, the task example, old
     scrollback, `WORKER-DONE a b c |` not matching, a finished worker
     unnoticed for ~6 h): gone with headless workers (the final text is
     structured, not scraped); the driver's contract allows one sha.
  4. Waits end on a server restart: `herdr todo wait` reconnects (8eaec1f2);
     `pane wait-output`/`agent wait` still do not.
  5. `agent read --lines` fails while the agent works: open.
  6. A stuck worker is invisible (no output for 2.5 h): open; must be an
     observable fact, not a silence timer (the delay rule).
  7. Text the user typed into a worker's input box is lost when its tab or
     worktree closes, and a coordinator's prompt is appended to it: open.
  8. `worktree create` without `--cwd` uses the focused workspace's repo:
     open.
  9. `agent start` in a never-trusted folder returns `agent_not_ready`
     without naming the trust dialog: `agent prompt` names it since
     (`agent_prompt_blocked`); `agent start` should too.
  Proposed: move that coordinator to headless workers and `herdr todo run`
  (needs the driver to run in its repository: checks and install registered
  in its `.herdr/`), and fix 4 (pane waits), 5, 7, 8, 9 for pane workers.
  Approved by the user 2026-10-10 ("move it"): the try-roguix coordinator
  moves to headless workers and `herdr todo run` (its `.herdr/` checks and
  install registered in that repository), and 4, 5, 7, 8 and 9 are fixed for
  pane workers.
  Done 2026-10-10 by the herdr coordinator at the user's request: told the
  try-roguix coordinator (w6:p87) to run its workers headless from its next
  item (start, `worker wait --attention`, answer, ack, `verify` before a
  cherry-pick), and to propose registering its checks for `herdr todo run`
  to the user. Left here: the pane-worker fixes 4, 5, 7, 8, 9.
  Second report from the try-roguix coordinator (2026-10-10, pane w6:pAH,
  sent at the user's request; its round 20261010-025030-401e, sol + MiMo).
  It still needs pane (TUI) workers for VM items (the sandbox case). Seen:
  `herdr-job wait-agent <pane> --request <id>` exited 8 after a herdr
  restart while the worker kept working; the default wait fired on a
  transient idle (the worker woken by its own background-shell
  notification); `--until done|blocked` fired on a turn end while the
  worker's own background test ran; its stopgap (wait for working, then
  for done, re-armed per turn) can latch a stale state. Agreed there and
  taken into this item: the positive event is "this assignment produced a
  WORKER-DONE/WORKER-BLOCKED line, or the worker asks", not a pane state
  or a turn end; so `herdr-job wait-agent --until verdict`, scoped to the
  assignment (the prompt's request id), inspect-then-subscribe atomically
  (a verdict already in the transcript ends it at once), recovering after
  a herdr restart by re-reading the transcript instead of exiting 8; the
  `todo` skill then names that wait for pane workers and its exits; a
  worker-contract line "emit the WORKER line only after your own background
  jobs end" only as a complement.
  Third note from try-roguix (2026-10-10): `wait-agent --until done` exits 0
  both on a verdict and on a bare turn end, so the exit code cannot tell them
  apart; the verdict mode gets its own exit code.
  Third occurrence 2026-10-10 03:12 (try-roguix, pane worker w5Q:p1): the
  worker ended its turn while its own background test job ran;
  `wait-agent --until done --until blocked` fired without a WORKER line
  (also 02:43 and 03:11 in the same item).
  Fourth report (guix-x86_64-iso-omguix coordinator, 2026-10-10): a pane
  worker ended its turn "done" while its own background herdr-job wait ran;
  `agent wait --until done|blocked|idle` returned at once. Round
  20261010-152938-328c (sol, MiMo, DeepSeek): do not redefine `done` from
  the `bg:N` count (it is stale until the next Stop and can stay above
  zero forever); the verdict wait of this item is the answer. Added here:
  `agent status` and `wait-agent` output show the last Stop payload's
  background tasks as information; the worker contract says "write your
  WORKER line only after your own background jobs ended"; an optional
  `--until background-drained` ends on a Stop payload of the same session
  listing no running task (MiMo, DeepSeek).
  Addendum from guix-x86_64-iso-omguix (2026-10-10): `herdr agent wait w6S:p1
  --until done --until blocked --until idle --global` inside herdr-job
  20261010-155722-59b1 failed at once with `EmptyResponse` (exit 1) while
  the agent was working. Checked by the coordinator: it coincides with the
  herdr coordinator's own install (run `r-6fotwqpt`, live handoff complete
  at 15:57:50). Same class as t-bojbiegs ("a live handoff breaks other
  sessions' waits"); `herdr todo wait` reconnects since 8eaec1f2, but
  `agent wait`, `pane wait-output` and `wait-agent` do not: they must
  reconnect to the new server and continue waiting on the same target.
  Addendum 2 (2026-10-10): again, herdr-job 20261010-171947-6460 (`agent
  wait w7E:p1 ... --global`, the pane working) ended at 17:27:56 with
  `EmptyResponse` after 8 min of waiting, the second the live handoff of the
  install of run `r-fle3tkul` completed (server log: "live handoff
  completed; old server exiting" at 17:27:56). Every install of this queue
  kills other sessions' `agent wait`; raises the priority of this fix.
  Landed 2026-10-11 (d05883cc): `herdr-job wait-agent <pane> --request <id>
  --until verdict` ends only on the assignment's WORKER-DONE/WORKER-BLOCKED
  or a question (inspect-then-subscribe through the new `agent.wait_change`,
  survives restarts through the recorded assignment); exit codes: done,
  blocked, question, 3 agent exited without a verdict, 7 WORKER-DONE names a
  missing commit; a bare turn end never ends it. `wait-agent` on a worker
  pane without `--request` refuses and names the verdict form. Point 9 done:
  `agent_not_ready` names the startup dialog and its pane. Left: points 5,
  6, 7, 8; the `todo` skill naming this wait for pane workers (a text edit
  of the user's skill, done by the coordinator's worker next);
  `--until background-drained` (optional, from the fourth report).

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
  Step 1, first module landed 2026-10-11 (6cb6395c): `status.rs` (Status,
  questions, Direction, BrokerRecord, their tests); `apply` was already
  pure, an effects-returning `apply` stays a follow-up. Next: `journal.rs`,
  then `live.rs`/`process.rs`, `registry.rs`, `types.rs`, `supervisor/`.
  Step 1, second module landed 2026-10-11 (1ad6614a): `journal.rs`
  (Journal, read/parse, gap, replay, import, owner lock). Next:
  `live.rs`/`process.rs`, `registry.rs`, `types.rs`, `supervisor/`.
  Step 1, third module landed 2026-10-11 (a371baba): `live.rs` (Input,
  Live, Spooled, spawn_worker, Reader). Next: `registry.rs`, `types.rs`,
  `supervisor/`.
  Step 1, fourth commit landed 2026-10-11 (e53b12b3): `registry.rs`,
  `error.rs` (WorkerError), `types.rs`. Next: `supervisor/` by verb.
  Step 1, fifth commit (23f4d201, supervisor lifecycle/questions) failed
  the install's `just check` on a race from ce9d68c0: an owner-asked exit
  advanced `acked_seq`. Fixed 2026-10-11 (69ad3474): a persisted
  `end_by_owner` flag keeps such an exit obligation-free without advancing
  the ack; the test waits for the exit event; 40/40 under stress. Next:
  takeover and views.
  Step 1 done 2026-10-11 (66bbd4f8): `src/workers/mod.rs` is 688 lines;
  `WorkerSupervisor` lives in `supervisor/{lifecycle,questions,commands,
  handoff,takeover,views}.rs` (340-922 lines each). Still large:
  `runs.rs` 3987, `store.rs` 3813, `tests.rs` 12786 (split later the same
  way if step 2 needs it). Next: step 2, the per-module review with the
  models against T3 Code.

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
  Landed 2026-10-11 (da36cd4e): Claude and pi panes get a "restart
  pending" mark when their CLI's `--version` changes (checked on agent
  start and on watched launcher changes, no timer): `↻` on the tab, a
  status legend entry, optional `restart_pending` in `agent.get`/`list`.
  Left: the menu's preview/picker counts and the idle checks (no pi draft,
  no subagents, no background tasks) in `plugins/restart`, which can now read
  `restart_pending`.

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

- [ ] Draft an upstream issue on Claude Code's shared sandbox `$TMPDIR` [t-uilmqtpb]
  (decided by the user 2026-10-09: "prepare a draft"): a worker writes the
  issue text to `docs/upstream/claude-code-sandbox-tmpdir.md` (current vs
  expected behavior, a minimal reproduction, version 2.1.293+, herdr's
  workaround of a private 0700 temp dir, `mktemp -d` ignoring `$TMPDIR` in
  a vendored script); the user reads and files it himself. Nothing is
  posted by an agent.

- [ ] The footer showed "OR $0.39 balance" after a $10 top-up (user, [t-mwwgqlah]
  2026-10-09: "why does the OR usage widget show $0.39 although I already
  topped up $10; add to TODO, ask the models"). Checked by the coordinator:
  the formula is right (`/credits` total minus usage); `usage.read` returned
  $10.39 a few minutes later. Cause: the server polls every 300 s and the
  footer shows no age, so a fresh top-up looks lost. Consult round
  20261009-233730-c86a (sol, MiMo, DeepSeek): all three: show the reading's
  age in the footer row (e.g. `OR $10.39 · 4m`) and offer a manual refresh
  through the existing rate-limited `usage.read --refresh` path, never a
  shorter interval or a refresh on focus or timers. Diverged on where:
  sol: a Refresh button in the details popup (the row's click keeps
  opening details); MiMo, DeepSeek: clicking the row refreshes. Decided by
  the coordinator: the age in the row and a Refresh action in the details
  popup, with feedback (in flight, done "checked just now", or "refreshed
  20s ago, next in 40s" when the minimum gap refuses). A failed refresh
  keeps the last successful reading's time; label a `limit_remaining`
  fallback as the key's limit, not the account balance.

- [ ] Consult helpers: clearer refusals for two caller mistakes (reported [t-65oyloai]
  by the rormpc coordinator, 2026-10-09). (1) `ask_gpt.sh` reads stdin only
  with `-f -` (on purpose: a background job can inherit a stdin that never
  closes), but a piped brief without it just prints "Empty prompt"; when the
  prompt is empty and stdin is not a TTY, say "stdin is read only with
  -f -". (2) `ask_openrouter.sh -f <file>` refuses a file outside a git
  repository (deliberate vetting), while agents keep drafts in their
  scratchpad: name the way out in the refusal (pass the brief inline or via
  `-f -`) and say it in the consult and openrouter SKILL.md files.
  Approved by the user 2026-10-09 ("zgoda jest").

- [ ] A state for "a question waits for you, and work goes on" (user, [t-exksqhhc]
  2026-10-11, screenshot of try-roguix: `?` with `bg:1` while its worker
  built: "but a new state is also needed: there is a question, but the
  agent is still working or has delegated something and work goes on").
  Since f2c56a2e an open question persists while the agent works, and the
  `?` mark and the working/`◔` marks compete for one glyph. Want one
  combined mark (both facts visible at once: an open question for the user
  and the agent working, or its own background work or worker running),
  with the status legend entry and the tooltip naming both. Same item:
  the screenshot's tab line showed an answered question first ("Zwolnisz
  19 GiB?", the space was freed): check why it stayed (asked before the
  install of f2c56a2e, or the submit path did not clear it) and fix it;
  the line shows the newest open question.

- [ ] Wake a coordinator when an external reply arrives (the owner via the [t-22gwwmer]
  roguix-vps coordinator, 2026-10-11: "why is nothing happening here?
  report it to the herdr coordinator"). roguix-vps waits on RackNerd
  ticket MJ96115 answered by mail; four times in ~18 h the session sat
  idle until the owner noticed the mail himself (one question sat 1.5 h).
  No allowed way to wait exists: `herdr-job watch` waits on a pid,
  `awaiting-reply` waits on the user, and a session's own poll loop is
  forbidden. Want: a herdr-job wait on an external condition that shows in
  the sidebar and re-invokes the session when it fires, e.g. IMAP IDLE
  through himalaya if it supports it (a push event), else `herdr-job watch
  --until '<command exits 0>' --every <interval>` labelled `delay: external
  polling` (mail has no push here; Rule 10 allows external polling); and
  a pane mark "waiting on someone outside" with the condition named, so
  the owner sees why it is idle. Decide the shape with the models when
  picked up.

- [ ] A check that TODO.md, DECISIONS.md and decision answers are in [t-kshbojvf]
  English (the user via the guix coordinator, 2026-10-11: "did you add
  Polish text to TODO.md? report it to the herdr coordinator"; it had
  quoted the user verbatim 24 times; the rules now say to translate,
  dotfiles rule fix pending review). `scripts/todo_edit.py` refuses text
  with Polish diacritics (ąćęłńóśźż) or a short list of common Polish
  words, naming the line; `herdr decision add|decide` does the same for its
  statement and answer; a test for each.

- [ ] A space shows its open owner steps and external waits (the owner via [t-rgvdor7o]
  the roguix-vps coordinator, 2026-10-11: "why does this space not show
  that there is work? report it to the herdr coordinator; ask the
  models"). roguix-vps's real work is owner steps (run commands on the VPS,
  a site returning 500, cert renewal) and a provider's ticket, kept only as
  prose under "Waiting on others" in its docs/TODO.md; herdr sees none of
  it, so the space looks finished. Round 20261011-160537-9169 (sol, MiMo,
  DeepSeek agreeing), decided:
  - First-class server state in the decision ledger with kinds `question`,
    `owner_step`, `external_wait`: who it waits on, a completion condition,
    evidence, created time; one entry per deliverable ("renew certs"), not
    per command; TODO.md links to the entries, the ledger is authoritative;
    the question gate keeps accepting only questions.
  - Closing: a question on its answer or withdrawal; an owner step and an
    external wait only by the coordinator's explicit close citing the
    evidence (the owner's reply or the provider's mail is evidence, not
    closure); every close keeps its reason. Nothing expires; the age is
    shown, and open entries are listed to the coordinator when its turn
    starts (an event), never flagged by a timer.
  - Space row: compact non-zero counts per kind with distinct glyphs and the
    oldest age; details and ages in the Supervision tab and its finding
    list.
  - The coordinator rules: a coordinator that reports awaiting-reply or
    writes "Waiting on others" opens the matching entry (rule text, then
    the stop check enforces it).
  - Slice 1: the kinds and fields, `herdr decision add --kind owner_step|
    external_wait --waits-on ... --until ...`, `close --evidence`, the space
    row counts. Separately check whether a running headless worker shows in
    its space (reported as not visible, unverified).

- [ ] `worker stop` gets EPERM from killpg on a zombie-only process group [t-gwmojmax]
  (macOS; found 2026-10-11 by the worker of bdd3a8f7): on EPERM check
  whether the group still has a live non-zombie process and, if none, treat
  it as ESRCH (gone); code in src/platform/macos.rs, with a test.

- [ ] The preflight checks that the folder slot belongs to the run's [t-lyxua57c]
  repository (rormpc, 2026-10-11, `herdr report` r-6): before 27f3f475 a run
  passed preflight and then failed at the worker start with "folder slot
  ... is not a worktree of .../rormpc", leaving only abort; the preflight
  refuses (or the start creates the repository's slot) instead.

- [ ] The driver's task suffix asks the worker to verify the task's facts [t-c5bni564]
  (the owner via the roguix-vps coordinator, 2026-10-11: a task said
  `ssh root@<domain>` without the repository's documented port; the
  worker did not check, the coordinator's review did not either, the run
  failed). `herdr todo run` appends the contract to every task; add one line:
  verify each concrete fact the task gives (host, port, user, path,
  command) against the repository, name the source in the final message,
  and stop with WORKER-BLOCKED when the task and the repository disagree.
  The todo skill's matching rule is the coordinator's text edit.

- [ ] A worker's space says who opened it and why, and never takes focus [t-qf7hytjv]
  (the user, 2026-10-11, screenshot: "why was a new space opened? ask the
  models"). The guix coordinator started a pane worker for an item in
  another repository (roguix): `herdr worktree create --cwd <roguix>` made a
  child space `todo-ci-desktop` under `roguix`, far from the coordinator,
  focused 26 s later (by whom is unknown), and the worker sat at an empty
  prompt (the task not yet sent). Round 20261011-174125-45c5 (sol, MiMo;
  DeepSeek refused by herdr-job's 64-tab limit), decided:
  - The worktree space stays under its repository; it shows its owner
    ("opened by <coordinator> for <item>", a link back), the task's first
    line and its state (preparing, running, needs attention); the
    coordinator's space lists the worker with its repository: two views of
    one worker.
  - Creating a worktree or worker never takes focus; record focus
    provenance (who focused a tab: a click, an API call by pane X) so a
    surprise focus can be attributed.
  - Cross-repository work goes through that repository's per-repository
    slot as a headless worker by default; a pane worker is a deliberate
    choice; the task is delivered with the start (`agent start` with a
    prompt returns its request id, see the agent-prompt item).

- [ ] One space per git repository, and the space is its coordinator (the [t-ntwtpg5y]
  user, 2026-10-11: "there is to be no button to start a coordinator at
  all; a space equals a coordinator; always one space per git repository
  and one coordinator; ask the models"; this replaces the crown-chip item
  of round 20261011-173950-bc35). Round 20261011-174413-916f (sol, MiMo,
  DeepSeek) on the how, decided by the coordinator:
  - Identity: a git space is keyed by the resolved git common directory
    (worktrees map to their repository; nested repositories and submodules
    are their own); `herdr workspace create` for a repository that has a
    space returns and focuses it, atomically. Non-git spaces (`~`, chats)
    are explicitly exempt and have no coordinator.
  - The coordinator is the space's role, not a process: tab 1, pinned, role
    `coordinator`, created with the space; its agent starts when the space
    is created and is resumed (same session) on reopen and after a handoff;
    an unexpected exit restarts it with designed, bounded backoff; a
    deliberate stop, a usage limit or a login failure shows that state in
    tab 1 with its recovery, never a restart loop. Plain chats are other
    tabs. Closing the space is the only way to end the role.
  - No start button: the crown chip and its `todo_command` launcher go (a
    `herdr coordinator start` CLI stays for scripts one release).
  - Worktrees: `herdr worktree create` returns a tab inside the repository's
    space (each tab keeps its checkout path and branch, labelled), never a
    new space; headless workers stay headless with their per-repository
    slot.
  - Existing duplicates (worktree child spaces, two spaces of one repo):
    no automatic tab moving; old space ids become aliases of the canonical
    one server-side; a `herdr space merge --into` command merges on the
    user's request. Must not break: tenure exclusivity, handoff, session
    restore, pane cwds, worker slots, the endpoint contract (new methods
    only).
  - Slice 1: identity + create-or-focus + non-git exemption + the
    coordinator tab created with the space + removing the crown; slice 2:
    worktree tabs; slice 3: aliases and merge; restart policy with slice 1.

- [ ] The stop check counts every remaining item, and a trust dialog is [t-iywq5qf4]
  reported at once (the user via the guix coordinator, 2026-10-11: "work;
  report it to the herdr coordinator; ask the models": it asked about two
  user-blocked items and stopped although a third, independent item was
  runnable; the enforced stop check let it through because a question was
  open). Round 20261011-174524-db36 (sol, MiMo, DeepSeek), decided:
  1. An open question exempts nothing: the stop goes through only when
     every remaining item in "Next, in order" (or the driver's state) is
     linked (`herdr decision ... --item`) to an open decision or owner step
     awaiting the user that names what is needed; an unlinked or
     unclassified item counts as runnable and blocks; links to answered or
     closed decisions do not count; many items on one decision are flagged.
  2. `herdr agent start` and `worktree create` report
     `trust_dialog_pending` with the folder as soon as screen detection sees
     Claude Code's folder-trust dialog (as `agent prompt` names dialogs
     today), so the coordinator asks the user at once and goes on with
     other work; workers default to headless where they can (no dialog);
     agents never answer the dialog. Pre-trusting herdr's worktree roots is
     the user's choice ("Needs a decision").
  Also (guix addendum): after `agent start` failed with agent_name_taken
  behind the trust dialog, a later `agent prompt` returned request
  prompt_1a12ba24d47_3 but `wait-agent --request` said "herdr does not know
  request"; find why the request was not tracked for that pane.

- [ ] Three gaps from the job-seeker coordinator (the user, 2026-10-11: [t-jarmf2sq]
  "why is there no work here?"):
  1. Ready items were handed (user decision 2026-10-07) to "the separate
     job-search session", which no live pane holds; the space looked idle
     with nothing waiting. With one space = one coordinator (the space
     item) such handoffs disappear; until then the supervisor reports an
     item owned by a role no live agent holds.
  2. Headless worker w79 waited ~14 h in waiting_approval: its task needed
     the network (the consult scripts) which the sandbox lacks, and its
     escalated question after `owner_gone` reached no one visible; the
     coordinator waited with `herdr-job watch --pid`, which sees only the
     exit. Fix: `herdr todo run`/`worker start` refuse a task that names
     network tools unless the run is granted `net.egress` (capabilities
     item); an escalated worker question appears in the user's `?` list
     and notifies; the todo skill already says `herdr worker wait
     --attention` (check its wording names watch --pid as wrong).
  3. `herdr-job wait-agent --until verdict` exits 7 ("turn finished without
     a WORKER line") on `WORKER-DONE none | ...`, a valid no-commit result;
     accept `none` as a done verdict without a commit check.

- [ ] An interactive worker gets its own unfocused tab, never a split of the [t-ollsffj5]
  coordinator's pane (the owner via classify-x-posts, 2026-10-11: "why is a
  split open here?"): its coordinator ran `herdr pane split w7:pV ...` then
  `agent start --pane` for workers that need SSH and a browser (no
  worktree), so the coordinator and a worker share a tab at half width.
  Want: `herdr agent start --new-tab [--no-focus] [--cwd]` (a new tab with
  the worker role in the coordinator's space, unfocused by default; it
  returns the pane and, with a prompt, its request id); the coordinator
  allowlist refuses `pane split` of the caller's own pane in coordinator
  tabs naming `--new-tab`; the todo skill says so (text edit done
  2026-10-11).

- [ ] A "chrome doctor" before a Claude in Chrome task (the owner via [t-wyvdvqrk]
  roguix-vps, 2026-10-11: "fix it; ask the models": Chrome tools worked in
  the morning, then "Browser extension is not connected" with
  `list_connected_browsers` []; Chrome was not running; after `open -g -a
  "Google Chrome"` the native host started but the list stayed empty).
  Round 20261011-174924-* (sol, MiMo, DeepSeek): a running native host only
  proves the extension reached the local host, not that the browser is
  bound to this session; whether one browser binds to one session at a time
  is disputed between the models and unverified. Decided: a script
  (`~/scripts/chrome-doctor`, in the dotfiles) that checks, in order, each
  with its positive fact: Chrome runs; its front window is a real profile,
  not `chrome://profile-picker` (osascript); the native host manifest points
  to an existing `claude`; the host process is alive; then the session's
  own `list_connected_browsers` is non-empty (select the browser if it
  lists one) and `tabs_context_mcp` answers. The coordinator runs it in the
  worker's session before handing over a Chrome task; on a failed step it
  reports that step and the user's action once and stops (no retry). A
  session may launch Chrome (`open -g`), never quit or restart it; the
  profile picker, sign-in and enabling the extension are the user's. The
  todo skill names the doctor step. Verify the per-session binding
  question with two sessions.
  Checked 2026-10-11 20:55 by the herdr coordinator: its own session lists
  one connected browser (macOS, local, in use, connected 17:54 CEST through
  the single native host) while roguix-vps's lists none: the list is per
  account, so the browser was connected and free; roguix-vps's MCP
  connection was stale (set up before Chrome restarted, never reconnected).
  So the doctor's failing step is "this session's MCP link", fixed by
  `/mcp` reconnect or a resumed session (the user's action), not Chrome.

- [ ] Scheduled repository work triggered by a macOS launchd service [t-qybebntk]
  (the user via job-seeker, 2026-10-11: "it is to be a macOS service, so
  it is independent of herdr; the coordinator works here, but gets its
  signal from the macOS service somehow; ask the models"; job-seeker's
  daily batch, d-mqk3ivac). Round 20261011-175313-7fd3 (sol, MiMo,
  DeepSeek), decided:
  - Declarations in the repository (`.herdr/schedule.toml`: task id,
    Warsaw time of day, what to run); one installer (`herdr schedule sync`,
    from the repository) generates one LaunchAgent plist per repository
    and task (exact launchd calendar schedule) and deletes stale ones;
    plists carry only repository and task, never secrets.
  - launchd runs a tiny non-interactive dispatcher that first writes a
    durable due record keyed (repository, task, Warsaw date) atomically in
    a herdr-independent store, then, if herdr answers, delivers it to the
    repository's coordinator (`herdr agent prompt` with the record id; the
    coordinator claims it and runs the work through its workers; no second
    driver). Herdr down or no coordinator: the record waits and is
    delivered at coordinator start (an event).
  - States due, claimed, done, failed, missed; one batch per (repository,
    task, date); launchd coalesces missed firings, so only today's Warsaw
    date is claimable, earlier dates become `missed` and are reported,
    never backfilled; the Warsaw date is computed explicitly
    (Europe/Warsaw), not assumed from the Mac's zone; external actions are
    journalled so a crash mid-batch does not resend (the 15/day cap counts
    across retries).
  - Missed and failed records are supervision findings with a
    notification; the coordinator writes the outcome (counts) back.
  - Never: dialogs from the dispatcher (no osascript/open), secrets in
    plists or logs, a backfill, a second batch the same day.

- [ ] The stop check asks to renew a question raised in the same turn [t-254ex54k]
  (classify-x-posts, 2026-10-11): in a turn started by a background-task
  notification, `herdr agent awaiting-reply` ran as the last command; the
  Stop hook then demanded "renew or withdraw" a question "asked before this
  turn", forcing an extra turn. A question reported in the current turn
  (its report's turn id or time after the turn's start) is current; only
  older open questions get the reminder.
  Second classify-x-posts report (2026-10-11, the owner: "what is going
  on here?"): one wait shows twice in the `?` list: the worker
  fomobox-dns asks "will you connect the Claude extension in Chrome?" and
  its coordinator holds its own copy ("connect Chrome for fomobox-dns"),
  which the stop check keeps asking it to renew. Want: a coordinator
  waiting on its worker's question holds none of its own; herdr links the
  coordinator's report to the worker's question (one entry tied to the
  worker) or refuses a duplicate awaiting-reply that names an owned
  worker's open question. (The pane was moved out of the split by someone
  else; not herdr.)
  Landed 2026-10-11 (e91e0a43): the stop check skips open questions reported
  at or after the turn's start; only earlier ones get the renew-or-withdraw
  reminder. Left: the duplicate coordinator/worker question (the second
  classify-x-posts report above).

- [ ] Headless workers get granted network and file access instead of a [t-dy4nxuj2]
  pane fallback (the user via the guix coordinator, 2026-10-11: "a
  workaround, fix it in herdr; ask the models": worker w185 was blocked by
  the sandbox: guix.gnu.org through the proxy, ~/.config/gh, /private/tmp;
  earlier no ssh; each time the coordinator fell back to a pane worker, and
  w185's temp files under ~/.local/state/herdr/workers/tmp/w185/ were gone
  after the blocked turn). Round 20261011-210835-20a3 (sol, MiMo, DeepSeek
  agreeing), decided: build slice 2 of t-ih3cmtjf now: requested
  capabilities (repository `.herdr/` or the task) are mapped into the
  headless worker's Claude sandbox settings: `net.egress{hosts[:port]}` to
  the proxy allowlist, `fs.write{paths}`, a new `fs.read{paths}` (e.g.
  ~/.config/gh, an ssh key), ssh as host+port plus `exec{ssh}` plus the
  key read; credential access is its own grant, separate from reaching a
  host; a capability the sandbox cannot enforce is refused (never silently
  widened); only the user grants, at the scope he chooses (repository or
  run), through the `?` grant question. A preflight diffs the declared needs
  against grants and refuses before the model runs, naming the missing
  capability; a runtime denial returns the same structured request and
  pauses the run (no switch to a pane). The run's temp dir is a granted
  write root kept until the run ends, not the turn. Pane workers stay for
  attended interactive work only; the todo skill says so when this lands.
  Addendum (guix, 2026-10-11): worker w189 in a linked worktree of a
  submodule (git dir ~/personal_projects/guix/roguix-vps/.git/modules/roguix)
  could not commit: the sandbox denied the worktree's git dir
  (index.lock: Operation not permitted). So a headless worker cannot commit
  in any linked worktree whose git dir lies outside its cwd. Part of this
  slice: the worker's write roots always include its worktree's resolved
  git dir (`git rev-parse --git-dir` and the common dir's refs/objects), as
  herdr's own folder slot already works because its git dir is under the
  repository; no grant needed for that.

- [ ] What to do about the auto-mode classifier's inconsistent denials (the [t-aabu5rjo]
  user, 2026-10-11: "plan it with the models as a TODO item"; reports from
  roguix-vps, classify-x-posts and others: plain local reads denied right
  after another denial and allowed later; a production write allowed but its
  read-only status and revert denied; `herdr worker ack` denied, trapping a
  Stop hook (fixed in 03b46496); typing into a web form the user asked for
  denied until he said "unblock"). Round 20261011-211427-bb48 (sol, MiMo,
  DeepSeek agreeing), planned:
  1. herdr records every denial (hook payload where Claude Code gives one,
     else the transcript) as a deduplicated supervision finding: action,
     category, session, cwd, time, the preceding denial, whether the same
     action passed later; secrets redacted. A denial is evidence, not proof
     of a misclassification.
  2. The rule (global CLAUDE.md, the todo skill, herdr's reminder): after a
     denial, record it once, report it once, go on with independent work or
     mark the item blocked; never retry, rephrase, use another tool or ask
     another session; never fish for the user's "unblock".
  3. A grouped `?` entry proposes a narrow allow rule from the findings
     (herdr's coordination commands, specific read-only commands scoped to
     paths), tested against what must stay denied; applied only after the
     user approves the patch; no blanket `grep`/`git` rules, no automatic
     widening.
  4. Recurring production work: reviewed scripts with bounded targets,
     status and rollback included, plus an allow rule (as for Porkbun and
     the VPS check).
  5. The user's explicit word stays the one-off override.
  6. A redacted, aggregated report of contradictory outcomes, ready for the
     user's `/feedback` when findings accumulate (on the event, no cadence
     timer); never sent automatically.

- [ ] An optional evidence-backed model review between verify and cherry-pick [t-aiizvfcy]
  (from NVIDIA Boro's adjudication, github.com/NVIDIA/boro; the user
  delegated the choice to the models, d-6vlqbplk, round
  20261011-232549-57cc: sol and MiMo add, DeepSeek skip). After `verify`
  passes, `herdr todo run` can ask a reviewer model for findings on the
  worker's diff; each finding must cite file:line and a concrete failure
  mechanism; a serious finding is dismissed only with evidence (a command's
  output or file:line), and every disposition is recorded as a supervision
  finding (merged with the supervisor's findings, not a second store).
  Opt-in per run or per repository; it never blocks on ambiguity alone; the
  coordinator still reads the full diff. Skipped by the same round: a
  false-positive memory for consult rounds and baseline-plus-novel-only
  rounds.

- [ ] The server publishes its socket only once it listens (follows from [t-fyxvckg7]
  [t-3ifvwy3l]: herdr-job's `wait` wakes on socket-directory events, and a
  connect landing between the server's `bind` and `listen` is refused and
  retried only on the next directory change). Bind on a temporary name,
  `listen`, then rename it into place, so the directory event means
  connectable; a test that connects on the event.

- [ ] `herdr todo run` serves a coordinator that owns two repositories [t-o73wcaoq]
  (the user via the rormpc coordinator, 2026-10-12: "report it to the herdr
  coordinator and let it ask the models"; todo-rormpc owns rormpc, with the
  TODO.md, and rormpc-tools, Python, branch `main`, no TODO.md; four items
  blocked). Round of 2026-10-12 (sol, MiMo, DeepSeek agreeing), decided:
  1. Separate the backlog owner from the execution target: `todo run
     --repo <path>` reads the item from the owner's TODO.md and updates
     TODO.md/DECISIONS.md there, while the folder slot, sandbox, checks,
     allowed paths, commit, [install] and push come from the target;
     the target's default branch is read from git (never a hard-coded
     `master`); the target must be one the coordinator owns (its
     AGENTS.md or a registration), not an arbitrary path; the run records
     both repositories and the landed commit.
  2. Release recipes registered in the target's `.herdr/` (argv), run by the
     driver after landing, never by a worker: staged and resumable (version
     landing, tag and push, the dependent repository's pin, install), each
     stage recorded, never repeating a tag, push or install blindly after
     an interruption; repository-scoped.
  3. A change spanning both repositories is two chained items (`--next`),
     the dependency first; the consumer tolerates the intermediate state
     (a version gate). No multi-repository worker (rejected by all three:
     no atomic landing across pushes, two write roots weaken the sandbox).
  Meanwhile (sol, MiMo; DeepSeek: single use only): rormpc runs
  rormpc-tools items the old way with the allowlist override, one start per
  item, keeping per-repository checks and verify; releases stay by hand
  with a checklist; the two-repository feature is split now.

- [ ] Requirements from reports are traced to tests before an item closes [t-jyicwrb4]
  (follows from [t-276mtaqw]; round 20261012-010450-e610: the supervisor
  predicate "an assignment whose waiter's anchor never resolved" was
  requested in a report and silently dropped). Each point a report or the
  user asks for is numbered in its TODO item (R1, R2, ...); the worker's
  final message maps each to its implementation and test or an explicit
  deferral; the coordinator checks that list before `--close` and moves
  any deferral to its own item. Text change in the todo skill and
  AGENTS.md (a worker); a `herdr todo` close check that refuses an item
  whose numbered requirements are unmapped is a follow-up.

- [ ] A click on a todo run's sidebar line always opens its run view (the [t-ewtqmtqo]
  user, 2026-10-12, screenshot: "not clickable; ask the models"). The
  line opened the step log only when `has_log`; during the worker step
  (10-40 min) a click did nothing. Round of 2026-10-12 (MiMo, DeepSeek;
  sol preferred opening the worker's live log directly), decided: the
  click opens one run view popup, the same for every step: the item's
  title, run id and attempt, the steps with their times, the current
  step, the pending event and its actions; it embeds and follows the
  current step's log, and during the worker step the worker's live log;
  with no output yet it says so ("worker running 12m, no output yet"),
  never blank; Enter/Space opens it too; the right-click menu mirrors its
  actions (takeover where a worker allows it). Rule: a line that looks
  clickable never silently does nothing.

- [ ] `todo run` takes its checks and install argv from the reviewed base, [t-zgrddkbc]
  not from the landed commit (round 20261012-020315-c8e8, DeepSeek, while
  rejecting multi-step installs): `.herdr/checks.toml` is read after the
  worker's commit lands, so a commit can change the argv that runs with
  the coordinator's environment. Read the checks and [install] from the
  run's base commit (what the coordinator approved), and refuse a run
  whose commit changes `.herdr/checks.toml` unless the item says so.

- [ ] Two tests fail only under the full suite's load (found by the [t-rlvhvswr]
  quiet-supervision worker, 2026-10-12): plugins/job
  `WaitAgentTests.test_a_gone_agent_ends_the_wait_without_a_retry`
  recorded sleeps [0.001 ... 0.05] instead of none (passes 5/5 alone), and
  `session_restore::server_restores_while_git_metadata_is_blocked` (a
  5-second read timeout in the test). Reproduce each with the stress loop
  in AGENTS.md and fix the asynchronous cause (Rule 10), not the timeout.

- [ ] The roherdr client on the owner's 2010 MacBook connects to the server [t-wtxc5taz]
  on his M1 MacBook (the user, 2026-10-12: "make the roherdr client on the
  2010 MacBook connect to the server on the M1 MacBook; ask the models").
  Uses the existing `herdr --remote <ssh host>`. Round
  20261012-052026-525b (sol, MiMo, DeepSeek agreeing), plan:
  1. Facts first (the owner runs them on the old Mac): `sw_vers`,
     `uname -m`, `sysctl -n machdep.cpu.brand_string
     machdep.cpu.features`; a 2010 MacBook tops out at macOS 10.13 or
     lower and may lack SSE4.2/AVX (Core 2 Duo).
  2. Probe before building: tiny Rust and Zig programs built on the M1 for
     x86_64 with `MACOSX_DEPLOYMENT_TARGET=10.13` (and Zig
     `-Dtarget=x86_64-macos.10.13`, `-Dcpu=core2` or baseline), checked
     with `otool -l`/`vtool -show-build` and run on the old Mac.
  3. Then a client build of the same commit: x86_64, explicit deployment
     target, `-C target-cpu` baseline (never native), the same settings
     passed to libghostty-vt (no unconditional AVX) and SQLite; check the
     binary with `otool` and `objdump` for AVX; test an attach (startup
     alone does not exercise rendering).
  4. The M1 side: Remote Login on and a key (the owner's steps); herdr on
     the PATH of a non-interactive ssh session (or an absolute path);
     `ssh m1 'command -v herdr; herdr --version'`; client and server on
     the same commit (the handshake rejects other protocol versions).
  5. If the old Mac runs 10.11 or older, or the build cannot avoid newer
     instructions: Linux on the old Mac, or the fallback that works today,
     `ssh -t <m1> herdr` from any terminal.
  Facts checked by the coordinator over ssh (2026-10-12): the old MacBook
  runs Guix System (Linux 6.12, x86_64), CPU Core 2 Duo P8600: SSSE3 and
  SSE4.1 but no SSE4.2, POPCNT or AVX; 2 cores, 16 GB; no herdr there yet.
  So: not macOS; build the static `x86_64-unknown-linux-musl` client with
  a baseline CPU (Rust `-C target-cpu=x86-64`, Zig/libghostty-vt
  baseline, no SSE4.2/POPCNT/AVX), verify with objdump that the binary has
  none of those instructions, copy it to the old Mac and attach with
  `herdr --remote <m1>`; the M1 needs Remote Login. Guix packaging later.
  Packaging decided 2026-10-12 (the user: "why not a Guix package, so the
  dependencies are handled? ask the models"; round of 2026-10-12, sol,
  MiMo, DeepSeek): a static musl binary has no runtime dependencies; the
  hard dependencies are build-time (Rust, ~310 crates, Zig, SQLite's C).
  Order: (1) the static baseline-CPU musl binary now, tested on the
  laptop; (2) a Guix package in the owner's app channel wrapping the
  hash-pinned release artifact (copy-build-system): Guix profiles,
  rollbacks and `guix package -u`, one hash bump per release (MiMo,
  DeepSeek); (3) later, if source provenance matters more than release
  speed, a source build with a vendored crate tarball, Guix's zig 0.16 and
  rust (first test whether herdr builds with Guix's rust 1.93: the 1.96.1
  pin may not be a minimum), substitutes from the owner's CI so the 2-core
  laptop never compiles (sol). Rejected: every crate as its own Guix
  package (weeks of churn for a fast-moving fork).

- [ ] The usage widget groups a provider's API spend under its subscription [t-tvenbg7r]
  line (the user, 2026-10-12: "the AN line with tokens should come after
  the AN subscription line, I think; make visualizations with the models").
  Round of 2026-10-12 (sol, MiMo, DeepSeek agreeing on grouping), chosen
  mockup (sol's, with MiMo's marker):
  ```
  AN   7% 2h22m 91% 2d13h
     API $0.00/$200 spend  exp28d
  OA  14% 4h08m 24% 5d21h  exp10d
     API $0.00 spend
  GO            0% 6d23h
  DS $7.09 balance
  OR $9.81 balance
  ```
  The sub-line is indented under its provider with an `API` tag (no
  repeated label); the credit expiry is its own clock, shown as a
  countdown; only the amount is colored: amber from 80% of the cap, red
  from 95%, plus a `!` marker so it reads without color; the expiry turns
  amber within 7 days, red within 2, and `expired` after.
  Also (the user, 2026-10-12: "Antigravity's two weekly groups: add them
  to the usage widget? ask the models"; round of 2026-10-12, MiMo and
  DeepSeek; sol preferred two sub-lines): the GO line keeps the "Gemini
  Models" group, and the "Claude and GPT models" group, which herdr already
  parses, gets one tagged sub-line on the same column grid, never a
  combined percent (the resets are independent):
  ```
  GO            0% 6d23h
     CG         0% 6d23h
  ```
  Labels decided (the user, 2026-10-12: "what does OA $0.00 spend mean? and
  show when the AN $200 renews; ask the models"): `OA API` = OpenAI API
  pay-as-you-go month-to-date spend (no balance exists) -> `   API $0.00 MTD
  spend`; the AN credits renew on the same date the unused ones expire, so
  the line leads with the renewal: `   API $0.21/$200 spend  renew28d`
  (countdown; the tooltip says "unused credits expire then").

- [ ] Headless workers on the owner's Google AI subscription through the [t-dnqkeub6]
  Antigravity CLI (the user, 2026-10-12: "check using Gemini for tasks too;
  I have a subscription there but they give few tokens"). Checked by the
  coordinator: pi has an `antigravity` login but lists no Antigravity models
  (only OpenRouter's Gemini, paid); the Antigravity CLI `agy` (already
  read by herdr's usage widget, `agy -p /quota`) runs headless
  (`--print`, `--input-format/--output-format stream-json`, `--model`,
  `--effort`, `--sandbox`) and its quota has two weekly groups, both at
  100% left now: "Gemini models" (gemini-3.8-flash, gemini-3.1-pro) and
  "Claude and GPT models" (claude-sonnet-4-6, claude-opus-4-6, gpt-oss);
  quota is consumed in proportion to token cost. Plan: `todo run --agent
  agy` with a model setting, the same folder slot, verify and WORKER line
  contract; the usage gate reads that agy group's weekly limit; agy's own
  `--sandbox` plus herdr's sandbox (check what agy's sandbox enforces; never
  `--dangerously-skip-permissions` without herdr's sandbox around it);
  prefer it for small items since the quota is small.

- [ ] What each project costs: usage per space over time and per TODO item [t-twmvyrnk]
  (the user, 2026-10-12: "a summary of how many tokens each herdr space
  used, over time, maybe per TODO item"; "so I can say: this project uses
  a lot, is it worth it for me? or: this project cost me this much";
  rounds 20261012-140238-ec7f and the next, sol, MiMo, DeepSeek).
  1. A persistent per-session usage ledger written as transcripts are read
     (session id, agent kind, model, space, day, input/output/cache tokens,
     source; keyed by session, kind and transcript offset, deltas only),
     so forgotten sessions keep counting; the report states its coverage
     start date.
  2. Per TODO item: worker runs exact (their sessions, all attempts); the
     coordinator's usage shown separately, an optional "during this run"
     column labelled as an overlap estimate, never summed into the item;
     unattributed usage shown so totals reconcile.
  3. Money, never blended: share of plan capacity ("38% of your Claude
     weekly windows this month") as the headline; exact metered spend
     (API credits via the proxy, DeepSeek, OpenRouter) as "actual cash";
     allocated subscription cost (monthly fee x the project's share of the
     consumed capacity) labelled "attributed, not incremental"; list-price
     equivalent only as a muted "API-equivalent value", never as spend.
  4. `herdr usage report` (spaces x days, `--by todo`, `--json`) and a TUI
     Usage view rendering the same query (spaces x days, drill into
     models and items).

- [ ] A pause button on a space for when tokens run short (the user, [t-q5rybg3y]
  2026-10-12: "maybe a pause button on a herdr space that I can press when
  I run short of tokens? ask the models", "as a TODO"; round of
  2026-10-12, sol, MiMo, DeepSeek agreeing):
  1. One button pauses a space: new worker starts are held (the same
     admission chokepoint as the usage gate's pending starts, the item and
     its held state persisted before any start); running workers finish
     their current turn, then hold; the coordinator gets one "paused since
     <time>: <reason>" notice and its next herdr-driven prompt is held,
     never silently swallowed. Nothing is killed; the landing path (verify,
     cherry-pick, install, push) of a run already past review finishes.
  2. Resume only by the same button (or `herdr space resume`), never by the
     usage gate reopening; held starts resume in order, exactly once.
  3. Per space by default; a separate, labelled "pause all"; no
     per-provider pause (the usage gate already does that).
  4. The space header shows a pause icon with the reason and what still
     runs ("paused, 2 turns finishing, 4 starts held"); its right-click
     menu sets the reason; `herdr todo queue pause` maps onto it; the state
     survives restarts and handoffs.
  Confirmation decided (the user, 2026-10-12: "and a confirmation whether
  I want to pause or resume? or a context menu? ask the models"; sol,
  MiMo, DeepSeek agreeing): pause is instant (reversible, loses nothing;
  a confirm would only train dismissing dialogs); resume confirms in a small
  dialog with the real counts ("Resume rormpc? 4 held starts launch now",
  Resume/Cancel), skipped when nothing is held; the header icon is both
  status and action (distinct pause/resume icons, a dimmed paused state);
  the space's context menu offers the same actions as a second path, never
  the only one.

- [ ] [t-csew6r2l] Allow a reviewed DNS cutover script past the auto-mode classifier?
  Options: an allow rule in settings.json for one named, reviewed script (e.g. the roguix-vps Porkbun cutover), approved by you as a whole patch (Recommended) | keep running such scripts yourself with `!` | a herdr "grant" decision kind the classifier honours (not possible: herdr cannot change Claude Code's classifier)
  Reported by the roguix-vps coordinator 2026-10-11: after RackNerd moved
  the VPS to <new IP>, 11 Porkbun A records had to move; the owner
  said "change the records, you have Porkbun API access", but the
  classifier's DNS category denied even reading the variable names in
  the DNS provider's env file; the agent handed a script to run with `!`. An
  in-session request does not lift a classifier category; only a
  permission rule does, and permission changes need your approval of the
  whole patch (AGENTS.md); a peer coordinator cannot grant it.
  Decided by the user 2026-10-12: an allow rule in settings.json for one
  named, reviewed script, approved by the user as a whole patch. A worker
  prepares the patch outside `~`; the coordinator applies it after a yes.

- [ ] A short-lived conversation session replaces the resident coordinator [t-botcpa2j]
  for talking with the user (the user, 2026-10-12, agreeing to the
  coordinator's proposal: "it can be done; ask the models"; part of the
  queue-mode trial, d-amwperuu). Round 20261012-155443-74c0 (sol, MiMo,
  DeepSeek agreeing; boundaries decided by the coordinator):
  - A conversation is a durable thread record in herdr (id, the user's
    Polish words verbatim beside the English, linked TODO and decision
    ids), not a process. A model execution starts on the user's input,
    never on a herdr event alone (escalations and peer messages wait in
    the question list); it ends on an explicit disposition (request queued
    and committed, answer given, decision recorded, cancelled), never on
    idleness; an unanswered clarification stays pending without a process.
  - The snapshot herdr generates is bounded (about 3-6k tokens): the
    thread's recent turns, pending questions, open TODO titles with the
    user's words, the decisions index, the last runs' outcomes, queue
    status and usage, unread peer messages. Everything else is paged in by
    commands; "that thing from before" is found with a herdr search over
    threads, TODO, DECISIONS and run history, not with a digest.
  - Every write goes through herdr commands carrying an expected revision
    and an idempotency key (TODO/DECISIONS edits, decisions, messages,
    consult requests); a stale one is refused with a readable conflict, so
    phone and desk sessions cannot overwrite each other. Starting,
    approving and closing runs stay herdr's.
  - A role-specific instruction file (about 1k tokens), not the full
    AGENTS.md and skills; measure a fresh launch's input tokens.
  - First slice: intake only, in one repository: a request becomes a TODO
    entry with its Polish original and an explicit "queued as <id>"
    receipt; thread records and search; reuse the question list.
  Decided by the user 2026-10-12 (d-zihxrk6h): Claude Code with a small
  role file (not a small model herdr calls directly).

- [ ] Credit funding without `.herdr/checks.toml` (the roguix-vps [t-7yl4ao3x]
  coordinator, 2026-10-12, relaying the user: "strange that every
  repository needs checks.toml before its workers can use the credits";
  their round 20261012-192228-5cfb, sol, MiMo, DeepSeek: the agent and
  funding choice and verify-and-land are separate axes). Already true
  since b124c8c55: `herdr worker start --agent pi-anthropic|pi|claude`
  needs no checks (src/cli/worker.rs:718), but `worker start --help`
  does not list `--agent`. Proposed: list it in the help; let `todo run`
  run in a repository without checks under an explicit per-repository
  policy (checks none: the run works and reports "checks skipped" but
  never auto-lands, installs or pushes); spend and concurrency limits per
  credit worker (see "Concurrent credit workers refuse each other").
  Decided by the user 2026-10-12 (d-wbfmnx4c): all three parts: list
  `--agent` in `worker start --help`; `todo run` under an explicit
  per-repository checks-none policy that never auto-lands, installs or
  pushes; spend and concurrency limits per credit worker (after
  "Concurrent credit workers refuse each other").

- [ ] A hard cap on a coordinator's context size, enforced by supervision [t-2k3ima2h]
  (the user, 2026-10-12: "maybe add some hard cap on the coordinator's
  context size, so supervision does something about it: warn the
  coordinator, a warning threshold and then a shutdown threshold; ask the
  models").
  Round 20261012-202058-a92c (sol, MiMo, DeepSeek agreeing; decided by
  the coordinator where they split):
  - No kill at any threshold. "Shutdown" means retire: at the
    coordinator's next turn end herdr freezes the tenure (refuses its
    mutations by tenure credentials, not pane identity; text replies to
    the user stay allowed), runs the `coordinator ready` semantics and
    starts the successor through [t-ic5rrxn3]'s handoff; workers keep
    running and pass to the successor; open questions travel in the
    manifest. A failed handoff unfreezes but keeps an open finding and
    tells the user; it never silently resumes normal work above the cap.
  - Metric: the input context of the latest model request as the agent
    reports it (uncached + cache writes + cache reads), never a transcript
    sum and never cost. Evaluated on each request-usage / turn-end event;
    the existing 5-minute sweep only catches missed events and a wedged
    session. Defaults: warn at the smaller of 300k and 60% of the
    agent's effective window, retire at the smaller of 400k and 80%,
    configurable per agent kind; a measured fresh-session floor at or
    above the warn threshold is a configuration error.
  - Thrash guard: the successor's starting context is measured; one
    automatic retirement per tenure, a second breach only alerts the user.
  - A wedged session (no turn end) cannot be bounded without kill
    authority: herdr records a finding when its usage event crosses the
    cap and alerts the user; no timer, no grace period.
  - Dismissed: request admission control (MiMo, sol) — Claude Code
    coordinators run on the subscription, outside herdr's credit proxy,
    so herdr cannot refuse their requests; DeepSeek's "grace, then reap"
    and "escalate after X minutes" are timers.
  - Order: after the handoff slice lands, (1) show per-request input
    context and growth per coordinator in `supervise --report`, with a
    warn finding at the warn threshold and no other action; (2) retire at
    the cap with the user told; (3) automatic only after a few handoffs
    show the successor's cost per hour is lower.
  Asked 2026-10-12 (the user, on "the 5-minute sweep only catches missed
  events and a wedged session": "an asynchronous workaround again? ask the
  models").
  Round 20261012-203001-263e (sol, MiMo, DeepSeek agreeing; decided by
  the coordinator): yes, a sweep that "catches missed events" is a Rule 10
  workaround; it replaces the sweep line above. The agent's transcript is
  the source of truth; a hook or a file-watch notification only wakes a
  reconcile that reads every complete line from a persisted cursor
  (session, file identity, byte offset, last usage), applies the
  thresholds idempotently on the maximum per-request input seen (a cache
  discount never un-retires), and advances the cursor atomically with the
  transition. Watch the directory, not only the file (rotation and
  compaction replace it): a new file identity or a shrink rescans.
  Reconcile at startup, handoff, Stop and after a watch is re-armed, so a
  dropped notification only makes enforcement later, never wrong; a hook
  that fires before the line is written reads nothing and the line's own
  write wakes the next reconcile (no sleep). Linux and Windows use their
  watchers (inotify, ReadDirectoryChangesW) feeding the same reconcile.
  A wedged session makes no requests, so it grows no context and needs no
  cap action; detecting it would be a negative-time condition and stays
  out of this item. Periodic work is allowed only for external state
  without notifications (provider billing, quota), labelled as external
  reconciliation with its source.

- [ ] A "reviewed" state for TODO.md, and a coordinator that does one item [t-ktz7l3hl]
  and exits (the user, 2026-10-12: "maybe set some state for TODO.md that
  the list has been reviewed; when something is added or changed it has to
  be reviewed again. And the coordinator does one task and disappears? ask
  the models").
  Round 20261012-202716-dbce (sol, MiMo, DeepSeek agreeing; decided by
  the coordinator where they split):
  - "Reviewed" means checked for current applicability, never the user's
    approval; it is a receipt per item (by id), not a flag on the file or
    a section (one edit would invalidate everything). The receipt records
    the item's canonical text hash, the linked decision ids with their
    revisions, the dependency items' hashes, the base commit examined,
    the reviewer, the typed verdict (`ready`, `needs_decision`,
    `obsolete`) with reasons and cited evidence; append-only.
  - Invalidation is lazy: an edit of the item, a linked decision or a
    dependency invalidates its receipt at once; commits on master do not,
    instead the queue checks at dispatch whether commits since the base
    touch the item's paths and asks for a bounded re-review only then
    (path overlap is a signal, not proof: misses callers and config).
  - Review: deterministic checks first (id, structure, links resolve,
    approval provenance, receipt fresh), then one bounded model call
    without edit rights, in a context other than the item's drafter. A
    model that changes an item's substance needs the user's approval
    again. Hand edits outside `todo_edit.py` are detected by hash and
    re-reviewed; writes carry an expected revision (CAS).
  - Queue mode starts only user-approved items with a fresh receipt,
    pinned to the reviewed revision and commit.
  - A review that ends without a typed verdict is a failed state: the
    item is blocked with a finding, never approved by default. Dismissed:
    review deadlines and timeouts (sol, MiMo, DeepSeek) are timers.
  - "One task and disappears": a fresh agent per item that picks and
    drives the next item is the per-item coordinator rejected on
    2026-10-10; queue mode's typed calls (draft, review, answer) plus the
    short-lived conversation session [t-botcpa2j] are that idea. Findings
    persist in the receipt and DECISIONS.md so item N's decisions reach
    item N+1. MiMo: the receipt does not cut the 300-700k a day; the
    queue-mode trial does; do not build it instead of the trial.
  - Order (coordinator): the receipt goes into queue mode's dispatch
    after the trial starts, not before it.

- [ ] Audit what the supervisor's 5-minute sweep catches that an event [t-ybzw6mhn]
  should (follow-up of [t-2k3ima2h], round 20261012-203001-263e: a sweep
  that catches missed events is a Rule 10 workaround). List each
  predicate the timer sweep evaluates; for each, name the event and
  durable cursor that should drive it, or label it external
  reconciliation with its source; then move the event-driven ones off the
  timer. Cost: one worker audit (text) plus small src slices; no build of
  guests.
  Decided by the user 2026-10-12 (d-nwqqrjxw): approved, queued at the
  end of "Next, in order".

- [ ] The supervisor drives the TODO itself: a dirty TODO.md starts an [t-lpqiube5]
  analysis agent; a clean one starts an agent per item in order, with a
  funding fallback (the user, 2026-10-12: "maybe like this: when TODO.md
  is dirty, the supervisor starts an agent to analyse it. When it is ok,
  it starts an agent on each TODO item in turn, deterministically: first
  with the $200 Anthropic credits, then the Anthropic subscription, when
  credits run short pi with Sol, etc. Ask the models").
  Round 20261012-203917-b6de (sol, MiMo, DeepSeek agreeing; the first
  round with the Rule 10 preamble: every answer ended with a Timing list,
  only events and external reconciliation of provider balances; decided
  by the coordinator where they split):
  - Queue mode's state machine owns the loop, not the supervisor: the
    supervisor stays observe, record, wake, alert and never starts
    anything (two components able to start workers cannot prove a single
    start). On a stalled queue it files a finding.
  - "Dirty" means committed items without a fresh receipt ([t-...]
    "reviewed" state); uncommitted TODO.md edits are unpublished input,
    never approval. The trigger is the event "a `todo_edit.py` commit
    touched TODO.md", reconciled against receipts at startup, handoff and
    turn end; the analysis's own writes do not retrigger it.
  - The analysis call inspects code and decisions and proposes a typed
    verdict per changed item with evidence; the server validates and
    stores the receipt. It never edits item text, promotes Proposed
    items, invents the owner's decisions, starts workers or lands code;
    `ready` is technical readiness, not approval.
  - Funding is chosen per stage (draft, worker, review), at each stage's
    start from live readings (credit proxy balance and cap, subscription
    usage gate, pi availability): Anthropic credits, then the Claude
    subscription, then pi with Sol. Estimated cost is reserved before the
    start ([t-alwaqkn5]). Review is never silently downgraded to a weaker
    model: without a strong reviewer the item waits and the owner is told.
    A fallback after a refusal only when the refused run provably never
    started. All routes exhausted: `blocked_funding`, owner told, resumes
    on a funding change event or the owner's action; no retry loop.
  - First step: the queue-mode trial already decided (d-amwperuu), one
    approved item, credits only, mandatory review, small spend cap; the
    ladder and automatic analysis come after the trial's numbers.
  (The "reviewed" state above is [t-ktz7l3hl].)

- [ ] The coordinator's tab shows its context size in tokens (the user, [t-gpnmlys3]
  2026-10-12: "on the coordinator's tab I want to see how many tokens its
  context has; ask the models").
  Round 20261012-205835-a0b3 (sol, MiMo, DeepSeek agreeing; all Timing:
  none; decided by the coordinator):
  - The number: the latest completed request's input context, normalized
    per agent (Claude: input + cache creation + cache read, since its
    `input_tokens` excludes cache traffic; pi's provider may already
    include cache in input, never add blindly), output excluded. Shown as
    `ctx 312k` with a hover/help text "input of the last request, not
    cost or cumulative"; exact integers kept for the cap. No percentage
    until the model's window is a known fact.
  - The server's shared transcript reader (persisted cursor keyed by
    session and transcript identity, reconciled on transcript writes,
    startup, handoff and turn end) stores `last_request_input_tokens` with
    its provenance (session id, request sequence) on the agent's state,
    exposed in the JSON API and an agent-updated event; computed for
    every supported agent, shown first on the coordinator's tab line
    beside the crown (TUI decides placement and colour). Not a metadata
    badge with expiry. No file I/O in render.
  - After `/clear`, resume of another session, a handoff or a rewritten
    transcript the value becomes unknown until the new session's first
    completed request; unknown is shown as unknown, never 0. Mid-turn it
    stays "last request".
  - This is step 1 of [t-2k3ima2h]: the cap consumes the same fact, so
    this item lands before the cap's thresholds.

## Proposed

Items agents add. Not approved until the user moves them up.

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

- [ ] A coordinator that hits a gap in its own protocol (proposed by the [t-aketwwun]
  try-roguix coordinator, 2026-10-10): it uses the smallest reversible
  stopgap the existing rules allow, says so in its reply, and records a
  Proposed item (a tool or skill change) for the user, rather than silently
  inventing machinery or stopping while the user is away. A line in the
  `todo` skill (the user's agent config: a text edit that widens nothing,
  so no question needed per DECISIONS.md, but the user approves the item).

- [ ] The `todo` skill's worker step reads as herdr-only (user, 2026-10-10: [t-cutymffq]
  "why did the roguix-vps coordinator not use headless workers? ask the
  models"; round 20261010-025447-fba3, sol + MiMo + DeepSeek agreeing).
  Step 4 gates headless workers on "where herdr has headless workers (...;
  the herdr fork)" and then gives a herdr-only recipe (`--folder-slot
  worker`, `git rev-parse master`, "the repository's notes on headless
  workers"); in roguix-vps none of these exist, so the coordinator took the
  "Elsewhere ... TUI worker" path without stating a reason (its VM items
  would have needed a pane worker anyway, but that was not why). Fix, text
  only (no widening): headless in any repository when `herdr worker --help`
  lists `verify` and the item's build and checks fit the worker sandbox
  (`herdr worker start --cwd <worktree>`, base from the repository's
  default branch; `--folder-slot worker` only where that slot exists);
  otherwise, or when the item needs what the sandbox blocks (VM launches,
  PTYs, local sockets, `ps`), a pane worker, with the reason said in the
  reply. The global rule in ~/.claude/CLAUDE.md uses the same condition.

- [ ] Smaller herdr and skill gaps reported by the try-roguix coordinator [t-3schklfu]
  (2026-10-10, pane w6:pAH, at the user's request):
  1. A pane worker's workspace was gone after a herdr restart while its
     worktree and uncommitted work stayed; `herdr worktree open --path <it>`
     failed with `not_git_worktree` until `--cwd <main checkout>`: the error
     should say it checks the calling pane's cwd (or use `--path`'s repo).
  2. `herdr agent list` (~37 KB) and `herdr worker list` (~390 KB, whole
     journals) are too big to use: filters (`--repo`, `--cwd`, `--name`)
     and a compact default.
  3. The `todo` skill's step 5 says "exactly this message, no body or
     trailers", but some repositories require a body or trailers: it
     should say "the message the repository's rules require" (herdr's own
     verify keeps subject-only for herdr).
  4. `herdr tab role <tab> worker` is easy to forget: a `--role worker` on
     `worktree create` or `agent start`.
  5. The uncommitted-edits Stop hook fired mid-rebase listing conflict
     resolutions as possibly other sessions' edits: during a rebase, merge
     or cherry-pick it should say so instead.
  6. Claude Code refused `herdr-job run -- env X=Y bash -c "<script>"` (it
     cannot inspect a `bash -c` script for `rm`); a script file passed:
     document "pass a script file, not `bash -c`" with herdr-job.

## Needs a decision

Moved here in the 2026-10-06 triage: each item's last line states what the
user needs to decide or do.

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
  Promoted 2026-10-10 (the user: "choose with the models"; round
  20261010-010542-7df5): the coordinator's write scope and install/push only
  through allowed commands go into the allowlist slice of `t-s3oaxcki`;
  GitHub branch protection and CI stay here as their own slice.
  Moved to "Needs a decision" 2026-10-10 by the coordinator: the coordinator's
  write scope and install/push paths are done in the allowlist (e08a6305).
  Left: GitHub protection of the fork and CI. CI alone is not a hard boundary
  (an agent can still push without it); it becomes one only with a ruleset that
  requires its checks, and a ruleset against force-push conflicts with the
  fork sync, which rebases onto upstream and force-pushes `master`.
  Question: protect the fork's master on GitHub, and how does the fork sync push then?
  Options: a ruleset requiring CI checks and blocking deletion, force-push allowed only to a separate credential the agents do not have (Recommended) | CI checks only, no ruleset (visible, not enforced) | nothing on GitHub; the local allowlist and verify are enough
  Decided 2026-10-10 with the models (round 20261010-142705-3267, all
  three): a GitHub ruleset on rofrol/roherdr `master` requiring CI and
  blocking deletion, force-push allowed only to a separate credential the
  agents do not hold (the fork-sync procedure uses it). Needs the user:
  create that credential (a fine-grained token or a second account) and
  approve the ruleset change on GitHub (outward-facing); CI checks to
  require are listed by the worker that prepares it.
  Asked 2026-10-12: the user creates the credential later; the item waits.
  Asked again 2026-10-12 (d-vo6tzvkm): the user confirmed the ruleset
  with a separate credential; still waits on the user creating it.

- [ ] [t-urqxwnnr] Run vibe-kanban and mini-swe-agent hands-on under failure, before building queue mode?
  Options: vibe-kanban and mini-swe-agent in a throwaway Linux VM with a capped throwaway API key, six scenarios each (Recommended) | read their failure-path code only (~4-8 h, no keys) | skip, build herdr's own semantics
  Checked (user 2026-10-10: "maybe test these open-source projects hands-on? ask the models"; round 20261010-115231-6369, sol + MiMo + DeepSeek): all three pick vibe-kanban (Rust + SQLite + worktrees, its restart marks orphans Failed) and mini-swe-agent (cheap baseline); LangGraph or Restate optional; skip Temporal, Inngest and the OpenHands resolver (heavy infra, documented semantics). Scenarios, same toy repo and fixed prompts (a scripted fake model where supported): success; kill the orchestrator mid-task and restart; kill the agent process; an agent stuck on a silent command; an agent asking a question (restart while it waits); a failed check then retry. Record state rows before and after, resume/duplicate/mark-failed, what the user sees, leftover processes and worktrees, cost, manual steps. Safety: a VM (not a container: agents run arbitrary shell and scan the environment), no host mounts, SSH agent, credentials or real remotes (a local bare repo), egress only to the model API, a spend-capped throwaway key (needs the user), snapshots. Estimate 6-10 h for the two.
  Decided by the user 2026-10-10: VM plus the two projects. Needs from the
  user: a throwaway, spend-capped API key for the agents (Anthropic or
  OpenRouter) put in a file the coordinator names, never pasted into chat.
  VM: OrbStack is installed here (`orb`), a fresh Linux machine without
  host mounts or SSH agent forwarding; the worker runs inside it only.
  Decided by the user 2026-10-10: no API key. mini-swe-agent runs with a
  scripted fake model (no key). vibe-kanban's Claude Code logs in with the
  user's subscription inside the VM (the user runs `claude login` there
  himself; agents never handle the login); after the test: log out and
  delete the VM. VM still to choose (OrbStack shares the Mac's files by
  default; a fresh UTM VM without shared folders is the safe default).
  Selection revisited 2026-10-10 (user asked why only two; round
  20261010-120404-6a53): all three models: two test agent lifecycle but not
  herdr's hard part, durable replay. Add Restate's durable coding-agent
  example as the third (sol, DeepSeek; MiMo preferred LangGraph): a decision
  call recorded once and not repeated on replay, a human wait surviving a
  restart, bounded retries. vibe-kanban's "orphan -> Failed" is the naive
  policy herdr should beat. Add a seventh scenario for every project: a
  side effect done (or a decision recorded), crash before the next
  transition, restart: reused, repeated or reconciled?
  Changed by the user 2026-10-10: drop vibe-kanban and mini-swe-agent;
  test only projects with durable resume. Nothing had started yet. The
  set, decided by the coordinator from the earlier rounds: Restate (single
  binary; its durable coding-agent example), Temporal through its
  single-binary dev server (`temporal server start-dev`, not the full
  deployment the models objected to), LangGraph with its SQLite
  checkpointer and `interrupt()`, DBOS with SQLite if its current version
  supports it (else skip). Each with a scripted fake model (no
  subscription or key needed, so no login in the VM) and the seven
  scenarios, the crash-between-effect-and-record window first. Same VM
  rules.
  Set revised with the models (round 20261010-120557-d572; licences checked
  with gh): Restate (journaled side effects, durable waits; scenario 7
  first) and Temporal's dev server (activity heartbeats, cancellation,
  bounded retry policy; scenarios 3, 4, 6), all three agreeing; LangGraph
  with SQLite only for the human-question wait across a restart (scenario
  5); one Rust + SQLite engine to study as the closest model for herdr:
  microsoft/duroxide (MIT, Rust, SQLite provider, event-sourced replay,
  active) preferred over Obelisk (AGPL-3.0: concepts only). DBOS dropped
  (Postgres-first). Lessons to carry, all three: durable execution does not
  make external effects exactly-once; a step done before its record needs
  an idempotency key or reconciliation, and an explicit "outcome unknown"
  state; a command stuck silently is not a durable-execution feature
  anywhere (it stays herdr's own process supervision, under the delay rule).
  Corrected 2026-10-10 (user: "Postgres doesn't bother me; where do you get
  artificial restrictions like AGPL or Postgres?"; rounds
  20261010-120917-b641 and 20261010-121012-4292): the exclusions were
  wrong. The coordinator mixed "could herdr embed it" with "can we learn
  from crashing it". AGPL binds only copying or linking code into herdr
  (or shipping Obelisk binaries or a modified networked Obelisk), not
  running or reading it in a throwaway VM; Postgres in a VM is trivial.
  Test set now: Temporal (event-history replay, signals, activity retries),
  Restate (journal, durable promises, Rust core), DBOS (effects recorded in
  the same Postgres transaction as the state), Obelisk (Rust + SQLite +
  WASM, persisted execution history, run it), Inngest (step memoization,
  event-resumed waits), duroxide (Rust + SQLite event-sourced replay);
  LangGraph only if time remains. Judge what survives, what repeats, and
  which guarantees still need herdr's own idempotency.
  LangGraph joins the set unconditionally (user 2026-10-10: "only if time
  remains: do we have little time?"; no deadline exists): SQLite
  checkpointer, `interrupt()` for a human question across a restart, and
  which node code reruns before an interrupt.
  Expanded 2026-10-10 (user: "survey HN, Reddit, GitHub trending, X"; a web
  survey, repos verified with gh; round 20261010-121933-62f8, sol + MiMo +
  DeepSeek), grouped by recovery contract, one or two per group:
  - history/journal replay: Temporal (all three: the baseline) and Golem
    (transparent WASM replay: may it silently re-run host effects?);
  - checkpoint-continuation without replay: Orch8 (Rust single binary +
    SQLite, dead-letter state, approval gates; closest to herdr) and Sayiir
    (Rust, no replay);
  - transactional DB steps: DBOS (all three);
  - LLM-decision journaling: Chidori (replay with zero model calls);
  - an effect whose outcome is unknown: tool-journal (all three: stops
    `indeterminate`) and kiso (coding-agent sessions; an unknown effect
    goes to a human).
  Deferred as overlapping: Restate, Obelisk, Inngest, duroxide, LangGraph,
  Trigora, pg_durable, OpenWorkflow, Resonate, and the agent-session tools
  (mjolnir, coven, acpus, Smithers); one joins later only for a specific
  unanswered question. Test-plan corrections: the "effect done, crash
  before the record" scenario uses an external fake-effect service with
  its own durable log, so an unknown outcome is real; every test counts
  model calls and external effects (otherwise replay tests show nothing).
  Herdr's own failures as scenarios (user 2026-10-10: "can you run there
  the scenarios that fail for us?"), added to every project's test, each
  rebuilt in the project's terms with the fake model:
  H1 the step owner ends its turn with a promise and starts nothing (a
     task/agent that returns "will continue" without scheduling): does the
     engine advance by itself?
  H2 a step's own background job still runs when the step reports done
     (turn end is not work end): does the engine wait for the real end?
  H3 a step stuck on a silent command whose pipe orphaned children hold
     (ssh with background processes keeping stderr): what does it see?
  H4 the orchestrator restarts while a client waits on a step (our
     EmptyResponse, exit 8): does the waiter reconnect and keep its place?
  H5 an orphaned child holds a lock or file the next step needs (our
     broker holding the job slot's flock): detected, cleaned, or hung?
  H6 a human question pending while its asker's owner goes away (our
     closed coordinator pane): where does the question go?
  H7 a decision input that goes stale at a known instant (our usage
     window reset): does it re-read at decision time or trust a cache?
  H8 two drivers of one execution in one process after a resume (our run
     lock race): fenced, or both proceed?
  H9 a step "succeeds" but a later gate fails (our commit that broke the
     v1 contract only at install): can it roll back or compensate?
  Where each H scenario belongs (round 20261010-125427-0303, sol + MiMo +
  DeepSeek): engines have no LLM coordinator and no "turn", so run them as
  capability probes, not a ranking.
  - In the eight engines: H7 (stale decision input: re-read or trust a
    cache), H8 (two drivers: fencing or double execution), H9 (a later
    gate fails: compensation), H4 as a durable client wait across a
    restart, H6 as a durable signal or event sent while the asker is gone.
  - As a generic subprocess harness in the same VM (all three: an engine
    joins only what it spawned): H2 (work outliving its step), H3 (a pipe
    held by orphans), H5 (an orphan holding a lock).
  - In herdr's own deterministic tests and stress loops (all three): H1
    (a promise with nothing scheduled), H4's transport errors
    (EmptyResponse, exit 8), H6's pane ownership, H8's same-process race.
  Classes added for every engine: an effect that succeeds but whose
  acknowledgement is lost; duplicate delivery of a step (at-least-once
  retries); cancellation racing completion; a poison step exhausting its
  retries; code or schema changed under a running execution (replay
  nondeterminism); a partial model output.
  Decided 2026-10-10 (the user: "decide with the models"; round
  20261010-142705-3267, all three): a fresh UTM Linux VM without shared
  folders or credentials, snapshot before each scenario, discarded after.
  Asked again 2026-10-12 (d-geajntlu): the user confirmed the throwaway
  VM; still waits on the VM choice and the user's `claude login` there.

### Decide

### Needs you to act or watch
