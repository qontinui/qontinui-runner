/**
 * Tracks file lock states per terminal tab.
 *
 * Uses three data sources:
 * 1. Tauri events (`file-lock-waiting`, `file-lock-acquired`,
 *    `file-lock-released`) for real-time waiting/holding/idle state.
 * 2. Polling `/file-locks/info` for which sessions currently hold locks
 *    (also used to backfill `waiterCount` per holder).
 *
 * The hook keys per-tab state on `tab.id` (a runner-local terminal id),
 * but the events arrive keyed by `holder_name` — which (per the
 * existing convention) matches `tab.title`. See
 * `findTabByHolderName` below.
 *
 * Phase 2 (`conflict-tooling-pauses-aligned`) extended the per-tab
 * `LockState` from a scalar (`"holding" | "waiting" | null`) to an
 * object carrying counterparty / file / since / waiter-count
 * fields. The {@link lockStateKind} helper exposes the kind for
 * consumers that only need the scalar.
 */

import { useState, useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import { resolvePort } from "@/lib/runner-api";
import type { TerminalTab } from "./useTerminalManager";

// ── Pure helpers (exported for tests) ────────────────────────────────────────

/**
 * Build the {@link LockState} patch produced by a `file-lock-waiting`
 * event. `holder_name` in the payload is the WAITER's name; the
 * blocker is in `blocked_by`.
 */
export function lockStateFromWaiting(payload: {
  file_path: string;
  blocked_by?: string | null;
}, nowMs: number): LockState {
  return {
    kind: "waiting",
    filePath: payload.file_path,
    counterpartyName: payload.blocked_by ?? undefined,
    sinceMs: nowMs,
  };
}

/**
 * Build the {@link LockState} patch produced by a `file-lock-acquired`
 * event (the prior waiter has now acquired the file).
 */
export function lockStateFromAcquired(payload: { file_path: string }, nowMs: number): LockState {
  return {
    kind: "holding",
    filePath: payload.file_path,
    sinceMs: nowMs,
  };
}

/**
 * Approximate per-holder waiter counts from the in-flight waiters map.
 * The runner's `/file-locks/info` endpoint doesn't carry waiter
 * counts directly today; we infer them from `file-lock-waiting`
 * events captured in {@link useFileLockTracking}.
 */
export function deriveWaiterCounts(
  waiters: Iterable<{ blockedBy: string }>,
): Map<string, number> {
  const counts = new Map<string, number>();
  for (const w of waiters) {
    counts.set(w.blockedBy, (counts.get(w.blockedBy) ?? 0) + 1);
  }
  return counts;
}

/**
 * Per-tab lock state.
 *
 * - `kind: "idle"` — tab holds no lock and is not waiting.
 * - `kind: "waiting"` — tab is blocked waiting for `filePath`. The
 *   `counterpartyName` is the *holder* (the OTHER party) per the
 *   `file-lock-waiting` payload's `blocked_by` field. `sinceMs` is
 *   captured at the moment we transition into waiting state.
 * - `kind: "holding"` — tab currently holds `filePath`.
 *   `counterpartyName` is left undefined for now (the runner doesn't
 *   emit holder-side events; we'd need a server roundtrip to resolve
 *   the most-significant waiter). `waiterCount` is filled from the
 *   `/file-locks/info` poll's per-holder waiter aggregate.
 */
export type LockState = {
  kind: "waiting" | "holding" | "idle";
  /** Path the lock is on (waiting + holding states). */
  filePath?: string;
  /** Friendly name of the OTHER party (holder if waiting, waiter if holding). */
  counterpartyName?: string;
  /** When this state began (epoch ms). */
  sinceMs?: number;
  /** Number of waiters (holding state only). */
  waiterCount?: number;
};

/**
 * Backwards-compat helper: returns the `kind` of a {@link LockState}
 * (or `null` for nullish input). Older consumers that only want the
 * scalar can `import { lockStateKind } from "./useFileLockTracking"`.
 */
export function lockStateKind(
  s: LockState | null | undefined,
): "waiting" | "holding" | null {
  if (!s) return null;
  if (s.kind === "waiting" || s.kind === "holding") return s.kind;
  return null;
}

/**
 * Legacy alias — some sibling files (e.g. TerminalTabBar) typed against
 * the prior scalar. Kept to soften the migration; new code should
 * import {@link LockState} directly.
 */
export type FileLockState = LockState;

/**
 * Incoming yield request payload as seen by the holder's tab.
 *
 * Emitted by the runner's POST /file-locks/yield-request handler
 * (`mcp/file_registry.rs::request_yield`) as a Tauri event named
 * `file-lock-yield-requested`. The event payload uses snake_case keys
 * (the handler builds the JSON via `serde_json::json!` directly, NOT a
 * struct with `#[serde(rename_all = "camelCase")]`), so the listener
 * here destructures `file_path`, `requester_task_run_id`,
 * `requester_name`, `holder_task_run_id`, and `requested_at`.
 *
 * The hook keys these per the HOLDER's tab id (resolved via
 * {@link findTabByTaskRunId} against `holder_task_run_id`) and dedups
 * by `(requesterTaskRunId, filePath)` — a duplicate replaces the prior
 * entry rather than appending so the banner shows a single up-to-date
 * "asked you to yield" stamp per (requester, file) pair.
 */
export type IncomingYieldRequest = {
  filePath: string;
  requesterName: string;
  requesterTaskRunId: string;
  requestedAtMs: number;
};

interface FileLockEvent {
  type: string;
  file_path: string;
  task_run_id: string;
  /**
   * For `file-lock-waiting`: the **waiter's** name (the tab that's
   * blocked). For `file-lock-acquired`: the name of the tab that just
   * acquired (the prior waiter). For `file-lock-released`: the name of
   * the tab that just released (the prior holder).
   *
   * Field-name fix (vet pass): older notes called this "holder_name"
   * everywhere, but the runner's payload sets it to the active party
   * (waiter or holder) of the event, not the static holder. The
   * `blocked_by` field below carries the actual blocker.
   */
  holder_name: string;
  /**
   * Only populated on `file-lock-waiting` — the friendly name of the
   * session currently *holding* the file (the blocker the waiting tab
   * is queued behind).
   */
  blocked_by?: string;
}

interface FileLockInfoEntry {
  file_path: string;
  holder_task_run_id: string;
  holder_name: string;
  acquired_at: number;
}

/**
 * `file-lock-yield-requested` Tauri event payload.
 *
 * Wire-format keys are snake_case (Rust handler emits via
 * `serde_json::json!` literal — see `mcp/file_registry.rs::request_yield`).
 */
interface FileLockYieldRequestedEvent {
  type: string;
  file_path: string;
  requester_task_run_id: string;
  requester_name: string;
  holder_task_run_id: string;
  requested_at: number;
}

/**
 * Return shape of {@link useFileLockTracking}.
 *
 * Phase 3 widened the hook from a bare `Record<string, LockState>` to
 * an object so callers can pull `pendingYieldRequests` (per-holder-tab
 * incoming yield request queues) without a second hook + a duplicate
 * `tabs` ref. Existing consumers destructure `{ lockStates }` for the
 * old slot.
 */
export interface FileLockTracking {
  lockStates: Record<string, LockState>;
  /**
   * Map of holder-tab id → list of incoming yield requests targeting
   * that tab. Keys correspond to `tab.id`; the request is routed to
   * the tab whose `tab.claudeSessionId === payload.holder_task_run_id`.
   *
   * Cleared on `file-lock-released` for the matching
   * `(holder_task_run_id, file_path)` pair — once the holder lets go,
   * any pending "please yield" request for that path is moot.
   *
   * Dedup invariant: within a tab's list, at most one entry exists per
   * `(requesterTaskRunId, filePath)` tuple. A subsequent
   * yield-requested event for the same pair replaces the prior entry
   * (refreshing `requestedAtMs`) rather than appending a duplicate.
   */
  pendingYieldRequests: Record<string, IncomingYieldRequest[]>;
}

export function useFileLockTracking(tabs: TerminalTab[]): FileLockTracking {
  const [lockStates, setLockStates] = useState<Record<string, LockState>>({});
  const [pendingYieldRequests, setPendingYieldRequests] = useState<
    Record<string, IncomingYieldRequest[]>
  >({});
  const tabsRef = useRef(tabs);
  useEffect(() => {
    tabsRef.current = tabs;
  }, [tabs]);

  /**
   * Per-waiter context keyed by waiter `holder_name` (= the tab title
   * of the WAITING side). Holds the blocker's name + path + sinceMs
   * captured when the waiting event arrived. Cleared when the same
   * waiter acquires (or when a release for this file arrives — see
   * the release handler).
   */
  const waitingHolders = useRef(
    new Map<
      string,
      { blockedBy: string; filePath: string; sinceMs: number }
    >(),
  );

  // Find tab ID by holder_name (matches tab title)
  const findTabByHolderName = (holderName: string): string | undefined => {
    for (const tab of tabsRef.current) {
      if (tab.title === holderName) return tab.id;
    }
    return undefined;
  };

  // Find tab ID by task_run_id (matches tab.claudeSessionId — the runner
  // sets task_run_id = claudeSessionId for live AI tabs).
  const findTabByTaskRunId = (taskRunId: string): string | undefined => {
    for (const tab of tabsRef.current) {
      if (tab.claudeSessionId === taskRunId) return tab.id;
    }
    return undefined;
  };

  // Listen for file-lock events
  useEffect(() => {
    let unlistenWaiting: (() => void) | null = null;
    let unlistenAcquired: (() => void) | null = null;
    let unlistenReleased: (() => void) | null = null;
    let unlistenYieldRequested: (() => void) | null = null;

    /**
     * Refresh `waiterCount` on every holding tab from the current
     * `waitingHolders.current` map. Used by all three event listeners
     * (waiting/acquired/released) so the per-holder waiter count tracks
     * waiter arrivals/departures live — without this, holding tabs only
     * pick up new waiter counts on the next 10s `/file-locks/info` poll
     * tick, which is too slow for the Phase 2 yield banner UX (the
     * banner needs to appear within one notify cycle of the waiter
     * arriving).
     *
     * Pure-functional: takes a `Record<tabId, LockState>` snapshot and
     * returns a new snapshot with refreshed `waiterCount` on holding
     * entries. Idle/waiting entries pass through unchanged. Bails out
     * early if no `waiterCount` actually changed so React's strict
     * equality check skips the re-render.
     */
    const refreshWaiterCounts = (
      prev: Record<string, LockState>,
    ): Record<string, LockState> => {
      const counts = deriveWaiterCounts(waitingHolders.current.values());
      let dirty = false;
      const next: Record<string, LockState> = { ...prev };
      for (const tab of tabsRef.current) {
        const state = prev[tab.id];
        if (!state || state.kind !== "holding") continue;
        const fresh = counts.get(tab.title) ?? 0;
        if ((state.waiterCount ?? 0) !== fresh) {
          next[tab.id] = { ...state, waiterCount: fresh };
          dirty = true;
        }
      }
      return dirty ? next : prev;
    };

    listen<FileLockEvent>("file-lock-waiting", (event) => {
      // `holder_name` here is the WAITER. `blocked_by` is the holder.
      const { holder_name, blocked_by, file_path } = event.payload;
      const nowMs = Date.now();
      waitingHolders.current.set(holder_name, {
        blockedBy: blocked_by ?? "another session",
        filePath: file_path,
        sinceMs: nowMs,
      });
      const tabId = findTabByHolderName(holder_name);
      setLockStates((prev) => {
        // Apply the waiter's own state transition (if any) first, then
        // refresh all holding-tab waiter counts using the new map.
        let base = prev;
        if (tabId) {
          const patch = lockStateFromWaiting(
            { file_path, blocked_by: blocked_by ?? null },
            nowMs,
          );
          base = { ...prev, [tabId]: patch };
        }
        return refreshWaiterCounts(base);
      });
    }).then((fn) => {
      unlistenWaiting = fn;
    });

    listen<FileLockEvent>("file-lock-acquired", (event) => {
      // The waiter just unblocked; transition to holding. The poll loop
      // also backfills `waiterCount` every 10s, but we refresh here too
      // so the prior holder's count drops immediately.
      const { holder_name, file_path } = event.payload;
      waitingHolders.current.delete(holder_name);
      const tabId = findTabByHolderName(holder_name);
      setLockStates((prev) => {
        let base = prev;
        if (tabId) {
          const patch = lockStateFromAcquired({ file_path }, Date.now());
          base = { ...prev, [tabId]: patch };
        }
        return refreshWaiterCounts(base);
      });
    }).then((fn) => {
      unlistenAcquired = fn;
    });

    // New event (added by the Rust agent in this plan iteration). Fires
    // when a holder releases a lock — payload mirrors the other
    // file-lock events.
    listen<FileLockEvent>("file-lock-released", (event) => {
      const { holder_name, task_run_id, file_path } = event.payload;
      // Try matching by holder_name first (tab title), fall back to
      // task_run_id (claudeSessionId) so SDK-style tabs still clear.
      const tabId =
        findTabByHolderName(holder_name) ?? findTabByTaskRunId(task_run_id);
      setLockStates((prev) => {
        let base = prev;
        if (tabId) {
          // Only flip to idle if we still believe the tab held this
          // path — avoids stomping a state set by a later event.
          const current = prev[tabId];
          if (current && current.kind === "holding") {
            base = { ...prev, [tabId]: { kind: "idle" } };
          }
        }
        // The released event may unblock a waiter elsewhere on the same
        // file; their `file-lock-acquired` will arrive next and prune
        // its `waitingHolders` entry, but for the in-between moment the
        // remaining holders' counts may have dropped. Refresh now so
        // the banner clears promptly even if the acquired event is
        // delayed.
        return refreshWaiterCounts(base);
      });

      // Phase 3 — clear any pending yield request targeting THIS holder
      // for THIS file path. Once the holder has released, the request is
      // moot; if a new holder picks the lock back up, the original
      // requester needs to re-issue (the request was directed at the
      // prior holder's task_run_id, not "whoever holds it").
      if (tabId) {
        setPendingYieldRequests((prev) => {
          const list = prev[tabId];
          if (!list || list.length === 0) return prev;
          const filtered = list.filter((r) => r.filePath !== file_path);
          if (filtered.length === list.length) return prev;
          return { ...prev, [tabId]: filtered };
        });
      }
    }).then((fn) => {
      unlistenReleased = fn;
    });

    // Phase 3 — wait-side yield-request listener. Routes the incoming
    // request to the HOLDER's tab (lookup by `holder_task_run_id` →
    // `tab.claudeSessionId` via {@link findTabByTaskRunId}). Dedups by
    // `(requesterTaskRunId, filePath)` — a duplicate replaces the prior
    // entry rather than appending so we don't compound the banner
    // counter on retries.
    listen<FileLockYieldRequestedEvent>("file-lock-yield-requested", (event) => {
      const {
        file_path,
        requester_task_run_id,
        requester_name,
        holder_task_run_id,
        requested_at,
      } = event.payload;
      const tabId = findTabByTaskRunId(holder_task_run_id);
      if (!tabId) return; // Holder tab isn't in this window; ignore.
      const incoming: IncomingYieldRequest = {
        filePath: file_path,
        requesterName: requester_name,
        requesterTaskRunId: requester_task_run_id,
        requestedAtMs: requested_at,
      };
      setPendingYieldRequests((prev) => {
        const existing = prev[tabId] ?? [];
        const dedupIdx = existing.findIndex(
          (r) =>
            r.requesterTaskRunId === requester_task_run_id &&
            r.filePath === file_path,
        );
        let next: IncomingYieldRequest[];
        if (dedupIdx >= 0) {
          // Replace in-place so request ordering is preserved (the
          // banner shows the oldest first; a re-issued request shouldn't
          // jump the queue past unrelated requests).
          next = existing.slice();
          next[dedupIdx] = incoming;
        } else {
          next = [...existing, incoming];
        }
        return { ...prev, [tabId]: next };
      });
    }).then((fn) => {
      unlistenYieldRequested = fn;
    });

    return () => {
      unlistenWaiting?.();
      unlistenAcquired?.();
      unlistenReleased?.();
      unlistenYieldRequested?.();
    };
  }, []);

  // Poll /file-locks/info to detect which tabs hold locks
  useEffect(() => {
    let active = true;

    const poll = async () => {
      try {
        // Resolve the port at each poll (not once at mount): on secondary
        // runners `useApiReady` populates `getApiPort()` asynchronously
        // via the `api-ready` Tauri event, so a hook that mounted
        // before the event fires would otherwise keep polling 9876
        // (the primary) forever. Re-reading here lets us catch up by
        // the second tick. `__QONTINUI_PORT__` still wins when set
        // (manual-test override).
        const port = resolvePort();
        const resp = await fetch(`http://127.0.0.1:${port}/file-locks/info`);
        if (!resp.ok || !active) return;
        const locks = (await resp.json()) as FileLockInfoEntry[];

        // Index holders by holder_name → list of held entries.
        const holderEntries = new Map<string, FileLockInfoEntry[]>();
        for (const lock of locks) {
          const list = holderEntries.get(lock.holder_name);
          if (list) list.push(lock);
          else holderEntries.set(lock.holder_name, [lock]);
        }

        // Approximate waiter counts per holder: for each currently-waiting
        // tab tracked in `waitingHolders`, increment the count of the
        // holder it's blocked on. The runner doesn't expose this
        // directly today (Phase 1 gap), but the tracked event data
        // gives a correct lower bound.
        const waiterCountByHolder = deriveWaiterCounts(waitingHolders.current.values());

        setLockStates((prev) => {
          const next: Record<string, LockState> = {};
          for (const tab of tabsRef.current) {
            const held = holderEntries.get(tab.title);
            if (held && held.length > 0) {
              // Tab holds locks — clear any stale waiting entry and
              // promote to holding. Pick the oldest-acquired path as
              // the "primary" file shown in tooltips.
              waitingHolders.current.delete(tab.title);
              const oldest = held.reduce((a, b) =>
                a.acquired_at <= b.acquired_at ? a : b,
              );
              const prevState = prev[tab.id];
              const sinceMs =
                prevState?.kind === "holding" && prevState.sinceMs !== undefined
                  ? prevState.sinceMs
                  : oldest.acquired_at;
              next[tab.id] = {
                kind: "holding",
                filePath: oldest.file_path,
                waiterCount: waiterCountByHolder.get(tab.title) ?? 0,
                sinceMs,
              };
              continue;
            }
            const waiting = waitingHolders.current.get(tab.title);
            if (waiting) {
              next[tab.id] = {
                kind: "waiting",
                filePath: waiting.filePath,
                counterpartyName: waiting.blockedBy,
                sinceMs: waiting.sinceMs,
              };
              continue;
            }
            // Preserve a recently-set holding/waiting state from events
            // even if the poll snapshot doesn't yet show it (events
            // arrive before the next poll cycle). Otherwise default
            // to idle.
            const prevState = prev[tab.id];
            if (
              prevState &&
              (prevState.kind === "waiting" || prevState.kind === "holding")
            ) {
              next[tab.id] = prevState;
            } else {
              next[tab.id] = { kind: "idle" };
            }
          }
          return next;
        });
      } catch {
        // Silently fail
      }
    };

    poll();
    const interval = setInterval(poll, 10_000);
    return () => {
      active = false;
      clearInterval(interval);
    };
  }, []);

  return { lockStates, pendingYieldRequests };
}
