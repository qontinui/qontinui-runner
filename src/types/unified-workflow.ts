/**
 * Unified Workflow Types
 *
 * All workflow data types and UI constants come from @qontinui/shared-types,
 * which is the canonical Rust-generated package (schemars → JSON Schema →
 * json-schema-to-typescript, bundled with tsup).
 *
 * Utility functions are re-exported from @qontinui/workflow-utils.
 *
 * Execution Order:
 *   Setup (once) -> [Verification <-> Agentic]* -> Completion (once)
 */

// =============================================================================
// Data types, UI constants, and feature detection — single source of truth
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
  type WorkflowFeatures,
  type StepTypeInfo,
  STEP_TYPES,
  PHASE_INFO,
  DEFAULT_SUMMARY_PROMPT,
} from "@qontinui/shared-types/workflow";

// Runner UI treats workflow steps as strictly-canonical values — every
// consumer narrows by the `type` discriminator and reads typed fields. The
// wire contract's `UnifiedStep = CanonicalStep | { [k: string]: unknown }`
// preserves lossless round-trip of unknown step shapes, but that `Other`
// variant defeats field-level type inference at every consumer site. Alias
// the runner's `UnifiedStep` to `CanonicalStep` so the UI keeps its strict
// view. The wire-typed variant from `@qontinui/shared-types/workflow`
// still exists for code that needs to round-trip unknown shapes.
export type { CanonicalStep as UnifiedStep } from "@qontinui/shared-types/workflow";

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
