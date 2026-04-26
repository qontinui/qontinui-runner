/**
 * Productivity Stack — Reflection panel + plan-recommendations Tauri
 * command wrappers (Phase 5).
 *
 * Thin async wrappers around the new Phase 5 commands the runner exposes
 * for the per-plan ReflectionPanel and the Coordinator dashboard's
 * "suggest next plan" card. Field shapes are camelCase by virtue of
 * `serde(rename_all = "camelCase")` on the Rust structs in
 * `mcp::reflection`.
 */

import { invoke } from "@tauri-apps/api/core";
import type { PlanRow } from "./types";
import type { ReviewVerdict } from "./reviewsApi";

/** One row in the ReflectionPanel's "Top knowledge" list. */
export interface TopKnowledgeEntry {
  id: string;
  area: string;
  summary: string;
}

/** One row in the ReflectionPanel's "Review trail" list. */
export interface ReviewTrailEntry {
  reviewId: string;
  taskId: string;
  verdict: ReviewVerdict;
  confidence: number;
  createdAt: string;
}

/** Per-plan reflection — what the panel renders. `avgConfidence` is null
 *  when no reviews exist yet; the panel renders "—" in that case. */
export interface Reflection {
  planId: string;
  totalTasks: number;
  completedTasks: number;
  escalatedTasks: number;
  avgConfidence: number | null;
  knowledgeCount: number;
  topKnowledge: TopKnowledgeEntry[];
  reviewTrail: ReviewTrailEntry[];
}

/** A ranked plan candidate — what `<PlanRecommendations>` renders as one
 *  card in its top-3 list. */
export interface PlanRecommendation {
  planId: string;
  title: string | null;
  readyTaskCount: number;
  idleSessionCount: number;
  unconflictingClaimCount: number;
  /** Server-generated cheap-rule explanation (e.g. "3 ready tasks, 2 idle
   *  sessions, no upcoming-claim conflicts"). */
  reasoning: string;
}

/** Fetch the per-plan reflection rollup. */
export async function getReflection(planId: string): Promise<Reflection> {
  return invoke<Reflection>("get_reflection", { planId });
}

/** Fetch the top-3 plan recommendations the dashboard surfaces. */
export async function getPlanRecommendations(): Promise<PlanRecommendation[]> {
  return invoke<PlanRecommendation[]>("get_plan_recommendations");
}

/** Archive a plan (status → 'abandoned'). Returns true on update. */
export async function archivePlan(planId: string): Promise<boolean> {
  return invoke<boolean>("archive_plan", { planId });
}

/** Unarchive a plan (back to 'decomposed' if it has tasks; 'vetted' otherwise). */
export async function unarchivePlan(planId: string): Promise<boolean> {
  return invoke<boolean>("unarchive_plan", { planId });
}

/** List plans, optionally including archived rows (`done` / `abandoned`). */
export async function listPlansFiltered(includeArchived: boolean): Promise<PlanRow[]> {
  return invoke<PlanRow[]>("list_plans_filtered", { includeArchived });
}

/** List archived plans only. Convenience wrapper around
 *  `list_plans_filtered(true)` that filters down to `done`/`abandoned`. */
export async function listArchivedPlans(): Promise<PlanRow[]> {
  const rows = await listPlansFiltered(true);
  return rows.filter((p) => p.status === "done" || p.status === "abandoned");
}

/** Acknowledge an LLM-driven advisory — flips the underlying decision row's
 *  `resolved` flag. Returns true on update. */
export async function acknowledgeAdvisory(decisionId: string): Promise<boolean> {
  return invoke<boolean>("acknowledge_advisory", { decisionId });
}
