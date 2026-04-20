/**
 * Script Emitter
 *
 * Adapter interface for the LLM "script emitter" loop used by
 * {@link ScriptedOutputHandler}: given a truncated output preview and a
 * natural-language goal, return a short JS expression (in terms of a
 * variable named `output`) that extracts the minimum data the downstream
 * summarizer actually needs.
 *
 * ## Why this exists
 *
 * Phase 2 of the orchestration-upgrades plan (see
 * `plans/scout-2026-04-16-orchestration-upgrades.md`) calls for a
 * think-in-code indirection layer.  The production emitter
 * ({@link TauriScriptEmitter}) routes the prompt through the runner's
 * Rust `ai_provider` module via a Tauri command (see
 * `plans/script-emitter-wiring-2026-04-18.md` — Phase A shipped the Rust
 * side, Phase B wires this TS shim).
 *
 * In non-Tauri contexts (e.g. unit tests, web builds) the
 * {@link NoopScriptEmitter} preserves legacy truncation semantics so the
 * pipeline remains safe-by-default.
 */

import { createLogger } from "../logger";
import { invoke } from "@tauri-apps/api/core";

const log = createLogger("scripted-output-handler.emitter");

/**
 * Optional, structured context that an emitter can use to improve extraction
 * quality and enforce per-run cost ceilings.
 *
 * Both fields are optional so callers that don't have them yet can keep the
 * old two-arg call shape; the implementation supplies safe defaults.
 */
export interface ScriptEmitterOptions {
  /**
   * Top-level key+type summary of the expected extracted shape (see
   * {@link summarizeSchemaHint}).  The Rust emitter uses this to stabilize
   * cache keys across runs whose data differs but whose structure is
   * identical, and includes it in the LLM prompt.  If omitted, the
   * {@link TauriScriptEmitter} will derive one from `outputPreview`.
   */
  schemaHint?: unknown;
  /**
   * Task-run identifier for cost-budget enforcement on the Rust side.
   * When `null` or omitted, per-run caps are not applied — fine for ad-hoc
   * or test calls, but production callers should pass the real task_run_id.
   */
  taskRunId?: string | null;
}

export interface ScriptEmitter {
  /**
   * Emit a JS expression that will be run against the full output.
   *
   * @param goal           Human-readable description of what the summarizer
   *                       needs to extract (e.g. "find the last 10 ERROR
   *                       lines from a log").
   * @param outputPreview  The first ~2KB of the output, provided as
   *                       context for the LLM.  Full output is NOT passed
   *                       here — the whole point of the indirection is to
   *                       avoid that cost.
   * @param opts           Optional structured context — see
   *                       {@link ScriptEmitterOptions}.
   * @returns              A single-expression JS string referencing
   *                       `output`.  Must be synchronous and
   *                       JSON-serializable.
   */
  emit(goal: string, outputPreview: string, opts?: ScriptEmitterOptions): Promise<string>;
}

/**
 * Default emitter: returns a trivial slice-first-4KB expression.
 *
 * This preserves the legacy truncation semantics so the infrastructure
 * remains safe-by-default even when no LLM surface is wired.  It also logs
 * a warning so operators notice when the feature flag is set but no
 * emitter has been registered.
 */
class NoopScriptEmitter implements ScriptEmitter {
  private warned = false;

  async emit(goal: string, _outputPreview: string, _opts?: ScriptEmitterOptions): Promise<string> {
    if (!this.warned) {
      this.warned = true;
      log.warn(
        "No ScriptEmitter is registered; falling back to naive slice(0, 4000). " +
          "Wire a real emitter via setScriptEmitter() to benefit from " +
          "QONTINUI_SCRIPTED_OUTPUT=1. goal=%s",
        goal,
      );
    }
    // Mirror the historical 4000-char truncation from command-handler.ts.
    return "output.slice(0, 4000)";
  }
}

/**
 * Shape of a successful response from the Rust `emit_extraction_script`
 * Tauri command (Phase A).  Mirrors `EmitResult` in
 * `src-tauri/src/step_output/script_emitter.rs`.
 */
interface TauriEmitResult {
  expression: string;
  modelId: string;
  tokensIn: number;
  tokensOut: number;
  source: "cache" | "llm";
  cacheTier: number | null;
}

/**
 * Shape of a structured error rejection from the Tauri command.
 * Mirrors `EmitExtractionScriptError` in
 * `src-tauri/src/commands/script_emitter.rs`.
 */
interface TauriEmitError {
  kind:
    | "cost_cap"
    | "token_budget"
    | "timeout"
    | "breaker_open"
    | "disabled"
    | "llm_error"
    | "invalid_response";
  message: string;
}

/**
 * Name of the env var that gates scripted extraction at runtime.  Both
 * the TS side (this emitter) and the Rust side check it — TS first so a
 * disabled run never crosses the Tauri boundary.
 */
const SCRIPTED_OUTPUT_ENV_FLAG = "QONTINUI_SCRIPTED_OUTPUT";

/**
 * Check the `QONTINUI_SCRIPTED_OUTPUT` env flag lazily.  Exported-style
 * readers were a bad fit for Vitest's env-mutation tests (see
 * `scripted-base.ts`).  Returns false in the browser / non-Node builds,
 * which intentionally blocks the production emitter from ever running
 * outside a Tauri context.
 */
function scriptedOutputEnvEnabled(): boolean {
  try {
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
 * Derive a stable "shape" summary of the output preview for use as the
 * emitter's `schemaHint`.  The Rust cache keys include the schema hint, so
 * keeping this deterministic maximizes cross-run hit rate.
 *
 * Strategy (kept intentionally tiny — ~20 LOC):
 *   - JSON object   → `{ kind: "json", keys: { [k]: typeof v } }`
 *   - JSON array    → `{ kind: "json_array", itemType: typeof items[0] }`
 *   - everything else (parse fails, primitive, etc.) → `{ kind: "text" }`
 */
export function summarizeSchemaHint(outputPreview: string): unknown {
  try {
    const parsed: unknown = JSON.parse(outputPreview);
    if (Array.isArray(parsed)) {
      return {
        kind: "json_array",
        itemType: parsed.length > 0 ? typeof parsed[0] : "unknown",
      };
    }
    if (parsed !== null && typeof parsed === "object") {
      const keys: Record<string, string> = {};
      for (const k of Object.keys(parsed as Record<string, unknown>)) {
        keys[k] = typeof (parsed as Record<string, unknown>)[k];
      }
      return { kind: "json", keys };
    }
  } catch {
    // fall through
  }
  return { kind: "text" };
}

/**
 * Production emitter — routes prompts through the runner's Rust
 * `ai_provider` module via the `emit_extraction_script` Tauri command.
 *
 * All cost ceilings (per-run call cap, token budget, circuit breaker,
 * settings kill switch) live on the Rust side; this shim only enforces
 * the env-var gate as a defense-in-depth no-round-trip short-circuit.
 *
 * On any Tauri rejection (including `cost_cap`, `disabled`, etc.), this
 * throws so {@link ScriptedOutputHandler.summarizeViaScript}'s existing
 * catch triggers the truncation fallback.  The thrown error carries the
 * rejection `kind` so the base handler's log line is informative.
 */
export class TauriScriptEmitter implements ScriptEmitter {
  async emit(goal: string, outputPreview: string, opts?: ScriptEmitterOptions): Promise<string> {
    if (!scriptedOutputEnvEnabled()) {
      // Short-circuit before round-tripping to Rust.  The base class's
      // catch will route to the truncation fallback.
      throw new Error(
        "TauriScriptEmitter: QONTINUI_SCRIPTED_OUTPUT is not set to 1 (env-gate closed)",
      );
    }

    const schemaHint = opts?.schemaHint ?? summarizeSchemaHint(outputPreview);
    const taskRunId = opts?.taskRunId ?? null;

    try {
      // The Rust command takes a single `args: EmitExtractionScriptArgs`
      // parameter, so Tauri expects the invoke payload wrapped under the
      // literal `args` key.  The inner object is camelCase (see the
      // `#[serde(rename_all = "camelCase")]` on the Rust struct).
      const result = await invoke<TauriEmitResult>("emit_extraction_script", {
        args: {
          goal,
          schemaHint,
          outputPreview,
          taskRunId,
        },
      });
      return result.expression;
    } catch (err) {
      // Tauri rejects with the structured `EmitExtractionScriptError` for
      // application errors (cost_cap, disabled, timeout, …).  For those,
      // `err` is the deserialized JSON object — not an Error instance.
      // Normalize to a thrown Error whose message carries the kind so the
      // base handler's fallback log is informative.
      const typed = err as Partial<TauriEmitError> | null | undefined;
      if (typed && typeof typed.kind === "string") {
        throw new Error(
          `TauriScriptEmitter rejected (${typed.kind}): ${typed.message ?? "<no message>"}`,
        );
      }
      // Transport / unexpected error — re-throw as-is (wrapped if not an Error).
      if (err instanceof Error) throw err;
      throw new Error(`TauriScriptEmitter failed: ${String(err)}`);
    }
  }
}

let currentEmitter: ScriptEmitter = new NoopScriptEmitter();

/**
 * Register a ScriptEmitter implementation.
 *
 * Typically called once during runner bootstrap, after the prompt client
 * is ready.  Passing `null` resets to the no-op default (useful in tests).
 */
export function setScriptEmitter(emitter: ScriptEmitter | null): void {
  currentEmitter = emitter ?? new NoopScriptEmitter();
}

/**
 * Get the currently-registered ScriptEmitter.
 */
export function getScriptEmitter(): ScriptEmitter {
  return currentEmitter;
}
