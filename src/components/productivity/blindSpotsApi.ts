/**
 * Blind-Spot Recommender — types + fetcher.
 *
 * Backend: `GET /blind-spots` (runner MCP API). The endpoint scores every
 * observability sub-space (DOM, filesystem path/content, git ref/commit,
 * process instance, coordinator) for "demanded but unobserved" regions and
 * returns a ranked list of instrumentation recommendations.
 *
 * Response envelope is the runner's standard `{ success, data }` shape;
 * `data` is pre-sorted by `score` descending (highest-value blind spot
 * first). This mirrors `fileActivityApi.ts`'s HTTP fetch mechanism exactly
 * (port resolution via `resolvePort()`, `fetch`, `resp.ok` guard) — it does
 * NOT use the Tauri `invoke` path because the contract is an HTTP endpoint.
 */

import { resolvePort } from "@/lib/runner-api";

/** Observability sub-space a blind region belongs to. Matches the Rust
 *  `SubSpace` enum (serde camelCase). */
export type SubSpace =
  | "dom"
  | "fsPath"
  | "fsContent"
  | "gitRef"
  | "gitCommit"
  | "procInstance"
  | "coord";

/** The recommended remediation kind for a blind region. Matches the Rust
 *  `RecommendationKind` enum (serde camelCase). */
export type RecommendationKind = "reactivate-observer" | "extend-scope" | "deploy-observer";

/** Estimated implementation cost of acting on a recommendation. */
export type EstimatedCost = "low" | "medium" | "high";

/** A contiguous region of an observability sub-space the framework is
 *  blind to, with the members that fall inside it. */
export interface BlindRegion {
  subSpace: SubSpace;
  blindMembers: string[];
  confidence: number;
}

/** Per-component breakdown of the blind-spot score. The sum of these is
 *  `ScoredBlindSpot.score`. */
export interface ScoreBreakdown {
  gitSupervisedStates: number;
  unmatchedEvents: number;
  verificationGaps: number;
  stakesWeighted: number;
}

/** The actionable recommendation attached to a scored blind spot. */
export interface Recommendation {
  kind: RecommendationKind;
  detail: string;
  estimatedCost: EstimatedCost;
  expectedGain: number;
}

/** One ranked blind spot — a region, its score, and what acting on it
 *  would unblock. The list is sorted by `score` descending server-side. */
export interface ScoredBlindSpot {
  region: BlindRegion;
  score: number;
  scoreBreakdown: ScoreBreakdown;
  unblocks: string[];
  recommendation: Recommendation;
}

/** Runner standard response envelope for `GET /blind-spots`. */
interface BlindSpotsEnvelope {
  success: boolean;
  data: ScoredBlindSpot[];
}

/**
 * Fetch the ranked blind-spot recommendations. Unwraps the runner's
 * `{ success, data }` envelope and returns the `data` array as-is (already
 * sorted by score descending). Throws on a non-200 response or a
 * `success: false` body so the panel can render its error state.
 */
export async function getBlindSpots(signal?: AbortSignal): Promise<ScoredBlindSpot[]> {
  const url = `http://127.0.0.1:${resolvePort()}/blind-spots`;
  const resp = await fetch(url, { signal });
  if (!resp.ok) {
    throw new Error(`blind-spots fetch failed: HTTP ${resp.status}`);
  }
  const body = (await resp.json()) as BlindSpotsEnvelope;
  if (!body.success) {
    throw new Error("blind-spots fetch failed: server reported success=false");
  }
  return body.data ?? [];
}
