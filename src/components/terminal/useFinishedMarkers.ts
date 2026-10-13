/**
 * The work-axis finished marker (`/finish-session`; `session_status=finished`)
 * per Claude session id, for the Session Manager's `Finished` activity.
 *
 * A live `UnifiedSession` carries no finished field, so the marker is read from
 * the runner's session history (`terminal_session_list_history`), which
 * projects each lifecycle record's `finished` / `finished_at`. Plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`
 * Phase 4 moves this onto the batched session-info listing; until then this is
 * the source.
 *
 * **A missing marker is UNKNOWN, never "not finished".** {@link finishedFor}
 * answers `null` for a session the history does not describe, for a record
 * from a runner build that omits the field, and for every session while the
 * history is unread or failed.
 *
 * One ref-counted poller for every mounted page scope.
 */

import { useSyncExternalStore } from "react";
import { invoke } from "@tauri-apps/api/core";

import type { CommandResponse } from "./types";

const HISTORY_POLL_MS = 60_000;

export interface FinishedMarkers {
  read: "pending" | "ok" | "failed";
  /** `claudeSessionId → finished`, only for records that carry the field. */
  byId: ReadonlyMap<string, boolean>;
}

const PENDING: FinishedMarkers = { read: "pending", byId: new Map() };

let current: FinishedMarkers = PENDING;
const subscribers = new Set<() => void>();
let refCount = 0;
let timer: ReturnType<typeof setInterval> | null = null;
let inFlight = false;

/**
 * Interpret one `terminal_session_list_history` response. Exported for tests.
 * A record without a boolean `finished` contributes nothing — its marker stays
 * unknown.
 */
export function readFinishedMarkers(res: CommandResponse | null | undefined): FinishedMarkers {
  if (!res?.success) return { read: "failed", byId: new Map() };
  const data = res.data as { sessions?: unknown } | unknown[] | null | undefined;
  const rows = Array.isArray(data) ? data : (data as { sessions?: unknown })?.sessions;
  if (!Array.isArray(rows)) return { read: "failed", byId: new Map() };
  const byId = new Map<string, boolean>();
  for (const row of rows) {
    const r = row as { claudeSessionId?: unknown; finished?: unknown } | null;
    if (r && typeof r.claudeSessionId === "string" && typeof r.finished === "boolean") {
      // Several records may name one session (it was reopened); a finish on
      // any of them is the marker.
      byId.set(r.claudeSessionId, (byId.get(r.claudeSessionId) ?? false) || r.finished);
    }
  }
  return { read: "ok", byId };
}

/** `true` / `false` from a record carrying the marker; `null` = unknown. */
export function finishedFor(markers: FinishedMarkers, sessionId: string): boolean | null {
  if (markers.read !== "ok") return null;
  return markers.byId.get(sessionId) ?? null;
}

function sameMarkers(a: FinishedMarkers, b: FinishedMarkers): boolean {
  if (a.read !== b.read || a.byId.size !== b.byId.size) return false;
  for (const [id, f] of a.byId) if (b.byId.get(id) !== f) return false;
  return true;
}

/** Re-read the history now and publish the markers if they changed. */
export async function refreshFinishedMarkers(): Promise<void> {
  if (inFlight) return;
  inFlight = true;
  try {
    const next = readFinishedMarkers(
      await invoke<CommandResponse>("terminal_session_list_history", { opts: null }),
    );
    // A failed read after a good one keeps nothing: an old marker cannot be
    // trusted to still hold (`--undo` reverses it), so the axis goes UNKNOWN.
    if (!sameMarkers(current, next)) {
      current = next;
      for (const notify of subscribers) notify();
    }
  } catch {
    if (current.read !== "failed") {
      current = { read: "failed", byId: new Map() };
      for (const notify of subscribers) notify();
    }
  } finally {
    inFlight = false;
  }
}

function subscribe(onStoreChange: () => void): () => void {
  subscribers.add(onStoreChange);
  if (++refCount === 1) {
    void refreshFinishedMarkers();
    timer = setInterval(() => void refreshFinishedMarkers(), HISTORY_POLL_MS);
  }
  return () => {
    subscribers.delete(onStoreChange);
    if (--refCount === 0 && timer !== null) {
      clearInterval(timer);
      timer = null;
    }
  };
}

function getSnapshot(): FinishedMarkers {
  return current;
}

/** The latest finished markers. Identity is stable while unchanged. */
export function useFinishedMarkers(): FinishedMarkers {
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
}
