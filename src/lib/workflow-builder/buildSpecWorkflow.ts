/**
 * buildSpecWorkflow
 *
 * Builds a complete UnifiedWorkflow from a SpecConfig. Extracted from
 * SpecWorkflowBuilder.tsx for reuse in both the spec workflow builder
 * and the live page generator's spec workflow mode.
 */

import type { UnifiedWorkflow, PromptStep } from "../../types/unified-workflow";
import { createSummaryStep } from "../../types/unified-workflow";

// Re-export SpecConfig type shape for consumers that don't import ui-bridge directly
export interface SpecGroup {
  id: string;
  name: string;
  description: string;
  category: string;
  assertions: Array<{
    id: string;
    description: string;
    severity: string;
    enabled: boolean;
    [key: string]: unknown;
  }>;
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
}

/**
 * Build a complete UnifiedWorkflow from a SpecConfig.
 *
 * Produces: setup (optional navigate) -> verification (prompt steps) -> agentic (prompt) -> completion (summary)
 */
export function buildSpecWorkflow(input: BuildSpecWorkflowInput): UnifiedWorkflow {
  const {
    specConfig,
    selectedGroupIds,
    agenticPrompt,
    maxIterations = 3,
    pageUrl,
    workflowName,
  } = input;

  // Resolve element source: explicit param > spec metadata > "control"
  const metadataSource = specConfig.metadata?.elementSource as "control" | "external" | undefined;
  const elementSource = input.elementSource ?? metadataSource ?? "control";

  const now = new Date().toISOString();

  // Filter groups
  const selectedGroups = selectedGroupIds
    ? specConfig.groups.filter((g) => selectedGroupIds.has(g.id))
    : specConfig.groups;

  // Setup steps: navigate to page URL if provided
  const setupSteps: PromptStep[] = [];
  // Note: We don't add a step for navigation in spec workflow mode
  // because the page is already connected via the SDK or extension.
  // The workflow assumes the page is already loaded.

  // Verification steps: each selected group becomes a verification prompt
  const sourceExplanation =
    elementSource === "external"
      ? "Elements are fetched from an external SDK-connected app. The AI must fix the web application code so these assertions pass when checked against the live page."
      : "Elements are fetched from the runner's own webview (control mode).";

  const verificationSteps: PromptStep[] = selectedGroups.map((group) => {
    const enabledAssertions = group.assertions.filter((a) => a.enabled);
    const assertionDescriptions = enabledAssertions
      .map((a) => `- ${a.description} [${a.severity}]`)
      .join("\n");

    return {
      id: crypto.randomUUID(),
      type: "prompt",
      phase: "verification",
      name: group.name,
      content: `${group.description}\n\nElement source: ${elementSource} — ${sourceExplanation}\n\nAssertions:\n${assertionDescriptions}`,
    };
  });

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
    (sum, g) => sum + g.assertions.filter((a) => a.enabled).length,
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
