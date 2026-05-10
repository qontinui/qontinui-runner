/**
 * FileActivityPanel — Phase 2 of the file-ownership-heatmap plan.
 *
 * Three vertically-stacked sub-sections:
 *   1. Live snapshot  — `FileRegistryManager.info()` grouped by
 *      `holder_name`; files indented beneath their holder.
 *   2. Hot files      — top N most-touched paths in the selected
 *      window, with bar-length rendering of the touch count.
 *   3. Hot sessions   — top N most-active sessions in the same window,
 *      with their file count and latest activity.
 *
 * Lives at the bottom of `CoordinatorDashboard` as a sixth panel,
 * outside the productivity-stack spec-locked five-panel block. The
 * locked block asserts only `exists` for each panel (not relative
 * order), so appending here doesn't break the assertion — but the
 * spec's `Coordinator Dashboard — Five-Panel Stack` description gets
 * a parallel update in the same commit.
 *
 * Per `proj_runner_analysis_state_split.md`, sibling-panel data
 * sharing in the runner is done through explicit channels — props
 * or CustomEvents — not implicit context. This panel currently
 * mirrors `WorkersPanel`'s "click reveals the Terminals page, user
 * picks their tab from there" pattern; per-tab focus is a documented
 * follow-up (the substrate to dispatch `setActiveId` across siblings
 * doesn't exist yet at the CoordinatorDashboard scope).
 */

import { useCallback, useMemo, useState } from "react";
import { Flame, FileText, Clock, RefreshCw, Users } from "lucide-react";
import {
  DEFAULT_WINDOW_SECS,
  WINDOW_OPTIONS,
  fetchHeatmap,
  loadStoredWindowSecs,
  storeWindowSecs,
  useFileActivity,
  type FileRegistryInfoEntry,
  type HotFileRow,
  type HotSessionRow,
} from "./fileActivityApi";

/** Render relative-time label without pulling in date-fns again — the
 *  existing import in CoordinatorDashboard works, but the panel is
 *  intentionally standalone so the unit test can mount it in isolation
 *  without configuring date-fns mocks. `nowMs` is injected for tests;
 *  production callers omit it. */
export function ageLabel(isoTimestamp: string, nowMs: number = Date.now()): string {
  const ts = Date.parse(isoTimestamp);
  if (!Number.isFinite(ts)) return "unknown";
  const ageMs = nowMs - ts;
  if (ageMs < 0) return "now";
  const s = Math.floor(ageMs / 1000);
  if (s < 60) return `${s}s ago`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ago`;
  return `${Math.floor(h / 24)}d ago`;
}

/** Build a 10-segment bar string. Plan calls for sort + bar length, not
 *  color intensity — color without calibration is fake precision. */
export function barFor(count: number, maxCount: number): string {
  if (maxCount <= 0) return "";
  const filled = Math.max(1, Math.round((count / maxCount) * 10));
  const empty = Math.max(0, 10 - filled);
  return "▮".repeat(filled) + "▯".repeat(empty); // ▮ filled, ▯ empty
}

interface PanelHeaderProps {
  windowSecs: number;
  onWindowChange: (secs: number) => void;
  onRefresh: () => void;
  isStale: boolean;
  error: string | null;
}

function PanelHeader({
  windowSecs,
  onWindowChange,
  onRefresh,
  isStale,
  error,
}: PanelHeaderProps) {
  return (
    <header className="flex items-center justify-between">
      <div className="flex items-center gap-2">
        <Flame className="w-4 h-4 text-muted-foreground" />
        <h2
          id="productivity-file-activity-heading"
          className="text-sm font-semibold text-foreground"
        >
          File activity
        </h2>
        {isStale && (
          <span
            title={error ?? "data older than 2s — refreshing"}
            data-ui-bridge-id="productivity.file-activity-stale"
            className="inline-flex items-center gap-1 text-xs text-amber-400"
          >
            <Clock className="w-3 h-3" />
            stale
          </span>
        )}
      </div>
      <div className="flex items-center gap-2">
        <label
          htmlFor="productivity-file-activity-window"
          className="text-xs text-muted-foreground"
        >
          window:
        </label>
        <select
          id="productivity-file-activity-window"
          value={windowSecs}
          onChange={(e) => onWindowChange(Number.parseInt(e.target.value, 10))}
          data-ui-bridge-id="productivity.file-activity-window-select"
          className="rounded-md border border-border bg-background px-2 py-1 text-xs text-foreground"
        >
          {WINDOW_OPTIONS.map((o) => (
            <option key={o.secs} value={o.secs}>
              {o.label}
            </option>
          ))}
        </select>
        <button
          type="button"
          onClick={onRefresh}
          data-ui-bridge-id="productivity.file-activity-refresh"
          className="inline-flex items-center gap-1 rounded-md border border-border px-2 py-1 text-xs text-muted-foreground hover:text-foreground hover:bg-muted/30"
        >
          <RefreshCw className="w-3 h-3" />
          Refresh
        </button>
      </div>
    </header>
  );
}

interface LiveSnapshotSectionProps {
  rows: readonly FileRegistryInfoEntry[];
  onJumpToHolder: (holderName: string) => void;
}

function LiveSnapshotSection({ rows, onJumpToHolder }: LiveSnapshotSectionProps) {
  // Group by holder_name. Map insertion order is preserved — sort holders
  // by their newest-registered file so the most recently active appears
  // first; ties broken alphabetically.
  const grouped = useMemo(() => {
    const byHolder = new Map<string, FileRegistryInfoEntry[]>();
    for (const r of rows) {
      const list = byHolder.get(r.holder_name) ?? [];
      list.push(r);
      byHolder.set(r.holder_name, list);
    }
    return Array.from(byHolder.entries())
      .map(([holder, entries]) => ({
        holder,
        entries: [...entries].sort((a, b) => b.registered_at - a.registered_at),
        latest: Math.max(...entries.map((e) => e.registered_at)),
      }))
      .sort((a, b) => b.latest - a.latest || a.holder.localeCompare(b.holder));
  }, [rows]);

  if (grouped.length === 0) {
    return (
      <div
        className="rounded-md border border-border/40 bg-muted/10 p-3 text-xs text-muted-foreground"
        data-ui-bridge-id="productivity.file-activity-live-empty"
      >
        No sessions currently hold files in this runner.
      </div>
    );
  }

  return (
    <ul
      className="flex flex-col gap-2"
      data-ui-bridge-id="productivity.file-activity-live"
    >
      {grouped.map(({ holder, entries }) => (
        <li
          key={holder}
          className="rounded-md border border-border/40 bg-background/40 p-2"
        >
          <button
            type="button"
            onClick={() => onJumpToHolder(holder)}
            className="text-xs font-mono text-foreground hover:underline"
            data-ui-bridge-id="productivity.file-activity-holder-row"
            data-holder-name={holder}
          >
            {holder}
          </button>
          <ul className="mt-1 pl-3 text-xs text-muted-foreground space-y-0.5">
            {entries.map((e) => (
              <li
                key={e.file_path}
                className="truncate"
                title={e.file_path}
              >
                {e.file_path}
              </li>
            ))}
          </ul>
        </li>
      ))}
    </ul>
  );
}

interface HotFilesSectionProps {
  rows: readonly HotFileRow[];
}

function HotFilesSection({ rows }: HotFilesSectionProps) {
  const max = rows.length === 0 ? 0 : Math.max(...rows.map((r) => r.distinct_sessions));
  if (rows.length === 0) {
    return (
      <div
        className="rounded-md border border-border/40 bg-muted/10 p-3 text-xs text-muted-foreground"
        data-ui-bridge-id="productivity.file-activity-hot-files-empty"
      >
        No file activity in this window.
      </div>
    );
  }
  return (
    <table
      className="w-full text-xs"
      data-ui-bridge-id="productivity.file-activity-hot-files"
    >
      <thead className="text-muted-foreground">
        <tr>
          <th className="text-left font-normal pb-1">Path</th>
          <th className="text-left font-normal pb-1 w-32">Touches</th>
          <th className="text-left font-normal pb-1 w-24">Latest</th>
        </tr>
      </thead>
      <tbody>
        {rows.map((r) => (
          <tr
            key={r.file_path}
            data-ui-bridge-id="productivity.file-activity-hot-file-row"
            data-file-path={r.file_path}
          >
            <td className="text-foreground truncate max-w-[18rem]" title={r.file_path}>
              {r.file_path}
            </td>
            <td>
              <span className="font-mono text-foreground/90 mr-2">
                {r.distinct_sessions}
              </span>
              <span className="font-mono text-muted-foreground">
                {barFor(r.distinct_sessions, max)}
              </span>
            </td>
            <td className="text-muted-foreground">{ageLabel(r.latest_recorded_at)}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

interface HotSessionsSectionProps {
  rows: readonly HotSessionRow[];
}

function HotSessionsSection({ rows }: HotSessionsSectionProps) {
  const max = rows.length === 0 ? 0 : Math.max(...rows.map((r) => r.distinct_files));
  if (rows.length === 0) {
    return (
      <div
        className="rounded-md border border-border/40 bg-muted/10 p-3 text-xs text-muted-foreground"
        data-ui-bridge-id="productivity.file-activity-hot-sessions-empty"
      >
        No session activity in this window.
      </div>
    );
  }
  return (
    <table
      className="w-full text-xs"
      data-ui-bridge-id="productivity.file-activity-hot-sessions"
    >
      <thead className="text-muted-foreground">
        <tr>
          <th className="text-left font-normal pb-1">Session</th>
          <th className="text-left font-normal pb-1 w-32">Files</th>
          <th className="text-left font-normal pb-1 w-24">Latest</th>
        </tr>
      </thead>
      <tbody>
        {rows.map((r) => (
          <tr
            key={r.task_run_id}
            data-ui-bridge-id="productivity.file-activity-hot-session-row"
            data-task-run-id={r.task_run_id}
          >
            <td className="font-mono text-foreground truncate max-w-[18rem]" title={r.task_run_id}>
              {r.task_run_id}
            </td>
            <td>
              <span className="font-mono text-foreground/90 mr-2">{r.distinct_files}</span>
              <span className="font-mono text-muted-foreground">
                {barFor(r.distinct_files, max)}
              </span>
            </td>
            <td className="text-muted-foreground">{ageLabel(r.latest_recorded_at)}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

export interface FileActivityPanelProps {
  /** Test seam — when supplied, the panel skips its polling loop and
   *  renders the supplied snapshot. Production callers omit this. */
  initialDataForTest?: import("./fileActivityApi").HeatmapResponse;
  /** Optional callback fired when the user clicks a holder name. Used
   *  by tests to assert click-to-jump behavior. Production wires to
   *  `__UI_BRIDGE__.navigateHandler("terminal")` (page-nav only;
   *  per-tab focus is a follow-up — see panel header comment). */
  onJumpToHolder?: (holderName: string) => void;
}

export function FileActivityPanel({
  initialDataForTest,
  onJumpToHolder,
}: FileActivityPanelProps) {
  const [windowSecs, setWindowSecsState] = useState<number>(() =>
    initialDataForTest ? DEFAULT_WINDOW_SECS : loadStoredWindowSecs(),
  );

  const { data, error, isStale, refresh } = useFileActivity({
    windowSecs,
    enabled: initialDataForTest == null,
  });

  const snapshot = initialDataForTest ?? data;

  const handleWindowChange = useCallback((secs: number) => {
    setWindowSecsState(secs);
    storeWindowSecs(secs);
  }, []);

  const defaultJumpHandler = useCallback((_holderName: string) => {
    // Mirror WorkersPanel: page-nav to Terminals; per-tab focus is a
    // documented follow-up (the dashboard scope doesn't currently own
    // `setActiveId`).
    const handler = (
      window as unknown as {
        __UI_BRIDGE__?: { navigateHandler?: (url: string) => void };
      }
    )?.__UI_BRIDGE__?.navigateHandler;
    handler?.("terminal");
  }, []);

  const jumpHandler = onJumpToHolder ?? defaultJumpHandler;

  return (
    <section
      role="region"
      aria-labelledby="productivity-file-activity-heading"
      className="flex flex-col rounded-lg border border-border bg-card/30 p-4 gap-3"
      data-ui-bridge-id="productivity.file-activity"
    >
      <PanelHeader
        windowSecs={windowSecs}
        onWindowChange={handleWindowChange}
        onRefresh={refresh}
        isStale={isStale}
        error={error}
      />

      <div className="flex flex-col gap-3">
        <section
          aria-label="Live file holdings"
          data-ui-bridge-id="productivity.file-activity-live-section"
        >
          <div className="flex items-center gap-1.5 text-xs text-muted-foreground mb-1">
            <Users className="w-3 h-3" />
            Currently editing
          </div>
          <LiveSnapshotSection
            rows={snapshot?.live ?? []}
            onJumpToHolder={jumpHandler}
          />
        </section>

        <section
          aria-label="Hot files"
          data-ui-bridge-id="productivity.file-activity-hot-files-section"
        >
          <div className="flex items-center gap-1.5 text-xs text-muted-foreground mb-1">
            <FileText className="w-3 h-3" />
            Most touched files
          </div>
          <HotFilesSection rows={snapshot?.hot_files ?? []} />
        </section>

        <section
          aria-label="Hot sessions"
          data-ui-bridge-id="productivity.file-activity-hot-sessions-section"
        >
          <div className="flex items-center gap-1.5 text-xs text-muted-foreground mb-1">
            <Flame className="w-3 h-3" />
            Most active sessions
          </div>
          <HotSessionsSection rows={snapshot?.hot_sessions ?? []} />
        </section>
      </div>
    </section>
  );
}

export default FileActivityPanel;

// Re-export the raw fetcher so the unit test (and a future server-side
// renderer) can drive the panel without the React lifecycle.
export { fetchHeatmap };
