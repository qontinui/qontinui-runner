/**
 * Reading a `get_ai_output` reply.
 *
 * When the runner's Postgres is unavailable, `get_ai_output` answers
 * `success: false` with `data.db_output_unavailable: true` — the persisted
 * history could not be read — but `data.output_log` still carries the live
 * session's in-memory output. A caller that reads `output_log` only on
 * `success` hides that live output, and one that reads "no output" as "the
 * session is gone" forgets a session because the database was down (plan
 * 2026-10-19-runner-pg-unreachable-costs-every-caller-a-pool-timeout,
 * Phase 6: a down database never reads as empty data).
 */

/** The reply shape `get_ai_output` returns. */
export interface AiOutputResponse {
  success: boolean;
  message?: string;
  data?: Record<string, unknown>;
}

/** What a `get_ai_output` reply says. */
export interface AiOutput {
  /** The output log to show, or `null` when the reply carries none. */
  log: string | null;
  /**
   * The persisted history could not be read (a database outage). The `log`
   * is then the live in-memory output only, and an empty one means UNKNOWN,
   * never "no conversation".
   */
  historyUnavailable: boolean;
}

/** Interpret one `get_ai_output` reply. */
export function aiOutputOf(response: AiOutputResponse): AiOutput {
  const historyUnavailable = response.data?.db_output_unavailable === true;
  if (!response.success && !historyUnavailable) {
    return { log: null, historyUnavailable: false };
  }
  const raw = response.data?.output_log;
  return { log: typeof raw === "string" && raw.length > 0 ? raw : null, historyUnavailable };
}
