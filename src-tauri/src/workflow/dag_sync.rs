//! YAML workflow file synchronization.
//!
//! Provides import/export between `.qontinui/workflows/*.yaml` files and
//! the `unified_workflows` database table.
//!
//! This module is intentionally on-demand (no background watcher). Call
//! `import_workflows_from_directory` at startup or on user request.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{error, info, warn};

use crate::database::pg::PgDb;
use crate::workflow::dag_parser::parse_dag_workflow;
use crate::workflow::dag_schema::DagWorkflowDef;

// ─────────────────────────────────────────────────────────────────────────────
// Public types
// ─────────────────────────────────────────────────────────────────────────────

/// Result of importing a single YAML workflow file.
#[derive(Debug, Clone)]
pub struct ImportResult {
    pub file_path: String,
    pub workflow_name: String,
    pub workflow_id: String,
    /// `true` if an existing workflow was updated; `false` if a new one was created.
    pub was_updated: bool,
    /// Non-`None` when the import failed; contains a human-readable error message.
    pub error: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Directory scan + import
// ─────────────────────────────────────────────────────────────────────────────

/// Scan a directory for `*.yaml` / `*.yml` workflow files and import each one.
///
/// Returns one [`ImportResult`] per file found (including failures).
/// Missing or unreadable directories return an empty `Vec` (a warning is logged).
pub async fn import_workflows_from_directory(dir: &Path, pg_db: &Arc<PgDb>) -> Vec<ImportResult> {
    let read_dir = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) => {
            warn!(
                "dag_sync: cannot read workflow directory '{}': {}",
                dir.display(),
                e
            );
            return vec![];
        }
    };

    let mut results = Vec::new();

    for entry in read_dir.flatten() {
        let path = entry.path();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();

        if ext != "yaml" && ext != "yml" {
            continue;
        }

        results.push(import_workflow_file(&path, pg_db).await);
    }

    results
}

/// Import a single YAML workflow file into the database.
///
/// * If a workflow with the same **name** already exists the row is updated and
///   `was_updated = true`.
/// * Otherwise a new row is inserted and `was_updated = false`.
/// * The raw YAML source is stored in the `source_yaml` column.
pub async fn import_workflow_file(file_path: &Path, pg_db: &Arc<PgDb>) -> ImportResult {
    let path_str = file_path.display().to_string();

    // 1. Read file
    let yaml_content = match std::fs::read_to_string(file_path) {
        Ok(s) => s,
        Err(e) => {
            let msg = format!("Failed to read '{}': {}", path_str, e);
            error!("dag_sync: {}", msg);
            return ImportResult {
                file_path: path_str,
                workflow_name: String::new(),
                workflow_id: String::new(),
                was_updated: false,
                error: Some(msg),
            };
        }
    };

    // 2. Parse YAML
    let dag_def = match parse_dag_workflow(&yaml_content) {
        Ok(d) => d,
        Err(e) => {
            let msg = format!("Parse error in '{}': {}", path_str, e);
            warn!("dag_sync: {}", msg);
            return ImportResult {
                file_path: path_str,
                workflow_name: String::new(),
                workflow_id: String::new(),
                was_updated: false,
                error: Some(msg),
            };
        }
    };

    let workflow_name = dag_def.name.clone();

    // 3. Check if a workflow with this name already exists
    let existing = match pg_db.get_unified_workflow_by_name(&workflow_name).await {
        Ok(opt) => opt,
        Err(e) => {
            let msg = format!("DB lookup failed for '{}': {}", workflow_name, e);
            error!("dag_sync: {}", msg);
            return ImportResult {
                file_path: path_str,
                workflow_name,
                workflow_id: String::new(),
                was_updated: false,
                error: Some(msg),
            };
        }
    };

    // 4. Derive a stable ID
    let workflow_id = existing
        .as_ref()
        .map(|w| w.id.clone())
        .unwrap_or_else(|| slugify(&workflow_name));

    let was_updated = existing.is_some();

    // 5. Build the minimal JSON representations for the step columns.
    //    For pure DAG workflows imported from YAML the "phases" don't map cleanly,
    //    so we store an empty JSON array and rely on source_yaml for re-execution.
    let description = dag_def.description.clone().unwrap_or_default();
    // YAML-imported DAG workflows keep whatever cap the YAML specified;
    // missing = unlimited (None) to match the wider convention.
    let max_iterations: Option<i64> = dag_def.settings.max_iterations.map(|v| v as i64);

    // 6. Upsert via raw SQL (pool is public via PgDb::pool())
    let upsert_result = pg_db
        .upsert_workflow_from_yaml(
            &workflow_id,
            &workflow_name,
            &description,
            &yaml_content,
            max_iterations,
        )
        .await;

    match upsert_result {
        Ok(_) => {
            if was_updated {
                info!(
                    "dag_sync: updated workflow '{}' ({})",
                    workflow_name, workflow_id
                );
            } else {
                info!(
                    "dag_sync: imported new workflow '{}' ({})",
                    workflow_name, workflow_id
                );
            }
            ImportResult {
                file_path: path_str,
                workflow_name,
                workflow_id,
                was_updated,
                error: None,
            }
        }
        Err(e) => {
            let msg = format!("DB upsert failed for '{}': {}", workflow_name, e);
            error!("dag_sync: {}", msg);
            ImportResult {
                file_path: path_str,
                workflow_name,
                workflow_id,
                was_updated,
                error: Some(msg),
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Export
// ─────────────────────────────────────────────────────────────────────────────

/// Export a workflow from the database as a YAML string.
///
/// * If the workflow row has a non-empty `source_yaml` column the stored YAML
///   is returned verbatim (round-trip safe).
/// * Otherwise a minimal `DagWorkflowDef` is constructed from the workflow
///   metadata and serialised.
pub async fn export_workflow_as_yaml(
    workflow_id: &str,
    pg_db: &Arc<PgDb>,
) -> Result<String, String> {
    // 1. Fetch the source_yaml directly (may not be in UnifiedWorkflow struct)
    let conn = pg_db
        .pool()
        .get()
        .await
        .map_err(|e| format!("PG pool error: {}", e))?;

    let row = conn
        .query_opt(
            "SELECT source_yaml, name, description, max_iterations \
             FROM unified_workflows WHERE id = $1 AND deleted_at IS NULL",
            &[&workflow_id],
        )
        .await
        .map_err(|e| format!("PG query error: {}", e))?
        .ok_or_else(|| format!("Workflow not found: {}", workflow_id))?;

    let source_yaml: Option<String> = row.get(0);
    let name: String = row.get(1);
    let description: Option<String> = row.get(2);
    let max_iterations: i64 = row.get(3);

    // 2. Return stored YAML if available
    if let Some(yaml) = source_yaml.filter(|s| !s.is_empty()) {
        return Ok(yaml);
    }

    // 3. Build a minimal DagWorkflowDef from DB fields.
    //    We emit a single prompt node as a placeholder so the YAML is valid
    //    (at least one node required by the validator).
    use crate::workflow::dag_schema::{DagNodeDef, NodeDefaults, TriggerRule, WorkflowSettings};
    use std::collections::HashMap;

    let placeholder_node = DagNodeDef {
        name: Some(name.clone()),
        depends_on: vec![],
        when: None,
        trigger_rule: TriggerRule::AllSuccess,
        retry: None,
        timeout_seconds: None,
        context: None,
        prompt: Some(format!(
            "# Auto-generated placeholder for workflow '{}'\n\
             # Edit this file and re-import to define actual steps.",
            name
        )),
        prompt_mode: None,
        command: None,
        working_directory: None,
        fail_on_error: None,
        check_type: None,
        check_group_id: None,
        ui_bridge_action: None,
        ui_bridge_url: None,
        ui_bridge_instruction: None,
        a11y_action: None,
        a11y_target: None,
        loop_body: None,
        until: None,
        until_bash: None,
        max_loop_iterations: None,
        commit_interval: None,
        approval: None,
        workflow_ref: None,
        workflow_inputs: None,
        cancel_reason: None,
        inputs: None,
        extract: None,
    };

    let mut nodes = HashMap::new();
    nodes.insert("step_1".to_string(), placeholder_node);

    let def = DagWorkflowDef {
        name,
        description,
        version: Some("1".to_string()),
        nodes,
        defaults: NodeDefaults::default(),
        settings: WorkflowSettings {
            max_iterations: Some(max_iterations as u32),
            ..WorkflowSettings::default()
        },
    };

    // 4. Serialise to YAML
    serde_yaml::to_string(&def).map_err(|e| format!("YAML serialization error: {}", e))
}

// ─────────────────────────────────────────────────────────────────────────────
// Utility helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Generate a stable workflow ID from a name (lowercase, hyphen-separated).
///
/// ```
/// # use qontinui_runner::workflow::dag_sync::slugify; // example only
/// assert_eq!(slugify("My Workflow! v2"), "my-workflow-v2");
/// ```
pub fn slugify(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Return the conventional workflow directory for a project root.
///
/// The directory is `<project_path>/.qontinui/workflows`.
pub fn workflow_directory(project_path: &Path) -> PathBuf {
    project_path.join(".qontinui").join("workflows")
}
