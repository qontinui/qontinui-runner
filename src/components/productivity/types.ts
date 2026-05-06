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
  /** Phase 4 (productivity-coordinator-completion-reports): structured
   *  handoff payload written to `coord.tasks.completion_report`. Null when
   *  the task has not produced a report yet. */
  completionReport: CompletionReport | null;
  /** Tag identifying who produced `completionReport`. Null when no report
   *  has been written. */
  completionSource: CompletionSource | null;
  /** True when the task has stashed upstream reports waiting to be injected
   *  into its first message — Rule E populates this when flipping pending →
   *  ready. The "Briefing preview" panel is gated on this flag. */
  hasAssignmentBriefExtras: boolean;
}

// ============================================================================
// Completion reports — Phase 4 type contract.
//
// Mirrors the Rust `CompletionReport` types in
// `database/pg/completion_reports.rs`. serde renames to camelCase so these
// match the wire format exactly.
// ============================================================================

/** Open enum of `Deliverable.kind` values. The Rust side accepts any string;
 *  these are the conventional values surfaced by the manual-fire form. */
export type DeliverableKind =
  | "commit"
  | "pr"
  | "file"
  | "endpoint"
  | "schema-change"
  | "spec"
  | "plan-stamp"
  | "fixture"
  | "doc-update"
  | "other";

export type FollowUpPriority = "critical" | "important" | "nice-to-have";

export type CompletionSource =
  | "session-self-report"
  | "reviewer-attestation"
  | "github-merge"
  | "ci-pipeline"
  | "manual-user-fire"
  | "legacy-pre-completion-reports";

export interface Deliverable {
  kind: string;
  reference: string;
  description: string;
}

export interface BreakingChange {
  area: string;
  description: string;
  migrationStepsMd: string;
}

export interface FollowUp {
  description: string;
  priority: string;
  blockingForDependents: boolean;
}

export interface CompletionReport {
  summaryMd: string;
  deliverables: Deliverable[];
  breakingChanges: BreakingChange[];
  followUps: FollowUp[];
  artifacts?: Record<string, unknown>;
}

/** Local sub-view router state for the productivity tab. */
export type ProductivityView = "plans" | "coordinator" | "knowledge";
