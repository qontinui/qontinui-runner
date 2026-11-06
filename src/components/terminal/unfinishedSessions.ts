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
import { fetchLiveClaudeSessionIds } from "./liveClaudeSessions";
import type { PastSession } from "./usePastSessions";

/**
 * Close reasons recorded so that a restore does NOT pick the session up again
 * (Rust `NON_RESTORABLE_CLOSE_REASONS`, session_lifecycle_store.rs).
 */
const NON_RESTORABLE_CLOSE_REASONS: ReadonlySet<string> = new Set([
  "no-terminal",
  "never-started",
  "terminal-only-idle",
]);

const hasText = (v: string | undefined) => (v ?? "").trim() !== "";

/**
 * A closed claude session whose work was not marked finished AND that the resume
 * door can act on, so the list never offers a Resume the door must refuse:
 *
 * - `closedAt` marks a registry row, the only store the door reads. A snapshot
 *   row has none, and it reports `finished: false` for every session because the
 *   snapshot schema carries no finish mark, so it says nothing about the work.
 * - the door needs a transcript, an account config dir and a working dir.
 */
export function isUnfinished(s: PastSession): boolean {
  return (
    s.state === "closed" &&
    s.provider === "claude" &&
    s.finished !== true &&
    s.closedAt !== undefined &&
    s.restorable === true &&
    hasText(s.configDir) &&
    hasText(s.workingDir) &&
    !NON_RESTORABLE_CLOSE_REASONS.has(s.closeReason ?? "")
  );
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

export type ResumeOutcome = "resumed" | "failed" | "skipped" | "unknown";

export interface ResumeVerdict {
  id: string;
  outcome: ResumeOutcome;
  reason: string | null;
  /** Spelling shown to the operator, e.g. `skipped(transcript-missing)`. */
  verdict: string;
}

/**
 * Verdict for a call whose reply never arrived. The door keeps working after a
 * dropped request, so this cannot claim the resume failed.
 */
export function requestLost(ids: readonly string[], reason: string): ResumeVerdict[] {
  return ids.map((id) => ({
    id,
    outcome: "unknown",
    reason,
    verdict: `unknown(${reason})`,
  }));
}

/** Verdict for an HTTP failure of the whole call — never a guess per id. */
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
  liveSessionIds: () => Promise<ReadonlySet<string> | null> = fetchLiveClaudeSessionIds,
): Promise<ResumeVerdict[]> {
  // The door's own check only sees an id on a process command line, and an id
  // changed in-process (`/clear`, the `/resume` picker) is not there. Claude's
  // live registry reports the current id, so ask it first, as a roster resume
  // does. An unreadable registry leaves the decision to the door.
  const live = await liveSessionIds();
  const running = ids.filter((id) => live?.has(id));
  const toResume = ids.filter((id) => !running.includes(id));
  const skipped: ResumeVerdict[] = running.map((id) => ({
    id,
    outcome: "skipped",
    reason: "already-running",
    verdict: "skipped(already-running)",
  }));
  if (toResume.length === 0) return skipped;
  return [...skipped, ...(await postResume(toResume, fetchImpl, port))];
}

async function postResume(
  ids: readonly string[],
  fetchImpl: typeof fetch,
  port: number,
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
    return requestLost(ids, describeThrown(e, "request-lost"));
  }
}
