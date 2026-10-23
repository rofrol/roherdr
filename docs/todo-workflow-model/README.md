# TODO workflow model

A hand-written model of how a repository's TODO is worked through with herdr:
the coordination tenure and its handoff, one item's run through
`herdr todo run`, the supervisor that watches a worker for its owner, and the
coordinator's decision at an item boundary.

**These proofs establish properties of this model only.** `TodoWorkflow.lean`
was written by hand on 2026-10-23 (at commit `8400a511e`; revised after consult round `20261023-133232-9be6`) from `AGENTS.md`,
`DECISIONS.md`, `.herdr/checks.toml`, the CLI help and the global `todo` and
`handoff` skills. It is not extracted from the Rust code, and nothing checks
that the code still matches it. The Rust code and its tests are the source of
truth; the diagram below only illustrates the model. Consult round
`20261023-132611-0125` chose this place and form.

## Check

```sh
lean docs/todo-workflow-model/TodoWorkflow.lean   # Lean 4.34.0, no Mathlib
```

It prints the one `#eval` (`TodoWorkflow.Action.dispatch "t-b"`) and nothing
else when every theorem holds. Lean is not part of `just check`.

## What it proves

Over any sequence of events, not just one step:

- Nothing lands (cherry-pick, install, TODO edit, push, done) without a
  `verified` verdict (`landing_needs_verified`, `done_needs_verified`).
- Every attempt number stays within 1..3 (`attempts_bounded`).

For single steps:

- Only the holder ends a tenure; a held, frozen or orphaned tenure refuses a
  plain start; one successor per handoff; only the named successor acks, and
  only after winning the note's claim; after the frozen pane is gone only a
  start that wins the claim takes over.
- A refused admission waits for the same run's attach; it never starts a
  worker by itself.
- The supervisor: every worker event wakes the owner and blocks its stop until
  handled; an open question reaches the user when the owner goes away.
- The coordinator dispatches only from its own pane holding an unfrozen tenure.

## Where it maps to the code

| Model | Code |
|---|---|
| `Tenure`, `Tenure.step` | `src/workers/coordinators.rs`, `src/workers/coordinator_handoff.rs`, `src/cli/coordinator.rs` |
| handoff note claim | `src/cli/handoff.rs` |
| `Run`, `Run.step` | `src/workers/runs/` (`finish.rs`: install, TODO edit, push), `src/cli/todo.rs` |
| verify | `src/workers/verify/`, `.herdr/checks.toml` |
| `Watch` (the supervisor) | `src/workers/supervisor/questions.rs` (questions, escalation), worker wait/ack and the Stop hook |
| `Coordinator.decide` | the global `todo` skill (agent-kit), not herdr code |

`Watch` is the supervision the global rules describe (a deterministic event
loop that wakes the owner or alerts the user, never kills, no timer); the
`WorkerSupervisor` type in the code also starts, stops and kills workers,
which the model leaves out. Left out too: `todo wait`/`resume` events and
their acknowledgements, queue mode, `--close`/`--next` chaining, the counterfactual
check and auto-review.

## Keeping it current

A commit that changes tenure, handoff, dispatch, verify, landing or a failed
state updates the model and the diagram in the same commit, or says in its
message why they are unaffected. Then update the commit and date above.

`todo-workflow.svg` is the diagram below, rendered with plain SVG text labels
(so viewers without HTML-in-SVG show them). Re-render it whenever the
diagram changes:

```sh
echo '{"htmlLabels":false,"flowchart":{"htmlLabels":false}}' > config.json
npx -p @mermaid-js/mermaid-cli@11.4.2 mmdc -c config.json -i diagram.mmd -o todo-workflow.svg -b white
```

with the block below saved as `diagram.mmd`, and commit both.

## Diagram

```mermaid
flowchart TD
  subgraph C["Coordinator (LLM in a herdr pane) — Coordinator.decide"]
    C0{"in a herdr pane?"} -->|no| C1["refuseOutsidePane: offer herdr coordinator open, stop"]
    C0 -->|yes| CT{"this pane holds an unfrozen tenure?"}
    CT -->|no| CN["refuseNoTenure"]
    CT -->|yes| C2{"context over 70%?"}
    C2 -->|yes| HO["handOff"]
    C2 -->|no| C3{"a Next item?"}
    C3 -->|yes| D["dispatch: herdr todo run item"]
    C3 -->|"no, items wait"| ASK["askUser"]
    C3 -->|"queue empty"| STOP["stop"]
  end

  subgraph T["Tenure (herdr server) — Tenure.step"]
    TF(["free"]) -->|"coordinator start"| TH(["held pane"])
    TH -->|"coordinator end / pane gone"| TF
    TH -->|"coordinator ready --note"| TZ(["frozen, no successor"])
    TZ -->|"coordinator handoff --successor (once)"| TZS(["frozen, successor named"])
    TZS -->|"successor wins the note claim + coordinator ack"| TH2(["held by the successor"])
    TZ -->|"--abort"| TH
    TZS -->|"--abort"| TH
    TZ -->|"frozen pane gone"| TO(["orphaned note"])
    TZS -->|"frozen pane gone"| TO
    TO -->|"a start that wins the note claim"| TH3(["held by the taker"])
  end
  HO --> TZ

  subgraph R["One item's run (herdr todo run) — Run.step"]
    P["preflight: checks.toml, operations.toml, disk"] -->|ok| G{"usage gate"}
    P -->|refused| B(["blocked"])
    G -->|refuses| AR(["admission refused, run id kept"])
    AR -->|"attach the same run id"| G
    G -->|admits| W["worker working, attempt n (folder slot)"]
    W -->|"WORKER-DONE or WORKER-BLOCKED"| S["stopping"]
    S -->|exited| V{"herdr worker verify"}
    S -->|still_alive| SA(["still alive"])
    SA -->|force-stop| V
    V -->|unavailable| VU(["unavailable"])
    VU -->|"verify again"| V
    V -->|failed| VF(["failed"])
    VF -->|"retry with a new task, n < 3"| W
    VU -->|abort| B
    VF -->|abort| B
    V -->|verified| CP["cherry-pick onto master"]
    CP -->|fail| B
    CP -->|ok| I["install: just clean-install TODO.md"]
    I -->|fail| IF(["install_failed"])
    IF -->|retry-install| I
    IF -->|skip-install| E
    I -->|ok| E["TODO edit: approve --note / --close"]
    E -->|fail| EF(["todo_failed"])
    EF -->|retry-todo| E
    EF -->|skip-todo| PU
    E -->|ok| PU["push: fast-forward only"]
    PU -->|fail| PF(["push_failed"])
    PF -->|retry-push| PU
    PU -->|ok| DONE(["done"])
    IF -->|abort| B
    EF -->|abort| B
    PF -->|abort| B
  end
  D --> P

  subgraph SV["Supervisor (deterministic, no timer, never kills) — Watch"]
    WE["worker asks / ends a turn / exits"] -->|"obligation + wake the owner"| OB(["open obligation: owner may not stop"])
    OB -->|"answer or ack after handling"| CL(["cleared"])
    OB -->|"owner escalates, or owner gone with a question open"| UQ(["question in the user's ? list"])
  end
  W -.-> WE
  OB -.->|"herdr worker wait --attention returns"| C0

  DONE -->|"next item boundary"| C0
  B -->|"Needs a decision / report"| C0
```
