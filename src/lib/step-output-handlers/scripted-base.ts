/**
 * ScriptedOutputHandler — base class for handlers that may use
 * "think-in-code" indirection for high-volume output.
 *
 * When the raw output from a step exceeds {@link SCRIPTED_OUTPUT_THRESHOLD_BYTES}
 * and the `QONTINUI_SCRIPTED_OUTPUT=1` env flag is set, the handler asks
 * the registered {@link ScriptEmitter} for a short JS expression and runs
 * it inside a sandboxed `node:vm` worker (see {@link runScript}).  Only
 * the script's result is piped back to the summarizer/LLM — not the raw
 * output.
 *
 * If any step fails (emitter rejects, worker throws, timeout, result is
 * non-JSON-serializable) the handler falls back to the legacy truncation
 * behavior, logs a warning, and records the fallback on the return value.
 *
 * See {@link plans/scout-2026-04-16-orchestration-upgrades.md} Phase 2.
 */

import type { StepOutput, StepOutputType } from "../../types/step-output";
import type {
  StepOutputHandler,
  StepOutputMetadata,
  TestAutoPopulateConfig,
  AssertableField,
} from "./types";
import { createLogger } from "../logger";
import { runScript, DEFAULT_SCRIPT_TIMEOUT_MS, ScriptWorkerError } from "./script-worker";
import { getScriptEmitter } from "./script-emitter";

const log = createLogger("scripted-output-handler");

/** Byte-size threshold above which scripted extraction is attempted. */
export const SCRIPTED_OUTPUT_THRESHOLD_BYTES = 8192;

/** Env-var name that gates scripted extraction at runtime. */
export const SCRIPTED_OUTPUT_ENV_FLAG = "QONTINUI_SCRIPTED_OUTPUT";

/** Size of the "preview" the emitter sees (enough to grok schema). */
const SCRIPT_EMITTER_PREVIEW_BYTES = 2048;

/** Fallback truncation cap — matches pre-existing command-handler behavior. */
const FALLBACK_TRUNCATE_BYTES = 4000;

/** Signature of a safe-access check; evaluated lazily to allow tests to mutate env. */
function scriptedOutputEnabled(): boolean {
  try {
    // `process` is available under Node / Vitest; guard for browser builds too.
    return (
      typeof process !== "undefined" &&
      process.env != null &&
      process.env[SCRIPTED_OUTPUT_ENV_FLAG] === "1"
    );
  } catch {
    return false;
  }
}

/**
 * Result returned by {@link ScriptedOutputHandler.summarizeViaScript}.
 */
export interface ScriptedSummarizationResult {
  /** The extracted value.  On fallback, this is the truncated raw string. */
  extracted: unknown;
  /** Bytes elided by the indirection (0 on fallback). */
  bytesAvoided: number;
  /** True when the worker path failed and truncation was used. */
  fallback: boolean;
}

/**
 * Abstract base class.  Concrete handlers keep their existing
 * `StepOutputHandler` shape and call {@link summarizeViaScript} from
 * inside their `summarizeForAI` / async sibling implementation when they
 * want scripted indirection.
 */
export abstract class ScriptedOutputHandler<
  T extends StepOutput = StepOutput,
> implements StepOutputHandler<T> {
  abstract readonly stepType: StepOutputType;
  abstract readonly displayName: string;
  abstract readonly description: string;

  abstract parseOutput(rawOutput: unknown, stepConfig?: unknown, metadata?: StepOutputMetadata): T;
  abstract summarizeForAI(output: T): string;
  abstract toTestConfig(output: T): TestAutoPopulateConfig | null;
  abstract getAssertableFields(output: T): AssertableField[];

  /**
   * Extract a minimal representation of `rawOutput` via the LLM-emitted
   * script path.  Returns a safe fallback (truncated raw string) on any
   * failure — never throws.
   *
   * @param rawOutput   The raw output string that may be too large.
   * @param goal        Human-readable description of what the summary
   *                    needs (drives the emitter prompt).
   * @param schemaHint  Optional schema hint passed to the emitter.
   *                    Currently used only for log context; reserved for
   *                    when the emitter interface grows a schema param.
   */
  protected async summarizeViaScript(
    rawOutput: string,
    goal: string,
    schemaHint?: object,
  ): Promise<ScriptedSummarizationResult> {
    const byteLength = rawOutput.length;

    // Gate: below threshold OR flag off → pass raw through unchanged.
    // This is the "fallback path" in the non-scripted sense: the caller
    // uses the raw string directly and no indirection is attempted.
    if (byteLength <= SCRIPTED_OUTPUT_THRESHOLD_BYTES || !scriptedOutputEnabled()) {
      return { extracted: rawOutput, bytesAvoided: 0, fallback: false };
    }

    const preview = rawOutput.slice(0, SCRIPT_EMITTER_PREVIEW_BYTES);

    let expr: string;
    try {
      expr = await getScriptEmitter().emit(goal, preview);
    } catch (err) {
      log.warn(
        "Script emitter rejected; falling back to truncation. goal=%s err=%s",
        goal,
        describeError(err),
      );
      return this.truncationFallback(rawOutput);
    }

    try {
      const extracted = await runScript(expr, rawOutput, DEFAULT_SCRIPT_TIMEOUT_MS);
      const outputBytes = approximateJsonByteLength(extracted);
      const bytesAvoided = Math.max(0, byteLength - outputBytes);
      return { extracted, bytesAvoided, fallback: false };
    } catch (err) {
      log.warn(
        "Scripted extraction failed; falling back to truncation. goal=%s schema=%s expr=%s err=%s",
        goal,
        schemaHint ? JSON.stringify(Object.keys(schemaHint)) : "none",
        truncateForLog(expr),
        err instanceof ScriptWorkerError ? err.message : describeError(err),
      );
      return this.truncationFallback(rawOutput);
    }
  }

  private truncationFallback(rawOutput: string): ScriptedSummarizationResult {
    const truncated =
      rawOutput.length > FALLBACK_TRUNCATE_BYTES
        ? rawOutput.substring(0, FALLBACK_TRUNCATE_BYTES) + "\n... (truncated)"
        : rawOutput;
    return {
      extracted: truncated,
      bytesAvoided: 0,
      fallback: true,
    };
  }
}

function approximateJsonByteLength(value: unknown): number {
  try {
    const json = JSON.stringify(value);
    return json ? json.length : 0;
  } catch {
    return 0;
  }
}

function truncateForLog(s: string): string {
  if (s.length <= 120) return s;
  return s.slice(0, 117) + "...";
}

function describeError(err: unknown): string {
  if (err instanceof Error) return err.message;
  try {
    return String(err);
  } catch {
    return "<unknown error>";
  }
}
