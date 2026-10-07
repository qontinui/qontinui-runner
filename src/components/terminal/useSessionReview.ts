/**
 * The two reads a review surface holds — a session's file changes and its
 * review store — plus the review mutations, for one session id.
 *
 * `{sessionId}` is the tab's `claudeSessionId` for a PTY tab and the task-run
 * id for a Conductor worker; both routes key on it.
 *
 * Both reads are VISIBLE-GATED exactly as the worker cell's change list always
 * was (`shouldFetchChanges`): the file-changes route reads every touched file
 * off disk, so a surface the grid has hidden reads nothing and remembers that
 * a read is owed. Refresh triggers:
 *
 * - `commit-state-changed` (payload `task_run_id`, the same key) — emitted
 *   after every Edit/Write hook and by the PTY transcript tail; debounced so a
 *   burst of edits costs one read.
 * - `session-review-changed` (payload `sessionId`) — every review mutation and
 *   every transcript confirmation / session-end settlement.
 * - for a worker, a turn ending (`processing → ready|closed`), which catches
 *   edits a tool made without the hook seeing them.
 *
 * A failed read is UNKNOWN: it keeps the last good read as `previous` and
 * never becomes an empty list.
 */

import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import type { AiSessionState } from "@qontinui/shared-types";
import { describeThrown } from "@/lib/utils";
import {
  createReviewNote,
  fetchSessionReview,
  insertReview,
  markHunksRead,
  patchReviewNote,
  sendReview,
  type HunkRef,
  type NewNote,
  type NotePatch,
  type ReviewNoteRow,
  type SendOutcome,
  type SendRequest,
  type SessionReview,
} from "./sessionReviewApi";
import type { SessionReviewRead } from "./sessionReviewView";
import {
  fetchSessionFileChanges,
  type FileChangesRead,
  type SessionFileChangesResponse,
} from "./workerFileChanges";

/** Debounce for `commit-state-changed` bursts (one per Edit/Write hook). */
const CHANGES_REFRESH_DEBOUNCE_MS = 750;

/**
 * Whether a visible-gated read should be issued right now. Pure, exported for
 * the test.
 *
 * A surface the grid has hidden (behind a compact card, an off-screen zone, a
 * closed panel) reads NOTHING: with N sessions on a page an unconditional read
 * meant N whole-file sweeps per edit burst for a list no one could see. The
 * read is deferred to the moment the surface becomes visible, and a refresh
 * triggered while hidden is remembered as `stale` rather than performed.
 */
export function shouldFetchChanges(args: {
  visible: boolean;
  /** Session id the last read was issued for, or `null` if none ever was. */
  fetchedFor: string | null;
  taskRunId: string;
  /** A refresh was wanted while hidden. */
  stale: boolean;
}): boolean {
  if (!args.visible) return false;
  if (args.fetchedFor !== args.taskRunId) return true;
  return args.stale;
}

/**
 * Whether a refresh closure captured for `captured` may still run, given the
 * session id the hook is rendering for NOW. Pure, exported for the test.
 *
 * A refresh is captured by long-lived things — the debounce timer, a review
 * mutation's `finally` — that can outlive an id change (prev/next in the
 * maximized header, a zone reassignment). Run for the OLD id after the switch,
 * it would abort the new id's in-flight read, point `fetchedFor` back at the
 * old id and tag the state with it, so the new id's surface sat on `loading`
 * forever (nothing re-runs the fetch effect: its deps did not change).
 */
export function isCurrentSession(
  captured: string | null,
  current: string | null,
): captured is string {
  return captured !== null && captured === current;
}

/** The read state a visible-gated fetcher carries — shared by both reads. */
interface GatedRead<T> {
  read:
    | { status: "loading"; previous: T | null }
    | { status: "ok"; value: T }
    | {
        status: "error";
        error: string;
        atMs: number;
        previous: T | null;
      };
  refresh: () => void;
  refreshIfVisible: () => void;
}

/**
 * One visible-gated, abortable read of `fetcher(sessionId)`. `sessionId: null`
 * disables it entirely (a tab with no Claude session has nothing to read).
 */
function useGatedRead<T>(
  sessionId: string | null,
  visible: boolean,
  fetcher: (id: string, signal: AbortSignal) => Promise<T>,
  /** `describeThrown` fallback when a failed read carries no message of its own. */
  errorFallback: string,
): GatedRead<T> {
  // The read is tagged with the session it belongs to, so a different
  // session's data is never shown under this one's id — without an effect
  // that resets state on every id change.
  const [tagged, setTagged] = useState<{ for: string | null; read: GatedRead<T>["read"] }>({
    for: null,
    read: { status: "loading", previous: null },
  });
  // Referentially stable across renders that change nothing, so a consumer's
  // memo keyed on the read (the badge walks every hunk) actually holds.
  const read: GatedRead<T>["read"] = useMemo(
    () => (tagged.for === sessionId ? tagged.read : { status: "loading", previous: null }),
    [tagged, sessionId],
  );
  const latestRef = useRef<T | null>(null);
  const inFlightRef = useRef<AbortController | null>(null);
  const fetchedForRef = useRef<string | null>(null);
  const staleRef = useRef(false);
  const visibleRef = useRef(visible);
  useEffect(() => {
    visibleRef.current = visible;
  }, [visible]);
  // The id being rendered NOW. A layout effect, so it is current before any
  // passive effect (the fetch effect below) or a later timer can run.
  const sessionIdRef = useRef(sessionId);
  useLayoutEffect(() => {
    sessionIdRef.current = sessionId;
  }, [sessionId]);

  const refresh = useCallback(() => {
    // A stale closure for a previous id is a no-op (see `isCurrentSession`).
    if (!isCurrentSession(sessionId, sessionIdRef.current)) return;
    inFlightRef.current?.abort();
    const ctrl = new AbortController();
    inFlightRef.current = ctrl;
    // A new session starts from nothing: its first read has no `previous`.
    if (fetchedForRef.current !== sessionId) latestRef.current = null;
    fetchedForRef.current = sessionId;
    staleRef.current = false;
    const id = sessionId;
    const setRead = (next: GatedRead<T>["read"]) => setTagged({ for: id, read: next });
    setRead({ status: "loading", previous: latestRef.current });
    fetcher(sessionId, ctrl.signal)
      .then((value) => {
        if (ctrl.signal.aborted) return;
        latestRef.current = value;
        setRead({ status: "ok", value });
      })
      .catch((err: unknown) => {
        if (ctrl.signal.aborted) return;
        // A read is still OWED: without this, a first FAILED read left
        // `shouldFetchChanges` answering false forever (visible, same id, not
        // stale) and the surface sat on its error until something else fired.
        staleRef.current = true;
        setRead({
          status: "error",
          error: describeThrown(err, errorFallback),
          atMs: Date.now(),
          previous: latestRef.current,
        });
      });
  }, [sessionId, fetcher, errorFallback]);

  const refreshIfVisible = useCallback(() => {
    if (!visibleRef.current) {
      staleRef.current = true;
      return;
    }
    refresh();
  }, [refresh]);

  useEffect(() => {
    if (!sessionId) return;
    if (
      shouldFetchChanges({
        visible,
        fetchedFor: fetchedForRef.current,
        taskRunId: sessionId,
        stale: staleRef.current,
      })
    ) {
      refresh();
    }
  }, [visible, sessionId, refresh]);

  useEffect(() => () => inFlightRef.current?.abort(), []);

  return { read, refresh, refreshIfVisible };
}

const fetchChanges = (id: string, signal: AbortSignal): Promise<SessionFileChangesResponse> =>
  fetchSessionFileChanges(id, signal);
const fetchReview = (id: string, signal: AbortSignal): Promise<SessionReview> =>
  fetchSessionReview(id, signal);

function toChangesRead(r: GatedRead<SessionFileChangesResponse>["read"]): FileChangesRead {
  return r.status === "ok" ? { status: "ok", response: r.value } : r;
}

function toReviewRead(r: GatedRead<SessionReview>["read"]): SessionReviewRead {
  return r.status === "ok" ? { status: "ok", review: r.value } : r;
}

/**
 * Subscribe `handler` to a Tauri event for this component's lifetime, keyed on
 * the payload field that names the session. Payloads for other sessions are
 * ignored.
 */
function useSessionEvent(
  event: string,
  field: "task_run_id" | "sessionId",
  sessionId: string | null,
  handler: () => void,
): void {
  useEffect(() => {
    if (!sessionId) return;
    let unlisten: (() => void) | null = null;
    let disposed = false;
    listen<Record<string, unknown>>(event, (e) => {
      if (e.payload?.[field] !== sessionId) return;
      handler();
    })
      .then((fn) => {
        if (disposed) fn();
        else unlisten = fn;
      })
      .catch(() => {
        // No Tauri event bus (a browser preview): the explicit refresh still works.
      });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [event, field, sessionId, handler]);
}

/** The file-changes read alone — what the worker cell needs for its Changes pane. */
export function useSessionFileChanges(
  sessionId: string | null,
  options: { visible: boolean; sessionState?: AiSessionState },
): { read: FileChangesRead; refresh: () => void } {
  const { visible, sessionState } = options;
  const changes = useGatedRead(
    sessionId,
    visible,
    fetchChanges,
    "Failed to load session file changes",
  );
  const { refreshIfVisible } = changes;

  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const onCommitState = useCallback(() => {
    if (timerRef.current) clearTimeout(timerRef.current);
    timerRef.current = setTimeout(refreshIfVisible, CHANGES_REFRESH_DEBOUNCE_MS);
  }, [refreshIfVisible]);
  // A pending debounce belongs to the id it was armed for: cleared on every id
  // change (and on unmount), so session A's timer never fires after the
  // surface moved to B. `refresh` also refuses a stale id, as a second line.
  // Keyed on `sessionId` ONLY: keying on `refreshIfVisible` would silently
  // cancel a pending debounce whenever a caller passed an unstable fetcher or
  // fallback, and the cross-session case is already covered by the id key.
  useEffect(
    () => () => {
      if (timerRef.current) clearTimeout(timerRef.current);
      timerRef.current = null;
    },
    [sessionId],
  );
  useSessionEvent("commit-state-changed", "task_run_id", sessionId, onCommitState);

  // Turn-end refresh (workers only): catches edits made by a tool the hook did not see.
  const prevStateRef = useRef<AiSessionState | undefined>(sessionState);
  useEffect(() => {
    const prev = prevStateRef.current;
    prevStateRef.current = sessionState;
    if (prev === "processing" && (sessionState === "ready" || sessionState === "closed")) {
      refreshIfVisible();
    }
  }, [sessionState, refreshIfVisible]);

  const read = useMemo(() => toChangesRead(changes.read), [changes.read]);
  return { read, refresh: changes.refresh };
}

export interface SessionReviewHandle {
  /** `null` session id: the surface is disabled and both reads stay `loading`. */
  sessionId: string | null;
  changes: FileChangesRead;
  review: SessionReviewRead;
  /** Re-read both. */
  refresh: () => void;
  refreshChanges: () => void;
  markRead: (hunks: HunkRef[], read: boolean) => Promise<void>;
  addNote: (note: NewNote) => Promise<ReviewNoteRow>;
  patchNote: (noteId: string, patch: NotePatch) => Promise<ReviewNoteRow>;
  deliver: (mode: "send" | "insert", req: SendRequest) => Promise<SendOutcome>;
}

/**
 * Both reads plus the review mutations. Each mutation re-reads the review
 * store when it settles (success or failure) — the `session-review-changed`
 * event also arrives, but a surface must not depend on an event for the
 * outcome of its own call.
 */
export function useSessionReview(
  sessionId: string | null,
  options: { visible: boolean; sessionState?: AiSessionState },
): SessionReviewHandle {
  const { read: changes, refresh: refreshChanges } = useSessionFileChanges(sessionId, options);
  const review = useGatedRead(
    sessionId,
    options.visible,
    fetchReview,
    "Failed to load the session's review notes",
  );
  const { refresh: refreshReview, refreshIfVisible: refreshReviewIfVisible } = review;

  useSessionEvent("session-review-changed", "sessionId", sessionId, refreshReviewIfVisible);
  useSessionEvent("commit-state-changed", "task_run_id", sessionId, refreshReviewIfVisible);

  const refresh = useCallback(() => {
    refreshChanges();
    refreshReview();
  }, [refreshChanges, refreshReview]);

  const settle = useCallback(
    async <T>(op: (id: string) => Promise<T>): Promise<T> => {
      if (!sessionId) throw new Error("no session to review");
      try {
        return await op(sessionId);
      } finally {
        refreshReview();
      }
    },
    [sessionId, refreshReview],
  );

  const markRead = useCallback(
    (hunks: HunkRef[], read: boolean) => settle((id) => markHunksRead(id, hunks, read)),
    [settle],
  );
  const addNote = useCallback(
    (note: NewNote) => settle((id) => createReviewNote(id, note)),
    [settle],
  );
  const patchNote = useCallback(
    (noteId: string, patch: NotePatch) => settle((id) => patchReviewNote(id, noteId, patch)),
    [settle],
  );
  const deliver = useCallback(
    (mode: "send" | "insert", req: SendRequest) =>
      settle((id) => (mode === "send" ? sendReview(id, req) : insertReview(id, req))),
    [settle],
  );

  const reviewRead = useMemo(() => toReviewRead(review.read), [review.read]);
  return useMemo(
    () => ({
      sessionId,
      changes,
      review: reviewRead,
      refresh,
      refreshChanges,
      markRead,
      addNote,
      patchNote,
      deliver,
    }),
    [
      sessionId,
      changes,
      reviewRead,
      refresh,
      refreshChanges,
      markRead,
      addNote,
      patchNote,
      deliver,
    ],
  );
}
