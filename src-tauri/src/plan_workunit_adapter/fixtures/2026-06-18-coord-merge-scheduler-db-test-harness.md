# Add a `coord_test` DB harness to `merge_scheduler.rs` + land-push-failure transition/lease-gauge tests

> **Status: IN PROGRESS 2026-06-18 — implemented, coord PR #694 OPEN pending merge.**
> Harness + DB tests built on branch `test/land-push-failure-db-tests` (worktree
> `qontinui-coord-wt-landfail`), +993 lines in `merge_scheduler.rs` (tests-only).
> All new tests PASS in CI (4115 passed); the sole `coord-db-tests` red was an
> UNRELATED flake — `escalation_metrics_worker::tests::test_metrics_log_file_creation`
> (shared process-global metrics-log path, races under `--test-threads=2`; that file
> is NOT in this PR's diff) — re-run triggered to clear it. Upstream wedge fix #687
> MERGED + SHIPPED (`4dd316b`). A `pr_merged` gate auto-resumes the SHIPPED stamp +
> archive on #694 merge. Started from DRAFT 2026-06-18 (implemented without a separate
> vet pass — the plan was small + tests-only). Depends-On: 2026-06-17-coord-land-push-failure-wedge-and-terminal-classification.

## Why

`on_land_push_failure` (row-state) and `land_or_shadow`'s push-failure lease release
are new and unverified end-to-end. The lease math is the dangerous part: `release_main_merge_claims`
decrements `MAIN_MERGE_LEASES_HELD` on **both** `Released` and `NotHeld`
(`merge_scheduler.rs:~12176`), so a release for a never-acquired claim underflows the
gauge — exactly the bug the wedge-fix design avoided by keeping release path-specific.
A regression here is silent (a wrong gauge), so it needs a DB-backed assertion.

## Discovered prior art (reuse — do NOT invent a harness)

| Piece | Location | Notes |
|---|---|---|
| Per-module test-state builder | `agent_sessions.rs:122` `async fn test_state() -> Option<Arc<AppState>>` | Reads `COORD_TEST_PG_DSN`; `None` ⇒ test returns early (skips when no DB). Copy this shape verbatim. |
| State constructor | `crate::state::build_test_state(&dsn, "redis://127.0.0.1:6379/0")` | Builds `Arc<AppState>` from params (no `set_var` races). |
| Idempotent DDL bootstrap | `agent_sessions.rs:130` `ensure_schema` → `crate::test_fixtures::batch_execute_ddl_locked(&conn, r#"CREATE SCHEMA IF NOT EXISTS coord; CREATE TABLE IF NOT EXISTS …"#)` | Advisory-locked so parallel tests don't race the DDL. Bootstrap only the tables this test touches. |
| Skip guard | `match test_state().await { Some(s) => s, None => return }` | Standard early-return; CI sets `COORD_TEST_PG_DSN` (ci.yml `cargo test --bins`, COORD_TEST_PG_DSN exported), local-without-PG just skips. |
| Claim primitives | `crate::claims::acquire` / `release` (kind `MainMerge`, `resource_key=format!("repo:{}", repo)`, `machine_id = state.leader.holder_id()`) | Same calls the production land paths use — acquire to set up "lease held", assert the gauge, then drive the release. |
| Gauge | `static MAIN_MERGE_LEASES_HELD` | Read `.load(Ordering::Relaxed)` before/after; assert deltas, not absolutes (it's process-global). |
| Partial counter | `static MERGE_PARTIAL_MULTI_REPO_LAND` (added by #687) | Same before/after-delta read. |
| Existing test module | `merge_scheduler.rs:12390` `mod tests` (`use serial_test::serial;` already imported) | Add the harness + DB tests here. The pure-helper `on_land_push_failure_branch_selection` test (`:~12424`) stays; the new DB tests assert the real side effects it can't. |

## Scope

In scope: a `test_state`/`ensure_schema` harness local to `merge_scheduler.rs`'s test
module, and DB-backed tests for (a) `on_land_push_failure`'s three row transitions +
metric increment, (b) the land-layer lease-release gauge balance.
Out of scope: a shared cross-module harness refactor (every module rolls its own
`test_state` today — follow that convention, don't boil the ocean); changing any
production code in `merge_scheduler.rs`/`outbound_git.rs` (this is tests-only — if a
test reveals a seam is untestable, note it, don't refactor under a test plan).

## Design

### Phase 1 — harness in the `merge_scheduler` test module
Add (inside `mod tests`, gated like `agent_sessions`):
- `async fn test_state() -> Option<Arc<AppState>>` — copy `agent_sessions.rs:122`.
- `async fn ensure_schema(state)` — `batch_execute_ddl_locked` bootstrapping ONLY what
  these tests touch: `coord.merge_proposals` (the columns `on_land_push_failure`/`set_error`
  write: `proposal_id`, `status`, `error`, `updated_at`, plus any NOT NULL the production
  INSERT path needs — derive the minimal subset from the real migration, mirroring how
  `agent_sessions::ensure_schema` mirrors its Phase-1 migration) and the `coord.claims`
  table(s) `crate::claims::acquire/release` read/write. Idempotent (`IF NOT EXISTS`).
  **Vet must confirm** the exact NOT-NULL/column set against the live migrations
  (`alembic/versions/`) and against `claims.rs` — under-bootstrapping a NOT NULL column
  is the classic first-CI-red (cf. `test_support::create_plans_for_test` lesson, memory
  [[reference_cross_repo_schema_add_drift_gate_deadlock]] sibling).
- A small `seed_landing_proposal(state, proposal_id, repos)` helper that INSERTs a
  `coord.merge_proposals` row in `status='landing'`.

### Phase 2 — `on_land_push_failure` row-transition tests (`#[serial]`, DB)
Seed a `landing` row, call `on_land_push_failure(&state, pid, repos_pushed, &err)`
directly (no git needed — it only does DB writes), assert the row afterward:
1. **Transient** (`PushError::Transient` or an opaque error classifying transient),
   `repos_pushed=[]` → row `status='queued'`, `error` set.
2. **Deterministic** (`anyhow::Error::new(PushError::Rejected{deterministic:true,..})`,
   optionally `.context("fast-forward push to …")` to prove downcast-through-context),
   `repos_pushed=[]` → row `status='conflict'`, error names the deterministic reason.
3. **Partial multi-repo** (`repos_pushed=["repo-a"]`, any class) → row `status='conflict'`,
   `error` names the pushed repo, AND `MERGE_PARTIAL_MULTI_REPO_LAND` delta == 1.
   (`fanout_conflict` will no-op / best-effort without a real PR — assert it doesn't panic;
   the row write is the contract.)

### Phase 3 — lease-release gauge-balance test (`#[serial]`, DB)
The new wiring is "`land_or_shadow` releases the land-layer lease on a push failure."
Two ways to exercise it — vet/implementer picks by cost:
- **Preferred (true integration)** if the existing git-temp-repo helpers make a *failing*
  land push cheap: set up a scratch repo whose push to its "main" is rejected (non-FF or a
  bare remote that refuses), acquire the `MainMerge` claim for the repo (assert gauge +1),
  drive `land_or_shadow` (or the smallest seam that reaches `land_all_repos`' push), and
  assert: row left `landing`? NO — moved to `queued`/`conflict`; claim **released**; gauge
  delta == 0 (back to baseline). This is the only test that proves the end-to-end wiring.
- **Fallback (contract test)** if a deterministic failing push is too costly to stage:
  acquire the `MainMerge` claim (gauge +1), call the exact release `land_or_shadow` makes on
  failure — `release_main_merge_claims(&state, &cfg, &specs, &holder)` — assert claim released
  + gauge delta == 0. Plus a guard test: calling it for a **never-acquired** repo decrements
  the gauge (documents the underflow hazard ⇒ proves WHY release must stay path-specific and
  must never run for the lease-less batch path).
Prefer the integration form; fall back only with a code comment explaining the staging cost.

## Tests / verification
- `cargo test --bins -- on_land_push_failure` + the new gauge test names, with
  `COORD_TEST_PG_DSN` pointed at a local `coord_test` Postgres (the CI shape — ci.yml
  `cargo test --bins`, `--test-threads=2`).
- Without `COORD_TEST_PG_DSN` the new tests early-return (skip) — confirm `cargo test`
  still green on a no-PG box (parity with every other `test_state`-gated module).
- `cargo clippy --workspace --tests -- -D warnings` clean.

## Non-goals
- No production-code changes (tests only).
- No shared/global test-harness extraction (per-module `test_state` is the house style).
- Not asserting `fanout_conflict`'s PR side effects (needs a GitHub stub — separate concern).

## File inventory
| Piece | Location | Change |
|---|---|---|
| Test harness | `merge_scheduler.rs:12390` `mod tests` | add `test_state`/`ensure_schema`/`seed_landing_proposal` |
| Row-transition tests | same module | 3 `#[serial]` DB tests over `on_land_push_failure` |
| Gauge-balance test | same module | 1–2 `#[serial]` DB tests over the release path |
| Harness deps (already exist) | `crate::state::build_test_state`, `crate::test_fixtures::batch_execute_ddl_locked`, `crate::claims::{acquire,release}` | reuse, no change |
