/**
 * buildSpecExpansionWorkflow
 *
 * Builds a UnifiedWorkflow that expands an existing spec by:
 *   1. Analyzing the current page for uncovered interactive elements
 *   2. Generating new assertion groups for discovered elements
 *   3. Verifying the expanded spec against the live page
 *
 * This is useful when a spec was initially generated with basic coverage
 * and needs to be enriched with additional assertions for edge cases,
 * accessibility, state transitions, etc.
 */

import type { UnifiedWorkflow } from "../../types/unified-workflow";
import { createSummaryStep } from "../../types/unified-workflow";
import type { SpecConfig } from "./buildSpecWorkflow";
import { DEFAULT_WORKFLOW_FLAGS } from "./workflowDefaults";

export interface BuildSpecExpansionWorkflowInput {
  /** The existing spec configuration to expand */
  specConfig: SpecConfig;
  /** The spec ID being expanded */
  specId: string;
  /** Page URL for navigation (optional — uses spec metadata if not provided) */
  pageUrl?: string;
  /** Element source: "control" (runner UI) or "external" (browser tab) */
  elementSource?: "control" | "external";
  /** Max verification/agentic iterations (default: 3) */
  maxIterations?: number;
}

/**
 * Build a workflow that expands an existing spec with additional coverage.
 */
export function buildSpecExpansionWorkflow(
  input: BuildSpecExpansionWorkflowInput,
): UnifiedWorkflow {
  const { specConfig, specId, pageUrl, elementSource = "control", maxIterations = 3 } = input;

  const resolvedPageUrl = pageUrl ?? (specConfig.metadata?.pageUrl as string | undefined) ?? "";

  const existingGroupNames = specConfig.groups.map((g) => g.name).join(", ");
  const existingAssertionCount = specConfig.groups.reduce((sum, g) => sum + g.assertions.length, 0);

  const specSummary = JSON.stringify(
    {
      specId,
      description: specConfig.description,
      version: specConfig.version,
      groupCount: specConfig.groups.length,
      assertionCount: existingAssertionCount,
      groups: existingGroupNames,
    },
    null,
    2,
  );

  const snapshotTarget = elementSource === "external" ? "sdk" : "control";

  const now = new Date().toISOString();

  const workflow: UnifiedWorkflow = {
    id: crypto.randomUUID(),
    name: `Expand Spec: ${specConfig.description?.slice(0, 50) ?? specId}`,
    description: `Expand coverage of spec "${specId}" by discovering uncovered elements and adding new assertion groups.`,
    category: "spec-expansion",
    tags: ["spec", "expansion", "coverage", "auto-generated"],
    createdAt: now,
    modified_at: now,

    setupSteps: [],

    verificationSteps: [
      {
        id: crypto.randomUUID(),
        type: "ui_bridge",
        phase: "verification",
        name: "Snapshot current page elements",
        ui_bridge_action: "snapshot",
        ui_bridge_snapshot_target: snapshotTarget,
        ...(resolvedPageUrl ? { ui_bridge_navigate_url: resolvedPageUrl } : {}),
      } as unknown as UnifiedWorkflow["verificationSteps"][number],
      {
        id: crypto.randomUUID(),
        type: "prompt",
        phase: "verification",
        name: "Analyze coverage gaps",
        content: `You are analyzing a UI spec for coverage gaps.

Current spec summary:
\`\`\`json
${specSummary}
\`\`\`

Review the page snapshot and identify:
1. Interactive elements (buttons, inputs, links, dropdowns) not covered by existing assertion groups: ${existingGroupNames}
2. State transitions not tested (hover, focus, disabled states)
3. Accessibility gaps (missing ARIA labels, keyboard navigation)
4. Error/edge cases not covered (empty states, loading states, validation)

For each gap found, describe what assertion group should be added and what assertions it should contain.

If coverage is already comprehensive, report PASS.
If there are gaps, report FAIL and list the specific gaps.`,
      },
    ],

    agenticSteps: [
      {
        id: crypto.randomUUID(),
        type: "prompt",
        phase: "agentic",
        name: "Generate expansion assertions",
        content: `Based on the coverage gaps identified in verification, generate new spec assertion groups to expand the spec "${specId}".

Current spec has ${specConfig.groups.length} groups with ${existingAssertionCount} assertions covering: ${existingGroupNames}.

For each coverage gap:
1. Create a new assertion group with a descriptive name and category
2. Add specific assertions with appropriate severity levels (critical/warning/info)
3. Include element targets with search criteria where possible
4. Use assertion types: exists, visible, contains, not_exists, hasText, enabled, disabled

Output the new groups as JSON that can be merged into the existing spec.
Each group should follow this structure:
{
  "id": "<unique-id>",
  "name": "<group-name>",
  "description": "<what-this-group-verifies>",
  "category": "<category>",
  "assertions": [...]
}

Focus on quality over quantity — each assertion should verify something meaningful that the existing spec misses.`,
      },
    ],

    completionSteps: [createSummaryStep()],

    maxIterations: maxIterations,
    ...DEFAULT_WORKFLOW_FLAGS,
    reflectionMode: true,
  };

  // Add navigation setup step if we have a page URL
  if (resolvedPageUrl) {
    workflow.setupSteps.push({
      id: crypto.randomUUID(),
      type: "command",
      phase: "setup",
      name: "Navigate to target page",
      command: "",
      content: `Navigate to ${resolvedPageUrl} and wait for the page to load.`,
    } as unknown as UnifiedWorkflow["setupSteps"][number]);
  }

  return workflow;
}
