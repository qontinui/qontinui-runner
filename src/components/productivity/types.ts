/**
 * Productivity Stack — TypeScript Type Contracts
 *
 * camelCase shapes returned by the Tauri commands wired up by the parallel
 * backend agent. Field names match the productivity-stack plan §6 contract
 * exactly; do not rename.
 */

export type PlanStatus = "draft" | "vetted" | "decomposed" | "implementing" | "done" | "abandoned";

export type TaskStatus =
  | "pending"
  | "ready"
  | "assigned"
  | "running"
  | "review"
  | "needs_fix"
  | "done"
  | "escalated"
  | "cancelled";

export interface PlanRow {
  id: string;
  markdownPath: string;
  versionHash: string;
  status: PlanStatus;
  title: string | null;
  summary: string | null;
  createdAt: string;
  updatedAt: string;
}

export interface TaskRow {
  id: string;
  planId: string;
  phaseName: string;
  sequenceInPhase: number;
  description: string;
  expectedFileClaims: string[];
  expectedDirs: string[];
  dependsOn: string[];
  status: TaskStatus;
  assignedSessionId: string | null;
  startedAt: string | null;
  completedAt: string | null;
  createdAt: string;
  updatedAt: string;
  notes: string | null;
}

export interface UpcomingClaim {
  planId: string;
  taskId: string;
  path: string;
  planVersionHash: string;
  registeredAtMs: number;
}

export interface TaskDetail {
  task: TaskRow;
  claimersByPath: Record<string, UpcomingClaim[]>;
  /** Phase 3: populated when a `reviews` row exists for the task. The
   *  backend formats it as a one-paragraph "verdict + confidence + top
   *  reason" digest. */
  latestReviewSummary: string | null;
  /** Phase 1: always null. Populated by Phase 5 when sessions get assigned. */
  workerSessionMeta: unknown | null;
}

/** Local sub-view router state for the productivity tab. */
export type ProductivityView = "plans" | "coordinator" | "knowledge";
