//! Tauri commands for DAG workflow operations.

use std::path::PathBuf;
use std::sync::Arc;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tauri::State;

use super::AppState;

/// Validate a YAML DAG workflow definition without executing it.
///
/// Returns validation errors if any, or a success confirmation with node count.
#[tauri::command]
pub async fn validate_dag_workflow(
    yaml_content: String,
    _state: State<'_, Arc<AppState>>,
) -> Result<serde_json::Value, String> {
    use crate::workflow::dag_parser::parse_dag_workflow;

    match parse_dag_workflow(&yaml_content) {
        Ok(def) => Ok(serde_json::json!({
            "valid": true,
            "name": def.name,
            "node_count": def.nodes.len(),
            "description": def.description,
        })),
        Err(e) => Ok(serde_json::json!({
            "valid": false,
            "error": format!("{}", e),
        })),
    }
}

/// Import a single YAML workflow file into the database.
#[tauri::command]
pub async fn import_dag_workflow(
    file_path: String,
    state: State<'_, Arc<AppState>>,
) -> Result<serde_json::Value, String> {
    use crate::workflow::dag_sync::import_workflow_file;

    let path = PathBuf::from(&file_path);
    if !path.exists() {
        return Err(format!("File not found: {}", file_path));
    }

    let result = import_workflow_file(&path, &state.pg_db).await;

    if let Some(ref error) = result.error {
        Err(error.clone())
    } else {
        Ok(serde_json::json!({
            "workflow_id": result.workflow_id,
            "workflow_name": result.workflow_name,
            "was_updated": result.was_updated,
        }))
    }
}

/// Import all YAML workflows from a project's .qontinui/workflows/ directory.
#[tauri::command]
pub async fn import_dag_workflows_from_project(
    project_path: String,
    state: State<'_, Arc<AppState>>,
) -> Result<serde_json::Value, String> {
    use crate::workflow::dag_sync::{import_workflows_from_directory, workflow_directory};

    let dir = workflow_directory(&PathBuf::from(&project_path));
    if !dir.exists() {
        return Ok(serde_json::json!({
            "imported": 0,
            "results": [],
            "message": format!("No workflow directory found at {}", dir.display()),
        }));
    }

    let results = import_workflows_from_directory(&dir, &state.pg_db).await;
    let imported = results.iter().filter(|r| r.error.is_none()).count();
    let failed = results.iter().filter(|r| r.error.is_some()).count();

    Ok(serde_json::json!({
        "imported": imported,
        "failed": failed,
        "results": results.iter().map(|r| serde_json::json!({
            "file": r.file_path,
            "name": r.workflow_name,
            "id": r.workflow_id,
            "was_updated": r.was_updated,
            "error": r.error,
        })).collect::<Vec<_>>(),
    }))
}

/// Export a workflow as YAML.
#[tauri::command]
pub async fn export_dag_workflow(
    workflow_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<String, String> {
    use crate::workflow::dag_sync::export_workflow_as_yaml;
    export_workflow_as_yaml(&workflow_id, &state.pg_db).await
}

/// Respond to a pending DAG approval gate.
///
/// The frontend invokes this command from the `ApprovalDialog` component after
/// the user clicks Approve or Reject. The approval_id matches the one emitted
/// with the `dag:approval-requested` event.
#[tauri::command]
pub async fn respond_dag_approval(
    approval_id: String,
    approved: bool,
    _state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    crate::step_executor::handlers::dag_nodes::resolve_approval(&approval_id, approved)
}

/// Build the Tauri plugin that registers this module's command handlers.
///
/// See `commands/mod.rs` for the migration guide explaining the plugin pattern.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_dag_workflows")
        .invoke_handler(tauri::generate_handler![
            validate_dag_workflow,
            import_dag_workflow,
            import_dag_workflows_from_project,
            export_dag_workflow,
            respond_dag_approval,
        ])
        .build()
}
