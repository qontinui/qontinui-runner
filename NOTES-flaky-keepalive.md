# Checkpoint: flaky `a_held_sessions_own_keepalive_is_not_serving_evidence`

Uncommitted working note (paused 2026-11-19 by the steward: agent-builds.slice over its memory limit).
Base: runner main 9a58d63a4. Test: `src-tauri/src/session/coord_sync_finish_ack_drain_tests.rs` (~L403-440).
Symptom: the final `kinds(&pending_of(&outbox, s)) == ["finished"]` fails with `["finished","heartbeat"]`,
meaning the LAST round's keep-alive heartbeat was never ACKed.

## Done
- Duplicate check: `coord_who_is_working_on` was clear for coord_sync.rs and the test file; there were no recent findings; no related remote branch.
  Open runner PRs were NOT checked over REST (the token was dry).
- No local run has completed yet. The lock and package-cache contention, two account migrations and the memory pause killed every run.

## Analysis so far (by reading; NOT confirmed empirically)
In `drain_tick` (coord_sync.rs ~L1895), a session whose `retry.unconfirmed_finish` is Some gets its newest
heartbeat lifted into its OWN one-row "keep-alive" chain. That chain runs concurrently with the session's
backlog chain (the `finished` row) via `buffer_unordered`, and both chains share one `abort: AtomicBool`.
- In every `push_chain` outcome a heartbeat is ACKed: Acked, Conflict, Refused, PermanentFailure,
  and Transport (a heartbeat is dropped on transport failure). So it stays pending ONLY if:
  (a) its chain hits `abort.load()==true` at the top of `push_chain` (outcome.aborted, nothing pushed), or
  (b) it is not a front candidate and so sits in the finished chain behind the 503. That would mean `front_eligible` is false,
      but the test asserts `unconfirmed_finish.is_some()` every round, so (b) looks impossible.
- Under (a), the finished row's 503 is a Transport fail with `trip_abort == true`, because `ack_ticks` never moves: keep-alives are
  deliberately excluded from `delivered`. So the backlog chain DOES store abort=true. The open question is ordering.
  futures-util 0.3.34's FuturesUnordered polls both newly pushed futures in one `poll_next`, FIFO, and the backlog chain comes first.
  So the keep-alive's abort check should run before the 503 response is processed, even on the `Dequeue::Inconsistent` path.
  The one way through I found: the backlog chain's FIRST poll completes with an immediate transport error (a connect
  syscall error such as EMFILE or EADDRNOTAVAIL on a loaded CI box, or a non-retriable closed pooled conn). In that case abort is set before
  the keep-alive is polled. That is unproven.
- Conclusion to verify: the keep-alive's delivery depends on whether its own session's backlog chain trips the
  shared abort first. It is a product-side ordering race, not a test-only one.

## Next steps
1. Debug instrumentation is already applied in coord_sync.rs (the `DBGFLAKE` eprintln after `push_chain` in drain_tick).
   Build with `cargo-guard.sh test -- a_held_sessions_own_keepalive_is_not_serving_evidence --nocapture` and loop
   it under load until it fails. Read the `aborted=` field of the keepalive=true line in the failing round.
2. If (a) is confirmed, the deterministic fix candidate is: a keep-alive chain is never aborted by a trip from its OWN session's backlog
   chain. Options: give keep-alive chains their own abort flag, or let them trip and ignore abort only for unreachable
   heartbeats; or run the keep-alive chains to completion BEFORE the backlog chains are started (explicit ordering).
   The fix must keep the outage bound (`MAX_CONCURRENT_PUSH_CHAINS`) intact.
   Add a deterministic test that forces the trip-before-keepalive order. Do NOT use a sleep and do NOT loosen the assertion.
3. Remove the DBGFLAKE instrumentation. Run the test 50+ times via cargo-guard. Show that it fails with the fix reverted.
4. Independent review of the diff only. Open the PR, unlabelled unless main is red because of this test. Hand back the worktree:
   `allocate-worktree.sh --done --path <worktree>`.
