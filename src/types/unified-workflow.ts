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
// variant defeats field-level type inference at every consumer site. We
// alias the runner's `UnifiedStep` to `CanonicalStep` plus the runner-only
// step variants (e.g. `wrapper_action`) so the UI keeps strict narrowing
// over a known set of types. The wire-typed variant from
// `@qontinui/shared-types/workflow` still exists for code that needs to
// round-trip unknown shapes.
import type { CanonicalStep } from "@qontinui/shared-types/workflow";
import type { WorkflowPhase as _WorkflowPhase } from "@qontinui/shared-types/workflow";

/**
 * Workflow step that dispatches a typed action through an installed wrapper
 * (Phase 3 of the wrapper-runner integration plan). Unlike the four
 * canonical step types, wrapper actions are not part of the cross-runner
 * wire contract — they're a runner-only extension surface added on top of
 * `CanonicalStep`.
 *
 * The Rust backend deserializes these via `ExecutionStepConfig`'s
 * `wrapperId` / `actionId` / `wrapperParams` / `resultVariable` fields and
 * dispatches through the handler registry under the key `"wrapper_action"`.
 */
export interface WrapperActionStep {
  type: "wrapper_action";
  id: string;
  name: string;
  phase: _WorkflowPhase;
  /** Installed wrapper id (e.g. `"v0"`, `"gmail"`). */
  wrapperId: string;
  /** Action id exposed by the wrapper. */
  actionId: string;
  /**
   * Params to pass to the action. String-typed leaves may contain
   * `{{ variableName }}` template references resolved against the
   * workflow's runtime context before dispatch.
   */
  params: Record<string, unknown>;
  /** Optional workflow variable name to write the dispatch result to. */
  resultVariable?: string;
  /**
   * Provenance of this step when generated from a skill template.
   * Mirrored from `BaseStepFields.skill_origin` for consistency with
   * `CanonicalStep`; the `StepConfigPanel` "from skill" indicator reads
   * this on every variant.
   */
  skillOrigin?: { [k: string]: unknown };
  /** Optional retry policy mirrored from `BaseStepFields.retry`. */
  retry?: unknown;
  /** Step-level depends-on list mirrored from `BaseStepFields.depends_on`. */
  dependsOn?: string[];
  /** Optional `criterionIds` mirrored from `BaseStepFields.criterion_ids`. */
  criterionIds?: string[];
  /** Optional `extract` map mirrored from `BaseStepFields.extract`. */
  extract?: { [k: string]: string };
  /** Optional `inputs` map mirrored from `BaseStepFields.inputs`. */
  inputs?: { [k: string]: string };
  /** Optional `required` flag mirrored from `BaseStepFields.required`. */
  required?: boolean | null;
  /** Optional `timeoutSeconds` mirrored from `BaseStepFields.timeout_seconds`. */
  timeoutSeconds?: number | null;
  /** Optional `failOnConsoleErrors` mirrored from `BaseStepFields`. */
  failOnConsoleErrors?: boolean | null;
  /** Optional `runOnSubsequentIterations` mirrored from `BaseStepFields`. */
  runOnSubsequentIterations?: boolean | null;
  /** Forward-compat for fields the backend serializes alongside the canonical struct. */
  [extra: string]: unknown;
}

/**
 * Runner-side `UnifiedStep` alias: the four canonical step variants plus
 * runner-only extensions. Adding a new runner-only step variant is a
 * matter of adding a new arm here and a new type-narrowed config in
 * `step-config/`.
 */
export type UnifiedStep = CanonicalStep | WrapperActionStep;

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
