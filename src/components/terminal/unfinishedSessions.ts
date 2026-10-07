/**
 * "Unfinished" filter over THIS device's session history (plan
 * `2026-10-06-closed-sessions-whose-work-is-unfinished-are-found-fleet-wide-and-resumed`,
 * Phase 7, runner half).
 *
 * Pure decisions only: which history rows are unfinished, how the list's
 * three-way read state is named (UNKNOWN is never "empty"), and the call to the
 * backend resume door `POST /control/sessions/resume` with its per-id verdicts.
 */

import { resolvePort } from "@/lib/runner-api";
import { describeThrown } from "@/lib/utils";
import type { PastSession } from "./usePastSessions";

/** A closed claude session whose work was not marked finished. */
export function isUnfinished(s: PastSession): boolean {
  return s.state === "closed" && s.provider === "claude" && s.finished !== true;
}

export function unfinishedSessions(sessions: readonly PastSession[]): PastSession[] {
  return sessions.filter(isUnfinished);
}

/** `unknown` = history unread or errored; `empty` = read fine, nothing unfinished. */
export type UnfinishedReadState = "unknown" | "empty" | "rows";

export function unfinishedReadState(input: {
  loaded: boolean;
  error: string | null;
  rows: readonly unknown[];
}): UnfinishedReadState {
  if (input.error || !input.loaded) return "unknown";
  return input.rows.length === 0 ? "empty" : "rows";
}

/** Rows from a runner predating the finish marker report no `finished` at all. */
export function finishMarkReported(s: PastSession): boolean {
  return s.finished !== undefined;
}

export type ResumeOutcome = "resumed" | "failed" | "skipped";

export interface ResumeVerdict {
  id: string;
  outcome: ResumeOutcome;
  reason: string | null;
  /** Spelling shown to the operator, e.g. `skipped(transcript-missing)`. */
  verdict: string;
}

/** Verdict for a transport/HTTP failure of the whole call — never a guess per id. */
export function callFailed(ids: readonly string[], reason: string): ResumeVerdict[] {
  return ids.map((id) => ({
    id,
    outcome: "failed",
    reason,
    verdict: `failed(${reason})`,
  }));
}

/** Parse the door's `ApiResponse<ResumeReport>` into one verdict per requested id. */
export function parseResumeResponse(ids: readonly string[], body: unknown): ResumeVerdict[] {
  const root = body as { success?: boolean; data?: unknown; error?: string } | null;
  const report = (root && typeof root === "object" && "data" in root ? root.data : body) as {
    results?: unknown;
  } | null;
  const results = report && Array.isArray(report.results) ? report.results : null;
  if (!results) return callFailed(ids, root?.error ?? "unreadable-response");
  const byId = new Map<string, ResumeVerdict>();
  for (const r of results as Array<Record<string, unknown>>) {
    const id = typeof r.id === "string" ? r.id : null;
    const outcome = r.outcome;
    if (!id || (outcome !== "resumed" && outcome !== "failed" && outcome !== "skipped")) continue;
    const reason = typeof r.reason === "string" ? r.reason : null;
    byId.set(id, {
      id,
      outcome,
      reason,
      verdict:
        typeof r.verdict === "string" ? r.verdict : reason ? `${outcome}(${reason})` : outcome,
    });
  }
  // An id the door did not answer for is a failure to hear, not a success.
  return ids.map((id) => byId.get(id) ?? callFailed([id], "no-verdict-returned")[0]);
}

export async function resumeUnfinished(
  ids: readonly string[],
  fetchImpl: typeof fetch = fetch,
  port: number = resolvePort(),
): Promise<ResumeVerdict[]> {
  try {
    const resp = await fetchImpl(`http://127.0.0.1:${port}/control/sessions/resume`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ ids }),
    });
    let body: unknown = null;
    try {
      body = await resp.json();
    } catch {
      /* non-JSON body handled below */
    }
    if (!resp.ok) {
      const err = (body as { error?: string } | null)?.error;
      return callFailed(ids, err ?? `http-${resp.status}`);
    }
    return parseResumeResponse(ids, body);
  } catch (e) {
    return callFailed(ids, describeThrown(e, "request-failed"));
  }
}
