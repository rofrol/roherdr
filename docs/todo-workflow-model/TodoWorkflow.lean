/-!
# The herdr TODO workflow as a state machine

A hand-written model (see README.md) of how a repository's TODO is worked
through in herdr: the coordination tenure and its handoff, one item's run
through `herdr todo run` and how a landed run is closed, the relay of a
worker's questions to its owner, the read-only supervision that detects
stuck coordinators and workers, and the coordinator's decision at an item
boundary. Commands are
named in comments. Only behavior is modelled; no I/O.

`none` from a `step` function means the command is refused or does not
apply in that state; the state is unchanged. `Run.run` and `Watch.run`
evaluate a script of events and fail (`none`) at the first refused one.
Abstracted away: the content of the cherry-picked commit (verify checks it;
the model trusts it), how the server derives supervision's facts, and the
composition of the parts: `Tenure`, `Repo`, `Git` and `Watch` are separate
machines, so nothing here proves that a frozen
tenure stops worker starts across them.
-/

namespace TodoWorkflow

abbrev PaneId := String
abbrev NoteId := String
abbrev RunId := String

/-! ## The coordination tenure (`herdr coordinator ...`) -/

/-- One per repository, kept by the herdr server. While a handoff is pending
the server also keeps who claimed its note (`herdr handoff claim`): the
first claim wins, so exactly one taker can go on. -/
inductive Tenure where
  | free
  | held (pane : PaneId)
  /-- after `coordinator ready --note`: no worker start, answer or ack -/
  | frozen (pane : PaneId) (note : NoteId) (successor claimant : Option PaneId)
  /-- the frozen pane is gone; the successor, if one was named, stays the only taker -/
  | orphaned (note : NoteId) (successor claimant : Option PaneId)
  deriving DecidableEq, Repr

inductive TenureCmd where
  | start    (pane : PaneId)                  -- coordinator start
  | finish   (pane : PaneId)                  -- coordinator end
  | ready    (pane : PaneId) (note : NoteId)  -- coordinator ready --note
  | handoff  (pane succ : PaneId)             -- coordinator handoff --successor
  | claim    (pane : PaneId)                  -- herdr handoff claim <note>
  | ack      (succ : PaneId)                  -- coordinator ack
  | abort    (pane : PaneId)                  -- coordinator handoff --abort
  | takeOver (pane : PaneId)                  -- a start after the frozen pane is gone
  | paneGone (pane : PaneId)                  -- the pane-close event
  deriving DecidableEq, Repr

/-- `o` without pane `p`: a successor or claimant whose pane closed is forgotten. -/
def dropPane (o : Option PaneId) (p : PaneId) : Option PaneId :=
  if o = some p then none else o

/-- A taker allowed by the named successor, if there is one. -/
def mayTake (successor : Option PaneId) (p : PaneId) : Bool :=
  match successor with
  | none   => true
  | some s => s == p

def Tenure.step : Tenure → TenureCmd → Option Tenure
  | .free,          .start p     => some (.held p)
  | .held h,        .finish p    => if h = p then some .free else none
  | .held h,        .paneGone p  => if h = p then some .free else none
  | .held h,        .ready p n   => if h = p then some (.frozen h n none none) else none
  -- one successor pane, not the frozen pane itself
  -- a note already claimed by another pane keeps that pane as the only taker
  | .frozen h n none c, .handoff p s =>
      if h = p ∧ s ≠ h ∧ mayTake c s then some (.frozen h n (some s) c) else none
  -- the first claim by an allowed taker wins; a second is refused
  | .frozen h n s none, .claim p =>
      if p ≠ h ∧ mayTake s p then some (.frozen h n s (some p)) else none
  | .orphaned n s none, .claim p =>
      if mayTake s p then some (.orphaned n s (some p)) else none
  | .frozen _ _ (some s) (some c), .ack p =>
      if s = p ∧ c = p then some (.held p) else none
  | .frozen h _ _ _, .abort p    => if h = p then some (.held h) else none
  | .frozen h n s c, .paneGone p =>
      if h = p then some (.orphaned n s c)
      -- a successor or claimant that closes no longer blocks the handoff
      else if s = some p ∨ c = some p then some (.frozen h n (dropPane s p) (dropPane c p))
      else none
  | .orphaned n s c, .paneGone p =>
      if s = some p ∨ c = some p then some (.orphaned n (dropPane s p) (dropPane c p))
      else none
  | .orphaned _ s (some c), .takeOver p =>
      if c = p ∧ mayTake s p then some (.held p) else none
  | _,              _            => none

/-- The pane that may act as coordinator, if any: a frozen holder may not. -/
def Tenure.actingHolder : Tenure → Option PaneId
  | .held p => some p
  | _       => none
/-! ## One item's run (`herdr todo run <item> --task --message --paths --check`) -/

inductive Verdict where
  | verified | failed | unavailable
  deriving DecidableEq, Repr

inductive Run where
  | preflight                     -- checks.toml, operations.toml, free disk
  | gate                          -- the usage gate must admit the subscription
  | admissionRefused              -- the run keeps its id (see `Repo`); attached to later
  | working (attempt : Nat)       -- headless worker in the folder slot
  | stopping (attempt : Nat)      -- WORKER-DONE or WORKER-BLOCKED: stop the worker
  | stillAlive (attempt : Nat)    -- stopped but not exited: needs force-stop
  | verifying (attempt : Nat)     -- herdr worker verify
  | unavailable (attempt : Nat)   -- a check could not run: not a pass
  | failed (attempt : Nat)        -- the coordinator may retry with a new task
  | cherryPick
  | installing                    -- [install] argv: just clean-install TODO.md
  | todoEdit                      -- approve --note / --close via todo_edit.py
  | pushing                       -- fast-forward only, never forced
  | done
  | installFailed | todoFailed | pushFailed
  | blocked
  deriving DecidableEq, Repr

inductive Event where
  | preflightOk | preflightRefused
  | gateAdmits | gateRefuses | attach
  | workerDone | workerBlocked
  | exited | stillAliveSeen | forceStop
  | verify (v : Verdict) | verifyAgain | retry
  | picked | pickFailed
  | installed | installFails | retryInstall | skipInstall
  | edited | editFails | retryTodo | skipTodo
  | pushed | pushFails | retryPush
  | abort
  deriving DecidableEq, Repr

def maxAttempts : Nat := 3

def Run.step : Run → Event → Option Run
  | .preflight,    .preflightOk       => some .gate
  | .preflight,    .preflightRefused  => some .blocked
  -- high or unknown usage is never bypassed; the refused run is attached to, not relaunched
  | .gate,         .gateAdmits        => some (.working 1)
  | .gate,         .gateRefuses       => some .admissionRefused
  | .admissionRefused, .attach        => some .gate
  | .working a,    .workerDone        => some (.stopping a)
  | .working a,    .workerBlocked     => some (.stopping a)
  | .stopping a,   .exited            => some (.verifying a)
  | .stopping a,   .stillAliveSeen    => some (.stillAlive a)
  | .stillAlive a, .forceStop         => some (.verifying a)
  | .stillAlive a, .exited            => some (.verifying a)
  | .verifying _,  .verify .verified  => some .cherryPick
  | .verifying a,  .verify .unavailable => some (.unavailable a)
  | .verifying a,  .verify .failed    => some (.failed a)
  | .unavailable a, .verifyAgain      => some (.verifying a)
  | .failed a,     .retry             =>
      if a < maxAttempts then some (.working (a + 1)) else none
  | .cherryPick,   .picked            => some .installing
  | .cherryPick,   .pickFailed        => some .blocked
  | .installing,   .installed         => some .todoEdit
  | .installing,   .installFails      => some .installFailed
  | .installFailed, .retryInstall     => some .installing
  | .installFailed, .skipInstall      => some .todoEdit
  | .todoEdit,     .edited            => some .pushing
  | .todoEdit,     .editFails         => some .todoFailed
  | .todoFailed,   .retryTodo         => some .todoEdit
  | .todoFailed,   .skipTodo          => some .pushing
  | .pushing,      .pushed            => some .done
  | .pushing,      .pushFails         => some .pushFailed
  | .pushFailed,   .retryPush         => some .pushing
  | .failed _,     .abort | .unavailable _, .abort
  | .installFailed, .abort | .todoFailed, .abort | .pushFailed, .abort
                                      => some .blocked
  | _,             _                  => none

/-- Runs the events in order; `none` at the first one that does not apply. -/
def Run.run (r : Run) : List Event → Option Run
  | []      => some r
  | e :: es => (r.step e).bind (·.run es)

/-- The states after a `verified` verdict: the commit is being landed. -/
def Run.landing : Run → Bool
  | .cherryPick | .installing | .todoEdit | .pushing | .done
  | .installFailed | .todoFailed | .pushFailed => true
  | _ => false

/-- The attempt a state belongs to, if it has one. -/
def Run.attempt? : Run → Option Nat
  | .working a | .stopping a | .stillAlive a | .verifying a
  | .unavailable a | .failed a => some a
  | _ => none

/-- Every attempt number in a state lies in `1..maxAttempts`. -/
def Run.attemptOk (r : Run) : Prop :=
  ∀ a, r.attempt? = some a → 1 ≤ a ∧ a ≤ maxAttempts

/-! ## Runs of a repository: identity, the one worker slot, and closing

Every run is kept with its id, its TODO item and the coordination tenure that
owns it. A run id is never reused, an item with an unfinished run (a refused
admission included) is attached to, not launched again, and only one run at
a time holds the worker slot. A landed run is closed one of three ways:
`--next` (the driver starts that item's run), `--stop-reason` (a stop the
user sees until a person acknowledges it) or `--continue-self` (between
runs: the coordinator starts the next run itself, and nobody is asked). A
run started with `--continues <run>` names its predecessor, a durable link,
and clears it; if the owning tenure ends first, that event turns the run
into a stop the user sees. Decided here, after consult round
`20261023-150810-9b94`: the link stays within one tenure (its id survives a
handoff), a predecessor has at most one successor, only a `done` run is
closed, and `--continue-self` is refused once the tenure has ended. -/

abbrev ItemId := String
abbrev TenureId := String

/-- A run that holds the folder slot: from admission until its commit is
cherry-picked or the run ends (verify runs in the slot's worktree too). -/
def Run.busy : Run → Bool
  | .working _ | .stopping _ | .stillAlive _ | .verifying _ | .unavailable _
  | .failed _ | .cherryPick => true
  | _ => false

def Run.finished : Run → Bool
  | .done | .blocked => true
  | _ => false

inductive Disposition where
  | open_                       -- not closed yet
  | next (item : ItemId)        -- --next
  | stop (acked : Bool)         -- --stop-reason, or the owner's tenure ended
  | betweenRuns                 -- --continue-self
  | continuedBy (succ : RunId)  -- a run started with --continues names it
  deriving DecidableEq, Repr

structure RunRec where
  id        : RunId
  item      : ItemId
  owner     : TenureId
  run       : Run
  disp      : Disposition
  continues : Option RunId    -- the durable link to its predecessor
  deriving DecidableEq, Repr

/-- The user is asked about a run only while it is an unacknowledged stop. -/
def RunRec.asksUser (x : RunRec) : Bool := x.disp == .stop false

structure Repo where
  runs  : List RunRec
  ended : List TenureId   -- tenures whose pane closed or agent exited
  deriving Repr

inductive Close where
  | next (item : ItemId) | stop | continueSelf
  deriving DecidableEq, Repr

inductive Actor where
  | user | coordinator
  deriving DecidableEq, Repr

inductive RepoCmd where
  | launch (id : RunId) (item : ItemId) (owner : TenureId) (continues : Option RunId)
  | event (id : RunId) (e : Event)            -- an event of that run (attach included)
  | close (id : RunId) (c : Close)            -- approve with --next / --stop-reason / --continue-self
  | ackStop (who : Actor) (id : RunId)        -- ack-stop: a person saw it
  | tenureEnded (owner : TenureId)            -- the owner's pane closed or agent exited
  deriving Repr

/-- `l` with record `x` replaced by `x'`. -/
def replaceRec (l : List RunRec) (x x' : RunRec) : List RunRec :=
  l.map fun y => if y = x then x' else y

def Repo.step (s : Repo) : RepoCmd → Option Repo
  | .launch id item o k =>
      if s.runs.any (·.id == id) then none
      else if s.runs.any (fun x => x.item == item && !x.run.finished) then none
      else if o ∈ s.ended then none
      else
        let fresh : RunRec := ⟨id, item, o, .preflight, .open_, k⟩
        match k with
        | none => some { s with runs := s.runs ++ [fresh] }
        | some p =>
          match s.runs.find? (·.id == p) with
          | some x =>
            if x.disp = .betweenRuns ∧ x.owner = o then
              some { s with runs := replaceRec s.runs x { x with disp := .continuedBy id } ++ [fresh] }
            else none
          | none => none
  | .event id e =>
      match s.runs.find? (·.id == id) with
      | none => none
      | some x =>
        match x.run.step e with
        | none => none
        | some r' =>
          if r'.busy && s.runs.any (fun y => y.id != id && y.run.busy) then none
          else some { s with runs := s.runs.map fun y => if y.id = id then { y with run := r' } else y }
  | .close id c =>
      match s.runs.find? (·.id == id) with
      | some x =>
        if x.run = .done ∧ x.disp = .open_ then
          match c with
          | .next i => some { s with runs := replaceRec s.runs x { x with disp := .next i } }
          | .stop => some { s with runs := replaceRec s.runs x { x with disp := .stop false } }
          | .continueSelf =>
            if x.owner ∈ s.ended then none
            else some { s with runs := replaceRec s.runs x { x with disp := .betweenRuns } }
        else none
      | none => none
  | .ackStop who id =>
      match who with
      | .coordinator => none
      | .user =>
        match s.runs.find? (·.id == id) with
        | some x =>
          if x.disp = .stop false then
            some { s with runs := replaceRec s.runs x { x with disp := .stop true } }
          else none
        | none => none
  | .tenureEnded o =>
      some { runs := s.runs.map fun y =>
               if y.owner = o ∧ y.disp = .betweenRuns then { y with disp := .stop false } else y,
             ended := o :: s.ended }

def Repo.exec (s : Repo) : List RepoCmd → Option Repo
  | []      => some s
  | c :: cs => (s.step c).bind (·.exec cs)

/-- At most one run holds the worker slot. -/
def Repo.oneWorker (s : Repo) : Prop :=
  ∀ x ∈ s.runs, ∀ y ∈ s.runs, x.run.busy = true → y.run.busy = true → x.id = y.id

/-! ## Landing on master: fast-forward only

`master` and `origin` are histories (oldest first). The cherry-pick appends
the verified worker commit to `master` (its content is trusted, see
`Run.step`); another push may advance `origin` meanwhile; a push succeeds
only when `origin` is a prefix of `master`, otherwise it waits as
`push_failed` and changes nothing. -/

abbrev Commit := String

structure Git where
  master : List Commit
  origin : List Commit
  deriving DecidableEq, Repr

inductive GitEvent where
  | pick (c : Commit)        -- cherry-pick the verified commit onto master
  | otherPush (c : Commit)   -- someone else's commit reaches origin
  | push                     -- git push origin master, never forced
  deriving Repr

/-- The new state, and for a push whether it went through. -/
def Git.step (g : Git) : GitEvent → Git × Bool
  | .pick c      => if c ∈ g.master then (g, false) else ({ g with master := g.master ++ [c] }, true)
  | .otherPush c => if c ∈ g.origin then (g, false) else ({ g with origin := g.origin ++ [c] }, true)
  | .push        =>
      if g.origin.isPrefixOf g.master then ({ g with origin := g.master }, true)
      else (g, false)

def Git.exec (g : Git) : List GitEvent → Git
  | []      => g
  | e :: es => (g.step e).1.exec es
/-! ## Worker questions (the relay in `src/workers/supervisor/questions.rs`)

Not the supervisor of the global rules (that is `Supervision` below): this
relay, part of `WorkerSupervisor`, watches one worker for the pane that
started it (its owner) and turns
the worker's events into numbered obligations of that owner: `herdr worker
wait --attention` returns them, `herdr worker ack <seq>` clears one after it
is handled, and the Stop hook refuses the owner's stop while one is open. A
question is handled once answered (`worker answer --request`) or handed to
the user (`worker escalate`). On the owner's own events (its pane closes,
its agent exits, hits a limit, is blocked on its own question, or ends its
turn with a question unanswered; herdr starts) the relay escalates every
unanswered question to the user and drops the owner's other obligations. It
acts on no timer. (`WorkerSupervisor` also starts, stops and kills workers;
that part is not modelled.)
Questions are accounted for by their number, not their text. -/

inductive Kind where
  | question (request : String)
  | turnEnded
  | exited
  deriving DecidableEq, Repr

structure Obligation where
  seq      : Nat
  kind     : Kind
  answered : Bool
  deriving DecidableEq, Repr

def Obligation.isQuestion (o : Obligation) : Bool :=
  match o.kind with
  | .question _ => true
  | _ => false

def Obligation.unanswered (o : Obligation) : Bool := o.isQuestion && !o.answered

structure Watch where
  nextSeq     : Nat
  obligations : List Obligation   -- not yet acknowledged by the owner
  escalated   : List Nat          -- questions handed to the user's `?` list
  handled     : List Nat          -- acknowledged, or answered when the owner went
  ownerGone   : Bool              -- the owner's pane or agent is gone
  deriving Repr

inductive WorkerEvent where
  | asks (request : String)
  | turnEnds
  | exits
  deriving DecidableEq, Repr

inductive OwnerEvent where
  | answers (seq : Nat)    -- herdr worker answer --request
  | acks (seq : Nat)       -- herdr worker ack <seq>, after handling it
  | escalates (seq : Nat)  -- herdr worker escalate --request
  | gone                   -- pane closed, agent exited or hit a limit,
                           -- blocked, turn ended with a question open
  deriving DecidableEq, Repr

inductive SupAction where
  | wakeOwner     -- the owner's wait returns
  | alertUser     -- the question shows in the user's `?` list
  | nothing
  deriving DecidableEq, Repr

def Watch.onWorker (w : Watch) (e : WorkerEvent) : Watch × SupAction :=
  let kind := match e with
    | .asks r => Kind.question r
    | .turnEnds => .turnEnded
    | .exits => .exited
  if w.ownerGone then
    -- nobody can answer or acknowledge: a question goes straight to the user
    match e with
    | .asks _ => ({ w with nextSeq := w.nextSeq + 1,
                           escalated := w.escalated ++ [w.nextSeq] }, .alertUser)
    | _ => (w, .nothing)
  else
    ({ w with nextSeq := w.nextSeq + 1,
              obligations := w.obligations ++ [⟨w.nextSeq, kind, false⟩] }, .wakeOwner)

/-- `none`: refused (an unknown seq, an answer or escalation of something that
is not an open question, an unanswered question acked). -/
def Watch.ownerStep (w : Watch) : OwnerEvent → Option (Watch × SupAction)
  | .answers s =>
      match w.obligations.find? (·.seq == s) with
      | some o =>
        if o.unanswered then
          some ({ w with obligations := w.obligations.map fun o' =>
                    if o' = o then { o with answered := true } else o' }, .nothing)
        else none
      | none => none
  | .acks s =>
      match w.obligations.find? (·.seq == s) with
      | some o =>
        if o.unanswered then none
        else some ({ w with obligations := w.obligations.erase o,
                            handled := w.handled ++ [o.seq] }, .nothing)
      | none => none
  | .escalates s =>
      match w.obligations.find? (·.seq == s) with
      | some o =>
        if o.unanswered then
          some ({ w with obligations := w.obligations.erase o,
                         escalated := w.escalated ++ [o.seq] }, .alertUser)
        else none
      | none => none
  | .gone =>
      let open_ := (w.obligations.filter (·.unanswered)).map (·.seq)
      let done_ := (w.obligations.filter (fun o => o.isQuestion && o.answered)).map (·.seq)
      some ({ w with obligations := [], escalated := w.escalated ++ open_,
                     handled := w.handled ++ done_, ownerGone := true },
            if open_.isEmpty then .nothing else .alertUser)

/-- A gone owner does nothing more. -/
def Watch.onOwner (w : Watch) (e : OwnerEvent) : Option (Watch × SupAction) :=
  if w.ownerGone then none else w.ownerStep e

/-- Every number in use (open, escalated or handled) is below `nextSeq`, so a
new one is fresh. -/
def Watch.seqsBelow (w : Watch) : Prop :=
  (∀ o ∈ w.obligations, o.seq < w.nextSeq) ∧
  (∀ s ∈ w.escalated, s < w.nextSeq) ∧ (∀ s ∈ w.handled, s < w.nextSeq)

inductive SupEvent where
  | worker (e : WorkerEvent)
  | owner (e : OwnerEvent)

def Watch.step (w : Watch) : SupEvent → Option Watch
  | .worker e => some (w.onWorker e).1
  | .owner e  => (w.onOwner e).map (·.1)

def Watch.run (w : Watch) : List SupEvent → Option Watch
  | []      => some w
  | e :: es => (w.step e).bind (·.run es)

/-- The Stop hook: the owner may end its turn only with nothing open. -/
def Watch.ownerMayStop (w : Watch) : Bool := w.obligations.isEmpty

/-- Question number `s` is accounted for: open, handed to the user, or
answered and acknowledged. -/
def Watch.accounts (w : Watch) (s : Nat) : Prop :=
  (∃ o ∈ w.obligations, o.seq = s ∧ o.isQuestion = true) ∨ s ∈ w.escalated ∨ s ∈ w.handled
/-! ## Supervision (`src/workers/supervision.rs`): the read-only sweep

This is the supervisor the global rules meant: a deterministic check that
coordinators and workers are not stuck. It joins what the server already
knows about each subject into facts, checks named predicates against them,
and records each predicate that holds as a finding in an append-only ledger.
A new finding lists a `?` entry for the user and queues a notice to the
coordinator; only the user or the herdr repository's coordinator acknowledges
one, and a finding no longer seen is resolved. It never answers, prompts,
kills, restarts or marks work done. It runs when a coordinator's pane
reports a turn or changes state, and on a timer (see README). The facts are
abstract inputs here: how the server derives them is not modelled. -/

/-- What the server knows about one subject (a coordinator or an assignment). -/
structure Facts where
  subject          : String
  isCoordinator    : Bool
  agentIdle        : Bool
  runnableItems    : Bool   -- its repository has runnable TODO items
  openDecision     : Bool   -- a decision waits on the user
  knownWait        : Bool   -- a wait the server knows of (job, worker, run)
  workingAfterStop : Bool   -- shows working after its reported turn end
  untrackedShells  : Bool   -- more background tasks than tracked waits
  verdictUnacked   : Bool   -- a verdict recorded, no waiter acknowledged it
  stoppedNoVerdict : Bool   -- a worker's turn ended without a WORKER line
  unaccounted      : Bool   -- an accepted assignment nothing accounts for
  diskLow          : Bool   -- under the build guard's free space
  deriving DecidableEq, Repr

inductive Predicate where
  | coordinatorIdle | workingAfterStop | untrackedShell | diskLow
  | verdictUndelivered | stoppedWithoutVerdict | unaccountedAssignment
  deriving DecidableEq, Repr

/-- Each predicate is a positive fact plus the event that did not follow;
quiet output alone is never a finding. -/
def Predicate.holds : Predicate → Facts → Bool
  | .coordinatorIdle, f =>
      f.isCoordinator && f.agentIdle && f.runnableItems && !f.openDecision && !f.knownWait
  | .workingAfterStop, f      => f.workingAfterStop
  | .untrackedShell, f        => f.isCoordinator && f.untrackedShells
  | .diskLow, f               => f.isCoordinator && f.diskLow
  | .verdictUndelivered, f    => f.verdictUnacked
  | .stoppedWithoutVerdict, f => f.stoppedNoVerdict
  | .unaccountedAssignment, f => f.unaccounted

def allPredicates : List Predicate :=
  [.coordinatorIdle, .workingAfterStop, .untrackedShell, .diskLow,
   .verdictUndelivered, .stoppedWithoutVerdict, .unaccountedAssignment]

structure Finding where
  predicate : Predicate
  subject   : String
  deriving DecidableEq, Repr

/-- A sweep's findings: a pure function of the facts. -/
def detect (snap : List Facts) : List Finding :=
  snap.flatMap fun f => (allPredicates.filter (·.holds f)).map fun p => ⟨p, f.subject⟩

inductive FindingState where
  | open_ | acknowledged | resolved
  deriving DecidableEq, Repr

/-- Append-only: rows are never deleted, only resolved. -/
structure Ledger where
  entries : List (Finding × FindingState)
  deriving Repr

def Ledger.active (l : Ledger) (x : Finding) : Bool :=
  l.entries.any fun e => e.1 == x && e.2 != .resolved

/-- What a sweep opens: each detected finding once, unless it is already
active. Each one lists a `?` entry for the user and queues a notice to its
coordinator. -/
def Ledger.newFindings (l : Ledger) (snap : List Facts) : List Finding :=
  (detect snap).eraseDups.filter fun x => !l.active x

/-- Resolve what is no longer seen, then open what is new. -/
def Ledger.sweep (l : Ledger) (snap : List Facts) : Ledger :=
  let found := detect snap
  let kept := l.entries.map fun e =>
    if e.2 != .resolved && !found.contains e.1 then (e.1, .resolved) else e
  { entries := kept ++ (l.newFindings snap).map fun x => (x, .open_) }

inductive Acker where
  | user                    -- a click in their own client's `?` list
  | herdrCoordinator        -- supervise.acknowledge from its tenure's pane
  | agent (pane : PaneId)   -- any other agent, the stalled one included
  deriving DecidableEq, Repr

/-- Refused for an agent, and for a finding with no open row. -/
def Ledger.ack (l : Ledger) (who : Acker) (x : Finding) : Option Ledger :=
  match who with
  | .agent _ => none
  | _ =>
    if l.entries.any (fun e => e.1 == x && e.2 == .open_) then
      some { entries := l.entries.map fun e =>
               if e.1 = x ∧ e.2 = .open_ then (e.1, .acknowledged) else e }
    else none

/-- Everything the workflow keeps, with the supervision ledger beside it. -/
structure System where
  tenure : Tenure
  repo   : Repo
  git    : Git
  relay  : Watch
  ledger : Ledger

def System.supervise (sys : System) (snap : List Facts) : System :=
  { sys with ledger := sys.ledger.sweep snap }

/-! ## The coordinator (an LLM session in a herdr pane) -/

inductive Item where
  | next (id : String)
  | needsDecision (id : String) (question : String)
  deriving Repr

structure Coordinator where
  pane       : Option PaneId   -- none: outside a herdr pane
  tenure     : Tenure          -- the repository's, as the server reports it
  contextPct : Nat
  queue      : List Item
  deriving Repr

inductive Action where
  | refuseOutsidePane      -- offer `herdr coordinator open`, stop
  | refuseNoTenure         -- another pane coordinates, or a handoff is pending
  | handOff                -- over 70% at an item boundary
  | askUser                -- every remaining item waits on the user
  | dispatch (id : String) -- herdr todo run for the top "Next" item
  | stop                   -- "Next, in order" is empty
  deriving DecidableEq, Repr

def handoffAt : Nat := 70

def Coordinator.decide (c : Coordinator) : Action :=
  match c.pane with
  | none => .refuseOutsidePane
  | some p =>
    if c.tenure.actingHolder ≠ some p then .refuseNoTenure
    else if c.queue.isEmpty then .stop
    else if c.contextPct > handoffAt then .handOff
    else match c.queue.findSome? (fun | .next id => some id | _ => none) with
      | some id => .dispatch id
      | none => .askUser

/-! ## Properties Lean checks -/

section Tenure

/-- Only the holder ends a tenure. -/
theorem finish_only_holder (h p : PaneId) (hp : h ≠ p) :
    (Tenure.held h).step (.finish p) = none := by
  simp [Tenure.step, hp]

/-- Any tenure that is not free refuses a plain start. -/
theorem no_second_start (t : Tenure) (p : PaneId) (ht : t ≠ .free) :
    t.step (.start p) = none := by
  cases t <;> simp_all [Tenure.step]

/-- The note's claim is exclusive: once claimed, every other claim is refused. -/
theorem claim_exclusive (h p q : PaneId) (n : NoteId) (s : Option PaneId) :
    (Tenure.frozen h n s (some q)).step (.claim p) = none ∧
    (Tenure.orphaned n s (some q)).step (.claim p) = none := by
  simp [Tenure.step]

/-- Only the named successor that also won the claim acks, and it gets the tenure. -/
theorem ack_needs_successor_and_claim (h p : PaneId) (n : NoteId) (s c : Option PaneId)
    (t : Tenure) (hs : (Tenure.frozen h n s c).step (.ack p) = some t) :
    s = some p ∧ c = some p ∧ t = .held p := by
  cases s <;> cases c <;> simp [Tenure.step] at hs
  obtain ⟨⟨rfl, rfl⟩, rfl⟩ := hs
  exact ⟨rfl, rfl, rfl⟩

/-- One successor per handoff, and never the frozen pane itself. -/
theorem one_successor (h p s s' : PaneId) (n : NoteId) (c : Option PaneId) :
    (Tenure.frozen h n (some s) c).step (.handoff p s') = none ∧
    (Tenure.frozen h n none c).step (.handoff p h) = none := by
  simp [Tenure.step]

/-- The holder names a successor other than itself, compatible with any claim. -/
theorem handoff_names_successor (h s : PaneId) (n : NoteId) (c : Option PaneId)
    (hs : s ≠ h) (hc : mayTake c s = true) :
    (Tenure.frozen h n none c).step (.handoff h s) = some (.frozen h n (some s) c) := by
  simp [Tenure.step, hs, hc]

/-- No claim by the frozen pane or by a pane the named successor excludes, so
a wrong first claim cannot block the handoff. -/
theorem claim_only_allowed_taker (h p : PaneId) (n : NoteId) (s : Option PaneId) (t : Tenure)
    (hs : (Tenure.frozen h n s none).step (.claim p) = some t) :
    p ≠ h ∧ mayTake s p = true := by
  simp [Tenure.step] at hs
  exact ⟨hs.1.1, hs.1.2⟩

/-- When the claimant's pane closes, the note can be claimed again, so a dead
claimant never locks the handoff. -/
theorem claimant_gone_reopens (n : NoteId) (s : Option PaneId) (c : PaneId) :
    (Tenure.orphaned n s (some c)).step (.paneGone c) =
      some (.orphaned n (dropPane s c) none) := by
  simp [Tenure.step, dropPane]

/-- When a named successor's pane closes, the frozen holder can name another. -/
theorem successor_gone_reopens (h s : PaneId) (n : NoteId) (hs : s ≠ h) :
    (Tenure.frozen h n (some s) none).step (.paneGone s) =
      some (.frozen h n none none) := by
  simp [Tenure.step, dropPane, Ne.symm hs]

/-- After the frozen pane is gone, only the pane that won the claim takes the
tenure, and only if it is the named successor or none was named. -/
theorem orphaned_taken_only_by_claimant (n : NoteId) (s c : Option PaneId)
    (cmd : TenureCmd) (p : PaneId)
    (h : (Tenure.orphaned n s c).step cmd = some (.held p)) :
    c = some p ∧ mayTake s p = true := by
  cases cmd <;> cases c <;> simp [Tenure.step] at h
  obtain ⟨⟨rfl, hm⟩, rfl⟩ := h
  exact ⟨rfl, hm⟩

end Tenure

section Run

/-- A single step that enters landing comes from a `verified` verdict. -/
theorem step_into_landing (r r' : Run) (e : Event)
    (h : r.step e = some r') (hr : r.landing = false) (hr' : r'.landing = true) :
    e = .verify .verified := by
  cases r <;> cases e <;> simp only [Run.step] at h <;>
    (try (rename_i v; cases v <;> simp only at h)) <;>
    (try split at h) <;> simp at h <;> subst_vars <;> simp_all [Run.landing]

/-- Nothing lands without a `verified` verdict, over any sequence of events. -/
theorem landing_needs_verified (es : List Event) (r r' : Run)
    (hrun : r.run es = some r') (hr : r.landing = false) (hr' : r'.landing = true) :
    Event.verify .verified ∈ es := by
  induction es generalizing r with
  | nil => simp [Run.run] at hrun; subst hrun; simp_all
  | cons e es ih =>
    simp only [Run.run, Option.bind_eq_some_iff] at hrun
    obtain ⟨m, hm, hrest⟩ := hrun
    cases hml : m.landing
    · exact List.mem_cons_of_mem _ (ih m hrest hml)
    · rw [step_into_landing r m e hm hr hml]; exact List.mem_cons_self

/-- From preflight, nothing reaches `done` without `verified`. -/
theorem done_needs_verified (es : List Event)
    (h : Run.preflight.run es = some .done) : Event.verify .verified ∈ es :=
  landing_needs_verified es .preflight .done h rfl rfl

/-- A step keeps every attempt number within `1..maxAttempts`. -/
theorem step_attemptOk (r r' : Run) (e : Event)
    (h : r.step e = some r') (hr : r.attemptOk) : r'.attemptOk := by
  unfold Run.attemptOk at *
  intro a ha
  cases r <;> cases e <;> simp only [Run.step] at h <;>
    (try (rename_i v; cases v <;> simp only at h)) <;>
    (try split at h) <;> simp at h <;> subst_vars <;>
    simp_all [Run.attempt?, maxAttempts] <;> omega

/-- No fourth attempt, over any sequence of events. -/
theorem attempts_bounded (es : List Event) (r r' : Run)
    (hrun : r.run es = some r') (hr : r.attemptOk) : r'.attemptOk := by
  induction es generalizing r with
  | nil => simp [Run.run] at hrun; subst hrun; exact hr
  | cons e es ih =>
    simp only [Run.run, Option.bind_eq_some_iff] at hrun
    obtain ⟨m, hm, hrest⟩ := hrun
    exact ih m hrest (step_attemptOk r m e hm hr)

/-- A refused admission never starts a worker by itself: only an attach of
that run goes back to the gate. -/
theorem refused_admission_waits (e : Event) (r : Run)
    (h : Run.admissionRefused.step e = some r) : r = .gate ∧ e = .attach := by
  cases e <;> simp [Run.step] at h
  exact ⟨h.symm, rfl⟩

/-- The start of a run satisfies the attempt bound ... -/
theorem preflight_attemptOk : Run.preflight.attemptOk := by
  intro a h; simp [Run.attempt?] at h

/-- ... so no run from preflight ever reaches a fourth attempt. -/
theorem attempts_bounded_from_preflight (es : List Event) (r : Run)
    (h : Run.preflight.run es = some r) : r.attemptOk :=
  attempts_bounded es .preflight r h preflight_attemptOk

/-- Landing is a trap: once a commit is being landed, every step stays in
landing or blocks; no step goes back to a worker or a verify. -/
theorem landing_is_trap (r r' : Run) (e : Event)
    (h : r.step e = some r') (hr : r.landing = true) :
    r'.landing = true ∨ r' = .blocked := by
  cases r <;> cases e <;> simp only [Run.step] at h <;>
    (try (rename_i v; cases v <;> simp only at h)) <;>
    (try split at h) <;> simp at h <;> subst_vars <;> simp_all [Run.landing]

/-- Over any sequence of events, a run that is landing stays landing or blocks. -/
theorem landing_stays (es : List Event) (r r' : Run)
    (hrun : r.run es = some r') (hr : r.landing = true ∨ r = .blocked) :
    r'.landing = true ∨ r' = .blocked := by
  induction es generalizing r with
  | nil => simp [Run.run] at hrun; subst hrun; exact hr
  | cons e es ih =>
    simp only [Run.run, Option.bind_eq_some_iff] at hrun
    obtain ⟨m, hm, hrest⟩ := hrun
    apply ih m hrest
    rcases hr with hr | rfl
    · exact landing_is_trap r m e hm hr
    · cases e <;> simp [Run.step] at hm

end Run

section Repo

/-- A run id is never reused. -/
theorem launch_new_id (s : Repo) (id : RunId) (item : ItemId) (o : TenureId)
    (k : Option RunId) (h : ∃ x ∈ s.runs, x.id = id) :
    s.step (.launch id item o k) = none := by
  obtain ⟨x, hx, rfl⟩ := h
  have : s.runs.any (·.id == x.id) = true := List.any_eq_true.2 ⟨x, hx, by simp⟩
  simp [Repo.step, this]

/-- An item with an unfinished run (a refused admission included) is never
launched again: that run is attached to. -/
theorem no_relaunch (s : Repo) (id : RunId) (item : ItemId) (o : TenureId)
    (k : Option RunId) (h : ∃ x ∈ s.runs, x.item = item ∧ x.run.finished = false) :
    s.step (.launch id item o k) = none := by
  obtain ⟨x, hx, rfl, hf⟩ := h
  have : s.runs.any (fun y => y.item == x.item && !y.run.finished) = true :=
    List.any_eq_true.2 ⟨x, hx, by simp [hf]⟩
  simp only [Repo.step, this]
  split <;> simp

/-- The slot invariant follows when every busy record of the new state was
already busy, under the same id, in the old one. -/
theorem oneWorker_of_busy_from (s s' : Repo) (h : s.oneWorker)
    (hsub : ∀ y ∈ s'.runs, y.run.busy = true → ∃ x ∈ s.runs, x.id = y.id ∧ x.run.busy = true) :
    s'.oneWorker := by
  intro x hx y hy bx byy
  obtain ⟨x0, hx0, hxi, bx0⟩ := hsub x hx bx
  obtain ⟨y0, hy0, hyi, by0⟩ := hsub y hy byy
  rw [← hxi, ← hyi]; exact h x0 hx0 y0 hy0 bx0 by0

/-- Replacing a record by one with the same id and run keeps busy records. -/
theorem replace_busy_from (l : List RunRec) (x x' : RunRec) (y : RunRec)
    (hy : y ∈ replaceRec l x x') (hb : y.run.busy = true) (hid : x'.id = x.id)
    (hrun : x'.run = x.run) :
    ∃ z ∈ l, z.id = y.id ∧ z.run.busy = true := by
  obtain ⟨z, hz, rfl⟩ := List.mem_map.1 hy
  refine ⟨z, hz, ?_, ?_⟩ <;> by_cases hzx : z = x <;> simp_all

theorem step_oneWorker (s s' : Repo) (c : RepoCmd)
    (h : s.oneWorker) (hs : s.step c = some s') : s'.oneWorker := by
  cases c with
  | launch id item o k =>
    apply oneWorker_of_busy_from s s' h
    simp only [Repo.step] at hs
    split at hs; · simp at hs
    split at hs; · simp at hs
    split at hs; · simp at hs
    intro y hy hb
    split at hs
    · simp only [Option.some.injEq] at hs; subst hs
      rcases List.mem_append.1 hy with hy | hy
      · exact ⟨y, hy, rfl, hb⟩
      · simp at hy; subst hy; simp [Run.busy] at hb
    · split at hs
      · rename_i x _
        split at hs
        · simp only [Option.some.injEq] at hs; subst hs
          rcases List.mem_append.1 hy with hy | hy
          · exact replace_busy_from _ x _ y hy hb rfl rfl
          · simp at hy; subst hy; simp [Run.busy] at hb
        · simp at hs
      · simp at hs
  | event id e =>
    simp only [Repo.step] at hs
    split at hs
    · simp at hs
    · split at hs
      · simp at hs
      · rename_i r' _
        split at hs
        · simp at hs
        · rename_i hguard
          simp only [Option.some.injEq] at hs
          subst hs
          have other : r'.busy = true → ∀ y ∈ s.runs, y.run.busy = true → y.id = id := by
            intro hb y hy hby
            refine Classical.byContradiction fun hne => hguard ?_
            simp only [hb, Bool.true_and]
            exact List.any_eq_true.2 ⟨y, hy, by simp [hne, hby]⟩
          have hid : ∀ y : RunRec,
              (if y.id = id then { y with run := r' } else y).id = y.id := by
            intro y; split <;> rfl
          have hbusy : ∀ y : RunRec,
              (if y.id = id then { y with run := r' } else y).run.busy = true →
              (y.id = id ∧ r'.busy = true) ∨ y.run.busy = true := by
            intro y hb; split at hb
            · exact Or.inl ⟨by assumption, hb⟩
            · exact Or.inr hb
          intro x hx y hy bx byy
          simp only [List.mem_map] at hx hy
          obtain ⟨x0, hx0, rfl⟩ := hx
          obtain ⟨y0, hy0, rfl⟩ := hy
          rw [hid, hid]
          rcases hbusy x0 bx with ⟨hx, bx'⟩ | bx' <;> rcases hbusy y0 byy with ⟨hy', by'⟩ | by'
          · rw [hx, hy']
          · rw [hx]; exact (other bx' y0 hy0 by').symm
          · rw [hy']; exact other by' x0 hx0 bx'
          · exact h x0 hx0 y0 hy0 bx' by'
  | close id c =>
    apply oneWorker_of_busy_from s s' h
    intro y hy hb
    simp only [Repo.step] at hs
    split at hs
    · rename_i x _
      split at hs
      · cases c <;> simp only [Option.some.injEq] at hs
        · subst hs; exact replace_busy_from _ x _ y hy hb rfl rfl
        · subst hs; exact replace_busy_from _ x _ y hy hb rfl rfl
        · split at hs; · simp at hs
          simp only [Option.some.injEq] at hs; subst hs
          exact replace_busy_from _ x _ y hy hb rfl rfl
      · simp at hs
    · simp at hs
  | ackStop who id =>
    apply oneWorker_of_busy_from s s' h
    intro y hy hb
    cases who
    · simp only [Repo.step] at hs
      split at hs
      · rename_i x _
        split at hs
        · simp only [Option.some.injEq] at hs; subst hs
          exact replace_busy_from _ x _ y hy hb rfl rfl
        · simp at hs
      · simp at hs
    · simp [Repo.step] at hs
  | tenureEnded o =>
    apply oneWorker_of_busy_from s s' h
    intro y hy hb
    simp only [Repo.step, Option.some.injEq] at hs; subst hs
    obtain ⟨z, hz, rfl⟩ := List.mem_map.1 hy
    refine ⟨z, hz, ?_, ?_⟩
    · split <;> rfl
    · split at hb <;> exact hb

/-- Over any sequence of commands, at most one run holds the worker slot. -/
theorem one_worker (cs : List RepoCmd) (s s' : Repo)
    (h : s.oneWorker) (hrun : s.exec cs = some s') : s'.oneWorker := by
  induction cs generalizing s with
  | nil => simp [Repo.exec] at hrun; subst hrun; exact h
  | cons c cs ih =>
    simp only [Repo.exec, Option.bind_eq_some_iff] at hrun
    obtain ⟨m, hm, hrest⟩ := hrun
    exact ih m (step_oneWorker s m c h hm) hrest

/-- `--continue-self` never asks the user: a run between runs is not a stop. -/
theorem between_runs_asks_nobody (x : RunRec) (h : x.disp = .betweenRuns) :
    x.asksUser = false := by
  simp [RunRec.asksUser, h]

/-- Only a person acknowledges a stop; a coordinator never does. -/
theorem coordinator_cannot_ack_stop (s : Repo) (id : RunId) :
    s.step (.ackStop .coordinator id) = none := by
  simp [Repo.step]

/-- The link is explicit: a launch with `--continues p` goes through only when
`p` is between runs, owned by the same tenure, and the new run records `p`. -/
theorem continues_is_explicit (s s' : Repo) (id p : RunId) (item : ItemId) (o : TenureId)
    (hs : s.step (.launch id item o (some p)) = some s') :
    (∃ x ∈ s.runs, x.id = p ∧ x.disp = .betweenRuns ∧ x.owner = o) ∧
    (⟨id, item, o, .preflight, .open_, some p⟩ : RunRec) ∈ s'.runs := by
  simp only [Repo.step] at hs
  split at hs; · simp at hs
  split at hs; · simp at hs
  split at hs; · simp at hs
  split at hs
  · rename_i x hf
    split at hs
    · rename_i hc
      simp only [Option.some.injEq] at hs; subst hs
      have hx := List.mem_of_find?_eq_some hf
      have hid : x.id = p := by simpa using List.find?_some hf
      exact ⟨⟨x, hx, hid, hc.1, hc.2⟩, List.mem_append_right _ (by simp)⟩
    · simp at hs
  · simp at hs

/-- The launch also clears the predecessor: its row now names the successor. -/
theorem continues_clears (s s' : Repo) (id p : RunId) (item : ItemId) (o : TenureId)
    (hs : s.step (.launch id item o (some p)) = some s') :
    ∃ x ∈ s.runs, x.id = p ∧ x.disp = .betweenRuns ∧
      { x with disp := .continuedBy id } ∈ s'.runs := by
  simp only [Repo.step] at hs
  split at hs; · simp at hs
  split at hs; · simp at hs
  split at hs; · simp at hs
  split at hs
  · rename_i x hf
    split at hs
    · rename_i hc
      simp only [Option.some.injEq] at hs; subst hs
      have hx := List.mem_of_find?_eq_some hf
      have hid : x.id = p := by simpa using List.find?_some hf
      exact ⟨x, hx, hid, hc.1, List.mem_append_left _ (List.mem_map.2 ⟨x, hx, by simp⟩)⟩
    · simp at hs
  · simp at hs

/-- Only a landed run that is still open is closed. -/
theorem close_only_done (s s' : Repo) (id : RunId) (c : Close)
    (hs : s.step (.close id c) = some s') :
    ∃ x ∈ s.runs, x.id = id ∧ x.run = .done ∧ x.disp = .open_ := by
  simp only [Repo.step] at hs
  split at hs
  · rename_i x hf
    split at hs
    · rename_i hc
      exact ⟨x, List.mem_of_find?_eq_some hf, by simpa using List.find?_some hf, hc.1, hc.2⟩
    · simp at hs
  · simp at hs

/-- If the owning tenure ends while a run is between runs, the run becomes a
stop the user sees. -/
theorem tenure_end_surfaces (s : Repo) (x : RunRec) (hx : x ∈ s.runs)
    (hb : x.disp = .betweenRuns) :
    ∃ s', s.step (.tenureEnded x.owner) = some s' ∧
      ∃ y ∈ s'.runs, y.id = x.id ∧ y.asksUser = true := by
  refine ⟨_, rfl, { x with disp := .stop false }, ?_, rfl, by simp [RunRec.asksUser]⟩
  exact List.mem_map.2 ⟨x, hx, by simp [hb]⟩

/-- A pending stop never disappears unless a person acknowledges it. -/
theorem stop_stays_until_user_ack (s s' : Repo) (c : RepoCmd) (x : RunRec)
    (hx : x ∈ s.runs) (hd : x.disp = .stop false) (hs : s.step c = some s')
    (hc : c ≠ .ackStop .user x.id) :
    ∃ y ∈ s'.runs, y.id = x.id ∧ y.disp = .stop false := by
  have keep : ∀ (a a' : RunRec), a.disp ≠ .stop false →
      ∃ y ∈ replaceRec s.runs a a', y.id = x.id ∧ y.disp = .stop false := by
    intro a a' ha
    refine ⟨x, List.mem_map.2 ⟨x, hx, ?_⟩, rfl, hd⟩
    have : x ≠ a := fun e => ha (e ▸ hd)
    simp [this]
  cases c with
  | launch id item o k =>
    simp only [Repo.step] at hs
    split at hs; · simp at hs
    split at hs; · simp at hs
    split at hs; · simp at hs
    split at hs
    · simp only [Option.some.injEq] at hs; subst hs
      exact ⟨x, List.mem_append_left _ hx, rfl, hd⟩
    · split at hs
      · rename_i a _
        split at hs
        · rename_i ha
          simp only [Option.some.injEq] at hs; subst hs
          obtain ⟨y, hy, h1, h2⟩ := keep a _ (by rw [ha.1]; simp)
          exact ⟨y, List.mem_append_left _ hy, h1, h2⟩
        · simp at hs
      · simp at hs
  | event id e =>
    simp only [Repo.step] at hs
    split at hs; · simp at hs
    split at hs; · simp at hs
    split at hs; · simp at hs
    simp only [Option.some.injEq] at hs; subst hs
    refine ⟨_, List.mem_map.2 ⟨x, hx, rfl⟩, ?_, ?_⟩ <;> split <;> simp [hd]
  | close id c =>
    simp only [Repo.step] at hs
    split at hs
    · rename_i a _
      split at hs
      · rename_i ha
        have hna : a.disp ≠ .stop false := by rw [ha.2]; simp
        cases c <;> simp only [Option.some.injEq] at hs
        · subst hs; exact keep a _ hna
        · subst hs; exact keep a _ hna
        · split at hs; · simp at hs
          simp only [Option.some.injEq] at hs; subst hs
          exact keep a _ hna
      · simp at hs
    · simp at hs
  | ackStop who id =>
    cases who
    · simp only [Repo.step] at hs
      split at hs
      · rename_i a hf
        split at hs
        · simp only [Option.some.injEq] at hs; subst hs
          have hid : a.id = id := by simpa using List.find?_some hf
          by_cases hax : x = a
          · subst hax; exact absurd (by rw [hid]) hc
          · exact ⟨x, List.mem_map.2 ⟨x, hx, by simp [hax]⟩, rfl, hd⟩
        · simp at hs
      · simp at hs
    · simp [Repo.step] at hs
  | tenureEnded o =>
    simp only [Repo.step, Option.some.injEq] at hs; subst hs
    exact ⟨x, List.mem_map.2 ⟨x, hx, by simp [hd]⟩, rfl, hd⟩

end Repo


section Git

/-- A push that goes through is a fast-forward: what origin had stays its prefix. -/
theorem push_fast_forward (g : Git) (h : (g.step .push).2 = true) :
    g.origin <+: (g.step .push).1.origin := by
  unfold Git.step at *
  by_cases hp : g.origin.isPrefixOf g.master = true
  · simp only [hp, ↓reduceIte]; exact List.isPrefixOf_iff_prefix.1 hp
  · simp only [hp, ↓reduceIte, Bool.false_eq_true] at h

/-- A push that is not a fast-forward changes nothing and reports failure. -/
theorem non_ff_push_waits (g : Git) (h : g.origin.isPrefixOf g.master = false) :
    g.step .push = (g, false) := by
  simp [Git.step, h]

/-- Origin's history is never rewritten, over any events. -/
theorem origin_only_grows (es : List GitEvent) (g : Git) :
    g.origin <+: (g.exec es).origin := by
  induction es generalizing g with
  | nil => exact List.prefix_refl _
  | cons e es ih =>
    refine List.IsPrefix.trans ?_ (ih _)
    cases e with
    | pick c => simp only [Git.step]; split <;> exact List.prefix_refl _
    | otherPush c =>
      simp only [Git.step]; split
      · exact List.prefix_refl _
      · exact List.prefix_append _ _
    | push =>
      unfold Git.step
      by_cases hp : g.origin.isPrefixOf g.master = true
      · simp only [hp, ↓reduceIte]; exact List.isPrefixOf_iff_prefix.1 hp
      · simp only [hp, ↓reduceIte, Bool.false_eq_true]; exact List.prefix_refl _

/-- Master's history is never rewritten either. -/
theorem master_only_grows (es : List GitEvent) (g : Git) :
    g.master <+: (g.exec es).master := by
  induction es generalizing g with
  | nil => exact List.prefix_refl _
  | cons e es ih =>
    refine List.IsPrefix.trans ?_ (ih _)
    cases e with
    | pick c =>
      simp only [Git.step]; split
      · exact List.prefix_refl _
      · exact List.prefix_append _ _
    | otherPush c => simp only [Git.step]; split <;> exact List.prefix_refl _
    | push =>
      unfold Git.step
      by_cases hp : g.origin.isPrefixOf g.master = true
      · simp only [hp, ↓reduceIte]; exact List.prefix_refl _
      · simp only [hp, ↓reduceIte, Bool.false_eq_true]; exact List.prefix_refl _

end Git
section WorkerQuestions

/-- Every worker event wakes the owner and leaves it an obligation, so the
owner cannot stop before acknowledging it. -/
theorem worker_event_blocks_stop (w : Watch) (e : WorkerEvent) (hg : w.ownerGone = false) :
    (w.onWorker e).2 = .wakeOwner ∧ (w.onWorker e).1.ownerMayStop = false := by
  cases e <;> simp [Watch.onWorker, Watch.ownerMayStop, hg]

/-- After the owner is gone, worker events leave no obligation and never wake it. -/
theorem gone_owner_not_woken (w : Watch) (e : WorkerEvent) (hg : w.ownerGone = true) :
    (w.onWorker e).1.obligations = w.obligations ∧ (w.onWorker e).2 ≠ .wakeOwner := by
  cases e <;> simp [Watch.onWorker, hg]

/-- An asked question is accounted for at once, under its own number. -/
theorem asked_accounted (w : Watch) (r : String) :
    (w.onWorker (.asks r)).1.accounts w.nextSeq := by
  cases hg : w.ownerGone
  · left
    exact ⟨⟨w.nextSeq, .question r, false⟩, by simp [Watch.onWorker, hg],
      rfl, by simp [Obligation.isQuestion]⟩
  · right; left; simp [Watch.onWorker, hg]

theorem onWorker_accounts (w : Watch) (e : WorkerEvent) (s : Nat)
    (h : w.accounts s) : (w.onWorker e).1.accounts s := by
  rcases h with ⟨o, ho, hk⟩ | h | h
  · left; refine ⟨o, ?_, hk⟩
    cases hg : w.ownerGone <;> cases e <;> simp [Watch.onWorker, hg, ho]
  · right; left; cases hg : w.ownerGone <;> cases e <;> simp [Watch.onWorker, hg, h]
  · right; right; cases hg : w.ownerGone <;> cases e <;> simp [Watch.onWorker, hg, h]

theorem ownerStep_accounts (w w' : Watch) (a : SupAction) (e : OwnerEvent) (s : Nat)
    (h : w.accounts s) (he : w.ownerStep e = some (w', a)) : w'.accounts s := by
  cases e with
  | answers t =>
    simp only [Watch.ownerStep] at he
    split at he
    · rename_i o _
      split at he
      · simp only [Option.some.injEq, Prod.mk.injEq] at he
        obtain ⟨rfl, -⟩ := he
        rcases h with ⟨o', ho', hk⟩ | h | h
        · refine Or.inl ⟨_, List.mem_map.2 ⟨o', ho', rfl⟩, ?_⟩
          split
          · subst_vars; simpa [Obligation.isQuestion] using hk
          · exact hk
        · exact Or.inr (Or.inl h)
        · exact Or.inr (Or.inr h)
      · simp at he
    · simp at he
  | acks t =>
    simp only [Watch.ownerStep] at he
    split at he
    · rename_i o _
      split at he
      · simp at he
      · simp only [Option.some.injEq, Prod.mk.injEq] at he
        obtain ⟨rfl, -⟩ := he
        rcases h with ⟨o', ho', hk⟩ | h | h
        · by_cases heq : o' = o
          · subst heq; right; right; simp [hk.1]
          · exact Or.inl ⟨o', (List.mem_erase_of_ne heq).2 ho', hk⟩
        · exact Or.inr (Or.inl h)
        · exact Or.inr (Or.inr (List.mem_append_left _ h))
    · simp at he
  | escalates t =>
    simp only [Watch.ownerStep] at he
    split at he
    · rename_i o _
      split at he
      · simp only [Option.some.injEq, Prod.mk.injEq] at he
        obtain ⟨rfl, -⟩ := he
        rcases h with ⟨o', ho', hk⟩ | h | h
        · by_cases heq : o' = o
          · subst heq; right; left; simp [hk.1]
          · exact Or.inl ⟨o', (List.mem_erase_of_ne heq).2 ho', hk⟩
        · exact Or.inr (Or.inl (List.mem_append_left _ h))
        · exact Or.inr (Or.inr h)
      · simp at he
    · simp at he
  | gone =>
    simp only [Watch.ownerStep, Option.some.injEq, Prod.mk.injEq] at he
    obtain ⟨rfl, -⟩ := he
    rcases h with ⟨o, ho, hs, hq⟩ | h | h
    · cases ha : o.answered
      · right; left
        exact List.mem_append_right _ (List.mem_map.2
          ⟨o, List.mem_filter.2 ⟨ho, by simp [Obligation.unanswered, hq, ha]⟩, hs⟩)
      · right; right
        exact List.mem_append_right _ (List.mem_map.2
          ⟨o, List.mem_filter.2 ⟨ho, by simp [hq, ha]⟩, hs⟩)
    · exact Or.inr (Or.inl (List.mem_append_left _ h))
    · exact Or.inr (Or.inr (List.mem_append_left _ h))

theorem onOwner_accounts (w w' : Watch) (a : SupAction) (e : OwnerEvent) (s : Nat)
    (h : w.accounts s) (he : w.onOwner e = some (w', a)) : w'.accounts s := by
  unfold Watch.onOwner at he
  split at he
  · simp at he
  · exact ownerStep_accounts w w' a e s h he

/-- A question is never lost: over any events of the worker and its owner,
its number stays open, goes to the user, or is answered and acknowledged. -/
theorem question_never_lost (es : List SupEvent) (w w' : Watch) (s : Nat)
    (h : w.accounts s) (hrun : w.run es = some w') : w'.accounts s := by
  induction es generalizing w with
  | nil => simp [Watch.run] at hrun; subst hrun; exact h
  | cons e es ih =>
    simp only [Watch.run, Option.bind_eq_some_iff] at hrun
    obtain ⟨m, hm, hrest⟩ := hrun
    apply ih m _ hrest
    cases e with
    | worker e => simp [Watch.step] at hm; subst hm; exact onWorker_accounts w e s h
    | owner e =>
      simp only [Watch.step, Option.map_eq_some_iff] at hm
      obtain ⟨⟨w1, a⟩, h1, rfl⟩ := hm
      exact onOwner_accounts w w1 a e s h h1

/-- An unanswered question cannot be acknowledged away. -/
theorem ack_refuses_unanswered (w : Watch) (s : Nat) (o : Obligation)
    (hf : w.obligations.find? (·.seq == s) = some o) (hu : o.unanswered = true) :
    w.onOwner (.acks s) = none := by
  simp [Watch.onOwner, Watch.ownerStep, hf, hu]

/-- When the owner goes away, every unanswered question reaches the user and
nothing is left that only the gone owner could acknowledge. -/
theorem gone_escalates (w : Watch) (o : Obligation) (hg : w.ownerGone = false)
    (ho : o ∈ w.obligations) (hu : o.unanswered = true) :
    ∃ w', w.onOwner .gone = some (w', .alertUser) ∧ o.seq ∈ w'.escalated ∧
      w'.ownerMayStop = true := by
  have hm : o.seq ∈ (w.obligations.filter (·.unanswered)).map (·.seq) :=
    List.mem_map.2 ⟨o, List.mem_filter.2 ⟨ho, hu⟩, rfl⟩
  have hne : ((w.obligations.filter (·.unanswered)).map (·.seq)).isEmpty = false := by
    cases hl : (w.obligations.filter (·.unanswered)).map (·.seq) <;> simp_all
  refine ⟨_, by simp only [Watch.onOwner, hg, Watch.ownerStep, hne]; rfl,
    List.mem_append_right _ hm, ?_⟩
  simp [Watch.ownerMayStop]

/-- An owner step keeps `nextSeq`, only keeps, marks or drops obligations,
and escalates or handles only numbers that were already in use. -/
theorem ownerStep_shrinks (w w' : Watch) (a : SupAction) (e : OwnerEvent)
    (he : w.ownerStep e = some (w', a)) :
    w'.nextSeq = w.nextSeq ∧ (∀ o ∈ w'.obligations, ∃ o0 ∈ w.obligations, o.seq = o0.seq) ∧
    (∀ s ∈ w'.escalated ++ w'.handled,
      s ∈ w.escalated ++ w.handled ∨ ∃ o0 ∈ w.obligations, s = o0.seq) := by
  cases e with
  | answers t =>
    simp only [Watch.ownerStep] at he
    split at he
    · rename_i o _
      split at he
      · simp only [Option.some.injEq, Prod.mk.injEq] at he
        obtain ⟨rfl, -⟩ := he
        refine ⟨rfl, fun o' ho' => ?_, fun s hs => Or.inl hs⟩
        obtain ⟨o0, h0, rfl⟩ := List.mem_map.1 ho'
        refine ⟨o0, h0, ?_⟩
        split <;> simp_all
      · simp at he
    · simp at he
  | acks t =>
    simp only [Watch.ownerStep] at he
    split at he
    · rename_i o hf
      split at he
      · simp at he
      · simp only [Option.some.injEq, Prod.mk.injEq] at he
        obtain ⟨rfl, -⟩ := he
        refine ⟨rfl, fun o' ho' => ⟨o', List.mem_of_mem_erase ho', rfl⟩, fun s hs => ?_⟩
        simp only [List.mem_append, List.mem_singleton] at hs
        rcases hs with hs | hs | rfl
        · exact Or.inl (List.mem_append_left _ hs)
        · exact Or.inl (List.mem_append_right _ hs)
        · exact Or.inr ⟨o, List.mem_of_find?_eq_some hf, rfl⟩
    · simp at he
  | escalates t =>
    simp only [Watch.ownerStep] at he
    split at he
    · rename_i o hf
      split at he
      · simp only [Option.some.injEq, Prod.mk.injEq] at he
        obtain ⟨rfl, -⟩ := he
        refine ⟨rfl, fun o' ho' => ⟨o', List.mem_of_mem_erase ho', rfl⟩, fun s hs => ?_⟩
        simp only [List.mem_append, List.mem_singleton] at hs
        rcases hs with (hs | rfl) | hs
        · exact Or.inl (List.mem_append_left _ hs)
        · exact Or.inr ⟨o, List.mem_of_find?_eq_some hf, rfl⟩
        · exact Or.inl (List.mem_append_right _ hs)
      · simp at he
    · simp at he
  | gone =>
    simp only [Watch.ownerStep, Option.some.injEq, Prod.mk.injEq] at he
    obtain ⟨rfl, -⟩ := he
    refine ⟨rfl, fun o' ho' => by simp at ho', fun s hs => ?_⟩
    simp only [List.mem_append, List.mem_map, List.mem_filter] at hs
    rcases hs with (hs | ⟨o, ⟨ho, -⟩, rfl⟩) | (hs | ⟨o, ⟨ho, -⟩, rfl⟩)
    · exact Or.inl (List.mem_append_left _ hs)
    · exact Or.inr ⟨o, ho, rfl⟩
    · exact Or.inl (List.mem_append_right _ hs)
    · exact Or.inr ⟨o, ho, rfl⟩

/-- A worker step adds at most the number `nextSeq`, and then moves past it. -/
theorem onWorker_bounds (w : Watch) (e : WorkerEvent) :
    let w' := (w.onWorker e).1
    w.nextSeq ≤ w'.nextSeq ∧ w'.handled = w.handled ∧
    (∀ o ∈ w'.obligations, o ∈ w.obligations ∨ (o.seq = w.nextSeq ∧ w'.nextSeq = w.nextSeq + 1)) ∧
    (∀ s ∈ w'.escalated, s ∈ w.escalated ∨ (s = w.nextSeq ∧ w'.nextSeq = w.nextSeq + 1)) := by
  cases hg : w.ownerGone <;> cases e <;>
    simp (config := { contextual := true }) [Watch.onWorker, hg] <;>
    intro o ho <;> rcases ho with ho | rfl <;> simp_all

theorem step_seqsBelow (w w' : Watch) (e : SupEvent)
    (h : w.seqsBelow) (hs : w.step e = some w') : w'.seqsBelow := by
  obtain ⟨hO, hE, hH⟩ := h
  cases e with
  | worker e =>
    simp only [Watch.step, Option.some.injEq] at hs; subst hs
    obtain ⟨hn, hh, ho, he⟩ := onWorker_bounds w e
    refine ⟨fun o h1 => ?_, fun s h1 => ?_, fun s h1 => ?_⟩
    · rcases ho o h1 with h2 | ⟨h2, h3⟩
      · have := hO o h2; omega
      · omega
    · rcases he s h1 with h2 | ⟨h2, h3⟩
      · have := hE s h2; omega
      · omega
    · rw [hh] at h1; have := hH s h1; omega
  | owner e =>
    simp only [Watch.step, Option.map_eq_some_iff] at hs
    obtain ⟨⟨w1, a⟩, h1, rfl⟩ := hs
    unfold Watch.onOwner at h1
    split at h1
    · simp at h1
    · obtain ⟨hn, hsub, hlog⟩ := ownerStep_shrinks w w1 a e h1
      have old : ∀ s, (s ∈ w.escalated ++ w.handled ∨ ∃ o0 ∈ w.obligations, s = o0.seq) →
          s < w.nextSeq := by
        intro s hs
        rcases hs with hs | ⟨o0, h0, rfl⟩
        · rcases List.mem_append.1 hs with hs | hs
          · exact hE s hs
          · exact hH s hs
        · exact hO o0 h0
      refine ⟨fun o ho => ?_, fun s hs => ?_, fun s hs => ?_⟩ <;> rw [hn]
      · obtain ⟨o0, h0, heq⟩ := hsub o ho; rw [heq]; exact hO o0 h0
      · exact old s (hlog s (List.mem_append_left _ hs))
      · exact old s (hlog s (List.mem_append_right _ hs))

/-- Over any events, every number in use stays below `nextSeq`, so the number
a new obligation gets is in use nowhere. -/
theorem seqs_fresh (es : List SupEvent) (w w' : Watch)
    (h : w.seqsBelow) (hrun : w.run es = some w') :
    w'.seqsBelow ∧ (∀ o ∈ w'.obligations, o.seq ≠ w'.nextSeq) ∧
      w'.nextSeq ∉ w'.escalated ∧ w'.nextSeq ∉ w'.handled := by
  induction es generalizing w with
  | nil =>
    simp [Watch.run] at hrun; subst hrun
    obtain ⟨hO, hE, hH⟩ := h
    exact ⟨⟨hO, hE, hH⟩, fun o ho => Nat.ne_of_lt (hO o ho),
      fun hm => Nat.lt_irrefl _ (hE _ hm), fun hm => Nat.lt_irrefl _ (hH _ hm)⟩
  | cons e es ih =>
    simp only [Watch.run, Option.bind_eq_some_iff] at hrun
    obtain ⟨m, hm, hrest⟩ := hrun
    exact ih m (step_seqsBelow w m e h hm) hrest

end WorkerQuestions
section Supervision

theorem mem_allPredicates (p : Predicate) : p ∈ allPredicates := by
  cases p <;> simp [allPredicates]

/-- A finding is detected exactly when its predicate holds for its subject. -/
theorem detect_iff (snap : List Facts) (p : Predicate) (s : String) :
    (⟨p, s⟩ : Finding) ∈ detect snap ↔ ∃ f ∈ snap, f.subject = s ∧ p.holds f = true := by
  simp only [detect, List.mem_flatMap, List.mem_map, List.mem_filter]
  constructor
  · rintro ⟨f, hf, q, ⟨-, hq⟩, hx⟩
    cases hx
    exact ⟨f, hf, rfl, hq⟩
  · rintro ⟨f, hf, rfl, hp⟩
    exact ⟨f, hf, p, ⟨mem_allPredicates p, hp⟩, rfl⟩

/-- A stuck coordinator (idle with runnable items, no open decision and no
known wait) is detected. -/
theorem idle_coordinator_detected (snap : List Facts) (f : Facts) (hf : f ∈ snap)
    (hc : f.isCoordinator = true) (hi : f.agentIdle = true) (hr : f.runnableItems = true)
    (hd : f.openDecision = false) (hw : f.knownWait = false) :
    (⟨.coordinatorIdle, f.subject⟩ : Finding) ∈ detect snap :=
  (detect_iff snap _ _).2 ⟨f, hf, rfl, by simp [Predicate.holds, hc, hi, hr, hd, hw]⟩

/-- After a sweep, every detected finding has a row that is not resolved. -/
theorem sweep_records (l : Ledger) (snap : List Facts) (x : Finding)
    (hx : x ∈ detect snap) :
    ∃ st, (x, st) ∈ (l.sweep snap).entries ∧ st ≠ .resolved := by
  simp only [Ledger.sweep]
  cases ha : l.active x
  · refine ⟨.open_, List.mem_append_right _ (List.mem_map.2 ⟨x, ?_, rfl⟩), by simp⟩
    exact List.mem_filter.2 ⟨List.mem_eraseDups.2 hx, by simp [ha]⟩
  · obtain ⟨e, he, hm⟩ := List.any_eq_true.1 ha
    simp only [Bool.and_eq_true, beq_iff_eq, bne_iff_ne, ne_eq] at hm
    obtain ⟨rfl, hst⟩ := hm
    refine ⟨e.2, List.mem_append_left _ (List.mem_map.2 ⟨e, he, ?_⟩), hst⟩
    have : (detect snap).contains e.1 = true := List.elem_iff.2 hx
    simp only [this, Bool.not_true, Bool.and_false, Bool.false_eq_true, ↓reduceIte]

/-- The ledger is append-only: every finding it had stays in it. -/
theorem sweep_keeps (l : Ledger) (snap : List Facts) (e : Finding × FindingState)
    (he : e ∈ l.entries) : ∃ st, (e.1, st) ∈ (l.sweep snap).entries := by
  simp only [Ledger.sweep]
  by_cases hc : (e.2 != .resolved && !(detect snap).contains e.1) = true
  · exact ⟨.resolved, List.mem_append_left _ (List.mem_map.2 ⟨e, he,
      by simp only [hc, ↓reduceIte]⟩)⟩
  · exact ⟨e.2, List.mem_append_left _ (List.mem_map.2 ⟨e, he,
      by simp only [hc, Bool.false_eq_true, ↓reduceIte]⟩)⟩

/-- A detected finding that is not already active is opened by the sweep,
so it reaches the user's `?` list and its coordinator's notices. -/
theorem new_finding_surfaces (l : Ledger) (snap : List Facts) (x : Finding)
    (hx : x ∈ detect snap) (ha : l.active x = false) :
    x ∈ l.newFindings snap ∧ (x, FindingState.open_) ∈ (l.sweep snap).entries := by
  have hn : x ∈ l.newFindings snap :=
    List.mem_filter.2 ⟨List.mem_eraseDups.2 hx, by simp [ha]⟩
  exact ⟨hn, List.mem_append_right _ (List.mem_map.2 ⟨x, hn, rfl⟩)⟩

/-- An acknowledgement needs an open row. -/
theorem ack_needs_open (l l' : Ledger) (who : Acker) (x : Finding)
    (h : l.ack who x = some l') : ∃ e ∈ l.entries, e.1 = x ∧ e.2 = .open_ := by
  cases who <;> simp only [Ledger.ack] at h
  all_goals first
    | (split at h
       · rename_i hany
         obtain ⟨e, he, hm⟩ := List.any_eq_true.1 hany
         exact ⟨e, he, by simpa using hm⟩
       · simp at h)
    | simp at h

/-- The stalled agent cannot silence its own finding. -/
theorem agent_cannot_ack (l : Ledger) (p : PaneId) (x : Finding) :
    l.ack (.agent p) x = none := rfl

/-- Supervision never acts: a sweep changes only its own ledger, never the
tenure, the runs, the branches or the question relay. -/
theorem supervise_never_acts (sys : System) (snap : List Facts) :
    (sys.supervise snap).tenure = sys.tenure ∧ (sys.supervise snap).repo = sys.repo ∧
    (sys.supervise snap).git = sys.git ∧ (sys.supervise snap).relay = sys.relay := by
  simp [System.supervise]

end Supervision


section Coordinator

/-- Outside a herdr pane the coordinator never dispatches. -/
theorem outside_pane_no_dispatch (c : Coordinator) (h : c.pane = none) :
    c.decide = .refuseOutsidePane := by
  simp [Coordinator.decide, h]

/-- It dispatches only while its own pane holds an unfrozen tenure. -/
theorem dispatch_needs_tenure (c : Coordinator) (id : String)
    (h : c.decide = .dispatch id) : ∃ p, c.pane = some p ∧ c.tenure = .held p := by
  unfold Coordinator.decide at h
  cases hp : c.pane with
  | none => simp [hp] at h
  | some p =>
    simp only [hp] at h
    refine ⟨p, rfl, ?_⟩
    cases ht : c.tenure <;> simp_all [Tenure.actingHolder]
    split at h <;> simp_all

end Coordinator

/-- A happy path, end to end. -/
example :
    Run.preflight.run
      [.preflightOk, .gateAdmits, .workerDone, .exited, .verify .verified,
       .picked, .installed, .edited, .pushed] = some .done := rfl

/-- A failed verify, a retry with a new task, then a failed push and `retry-push`. -/
example :
    Run.preflight.run
      [.preflightOk, .gateAdmits, .workerDone, .exited, .verify .failed, .retry,
       .workerDone, .stillAliveSeen, .forceStop, .verify .verified,
       .picked, .installed, .edited, .pushFails, .retryPush, .pushed] = some .done := rfl

#guard (Coordinator.mk (some "pane-1") (.held "pane-1") 40
          [.needsDecision "t-a" "which?", .next "t-b"]).decide == .dispatch "t-b"


/-! ## Target design: a fresh coordinator session per item (not implemented yet)

Decided by the user on 2026-10-23 after consult rounds
`20261023-211512-5f06`, `20261023-211937-9803`, `20261023-212316-447d` and
`20261023-212927-b9d8`: the long-lived coordinator re-sent a ~550k-token
context about 36 times per item. In the target design herdr's queue starts a
fresh coordinator session for the top item on a durable event (a run landed,
the user answered, server start); the session reads only that item, records
every open question and consult finding before it acts, and ends when its
item has landed or waits on the user. A mid-item handoff stays as a backstop
for a huge item. Workers are routed by item class, a non-trivial diff needs a
short-context review before it lands, and a pre-task consult is skipped when
the item's decided round is newer than the last commit on its target paths.

`PerItem.Queue` replaces `Coordinator.decide`'s 70% handoff once it is
implemented; the run, git, relay and supervision machines above stay. -/

namespace PerItem

abbrev ItemId := String
abbrev NoteId := Nat

/-- One coordinator session: the tenure epoch it was started under, its one
item, and what it has learned but not yet recorded (open questions to the
user and consult findings). -/
structure Session where
  epoch   : Nat
  item    : ItemId
  pending : List NoteId
  deriving DecidableEq, Repr

structure Queue where
  epoch   : Nat              -- the tenure epoch; every session start advances it
  session : Option Session
  next    : List ItemId      -- "Next, in order"
  parked  : List ItemId      -- waiting on the user ("Needs a decision")
  landed  : List ItemId
  records : List NoteId      -- durable records of questions and findings
  deriving Repr

inductive Cmd where
  | wake                          -- a durable event: run landed, user answered, server start
  | learn (e : Nat) (n : NoteId)  -- the session meets a question or a consult finding
  | record (e : Nat)              -- it writes what it learned to durable records
  | land (e : Nat)                -- its item's run landed through the driver; the session ends
  | park (e : Nat)                -- its item waits on the user; the session ends
  | handOff (e : Nat)             -- backstop for a huge item: a fresh session, same item
  | crash                         -- the session's process is gone
  deriving Repr

/-- The epoch a session command carries; `wake` and `crash` come from herdr. -/
def Cmd.epoch? : Cmd → Option Nat
  | .learn e _ | .record e | .land e | .park e | .handOff e => some e
  | .wake | .crash => none

/-- The live session, if `e` is its epoch: a superseded session acts on nothing. -/
def Queue.live (q : Queue) (e : Nat) : Option Session :=
  match q.session with
  | some s => if s.epoch = e then some s else none
  | none => none

/-- What every durable event does: a pure function of the durable state, so a
lost wake only delays the next start and a repeated one starts nothing more. -/
def Queue.reconcile (q : Queue) : Queue :=
  match q.session, q.next with
  | none, i :: _ => { q with epoch := q.epoch + 1, session := some ⟨q.epoch + 1, i, []⟩ }
  | _, _ => q

def Queue.step (q : Queue) : Cmd → Option Queue
  | .wake => some q.reconcile
  | .learn e n => (q.live e).map fun s =>
      { q with session := some { s with pending := s.pending ++ [n] } }
  | .record e => (q.live e).map fun s =>
      { q with session := some { s with pending := [] }, records := q.records ++ s.pending }
  | .land e => match q.live e with
      | some s => if s.pending = [] then
          some { q with session := none, next := q.next.filter (· ≠ s.item),
                        landed := q.landed ++ [s.item] }
        else none
      | none => none
  | .park e => match q.live e with
      | some s => if s.pending = [] then
          some { q with session := none, next := q.next.filter (· ≠ s.item),
                        parked := q.parked ++ [s.item] }
        else none
      | none => none
  | .handOff e => match q.live e with
      | some s => if s.pending = [] then
          some { q with epoch := q.epoch + 1, session := some ⟨q.epoch + 1, s.item, []⟩ }
        else none
      | none => none
  | .crash => some { q with session := none }

/-- Runs the commands in order; `none` at the first refused one. -/
def Queue.exec (q : Queue) : List Cmd → Option Queue
  | [] => some q
  | c :: cs => (q.step c).bind (·.exec cs)

/-- The live session, if any, holds the current epoch. -/
def Queue.wf (q : Queue) : Prop := ∀ s, q.session = some s → s.epoch = q.epoch

/-! ### Worker routing, review before landing, and when to consult -/

inductive ItemClass where
  | routine     -- a bounded feature or fix, tests, docs
  | risky       -- concurrency, protocol or wire, persisted state, refactor-risk
  | mechanical  -- renames, regenerated files, text-only edits
  deriving DecidableEq, Repr

inductive WorkerModel where
  | sonnet | opus | haiku
  deriving DecidableEq, Repr

/-- Sonnet by default, Opus for risky items and after any failed verify,
Haiku only for mechanical items while its trial is on. -/
def route (cls : ItemClass) (failedVerifies : Nat) (haikuTrial : Bool) : WorkerModel :=
  if failedVerifies > 0 then .opus
  else match cls with
    | .risky => .opus
    | .mechanical => if haikuTrial then .haiku else .sonnet
    | .routine => .sonnet

/-- A commit lands only when verified, and a non-trivial diff only after the
short-context review (diff, item, decisions excerpt) passed. -/
def mayLand (v : Verdict) (trivialDiff reviewed : Bool) : Bool :=
  match v with
  | .verified => trivialDiff || reviewed
  | _ => false

/-- A pre-task consult is needed unless the item records a decided round newer
than the last commit on its target paths and nothing new came up since. -/
def needsPreConsult (decidedAt : Option Nat) (lastTargetCommit : Nat) (newFact : Bool) : Bool :=
  newFact || match decidedAt with
    | none => true
    | some t => decide (t ≤ lastTargetCommit)

section PerItemProofs

/-- A command from a superseded (or absent) session changes nothing. -/
theorem stale_refused (q : Queue) (c : Cmd) (e : Nat) (he : c.epoch? = some e)
    (hs : ∀ s, q.session = some s → s.epoch ≠ e) : q.step c = none := by
  have hl : q.live e = none := by
    unfold Queue.live
    cases h : q.session with
    | none => rfl
    | some s => simp [hs s h]
  cases c <;> simp_all [Cmd.epoch?, Queue.step]

/-- Reconciling twice is reconciling once: a repeated wake starts nothing more. -/
theorem reconcile_idem (q : Queue) : q.reconcile.reconcile = q.reconcile := by
  unfold Queue.reconcile
  cases hs : q.session <;> cases hn : q.next <;> simp [hs, hn]

/-- With no live session, a wake starts one exactly when an item is next. -/
theorem reconcile_starts_iff (q : Queue) (h : q.session = none) :
    q.reconcile.session.isSome ↔ q.next ≠ [] := by
  unfold Queue.reconcile
  cases q.next <;> simp [h]

/-- A wake never replaces a live session. -/
theorem reconcile_keeps_live (q : Queue) (s : Session) (h : q.session = some s) :
    q.reconcile = q := by
  simp [Queue.reconcile, h]

/-- The item a wake starts is the top of "Next, in order". -/
theorem reconcile_takes_top (q : Queue) (s : Session) (h : q.session = none)
    (h' : q.reconcile.session = some s) : q.next.head? = some s.item := by
  unfold Queue.reconcile at h'
  cases hn : q.next <;> simp_all
  subst h'; rfl

/-- Landing, parking and the backstop handoff need everything learned recorded. -/
theorem acts_need_recorded (q q' : Queue) (c : Cmd) (e : Nat)
    (hc : c = .land e ∨ c = .park e ∨ c = .handOff e) (h : q.step c = some q') :
    ∃ s, q.session = some s ∧ s.epoch = e ∧ s.pending = [] := by
  rcases hc with rfl | rfl | rfl <;>
  · simp only [Queue.step] at h
    unfold Queue.live at h
    cases hs : q.session with
    | none => simp [hs] at h
    | some s =>
      by_cases he : s.epoch = e <;> by_cases hp : s.pending = [] <;> simp_all

/-- A step keeps the live session on the current epoch. -/
theorem step_wf (q q' : Queue) (c : Cmd) (hw : q.wf) (h : q.step c = some q') : q'.wf := by
  intro s' hs'
  cases c with
  | wake =>
    simp only [Queue.step, Option.some.injEq] at h; subst h
    unfold Queue.reconcile at hs' ⊢
    cases hs : q.session with
    | some s => simp only [hs, Option.some.injEq] at hs' ⊢; subst hs'; exact hw s hs
    | none =>
      cases hn : q.next with
      | nil => simp [hs, hn] at hs'
      | cons i is => simp only [hs, hn, Option.some.injEq] at hs' ⊢; subst hs'; rfl
  | crash => simp only [Queue.step, Option.some.injEq] at h; subst h; simp at hs'
  | learn e n =>
    simp only [Queue.step, Option.map_eq_some_iff] at h
    obtain ⟨s, hl, rfl⟩ := h
    simp only [Option.some.injEq] at hs'; subst hs'
    unfold Queue.live at hl
    split at hl
    · rename_i s0 hs0
      split at hl
      · cases hl; exact hw s hs0
      · cases hl
    · cases hl
  | record e =>
    simp only [Queue.step, Option.map_eq_some_iff] at h
    obtain ⟨s, hl, rfl⟩ := h
    simp only [Option.some.injEq] at hs'; subst hs'
    unfold Queue.live at hl
    split at hl
    · rename_i s0 hs0
      split at hl
      · cases hl; exact hw s hs0
      · cases hl
    · cases hl
  | land e =>
    simp only [Queue.step] at h
    split at h
    · split at h
      · cases h; simp at hs'
      · cases h
    · cases h
  | park e =>
    simp only [Queue.step] at h
    split at h
    · split at h
      · cases h; simp at hs'
      · cases h
    · cases h
  | handOff e =>
    simp only [Queue.step] at h
    split at h
    · split at h
      · cases h; simp only [Option.some.injEq] at hs'; subst hs'; rfl
      · cases h
    · cases h

/-- Over any commands, the live session holds the current epoch, so together
with `stale_refused` a superseded session can never act. -/
theorem exec_wf (cs : List Cmd) (q q' : Queue) (hw : q.wf) (h : q.exec cs = some q') :
    q'.wf := by
  induction cs generalizing q with
  | nil => simp [Queue.exec] at h; subst h; exact hw
  | cons c cs ih =>
    simp only [Queue.exec, Option.bind_eq_some_iff] at h
    obtain ⟨q1, h1, h2⟩ := h
    exact ih q1 (step_wf q q1 c hw h1) h2

/-- One step never drops a durable record. -/
theorem step_records (q q' : Queue) (c : Cmd) (h : q.step c = some q') :
    q.records <+: q'.records := by
  cases c with
  | wake =>
    simp only [Queue.step, Option.some.injEq] at h; subst h
    unfold Queue.reconcile
    split <;> exact List.prefix_refl _
  | crash => simp only [Queue.step, Option.some.injEq] at h; subst h; exact List.prefix_refl _
  | learn e n =>
    simp only [Queue.step, Option.map_eq_some_iff] at h
    obtain ⟨s, -, rfl⟩ := h; exact List.prefix_refl _
  | record e =>
    simp only [Queue.step, Option.map_eq_some_iff] at h
    obtain ⟨s, -, rfl⟩ := h; exact List.prefix_append _ _
  | land e =>
    simp only [Queue.step] at h
    split at h
    · split at h
      · cases h; exact List.prefix_refl _
      · cases h
    · cases h
  | park e =>
    simp only [Queue.step] at h
    split at h
    · split at h
      · cases h; exact List.prefix_refl _
      · cases h
    · cases h
  | handOff e =>
    simp only [Queue.step] at h
    split at h
    · split at h
      · cases h; exact List.prefix_refl _
      · cases h
    · cases h

/-- Durable records only grow, over any commands (a crash loses only what was
never recorded, and nothing acted on it: `acts_need_recorded`). -/
theorem records_only_grow (cs : List Cmd) (q q' : Queue) (h : q.exec cs = some q') :
    q.records <+: q'.records := by
  induction cs generalizing q with
  | nil => simp [Queue.exec] at h; subst h; exact List.prefix_refl _
  | cons c cs ih =>
    simp only [Queue.exec, Option.bind_eq_some_iff] at h
    obtain ⟨q1, h1, h2⟩ := h
    exact (step_records q q1 c h1).trans (ih q1 h2)

/-- One step never lowers the epoch. -/
theorem step_epoch (q q' : Queue) (c : Cmd) (h : q.step c = some q') : q.epoch ≤ q'.epoch := by
  cases c with
  | wake =>
    simp only [Queue.step, Option.some.injEq] at h; subst h
    unfold Queue.reconcile
    split <;> simp
  | crash => simp only [Queue.step, Option.some.injEq] at h; subst h; simp
  | learn e n =>
    simp only [Queue.step, Option.map_eq_some_iff] at h
    obtain ⟨s, -, rfl⟩ := h; simp
  | record e =>
    simp only [Queue.step, Option.map_eq_some_iff] at h
    obtain ⟨s, -, rfl⟩ := h; simp
  | land e =>
    simp only [Queue.step] at h
    split at h
    · split at h
      · cases h; simp
      · cases h
    · cases h
  | park e =>
    simp only [Queue.step] at h
    split at h
    · split at h
      · cases h; simp
      · cases h
    · cases h
  | handOff e =>
    simp only [Queue.step] at h
    split at h
    · split at h
      · cases h; simp
      · cases h
    · cases h

/-- A landed item leaves "Next, in order", so no later wake starts it again. -/
theorem landed_leaves_next (q q' : Queue) (e : Nat) (s : Session)
    (hs : q.live e = some s) (h : q.step (.land e) = some q') :
    s.item ∉ q'.next ∧ s.item ∈ q'.landed := by
  simp only [Queue.step, hs] at h
  split at h
  · cases h; simp
  · cases h

/-- Risky items get Opus. -/
theorem route_risky (n : Nat) (t : Bool) : route .risky n t = .opus := by
  unfold route; split <;> rfl

/-- Any failed verify moves the next attempt to Opus. -/
theorem route_after_failure (c : ItemClass) (n : Nat) (t : Bool) (h : n > 0) :
    route c n t = .opus := by
  simp [route, h]

/-- Haiku only for a first attempt at a mechanical item during its trial. -/
theorem haiku_only_on_trial (c : ItemClass) (n : Nat) (t : Bool) (h : route c n t = .haiku) :
    t = true ∧ c = .mechanical ∧ n = 0 := by
  unfold route at h
  by_cases hn : n > 0
  · simp [hn] at h
  · cases c <;> cases t <;> simp_all <;> omega

/-- A non-trivial diff lands only verified and reviewed. -/
theorem nontrivial_needs_review (v : Verdict) (r : Bool) (h : mayLand v false r = true) :
    v = .verified ∧ r = true := by
  cases v <;> simp_all [mayLand]

/-- Nothing lands unverified, trivial or not. -/
theorem mayLand_needs_verified (v : Verdict) (t r : Bool) (h : mayLand v t r = true) :
    v = .verified := by
  cases v <;> simp_all [mayLand]

/-- The pre-task consult is skipped exactly when the decided round is newer
than the last commit on the target paths and nothing new came up. -/
theorem consult_skipped_iff (t c : Nat) :
    needsPreConsult (some t) c false = false ↔ c < t := by
  simp [needsPreConsult]

end PerItemProofs

-- Two items: the first one's session learns a finding, records it and lands;
-- the next wake starts the second item in a fresh epoch.
#guard ((Queue.mk 0 none ["t-a", "t-b"] [] [] []).exec
          [.wake, .learn 1 7, .record 1, .land 1, .wake]).map
          (fun q => (q.epoch, q.session, q.landed, q.records)) ==
        some (2, some ⟨2, "t-b", []⟩, ["t-a"], [7])

-- Landing with an unrecorded finding is refused; so is a superseded epoch.
#guard ((Queue.mk 0 none ["t-a"] [] [] []).exec [.wake, .learn 1 7, .land 1]).isNone
#guard ((Queue.mk 0 none ["t-a"] [] [] []).exec [.wake, .handOff 1, .learn 1 7]).isNone

end PerItem

end TodoWorkflow
