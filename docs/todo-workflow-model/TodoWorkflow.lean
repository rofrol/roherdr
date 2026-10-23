/-!
# The herdr TODO workflow as a state machine

A hand-written model (see README.md) of how a repository's TODO is worked
through in herdr: the coordination tenure and its handoff, one item's run
through `herdr todo run`, the supervisor that watches a worker for its
owner, and the coordinator's decision at an item boundary. Commands are
named in comments. Only behavior is modelled; no I/O.

`none` from a `step` function means the command is refused or does not
apply in that state; the state is unchanged.
-/

namespace TodoWorkflow

abbrev PaneId := String
abbrev NoteId := String
abbrev RunId := String

/-! ## The coordination tenure (`herdr coordinator ...`) -/

/-- One per repository, kept by the herdr server. -/
inductive Tenure where
  | free
  | held (pane : PaneId)
  /-- after `coordinator ready --note`: no worker start, answer or ack -/
  | frozen (pane : PaneId) (note : NoteId) (successor : Option PaneId)
  /-- the frozen pane is gone: only the winner of the note's claim takes over -/
  | orphaned (note : NoteId)
  deriving DecidableEq, Repr

/-- The result of `herdr handoff claim`: exclusive, exactly one taker wins. -/
inductive Claim where
  | won | lost
  deriving DecidableEq, Repr

inductive TenureCmd where
  | start    (pane : PaneId)                  -- coordinator start
  | finish   (pane : PaneId)                  -- coordinator end
  | ready    (pane : PaneId) (note : NoteId)  -- coordinator ready --note
  | handoff  (pane succ : PaneId)             -- coordinator handoff --successor
  | ack      (succ : PaneId) (claim : Claim)  -- handoff claim, then coordinator ack
  | abort    (pane : PaneId)                  -- coordinator handoff --abort
  | takeOver (pane : PaneId) (claim : Claim)  -- plain start after the frozen pane is gone
  | paneGone (pane : PaneId)                  -- the pane-close event
  deriving DecidableEq, Repr

def Tenure.step : Tenure → TenureCmd → Option Tenure
  | .free,          .start p     => some (.held p)
  | .held h,        .finish p    => if h = p then some .free else none
  | .held h,        .paneGone p  => if h = p then some .free else none
  | .held h,        .ready p n   => if h = p then some (.frozen h n none) else none
  -- one successor pane, not the frozen pane itself
  | .frozen h n none, .handoff p s =>
      if h = p ∧ s ≠ h then some (.frozen h n (some s)) else none
  | .frozen _ _ (some s), .ack p .won => if s = p then some (.held s) else none
  | .frozen h _ _,  .abort p     => if h = p then some (.held h) else none
  | .frozen h n _,  .paneGone p  => if h = p then some (.orphaned n) else none
  | .orphaned _,    .takeOver p .won => some (.held p)
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
  | admissionRefused (id : RunId) -- keeps its durable run id; attached to later
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
  | gateAdmits | gateRefuses (id : RunId) | attach (id : RunId)
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
  | .gate,         .gateRefuses id    => some (.admissionRefused id)
  | .admissionRefused i, .attach j    => if i = j then some .gate else none
  | .working a,    .workerDone        => some (.stopping a)
  | .working a,    .workerBlocked     => some (.stopping a)
  | .stopping a,   .exited            => some (.verifying a)
  | .stopping a,   .stillAliveSeen    => some (.stillAlive a)
  | .stillAlive a, .forceStop         => some (.verifying a)
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

/-! ## The supervisor (deterministic, not an LLM)

It watches one worker for the pane that started it (its owner) and turns
the worker's events into obligations of that owner: `herdr worker wait
--attention` returns them, `herdr worker ack <seq>` clears them after they
are handled, and the Stop hook refuses the owner's stop while one is open.
On the owner's own events (its pane closes, its agent exits, hits a limit,
is blocked on its own question, or ends its turn with a question
unanswered; herdr starts) it escalates an open question to the user. It
never kills or restarts, and acts on no timer. -/

inductive Pending where
  | question (request : String)
  | turnEnded
  | exited
  deriving DecidableEq, Repr

structure Watch where
  open_     : List Pending   -- obligations the owner has not acknowledged
  escalated : List String    -- questions handed to the user's `?` list
  deriving Repr

inductive WorkerEvent where
  | asks (request : String)
  | turnEnds
  | exits
  deriving DecidableEq, Repr

inductive OwnerEvent where
  | answers (request : String)    -- herdr worker answer --request
  | acks (p : Pending)            -- herdr worker ack, after handling it
  | escalates (request : String)  -- herdr worker escalate --request
  | gone                          -- pane closed, agent exited or hit a limit,
                                  -- blocked, turn ended with the question open
  deriving DecidableEq, Repr

inductive SupAction where
  | wakeOwner     -- the owner's wait returns
  | alertUser     -- the question shows in the user's `?` list
  | nothing
  deriving DecidableEq, Repr

def Watch.onWorker (w : Watch) : WorkerEvent → Watch × SupAction
  | .asks r   => ({ w with open_ := w.open_ ++ [.question r] }, .wakeOwner)
  | .turnEnds => ({ w with open_ := w.open_ ++ [.turnEnded] }, .wakeOwner)
  | .exits    => ({ w with open_ := w.open_ ++ [.exited] }, .wakeOwner)

def openQuestions (ps : List Pending) : List String :=
  ps.filterMap fun | .question r => some r | _ => none

def Watch.onOwner (w : Watch) : OwnerEvent → Watch × SupAction
  | .answers r   => ({ w with open_ := w.open_.erase (.question r) }, .nothing)
  | .acks p      => ({ w with open_ := w.open_.erase p }, .nothing)
  | .escalates r => ({ w with escalated := w.escalated ++ [r] }, .alertUser)
  | .gone        =>
      let qs := openQuestions w.open_
      ({ w with escalated := w.escalated ++ qs },
       if qs.isEmpty then .nothing else .alertUser)

/-- The Stop hook: the owner may end its turn only with nothing open. -/
def Watch.ownerMayStop (w : Watch) : Bool := w.open_.isEmpty

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
    else if c.contextPct > handoffAt then .handOff
    else match c.queue.findSome? (fun | .next id => some id | _ => none) with
      | some id => .dispatch id
      | none => if c.queue.isEmpty then .stop else .askUser

/-! ## Properties Lean checks -/

section Tenure

/-- Only the holder ends a tenure, and a held tenure refuses a second start. -/
theorem finish_only_holder (h p : PaneId) (hp : h ≠ p) :
    (Tenure.held h).step (.finish p) = none := by
  simp [Tenure.step, hp]

theorem no_second_start (t : Tenure) (p : PaneId) (ht : t ≠ .free) :
    t.step (.start p) = none := by
  cases t <;> simp_all [Tenure.step]

/-- The successor gets the tenure only by winning the note's claim. -/
theorem ack_needs_claim (h s p : PaneId) (n : NoteId) :
    (Tenure.frozen h n (some s)).step (.ack p .lost) = none := by
  simp [Tenure.step]

/-- Only the named successor can ack. -/
theorem ack_only_successor (h s p : PaneId) (n : NoteId) (c : Claim) (t : Tenure)
    (hstep : (Tenure.frozen h n (some s)).step (.ack p c) = some t) :
    p = s ∧ c = .won := by
  cases c <;> simp [Tenure.step] at hstep
  exact ⟨hstep.1.symm, rfl⟩

/-- After the frozen pane is gone, no plain start takes over: only a won claim. -/
theorem orphaned_needs_claim (n : NoteId) (p : PaneId) :
    (Tenure.orphaned n).step (.start p) = none ∧
    (Tenure.orphaned n).step (.takeOver p .lost) = none := by
  simp [Tenure.step]

/-- One successor per handoff. -/
theorem one_successor (h s s' p : PaneId) (n : NoteId) :
    (Tenure.frozen h n (some s)).step (.handoff p s') = none := by
  simp [Tenure.step]

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

/-- A refused admission never starts a worker by itself: only the same run's
attach goes back to the gate. -/
theorem refused_admission_waits (i : RunId) (e : Event) (r : Run)
    (h : (Run.admissionRefused i).step e = some r) : r = .gate ∧ e = .attach i := by
  cases e <;> simp [Run.step] at h
  obtain ⟨rfl, rfl⟩ := h
  exact ⟨rfl, rfl⟩

end Run

section Supervisor

/-- An open question is never lost when the owner goes away: it reaches the user. -/
theorem gone_escalates (w : Watch) (r : String)
    (h : Pending.question r ∈ w.open_) :
    r ∈ (w.onOwner .gone).1.escalated ∧ (w.onOwner .gone).2 = .alertUser := by
  have hq : r ∈ openQuestions w.open_ := by
    simp only [openQuestions, List.mem_filterMap]; exact ⟨_, h, rfl⟩
  have hne : (openQuestions w.open_).isEmpty = false := by
    cases hl : openQuestions w.open_ <;> simp_all
  simp [Watch.onOwner, hne, hq]

/-- Every worker event wakes the owner and leaves it an obligation, so the
owner cannot stop before handling it. -/
theorem worker_event_blocks_stop (w : Watch) (e : WorkerEvent) :
    (w.onWorker e).2 = .wakeOwner ∧ (w.onWorker e).1.ownerMayStop = false := by
  cases e <;> simp [Watch.onWorker, Watch.ownerMayStop]

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
