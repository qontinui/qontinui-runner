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

import { useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import { resolvePort } from "@/lib/runner-api";
import type { TerminalTab } from "./useTerminalManager";
import { getTerminalHotStore } from "./terminalHotStore";

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
  /**
   * Only populated for `kind === "waiting"`: milliseconds since the
   * HOLDER's last stdout activity, joined from `GET /sessions/idle-status`
   * (Phase 1 of the stuck-session heartbeat plan). The waiter banner
   * surfaces "(holder idle Xm)" when this exceeds the 60s display
   * threshold — see {@link WaitingLockBanner}. `undefined` when no
   * idle entry matched the holder, the idle fetch failed, or the
   * holder isn't an AI session (PTY holders aren't in the snapshot).
   */
  holderIdleMs?: number;
};

/**
 * Field-wise equality for two {@link LockState}s.
 *
 * The 10 s `/file-locks/info` poll rebuilds every tab's state object from
 * scratch, so a reference check alone can never bail out and the sweep
 * republished a brand-new map (and re-rendered every consumer) on every tick
 * even when nothing about the locks had moved. Callers reuse the previous
 * object when this returns true, which lets the hot store's identity check
 * short-circuit the publish entirely.
 */
export function lockStateEquals(
  a: LockState | undefined,
  b: LockState | undefined,
): boolean {
  if (a === b) return true;
  if (!a || !b) return false;
  return (
    a.kind === b.kind &&
    a.filePath === b.filePath &&
    a.counterpartyName === b.counterpartyName &&
    a.sinceMs === b.sinceMs &&
    a.waiterCount === b.waiterCount &&
    a.holderIdleMs === b.holderIdleMs
  );
}

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

/**
 * Incoming long-wait signal payload as seen by a WAITER tab.
 *
 * Emitted by the runner's POST /file-locks/signal-long-wait handler
 * (`mcp/file_registry.rs::signal_long_wait`) as a Tauri event named
 * `file-lock-long-wait-signaled`. The signal flows from the HOLDER to
 * every tab currently waiting on the same `file_path` — opposite
 * direction to {@link IncomingYieldRequest} (which flows waiter → holder).
 *
 * Wire-format keys are snake_case (handler emits via `serde_json::json!`
 * directly, not a `#[serde(rename_all = "camelCase")]` struct); the
 * frontend uses camelCase on this type.
 *
 * Dedup invariant on the consumer side: at most one entry per
 * `(holder_task_run_id, file_path)` tuple per waiter — a re-issued signal
 * replaces the prior entry rather than appending. Cleared on
 * `file-lock-released` or `file-lock-acquired` for the same `file_path`
 * (the waiter is no longer blocked, so the advisory is moot).
 */
export type IncomingLongWaitSignal = {
  filePath: string;
  holderName: string;
  holderTaskRunId: string;
  /** Optional estimated remaining time in ms; `undefined` when the holder didn't supply one. */
  estimatedRemainingMs?: number;
  signaledAtMs: number;
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
 * Per-session idle stats returned by `GET /sessions/idle-status`
 * (Phase 1 of the stuck-session heartbeat plan; backend handler at
 * `mcp/ai_session.rs::idle_status`).
 *
 * The poll loop below joins these entries against the waiting branch
 * of `LockState`: for each waiting tab, look up the holder's idle ms
 * by `holder_name` (the friendly name carried on the original
 * `file-lock-waiting` event's `blocked_by` field) and attach as
 * `holderIdleMs`. Keyed by `holder_name` rather than `task_run_id`
 * because `LockState.counterpartyName` is the friendly name, not a
 * task_run_id — the waiter doesn't know the holder's task_run_id
 * directly.
 */
interface SessionIdleEntry {
  task_run_id: string;
  holder_name: string;
  last_activity_ms: number;
  idle_ms: number;
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
 * `file-lock-long-wait-signaled` Tauri event payload (Phase 3 of the
 * stuck-session heartbeat plan).
 *
 * Wire-format keys are snake_case (Rust handler emits via
 * `serde_json::json!` literal — see
 * `mcp/file_registry.rs::signal_long_wait`). `estimated_remaining_ms`
 * is `null` over the wire when the holder didn't supply an estimate;
 * the listener normalises it to `undefined` on the camelCase
 * {@link IncomingLongWaitSignal}.
 */
interface FileLockLongWaitSignaledEvent {
  type: string;
  file_path: string;
  holder_task_run_id: string;
  holder_name: string;
  estimated_remaining_ms: number | null;
  signaled_at: number;
}

/**
 * The three maps {@link useFileLockTracking} publishes into the page's
 * terminal hot store. Kept as a named interface because it documents the
 * routing/dedup invariants of each map; consumers read the fields through
 * `useHotField(pageId, "lockStates" | "pendingYieldRequests" |
 * "pendingLongWaitSignals")` or `useTabHotSlice`.
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
  /**
   * Map of WAITER-tab id → list of incoming long-wait signals targeting
   * that waiter. Opposite key polarity to {@link pendingYieldRequests}:
   * signals flow from holder → every waiter, so the consumer is the
   * waiting tab. Keys correspond to `tab.id`; signals are routed to
   * every tab whose current `lockState.kind === "waiting"` and
   * `lockState.filePath === payload.file_path`.
   *
   * Cleared on `file-lock-released` AND `file-lock-acquired` for the
   * matching `file_path` — both events mean the waiter is no longer
   * blocked, so the "I'll be a while" advisory is moot.
   *
   * Dedup invariant: within a tab's list, at most one entry exists per
   * `(holderTaskRunId, filePath)` tuple. A re-issued signal for the
   * same pair replaces the prior entry (refreshing `signaledAtMs` /
   * `estimatedRemainingMs`) rather than appending a duplicate — the
   * waiter banner consumes only the most-recent entry.
   */
  pendingLongWaitSignals: Record<string, IncomingLongWaitSignal[]>;
}

/**
 * Track per-tab file-lock state for one terminal page.
 *
 * Phase 1 of the render-storm plan moved the three maps out of React state
 * and into the page's {@link getTerminalHotStore}: they used to be spread into
 * the single `TerminalSessionContext` value, so a 10 s poll tick re-rendered
 * the entire terminal page. Consumers now read them with `useHotField` /
 * `useTabHotSlice`, and the hook itself holds no reactive state at all — it is
 * a pure publisher, so it never re-renders its host.
 */
export function useFileLockTracking(pageId: string, tabs: TerminalTab[]): void {
  const store = getTerminalHotStore(pageId);
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
    // Functional publishers with `useState`-setter semantics: returning the
    // same object from the updater is the no-change bailout (the store drops
    // it without notifying anyone).
    const setLockStates = (
      updater: (prev: Record<string, LockState>) => Record<string, LockState>,
    ) => store.updateField("lockStates", updater);
    const setPendingYieldRequests = (
      updater: (
        prev: Record<string, IncomingYieldRequest[]>,
      ) => Record<string, IncomingYieldRequest[]>,
    ) => store.updateField("pendingYieldRequests", updater);
    const setPendingLongWaitSignals = (
      updater: (
        prev: Record<string, IncomingLongWaitSignal[]>,
      ) => Record<string, IncomingLongWaitSignal[]>,
    ) => store.updateField("pendingLongWaitSignals", updater);

    let unlistenWaiting: (() => void) | null = null;
    let unlistenAcquired: (() => void) | null = null;
    let unlistenReleased: (() => void) | null = null;
    let unlistenYieldRequested: (() => void) | null = null;
    let unlistenLongWaitSignaled: (() => void) | null = null;

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

      // Phase 3 (stuck-session heartbeat) — clear any long-wait signal
      // targeting THIS waiter for THIS file path. Once the waiter has
      // acquired, the holder's "I'll be a while" advisory is moot.
      if (tabId) {
        setPendingLongWaitSignals((prev) => {
          const list = prev[tabId];
          if (!list || list.length === 0) return prev;
          const filtered = list.filter((s) => s.filePath !== file_path);
          if (filtered.length === list.length) return prev;
          return { ...prev, [tabId]: filtered };
        });
      }
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

      // Phase 3 (stuck-session heartbeat) — clear long-wait signals for
      // THIS file path across every waiter tab. Unlike yield requests
      // (keyed by holder tab id), long-wait signals are keyed by waiter
      // tab id; ANY waiter still holding a stale signal for this file
      // should drop it once the lock is released. The next acquire by
      // a fresh holder produces a new signal if applicable.
      setPendingLongWaitSignals((prev) => {
        let dirty = false;
        const next: Record<string, IncomingLongWaitSignal[]> = {};
        for (const [waiterTabId, signals] of Object.entries(prev)) {
          const filtered = signals.filter((s) => s.filePath !== file_path);
          if (filtered.length !== signals.length) {
            dirty = true;
            next[waiterTabId] = filtered;
          } else {
            next[waiterTabId] = signals;
          }
        }
        return dirty ? next : prev;
      });
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

    // Phase 3 (stuck-session heartbeat) — hold-side long-wait signal
    // listener. The signal flows from HOLDER to every WAITER tab on the
    // same `file_path` (opposite polarity to the yield-request which is
    // waiter → holder). Iterate every tab whose current `lockState.kind
    // === "waiting"` and `lockState.filePath === payload.file_path`, and
    // for each such tab append the signal to `pendingLongWaitSignals`.
    // Dedup invariant: at most one entry per `(holderTaskRunId,
    // filePath)` tuple per waiter — a re-issued signal replaces the
    // prior entry rather than appending.
    listen<FileLockLongWaitSignaledEvent>("file-lock-long-wait-signaled", (event) => {
      const {
        file_path,
        holder_task_run_id,
        holder_name,
        estimated_remaining_ms,
        signaled_at,
      } = event.payload;
      const incoming: IncomingLongWaitSignal = {
        filePath: file_path,
        holderName: holder_name,
        holderTaskRunId: holder_task_run_id,
        // serde_json renders Rust's `None` as JSON `null`; the camelCase
        // type uses `undefined` per the optional-field convention.
        estimatedRemainingMs:
          estimated_remaining_ms === null || estimated_remaining_ms === undefined
            ? undefined
            : estimated_remaining_ms,
        signaledAtMs: signaled_at,
      };
      // Find every waiter tab currently blocked on this file. Read straight
      // from the hot store so the listener closure stays stable — it is
      // always the live map, with no ref-sync effect to keep in step.
      const currentLockStates = store.getField("lockStates");
      const waiterTabIds: string[] = [];
      for (const tab of tabsRef.current) {
        const s = currentLockStates[tab.id];
        if (
          s &&
          s.kind === "waiting" &&
          s.filePath === file_path
        ) {
          waiterTabIds.push(tab.id);
        }
      }
      if (waiterTabIds.length === 0) return; // No waiter for this file in this window.
      setPendingLongWaitSignals((prev) => {
        const next: Record<string, IncomingLongWaitSignal[]> = { ...prev };
        for (const tabId of waiterTabIds) {
          const existing = prev[tabId] ?? [];
          const dedupIdx = existing.findIndex(
            (s) =>
              s.holderTaskRunId === holder_task_run_id &&
              s.filePath === file_path,
          );
          let updated: IncomingLongWaitSignal[];
          if (dedupIdx >= 0) {
            // Replace in-place so queue position is preserved.
            updated = existing.slice();
            updated[dedupIdx] = incoming;
          } else {
            updated = [...existing, incoming];
          }
          next[tabId] = updated;
        }
        return next;
      });
    }).then((fn) => {
      unlistenLongWaitSignaled = fn;
    });

    return () => {
      unlistenWaiting?.();
      unlistenAcquired?.();
      unlistenReleased?.();
      unlistenYieldRequested?.();
      unlistenLongWaitSignaled?.();
    };
  }, [store]);

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
        // Phase 2 (stuck-session heartbeat) — piggyback the idle-status
        // fetch on the same 10s poll cycle, in parallel, with allSettled
        // so a transient failure on either endpoint doesn't tear down
        // the loop. We always honour the locks-info shape (the
        // pre-Phase-2 contract); idle-status is purely additive — when
        // missing/failed, `holderIdleMs` stays `undefined` on every
        // waiting LockState.
        const [locksRes, idleRes] = await Promise.allSettled([
          fetch(`http://127.0.0.1:${port}/file-locks/info`),
          fetch(`http://127.0.0.1:${port}/sessions/idle-status`),
        ]);
        if (!active) return;

        // Build the idle-by-holder-name index FIRST (it's optional so
        // any failure mode keeps the rest of the poll behaviour
        // identical to pre-Phase-2). Keyed by `holder_name` because
        // `LockState.counterpartyName` carries the friendly name — the
        // waiter doesn't know the holder's task_run_id directly.
        const idleByHolderName = new Map<string, number>();
        if (idleRes.status === "fulfilled" && idleRes.value.ok) {
          try {
            const entries = (await idleRes.value.json()) as SessionIdleEntry[];
            for (const e of entries) {
              idleByHolderName.set(e.holder_name, e.idle_ms);
            }
          } catch {
            // JSON parse failure — treat as empty.
          }
        }

        // If the locks fetch failed we have nothing to derive; bail out
        // (silently, same as the pre-Phase-2 catch).
        if (locksRes.status !== "fulfilled" || !locksRes.value.ok) return;
        const locks = (await locksRes.value.json()) as FileLockInfoEntry[];

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

        store.updateField("lockStates", (prev) => {
          const next: Record<string, LockState> = {};
          // Reuse the previous object for every tab whose state is
          // field-identical, so an unchanged sweep publishes nothing at all
          // (the store bails on reference equality across the whole map).
          const put = (tabId: string, state: LockState) => {
            const before = prev[tabId];
            next[tabId] = lockStateEquals(before, state) ? (before as LockState) : state;
          };
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
              put(tab.id, {
                kind: "holding",
                filePath: oldest.file_path,
                waiterCount: waiterCountByHolder.get(tab.title) ?? 0,
                sinceMs,
              });
              continue;
            }
            const waiting = waitingHolders.current.get(tab.title);
            if (waiting) {
              put(tab.id, {
                kind: "waiting",
                filePath: waiting.filePath,
                counterpartyName: waiting.blockedBy,
                sinceMs: waiting.sinceMs,
                // Phase 2 — attach the holder's idle_ms if the join hit.
                // We DON'T attach holderIdleMs on holding or idle
                // branches; the field is a waiter-only signal.
                holderIdleMs: idleByHolderName.get(waiting.blockedBy),
              });
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
              put(tab.id, { kind: "idle" });
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
  }, [store]);
}
