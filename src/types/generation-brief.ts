/**
 * GenerationBrief — structured hints the spec path feeds into the AI
 * workflow generation pipeline. Produced by `buildSpecBrief` and consumed
 * by `generateFromBrief`, which serializes it into the prompt-ready
 * context block for `generate_workflow_standalone`.
 *
 * The brief is the bridge that unifies the deterministic spec→workflow
 * path with the Builder-Verifier-Fixer AI pipeline.
 */

import type { SetupAction, SpecAssertion } from "../lib/workflow-builder/buildSpecWorkflow";

export interface BriefGroup {
  id: string;
  name: string;
  description: string;
  /** Preconditions derived from the spec — describe UI state assertions depend on (e.g., "Findings panel is open"). */
  preconditions: string[];
  /** Setup actions copied verbatim from spec.setupActions for the group (if any). */
  setupHints: SetupAction[];
  /** Assertions that CAN be batched into a single snapshot_assert step (exists/contains/visible/not_exists with targets). */
  deterministicAssertions: SpecAssertion[];
  /** Assertions that need AI reasoning (semantic, behavior, source_review, or deterministic types missing a target). */
  semanticAssertions: SpecAssertion[];
  /** ID of a state in the spec stateMachine matching this group's preconditions, if any. Enables one-step state-machine navigation instead of click+wait pairs. */
  targetStateId?: string;
}

export interface GenerationBrief {
  version: "1";
  specId?: string;
  /** Short human summary — e.g., "Verify page spec: Terminal (13 groups, 55 assertions)". */
  summary: string;
  pageUrl?: string;
  elementSource: "control" | "external";
  workflowName?: string;
  groups: BriefGroup[];
  /** Optional high-level instructions added to the brief. */
  additionalInstructions?: string;
}
