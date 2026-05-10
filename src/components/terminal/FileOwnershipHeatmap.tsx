/**
 * File-Ownership Heatmap (Phase 3 of
 * `conflict-tooling-pauses-aligned-plan.md`).
 *
 * Surfaces a panel-level view of "who's editing what across all active AI
 * sessions" by querying `coord.session_touched_files` over a sliding
 * window. Rows = unique file paths, sorted contention-first then
 * recency-descending. Each row is tinted by max recency (10-minute
 * decay-ish via `Math.exp`) and gets a red left border when the file is
 * touched by more than one distinct `task_run_id` in the window.
 *
 * Refresh model:
 *   1. Initial fetch on mount.
 *   2. 30 s background poll while the panel is mounted.
 *   3. Subscribe to `commit-state-changed` Tauri events — any fire
 *      triggers a refetch (the event signals a session just touched a
 *      file via the dispatcher / transcript watcher / commit pathway,
 *      so the heatmap data may have changed).
 *
 * v1 is a sortable table, NOT a real heatmap. The plan explicitly says
 * "Don't gold-plate v1." v2 (deferred): proper heatmap visualization
 * (D3 / react-table-heatmap) once we know the panel is being used.
 *
 * Pure helpers (`computeHeatmapRows`, `decayRecency`) are exported so
 * they can be unit-tested without a DOM (vitest runs in `node`
 * environment per `vitest.config.ts`; see `CommitTrafficLight.test.tsx`
 * for the precedent).
 */

import { useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { createLogger } from "@/lib/logger";

const logger = createLogger("FileOwnershipHeatmap");

/** Mirrors `commands::ai_session::TouchedFileRow` in Rust. */
export interface TouchedFileRow {
  file_path: string;
  task_run_id: string;
  /** Epoch milliseconds. */
  recorded_at_ms: number;
}

/** One row of the heatmap table — aggregated from ≥1 `TouchedFileRow`s
 *  for the same `file_path`. */
export interface HeatmapRow {
  filePath: string;
  /** Distinct task_run_ids that touched this file in the window, sorted
   *  most-recent first. */
  taskRunIds: string[];
  /** Max recordedAt across all rows for this file. */
  lastTouchMs: number;
  /** 0..1 decayed against `nowMs`; bright for recent, dim for old. */
  recency: number;
  /** True iff `taskRunIds.length > 1`. */
  contended: boolean;
}

/** Default sliding window (seconds). 30 s matches the plan §3 spec. */
const DEFAULT_WINDOW_SECS = 30;

/** Background poll interval (ms). 30 s matches the plan §3 spec. */
const POLL_INTERVAL_MS = 30_000;

/** Decay half-life-ish constant (seconds). At ~600 s the recency
 *  multiplier is `exp(-1) ≈ 0.37`; at 60 s it's `exp(-0.1) ≈ 0.90`. */
const RECENCY_DECAY_TAU_SEC = 600;

/**
 * Decay-weighted recency in [0, 1]. `recordedAtMs` newer than `nowMs`
 * (clock skew) clamps to 1; far-past values asymptote toward 0.
 *
 * Exported for unit-testing without React.
 */
export function decayRecency(recordedAtMs: number, nowMs: number): number {
  const deltaSec = Math.max(0, (nowMs - recordedAtMs) / 1000);
  // Math.exp(-0) === 1; Math.exp(-Infinity) === 0. Both safe.
  return Math.exp(-deltaSec / RECENCY_DECAY_TAU_SEC);
}

/**
 * Group raw touched-file rows into per-file heatmap rows, computing
 * contention + decayed recency, and sort contended-first then
 * recency-descending. Stable on tied recencies via filePath secondary
 * sort.
 *
 * Exported for unit-testing.
 */
export function computeHeatmapRows(
  rows: ReadonlyArray<TouchedFileRow>,
  nowMs: number,
): HeatmapRow[] {
  // Group by filePath. Per file, dedupe task_run_ids and track most-recent
  // touch overall so the table can show a single Last-Touch timestamp.
  const groups = new Map<
    string,
    {
      filePath: string;
      // Per-session most-recent timestamp, for sorting taskRunIds within
      // the row most-recent-first.
      perSession: Map<string, number>;
      lastTouchMs: number;
    }
  >();

  for (const r of rows) {
    let g = groups.get(r.file_path);
    if (!g) {
      g = {
        filePath: r.file_path,
        perSession: new Map(),
        lastTouchMs: r.recorded_at_ms,
      };
      groups.set(r.file_path, g);
    }
    const prevSessionTs = g.perSession.get(r.task_run_id) ?? -Infinity;
    if (r.recorded_at_ms > prevSessionTs) {
      g.perSession.set(r.task_run_id, r.recorded_at_ms);
    }
    if (r.recorded_at_ms > g.lastTouchMs) {
      g.lastTouchMs = r.recorded_at_ms;
    }
  }

  const out: HeatmapRow[] = [];
  for (const g of groups.values()) {
    const sessionEntries = [...g.perSession.entries()].sort(
      (a, b) => b[1] - a[1],
    );
    const taskRunIds = sessionEntries.map(([id]) => id);
    out.push({
      filePath: g.filePath,
      taskRunIds,
      lastTouchMs: g.lastTouchMs,
      recency: decayRecency(g.lastTouchMs, nowMs),
      contended: taskRunIds.length > 1,
    });
  }

  // Sort: contended-first (descending — true comes first), then
  // recency-descending. Tie-break on filePath ascending so the order is
  // deterministic.
  out.sort((a, b) => {
    if (a.contended !== b.contended) return a.contended ? -1 : 1;
    if (a.recency !== b.recency) return b.recency - a.recency;
    return a.filePath.localeCompare(b.filePath);
  });

  return out;
}

function formatLastTouch(lastTouchMs: number, nowMs: number): string {
  const deltaSec = Math.max(0, Math.floor((nowMs - lastTouchMs) / 1000));
  if (deltaSec < 60) return `${deltaSec}s ago`;
  const deltaMin = Math.floor(deltaSec / 60);
  if (deltaMin < 60) return `${deltaMin}m ago`;
  const deltaHr = Math.floor(deltaMin / 60);
  return `${deltaHr}h ago`;
}

interface FileOwnershipHeatmapProps {
  windowSecs?: number;
  onClose?: () => void;
}

export function FileOwnershipHeatmap({
  windowSecs = DEFAULT_WINDOW_SECS,
  onClose,
}: FileOwnershipHeatmapProps) {
  const [rawRows, setRawRows] = useState<TouchedFileRow[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  // Bumped on every refresh so memoized rows recompute decay against a
  // fresh `Date.now()`.
  const [tickMs, setTickMs] = useState(() => Date.now());

  const windowSecsRef = useRef(windowSecs);
  useEffect(() => {
    windowSecsRef.current = windowSecs;
  }, [windowSecs]);

  const fetchRows = useMemo(() => {
    return async () => {
      try {
        const result = await invoke<TouchedFileRow[]>(
          "recent_session_touched_files",
          { windowSecs: windowSecsRef.current },
        );
        setRawRows(Array.isArray(result) ? result : []);
        setError(null);
      } catch (e) {
        const msg = e instanceof Error ? e.message : String(e);
        logger.warn("recent_session_touched_files failed:", msg);
        setError(msg);
      } finally {
        setLoading(false);
        setTickMs(Date.now());
      }
    };
  }, []);

  // Initial fetch + 30 s background poll.
  useEffect(() => {
    let active = true;
    void fetchRows();
    const intervalId = setInterval(() => {
      if (active) void fetchRows();
    }, POLL_INTERVAL_MS);
    return () => {
      active = false;
      clearInterval(intervalId);
    };
  }, [fetchRows]);

  // Subscribe to `commit-state-changed` — any fire means a session just
  // touched a file (dispatcher / transcript-watcher / commit emit it),
  // so the heatmap data may have changed. Refetch is cheap (LIMIT 5000).
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let cancelled = false;

    listen("commit-state-changed", () => {
      void fetchRows();
    })
      .then((fn) => {
        if (cancelled) {
          fn();
          return;
        }
        unlisten = fn;
      })
      .catch((err) => {
        logger.warn("Failed to subscribe to commit-state-changed:", err);
      });

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [fetchRows]);

  const rows = useMemo(
    () => computeHeatmapRows(rawRows, tickMs),
    [rawRows, tickMs],
  );

  return (
    <div className="w-[420px] h-full shrink-0 flex flex-col bg-[#1a1b26] border-l border-[#2a2d3d]">
      <div className="flex items-center justify-between px-3 py-2 border-b border-[#2a2d3d]">
        <div className="flex items-center gap-2">
          <span className="text-sm font-medium text-[#a9b1d6]">File Ownership</span>
          <span className="text-[10px] text-[#565f89]">last {windowSecs}s</span>
        </div>
        {onClose && (
          <button
            onClick={onClose}
            className="text-[#565f89] hover:text-[#a9b1d6] text-xs px-2 py-0.5 rounded hover:bg-[#2a2d3d]"
            aria-label="Close file-ownership panel"
          >
            ×
          </button>
        )}
      </div>

      <div className="flex-1 overflow-auto">
        {loading && (
          <div className="flex items-center justify-center py-8">
            <div className="w-4 h-4 border-2 border-[#7aa2f7] border-t-transparent rounded-full animate-spin" />
          </div>
        )}

        {!loading && error && (
          <div className="p-3 text-xs text-[#f7768e]">
            Failed to load file activity: {error}
          </div>
        )}

        {!loading && !error && rows.length === 0 && (
          <div className="p-4 text-xs text-[#565f89] italic">
            No file activity in the last {windowSecs} seconds.
          </div>
        )}

        {!loading && !error && rows.length > 0 && (
          <table className="w-full text-xs">
            <thead className="sticky top-0 bg-[#1a1b26] border-b border-[#2a2d3d]">
              <tr className="text-left text-[#565f89]">
                <th className="px-2 py-1 font-normal">File</th>
                <th className="px-2 py-1 font-normal">Sessions</th>
                <th className="px-2 py-1 font-normal">Last Touch</th>
                <th className="px-2 py-1 font-normal">Contended</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((row) => {
                const tintAlpha = Math.min(0.3, row.recency * 0.3);
                const baseClass =
                  "border-b border-[#2a2d3d]/50 text-[#a9b1d6]" +
                  (row.contended ? " border-l-2 border-l-[#f7768e]" : "");
                return (
                  <tr
                    key={row.filePath}
                    className={baseClass}
                    style={{ backgroundColor: `rgba(122,162,247,${tintAlpha})` }}
                  >
                    <td
                      className="px-2 py-1 font-mono text-[11px] truncate max-w-[180px]"
                      title={row.filePath}
                    >
                      {row.filePath}
                    </td>
                    <td className="px-2 py-1 text-[11px]" title={row.taskRunIds.join("\n")}>
                      {row.taskRunIds.length}
                    </td>
                    <td className="px-2 py-1 text-[11px] text-[#565f89]">
                      {formatLastTouch(row.lastTouchMs, tickMs)}
                    </td>
                    <td className="px-2 py-1 text-[11px]">
                      {row.contended ? (
                        <span className="text-[#f7768e]">yes</span>
                      ) : (
                        <span className="text-[#565f89]">no</span>
                      )}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        )}
      </div>
    </div>
  );
}
