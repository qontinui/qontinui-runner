//! Review agent subtask: spawns a cheaper-model child task to review a PR diff.
//!
//! After CI passes, the PR watcher calls `spawn_review_subtask` which creates
//! a child TaskRun that reads the PR diff and either approves or requests changes.
//! The child task blocks the parent from completing via `blocks_parent: true`.

use crate::database::pg::PgDb;
use crate::database::CreateTaskRunInput;

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
        parent_prefix,
        config.pr_number,
        uuid_prefix,
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

    // Store model override in result_data so the executor can read it at run time
    if let Some(ref model) = config.review_model {
        let model_json = serde_json::json!({ "model_override": model }).to_string();
        pg.set_task_run_result_data(&review_id, &model_json)
            .await
            .map_err(|e| format!("Failed to set review model override: {}", e))?;
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
