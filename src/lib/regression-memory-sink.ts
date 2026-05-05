/**
 * PG-backed `MemorySink` adapter (Section 11, Phase A3).
 *
 * Adapts the pure `MemorySink` contract from `@qontinui/ui-bridge-auto` onto
 * the runner's PostgreSQL persistence layer via the Tauri command
 * `record_regression_diagnosis` (shipped in Phase A2). The library does not
 * own persistence — it just calls `sink.record(diagnosis)`. This adapter is
 * the single point where that call lands in PG.
 *
 * The library defines `MemorySink.record` synchronously (returns `void`), but
 * `invoke()` is async. We fire-and-forget at the call site and return a
 * thenable from the wrapper that callers can `await` when they care; the
 * library itself never awaits.
 */

import { invoke } from "@tauri-apps/api/core";
import type { MemorySink, SelfDiagnosis } from "@qontinui/ui-bridge-auto/diagnosis";

/**
 * A `MemorySink` whose `record` persists the diagnosis to the runner's PG
 * via the `record_regression_diagnosis` Tauri command. The corresponding
 * promise is exposed on `lastWrite` so callers (the regression executor)
 * can await persistence completion before returning.
 */
export interface PgMemorySink extends MemorySink {
  /**
   * Promise tracking the most recent `record` call's persistence write.
   * `null` until the first `record` invocation.
   */
  readonly lastWrite: Promise<string> | null;
}

/**
 * Build a {@link PgMemorySink} bound to a specific persisted run UUID.
 *
 * @param runDbId — UUID returned by `record_regression_run`. Diagnoses
 *   recorded through this sink are associated with that run.
 */
export function createPgMemorySink(runDbId: string): PgMemorySink {
  let lastWrite: Promise<string> | null = null;
  return {
    record(diagnosis: SelfDiagnosis): void {
      lastWrite = invoke<string>("record_regression_diagnosis", {
        runId: runDbId,
        diagnosisJson: diagnosis,
      });
    },
    get lastWrite(): Promise<string> | null {
      return lastWrite;
    },
  };
}
