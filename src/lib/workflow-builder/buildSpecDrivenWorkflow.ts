/**
 * buildSpecDrivenWorkflow
 *
 * Orchestrator that produces a UnifiedWorkflow from a spec in one of three modes:
 *   - "implement": Full implementation from spec (multi-stage plan workflow)
 *   - "update": Targeted changes from spec diff (only changed groups become phases)
 *   - "verify": Existing verification-only workflow (delegates to buildSpecWorkflow)
 */

import type { UnifiedWorkflow } from "../../types/unified-workflow";
import type { SpecConfig } from "./buildSpecWorkflow";
import { buildSpecWorkflow } from "./buildSpecWorkflow";
import { buildPlanWorkflow } from "./buildPlanWorkflow";
import { buildSpecImplementationPlan, buildSpecDiffPlan } from "./buildSpecImplementationPlan";
import { diffSpecs } from "../spec-diff";

export interface SpecDrivenWorkflowInput {
  /** The spec to implement/verify */
  specConfig: SpecConfig;
  /** Mode: implement from scratch, update from diff, or verify only */
  mode: "implement" | "update" | "verify";
  /** For "update" mode: the previous spec version to diff against */
  previousSpec?: SpecConfig;
  /** Element source */
  elementSource?: "control" | "external";
  /** Page URL */
  pageUrl?: string;
  /** Additional project context for agentic prompts */
  projectContext?: string;
  /**
   * Max iterations per phase. `null` (or omitted) means unlimited — the
   * plan exits on success or explicit stop rather than an iteration count.
   */
  maxIterations?: number | null;
  /** Workflow name override */
  workflowName?: string;
}

/**
 * Build a workflow from a spec based on the selected mode.
 */
export function buildSpecDrivenWorkflow(input: SpecDrivenWorkflowInput): UnifiedWorkflow {
  const { mode } = input;

  switch (mode) {
    case "implement":
      return buildImplementWorkflow(input);
    case "update":
      return buildUpdateWorkflow(input);
    case "verify":
      return buildVerifyWorkflow(input);
  }
}

function buildImplementWorkflow(input: SpecDrivenWorkflowInput): UnifiedWorkflow {
  const phases = buildSpecImplementationPlan({
    specConfig: input.specConfig,
    elementSource: input.elementSource,
    projectContext: input.projectContext,
    pageUrl: input.pageUrl,
  });

  const workflow = buildPlanWorkflow({
    phases,
    maxIterations: input.maxIterations ?? null,
    workflowName:
      input.workflowName ??
      `Spec Implementation — ${input.specConfig.description?.slice(0, 60) ?? "page"}`,
    snapshotTarget: input.elementSource === "external" ? "sdk" : "control",
  });

  // Override category and tags for spec-driven workflows
  workflow.category = "spec-driven";
  workflow.tags = ["spec", "implementation", "plan-generated", "spec-driven"];

  return workflow;
}

function buildUpdateWorkflow(input: SpecDrivenWorkflowInput): UnifiedWorkflow {
  if (!input.previousSpec) {
    // No previous spec — fall back to full implementation
    return buildImplementWorkflow(input);
  }

  const diff = diffSpecs(input.previousSpec, input.specConfig);

  if (!diff.hasChanges) {
    // No changes — return a verify-only workflow
    return buildVerifyWorkflow(input);
  }

  const phases = buildSpecDiffPlan(diff, input.specConfig, input.elementSource);

  if (phases.length === 0) {
    return buildVerifyWorkflow(input);
  }

  const workflow = buildPlanWorkflow({
    phases,
    maxIterations: input.maxIterations ?? null,
    workflowName: input.workflowName ?? `Spec Update — ${diff.summary}`,
    snapshotTarget: input.elementSource === "external" ? "sdk" : "control",
  });

  workflow.category = "spec-driven";
  workflow.tags = ["spec", "update", "plan-generated", "spec-driven", "diff-based"];

  return workflow;
}

function buildVerifyWorkflow(input: SpecDrivenWorkflowInput): UnifiedWorkflow {
  return buildSpecWorkflow({
    specConfig: input.specConfig,
    elementSource: input.elementSource,
    pageUrl: input.pageUrl,
    maxIterations: input.maxIterations ?? null,
    workflowName: input.workflowName,
  });
}
