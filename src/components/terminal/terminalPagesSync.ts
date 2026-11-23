import { emit, listen } from "@tauri-apps/api/event";

/**
 * Cross-window resync of the terminal-page roster (plan
 * 2026-11-23-runner-terminal-page-roster-resyncs-across-windows).
 *
 * Every window of one runner process (main and each pop-out) mounts
 * `useTerminalPages`, each with its own in-memory copy of the roster. Writes
 * already build on the PERSISTED roster, so they are correct across windows;
 * this module carries the signal that lets the OTHER windows refresh what they
 * DISPLAY after a write.
 *
 * The signal is a Tauri event, not the `window` `storage` event: Tauri v2
 * `emit` reaches every webview of the process (the sender included), while
 * `storage` delivery between sibling webviews is unverified per platform (the
 * same reason `pageBindings.ts` recomputes on Tauri events). Tauri events are
 * process-local, which matches the port-namespaced store's instance scoping.
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
 * before `listen` resolves unlistens as soon as it does.
 */
export function listenRosterSaved(onSaved: (payload: RosterSavedPayload) => void): () => void {
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
