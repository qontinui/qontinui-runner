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

/** Result of `launch_coordinator_session` / `spawn_worker_session`. The
 *  frontend uses `terminalId` only to confirm the launch succeeded; the
 *  sidebar state-machine `navigateTo("tab-terminal")` reveals the new
 *  pty without needing the id. */
export interface LaunchResult {
  terminalId: string;
}

/** Read the current `coordinator_leader` row + derived lease status. */
export async function getCoordinatorLeader(): Promise<LeaderResponse> {
  return invoke<LeaderResponse>("get_coordinator_leader");
}

/** Spawn a Coordinator Claude session in a fresh Terminal tab. The new
 *  pty runs `claude "/coordinate ..."` with the runner port baked in. */
export async function launchCoordinatorSession(args: {
  planPath?: string | null;
  titleHint?: string | null;
}): Promise<LaunchResult> {
  return invoke<LaunchResult>("launch_coordinator_session", args);
}

/** Spawn an idle worker Claude session in a fresh Terminal tab. */
export async function spawnWorkerSession(args: {
  titleHint?: string | null;
}): Promise<LaunchResult> {
  return invoke<LaunchResult>("spawn_worker_session", args);
}
