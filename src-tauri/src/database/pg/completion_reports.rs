//! The typed completion report a worker hands back when it finishes a
//! subtask — the wire shape consumed by the Conductor
//! (`orchestration_loop::{conductor, ledger, org_chart}`,
//! `database/pg/orchestration.rs`) and submitted through the runner MCP tool
//! `mcp::orchestration_report` (`POST /orchestration/report-subtask`).
//!
//! Originally Phase 1 of the productivity-coordinator-completion-reports plan
//! (§2 "Rust schema"). The PG helpers that lived beside these types —
//! `project.tasks.completion_report` writes, the dependency-cycle walk and the
//! assignment-brief extras — served only the Productivity scheduler and the
//! plan/task board, and were deleted with them by Phase 4 of
//! `2026-09-12-consolidate-local-orchestration-onto-conductor`. What remains
//! is the report shape itself; validation of a submitted report is the
//! submitting route's concern.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

// ============================================================================
// Wire types — match the §2 "Rust schema" exactly.
// ============================================================================

/// Typed handoff payload a worker returns for a finished subtask.
///
/// Workers submit one via `POST /orchestration/report-subtask`; the Conductor
/// reads it back off the orchestration ledger.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CompletionReport {
    /// Human-readable narrative. Markdown. Acts as the "executive summary"
    /// the Conductor prepends to downstream briefs verbatim.
    pub summary_md: String,

    /// What the upstream produced. Each entry is a typed pointer.
    pub deliverables: Vec<Deliverable>,

    /// What the upstream broke or changed in a way that affects downstream
    /// work. Empty vec is fine; absence implies "no breaking changes claimed"
    /// but does NOT prove safety.
    pub breaking_changes: Vec<BreakingChange>,

    /// Loose ends. Each carries a `blocking_for_dependents` boolean — when
    /// true, dependents must not be started until a human decides.
    pub follow_ups: Vec<FollowUp>,

    /// Open extension point. Source-specific fields land here. The Conductor
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
    /// Conductor-side dispatching.
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

    /// 'critical' | 'important' | 'nice-to-have'. The Conductor filters on
    /// this for escalation thresholds.
    pub priority: String,

    /// When true, dependents must not be started until the user decides
    /// whether to override. Default false.
    #[serde(default)]
    pub blocking_for_dependents: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
