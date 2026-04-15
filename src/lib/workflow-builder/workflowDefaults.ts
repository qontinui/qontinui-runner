/**
 * Workflow/Stage default fields.
 *
 * Upstream tightened UnifiedWorkflow and WorkflowStage so that every
 * `#[serde(default)]` Rust field is REQUIRED in the generated TS. These
 * helpers centralize the boolean/numeric defaults so call sites building
 * workflow/stage literals stay compact.
 */

/**
 * Required boolean/numeric fields on UnifiedWorkflow that have Rust-side
 * `#[serde(default)]` attributes. Safe to spread into any workflow literal.
 */
export const DEFAULT_WORKFLOW_FLAGS = {
  ai_reviewed: false,
  approval_gate: false,
  auto_include_contexts: true,
  completion_prompts_first: false,
  enable_sweep: false,
  enforce_token_budget: false,
  health_check_enabled: false,
  is_favorite: false,
  log_watch_enabled: true,
  max_ci_auto_resumes: 10,
  max_fix_attempts: 3,
  max_sweep_iterations: 5,
  multi_agent_mode: true,
  preflight_check_enabled: true,
  reflection_mode: true,
  skip_ai_summary: false,
  stop_on_failure: false,
  strict_cwd: false,
  use_worktree: false,
} as const;

/**
 * Required boolean fields on WorkflowStage (from `#[serde(default)]`).
 */
export const DEFAULT_STAGE_FLAGS = {
  approval_gate: false,
  completion_prompts_first: false,
} as const;
