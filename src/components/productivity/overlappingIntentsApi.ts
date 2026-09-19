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

/**
 * Describe a THROWN value, whatever shape it arrived in.
 *
 * THE DEFECT this closes: a loader written as
 * `err instanceof Error ? err.message : "<bare constant>"` takes the `else`
 * arm on every real Tauri failure — `invoke()` rejects with a plain STRING
 * carrying the Rust command's own error text — and so DISCARDS the diagnosis.
 * The overlapping-intents panel once said `Failed to load overlap pairs` and
 * nothing else, no matter why it failed.
 *
 * Everything non-`Error` is serialized rather than dropped: a string
 * verbatim, an object's `status` / `code` / `error` / `message` / `detail`
 * fields when it has any, and a JSON dump as the last resort. `fallback` is
 * used ONLY when the value carries no information at all — losing the cause
 * is strictly worse than showing it ugly.
 */
export function describeThrown(err: unknown, fallback: string): string {
  if (err instanceof Error) return err.message || fallback;
  if (typeof err === "string") return err.trim() || fallback;
  if (typeof err === "number" || typeof err === "boolean") return String(err);
  if (err && typeof err === "object") {
    const o = err as Record<string, unknown>;
    const status =
      typeof o.status === "number" || typeof o.status === "string" ? `HTTP ${o.status}` : null;
    const body = [o.error, o.message, o.detail, o.code].find(
      (c): c is string => typeof c === "string" && c.trim() !== "",
    );
    if (status && body) return `${status}: ${body.trim()}`;
    if (body) return body.trim();
    if (status) return status;
    try {
      const dump = JSON.stringify(err);
      if (dump && dump !== "{}") return `${fallback} (${dump})`;
    } catch {
      // Circular / non-serializable — fall through to the fallback.
    }
  }
  return fallback;
}
