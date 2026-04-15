//! Review agent subtask: spawns a cheaper-model child task to review a PR diff.
//!
//! After CI passes, the PR watcher calls `spawn_review_subtask` which creates
//! a child TaskRun that reads the PR diff and either approves or requests changes.
//! The child task blocks the parent from completing via `blocks_parent: true`.

use std::sync::Arc;

use crate::config_storage::ConfigStorage;
use crate::database::pg::PgDb;
use crate::database::CreateTaskRunInput;
use crate::AppState;

/// Configuration for spawning a review subtask.
pub struct ReviewSubtaskConfig {
    /// Parent task run ID (the coding task that opened the PR).
    pub parent_task_run_id: String,
    /// PR number to review.
    pub pr_number: u64,
    /// Full repo name (e.g., "owner/repo").
    pub repo_full_name: String,
    /// Model override for the review (cheaper model, e.g., "sonnet" or "haiku").
    pub review_model: Option<String>,
    /// Whether this review blocks the parent from completing. Default: true.
    pub blocks_parent: bool,
}

/// Review outcome parsed from the AI's response.
#[derive(Debug, Clone, PartialEq)]
pub enum ReviewOutcome {
    /// PR approved -- no issues found.
    Approved,
    /// Changes requested -- issues listed.
    ChangesRequested { feedback: String },
    /// Could not determine outcome.
    Inconclusive,
}

/// Spawn a review subtask for a PR.
///
/// Creates a child TaskRun with:
/// - `is_review: true`, `blocks_parent: true`
/// - A prompt that instructs the AI to review the PR diff
/// - Model override to a cheaper tier
///
/// Returns the review task run ID.
pub async fn spawn_review_subtask(
    pg: &PgDb,
    config: ReviewSubtaskConfig,
) -> Result<String, String> {
    let parent_prefix: String = config.parent_task_run_id.chars().take(8).collect();
    let uuid_prefix: String = uuid::Uuid::new_v4().to_string().chars().take(8).collect();
    let review_id = format!(
        "review-{}-pr{}-{}",
        parent_prefix, config.pr_number, uuid_prefix,
    );
    let review_name = format!(
        "Review PR #{} ({})",
        config.pr_number, config.repo_full_name
    );

    let prompt = format!(
        "You are a code reviewer. Review the pull request diff for PR #{number} in {repo}.\n\n\
         ## Instructions\n\n\
         1. Run `gh pr diff {number} --repo {repo}` to get the full diff\n\
         2. Review the changes for:\n\
            - Bugs, logic errors, or incorrect behavior\n\
            - Security vulnerabilities (injection, auth bypass, data exposure)\n\
            - Performance issues (N+1 queries, unbounded allocations, blocking I/O)\n\
            - Missing error handling at system boundaries\n\
            - Breaking changes to public APIs\n\
         3. Do NOT flag: style preferences, naming opinions, missing docs/comments, \
            or minor refactoring opportunities\n\
         4. After reviewing, output your decision:\n\
            - If the PR is acceptable: output `[REVIEW_APPROVED]`\n\
            - If changes are needed: output `[REVIEW_CHANGES_REQUESTED]` followed by \
              a numbered list of specific, actionable issues\n\n\
         Focus on correctness and security. Be concise.",
        number = config.pr_number,
        repo = config.repo_full_name,
    );

    // Look up parent to get root_task_run_id and depth
    let parent = pg
        .get_task_run(&config.parent_task_run_id)
        .await
        .map_err(|e| format!("Failed to get parent task run: {}", e))?;

    let root_id = parent
        .as_ref()
        .and_then(|p| p.root_task_run_id.clone())
        .unwrap_or_else(|| config.parent_task_run_id.clone());
    let depth = parent.as_ref().map_or(1, |p| p.depth + 1);

    let input = CreateTaskRunInput::new(&review_id, &review_name)
        .with_prompt(prompt)
        .with_workflow_name(&review_name)
        .with_workflow_type("unified")
        .with_task_type("review")
        .with_parent_task_run_id(&config.parent_task_run_id)
        .with_root_task_run_id(&root_id)
        .with_depth(depth)
        .with_is_review(true)
        .with_blocks_parent(config.blocks_parent);

    pg.create_task_run(&input)
        .await
        .map_err(|e| format!("Failed to create review task run: {}", e))?;

    // Store review metadata in result_data: model override + PR context for outcome processing
    {
        let mut meta = serde_json::json!({
            "pr_number": config.pr_number,
            "repo_full_name": config.repo_full_name,
        });
        if let Some(ref model) = config.review_model {
            meta["model_override"] = serde_json::json!(model);
        }
        pg.merge_task_run_result_data(&review_id, &meta.to_string())
            .await
            .map_err(|e| format!("Failed to set review result_data: {}", e))?;
    }

    // Set the review-specific columns via raw SQL (not yet in Clorinde schema)
    pg.set_review_flags(&review_id, true, config.blocks_parent)
        .await
        .map_err(|e| format!("Failed to set review flags: {}", e))?;

    tracing::info!(
        "Spawned review subtask {} for PR #{} (parent: {}, blocks_parent: {})",
        review_id,
        config.pr_number,
        config.parent_task_run_id,
        config.blocks_parent,
    );

    Ok(review_id)
}

/// Dependencies required to execute a review subtask workflow.
///
/// Bundles all the Arc-cloned state needed to construct a LoopController
/// and spawn the workflow. Mirrors `ReflectionDeps` / `FollowUpDeps`.
pub struct ReviewDeps {
    pub app_state: Arc<AppState>,
    pub config_storage: Arc<tokio::sync::Mutex<ConfigStorage>>,
    pub app_handle: tauri::AppHandle,
    pub pid_tracker: Arc<std::sync::Mutex<Vec<u32>>>,
}

/// Execute a review subtask that was previously created via `spawn_review_subtask`.
///
/// Builds a minimal LoopConfig (single iteration, prompt-only, no verification)
/// and spawns the workflow via `spawn_workflow_with_panic_guard`.
///
/// This follows the same pattern as `launch_reflection`, `launch_follow_up`, and
/// `launch_fixer`: create a LoopController, build a LoopConfig, and fire-and-forget.
pub fn execute_review_subtask(
    deps: ReviewDeps,
    review_id: String,
    review_name: String,
    prompt: String,
    model_override: Option<String>,
) {
    use std::collections::HashMap;

    let loop_config = super::types::LoopConfig {
        max_iterations: 1,
        base_prompt: prompt,
        workflow_name: review_name.clone(),
        workflow_id: review_id.clone(),
        execution_id: review_id.clone(),
        targeted_error_ids: Vec::new(),
        starting_iteration: 0,
        run_agentic_first: true, // Review is agentic-only (no verification to run first)
        artifact_dir: None,
        is_dev_mode: false,
        enable_sweep: false,
        max_sweep_iterations: 0,
        stages: Vec::new(),
        stop_on_failure: false,
        constraint_overrides: HashMap::new(),
        reflection_mode: false,
        provider_override: None,
        model_override,
        model_overrides: HashMap::new(),
        stage_index: None,
        max_sessions: Some(1),
        auto_run_generated: false,
        approval_gate: false,
        blocking_approval: false,
        confidence_threshold: 0.85,
        max_context_tokens: 100_000,
        enforce_token_budget: false,
        cross_workflow_learning: false,
        verification_history: HashMap::new(),
        routing_context: Default::default(),
        project_path: crate::mcp::shared::current_project_path(),
        acceptance_criteria: None,
        multi_agent_mode: false,
        rollback_policy: super::RollbackPolicy::None,
        escalation_policy: super::blame::EscalationPolicy::default(),
        iteration_diffs: Vec::new(),
        strict_cwd: false,
        tool_tags: Vec::new(),
        use_worktree: false,
        worktree_path: None,
        worktree_branch: None,
        workflow_architecture: None,
        agentic_verification_config: None,
        multi_agent_pipeline_config: None,
        active_canary: None,
        is_canary_run: false,
        phase_timeout_ms: None,
        max_fix_attempts: 0,
        max_ci_auto_resumes: 0,
        ci_failure_context: None,
        htn_config: crate::planning_bridge::HtnConfig::default(),
    };

    let mut controller = super::LoopController::new(
        deps.app_state.clone(),
        deps.config_storage.clone(),
        deps.app_handle.clone(),
        deps.pid_tracker.clone(),
    );

    tracing::info!(
        "Executing review subtask '{}' (id: {})",
        review_name,
        review_id,
    );

    let exec_id = review_id.clone();
    let wf_name = review_name;
    let url_lock = Some(deps.app_state.url_lock_manager.clone());
    let file_registry = Some(deps.app_state.file_registry_manager.clone());
    let file_lock = Some(deps.app_state.file_lock_manager.clone());

    super::spawn_workflow_with_panic_guard(
        exec_id,
        wf_name,
        url_lock,
        file_registry,
        file_lock,
        deps.app_state.pg_db.clone(),
        Box::pin(async move {
            controller
                .run(
                    loop_config,
                    Vec::new(), // setup automation steps
                    Vec::new(), // setup prompt steps
                    Vec::new(), // verification steps (review has none)
                    Vec::new(), // agentic steps (prompt is in loop_config.base_prompt)
                    Vec::new(), // completion automation steps
                    Vec::new(), // completion prompt steps
                )
                .await
        }),
    );

    tracing::info!("Review subtask '{}' spawned", review_id);
}

/// Process a completed review subtask: parse the AI output, post a GitHub review,
/// and determine whether the parent should unblock.
///
/// Called from `mark_task_completed` BEFORE the status is written. Returns `true`
/// if the task should complete normally (approved/inconclusive), `false` if the
/// caller should redirect to `mark_task_failed` (changes requested).
pub async fn process_review_outcome(
    pg: &Arc<PgDb>,
    review_task_run_id: &str,
    parent_task_run_id: &str,
) -> bool {
    // 1. Read the review's output
    let output = match pg.get_task_output(review_task_run_id).await {
        Ok(o) if !o.is_empty() => o,
        Ok(_) => {
            tracing::warn!(
                "Review subtask {} produced no output, treating as inconclusive",
                review_task_run_id
            );
            return true; // Don't block parent on empty output
        }
        Err(e) => {
            tracing::warn!(
                "Failed to read review output for {}: {}, allowing parent to proceed",
                review_task_run_id,
                e
            );
            return true;
        }
    };

    // 2. Parse the outcome
    let outcome = parse_review_outcome(&output);
    tracing::info!(
        "Review subtask {} outcome: {:?}",
        review_task_run_id,
        outcome
    );

    // 3. Read PR context from result_data
    let (pr_number, repo_full_name) = match pg.get_task_run_result_data(review_task_run_id).await {
        Ok(Some(data)) => {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&data) {
                let pr = json["pr_number"].as_u64().unwrap_or(0);
                let repo = json["repo_full_name"].as_str().unwrap_or("").to_string();
                (pr, repo)
            } else {
                (0, String::new())
            }
        }
        _ => (0, String::new()),
    };

    // 4. Post GitHub review if we have PR context
    let mut github_post_failed = false;
    if pr_number > 0 && !repo_full_name.is_empty() {
        if let Some((owner, repo)) = repo_full_name.split_once('/') {
            // Get the GitHub token from the parent's PR watch
            let token_result = pg.get_pr_watch_token_for_task(parent_task_run_id).await;
            if let Ok(Some(token)) = token_result {
                let client_result = crate::trigger_system::github_api::GitHubClient::new(&token);
                if let Ok(client) = client_result {
                    let (event, body) = match &outcome {
                        ReviewOutcome::Approved => (
                            "APPROVE",
                            "Automated review: no issues found. Approving.".to_string(),
                        ),
                        ReviewOutcome::ChangesRequested { feedback } => {
                            // Truncate feedback to avoid GitHub API limits (64KB).
                            // Use char_indices for UTF-8 safe truncation.
                            let truncated = if feedback.len() > 60_000 {
                                let mut end = 60_000;
                                while !feedback.is_char_boundary(end) && end > 0 {
                                    end -= 1;
                                }
                                format!("{}...\n\n(truncated)", &feedback[..end])
                            } else {
                                feedback.clone()
                            };
                            ("REQUEST_CHANGES", truncated)
                        }
                        ReviewOutcome::Inconclusive => (
                            "COMMENT",
                            "Automated review completed but could not determine a clear verdict. Please review manually.".to_string(),
                        ),
                    };

                    if let Err(e) = client
                        .submit_pr_review(owner, repo, pr_number, event, &body)
                        .await
                    {
                        tracing::warn!(
                            "Failed to post GitHub review for PR #{} in {}: {}",
                            pr_number,
                            repo_full_name,
                            e
                        );
                        github_post_failed = true;
                    } else {
                        tracing::info!(
                            "Posted {} review on PR #{} in {}",
                            event,
                            pr_number,
                            repo_full_name
                        );
                    }
                } else {
                    tracing::warn!(
                        "Failed to create GitHub client for review of PR #{}: {:?}",
                        pr_number,
                        client_result.err()
                    );
                    github_post_failed = true;
                }
            } else {
                tracing::warn!(
                    "No GitHub token found for parent task {} — cannot post review",
                    parent_task_run_id
                );
                github_post_failed = true;
            }
        }
    }

    // 5. Determine whether parent should unblock.
    // The caller (mark_task_completed) will redirect to mark_task_failed if false.
    match outcome {
        ReviewOutcome::Approved | ReviewOutcome::Inconclusive => true,
        ReviewOutcome::ChangesRequested { .. } => {
            // If we failed to post the review to GitHub, the PR author has no idea
            // changes were requested. Don't permanently block the parent in that case.
            if github_post_failed {
                tracing::warn!(
                    "Review {} requested changes but GitHub posting failed — \
                     allowing parent to proceed to avoid permanent block",
                    review_task_run_id
                );
                true
            } else {
                false
            }
        }
    }
}

/// Parse the review outcome from the AI's response text.
///
/// Checks for CHANGES_REQUESTED before APPROVED so that if both markers are
/// present (e.g. the AI quoted the approval marker while requesting changes),
/// the conservative choice wins.
pub fn parse_review_outcome(output: &str) -> ReviewOutcome {
    if output.contains("[REVIEW_CHANGES_REQUESTED]") {
        // Extract feedback after the marker
        let feedback = output
            .split("[REVIEW_CHANGES_REQUESTED]")
            .nth(1)
            .unwrap_or("")
            .trim()
            .to_string();
        ReviewOutcome::ChangesRequested {
            feedback: if feedback.is_empty() {
                "Changes requested (no specific feedback provided)".to_string()
            } else {
                feedback
            },
        }
    } else if output.contains("[REVIEW_APPROVED]") {
        ReviewOutcome::Approved
    } else {
        ReviewOutcome::Inconclusive
    }
}
