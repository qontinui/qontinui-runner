//! Constraint management HTTP endpoints.
//!
//! Provides HTTP endpoints for inspecting and validating constraints:
//! - `GET /constraints/active` — list active constraints for a project
//! - `GET /constraints/config` — read raw constraints.toml content
//! - `GET /constraints/results/:task_run_id` — get constraint results for a task run
//! - `POST /constraints/config` — write/update constraints.toml (validates first)
//! - `POST /constraints/validate` — validate a constraints.toml config string

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::info;

use crate::constraint_engine::config::{
    find_config_file, load_config, parse_config_str, CONFIG_FILENAME,
};
use crate::constraint_engine::{Constraint, ConstraintEngine};
use crate::mcp::types::{ApiResponse, ApiState};

// ============================================================================
// Request / Response Types
// ============================================================================

/// Query params for GET /constraints/active.
#[derive(Debug, Deserialize)]
pub struct ActiveConstraintsQuery {
    /// Project path to load constraints from. Defaults to workspace root.
    pub project_path: Option<String>,
}

/// Query params for GET /constraints/results/:task_run_id.
#[derive(Debug, Deserialize)]
pub struct ConstraintResultsQuery {
    /// Optional iteration filter.
    pub iteration: Option<i64>,
}

/// Request body for POST /constraints/validate.
#[derive(Debug, Deserialize)]
pub struct ValidateConfigRequest {
    /// Raw TOML content to validate.
    pub toml: String,
}

/// Response for POST /constraints/validate.
#[derive(Debug, Serialize)]
pub struct ValidateConfigResponse {
    /// Whether the config is fully valid (parseable with no errors or warnings).
    pub valid: bool,
    /// Parse errors or non-fatal warnings (e.g., constraints skipped due to bad regex).
    pub errors: Vec<String>,
    /// Successfully parsed constraints (may be partial if some were skipped).
    pub constraints: Vec<Constraint>,
}

/// Query params for GET /constraints/config.
#[derive(Debug, Deserialize)]
pub struct ReadConfigQuery {
    /// Project path to read constraints.toml from. Defaults to workspace root.
    pub project_path: Option<String>,
}

/// Response for GET /constraints/config.
#[derive(Debug, Serialize)]
pub struct ReadConfigResponse {
    /// Raw TOML content of the constraints.toml file (empty string if not found).
    pub toml: String,
    /// Resolved file path, if a config file was found.
    pub path: Option<String>,
}

/// Request body for POST /constraints/config.
#[derive(Debug, Deserialize)]
pub struct WriteConfigRequest {
    /// Project path for the constraints.toml. Defaults to workspace root.
    pub project_path: Option<String>,
    /// Raw TOML content to validate and write.
    pub toml: String,
}

/// Response for POST /constraints/config.
#[derive(Debug, Serialize)]
pub struct WriteConfigResponse {
    /// Whether the config is fully valid (parseable with no errors or warnings).
    pub valid: bool,
    /// Parse errors or non-fatal warnings (e.g., constraints skipped due to bad regex).
    pub errors: Vec<String>,
    /// The file path that was written to.
    pub path: String,
}

// ============================================================================
// Handlers
// ============================================================================

/// GET /constraints/active
///
/// List all active constraints for a given project path.
/// Loads builtins, applies config-file overrides, and returns enabled constraints.
pub async fn get_active_constraints(
    State(_state): State<Arc<ApiState>>,
    Query(query): Query<ActiveConstraintsQuery>,
) -> Json<ApiResponse<Vec<Constraint>>> {
    let project_path = resolve_project_path(query.project_path);

    let engine = match &project_path {
        Some(path) => {
            info!(
                "CONSTRAINTS-API: Loading constraints for project: {}",
                path.display()
            );
            match load_config(path) {
                Some(loaded) => ConstraintEngine::from_config(&loaded),
                None => {
                    info!("CONSTRAINTS-API: No config found, using builtins only");
                    ConstraintEngine::new()
                }
            }
        }
        None => {
            info!("CONSTRAINTS-API: No project path, using builtins only");
            ConstraintEngine::new()
        }
    };

    let active: Vec<Constraint> = engine
        .constraints()
        .iter()
        .filter(|c| c.enabled)
        .cloned()
        .collect();

    info!(
        "CONSTRAINTS-API: Returning {} active constraint(s)",
        active.len()
    );
    Json(ApiResponse::success(active))
}

/// GET /constraints/results/:task_run_id
///
/// Get constraint results for a specific task run.
/// Optionally filter by iteration number.
pub async fn get_constraint_results(
    State(state): State<Arc<ApiState>>,
    Path(task_run_id): Path<String>,
    Query(query): Query<ConstraintResultsQuery>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, (StatusCode, String)> {
    let iteration = query.iteration.map(|i| i as u32);

    info!(
        "CONSTRAINTS-API: Results requested for task_run_id={}, iteration={:?}",
        task_run_id, iteration
    );

    let results = state
        .app_state
        .checkpoint_db
        .get_constraint_results(&task_run_id, iteration)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    info!(
        "CONSTRAINTS-API: Returning {} constraint result(s)",
        results.len()
    );
    Ok(Json(ApiResponse::success(results)))
}

/// Resolve project path from an optional query/body parameter, falling back to
/// the workspace root detected from the executable location.
fn resolve_project_path(explicit: Option<String>) -> Option<std::path::PathBuf> {
    explicit.map(std::path::PathBuf::from).or_else(|| {
        crate::mcp::shared::get_workspace_paths_internal()
            .ok()
            .map(|(root, _, _)| root)
    })
}

/// GET /constraints/config
///
/// Read the raw constraints.toml file content for a project.
/// Returns an empty string (not an error) if the file does not exist.
pub async fn get_config(
    State(_state): State<Arc<ApiState>>,
    Query(query): Query<ReadConfigQuery>,
) -> Json<ApiResponse<ReadConfigResponse>> {
    let project_path = resolve_project_path(query.project_path);

    let (toml_content, file_path) = match &project_path {
        Some(path) => {
            info!(
                "CONSTRAINTS-API: Reading config for project: {}",
                path.display()
            );
            match find_config_file(path) {
                Some(config_path) => {
                    let content = std::fs::read_to_string(&config_path).unwrap_or_default();
                    info!(
                        "CONSTRAINTS-API: Read {} bytes from {}",
                        content.len(),
                        config_path.display()
                    );
                    (content, Some(config_path.to_string_lossy().to_string()))
                }
                None => {
                    info!("CONSTRAINTS-API: No config file found, returning empty");
                    (String::new(), None)
                }
            }
        }
        None => {
            info!("CONSTRAINTS-API: No project path available, returning empty");
            (String::new(), None)
        }
    };

    Json(ApiResponse::success(ReadConfigResponse {
        toml: toml_content,
        path: file_path,
    }))
}

/// POST /constraints/config
///
/// Validate and write a constraints.toml file for a project.
/// Creates the `.qontinui/` directory and file if they don't exist.
/// If an existing config file is found, it is overwritten in place.
pub async fn write_config(
    State(_state): State<Arc<ApiState>>,
    Json(body): Json<WriteConfigRequest>,
) -> Result<Json<ApiResponse<WriteConfigResponse>>, (StatusCode, String)> {
    let project_path = resolve_project_path(body.project_path);

    let project_dir = project_path.ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            "No project_path provided and no workspace root could be detected".to_string(),
        )
    })?;

    info!(
        "CONSTRAINTS-API: Writing config for project: {} ({} bytes)",
        project_dir.display(),
        body.toml.len()
    );

    // Validate the TOML content first
    let (valid, errors) = match parse_config_str(&body.toml) {
        Ok(loaded) => (loaded.warnings.is_empty(), loaded.warnings),
        Err(errs) => {
            // Hard parse error — return validation failure without writing
            return Ok(Json(ApiResponse::success(WriteConfigResponse {
                valid: false,
                errors: errs,
                path: String::new(),
            })));
        }
    };

    // Determine the write path: reuse existing file location, or default to
    // <project>/.qontinui/constraints.toml
    let write_path = find_config_file(&project_dir)
        .unwrap_or_else(|| project_dir.join(".qontinui").join(CONFIG_FILENAME));

    // Ensure parent directory exists
    if let Some(parent) = write_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to create directory {}: {}", parent.display(), e),
            )
        })?;
    }

    // Write the file
    std::fs::write(&write_path, &body.toml).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to write {}: {}", write_path.display(), e),
        )
    })?;

    info!("CONSTRAINTS-API: Wrote config to {}", write_path.display());

    Ok(Json(ApiResponse::success(WriteConfigResponse {
        valid,
        errors,
        path: write_path.to_string_lossy().to_string(),
    })))
}

/// POST /constraints/validate
///
/// Validate a TOML config string and return the parsed constraints.
pub async fn validate_config(
    State(_state): State<Arc<ApiState>>,
    Json(body): Json<ValidateConfigRequest>,
) -> Json<ApiResponse<ValidateConfigResponse>> {
    info!(
        "CONSTRAINTS-API: Validating config ({} bytes)",
        body.toml.len()
    );

    match parse_config_str(&body.toml) {
        Ok(loaded) => {
            let warnings = loaded.warnings.clone();

            // Build an engine to show the full resolved constraint list
            let engine = ConstraintEngine::from_config(&loaded);
            let all_constraints: Vec<Constraint> = engine.constraints().to_vec();

            Json(ApiResponse::success(ValidateConfigResponse {
                valid: warnings.is_empty(),
                errors: warnings,
                constraints: all_constraints,
            }))
        }
        Err(errors) => Json(ApiResponse::success(ValidateConfigResponse {
            valid: false,
            errors,
            constraints: vec![],
        })),
    }
}

// ============================================================================
// Routes
// ============================================================================

/// Register constraint management API routes.
pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::{get, post};

    axum::Router::new()
        .route("/constraints/active", get(get_active_constraints))
        .route("/constraints/config", get(get_config).post(write_config))
        .route(
            "/constraints/results/:task_run_id",
            get(get_constraint_results),
        )
        .route("/constraints/validate", post(validate_config))
}
