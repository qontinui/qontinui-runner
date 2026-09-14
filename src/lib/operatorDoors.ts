/**
 * Operator doors — the runner UI's own way to start prompts, workflows and task
 * resumes (plan `2026-09-13-drained-runner-never-reaches-idle`, review round 1).
 *
 * The runner's HTTP spawn routes (`/prompts/run`, `/unified-workflows/{id}/run`,
 * …) cannot tell this UI from a script or a UI-Bridge-driven automation, so
 * while coord has drained the device they refuse every caller as autonomous
 * (409). These Tauri commands are the same doors called in-process as an
 * OPERATOR, which the drain never defers. Each returns the door's HTTP status
 * and JSON body unchanged, so callers keep their existing response handling.
 *
 * UI-Bridge-driven starts must keep using the HTTP routes — they are
 * autonomous by design.
 */

import { invoke } from "@tauri-apps/api/core";

export type OperatorDoorCommand =
  | "operator_run_prompt"
  | "operator_run_unified_workflow"
  | "operator_execute_inline_workflow"
  | "operator_run_composed_workflow"
  | "operator_generate_unified_workflow_async"
  | "operator_resume_task_run";

export interface OperatorDoorReply<T = unknown> {
  status: number;
  /** `status` in 200–299, mirroring `Response.ok`. */
  ok: boolean;
  body: T;
}

/** PURE: shape a raw command reply. Exported for tests. */
export function toOperatorDoorReply<T>(raw: { status: number; body: T }): OperatorDoorReply<T> {
  return { status: raw.status, ok: raw.status >= 200 && raw.status < 300, body: raw.body };
}

export async function invokeOperatorDoor<T = unknown>(
  command: OperatorDoorCommand,
  args: Record<string, unknown>,
): Promise<OperatorDoorReply<T>> {
  return toOperatorDoorReply(await invoke<{ status: number; body: T }>(command, args));
}

/** The `error` string a door's JSON body carries, if any. */
export function doorError(body: unknown): string | undefined {
  if (body && typeof body === "object" && "error" in body) {
    const e = (body as { error?: unknown }).error;
    return typeof e === "string" && e.length > 0 ? e : undefined;
  }
  return undefined;
}
