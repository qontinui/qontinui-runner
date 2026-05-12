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

import { useCallback, useEffect, useMemo, useState } from "react";
import { Flame, FileText, Clock, Hand, RefreshCw, Users } from "lucide-react";
import { getApiPort } from "@/lib/runner-api";
import { createLogger } from "@/lib/logger";
import {
  DEFAULT_WINDOW_SECS,
  WINDOW_OPTIONS,
  fetchHeatmap,
  loadStoredWindowSecs,
  storeWindowSecs,
  useFileActivity,
  type FileLockInfoEntry,
  type FileRegistryInfoEntry,
  type HotFileRow,
  type HotSessionRow,
} from "./fileActivityApi";

const logger = createLogger("FileActivityPanel");

/**
 * Lock-Yield cooldown — duplicate of `WaitingLockBanner.REQUEST_YIELD_COOLDOWN_MS`
 * (Phase 3). Kept local rather than imported across the productivity ↔ terminal
 * folder boundary; the two surfaces use the same UX so a future plan should
 * dedupe by hoisting both into a shared `lockYield` module.
 *
 * TODO(lock-yield): dedupe with WaitingLockBanner's constant.
 */
export const REQUEST_YIELD_COOLDOWN_MS = 30_000;

/** Cooldown map key: `${file_path}::${holder_task_run_id}` — per the plan's
 *  Phase 4 spec, the cooldown is per-(file,holder) pair so clicking yield
 *  on one row doesn't disable yield on another. */
export function lockYieldCooldownKey(filePath: string, holderTaskRunId: string): string {
  return `${filePath}::${holderTaskRunId}`;
}

/** Returns true iff the registry entry's (holder_task_run_id, file_path)
 *  pair matches a currently-held exclusive lock. Pure / exported for tests. */
export function hasExclusiveLock(
  entry: Pick<FileRegistryInfoEntry, "file_path" | "holder_task_run_id">,
  lockInfo: readonly FileLockInfoEntry[] | null,
): boolean {
  if (!lockInfo) return false;
  return lockInfo.some(
    (l) =>
      l.file_path === entry.file_path &&
      l.holder_task_run_id === entry.holder_task_run_id,
  );
}

/** Compute cooldown's remaining seconds (rounded UP to mirror
 *  `WaitingLockBanner.cooldownRemainingSecs`). Pure / exported for tests. */
export function lockYieldCooldownRemainingSecs(
  cooldownUntilMs: number,
  nowMs: number,
): number {
  if (cooldownUntilMs <= nowMs) return 0;
  return Math.ceil((cooldownUntilMs - nowMs) / 1000);
}

/** Build the POST body for the Phase 1 `/file-locks/yield-request` endpoint
 *  using the synthetic Coordinator Dashboard requester identity per
 *  §Open Q5 of the lock-yield plan. The holder banner will display
 *  "Coordinator Dashboard has asked you to yield" — intentional signal
 *  that the request came from the global dashboard view, not a peer
 *  session. */
export function buildYieldRequestBody(
  filePath: string,
  holderTaskRunId: string,
): {
  file_path: string;
  requester_task_run_id: string;
  requester_name: string;
  holder_task_run_id: string;
} {
  return {
    file_path: filePath,
    requester_task_run_id: "coordinator-dashboard",
    requester_name: "Coordinator Dashboard",
    holder_task_run_id: holderTaskRunId,
  };
}

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
  /** Subset of `rows` that are also currently held as exclusive locks.
   *  Null until the first `/file-locks/info` fetch resolves — yield
   *  buttons stay hidden until then to avoid flashing in-then-out
   *  during initial poll. */
  lockInfo: readonly FileLockInfoEntry[] | null;
  onJumpToHolder: (holderName: string) => void;
  /** Test seam — defaults to `globalThis.fetch`. */
  fetchImpl?: typeof fetch;
}

function LiveSnapshotSection({
  rows,
  lockInfo,
  onJumpToHolder,
  fetchImpl,
}: LiveSnapshotSectionProps) {
  // Per-(file,holder) cooldown map. Key shape: `lockYieldCooldownKey()`.
  // Value: epoch ms at which the cooldown ends. Entries are never pruned
  // — the map grows with each click but stays bounded by the live row
  // count (no leak on the time-scale of a single panel mount).
  const [cooldowns, setCooldowns] = useState<Map<string, number>>(
    () => new Map(),
  );

  // Re-render once per second so the cooldown countdown stays visually
  // accurate. Cheap — same cadence WaitingLockBanner uses.
  const [nowMs, setNowMs] = useState(() => Date.now());
  useEffect(() => {
    // Skip the timer when there are no active cooldowns to avoid the
    // tick churn on the dashboard. The effect re-runs when `cooldowns`
    // changes, so a new click immediately re-arms the interval.
    let anyActive = false;
    const t0 = Date.now();
    for (const until of cooldowns.values()) {
      if (until > t0) {
        anyActive = true;
        break;
      }
    }
    if (!anyActive) return;
    const id = setInterval(() => setNowMs(Date.now()), 1000);
    return () => clearInterval(id);
  }, [cooldowns]);

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

  const handleRequestYield = useCallback(
    async (filePath: string, holderTaskRunId: string) => {
      const key = lockYieldCooldownKey(filePath, holderTaskRunId);
      // Optimistically start the cooldown — even on POST failure we
      // don't want the user spamming the button.
      const cooldownUntil = Date.now() + REQUEST_YIELD_COOLDOWN_MS;
      setCooldowns((prev) => {
        const next = new Map(prev);
        next.set(key, cooldownUntil);
        return next;
      });

      try {
        const f = fetchImpl ?? globalThis.fetch;
        const resp = await f(
          `http://127.0.0.1:${getApiPort()}/file-locks/yield-request`,
          {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify(buildYieldRequestBody(filePath, holderTaskRunId)),
          },
        );
        if (!resp.ok) {
          logger.warn(
            `yield-request POST returned ${resp.status} for ${filePath} (holder=${holderTaskRunId})`,
          );
        }
      } catch (err) {
        logger.error("yield-request POST failed:", err);
      }
    },
    [fetchImpl],
  );

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
            {entries.map((e) => {
              const yieldable = hasExclusiveLock(e, lockInfo);
              const key = lockYieldCooldownKey(e.file_path, e.holder_task_run_id);
              const cooldownUntil = cooldowns.get(key) ?? 0;
              const cooldownLeft = lockYieldCooldownRemainingSecs(
                cooldownUntil,
                nowMs,
              );
              const inCooldown = cooldownLeft > 0;
              const buttonTitle = inCooldown
                ? `Cooldown — request again in ${cooldownLeft}s`
                : "Ask the holder to yield this lock";

              return (
                <li
                  key={e.file_path}
                  className="flex items-center gap-2"
                  data-ui-bridge-id="productivity.file-activity-live-file-row"
                  data-file-path={e.file_path}
                  data-holder-task-run-id={e.holder_task_run_id}
                >
                  <span className="truncate flex-1" title={e.file_path}>
                    {e.file_path}
                  </span>
                  {yieldable && (
                    <button
                      type="button"
                      data-ui-bridge-id="productivity.file-activity-yield"
                      data-file-path={e.file_path}
                      data-holder-task-run-id={e.holder_task_run_id}
                      disabled={inCooldown}
                      onClick={() =>
                        void handleRequestYield(e.file_path, e.holder_task_run_id)
                      }
                      title={buttonTitle}
                      className={
                        inCooldown
                          ? "inline-flex items-center gap-1 rounded-md border border-border px-1.5 py-0.5 text-[10px] text-muted-foreground/60 cursor-not-allowed"
                          : "inline-flex items-center gap-1 rounded-md border border-border px-1.5 py-0.5 text-[10px] text-muted-foreground hover:text-foreground hover:bg-muted/30"
                      }
                    >
                      <Hand className="w-3 h-3" />
                      {inCooldown ? `${cooldownLeft}s` : "Yield"}
                    </button>
                  )}
                </li>
              );
            })}
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
  /** Test seam — companion lock-info snapshot when `initialDataForTest`
   *  is set. Production callers omit this; the panel's poll loop will
   *  fetch it on the same 5s cadence as the heatmap. */
  initialLockInfoForTest?: FileLockInfoEntry[];
  /** Optional callback fired when the user clicks a holder name. Used
   *  by tests to assert click-to-jump behavior. Production wires to
   *  `__UI_BRIDGE__.navigateHandler("terminal")` (page-nav only;
   *  per-tab focus is a follow-up — see panel header comment). */
  onJumpToHolder?: (holderName: string) => void;
}

export function FileActivityPanel({
  initialDataForTest,
  initialLockInfoForTest,
  onJumpToHolder,
}: FileActivityPanelProps) {
  const [windowSecs, setWindowSecsState] = useState<number>(() =>
    initialDataForTest ? DEFAULT_WINDOW_SECS : loadStoredWindowSecs(),
  );

  const { data, lockInfo, error, isStale, refresh } = useFileActivity({
    windowSecs,
    enabled: initialDataForTest == null,
  });

  const snapshot = initialDataForTest ?? data;
  const effectiveLockInfo = initialDataForTest
    ? (initialLockInfoForTest ?? null)
    : lockInfo;

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
            lockInfo={effectiveLockInfo}
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
