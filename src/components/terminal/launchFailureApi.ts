/**
 * Typed wrappers over the two launch-failure tauri commands (plan
 * `2026-11-20-terminal-launch-failure-analyze-button`, Phase 2 backend).
 *
 * Same `CommandResponse` reading as `aiLaunchCommand.ts`: the payload is
 * `resp.data`, and a data-less response is a failure carrying `resp.message`.
 */

import { invoke } from "@tauri-apps/api/core";
import type { CommandResponse } from "../../types";

/** What `capture_launch_evidence` wrote. */
export interface LaunchEvidence {
  /** Absolute path of the evidence file (`~/.qontinui/launch-evidence/…`). */
  path: string;
  /** The failed pane's cwd, or null when the backend could not resolve it. */
  cwd: string | null;
  /** Whether the fleet commands were provisioned into that cwd (D2). */
  commandsProvisioned: boolean;
}

/** Write the failed pane's launch-evidence file. Throws on any failure. */
export async function captureLaunchEvidence(params: {
  terminalId: string;
  launchCommand: string | null;
  configDir: string | null;
  pinnedSessionId: string | null;
}): Promise<LaunchEvidence> {
  const resp = await invoke<CommandResponse<LaunchEvidence>>("capture_launch_evidence", params);
  const data = resp.data;
  if (!data || typeof data.path !== "string" || data.path.length === 0) {
    throw new Error(resp.message ?? "capture_launch_evidence returned no path");
  }
  return {
    path: data.path,
    cwd: typeof data.cwd === "string" && data.cwd.length > 0 ? data.cwd : null,
    commandsProvisioned: data.commandsProvisioned === true,
  };
}

/**
 * The shim's launch-failure record for one pane, as READ:
 * - `recorded`: the shim beaconed a failure (`kind`, e.g. `resolve-failed`).
 * - `not-recorded`: the registry answered and holds nothing. That is NOT
 *   "launched OK" — only that no beacon arrived.
 * - `unread`: the invoke failed or answered nothing usable. Says nothing.
 */
export type LaunchFailureRead =
  | { state: "recorded"; kind: string; detail?: string }
  | { state: "not-recorded" }
  | { state: "unread" };

/** Read `get_terminal_launch_failure`. Never throws. */
export async function readTerminalLaunchFailure(
  terminalId: string,
  invoker: (cmd: string, args: Record<string, unknown>) => Promise<unknown> = invoke,
): Promise<LaunchFailureRead> {
  try {
    const resp = await invoker("get_terminal_launch_failure", { terminalId });
    const data = (resp as { data?: unknown } | null)?.data as
      | { recorded?: unknown; kind?: unknown; detail?: unknown }
      | undefined;
    if (!data || typeof data.recorded !== "boolean") return { state: "unread" };
    if (!data.recorded) return { state: "not-recorded" };
    if (typeof data.kind !== "string" || data.kind.length === 0) return { state: "unread" };
    return typeof data.detail === "string" && data.detail.length > 0
      ? { state: "recorded", kind: data.kind, detail: data.detail }
      : { state: "recorded", kind: data.kind };
  } catch {
    return { state: "unread" };
  }
}
