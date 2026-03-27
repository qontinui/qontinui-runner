//! Skills library CRUD handlers for MCP API
//!
//! Provides HTTP handlers for managing user-created skills:
//! list (builtin + user), get, create, update, delete, search,
//! push/pull sync with web backend.

use axum::{extract::State, http::StatusCode, response::Json};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::skills::SkillDefinition;

// ============================================================================
// Request Types
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct CreateSkillRequest {
    pub name: String,
    pub slug: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_category")]
    pub category: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default = "default_icon")]
    pub icon: String,
    #[serde(default = "default_color")]
    pub color: String,
    pub allowed_phases: Vec<String>,
    #[serde(default)]
    pub parameters: Vec<crate::skills::SkillParameter>,
    pub template: crate::skills::SkillTemplate,
}

fn default_category() -> String {
    "custom".to_string()
}
fn default_icon() -> String {
    "puzzle".to_string()
}
fn default_color() -> String {
    "gray".to_string()
}

#[derive(Debug, Deserialize)]
pub struct UpdateSkillRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub allowed_phases: Option<Vec<String>>,
    #[serde(default)]
    pub parameters: Option<Vec<crate::skills::SkillParameter>>,
    #[serde(default)]
    pub template: Option<crate::skills::SkillTemplate>,
}

#[derive(Debug, Deserialize)]
pub struct InstantiateSkillRequest {
    pub phase: String,
    #[serde(default)]
    pub parameter_values: HashMap<String, Value>,
}

#[derive(Debug, Deserialize)]
pub struct ExportSkillsRequest {
    #[serde(default)]
    pub skill_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct ImportSkillsRequest {
    pub skills: Vec<crate::skills::SkillDefinition>,
    #[serde(default)]
    pub manifest: Option<crate::skills::SkillExportManifest>,
    #[serde(default = "default_skip")]
    pub conflict_mode: String,
}

fn default_skip() -> String {
    "skip".to_string()
}

#[derive(Debug, Deserialize)]
pub struct ApproveSkillRequest {
    pub status: String, // "approved" | "rejected" | "pending"
}

#[derive(Debug, Deserialize)]
pub struct BumpVersionRequest {
    pub bump_type: String, // "patch" | "minor" | "major"
}

#[derive(Debug, Deserialize)]
pub struct ForkSkillRequest {
    #[serde(default)]
    pub new_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SyncPushRequest {
    /// Skill IDs to push. If empty, pushes all user skills.
    #[serde(default)]
    pub skill_ids: Vec<String>,
    /// Web backend URL (e.g., "http://localhost:8000")
    pub backend_url: String,
    /// Auth token for the web backend
    pub auth_token: String,
}

#[derive(Debug, Serialize)]
pub struct SyncPushResult {
    pub pushed: usize,
    pub failed: usize,
    pub errors: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct SyncPullRequest {
    /// Web backend URL (e.g., "http://localhost:8000")
    pub backend_url: String,
    /// Auth token for the web backend
    pub auth_token: String,
    /// Organization ID to pull skills from
    pub organization_id: String,
}

#[derive(Debug, Serialize)]
pub struct SyncPullResult {
    pub pulled: usize,
    pub skipped: usize,
    pub errors: Vec<String>,
}

// ============================================================================
// Handlers
// ============================================================================

/// List all skills (builtin + user)
pub async fn list_skills(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<SkillDefinition>>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Load user skills from database
    let user_skills_result = if let Some(pg) = &state.app_state.pg_db {
        pg.list_user_skills().await
    } else {
        state.app_state.checkpoint_db.list_user_skills()
    };
    let user_skills = match user_skills_result {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("Failed to list user skills: {}", e);
            vec![]
        }
    };

    // Build registry with user skills
    let mut registry = crate::skills::SkillRegistry::new();
    registry.set_user_skills(user_skills);

    let all: Vec<SkillDefinition> = registry.all().into_iter().cloned().collect();
    Ok(Json(ApiResponse::success(all)))
}

/// Get a single skill by ID (works for both builtin and user)
pub async fn get_skill(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<SkillDefinition>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Check builtin first
    let registry = crate::skills::SkillRegistry::new();
    if let Some(skill) = registry.get(&id) {
        return Ok(Json(ApiResponse::success(skill.clone())));
    }

    // Check user skills in database
    let skill_result = if let Some(pg) = &state.app_state.pg_db {
        pg.get_user_skill(&id).await
    } else {
        state.app_state.checkpoint_db.get_user_skill(&id)
    };
    match skill_result {
        Ok(Some(skill)) => Ok(Json(ApiResponse::success(skill))),
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Skill not found: {}", id))),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to get skill: {}", e))),
        )),
    }
}

/// Create a new user skill
pub async fn create_skill(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CreateSkillRequest>,
) -> Result<Json<ApiResponse<SkillDefinition>>, (StatusCode, Json<ApiResponse<()>>)> {
    let result = if let Some(pg) = &state.app_state.pg_db {
        pg.create_user_skill(&request).await
    } else {
        state.app_state.checkpoint_db.create_user_skill(&request)
    };
    match result {
        Ok(skill) => Ok(Json(ApiResponse::success(skill))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to create skill: {}", e))),
        )),
    }
}

/// Update a user skill
pub async fn update_skill(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(request): Json<UpdateSkillRequest>,
) -> Result<Json<ApiResponse<SkillDefinition>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Don't allow editing builtin skills
    if id.starts_with("builtin:") {
        return Err((
            StatusCode::FORBIDDEN,
            Json(api_error("Cannot modify builtin skills".to_string())),
        ));
    }

    let result = if let Some(pg) = &state.app_state.pg_db {
        pg.update_user_skill(&id, &request).await
    } else {
        state.app_state.checkpoint_db.update_user_skill(&id, &request)
    };
    match result {
        Ok(skill) => Ok(Json(ApiResponse::success(skill))),
        Err(e) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Failed to update skill: {}", e))),
        )),
    }
}

/// Delete a user skill
pub async fn delete_skill(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Don't allow deleting builtin skills
    if id.starts_with("builtin:") {
        return Err((
            StatusCode::FORBIDDEN,
            Json(api_error("Cannot delete builtin skills".to_string())),
        ));
    }

    let del_result = if let Some(pg) = &state.app_state.pg_db {
        pg.delete_user_skill(&id).await
    } else {
        state.app_state.checkpoint_db.delete_user_skill(&id)
    };
    match del_result {
        Ok(true) => Ok(Json(ApiResponse::success(()))),
        Ok(false) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Skill not found: {}", id))),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to delete skill: {}", e))),
        )),
    }
}

/// Search skills (builtin + user)
pub async fn search_skills(
    State(state): State<Arc<ApiState>>,
    axum::extract::Query(params): axum::extract::Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<Vec<SkillDefinition>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let query = params.get("q").map(|s| s.as_str()).unwrap_or("");

    let user_skills = if let Some(pg) = &state.app_state.pg_db {
        pg.list_user_skills().await.unwrap_or_default()
    } else {
        state.app_state.checkpoint_db.list_user_skills().unwrap_or_default()
    };

    let mut registry = crate::skills::SkillRegistry::new();
    registry.set_user_skills(user_skills);

    let results: Vec<SkillDefinition> = registry.search(query).into_iter().cloned().collect();
    Ok(Json(ApiResponse::success(results)))
}

/// Instantiate a skill into concrete steps (preview without adding to workflow)
pub async fn instantiate_skill(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(request): Json<InstantiateSkillRequest>,
) -> Result<Json<ApiResponse<Vec<Value>>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Load user skills for registry
    let user_skills = if let Some(pg) = &state.app_state.pg_db {
        pg.list_user_skills().await.unwrap_or_default()
    } else {
        state.app_state.checkpoint_db.list_user_skills().unwrap_or_default()
    };

    let mut registry = crate::skills::SkillRegistry::new();
    registry.set_user_skills(user_skills);

    let skill = match registry.get(&id) {
        Some(s) => s.clone(),
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(api_error(format!("Skill not found: {}", id))),
            ))
        }
    };

    match crate::skills::instantiate_skill(&skill, &request.phase, &request.parameter_values) {
        Ok(steps) => Ok(Json(ApiResponse::success(steps))),
        Err(e) => Err((StatusCode::BAD_REQUEST, Json(api_error(e)))),
    }
}

/// Export skills to a shareable format
pub async fn export_skills(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ExportSkillsRequest>,
) -> Result<Json<ApiResponse<crate::skills::SkillExport>>, (StatusCode, Json<ApiResponse<()>>)> {
    let export_result = if let Some(pg) = &state.app_state.pg_db {
        pg.export_user_skills(&request.skill_ids).await
    } else {
        state.app_state.checkpoint_db.export_user_skills(&request.skill_ids)
    };
    let mut skills = match export_result {
        Ok(s) => s,
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to export skills: {}", e))),
            ))
        }
    };

    // Compute individual skill checksums for any that don't have one
    for skill in &mut skills {
        if skill.checksum.is_none() {
            skill.checksum = Some(crate::skills::compute_skill_checksum(skill));
        }
    }

    // Compute overall export checksum
    let export_checksum = crate::skills::compute_export_checksum(&skills);

    let manifest = crate::skills::SkillExportManifest {
        version: "1.0.0".to_string(),
        exported_at: chrono::Utc::now().to_rfc3339(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        content_type: "skills".to_string(),
        skill_count: skills.len(),
        checksum: Some(export_checksum),
    };

    Ok(Json(ApiResponse::success(crate::skills::SkillExport {
        manifest,
        skills,
    })))
}

/// Import skills from a shared export
pub async fn import_skills(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ImportSkillsRequest>,
) -> Result<Json<ApiResponse<crate::skills::SkillImportResult>>, (StatusCode, Json<ApiResponse<()>>)>
{
    let mut checksum_warnings: Vec<String> = Vec::new();

    // Verify export checksum if manifest is present
    if let Some(ref manifest) = request.manifest {
        if let Some(ref manifest_checksum) = manifest.checksum {
            let computed = crate::skills::compute_export_checksum(&request.skills);
            if &computed != manifest_checksum {
                tracing::warn!(
                    "Export checksum mismatch: manifest has {}, computed {}",
                    manifest_checksum,
                    computed
                );
                checksum_warnings.push(format!(
                    "Export checksum mismatch: data may have been modified since export (expected {}, got {})",
                    manifest_checksum, computed
                ));
            }
        }
    }

    let import_result = if let Some(pg) = &state.app_state.pg_db {
        pg.import_skills(&request.skills, &request.conflict_mode).await
    } else {
        state.app_state.checkpoint_db.import_skills(&request.skills, &request.conflict_mode)
    };
    match import_result {
        Ok(mut result) => {
            result.warnings.extend(checksum_warnings);
            Ok(Json(ApiResponse::success(result)))
        }
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to import skills: {}", e))),
        )),
    }
}

/// POST /skills/{id}/approve — Set approval status
pub async fn approve_skill(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(req): Json<ApproveSkillRequest>,
) -> Result<Json<ApiResponse<Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let valid_statuses = ["approved", "rejected", "pending"];
    if !valid_statuses.contains(&req.status.as_str()) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(
                "Invalid status. Must be: approved, rejected, or pending".to_string(),
            )),
        ));
    }

    let approval_result = if let Some(pg) = &state.app_state.pg_db {
        pg.update_skill_approval(&id, &req.status).await
    } else {
        state.app_state.checkpoint_db.update_skill_approval(&id, &req.status)
    };
    match approval_result {
        Ok(_) => Ok(Json(ApiResponse::success(serde_json::json!({
            "id": id,
            "approval_status": req.status
        })))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!(
                "Failed to update approval status: {}",
                e
            ))),
        )),
    }
}

/// POST /skills/{id}/fork — Create a copy of a skill
pub async fn fork_skill(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(req): Json<ForkSkillRequest>,
) -> Result<Json<ApiResponse<SkillDefinition>>, (StatusCode, Json<ApiResponse<()>>)> {
    let fork_result = if let Some(pg) = &state.app_state.pg_db {
        pg.fork_skill(&id, req.new_name.as_deref()).await
    } else {
        state.app_state.checkpoint_db.fork_skill(&id, req.new_name.as_deref())
    };
    match fork_result {
        Ok(new_skill) => Ok(Json(ApiResponse::success(new_skill))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to fork skill: {}", e))),
        )),
    }
}

/// POST /skills/{id}/increment-usage — Track skill usage
pub async fn increment_usage(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let inc_result = if let Some(pg) = &state.app_state.pg_db {
        pg.increment_skill_usage(&id).await
    } else {
        state.app_state.checkpoint_db.increment_skill_usage(&id)
    };
    match inc_result {
        Ok(count) => Ok(Json(ApiResponse::success(serde_json::json!({
            "id": id,
            "usage_count": count
        })))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to increment usage count: {}", e))),
        )),
    }
}

// ============================================================================
// Push / Pull Sync with Web Backend
// ============================================================================

/// POST /skills/sync/push — Push local user skills to the web backend
pub async fn sync_push(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<SyncPushRequest>,
) -> Result<Json<ApiResponse<SyncPushResult>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Export skills from local DB
    let push_export_result = if let Some(pg) = &state.app_state.pg_db {
        pg.export_user_skills(&req.skill_ids).await
    } else {
        state.app_state.checkpoint_db.export_user_skills(&req.skill_ids)
    };
    let mut skills = match push_export_result {
        Ok(s) => s,
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to export local skills: {}", e))),
            ))
        }
    };

    if skills.is_empty() {
        return Ok(Json(ApiResponse::success(SyncPushResult {
            pushed: 0,
            failed: 0,
            errors: vec!["No user skills found to push".to_string()],
        })));
    }

    // Ensure checksums are computed before pushing
    for skill in &mut skills {
        if skill.checksum.is_none() {
            skill.checksum = Some(crate::skills::compute_skill_checksum(skill));
        }
    }

    let client = reqwest::Client::new();
    let mut pushed = 0;
    let mut errors = Vec::new();

    for skill in &skills {
        let payload = serde_json::json!({
            "name": skill.name,
            "slug": skill.slug,
            "description": skill.description,
            "category": skill.category,
            "tags": skill.tags,
            "icon": skill.icon,
            "color": skill.color,
            "allowed_phases": skill.allowed_phases,
            "parameters": skill.parameters,
            "template": skill.template,
            "version": skill.version.as_deref().unwrap_or("1.0.0"),
            "author": skill.author,
            "depends_on": skill.depends_on.as_deref().unwrap_or(&[]),
            "checksum": skill.checksum,
        });

        match client
            .post(format!("{}/api/v1/skills", req.backend_url))
            .bearer_auth(&req.auth_token)
            .json(&payload)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                tracing::info!("Pushed skill '{}' to web backend", skill.name);
                pushed += 1;
            }
            Ok(resp) => {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                let msg = format!("Failed to push '{}': {} - {}", skill.name, status, body);
                tracing::warn!("{}", msg);
                errors.push(msg);
            }
            Err(e) => {
                let msg = format!("Failed to push '{}': {}", skill.name, e);
                tracing::warn!("{}", msg);
                errors.push(msg);
            }
        }
    }

    Ok(Json(ApiResponse::success(SyncPushResult {
        pushed,
        failed: errors.len(),
        errors,
    })))
}

/// POST /skills/sync/pull — Pull org skills from the web backend into local SQLite
pub async fn sync_pull(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<SyncPullRequest>,
) -> Result<Json<ApiResponse<SyncPullResult>>, (StatusCode, Json<ApiResponse<()>>)> {
    let client = reqwest::Client::new();

    // Fetch org skills from web backend
    let resp = match client
        .get(format!(
            "{}/api/v1/skills/org/{}",
            req.backend_url, req.organization_id
        ))
        .bearer_auth(&req.auth_token)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return Err((
                StatusCode::BAD_GATEWAY,
                Json(api_error(format!("Failed to fetch org skills: {}", e))),
            ))
        }
    };

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err((
            StatusCode::BAD_GATEWAY,
            Json(api_error(format!(
                "Backend returned {} when fetching org skills: {}",
                status, body
            ))),
        ));
    }

    let body: Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to parse backend response: {}",
                    e
                ))),
            ))
        }
    };

    // Extract skills from the response (supports both { items: [...] } and direct array)
    let items = if let Some(arr) = body.get("items").and_then(|v| v.as_array()) {
        arr.clone()
    } else if let Some(data) = body.get("data") {
        if let Some(arr) = data.get("items").and_then(|v| v.as_array()) {
            arr.clone()
        } else if let Some(arr) = data.as_array() {
            arr.clone()
        } else {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(
                    "Invalid response format: could not find skills array".to_string(),
                )),
            ));
        }
    } else if let Some(arr) = body.as_array() {
        arr.clone()
    } else {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(
                "Invalid response format: could not find skills array".to_string(),
            )),
        ));
    };

    let mut skills: Vec<SkillDefinition> = Vec::new();
    let mut parse_errors: Vec<String> = Vec::new();

    for item in &items {
        match serde_json::from_value::<SkillDefinition>(item.clone()) {
            Ok(mut skill) => {
                skill.source = "community".to_string();
                // Prefix ID to avoid collisions with local skills
                if !skill.id.starts_with("community:") {
                    skill.id = format!("community:{}", skill.slug);
                }
                skills.push(skill);
            }
            Err(e) => {
                let name = item
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                parse_errors.push(format!("Failed to parse skill '{}': {}", name, e));
            }
        }
    }

    if skills.is_empty() && parse_errors.is_empty() {
        return Ok(Json(ApiResponse::success(SyncPullResult {
            pulled: 0,
            skipped: 0,
            errors: vec!["No skills found in organization".to_string()],
        })));
    }

    // Import into local DB (PG-primary, SQLite fallback)
    let import_result = if let Some(pg) = &state.app_state.pg_db {
        pg.import_skills(&skills, "skip").await
    } else {
        state.app_state.checkpoint_db.import_skills(&skills, "skip")
    };
    match import_result {
        Ok(result) => {
            let mut all_errors = parse_errors;
            all_errors.extend(result.errors);

            tracing::info!(
                "Pulled {} skills from org {} ({} skipped, {} errors)",
                result.imported,
                req.organization_id,
                result.skipped,
                all_errors.len()
            );

            Ok(Json(ApiResponse::success(SyncPullResult {
                pulled: result.imported,
                skipped: result.skipped,
                errors: all_errors,
            })))
        }
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to import pulled skills: {}", e))),
        )),
    }
}

// ============================================================================
// Version Bumping
// ============================================================================

fn bump_semver(version: &str, bump_type: &str) -> String {
    let parts: Vec<u32> = version.split('.').filter_map(|p| p.parse().ok()).collect();
    let (major, minor, patch) = match parts.as_slice() {
        [ma, mi, pa, ..] => (*ma, *mi, *pa),
        [ma, mi] => (*ma, *mi, 0),
        [ma] => (*ma, 0, 0),
        _ => (1, 0, 0),
    };

    match bump_type {
        "major" => format!("{}.0.0", major + 1),
        "minor" => format!("{}.{}.0", major, minor + 1),
        _ => format!("{}.{}.{}", major, minor, patch + 1),
    }
}

/// POST /skills/{id}/bump-version — Bump the skill version and recompute checksum
pub async fn bump_version(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(req): Json<BumpVersionRequest>,
) -> Result<Json<ApiResponse<Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Don't allow bumping builtin skills
    if id.starts_with("builtin:") {
        return Err((
            StatusCode::FORBIDDEN,
            Json(api_error("Cannot modify builtin skills".to_string())),
        ));
    }

    let valid_types = ["patch", "minor", "major"];
    if !valid_types.contains(&req.bump_type.as_str()) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(
                "Invalid bump_type. Must be: patch, minor, or major".to_string(),
            )),
        ));
    }

    // Get the current skill
    let skill_result = if let Some(pg) = &state.app_state.pg_db {
        pg.get_user_skill(&id).await
    } else {
        state.app_state.checkpoint_db.get_user_skill(&id)
    };
    let skill = match skill_result {
        Ok(Some(s)) => s,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(api_error(format!("Skill not found: {}", id))),
            ))
        }
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get skill: {}", e))),
            ))
        }
    };

    let current_version = skill.version.as_deref().unwrap_or("1.0.0");
    let new_version = bump_semver(current_version, &req.bump_type);

    // Recompute checksum with the new version
    let mut updated = skill.clone();
    updated.version = Some(new_version.clone());
    let checksum = crate::skills::compute_skill_checksum(&updated);

    // Update in DB
    let version_result = if let Some(pg) = &state.app_state.pg_db {
        pg.update_skill_version(&id, &new_version, &checksum).await
    } else {
        state.app_state.checkpoint_db.update_skill_version(&id, &new_version, &checksum)
    };
    match version_result {
        Ok(_) => Ok(Json(ApiResponse::success(serde_json::json!({
            "id": id,
            "previous_version": current_version,
            "version": new_version,
            "checksum": checksum,
        })))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to update skill version: {}", e))),
        )),
    }
}

// ============================================================================
// Routes
// ============================================================================

pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/skills", get(list_skills).post(create_skill))
        .route("/skills/search", get(search_skills))
        .route("/skills/export", post(export_skills))
        .route("/skills/import", post(import_skills))
        .route("/skills/sync/push", post(sync_push))
        .route("/skills/sync/pull", post(sync_pull))
        .route(
            "/skills/{id}",
            get(get_skill).put(update_skill).delete(delete_skill),
        )
        .route("/skills/{id}/instantiate", post(instantiate_skill))
        .route("/skills/{id}/approve", post(approve_skill))
        .route("/skills/{id}/fork", post(fork_skill))
        .route("/skills/{id}/bump-version", post(bump_version))
        .route("/skills/{id}/increment-usage", post(increment_usage))
}
