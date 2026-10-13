import { useSyncExternalStore } from "react";
import { invoke } from "@tauri-apps/api/core";

import {
  extractLiveSessionsStrict,
  liveRegistryBySessionId,
  registryNamesBySessionId,
  type LiveRegistryEntry,
} from "./liveClaudeSessions";
import type { CommandResponse } from "./types";

/**
 * How often to re-read Claude Code's live-session registry.
 *
 * The name changes when the operator runs `/rename`, and the self-reported
 * `status` (`idle` / `busy` / `waiting`) is a Session Manager activity source,
 * so the cadence sets how long a card can show a stale name or state.
 *
 * **This is not a free read.** `terminal_claude_session_list_live` parses ~20
 * sub-1KB JSON files (cheap) but first takes a full process-table snapshot to
 * drop entries whose PID died — on Windows that spawns `powershell.exe` running
 * `Get-CimInstance Win32_Process`, which dominates the cost. `runnerManaged` is
 * computed from that same snapshot, so it adds no second one. 30s matches the
 * sibling `transcript_find_external_processes` poll (`useExternalClaudeScan`),
 * which already pays for a snapshot of its own; do not raise the cadence
 * without first caching that snapshot on the Rust side.
 */
const REGISTRY_POLL_MS = 30_000;

/**
 * One read of the registry, as every consumer sees it.
 *
 * `read` is `pending` until the first answer and `failed` when the last read
 * errored or returned a payload whose shape was not understood. A failed read
 * KEEPS the last good names (a card showing its previous name beats every card
 * blanking out), but the Session Manager reads `failed` as "liveness UNKNOWN"
 * and places a tab-less row `Unknown` rather than `Not running`.
 */
export interface LiveClaudeSessionsSnapshot {
  read: "pending" | "ok" | "failed";
  /** `sessionId → {pid, status, runnerManaged}`, one entry per live session id. */
  bySessionId: ReadonlyMap<string, LiveRegistryEntry>;
  /** `sessionId → window name`, operator-named live sessions only. */
  names: ReadonlyMap<string, string>;
  /** Every live registry pid — the join key for the unidentified-process count. */
  pids: ReadonlySet<number>;
}

const EMPTY_SNAPSHOT: LiveClaudeSessionsSnapshot = {
  read: "pending",
  bySessionId: new Map(),
  names: new Map(),
  pids: new Set(),
};

// ── Module-scope store ───────────────────────────────────────────────────────
// ONE poller for the whole webview, ref-counted across mounts, in the shape
// `src/hooks/ui-bridge-events/singleton-listener.ts` established for this exact
// "N components, one shared subscription" problem.
//
// Per-hook-instance state would be wrong on both axes: it multiplies the
// process-table snapshot above by the number of mounted consumers (every page
// scope's `useSessionManager`, `PastSessionsView`, every titled tab), and their
// intervals are offset by mount time, so two surfaces could show DIFFERENT
// names or states for the same session for up to a full poll period.

let current: LiveClaudeSessionsSnapshot = EMPTY_SNAPSHOT;
const subscribers = new Set<() => void>();
let refCount = 0;
let timer: ReturnType<typeof setInterval> | null = null;
let inFlight = false;

function publish(next: LiveClaudeSessionsSnapshot): void {
  // Publish only on a real change so consumers memoized on the snapshot (the
  // row build in `useSessionManager`) do not rebuild every poll. Each part
  // keeps its identity when its content is unchanged, so a consumer that reads
  // only `names` stays stable across a status-only change.
  const merged: LiveClaudeSessionsSnapshot = {
    read: next.read,
    bySessionId: sameEntries(current.bySessionId, next.bySessionId)
      ? current.bySessionId
      : next.bySessionId,
    names: sameNames(current.names, next.names) ? current.names : next.names,
    pids: samePids(current.pids, next.pids) ? current.pids : next.pids,
  };
  if (
    merged.read === current.read &&
    merged.bySessionId === current.bySessionId &&
    merged.names === current.names &&
    merged.pids === current.pids
  ) {
    return;
  }
  current = merged;
  for (const notify of subscribers) notify();
}

/**
 * Re-read the registry now and publish the result if it changed.
 *
 * Exported so a manual "Refresh" affordance can converge the names and states
 * it shows alongside the list it reloads, instead of leaving them up to a poll
 * period stale.
 */
export async function refreshLiveClaudeSessions(): Promise<void> {
  // The underlying command spawns a process-table snapshot, so a call can
  // outlive the interval on a loaded box. Without this guard two overlapping
  // reads race and the older response can overwrite the newer one.
  if (inFlight) return;
  inFlight = true;
  try {
    const res = await invoke<CommandResponse>("terminal_claude_session_list_live");
    const sessions = res?.success ? extractLiveSessionsStrict(res.data) : null;
    const byId = sessions ? liveRegistryBySessionId(sessions) : null;
    if (!sessions || !byId) {
      publish({ ...current, read: "failed" });
      return;
    }
    publish({
      read: "ok",
      bySessionId: byId,
      names: registryNamesBySessionId(sessions),
      pids: new Set(sessions.map((s) => s.pid)),
    });
  } catch {
    // Command unavailable or registry unreadable: liveness is UNKNOWN.
    publish({ ...current, read: "failed" });
  } finally {
    inFlight = false;
  }
}

function subscribe(onStoreChange: () => void): () => void {
  subscribers.add(onStoreChange);
  if (++refCount === 1) {
    void refreshLiveClaudeSessions();
    timer = setInterval(() => void refreshLiveClaudeSessions(), REGISTRY_POLL_MS);
  }
  return () => {
    subscribers.delete(onStoreChange);
    if (--refCount === 0 && timer !== null) {
      clearInterval(timer);
      timer = null;
    }
  };
}

function getSnapshot(): LiveClaudeSessionsSnapshot {
  return current;
}

/**
 * Claude Code's live-session registry, as one snapshot: which sessions are
 * live, their pid, self-reported status and whether this runner owns the
 * process, plus the operator-chosen window names derived from the same read.
 *
 * This is the single frontend entry point for the registry. It reuses the
 * existing `terminal_claude_session_list_live` command — the runner has exactly
 * one registry reader (`session::claude_session_registry`) and must keep it
 * that way.
 *
 * Properties the callers rely on:
 *
 * - **A `names` miss is meaningful.** Rows whose `nameSource` is `"derived"`
 *   are filtered out by `registryNamesBySessionId`, so a miss means "nothing
 *   here beats what you already show".
 * - **Closed sessions are always a miss.** Claude Code deletes the backing file
 *   when a process exits and the reader re-checks liveness against the process
 *   table, so this snapshot never describes a dead session.
 * - **Each part's identity is stable while its contents are unchanged**, so it
 *   is safe to use directly as a `useMemo` / `useEffect` dependency.
 */
export function useLiveClaudeSessions(): LiveClaudeSessionsSnapshot {
  // Third argument = server snapshot; identical here because the store is
  // client-only and starts empty.
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
}

/**
 * Shallow map equality — the invariant the `useMemo` dependency rests on.
 *
 * Exported for tests: if this ever returns `false` for equal content, every
 * poll rebuilds the whole `UnifiedSession[]` and the regression is invisible
 * except as jank.
 */
export function sameNames(a: ReadonlyMap<string, string>, b: ReadonlyMap<string, string>): boolean {
  if (a === b) return true;
  if (a.size !== b.size) return false;
  for (const [id, name] of a) {
    if (b.get(id) !== name) return false;
  }
  return true;
}

/** Entry-wise equality of two `bySessionId` maps. Exported for tests. */
export function sameEntries(
  a: ReadonlyMap<string, LiveRegistryEntry>,
  b: ReadonlyMap<string, LiveRegistryEntry>,
): boolean {
  if (a === b) return true;
  if (a.size !== b.size) return false;
  for (const [id, e] of a) {
    const o = b.get(id);
    if (!o || o.pid !== e.pid || o.status !== e.status || o.runnerManaged !== e.runnerManaged) {
      return false;
    }
  }
  return true;
}

function samePids(a: ReadonlySet<number>, b: ReadonlySet<number>): boolean {
  if (a === b) return true;
  if (a.size !== b.size) return false;
  for (const pid of a) if (!b.has(pid)) return false;
  return true;
}

/** Test-only: reset the module store between tests. */
export function __resetLiveClaudeSessionsForTest(): void {
  current = EMPTY_SNAPSHOT;
  inFlight = false;
}
