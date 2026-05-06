/**
 * Productivity Stack — Session-action Tauri command wrappers (Phase 5).
 *
 * Thin async wrappers around the new Phase 5 commands that promote
 * `/summarize-session` and `/rewind-session` from personal slash
 * commands to in-product Rust features. Field shapes are camelCase by
 * virtue of `serde(rename_all = "camelCase")` on the Rust result structs
 * in `productivity::summarize` / `productivity::rewind`.
 */

import { invoke } from "@tauri-apps/api/core";

/**
 * Result returned by `summarize_session`. `placeholder=true` means no
 * LLM provider was configured and a single placeholder knowledge row
 * was inserted instead — the UI should surface the affordance to
 * configure a provider.
 */
export interface SummarizeResult {
  taskRunId: string;
  /** One of: approved | needs_fix | escalate. Defaults to `approved`
   *  when no review row exists yet for the session. */
  verdict: string;
  /** Total number of learnings persisted (sum of `byArea` values). */
  learningCount: number;
  /** Per-area counts. Stable keys = whatever the LLM emitted; the UI
   *  doesn't enumerate, just renders. */
  byArea: Record<string, number>;
  /** True when the placeholder fallback path was used. */
  placeholder: boolean;
}

/**
 * Result returned by `rewind_session`. `replaySessionId` is set when
 * `noReplay` was false and the spawn succeeded; null otherwise.
 */
export interface RewindResult {
  taskRunId: string;
  filesRestored: number;
  filesSkipped: number;
  /** Set when replay was enabled and the spawn succeeded. */
  replaySessionId: string | null;
  /** Whether the summarize step ran (false when replay was off, the
   *  LLM is disabled, or the summary failed — logged on the backend). */
  summarized: boolean;
  /** The summary's verdict, when summarize ran. */
  verdict: string | null;
}

/**
 * Summarize a finished AI session: extract learnings via the configured
 * LLM provider and persist them to `productivity_knowledge`. Falls
 * back to a placeholder row when no provider is configured.
 *
 * Throws on session not found or session still running.
 */
export async function summarizeSession(taskRunId: string): Promise<SummarizeResult> {
  return invoke<SummarizeResult>("summarize_session", { taskRunId });
}

/**
 * Rewind a failed AI session: restore pre-edit file snapshots, kill
 * the failed worker, and (by default) spawn a replacement with
 * failure-context prepended. Pass `noReplay=true` for revert + leave-
 * tab-empty (manual re-prompt).
 *
 * File-restore + kill are LLM-independent; the summarize step that
 * builds the failure-context block silently skips when no LLM is
 * configured.
 *
 * Throws on partial restore (workspace in a half-restored state must
 * be resolved manually before retrying), spawn failure, or kill
 * failure.
 */
export async function rewindSession(
  taskRunId: string,
  noReplay = false,
): Promise<RewindResult> {
  return invoke<RewindResult>("rewind_session", { taskRunId, noReplay });
}
