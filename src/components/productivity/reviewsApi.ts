/**
 * Productivity Stack — Review automation Tauri command wrappers (Phase 3).
 *
 * Thin async wrappers around the new Phase 3 commands the runner exposes
 * for the Coordinator dashboard's Recommendations queue and the
 * `ReviewBadge` per-tab status indicator. Field shapes are camelCase by
 * virtue of `serde(rename_all = "camelCase")` on the Rust structs in
 * `database/pg/reviews.rs` and `commands/productivity.rs`.
 */

import { invoke } from "@tauri-apps/api/core";

/** Verdict tag emitted by `/auto-review`. */
export type ReviewVerdict = "approved" | "needs_fix" | "escalate";

/** Diff summary produced by `git diff --stat`, attached to each review. */
export interface ReviewDiffSummary {
  filesChanged: number;
  linesAdded: number;
  linesRemoved: number;
}

/** A single review row. */
export interface ReviewRow {
  id: string;
  taskId: string;
  reviewerSessionId: string;
  reviewedSessionId: string;
  verdict: ReviewVerdict;
  /** [0, 1] inclusive. */
  confidence: number;
  reasoning: string;
  diffSummary: ReviewDiffSummary | null;
  testResults: Record<string, unknown> | null;
  /** Set once the user approves/rejects a medium-confidence
   *  recommendation; null otherwise. */
  userDecision: "approved" | "rejected" | null;
  userDecidedAt: string | null;
  createdAt: string;
}

/** A review enriched with task + plan context for the Recommendations
 *  panel. Matches the TS contract in productivity-stack §8. */
export type Recommendation = ReviewRow & {
  taskDescription: string;
  planTitle: string | null;
};

/** Fetch reviews waiting in the medium-confidence approval gate
 *  (verdict=approved AND confidence in [0.7, 0.85) AND
 *  user_decision IS NULL). */
export async function getRecommendations(): Promise<Recommendation[]> {
  return invoke<Recommendation[]>("get_recommendations");
}

/** Convenience alias matching the spec's "getRecommendationsForReview"
 *  method-name in §6 — same payload, narrower verb. */
export async function getRecommendationsForReview(): Promise<Recommendation[]> {
  return getRecommendations();
}

/** User accepts a recommendation. Backend records the decision, flips the
 *  task to `done`, and emits `review-approved`. Returns true if the row
 *  updated (existed and wasn't already decided). */
export async function approveRecommendation(reviewId: string): Promise<boolean> {
  return invoke<boolean>("approve_recommendation", { reviewId });
}

/** User declines a recommendation. Backend records the decision and
 *  emits `review-rejected`. Task state is unchanged — manual triage. */
export async function rejectRecommendation(reviewId: string): Promise<boolean> {
  return invoke<boolean>("reject_recommendation", { reviewId });
}
