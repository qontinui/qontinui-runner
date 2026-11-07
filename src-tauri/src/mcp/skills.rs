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

use crate::mcp::envelope::{RequestHints, UiBridgeJson};
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

impl RequestHints for ApproveSkillRequest {
    fn shape_error_suggestions() -> Option<Vec<String>> {
        Some(vec![
            "Required field: `status` (one of: \"approved\", \"rejected\", \"pending\")."
                .to_string(),
        ])
    }
    fn shape_error_data() -> Option<serde_json::Value> {
        Some(serde_json::json!({ "allowedStatuses": ["approved", "rejected", "pending"] }))
    }
}

#[derive(Debug, Deserialize)]
pub struct BumpVersionRequest {
    pub bump_type: String, // "patch" | "minor" | "major"
}

impl RequestHints for BumpVersionRequest {
    fn shape_error_suggestions() -> Option<Vec<String>> {
        Some(vec![
            "Required field: `bump_type` (one of: \"patch\", \"minor\", \"major\").".to_string(),
        ])
    }
    fn shape_error_data() -> Option<serde_json::Value> {
        Some(serde_json::json!({ "allowedBumpTypes": ["patch", "minor", "major"] }))
    }
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
    /// Local skills not pushed because their stored template does not parse:
    /// pushing the served fallback would publish an empty skill to the org.
    /// Not in `errors` and not counted in `failed`.
    pub withheld: Vec<crate::skills::WithheldSkill>,
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
    /// Org skills refused before import, each with its typed reason (a
    /// template of the retired `playbook` kind). Not in `errors`, and not
    /// counted in `skipped`, which counts the slug conflicts the import left
    /// alone.
    pub refused: Vec<crate::skills::SkillIntakeRefusal>,
}

/// What sync pull makes of the org's skill items, before anything is written.
#[derive(Debug, Default)]
struct PulledSkills {
    /// Definitions to import, re-sourced as community skills.
    skills: Vec<SkillDefinition>,
    /// Items that did not deserialize as a skill definition.
    parse_errors: Vec<String>,
    /// Items refused with a typed reason before deserialization.
    refused: Vec<crate::skills::SkillIntakeRefusal>,
}

/// Classify the org's pulled skill items. An item whose template is of the
/// retired `playbook` kind is refused with its typed reason BEFORE typed
/// deserialization, which would otherwise file it under `parse_errors` as
/// serde's bare `unknown variant`.
fn classify_pulled_items(items: &[Value]) -> PulledSkills {
    let mut out = PulledSkills::default();
    for item in items {
        if let Some(refusal) = crate::skills::refuse_retired_template(item) {
            out.refused.push(refusal);
            continue;
        }
        match serde_json::from_value::<SkillDefinition>(item.clone()) {
            Ok(mut skill) => {
                skill.source = crate::skills::SkillSource::Community;
                // Prefix ID to avoid collisions with local skills
                if !skill.id.starts_with("community:") {
                    skill.id = format!("community:{}", skill.slug);
                }
                out.skills.push(skill);
            }
            Err(e) => {
                let name = item
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                out.parse_errors
                    .push(format!("Failed to parse skill '{}': {}", name, e));
            }
        }
    }
    out
}

// ============================================================================
// Handlers
// ============================================================================

/// List all skills (builtin + user)
pub async fn list_skills(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<SkillDefinition>>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Load user skills from database
    let user_skills = match state.app_state.pg_db.list_user_skills().await {
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
    match state.app_state.pg_db.get_user_skill(&id).await {
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
    match state.app_state.pg_db.create_user_skill(&request).await {
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

    match state.app_state.pg_db.update_user_skill(&id, &request).await {
        Ok(skill) => Ok(Json(ApiResponse::success(skill))),
        Err(e) => Err(skill_rewrite_failure("update", e)),
    }
}

/// The HTTP answer for a skill rewrite (update, fork, version bump) that wrote nothing: no
/// such skill is 404, a stored template that does not parse is a typed 409
/// (the row's state, not the request, is what prevents the rewrite), and a
/// database failure is 500.
fn skill_rewrite_failure(
    verb: &str,
    e: crate::database::pg::skills::SkillRewriteError,
) -> (StatusCode, Json<ApiResponse<()>>) {
    use crate::database::pg::skills::SkillRewriteError;
    let message = format!("Failed to {verb} skill: {e}");
    match e {
        SkillRewriteError::NotFound(_) => (StatusCode::NOT_FOUND, Json(api_error(message))),
        SkillRewriteError::UnparseableStoredTemplate { .. } => (
            StatusCode::CONFLICT,
            Json(ApiResponse::error_with_code(
                message,
                crate::skills::STORED_SKILL_TEMPLATE_UNPARSEABLE,
            )),
        ),
        SkillRewriteError::Failed(_) => {
            (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(message)))
        }
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

    match state.app_state.pg_db.delete_user_skill(&id).await {
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

    let user_skills = state
        .app_state
        .pg_db
        .list_user_skills()
        .await
        .unwrap_or_default();

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
    let user_skills = state
        .app_state
        .pg_db
        .list_user_skills()
        .await
        .unwrap_or_default();

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
    let crate::database::pg::skills::ExportableUserSkills {
        mut skills,
        withheld,
    } = match state
        .app_state
        .pg_db
        .export_user_skills(&request.skill_ids)
        .await
    {
        Ok(s) => s,
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to export skills: {}", e))),
            ))
        }
    };
    for w in &withheld {
        tracing::warn!(
            skill_id = %w.skill_id,
            code = %w.code,
            error = %w.error,
            "skills export: withheld a skill whose stored template does not parse"
        );
    }

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
        withheld,
    })))
}

/// Read an import request body, refusing each skill whose template is of the
/// retired `playbook` kind with its typed reason
/// ([`crate::skills::SkillIntakeRefusal`]) and keeping the rest. The refusal
/// reads the RAW body, before typed deserialization, which would otherwise fail
/// the whole request with serde's bare `unknown variant`. A body that is not
/// otherwise a valid import request is a 422 `INVALID_REQUEST`.
fn import_request_from_body(
    mut body: Value,
) -> Result<
    (ImportSkillsRequest, Vec<crate::skills::SkillIntakeRefusal>),
    (StatusCode, Json<ApiResponse<()>>),
> {
    let mut refused = Vec::new();
    if let Some(skills) = body.get_mut("skills").and_then(Value::as_array_mut) {
        skills.retain(
            |skill| match crate::skills::refuse_retired_template(skill) {
                Some(refusal) => {
                    refused.push(refusal);
                    false
                }
                None => true,
            },
        );
    }
    let request = serde_json::from_value(body)
        .map_err(|e| crate::mcp::envelope::envelope_422(format!("Invalid import request: {e}")))?;
    Ok((request, refused))
}

/// Import skills from a shared export
pub async fn import_skills(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<Value>,
) -> Result<Json<ApiResponse<crate::skills::SkillImportResult>>, (StatusCode, Json<ApiResponse<()>>)>
{
    let (request, refused) = import_request_from_body(body)?;
    for refusal in &refused {
        tracing::warn!(
            skill = refusal.skill(),
            code = refusal.code(),
            "skills import: refused a skill: {refusal}"
        );
    }
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

    match state
        .app_state
        .pg_db
        .import_skills(&request.skills, &request.conflict_mode)
        .await
    {
        Ok(mut result) => {
            result.warnings.extend(checksum_warnings);
            result.refused = refused;
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
    UiBridgeJson(req): UiBridgeJson<ApproveSkillRequest>,
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

    match state
        .app_state
        .pg_db
        .update_skill_approval(&id, &req.status)
        .await
    {
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
    match state
        .app_state
        .pg_db
        .fork_skill(&id, req.new_name.as_deref())
        .await
    {
        Ok(new_skill) => Ok(Json(ApiResponse::success(new_skill))),
        Err(e) => Err(skill_rewrite_failure("fork", e)),
    }
}

/// POST /skills/{id}/increment-usage — Track skill usage
pub async fn increment_usage(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    match state.app_state.pg_db.increment_skill_usage(&id).await {
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
    let crate::database::pg::skills::ExportableUserSkills {
        mut skills,
        withheld,
    } = match state
        .app_state
        .pg_db
        .export_user_skills(&req.skill_ids)
        .await
    {
        Ok(s) => s,
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to export local skills: {}", e))),
            ))
        }
    };
    for w in &withheld {
        tracing::warn!(
            skill_id = %w.skill_id,
            code = %w.code,
            error = %w.error,
            "skills sync push: withheld a skill whose stored template does not parse"
        );
    }

    if skills.is_empty() {
        let errors = if withheld.is_empty() {
            vec!["No user skills found to push".to_string()]
        } else {
            Vec::new()
        };
        return Ok(Json(ApiResponse::success(SyncPushResult {
            pushed: 0,
            failed: 0,
            errors,
            withheld,
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
        withheld,
    })))
}

/// POST /skills/sync/pull — Pull org skills from the web backend into local PG
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

    let PulledSkills {
        skills,
        parse_errors,
        refused,
    } = classify_pulled_items(&items);
    for refusal in &refused {
        tracing::warn!(
            organization_id = %req.organization_id,
            skill = refusal.skill(),
            code = refusal.code(),
            "skills sync pull: refused an org skill: {refusal}"
        );
    }

    if skills.is_empty() && parse_errors.is_empty() && refused.is_empty() {
        return Ok(Json(ApiResponse::success(SyncPullResult {
            pulled: 0,
            skipped: 0,
            errors: vec!["No skills found in organization".to_string()],
            refused,
        })));
    }

    // Import into local DB
    match state.app_state.pg_db.import_skills(&skills, "skip").await {
        Ok(result) => {
            let mut all_errors = parse_errors;
            all_errors.extend(result.errors);

            tracing::info!(
                "Pulled {} skills from org {} ({} skipped, {} errors, {} refused)",
                result.imported,
                req.organization_id,
                result.skipped,
                all_errors.len(),
                refused.len()
            );

            Ok(Json(ApiResponse::success(SyncPullResult {
                pulled: result.imported,
                skipped: result.skipped,
                errors: all_errors,
                refused,
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
    UiBridgeJson(req): UiBridgeJson<BumpVersionRequest>,
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

    // Get the current skill. Read through the rewrite gate: the checksum is
    // computed over the template, and over the served fallback it would be
    // the checksum of a skill that is not the stored one.
    let skill = match state.app_state.pg_db.get_user_skill_for_rewrite(&id).await {
        Ok(Some(s)) => s,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(api_error(format!("Skill not found: {}", id))),
            ))
        }
        Err(e) => return Err(skill_rewrite_failure("bump the version of", e)),
    };

    let current_version = skill.version.as_deref().unwrap_or("1.0.0");
    let new_version = bump_semver(current_version, &req.bump_type);

    // Recompute checksum with the new version
    let mut updated = skill.clone();
    updated.version = Some(new_version.clone());
    let checksum = crate::skills::compute_skill_checksum(&updated);

    // Update in DB
    match state
        .app_state
        .pg_db
        .update_skill_version(&id, &new_version, &checksum)
        .await
    {
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

// ============================================================================
// Unit tests — Phase A4 UiBridgeJson migration
// ============================================================================

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        http::{header, Request, StatusCode},
        routing::post,
        Router,
    };
    use tower::ServiceExt;

    use super::{ApproveSkillRequest, BumpVersionRequest};
    use crate::mcp::envelope::UiBridgeJson;

    // ── helper ────────────────────────────────────────────────────────────────

    async fn send_uib<T>(body: &'static str) -> (StatusCode, serde_json::Value)
    where
        T: serde::de::DeserializeOwned + crate::mcp::envelope::RequestHints + Send + 'static,
    {
        async fn handler<
            T: serde::de::DeserializeOwned + crate::mcp::envelope::RequestHints + Send,
        >(
            UiBridgeJson(_): UiBridgeJson<T>,
        ) -> axum::http::StatusCode {
            axum::http::StatusCode::OK
        }

        let app = Router::new().route("/test", post(handler::<T>));
        let req = Request::builder()
            .method("POST")
            .uri("/test")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        (status, json)
    }

    // ── ApproveSkillRequest ───────────────────────────────────────────────────

    /// Missing `status` field → 422 with `allowedStatuses` in hint.
    #[tokio::test]
    async fn approve_skill_request_422_carries_allowed_statuses() {
        let (status, body) =
            send_uib::<ApproveSkillRequest>(r#"{"wrong_field": "approved"}"#).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["success"], false);
        assert_eq!(body["code"], "INVALID_REQUEST");
        let suggestions = body["suggestions"].as_array().expect("suggestions array");
        assert!(!suggestions.is_empty(), "suggestions must not be empty");
        let hint = &body["hint"];
        assert!(!hint.is_null(), "hint must carry allowedStatuses");
        let statuses = hint["allowedStatuses"]
            .as_array()
            .expect("allowedStatuses array");
        assert_eq!(statuses.len(), 3, "must expose all 3 approval statuses");
        assert!(
            statuses.iter().any(|s| s.as_str() == Some("approved")),
            "approved must be listed"
        );
        assert!(
            statuses.iter().any(|s| s.as_str() == Some("rejected")),
            "rejected must be listed"
        );
        assert!(
            statuses.iter().any(|s| s.as_str() == Some("pending")),
            "pending must be listed"
        );
    }

    // ── BumpVersionRequest ────────────────────────────────────────────────────

    /// Missing `bump_type` field → 422 with `allowedBumpTypes` in hint.
    #[tokio::test]
    async fn bump_version_request_422_carries_allowed_bump_types() {
        let (status, body) = send_uib::<BumpVersionRequest>(r#"{"wrong_field": "patch"}"#).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["success"], false);
        assert_eq!(body["code"], "INVALID_REQUEST");
        let suggestions = body["suggestions"].as_array().expect("suggestions array");
        assert!(!suggestions.is_empty(), "suggestions must not be empty");
        let hint = &body["hint"];
        assert!(!hint.is_null(), "hint must carry allowedBumpTypes");
        let bump_types = hint["allowedBumpTypes"]
            .as_array()
            .expect("allowedBumpTypes array");
        assert_eq!(bump_types.len(), 3, "must expose all 3 bump types");
        assert!(
            bump_types.iter().any(|t| t.as_str() == Some("patch")),
            "patch must be listed"
        );
        assert!(
            bump_types.iter().any(|t| t.as_str() == Some("minor")),
            "minor must be listed"
        );
        assert!(
            bump_types.iter().any(|t| t.as_str() == Some("major")),
            "major must be listed"
        );
    }

    // ── The retired `playbook` template kind at the intake doors ──────────────

    /// A minimal skill definition as an export or an org pull carries it.
    fn skill_item(slug: &str, template: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "id": format!("user:{slug}"),
            "name": format!("Skill {slug}"),
            "slug": slug,
            "description": "",
            "category": "custom",
            "tags": [],
            "icon": "puzzle",
            "color": "gray",
            "allowed_phases": ["setup"],
            "parameters": [],
            "template": template,
            "source": "user"
        })
    }

    fn single_step() -> serde_json::Value {
        serde_json::json!({"kind": "single_step", "step": {"type": "command"}})
    }

    fn playbook() -> serde_json::Value {
        serde_json::json!({"kind": "playbook", "content": "x"})
    }

    /// Import refuses each `playbook`-kind skill with its typed reason and
    /// keeps the rest: the live skill deserializes for import, the refused
    /// ones are named by slug, and neither is filed as the other.
    #[test]
    fn import_refuses_each_playbook_template_by_type_and_keeps_the_rest() {
        let body = serde_json::json!({"skills": [
            skill_item("lint", single_step()),
            skill_item("sf-login", playbook()),
            skill_item("crm-notes", playbook()),
        ]});
        let (request, refused) =
            super::import_request_from_body(body).expect("the live skill still imports");
        let kept: Vec<&str> = request.skills.iter().map(|s| s.slug.as_str()).collect();
        assert_eq!(kept, ["lint"]);
        assert_eq!(
            refused,
            vec![
                crate::skills::SkillIntakeRefusal::PlaybookTemplateRetired {
                    skill: "sf-login".to_string()
                },
                crate::skills::SkillIntakeRefusal::PlaybookTemplateRetired {
                    skill: "crm-notes".to_string()
                },
            ]
        );
        let wire = serde_json::to_value(&refused).unwrap();
        assert_eq!(wire[0]["code"], "PLAYBOOK_TEMPLATE_RETIRED");
        assert_eq!(wire[0]["skill"], "sf-login");
    }

    /// Control arms of the same door: an import with no playbook template
    /// deserializes as before with nothing refused, and a body that is not an
    /// import request is a typed 422 INVALID_REQUEST.
    #[test]
    fn import_without_a_playbook_template_reads_as_before() {
        let (request, refused) = super::import_request_from_body(serde_json::json!({"skills": [
            skill_item("lint", single_step())
        ]}))
        .expect("a live template imports");
        assert_eq!(request.skills.len(), 1);
        assert_eq!(request.skills[0].slug, "lint");
        assert_eq!(request.conflict_mode, "skip");
        assert!(refused.is_empty(), "{refused:?}");

        let (status, json) = super::import_request_from_body(serde_json::json!({"skill": []}))
            .expect_err("no `skills` array is not an import request");
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let json = serde_json::to_value(&json.0).unwrap();
        assert_eq!(json["code"], "INVALID_REQUEST");
    }

    /// Sync pull refuses a `playbook` item with its typed reason, imports the
    /// live item as a community skill, and keeps a malformed item a parse
    /// error: the refusal is neither a parse error nor an import.
    #[test]
    fn sync_pull_refuses_a_playbook_item_by_type_and_keeps_the_rest() {
        let items = vec![
            skill_item("lint", single_step()),
            skill_item("sf-login", playbook()),
            serde_json::json!({"name": "broken", "template": single_step()}),
        ];
        let pulled = super::classify_pulled_items(&items);

        assert_eq!(
            pulled.refused,
            vec![crate::skills::SkillIntakeRefusal::PlaybookTemplateRetired {
                skill: "sf-login".to_string()
            }]
        );
        assert_eq!(pulled.skills.len(), 1);
        assert_eq!(pulled.skills[0].id, "community:lint");
        assert_eq!(
            pulled.skills[0].source,
            crate::skills::SkillSource::Community
        );
        assert_eq!(pulled.parse_errors.len(), 1, "{:?}", pulled.parse_errors);
        assert!(
            pulled.parse_errors[0].contains("'broken'"),
            "{:?}",
            pulled.parse_errors
        );
        assert!(
            !pulled.parse_errors.iter().any(|e| e.contains("playbook")),
            "the playbook item is refused, not a parse error: {:?}",
            pulled.parse_errors
        );
    }

    /// Each way a rewrite (update, fork, version bump) can write nothing has its own status:
    /// no skill 404, a stored template that does not parse a typed 409, a
    /// database failure 500.
    #[test]
    fn a_skill_rewrite_failure_answers_the_status_of_its_cause() {
        use crate::database::pg::skills::SkillRewriteError;

        let (status, json) =
            super::skill_rewrite_failure("update", SkillRewriteError::NotFound("user:x".into()));
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(json.0.code.is_none());

        let (status, json) = super::skill_rewrite_failure(
            "fork",
            SkillRewriteError::UnparseableStoredTemplate {
                skill_id: "user:sf-login".into(),
                error: "unknown variant `playbook`".into(),
            },
        );
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(
            json.0.code.as_deref(),
            Some("STORED_SKILL_TEMPLATE_UNPARSEABLE")
        );
        let error = json.0.error.as_deref().unwrap_or_default();
        assert!(error.starts_with("Failed to fork skill: "), "{error}");
        assert!(error.contains("user:sf-login"), "{error}");

        let (status, json) = super::skill_rewrite_failure(
            "update",
            SkillRewriteError::Failed("PG pool error".into()),
        );
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(json.0.code.is_none());
    }
}
