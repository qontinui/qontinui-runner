/**
 * Target-runner picker rules for the Orchestration Loop panels.
 *
 * Runner-owned truth first: `get_runner_instances` is this runner's own view
 * and the only source a published install has. The dev supervisor's
 * `/runners` is a supplement merged AFTER it, contributing rows only for ports
 * the runner-owned list does not already hold.
 *
 * Supervisor ids and runner-owned row ids (settings-slot ids, discovered
 * `ext-<port>-<n>` ids) are different namespaces. The backend resolves a
 * restart target by PORT and uses `target_runner_id` only as the dev
 * supervisor's id for a rebuild — it refuses an explicit id that is not the
 * supervisor's. So `target_runner_id` is sent ONLY for a supervisor row.
 *
 * Plan: 2026-09-22-orchestration-loop-restart-modes-depend-on-the-dev-only-supervisor.
 */

import { defaultBetween, restoreBetween } from "./restartCapability";

export interface TargetRunnerRow {
  id: string;
  name: string;
  port: number;
  running: boolean;
  pid: number | null;
  api_ready: boolean;
  /**
   * `"configured"` / `"discovered"` come from `get_runner_instances`;
   * `"supervisor"` marks a row only the dev supervisor's `/runners` reported.
   * Absent on older runner builds (treated as runner-owned).
   */
  source?: "configured" | "discovered" | "supervisor";
}

/** Subset of supervisor `/runners` rows the picker consumes. */
export interface SupervisorRunnerRow {
  id: string;
  name: string;
  port: number;
  is_primary: boolean;
  running: boolean;
  pid: number | null;
  api_responding: boolean;
}

/**
 * The picker's rows: running runner-owned rows first, then running
 * supervisor rows for ports not already present. The orchestrating runner's
 * own port is excluded from both (it is the "This runner (self)" option).
 */
export function mergeTargetRunners(
  owned: TargetRunnerRow[],
  supervisor: SupervisorRunnerRow[],
  ownPort: number | null,
): TargetRunnerRow[] {
  const ownedRows = owned.filter((r) => r.running && r.port !== ownPort);
  const taken = new Set(ownedRows.map((r) => r.port));
  const supplement: TargetRunnerRow[] = supervisor
    .filter((r) => r.running && r.port !== ownPort && !taken.has(r.port))
    .map((r) => ({
      id: r.id,
      name: r.name,
      port: r.port,
      running: r.running,
      pid: r.pid,
      api_ready: r.api_responding,
      source: "supervisor" as const,
    }));
  return [...ownedRows, ...supplement];
}

/** The `target_runner_id` to send for a picked row: the supervisor id, or null. */
export function targetRunnerIdFor(row: Pick<TargetRunnerRow, "id" | "source">): string | null {
  return row.source === "supervisor" ? row.id : null;
}

/** Form fields for a picker selection (`"self"` or a row id). */
export interface TargetSelection {
  targetRunner: string;
  targetPort: string;
  targetRunnerId: string;
}

export function selectTarget(value: string, rows: TargetRunnerRow[]): TargetSelection | null {
  if (value === "self") return { targetRunner: "self", targetPort: "", targetRunnerId: "" };
  const row = rows.find((r) => r.id === value);
  if (!row) return null;
  return {
    targetRunner: row.id,
    targetPort: String(row.port),
    targetRunnerId: targetRunnerIdFor(row) ?? "",
  };
}

/**
 * Target fields and "Between" value for a restored saved config. The target
 * is re-matched by port against the current rows, and the id is re-derived
 * from the matched row (a saved runner-owned id is never re-sent). With no
 * matching row the saved port/id are kept as saved — the capability check
 * then explains what is wrong with them. The saved mode is kept verbatim.
 */
export function restoreTarget(
  saved: { targetPort?: string; targetRunnerId?: string; between?: string },
  rows: TargetRunnerRow[],
): TargetSelection & { between: string } {
  const targetPort = saved.targetPort || "";
  const between = restoreBetween(saved.between, !targetPort);
  if (!targetPort) return { targetRunner: "self", targetPort: "", targetRunnerId: "", between };
  const match = rows.find((r) => String(r.port) === targetPort);
  if (!match) {
    return {
      targetRunner: "self",
      targetPort,
      targetRunnerId: saved.targetRunnerId || "",
      between,
    };
  }
  return {
    targetRunner: match.id,
    targetPort,
    targetRunnerId: targetRunnerIdFor(match) ?? "",
    between,
  };
}

/** The "Between" value a fresh form starts on (target = this runner). */
export const FRESH_FORM_BETWEEN = defaultBetween(true);

/**
 * A token that changes whenever the runner list changes in a way that can
 * flip a capability verdict — a runner appearing, stopping, or being
 * relaunched (new pid) — so the panels re-ask the capability command after
 * e.g. the user launches an instance in Settings.
 */
export function runnerListToken(
  rows: Pick<TargetRunnerRow, "id" | "port" | "running" | "pid" | "source">[],
): string {
  return rows
    .map((r) => `${r.source ?? "owned"}:${r.id}:${r.port}:${r.running ? 1 : 0}:${r.pid ?? ""}`)
    .sort()
    .join("|");
}
