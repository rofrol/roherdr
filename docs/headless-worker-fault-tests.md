# Fault-injection tests for headless workers

Plan step 7 of the TODO item "Make coordinating headless workers reliable",
with the extra cases of fix 12 in `docs/atomicity-review-2026-10-07.md`.

Pass criterion for every case: no event is lost, every question is resolved
or explicitly escalated, and no effect happens twice.

The Rust tests are in `src/workers/tests.rs` (module `workers::tests`). They
run the supervisor against a stub `claude` and inject faults directly: a
store trigger that raises (`disk full`), a directory standing at the journal
export's path, `Live::fail_next_write` (a broken pipe) and
`Live::hold_next_write` (stops an answer between its stored intent and its
write; both test-only, behind `#[cfg(test)]`). Nothing sleeps: each test
drives the state directly or waits on the supervisor's change events. The
Python tests are in `scripts/test_todo_edit.py`.

"New" marks the tests this step added.

| Case | Test(s) | Notes |
|---|---|---|
| Coordinator killed mid-answer (after `answer_intent`, before `answer_sent`) | New `a_coordinator_killed_mid_answer_retries_its_command_and_nothing_is_sent_twice`; `a_repeated_answer_command_id_sends_one_control_response`, `a_turn_end_cleanup_does_not_erase_an_answer_in_flight`, `a_failed_answer_write_leaves_the_question_pending_and_retryable` | The answer is held between its intent and its write: the question is `answering`, owes nothing and is off the `?` list; another answer (no id, or a new id) is refused as "being sent"; the retry with the same `command_id` gets the first call's reply; one `control_response`. |
| Coordinator killed between an answer and re-arming the wait | New `a_turn_end_before_the_wait_is_re_armed_still_wakes_it`; `an_attention_wait_wakes_on_new_events_and_skips_what_after_has_seen` | The turn ends while no wait is armed; the wait re-armed with the old `after` returns the turn end at once, and the obligation stays until acked. |
| Two questions at once | `an_answer_must_name_one_of_several_questions`; new `two_questions_at_once_reach_the_wait_and_each_answer_lands_once` | One wait returns both; two callers answer them concurrently, each lands once. |
| Two answers without a request id | `an_answer_must_name_one_of_several_questions` (refused while several are pending); new `two_answers_without_a_request_id_send_one` | Two concurrent unnamed answers to one question: exactly one wins, one `answer_intent`, one `control_response`. |
| A worker exiting during a wait | `an_attention_wait_wakes_on_new_events_and_skips_what_after_has_seen` (stop during the wait), `questions_end_with_the_worker`, `a_worker_that_exits_resolves_its_questions_without_escalating`, `a_crash_ends_as_exited_without_a_result` | |
| Server restart (store reopen) mid-wait | New `a_wait_on_the_next_server_wakes_when_the_old_one_lets_its_worker_go`; `a_restart_leaves_a_journal_another_server_owns_until_it_lets_go`, `a_restart_marks_unfinished_workers_lost_and_keeps_exited_ones`, `a_reopened_supervisor_rebuilds_the_same_workers_from_the_store` | A wait re-armed on the next server with the `after` the old one gave wakes (Gone) when a forced handoff ends the worker; the question settles as "the worker exited"; no `lost`; the owner owes the end. |
| Server restart with an `answering` question | `a_restart_expires_an_answer_in_flight_of_a_gone_worker` (process gone: expired); new `a_restart_with_an_answer_in_flight_to_a_live_process_says_so` (process alive: `degraded`, the question kept as `answering`, the wait answers Gone) | |
| Live handoff with a running worker (refused) | `a_handoff_is_refused_while_any_worker_process_is_alive` | |
| Live handoff with an idle worker | `a_handoff_is_refused_while_any_worker_process_is_alive` (idle between turns is refused too), `a_handoff_goes_ahead_once_stopped_workers_have_exited` | |
| Duplicate ack; ack older than the stored one | `an_ack_is_idempotent_and_monotonic` | |
| Lost ack (the wait wakes again) | New `a_lost_ack_leaves_the_obligation_and_the_wait_returns_it_again` | The obligation stays, also after a restart; a wait without `after` returns the same event; the late ack, repeated, records one `acked`. |
| Event committed between the status read and the wait's registration | `an_event_committed_between_the_check_and_the_block_is_not_missed` | |
| Two takeovers of an exited worker | `an_exited_worker_is_taken_over_once` (one after the other); new `two_takeovers_of_an_exited_worker_at_once_claim_it_once` | Concurrent: one wins, one `takeover` event, the other `worker_busy`. |
| Store write failing (disk full) | `a_failed_store_write_marks_the_worker_degraded`, `a_command_id_is_reserved_with_its_first_event`; new `a_question_asked_while_the_store_fails_is_not_lost` | The question still wakes the wait, is owed and listed, is answered once; the worker is `degraded`; the journal export keeps the event without a `seq`. |
| Server restart after a failed store write | `a_degraded_mark_is_stored_with_the_next_write_and_survives_a_restart`, `a_restart_after_a_failed_store_write_still_says_the_record_has_a_gap`, `an_imported_journal_without_seqs_has_no_gap` | The mark is stored with the next write the store takes; when none came before the server ended, the next one finds the journal export's records after its last `seq` and marks the worker degraded with that count. |
| Journal write failing | New `a_failed_journal_write_marks_the_worker_degraded_and_loses_no_event` | The worker is `degraded`; the store has every event. |
| TODO write conflict (todo_edit while the file changes) | `test_concurrent_change_refuses` (mocked reads, both checks); new `test_a_real_write_while_editing_refuses_and_keeps_the_other_change`; `test_a_write_while_the_temp_file_is_written_refuses_and_is_not_lost`; `test_two_concurrent_runs_serialize` | Two todo_edit runs take turns on `<file>.lock`; a writer without the lock is caught by the byte comparison right before the rename. |

## Gaps found, then closed

- **todo_edit's check-then-rename window.** `atomic_write` compared the file
  with what it read, then renamed its temporary file over it, without a
  lock, so another writer's change in between was replaced silently. Closed:
  every edit holds an exclusive `flock` on `<file>.lock` from its read to its
  rename, so two todo_edit runs serialize, and the file is compared byte for
  byte again right before the rename, after the temporary file is written,
  so a writer that does not take the lock makes the edit refuse. Left open:
  such a writer landing between that last comparison and the rename itself;
  only a lock it takes too could close that.
- **`degraded` was kept in memory only.** A store write failure marked the
  worker degraded, but after a server restart the status no longer said
  that the store's record of that worker has gaps. Closed: the mark is a
  column of the worker's row (`degraded`), written with the next event the
  store takes; and when the server ended before that, the next one counts
  the journal export's records after its last one with a `seq` (only a
  failed store write exports one without) and marks the worker degraded
  with that count; the next event it stores (such as `lost`) stores the
  mark, and until then every open finds the gap again.
