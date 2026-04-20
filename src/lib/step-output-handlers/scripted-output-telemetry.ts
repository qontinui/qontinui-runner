/**
 * Scripted-output telemetry (TS side).
 *
 * Writes the three TS-originating events from
 * {@link ScriptedOutputHandler.summarizeViaScript} into the shared
 * `activity_timeline` table via the `insert_activity_entry` Tauri command.
 * The Rust emitter (`src-tauri/src/step_output/script_emitter.rs`) writes
 * the other three (`cache_hit`, `llm_ok`, `fallback`) directly.
 *
 * All six events are aggregated by the LLM Analytics widget via
 * `get_scripted_output_stats` (Phase C).
 *
 * Telemetry is fire-and-forget: a failed write must never disrupt the
 * emitter path.  Outside a Tauri context (unit tests, web build) the
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
 * undefined `taskRunId` — the Rust side tolerates that via the literal
 * "unassigned" sentinel.
 */
export function emitScriptedOutputEvent(
  name: ScriptedOutputEventName,
  metadata: Record<string, unknown>,
  taskRunId?: string | null,
): void {
  if (!isTauri()) return;

  // Field names are camelCase because `ActivityTimelineInput` carries
  // `#[serde(rename_all = "camelCase")]`.  Optional fields with `None` on
  // the Rust side must be omitted (serde `skip_serializing_if` is
  // deserialize-asymmetric).
  const input: Record<string, unknown> = {
    textContent: name,
    sourceType: "scripted_output",
    captureMode: "runner",
    appName: "runner",
    taskRunId: taskRunId ?? "unassigned",
    metadataJson: safeStringify(metadata),
  };

  void invoke("insert_activity_entry", { input }).catch((err) => {
    log.debug("scripted-output telemetry write failed for %s: %s", name, describeError(err));
  });
}

function safeStringify(value: unknown): string {
  try {
    return JSON.stringify(value) ?? "{}";
  } catch {
    return "{}";
  }
}

function describeError(err: unknown): string {
  if (err instanceof Error) return err.message;
  try {
    return String(err);
  } catch {
    return "<unknown error>";
  }
}
