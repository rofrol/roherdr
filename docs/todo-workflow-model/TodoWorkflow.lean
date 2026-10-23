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
Abstracted away: run and item identity (one run at a time), branch heads and
ancestry (the cherry-pick and the fast-forward push are trusted steps).
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
  | .frozen h n s c, .paneGone p => if h = p then some (.orphaned n s c) else none
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
the worker's events into numbered obligations of that owner: `herdr worker
wait --attention` returns them, `herdr worker ack <seq>` clears one after it
is handled, and the Stop hook refuses the owner's stop while one is open. A
question is handled once answered (`worker answer --request`) or handed to
the user (`worker escalate`). On the owner's own events (its pane closes,
its agent exits, hits a limit, is blocked on its own question, or ends its
turn with a question unanswered; herdr starts) the supervisor escalates every
unanswered question to the user. It never kills or restarts, and acts on no
timer. (`WorkerSupervisor` in the code also starts, stops and kills workers;
that part is not modelled.) -/

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

structure Watch where
  nextSeq     : Nat
  obligations : List Obligation   -- not yet acknowledged by the owner
  escalated   : List String       -- questions handed to the user's `?` list
  handled     : List String       -- questions answered and acknowledged
  deriving Repr

inductive WorkerEvent where
  | asks (request : String)
  | turnEnds
  | exits
  deriving DecidableEq, Repr

inductive OwnerEvent where
  | answers (seq : Nat)            -- herdr worker answer --request
  | acks (seq : Nat)              -- herdr worker ack <seq>, after handling it
  | escalates (seq : Nat)          -- herdr worker escalate --request
  | gone                          -- pane closed, agent exited or hit a limit,
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
  ({ w with nextSeq := w.nextSeq + 1,
            obligations := w.obligations ++ [⟨w.nextSeq, kind, false⟩] }, .wakeOwner)

def Obligation.unanswered : Obligation → Option String
  | ⟨_, .question r, false⟩ => some r
  | _ => none

/-- `none`: refused (an unknown seq, an answer or escalation of something that
is not an open question, an unanswered question acked). -/
def Watch.onOwner (w : Watch) : OwnerEvent → Option (Watch × SupAction)
  | .answers s =>
      match w.obligations.find? (·.seq == s) with
      | some o =>
        if o.unanswered.isSome then
          some ({ w with obligations := w.obligations.map fun o' =>
                    if o' = o then { o with answered := true } else o' }, .nothing)
        else none
      | none => none
  | .acks s =>
      match w.obligations.find? (·.seq == s) with
      | none => none
      | some o =>
        if o.unanswered.isSome then none
        else some ({ w with obligations := w.obligations.erase o,
                            handled := w.handled ++ match o.kind with
                              | .question r => [r] | _ => [] }, .nothing)
  | .escalates s =>
      match w.obligations.find? (·.seq == s) with
      | some o =>
        match o.unanswered with
        | some r => some ({ w with obligations := w.obligations.erase o,
                                   escalated := w.escalated ++ [r] }, .alertUser)
        | none => none
      | none => none
  | .gone =>
      let qs := w.obligations.filterMap Obligation.unanswered
      some ({ w with obligations := w.obligations.filter (·.unanswered.isNone),
                     escalated := w.escalated ++ qs },
            if qs.isEmpty then .nothing else .alertUser)

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

/-- A question the user asked about is still accounted for: open, handed to
the user, or answered and acknowledged. -/
def Watch.accounts (w : Watch) (r : String) : Prop :=
  (∃ o ∈ w.obligations, o.kind = .question r) ∨ r ∈ w.escalated ∨ r ∈ w.handled
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

/-- A refused admission never starts a worker by itself: only the same run's
attach goes back to the gate. -/
theorem refused_admission_waits (i : RunId) (e : Event) (r : Run)
    (h : (Run.admissionRefused i).step e = some r) : r = .gate ∧ e = .attach i := by
  cases e <;> simp [Run.step] at h
  obtain ⟨rfl, rfl⟩ := h
  exact ⟨rfl, rfl⟩

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

section Supervisor

/-- Every worker event wakes the owner and leaves it an obligation, so the
owner cannot stop before acknowledging it. -/
theorem worker_event_blocks_stop (w : Watch) (e : WorkerEvent) :
    (w.onWorker e).2 = .wakeOwner ∧ (w.onWorker e).1.ownerMayStop = false := by
  cases e <;> simp [Watch.onWorker, Watch.ownerMayStop]

/-- An asked question is accounted for at once. -/
theorem asked_accounted (w : Watch) (r : String) :
    (w.onWorker (.asks r)).1.accounts r := by
  left; exact ⟨⟨w.nextSeq, .question r, false⟩, by simp [Watch.onWorker], rfl⟩

theorem onWorker_accounts (w : Watch) (e : WorkerEvent) (r : String)
    (h : w.accounts r) : (w.onWorker e).1.accounts r := by
  rcases h with ⟨o, ho, hk⟩ | h | h
  · exact Or.inl ⟨o, by simp [Watch.onWorker, ho], hk⟩
  · exact Or.inr (Or.inl (by simpa [Watch.onWorker] using h))
  · exact Or.inr (Or.inr (by simpa [Watch.onWorker] using h))

theorem onOwner_accounts (w w' : Watch) (a : SupAction) (e : OwnerEvent) (r : String)
    (h : w.accounts r) (he : w.onOwner e = some (w', a)) : w'.accounts r := by
  cases e with
  | answers s =>
    simp only [Watch.onOwner] at he
    split at he
    · rename_i o _
      split at he
      · simp only [Option.some.injEq, Prod.mk.injEq] at he
        obtain ⟨rfl, -⟩ := he
        rcases h with ⟨o', ho', hk⟩ | h | h
        · refine Or.inl ⟨_, List.mem_map.2 ⟨o', ho', rfl⟩, ?_⟩
          split <;> simp_all
        · exact Or.inr (Or.inl h)
        · exact Or.inr (Or.inr h)
      · simp at he
    · simp at he
  | acks s =>
    simp only [Watch.onOwner] at he
    split at he
    · simp at he
    · rename_i o _
      split at he
      · simp at he
      · simp only [Option.some.injEq, Prod.mk.injEq] at he
        obtain ⟨rfl, -⟩ := he
        rcases h with ⟨o', ho', hk⟩ | h | h
        · by_cases heq : o' = o
          · subst heq; right; right; simp [hk]
          · exact Or.inl ⟨o', (List.mem_erase_of_ne heq).2 ho', hk⟩
        · exact Or.inr (Or.inl h)
        · exact Or.inr (Or.inr (List.mem_append_left _ h))
  | escalates s =>
    simp only [Watch.onOwner] at he
    split at he
    · rename_i o _
      split at he
      · rename_i q hu
        simp only [Option.some.injEq, Prod.mk.injEq] at he
        obtain ⟨rfl, -⟩ := he
        rcases h with ⟨o', ho', hk⟩ | h | h
        · by_cases heq : o' = o
          · subst heq
            have : q = r := by
              obtain ⟨_, k, b⟩ := o'
              cases k <;> cases b <;> simp_all [Obligation.unanswered]
            subst this
            right; left; simp
          · exact Or.inl ⟨o', (List.mem_erase_of_ne heq).2 ho', hk⟩
        · exact Or.inr (Or.inl (List.mem_append_left _ h))
        · exact Or.inr (Or.inr h)
      · simp at he
    · simp at he
  | gone =>
    simp only [Watch.onOwner, Option.some.injEq, Prod.mk.injEq] at he
    obtain ⟨rfl, -⟩ := he
    rcases h with ⟨o, ho, hk⟩ | h | h
    · cases hu : o.unanswered with
      | none => exact Or.inl ⟨o, List.mem_filter.2 ⟨ho, by simp [hu]⟩, hk⟩
      | some q =>
        right; left
        have : q = r := by
          obtain ⟨_, k, b⟩ := o
          cases k <;> cases b <;> simp_all [Obligation.unanswered]
        subst this
        exact List.mem_append_right _ (List.mem_filterMap.2 ⟨o, ho, hu⟩)
    · exact Or.inr (Or.inl (List.mem_append_left _ h))
    · exact Or.inr (Or.inr h)

/-- A question is never lost: over any events of the worker and its owner,
it stays open, goes to the user, or is answered and acknowledged. -/
theorem question_never_lost (es : List SupEvent) (w w' : Watch) (r : String)
    (h : w.accounts r) (hrun : w.run es = some w') : w'.accounts r := by
  induction es generalizing w with
  | nil => simp [Watch.run] at hrun; subst hrun; exact h
  | cons e es ih =>
    simp only [Watch.run, Option.bind_eq_some_iff] at hrun
    obtain ⟨m, hm, hrest⟩ := hrun
    apply ih m _ hrest
    cases e with
    | worker e => simp [Watch.step] at hm; subst hm; exact onWorker_accounts w e r h
    | owner e =>
      simp only [Watch.step, Option.map_eq_some_iff] at hm
      obtain ⟨⟨w1, a⟩, h1, rfl⟩ := hm
      exact onOwner_accounts w w1 a e r h h1

/-- An unanswered question cannot be acknowledged away. -/
theorem ack_refuses_unanswered (w : Watch) (s : Nat) (o : Obligation)
    (hf : w.obligations.find? (·.seq == s) = some o) (hu : o.unanswered.isSome) :
    w.onOwner (.acks s) = none := by
  simp [Watch.onOwner, hf, hu]

/-- When the owner goes away, every unanswered question reaches the user. -/
theorem gone_escalates (w : Watch) (o : Obligation) (r : String)
    (ho : o ∈ w.obligations) (hk : o.kind = .question r) (ha : o.answered = false) :
    ∃ w', w.onOwner .gone = some (w', .alertUser) ∧ r ∈ w'.escalated := by
  have hu : o.unanswered = some r := by
    obtain ⟨_, k, b⟩ := o; simp_all [Obligation.unanswered]
  have hm : r ∈ w.obligations.filterMap Obligation.unanswered :=
    List.mem_filterMap.2 ⟨o, ho, hu⟩
  have hne : (w.obligations.filterMap Obligation.unanswered).isEmpty = false := by
    cases hl : w.obligations.filterMap Obligation.unanswered <;> simp_all
  refine ⟨{ w with obligations := w.obligations.filter (·.unanswered.isNone),
                   escalated := w.escalated ++ w.obligations.filterMap Obligation.unanswered },
          by simp [Watch.onOwner, hne], List.mem_append_right _ hm⟩

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
