/**
 * Wire shapes for the Projects dashboard.
 *
 * These mirror `qontinui_types::projects` (qontinui-schemas
 * `rust/src/projects.rs`) serialized with `#[serde(rename_all = "camelCase")]`.
 * The enums are `snake_case` on the wire — `ProcessState` and `SessionSource`
 * and `HealthLevel` all carry `#[serde(rename_all = "snake_case")]`, so
 * `ExternallyOwned` crosses as `"externally_owned"`.
 *
 * WHY HAND-WRITTEN, and what would replace it: the generated bindings exist in
 * the schemas repo (`ts/src/generated/`), but that tree is untracked build
 * output and `.github/workflows/publish.yml` has no codegen step, so
 * `@qontinui/shared-types` has failed to publish since 2026-06-29 and npm is
 * stuck at 0.8.1 while this app pins `^0.6.0`. Until that pipeline is repaired,
 * importing these from the package is not possible. When it is, delete this
 * file and re-export from `@qontinui/shared-types` — the Rust struct is the
 * source of truth and these declarations are a mirror, not a second definition.
 *
 * `SavedProject` deliberately is NOT redeclared here; it already has one
 * canonical TS home in `@/hooks/useSavedProjects`.
 */

import type { SavedProject } from "@/hooks/useSavedProjects";

/** Lifecycle state as reported by the process manager. */
export type ProcessState =
  | "stopped"
  | "starting"
  | "building"
  | "running"
  | "healthy"
  | "stopping"
  | "failed"
  | "externally_owned";

/** Process states that mean "this is up right now". */
const LIVE_PROCESS_STATES: ReadonlySet<ProcessState> = new Set<ProcessState>([
  "running",
  "healthy",
  "externally_owned",
]);

/** True when the state counts as running for the card's status dot. */
export function isLiveProcessState(state: ProcessState): boolean {
  return LIVE_PROCESS_STATES.has(state);
}

/** Where a {@link SessionLite} was observed. */
export type SessionSource = "terminal" | "touched_files";

/** The traffic light. `unknown` means "nothing observable", not "broken". */
export type HealthLevel = "green" | "amber" | "red" | "unknown";

export interface ProcessStatusLite {
  id: string;
  name: string;
  state: ProcessState;
  healthPort?: number;
  /** `undefined` when the process declares no health port. */
  portHealthy?: boolean;
  cwd: string;
}

export interface SessionLite {
  id: string;
  name?: string;
  /** `task_runs.status` for AI sessions; absent for live terminals. */
  status?: string;
  source: SessionSource;
  lastActivityMs?: number;
  filesTouched: number;
  workingDir?: string;
}

export interface GitCommitLite {
  sha: string;
  subject: string;
  committedMs: number;
}

export interface GitLite {
  branch?: string;
  dirtyCount: number;
  lastCommits: GitCommitLite[];
}

export interface PendingQuestion {
  id: string;
  taskRunId: string;
  question: string;
  riskLevel?: string;
  createdAtMs?: number;
}

export interface HealthLite {
  level: HealthLevel;
  /** A sentence a non-developer can read. */
  reason: string;
  failingProcesses: string[];
}

/** The one joined view the dashboard renders, computed server-side per project. */
export interface ProjectSnapshot {
  project: SavedProject;
  processes: ProcessStatusLite[];
  liveSessions: SessionLite[];
  recentSessions: SessionLite[];
  git?: GitLite;
  questions: PendingQuestion[];
  health: HealthLite;
  /**
   * Rolling 7-day spend in USD. `undefined` means "not measured" — distinct
   * from `0`, which means "measured, and it was free".
   */
  spend7dUsd?: number;
  lastActivityMs?: number;
}
