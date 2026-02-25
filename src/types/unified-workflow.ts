/**
 * Unified Workflow Types
 *
 * Data types are imported from the canonical @qontinui/schemas package.
 * Utility functions, constants, and UI-specific interfaces are defined locally.
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
  type WorkflowExport,
  type WorkflowImportResult,
} from "@qontinui/schemas/unified_workflow";

import type {
  WorkflowPhase,
  UnifiedStep,
  UnifiedWorkflow,
  PromptStep,
  WorkflowStep,
  WorkflowStage,
} from "@qontinui/schemas/unified_workflow";

// =============================================================================
// Feature Detection
// =============================================================================

/**
 * Features detected from workflow steps
 */
export interface WorkflowFeatures {
  /** Has any setup steps */
  hasSetup: boolean;
  /** Has any verification steps */
  hasVerification: boolean;
  /** Has any agentic steps */
  hasAgentic: boolean;
  /** Has any completion steps */
  hasCompletion: boolean;
  /** Has UI Bridge steps in any phase */
  hasUiBridge: boolean;
  /** Show iteration settings (has agentic steps) */
  showIterationSettings: boolean;
  /** Has AI prompts in any phase */
  hasAiPrompts: boolean;
}

/**
 * Detect features from workflow steps
 */
export function detectWorkflowFeatures(workflow: UnifiedWorkflow): WorkflowFeatures {
  // Collect steps from top-level and all stages
  const allSteps: UnifiedStep[] = [
    ...workflow.setup_steps,
    ...workflow.verification_steps,
    ...workflow.agentic_steps,
    ...(workflow.completion_steps ?? []),
    ...(workflow.stages ?? []).flatMap((s) => [
      ...s.setup_steps,
      ...s.verification_steps,
      ...s.agentic_steps,
      ...(s.completion_steps ?? []),
    ]),
  ];

  const hasSetup =
    workflow.setup_steps.length > 0 ||
    (workflow.stages ?? []).some((s) => s.setup_steps.length > 0);
  const hasVerification =
    workflow.verification_steps.length > 0 ||
    (workflow.stages ?? []).some((s) => s.verification_steps.length > 0);
  const hasAgentic =
    workflow.agentic_steps.length > 0 ||
    (workflow.stages ?? []).some((s) => s.agentic_steps.length > 0);
  const hasCompletion =
    (workflow.completion_steps ?? []).length > 0 ||
    (workflow.stages ?? []).some((s) => (s.completion_steps ?? []).length > 0);

  const hasUiBridge = allSteps.some((s) => s.type === "ui_bridge");
  const hasAiPrompts = allSteps.some((s) => s.type === "prompt");

  return {
    hasSetup,
    hasVerification,
    hasAgentic,
    hasCompletion,
    hasUiBridge,
    showIterationSettings: hasAgentic,
    hasAiPrompts,
  };
}

// =============================================================================
// Step Type Constants
// =============================================================================

/**
 * Step type display information
 */
export interface StepTypeInfo {
  type: string;
  label: string;
  description: string;
  icon: string;
  color: string;
  phase: WorkflowPhase | "setup" | "verification";
}

/**
 * All step types organized by phase.
 * 4 core types available in setup, verification, and completion.
 * Agentic phase is restricted to AI Prompt only.
 */
export const STEP_TYPES: Record<WorkflowPhase, StepTypeInfo[]> = {
  setup: [
    {
      type: "command",
      label: "Command",
      description: "Run shell commands, checks, or tests",
      icon: "Terminal",
      color: "gray",
      phase: "setup",
    },
    {
      type: "ui_bridge",
      label: "UI Bridge",
      description: "Interact with UI via UI Bridge SDK",
      icon: "Monitor",
      color: "emerald",
      phase: "setup",
    },
    {
      type: "prompt",
      label: "AI Task",
      description: "AI-driven task",
      icon: "Bot",
      color: "violet",
      phase: "setup",
    },
    {
      type: "workflow",
      label: "Workflow",
      description: "Run a saved workflow",
      icon: "Workflow",
      color: "blue",
      phase: "setup",
    },
  ],
  verification: [
    {
      type: "command",
      label: "Command",
      description: "Run commands, checks, or tests for verification",
      icon: "Terminal",
      color: "gray",
      phase: "verification",
    },
    {
      type: "ui_bridge",
      label: "UI Bridge",
      description: "Verify UI state via UI Bridge",
      icon: "Monitor",
      color: "emerald",
      phase: "verification",
    },
    {
      type: "prompt",
      label: "AI Verification",
      description: "AI-evaluated criteria",
      icon: "Bot",
      color: "violet",
      phase: "verification",
    },
    {
      type: "workflow",
      label: "Workflow",
      description: "Run a saved workflow for verification",
      icon: "Workflow",
      color: "blue",
      phase: "verification",
    },
  ],
  agentic: [
    {
      type: "prompt",
      label: "Prompt",
      description: "AI task instructions",
      icon: "MessageSquare",
      color: "amber",
      phase: "agentic",
    },
  ],
  completion: [
    {
      type: "command",
      label: "Command",
      description: "Run cleanup commands or final tests",
      icon: "Terminal",
      color: "gray",
      phase: "completion",
    },
    {
      type: "ui_bridge",
      label: "UI Bridge",
      description: "Final UI interactions",
      icon: "Monitor",
      color: "emerald",
      phase: "completion",
    },
    {
      type: "prompt",
      label: "AI Completion",
      description: "Final AI actions",
      icon: "Bot",
      color: "violet",
      phase: "completion",
    },
    {
      type: "workflow",
      label: "Workflow",
      description: "Run a saved workflow as a completion step",
      icon: "Workflow",
      color: "blue",
      phase: "completion",
    },
  ],
};

/**
 * Phase display information
 */
export const PHASE_INFO: Record<
  WorkflowPhase,
  { label: string; description: string; color: string }
> = {
  setup: {
    label: "Setup",
    description: "Runs once at the beginning",
    color: "blue",
  },
  verification: {
    label: "Verification",
    description: "Checks success criteria, loops with agentic",
    color: "green",
  },
  agentic: {
    label: "Agentic",
    description: "AI work, iterates until verification passes",
    color: "amber",
  },
  completion: {
    label: "Completion",
    description: "Runs once after the loop exits",
    color: "purple",
  },
};

// =============================================================================
// Summary Step Constants and Helpers
// =============================================================================

/**
 * Default prompt content for the AI summary step.
 * This step generates a summary of all tasks completed in the workflow.
 */
export const DEFAULT_SUMMARY_PROMPT = `Write a one-paragraph summary of all the tasks completed in this workflow. Include what was accomplished, whether the stated goal was achieved, any issues encountered and how they were resolved, and remaining work if the goal was not fully achieved. Be concise but comprehensive.`;

/**
 * Create a summary step for the completion phase.
 * Summary steps are locked to the last position and cannot be moved.
 */
export function createSummaryStep(): PromptStep {
  return {
    id: crypto.randomUUID(),
    type: "prompt",
    phase: "completion",
    name: "AI Summary",
    content: DEFAULT_SUMMARY_PROMPT,
    is_summary_step: true,
  };
}

// =============================================================================
// Utility Functions
// =============================================================================

/**
 * Generate a unique ID for a step
 */
export function generateStepId(): string {
  return crypto.randomUUID();
}

/**
 * Create a default step of a given type
 */
export function createDefaultStep(type: UnifiedStep["type"], phase: WorkflowPhase): UnifiedStep {
  const id = generateStepId();

  switch (type) {
    case "command":
      return {
        id,
        type: "command",
        phase: phase as "setup" | "verification" | "completion",
        name: "Command",
        mode: "shell",
        command: "",
      };
    case "ui_bridge":
      return {
        id,
        type: "ui_bridge",
        phase: phase as "setup" | "verification" | "completion",
        name: "UI Bridge",
        action: "snapshot",
      };
    case "prompt": {
      // Different default names based on phase
      const promptNames: Record<WorkflowPhase, string> = {
        setup: "AI Setup Task",
        verification: "AI Verification",
        agentic: "Prompt",
        completion: "AI Completion Task",
      };
      return {
        id,
        type: "prompt",
        phase: phase as "setup" | "verification" | "agentic" | "completion",
        name: promptNames[phase] ?? "Prompt",
        content: "",
      };
    }
    case "workflow":
      return {
        id,
        type: "workflow",
        phase: phase as "setup" | "verification" | "completion",
        name: "Workflow",
        workflow_id: "",
        workflow_name: "",
      } as WorkflowStep as UnifiedStep;
    default:
      throw new Error(`Unknown step type: ${type}`);
  }
}

/**
 * Create a default empty workflow
 *
 * @param includeSummaryStep - Whether to include the AI Summary step in the completion phase.
 *                             Defaults to true. The summary step is locked to last position.
 */
export function createDefaultWorkflow(
  includeSummaryStep: boolean = true,
): Omit<UnifiedWorkflow, "id" | "created_at" | "modified_at"> {
  return {
    name: "",
    description: "",
    setup_steps: [],
    verification_steps: [],
    agentic_steps: [],
    completion_steps: includeSummaryStep ? [createSummaryStep()] : [],
    category: "general",
    tags: [],
  };
}

/**
 * Check if a workflow is empty (has no user-added steps).
 * A workflow with only a summary step is considered empty since it's the default state.
 */
export function isWorkflowEmpty(workflow: UnifiedWorkflow): boolean {
  // If workflow has stages with content, it's not empty
  if ((workflow.stages ?? []).length > 0) {
    return false;
  }

  const completionSteps = workflow.completion_steps ?? [];
  // A workflow is empty if it has no setup/verification/agentic steps
  // and completion only has the default summary step (or nothing)
  const hasOnlySummaryStep =
    completionSteps.length === 0 ||
    (completionSteps.length === 1 &&
      completionSteps[0].type === "prompt" &&
      (completionSteps[0] as PromptStep).is_summary_step === true);

  return (
    workflow.setup_steps.length === 0 &&
    workflow.verification_steps.length === 0 &&
    workflow.agentic_steps.length === 0 &&
    hasOnlySummaryStep
  );
}

/**
 * Get total step count across all phases
 */
export function getTotalStepCount(workflow: UnifiedWorkflow): number {
  const topLevelCount =
    workflow.setup_steps.length +
    workflow.verification_steps.length +
    workflow.agentic_steps.length +
    (workflow.completion_steps ?? []).length;

  const stagesCount = (workflow.stages ?? []).reduce(
    (sum, s) =>
      sum +
      s.setup_steps.length +
      s.verification_steps.length +
      s.agentic_steps.length +
      (s.completion_steps ?? []).length,
    0,
  );

  return topLevelCount + stagesCount;
}

/**
 * Get the phase for a given step
 */
export function getStepPhase(step: UnifiedStep): WorkflowPhase {
  return step.phase;
}

/**
 * Check if a step type can exist in a given phase
 */
export function canStepExistInPhase(stepType: UnifiedStep["type"], phase: WorkflowPhase): boolean {
  // Agentic phase only allows prompts
  if (phase === "agentic") {
    return stepType === "prompt";
  }

  // All 4 step types are allowed in setup, verification, and completion
  switch (stepType) {
    case "command":
    case "ui_bridge":
    case "prompt":
    case "workflow":
      return true;
    default:
      return false;
  }
}

// =============================================================================
// Phase Normalization Helpers
// =============================================================================

/** Convert any workflow to its phases (stages) representation */
export function normalizeToPhases(workflow: UnifiedWorkflow): WorkflowStage[] {
  if (workflow.stages && workflow.stages.length > 0) {
    return workflow.stages;
  }
  return [
    {
      id: workflow.id + "-phase-1",
      name: workflow.name,
      description: workflow.description,
      setup_steps: workflow.setup_steps,
      verification_steps: workflow.verification_steps,
      agentic_steps: workflow.agentic_steps,
      completion_steps: workflow.completion_steps ?? [],
      max_iterations: workflow.max_iterations,
      timeout_seconds: workflow.timeout_seconds,
      provider: workflow.provider,
      model: workflow.model,
    },
  ];
}

/** Get the number of phases in a workflow */
export function getPhaseCount(workflow: UnifiedWorkflow): number {
  return normalizeToPhases(workflow).length;
}
