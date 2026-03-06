/**
 * buildSpecWorkflow
 *
 * Builds a complete UnifiedWorkflow from a SpecConfig. Extracted from
 * SpecWorkflowBuilder.tsx for reuse in both the spec workflow builder
 * and the live page generator's spec workflow mode.
 *
 * Generates a hybrid workflow:
 *  - Assertions with deterministic types (exists, contains, visible, not_exists)
 *    AND a target with search criteria become fast UiBridge "snapshot_assert" steps.
 *  - Assertions requiring AI reasoning (behavior, source_review, semantic, or
 *    those without a target) become prompt steps evaluated by Claude.
 *
 * Deterministic assertions within the same group are batched into a single
 * UiBridge step that fetches one snapshot and evaluates all assertions locally
 * in Rust — no AI tokens, completes in seconds.
 */

import type { UnifiedWorkflow, PromptStep, VerificationStep } from "../../types/unified-workflow";
import { createSummaryStep } from "../../types/unified-workflow";

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
  expected?: string;
  [key: string]: unknown;
}

export interface SpecGroup {
  id: string;
  name: string;
  description: string;
  category: string;
  assertions: SpecAssertion[];
  [key: string]: unknown;
}

export interface SpecConfig {
  version: string;
  description?: string;
  groups: SpecGroup[];
  assertions?: unknown[];
  metadata?: Record<string, unknown>;
}

export interface BuildSpecWorkflowInput {
  /** The spec configuration to build from */
  specConfig: SpecConfig;
  /** Which group IDs to include (default: all groups) */
  selectedGroupIds?: Set<string>;
  /** Custom agentic prompt for failure recovery */
  agenticPrompt?: string;
  /** Max verification/agentic iterations (default: 3) */
  maxIterations?: number;
  /** Element source: "control" (runner UI) or "external" (browser tab).
   *  Falls back to specConfig.metadata.elementSource, then "control". */
  elementSource?: "control" | "external";
  /** Page URL for setup navigation (optional) */
  pageUrl?: string;
  /** Workflow name override */
  workflowName?: string;
  /** Force all assertions to use prompt steps (disables deterministic optimization) */
  forcePromptOnly?: boolean;
}

// Assertion types that can be evaluated deterministically against a UI snapshot
const DETERMINISTIC_TYPES = new Set(["exists", "contains", "visible", "not_exists"]);

/** Check if an assertion can be evaluated deterministically (no AI needed). */
function isDeterministic(assertion: SpecAssertion): boolean {
  const aType = assertion.assertionType || "exists";
  return DETERMINISTIC_TYPES.has(aType) && assertion.target?.criteria != null;
}

/** Build a batched UiBridge snapshot_assert step from a list of deterministic assertions. */
function buildSnapshotAssertStep(
  groupName: string,
  assertions: SpecAssertion[],
  elementSource: string,
): VerificationStep {
  // Pack all assertions into a JSON array for the Rust handler to evaluate
  const assertionSpecs = assertions.map((a) => ({
    id: a.id,
    description: a.description,
    severity: a.severity,
    assertionType: a.assertionType || "exists",
    criteria: a.target?.criteria ?? {},
    expected: a.expected,
  }));

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
    maxIterations = 3,
    pageUrl,
    workflowName,
    forcePromptOnly = false,
  } = input;

  // Resolve element source: explicit param > spec metadata > "control"
  const metadataSource = specConfig.metadata?.elementSource as "control" | "external" | undefined;
  const elementSource = input.elementSource ?? metadataSource ?? "control";

  const now = new Date().toISOString();

  // Filter groups
  const selectedGroups = selectedGroupIds
    ? specConfig.groups.filter((g) => selectedGroupIds.has(g.id))
    : specConfig.groups;

  // Setup steps
  const setupSteps: PromptStep[] = [];

  const sourceExplanation =
    elementSource === "external"
      ? "Elements are fetched from an external SDK-connected app. The AI must fix the web application code so these assertions pass when checked against the live page."
      : "Elements are fetched from the runner's own webview (control mode).";

  // Verification steps: split each group's assertions into deterministic vs AI-requiring
  const verificationSteps: VerificationStep[] = [];

  for (const group of selectedGroups) {
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

  // Agentic steps
  const agenticSteps: PromptStep[] = [];
  if (agenticPrompt || selectedGroups.length > 0) {
    const defaultPrompt =
      agenticPrompt ||
      `Some verification steps failed. Analyze the failures and fix the issues.\n\nThe test specifications describe what the application should look like and how it should behave. Review the failing assertions and make the necessary code changes to fix them.`;
    agenticSteps.push({
      id: crypto.randomUUID(),
      type: "prompt",
      phase: "agentic",
      name: "Fix Verification Failures",
      content: defaultPrompt,
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
    setup_steps: setupSteps,
    verification_steps: verificationSteps,
    agentic_steps: agenticSteps,
    completion_steps: completionSteps,
    max_iterations: maxIterations,
    category: "spec-generated",
    tags: ["spec", "auto-generated", "live-page"],
    created_at: now,
    modified_at: now,
  };
}
