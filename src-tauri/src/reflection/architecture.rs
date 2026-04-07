//! Architecture Model types.
//!
//! Aggregates component-level data from reflection fixes, causal events, and
//! knowledge into a queryable graph. The storage/query implementation lives
//! in `database/pg/reflection.rs`; this module only holds the shared types
//! and path normalization helpers used across the API surface.

use serde::Serialize;

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
