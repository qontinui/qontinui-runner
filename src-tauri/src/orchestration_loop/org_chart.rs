//! Approach-D Conductor/Engine — Phase 4 org-chart seeds, the planner
//! prompt/parse for the §3 shape, and the ONE shared splice path both the
//! DESIGN bootstrap and progressive elaboration end in.
//!
//! Contract refs: `D:/qontinui-root/qontinui-dev-notes/plans/2026-06-13-terminal-orchestration-handoff-contract.md`
//! §3 (org-chart shape), §3.5 (progressive elaboration + monotonic-growth
//! invariants), §4 (the `artifacts["orchestration"].next_subtasks` carrier).
//!
//! ## Why one module for DESIGN + harvest
//!
//! Both the run's initial org-chart (produced by the Planning phase, written
//! with `produced_by = None`) and a mid-run elaboration (an `emits_subtasks`
//! worker's emitted children, written with `produced_by = Some(parent)`) are
//! *the same operation*: take a list of §3 seeds, materialize them into
//! ledger [`Subtask`] rows, and INSERT-IF-NOT-EXISTS them via
//! [`PgDb::splice_subtasks`]. Factoring that as [`splice_org_chart`] means the
//! idempotency + monotonic-growth invariants are enforced in exactly one place
//! and the DESIGN bootstrap can never drift from the harvest path.
//!
//! ## Monotonic-growth invariants (contract §3.5 — enforced here)
//!
//! - The splice MAY add rows; it MUST NOT edit or delete live rows. This is
//!   guaranteed structurally: [`splice_org_chart`] only ever calls
//!   [`PgDb::splice_subtasks`] (`INSERT … ON CONFLICT DO NOTHING`), NEVER
//!   [`PgDb::upsert_subtask`] (`DO UPDATE`). Re-reading the same artifact on a
//!   later tick is therefore a no-op (returns 0 new rows).
//! - A spec revision that invalidates an in-flight subtask is handled by
//!   `Canceled` + a replacement row (a future revision pipeline), **never** by
//!   editing the live row. v1 does not implement a revision pipeline, but the
//!   code here cannot mutate a live row even if asked to (insert-only).

use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use uuid::Uuid;

use super::ledger::{Subtask, SubtaskState};
use crate::database::pg::completion_reports::CompletionReport;

/// The reserved `CompletionReport.artifacts` key carrying this contract's
/// control surface (§4). Domain payloads (`functional_spec`, etc.) live under
/// their own keys alongside it.
pub const ORCHESTRATION_ARTIFACT_KEY: &str = "orchestration";

/// One org-chart seed — the §3 shape an elaborating worker (or the DESIGN
/// pass) emits. Deserialized from the planner/worker JSON; materialized into a
/// ledger [`Subtask`] by [`splice_org_chart`].
///
/// `brief` maps to the existing decomposer `description` (contract §3 mapping
/// note). `phase`/`repo`/`emits_subtasks` are the net-new fields this contract
/// adds. `emits_subtasks` defaults to `false` (§3.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgChartSeed {
    /// Stable id within the run (the DAG node key). `== task_id` once spliced.
    pub id: String,
    pub title: String,
    /// Full prompt/instructions handed to the worker. `== Subtask.brief`.
    pub brief: String,
    pub phase: String,
    /// Seed-declared dependency ids (edges of the DAG). The producing parent's
    /// id is added on top of these by [`splice_org_chart`] for harvested seeds.
    #[serde(default)]
    pub depends_on: Vec<String>,
    pub expected_output: String,
    /// Worktree allocation target; `null` for read-only tasks.
    #[serde(default)]
    pub repo: Option<String>,
    /// `true` for an elaborating seed whose completion artifact will itself
    /// carry `next_subtasks`. Default `false`.
    #[serde(default)]
    pub emits_subtasks: bool,
}

/// Parse the `next_subtasks` seed array out of a completed elaborator's
/// artifact, reading `artifact.artifacts["orchestration"]["next_subtasks"]`
/// (contract §3.5 / §4).
///
/// Gracefully returns `Ok(vec![])` when the orchestration key, the
/// `next_subtasks` field, or its value is absent/null — harvesting zero seeds
/// is a valid outcome (an `emits_subtasks` worker that chose not to elaborate).
/// Returns `Err` ONLY when `next_subtasks` is present but malformed (not an
/// array of the seed shape) — per contract §8 Q1 a malformed machine-readable
/// `data` part must surface as a failure, not silently advance the DAG.
pub fn parse_next_subtasks(artifact: &CompletionReport) -> Result<Vec<OrgChartSeed>, String> {
    let Some(orchestration) = artifact.artifacts.get(ORCHESTRATION_ARTIFACT_KEY) else {
        return Ok(Vec::new());
    };
    let next = match orchestration.get("next_subtasks") {
        None | Some(serde_json::Value::Null) => return Ok(Vec::new()),
        Some(v) => v,
    };
    let arr = next.as_array().ok_or_else(|| {
        format!(
            "next_subtasks must be a JSON array of seeds, got {}",
            kind_of(next)
        )
    })?;
    let mut seeds = Vec::with_capacity(arr.len());
    for (i, item) in arr.iter().enumerate() {
        let seed: OrgChartSeed = serde_json::from_value(item.clone())
            .map_err(|e| format!("next_subtasks[{i}] is not a valid org-chart seed: {e}"))?;
        seeds.push(seed);
    }
    Ok(seeds)
}

fn kind_of(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// Materialize a batch of §3 seeds into ledger [`Subtask`] rows starting at a
/// given `idx`, wiring each row's `produced_by` + (for harvested seeds) the
/// parent dependency. Pure — no DB. Split out so the seed→row mapping is
/// unit-testable without PG.
///
/// - `produced_by = Some(parent)` for harvested seeds (§3.5); `None` for the
///   DESIGN bootstrap.
/// - When `produced_by` is `Some(parent)`, the parent `task_id` is appended to
///   each seed's declared `depends_on` (de-duplicated) so a generated
///   build/test seed can never go `Submitted` before the producing
///   comprehension artifact exists (contract §3.5).
/// - `idx` is assigned sequentially after `start_idx`, preserving seed order.
/// - `state = Submitted`, `task_run_id = None`, `artifact = None` on every new
///   row.
pub fn seeds_to_subtasks(
    run_id: Uuid,
    seeds: &[OrgChartSeed],
    produced_by: Option<&str>,
    start_idx: i32,
) -> Vec<Subtask> {
    let now = chrono::Utc::now();
    seeds
        .iter()
        .enumerate()
        .map(|(offset, seed)| {
            let mut depends_on = seed.depends_on.clone();
            if let Some(parent) = produced_by {
                // Parent dependency wiring (§3.5): the comprehension artifact
                // must exist before any harvested child can dispatch. De-dup
                // so a seed that already names the parent doesn't double it.
                if !depends_on.iter().any(|d| d == parent) {
                    depends_on.push(parent.to_string());
                }
            }
            Subtask {
                task_id: seed.id.clone(),
                run_id,
                idx: start_idx + offset as i32,
                title: seed.title.clone(),
                brief: seed.brief.clone(),
                phase: seed.phase.clone(),
                repo: seed.repo.clone(),
                depends_on,
                expected_output: seed.expected_output.clone(),
                emits_subtasks: seed.emits_subtasks,
                state: SubtaskState::Submitted,
                task_run_id: None,
                artifact: None,
                produced_by: produced_by.map(|p| p.to_string()),
                // Phase 6: a freshly-spliced subtask carries no gate yet; the
                // reconciler classifies its `expected_output` + registers a gate
                // on its first dispatch tick (if observable).
                gate_id: None,
                gate_status: None,
                created_at: now,
                updated_at: now,
            }
        })
        .collect()
}

/// THE shared splice path both the DESIGN bootstrap (`produced_by = None`) and
/// [`super::conductor::harvest_elaboration`] (`produced_by = Some(parent)`)
/// call.
///
/// Re-reads the current rows to compute the next free `idx` (so harvested
/// children land after everything already in the run), maps the seeds to rows
/// via [`seeds_to_subtasks`], and idempotently inserts them via
/// [`PgDb::splice_subtasks`] (INSERT-IF-NOT-EXISTS keyed `(run_id, task_id)`).
///
/// Returns the number of rows actually inserted — 0 when every seed already
/// exists (idempotent re-read) or when `seeds` is empty.
pub async fn splice_org_chart(
    pg: &std::sync::Arc<crate::database::pg::PgDb>,
    run_id: Uuid,
    seeds: &[OrgChartSeed],
    produced_by: Option<&str>,
) -> Result<u64, String> {
    if seeds.is_empty() {
        return Ok(0);
    }

    // Compute the next free idx AFTER the current max idx in the run, so
    // harvested children sort after the parent (and after each other). Reading
    // the live rows here keeps the splice correct under concurrent growth.
    let existing = pg.list_subtasks(run_id).await?;
    let start_idx = existing
        .iter()
        .map(|s| s.idx)
        .max()
        .map(|m| m + 1)
        .unwrap_or(0);

    let rows = seeds_to_subtasks(run_id, seeds, produced_by, start_idx);
    let inserted = pg.splice_subtasks(&rows).await?;

    info!(
        "splice_org_chart: run={} produced_by={:?} seeds={} inserted={} (start_idx={})",
        run_id,
        produced_by,
        seeds.len(),
        inserted,
        start_idx
    );
    if inserted < rows.len() as u64 {
        // Expected on a re-read (idempotency) — log so an UNEXPECTED partial
        // splice during DESIGN bootstrap is still visible.
        warn!(
            "splice_org_chart: run={} {} of {} seeds already existed (idempotent skip)",
            run_id,
            rows.len() as u64 - inserted,
            rows.len()
        );
    }
    Ok(inserted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn report_with_next(next: serde_json::Value) -> CompletionReport {
        let mut artifacts = HashMap::new();
        artifacts.insert(
            ORCHESTRATION_ARTIFACT_KEY.to_string(),
            serde_json::json!({ "next_subtasks": next }),
        );
        CompletionReport {
            summary_md: "spec done".to_string(),
            deliverables: vec![],
            breaking_changes: vec![],
            follow_ups: vec![],
            artifacts,
        }
    }

    fn empty_report() -> CompletionReport {
        CompletionReport {
            summary_md: "done".to_string(),
            deliverables: vec![],
            breaking_changes: vec![],
            follow_ups: vec![],
            artifacts: HashMap::new(),
        }
    }

    // --- parse_next_subtasks ------------------------------------------------

    #[test]
    fn parse_absent_orchestration_key_is_zero_seeds() {
        let seeds = parse_next_subtasks(&empty_report()).unwrap();
        assert!(seeds.is_empty(), "no orchestration key ⇒ harvest 0 (valid)");
    }

    #[test]
    fn parse_null_next_subtasks_is_zero_seeds() {
        let r = report_with_next(serde_json::Value::Null);
        assert!(parse_next_subtasks(&r).unwrap().is_empty());
    }

    #[test]
    fn parse_empty_array_is_zero_seeds() {
        let r = report_with_next(serde_json::json!([]));
        assert!(parse_next_subtasks(&r).unwrap().is_empty());
    }

    #[test]
    fn parse_two_build_seeds() {
        let r = report_with_next(serde_json::json!([
            {
                "id": "build-1",
                "title": "Build the login screen",
                "brief": "Implement login per the spec",
                "phase": "implement",
                "depends_on": [],
                "expected_output": "a green PR",
                "repo": "qontinui-mobile"
            },
            {
                "id": "build-2",
                "title": "Build the home screen",
                "brief": "Implement home per the spec",
                "phase": "implement",
                "depends_on": ["build-1"],
                "expected_output": "a green PR",
                "repo": "qontinui-mobile",
                "emits_subtasks": false
            }
        ]));
        let seeds = parse_next_subtasks(&r).unwrap();
        assert_eq!(seeds.len(), 2);
        assert_eq!(seeds[0].id, "build-1");
        assert!(!seeds[0].emits_subtasks, "default false when omitted");
        assert_eq!(seeds[1].depends_on, vec!["build-1".to_string()]);
        assert_eq!(seeds[1].repo.as_deref(), Some("qontinui-mobile"));
    }

    #[test]
    fn parse_malformed_next_subtasks_is_error() {
        // Present but not an array → reject (contract §8 Q1).
        let r = report_with_next(serde_json::json!({"not": "an array"}));
        assert!(parse_next_subtasks(&r).is_err());
        // Array element missing required fields → reject.
        let r2 = report_with_next(serde_json::json!([{"id": "x"}]));
        assert!(parse_next_subtasks(&r2).is_err());
    }

    // --- seeds_to_subtasks (pure mapping) -----------------------------------

    #[test]
    fn harvested_seeds_carry_parent_dep_and_produced_by_and_idx() {
        let run_id = Uuid::new_v4();
        let seeds = vec![
            OrgChartSeed {
                id: "build-1".to_string(),
                title: "B1".to_string(),
                brief: "b".to_string(),
                phase: "implement".to_string(),
                depends_on: vec![],
                expected_output: "x".to_string(),
                repo: None,
                emits_subtasks: false,
            },
            OrgChartSeed {
                id: "build-2".to_string(),
                title: "B2".to_string(),
                brief: "b".to_string(),
                phase: "implement".to_string(),
                depends_on: vec!["build-1".to_string()],
                expected_output: "x".to_string(),
                repo: None,
                emits_subtasks: false,
            },
        ];
        // Parent "spec" already at idx 0..=4 in the run; start_idx = 5.
        let rows = seeds_to_subtasks(run_id, &seeds, Some("spec"), 5);
        assert_eq!(rows.len(), 2);

        // produced_by = parent on every row.
        assert_eq!(rows[0].produced_by.as_deref(), Some("spec"));
        assert_eq!(rows[1].produced_by.as_deref(), Some("spec"));

        // depends_on contains the parent task_id.
        assert!(
            rows[0].depends_on.contains(&"spec".to_string()),
            "parent dep wired"
        );
        assert!(rows[1].depends_on.contains(&"spec".to_string()));
        // and preserves the seed's declared cross-edge.
        assert!(rows[1].depends_on.contains(&"build-1".to_string()));

        // idx after the parent / sequential.
        assert_eq!(rows[0].idx, 5);
        assert_eq!(rows[1].idx, 6);

        // Submitted, no run id, no artifact on a fresh splice.
        assert_eq!(rows[0].state, SubtaskState::Submitted);
        assert!(rows[0].task_run_id.is_none());
        assert!(rows[0].artifact.is_none());
        assert_eq!(rows[0].run_id, run_id);
    }

    #[test]
    fn design_seeds_have_null_produced_by_and_no_injected_dep() {
        let run_id = Uuid::new_v4();
        let seeds = vec![OrgChartSeed {
            id: "spec".to_string(),
            title: "Comprehend".to_string(),
            brief: "read the website".to_string(),
            phase: "plan".to_string(),
            depends_on: vec![],
            expected_output: "a functional spec".to_string(),
            repo: None,
            emits_subtasks: true,
        }];
        // DESIGN bootstrap: produced_by = None, idx from 0.
        let rows = seeds_to_subtasks(run_id, &seeds, None, 0);
        assert_eq!(rows.len(), 1);
        assert!(
            rows[0].produced_by.is_none(),
            "DESIGN-origin rows have null produced_by"
        );
        assert!(
            rows[0].depends_on.is_empty(),
            "no parent dep injected for DESIGN"
        );
        assert!(rows[0].emits_subtasks, "elaborator flag carried from seed");
        assert_eq!(rows[0].idx, 0);
    }

    #[test]
    fn parent_dep_not_duplicated_when_seed_already_names_it() {
        let run_id = Uuid::new_v4();
        let seeds = vec![OrgChartSeed {
            id: "c".to_string(),
            title: "C".to_string(),
            brief: "b".to_string(),
            phase: "implement".to_string(),
            depends_on: vec!["spec".to_string()], // already names the parent
            expected_output: "x".to_string(),
            repo: None,
            emits_subtasks: false,
        }];
        let rows = seeds_to_subtasks(run_id, &seeds, Some("spec"), 1);
        let spec_count = rows[0].depends_on.iter().filter(|d| *d == "spec").count();
        assert_eq!(spec_count, 1, "parent dep must not be duplicated");
    }
}
