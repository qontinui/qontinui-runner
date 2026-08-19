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

// ----------------------------------------------------------------------------
// Briefing preview — Phase 4 of productivity-coordinator-completion-reports.
// ----------------------------------------------------------------------------

/**
 * Server-side rendered preview of the assignment-brief Markdown that Rule B
 * will inject into the worker's first message for `taskId`. Returns the
 * empty string when no brief would be composed (no upstreams stashed and no
 * resume-from-pause audit row).
 *
 * The Rust handler calls `coordinator::act::build_assignment_brief`, the
 * exact function used by the `AssignTask` apply branch — so the preview is,
 * by construction, identical to what the worker will see.
 */
export async function previewAssignmentBrief(taskId: string): Promise<string> {
  return invoke<string>("preview_assignment_brief", { taskId });
}
