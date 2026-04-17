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
 * think-in-code indirection layer.  A real emitter would prompt the
 * runner's LLM with something like:
 *
 *   "Given this output (first 2KB) and this goal, return a JS expression
 *    using `output` that extracts the minimum needed."
 *
 * At the time this module was written, the runner has no single "prompt
 * client" module to wire into — the closest matches
 * (`lib/ai-test-generation/prompt-builder.ts`,
 * `lib/hook-gen-prompt-builder.ts`) are prompt _builders_, not chat
 * clients.  Rather than invent a cross-cutting LLM client here, this file
 * defines the interface and ships a safe no-op default so the rest of the
 * pipeline is testable today.  Callers that have an LLM surface can
 * register a real emitter via {@link setScriptEmitter}.
 */

import { createLogger } from "../logger";

const log = createLogger("scripted-output-handler.emitter");

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
   * @returns              A single-expression JS string referencing
   *                       `output`.  Must be synchronous and
   *                       JSON-serializable.
   */
  emit(goal: string, outputPreview: string): Promise<string>;
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

  async emit(goal: string, _outputPreview: string): Promise<string> {
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
