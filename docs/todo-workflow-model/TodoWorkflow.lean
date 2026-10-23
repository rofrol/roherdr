/-!
# The herdr TODO workflow as a state machine

A hand-written model (see README.md) of how a repository's TODO is worked
through in herdr: the coordination tenure and its handoff, one item's run
through `herdr todo run`, the supervisor that watches a worker for its
owner, and the coordinator's decision at an item boundary. Commands are
named in comments. Only behavior is modelled; no I/O.

`none` from a `step` function means the command is refused or does not
apply in that state; the state is unchanged. `Run.run` and `Watch.run`
evaluate a script of events and fail (`none`) at the first refused one.
Abstracted away: the content of the cherry-picked commit (verify checks it;
the model trusts it), and the composition of the parts: `Tenure`, `Repo`,
`Git` and `Watch` are separate machines, so nothing here proves that a frozen
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

/-! ## Runs of a repository: identity and the one worker slot

Every run is kept with its id and its TODO item. A run id is never reused,
an item with an unfinished run (a refused admission included) is attached to,
not launched again, and only one run at a time holds the worker slot. -/

abbrev ItemId := String

/-- A run that holds the folder slot: from admission until its commit is
cherry-picked or the run ends (verify runs in the slot's worktree too). -/
def Run.busy : Run → Bool
  | .working _ | .stopping _ | .stillAlive _ | .verifying _ | .unavailable _
  | .failed _ | .cherryPick => true
  | _ => false

def Run.finished : Run → Bool
  | .done | .blocked => true
  | _ => false

structure RunRec where
  id   : RunId
  item : ItemId
  run  : Run
  deriving DecidableEq, Repr

structure Repo where
  runs : List RunRec
  deriving Repr

inductive RepoCmd where
  | launch (id : RunId) (item : ItemId)   -- herdr todo run <item>
  | event (id : RunId) (e : Event)        -- an event of that run (attach included)
  deriving Repr

def Repo.step (s : Repo) : RepoCmd → Option Repo
  | .launch id item =>
      if s.runs.any (·.id == id) then none
      else if s.runs.any (fun x => x.item == item && !x.run.finished) then none
      else some { runs := s.runs ++ [⟨id, item, .preflight⟩] }
  | .event id e =>
      match s.runs.find? (·.id == id) with
      | none => none
      | some x =>
        match x.run.step e with
        | none => none
        | some r' =>
          if r'.busy && s.runs.any (fun y => y.id != id && y.run.busy) then none
          else some { runs := s.runs.map fun y => if y.id = id then { y with run := r' } else y }

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
/-! ## The supervisor (deterministic, not an LLM)

It watches one worker for the pane that started it (its owner) and turns
the worker's events into numbered obligations of that owner: `herdr worker
wait --attention` returns them, `herdr worker ack <seq>` clears one after it
is handled, and the Stop hook refuses the owner's stop while one is open. A
question is handled once answered (`worker answer --request`) or handed to
the user (`worker escalate`). On the owner's own events (its pane closes,
its agent exits, hits a limit, is blocked on its own question, or ends its
turn with a question unanswered; herdr starts) the supervisor escalates every
unanswered question to the user and drops the owner's other obligations. It
never kills or restarts, and acts on no timer. (`WorkerSupervisor` in the
code also starts, stops and kills workers; that part is not modelled.)
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
theorem launch_new_id (s : Repo) (id : RunId) (item : ItemId)
    (h : ∃ x ∈ s.runs, x.id = id) : s.step (.launch id item) = none := by
  obtain ⟨x, hx, rfl⟩ := h
  have : s.runs.any (·.id == x.id) = true := List.any_eq_true.2 ⟨x, hx, by simp⟩
  simp [Repo.step, this]

/-- An item with an unfinished run (a refused admission included) is never
launched again: that run is attached to. -/
theorem no_relaunch (s : Repo) (id : RunId) (item : ItemId)
    (h : ∃ x ∈ s.runs, x.item = item ∧ x.run.finished = false) :
    s.step (.launch id item) = none := by
  obtain ⟨x, hx, rfl, hf⟩ := h
  have : s.runs.any (fun y => y.item == x.item && !y.run.finished) = true :=
    List.any_eq_true.2 ⟨x, hx, by simp [hf]⟩
  simp only [Repo.step, this]
  split <;> simp

theorem step_oneWorker (s s' : Repo) (c : RepoCmd)
    (h : s.oneWorker) (hs : s.step c = some s') : s'.oneWorker := by
  cases c with
  | launch id item =>
    simp only [Repo.step] at hs
    split at hs
    · simp at hs
    · split at hs
      · simp at hs
      · simp only [Option.some.injEq] at hs
        subst hs
        intro x hx y hy bx byy
        simp only [List.mem_append, List.mem_singleton] at hx hy
        rcases hx with hx | rfl <;> rcases hy with hy | rfl
        · exact h x hx y hy bx byy
        · simp [Run.busy] at byy
        · simp [Run.busy] at bx
        · rfl
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

/-- Over any sequence of commands, at most one run holds the worker slot. -/
theorem one_worker (cs : List RepoCmd) (s s' : Repo)
    (h : s.oneWorker) (hrun : s.exec cs = some s') : s'.oneWorker := by
  induction cs generalizing s with
  | nil => simp [Repo.exec] at hrun; subst hrun; exact h
  | cons c cs ih =>
    simp only [Repo.exec, Option.bind_eq_some_iff] at hrun
    obtain ⟨m, hm, hrest⟩ := hrun
    exact ih m (step_oneWorker s m c h hm) hrest

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
section Supervisor

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

end Supervisor

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

end TodoWorkflow
