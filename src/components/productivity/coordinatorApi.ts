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
