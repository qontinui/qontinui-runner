//! Architecture Model Engine
//!
//! Aggregates component-level data from reflection fixes, causal events, and
//! knowledge into a queryable graph of components and their relationships.
//! Provides health scoring, impact analysis, and component detail queries.

use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};
use tracing::{info, warn};
use uuid::Uuid;

use crate::reflection::prediction;

// =============================================================================
// Types
// =============================================================================

#[derive(Debug, Clone, Serialize)]
pub struct ComponentNode {
    pub id: String,
    pub component_path: String,
    pub component_type: String,
    pub fix_count: u32,
    pub error_count: u32,
    pub causal_involvement_count: u32,
    pub effective_fix_count: u32,
    pub ineffective_fix_count: u32,
    pub health_score: f64,
    pub change_velocity: f64,
    pub last_activity_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComponentEdge {
    pub source: String,
    pub target: String,
    pub relationship_type: String,
    pub strength: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphStats {
    pub total_components: u32,
    pub total_relationships: u32,
    pub avg_health_score: f64,
    pub most_volatile: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComponentGraph {
    pub nodes: Vec<ComponentNode>,
    pub edges: Vec<ComponentEdge>,
    pub stats: GraphStats,
}

#[derive(Debug, Clone, Serialize)]
pub struct RebuildResult {
    pub components_count: u32,
    pub relationships_count: u32,
    pub workflow_name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FixSummary {
    pub id: String,
    pub fix_type: String,
    pub fix_description: String,
    pub effectiveness: Option<String>,
    pub applied_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImpactEntry {
    pub component_path: String,
    pub relationship_type: String,
    pub strength: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComponentDetails {
    pub node: ComponentNode,
    pub recent_fixes: Vec<FixSummary>,
    pub impacted_by: Vec<ImpactEntry>,
    pub impacts: Vec<ImpactEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImpactAnalysis {
    pub component: String,
    pub direct_impacts: Vec<ImpactEntry>,
    pub transitive_impacts: Vec<ImpactEntry>,
    pub total_impact_radius: u32,
    pub highest_risk_path: Vec<String>,
}

// =============================================================================
// Path Normalization
// =============================================================================

/// Normalize a raw file path: lowercase, forward slashes, strip `./` prefix and drive letters.
pub fn normalize_component_path(raw: &str) -> String {
    let mut path = raw.replace('\\', "/").to_lowercase();
    // Strip drive letters like C:/
    if path.len() >= 3 && path.as_bytes()[1] == b':' && path.as_bytes()[2] == b'/' {
        path = path[3..].to_string();
    }
    // Strip leading ./
    if let Some(stripped) = path.strip_prefix("./") {
        path = stripped.to_string();
    }
    path
}

/// Infer component type from a path.
pub fn infer_component_type(path: &str) -> &str {
    if path.contains("/services/") || path.contains("/service/") {
        return "service";
    }
    if path.ends_with('/') || !path.contains('.') {
        return "module";
    }
    "file"
}

// =============================================================================
// Rebuild
// =============================================================================

/// Full rebuild of the architecture model for a workflow.
///
/// Deletes existing data and re-extracts from reflection_fixes, causal_events,
/// and task_knowledge tables.
pub fn rebuild_architecture_model(
    workflow_name: &str,
) -> Result<RebuildResult, String> {
    Err("SQLite removed".to_string())
}

/// Internal accumulator for component data during rebuild.
#[derive(Default)]
struct ComponentData {
    fix_count: u32,
    error_count: u32,
    causal_involvement_count: u32,
    effective_fix_count: u32,
    ineffective_fix_count: u32,
    last_activity_at: Option<String>,
}

// =============================================================================
// Queries
// =============================================================================

/// Get the full component graph for a workflow.
pub fn get_component_graph(
    workflow_name: &str,
) -> Result<ComponentGraph, String> {
    Err("SQLite removed".to_string())
}

/// Get detailed info for a single component.
pub fn get_component_details(
    workflow_name: &str,
    component_path: &str,
) -> Result<ComponentDetails, String> {
    Err("SQLite removed".to_string())
}

/// BFS impact analysis from a component (max 3 hops).
pub fn get_impact_analysis(
    workflow_name: &str,
    component_path: &str,
) -> Result<ImpactAnalysis, String> {
    Err("SQLite removed".to_string())
}

// =============================================================================
// Graph-Enhanced Impact Analysis
// =============================================================================

/// Extended impact analysis that supplements the BFS-on-component_relationships approach
/// with additional relationship data from step_finding_links and step_provenance tables.
pub fn rebuild_architecture_with_graph(
    workflow_name: &str,
) -> Result<RebuildResult, String> {
    Err("SQLite removed".to_string())
}

/// Graph-enhanced impact analysis.
pub fn get_impact_analysis_with_graph(
    workflow_name: &str,
    component_path: &str,
) -> Result<ImpactAnalysis, String> {
    Err("SQLite removed".to_string())
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_test_db() -> Connection {
        panic!("SQLite tests disabled — use PG-based tests instead")
    }

    /// Insert a test task_run and return its ID.
    fn insert_test_run(id: &str, workflow_name: &str) {
        // SQLite removed - no-op
    }

    #[test]
    fn test_normalize_component_path() {
        assert_eq!(
            normalize_component_path("C:\\Users\\test\\src\\main.rs"),
            "users/test/src/main.rs"
        );
        assert_eq!(normalize_component_path("./src/lib.rs"), "src/lib.rs");
        assert_eq!(
            normalize_component_path("src/Auth/Middleware.rs"),
            "src/auth/middleware.rs"
        );
    }

    #[test]
    fn test_infer_component_type() {
        assert_eq!(infer_component_type("src/services/auth.rs"), "service");
        assert_eq!(infer_component_type("src/utils"), "module");
        assert_eq!(infer_component_type("src/main.rs"), "file");
    }

    #[test]
    fn test_rebuild_empty() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_rebuild_with_fixes() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_get_component_details() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_impact_analysis() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_health_score_computation() {
        // SQLite removed - no-op
    }
}
