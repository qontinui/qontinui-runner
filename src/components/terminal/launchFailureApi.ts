/**
 * Typed wrappers over the two launch-failure tauri commands (plan
 * `2026-11-20-terminal-launch-failure-analyze-button`, Phase 2 backend).
 *
 * THE WIRE SHAPE IS BARE, not `CommandResponse`. Both commands return a typed
 * struct shared with their HTTP twin (`capture_launch_evidence` →
 * `Result<LaunchEvidence, String>`, `get_terminal_launch_failure` →
 * `LaunchFailureView`, `src-tauri/src/commands/terminal.rs`), the same bare
 * `Result<T, String>` convention `session_ledger_report` uses in that file.
 * Tauri serializes an `Ok(T)` as `T` itself and rejects the invoke with the
 * `Err` string, so the payload is the resolved value — there is no `.data`.
 * The HTTP twins wrap the same struct in `ApiResponse`; that is the HTTP
 * route's envelope, not the IPC one.
 */

import { invoke } from "@tauri-apps/api/core";

type Invoker = (cmd: string, args: Record<string, unknown>) => Promise<unknown>;

/** What `capture_launch_evidence` wrote. */
export interface LaunchEvidence {
  /** Absolute path of the evidence file (`~/.qontinui/launch-evidence/…`). */
  path: string;
  /** The failed pane's cwd, or null when the backend could not resolve it. */
  cwd: string | null;
  /** Whether the fleet commands were provisioned into that cwd (D2). */
  commandsProvisioned: boolean;
}

/**
 * Write the failed pane's launch-evidence file. Throws on any failure: the
 * backend's refusal (the invoke rejects with its message) or a payload that
 * names no evidence path.
 */
export async function captureLaunchEvidence(
  params: {
    terminalId: string;
    launchCommand: string | null;
    configDir: string | null;
    pinnedSessionId: string | null;
  },
  invoker: Invoker = invoke,
): Promise<LaunchEvidence> {
  const data = (await invoker("capture_launch_evidence", params)) as Partial<
    Record<keyof LaunchEvidence, unknown>
  > | null;
  if (!data || typeof data.path !== "string" || data.path.length === 0) {
    throw new Error("capture_launch_evidence returned no evidence path");
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

/** Read `get_terminal_launch_failure` (a bare `LaunchFailureView`). Never throws. */
export async function readTerminalLaunchFailure(
  terminalId: string,
  invoker: Invoker = invoke,
): Promise<LaunchFailureRead> {
  try {
    const data = (await invoker("get_terminal_launch_failure", { terminalId })) as {
      recorded?: unknown;
      kind?: unknown;
      detail?: unknown;
    } | null;
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
