/**
 * buildSpecImplementationPlan
 *
 * Converts a SpecConfig into PlanPhase[] suitable for buildPlanWorkflow().
 * Each spec group becomes a phase with UI Bridge verification steps derived
 * from the group's assertions. The agentic prompt instructs the AI to
 * *implement* features (not just fix failures).
 *
 * Also supports diff-based plan generation: given a SpecDiff, only generate
 * phases for changed/added groups.
 */

import type { SpecConfig, SpecGroup, SpecAssertion } from "./buildSpecWorkflow";
import type { PlanPhase, PlanTask, UiCheckAssertion } from "./buildPlanWorkflow";
import type { SpecDiff } from "../spec-diff";

// Assertion types that map to deterministic ui_check verifications
const DETERMINISTIC_TYPES = new Set(["exists", "contains", "visible", "not_exists"]);

export interface SpecImplementationPlanInput {
  /** The spec configuration to build the plan from */
  specConfig: SpecConfig;
  /** Only generate plan for these groups (default: all) */
  selectedGroupIds?: Set<string>;
  /** Element source for UI checks */
  elementSource?: "control" | "external";
  /** Additional context for the agentic prompts */
  projectContext?: string;
  /** The URL of the page this spec describes */
  pageUrl?: string;
}

/**
 * Convert a SpecConfig into implementation plan phases.
 *
 * Phase 0: Page scaffold (compile check)
 * Phase 1..N: One per spec group (UI assertions + agentic implementation)
 * Phase N+1: Integration (full compile + all assertions combined)
 */
export function buildSpecImplementationPlan(input: SpecImplementationPlanInput): PlanPhase[] {
  const {
    specConfig,
    selectedGroupIds,
    elementSource = "external",
    projectContext,
    pageUrl,
  } = input;

  const snapshotTarget = elementSource === "external" ? "sdk" : "control";

  const selectedGroups = selectedGroupIds
    ? specConfig.groups.filter((g) => selectedGroupIds.has(g.id))
    : specConfig.groups;

  if (selectedGroups.length === 0) return [];

  const phases: PlanPhase[] = [];

  // Phase 0: Page scaffold
  phases.push({
    id: "phase-scaffold",
    name: "Page Scaffold",
    description: buildScaffoldDescription(specConfig, pageUrl),
    tasks: [
      {
        id: "task-scaffold-component",
        name: "Create page component and route",
        description: `Create the page component for "${specConfig.description ?? "this page"}". Set up routing so the page is accessible${pageUrl ? ` at ${pageUrl}` : ""}.`,
        verifications: [{ type: "compile", description: "Project compiles after scaffold" }],
      },
    ],
    max_iterations: 3,
  });

  // One phase per spec group
  for (const group of selectedGroups) {
    const enabledAssertions = group.assertions.filter((a) => a.enabled);
    if (enabledAssertions.length === 0) continue;

    const tasks = buildTasksFromGroup(group, enabledAssertions, snapshotTarget);
    const phaseDescription = buildImplementationPrompt(group, enabledAssertions, projectContext);

    phases.push({
      id: `phase-${group.id}`,
      name: group.name,
      description: phaseDescription,
      tasks,
      max_iterations: 5,
    });
  }

  // Final integration phase: compile + all assertions
  const allAssertions = selectedGroups.flatMap((g) => g.assertions.filter((a) => a.enabled));
  const allUiChecks = allAssertions
    .filter((a) => DETERMINISTIC_TYPES.has(a.assertionType ?? "exists") && a.target?.criteria)
    .map((a) => assertionToUiCheck(a));

  if (allUiChecks.length > 0) {
    phases.push({
      id: "phase-integration",
      name: "Integration & Polish",
      description:
        "All individual features have been implemented. This phase verifies everything works together — all spec assertions pass simultaneously and the project compiles cleanly.",
      tasks: [
        {
          id: "task-integration-compile",
          name: "Full compilation check",
          description: "Verify the entire project compiles without errors",
          verifications: [{ type: "compile", description: "Full project compiles" }],
        },
        {
          id: "task-integration-ui",
          name: "Full spec verification",
          description: "All spec assertions pass against the live page",
          verifications: [
            {
              type: "ui_check",
              description: "All spec assertions pass",
              assertions: allUiChecks,
              snapshot_target: snapshotTarget as "control" | "sdk",
            },
          ],
        },
      ],
      max_iterations: 3,
    });
  }

  return phases;
}

/**
 * Generate a targeted plan from a spec diff.
 * Only creates phases for added/modified groups.
 */
export function buildSpecDiffPlan(
  diff: SpecDiff,
  fullSpec: SpecConfig,
  elementSource: "control" | "external" = "external",
): PlanPhase[] {
  if (!diff.hasChanges) return [];

  const snapshotTarget = elementSource === "external" ? "sdk" : "control";
  const groupMap = new Map(fullSpec.groups.map((g) => [g.id, g]));
  const phases: PlanPhase[] = [];

  // Added groups → full implementation phase
  for (const added of diff.addedGroups) {
    const group = groupMap.get(added.id);
    if (!group) continue;

    const enabledAssertions = group.assertions.filter((a) => a.enabled);
    if (enabledAssertions.length === 0) continue;

    phases.push({
      id: `phase-add-${group.id}`,
      name: `New: ${group.name}`,
      description: buildImplementationPrompt(group, enabledAssertions),
      tasks: buildTasksFromGroup(group, enabledAssertions, snapshotTarget),
      max_iterations: 5,
    });
  }

  // Modified groups → incremental update phase
  for (const modified of diff.modifiedGroups) {
    const group = groupMap.get(modified.id);
    if (!group) continue;

    // Only include new/changed assertions as verification targets
    const changedIds = new Set([
      ...modified.addedAssertions,
      ...modified.modifiedAssertions.map((m) => m.id),
    ]);

    const changedAssertions = group.assertions.filter((a) => a.enabled && changedIds.has(a.id));

    if (changedAssertions.length === 0) continue;

    const changeDesc = [];
    if (modified.addedAssertions.length > 0)
      changeDesc.push(`${modified.addedAssertions.length} new assertion(s)`);
    if (modified.modifiedAssertions.length > 0)
      changeDesc.push(`${modified.modifiedAssertions.length} modified assertion(s)`);
    if (modified.removedAssertions.length > 0)
      changeDesc.push(`${modified.removedAssertions.length} removed assertion(s)`);

    phases.push({
      id: `phase-update-${group.id}`,
      name: `Update: ${group.name}`,
      description: `The spec for "${group.name}" has been updated (${changeDesc.join(", ")}). Implement the changes to satisfy the new/modified assertions.\n\n${group.description}`,
      tasks: buildTasksFromGroup(
        { ...group, assertions: changedAssertions },
        changedAssertions,
        snapshotTarget,
      ),
      max_iterations: 3,
    });
  }

  // Removed groups → cleanup phase with not_exists checks
  const removedWithAssertions = diff.removedGroups.filter((g) => g.assertionCount > 0);
  if (removedWithAssertions.length > 0) {
    phases.push({
      id: "phase-cleanup",
      name: "Cleanup Removed Features",
      description: `The following features have been removed from the spec: ${removedWithAssertions.map((g) => g.name).join(", ")}. Remove the corresponding UI components and code.`,
      tasks: [
        {
          id: "task-cleanup",
          name: "Remove deprecated features",
          description: `Clean up code for removed spec groups: ${removedWithAssertions.map((g) => g.name).join(", ")}`,
          verifications: [{ type: "compile", description: "Project compiles after cleanup" }],
        },
      ],
      max_iterations: 2,
    });
  }

  return phases;
}

// ── Helpers ──────────────────────────────────────────────────────────

function buildTasksFromGroup(
  group: SpecGroup,
  enabledAssertions: SpecAssertion[],
  snapshotTarget: string,
): PlanTask[] {
  const tasks: PlanTask[] = [];

  // Group deterministic assertions into one ui_check task
  const deterministic = enabledAssertions.filter(
    (a) => DETERMINISTIC_TYPES.has(a.assertionType ?? "exists") && a.target?.criteria,
  );
  const aiRequired = enabledAssertions.filter(
    (a) => !DETERMINISTIC_TYPES.has(a.assertionType ?? "exists") || !a.target?.criteria,
  );

  if (deterministic.length > 0) {
    tasks.push({
      id: `task-${group.id}-ui`,
      name: `${group.name}: UI Elements`,
      description: deterministic.map((a) => `- ${a.description}`).join("\n"),
      verifications: [
        {
          type: "ui_check",
          description: `${group.name} — ${deterministic.length} UI assertion(s)`,
          assertions: deterministic.map(assertionToUiCheck),
          snapshot_target: snapshotTarget as "control" | "sdk",
        },
      ],
    });
  }

  if (aiRequired.length > 0) {
    tasks.push({
      id: `task-${group.id}-semantic`,
      name: `${group.name}: Semantic Checks`,
      description: aiRequired.map((a) => `- ${a.description}`).join("\n"),
      verifications: aiRequired.map((a) => ({
        type: "ai_review" as const,
        description: a.description,
        criteria: `${a.description} [${a.severity}]`,
      })),
    });
  }

  return tasks;
}

function assertionToUiCheck(a: SpecAssertion): UiCheckAssertion {
  return {
    id: a.id,
    description: a.description,
    assertionType:
      (a.assertionType as "exists" | "contains" | "visible" | "not_exists") ?? "exists",
    criteria: (a.target?.criteria as Record<string, unknown>) ?? {},
    expected: a.expected,
    severity: a.severity,
  };
}

function buildScaffoldDescription(spec: SpecConfig, pageUrl?: string): string {
  const desc = spec.description ?? "this page";
  return `Create the initial page structure for: ${desc}${pageUrl ? `\n\nThe page should be accessible at: ${pageUrl}` : ""}\n\nThis phase sets up the page component, routing, and basic layout. Subsequent phases will implement specific features defined in the spec.`;
}

function buildImplementationPrompt(
  group: SpecGroup,
  assertions: SpecAssertion[],
  projectContext?: string,
): string {
  const assertionList = assertions
    .map(
      (a, i) =>
        `${i + 1}. **${a.description}** [${a.severity}]${a.assertionType ? ` (${a.assertionType})` : ""}`,
    )
    .join("\n");

  const contextSection = projectContext ? `\n\n## Project Context\n\n${projectContext}` : "";

  return `## Implement: ${group.name}

${group.description}

You are implementing this feature from scratch. The verification steps describe the expected UI state — elements that should exist, text that should be visible, interactions that should work. Your job is to write the code that makes all verifications pass.

### Required Elements

${assertionList}

Verification failures tell you exactly what's missing. Read the failure details carefully and implement the required components, state management, and styling.${contextSection}`;
}
