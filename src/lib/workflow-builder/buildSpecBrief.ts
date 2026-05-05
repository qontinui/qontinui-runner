/**
 * buildSpecBrief
 *
 * Converts a SpecConfig into a GenerationBrief — the structured hint
 * payload the AI pipeline consumes. Mirrors `buildSpecWorkflow` inputs
 * but emits a partitioned (deterministic vs semantic) view of the spec
 * instead of a UnifiedWorkflow.
 *
 * Partitioning rules:
 *  - `isDeterministic(assertion)` → deterministicAssertions
 *  - everything else → semanticAssertions
 *
 * Preconditions are derived from:
 *  1. Any `assertion.precondition` string present on a group's assertions
 *     (deduped, preserving first-seen order).
 *  2. A heuristic scan of the group name and each assertion description
 *     for UI-container keywords (panel/modal/dialog/sidebar/overlay). For
 *     each keyword hit not already covered by an explicit precondition,
 *     adds "<Keyword> must be open".
 */

import type { GenerationBrief, BriefGroup } from "../../types/generation-brief";
import type { SpecAssertion, SpecConfig, SetupAction, SpecStateShape } from "./buildSpecWorkflow";
import { isDeterministic } from "./buildSpecWorkflow";

export interface BuildSpecBriefInput {
  /** The spec configuration to build from */
  specConfig: SpecConfig;
  /** Which group IDs to include (default: all groups) */
  selectedGroupIds?: Set<string>;
  /** Element source: "control" (runner UI) or "external" (browser tab).
   *  Falls back to specConfig.metadata.elementSource, then "control". */
  elementSource?: "control" | "external";
  /** Page URL for navigation / summary hints (optional) */
  pageUrl?: string;
  /** Workflow name override */
  workflowName?: string;
  /** Additional high-level instructions to attach to the brief */
  additionalInstructions?: string;
}

const CONTAINER_KEYWORDS = ["panel", "modal", "dialog", "sidebar", "overlay"] as const;

function capitalize(word: string): string {
  if (!word) return word;
  return word.charAt(0).toUpperCase() + word.slice(1);
}

/**
 * Extract an explicit precondition string from an assertion, if any.
 * Tolerant of shape drift — accepts any string at `assertion.precondition`.
 */
function readAssertionPrecondition(assertion: SpecAssertion): string | undefined {
  const raw = assertion.precondition;
  if (typeof raw === "string" && raw.trim().length > 0) {
    return raw.trim();
  }
  return undefined;
}

/**
 * Walk a single group's name + assertion descriptions and emit heuristic
 * preconditions for any container keyword (panel/modal/dialog/sidebar/overlay)
 * not already covered by an explicit precondition.
 */
function derivePreconditions(groupName: string, assertions: SpecAssertion[]): string[] {
  const preconditions: string[] = [];
  const seen = new Set<string>();

  // Pass 1: explicit preconditions declared on assertions.
  for (const a of assertions) {
    const explicit = readAssertionPrecondition(a);
    if (explicit && !seen.has(explicit.toLowerCase())) {
      seen.add(explicit.toLowerCase());
      preconditions.push(explicit);
    }
  }

  // Pass 2: heuristic — scan group name + assertion descriptions for container words.
  const haystack = [groupName, ...assertions.map((a) => a.description || "")]
    .join(" ")
    .toLowerCase();

  for (const keyword of CONTAINER_KEYWORDS) {
    if (!haystack.includes(keyword)) continue;

    const candidate = `${capitalize(keyword)} must be open`;
    const candidateKey = candidate.toLowerCase();
    if (seen.has(candidateKey)) continue;

    // If an explicit precondition already mentions this keyword, assume covered.
    const alreadyCovered = preconditions.some((p) => p.toLowerCase().includes(keyword));
    if (alreadyCovered) continue;

    seen.add(candidateKey);
    preconditions.push(candidate);
  }

  return preconditions;
}

/**
 * Try to match any of the group's free-text preconditions to a state in the
 * spec's stateMachine. Returns the matching state's `id`, or undefined if no
 * state matches. Matching is intentionally simple (substring on lowercased
 * name, then token-split on id) — the plan calls out "simple substring match
 * is fine."
 */
function matchPreconditionsToState(
  preconditions: string[],
  states: SpecStateShape[],
): string | undefined {
  for (const p of preconditions) {
    const lower = p.toLowerCase();

    // Prefer a name-based match (two-way substring so "Findings Panel Open"
    // matches "Findings panel must be open" in either direction).
    const byName = states.find((s) => {
      const name = s.name.toLowerCase();
      return lower.includes(name) || name.includes(lower);
    });
    if (byName) return byName.id;

    // Fallback: id-based match — every token of the hyphen/underscore-split
    // id must appear in the precondition text.
    const byId = states.find((s) => {
      const idTokens = s.id.toLowerCase().split(/[-_]/).filter(Boolean);
      if (idTokens.length === 0) return false;
      return idTokens.every((t) => lower.includes(t));
    });
    if (byId) return byId.id;
  }
  return undefined;
}

function buildBriefGroup(
  group: SpecConfig["groups"][number],
  stateMachineStates?: SpecStateShape[],
): BriefGroup {
  const enabled = group.assertions.filter((a) => a.enabled);
  const deterministic: SpecAssertion[] = [];
  const semantic: SpecAssertion[] = [];
  for (const a of enabled) {
    if (isDeterministic(a)) deterministic.push(a);
    else semantic.push(a);
  }

  const setupHints: SetupAction[] = group.setupActions ? [...group.setupActions] : [];
  const preconditions = derivePreconditions(group.name, enabled);

  const targetStateId =
    stateMachineStates && stateMachineStates.length > 0
      ? matchPreconditionsToState(preconditions, stateMachineStates)
      : undefined;

  // Defensive sanity check — Known Risks §4 of the state-machine-navigation
  // plan calls out spec drift. `targetStateId` is derived from `states`, so
  // this should be impossible, but fail loudly if it ever isn't.
  if (
    targetStateId !== undefined &&
    !(stateMachineStates ?? []).some((s) => s.id === targetStateId)
  ) {
    throw new Error(
      `[buildSpecBrief] targetStateId "${targetStateId}" for group "${group.id}" does not match any state in the spec stateMachine. This indicates spec drift.`,
    );
  }

  return {
    id: group.id,
    name: group.name,
    description: group.description,
    preconditions,
    setupHints,
    deterministicAssertions: deterministic,
    semanticAssertions: semantic,
    ...(targetStateId !== undefined ? { targetStateId } : {}),
  };
}

/**
 * Build a GenerationBrief from a spec configuration.
 */
export function buildSpecBrief(input: BuildSpecBriefInput): GenerationBrief {
  const { specConfig, selectedGroupIds, pageUrl, workflowName, additionalInstructions } = input;

  const metadataSource = specConfig.metadata?.elementSource as "control" | "external" | undefined;
  const elementSource = input.elementSource ?? metadataSource ?? "control";

  const selectedGroups = selectedGroupIds
    ? specConfig.groups.filter((g) => selectedGroupIds.has(g.id))
    : specConfig.groups;

  const stateMachineStates = specConfig.stateMachine?.states;
  const groups = selectedGroups.map((g) => buildBriefGroup(g, stateMachineStates));

  const totalAssertions = groups.reduce(
    (sum, g) => sum + g.deterministicAssertions.length + g.semanticAssertions.length,
    0,
  );

  const specId =
    typeof specConfig.metadata?.specId === "string"
      ? (specConfig.metadata.specId as string)
      : undefined;

  const summaryLabel = pageUrl ?? specId ?? "unnamed";
  const summary = `Verify page spec: ${summaryLabel} (${groups.length} groups, ${totalAssertions} assertions)`;

  return {
    version: "1",
    specId,
    summary,
    pageUrl,
    elementSource,
    workflowName,
    groups,
    additionalInstructions,
  };
}
