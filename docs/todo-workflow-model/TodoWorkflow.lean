/-!
# The herdr TODO workflow as a state machine

A model of how a repository's TODO is worked through in herdr (2026-10-23):
the coordination tenure and its handoff, one item's run through
`herdr todo run`, and the coordinator's loop. Commands are named in the
comments. Only behavior is modelled; no I/O.
-/

namespace TodoWorkflow

abbrev PaneId := String
abbrev NoteId := String

/-! ## The coordination tenure (`herdr coordinator ...`) -/

/-- One per repository, kept by the herdr server. -/
inductive Tenure where
  | free
  | held   (pane : PaneId)
  /-- after `coordinator ready --note`: no worker start, answer or ack -/
  | frozen (pane : PaneId) (note : NoteId) (successor : Option PaneId)
  deriving DecidableEq, Repr

inductive TenureCmd where
  | start     (pane : PaneId)                 -- coordinator start
  | finish    (pane : PaneId)                 -- coordinator end
  | ready     (pane : PaneId) (note : NoteId) -- coordinator ready --note
  | handoff   (pane succ : PaneId)            -- coordinator handoff --successor
  | ack       (succ : PaneId) (claimed : Bool) -- handoff claim, then coordinator ack
  | abort     (pane : PaneId)                 -- coordinator handoff --abort
  | paneGone  (pane : PaneId)                 -- the kernel/pane-close event
  deriving Repr

/-- `none` is a refusal (`coordinator_active` and the like): the tenure is unchanged. -/
def Tenure.step : Tenure → TenureCmd → Option Tenure
  | .free,          .start p        => some (.held p)
  | .held h,        .finish p       => if h = p then some .free else none
  | .held h,        .ready p n      => if h = p then some (.frozen h n none) else none
  | .frozen h n _,  .handoff p s    => if h = p then some (.frozen h n (some s)) else none
  -- the named successor takes it only after winning the note's claim
  | .frozen _ _ (some s), .ack p c  => if s = p ∧ c then some (.held s) else none
  | .frozen h _ _,  .abort p        => if h = p then some (.held h) else none
  | .held h,        .paneGone p     => if h = p then some .free else none
  -- the frozen holder is gone: a plain start may claim the note and take over
  | .frozen h _ _,  .paneGone p     => if h = p then some .free else none
  | _,              _               => none

/-! ## One item's run (`herdr todo run <item> --task --message --paths --check`) -/

inductive Run where
  | preflight          -- checks.toml, operations.toml, free disk
  | gate               -- usage gate admits the subscription
  | working  (attempt : Nat)   -- headless worker in the folder slot
  | stopping (attempt : Nat)   -- worker stopped; `still_alive` needs force-stop
  | verifying (attempt : Nat)  -- herdr worker verify
  | cherryPick
  | installing         -- [install] argv: just clean-install TODO.md
  | todoEdit           -- approve --note / --close via todo_edit.py
  | pushing            -- fast-forward only, never forced
  | done
  | blocked (why : String)
  | installFailed | todoFailed | pushFailed
  deriving DecidableEq, Repr

inductive Verdict where
  | verified | failed | unavailable
  deriving DecidableEq, Repr

inductive Event where
  | preflightOk | preflightRefused (why : String)
  | gateAdmits | gateRefuses
  | workerDone | workerBlocked (why : String)
  | exited | stillAlive | forceStop
  | verify (v : Verdict) | retry | verifyAgain
  | ok | fail
  | retryInstall | skipInstall | retryTodo | skipTodo | retryPush | abort
  deriving Repr

def maxAttempts : Nat := 3

/-- The driver's transitions; `none` means the event does not apply. -/
def Run.step : Run → Event → Option Run
  | .preflight,    .preflightOk          => some .gate
  | .preflight,    .preflightRefused w   => some (.blocked w)
  -- high or unknown usage is a blocker, never bypassed
  | .gate,         .gateAdmits           => some (.working 1)
  | .gate,         .gateRefuses          => some (.blocked "usage gate")
  | .working a,    .workerDone           => some (.stopping a)
  | .working _,    .workerBlocked w      => some (.blocked w)
  | .stopping a,   .exited               => some (.verifying a)
  | .stopping a,   .stillAlive           => some (.stopping a)
  | .stopping a,   .forceStop            => some (.verifying a)
  | .verifying _,  .verify .verified     => some .cherryPick
  | .verifying a,  .verify .unavailable  => some (.verifying a)   -- not a pass
  | .verifying a,  .verify .failed       =>
      if a < maxAttempts then some (.verifying a) else some (.blocked "verify failed")
  | .verifying a,  .retry                =>
      if a < maxAttempts then some (.working (a + 1)) else none
  | .cherryPick,   .ok                   => some .installing
  | .cherryPick,   .fail                 => some (.blocked "cherry-pick")
  | .installing,   .ok                   => some .todoEdit
  | .installing,   .fail                 => some .installFailed
  | .installFailed, .retryInstall        => some .installing
  | .installFailed, .skipInstall         => some .todoEdit
  | .todoEdit,     .ok                   => some .pushing
  | .todoEdit,     .fail                 => some .todoFailed
  | .todoFailed,   .retryTodo            => some .todoEdit
  | .todoFailed,   .skipTodo             => some .pushing
  | .pushing,      .ok                   => some .done
  | .pushing,      .fail                 => some .pushFailed
  | .pushFailed,   .retryPush            => some .pushing
  | .installFailed, .abort | .todoFailed, .abort | .pushFailed, .abort
                                         => some (.blocked "aborted")
  | _,             _                     => none

/-- Runs every event in order; stops at the first one that does not apply. -/
def Run.run (r : Run) : List Event → Option Run
  | []      => some r
  | e :: es => (r.step e).bind (·.run es)

/-! ## The coordinator (an LLM session in a herdr pane) -/

inductive Item where
  | next (id : String)
  | needsDecision (id : String) (question : String)
  deriving Repr

structure Coordinator where
  pane       : Option PaneId   -- none: outside a herdr pane
  contextPct : Nat
  queue      : List Item
  deriving Repr

inductive Action where
  | refuseOutsidePane      -- offer `herdr coordinator open` and stop
  | handOff                -- over 70% at an item boundary
  | askUser                -- every remaining item waits on the user
  | dispatch (id : String) -- herdr todo run for the top item
  | stop                   -- "Next, in order" is empty
  deriving DecidableEq, Repr

def handoffAt : Nat := 70

/-- What the coordinator does at an item boundary. -/
def Coordinator.decide (c : Coordinator) : Action :=
  match c.pane with
  | none => .refuseOutsidePane
  | some _ =>
    if c.contextPct > handoffAt then .handOff else
    match c.queue.find? (fun | .next _ => true | _ => false) with
    | some (.next id) => .dispatch id
    | _ => if c.queue.isEmpty then .stop else .askUser

/-! ## Properties Lean checks -/

/-- Only the holder ends a tenure. -/
theorem finish_only_holder (h p : PaneId) (hp : h ≠ p) :
    (Tenure.held h).step (.finish p) = none := by
  simp [Tenure.step, hp]

/-- A held tenure refuses every other start (no second coordinator). -/
theorem no_second_start (h p : PaneId) :
    (Tenure.held h).step (.start p) = none := by
  simp [Tenure.step]

/-- A frozen tenure refuses a plain start while its pane lives. -/
theorem frozen_refuses_start (h p : PaneId) (n : NoteId) (s : Option PaneId) :
    (Tenure.frozen h n s).step (.start p) = none := by
  simp [Tenure.step]

/-- The successor gets the tenure only after winning the note's claim. -/
theorem ack_needs_claim (h s : PaneId) (n : NoteId) :
    (Tenure.frozen h n (some s)).step (.ack s false) = none := by
  simp [Tenure.step]

/-- Nothing lands without a `verified` verdict: from verifying, only it leads on. -/
theorem land_needs_verified (a : Nat) (v : Verdict) (r : Run)
    (h : (Run.verifying a).step (.verify v) = some r) (hr : r = .cherryPick) :
    v = .verified := by
  cases v <;> simp [Run.step] at h
  · rfl
  · split at h <;> simp_all
  · simp_all

/-- No fourth attempt. -/
theorem no_fourth_attempt :
    (Run.verifying maxAttempts).step .retry = none := by
  simp [Run.step, maxAttempts]

/-- The usage gate is never bypassed: a refused gate never starts a worker. -/
theorem gate_refusal_blocks :
    Run.gate.step .gateRefuses = some (.blocked "usage gate") := rfl

/-- Outside a herdr pane the coordinator never dispatches. -/
theorem outside_pane_no_dispatch (c : Coordinator) (h : c.pane = none) :
    c.decide = .refuseOutsidePane := by
  simp [Coordinator.decide, h]

/-- A happy path, end to end. -/
example :
    Run.preflight.run
      [.preflightOk, .gateAdmits, .workerDone, .exited, .verify .verified,
       .ok, .ok, .ok, .ok] = some .done := rfl

/-- A failed push waits for `retry-push`, then lands. -/
example :
    Run.cherryPick.run [.ok, .ok, .ok, .fail, .retryPush, .ok] = some .done := rfl

#eval (Coordinator.mk (some "pane-1") 40
        [.needsDecision "t-a" "which?", .next "t-b"]).decide   -- dispatch "t-b"

end TodoWorkflow
