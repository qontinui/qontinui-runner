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
  aiReviewed: false,
  approvalGate: false,
  autoIncludeContexts: true,
  completionPromptsFirst: false,
  enableSweep: false,
  enforceTokenBudget: false,
  healthCheckEnabled: false,
  htnEnabled: false,
  isFavorite: false,
  logWatchEnabled: true,
  maxCiAutoResumes: 10,
  maxFixAttempts: 3,
  maxSweepIterations: 5,
  multiAgentMode: true,
  preflightCheckEnabled: true,
  reflectionMode: true,
  skipAiSummary: false,
  stopOnFailure: false,
  strictCwd: false,
  useWorktree: false,
} as const;

/**
 * Required boolean fields on WorkflowStage (from `#[serde(default)]`).
 */
export const DEFAULT_STAGE_FLAGS = {
  approvalGate: false,
  completionPromptsFirst: false,
} as const;
