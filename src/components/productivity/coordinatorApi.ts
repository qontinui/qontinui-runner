/**
 * Productivity Stack — Coordinator dashboard Tauri command wrappers.
 *
 * Thin async wrappers around the new Phase 2 commands the runner exposes
 * for the Coordinator dashboard. Field shapes are camelCase by virtue of
 * `serde(rename_all = "camelCase")` on the Rust structs in
 * `database/pg/coordinator_decisions.rs`.
 */

import { invoke } from "@tauri-apps/api/core";

/** Discriminator strings written to `coordinator_decisions.rule`. */
export type CoordinatorRule = "A" | "B" | "C" | "D" | "E" | "LLM" | "idle" | string;

/** All actions the Coordinator can record. Matches the TS contract in
 *  productivity-stack §8. */
export type CoordinatorActionName =
  | "assign-task"
  | "pause-session"
  | "merge-task"
  | "reassign-needs-fix"
  | "escalate"
  | "advise-with-text"
  | "escalate-with-text"
  | "kill-session"
  | "force-promote-to-worktree"
  | "cancel-task"
  | "idle-no-action";

/** A single decision-log row. Open escalations are the subset where
 *  `autoActed === false` AND `action` is in the destructive set. */
export interface CoordinatorDecision {
  id: string;
  sessionId: string;
  iteration: number;
  rule: CoordinatorRule;
  action: CoordinatorActionName | string;
  targetId: string | null;
  reasoning: string;
  autoActed: boolean;
  resolved: boolean;
  resolution: string | null;
  resolvedAt: string | null;
  createdAt: string;
}

/** An open escalation — a `coordinator_decisions` row with
 *  `autoActed=false` and `resolved=false`. The Rust side returns the same
 *  `CoordinatorDecision` shape; alias kept for readability at call sites. */
export type Escalation = CoordinatorDecision;

/** Filter inputs for the Decision Log dropdown. Empty string means "no
 *  filter". */
export interface DecisionLogFilter {
  rule?: string;
  action?: string;
}

/** Fetch the Decision Log feed (newest-first), optionally filtered. */
export async function getCoordinatorDecisions(
  limit = 200,
  filter: DecisionLogFilter = {},
): Promise<CoordinatorDecision[]> {
  const ruleFilter = filter.rule && filter.rule.length > 0 ? filter.rule : null;
  const actionFilter = filter.action && filter.action.length > 0 ? filter.action : null;
  return invoke<CoordinatorDecision[]>("get_coordinator_decisions", {
    limit,
    ruleFilter,
    actionFilter,
  });
}

/** Fetch unresolved escalations (`autoActed=false`, destructive action). */
export async function getEscalations(): Promise<Escalation[]> {
  return invoke<Escalation[]>("get_escalations");
}

/** Mark an escalation resolved with a free-form `resolution` note. */
export async function resolveEscalation(decisionId: string, resolution: string): Promise<boolean> {
  return invoke<boolean>("resolve_escalation", { decisionId, resolution });
}

// ---------------------------------------------------------------------------
// Phase 3 — Coordinator launch controls (header buttons + lease status pill)
// ---------------------------------------------------------------------------

/** A single `coordinator_leader` lease row. Timestamps are RFC3339 UTC
 *  strings (the Rust side serializes them via `::text` from PG). */
export interface CoordinatorLeaderRow {
  instanceId: string;
  leasedUntil: string;
  acquiredAt: string;
  renewedAt: string;
}

/** Result of `get_coordinator_leader`. `leaseStatus` is computed Rust-side
 *  off `leased_until` vs `NOW()` and `renewed_at` heartbeat freshness:
 *  - `active`  — lease holder is alive (renewed within ~90s).
 *  - `stale`   — lease still owned but heartbeat older than ~90s.
 *  - `vacant`  — no row, or `leased_until` already in the past. */
export interface LeaderResponse {
  leader: CoordinatorLeaderRow | null;
  leaseStatus: "active" | "stale" | "vacant";
}

/** Result of `launch_coordinator_session` / `spawn_worker_session`.
 *
 *  - `mode` echoes back which path ran. For `launch_coordinator_session`
 *    it's `"rust"` (in-process scheduler — Phase 1.5 default) or
 *    `"claude_skill"` (legacy pty spawn). `spawn_worker_session` returns
 *    `"worker"`.
 *  - `terminalId` is `null` for `mode === "rust"` (no pty was opened);
 *    for the other modes it identifies the new tab so the sidebar
 *    state-machine can focus it.
 *  - `taskRunId` is the `task_run_id` under which a worker is registered
 *    with `SessionManager` (Phase 6). Populated only for `mode ===
 *    "worker"`; `null` for the other modes. */
export interface LaunchResult {
  mode: string;
  terminalId: string | null;
  taskRunId: string | null;
}

/** Read the current `coordinator_leader` row + derived lease status. */
export async function getCoordinatorLeader(): Promise<LeaderResponse> {
  return invoke<LeaderResponse>("get_coordinator_leader");
}

/** Coordinator launch modes. `"rust"` (the Phase 1.5 default) flips the
 *  in-process Rust scheduler's runtime-toggle flag — no pty, no Claude
 *  CLI dependency. `"claude_skill"` keeps the legacy pty-spawn path for
 *  Joshua's debug use. */
export type CoordinatorLaunchMode = "rust" | "claude_skill";

/** Start the Coordinator. Defaults to `mode = "rust"` (in-process Rust
 *  scheduler). When `mode === "claude_skill"`, spawns a Claude pty in a
 *  fresh Terminal tab — the legacy debug path. */
export async function launchCoordinatorSession(args: {
  mode?: CoordinatorLaunchMode | null;
  planPath?: string | null;
  titleHint?: string | null;
}): Promise<LaunchResult> {
  return invoke<LaunchResult>("launch_coordinator_session", args);
}

/** Stop the in-process Rust coordinator scheduler — flips the
 *  `rust_scheduler_enabled` flag back to `false`. Returns the previous
 *  flag value (true ⇒ scheduler was running, false ⇒ already stopped).
 *
 *  Has no effect on `claude_skill`-mode pty sessions; the user closes
 *  those via the terminal UI. */
export async function stopCoordinatorSession(): Promise<boolean> {
  return invoke<boolean>("stop_coordinator_session");
}

/** Spawn an idle worker Claude session in a fresh Terminal tab. */
export async function spawnWorkerSession(args: {
  titleHint?: string | null;
}): Promise<LaunchResult> {
  return invoke<LaunchResult>("spawn_worker_session", args);
}

// ---------------------------------------------------------------------------
// POST /coordinator/tasks/reset-stale — flip stale assigned/needs_fix tasks
// back to `ready` when their `assigned_session_id` no longer exists in
// SessionManager. Useful between test runs against pre-decomposed plan
// fixtures. Idempotent — second call sees the rows already in `ready` and
// reports them under `skipped`.
// ---------------------------------------------------------------------------

/** One task examined by the reset-stale sweep that was NOT flipped, with
 *  the stable reason string (`"status not assigned/needs_fix"`,
 *  `"session still alive"`, `"no assigned_session_id"`). */
export interface ResetStaleSkippedTask {
  taskId: string;
  status: string;
  assignedSessionId: string | null;
  reason: string;
}

/** Response shape for `POST /coordinator/tasks/reset-stale`. `reset` is the
 *  set of task ids that were (or, in `dryRun` mode, would be) flipped back
 *  to `ready`. `skipped` is the audit trail. */
export interface ResetStaleTasksResponse {
  reset: string[];
  skipped: ResetStaleSkippedTask[];
  dryRun: boolean;
}

/** Flip stale `assigned`/`needs_fix` tasks back to `ready` when the worker
 *  they're pinned to is no longer alive in SessionManager. Pass
 *  `{ dryRun: true }` to preview without writing. Mirrors the runner-port
 *  fetch pattern used by the manual-user-fire form (calls
 *  `get_api_port` then POSTs to the resolved port). */
export async function resetStaleTasks(
  args: { dryRun?: boolean } = {},
): Promise<ResetStaleTasksResponse> {
  const port = await invoke<number>("get_api_port");
  if (!port || port <= 0) throw new Error(`invalid runner port: ${port}`);
  const qs = args.dryRun ? "?dryRun=true" : "";
  const res = await fetch(`http://localhost:${port}/coordinator/tasks/reset-stale${qs}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: "",
  });
  if (!res.ok) {
    const text = await res.text().catch(() => "");
    throw new Error(`HTTP ${res.status}: ${text || res.statusText}`);
  }
  return (await res.json()) as ResetStaleTasksResponse;
}

// ---------------------------------------------------------------------------
// GET /coordinator/state — snapshot of live sessions + non-terminal tasks.
// Used by the Workers panel so the dashboard can show users the workers they
// just spawned without auto-revealing the terminal tab. Mirrors the runner-
// port fetch pattern (no dedicated Tauri command exists for this endpoint).
// ---------------------------------------------------------------------------

/** A single live session row from `GET /coordinator/state`. `terminalId`
 *  and `title` are populated only for pty-backed Workers (Phase 6); for
 *  ClaudeSessions both are absent (the Rust side uses
 *  `skip_serializing_if = "Option::is_none"`). */
export interface LiveSession {
  taskRunId: string;
  state: string;
  isActive: boolean;
  terminalId?: string | null;
  title?: string | null;
}

/** Subset of the `/coordinator/state` payload the Workers panel consumes.
 *  The endpoint returns more fields (file registries, escalations, etc.)
 *  but the panel only needs `liveSessions` + `tasks` (for the
 *  assigned-task lookup that surfaces "task X — description" on
 *  Processing rows). Extra fields are ignored. */
export interface CoordinatorState {
  liveSessions: LiveSession[];
  tasks: Array<{
    id: string;
    status: string;
    assignedSessionId: string | null;
    updatedAt: string;
    description: string;
  }>;
}

/** Fetch the composite coordinator state snapshot. Mirrors
 *  `resetStaleTasks` — calls `get_api_port` then GETs the HTTP endpoint
 *  on the runner's API server. */
export async function getCoordinatorState(): Promise<CoordinatorState> {
  const port = await invoke<number>("get_api_port");
  if (!port || port <= 0) throw new Error(`invalid runner port: ${port}`);
  const res = await fetch(`http://localhost:${port}/coordinator/state`, {
    method: "GET",
    headers: { Accept: "application/json" },
  });
  if (!res.ok) {
    const text = await res.text().catch(() => "");
    throw new Error(`HTTP ${res.status}: ${text || res.statusText}`);
  }
  return (await res.json()) as CoordinatorState;
}

/** Discriminated wire shape consumed by `POST /coordinator/dispatch-action`.
 *  Mirrors `CoordinatorAction` (kebab-case `type`, camelCase fields). Only
 *  the variants the deferrals panel actually fires are listed; extend as
 *  more dashboard surfaces need manual dispatch. */
export type DispatchCoordinatorAction =
  | { type: "assign-task"; taskId: string; sessionId: string; reasoning?: string };

/** Fire an arbitrary CoordinatorAction through the manual dispatch path.
 *  Audited as `rule = "manual"`, `sessionId = "manual-<uuid>"`. */
export async function dispatchCoordinatorAction(
  action: DispatchCoordinatorAction,
  reasoning?: string,
): Promise<void> {
  const port = await invoke<number>("get_api_port");
  if (!port || port <= 0) throw new Error(`invalid runner port: ${port}`);
  const res = await fetch(`http://localhost:${port}/coordinator/dispatch-action`, {
    method: "POST",
    headers: { "Content-Type": "application/json", Accept: "application/json" },
    body: JSON.stringify({ action, reasoning }),
  });
  if (!res.ok) {
    const text = await res.text().catch(() => "");
    throw new Error(`HTTP ${res.status}: ${text || res.statusText}`);
  }
}
