//! New Project commands — remote-first GitHub provisioning + local scaffold.
//!
//! Implements the runner half of the "New Project Initiation" plan
//! (PR-D): validate → check name / create the GitHub repo via the
//! qontinui-web operations proxy → local folder + `git init` + template
//! scaffold + initial commit → wire remote + push with a short-TTL scoped
//! credential → post-push enroll → register in Saved Projects (and the
//! multi-app registry for UI-Bridge templates).
//!
//! # Ordering & idempotency
//!
//! The remote repo is created BEFORE anything touches disk, so a taken name
//! never leaves a half-made folder. Every step is idempotent for resume:
//! when `resume_from` is set, steps before it are re-verified (repo exists →
//! `create_remote` done; dir has `.git` → `init_local` done; `HEAD` exists →
//! `commit` done; origin configured + `refs/remotes/origin/main` exists →
//! `push` done) instead of re-executed. On failure the result carries
//! `{failedStep, completedSteps, error, resumeToken}` and the UI re-invokes
//! with `resumeFrom`.
//!
//! # Credential hygiene
//!
//! The push credential is used exactly like `setup_wizard::github_clone_repo`
//! uses the clone credential: embedded transiently in the remote URL for the
//! one push, then scrubbed with `git remote set-url origin <clean-url>`. The
//! token never persists in `.git/config`, and is redacted from any error
//! output before it reaches logs or the UI.
//!
//! # Failure policy
//!
//! After the remote repo exists, no failure ever deletes it (destructive
//! rollback is never automatic). A multi-app-registry failure during
//! `register` degrades to a warning — it never fails the project.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter};
use tracing::{info, warn};

use crate::commands::compartments::StorageCompartment;
use crate::commands::new_project_templates as templates;
use crate::settings::SavedProject;

/// Tauri event carrying per-step progress for the New Project checklist UI.
pub const NEW_PROJECT_PROGRESS_EVENT: &str = "new-project://progress";

/// Filename of the flow-owned marker written under a new project's `.git/`
/// directory in `init_local`. It proves to a later resume that this tree is one
/// this flow created (so scaffold may safely overwrite it), distinguishing it
/// from an arbitrary pre-existing repo the caller might point `resume_from` at.
/// Living inside `.git/` means it is never staged, committed, or pushed.
pub(crate) const FLOW_MARKER: &str = "qontinui-new-project";

/// Step names in execution order. `resume_from` must be one of these.
pub const STEP_ORDER: [&str; 8] = [
    "validate",
    "create_remote",
    "init_local",
    "scaffold",
    "commit",
    "push",
    "enroll",
    "register",
];

// ============================================================================
// Wire types
// ============================================================================

/// Result of a project-name check (local rules; plus the GitHub-side check
/// through the web proxy when `owner` is provided).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NameCheckResult {
    /// "available" | "taken" | "invalid" | "owner_unbound"
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The name GitHub would actually use. Differs from the input when the
    /// input contains characters outside `[A-Za-z0-9._-]`.
    pub normalized_name: String,
}

/// Whether the qontinui-dev GitHub App is user-authorized for `owner`, and
/// where to send the user when it isn't.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubOauthStatusResult {
    pub authorized: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorize_url: Option<String>,
}

fn default_private() -> bool {
    true
}

/// Request body for [`create_new_project`].
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateNewProjectRequest {
    /// Repository / folder name. Must already satisfy GitHub name rules
    /// (`[A-Za-z0-9._-]`, not empty/`.`/`..`) — run [`check_project_name`]
    /// first and use its `normalizedName`.
    pub name: String,
    /// Existing parent directory; the project is created at
    /// `<location>/<name>`.
    pub location: String,
    /// GitHub owner (user or org login). `None` = local-only project: the
    /// `create_remote`, `push`, and `enroll` steps are skipped.
    #[serde(default)]
    pub owner: Option<String>,
    /// Repo visibility. Defaults to private.
    #[serde(default = "default_private")]
    pub private: bool,
    /// "empty" | "react-ui-bridge"
    pub template: String,
    /// Enroll the new repo with the Qontinui GitHub Apps after the first
    /// push (coord-managed from day one). Only meaningful with an owner.
    #[serde(default)]
    pub enroll: bool,
    /// Step name to resume from after a partial failure; earlier steps are
    /// re-verified instead of re-executed.
    #[serde(default)]
    pub resume_from: Option<String>,
}

/// Identity of the created (or re-verified) GitHub repository.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteRepoInfo {
    pub full_name: String,
    pub clone_url: String,
    pub html_url: String,
}

/// Typed error for the New Project flow.
///
/// `code` values the UI maps onto fields/buttons:
/// - `invalid_name` / `invalid_location` / `invalid_template` /
///   `invalid_resume_token` / `target_exists` — validation, `field` set.
/// - `not_signed_in` — no Cognito session; sign-in required.
/// - `user_authorization_required` — GitHub App OAuth needed for a personal
///   owner; `authorize_url` carries the button target.
/// - `name_taken` — repo name exists on GitHub; maps back to the name field.
/// - `git_missing` / `git` / `io` — local environment/tooling failures.
/// - `backend` / `enroll_failed` — web-proxy/coord failures.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NewProjectError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorize_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}

impl NewProjectError {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
            authorize_url: None,
            field: None,
        }
    }

    fn with_field(code: &str, field: &str, message: impl Into<String>) -> Self {
        Self {
            field: Some(field.to_string()),
            ..Self::new(code, message)
        }
    }
}

/// Result of [`create_new_project`]. `ok: false` carries the failed step +
/// typed error + `resumeToken` (== the failed step) for a retry invocation.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateNewProjectResult {
    pub ok: bool,
    /// Absolute path of the (attempted) project directory.
    pub project_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<RemoteRepoInfo>,
    pub completed_steps: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_step: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<NewProjectError>,
    /// Pass back as `resumeFrom` to retry the remaining steps.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resume_token: Option<String>,
    pub warnings: Vec<String>,
}

/// Payload of the [`NEW_PROJECT_PROGRESS_EVENT`] Tauri event.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProgressEvent {
    step: String,
    /// "running" | "ok" | "failed" | "skipped"
    status: String,
    detail: String,
}

// ============================================================================
// Pure helpers (unit-tested)
// ============================================================================

fn is_valid_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-'
}

/// Normalize a raw project name the way GitHub does: every run of characters
/// outside `[A-Za-z0-9._-]` collapses to a single `-`.
pub(crate) fn normalize_repo_name(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut pending_dash = false;
    for c in raw.trim().chars() {
        if is_valid_name_char(c) {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.push(c);
        } else {
            pending_dash = true;
        }
    }
    out
}

/// Validate a (already-normalized) repository/folder name against local
/// rules. Returns a human-readable reason on rejection.
pub(crate) fn validate_repo_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Project name cannot be empty".to_string());
    }
    if name == "." || name == ".." {
        return Err(format!("'{}' is not a valid project name", name));
    }
    if name.len() > 100 {
        return Err("Project name must be 100 characters or fewer".to_string());
    }
    if let Some(bad) = name.chars().find(|c| !is_valid_name_char(*c)) {
        return Err(format!(
            "Project name may only contain letters, digits, '.', '_' and '-' (found '{}')",
            bad
        ));
    }
    Ok(())
}

/// Derive a multi-app-registry slug (`[a-z0-9-]`, 1-64 chars, no leading
/// hyphen) from a validated project name.
pub(crate) fn derive_app_id(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut pending_dash = false;
    for c in name.to_ascii_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.push(c);
        } else {
            pending_dash = true;
        }
    }
    let trimmed: String = out.trim_matches('-').chars().take(64).collect();
    let trimmed = trimmed.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "app".to_string()
    } else {
        trimmed
    }
}

/// Index of a step name in [`STEP_ORDER`].
pub(crate) fn step_index(step: &str) -> Option<usize> {
    STEP_ORDER.iter().position(|s| *s == step)
}

/// Remove a secret from arbitrary text before it reaches logs or the UI.
pub(crate) fn redact(text: &str, token: &str) -> String {
    if token.is_empty() {
        text.to_string()
    } else {
        text.replace(token, "***")
    }
}

/// Deterministic remote identity for `<owner>/<name>` — used on resume when
/// the create-repo response from the original run is no longer at hand.
pub(crate) fn reconstructed_remote(owner: &str, name: &str) -> RemoteRepoInfo {
    RemoteRepoInfo {
        full_name: format!("{}/{}", owner, name),
        clone_url: format!("https://github.com/{}/{}.git", owner, name),
        html_url: format!("https://github.com/{}/{}", owner, name),
    }
}

/// Depth-first search a JSON value for the first string under any of `keys`.
fn find_string_key(v: &Value, keys: &[&str]) -> Option<String> {
    match v {
        Value::Object(map) => {
            for k in keys {
                if let Some(s) = map.get(*k).and_then(|x| x.as_str()) {
                    return Some(s.to_string());
                }
            }
            map.values().find_map(|child| find_string_key(child, keys))
        }
        Value::Array(items) => items.iter().find_map(|child| find_string_key(child, keys)),
        _ => None,
    }
}

// ============================================================================
// Git + HTTP plumbing
// ============================================================================

async fn run_git(
    args: Vec<String>,
    cwd: Option<PathBuf>,
) -> Result<std::process::Output, NewProjectError> {
    tokio::task::spawn_blocking(move || {
        let mut cmd = crate::process_helpers::no_window("git");
        if let Some(dir) = &cwd {
            cmd.current_dir(dir);
        }
        cmd.args(&args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
    })
    .await
    .map_err(|e| NewProjectError::new("git", format!("Task join error: {}", e)))?
    .map_err(|e| {
        NewProjectError::new(
            "git_missing",
            format!("Failed to run git: {}. Is git installed?", e),
        )
    })
}

/// Run git and require success; the error carries stderr.
async fn git_ok(args: &[&str], cwd: Option<&Path>) -> Result<(), NewProjectError> {
    let out = run_git(
        args.iter().map(|s| s.to_string()).collect(),
        cwd.map(Path::to_path_buf),
    )
    .await?;
    if out.status.success() {
        Ok(())
    } else {
        Err(NewProjectError::new(
            "git",
            format!(
                "git {} failed: {}",
                args.first().copied().unwrap_or(""),
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        ))
    }
}

/// Run git purely as a success/failure probe (used by resume verification).
async fn git_probe(args: &[&str], cwd: Option<&Path>) -> bool {
    matches!(
        run_git(
            args.iter().map(|s| s.to_string()).collect(),
            cwd.map(Path::to_path_buf),
        )
        .await,
        Ok(out) if out.status.success()
    )
}

fn api_url(path: &str) -> String {
    format!("{}{}", crate::api_config::get_api_base_url(), path)
}

/// POST a JSON body to the web operations proxy. Returns `(status, body)`;
/// the body degrades to `Value::Null` when unparseable.
async fn proxy_post(
    bearer: &str,
    path: &str,
    body: Value,
) -> Result<(reqwest::StatusCode, Value), NewProjectError> {
    let resp = reqwest::Client::new()
        .post(api_url(path))
        .bearer_auth(bearer)
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            NewProjectError::new(
                "backend",
                format!("Failed to reach Qontinui backend: {}", e),
            )
        })?;
    let status = resp.status();
    let body = resp.json::<Value>().await.unwrap_or(Value::Null);
    Ok((status, body))
}

fn backend_error(context: &str, status: reqwest::StatusCode, body: &Value) -> NewProjectError {
    let mut detail =
        find_string_key(body, &["message", "detail", "error"]).unwrap_or_else(|| body.to_string());
    if detail.chars().count() > 500 {
        detail = detail.chars().take(500).collect();
    }
    NewProjectError::new(
        "backend",
        format!("{} failed ({}): {}", context, status, detail.trim()),
    )
}

/// GitHub-side name check through the web proxy.
async fn check_name_remote(
    bearer: &str,
    owner: &str,
    name: &str,
) -> Result<NameCheckResult, NewProjectError> {
    let (status, body) = proxy_post(
        bearer,
        "/api/v1/operations/github/check-name",
        serde_json::json!({ "owner": owner, "name": name }),
    )
    .await?;
    if !status.is_success() {
        return Err(backend_error("Name check", status, &body));
    }
    Ok(NameCheckResult {
        status: body
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("invalid")
            .to_string(),
        reason: find_string_key(&body, &["reason"]),
        normalized_name: find_string_key(&body, &["normalized_name", "normalizedName"])
            .unwrap_or_else(|| normalize_repo_name(name)),
    })
}

/// Create the remote repo through the web proxy, mapping the two typed 409s
/// (`user_authorization_required`, `name_taken`) onto UI-actionable errors.
async fn create_remote_repo(
    bearer: &str,
    owner: &str,
    name: &str,
    private: bool,
) -> Result<RemoteRepoInfo, NewProjectError> {
    let (status, body) = proxy_post(
        bearer,
        "/api/v1/operations/github/create-repo",
        serde_json::json!({ "owner": owner, "name": name, "private": private }),
    )
    .await?;
    if !status.is_success() {
        let body_text = body.to_string();
        if body_text.contains("user_authorization_required") {
            let mut err = NewProjectError::with_field(
                "user_authorization_required",
                "owner",
                format!(
                    "GitHub authorization is required to create repositories on '{}'.",
                    owner
                ),
            );
            err.authorize_url = find_string_key(&body, &["authorize_url", "authorizeUrl"]);
            return Err(err);
        }
        if body_text.contains("name_taken") {
            return Err(NewProjectError::with_field(
                "name_taken",
                "name",
                format!("A repository named '{}/{}' already exists.", owner, name),
            ));
        }
        return Err(backend_error("Repository creation", status, &body));
    }
    let fallback = reconstructed_remote(owner, name);
    Ok(RemoteRepoInfo {
        full_name: find_string_key(&body, &["full_name", "fullName"]).unwrap_or(fallback.full_name),
        clone_url: find_string_key(&body, &["clone_url", "cloneUrl"]).unwrap_or(fallback.clone_url),
        html_url: find_string_key(&body, &["html_url", "htmlUrl"]).unwrap_or(fallback.html_url),
    })
}

// ============================================================================
// Tauri commands
// ============================================================================

/// Check a project name: local validation/normalization always; the
/// GitHub-side availability check (via the web proxy) when `owner` is given.
#[tauri::command]
pub async fn check_project_name(
    owner: Option<String>,
    name: String,
) -> Result<NameCheckResult, String> {
    let raw = name.trim();
    if raw.is_empty() || raw == "." || raw == ".." {
        return Ok(NameCheckResult {
            status: "invalid".to_string(),
            reason: Some(if raw.is_empty() {
                "Project name cannot be empty".to_string()
            } else {
                format!("'{}' is not a valid project name", raw)
            }),
            normalized_name: String::new(),
        });
    }
    let normalized = normalize_repo_name(raw);
    if let Err(reason) = validate_repo_name(&normalized) {
        return Ok(NameCheckResult {
            status: "invalid".to_string(),
            reason: Some(reason),
            normalized_name: normalized,
        });
    }

    let owner = owner
        .map(|o| o.trim().to_string())
        .filter(|o| !o.is_empty());
    let Some(owner) = owner else {
        // Local-only check.
        return Ok(NameCheckResult {
            status: "available".to_string(),
            reason: None,
            normalized_name: normalized,
        });
    };

    let Some(bearer) = crate::commands::setup_wizard::cognito_bearer().await else {
        return Err("Sign in to your Qontinui account to check GitHub name availability.".into());
    };
    check_name_remote(&bearer, &owner, raw)
        .await
        .map_err(|e| e.message)
}

/// Whether the signed-in user has authorized the qontinui-dev GitHub App for
/// `owner`, and the authorize URL to open when they haven't.
#[tauri::command]
pub async fn github_oauth_status(owner: String) -> Result<GithubOauthStatusResult, String> {
    let Some(bearer) = crate::commands::setup_wizard::cognito_bearer().await else {
        return Err("Sign in to your Qontinui account to check GitHub authorization.".into());
    };
    let url = api_url(&format!(
        "/api/v1/operations/github/oauth-status?owner={}",
        urlencoding::encode(owner.trim())
    ));
    let resp = reqwest::Client::new()
        .get(&url)
        .bearer_auth(&bearer)
        .send()
        .await
        .map_err(|e| format!("Failed to reach Qontinui backend: {}", e))?;
    let status = resp.status();
    let body = resp.json::<Value>().await.unwrap_or(Value::Null);
    if !status.is_success() {
        return Err(backend_error("OAuth status", status, &body).message);
    }
    Ok(GithubOauthStatusResult {
        authorized: body
            .get("authorized")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        authorize_url: find_string_key(&body, &["authorize_url", "authorizeUrl"]),
    })
}

fn emit_progress(app: &AppHandle, step: &str, status: &str, detail: impl Into<String>) {
    let payload = ProgressEvent {
        step: step.to_string(),
        status: status.to_string(),
        detail: detail.into(),
    };
    if let Err(e) = app.emit(NEW_PROJECT_PROGRESS_EVENT, &payload) {
        warn!("new_project: failed to emit progress event: {}", e);
    }
}

/// Create a new project end-to-end. Emits [`NEW_PROJECT_PROGRESS_EVENT`]
/// per step; never returns `Err` for step failures — those come back as an
/// `ok: false` result carrying the typed error + resume token.
#[tauri::command]
pub async fn create_new_project(
    app: AppHandle,
    storage: tauri::State<'_, StorageCompartment>,
    req: CreateNewProjectRequest,
) -> Result<CreateNewProjectResult, String> {
    let name = req.name.trim().to_string();
    let owner = req
        .owner
        .as_ref()
        .map(|o| o.trim().to_string())
        .filter(|o| !o.is_empty());
    let target = Path::new(req.location.trim()).join(&name);
    let target_str = target.to_string_lossy().to_string();

    let mut completed: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut remote: Option<RemoteRepoInfo> = None;

    let fail = |step: &str,
                completed: Vec<String>,
                warnings: Vec<String>,
                repo: Option<RemoteRepoInfo>,
                error: NewProjectError| CreateNewProjectResult {
        ok: false,
        project_path: target_str.clone(),
        repo,
        completed_steps: completed,
        failed_step: Some(step.to_string()),
        error: Some(error),
        resume_token: Some(step.to_string()),
        warnings,
    };

    // Resolve the resume point up front so a bad token fails fast.
    let resume_idx = match req.resume_from.as_deref() {
        None => 0,
        Some(s) => match step_index(s) {
            Some(i) => i,
            None => {
                let e = NewProjectError::new(
                    "invalid_resume_token",
                    format!("Unknown resume step '{}'", s),
                );
                emit_progress(&app, "validate", "failed", e.message.clone());
                return Ok(fail("validate", completed, warnings, None, e));
            }
        },
    };
    let resuming = resume_idx > 0;

    info!(
        "new_project: creating '{}' at {} (owner={:?}, template={}, resume_from={:?})",
        name, target_str, owner, req.template, req.resume_from
    );

    // Cognito bearer — required only for the remote steps.
    let bearer: Option<String> = if owner.is_some() {
        crate::commands::setup_wizard::cognito_bearer().await
    } else {
        None
    };
    let need_bearer = |step: &str| -> Result<String, NewProjectError> {
        bearer.clone().ok_or_else(|| {
            NewProjectError::new(
                "not_signed_in",
                format!(
                    "Sign in to your Qontinui account to run the '{}' step.",
                    step
                ),
            )
        })
    };

    // ---- Step 1: validate -------------------------------------------------
    {
        let step = "validate";
        emit_progress(&app, step, "running", "Validating name and location");
        let outcome: Result<(), NewProjectError> = async {
            if req.template != templates::TEMPLATE_EMPTY
                && req.template != templates::TEMPLATE_REACT_UI_BRIDGE
            {
                return Err(NewProjectError::with_field(
                    "invalid_template",
                    "template",
                    format!("Unknown template '{}'", req.template),
                ));
            }
            if let Err(reason) = validate_repo_name(&name) {
                let normalized = normalize_repo_name(&name);
                let hint = if !normalized.is_empty() && normalized != name {
                    format!("{} — did you mean '{}'?", reason, normalized)
                } else {
                    reason
                };
                return Err(NewProjectError::with_field("invalid_name", "name", hint));
            }
            // git preflight
            let out = run_git(vec!["--version".to_string()], None).await?;
            if !out.status.success() {
                return Err(NewProjectError::new(
                    "git_missing",
                    "git is installed but `git --version` failed",
                ));
            }
            // location must be an existing directory
            let parent = Path::new(req.location.trim());
            if req.location.trim().is_empty() || !parent.is_dir() {
                return Err(NewProjectError::with_field(
                    "invalid_location",
                    "location",
                    format!("Location does not exist: {}", parent.display()),
                ));
            }
            // target must not exist, or be an empty dir, or (resuming) be a
            // partial project THIS flow created — proven by the flow-owned
            // marker written under .git/ in init_local, NOT merely by the
            // presence of a .git dir (which any repo has). Without this a
            // resume pointed at an arbitrary existing repo would let scaffold
            // overwrite and commit over it.
            if target.exists() {
                let empty = std::fs::read_dir(&target)
                    .map(|mut e| e.next().is_none())
                    .unwrap_or(false);
                let ours = resuming && target.join(".git").join(FLOW_MARKER).exists();
                if !empty && !ours {
                    return Err(NewProjectError::with_field(
                        "target_exists",
                        "location",
                        format!(
                            "A non-empty folder already exists at {}. Remove it or choose a \
                             different name/location.",
                            target.display()
                        ),
                    ));
                }
            }
            Ok(())
        }
        .await;
        match outcome {
            Ok(()) => {
                emit_progress(&app, step, "ok", "Validated");
                completed.push(step.to_string());
            }
            Err(e) => {
                emit_progress(&app, step, "failed", e.message.clone());
                return Ok(fail(step, completed, warnings, None, e));
            }
        }
    }

    // ---- Step 2: create_remote (skipped for local-only) -------------------
    {
        let step = "create_remote";
        if let Some(owner) = &owner {
            emit_progress(
                &app,
                step,
                "running",
                format!("Creating GitHub repository {}/{}", owner, name),
            );
            let outcome: Result<RemoteRepoInfo, NewProjectError> = async {
                let bearer = need_bearer(step)?;
                if step_index(step).unwrap_or(0) < resume_idx {
                    // Re-verify: the repo existing under this owner/name means
                    // the previous run created it.
                    let check = check_name_remote(&bearer, owner, &name).await?;
                    if check.status == "taken" {
                        return Ok(reconstructed_remote(owner, &name));
                    }
                }
                create_remote_repo(&bearer, owner, &name, req.private).await
            }
            .await;
            match outcome {
                Ok(info) => {
                    emit_progress(&app, step, "ok", format!("Repository {}", info.full_name));
                    completed.push(step.to_string());
                    remote = Some(info);
                }
                Err(e) => {
                    emit_progress(&app, step, "failed", e.message.clone());
                    return Ok(fail(step, completed, warnings, None, e));
                }
            }
        } else {
            emit_progress(&app, step, "skipped", "Local-only project");
            completed.push(step.to_string());
        }
    }

    // ---- Step 3: init_local -----------------------------------------------
    {
        let step = "init_local";
        emit_progress(&app, step, "running", "Initializing git repository");
        let outcome: Result<(), NewProjectError> = async {
            std::fs::create_dir_all(&target).map_err(|e| {
                NewProjectError::new(
                    "io",
                    format!("Failed to create {}: {}", target.display(), e),
                )
            })?;
            if !target.join(".git").exists() {
                git_ok(&["init", "-b", "main"], Some(&target)).await?;
            }
            // Flow-owned marker inside .git/ (never committed, never leaves this
            // machine) proving a later resume that this tree is one WE created —
            // see the `ours` gate in validate. Best-effort: a resume simply
            // falls back to the target_exists guard if it's missing.
            let _ = std::fs::write(target.join(".git").join(FLOW_MARKER), b"1");
            Ok(())
        }
        .await;
        match outcome {
            Ok(()) => {
                emit_progress(&app, step, "ok", "git init -b main");
                completed.push(step.to_string());
            }
            Err(e) => {
                emit_progress(&app, step, "failed", e.message.clone());
                return Ok(fail(step, completed, warnings, remote, e));
            }
        }
    }

    let app_id = derive_app_id(&name);

    // ---- Step 4: scaffold ---------------------------------------------------
    {
        let step = "scaffold";
        emit_progress(
            &app,
            step,
            "running",
            format!("Writing '{}' template", req.template),
        );
        let outcome: Result<usize, NewProjectError> = async {
            if step_index(step).unwrap_or(0) < resume_idx {
                // Already scaffolded on a previous run. Re-writing would clobber
                // any edits the user made to the scaffolded files between the
                // failure and the retry, and stage them into the next commit.
                return Ok(0);
            }
            let files = templates::template_files(&req.template, &name, &app_id)
                .map_err(|msg| NewProjectError::with_field("invalid_template", "template", msg))?;
            let count = files.len();
            for file in files {
                let dest = target.join(file.path.replace('/', std::path::MAIN_SEPARATOR_STR));
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        NewProjectError::new(
                            "io",
                            format!("Failed to create {}: {}", parent.display(), e),
                        )
                    })?;
                }
                std::fs::write(&dest, file.content.as_bytes()).map_err(|e| {
                    NewProjectError::new("io", format!("Failed to write {}: {}", dest.display(), e))
                })?;
            }
            Ok(count)
        }
        .await;
        match outcome {
            Ok(count) => {
                emit_progress(&app, step, "ok", format!("Wrote {} files", count));
                completed.push(step.to_string());
            }
            Err(e) => {
                emit_progress(&app, step, "failed", e.message.clone());
                return Ok(fail(step, completed, warnings, remote, e));
            }
        }
    }

    // ---- Step 5: commit -----------------------------------------------------
    {
        let step = "commit";
        emit_progress(&app, step, "running", "Creating initial commit");
        let outcome: Result<(), NewProjectError> = async {
            if step_index(step).unwrap_or(0) < resume_idx
                && git_probe(&["rev-parse", "--verify", "HEAD"], Some(&target)).await
            {
                return Ok(()); // already committed on a previous run
            }
            git_ok(&["add", "-A"], Some(&target)).await?;
            let msg = format!("chore: initialize {}", name);
            match git_ok(&["commit", "-m", &msg], Some(&target)).await {
                Ok(()) => Ok(()),
                // "nothing to commit" after a resume where the commit landed
                // but the resume token pointed earlier — accept iff HEAD exists.
                Err(e) => {
                    if git_probe(&["rev-parse", "--verify", "HEAD"], Some(&target)).await {
                        Ok(())
                    } else {
                        Err(e)
                    }
                }
            }
        }
        .await;
        match outcome {
            Ok(()) => {
                emit_progress(&app, step, "ok", "Initial commit created");
                completed.push(step.to_string());
            }
            Err(e) => {
                emit_progress(&app, step, "failed", e.message.clone());
                return Ok(fail(step, completed, warnings, remote, e));
            }
        }
    }

    // ---- Step 6: push (skipped for local-only) ------------------------------
    {
        let step = "push";
        if let Some(owner) = &owner {
            emit_progress(&app, step, "running", "Pushing to GitHub");
            let info = remote
                .clone()
                .unwrap_or_else(|| reconstructed_remote(owner, &name));
            let outcome: Result<(), NewProjectError> = async {
                if step_index(step).unwrap_or(0) < resume_idx
                    && git_probe(&["config", "--get", "remote.origin.url"], Some(&target)).await
                    && git_probe(
                        &[
                            "rev-parse",
                            "--verify",
                            "--quiet",
                            "refs/remotes/origin/main",
                        ],
                        Some(&target),
                    )
                    .await
                {
                    return Ok(()); // origin wired + main pushed on a previous run
                }

                let bearer = need_bearer(step)?;
                let (status, body) = proxy_post(
                    &bearer,
                    "/api/v1/operations/github/push-credential",
                    serde_json::json!({ "owner": owner, "name": name }),
                )
                .await?;
                if !status.is_success() {
                    return Err(backend_error("Push credential", status, &body));
                }
                let token = body
                    .get("token")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        NewProjectError::new("backend", "Backend did not return a push token")
                    })?
                    .to_string();

                let clean_url = info.clone_url.clone();
                let auth_url = format!(
                    "https://x-access-token:{}@github.com/{}/{}.git",
                    token, owner, name
                );

                // Ensure origin exists, then point it at the transient authed
                // URL for the one push (same hygiene as github_clone_repo).
                if git_ok(&["remote", "add", "origin", &clean_url], Some(&target))
                    .await
                    .is_err()
                {
                    git_ok(&["remote", "set-url", "origin", &clean_url], Some(&target)).await?;
                }
                if let Err(mut e) =
                    git_ok(&["remote", "set-url", "origin", &auth_url], Some(&target)).await
                {
                    // This is the one token-bearing git call outside the push
                    // redaction below; scrub before the error can surface.
                    e.message = redact(&e.message, &token);
                    return Err(e);
                }

                let push_result = git_ok(&["push", "-u", "origin", "main"], Some(&target)).await;

                // Scrub the token from .git/config regardless of push outcome.
                let scrubbed =
                    git_ok(&["remote", "set-url", "origin", &clean_url], Some(&target)).await;

                if let Err(mut e) = push_result {
                    e.message = redact(&e.message, &token);
                    return Err(e);
                }
                if scrubbed.is_err() {
                    // Push succeeded but the short-TTL token is still in the
                    // remote URL — surface loudly, don't fail the project.
                    warn!("new_project: failed to scrub push token from remote URL");
                    return Err(NewProjectError::new(
                        "git",
                        "Push succeeded but the temporary credential could not be removed from \
                         .git/config. Run `git remote set-url origin <repo-url>` manually.",
                    ));
                }
                Ok(())
            }
            .await;
            match outcome {
                Ok(()) => {
                    emit_progress(
                        &app,
                        step,
                        "ok",
                        format!("Pushed main to {}", info.full_name),
                    );
                    completed.push(step.to_string());
                    remote = Some(info);
                }
                Err(e) => {
                    emit_progress(&app, step, "failed", e.message.clone());
                    return Ok(fail(step, completed, warnings, Some(info), e));
                }
            }
        } else {
            emit_progress(&app, step, "skipped", "Local-only project");
            completed.push(step.to_string());
        }
    }

    // ---- Step 7: enroll -----------------------------------------------------
    {
        let step = "enroll";
        match &owner {
            Some(owner) if req.enroll => {
                if step_index(step).unwrap_or(0) < resume_idx {
                    // Enrollment completed in a previous run (resume_from is
                    // past it); the server-side path is idempotent anyway.
                    emit_progress(&app, step, "skipped", "Already enrolled");
                    completed.push(step.to_string());
                } else {
                    emit_progress(&app, step, "running", "Enrolling with Qontinui");
                    let outcome: Result<(), NewProjectError> = async {
                        let bearer = need_bearer(step)?;
                        let (status, body) = proxy_post(
                            &bearer,
                            "/api/v1/operations/github/enroll-repo",
                            serde_json::json!({ "owner": owner, "name": name }),
                        )
                        .await?;
                        if !status.is_success() {
                            let mut e = backend_error("Enrollment", status, &body);
                            e.code = "enroll_failed".to_string();
                            return Err(e);
                        }
                        Ok(())
                    }
                    .await;
                    match outcome {
                        Ok(()) => {
                            emit_progress(&app, step, "ok", "Enrolled with Qontinui");
                            completed.push(step.to_string());
                        }
                        Err(e) => {
                            emit_progress(&app, step, "failed", e.message.clone());
                            return Ok(fail(step, completed, warnings, remote, e));
                        }
                    }
                }
            }
            _ => {
                emit_progress(&app, step, "skipped", "Enrollment not requested");
                completed.push(step.to_string());
            }
        }
    }

    // ---- Step 8: register ---------------------------------------------------
    {
        let step = "register";
        emit_progress(&app, step, "running", "Registering project");
        let project = SavedProject {
            // `id` is left defaulted (empty): `add_saved_project` mints the
            // UUID so there is exactly one place that assigns project ids.
            path: target_str.clone(),
            name: name.clone(),
            project_type: templates::project_type_for_template(&req.template).to_string(),
            manifest: templates::manifest_for_template(&req.template).to_string(),
            // We just created the repo, so the slug is known here without
            // shelling out to `git remote get-url` later.
            repo_slug: remote.as_ref().map(|r| r.full_name.clone()),
            ..Default::default()
        };
        if let Err(msg) = crate::commands::saved_projects::add_saved_project(project) {
            let e = NewProjectError::new("io", format!("Failed to save project: {}", msg));
            emit_progress(&app, step, "failed", e.message.clone());
            return Ok(fail(step, completed, warnings, remote, e));
        }

        // UI-Bridge templates additionally join the multi-app registry so the
        // Spec API can serve them. Registry failures degrade to warnings —
        // they never fail the project.
        if req.template == templates::TEMPLATE_REACT_UI_BRIDGE {
            use qontinui_types::apps::{AppError, RegisterAppRequest};
            let reg = RegisterAppRequest::new(
                app_id.clone(),
                target_str.clone(),
                // Vite dev-server default; PATCHable later via the apps API.
                "http://localhost:5173",
                name.clone(),
            );
            match storage.pg_db().insert_app(&reg).await {
                Ok(_) => info!("new_project: registered app '{}' in app registry", app_id),
                Err(AppError::AlreadyRegistered { .. }) => {
                    info!(
                        "new_project: app '{}' already registered (idempotent)",
                        app_id
                    );
                }
                Err(e) => {
                    warn!("new_project: app-registry registration failed: {}", e);
                    warnings.push(format!(
                        "App-registry registration failed (project is fine): {}",
                        e
                    ));
                }
            }
        }

        emit_progress(&app, step, "ok", "Project registered");
        completed.push(step.to_string());
    }

    info!("new_project: '{}' created at {}", name, target_str);
    Ok(CreateNewProjectResult {
        ok: true,
        project_path: target_str,
        repo: remote,
        completed_steps: completed,
        failed_step: None,
        error: None,
        resume_token: None,
        warnings,
    })
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_passes_valid_names_through() {
        assert_eq!(normalize_repo_name("my-app"), "my-app");
        assert_eq!(normalize_repo_name("My.App_2"), "My.App_2");
    }

    #[test]
    fn normalize_collapses_invalid_runs_to_single_dash() {
        assert_eq!(normalize_repo_name("My App"), "My-App");
        assert_eq!(normalize_repo_name("my   app"), "my-app");
        assert_eq!(normalize_repo_name("héllo wörld"), "h-llo-w-rld");
    }

    #[test]
    fn normalize_trims_and_drops_leading_invalids() {
        assert_eq!(normalize_repo_name("  my app  "), "my-app");
        // Leading invalid chars produce no leading dash.
        assert_eq!(normalize_repo_name("!!!app"), "app");
    }

    #[test]
    fn validate_rejects_empty_and_dots() {
        assert!(validate_repo_name("").is_err());
        assert!(validate_repo_name(".").is_err());
        assert!(validate_repo_name("..").is_err());
    }

    #[test]
    fn validate_rejects_invalid_chars_and_long_names() {
        assert!(validate_repo_name("has space").is_err());
        assert!(validate_repo_name("emoji🙂").is_err());
        assert!(validate_repo_name(&"a".repeat(101)).is_err());
        assert!(validate_repo_name(&"a".repeat(100)).is_ok());
    }

    #[test]
    fn validate_accepts_github_style_names() {
        for n in ["my-app", "My.App", "app_2", "...dots", "a"] {
            assert!(validate_repo_name(n).is_ok(), "{} should be valid", n);
        }
    }

    #[test]
    fn derive_app_id_produces_valid_slugs() {
        assert_eq!(derive_app_id("My.App"), "my-app");
        assert_eq!(derive_app_id("My App 2"), "my-app-2");
        assert_eq!(derive_app_id("---"), "app");
        assert_eq!(derive_app_id("_Leading_"), "leading");
        let long = derive_app_id(&"x".repeat(200));
        assert_eq!(long.len(), 64);
        // Every derived id must satisfy the registry's slug rules.
        for n in ["My.App", "a", "0number", "___", "Ünïcode Nämé"] {
            assert!(
                qontinui_types::apps::validate_app_id(&derive_app_id(n)).is_ok(),
                "derive_app_id({:?}) produced an invalid slug",
                n
            );
        }
    }

    #[test]
    fn step_index_matches_declared_order() {
        assert_eq!(step_index("validate"), Some(0));
        assert_eq!(step_index("create_remote"), Some(1));
        assert_eq!(step_index("register"), Some(7));
        assert_eq!(step_index("nonsense"), None);
    }

    #[test]
    fn redact_strips_token_everywhere() {
        let msg = "push https://x-access-token:sekret@github.com failed: sekret expired";
        assert_eq!(
            redact(msg, "sekret"),
            "push https://x-access-token:***@github.com failed: *** expired"
        );
        assert_eq!(redact("unchanged", ""), "unchanged");
    }

    #[test]
    fn reconstructed_remote_is_deterministic() {
        let r = reconstructed_remote("octo", "app");
        assert_eq!(r.full_name, "octo/app");
        assert_eq!(r.clone_url, "https://github.com/octo/app.git");
        assert_eq!(r.html_url, "https://github.com/octo/app");
    }

    #[test]
    fn find_string_key_searches_nested_and_camel() {
        let v = serde_json::json!({
            "detail": { "error": "user_authorization_required", "authorizeUrl": "https://x" }
        });
        assert_eq!(
            find_string_key(&v, &["authorize_url", "authorizeUrl"]),
            Some("https://x".to_string())
        );
        assert_eq!(find_string_key(&v, &["missing"]), None);
    }
}
