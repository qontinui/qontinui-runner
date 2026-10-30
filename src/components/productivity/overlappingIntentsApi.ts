/**
 * Overlapping intents — the one coord read the Productivity page keeps.
 *
 * Extracted from the deleted `coordinatorApi.ts` by Phase 4 of plan
 * `2026-09-12-consolidate-local-orchestration-onto-conductor`: the dashboard
 * that hosted this fetch is gone, and this is the L2 visibility surface
 * (Coordination Phase 1B §4.10) that survives it.
 */

import { invoke } from "@tauri-apps/api/core";

/**
 * Unordered pair of agents whose declared_overlap_paths intersect.
 *
 * `agentA` <= `agentB` lexically so the pair is stable across calls.
 * `intentA` / `intentB` are the agents' free-text intents; `overlappingPaths`
 * is the de-duplicated intersection (literal + glob-expanded — the same shape
 * coord publishes on `events.coord.overlap.detected`).
 *
 * WIRE SHAPE IS PINNED: coord serializes this camelCase for this consumer.
 * Do not rename a field here without the matching change on the Tauri
 * command `list_overlapping_intents`.
 */
export interface OverlappingIntentPair {
  agentA: string;
  agentB: string;
  intentA: string | null;
  intentB: string | null;
  overlappingPaths: string[];
}

/**
 * Fetch the L2 overlapping-intents snapshot.
 *
 * Coord computes the pairs and serves them over HTTP
 * (`GET /coord/agent-worktrees/overlapping-intents`); the runner proxies
 * that through `list_overlapping_intents` and never queries
 * `coord.agent_worktrees` itself. Read-only — the panel is informational
 * and takes no action client-side.
 *
 * A rejection is a FAILED READ, and the caller renders it as UNKNOWN — this
 * function never swallows it into an empty list, because "no overlap" and
 * "could not ask" are different facts.
 */
export async function listOverlappingIntents(limit = 200): Promise<OverlappingIntentPair[]> {
  return invoke<OverlappingIntentPair[]>("list_overlapping_intents", { limit });
}
