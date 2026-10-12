# Added delay is a bug signal

Before you raise a timeout or add a sleep, retry or wait, treat the urge as a
symptom of an asynchronous bug (a race, a lost event, a dropped input, a
non-atomic write) and find the cause: reproduce it with a stress loop, wait for
an observable condition instead of a duration, retry until the effect is
observed, publish files atomically. Lengthen time only for a real external
deadline, rate limit, external polling, designed backoff or measured slow
hardware, and say which next to the value. This covers your own commands
and scripts and the tasks you delegate; a label ("debounce", "tooling")
does not qualify a delay. Every wait names the positive event or condition
it waits for and what produces it, never a negative one ("idle for N", "no
marker for N"). A retry needs an acknowledgement and an idempotent action,
never a blind resend.
It also covers fixes and the options any report or question offers: never
offer a longer timeout, sleep, blind retry or debounce as a fix or option for
an asynchronous bug. A `delay:` label on a constant inside a fix (not a test
harness or tooling) needs the user's or the coordinating agent's agreement,
with the evidence, before it is added; it never serves to get past the delay
check. Whoever passes on another agent's report checks every option in it
against this rule first.

The positive idiom: the state's source of truth is a durable record, with a
persisted cursor. Reconcile that record at named events (startup, handoff, turn
end). A notification only wakes the reconcile, so a lost notification delays
but never corrupts.

Closing instruction: end your answer with a "Timing:" list naming every
time-based element you propose (timer, sweep, periodic check, timeout, retry,
debounce, grace period) and classify each as `event: <the positive event>` or
`external reconciliation: <the external source without notifications>`. If you
propose none, write `Timing: none`.

---

