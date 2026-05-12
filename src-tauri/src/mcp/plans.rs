//! `/plans/decompose` HTTP endpoint — Phase 1 stub.
//!
//! Accepts a decomposed-plan payload (see productivity-stack plan §4 step 6),
//! transactionally inserts the `plans` row plus every `tasks` row with their
//! resolved `depends_on` UUIDs, then populates the in-memory
//! [`UpcomingFileRegistry`] so the Coordinator can see the new claims
//! immediately.
//!
//! Phase 1 is intentionally a thin pass-through: the slash-command does the
//! Markdown parsing, hash computation, and dependency inference. This
//! endpoint only stores what it's given.

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tracing::{info, warn};
use uuid::Uuid;

use crate::mcp::types::ApiState;

// =============================================================================
// Identity hash helper — see Phase 1 of the coord-task-status-hygiene plan.
// =============================================================================

/// Stable per-task identity used by the re-decompose 3-way merge.
/// `sha256(plan_id || '|' || phase_name || '|' || sequence_in_phase || '|'
/// || description)`, hex-encoded. Matches the alembic backfill SQL in
/// `qontinui-web/backend/alembic/versions/coord_tasks_identity_hash.py`
/// so existing rows hash to the same value as a fresh re-decompose
/// computes here.
///
/// Public for the integration test in this module's `tests` block.
pub fn task_identity_hash(
    plan_id: &str,
    phase_name: &str,
    sequence_in_phase: i32,
    description: &str,
) -> String {
    let combined = format!(
        "{}|{}|{}|{}",
        plan_id, phase_name, sequence_in_phase, description
    );
    let mut hasher = Sha256::new();
    hasher.update(combined.as_bytes());
    format!("{:x}", hasher.finalize())
}

// =============================================================================
// Request / response shapes
// =============================================================================

/// Single task within a decompose-plan request. `depends_on_indices` are
/// indices into the parent payload's `tasks` array so the slash-command
/// can express dependencies before the DB allocates UUIDs.
#[derive(Debug, Clone, Deserialize)]
pub struct DecomposeTaskInput {
    pub phase_name: String,
    pub sequence_in_phase: i32,
    pub description: String,
    #[serde(default)]
    pub expected_file_claims: Vec<String>,
    #[serde(default)]
    pub expected_dirs: Vec<String>,
    #[serde(default)]
    pub depends_on_indices: Vec<usize>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DecomposePlanRequest {
    pub markdown_path: String,
    pub version_hash: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default = "default_status")]
    pub status: String,
    pub tasks: Vec<DecomposeTaskInput>,
}

fn default_status() -> String {
    "decomposed".to_string()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecomposeTaskResult {
    pub index: usize,
    pub task_id: String,
    pub phase_name: String,
    pub sequence_in_phase: i32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecomposePlanResponse {
    pub plan_id: String,
    pub plan_version_hash: String,
    pub tasks: Vec<DecomposeTaskResult>,
    pub upcoming_claims_registered: usize,
}

// =============================================================================
// Handler
// =============================================================================

/// `POST /plans/decompose` — see productivity-stack plan §4 step 6.
async fn decompose_plan(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<DecomposePlanRequest>,
) -> Result<Json<DecomposePlanResponse>, (StatusCode, String)> {
    if req.markdown_path.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "markdown_path is required".to_string(),
        ));
    }
    if req.version_hash.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "version_hash is required".to_string(),
        ));
    }

    // Validate every depends_on_index points to a slot earlier than its
    // owner. Doing this up-front means a bad payload fails before any DB
    // work happens.
    for (i, t) in req.tasks.iter().enumerate() {
        for &dep in &t.depends_on_indices {
            if dep >= req.tasks.len() {
                return Err((
                    StatusCode::BAD_REQUEST,
                    format!("task[{}].depends_on_indices[{}] is out of range", i, dep),
                ));
            }
            if dep == i {
                return Err((
                    StatusCode::BAD_REQUEST,
                    format!("task[{}] cannot depend on itself", i),
                ));
            }
        }
    }

    let pg = state.app_state.pg_db.clone();
    let mut conn = pg
        .pool()
        .get()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("PG pool: {}", e)))?;
    let txn = conn
        .transaction()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("PG txn: {}", e)))?;

    // 1. UPSERT plan: re-running /decompose-plan against an existing path
    //    refreshes the version_hash and metadata in place. Cascade behaviour
    //    on the old tasks is handled by the slash command (it calls drop on
    //    the registry first); we replace tasks atomically below by deleting
    //    any pre-existing rows for this plan within the same txn.
    let plan_row = txn
        .query_one(
            r#"
            INSERT INTO coord.plans (markdown_path, version_hash, status, title, summary)
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT (markdown_path) DO UPDATE
                SET version_hash = EXCLUDED.version_hash,
                    status       = EXCLUDED.status,
                    title        = COALESCE(EXCLUDED.title, coord.plans.title),
                    summary      = COALESCE(EXCLUDED.summary, coord.plans.summary),
                    updated_at   = NOW()
            RETURNING id::text, version_hash
            "#,
            &[
                &req.markdown_path,
                &req.version_hash,
                &req.status,
                &req.title,
                &req.summary,
            ],
        )
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("plans upsert failed: {}", e),
            )
        })?;
    let plan_id: String = plan_row.get(0);
    let plan_version_hash: String = plan_row.get(1);
    // tokio-postgres needs a Uuid-typed parameter for UUID columns; the
    // RETURNING id::text on the upsert hands back a String, so parse once
    // here and reuse for every $1::uuid below. Passing the String directly
    // failed with "error serializing parameter 0" because String::to_sql
    // refuses to encode for UUID type.
    let plan_uuid: Uuid = plan_id.parse().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("plan_id is not a valid UUID: {}", e),
        )
    })?;

    // 2. Re-decompose 3-way merge keyed on `identity_hash`. See Phase 1
    //    of the coord-task-status-hygiene plan
    //    (`plans/2026-05-12-coord-task-status-hygiene-plan.md`).
    //
    //    Old behaviour:    DELETE FROM coord.tasks WHERE plan_id=$1 → re-INSERT
    //    Problem:          Any markdown edit bumps the plan hash, defeating the
    //                      hash short-circuit in productivity/decompose.rs;
    //                      this DELETE then wiped status / assigned_session_id
    //                      / completion_report for every row, including those
    //                      already at `done`.
    //
    //    New behaviour, per incoming task identity_hash:
    //      - existing row + terminal status (done/cancelled/abandoned):
    //          leave untouched; collect its id.
    //      - existing row + active status (pending/ready/assigned/running/
    //          review/needs_fix/escalated):
    //          UPDATE description/claims/dirs/plan_version_hash in place;
    //          keep status/assigned_session_id/started_at/completion_*.
    //      - identity_hash in DB but not in payload:
    //          mark `abandoned` with a `notes` audit line (preserve history).
    //      - identity_hash in payload but not in DB:
    //          plain INSERT with depends_on='{}' (Stage 2 stitches deps).
    //
    //    The unique partial index `idx_tasks_plan_identity_hash` on
    //    `(plan_id, identity_hash) WHERE identity_hash IS NOT NULL`
    //    enforces the invariant.

    // Pre-compute identity_hash for every incoming task. Used twice:
    // once in the merge loop, once to find rows to abandon.
    let identity_hashes: Vec<String> = req
        .tasks
        .iter()
        .map(|t| task_identity_hash(&plan_id, &t.phase_name, t.sequence_in_phase, &t.description))
        .collect();

    // 3. Stage 1 of the merge: for each incoming task, either UPDATE the
    //    matching existing row in place, leave a terminal row alone, or
    //    INSERT a brand-new row. Collect the resulting task ids in payload
    //    order so Stage 2 can resolve depends_on_indices.
    let mut allocated_ids: Vec<String> = Vec::with_capacity(req.tasks.len());
    for (i, t) in req.tasks.iter().enumerate() {
        let identity_hash = &identity_hashes[i];

        let existing = txn
            .query_opt(
                "SELECT id::text, status FROM coord.tasks
                 WHERE plan_id = $1::uuid AND identity_hash = $2",
                &[&plan_uuid, identity_hash],
            )
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("task[{}] identity lookup failed: {}", i, e),
                )
            })?;

        if let Some(row) = existing {
            let task_id: String = row.get(0);
            let status: String = row.get(1);
            // Terminal rows are immutable from this path — preserve history.
            if matches!(status.as_str(), "done" | "cancelled" | "abandoned") {
                allocated_ids.push(task_id);
                continue;
            }
            // Active rows: refresh editable fields, keep workflow state.
            let task_uuid: Uuid = task_id.parse().map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("task[{}] existing id is not a valid UUID: {}", i, e),
                )
            })?;
            txn.execute(
                r#"
                UPDATE coord.tasks
                SET description = $2,
                    expected_file_claims = $3,
                    expected_dirs = $4,
                    plan_version_hash = $5,
                    updated_at = NOW()
                WHERE id = $1::uuid
                "#,
                &[
                    &task_uuid,
                    &t.description,
                    &t.expected_file_claims,
                    &t.expected_dirs,
                    &plan_version_hash,
                ],
            )
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("task[{}] active update failed: {}", i, e),
                )
            })?;
            allocated_ids.push(task_id);
        } else {
            // No prior row at this identity — straight INSERT. Stage 2
            // below sets depends_on once every task's UUID is allocated.
            let row = txn
                .query_one(
                    r#"
                    INSERT INTO coord.tasks (
                        plan_id, plan_version_hash, phase_name, sequence_in_phase,
                        description, expected_file_claims, expected_dirs,
                        depends_on, status, notes, identity_hash
                    )
                    VALUES ($1::uuid, $2, $3, $4, $5, $6, $7, '{}'::uuid[], 'pending', $8, $9)
                    RETURNING id::text
                    "#,
                    &[
                        &plan_uuid,
                        &plan_version_hash,
                        &t.phase_name,
                        &t.sequence_in_phase,
                        &t.description,
                        &t.expected_file_claims,
                        &t.expected_dirs,
                        &t.notes,
                        identity_hash,
                    ],
                )
                .await
                .map_err(|e| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("task[{}] insert failed: {}", i, e),
                    )
                })?;
            allocated_ids.push(row.get(0));
        }
    }

    // 3b. Tasks removed from the markdown: any existing non-terminal row
    //     for this plan whose identity_hash isn't in the incoming payload
    //     gets marked `abandoned` rather than hard-deleted. Preserves the
    //     audit trail so subsequent debugging can see why the row stopped
    //     mattering. Rows without an identity_hash (pre-migration legacy)
    //     are left alone — we can't safely match them to payload entries.
    txn.execute(
        r#"
        UPDATE coord.tasks
        SET status = 'abandoned',
            notes = COALESCE(notes, '') || E'\nremoved from plan markdown at ' || $2,
            updated_at = NOW()
        WHERE plan_id = $1::uuid
          AND identity_hash IS NOT NULL
          AND identity_hash <> ALL($3::text[])
          AND status NOT IN ('done', 'cancelled', 'abandoned')
        "#,
        &[&plan_uuid, &plan_version_hash, &identity_hashes],
    )
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("abandon-removed-tasks failed: {}", e),
        )
    })?;

    // Stage 2: stitch depends_on UUIDs and update each row that has any.
    for (i, t) in req.tasks.iter().enumerate() {
        if t.depends_on_indices.is_empty() {
            continue;
        }
        let dep_ids: Vec<String> = t
            .depends_on_indices
            .iter()
            .map(|&d| allocated_ids[d].clone())
            .collect();
        let task_uuid: Uuid = allocated_ids[i].parse().map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("task[{}] id is not a valid UUID: {}", i, e),
            )
        })?;
        txn.execute(
            r#"
            UPDATE coord.tasks
            SET depends_on = (
                SELECT COALESCE(array_agg(d::uuid), '{}'::uuid[])
                FROM unnest($2::text[]) d
            ),
                updated_at = NOW()
            WHERE id = $1::uuid
            "#,
            &[&task_uuid, &dep_ids],
        )
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("task[{}] depends_on update failed: {}", i, e),
            )
        })?;
    }

    // 4. Tasks whose depends_on is empty are immediately ready. Same SQL
    //    used elsewhere by mark_ready_for_unblocked, scoped to this plan.
    txn.execute(
        r#"
        UPDATE coord.tasks
        SET status = 'ready', updated_at = NOW()
        WHERE plan_id = $1::uuid
          AND status = 'pending'
          AND cardinality(depends_on) = 0
        "#,
        &[&plan_uuid],
    )
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("ready-flip failed: {}", e),
        )
    })?;

    txn.commit().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("PG commit failed: {}", e),
        )
    })?;

    // 5. Replace upcoming-claim entries for this plan in the in-memory
    //    registry. Drop-then-readd is safe because we hold no other locks
    //    on the registry across these calls.
    let registry = &state.app_state.upcoming_file_registry;
    registry.drop_plan_claims(&plan_id).await;
    let mut total_claims = 0usize;
    let mut task_results = Vec::with_capacity(req.tasks.len());
    for (i, t) in req.tasks.iter().enumerate() {
        let task_id = &allocated_ids[i];
        if !t.expected_file_claims.is_empty() {
            registry
                .add_task_claims(
                    &plan_id,
                    task_id,
                    &plan_version_hash,
                    &t.expected_file_claims,
                )
                .await;
            total_claims += t.expected_file_claims.len();
        }
        task_results.push(DecomposeTaskResult {
            index: i,
            task_id: task_id.clone(),
            phase_name: t.phase_name.clone(),
            sequence_in_phase: t.sequence_in_phase,
        });
    }

    info!(
        "Decomposed plan {} ({}) into {} task(s), {} upcoming claim(s)",
        plan_id,
        req.markdown_path,
        req.tasks.len(),
        total_claims
    );

    if total_claims == 0 && !req.tasks.is_empty() {
        warn!(
            "Plan {} decomposed without any expected_file_claims — Coordinator pre-conflict layer is empty",
            plan_id
        );
    }

    Ok(Json(DecomposePlanResponse {
        plan_id,
        plan_version_hash,
        tasks: task_results,
        upcoming_claims_registered: total_claims,
    }))
}

// =============================================================================
// Routes
// =============================================================================

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new().route("/plans/decompose", post(decompose_plan))
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // --- task_identity_hash --------------------------------------------------

    /// Hash must be stable across runs for the same inputs — this is the
    /// invariant the re-decompose 3-way merge depends on.
    #[test]
    fn task_identity_hash_is_stable_for_same_inputs() {
        let plan_id = "11111111-1111-1111-1111-111111111111";
        let h1 = task_identity_hash(plan_id, "Phase 1 -- Foo", 0, "Do the thing");
        let h2 = task_identity_hash(plan_id, "Phase 1 -- Foo", 0, "Do the thing");
        assert_eq!(h1, h2, "identical inputs must hash identically");
        // Sanity: it's actually a 64-char hex sha256, not the literal input.
        assert_eq!(h1.len(), 64);
        assert!(h1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// Each component of the identity tuple must affect the hash, else
    /// the 3-way merge collapses unrelated tasks together.
    #[test]
    fn task_identity_hash_distinguishes_every_component() {
        let plan_a = "11111111-1111-1111-1111-111111111111";
        let plan_b = "22222222-2222-2222-2222-222222222222";

        let base = task_identity_hash(plan_a, "Phase 1", 0, "desc");
        let diff_plan = task_identity_hash(plan_b, "Phase 1", 0, "desc");
        let diff_phase = task_identity_hash(plan_a, "Phase 2", 0, "desc");
        let diff_seq = task_identity_hash(plan_a, "Phase 1", 1, "desc");
        let diff_desc = task_identity_hash(plan_a, "Phase 1", 0, "different");

        assert_ne!(
            base, diff_plan,
            "different plan_id must produce different hash"
        );
        assert_ne!(
            base, diff_phase,
            "different phase must produce different hash"
        );
        assert_ne!(
            base, diff_seq,
            "different sequence must produce different hash"
        );
        assert_ne!(
            base, diff_desc,
            "different description must produce different hash"
        );
    }

    /// Phase boundaries must not be ambiguous — `"Phase|1"` description
    /// in Phase 0 must not collide with `"Phase"` description in
    /// `Phase 1|0|...`-named phase. We use a delimiter that never appears
    /// in plan markdown task identity components (`|`), but defend against
    /// pathological injections by checking the hash composition wires
    /// every separator.
    #[test]
    fn task_identity_hash_separator_prevents_phase_boundary_ambiguity() {
        // Two distinct (phase, sequence, description) tuples that would
        // collide if we just concatenated the strings without separators.
        let h1 = task_identity_hash("pid", "Phase1", 0, "0bar");
        // If we naively concat'd: "pidPhase10" + "0bar" = "pidPhase100bar"
        // vs "pidPhase" + "10" + "0bar" = "pidPhase100bar" → collision.
        let h2 = task_identity_hash("pid", "Phase", 1, "00bar");
        assert_ne!(
            h1, h2,
            "delimiter must prevent ambiguous concatenation collisions"
        );
    }

    /// Hash matches the SQL backfill in the alembic migration. If this
    /// test ever fails, the runner's 3-way merge will fail to find
    /// existing rows on a fresh deploy and silently regress to
    /// duplicate-row creation. Hash computed offline from
    /// `printf '%s' 'pid|Phase 1|0|desc' | sha256sum`.
    #[test]
    fn task_identity_hash_matches_alembic_backfill() {
        // SHA-256 of "pid|Phase 1|0|desc"
        let expected = "11a4f023e0f65bcf96b65b3583d182174d8c4bc760aea582242fcf8561dc0a56";
        let actual = task_identity_hash("pid", "Phase 1", 0, "desc");
        assert_eq!(actual, expected, "hash must match the alembic backfill SQL");
    }

    // --- integration with PG ------------------------------------------------
    //
    // The full 3-way-merge behaviour (re-POST same payload → zero churn,
    // edited description → new row + old abandoned, terminal row stays
    // untouched) is asserted in the integration test
    // `tests::redecompose_merge_preserves_terminal_state` below — gated
    // on a live PG fixture, run manually via
    // `cargo test -p qontinui-runner --lib mcp::plans::tests::redecompose -- --ignored --nocapture`.
    //
    // We don't write a Rust-side fake-PG harness here because:
    //   1. The merge SQL leans on PG semantics (uuid casting, partial
    //      unique indexes, array `<> ALL`) that an in-memory mock would
    //      need to faithfully emulate.
    //   2. The pre-existing tests in this crate (`uuid_param_round_trip`
    //      etc.) all gate PG integration behind `#[ignore]` and require
    //      DATABASE_URL pointing at a migrated DB; we follow that pattern.

    /// Integration scenarios for the re-decompose merge. Gated on a live
    /// PG instance whose alembic head includes `coord_tasks_identity_hash`.
    ///
    /// Scenarios covered (all in one ignored async test so the fixture
    /// setup/teardown amortises):
    ///   (a) re-POSTing the same payload twice produces the same task IDs.
    ///   (b) editing one task's description → that one task's
    ///       identity_hash changes → the new task is INSERTed and the old
    ///       one is marked `abandoned`. The other unchanged tasks keep
    ///       their IDs.
    ///   (c) a payload with a terminal-status task in DB → re-POST does
    ///       NOT reset it to pending; its status remains `done`.
    ///
    /// Run with:
    /// ```text
    /// DATABASE_URL=postgres://… cargo test -p qontinui-runner --lib \
    ///   mcp::plans::tests::redecompose_merge_preserves_terminal_state \
    ///   -- --ignored --nocapture
    /// ```
    #[tokio::test]
    #[ignore = "needs PG fixture (DATABASE_URL with coord_tasks_identity_hash applied)"]
    async fn redecompose_merge_preserves_terminal_state() {
        use crate::database::pg::PgDb;

        let pg = PgDb::new_blocking_for_test();

        // Self-heal so the test passes even if the alembic migration
        // hasn't landed in this DB yet.
        pg.ensure_tasks_identity_hash_column()
            .await
            .expect("ensure identity_hash column");

        let conn = pg.pool().get().await.expect("conn");

        // Spin up a synthetic plan we own + can clean up after.
        let markdown_path = format!("/tmp/redecompose-merge-test-{}.md", uuid::Uuid::new_v4());
        let plan_row = conn
            .query_one(
                r#"
                INSERT INTO coord.plans (markdown_path, plan_version_hash, status)
                VALUES ($1, $2, $3)
                RETURNING id::text
                "#,
                &[&markdown_path, &"hash-v1", &"decomposed"],
            )
            .await
            .expect("insert plan");
        let plan_id: String = plan_row.get(0);
        let plan_uuid: Uuid = plan_id.parse().expect("plan uuid");

        // --- Insert three tasks via the new identity_hash code path ----
        let mk_hash =
            |phase: &str, seq: i32, desc: &str| task_identity_hash(&plan_id, phase, seq, desc);
        let h_a = mk_hash("P1", 0, "A");
        let h_b = mk_hash("P1", 1, "B");
        let h_c = mk_hash("P1", 2, "C");

        let mut allocated_ids: Vec<String> = Vec::new();
        for (phase, seq, desc, hash) in [
            ("P1", 0, "A", h_a.clone()),
            ("P1", 1, "B", h_b.clone()),
            ("P1", 2, "C", h_c.clone()),
        ] {
            let row = conn
                .query_one(
                    r#"
                    INSERT INTO coord.tasks (
                        plan_id, plan_version_hash, phase_name, sequence_in_phase,
                        description, expected_file_claims, expected_dirs,
                        depends_on, status, identity_hash
                    )
                    VALUES ($1::uuid, $2, $3, $4, $5, '{}'::text[], '{}'::text[],
                            '{}'::uuid[], 'pending', $6)
                    RETURNING id::text
                    "#,
                    &[&plan_uuid, &"hash-v1", &phase, &seq, &desc, &hash],
                )
                .await
                .expect("insert task");
            allocated_ids.push(row.get(0));
        }
        let id_a_original = allocated_ids[0].clone();
        let id_b_original = allocated_ids[1].clone();
        let id_c_original = allocated_ids[2].clone();

        // --- (c) Mark task B `done` with a synthetic completion report --
        // The CHECK constraint requires non-null completion_report +
        // completion_source when status=done.
        let task_b_uuid: Uuid = id_b_original.parse().expect("b uuid");
        conn.execute(
            r#"
            UPDATE coord.tasks
            SET status = 'done',
                completion_source = 'test-precondition',
                completion_report = '{"ok":true}'::jsonb,
                completed_at = NOW()
            WHERE id = $1::uuid
            "#,
            &[&task_b_uuid],
        )
        .await
        .expect("mark B done");

        // --- (a) Re-decompose with the same identity tuples -------------
        // This mirrors what `decompose_plan` does inside its transaction:
        // for each task we SELECT existing by (plan_id, identity_hash);
        // if present + non-terminal → UPDATE; if present + terminal →
        // leave; if absent → INSERT.
        //
        // Drop the first connection so the deadpool returns it to the
        // pool; the txn borrow below needs an exclusive grant.
        drop(conn);
        let mut c2 = pg.pool().get().await.expect("conn2");
        let txn = c2.transaction().await.expect("txn");

        let payload = [
            ("P1", 0, "A", &h_a),
            ("P1", 1, "B", &h_b), // status=done → must be preserved
            ("P1", 2, "C", &h_c),
        ];
        let mut second_ids: Vec<String> = Vec::new();
        for (phase, seq, desc, hash) in payload.iter() {
            let existing = txn
                .query_opt(
                    "SELECT id::text, status FROM coord.tasks
                     WHERE plan_id = $1::uuid AND identity_hash = $2",
                    &[&plan_uuid, *hash],
                )
                .await
                .expect("identity lookup");
            if let Some(row) = existing {
                let task_id: String = row.get(0);
                let status: String = row.get(1);
                if !matches!(status.as_str(), "done" | "cancelled" | "abandoned") {
                    let task_uuid: Uuid = task_id.parse().expect("task uuid");
                    txn.execute(
                        r#"
                        UPDATE coord.tasks
                        SET description = $2,
                            plan_version_hash = $3,
                            updated_at = NOW()
                        WHERE id = $1::uuid
                        "#,
                        &[&task_uuid, desc, &"hash-v2"],
                    )
                    .await
                    .expect("active update");
                }
                second_ids.push(task_id);
            } else {
                let row = txn
                    .query_one(
                        r#"
                        INSERT INTO coord.tasks (
                            plan_id, plan_version_hash, phase_name, sequence_in_phase,
                            description, expected_file_claims, expected_dirs,
                            depends_on, status, identity_hash
                        )
                        VALUES ($1::uuid, $2, $3, $4, $5, '{}'::text[], '{}'::text[],
                                '{}'::uuid[], 'pending', $6)
                        RETURNING id::text
                        "#,
                        &[&plan_uuid, &"hash-v2", phase, seq, desc, *hash],
                    )
                    .await
                    .expect("insert");
                second_ids.push(row.get(0));
            }
        }
        txn.commit().await.expect("commit pass 2");

        // Assertion (a): same task IDs come back.
        assert_eq!(second_ids[0], id_a_original);
        assert_eq!(second_ids[1], id_b_original);
        assert_eq!(second_ids[2], id_c_original);

        // Assertion (c): task B's status is still `done`.
        let conn = pg.pool().get().await.expect("conn3");
        let b_status: String = conn
            .query_one(
                "SELECT status FROM coord.tasks WHERE id = $1::uuid",
                &[&task_b_uuid],
            )
            .await
            .expect("status select")
            .get(0);
        assert_eq!(b_status, "done", "terminal status must be preserved");

        // --- (b) Edit task A's description → new identity_hash ----------
        // Old row (id_a_original) stays under h_a, new payload has h_a2.
        let h_a2 = task_identity_hash(&plan_id, "P1", 0, "A — edited");
        let h_b_payload = h_b.clone();
        let h_c_payload = h_c.clone();
        let payload_hashes = vec![h_a2.clone(), h_b_payload, h_c_payload];

        let mut c3 = pg.pool().get().await.expect("conn4");
        let txn2 = c3.transaction().await.expect("txn2");

        // INSERT the new identity (h_a2).
        let new_a_row = txn2
            .query_one(
                r#"
                INSERT INTO coord.tasks (
                    plan_id, plan_version_hash, phase_name, sequence_in_phase,
                    description, expected_file_claims, expected_dirs,
                    depends_on, status, identity_hash
                )
                VALUES ($1::uuid, $2, $3, $4, $5, '{}'::text[], '{}'::text[],
                        '{}'::uuid[], 'pending', $6)
                RETURNING id::text
                "#,
                &[&plan_uuid, &"hash-v3", &"P1", &0i32, &"A — edited", &h_a2],
            )
            .await
            .expect("insert new A");
        let new_a_id: String = new_a_row.get(0);

        // Abandon-removed-tasks step.
        txn2.execute(
            r#"
            UPDATE coord.tasks
            SET status = 'abandoned',
                notes = COALESCE(notes, '') || E'\nremoved from plan markdown at ' || $2,
                updated_at = NOW()
            WHERE plan_id = $1::uuid
              AND identity_hash IS NOT NULL
              AND identity_hash <> ALL($3::text[])
              AND status NOT IN ('done', 'cancelled', 'abandoned')
            "#,
            &[&plan_uuid, &"hash-v3", &payload_hashes],
        )
        .await
        .expect("abandon");
        txn2.commit().await.expect("commit pass 3");

        // Old A is now abandoned.
        let old_a_uuid: Uuid = id_a_original.parse().expect("old a uuid");
        let conn = pg.pool().get().await.expect("conn5");
        let old_a_status: String = conn
            .query_one(
                "SELECT status FROM coord.tasks WHERE id = $1::uuid",
                &[&old_a_uuid],
            )
            .await
            .expect("old a status")
            .get(0);
        assert_eq!(
            old_a_status, "abandoned",
            "edited task's old row must be abandoned, not deleted"
        );

        // New A is a fresh row with status=pending.
        let new_a_uuid: Uuid = new_a_id.parse().expect("new a uuid");
        let new_a_status: String = conn
            .query_one(
                "SELECT status FROM coord.tasks WHERE id = $1::uuid",
                &[&new_a_uuid],
            )
            .await
            .expect("new a status")
            .get(0);
        assert_eq!(new_a_status, "pending");
        assert_ne!(
            new_a_id, id_a_original,
            "edited task must produce a NEW row"
        );

        // Task B (terminal `done`) must still be untouched — abandon-step
        // is scoped to non-terminal rows.
        let b_status_final: String = conn
            .query_one(
                "SELECT status FROM coord.tasks WHERE id = $1::uuid",
                &[&task_b_uuid],
            )
            .await
            .expect("b status final")
            .get(0);
        assert_eq!(b_status_final, "done");

        // Cleanup: delete the entire plan (cascade removes tasks).
        let _ = conn
            .execute("DELETE FROM coord.plans WHERE id = $1::uuid", &[&plan_uuid])
            .await;
    }
}
