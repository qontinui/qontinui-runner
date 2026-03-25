//! Generation pipeline instrumentation.
//!
//! Provides functions to emit pipeline events, create workflow versions,
//! and track step provenance during workflow generation. These functions
//! are called from hook points in generator.rs.

use crate::database::graph_ops;
use rusqlite::{params, Connection};
use serde_json::Value;
use tracing::{info, warn};

/// Record a generation pipeline event with timing and validation data.
/// Called at the end of each pipeline phase (discovery, builder, fixer, hardener, etc.).
/// Silently logs warnings on failure — pipeline events are telemetry, not critical path.
pub fn emit_pipeline_event(
    conn: &Connection,
    task_run_id: &str,
    workflow_id: Option<&str>,
    event_type: &str,
    phase: &str,
    iteration: Option<i32>,
    payload: Option<&Value>,
    duration_ms: Option<u64>,
    token_count: Option<u64>,
    errors_before: Option<i32>,
    errors_after: Option<i32>,
) {
    let payload_str = payload.map(|v| serde_json::to_string(v).unwrap_or_default());
    match graph_ops::insert_pipeline_event(
        conn,
        task_run_id,
        workflow_id,
        event_type,
        Some(phase),
        iteration,
        payload_str.as_deref(),
        duration_ms.map(|d| d as i64),
        token_count.map(|t| t as i64),
        errors_before,
        errors_after,
    ) {
        Ok(id) => info!("Pipeline event recorded: {} phase={} id={}", event_type, phase, id),
        Err(e) => warn!("Failed to record pipeline event: {}", e),
    }
}

/// Create a new workflow version snapshot, computing diff from the previous version.
/// Returns the version ID on success, or None if versioning fails (non-fatal).
pub fn create_workflow_version(
    conn: &Connection,
    workflow_id: &str,
    generation_task_run_id: Option<&str>,
    workflow_json: &Value,
    trigger: &str,
) -> Option<String> {
    // 1. Get the latest existing version for this workflow
    let latest = graph_ops::get_latest_workflow_version(conn, workflow_id)
        .ok()
        .flatten();

    // 2. Compute version number (latest + 1 or 1)
    let version_number = latest
        .as_ref()
        .map(|v| v.version_number + 1)
        .unwrap_or(1);
    let parent_version_id = latest.as_ref().map(|v| v.id.as_str());

    // 3. Compute diff summary if there's a parent version
    let diff_summary = if let Some(ref parent) = latest {
        compute_diff_summary(
            &parent.workflow_json,
            &serde_json::to_string(workflow_json).unwrap_or_default(),
        )
    } else {
        Some("Initial version".to_string())
    };

    // 4. Serialize workflow JSON
    let workflow_json_str = serde_json::to_string(workflow_json).unwrap_or_default();

    // 5. Insert
    match graph_ops::insert_workflow_version(
        conn,
        workflow_id,
        version_number,
        parent_version_id,
        generation_task_run_id,
        &workflow_json_str,
        diff_summary.as_deref(),
        None, // diff_json computed on demand
        trigger,
    ) {
        Ok(id) => {
            info!(
                "Created workflow version {} (v{}) for {}",
                id, version_number, workflow_id
            );
            Some(id)
        }
        Err(e) => {
            warn!("Failed to create workflow version: {}", e);
            None
        }
    }
}

/// Record provenance for all steps in a workflow's phase arrays.
/// Iterates over setup_steps, verification_steps, agentic_steps, completion_steps
/// and creates a step_provenance entry for each.
/// Returns count of entries created.
pub fn record_all_step_provenance(
    conn: &Connection,
    workflow_id: &str,
    version_id: Option<&str>,
    workflow_json: &Value,
    agent: &str,
    iteration: Option<i32>,
) -> u32 {
    let mut count = 0u32;
    let phases = [
        "setup_steps",
        "verification_steps",
        "agentic_steps",
        "completion_steps",
    ];
    let phase_names = ["setup", "verification", "agentic", "completion"];

    for (phase_key, phase_name) in phases.iter().zip(phase_names.iter()) {
        if let Some(steps) = workflow_json.get(phase_key).and_then(|v| v.as_array()) {
            for (i, step) in steps.iter().enumerate() {
                let step_name = step
                    .get("name")
                    .or_else(|| step.get("id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unnamed");

                let step_json_str = serde_json::to_string(step).unwrap_or_default();

                if let Err(e) = graph_ops::insert_step_provenance(
                    conn,
                    workflow_id,
                    version_id,
                    step_name,
                    i as i32,
                    phase_name,
                    agent,
                    iteration,
                    None, // original_step_json — only set for fixer modifications
                    Some(&step_json_str),
                ) {
                    warn!("Failed to record step provenance for {}: {}", step_name, e);
                } else {
                    count += 1;
                }
            }
        }
    }

    if count > 0 {
        info!(
            "Recorded {} step provenance entries for workflow {} agent={}",
            count, workflow_id, agent
        );
    }
    count
}

/// After a fixer iteration, diff old and new steps and record provenance for changed steps.
/// Compares steps by index within each phase. If a step changed, records with original_step_json.
pub fn record_fixer_provenance(
    conn: &Connection,
    workflow_id: &str,
    version_id: Option<&str>,
    old_workflow: &Value,
    new_workflow: &Value,
    iteration: i32,
) -> u32 {
    let mut count = 0u32;
    let phases = [
        "setup_steps",
        "verification_steps",
        "agentic_steps",
        "completion_steps",
    ];
    let phase_names = ["setup", "verification", "agentic", "completion"];

    for (phase_key, phase_name) in phases.iter().zip(phase_names.iter()) {
        let old_steps = old_workflow.get(phase_key).and_then(|v| v.as_array());
        let new_steps = new_workflow.get(phase_key).and_then(|v| v.as_array());

        if let (Some(old), Some(new)) = (old_steps, new_steps) {
            for (i, new_step) in new.iter().enumerate() {
                let old_step = old.get(i);
                let changed = old_step.map(|o| o != new_step).unwrap_or(true);

                if changed {
                    let step_name = new_step
                        .get("name")
                        .or_else(|| new_step.get("id"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("unnamed");

                    let old_json =
                        old_step.map(|s| serde_json::to_string(s).unwrap_or_default());
                    let new_json = serde_json::to_string(new_step).unwrap_or_default();

                    if let Err(e) = graph_ops::insert_step_provenance(
                        conn,
                        workflow_id,
                        version_id,
                        step_name,
                        i as i32,
                        phase_name,
                        "fixer",
                        Some(iteration),
                        old_json.as_deref(),
                        Some(&new_json),
                    ) {
                        warn!(
                            "Failed to record fixer provenance for step {}: {}",
                            step_name, e
                        );
                    } else {
                        count += 1;
                    }
                }
            }
        }
    }

    if count > 0 {
        info!(
            "Fixer iteration {} changed {} steps in workflow {}",
            iteration, count, workflow_id
        );
    }
    count
}

/// Record rule influence entries for all rules that were loaded during generation.
pub fn record_rule_influences(
    conn: &Connection,
    rules: &[(String, String, String)], // Vec of (rule_id, agent, section)
    task_run_id: &str,
    workflow_id: Option<&str>,
    phase: &str,
) {
    for (rule_id, _agent, _section) in rules {
        if let Err(e) = graph_ops::insert_rule_influence(
            conn,
            rule_id,
            task_run_id,
            workflow_id,
            "loaded",
            None,
            Some(phase),
        ) {
            warn!("Failed to record rule influence for {}: {}", rule_id, e);
        }
    }
}

/// Build graph-informed context for the workflow generator.
/// Queries the knowledge graph for:
/// 1. Active cross-run patterns for this workflow (warnings about known issues)
/// 2. Ineffective rules to exclude from the prompt
/// 3. Phase stats showing which phases are bottlenecks
/// Returns a formatted string to include in the builder prompt.
pub fn build_graph_context(conn: &Connection, workflow_name: Option<&str>) -> String {
    let mut sections = Vec::new();

    // 1. Active cross-run patterns
    if let Ok(patterns) =
        crate::database::cross_run_ops::get_active_patterns(conn, workflow_name)
    {
        if !patterns.is_empty() {
            let mut pattern_section = String::from(
                "## Known Cross-Run Patterns\n\
                 These patterns have been detected across multiple runs. Account for them:\n",
            );
            for p in patterns.iter().take(5) {
                pattern_section.push_str(&format!(
                    "- [{}] {} (occurred {} times, status: {})\n",
                    p.pattern_type, p.signature_hash, p.occurrence_count, p.status
                ));
            }
            sections.push(pattern_section);
        }
    }

    // 2. Ineffective rules to warn about
    if let Ok(ineffective) = graph_ops::get_ineffective_rules(conn, 3) {
        if !ineffective.is_empty() {
            let mut rule_section = String::from(
                "## Ineffective Generation Rules (auto-detected)\n\
                 These rules have not prevented any errors after multiple loads. They may add noise:\n",
            );
            for r in ineffective.iter().take(5) {
                rule_section.push_str(&format!(
                    "- Rule {} (loaded {}x, no effect {}x, prevented {}x)\n",
                    r.rule_id, r.total_loaded_count, r.no_effect_count, r.prevented_error_count
                ));
            }
            sections.push(rule_section);
        }
    }

    // 3. Phase stats for bottleneck awareness
    if let Some(wn) = workflow_name {
        // Get workflow ID from name
        let wf_id: Option<String> = conn
            .query_row(
                "SELECT id FROM unified_workflows WHERE name = ?1 ORDER BY updated_at DESC LIMIT 1",
                rusqlite::params![wn],
                |row| row.get::<_, String>(0),
            )
            .ok();

        if let Some(ref wf_id) = wf_id {
            if let Ok(stats) = graph_ops::get_phase_stats(conn, wf_id) {
                if !stats.is_empty() {
                    let mut stats_section = String::from(
                        "## Pipeline Phase Performance\n\
                         Historical stats for this workflow's generation pipeline:\n",
                    );
                    for s in &stats {
                        let avg_dur = s
                            .avg_duration_ms
                            .map(|d| format!("{:.0}ms", d))
                            .unwrap_or_else(|| "-".to_string());
                        stats_section.push_str(&format!(
                            "- {}: avg {}; {} tokens; {} runs\n",
                            s.phase, avg_dur, s.total_token_count, s.event_count
                        ));
                    }
                    sections.push(stats_section);
                }
            }
        }
    }

    if sections.is_empty() {
        String::new()
    } else {
        sections.join("\n")
    }
}

/// Compute a human-readable diff summary between two workflow JSON strings.
fn compute_diff_summary(old_json: &str, new_json: &str) -> Option<String> {
    let old: Value = serde_json::from_str(old_json).ok()?;
    let new: Value = serde_json::from_str(new_json).ok()?;

    let mut changes = Vec::new();

    let phases = [
        "setup_steps",
        "verification_steps",
        "agentic_steps",
        "completion_steps",
    ];
    for phase in &phases {
        let old_count = old
            .get(phase)
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        let new_count = new
            .get(phase)
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        if old_count != new_count {
            changes.push(format!("{}: {} -> {} steps", phase, old_count, new_count));
        }
    }

    // Check top-level field changes
    for key in ["max_iterations", "reflection_mode", "workflow_architecture"] {
        if old.get(key) != new.get(key) {
            changes.push(format!("{} changed", key));
        }
    }

    if changes.is_empty() {
        Some("No structural changes".to_string())
    } else {
        Some(changes.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use serde_json::json;

    /// Create an in-memory SQLite database with all tables needed by instrumentation.
    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE unified_workflows (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                description TEXT DEFAULT '',
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE task_runs (
                id TEXT PRIMARY KEY,
                task_name TEXT NOT NULL DEFAULT '',
                task_type TEXT NOT NULL DEFAULT 'task',
                status TEXT NOT NULL DEFAULT 'running',
                sessions_count INTEGER NOT NULL DEFAULT 0,
                auto_continue BOOLEAN NOT NULL DEFAULT 1,
                output_log TEXT DEFAULT '',
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE workflow_versions (
                id TEXT PRIMARY KEY,
                workflow_id TEXT NOT NULL,
                version_number INTEGER NOT NULL,
                parent_version_id TEXT,
                generation_task_run_id TEXT,
                workflow_json TEXT NOT NULL,
                diff_summary TEXT,
                diff_json TEXT,
                trigger TEXT NOT NULL DEFAULT 'manual',
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                UNIQUE(workflow_id, version_number)
            );

            CREATE TABLE step_provenance (
                id TEXT PRIMARY KEY,
                workflow_id TEXT NOT NULL,
                workflow_version_id TEXT,
                step_name TEXT NOT NULL,
                step_index INTEGER NOT NULL,
                phase TEXT NOT NULL,
                generating_agent TEXT NOT NULL,
                generation_iteration INTEGER,
                original_step_json TEXT,
                final_step_json TEXT,
                ui_bridge_event_ids TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE generation_pipeline_events (
                id TEXT PRIMARY KEY,
                task_run_id TEXT NOT NULL,
                workflow_id TEXT,
                event_type TEXT NOT NULL,
                phase TEXT,
                iteration INTEGER,
                payload TEXT,
                duration_ms INTEGER,
                token_count INTEGER,
                validation_errors_before INTEGER,
                validation_errors_after INTEGER,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE generation_rules (
                id TEXT PRIMARY KEY,
                agent TEXT NOT NULL,
                section TEXT NOT NULL,
                rule_number INTEGER NOT NULL,
                title TEXT NOT NULL,
                content TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'active',
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE rule_influence_log (
                id TEXT PRIMARY KEY,
                rule_id TEXT NOT NULL,
                task_run_id TEXT NOT NULL,
                workflow_id TEXT,
                influence_type TEXT NOT NULL DEFAULT 'loaded',
                evidence TEXT,
                phase TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            "#,
        )
        .unwrap();
        conn
    }

    #[test]
    fn test_emit_pipeline_event() {
        let conn = setup_test_db();
        let payload = json!({"step": "login", "selector": "#btn"});

        emit_pipeline_event(
            &conn,
            "tr-1",
            Some("wf-1"),
            "phase_complete",
            "builder",
            Some(1),
            Some(&payload),
            Some(1500),
            Some(4200),
            Some(3),
            Some(1),
        );

        // Verify the event was inserted
        let events = graph_ops::get_pipeline_events(&conn, "tr-1").unwrap();
        assert_eq!(events.len(), 1);

        let evt = &events[0];
        assert_eq!(evt.task_run_id, "tr-1");
        assert_eq!(evt.workflow_id, Some("wf-1".to_string()));
        assert_eq!(evt.event_type, "phase_complete");
        assert_eq!(evt.phase, Some("builder".to_string()));
        assert_eq!(evt.iteration, Some(1));
        assert_eq!(evt.duration_ms, Some(1500));
        assert_eq!(evt.token_count, Some(4200));
        assert_eq!(evt.validation_errors_before, Some(3));
        assert_eq!(evt.validation_errors_after, Some(1));
        // Payload should be serialized JSON
        assert!(evt.payload.as_ref().unwrap().contains("login"));
    }

    #[test]
    fn test_create_workflow_version_first() {
        let conn = setup_test_db();
        let wf_json = json!({"setup_steps": [], "verification_steps": []});

        let version_id = create_workflow_version(
            &conn,
            "wf-first",
            Some("tr-1"),
            &wf_json,
            "generation",
        );

        assert!(version_id.is_some());

        // Verify version_number = 1
        let versions = graph_ops::get_workflow_versions(&conn, "wf-first").unwrap();
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].version_number, 1);
        assert!(versions[0].parent_version_id.is_none());
        assert_eq!(versions[0].diff_summary, Some("Initial version".to_string()));
        assert_eq!(versions[0].trigger, "generation");
    }

    #[test]
    fn test_create_workflow_version_increments() {
        let conn = setup_test_db();
        let wf_json_v1 = json!({"setup_steps": [{"name": "step1"}], "verification_steps": []});
        let wf_json_v2 = json!({"setup_steps": [{"name": "step1"}, {"name": "step2"}], "verification_steps": []});

        // Create first version
        let v1_id = create_workflow_version(
            &conn,
            "wf-inc",
            Some("tr-1"),
            &wf_json_v1,
            "generation",
        );
        assert!(v1_id.is_some());

        // Create second version
        let v2_id = create_workflow_version(
            &conn,
            "wf-inc",
            Some("tr-2"),
            &wf_json_v2,
            "fixer",
        );
        assert!(v2_id.is_some());

        // Verify versions
        let versions = graph_ops::get_workflow_versions(&conn, "wf-inc").unwrap();
        assert_eq!(versions.len(), 2);

        // Versions are returned DESC, so index 0 is the latest
        let v2 = &versions[0];
        let v1 = &versions[1];

        assert_eq!(v2.version_number, 2);
        assert_eq!(v1.version_number, 1);
        assert_eq!(v2.parent_version_id, Some(v1.id.clone()));
        assert_eq!(v2.trigger, "fixer");

        // Diff summary should note the setup_steps change (1 -> 2)
        let summary = v2.diff_summary.as_ref().unwrap();
        assert!(
            summary.contains("setup_steps"),
            "Expected diff summary to mention setup_steps, got: {}",
            summary
        );
    }

    #[test]
    fn test_record_all_step_provenance() {
        let conn = setup_test_db();
        let workflow_json = json!({
            "setup_steps": [
                {"name": "open_browser", "type": "navigate"},
                {"name": "login", "type": "action"}
            ],
            "verification_steps": [
                {"name": "check_dashboard", "type": "verify"}
            ],
            "agentic_steps": [],
            "completion_steps": []
        });

        let count = record_all_step_provenance(
            &conn,
            "wf-prov",
            Some("wv-1"),
            &workflow_json,
            "builder",
            Some(1),
        );

        // 2 setup + 1 verification = 3 entries
        assert_eq!(count, 3);

        // Verify entries exist in the database
        let provs = graph_ops::get_provenance_for_workflow(&conn, "wf-prov").unwrap();
        assert_eq!(provs.len(), 3);

        // Check phases and names
        let setup_provs: Vec<_> = provs.iter().filter(|p| p.phase == "setup").collect();
        let verification_provs: Vec<_> = provs.iter().filter(|p| p.phase == "verification").collect();
        assert_eq!(setup_provs.len(), 2);
        assert_eq!(verification_provs.len(), 1);

        assert_eq!(setup_provs[0].step_name, "open_browser");
        assert_eq!(setup_provs[1].step_name, "login");
        assert_eq!(verification_provs[0].step_name, "check_dashboard");

        // All should have generating_agent = "builder"
        for p in &provs {
            assert_eq!(p.generating_agent, "builder");
            assert_eq!(p.generation_iteration, Some(1));
        }
    }

    #[test]
    fn test_record_fixer_provenance_only_changed() {
        let conn = setup_test_db();

        let old_workflow = json!({
            "setup_steps": [
                {"name": "open_browser", "url": "https://example.com"},
                {"name": "login", "selector": "#old-selector"},
                {"name": "navigate", "path": "/dashboard"}
            ],
            "verification_steps": [],
            "agentic_steps": [],
            "completion_steps": []
        });

        let new_workflow = json!({
            "setup_steps": [
                {"name": "open_browser", "url": "https://example.com"},
                {"name": "login", "selector": "#new-selector"},
                {"name": "navigate", "path": "/dashboard"}
            ],
            "verification_steps": [],
            "agentic_steps": [],
            "completion_steps": []
        });

        let count = record_fixer_provenance(
            &conn,
            "wf-fixer",
            Some("wv-2"),
            &old_workflow,
            &new_workflow,
            1,
        );

        // Only 1 step changed (login's selector changed)
        assert_eq!(count, 1);

        let provs = graph_ops::get_provenance_for_workflow(&conn, "wf-fixer").unwrap();
        assert_eq!(provs.len(), 1);

        let p = &provs[0];
        assert_eq!(p.step_name, "login");
        assert_eq!(p.generating_agent, "fixer");
        assert_eq!(p.generation_iteration, Some(1));
        // Should have original_step_json populated
        assert!(p.original_step_json.is_some());
        assert!(p.original_step_json.as_ref().unwrap().contains("#old-selector"));
        assert!(p.final_step_json.as_ref().unwrap().contains("#new-selector"));
    }

    #[test]
    fn test_compute_diff_summary() {
        let old_json = serde_json::to_string(&json!({
            "setup_steps": [{"name": "s1"}],
            "verification_steps": [{"name": "v1"}, {"name": "v2"}],
            "agentic_steps": [],
            "completion_steps": [],
            "max_iterations": 10
        }))
        .unwrap();

        let new_json = serde_json::to_string(&json!({
            "setup_steps": [{"name": "s1"}, {"name": "s2"}],
            "verification_steps": [{"name": "v1"}, {"name": "v2"}],
            "agentic_steps": [],
            "completion_steps": [],
            "max_iterations": 15
        }))
        .unwrap();

        let summary = compute_diff_summary(&old_json, &new_json);
        assert!(summary.is_some());

        let s = summary.unwrap();
        // Should detect setup_steps count change (1 -> 2)
        assert!(
            s.contains("setup_steps: 1 -> 2 steps"),
            "Expected setup_steps change, got: {}",
            s
        );
        // Should detect max_iterations change
        assert!(
            s.contains("max_iterations changed"),
            "Expected max_iterations changed, got: {}",
            s
        );
        // verification_steps did NOT change count
        assert!(
            !s.contains("verification_steps"),
            "Should not mention verification_steps, got: {}",
            s
        );
    }
}
