/**
 * Unified Workflow Types
 *
 * Data types are imported from the canonical @qontinui/schemas package.
 * UI constants (STEP_TYPES, PHASE_INFO, etc.) and feature detection types
 * are re-exported from @qontinui/shared-types.
 * Utility functions are re-exported from @qontinui/workflow-utils.
 *
 * Execution Order:
 *   Setup (once) -> [Verification <-> Agentic]* -> Completion (once)
 */

// =============================================================================
// Data types from canonical schema package
// =============================================================================

export {
  type WorkflowPhase,
  type LogSourceSelection,
  type HealthCheckUrl,
  type BaseStep,
  type HttpMethod,
  type ApiContentType,
  type ApiVariableExtraction,
  type ApiAssertion,
  type TestType,
  type PlaywrightExecutionMode,
  type CheckType,
  type CommandStep,
  type PromptStep,
  type UiBridgeStep,
  type StepTypeName,
  type UnifiedStep,
  type WorkflowStep,
  type SetupStep,
  type VerificationStep,
  type AgenticStep,
  type CompletionStep,
  type WorkflowStage,
  type UnifiedWorkflow,
  type WorkflowExportManifest,
  type ModelOverrideConfig,
  type ModelOverrides,
  type WorkflowExport,
  type WorkflowImportResult,
} from "@qontinui/schemas/unified_workflow";

// =============================================================================
// Types and constants from shared-types package
// =============================================================================

export {
  type WorkflowFeatures,
  type StepTypeInfo,
  STEP_TYPES,
  PHASE_INFO,
  DEFAULT_SUMMARY_PROMPT,
} from "@qontinui/shared-types/workflow";

// =============================================================================
// Utility functions from workflow-utils package
// =============================================================================

export {
  detectWorkflowFeatures,
  createSummaryStep,
  generateStepId,
  createDefaultStep,
  createDefaultWorkflow,
  isWorkflowEmpty,
  getTotalStepCount,
  getStepPhase,
  canStepExistInPhase,
  normalizeToPhases,
  getPhaseCount,
} from "@qontinui/workflow-utils";
