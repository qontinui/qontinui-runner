//! DAG workflow routing — determines whether a workflow should execute
//! via the DAG driver or the traditional 4-phase loop.
//!
//! Call [`check_dag_routing`] before setting up the traditional LoopController.
//! If it returns `Some(def)`, pass `def` to the DAG driver instead.
//!
//! ```ignore
//! if let Some(dag_def) = check_dag_routing(workflow_id, &pg_db).await {
//!     // execute via dag_driver::execute_dag_workflow(dag_def, deps)
//! } else {
//!     // traditional 4-phase loop
//! }
//! ```

use std::sync::Arc;

use tracing::info;

use crate::database::pg::PgDb;
use crate::workflow::dag_parser::parse_dag_workflow;
use crate::workflow::dag_schema::{DagWorkflowDef, TriggerRule};

/// Check if a workflow has DAG-specific YAML and should route to the DAG driver.
///
/// Returns `Some(DagWorkflowDef)` when the workflow's `source_yaml` parses
/// successfully as a DAG definition. Returns `None` when there is no
/// `source_yaml` or parsing fails, indicating the caller should fall back to
/// the traditional 4-phase loop.
///
/// # Detection heuristic
///
/// Any workflow that was authored as YAML is routed through the DAG executor.
/// Within that set, workflows that use DAG-specific node features (conditional
/// `when` clauses, non-default `trigger_rule`, `loop_body`, `approval`,
/// `cancel_reason`, or sub-`workflow_ref`) are also logged as explicitly
/// requiring the DAG executor so operators can observe the routing decision.
pub async fn check_dag_routing(
    workflow_id: &str,
    pg_db: &Arc<PgDb>,
) -> Option<DagWorkflowDef> {
    let source_yaml = get_source_yaml(workflow_id, pg_db).await?;

    let def = match parse_dag_workflow(&source_yaml) {
        Ok(def) => def,
        Err(e) => {
            tracing::warn!(
                workflow_id,
                "source_yaml present but failed to parse as DAG workflow — \
                 falling back to traditional loop: {}",
                e
            );
            return None;
        }
    };

    // Log whether any node uses DAG-specific features so the routing decision
    // is observable in traces without requiring extra tooling.
    let uses_dag_features = def.nodes.values().any(|node| {
        node.when.is_some()
            || node.trigger_rule != TriggerRule::AllSuccess
            || node.loop_body.is_some()
            || node.approval.is_some()
            || node.cancel_reason.is_some()
            || node.workflow_ref.is_some()
    });

    if uses_dag_features {
        info!(
            workflow_id,
            node_count = def.nodes.len(),
            "Routing to DAG executor (DAG-specific features detected)"
        );
    } else {
        info!(
            workflow_id,
            node_count = def.nodes.len(),
            "Routing to DAG executor (YAML-authored workflow)"
        );
    }

    Some(def)
}

/// Fetch just the `source_yaml` column for a workflow.
///
/// Returns `None` on any error or when the column is NULL, so callers can
/// treat both cases uniformly as "no DAG source available".
async fn get_source_yaml(workflow_id: &str, pg_db: &Arc<PgDb>) -> Option<String> {
    match pg_db.get_workflow_source_yaml(workflow_id).await {
        Ok(maybe_yaml) => maybe_yaml,
        Err(e) => {
            tracing::warn!(
                workflow_id,
                "Failed to fetch source_yaml for DAG routing check: {}",
                e
            );
            None
        }
    }
}
