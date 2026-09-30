import type { FleetSession, FleetSessionsResponse } from "./useFleetSessions";
import {
  FLEET_MAX_LIMIT,
  fleetCursorStalled,
  fleetErrorCode,
  fleetErrorMessage,
  normalizeFleetCursor,
  type FleetErrorCode,
} from "./fleetDiscovery";
import type { RemoteSessionEndOutcome, RemoteSessionEndResult } from "./remoteTabs";

/**
 * "End on remote" and "Close all finished" — the pure half of plan
 * `2026-09-30-close-remote-sessions-from-the-local-runner`, Phase 5.
 *
 * Everything the UI SAYS about an end lives here so it is testable without a
 * render (the runner's vitest config is `environment: "node"`). The rule the
 * whole module turns on: an outcome the target did not positively report as
 * `ended` is never rendered as ended — `unknown` (a timeout, a transport
 * failure, an unrecognised wire value) stays UNKNOWN, and the row it belongs
 * to stays on screen.
 */

// ---------------------------------------------------------------------------
// UI Bridge ids
// ---------------------------------------------------------------------------

/** The "End on remote session" button in a remote tab's hover cluster. */
export function remoteTabEndId(tabId: string): string {
  return `terminal.remote-end-session-${tabId}`;
}

/** The per-row "End" button in the Fleet picker. */
export function fleetSessionEndId(sessionId: string): string {
  return `terminal.fleet-session-end.${sessionId}`;
}

/** The inline line a Fleet row keeps after an end that did NOT end it. */
export function fleetSessionEndResultId(sessionId: string): string {
  return `terminal.fleet-session-end-result.${sessionId}`;
}

/** The single-session end dialog and its controls (one open at a time). */
export const REMOTE_END_DIALOG_ID = "terminal.remote-end-dialog";
export const REMOTE_END_CONFIRM_ID = "terminal.remote-end-confirm";
export const REMOTE_END_CANCEL_ID = "terminal.remote-end-cancel";
export const REMOTE_END_FORCE_ID = "terminal.remote-end-force";
export const REMOTE_END_FORCE_CONFIRM_ID = "terminal.remote-end-force-confirm";
export const REMOTE_END_RESULT_ID = "terminal.remote-end-result";

/** The bulk action and its dialog. */
export const FLEET_CLOSE_ALL_FINISHED_ID = "terminal.fleet-close-all-finished";
export const FLEET_CLOSE_ALL_FINISHED_DIALOG_ID = "terminal.fleet-close-all-finished-dialog";
export const FLEET_CLOSE_ALL_FINISHED_CONFIRM_ID = "terminal.fleet-close-all-finished-confirm";
export const FLEET_CLOSE_ALL_FINISHED_CANCEL_ID = "terminal.fleet-close-all-finished-cancel";
export const FLEET_CLOSE_ALL_FINISHED_RESULTS_ID = "terminal.fleet-close-all-finished-results";
/** Shown beside the bulk button when the finished read could not be made. */
export const FLEET_FINISHED_UNAVAILABLE_ID = "terminal.fleet-finished-unavailable";
/** The note naming rows hidden because THIS UI saw them end. */
export const FLEET_ENDED_HIDDEN_ID = "terminal.fleet-ended-hidden";

export function fleetCloseAllFinishedResultId(sessionId: string): string {
  return `terminal.fleet-close-all-finished-result.${sessionId}`;
}

// ---------------------------------------------------------------------------
// The finished guard
// ---------------------------------------------------------------------------

/** The value the fleet filter is asked for. */
export const FINISHED_SESSION_STATUS = "finished";

/**
 * True when a row's WORK-axis status means finished.
 *
 * `done` is the legacy spelling coord still stores on older rows, and its
 * `session_status=finished` filter matches both — so both count here.
 *
 * This is ALSO the belt-and-braces guard: an older coord ignores
 * `?session_status=` and serves an unfiltered page, and a bulk end must never
 * be offered for a session that is not finished because of it. Null (unset or
 * degraded) is not finished.
 */
export function isFinishedSessionStatus(v: string | null | undefined): boolean {
  const s = v?.trim().toLowerCase();
  return s === "finished" || s === "done";
}

// ---------------------------------------------------------------------------
// Per-row availability
// ---------------------------------------------------------------------------

export interface EndButtonState {
  disabled: boolean;
  reason: string | null;
}

/**
 * Whether a Fleet row can be ended from here, and why not when it cannot.
 * Same exclusions as attach: this machine's own sessions are local (end them
 * locally), a closed row has nothing to end, and a device id coord could not
 * vouch for cannot be addressed.
 */
export function endButtonState(
  s: Pick<FleetSession, "isCallerDevice" | "closedAt" | "deviceId" | "state">,
  deviceIdentityColumnsPresent: boolean | null | undefined,
): EndButtonState {
  if (s.isCallerDevice) {
    return { disabled: true, reason: "This session runs on this machine — end it locally." };
  }
  if (s.closedAt || s.state?.trim() === "closed") {
    return { disabled: true, reason: "coord already records this session as closed." };
  }
  if (!s.deviceId?.trim()) {
    return {
      disabled: true,
      reason: "coord did not report which device this session runs on — cannot address it.",
    };
  }
  if (deviceIdentityColumnsPresent === false) {
    return {
      disabled: true,
      reason:
        "coord could not read device identity for this read — the device id is unknown, not confirmed.",
    };
  }
  return { disabled: false, reason: null };
}

// ---------------------------------------------------------------------------
// Rendering an outcome
// ---------------------------------------------------------------------------

const KNOWN_OUTCOMES: readonly RemoteSessionEndOutcome[] = [
  "ended",
  "refused",
  "still_running",
  "unknown",
  "not_found",
];

export interface EndResultView {
  /** The outcome as rendered — an unrecognised wire value is `unknown`. */
  outcome: RemoteSessionEndOutcome;
  /** True ONLY for a positive `ended`. */
  isEnded: boolean;
  /** True when this UI may hide the row: it saw it end, or it was already gone. */
  hidesRow: boolean;
  /** True when a "Force end" (a second, explicit confirm) is offered. */
  offerForce: boolean;
  /** Short word for a result list. */
  label: string;
  /** One sentence saying what happened. */
  headline: string;
  /** The target's reason or the transport failure, when there is one. */
  detail: string | null;
  /** Colour class for the label. */
  toneClass: string;
}

function viaSentence(via: string | null): string {
  switch (via) {
    case "graceful":
      return "Claude exited through /exit and the terminal closed.";
    case "no_live_claude":
      return "No Claude was running there — the bare shell was closed.";
    case "force":
      return "The terminal was force-closed.";
    default:
      return "The target reported the session ended.";
  }
}

/**
 * Turn a `remote_session_end` result into what the UI shows.
 *
 * `wasForce` suppresses the Force-end offer on a result that already WAS a
 * force — there is no stronger step to offer, and offering the same one again
 * would loop.
 */
export function describeEndResult(r: RemoteSessionEndResult, wasForce = false): EndResultView {
  const outcome: RemoteSessionEndOutcome = KNOWN_OUTCOMES.includes(r.outcome)
    ? r.outcome
    : "unknown";
  const reason = r.reason?.trim() || null;
  switch (outcome) {
    case "ended":
      return {
        outcome,
        isEnded: true,
        hidesRow: true,
        offerForce: false,
        label: "ended",
        headline: viaSentence(r.via),
        detail: null,
        toneClass: "text-[#9ece6a]",
      };
    case "refused":
      return {
        outcome,
        isEnded: false,
        hidesRow: false,
        offerForce: !wasForce,
        label: "refused",
        headline: "The remote refused to end the session — it is still running.",
        detail: reason ?? "The target gave no reason.",
        toneClass: "text-[#e0af68]",
      };
    case "still_running":
      return {
        outcome,
        isEnded: false,
        hidesRow: false,
        offerForce: !wasForce,
        label: "still running",
        headline:
          "The session is still running — Claude did not exit before the deadline (wedged).",
        detail: reason,
        toneClass: "text-[#e0af68]",
      };
    case "not_found":
      return {
        outcome,
        isEnded: false,
        hidesRow: true,
        offerForce: false,
        label: "already gone",
        headline: "Already gone on the remote — it has no terminal for this session.",
        detail: reason,
        toneClass: "text-[#565f89]",
      };
    case "unknown":
    default:
      return {
        outcome: "unknown",
        isEnded: false,
        hidesRow: false,
        offerForce: false,
        label: "unknown",
        headline:
          "The outcome is unknown — no answer came back, so the session may still be running.",
        detail: reason,
        toneClass: "text-[#f7768e]",
      };
  }
}

/**
 * The result for an invoke that REJECTED instead of answering. The command
 * rejects only for a malformed id, but a rejection of any kind is not evidence
 * the session ended, so it is `unknown` carrying the error.
 */
export function endInvokeFailure(
  err: unknown,
  deviceId: string,
  sessionId: string,
): RemoteSessionEndResult {
  const raw = err instanceof Error ? err.message : typeof err === "string" ? err : String(err);
  return {
    outcome: "unknown",
    deviceId,
    sessionId,
    terminalId: null,
    via: null,
    reason: raw.trim() || "the end request failed with no reason given",
    grantSource: "none",
  };
}

// ---------------------------------------------------------------------------
// Close all finished
// ---------------------------------------------------------------------------

/**
 * The sessions "Close all finished" acts on: REMOTE (not this device's — those
 * are local), finished by the row's OWN status (the client-side guard), not
 * already closed, addressable, and not already seen ending here.
 */
export function closeAllFinishedCandidates(
  sessions: readonly FleetSession[],
  hiddenLocally: ReadonlySet<string>,
): FleetSession[] {
  return sessions.filter(
    (s) =>
      !s.isCallerDevice &&
      isFinishedSessionStatus(s.sessionStatus) &&
      !s.closedAt &&
      s.state?.trim() !== "closed" &&
      !!s.deviceId?.trim() &&
      !hiddenLocally.has(s.sessionId),
  );
}

/** What the finished read came to. */
export type FinishedFleetRead =
  | { kind: "loading" }
  | { kind: "ok"; sessions: FleetSession[]; capped: boolean; pages: number }
  | { kind: "unavailable"; message: string }
  | { kind: "error"; message: string; code: FleetErrorCode | null };

/** One page fetch, as the walk needs it — injected so the walk is testable. */
export type FinishedPageFetch = (args: {
  sessionStatus: string;
  limit: number;
  cursor: string | null;
}) => Promise<FleetSessionsResponse>;

/** Enough pages to be the whole set on any real tenant, bounded all the same. */
export const FINISHED_WALK_MAX_PAGES = 10;

/**
 * Walk `?session_status=finished` to the end (bounded), keeping only rows the
 * guard says are finished.
 *
 * - A `503 work_axis_columns_absent` / `work_axis_columns_unknown` (coord
 *   could not read, or could not probe, the column) is `unavailable`, never an
 *   empty set: an
 *   empty list would read as "no finished sessions".
 * - A page served with `workAxisColumnsPresent: false` is `unavailable` too —
 *   an older coord that ignored the filter and could not read the column would
 *   otherwise produce a guard-filtered zero with the same false meaning.
 * - Hitting the page bound, or a cursor coord hands back unchanged, is
 *   `capped` — the count is then a floor ("N+").
 */
export async function walkFinishedFleetSessions(
  fetchPage: FinishedPageFetch,
  opts: { limit?: number; maxPages?: number } = {},
): Promise<FinishedFleetRead> {
  const limit = opts.limit ?? FLEET_MAX_LIMIT;
  const maxPages = opts.maxPages ?? FINISHED_WALK_MAX_PAGES;
  const byId = new Map<string, FleetSession>();
  let cursor: string | null = null;
  let pages = 0;
  for (;;) {
    let page: FleetSessionsResponse;
    try {
      page = await fetchPage({ sessionStatus: FINISHED_SESSION_STATUS, limit, cursor });
    } catch (err) {
      const code = fleetErrorCode(err);
      if (code === "work_axis_columns_absent" || code === "work_axis_columns_unknown") {
        return { kind: "unavailable", message: fleetErrorMessage(code, err) };
      }
      return { kind: "error", message: fleetErrorMessage(code, err), code };
    }
    pages += 1;
    if (page.workAxisColumnsPresent === false) {
      return { kind: "unavailable", message: fleetErrorMessage("work_axis_columns_absent", null) };
    }
    for (const s of page.sessions ?? []) {
      if (isFinishedSessionStatus(s.sessionStatus)) byId.set(s.sessionId, s);
    }
    const next = normalizeFleetCursor(page.nextCursor);
    if (next === null) {
      return { kind: "ok", sessions: [...byId.values()], capped: false, pages };
    }
    if (fleetCursorStalled(cursor, next) || pages >= maxPages) {
      return { kind: "ok", sessions: [...byId.values()], capped: true, pages };
    }
    cursor = next;
  }
}

/**
 * The bulk button's label. Only an `ok` read yields a number; a failed or
 * unavailable read says so rather than showing 0.
 */
export function closeAllFinishedLabel(read: FinishedFleetRead, candidates: number): string {
  switch (read.kind) {
    case "loading":
      return "Close all finished (…)";
    case "unavailable":
      return "Close all finished (unavailable)";
    case "error":
      return "Close all finished (?)";
    case "ok":
      return `Close all finished (${candidates}${read.capped ? "+" : ""})`;
  }
}

/** The bulk button's tooltip — WHY it is disabled, or what it will do. */
export function closeAllFinishedTitle(read: FinishedFleetRead, candidates: number): string {
  switch (read.kind) {
    case "loading":
      return "Reading which remote sessions are finished…";
    case "unavailable":
    case "error":
      return read.message;
    case "ok":
      if (candidates === 0) return "No remote session is marked finished.";
      return (
        `End ${candidates}${read.capped ? "+" : ""} finished session(s) on other devices, ` +
        "each through a graceful /exit. Busy or wedged sessions are refused and left running." +
        (read.capped ? " More finished sessions exist than one read covers." : "")
      );
  }
}

// ---------------------------------------------------------------------------
// Bounded concurrency
// ---------------------------------------------------------------------------

/** Bulk end concurrency — bounded so per-session grant mints do not hit coord's rate limits. */
export const CLOSE_ALL_FINISHED_CONCURRENCY = 3;

/**
 * Run `fn` over `items` with at most `limit` in flight, calling `onResult` as
 * each settles. `fn` must not reject (wrap it) — a rejection is reported as
 * whatever the caller's wrapper makes of it, never dropped.
 */
export async function runBounded<T, R>(
  items: readonly T[],
  limit: number,
  fn: (item: T) => Promise<R>,
  onResult: (item: T, result: R) => void,
): Promise<void> {
  let next = 0;
  const worker = async () => {
    while (next < items.length) {
      const item = items[next++];
      onResult(item, await fn(item));
    }
  };
  const n = Math.max(1, Math.min(limit, items.length));
  await Promise.all(Array.from({ length: n }, worker));
}

/** The note beside a list with rows hidden because this UI saw them end. */
export function endedHiddenMessage(count: number): string | null {
  if (count <= 0) return null;
  return (
    `${count} session${count === 1 ? "" : "s"} ended from here ${count === 1 ? "is" : "are"} ` +
    "hidden. coord learns a device's close late, so the fleet list may still show " +
    `${count === 1 ? "it" : "them"} until it catches up.`
  );
}
