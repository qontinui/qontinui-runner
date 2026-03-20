//! Slash command import: scan, parse, and sync Claude Code slash commands as runner workflows.
//!
//! Scans `qontinui-claude-config/.claude/commands/*.md` for command files,
//! parses each into a workflow, and syncs with the database (create/update/delete).

use crate::database::CheckpointDb;
use crate::unified_workflows::{CreateUnifiedWorkflowRequest, UpdateUnifiedWorkflowRequest};
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

/// Find the commands directory relative to the workspace root.
fn find_commands_directory() -> Result<(PathBuf, PathBuf), String> {
    let (workspace_root, _, _) = crate::mcp::shared::get_workspace_paths_internal()?;
    let commands_dir = workspace_root
        .join("qontinui-claude-config")
        .join(".claude")
        .join("commands");

    if !commands_dir.exists() {
        return Err(format!(
            "Commands directory not found: {}",
            commands_dir.display()
        ));
    }

    Ok((workspace_root, commands_dir))
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
        format!("{}...", &desc[..197])
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
        max_iterations: 1,
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
    }
}

/// Sync slash commands from disk to the database.
///
/// Scans the commands directory, compares with existing DB entries,
/// and creates/updates/deletes as needed.
pub fn sync_slash_commands(db: &CheckpointDb) -> Result<SyncResult, String> {
    let (workspace_root, commands_dir) = find_commands_directory()?;

    // Scan for .md files
    let pattern = commands_dir.join("*.md");
    let pattern_str = pattern.to_string_lossy().replace('\\', "/");

    let files: Vec<PathBuf> = glob::glob(&pattern_str)
        .map_err(|e| format!("Failed to glob commands: {}", e))?
        .filter_map(|entry| entry.ok())
        .collect();

    info!(
        "Found {} slash command files in {}",
        files.len(),
        commands_dir.display()
    );

    // Parse all files
    let mut parsed_commands: Vec<SlashCommandParsed> = Vec::new();
    let mut parse_errors: Vec<String> = Vec::new();

    for file_path in &files {
        match parse_command_file(file_path, &workspace_root) {
            Ok(parsed) => parsed_commands.push(parsed),
            Err(e) => parse_errors.push(e),
        }
    }

    // Get existing slash command workflows from DB
    let existing = db.list_slash_command_sources()?;
    let mut existing_map: HashMap<String, (String, String)> = HashMap::new(); // path → (id, hash)
    for (id, path, hash) in &existing {
        existing_map.insert(path.clone(), (id.clone(), hash.clone()));
    }

    let mut result = SyncResult {
        created: 0,
        updated: 0,
        deleted: 0,
        unchanged: 0,
        errors: parse_errors,
    };

    // Track which existing entries we've seen (for deletion detection)
    let mut seen_paths: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Process each parsed command
    for parsed in &parsed_commands {
        seen_paths.insert(parsed.relative_path.clone());

        if let Some((existing_id, existing_hash)) = existing_map.get(&parsed.relative_path) {
            if existing_hash == &parsed.content_hash {
                // Unchanged
                result.unchanged += 1;
            } else {
                // Content changed — update
                let update_request = UpdateUnifiedWorkflowRequest {
                    name: Some(parsed.title.clone()),
                    description: Some(parsed.description.clone()),
                    tags: Some({
                        let mut tags =
                            vec!["slash-command".to_string(), parsed.command_name.clone()];
                        if parsed.has_arguments {
                            tags.push("requires-arguments".to_string());
                        } else {
                            tags.push("no-arguments".to_string());
                        }
                        tags
                    }),
                    agentic_steps: Some(vec![json!({
                        "type": "prompt",
                        "name": parsed.command_name,
                        "phase": "agentic",
                        "content": parsed.full_content
                    })]),
                    // Leave all other fields unchanged
                    category: None,
                    setup_steps: None,
                    verification_steps: None,
                    completion_steps: None,
                    max_iterations: None,
                    timeout_seconds: None,
                    provider: None,
                    model: None,
                    skip_ai_summary: None,
                    log_source_selection: None,
                    context_ids: None,
                    disabled_context_ids: None,
                    auto_include_contexts: None,
                    prompt_template: None,
                    log_watch_enabled: None,
                    health_check_enabled: None,
                    health_check_urls: None,
                    preflight_check_enabled: None,
                    enable_sweep: None,
                    max_sweep_iterations: None,
                    stages: None,
                    stop_on_failure: None,
                    constraint_overrides: None,
                    approval_gate: None,
                    reflection_mode: None,
                    completion_prompts_first: None,
                    model_overrides: None,
                    dependency_graph: None,
                    cost_annotations: None,
                    quality_report: None,
                    acceptance_criteria: None,
                    ai_reviewed: None,
                };

                match db.update_slash_command_content(
                    existing_id,
                    &update_request,
                    &parsed.content_hash,
                ) {
                    Ok(_) => {
                        info!(
                            "Updated slash command: {} ({})",
                            parsed.command_name, parsed.relative_path
                        );
                        result.updated += 1;
                    }
                    Err(e) => {
                        result
                            .errors
                            .push(format!("Failed to update {}: {}", parsed.command_name, e));
                    }
                }
            }
        } else {
            // New command — create
            let request = build_workflow_request(parsed);
            match db.create_slash_command_workflow(
                &request,
                &parsed.relative_path,
                &parsed.content_hash,
            ) {
                Ok(wf) => {
                    info!(
                        "Created slash command workflow: {} ({})",
                        wf.name, parsed.relative_path
                    );
                    result.created += 1;
                }
                Err(e) => {
                    result
                        .errors
                        .push(format!("Failed to create {}: {}", parsed.command_name, e));
                }
            }
        }
    }

    // Delete workflows whose source files no longer exist
    for (path, (id, _)) in &existing_map {
        if !seen_paths.contains(path) {
            match db.delete_unified_workflow(id) {
                Ok(true) => {
                    info!("Deleted slash command workflow for removed file: {}", path);
                    result.deleted += 1;
                }
                Ok(false) => {
                    warn!("Slash command workflow already gone: {}", path);
                }
                Err(e) => {
                    result
                        .errors
                        .push(format!("Failed to delete workflow for {}: {}", path, e));
                }
            }
        }
    }

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

    #[test]
    fn test_content_hash() {
        let content = b"# Test Command\n\nSome content.";
        let mut hasher = Sha256::new();
        hasher.update(content);
        let hash = format!("{:x}", hasher.finalize());
        assert_eq!(hash.len(), 64); // SHA-256 hex is 64 chars
    }
}
