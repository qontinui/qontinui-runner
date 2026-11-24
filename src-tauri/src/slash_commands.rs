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
use std::collections::{HashMap, HashSet};
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

/// A command file's path relative to the workspace root, with forward slashes —
/// the key an imported workflow's `source_file_path` is stored under.
fn relative_source_path(file_path: &Path, workspace_root: &Path) -> String {
    file_path
        .strip_prefix(workspace_root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| file_path.to_string_lossy().replace('\\', "/"))
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

    let relative_path = relative_source_path(file_path, workspace_root);

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

/// The previously imported slash-command workflows, as source path ->
/// (`id`, `name`). Rows with no source path were not written by this import and
/// are left out. A failed read is returned as an error — never as an empty map,
/// which the caller would act on as "nothing was imported yet".
fn existing_source_map(
    listed: Result<Vec<(String, String, String)>, String>,
) -> Result<HashMap<String, (String, String)>, String> {
    let rows =
        listed.map_err(|e| format!("Failed to list imported slash command workflows: {e}"))?;
    Ok(rows
        .into_iter()
        .filter(|(_, _, source)| !source.is_empty())
        .map(|(id, name, source)| (source, (id, name)))
        .collect())
}

/// The imported workflows whose source file is no longer on disk, i.e. the ones
/// to delete. An import whose file is present but `unreadable` is not stale.
///
/// Refuses (with the reason) to delete anything while imports exist and either
/// the directory listing was incomplete (`listing_complete == false`, so a
/// file may be missing from it) or the checkout yielded **no** command at all.
/// An empty yield is far more likely a broken or half-written checkout than a
/// deliberate removal of every command, and a deletion is not undoable: a
/// command restored later comes back under a new id.
fn stale_imports<'a>(
    disk_commands: &HashMap<String, SlashCommandParsed>,
    unreadable: &HashSet<String>,
    listing_complete: bool,
    existing: &'a HashMap<String, (String, String)>,
) -> Result<Vec<(&'a String, &'a (String, String))>, String> {
    if existing.is_empty() {
        return Ok(Vec::new());
    }
    if !listing_complete {
        return Err(format!(
            "Deletion withheld: the commands directory listing was incomplete, so none of \
             the {} previously imported slash command workflow(s) were deleted",
            existing.len()
        ));
    }
    if disk_commands.is_empty() {
        return Err(format!(
            "Deletion withheld: the commands directory yielded no parseable command, so \
             none of the {} previously imported slash command workflow(s) were deleted",
            existing.len()
        ));
    }
    Ok(existing
        .iter()
        .filter(|(rel_path, _)| {
            !disk_commands.contains_key(*rel_path) && !unreadable.contains(*rel_path)
        })
        .collect())
}

/// Record a freshly created workflow's source path and content hash — what
/// makes it findable by the next sync's list read.
async fn track_source(
    pg: &std::sync::Arc<crate::database::pg::PgDb>,
    workflow_id: &str,
    parsed: &SlashCommandParsed,
) -> Result<(), String> {
    let id = parse_workflow_id(workflow_id)?;
    let conn = pg.pool().get().await.map_err(|e| format!("PG pool: {e}"))?;
    let updated = conn
        .execute(
            "UPDATE unified_workflows SET source_file_path = $2, source_content_hash = $3 WHERE id = $1",
            &[&id, &parsed.relative_path, &parsed.content_hash],
        )
        .await
        .map_err(|e| format!("PG update: {e}"))?;
    if updated == 1 {
        Ok(())
    } else {
        Err(format!("updated {updated} rows, expected 1"))
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
    // Command files that are on disk but could not be read: their imports are
    // kept, not treated as removed.
    let mut unreadable: HashSet<String> = HashSet::new();
    // False when a directory entry itself could not be read, so the listing
    // may be missing files this sync cannot name.
    let mut listing_complete = true;
    let entries = std::fs::read_dir(&commands_dir)
        .map_err(|e| SyncError::Failed(format!("Failed to read commands directory: {}", e)))?;

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                warn!("Unreadable entry in {:?}: {}", commands_dir, e);
                listing_complete = false;
                continue;
            }
        };
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
                    unreadable.insert(relative_source_path(&path, &workspace_root));
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

    // Get existing slash command workflows from PG. A failed read is
    // `SyncError::Failed` (the route's 500), never an empty list: read as empty, every command on
    // disk would be re-created as a duplicate.
    let existing_map =
        existing_source_map(pg.list_slash_command_sources().await).map_err(SyncError::Failed)?;

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
                Ok(wf) => match track_source(pg, &wf.id, parsed).await {
                    Ok(()) => {
                        result.created += 1;
                        info!("Created slash command workflow: {}", parsed.title);
                    }
                    Err(e) => {
                        // A row with no source_file_path is invisible to the
                        // next list read, which would create the command a
                        // second time — so the untracked row is removed.
                        let removed = match pg.delete_unified_workflow(&wf.id).await {
                            Ok(true) => "it was removed".to_string(),
                            Ok(false) => "it was already gone".to_string(),
                            Err(del) => format!("removing it also failed: {del}"),
                        };
                        result.errors.push(format!(
                            "Create {}: recording its source failed ({e}); {removed}",
                            parsed.command_name
                        ));
                    }
                },
                Err(e) => result
                    .errors
                    .push(format!("Create {}: {}", parsed.command_name, e)),
            }
        }
    }

    // Delete workflows for commands no longer on disk. An unreadable file's
    // import is kept; a checkout that yielded no command, or whose listing was
    // incomplete, deletes nothing and reports the withheld deletion in
    // `errors`.
    match stale_imports(&disk_commands, &unreadable, listing_complete, &existing_map) {
        Ok(stale) => {
            for (rel_path, (existing_id, name)) in stale {
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
        Err(withheld) => {
            warn!("{}", withheld);
            result.errors.push(withheld);
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
        assert_eq!(hint["next_action"]["kind"], "set_setting");
        assert_eq!(
            hint["next_action"]["target"],
            qontinui_types::paths::WORKSPACE_ROOT_SETTING
        );
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

    fn parsed(rel: &str) -> SlashCommandParsed {
        SlashCommandParsed {
            file_name: rel.rsplit('/').next().unwrap_or(rel).to_string(),
            command_name: "cmd".into(),
            title: "Cmd".into(),
            description: String::new(),
            full_content: String::new(),
            has_arguments: false,
            content_hash: String::new(),
            relative_path: rel.to_string(),
        }
    }

    fn imported(paths: &[&str]) -> HashMap<String, (String, String)> {
        paths
            .iter()
            .enumerate()
            .map(|(i, p)| (p.to_string(), (format!("id-{i}"), format!("name-{i}"))))
            .collect()
    }

    /// A list-query failure must surface as an error (the route's 500), never
    /// as an empty map that makes every command on disk a duplicate create.
    #[test]
    fn a_failed_import_list_is_an_error_not_an_empty_map() {
        let err = existing_source_map(Err("PG pool error: timed out".into()))
            .expect_err("a failed read must not read as no imports");
        assert!(err.contains("PG pool error: timed out"), "{err}");
        // ...and the error the sync returns for it is the route's 500.
        let (status, _) = SyncError::Failed(err).into_response();
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn the_import_list_keys_by_source_and_skips_unsourced_rows() {
        let map = existing_source_map(Ok(vec![
            ("1".into(), "fix".into(), "c/.claude/commands/fix.md".into()),
            ("2".into(), "hand-made".into(), String::new()),
        ]))
        .expect("listed");
        assert_eq!(map.len(), 1);
        assert_eq!(
            map.get("c/.claude/commands/fix.md"),
            Some(&("1".to_string(), "fix".to_string()))
        );
    }

    /// A checkout that yielded no parseable command must not delete the
    /// previously imported workflows.
    #[test]
    fn a_checkout_with_no_parseable_command_deletes_nothing() {
        let existing = imported(&["c/a.md", "c/b.md"]);
        let why = stale_imports(&HashMap::new(), &HashSet::new(), true, &existing)
            .expect_err("an empty yield must withhold deletion");
        assert!(
            why.contains("yielded no parseable command, so none of the 2 previously"),
            "{why}"
        );
    }

    /// An incomplete directory listing deletes nothing either: a file it
    /// missed would read as removed.
    #[test]
    fn an_incomplete_listing_deletes_nothing() {
        let existing = imported(&["c/a.md", "c/b.md"]);
        let disk: HashMap<String, SlashCommandParsed> = [("c/a.md".to_string(), parsed("c/a.md"))]
            .into_iter()
            .collect();
        let why = stale_imports(&disk, &HashSet::new(), false, &existing)
            .expect_err("an incomplete listing must withhold deletion");
        assert!(
            why.contains("listing was incomplete, so none of the 2 previously"),
            "{why}"
        );
    }

    #[test]
    fn only_imports_missing_from_a_nonempty_checkout_are_stale() {
        let existing = imported(&["c/a.md", "c/b.md", "c/c.md"]);
        let disk: HashMap<String, SlashCommandParsed> = [("c/a.md".to_string(), parsed("c/a.md"))]
            .into_iter()
            .collect();
        // c/c.md is on disk but unreadable: kept, not stale.
        let unreadable: HashSet<String> = ["c/c.md".to_string()].into_iter().collect();
        let stale = stale_imports(&disk, &unreadable, true, &existing).expect("non-empty checkout");
        let paths: Vec<&str> = stale.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, vec!["c/b.md"]);

        // Nothing imported is never a refusal, whatever the checkout yielded.
        assert!(
            stale_imports(&HashMap::new(), &HashSet::new(), false, &HashMap::new())
                .expect("nothing to withhold")
                .is_empty()
        );
    }

    /// A commands checkout under the isolated workspace root, holding `files`
    /// (name -> body).
    fn commands_checkout(
        amb: &crate::test_env::IsolatedAmbient,
        files: &[(&str, &str)],
    ) -> PathBuf {
        let dir = amb
            .root()
            .join(COMMANDS_SIBLING)
            .join(".claude")
            .join("commands");
        std::fs::create_dir_all(&dir).expect("mkdir commands");
        for (name, body) in files {
            std::fs::write(dir.join(name), body).expect("write command");
        }
        dir
    }

    /// Through the real sync: a list read that fails is the route's 500 and
    /// writes nothing — it used to read as "nothing imported" and go on to
    /// create every command again.
    #[tokio::test]
    async fn sync_fails_on_an_unreadable_import_list_instead_of_creating() {
        let amb = crate::test_env::isolated_ambient();
        commands_checkout(&amb, &[("fix.md", "# Fix\n\nFix things.\n")]);
        let pg = crate::database::pg::PgDb::new_noop_for_test();
        let err = sync_slash_commands(&pg)
            .await
            .expect_err("a failed list read must fail the sync");
        let (status, _) = err.into_response();
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    /// Through the real sync against a real database: an import whose file is
    /// still present but unreadable, and then a checkout that yields nothing,
    /// both keep the imported workflows.
    #[tokio::test]
    async fn sync_keeps_imports_when_the_checkout_cannot_be_read() {
        let amb = crate::test_env::isolated_ambient();
        let dir = commands_checkout(
            &amb,
            &[
                ("alpha.md", "# Alpha\n\nFirst.\n"),
                ("beta.md", "# Beta\n\nSecond.\n"),
            ],
        );
        let pg = crate::database::pg::PgDb::new_for_test().await;
        let first = sync_slash_commands(&pg).await.expect("first sync");
        assert_eq!(first.created, 2, "{first:?}");

        // beta.md becomes unreadable (a directory with the same name).
        std::fs::remove_file(dir.join("beta.md")).expect("rm beta");
        std::fs::create_dir(dir.join("beta.md")).expect("beta as dir");
        let partial = sync_slash_commands(&pg).await.expect("partial sync");
        assert_eq!(partial.deleted, 0, "{partial:?}");
        assert_eq!(partial.unchanged, 1, "{partial:?}");

        // Nothing parseable left at all.
        std::fs::remove_file(dir.join("alpha.md")).expect("rm alpha");
        let empty = sync_slash_commands(&pg).await.expect("empty sync");
        assert_eq!(empty.deleted, 0, "{empty:?}");
        assert!(
            empty
                .errors
                .iter()
                .any(|e| e.starts_with("Deletion withheld")),
            "{empty:?}"
        );
        let sources = pg.list_slash_command_sources().await.expect("list");
        assert_eq!(sources.len(), 2, "{sources:?}");
    }
}
