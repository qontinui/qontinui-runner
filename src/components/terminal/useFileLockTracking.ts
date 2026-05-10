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

export function useFileLockTracking(tabs: TerminalTab[]): Record<string, LockState> {
  const [lockStates, setLockStates] = useState<Record<string, LockState>>({});
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
      if (tabId) {
        const patch = lockStateFromWaiting(
          { file_path, blocked_by: blocked_by ?? null },
          nowMs,
        );
        setLockStates((prev) => ({ ...prev, [tabId]: patch }));
      }
    }).then((fn) => {
      unlistenWaiting = fn;
    });

    listen<FileLockEvent>("file-lock-acquired", (event) => {
      // The waiter just unblocked; transition to holding. The poll loop
      // will backfill `waiterCount` shortly.
      const { holder_name, file_path } = event.payload;
      waitingHolders.current.delete(holder_name);
      const tabId = findTabByHolderName(holder_name);
      if (tabId) {
        const patch = lockStateFromAcquired({ file_path }, Date.now());
        setLockStates((prev) => ({ ...prev, [tabId]: patch }));
      }
    }).then((fn) => {
      unlistenAcquired = fn;
    });

    // New event (added by the Rust agent in this plan iteration). Fires
    // when a holder releases a lock — payload mirrors the other
    // file-lock events.
    listen<FileLockEvent>("file-lock-released", (event) => {
      const { holder_name, task_run_id } = event.payload;
      // Try matching by holder_name first (tab title), fall back to
      // task_run_id (claudeSessionId) so SDK-style tabs still clear.
      const tabId =
        findTabByHolderName(holder_name) ?? findTabByTaskRunId(task_run_id);
      if (tabId) {
        setLockStates((prev) => {
          // Only flip to idle if we still believe the tab held this
          // path — avoids stomping a state set by a later event.
          const current = prev[tabId];
          if (current && current.kind === "holding") {
            return { ...prev, [tabId]: { kind: "idle" } };
          }
          return prev;
        });
      }
    }).then((fn) => {
      unlistenReleased = fn;
    });

    return () => {
      unlistenWaiting?.();
      unlistenAcquired?.();
      unlistenReleased?.();
    };
  }, []);

  // Poll /file-locks/info to detect which tabs hold locks
  useEffect(() => {
    let active = true;

    const poll = async () => {
      try {
        const port =
          typeof window !== "undefined" &&
          (window as unknown as Record<string, unknown>).__QONTINUI_PORT__
            ? Number((window as unknown as Record<string, unknown>).__QONTINUI_PORT__)
            : 9876;
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

  return lockStates;
}
