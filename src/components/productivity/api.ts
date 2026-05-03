/**
 * Productivity Stack — Tauri command wrappers.
 *
 * Thin async wrappers around the `productivity_*` Tauri commands wired up by
 * the parallel backend agent. Phase 1 only consumes plan/task list + detail;
 * the upcoming-claim endpoints are exposed for future use by the
 * Coordinator dashboard (Phase 2).
 */

import { invoke } from "@tauri-apps/api/core";
import type { PlanRow, TaskDetail, TaskRow, UpcomingClaim } from "./types";

/** GET all plans ordered by `updated_at DESC`. */
export async function listPlans(): Promise<PlanRow[]> {
  return invoke<PlanRow[]>("list_plans");
}

/** GET tasks for a single plan, ordered by `phase_name, sequence_in_phase`. */
export async function getPlanTasks(planId: string): Promise<TaskRow[]> {
  return invoke<TaskRow[]>("get_plan_tasks", { planId });
}

/** GET full detail for a single task. */
export async function getTaskDetail(taskId: string): Promise<TaskDetail> {
  return invoke<TaskDetail>("get_task_detail", { taskId });
}

/** GET upcoming file claims, scoped to a plan or all plans. */
export async function getUpcomingClaims(planId: string | null = null): Promise<UpcomingClaim[]> {
  return invoke<UpcomingClaim[]>("get_upcoming_claims", { planId });
}

/** Bulk-lookup: which tasks (if any) plan to claim each of the given paths. */
export async function checkPathClaims(paths: string[]): Promise<Record<string, UpcomingClaim[]>> {
  return invoke<Record<string, UpcomingClaim[]>>("check_path_claims", { paths });
}

// ----------------------------------------------------------------------------
// Decompose Plan — Phase 3 (in-product /decompose-plan replacement)
// ----------------------------------------------------------------------------

/**
 * Result returned by the `decompose_plan` Tauri command. `idempotentSkip`
 * is true when the plan was already decomposed at this hash; `stamped` is
 * true when a fresh `<!-- decomposed: -->` marker was appended to the
 * markdown.
 */
export interface DecomposeResult {
  planId: string;
  taskCount: number;
  versionHash: string;
  idempotentSkip: boolean;
  stamped: boolean;
}

/**
 * Decompose a plan markdown file into the in-product task graph. Reads
 * the file, asks the active LLM provider to identify phases/tasks/
 * claims/dependencies, then POSTs the structured payload to the runner's
 * `/plans/decompose` endpoint. Throws on any failure — callers should
 * catch and inspect the message for the "Configure an LLM provider…"
 * affordance string.
 */
export async function decomposePlan(planPath: string): Promise<DecomposeResult> {
  return invoke<DecomposeResult>("decompose_plan", { planPath });
}

// ----------------------------------------------------------------------------
// Auto-Review — Phase 4 (in-product /auto-review replacement)
// ----------------------------------------------------------------------------

/** Result returned by the `auto_review_task` Tauri command. */
export interface ReviewResult {
  reviewId: string;
  verdict: string;
  confidence: number;
}

/**
 * Run an auto-review against `taskId`. Persists a `reviews` row and emits
 * the `review-completed` Tauri event so ReviewBadge updates.
 *
 * Throws when the LLM provider isn't configured — the error message is
 * prefixed with "LLM provider not configured" so the caller can render a
 * link to Settings → AI rather than a generic failure toast.
 */
export async function autoReviewTask(taskId: string): Promise<ReviewResult> {
  return invoke<ReviewResult>("auto_review_task", { taskId });
}
