/**
 * File Activity heatmap — types, fetcher, and `useFileActivity` hook.
 *
 * Backend: `GET /file-activity/heatmap?window_secs=N&limit=M` (runner
 * MCP API, shipped as Phase 1 of the file-ownership-heatmap plan). The
 * endpoint composes:
 *   - `FileRegistryManager.info()` live snapshot
 *   - `PgDb::hot_files`  windowed aggregate
 *   - `PgDb::hot_sessions` windowed aggregate
 *
 * Phase 3 lifecycle:
 *   - 5 s `setInterval` while mounted; cleared on unmount.
 *   - Polling pauses while `document.visibilityState !== "visible"` to
 *     avoid background PG load. A `visibilitychange` listener resumes it.
 *   - On fetch failure or response slower than 2 s, the prior data is
 *     kept and `isStale` flips to `true` (UI renders a clock icon). The
 *     plan is explicit: do *not* replace stale data with a loading
 *     state — keep the last good snapshot, just age it.
 */

import { useCallback, useEffect, useMemo, useState } from "react";

/** Live entry from the in-process `FileRegistryManager.info()` snapshot. */
export interface FileRegistryInfoEntry {
  file_path: string;
  worktree_id?: string;
  holder_task_run_id: string;
  holder_name: string;
  /** ms since unix epoch. */
  registered_at: number;
}

/**
 * Live entry from the in-process `FileLockManager::info()` snapshot — the
 * shape returned by `GET /file-locks/info` (see Rust `FileLockInfo` at
 * `src-tauri/src/executor/file_registry.rs:642+`).
 *
 * The registry (`FileRegistryInfoEntry`) tracks ALL files a session has
 * touched; the lock manager tracks the strict subset that are also held
 * as exclusive locks. The Lock-Yield Protocol Phase 4 surface joins the
 * two by `(holder_task_run_id, file_path)` so the "Request yield" action
 * only renders for rows that actually have a lock to yield.
 */
export interface FileLockInfoEntry {
  file_path: string;
  holder_task_run_id: string;
  holder_name: string;
  /** seconds since unix epoch (Rust `u64` from `SystemTime::UNIX_EPOCH`). */
  acquired_at: number;
}

/** One row of the windowed hot-files aggregate (PG-side). */
export interface HotFileRow {
  file_path: string;
  /** distinct sessions that touched this file in the window. */
  distinct_sessions: number;
  /** ISO 8601 timestamp of the most recent touch in the window. */
  latest_recorded_at: string;
  latest_task_run_id: string;
}

/** One row of the windowed hot-sessions aggregate (PG-side). */
export interface HotSessionRow {
  task_run_id: string;
  /** distinct files this session touched in the window. */
  distinct_files: number;
  latest_recorded_at: string;
}

export interface HeatmapResponse {
  live: FileRegistryInfoEntry[];
  hot_files: HotFileRow[];
  hot_sessions: HotSessionRow[];
  /** echo of the window the server resolved (after clamping). */
  window_secs: number;
  /** echo of the per-list cap the server resolved (after clamping). */
  limit: number;
}

/** Dropdown options for the window selector. Plan calls for fixed
 *  values, not a slider — 4 options keeps the UI honest about what's
 *  meaningful. */
export const WINDOW_OPTIONS: readonly { label: string; secs: number }[] = [
  { label: "15 min", secs: 900 },
  { label: "1 hr", secs: 3600 },
  { label: "6 hr", secs: 21600 },
  { label: "24 hr", secs: 86400 },
];

export const DEFAULT_WINDOW_SECS = 3600;
export const WINDOW_STORAGE_KEY = "fileActivity.windowSecs";

/** Default poll interval matches the plan's Phase 3 spec. */
export const DEFAULT_POLL_INTERVAL_MS = 5_000;

/** Beyond this latency a fetch is considered slow and triggers the
 *  stale indicator even if it eventually returns. */
const SLOW_FETCH_THRESHOLD_MS = 2_000;

/** Load the user's stored window choice. Falls back to 1 hour. Defensive
 *  against `localStorage` being unavailable (Tauri sandboxes, SSR). */
export function loadStoredWindowSecs(): number {
  if (typeof window === "undefined") return DEFAULT_WINDOW_SECS;
  try {
    const raw = window.localStorage.getItem(WINDOW_STORAGE_KEY);
    if (!raw) return DEFAULT_WINDOW_SECS;
    const n = Number.parseInt(raw, 10);
    if (!Number.isFinite(n)) return DEFAULT_WINDOW_SECS;
    if (!WINDOW_OPTIONS.some((o) => o.secs === n)) return DEFAULT_WINDOW_SECS;
    return n;
  } catch {
    return DEFAULT_WINDOW_SECS;
  }
}

export function storeWindowSecs(secs: number): void {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(WINDOW_STORAGE_KEY, String(secs));
  } catch {
    /* localStorage unavailable; no-op. */
  }
}

/** Resolve the runner MCP API base URL the same way `useFileLockTracking`
 *  does. The runner injects `__QONTINUI_PORT__` on its window object at
 *  boot; we fall back to the canonical 9876 port when missing. */
function apiBaseUrl(): string {
  if (typeof window === "undefined") return "http://127.0.0.1:9876";
  const port = (window as unknown as Record<string, unknown>).__QONTINUI_PORT__;
  const parsed = typeof port === "number" ? port : Number.parseInt(String(port ?? ""), 10);
  return `http://127.0.0.1:${Number.isFinite(parsed) && parsed > 0 ? parsed : 9876}`;
}

export async function fetchHeatmap(
  windowSecs: number,
  limit = 25,
  signal?: AbortSignal,
): Promise<HeatmapResponse> {
  const url = `${apiBaseUrl()}/file-activity/heatmap?window_secs=${windowSecs}&limit=${limit}`;
  const resp = await fetch(url, { signal });
  if (!resp.ok) {
    throw new Error(`heatmap fetch failed: HTTP ${resp.status}`);
  }
  return (await resp.json()) as HeatmapResponse;
}

/**
 * Fetch the live snapshot of currently-held exclusive file locks.
 *
 * Used by the Lock-Yield Protocol Phase 4 surface to decide which
 * registry rows in `FileActivityPanel`'s Live snapshot section should
 * show the "Request yield" action: only rows whose
 * `(holder_task_run_id, file_path)` pair is present in this response
 * are actually held as locks (registry membership alone does not imply
 * lock ownership).
 *
 * Mirror of {@link fetchHeatmap}'s shape — error handling, abort signal,
 * port resolution. Errors are surfaced to the caller (the hook degrades
 * to "show no yield buttons" rather than failing the whole panel).
 */
export async function fetchLockInfo(
  signal?: AbortSignal,
): Promise<FileLockInfoEntry[]> {
  const url = `${apiBaseUrl()}/file-locks/info`;
  const resp = await fetch(url, { signal });
  if (!resp.ok) {
    throw new Error(`lock info fetch failed: HTTP ${resp.status}`);
  }
  return (await resp.json()) as FileLockInfoEntry[];
}

export interface UseFileActivityResult {
  data: HeatmapResponse | null;
  /** Companion snapshot of currently-held exclusive locks, fetched on
   *  the same poll cadence as `data`. The Lock-Yield Protocol Phase 4
   *  UI joins this with `data.live` by `(holder_task_run_id, file_path)`
   *  to decide which rows render the "Request yield" action.
   *
   *  Null until the first successful fetch; falls back to the previous
   *  value on transient errors (same pattern as `data`). The lock-info
   *  fetcher failing does NOT flip the heatmap `isStale` flag — lock
   *  state is auxiliary, and we'd rather show stale locks (or no yield
   *  buttons) than spuriously age the whole panel. */
  lockInfo: FileLockInfoEntry[] | null;
  error: string | null;
  /** True when the latest fetch failed OR took longer than the slow
   *  threshold. Cleared by the next successful fast fetch. */
  isStale: boolean;
  /** Force-refresh now. Does not reset `isStale` optimistically — that
   *  flips back when the refresh completes successfully. */
  refresh: () => void;
}

interface UseFileActivityOptions {
  windowSecs: number;
  /** When `false`, polling is suspended (mount-time + visibility gate).
   *  Defaults to `true`. */
  enabled?: boolean;
  /** Override the 5 s poll cadence; useful for tests. */
  pollIntervalMs?: number;
}

/**
 * Encapsulates the windowed fetch, the visibility-gated poll lifecycle,
 * and the stale flag. The hook deliberately keeps the previous payload
 * across failures (per plan's Phase 3 rule "don't replace stale data
 * with loading").
 */
export function useFileActivity({
  windowSecs,
  enabled = true,
  pollIntervalMs = DEFAULT_POLL_INTERVAL_MS,
}: UseFileActivityOptions): UseFileActivityResult {
  const [data, setData] = useState<HeatmapResponse | null>(null);
  const [lockInfo, setLockInfo] = useState<FileLockInfoEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [isStale, setIsStale] = useState<boolean>(false);
  /** Bumped by the user-facing `refresh()` to short-circuit the timer. */
  const [tick, setTick] = useState<number>(0);

  // Track document visibility so polling pauses in background tabs.
  const [visible, setVisible] = useState<boolean>(
    typeof document === "undefined" || document.visibilityState === "visible",
  );

  useEffect(() => {
    if (typeof document === "undefined") return;
    const onVisChange = () => setVisible(document.visibilityState === "visible");
    document.addEventListener("visibilitychange", onVisChange);
    return () => document.removeEventListener("visibilitychange", onVisChange);
  }, []);

  // Snapshot the most recent successful resolution time. Phase 3 spec:
  // stale flag flips on slow fetches, not just hard failures.
  //
  // Lifecycle: when any dep changes (or the component unmounts), React
  // runs the cleanup below — `cancelled` blocks late `setState` calls
  // and `ac.abort()` short-circuits the in-flight fetch. We don't need
  // a ref-based abort because every controller is captured by its own
  // effect-run closure.
  useEffect(() => {
    if (!enabled || !visible) return;

    let cancelled = false;
    const ac = new AbortController();

    const poll = async () => {
      const start = performance.now();
      // Fetch heatmap + lock-info in parallel on the same 5s cadence so
      // the Phase 4 yield-action join doesn't lag behind the registry
      // rows it decorates. Errors are handled independently — a flaky
      // lock-info fetch must not collapse the whole panel.
      const [heatmapResult, lockResult] = await Promise.allSettled([
        fetchHeatmap(windowSecs, 25, ac.signal),
        fetchLockInfo(ac.signal),
      ]);
      if (cancelled || ac.signal.aborted) return;
      const elapsed = performance.now() - start;

      if (heatmapResult.status === "fulfilled") {
        setData(heatmapResult.value);
        setError(null);
        setIsStale(elapsed > SLOW_FETCH_THRESHOLD_MS);
      } else {
        // Network errors and abort errors look the same shape in fetch;
        // we already skipped abort above. Treat anything else as stale.
        const msg =
          heatmapResult.reason instanceof Error
            ? heatmapResult.reason.message
            : String(heatmapResult.reason);
        setError(msg);
        setIsStale(true);
        // NB: keep `data` populated — see plan §Phase 3.
      }

      if (lockResult.status === "fulfilled") {
        setLockInfo(lockResult.value);
      }
      // On lock-info failure: keep the prior `lockInfo` value (yield
      // buttons may briefly point at locks that have since released —
      // benign; the POST is idempotent and the next poll heals it).
    };

    poll();
    const id = window.setInterval(poll, pollIntervalMs);
    return () => {
      cancelled = true;
      window.clearInterval(id);
      ac.abort();
    };
  }, [windowSecs, enabled, visible, pollIntervalMs, tick]);

  const refresh = useCallback(() => {
    setTick((t) => t + 1);
  }, []);

  return useMemo(
    () => ({ data, lockInfo, error, isStale, refresh }),
    [data, lockInfo, error, isStale, refresh],
  );
}
