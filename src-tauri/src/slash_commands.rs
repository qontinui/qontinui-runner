//! Slash command import: scan, parse, and sync Claude Code slash commands as runner workflows.
//!
//! Scans `qontinui-claude-config/.claude/commands/*.md` for command files,
//! parses each into a workflow, and syncs with the database (create/update/delete).

use crate::capability_manifest::{record_observation, record_provision, ProvisionReport, Rung};
use crate::database::pg::parse_workflow_id;
use crate::mcp::types::ApiResponse;
use crate::str_utils::truncate_str;
use crate::unified_workflows::CreateUnifiedWorkflowRequest;
use axum::http::StatusCode;
use qontinui_types::paths::{SiblingCheckoutAbsent, SiblingFallback, WorkspaceRootUnresolved};
use qontinui_types::refusal::RefusalSource;
use regex::Regex;
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

/// Parsed representation of a single slash command markdown file.
#[derive(Debug, Clone)]
struct SlashCommandParsed {
    /// Filename, e.g. "fix.md"
    file_name: String,
    /// Command name derived from filename, e.g. "fix"
    command_name: String,
    /// Title extracted from H1 header, e.g. "Autonomous Bug Fix"
    title: String,
    /// Short description (first paragraph after H1), truncated to 200 chars
    description: String,
    /// Full markdown body (everything after H1)
    full_content: String,
    /// Whether $ARGUMENTS placeholder is present
    has_arguments: bool,
    /// SHA-256 hex digest of the raw file content
    content_hash: String,
    /// Relative path from workspace root, e.g. "qontinui-claude-config/.claude/commands/fix.md"
    relative_path: String,
}

/// Result of a sync operation.
#[derive(Debug, Clone, Serialize)]
pub struct SyncResult {
    pub created: usize,
    pub updated: usize,
    pub deleted: usize,
    pub unchanged: usize,
    pub errors: Vec<String>,
}

/// The sibling checkout this import scans. It has no embedded or bundled copy.
const COMMANDS_SIBLING: &str = "qontinui-claude-config";

/// Why a sync could not run. The first two arms are the device simply not
/// having the checkout this import reads, which is the normal state of an
/// installed runner, so each one maps to the shared typed refusal (a `422`
/// carrying the schemas `Refusal`, next action included) instead of a `500`.
/// Only [`SyncError::Failed`] is a fault.
#[derive(Debug)]
pub enum SyncError {
    /// No workspace root resolved, so there was nowhere to look for the
    /// checkout.
    RootUnresolved(WorkspaceRootUnresolved),
    /// A workspace root resolved but holds no
    /// `qontinui-claude-config/.claude/commands`. Nothing else can serve the
    /// import, so this is the unsupported arm of
    /// [`SiblingCheckoutAbsent`].
    CheckoutAbsent { commands_dir: PathBuf },
    /// The checkout was there and the sync still failed: an unreadable
    /// directory or an unavailable database pool.
    Failed(String),
}

impl SyncError {
    fn sibling_absent() -> SiblingCheckoutAbsent {
        SiblingCheckoutAbsent::new(COMMANDS_SIBLING, SiblingFallback::Unsupported)
    }

    /// The HTTP status and error body for this failure. The two absent-checkout
    /// arms answer `422` with the schemas `Refusal` in `hint` and its code in
    /// `code` (the shape of [`ApiResponse::workspace_root_unresolved`]); a
    /// genuine fault stays a `500`.
    pub fn into_response(self) -> (StatusCode, ApiResponse<()>) {
        match self {
            SyncError::RootUnresolved(unresolved) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                ApiResponse::workspace_root_unresolved(&unresolved),
            ),
            SyncError::CheckoutAbsent { .. } => {
                let absent = Self::sibling_absent();
                // `Unsupported` always yields an envelope; `refusal` is `None`
                // only on the embedded-copy arm, which this import has not got.
                let refusal =
                    absent.refusal(RefusalSource::Runner, chrono::Utc::now().to_rfc3339());
                (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    ApiResponse {
                        success: false,
                        data: None,
                        error: Some(absent.to_string()),
                        error_detail: None,
                        code: refusal.as_ref().map(|r| r.code.as_str().to_string()),
                        hint: refusal.and_then(|r| serde_json::to_value(r).ok()),
                        suggestions: None,
                    },
                )
            }
            SyncError::Failed(why) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                ApiResponse::error(format!("Slash command sync failed: {why}")),
            ),
        }
    }
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncError::RootUnresolved(unresolved) => write!(f, "{unresolved}"),
            SyncError::CheckoutAbsent { commands_dir } => write!(
                f,
                "{} Looked for {}.",
                Self::sibling_absent(),
                commands_dir.display()
            ),
            SyncError::Failed(why) => f.write_str(why),
        }
    }
}

/// Find the commands directory relative to the workspace root, **and report
/// which rung answered**.
///
/// This import has no embedded or bundled fallback of any kind: it is a scan of
/// a SIBLING checkout (`qontinui-claude-config`) and nothing else. On a device
/// without that repo it does not degrade — it cannot run at all. The report
/// makes that a stated [`Rung::Unresolved`](crate::capability_manifest::Rung)
/// finding, and the typed [`SyncError`] makes it a refusal at the HTTP route
/// rather than a `500`.
///
/// The workspace root comes from the runner's one resolver
/// ([`crate::workspace_paths::runner_workspace_root`]), so an unresolved root is
/// its typed `WorkspaceRootUnresolved` — never a sentence of this module's own.
fn find_commands_directory_reported() -> (Result<(PathBuf, PathBuf), SyncError>, ProvisionReport) {
    commands_directory_in(crate::workspace_paths::runner_workspace_root().require())
}

/// [`find_commands_directory_reported`] over an already-resolved workspace
/// root, so the absent-checkout arms are testable without the ambient resolver.
fn commands_directory_in(
    workspace_root: Result<PathBuf, WorkspaceRootUnresolved>,
) -> (Result<(PathBuf, PathBuf), SyncError>, ProvisionReport) {
    let workspace_root = match workspace_root {
        Ok(root) => root,
        Err(unresolved) => {
            let report = ProvisionReport::unresolved(
                "slash_commands",
                0,
                "<workspace-root>/qontinui-claude-config/.claude/commands",
                unresolved.to_string(),
            );
            return (Err(SyncError::RootUnresolved(unresolved)), report);
        }
    };
    let commands_dir = workspace_root
        .join(COMMANDS_SIBLING)
        .join(".claude")
        .join("commands");

    if !commands_dir.is_dir() {
        let report = ProvisionReport::unresolved(
            "slash_commands",
            0,
            commands_dir.display().to_string(),
            "this device has no qontinui-claude-config checkout, and this import has \
             no embedded or bundled fallback — the workflows simply do not exist here",
        );
        return (Err(SyncError::CheckoutAbsent { commands_dir }), report);
    }

    let report = ProvisionReport::new("slash_commands", 0, Rung::OperatorCheckout)
        .with_destination(commands_dir.display().to_string());
    (Ok((workspace_root, commands_dir)), report)
}

/// Parse a single markdown file into a SlashCommandParsed.
fn parse_command_file(
    file_path: &Path,
    workspace_root: &Path,
) -> Result<SlashCommandParsed, String> {
    let raw_content = std::fs::read(file_path)
        .map_err(|e| format!("Failed to read {}: {}", file_path.display(), e))?;

    let content = String::from_utf8_lossy(&raw_content).to_string();

    // Compute SHA-256 hash of raw bytes
    let mut hasher = Sha256::new();
    hasher.update(&raw_content);
    let content_hash = format!("{:x}", hasher.finalize());

    let file_name = file_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown.md")
        .to_string();

    let command_name = file_name.trim_end_matches(".md").to_string();

    // Extract title from first H1 line
    let h1_re = Regex::new(r"(?m)^# (.+)$").unwrap();
    let title = h1_re
        .captures(&content)
        .and_then(|cap| cap.get(1))
        .map(|m| m.as_str().trim().to_string())
        .unwrap_or_else(|| {
            // Fallback: title-case the command name
            command_name
                .replace('-', " ")
                .split_whitespace()
                .map(|w| {
                    let mut c = w.chars();
                    match c.next() {
                        None => String::new(),
                        Some(f) => f.to_uppercase().to_string() + c.as_str(),
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        });

    // Extract description: text between H1 and first H2 or blank-line-then-non-text
    let description = extract_description(&content);

    // Full content is everything (we pass the entire markdown as the prompt)
    let full_content = content.clone();

    // Check for $ARGUMENTS
    let has_arguments = content.contains("$ARGUMENTS");

    // Compute relative path
    let relative_path = file_path
        .strip_prefix(workspace_root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| file_path.to_string_lossy().replace('\\', "/"));

    Ok(SlashCommandParsed {
        file_name,
        command_name,
        title,
        description,
        full_content,
        has_arguments,
        content_hash,
        relative_path,
    })
}

/// Extract description: first paragraph of text after the H1 line.
fn extract_description(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let mut found_h1 = false;
    let mut desc_lines: Vec<&str> = Vec::new();

    for line in &lines {
        if !found_h1 {
            if line.starts_with("# ") && !line.starts_with("## ") {
                found_h1 = true;
            }
            continue;
        }

        let trimmed = line.trim();

        // Skip blank lines right after H1
        if desc_lines.is_empty() && trimmed.is_empty() {
            continue;
        }

        // Stop at next heading or blank line after we've started collecting
        if trimmed.is_empty() || trimmed.starts_with("## ") || trimmed.starts_with("# ") {
            break;
        }

        desc_lines.push(trimmed);
    }

    let desc = desc_lines.join(" ");
    if desc.len() > 200 {
        format!("{}...", truncate_str(&desc, 197))
    } else {
        desc
    }
}

/// Build a CreateUnifiedWorkflowRequest from a parsed slash command.
fn build_workflow_request(parsed: &SlashCommandParsed) -> CreateUnifiedWorkflowRequest {
    let mut tags = vec!["slash-command".to_string(), parsed.command_name.clone()];
    if parsed.has_arguments {
        tags.push("requires-arguments".to_string());
    } else {
        tags.push("no-arguments".to_string());
    }

    CreateUnifiedWorkflowRequest {
        name: parsed.title.clone(),
        description: parsed.description.clone(),
        category: "slash-command".to_string(),
        tags,
        setup_steps: vec![],
        verification_steps: vec![],
        agentic_steps: vec![json!({
            "type": "prompt",
            "name": parsed.command_name,
            "phase": "agentic",
            "content": parsed.full_content
        })],
        completion_steps: vec![],
        // Slash-command runs are uncapped — the command itself decides when
        // it's done. Was previously `1`, which caused early termination.
        max_iterations: None,
        timeout_seconds: None,
        provider: None,
        model: None,
        skip_ai_summary: true,
        log_source_selection: None,
        context_ids: None,
        disabled_context_ids: None,
        auto_include_contexts: Some(true),
        prompt_template: None,
        log_watch_enabled: Some(false),
        health_check_enabled: Some(false),
        health_check_urls: None,
        preflight_check_enabled: Some(false),
        enable_sweep: Some(false),
        max_sweep_iterations: None,
        targeted_error_ids: None,
        generated_by_task_run_id: None,
        stages: None,
        stop_on_failure: Some(false),
        constraint_overrides: None,
        approval_gate: Some(false),
        reflection_mode: Some(false),
        completion_prompts_first: None,
        model_overrides: None,
        dependency_graph: None,
        cost_annotations: None,
        quality_report: None,
        acceptance_criteria: None,
        ai_reviewed: None,
        workflow_architecture: None,
        enforce_token_budget: None,
        strict_cwd: false,
        tool_tags: Vec::new(),
        flow_control_json: None,
        phase_timeouts_json: None,
        rollback_policy: None,
        htn_enabled: false,
        htn_ui_bridge_url: None,
        htn_state_machine_path: None,
    }
}

/// Sync slash commands from disk to the database (PG-backed).
///
/// Scans the commands directory, compares with existing DB entries,
/// and creates/updates/deletes as needed.
#[expect(
    clippy::disallowed_methods,
    reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
)]
pub async fn sync_slash_commands(
    pg: &std::sync::Arc<crate::database::pg::PgDb>,
) -> Result<SyncResult, SyncError> {
    let (resolved, mut report) = find_commands_directory_reported();
    let (workspace_root, commands_dir) = match resolved {
        Ok(paths) => paths,
        Err(e) => {
            // The absent checkout is a manifest row AND a typed error the
            // route renders as the shared refusal. Returning before the
            // database is touched also means an absent checkout never deletes
            // previously imported workflows.
            record_observation("slash_commands", report.observation());
            return Err(e);
        }
    };

    // Parse all command files from disk
    let mut disk_commands: HashMap<String, SlashCommandParsed> = HashMap::new();
    let entries = std::fs::read_dir(&commands_dir)
        .map_err(|e| SyncError::Failed(format!("Failed to read commands directory: {}", e)))?;

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "md") {
            report.expected += 1;
            match parse_command_file(&path, &workspace_root) {
                Ok(parsed) => {
                    disk_commands.insert(parsed.relative_path.clone(), parsed);
                    report.record_written();
                }
                Err(e) => {
                    warn!("Failed to parse command file {:?}: {}", path, e);
                    report.skip(
                        path.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| path.display().to_string()),
                        crate::capability_manifest::SkipReason::WriteFailed(e),
                    );
                }
            }
        }
    }
    if report.written == 0 {
        // The checkout was there and yielded nothing usable — `unresolved`
        // rather than a claim that the operator checkout answered.
        report.set_rung(Rung::Unresolved);
    }
    record_provision(&commands_dir.display().to_string(), report);

    // Get existing slash command workflows from PG
    let existing = pg.list_slash_command_sources().await.unwrap_or_default();
    let existing_map: HashMap<String, (String, String)> = existing
        .into_iter()
        .filter_map(|(id, _name, source)| {
            if source.is_empty() {
                None
            } else {
                Some((source, (id, _name)))
            }
        })
        .collect();

    let mut result = SyncResult {
        created: 0,
        updated: 0,
        deleted: 0,
        unchanged: 0,
        errors: vec![],
    };

    // Create or update workflows for commands on disk
    for (rel_path, parsed) in &disk_commands {
        if let Some((existing_id, _)) = existing_map.get(rel_path) {
            let existing_id_uuid = match parse_workflow_id(existing_id) {
                Ok(u) => u,
                Err(e) => {
                    result
                        .errors
                        .push(format!("Update {}: {}", parsed.command_name, e));
                    continue;
                }
            };
            // Check if content hash changed — if so, update
            let conn = pg
                .pool()
                .get()
                .await
                .map_err(|e| SyncError::Failed(format!("PG pool: {}", e)))?;
            let current_hash: Option<String> = conn
                .query_opt(
                    "SELECT source_content_hash FROM unified_workflows WHERE id = $1",
                    &[&existing_id_uuid],
                )
                .await
                .ok()
                .flatten()
                .and_then(|r| r.get(0));

            if current_hash.as_deref() == Some(&parsed.content_hash) {
                result.unchanged += 1;
            } else {
                // Update the workflow content via direct SQL (simpler than full UpdateRequest).
                // agentic_steps/tags are jsonb (migration
                // d7a3f1c8e024_realign_unified_workflows_to_model) — bind
                // the parsed Value, not a stringified copy.
                let req = build_workflow_request(parsed);
                let agentic_json = serde_json::to_value(&req.agentic_steps)
                    .unwrap_or_else(|_| serde_json::json!([]));
                let tags_json =
                    serde_json::to_value(&req.tags).unwrap_or_else(|_| serde_json::json!([]));
                match conn
                    .execute(
                        r#"UPDATE unified_workflows
                       SET name = $2, description = $3, agentic_steps = $4, tags = $5,
                           source_file_path = $6, source_content_hash = $7, updated_at = NOW()
                       WHERE id = $1"#,
                        &[
                            &existing_id_uuid,
                            &parsed.title,
                            &parsed.description,
                            &agentic_json,
                            &tags_json,
                            &parsed.relative_path,
                            &parsed.content_hash,
                        ],
                    )
                    .await
                {
                    Ok(_) => {
                        result.updated += 1;
                        info!("Updated slash command workflow: {}", parsed.title);
                    }
                    Err(e) => result
                        .errors
                        .push(format!("Update {}: {}", parsed.command_name, e)),
                }
            }
        } else {
            // Create new workflow
            let req = build_workflow_request(parsed);
            match pg.create_unified_workflow(&req).await {
                Ok(wf) => {
                    // Set source tracking fields
                    if let (Ok(conn), Ok(wf_id_uuid)) =
                        (pg.pool().get().await, parse_workflow_id(&wf.id))
                    {
                        let _ = conn.execute(
                            "UPDATE unified_workflows SET source_file_path = $2, source_content_hash = $3 WHERE id = $1",
                            &[&wf_id_uuid, &parsed.relative_path, &parsed.content_hash],
                        ).await;
                    }
                    result.created += 1;
                    info!("Created slash command workflow: {}", parsed.title);
                }
                Err(e) => result
                    .errors
                    .push(format!("Create {}: {}", parsed.command_name, e)),
            }
        }
    }

    // Delete workflows for commands no longer on disk
    for (rel_path, (existing_id, name)) in &existing_map {
        if !disk_commands.contains_key(rel_path) {
            match pg.delete_unified_workflow(existing_id).await {
                Ok(true) => {
                    result.deleted += 1;
                    info!("Deleted slash command workflow: {} ({})", name, rel_path);
                }
                Ok(false) => {
                    warn!(
                        "Slash command workflow {} not found for deletion",
                        existing_id
                    );
                }
                Err(e) => result.errors.push(format!("Delete {}: {}", name, e)),
            }
        }
    }

    info!(
        "Slash command sync: {} created, {} updated, {} deleted, {} unchanged, {} errors",
        result.created,
        result.updated,
        result.deleted,
        result.unchanged,
        result.errors.len()
    );

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_description() {
        let content = "# Autonomous Bug Fix\n\nWork completely autonomously to fix the bug described. Do not ask for clarification.\n\n## Steps\n\nStep 1...";
        let desc = extract_description(content);
        assert_eq!(
            desc,
            "Work completely autonomously to fix the bug described. Do not ask for clarification."
        );
    }

    #[test]
    fn test_extract_description_no_h1() {
        let content = "Just some text without a header.";
        let desc = extract_description(content);
        assert_eq!(desc, "");
    }

    #[test]
    fn test_extract_description_truncation() {
        let long_line = "A".repeat(250);
        let content = format!("# Title\n\n{}\n\n## Next", long_line);
        let desc = extract_description(&content);
        assert!(desc.len() <= 203); // 200 + "..."
        assert!(desc.ends_with("..."));
    }

    #[test]
    fn test_arguments_detection() {
        let with_args = "# Fix\n\nFix stuff.\n\n## Arguments\n\n$ARGUMENTS";
        assert!(with_args.contains("$ARGUMENTS"));

        let without_args = "# Clean\n\nClean stuff.\n\n## Steps\n\nDo things.";
        assert!(!without_args.contains("$ARGUMENTS"));
    }

    /// Regression for coord finding 3dad3856: on a device whose workspace root
    /// resolves but holds no qontinui-claude-config checkout (the parity
    /// harness's dev leg: an exe-relative root with no sibling), the sync used
    /// to fail with a bare string that the route served as a 500. It must be
    /// the typed `sibling_checkout_absent` refusal with a next action instead.
    #[test]
    fn a_root_without_the_commands_checkout_refuses_typed_not_500() {
        let root = tempfile::tempdir().expect("tempdir");
        let (resolved, report) = commands_directory_in(Ok(root.path().to_path_buf()));
        assert_eq!(report.observation().rung, Rung::Unresolved);
        let err = resolved.expect_err("no checkout, no sync");
        assert!(matches!(err, SyncError::CheckoutAbsent { .. }), "{err:?}");

        let (status, body) = err.into_response();
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(!body.success);
        assert_eq!(body.code.as_deref(), Some("sibling_checkout_absent"));
        let hint = body.hint.expect("the Refusal travels in hint");
        assert_eq!(hint["code"], "sibling_checkout_absent");
        assert_eq!(hint["discriminator"], COMMANDS_SIBLING);
        assert_eq!(hint["next_action"]["kind"], "none_terminal");
        assert!(!body.error.unwrap_or_default().is_empty());
    }

    /// The published leg: no workspace root at all. The route answers the
    /// shared workspace-root refusal, not a 500.
    #[test]
    fn an_unresolved_root_refuses_with_the_shared_workspace_root_refusal() {
        use qontinui_types::paths::{RejectedRoot, RootRejection, RootSource};
        let unresolved = WorkspaceRootUnresolved {
            rejected: RejectedRoot {
                source: RootSource::Probes,
                reason: RootRejection::NothingConfigured,
            },
        };
        let (resolved, report) = commands_directory_in(Err(unresolved));
        assert_eq!(report.observation().rung, Rung::Unresolved);
        let (status, body) = resolved.expect_err("no root, no sync").into_response();
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body.code.as_deref(), Some("workspace_root_unresolved"));
        let hint = body.hint.expect("the Refusal travels in hint");
        assert_eq!(hint["code"], "workspace_root_unresolved");
        assert!(hint["next_action"]["kind"].is_string());
    }

    /// A checkout that IS there resolves, and a genuine fault stays a 500.
    #[test]
    fn a_present_checkout_resolves_and_a_real_fault_stays_500() {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = root
            .path()
            .join(COMMANDS_SIBLING)
            .join(".claude")
            .join("commands");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let (resolved, report) = commands_directory_in(Ok(root.path().to_path_buf()));
        assert_eq!(report.observation().rung, Rung::OperatorCheckout);
        let (got_root, got_dir) = resolved.expect("checkout present");
        assert_eq!(got_root, root.path());
        assert_eq!(got_dir, dir);

        let (status, body) = SyncError::Failed("PG pool: down".into()).into_response();
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(body.error.unwrap_or_default().contains("PG pool: down"));
    }

    #[test]
    fn test_content_hash() {
        let content = b"# Test Command\n\nSome content.";
        let mut hasher = Sha256::new();
        hasher.update(content);
        let hash = format!("{:x}", hasher.finalize());
        assert_eq!(hash.len(), 64); // SHA-256 hex is 64 chars
    }
}
