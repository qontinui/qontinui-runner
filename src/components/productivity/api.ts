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
