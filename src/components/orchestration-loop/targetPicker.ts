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

import {
  defaultBetween,
  restoreBetween,
  type BetweenWire,
  type RestartCapabilityProbe,
} from "./restartCapability";

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

/** Picker value for a restored target whose port no current row matches (yet). */
export const SAVED_TARGET_PREFIX = "saved:";

export function isUnmatchedSavedTarget(targetRunner: string): boolean {
  return targetRunner.startsWith(SAVED_TARGET_PREFIX);
}

/**
 * Target fields and "Between" value for a restored saved config.
 *
 * The target is matched by PORT against the current rows and its id is
 * re-derived from the matched row, so a saved runner-owned id is never
 * re-sent. The saved id itself is NEVER sent: it may predate the rule that
 * only a dev-supervisor id goes out, and without a row there is no way to tell
 * a supervisor id from a runner-owned one.
 *
 * With no matching row yet (the list has not loaded, or the target is not
 * running) the port is kept, the picker shows the saved target as such
 * rather than "self", and `pending` asks the panel to re-match when rows
 * arrive ({@link rematchPendingTarget}). The saved mode is kept verbatim.
 */
export function restoreTarget(
  saved: { targetPort?: string; targetRunnerId?: string; between?: string },
  rows: TargetRunnerRow[],
): TargetSelection & { between: string; pending: boolean } {
  const targetPort = saved.targetPort || "";
  const between = restoreBetween(saved.between, !targetPort);
  if (!targetPort) {
    return { targetRunner: "self", targetPort: "", targetRunnerId: "", between, pending: false };
  }
  const match = rematchPendingTarget(targetPort, rows);
  if (!match) {
    return {
      targetRunner: `${SAVED_TARGET_PREFIX}${targetPort}`,
      targetPort,
      targetRunnerId: "",
      between,
      pending: true,
    };
  }
  return { ...match, between, pending: false };
}

/**
 * Re-match a restored target's port against rows that have since arrived.
 * `null` while still unmatched.
 */
export function rematchPendingTarget(
  targetPort: string,
  rows: TargetRunnerRow[],
): TargetSelection | null {
  const match = rows.find((r) => String(r.port) === targetPort);
  if (!match) return null;
  return {
    targetRunner: match.id,
    targetPort,
    targetRunnerId: targetRunnerIdFor(match) ?? "",
  };
}

/**
 * Restart-capability probes for the loops a multi-loop start will actually
 * send (the spec-partition wizard's generated entries), so the pre-start
 * verdict is asked about exactly what Start would submit.
 */
export function probesFromLoopEntries(
  loops: Array<{
    config: {
      target_runner_port: number | null;
      target_runner_id: string | null;
      supervisor_port: number;
      between_iterations: BetweenWire;
    };
  }>,
): RestartCapabilityProbe[] {
  return loops.map(({ config }) => ({
    target_runner_port: config.target_runner_port,
    target_runner_id: config.target_runner_id,
    supervisor_port: config.supervisor_port,
    between_iterations: config.between_iterations,
  }));
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
