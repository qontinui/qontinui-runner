/**
 * Client-side reader for `GET /task-runs/running`.
 *
 * The endpoint returns a **scope envelope**, not a bare array:
 *
 * ```jsonc
 * {
 *   "scope": "workflow task-runs on API port 9876; NOT a session census — see /restart-readiness",
 *   "task_runs": [ ... ]
 * }
 * ```
 *
 * Plan `2026-08-29-no-single-answer-to-is-it-safe-to-restart-the-runner`
 * Phase 2/D4. The pre-2026-08-29 shape was a top-level `Vec<TaskRun>`; an
 * operator asking *"are there sessions on this box?"* reached this endpoint by
 * name, saw `[]`, and read it as *"the runner is idle"* while 23 live agent
 * sessions ran. The endpoint was **scoped, not widened** — it still reports only
 * workflow task-runs, it now says so — and the bare array was replaced outright
 * rather than dual-emitted, because leaving it reachable preserves exactly the
 * misreading the change closes.
 *
 * Every frontend consumer goes through {@link extractRunningTaskRuns} so the
 * shape lives in one place.
 */

/** The `{ scope, task_runs }` envelope `GET /task-runs/running` returns. */
export interface RunningTaskRunsEnvelope<T> {
  /** Self-describing coverage string — what this listing does and does NOT cover. */
  scope: string;
  /** The running workflow task-runs on this runner's API port. */
  task_runs: T[];
}

/**
 * Pull the rows out of a parsed `GET /task-runs/running` body.
 *
 * Returns `[]` for anything that is not the envelope — consumers iterate this
 * value during render, so an unexpected shape must degrade to "no runs" rather
 * than throw into the render tree. A bare array is deliberately **not**
 * accepted: that shape no longer exists, and silently tolerating it would let a
 * stale runner's response read as authoritative.
 */
export function extractRunningTaskRuns<T>(body: unknown): T[] {
  if (body && typeof body === "object") {
    const rows = (body as { task_runs?: unknown }).task_runs;
    if (Array.isArray(rows)) return rows as T[];
  }
  return [];
}

/**
 * The `scope` string from a parsed body, or `null` when absent.
 *
 * Exposed so a diagnostic surface can echo what the endpoint said it covers
 * instead of restating it from memory.
 */
export function runningTaskRunsScope(body: unknown): string | null {
  if (body && typeof body === "object") {
    const scope = (body as { scope?: unknown }).scope;
    if (typeof scope === "string" && scope.length > 0) return scope;
  }
  return null;
}
