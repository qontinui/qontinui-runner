/**
 * buildPlanImplementationWorkflow
 *
 * Builds a multi-stage UnifiedWorkflow that maps the full /implement-plan
 * skill flow into the runner's workflow system. For each plan phase, generates
 * 3 stages (implement, review, next-steps), each with their own Claude sessions.
 *
 * This differs from buildPlanWorkflow which creates 1 stage per phase with only
 * a verify-fix loop. This builder adds review and next-steps cycles per phase,
 * matching the /implement-phase -> /review-plan -> /next-steps skill chain.
 */

import type {
  UnifiedWorkflow,
  VerificationStep,
  AgenticStep,
  CompletionStep,
  WorkflowStage,
} from "../../types/unified-workflow";
import { createSummaryStep } from "../../types/unified-workflow";
import { buildVerificationStep } from "./buildPlanWorkflow";
import type { PlanPhase } from "./buildPlanWorkflow";
import { DEFAULT_STAGE_FLAGS, DEFAULT_WORKFLOW_FLAGS } from "./workflowDefaults";

// =============================================================================
// Input Types
// =============================================================================

export interface BuildPlanImplementationWorkflowInput {
  /** The implementation plan phases */
  phases: PlanPhase[];
  /** Max iterations for implement stages (default: 15) */
  maxImplementIterations?: number;
  /** Max iterations for review stages (default: 6) */
  maxReviewIterations?: number;
  /** Max iterations for next-steps stages (default: 6) */
  maxNextStepsIterations?: number;
  /** Include a manual-test stage at the end (default: false) */
  includeManualTest?: boolean;
  /** Description of what to manually test */
  manualTestDescription?: string;
  /** Include a commit stage at the end (default: true) */
  includeCommit?: boolean;
  /** Workflow name override */
  workflowName?: string;
  /** Snapshot target for UI checks (default: "sdk") */
  snapshotTarget?: "control" | "sdk";
  /** Additional project context injected into all prompts */
  projectContext?: string;
}

// =============================================================================
// Prompt Builders
// =============================================================================

function buildTaskList(phase: PlanPhase): string {
  return phase.tasks.map((t, i) => `${i + 1}. **${t.name}**: ${t.description}`).join("\n");
}

function buildImplementPrompt(
  phase: PlanPhase,
  phaseIndex: number,
  projectContext?: string,
): string {
  const taskList = buildTaskList(phase);
  const contextSection = projectContext ? `\n### Project Context\n${projectContext}\n` : "";

  return `## Phase ${phaseIndex + 1}: ${phase.name}

${phase.description}

### Tasks to implement:
${taskList}

### Instructions

Implement ALL tasks listed above completely. Requirements:
- No stubs, no partial implementations, no TODOs
- Use subagents (Agent tool) for independent work across files/repos
- Fix any issues you encounter during implementation
- After implementing, run the verification checks to confirm your work compiles and passes
${contextSection}
Some verification steps may have failed. Analyze the failures and implement the necessary changes to make all checks pass.`;
}

function buildReviewPrompt(phase: PlanPhase, phaseIndex: number, projectContext?: string): string {
  const taskList = buildTaskList(phase);
  const contextSection = projectContext ? `\n### Project Context\n${projectContext}\n` : "";

  return `## Review: Phase ${phaseIndex + 1} — ${phase.name}

A previous session just implemented this phase. Your job is to review the implementation for completeness, correctness, and bugs, then fix everything you find.

### What was planned:
${taskList}

### How to review

1. **Understand what changed** — Run these commands to see recent work:
   \`\`\`bash
   git diff HEAD~5 --stat
   git log --oneline -10
   \`\`\`

2. **Audit completeness** — For each planned task, verify:
   - The code exists and is complete (not a stub)
   - It is wired up end-to-end (imports, exports, call sites connected)
   - No TODO/FIXME/HACK comments left behind

3. **Check for bugs** — Run type checkers and linters:
   - TypeScript: \`npx tsc --noEmit\`
   - Rust: \`cargo check\`
   - Python: \`ruff check .\`
   Then review the code for logical errors, missing error handling, race conditions, inconsistent state.

4. **Fix everything** — Do not just report issues. Fix them immediately:
   - Critical bugs first
   - Unfinished features second
   - Wiring gaps third
   - Lint/type errors fourth
${contextSection}
Some verification steps may have failed. Fix the underlying issues to make all checks pass.`;
}

function buildNextStepsPrompt(
  phase: PlanPhase,
  phaseIndex: number,
  projectContext?: string,
): string {
  const taskList = buildTaskList(phase);
  const contextSection = projectContext ? `\n### Project Context\n${projectContext}\n` : "";

  return `## Next Steps: Phase ${phaseIndex + 1} — ${phase.name}

Previous sessions implemented and reviewed this phase. Your job is to find and fix remaining wiring, polish, and integration issues.

### What was planned:
${taskList}

### How to analyze

Run \`git diff HEAD~10 --stat\` and \`git log --oneline -15\` to understand recent changes.

Look for:
1. **Missing wiring** — dead code, unconnected imports/exports, unused endpoints, interfaces defined but not consumed
2. **Unhandled fields** — type fields or interface properties defined but never used in the implementation
3. **Polish** — error messages that could be clearer, edge cases not handled, inconsistent naming
4. **Integration gaps** — new modules not exported from index.ts, new types not used by existing code, new functions not called anywhere
5. **Follow-up features** — small, natural extensions that complete the feature

### Instructions

For each issue found:
1. Implement the fix
2. Verify it compiles
3. Move on to the next issue

If no issues are found, report "No next steps identified" and finish.
${contextSection}
Some verification steps may have failed. Fix the underlying issues to make all checks pass.`;
}

function buildManualTestPrompt(description?: string): string {
  return `## Manual Testing

All implementation phases are complete. Test the application using the UI Bridge SDK.

### What to test:
${description ?? "Test all features that were just implemented. Focus on the UI changes."}

### Instructions

1. Check if a rebuild is needed (check git log for recent changes)
2. If Rust files changed, rebuild the runner via the supervisor API
3. Use the UI Bridge to navigate to relevant pages and test interactively
4. Fix any bugs found during testing
5. Report test results

Use \`curl -s http://localhost:9876/ui-bridge/control/snapshot\` for Runner UI testing.
Use \`curl -s http://localhost:3001/api/ui-bridge/control/snapshot\` for Web UI testing.`;
}

function buildCommitPrompt(): string {
  return `## Commit Changes

All implementation phases are complete and reviewed. Commit the changes.

### Instructions

1. Run \`git status\` and \`git diff --stat\` to see all changes
2. Stage all relevant files (exclude .env, credentials, node_modules)
3. Write a descriptive commit message summarizing the implementation
4. Commit with format: \`feat: <short summary>\`
5. Do NOT include AI attribution in the commit message`;
}

// =============================================================================
// Stage Builders
// =============================================================================

function buildPhaseVerificationSteps(phase: PlanPhase, snapshotTarget: string): VerificationStep[] {
  const steps: VerificationStep[] = [];
  for (const task of phase.tasks) {
    for (const v of task.verifications) {
      steps.push(buildVerificationStep(task, v, snapshotTarget));
    }
  }
  return steps;
}

function buildImplementStage(
  phase: PlanPhase,
  phaseIndex: number,
  verificationSteps: VerificationStep[],
  maxIterations: number,
  projectContext?: string,
): WorkflowStage {
  const agenticSteps: AgenticStep[] = [
    {
      id: crypto.randomUUID(),
      type: "prompt",
      phase: "agentic",
      name: `Implement: ${phase.name}`,
      content: buildImplementPrompt(phase, phaseIndex, projectContext),
    },
  ];

  return {
    id: `${phase.id}-implement`,
    name: `Phase ${phaseIndex + 1}: Implement — ${phase.name}`,
    description: `Implement all tasks for phase: ${phase.name}`,
    setup_steps: [],
    verification_steps: verificationSteps,
    agentic_steps: agenticSteps,
    completion_steps: [],
    max_iterations: maxIterations,
    ...DEFAULT_STAGE_FLAGS,
  };
}

function buildReviewStage(
  phase: PlanPhase,
  phaseIndex: number,
  verificationSteps: VerificationStep[],
  maxIterations: number,
  projectContext?: string,
): WorkflowStage {
  const agenticSteps: AgenticStep[] = [
    {
      id: crypto.randomUUID(),
      type: "prompt",
      phase: "agentic",
      name: `Review: ${phase.name}`,
      content: buildReviewPrompt(phase, phaseIndex, projectContext),
    },
  ];

  return {
    id: `${phase.id}-review`,
    name: `Phase ${phaseIndex + 1}: Review — ${phase.name}`,
    description: `Review implementation completeness and fix bugs for phase: ${phase.name}`,
    setup_steps: [],
    verification_steps: verificationSteps,
    agentic_steps: agenticSteps,
    completion_steps: [],
    max_iterations: maxIterations,
    ...DEFAULT_STAGE_FLAGS,
  };
}

function buildNextStepsStage(
  phase: PlanPhase,
  phaseIndex: number,
  verificationSteps: VerificationStep[],
  maxIterations: number,
  projectContext?: string,
): WorkflowStage {
  const agenticSteps: AgenticStep[] = [
    {
      id: crypto.randomUUID(),
      type: "prompt",
      phase: "agentic",
      name: `Next Steps: ${phase.name}`,
      content: buildNextStepsPrompt(phase, phaseIndex, projectContext),
    },
  ];

  return {
    id: `${phase.id}-next-steps`,
    name: `Phase ${phaseIndex + 1}: Next Steps — ${phase.name}`,
    description: `Find and fix remaining wiring, polish, and integration issues for phase: ${phase.name}`,
    setup_steps: [],
    verification_steps: verificationSteps,
    agentic_steps: agenticSteps,
    completion_steps: [],
    max_iterations: maxIterations,
    ...DEFAULT_STAGE_FLAGS,
  };
}

// =============================================================================
// Main Builder
// =============================================================================

/**
 * Build a multi-stage UnifiedWorkflow that maps the full /implement-plan flow.
 *
 * For each PlanPhase, generates 3 WorkflowStages:
 *   1. Implement — full implementation with verify-fix loop
 *   2. Review — audit completeness, find bugs, fix everything
 *   3. Next Steps — wiring, polish, integration gaps
 *
 * Optionally appends manual-test and commit stages at the end.
 * No stages are ever skipped — all run unconditionally for maximum completeness.
 */
export function buildPlanImplementationWorkflow(
  input: BuildPlanImplementationWorkflowInput,
): UnifiedWorkflow {
  const {
    phases,
    maxImplementIterations = 15,
    maxReviewIterations = 6,
    maxNextStepsIterations = 6,
    includeManualTest = false,
    manualTestDescription,
    includeCommit = true,
    workflowName,
    snapshotTarget = "sdk",
    projectContext,
  } = input;

  const now = new Date().toISOString();
  const stages: WorkflowStage[] = [];

  // Generate 3 stages per phase — each gets its own verification steps with unique UUIDs
  for (let i = 0; i < phases.length; i++) {
    const phase = phases[i];

    stages.push(
      buildImplementStage(
        phase,
        i,
        buildPhaseVerificationSteps(phase, snapshotTarget),
        maxImplementIterations,
        projectContext,
      ),
    );
    stages.push(
      buildReviewStage(
        phase,
        i,
        buildPhaseVerificationSteps(phase, snapshotTarget),
        maxReviewIterations,
        projectContext,
      ),
    );
    stages.push(
      buildNextStepsStage(
        phase,
        i,
        buildPhaseVerificationSteps(phase, snapshotTarget),
        maxNextStepsIterations,
        projectContext,
      ),
    );
  }

  // Optional manual-test stage
  if (includeManualTest) {
    const manualTestAgenticSteps: AgenticStep[] = [
      {
        id: crypto.randomUUID(),
        type: "prompt",
        phase: "agentic",
        name: "Manual Testing",
        content: buildManualTestPrompt(manualTestDescription),
      },
    ];

    stages.push({
      id: "manual-test",
      name: "Manual Testing",
      description: "Test implemented features via UI Bridge",
      setup_steps: [],
      verification_steps: [],
      agentic_steps: manualTestAgenticSteps,
      completion_steps: [],
      max_iterations: 3,
      ...DEFAULT_STAGE_FLAGS,
    });
  }

  // Optional commit stage
  if (includeCommit) {
    const commitCompletionSteps: CompletionStep[] = [
      {
        id: crypto.randomUUID(),
        type: "prompt",
        phase: "completion",
        name: "Commit Changes",
        content: buildCommitPrompt(),
      },
    ];

    stages.push({
      id: "commit",
      name: "Commit Changes",
      description: "Commit all implementation changes",
      setup_steps: [],
      verification_steps: [],
      agentic_steps: [],
      completion_steps: commitCompletionSteps,
      max_iterations: 1,
      ...DEFAULT_STAGE_FLAGS,
    });
  }

  // Count totals for description
  const totalTasks = phases.reduce((sum, p) => sum + p.tasks.length, 0);
  const totalVerifications = phases.reduce(
    (sum, p) => sum + p.tasks.reduce((s, t) => s + t.verifications.length, 0),
    0,
  );

  const name =
    workflowName ?? `Plan Implementation (${phases.length} phases, ${stages.length} stages)`;

  return {
    id: crypto.randomUUID(),
    name,
    description: `Auto-generated plan implementation workflow. ${phases.length} phases (${stages.length} stages), ${totalTasks} tasks, ${totalVerifications} verification checks. Each phase runs implement → review → next-steps.`,
    // Top-level steps are empty — all work is in stages
    setup_steps: [],
    verification_steps: [],
    agentic_steps: [],
    completion_steps: [createSummaryStep()],
    max_iterations: maxImplementIterations,
    stages,
    category: "plan-implementation",
    tags: ["plan", "auto-generated", "multi-stage", "implementation"],
    created_at: now,
    modified_at: now,
    ...DEFAULT_WORKFLOW_FLAGS,
  };
}
