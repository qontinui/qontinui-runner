import { useSyncExternalStore } from "react";
import { invoke } from "@tauri-apps/api/core";

import { extractLiveSessions, registryNamesBySessionId } from "./liveClaudeSessions";
import type { CommandResponse } from "./types";

/**
 * How often to re-read Claude Code's live-session registry.
 *
 * The name changes when the operator runs `/rename`, so the cadence sets how
 * long a card can show a stale name.
 *
 * **This is not a free read.** `terminal_claude_session_list_live` parses ~20
 * sub-1KB JSON files (cheap) but first takes a full process-table snapshot to
 * drop entries whose PID died — on Windows that spawns `powershell.exe` running
 * `Get-CimInstance Win32_Process`, which dominates the cost. 30s matches the
 * sibling `transcript_find_external_processes` poll in `useSessionManager`,
 * which already pays for a snapshot of its own; do not raise the cadence
 * without first caching that snapshot on the Rust side.
 */
const REGISTRY_POLL_MS = 30_000;

const EMPTY: ReadonlyMap<string, string> = new Map();

// ── Module-scope store ───────────────────────────────────────────────────────
// ONE poller for the whole webview, ref-counted across mounts, in the shape
// `src/hooks/ui-bridge-events/singleton-listener.ts` established for this exact
// "N components, one shared subscription" problem.
//
// Per-hook-instance state would be wrong on both axes: it multiplies the
// process-table snapshot above by the number of mounted consumers (today
// `useSessionManager` + `PastSessionsView`, which are visible simultaneously),
// and their intervals are offset by mount time, so two surfaces could show
// DIFFERENT names for the same session for up to a full poll period.

let current: ReadonlyMap<string, string> = EMPTY;
const subscribers = new Set<() => void>();
let refCount = 0;
let timer: ReturnType<typeof setInterval> | null = null;
let inFlight = false;

/**
 * Re-read the registry now and publish the result if it changed.
 *
 * Exported so a manual "Refresh" affordance can converge the names it shows
 * alongside the list it reloads, instead of leaving them up to a poll period
 * stale.
 *
 * Fails soft in both directions: a command error, an unsuccessful response, or
 * an exception all KEEP the last good map. A card showing its previous name is
 * a far better outcome than every card blanking out.
 */
export async function refreshLiveClaudeSessionNames(): Promise<void> {
  // The underlying command spawns a process-table snapshot, so a call can
  // outlive the interval on a loaded box. Without this guard two overlapping
  // reads race and the older response can overwrite the newer one.
  if (inFlight) return;
  inFlight = true;
  try {
    const res = await invoke<CommandResponse>("terminal_claude_session_list_live");
    if (!res.success) return;
    const next = registryNamesBySessionId(extractLiveSessions(res.data));
    // Publish only on a real change so consumers memoized on this map (the
    // `allSessions` build in `useSessionManager`) do not rebuild every poll.
    if (sameNames(current, next)) return;
    current = next;
    for (const notify of subscribers) notify();
  } catch {
    // Command unavailable (older binary) or registry unreadable.
  } finally {
    inFlight = false;
  }
}

function subscribe(onStoreChange: () => void): () => void {
  subscribers.add(onStoreChange);
  if (++refCount === 1) {
    void refreshLiveClaudeSessionNames();
    timer = setInterval(() => void refreshLiveClaudeSessionNames(), REGISTRY_POLL_MS);
  }
  return () => {
    subscribers.delete(onStoreChange);
    if (--refCount === 0 && timer !== null) {
      clearInterval(timer);
      timer = null;
    }
  };
}

function getSnapshot(): ReadonlyMap<string, string> {
  return current;
}

/**
 * `sessionId → the name shown in that session's Claude Code window`, for every
 * LIVE session that carries an operator-chosen name.
 *
 * This is the single frontend entry point for the window-name override that
 * `SessionCard`, `useSessionManager` and `PastSessionsView` apply. It reuses the
 * existing `terminal_claude_session_list_live` command — the runner has exactly
 * one registry reader (`session::claude_session_registry`) and must keep it that
 * way.
 *
 * Two properties the callers rely on:
 *
 * - **A miss is meaningful.** Rows whose `nameSource` is `"derived"` are
 *   filtered out by {@link registryNamesBySessionId}, so a miss means "nothing
 *   here beats what you already show" — callers keep their existing fallback
 *   chain untouched.
 * - **Closed sessions are always a miss.** Claude Code deletes the backing file
 *   when a process exits and the reader re-checks liveness against the process
 *   table, so this map never describes a dead session. That is what makes the
 *   live/closed asymmetry in `PastSessionsView` fall out for free rather than
 *   needing a second branch.
 *
 * The returned map's identity is stable while its contents are unchanged, so it
 * is safe to use directly as a `useMemo` / `useEffect` dependency.
 */
export function useLiveClaudeSessionNames(): ReadonlyMap<string, string> {
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
