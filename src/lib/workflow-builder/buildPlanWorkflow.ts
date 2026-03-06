/**
 * buildPlanWorkflow
 *
 * Builds a multi-stage UnifiedWorkflow from a structured implementation plan.
 * Each plan phase becomes a WorkflowStage with its own verification-agentic loop.
 *
 * Verification types map to step types:
 *   compile    → command step (tsc --noEmit, cargo check, etc.)
 *   ui_check   → UiBridge snapshot_assert step (deterministic element checks)
 *   command    → command step (arbitrary shell command)
 *   file_exists → command step (test -f <path>)
 *   api_check  → command step (curl with expected status)
 *   ai_review  → prompt step (AI evaluates criteria)
 *
 * All types except ai_review produce deterministic verification steps.
 */

import type {
  UnifiedWorkflow,
  PromptStep,
  CommandStep,
  VerificationStep,
  AgenticStep,
  WorkflowStage,
} from "../../types/unified-workflow";
import { createSummaryStep } from "../../types/unified-workflow";

// =============================================================================
// Input Types
// =============================================================================

export type VerificationType =
  | "compile"
  | "ui_check"
  | "command"
  | "file_exists"
  | "api_check"
  | "ai_review";

export interface UiCheckAssertion {
  id: string;
  description: string;
  assertionType: "exists" | "contains" | "visible" | "not_exists";
  criteria: Record<string, unknown>;
  expected?: string;
  severity?: string;
}

export interface TaskVerification {
  type: VerificationType;
  /** Human-readable description of what this verifies */
  description: string;

  // compile
  /** Shell command to run for compile/typecheck (e.g. "npx tsc --noEmit") */
  compile_command?: string;

  // ui_check
  /** Deterministic UI assertions to batch into a snapshot_assert step */
  assertions?: UiCheckAssertion[];
  /** Snapshot target: "control" or "sdk" (default: "sdk") */
  snapshot_target?: "control" | "sdk";

  // command
  /** Shell command for command/file_exists/api_check */
  command?: string;
  /** Working directory for the command */
  working_directory?: string;

  // file_exists
  /** File path to check for existence */
  file_path?: string;

  // api_check
  /** URL to check */
  url?: string;
  /** Expected HTTP status (default: 200) */
  expected_status?: number;

  // ai_review
  /** Criteria for AI to evaluate */
  criteria?: string;
}

export interface PlanTask {
  id: string;
  name: string;
  description: string;
  verifications: TaskVerification[];
}

export interface PlanPhase {
  id: string;
  name: string;
  description: string;
  tasks: PlanTask[];
  /** Max iterations for this phase's verification-agentic loop (default: 3) */
  max_iterations?: number;
}

export interface BuildPlanWorkflowInput {
  /** The implementation plan phases */
  phases: PlanPhase[];
  /** Custom agentic prompt (default: auto-generated from task descriptions) */
  agenticPrompt?: string;
  /** Global max iterations fallback (default: 3) */
  maxIterations?: number;
  /** Workflow name override */
  workflowName?: string;
  /** Snapshot target for UI checks (default: "sdk") */
  snapshotTarget?: "control" | "sdk";
}

// =============================================================================
// Step Builders
// =============================================================================

function buildCompileStep(task: PlanTask, v: TaskVerification): CommandStep {
  return {
    id: crypto.randomUUID(),
    type: "command",
    phase: "verification",
    name: `${task.name}: Compile`,
    mode: "check",
    check_type: "typecheck",
    command: v.compile_command ?? "npx tsc --noEmit",
    fail_on_error: true,
  };
}

function buildUiCheckStep(
  task: PlanTask,
  v: TaskVerification,
  defaultTarget: string,
): VerificationStep {
  const assertions = (v.assertions ?? []).map((a) => ({
    id: a.id,
    description: a.description,
    severity: a.severity ?? "critical",
    assertionType: a.assertionType,
    criteria: a.criteria,
    expected: a.expected,
  }));

  return {
    id: crypto.randomUUID(),
    type: "ui_bridge",
    phase: "verification",
    name: `${task.name}: UI Check`,
    ui_bridge_action: "snapshot_assert",
    ui_bridge_target: JSON.stringify(assertions),
    ui_bridge_snapshot_target: v.snapshot_target ?? defaultTarget,
  } as unknown as VerificationStep;
}

function buildCommandStep(task: PlanTask, v: TaskVerification): CommandStep {
  return {
    id: crypto.randomUUID(),
    type: "command",
    phase: "verification",
    name: `${task.name}: ${v.description}`,
    command: v.command ?? "echo 'no command specified'",
    working_directory: v.working_directory,
    fail_on_error: true,
  };
}

function buildFileExistsStep(task: PlanTask, v: TaskVerification): CommandStep {
  const path = v.file_path ?? v.command ?? "";
  return {
    id: crypto.randomUUID(),
    type: "command",
    phase: "verification",
    name: `${task.name}: File exists`,
    command: `test -f "${path}" && echo "OK: ${path} exists" || (echo "FAIL: ${path} not found" && exit 1)`,
    fail_on_error: true,
  };
}

function buildApiCheckStep(task: PlanTask, v: TaskVerification): CommandStep {
  const url = v.url ?? "";
  const expected = v.expected_status ?? 200;
  return {
    id: crypto.randomUUID(),
    type: "command",
    phase: "verification",
    name: `${task.name}: API Check`,
    command: `curl -s -o /dev/null -w "%{http_code}" "${url}" | grep -q "^${expected}$" && echo "OK: ${url} returned ${expected}" || (echo "FAIL: ${url} did not return ${expected}" && exit 1)`,
    fail_on_error: true,
  };
}

function buildAiReviewStep(task: PlanTask, v: TaskVerification): PromptStep {
  return {
    id: crypto.randomUUID(),
    type: "prompt",
    phase: "verification",
    name: `${task.name}: AI Review`,
    content: v.criteria ?? v.description,
  };
}

/** Map a single TaskVerification to the appropriate step. */
function buildVerificationStep(
  task: PlanTask,
  v: TaskVerification,
  defaultTarget: string,
): VerificationStep {
  switch (v.type) {
    case "compile":
      return buildCompileStep(task, v);
    case "ui_check":
      return buildUiCheckStep(task, v, defaultTarget);
    case "command":
      return buildCommandStep(task, v);
    case "file_exists":
      return buildFileExistsStep(task, v);
    case "api_check":
      return buildApiCheckStep(task, v);
    case "ai_review":
      return buildAiReviewStep(task, v);
  }
}

// =============================================================================
// Main Builder
// =============================================================================

/**
 * Build a multi-stage UnifiedWorkflow from a structured implementation plan.
 *
 * Each PlanPhase becomes a WorkflowStage. Within each stage:
 *   - setup_steps: empty (plan work is done by the agentic phase)
 *   - verification_steps: one step per TaskVerification across all tasks
 *   - agentic_steps: one prompt describing the phase's tasks
 *   - completion_steps: empty (summary is at workflow level)
 */
export function buildPlanWorkflow(input: BuildPlanWorkflowInput): UnifiedWorkflow {
  const { phases, agenticPrompt, maxIterations = 3, workflowName, snapshotTarget = "sdk" } = input;

  const now = new Date().toISOString();

  const stages: WorkflowStage[] = phases.map((phase) => {
    // Collect verification steps from all tasks in this phase
    const verificationSteps: VerificationStep[] = [];
    for (const task of phase.tasks) {
      for (const v of task.verifications) {
        verificationSteps.push(buildVerificationStep(task, v, snapshotTarget));
      }
    }

    // Build agentic prompt from task descriptions
    const taskList = phase.tasks
      .map((t, i) => `${i + 1}. **${t.name}**: ${t.description}`)
      .join("\n");

    const agenticContent =
      agenticPrompt ??
      `## Phase: ${phase.name}\n\n${phase.description}\n\nTasks to implement:\n${taskList}\n\nSome verification steps failed. Analyze the failures and implement the necessary changes to make all checks pass.`;

    const agenticSteps: AgenticStep[] = [
      {
        id: crypto.randomUUID(),
        type: "prompt",
        phase: "agentic",
        name: `Implement: ${phase.name}`,
        content: agenticContent,
      },
    ];

    return {
      id: phase.id,
      name: phase.name,
      description: phase.description,
      setup_steps: [],
      verification_steps: verificationSteps,
      agentic_steps: agenticSteps,
      completion_steps: [],
      max_iterations: phase.max_iterations ?? maxIterations,
    };
  });

  // Count totals for description
  const totalTasks = phases.reduce((sum, p) => sum + p.tasks.length, 0);
  const totalVerifications = phases.reduce(
    (sum, p) => sum + p.tasks.reduce((s, t) => s + t.verifications.length, 0),
    0,
  );

  const name = workflowName ?? `Implementation Plan (${phases.length} phases)`;

  return {
    id: crypto.randomUUID(),
    name,
    description: `Auto-generated from implementation plan. ${phases.length} phases, ${totalTasks} tasks, ${totalVerifications} verification checks.`,
    // Top-level steps are empty — all work is in stages
    setup_steps: [],
    verification_steps: [],
    agentic_steps: [],
    completion_steps: [createSummaryStep()],
    max_iterations: maxIterations,
    stages,
    category: "plan-generated",
    tags: ["plan", "auto-generated", "multi-stage"],
    created_at: now,
    modified_at: now,
  };
}
