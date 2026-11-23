import { emit, listen } from "@tauri-apps/api/event";

/**
 * Cross-window resync of the terminal-page roster (plan
 * 2026-11-23-runner-terminal-page-roster-resyncs-across-windows).
 *
 * Every window of one runner process (main and each pop-out) mounts
 * `useTerminalPages`, each with its own in-memory copy of the roster. Writes
 * build on the PERSISTED roster as this window's store can see it, so they keep
 * every other window's change except one made inside the cross-webview storage
 * lag (last writer wins on what that store held). This module carries the
 * signal that lets the OTHER windows refresh what they DISPLAY after a write.
 *
 * The signal is a Tauri event, not the `window` `storage` event: Tauri v2
 * `emit` reaches every webview of the process (the sender included), while
 * `storage` delivery between sibling webviews is unverified per platform. Tauri
 * events are process-local, which matches the port-namespaced store's
 * instance scoping.
 */

/** Emitted after a window's roster save committed. */
export const TERMINAL_PAGES_SAVED_EVENT = "terminal-pages-roster-saved";

/**
 * `source`: the saving window's label, so it can ignore its own echo.
 * `digest`: {@link rosterDigest} of the roster it saved. The store write can
 * reach another webview AFTER the event does, so a receiver re-reads until the
 * stored roster's digest equals this one.
 */
export interface RosterSavedPayload {
  source: string;
  digest: string;
}

/** JSON with object keys sorted, so equal values stringify identically. */
function stableStringify(value: unknown): string {
  return JSON.stringify(value, (_key, v: unknown) => {
    if (v && typeof v === "object" && !Array.isArray(v)) {
      const sorted: Record<string, unknown> = {};
      for (const k of Object.keys(v as Record<string, unknown>).sort()) {
        sorted[k] = (v as Record<string, unknown>)[k];
      }
      return sorted;
    }
    return v;
  });
}

/** A stable digest of a roster: whole page objects, in order. */
export function rosterDigest(pages: readonly unknown[]): string {
  return stableStringify(pages);
}

/**
 * Whether two rosters hold the same pages in the same order, comparing whole
 * page objects (stable JSON per page), so a field added to the page config
 * later is compared without touching this function.
 */
export function rosterEqual(a: readonly unknown[], b: readonly unknown[]): boolean {
  if (a === b) return true;
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    if (a[i] !== b[i] && stableStringify(a[i]) !== stableStringify(b[i])) return false;
  }
  return true;
}

/**
 * Tell every window of this process that `source` saved the roster whose
 * digest is `digest`. Fire-and-forget: a failed broadcast must not break the
 * write it follows, so a rejection is logged, never thrown.
 */
export function broadcastRosterSaved(source: string, digest: string): void {
  const payload: RosterSavedPayload = { source, digest };
  try {
    emit(TERMINAL_PAGES_SAVED_EVENT, payload).catch((err: unknown) => {
      console.warn("[terminal-pages] roster-saved broadcast failed", err);
    });
  } catch (err) {
    console.warn("[terminal-pages] roster-saved broadcast failed", err);
  }
}

function isRosterSavedPayload(p: unknown): p is RosterSavedPayload {
  return (
    !!p &&
    typeof (p as RosterSavedPayload).source === "string" &&
    typeof (p as RosterSavedPayload).digest === "string"
  );
}

/**
 * Subscribe to roster-saved events from every window (this one included —
 * the caller filters its own echo). Returns the unsubscribe; one called
 * before `listen` resolves unlistens as soon as it does (and `onReady` is then
 * never called).
 *
 * `onReady` runs once the listener is registered (`listen` resolves
 * asynchronously), so the caller can catch up on a save another window made
 * before then, which no event will report.
 */
export function listenRosterSaved(
  onSaved: (payload: RosterSavedPayload) => void,
  onReady?: () => void,
): () => void {
  let disposed = false;
  let unlisten: (() => void) | null = null;
  listen<RosterSavedPayload>(TERMINAL_PAGES_SAVED_EVENT, (event) => {
    if (!disposed && isRosterSavedPayload(event.payload)) onSaved(event.payload);
  })
    .then((fn) => {
      if (disposed) {
        fn();
      } else {
        unlisten = fn;
        onReady?.();
      }
    })
    .catch((err: unknown) => {
      console.warn("[terminal-pages] roster-saved listener failed to register", err);
    });
  return () => {
    disposed = true;
    unlisten?.();
  };
}

// ── Resync telemetry (plan
// 2026-11-23-runner-terminal-page-popout-ui-bridge-and-resync-lag, D3) ──
//
// How each foreign roster event ended in THIS window, served through the
// `terminal-page` component action `get-roster-resync-stats` (see
// `rosterResyncAction.ts`) so the cross-webview store lag is a value the UI
// Bridge returns rather than a console line nobody collects. Module state is
// per webview: every window of the runner has its own JS context, so this ring
// holds only the receiving window's events.

/**
 * How one foreign roster event's read ended:
 * - `matched`: the store reached the announced digest within the retry budget;
 * - `matched-late`: it reached it only at the delayed final re-read;
 * - `exhausted`: it never did, even at the final re-read;
 * - `unreadable`: every read in the budget found an empty or unreadable store,
 *   so nothing was applied;
 * - `superseded`: a newer foreign event cancelled the read before it ended;
 * - `own-save`: it never matched because this window saved during the read
 *   and the store holds that save (an ordinary overwrite, not a lag).
 */
export type ResyncOutcome =
  | "matched"
  | "matched-late"
  | "exhausted"
  | "unreadable"
  | "superseded"
  | "own-save";

export interface ResyncEntry {
  /** The window label that announced the save. */
  source: string;
  /** The window label that received it and recorded this entry. */
  receiver: string;
  /** The announced {@link rosterDigest}. */
  digest: string;
  /** When the event arrived here (`Date.now()`). */
  startedAt: number;
  /** From arrival to the read that settled it (or the cancel, for `superseded`). */
  elapsedMs: number;
  /** Store reads made for this event (the final re-read included). */
  attempts: number;
  outcome: ResyncOutcome;
}

/** How many entries the ring keeps; the oldest is dropped past this. */
export const RESYNC_LOG_CAPACITY = 50;

let resyncLog: ResyncEntry[] = [];
let resyncLogSince: number | null = null;

/** Record how one foreign event ended. Keeps the last {@link RESYNC_LOG_CAPACITY}. */
export function recordResync(entry: ResyncEntry): void {
  // The handler is live from registration, before `listen()` resolves and
  // `onReady` marks `since`: an event that arrives in that gap proves the
  // listener was up by its arrival, so `since` is never null beside entries.
  markResyncLogSince(entry.startedAt);
  resyncLog.push(entry);
  if (resyncLog.length > RESYNC_LOG_CAPACITY) {
    resyncLog = resyncLog.slice(resyncLog.length - RESYNC_LOG_CAPACITY);
  }
}

/**
 * Mark that a roster listener is registered in this window, so events are
 * being recorded from `now` on. Only the first call counts: `since` is when
 * recording began, not the latest remount.
 */
export function markResyncLogSince(now: number = Date.now()): void {
  if (resyncLogSince === null) resyncLogSince = now;
}

/**
 * The ring, oldest first, beside `since`: when this window began recording.
 * `entries: []` with a `since` means "no foreign event since then", never a
 * zero lag. `since: null` means no roster listener has mounted in this window
 * yet, so nothing could have been recorded: UNKNOWN, not "no events".
 */
export function readResyncLog(): { since: number | null; entries: ResyncEntry[] } {
  return { since: resyncLogSince, entries: resyncLog.map((e) => ({ ...e })) };
}

/** Test-only: empty the ring and forget `since`. */
export function resetResyncLogForTest(): void {
  resyncLog = [];
  resyncLogSince = null;
}
