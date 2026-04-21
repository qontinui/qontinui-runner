/**
 * buildSpecWorkflow — pure deterministic spec→workflow converter.
 *
 * ## When to use
 * Use this for **CI regression, batch verification, and offline/token-free
 * flows**: same spec against the same page every commit, reproducible output,
 * no API key or network required.
 *
 * ## When NOT to use
 * For interactive authoring on the Specs page, prefer `buildSpecBrief` +
 * `generateFromBrief` (gated by the `specs.useAiGeneration` flag / the
 * "Generate with AI" toggle in ConnectionBar). The AI path can synthesize
 * setup/navigation steps the spec itself may be missing, handle
 * semantic/behavior assertions via agentic orchestration, and inherits the
 * Builder→Verifier→Fixer pipeline plus post-execution reflection.
 *
 * ## What this builder does
 * Generates a hybrid workflow:
 *  - Assertions with deterministic types (exists, contains, visible, not_exists)
 *    AND a target with search criteria become fast UiBridge "snapshot_assert" steps.
 *  - Assertions requiring AI reasoning (behavior, source_review, semantic, or
 *    those without a target) become prompt steps evaluated by Claude.
 *
 * Deterministic assertions within the same group are batched into a single
 * UiBridge step that fetches one snapshot and evaluates all assertions locally
 * in Rust — no AI tokens, completes in seconds.
 *
 * ## Known limitation
 * This builder copies the spec verbatim. If the spec is missing `setupActions`
 * on groups whose assertions depend on a specific UI state (e.g., a panel must
 * be open), the workflow will silently fail on correct code. The AI path
 * (`generateFromBrief`) mitigates this by deriving preconditions and letting
 * the Builder agent synthesize setup steps.
 */

import type { UnifiedWorkflow, PromptStep, VerificationStep } from "../../types/unified-workflow";
import type { KnownIssue } from "@qontinui/shared-types";
import { createSummaryStep } from "../../types/unified-workflow";
import { DEFAULT_WORKFLOW_FLAGS } from "./workflowDefaults";

// Re-export SpecConfig type shape for consumers that don't import ui-bridge directly
export interface SpecAssertion {
  id: string;
  description: string;
  severity: string;
  enabled: boolean;
  assertionType?: string;
  target?: {
    type?: string;
    criteria?: Record<string, unknown>;
    label?: string;
  };
  relatedTarget?: {
    type?: string;
    criteria?: Record<string, unknown>;
    label?: string;
  };
  expected?: string;
  minGap?: number;
  [key: string]: unknown;
}

export interface SetupAction {
  type: "click" | "type" | "navigate" | "waitForElement" | "wait";
  target?: {
    type?: string;
    criteria?: Record<string, unknown>;
    elementId?: string;
    label?: string;
  };
  value?: string;
  clear?: boolean;
  url?: string;
  ms?: number;
  timeout?: number;
}

export interface SpecGroup {
  id: string;
  name: string;
  description: string;
  category: string;
  assertions: SpecAssertion[];
  setupActions?: SetupAction[];
  [key: string]: unknown;
}

/** Minimal shape of a state in the spec stateMachine. Kept structurally compatible
 *  with `SpecState` in `../spec-prompt-builder.ts` so callers can pass either. */
export interface SpecStateShape {
  id: string;
  name: string;
  description?: string;
  elements?: Record<string, unknown>[];
  isInitial?: boolean;
  transitions?: unknown[];
}

export interface SpecStateMachineShape {
  states: SpecStateShape[];
}

export interface SpecConfig {
  version: string;
  description?: string;
  groups: SpecGroup[];
  assertions?: unknown[];
  metadata?: Record<string, unknown>;
  /** Optional state machine section — when present, buildSpecBrief will map
   *  group preconditions to state IDs for one-step navigation. */
  stateMachine?: SpecStateMachineShape;
}

export interface BuildSpecWorkflowInput {
  /** The spec configuration to build from */
  specConfig: SpecConfig;
  /** Which group IDs to include (default: all groups) */
  selectedGroupIds?: Set<string>;
  /** Custom agentic prompt for failure recovery */
  agenticPrompt?: string;
  /** Additional instructions appended to the agentic prompt (preserves default context) */
  additionalInstructions?: string;
  /** Max verification/agentic iterations. `null` (or omitted) = unlimited. */
  maxIterations?: number | null;
  /** Element source: "control" (runner UI) or "external" (browser tab).
   *  Falls back to specConfig.metadata.elementSource, then "control". */
  elementSource?: "control" | "external";
  /** Page URL for setup navigation (optional) */
  pageUrl?: string;
  /** Workflow name override */
  workflowName?: string;
  /** Force all assertions to use prompt steps (disables deterministic optimization) */
  forcePromptOnly?: boolean;
  /** Include regression checks for known issues scoped to this spec */
  includeRegressionChecks?: boolean;
  /** Known issues to include as regression verification steps */
  knownIssues?: KnownIssue[];
}

// Assertion types that can be evaluated deterministically against a UI snapshot
const DETERMINISTIC_TYPES = new Set(["exists", "contains", "visible", "not_exists"]);

// Spatial assertions need both target and relatedTarget to be deterministic
const SPATIAL_TYPES = new Set(["noOverlap", "minSpacing"]);

/** Check if an assertion can be evaluated deterministically (no AI needed). */
export function isDeterministic(assertion: SpecAssertion): boolean {
  const aType = assertion.assertionType || "exists";
  if (SPATIAL_TYPES.has(aType)) {
    return assertion.target?.criteria != null && assertion.relatedTarget?.criteria != null;
  }
  return DETERMINISTIC_TYPES.has(aType) && assertion.target?.criteria != null;
}

/** Build a batched UiBridge snapshot_assert step from a list of deterministic assertions. */
function buildSnapshotAssertStep(
  groupName: string,
  assertions: SpecAssertion[],
  elementSource: string,
): VerificationStep {
  // Pack all assertions into a JSON array for the Rust handler to evaluate
  const assertionSpecs = assertions.map((a) => {
    const spec: Record<string, unknown> = {
      id: a.id,
      description: a.description,
      severity: a.severity,
      assertionType: a.assertionType || "exists",
      criteria: a.target?.criteria ?? {},
      expected: a.expected,
    };
    if (a.relatedTarget?.criteria) {
      spec.relatedCriteria = a.relatedTarget.criteria;
    }
    if (a.minGap !== undefined) {
      spec.minGap = a.minGap;
    }
    return spec;
  });

  // Use the ui_bridge_* prefixed field names that the Rust ExecutionStepConfig
  // deserializer expects (the bare "action"/"target" names aren't aliased)
  return {
    id: crypto.randomUUID(),
    type: "ui_bridge",
    phase: "verification",
    name: groupName,
    ui_bridge_action: "snapshot_assert",
    ui_bridge_target: JSON.stringify(assertionSpecs),
    ui_bridge_snapshot_target: elementSource === "external" ? "sdk" : "control",
  } as unknown as VerificationStep;
}

/** Build a prompt step for assertions that need AI evaluation. */
function buildPromptStep(
  groupName: string,
  groupDescription: string,
  assertions: SpecAssertion[],
  elementSource: string,
  sourceExplanation: string,
): PromptStep {
  const assertionDescriptions = assertions
    .map((a) => `- ${a.description} [${a.severity}]`)
    .join("\n");

  return {
    id: crypto.randomUUID(),
    type: "prompt",
    phase: "verification",
    name: groupName,
    content: `${groupDescription}\n\nElement source: ${elementSource} — ${sourceExplanation}\n\nAssertions:\n${assertionDescriptions}`,
  };
}

/** Build a UI Bridge setup step from a SetupAction. */
function buildSetupActionStep(
  groupName: string,
  action: SetupAction,
  elementSource: string,
): VerificationStep {
  const snapshotTarget = elementSource === "external" ? "sdk" : "control";

  switch (action.type) {
    case "navigate":
      return {
        id: crypto.randomUUID(),
        type: "ui_bridge",
        phase: "setup",
        name: `${groupName}: Navigate to ${action.url}`,
        ui_bridge_action: "navigate",
        ui_bridge_url: action.url || "",
        ui_bridge_snapshot_target: snapshotTarget,
      } as unknown as VerificationStep;

    case "click":
      return {
        id: crypto.randomUUID(),
        type: "ui_bridge",
        phase: "setup",
        name: `${groupName}: Click element`,
        ui_bridge_action: "element_action",
        ui_bridge_target: JSON.stringify({
          action: "click",
          criteria: action.target?.criteria ?? {},
        }),
        ui_bridge_snapshot_target: snapshotTarget,
      } as unknown as VerificationStep;

    case "type":
      return {
        id: crypto.randomUUID(),
        type: "ui_bridge",
        phase: "setup",
        name: `${groupName}: Type "${action.value}"`,
        ui_bridge_action: "element_action",
        ui_bridge_target: JSON.stringify({
          action: "type",
          criteria: action.target?.criteria ?? {},
          params: { text: action.value, clear: action.clear },
        }),
        ui_bridge_snapshot_target: snapshotTarget,
      } as unknown as VerificationStep;

    case "waitForElement":
      return {
        id: crypto.randomUUID(),
        type: "ui_bridge",
        phase: "setup",
        name: `${groupName}: Wait for element`,
        ui_bridge_action: "wait_for_element",
        ui_bridge_target: JSON.stringify({
          criteria: action.target?.criteria ?? {},
          timeout: action.timeout ?? 5000,
        }),
        ui_bridge_snapshot_target: snapshotTarget,
      } as unknown as VerificationStep;

    case "wait":
      return {
        id: crypto.randomUUID(),
        type: "ui_bridge",
        phase: "setup",
        name: `${groupName}: Wait ${action.ms}ms`,
        ui_bridge_action: "wait",
        ui_bridge_target: String(action.ms ?? 1000),
        ui_bridge_snapshot_target: snapshotTarget,
      } as unknown as VerificationStep;
  }
}

/**
 * Build a complete UnifiedWorkflow from a SpecConfig.
 *
 * Produces: setup -> verification (hybrid UiBridge + prompt steps) -> agentic -> completion
 */
export function buildSpecWorkflow(input: BuildSpecWorkflowInput): UnifiedWorkflow {
  const {
    specConfig,
    selectedGroupIds,
    agenticPrompt,
    additionalInstructions,
    maxIterations = null,
    pageUrl,
    workflowName,
    forcePromptOnly = false,
    includeRegressionChecks = false,
    knownIssues = [],
  } = input;

  // Resolve element source: explicit param > spec metadata > "control"
  const metadataSource = specConfig.metadata?.elementSource as "control" | "external" | undefined;
  const elementSource = input.elementSource ?? metadataSource ?? "control";

  const now = new Date().toISOString();

  // Filter groups
  const selectedGroups = selectedGroupIds
    ? specConfig.groups.filter((g) => selectedGroupIds.has(g.id))
    : specConfig.groups;

  // Setup steps — generated from group setupActions
  const setupSteps: Array<PromptStep | VerificationStep> = [];

  const sourceExplanation =
    elementSource === "external"
      ? "Elements are fetched from an external SDK-connected app. The AI must fix the web application code so these assertions pass when checked against the live page."
      : "Elements are fetched from the runner's own webview (control mode).";

  // Verification steps: split each group's assertions into deterministic vs AI-requiring
  const verificationSteps: VerificationStep[] = [];

  for (const group of selectedGroups) {
    // Generate setup steps from group setupActions
    if (group.setupActions && group.setupActions.length > 0) {
      for (const action of group.setupActions) {
        setupSteps.push(buildSetupActionStep(group.name, action, elementSource));
      }
    }

    const enabledAssertions = group.assertions.filter((a) => a.enabled);
    if (enabledAssertions.length === 0) continue;

    if (forcePromptOnly) {
      // Legacy mode: everything goes to AI
      verificationSteps.push(
        buildPromptStep(
          group.name,
          group.description,
          enabledAssertions,
          elementSource,
          sourceExplanation,
        ),
      );
      continue;
    }

    const deterministic = enabledAssertions.filter(isDeterministic);
    const aiRequired = enabledAssertions.filter((a) => !isDeterministic(a));

    if (deterministic.length > 0) {
      verificationSteps.push(buildSnapshotAssertStep(group.name, deterministic, elementSource));
    }

    if (aiRequired.length > 0) {
      const name = deterministic.length > 0 ? `${group.name} (AI)` : group.name;
      verificationSteps.push(
        buildPromptStep(name, group.description, aiRequired, elementSource, sourceExplanation),
      );
    }
  }

  // ── Regression checks from known issues ─────────────────────────────
  if (includeRegressionChecks && knownIssues.length > 0) {
    for (const issue of knownIssues) {
      if (issue.verification_step_template) {
        // Deterministic step from template
        const step = {
          ...issue.verification_step_template,
          id: `regression-${issue.id}`,
          name: `[Regression] ${issue.title}`,
          phase: "verification",
          regression_issue_id: issue.id,
        } as unknown as VerificationStep;
        verificationSteps.push(step);
      } else {
        // AI judgment step from description + hint
        const hint = issue.verification_hint
          ? `\nVerification hint: ${issue.verification_hint}`
          : "";
        verificationSteps.push({
          id: `regression-${issue.id}`,
          type: "prompt",
          name: `[Regression] ${issue.title}`,
          phase: "verification",
          content: `Check for a known issue: ${issue.description}${hint}`,
          response_mode: true,
          regression_issue_id: issue.id,
        } as unknown as VerificationStep);
      }
    }
  }

  // Agentic steps
  const agenticSteps: PromptStep[] = [];
  if (agenticPrompt || selectedGroups.length > 0) {
    const basePrompt =
      agenticPrompt ||
      `Some verification steps failed. Analyze the failures and fix the issues.\n\nThe test specifications describe what the application should look like and how it should behave. Review the failing assertions and make the necessary code changes to fix them.`;
    const extraInstructions = additionalInstructions
      ? `\n\n## Additional Instructions\n\n${additionalInstructions}`
      : "";
    agenticSteps.push({
      id: crypto.randomUUID(),
      type: "prompt",
      phase: "agentic",
      name: "Fix Verification Failures",
      content: `${basePrompt}${extraInstructions}`,
    });
  }

  // Completion steps
  const completionSteps = [createSummaryStep()];

  // Count total enabled assertions
  const totalAssertions = selectedGroups.reduce(
    (sum, g) => sum + g.assertions.filter((a: SpecAssertion) => a.enabled).length,
    0,
  );

  const name =
    workflowName || `Spec Verification${pageUrl ? ` — ${new URL(pageUrl).hostname}` : ""}`;

  return {
    id: crypto.randomUUID(),
    name,
    description: `Auto-generated from spec configuration. ${selectedGroups.length} groups selected with ${totalAssertions} total assertions.`,
    setupSteps: setupSteps,
    verificationSteps: verificationSteps,
    agenticSteps: agenticSteps,
    completionSteps: completionSteps,
    maxIterations: maxIterations,
    category: "spec-generated",
    tags: ["spec", "auto-generated", "live-page"],
    createdAt: now,
    modified_at: now,
    ...DEFAULT_WORKFLOW_FLAGS,
  };
}
