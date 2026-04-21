/**
 * Scripted-output telemetry (TS side).
 *
 * Writes the three TS-originating events from
 * {@link ScriptedOutputHandler.summarizeViaScript} via the
 * `emit_scripted_output_event` Tauri command.  The Rust emitter
 * (`src-tauri/src/step_output/script_emitter.rs`) writes the other three
 * (`cache_hit`, `llm_ok`, `fallback`) directly through the same FK-aware
 * insert path.
 *
 * All six events are aggregated by the LLM Analytics widget via
 * `get_scripted_output_stats`.
 *
 * We deliberately DO NOT use the generic `insert_activity_entry` command:
 * that path runs PII scrubbing and has no FK pre-check, so an ad-hoc
 * `task_run_id` (e.g. during tests or early in a step handler) would be
 * dropped by the `activity_timeline.task_run_id → task_runs(id)`
 * constraint. The dedicated command reuses the Rust emitter's FK-aware
 * fallback (null out the column, preserve the raw id in metadata).
 *
 * Telemetry is fire-and-forget: a failed write must never disrupt the
 * emitter path. Outside a Tauri context (unit tests, web build) the
 * helper is a no-op.
 */

import { invoke, isTauri } from "@tauri-apps/api/core";

import { createLogger } from "../logger";

const log = createLogger("scripted-output-handler.telemetry");

export type ScriptedOutputEventName =
  | "scripted_output.attempted"
  | "scripted_output.worker_ok"
  | "scripted_output.bytes_avoided"
  // The Rust side emits `scripted_output.fallback` for LLM-path rejections
  // (cost_cap, timeout, breaker_open, …).  The TS side emits it too, with
  // reason `bad_expression`, when a worker-executed script throws.
  | "scripted_output.fallback";

/**
 * Fire-and-forget activity_timeline write.  Safe to call with an
 * undefined `taskRunId` — the Rust command records the event with a
 * NULL FK column in that case (and preserves the raw value in
 * `metadata.task_run_id_raw` when one was supplied but didn't resolve).
 */
export function emitScriptedOutputEvent(
  name: ScriptedOutputEventName,
  metadata: Record<string, unknown>,
  taskRunId?: string | null,
): void {
  if (!isTauri()) return;

  void invoke("emit_scripted_output_event", {
    name,
    metadata,
    taskRunId: taskRunId ?? null,
  }).catch((err) => {
    log.debug("scripted-output telemetry write failed for %s: %s", name, describeError(err));
  });
}

function describeError(err: unknown): string {
  if (err instanceof Error) return err.message;
  try {
    return String(err);
  } catch {
    return "<unknown error>";
  }
}
