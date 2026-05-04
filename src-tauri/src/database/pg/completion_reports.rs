//! PostgreSQL CRUD for `coord.tasks.completion_report` and the worker-declared
//! emergent-dependency edge.
//!
//! Phase 1 of the productivity-coordinator-completion-reports plan
//! (D:/qontinui-root/plans/productivity-coordinator-completion-reports.md §2,
//! §3, §4). Defines the typed `CompletionReport` struct that flows along
//! resolved dependency edges, plus DB helpers for:
//!
//! - Writing / reading the structured report (`write_completion_report`,
//!   `get_completion_report`).
//! - Walking the upstream graph to detect cycles before adding a new edge
//!   (`detect_dependency_cycle`, implementing the §4 recursive CTE with
//!   PG14's `CYCLE … SET … USING …` clause and a `depth <= 64` backstop).
//! - Appending an edge to `coord.tasks.depends_on` (`add_task_dependency`).
//! - Phase 2 hooks for `assignment_brief_extras` (`write_assignment_brief_extras`,
//!   `clear_assignment_brief_extras`).
//!
//! Search-path note: the runner's PG pool sets `search_path TO project, public`
//! (`database/pg/mod.rs:165`). `coord` is NOT in search_path, so every SQL
//! statement here MUST schema-qualify `coord.tasks` and friends.
//!
//! UUID round-trip convention matches the rest of `database/pg/`: text in,
//! text out, `$x::uuid` casts inside the SQL.

use super::PgDb;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

// ============================================================================
// Wire types — match the §2 "Rust schema" exactly.
// ============================================================================

/// Typed handoff payload that flows along a resolved dependency edge.
///
/// Workers write one of these via `POST /tasks/{id}/report`; the Coordinator
/// reads them via Rule B's brief composer (Phase 2). Schema validation
/// enforced server-side at write time (see `write_completion_report`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CompletionReport {
    /// Human-readable narrative. Markdown, ≤4000 chars (enforced server-side
    /// at insert time). Acts as the "executive summary" the Coordinator
    /// prepends to downstream briefs verbatim.
    pub summary_md: String,

    /// What the upstream produced. Each entry is a typed pointer.
    pub deliverables: Vec<Deliverable>,

    /// What the upstream broke or changed in a way that affects downstream
    /// work. Empty vec is fine; absence implies "no breaking changes claimed"
    /// but does NOT prove safety.
    pub breaking_changes: Vec<BreakingChange>,

    /// Loose ends. Each carries a `blocking_for_dependents` boolean — when
    /// true, Rule E refuses to mark dependents `ready` and instead emits an
    /// escalation card.
    pub follow_ups: Vec<FollowUp>,

    /// Open extension point. Source-specific fields land here. Coordinator
    /// only reads typed fields above; consumers that want richer payloads
    /// (e.g. screencast links, future Restate workflow output) read
    /// `artifacts.<source>`.
    #[serde(default)]
    pub artifacts: HashMap<String, Value>,
}

/// Typed pointer to a thing the upstream produced.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Deliverable {
    /// What KIND of artifact. Open enum — start with the obvious set, add as
    /// needed. Don't gate Rust on this field; it's for display and
    /// Coordinator-side dispatching.
    /// Initial set: 'commit' | 'pr' | 'file' | 'endpoint' | 'schema-change'
    /// | 'spec' | 'plan-stamp' | 'fixture' | 'doc-update' | 'other'.
    pub kind: String,

    /// Stable reference for this kind. SHA for commits, URL for PRs, path
    /// for files, route string for endpoints, table name for schema changes.
    pub reference: String,

    /// One-line human description for the dashboard / brief.
    pub description: String,
}

/// A breaking change the upstream introduced.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BreakingChange {
    /// Subsystem affected. Free-form area tag — same convention
    /// `productivity_knowledge.area` uses.
    pub area: String,

    /// What broke + what users / dependent code now have to do.
    pub description: String,

    /// Markdown-formatted migration steps. Empty if "no migration required,
    /// just be aware."
    pub migration_steps_md: String,
}

/// A loose end that affects downstream work.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FollowUp {
    pub description: String,

    /// 'critical' | 'important' | 'nice-to-have'. Coordinator filters on
    /// this for escalation thresholds.
    pub priority: String,

    /// When true, Rule E refuses to flip dependents to `ready` and emits a
    /// coordinator-escalation event so the user can decide whether to
    /// override. Default false.
    #[serde(default)]
    pub blocking_for_dependents: bool,
}

/// Enum tag identifying who wrote the report. Stored as TEXT in
/// `coord.tasks.completion_source`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CompletionSource {
    SessionSelfReport,
    ReviewerAttestation,
    GithubMerge,
    CiPipeline,
    ManualUserFire,
    /// Backfill marker for tasks that completed before the
    /// `cr01a2b3c4d5_completion_reports` migration. Should never appear on
    /// rows authored after the migration.
    LegacyPreCompletionReports,
}

impl CompletionSource {
    /// Round-trip helper for storing as TEXT.
    pub fn as_str(self) -> &'static str {
        match self {
            CompletionSource::SessionSelfReport => "session-self-report",
            CompletionSource::ReviewerAttestation => "reviewer-attestation",
            CompletionSource::GithubMerge => "github-merge",
            CompletionSource::CiPipeline => "ci-pipeline",
            CompletionSource::ManualUserFire => "manual-user-fire",
            CompletionSource::LegacyPreCompletionReports => "legacy-pre-completion-reports",
        }
    }

    /// Parse the TEXT representation back into the enum. Returns `None` on
    /// unknown values (forward-compat — a future source value loaded by an
    /// older runner shouldn't crash the read path).
    pub fn from_str_value(s: &str) -> Option<Self> {
        Some(match s {
            "session-self-report" => CompletionSource::SessionSelfReport,
            "reviewer-attestation" => CompletionSource::ReviewerAttestation,
            "github-merge" => CompletionSource::GithubMerge,
            "ci-pipeline" => CompletionSource::CiPipeline,
            "manual-user-fire" => CompletionSource::ManualUserFire,
            "legacy-pre-completion-reports" => CompletionSource::LegacyPreCompletionReports,
            _ => return None,
        })
    }
}

// ============================================================================
// Validation — enforced before issuing the SQL.
// ============================================================================

/// Hard caps to prevent runaway-size payloads from landing on disk. Matches
/// the §2 "Rust schema" sizes and the §7 Phase 1 P2 hygiene note.
const MAX_SUMMARY_MD_LEN: usize = 4000;
const MAX_DELIVERABLES: usize = 50;
const MAX_BREAKING_CHANGES: usize = 50;
const MAX_FOLLOW_UPS: usize = 50;

/// Per-element bounds — Phase 5 hardening (plan §7 Phase 5 / §9 "Schema
/// drift"). Outer caller (HTTP body, Tauri command, github-merge synthesizer)
/// might submit malformed-but-deserializable shapes; these caps reject them
/// with a `"validation: …"` error so the HTTP layer maps to 400.
const MAX_DELIVERABLE_DESCRIPTION: usize = 500;
const MAX_BREAKING_CHANGE_AREA: usize = 100;
const MAX_BREAKING_CHANGE_DESCRIPTION: usize = 2000;
const MAX_BREAKING_CHANGE_MIGRATION_STEPS_MD: usize = 4000;
const MAX_FOLLOW_UP_DESCRIPTION: usize = 2000;

/// Canonical set of allowed `Deliverable.kind` values per plan §2 "Rust
/// schema":
/// `'commit' | 'pr' | 'file' | 'endpoint' | 'schema-change' | 'spec' |
/// 'plan-stamp' | 'fixture' | 'doc-update' | 'other'`.
const ALLOWED_DELIVERABLE_KINDS: &[&str] = &[
    "commit",
    "pr",
    "file",
    "endpoint",
    "schema-change",
    "spec",
    "plan-stamp",
    "fixture",
    "doc-update",
    "other",
];

/// Canonical set of allowed `FollowUp.priority` values per plan §2:
/// `'critical' | 'important' | 'nice-to-have'`.
const ALLOWED_FOLLOW_UP_PRIORITIES: &[&str] = &["critical", "important", "nice-to-have"];

/// Validate the report shape before issuing the UPDATE. Returns
/// `Err("validation: …")` on any violation; the HTTP handler maps that prefix
/// to 400. Bundles every Phase 5 rule (kind / priority enums, per-element
/// length caps, non-empty fields) in addition to the Phase 1 outer caps on
/// `summary_md.len()` / array lengths.
fn validate_report(report: &CompletionReport) -> Result<(), String> {
    if report.summary_md.is_empty() {
        return Err("validation: summary_md must not be empty".to_string());
    }
    if report.summary_md.len() > MAX_SUMMARY_MD_LEN {
        return Err(format!(
            "validation: summary_md too long ({} > {})",
            report.summary_md.len(),
            MAX_SUMMARY_MD_LEN
        ));
    }
    if report.deliverables.len() > MAX_DELIVERABLES {
        return Err(format!(
            "validation: too many deliverables ({} > {})",
            report.deliverables.len(),
            MAX_DELIVERABLES
        ));
    }
    if report.breaking_changes.len() > MAX_BREAKING_CHANGES {
        return Err(format!(
            "validation: too many breaking_changes ({} > {})",
            report.breaking_changes.len(),
            MAX_BREAKING_CHANGES
        ));
    }
    if report.follow_ups.len() > MAX_FOLLOW_UPS {
        return Err(format!(
            "validation: too many follow_ups ({} > {})",
            report.follow_ups.len(),
            MAX_FOLLOW_UPS
        ));
    }

    // Per-deliverable rules.
    for (i, d) in report.deliverables.iter().enumerate() {
        if !ALLOWED_DELIVERABLE_KINDS.contains(&d.kind.as_str()) {
            return Err(format!(
                "validation: deliverable[{}].kind must be one of {:?}, got {:?}",
                i, ALLOWED_DELIVERABLE_KINDS, d.kind
            ));
        }
        if d.reference.is_empty() {
            return Err(format!(
                "validation: deliverable[{}].reference must not be empty",
                i
            ));
        }
        if d.description.is_empty() {
            return Err(format!(
                "validation: deliverable[{}].description must not be empty",
                i
            ));
        }
        if d.description.len() > MAX_DELIVERABLE_DESCRIPTION {
            return Err(format!(
                "validation: deliverable[{}].description too long ({} > {})",
                i,
                d.description.len(),
                MAX_DELIVERABLE_DESCRIPTION
            ));
        }
    }

    // Per-breaking-change rules.
    for (i, bc) in report.breaking_changes.iter().enumerate() {
        if bc.area.is_empty() {
            return Err(format!(
                "validation: breakingChanges[{}].area must not be empty",
                i
            ));
        }
        if bc.area.len() > MAX_BREAKING_CHANGE_AREA {
            return Err(format!(
                "validation: breakingChanges[{}].area too long ({} > {})",
                i,
                bc.area.len(),
                MAX_BREAKING_CHANGE_AREA
            ));
        }
        if bc.description.is_empty() {
            return Err(format!(
                "validation: breakingChanges[{}].description must not be empty",
                i
            ));
        }
        if bc.description.len() > MAX_BREAKING_CHANGE_DESCRIPTION {
            return Err(format!(
                "validation: breakingChanges[{}].description too long ({} > {})",
                i,
                bc.description.len(),
                MAX_BREAKING_CHANGE_DESCRIPTION
            ));
        }
        if bc.migration_steps_md.len() > MAX_BREAKING_CHANGE_MIGRATION_STEPS_MD {
            return Err(format!(
                "validation: breakingChanges[{}].migrationStepsMd too long ({} > {})",
                i,
                bc.migration_steps_md.len(),
                MAX_BREAKING_CHANGE_MIGRATION_STEPS_MD
            ));
        }
    }

    // Per-follow-up rules.
    for (i, fu) in report.follow_ups.iter().enumerate() {
        if fu.description.is_empty() {
            return Err(format!(
                "validation: followUps[{}].description must not be empty",
                i
            ));
        }
        if fu.description.len() > MAX_FOLLOW_UP_DESCRIPTION {
            return Err(format!(
                "validation: followUps[{}].description too long ({} > {})",
                i,
                fu.description.len(),
                MAX_FOLLOW_UP_DESCRIPTION
            ));
        }
        if !ALLOWED_FOLLOW_UP_PRIORITIES.contains(&fu.priority.as_str()) {
            return Err(format!(
                "validation: followUps[{}].priority must be one of {:?}, got {:?}",
                i, ALLOWED_FOLLOW_UP_PRIORITIES, fu.priority
            ));
        }
    }

    Ok(())
}

// ============================================================================
// PgDb helpers.
// ============================================================================

impl PgDb {
    /// Persist a structured completion report to `coord.tasks` and tag the
    /// `completion_source`. Validates the report shape (see `validate_report`)
    /// before issuing the UPDATE; returns `Err("validation: …")` on cap
    /// violation so HTTP handlers can map cleanly to 400.
    ///
    /// Returns `Err` if no row was updated (caller passed an unknown task_id).
    pub async fn write_completion_report(
        &self,
        task_id: &str,
        report: &CompletionReport,
        source: CompletionSource,
    ) -> Result<(), String> {
        validate_report(report)?;

        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let report_json =
            serde_json::to_value(report).map_err(|e| format!("serialize report: {}", e))?;

        let n = conn
            .execute(
                r#"
                UPDATE coord.tasks
                SET completion_report = $1::jsonb,
                    completion_source = $2,
                    updated_at = NOW()
                WHERE id = $3::uuid
                "#,
                &[&report_json, &source.as_str(), &task_id],
            )
            .await
            .map_err(|e| format!("write_completion_report: {}", e))?;

        if n == 0 {
            return Err(format!("no task with id {}", task_id));
        }
        Ok(())
    }

    /// Load the structured report (and source tag) for a task, or `Ok(None)`
    /// if either column is null. Unknown enum values for `completion_source`
    /// are filtered to `None` (forward-compat).
    pub async fn get_completion_report(
        &self,
        task_id: &str,
    ) -> Result<Option<(CompletionReport, CompletionSource)>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                r#"
                SELECT completion_report, completion_source
                FROM coord.tasks
                WHERE id = $1::uuid
                "#,
                &[&task_id],
            )
            .await
            .map_err(|e| format!("get_completion_report: {}", e))?;

        let Some(row) = row else { return Ok(None) };

        let report_json: Option<Value> = row.get(0);
        let source_text: Option<String> = row.get(1);

        match (report_json, source_text) {
            (Some(rj), Some(st)) => {
                let report: CompletionReport = serde_json::from_value(rj)
                    .map_err(|e| format!("deserialize completion_report: {}", e))?;
                let source = match CompletionSource::from_str_value(&st) {
                    Some(s) => s,
                    None => return Ok(None),
                };
                Ok(Some((report, source)))
            }
            _ => Ok(None),
        }
    }

    /// Walk `coord.tasks.depends_on` recursively starting at
    /// `proposed_upstream_id`. Returns `Some(path)` (as UUID strings) if
    /// adding the edge `task_id -> proposed_upstream_id` would close a cycle
    /// (i.e. the walk reaches `task_id`); `None` if the edge is safe.
    ///
    /// Implements the canonical SQL from
    /// productivity-coordinator-completion-reports §4 — PG14 `CYCLE … SET …
    /// USING …` for the load-bearing cycle check, plus a `depth <= 64`
    /// backstop on legitimately-deep acyclic chains.
    ///
    /// Schema-qualified per the search_path note in the module docs.
    pub async fn detect_dependency_cycle(
        &self,
        task_id: &str,
        proposed_upstream_id: &str,
    ) -> Result<Option<Vec<String>>, String> {
        // The proposed edge would close a cycle if and only if walking
        // upstream from `proposed_upstream_id` reaches `task_id`. The CTE
        // tracks the path and `is_cycle` for every visited row; PG14 emits
        // exactly one row per (parent, dep) pair with `is_cycle = true` when
        // the walk would re-visit a node. We post-filter to the row where
        // `task_id::uuid = ANY(path)`, which captures the closing edge.
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // The CYCLE clause's `path` column is a `uuid[]` of the visited
        // `upstream_id` values along the walk. tokio-postgres in this crate
        // does not enable `with-uuid-1`, so we cast the array element-wise
        // to text in the SELECT and decode as Vec<String> — same convention
        // as `tasks.depends_on` in `database/pg/tasks.rs`.
        let row_opt = conn
            .query_opt(
                r#"
                WITH RECURSIVE upstream_chain AS (
                    -- Anchor: every direct upstream of the proposed parent.
                    SELECT t.id::uuid    AS task_id,
                           dep_id::uuid  AS upstream_id,
                           1             AS depth
                    FROM coord.tasks t,
                         unnest(t.depends_on) AS dep_id
                    WHERE t.id = $1::uuid

                  UNION ALL

                    -- Recursive step: walk one edge further up the chain.
                    SELECT c.upstream_id              AS task_id,
                           dep_id::uuid               AS upstream_id,
                           c.depth + 1                AS depth
                    FROM upstream_chain c
                    JOIN coord.tasks t ON t.id = c.upstream_id,
                         unnest(t.depends_on) AS dep_id
                    WHERE c.depth <= 64
                )
                CYCLE upstream_id SET is_cycle USING path
                SELECT (
                    SELECT COALESCE(array_agg(p::text), '{}')
                    FROM unnest(path) p
                ) AS path_text
                FROM upstream_chain
                WHERE upstream_id = $2::uuid
                LIMIT 1
                "#,
                &[&proposed_upstream_id, &task_id],
            )
            .await
            .map_err(|e| format!("detect_dependency_cycle: {}", e))?;

        match row_opt {
            Some(r) => {
                let path: Vec<String> = r.get(0);
                Ok(Some(path))
            }
            None => Ok(None),
        }
    }

    /// Append `upstream_task_id` to `task_id`'s `depends_on` array. Caller
    /// is responsible for cycle-checking before calling — this is just the
    /// mutation. Returns `Err` if the calling task does not exist.
    pub async fn add_task_dependency(
        &self,
        task_id: &str,
        upstream_task_id: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let n = conn
            .execute(
                r#"
                UPDATE coord.tasks
                SET depends_on = depends_on || $1::uuid,
                    updated_at = NOW()
                WHERE id = $2::uuid
                "#,
                &[&upstream_task_id, &task_id],
            )
            .await
            .map_err(|e| format!("add_task_dependency: {}", e))?;

        if n == 0 {
            return Err(format!("no task with id {}", task_id));
        }
        Ok(())
    }

    /// Stash a transient JSONB blob on the task for Phase 2's brief composer
    /// to consume. See plan §4 "Rule E extension" / "Rule B extension". Cleared
    /// via `clear_assignment_brief_extras` after the brief is dispatched.
    pub async fn write_assignment_brief_extras(
        &self,
        task_id: &str,
        extras: Value,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let n = conn
            .execute(
                r#"
                UPDATE coord.tasks
                SET assignment_brief_extras = $1::jsonb,
                    updated_at = NOW()
                WHERE id = $2::uuid
                "#,
                &[&extras, &task_id],
            )
            .await
            .map_err(|e| format!("write_assignment_brief_extras: {}", e))?;

        if n == 0 {
            return Err(format!("no task with id {}", task_id));
        }
        Ok(())
    }

    /// Null out the transient `assignment_brief_extras` column. Called by
    /// Phase 2's Rule B path after the assignment brief has been sent to
    /// the worker.
    pub async fn clear_assignment_brief_extras(&self, task_id: &str) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        conn.execute(
            r#"
            UPDATE coord.tasks
            SET assignment_brief_extras = NULL,
                updated_at = NOW()
            WHERE id = $1::uuid
            "#,
            &[&task_id],
        )
        .await
        .map_err(|e| format!("clear_assignment_brief_extras: {}", e))?;

        Ok(())
    }

    /// Read the transient `assignment_brief_extras` JSONB column. Returns
    /// `Ok(None)` when the row exists but the column is null, and `Ok(None)`
    /// when the row does not exist (callers usually only invoke this after
    /// confirming the row exists via `get_task_by_id`).
    pub async fn get_assignment_brief_extras(
        &self,
        task_id: &str,
    ) -> Result<Option<Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                r#"
                SELECT assignment_brief_extras
                FROM coord.tasks
                WHERE id = $1::uuid
                "#,
                &[&task_id],
            )
            .await
            .map_err(|e| format!("get_assignment_brief_extras: {}", e))?;

        Ok(row.and_then(|r| r.get::<_, Option<Value>>(0)))
    }

    /// Outcome of the per-row Rule-E flip — one of:
    ///   - `Flipped` — the row was flipped pending → ready and
    ///     `assignment_brief_extras` was populated with the upstream reports.
    ///   - `Blocked` — at least one upstream's `completion_report.follow_ups`
    ///     entry has `blockingForDependents=true`. The row stays pending.
    ///   - `MissingReport` — at least one upstream is `done` but has a null
    ///     `completion_report`. Data integrity anomaly; row stays pending.
    ///   - `Skipped` — deps not all done yet (rule E shouldn't have selected
    ///     this row, but a racing tick could).
    pub async fn evaluate_and_flip_unblocked_task(
        &self,
        task_id: &str,
    ) -> Result<UnblockEvaluation, String> {
        // Reload the task row inside this call so we have a fresh snapshot of
        // depends_on. The scheduler's observation could be 10s stale.
        let task = match self.get_task_by_id(task_id).await? {
            Some(t) => t,
            None => return Err(format!("no task with id {}", task_id)),
        };
        if task.status != "pending" {
            return Ok(UnblockEvaluation::Skipped {
                reason: format!("task status is {}, not pending", task.status),
            });
        }

        // Gather every upstream's status + completion_report in one fan-out.
        // depends_on can legitimately be empty — empty deps are trivially
        // unblocked, no upstream reports to attach.
        let mut upstream_pairs: Vec<(String, Option<CompletionReport>)> =
            Vec::with_capacity(task.depends_on.len());
        for dep_id in &task.depends_on {
            let dep = match self.get_task_by_id(dep_id).await? {
                Some(d) => d,
                None => {
                    return Ok(UnblockEvaluation::Skipped {
                        reason: format!("upstream {} not found", dep_id),
                    });
                }
            };
            if dep.status != "done" {
                return Ok(UnblockEvaluation::Skipped {
                    reason: format!("upstream {} is {}", dep.id, dep.status),
                });
            }
            let report = self
                .get_completion_report(&dep.id)
                .await?
                .map(|(r, _src)| r);
            upstream_pairs.push((dep.id.clone(), report));
        }

        // Data integrity check — every done upstream MUST have a non-null
        // report after the migration. Surface as a coordinator-escalation
        // (data-integrity-anomaly) rather than silently flipping.
        let missing: Vec<String> = upstream_pairs
            .iter()
            .filter(|(_, r)| r.is_none())
            .map(|(id, _)| id.clone())
            .collect();
        if !missing.is_empty() {
            return Ok(UnblockEvaluation::MissingReport {
                upstream_ids: missing,
            });
        }

        // Aggregate blocking_for_dependents across all upstream follow-ups.
        // ANY blocking follow-up holds the dependent in pending and emits an
        // escalation card.
        let mut blockers: Vec<BlockingFollowUp> = Vec::new();
        for (uid, report_opt) in &upstream_pairs {
            if let Some(report) = report_opt {
                for fu in &report.follow_ups {
                    if fu.blocking_for_dependents {
                        blockers.push(BlockingFollowUp {
                            upstream_task_id: uid.clone(),
                            description: fu.description.clone(),
                            priority: fu.priority.clone(),
                        });
                    }
                }
            }
        }
        if !blockers.is_empty() {
            return Ok(UnblockEvaluation::Blocked { blockers });
        }

        // No blockers → flip pending → ready and stash upstream reports.
        let upstreams_payload: Vec<serde_json::Value> = upstream_pairs
            .iter()
            .map(|(uid, report_opt)| {
                serde_json::json!({
                    "taskId": uid,
                    "report": report_opt,
                })
            })
            .collect();
        let extras = serde_json::json!({
            "upstreams": upstreams_payload,
        });

        // Conditional flip — same idempotency guard as transition_task_status:
        // the WHERE clause requires status='pending' so a racing tick that
        // already moved the row won't double-flip. Stash extras in the same
        // statement so Rule B downstream cannot observe a flipped row without
        // its briefing payload.
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let n = conn
            .execute(
                r#"
                UPDATE coord.tasks
                SET status = 'ready',
                    assignment_brief_extras = $2::jsonb,
                    updated_at = NOW()
                WHERE id = $1::uuid
                  AND status = 'pending'
                "#,
                &[&task_id, &extras],
            )
            .await
            .map_err(|e| format!("evaluate_and_flip_unblocked_task: {}", e))?;
        if n == 0 {
            return Ok(UnblockEvaluation::Skipped {
                reason: "row was flipped concurrently".to_string(),
            });
        }
        Ok(UnblockEvaluation::Flipped)
    }

    /// Plan-scoped wrapper for `evaluate_and_flip_unblocked_task`. Walks every
    /// `pending` task in the plan whose deps are all `done`, evaluates per
    /// row, and returns the per-row outcomes for the scheduler to log /
    /// escalate. Replaces the simpler `mark_ready_for_unblocked` for any path
    /// that needs blocking-follow-up awareness — old function stays for any
    /// caller that doesn't care about briefing payloads.
    pub async fn mark_ready_for_unblocked_with_briefs(
        &self,
        plan_id: &str,
    ) -> Result<Vec<UnblockOutcome>, String> {
        // Find every pending task in this plan whose every dep is done. The
        // SQL is the same predicate `mark_ready_for_unblocked` uses, but we
        // SELECT the candidate ids instead of UPDATE-ing in one shot —
        // per-row evaluation still has to read each upstream's report.
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                r#"
                SELECT t.id::text
                FROM coord.tasks t
                WHERE t.plan_id = $1::uuid
                  AND t.status = 'pending'
                  AND NOT EXISTS (
                      SELECT 1 FROM unnest(t.depends_on) AS dep_id
                      WHERE NOT EXISTS (
                          SELECT 1 FROM coord.tasks d
                          WHERE d.id = dep_id AND d.status = 'done'
                      )
                  )
                "#,
                &[&plan_id],
            )
            .await
            .map_err(|e| format!("mark_ready_for_unblocked_with_briefs select: {}", e))?;

        let candidate_ids: Vec<String> = rows.iter().map(|r| r.get(0)).collect();
        drop(conn);

        let mut outcomes = Vec::with_capacity(candidate_ids.len());
        for tid in candidate_ids {
            let outcome = self.evaluate_and_flip_unblocked_task(&tid).await?;
            outcomes.push(UnblockOutcome {
                task_id: tid,
                evaluation: outcome,
            });
        }
        Ok(outcomes)
    }

    /// Pull the most-recent `WORKER_ADDED_DEPENDENCY` `coordinator_decisions`
    /// row whose `target_id` equals `task_id`. Used by Rule B's brief composer
    /// to compute the "Resumed from pause — you added a dependency on …"
    /// prefix line. Returns `Ok(None)` when no such row exists (fresh
    /// assignment, not a resume).
    pub async fn latest_worker_added_dependency_for_task(
        &self,
        task_id: &str,
    ) -> Result<Option<crate::database::pg::coordinator_decisions::CoordinatorDecisionRow>, String>
    {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                r#"
                SELECT
                    id::text, session_id, iteration, rule, action, target_id,
                    reasoning, auto_acted, resolved, resolution,
                    resolved_at::text, created_at::text
                FROM coord.coordinator_decisions
                WHERE rule = 'WORKER_ADDED_DEPENDENCY'
                  AND target_id = $1
                ORDER BY created_at DESC
                LIMIT 1
                "#,
                &[&task_id],
            )
            .await
            .map_err(|e| format!("latest_worker_added_dependency_for_task: {}", e))?;

        Ok(row.map(|r| crate::database::pg::coordinator_decisions::CoordinatorDecisionRow {
            id: r.get(0),
            session_id: r.get(1),
            iteration: r.get(2),
            rule: r.get(3),
            action: r.get(4),
            target_id: r.get(5),
            reasoning: r.get(6),
            auto_acted: r.get(7),
            resolved: r.get(8),
            resolution: r.get(9),
            resolved_at: r.get(10),
            created_at: r.get(11),
        }))
    }

    /// Bypass-validator transition used by `mcp::completion_reports::add_dependency_inner`
    /// when a worker declares a new upstream while the task is `running` or
    /// `assigned`. The canonical state machine doesn't permit those edges
    /// directly (see `is_valid_transition`); this helper takes the
    /// responsibility for atomicity (WHERE status = $2) and skips the
    /// validator.
    ///
    /// Intentionally narrow: only used for the worker-added-dependency drop
    /// back to `pending`. Callers passing arbitrary transitions through this
    /// helper invalidate the state-machine invariant.
    pub async fn transition_task_status_unchecked(
        &self,
        task_id: &str,
        from: &str,
        to: &str,
    ) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let n = conn
            .execute(
                r#"
                UPDATE coord.tasks
                SET status = $3,
                    updated_at = NOW()
                WHERE id = $1::uuid AND status = $2
                "#,
                &[&task_id, &from, &to],
            )
            .await
            .map_err(|e| format!("transition_task_status_unchecked: {}", e))?;

        Ok(n > 0)
    }

    /// Write a single key into `coord.tasks.completion_report -> 'artifacts'`
    /// using `jsonb_set`. Phase 5 cache primitive (plan §9 "Token budget on
    /// aggregated briefs"): `dispatch_assignment_brief` calls this with
    /// `key = "condensedSummaryMd"` after computing a condensed summary so
    /// subsequent briefs reuse it instead of re-truncating.
    ///
    /// Idempotent — overwrites the existing value at that key. Best-effort:
    /// callers log+continue on failure (no transactional dependency on the
    /// cache write).
    ///
    /// Returns `Err` if no row exists for `task_id` or the JSONB structure is
    /// malformed (e.g. completion_report is null — caller should ensure the
    /// upstream actually has a report before invoking).
    pub async fn write_completion_report_artifact(
        &self,
        task_id: &str,
        key: &str,
        value: serde_json::Value,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // jsonb_set with `create_missing := true` adds the path if missing.
        // The path is a TEXT[] of two elements: 'artifacts' and the key.
        // tokio-postgres encodes Rust Vec<String> as text[] when bound to a
        // text[] parameter. completion_report itself must be non-null —
        // the WHERE clause guards against that case.
        let path: Vec<String> = vec!["artifacts".to_string(), key.to_string()];
        let n = conn
            .execute(
                r#"
                UPDATE coord.tasks
                SET completion_report = jsonb_set(
                    completion_report,
                    $2::text[],
                    $3::jsonb,
                    true
                ),
                    updated_at = NOW()
                WHERE id = $1::uuid AND completion_report IS NOT NULL
                "#,
                &[&task_id, &path, &value],
            )
            .await
            .map_err(|e| format!("write_completion_report_artifact: {}", e))?;

        if n == 0 {
            return Err(format!(
                "write_completion_report_artifact: no task with id {} (or completion_report is null)",
                task_id
            ));
        }
        Ok(())
    }

    /// Phase 5 startup sweep (plan §9 "Memory pressure audit"): clear any
    /// `assignment_brief_extras` rows on tasks whose status has moved past
    /// `pending`. Rule E populates `assignment_brief_extras` only for pending
    /// tasks; if the task has moved past pending without the AssignTask hook
    /// clearing the field, it's a stale leak. One-shot, idempotent — safe to
    /// run unconditionally at PG bootstrap.
    ///
    /// Returns the number of rows cleared.
    pub async fn clear_stale_assignment_brief_extras(&self) -> Result<u64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let n = conn
            .execute(
                r#"
                UPDATE coord.tasks
                SET assignment_brief_extras = NULL,
                    updated_at = NOW()
                WHERE status <> 'pending'
                  AND assignment_brief_extras IS NOT NULL
                "#,
                &[],
            )
            .await
            .map_err(|e| format!("clear_stale_assignment_brief_extras: {}", e))?;

        Ok(n)
    }

    /// Force-flip a task to `done` from any current state. Stamps
    /// `completed_at = NOW()` like `transition_task_status` does on the
    /// canonical `* -> done` edge.
    ///
    /// Used by `mcp::completion_sources` adapters (github-merge,
    /// manual-user-fire) where external authority overrides the in-flight
    /// state machine. Per plan §3 "External trigger sources":
    /// "Always also flips status to `done` if not already there — external
    /// sources are authoritative when invoked."
    ///
    /// Intentionally bypasses the `is_valid_transition` whitelist; callers
    /// must already have established external authority (e.g. a verified
    /// PR-merge webhook or a user-issued dashboard fire).
    ///
    /// Returns `true` if the row was updated, `false` if the task was
    /// already `done` (idempotent), and `Err` if no row exists at all.
    pub async fn force_task_status_to_done(&self, task_id: &str) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // First confirm the row exists. Avoids a silent no-op on a typo'd
        // task_id that would mask a bug at the call site.
        let exists = conn
            .query_opt(
                r#"SELECT 1 FROM coord.tasks WHERE id = $1::uuid"#,
                &[&task_id],
            )
            .await
            .map_err(|e| format!("force_task_status_to_done lookup: {}", e))?;
        if exists.is_none() {
            return Err(format!("no task with id {}", task_id));
        }

        let n = conn
            .execute(
                r#"
                UPDATE coord.tasks
                SET status = 'done',
                    completed_at = COALESCE(completed_at, NOW()),
                    updated_at = NOW()
                WHERE id = $1::uuid AND status <> 'done'
                "#,
                &[&task_id],
            )
            .await
            .map_err(|e| format!("force_task_status_to_done: {}", e))?;

        Ok(n > 0)
    }
}

/// Outcome of a per-row Rule-E flip attempt — see
/// `evaluate_and_flip_unblocked_task` for the semantics.
#[derive(Debug, Clone)]
pub enum UnblockEvaluation {
    /// Row was flipped pending → ready and `assignment_brief_extras` populated.
    Flipped,
    /// Row stays pending; one or more upstreams flagged a blocking follow-up.
    Blocked { blockers: Vec<BlockingFollowUp> },
    /// Row stays pending; one or more done upstreams have a null
    /// `completion_report` (data integrity anomaly).
    MissingReport { upstream_ids: Vec<String> },
    /// Row was not eligible (status changed, dep not yet done, etc.).
    Skipped { reason: String },
}

/// Wrapper pairing a candidate task id with its evaluation outcome.
#[derive(Debug, Clone)]
pub struct UnblockOutcome {
    pub task_id: String,
    pub evaluation: UnblockEvaluation,
}

/// One blocking follow-up entry surfaced from an upstream's completion report.
#[derive(Debug, Clone)]
pub struct BlockingFollowUp {
    pub upstream_task_id: String,
    pub description: String,
    pub priority: String,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Test helper: produce a `CompletionReport` that passes every validation
    /// rule. Each test mutates one field to assert that validator catches the
    /// specific violation.
    fn valid_report() -> CompletionReport {
        CompletionReport {
            summary_md: "Implementation complete.".to_string(),
            deliverables: vec![Deliverable {
                kind: "commit".to_string(),
                reference: "abc123".to_string(),
                description: "Initial commit".to_string(),
            }],
            breaking_changes: vec![BreakingChange {
                area: "api".to_string(),
                description: "Renamed function".to_string(),
                migration_steps_md: "Use the new name.".to_string(),
            }],
            follow_ups: vec![FollowUp {
                description: "Document the new helper".to_string(),
                priority: "important".to_string(),
                blocking_for_dependents: false,
            }],
            artifacts: HashMap::new(),
        }
    }

    #[test]
    fn valid_report_passes_validation() {
        let report = valid_report();
        validate_report(&report).expect("baseline valid report must pass");
    }

    #[test]
    fn validation_rejects_empty_summary_md() {
        let mut report = valid_report();
        report.summary_md = String::new();
        let err = validate_report(&report).expect_err("empty summary should reject");
        assert!(err.contains("summary_md must not be empty"), "got: {}", err);
    }

    #[test]
    fn validation_rejects_unknown_deliverable_kind() {
        let mut report = valid_report();
        report.deliverables[0].kind = "frob".to_string();
        let err = validate_report(&report).expect_err("unknown kind should reject");
        assert!(err.contains("deliverable[0].kind"), "got: {}", err);
        assert!(err.contains("must be one of"), "got: {}", err);
    }

    #[test]
    fn validation_rejects_empty_deliverable_reference() {
        let mut report = valid_report();
        report.deliverables[0].reference = String::new();
        let err = validate_report(&report).expect_err("empty reference should reject");
        assert!(err.contains("deliverable[0].reference"), "got: {}", err);
    }

    #[test]
    fn validation_rejects_empty_deliverable_description() {
        let mut report = valid_report();
        report.deliverables[0].description = String::new();
        let err = validate_report(&report).expect_err("empty description should reject");
        assert!(err.contains("deliverable[0].description"), "got: {}", err);
        assert!(err.contains("must not be empty"), "got: {}", err);
    }

    #[test]
    fn validation_rejects_oversize_deliverable_description() {
        let mut report = valid_report();
        report.deliverables[0].description = "x".repeat(MAX_DELIVERABLE_DESCRIPTION + 1);
        let err = validate_report(&report).expect_err("oversize description should reject");
        assert!(err.contains("deliverable[0].description"), "got: {}", err);
        assert!(err.contains("too long"), "got: {}", err);
    }

    #[test]
    fn validation_rejects_empty_breaking_change_area() {
        let mut report = valid_report();
        report.breaking_changes[0].area = String::new();
        let err = validate_report(&report).expect_err("empty area should reject");
        assert!(err.contains("breakingChanges[0].area"), "got: {}", err);
        assert!(err.contains("must not be empty"), "got: {}", err);
    }

    #[test]
    fn validation_rejects_oversize_breaking_change_area() {
        let mut report = valid_report();
        report.breaking_changes[0].area = "x".repeat(MAX_BREAKING_CHANGE_AREA + 1);
        let err = validate_report(&report).expect_err("oversize area should reject");
        assert!(err.contains("breakingChanges[0].area"), "got: {}", err);
        assert!(err.contains("too long"), "got: {}", err);
    }

    #[test]
    fn validation_rejects_empty_breaking_change_description() {
        let mut report = valid_report();
        report.breaking_changes[0].description = String::new();
        let err = validate_report(&report).expect_err("empty bc desc should reject");
        assert!(err.contains("breakingChanges[0].description"), "got: {}", err);
        assert!(err.contains("must not be empty"), "got: {}", err);
    }

    #[test]
    fn validation_rejects_oversize_breaking_change_description() {
        let mut report = valid_report();
        report.breaking_changes[0].description = "x".repeat(MAX_BREAKING_CHANGE_DESCRIPTION + 1);
        let err = validate_report(&report).expect_err("oversize bc desc should reject");
        assert!(err.contains("breakingChanges[0].description"), "got: {}", err);
        assert!(err.contains("too long"), "got: {}", err);
    }

    #[test]
    fn validation_rejects_oversize_breaking_change_migration_steps() {
        let mut report = valid_report();
        report.breaking_changes[0].migration_steps_md =
            "x".repeat(MAX_BREAKING_CHANGE_MIGRATION_STEPS_MD + 1);
        let err = validate_report(&report).expect_err("oversize migration steps should reject");
        assert!(
            err.contains("breakingChanges[0].migrationStepsMd"),
            "got: {}",
            err
        );
        assert!(err.contains("too long"), "got: {}", err);
    }

    #[test]
    fn validation_allows_empty_breaking_change_migration_steps() {
        let mut report = valid_report();
        report.breaking_changes[0].migration_steps_md = String::new();
        validate_report(&report).expect("empty migration_steps_md should pass");
    }

    #[test]
    fn validation_rejects_empty_follow_up_description() {
        let mut report = valid_report();
        report.follow_ups[0].description = String::new();
        let err = validate_report(&report).expect_err("empty fu desc should reject");
        assert!(err.contains("followUps[0].description"), "got: {}", err);
        assert!(err.contains("must not be empty"), "got: {}", err);
    }

    #[test]
    fn validation_rejects_oversize_follow_up_description() {
        let mut report = valid_report();
        report.follow_ups[0].description = "x".repeat(MAX_FOLLOW_UP_DESCRIPTION + 1);
        let err = validate_report(&report).expect_err("oversize fu desc should reject");
        assert!(err.contains("followUps[0].description"), "got: {}", err);
        assert!(err.contains("too long"), "got: {}", err);
    }

    #[test]
    fn validation_rejects_unknown_follow_up_priority() {
        let mut report = valid_report();
        report.follow_ups[0].priority = "URGENT".to_string();
        let err = validate_report(&report).expect_err("unknown priority should reject");
        assert!(err.contains("followUps[0].priority"), "got: {}", err);
        assert!(err.contains("must be one of"), "got: {}", err);
    }

    #[test]
    fn validation_accepts_all_canonical_deliverable_kinds() {
        for kind in ALLOWED_DELIVERABLE_KINDS {
            let mut report = valid_report();
            report.deliverables[0].kind = kind.to_string();
            validate_report(&report).unwrap_or_else(|e| {
                panic!("kind {:?} should be valid, got: {}", kind, e);
            });
        }
    }

    #[test]
    fn validation_accepts_all_canonical_priorities() {
        for prio in ALLOWED_FOLLOW_UP_PRIORITIES {
            let mut report = valid_report();
            report.follow_ups[0].priority = prio.to_string();
            validate_report(&report).unwrap_or_else(|e| {
                panic!("priority {:?} should be valid, got: {}", prio, e);
            });
        }
    }

    #[test]
    fn validation_caps_summary_md() {
        let report = CompletionReport {
            summary_md: "x".repeat(MAX_SUMMARY_MD_LEN + 1),
            deliverables: Vec::new(),
            breaking_changes: Vec::new(),
            follow_ups: Vec::new(),
            artifacts: HashMap::new(),
        };
        let err = validate_report(&report).expect_err("oversize summary should reject");
        assert!(err.starts_with("validation: summary_md"));
    }

    #[test]
    fn validation_caps_deliverables() {
        let mut report = CompletionReport {
            summary_md: "ok".to_string(),
            deliverables: Vec::new(),
            breaking_changes: Vec::new(),
            follow_ups: Vec::new(),
            artifacts: HashMap::new(),
        };
        for i in 0..=MAX_DELIVERABLES {
            report.deliverables.push(Deliverable {
                kind: "commit".to_string(),
                reference: format!("sha-{}", i),
                description: "x".to_string(),
            });
        }
        let err = validate_report(&report).expect_err("oversize deliverables should reject");
        assert!(err.contains("deliverables"));
    }

    #[test]
    fn validation_caps_breaking_changes() {
        let mut report = CompletionReport {
            summary_md: "ok".to_string(),
            deliverables: Vec::new(),
            breaking_changes: Vec::new(),
            follow_ups: Vec::new(),
            artifacts: HashMap::new(),
        };
        for _ in 0..=MAX_BREAKING_CHANGES {
            report.breaking_changes.push(BreakingChange {
                area: "x".to_string(),
                description: "y".to_string(),
                migration_steps_md: String::new(),
            });
        }
        let err = validate_report(&report).expect_err("oversize breaking_changes should reject");
        assert!(err.contains("breaking_changes"));
    }

    #[test]
    fn validation_caps_follow_ups() {
        let mut report = CompletionReport {
            summary_md: "ok".to_string(),
            deliverables: Vec::new(),
            breaking_changes: Vec::new(),
            follow_ups: Vec::new(),
            artifacts: HashMap::new(),
        };
        for _ in 0..=MAX_FOLLOW_UPS {
            report.follow_ups.push(FollowUp {
                description: "x".to_string(),
                priority: "nice-to-have".to_string(),
                blocking_for_dependents: false,
            });
        }
        let err = validate_report(&report).expect_err("oversize follow_ups should reject");
        assert!(err.contains("follow_ups"));
    }

    #[test]
    fn completion_source_round_trip() {
        for src in [
            CompletionSource::SessionSelfReport,
            CompletionSource::ReviewerAttestation,
            CompletionSource::GithubMerge,
            CompletionSource::CiPipeline,
            CompletionSource::ManualUserFire,
            CompletionSource::LegacyPreCompletionReports,
        ] {
            let s = src.as_str();
            let parsed = CompletionSource::from_str_value(s).unwrap();
            assert_eq!(parsed, src);
        }
        assert!(CompletionSource::from_str_value("nonexistent-source").is_none());
    }

    #[test]
    fn completion_source_serializes_kebab_case() {
        let v = serde_json::to_string(&CompletionSource::SessionSelfReport).unwrap();
        assert_eq!(v, "\"session-self-report\"");
    }

    #[test]
    fn completion_report_camel_case_fields() {
        let report = CompletionReport {
            summary_md: "hi".to_string(),
            deliverables: vec![Deliverable {
                kind: "pr".to_string(),
                reference: "https://github.com/x/y/pull/1".to_string(),
                description: "demo".to_string(),
            }],
            breaking_changes: vec![],
            follow_ups: vec![FollowUp {
                description: "do thing".to_string(),
                priority: "important".to_string(),
                blocking_for_dependents: true,
            }],
            artifacts: HashMap::new(),
        };
        let json = serde_json::to_value(&report).unwrap();
        assert!(json.get("summaryMd").is_some());
        assert!(json.get("breakingChanges").is_some());
        assert!(json.get("followUps").is_some());
        let fu = &json.get("followUps").unwrap()[0];
        assert!(fu.get("blockingForDependents").is_some());
    }

    /// Unit reference test from plan §4: insert a 3-task A→B→C cycle (insert
    /// A→B, insert B→C, attempt C→A) and confirm the recursive CTE returns
    /// `is_cycle = true` on the closing edge.
    ///
    /// Marked `#[ignore]` because it needs a live PG fixture; run manually
    /// with `cargo test -p qontinui-runner completion_reports::tests::cycle_detection_three_task -- --ignored --nocapture`
    /// after an alembic upgrade head against a temp DB.
    #[tokio::test]
    #[ignore = "needs PG fixture (DATABASE_URL with cr01a2b3c4d5 applied)"]
    async fn cycle_detection_three_task() {
        use crate::database::pg::tasks::InsertTaskInput;

        let pg = PgDb::new_blocking_for_test();

        // Spin up a synthetic plan first via a raw insert so we have a
        // plan_id to satisfy NOT NULL.
        let conn = pg.pool().get().await.expect("conn");
        let plan_row = conn
            .query_one(
                r#"
                INSERT INTO coord.plans (markdown_path, plan_version_hash, status)
                VALUES ($1, $2, $3)
                RETURNING id::text, plan_version_hash
                "#,
                &[
                    &"/tmp/cycle-test.md",
                    &"deadbeef",
                    &"decomposed",
                ],
            )
            .await
            .expect("insert plan");
        let plan_id: String = plan_row.get(0);
        let plan_version_hash: String = plan_row.get(1);

        // Insert tasks A, B, C with no deps initially.
        let a = pg
            .insert_task(&InsertTaskInput {
                plan_id: &plan_id,
                plan_version_hash: &plan_version_hash,
                phase_name: "P",
                sequence_in_phase: 1,
                description: "A",
                expected_file_claims: &[],
                expected_dirs: &[],
                depends_on: &[],
                status: "pending",
                notes: None,
            })
            .await
            .expect("insert A");
        let b = pg
            .insert_task(&InsertTaskInput {
                plan_id: &plan_id,
                plan_version_hash: &plan_version_hash,
                phase_name: "P",
                sequence_in_phase: 2,
                description: "B",
                expected_file_claims: &[],
                expected_dirs: &[],
                depends_on: &[],
                status: "pending",
                notes: None,
            })
            .await
            .expect("insert B");
        let c = pg
            .insert_task(&InsertTaskInput {
                plan_id: &plan_id,
                plan_version_hash: &plan_version_hash,
                phase_name: "P",
                sequence_in_phase: 3,
                description: "C",
                expected_file_claims: &[],
                expected_dirs: &[],
                depends_on: &[],
                status: "pending",
                notes: None,
            })
            .await
            .expect("insert C");

        // A depends on B, B depends on C. Now adding C -> A would close
        // the cycle (A -> B -> C -> A).
        pg.add_task_dependency(&a.id, &b.id).await.expect("A->B");
        pg.add_task_dependency(&b.id, &c.id).await.expect("B->C");

        // Sanity: A->B alone is acyclic.
        let no_cycle = pg
            .detect_dependency_cycle(&a.id, &b.id)
            .await
            .expect("detect_dependency_cycle ok");
        // Calling with the existing edge does not, by itself, indicate a
        // cycle since the CTE walks ABOVE the proposed parent. The call
        // semantics are "would *adding* this edge close a cycle?", and
        // because A -> B is already present, walking from B does not
        // reach A unless we ALSO had B -> A.
        assert!(no_cycle.is_none(), "A->B is acyclic");

        // The closing edge: adding C -> A should be detected as a cycle.
        let cycle = pg
            .detect_dependency_cycle(&c.id, &a.id)
            .await
            .expect("detect_dependency_cycle");
        assert!(cycle.is_some(), "C -> A must close a cycle");
        let path = cycle.unwrap();
        assert!(!path.is_empty(), "cycle path must contain at least 1 step");

        // Cleanup so re-runs don't accumulate.
        let _ = conn
            .execute("DELETE FROM coord.plans WHERE id = $1::uuid", &[&plan_id])
            .await;
    }

    /// Non-cycle case: a linear chain A → B → C is fine; adding D as a new
    /// upstream of C must not flag.
    #[tokio::test]
    #[ignore = "needs PG fixture (DATABASE_URL with cr01a2b3c4d5 applied)"]
    async fn cycle_detection_linear_chain_is_safe() {
        use crate::database::pg::tasks::InsertTaskInput;

        let pg = PgDb::new_blocking_for_test();
        let conn = pg.pool().get().await.expect("conn");
        let plan_row = conn
            .query_one(
                r#"
                INSERT INTO coord.plans (markdown_path, plan_version_hash, status)
                VALUES ($1, $2, $3)
                RETURNING id::text, plan_version_hash
                "#,
                &[
                    &"/tmp/linear-test.md",
                    &"feedface",
                    &"decomposed",
                ],
            )
            .await
            .expect("insert plan");
        let plan_id: String = plan_row.get(0);
        let plan_version_hash: String = plan_row.get(1);

        let mk = |seq: i32, desc: &'static str, deps: Vec<String>| {
            let pg = pg.clone();
            let plan_id = plan_id.clone();
            let plan_version_hash = plan_version_hash.clone();
            async move {
                pg.insert_task(&InsertTaskInput {
                    plan_id: &plan_id,
                    plan_version_hash: &plan_version_hash,
                    phase_name: "P",
                    sequence_in_phase: seq,
                    description: desc,
                    expected_file_claims: &[],
                    expected_dirs: &[],
                    depends_on: &deps,
                    status: "pending",
                    notes: None,
                })
                .await
                .expect("insert task")
            }
        };

        let c = mk(1, "C", vec![]).await;
        let b = mk(2, "B", vec![c.id.clone()]).await;
        let a = mk(3, "A", vec![b.id.clone()]).await;
        let d = mk(4, "D", vec![]).await;

        let cycle = pg
            .detect_dependency_cycle(&a.id, &d.id)
            .await
            .expect("detect_dependency_cycle");
        assert!(cycle.is_none(), "A -> D must be safe (D is a leaf)");

        let _ = conn
            .execute("DELETE FROM coord.plans WHERE id = $1::uuid", &[&plan_id])
            .await;
    }
}
