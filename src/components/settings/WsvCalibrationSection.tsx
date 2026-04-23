import { useState, useEffect, useMemo, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  BarChart3,
  RefreshCw,
  AlertTriangle,
  CheckCircle,
  XCircle,
  HelpCircle,
} from "lucide-react";
import { getAccentColors } from "@/design-system";
import type { WsvDisagreementRow, WsvMode, LogFunction } from "./types";

interface WsvCalibrationSectionProps {
  /** Current WSV mode — when not "shadow", show a hint banner. */
  mode: WsvMode;
  onLog: LogFunction;
}

type FilterKey = "any" | "text_pass_wsm_refused" | "text_pass_wsm_fail" | "text_fail_wsm_pass";

const FILTER_OPTIONS: { key: FilterKey; label: string; description: string }[] = [
  {
    key: "any",
    label: "Any mismatch",
    description: "All status disagreements",
  },
  {
    key: "text_pass_wsm_refused",
    label: "Text pass / WSM refused",
    description: "Text verifier said success; WSM flagged no-op. The key signal.",
  },
  {
    key: "text_pass_wsm_fail",
    label: "Text pass / WSM fail",
    description: "Text verifier said success; WSM said no visible progress.",
  },
  {
    key: "text_fail_wsm_pass",
    label: "Text fail / WSM pass",
    description: "WSM thinks the action worked but the text verifier is pessimistic.",
  },
];

const POLL_INTERVAL_MS = 10_000;

function applyFilter(rows: WsvDisagreementRow[], filter: FilterKey): WsvDisagreementRow[] {
  switch (filter) {
    case "any":
      return rows;
    case "text_pass_wsm_refused":
      return rows.filter((r) => r.text_status === "pass" && r.wsm_status === "refused");
    case "text_pass_wsm_fail":
      return rows.filter((r) => r.text_status === "pass" && r.wsm_status === "fail");
    case "text_fail_wsm_pass":
      return rows.filter((r) => r.text_status === "fail" && r.wsm_status === "pass");
  }
}

function formatRelativeTime(iso: string): string {
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return iso;
  const diffMs = Date.now() - then;
  const secs = Math.floor(diffMs / 1000);
  if (secs < 60) return `${secs}s ago`;
  const mins = Math.floor(secs / 60);
  if (mins < 60) return `${mins}m ago`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.floor(hours / 24);
  return `${days}d ago`;
}

function truncate(s: string, max: number): string {
  if (s.length <= max) return s;
  return `${s.slice(0, max - 1)}…`;
}

function StatusBadge({ status, confidence }: { status: string; confidence: number }) {
  const palette: Record<string, "green" | "yellow" | "red" | "blue" | "purple"> = {
    pass: "green",
    partial: "yellow",
    fail: "red",
    unreachable: "red",
    refused: "purple",
  };
  const color = palette[status] ?? "blue";
  const colors = getAccentColors(color);
  let Icon: typeof CheckCircle = HelpCircle;
  if (status === "pass") Icon = CheckCircle;
  else if (status === "refused") Icon = AlertTriangle;
  else if (status === "fail" || status === "unreachable") Icon = XCircle;
  return (
    <span
      className={`inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] font-medium ${colors.bg} ${colors.text}`}
      data-content-role="status"
      data-content-label={`wsv-${status}`}
    >
      <Icon className="w-3 h-3" />
      {status}
      <span className="opacity-70">{Math.round(confidence * 100)}%</span>
    </span>
  );
}

export function WsvCalibrationSection({ mode, onLog }: WsvCalibrationSectionProps) {
  const [rows, setRows] = useState<WsvDisagreementRow[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [filter, setFilter] = useState<FilterKey>("any");
  const [lastLoadedAt, setLastLoadedAt] = useState<number | null>(null);

  // Stash onLog in a ref so the polling interval below doesn't tear down
  // every time the parent re-renders with a new onLog identity. The parent
  // (Settings.tsx) does NOT wrap onLog in useCallback, so without this the
  // 10s interval would never actually fire in practice.
  const onLogRef = useRef(onLog);
  useEffect(() => {
    onLogRef.current = onLog;
  }, [onLog]);

  const loadRows = useCallback(async (isInitial: boolean) => {
    if (isInitial) setLoading(true);
    setError(null);
    try {
      const result = await invoke<WsvDisagreementRow[]>("list_wsv_disagreements", {
        taskRunId: null,
        limit: 200,
      });
      setRows(result);
      setLastLoadedAt(Date.now());
    } catch (err) {
      const message = `${err}`;
      setError(message);
      if (isInitial) {
        onLogRef.current("warning", `Failed to load WSV disagreements: ${message}`);
      }
    } finally {
      if (isInitial) setLoading(false);
    }
  }, []);

  useEffect(() => {
    // Async IIFE: avoid invoking a setState-bearing callback synchronously
    // in the effect body (set-state-in-effect).
    let cancelled = false;
    void (async () => {
      if (cancelled) return;
      await loadRows(true);
    })();
    const timer = setInterval(() => {
      loadRows(false);
    }, POLL_INTERVAL_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [loadRows]);

  const filtered = useMemo(() => applyFilter(rows, filter), [rows, filter]);

  const counts = useMemo(() => {
    const buckets: Record<FilterKey, number> = {
      any: rows.length,
      text_pass_wsm_refused: 0,
      text_pass_wsm_fail: 0,
      text_fail_wsm_pass: 0,
    };
    for (const r of rows) {
      if (r.text_status === "pass" && r.wsm_status === "refused") {
        buckets.text_pass_wsm_refused++;
      } else if (r.text_status === "pass" && r.wsm_status === "fail") {
        buckets.text_pass_wsm_fail++;
      } else if (r.text_status === "fail" && r.wsm_status === "pass") {
        buckets.text_fail_wsm_pass++;
      }
    }
    return buckets;
  }, [rows]);

  const isShadow = mode === "shadow";

  return (
    <div className="rounded-lg bg-card/50 p-4 space-y-4">
      <div className="flex items-center justify-between gap-3">
        <div className="flex items-center gap-3">
          <BarChart3 className="w-5 h-5 text-primary" />
          <div>
            <h4 className="font-medium text-sm">Shadow-Mode Calibration</h4>
            <p className="text-xs text-muted-foreground">
              Recent WSM ↔ text verifier disagreements. Review before flipping Enabled.
            </p>
          </div>
        </div>
        <button
          onClick={() => loadRows(false)}
          disabled={loading}
          className="p-1.5 rounded-md hover:bg-muted/40 disabled:opacity-50"
          title="Refresh"
          data-content-role="button"
          data-content-label="refresh wsv calibration"
        >
          <RefreshCw className={`w-4 h-4 ${loading ? "animate-spin" : ""}`} />
        </button>
      </div>

      {!isShadow && (
        <div className={`p-3 ${getAccentColors("yellow").bg} rounded-lg flex gap-2`}>
          <AlertTriangle className={`w-4 h-4 ${getAccentColors("yellow").text} shrink-0 mt-0.5`} />
          <p className={`text-xs ${getAccentColors("yellow").text}`}>
            Enable <strong>Shadow</strong> mode above and run a workflow to start collecting
            disagreement data. Existing rows from previous shadow sessions are still shown below.
          </p>
        </div>
      )}

      {error && (
        <div className={`p-2 ${getAccentColors("red").bg} rounded-md flex items-start gap-2`}>
          <XCircle className={`w-3.5 h-3.5 ${getAccentColors("red").text} shrink-0 mt-0.5`} />
          <span className={`text-xs ${getAccentColors("red").text}`}>{error}</span>
        </div>
      )}

      {/* Filter chips */}
      <div className="flex flex-wrap gap-1.5">
        {FILTER_OPTIONS.map((opt) => {
          const selected = filter === opt.key;
          const count = counts[opt.key];
          return (
            <button
              key={opt.key}
              onClick={() => setFilter(opt.key)}
              title={opt.description}
              className={`px-2.5 py-1 rounded-md text-[11px] font-medium transition-colors flex items-center gap-1.5 ${
                selected
                  ? "bg-primary text-primary-foreground"
                  : "bg-muted/40 text-muted-foreground hover:bg-muted/60"
              }`}
              data-content-role="button"
              data-content-label={`filter-${opt.key}`}
            >
              {opt.label}
              <span
                className={`inline-flex items-center justify-center min-w-[18px] h-[16px] px-1 rounded-full text-[10px] ${
                  selected ? "bg-primary-foreground/20" : "bg-muted/70"
                }`}
              >
                {count}
              </span>
            </button>
          );
        })}
      </div>

      {/* Table */}
      {filtered.length === 0 ? (
        <div
          className="text-center py-8 text-xs text-muted-foreground"
          data-content-role="status"
          data-content-label="wsv calibration empty"
        >
          {rows.length === 0
            ? "No shadow-mode disagreements recorded yet."
            : `No rows match the "${FILTER_OPTIONS.find((f) => f.key === filter)?.label}" filter.`}
          {rows.length === 0 && isShadow && (
            <div className="mt-1">Run an agentic workflow to populate this view.</div>
          )}
        </div>
      ) : (
        <div
          className="overflow-x-auto"
          data-content-role="table"
          data-content-label="wsv disagreements"
        >
          <table className="w-full text-[11px]">
            <thead className="text-muted-foreground">
              <tr className="border-b border-border/50">
                <th className="text-left py-1.5 pr-2 font-medium">Time</th>
                <th className="text-left py-1.5 pr-2 font-medium">Task Run</th>
                <th className="text-left py-1.5 pr-2 font-medium">Iter</th>
                <th className="text-left py-1.5 pr-2 font-medium">Text</th>
                <th className="text-left py-1.5 pr-2 font-medium">WSM</th>
                <th className="text-left py-1.5 pr-2 font-medium">Intent</th>
                <th className="text-left py-1.5 font-medium">WSM Observations</th>
              </tr>
            </thead>
            <tbody>
              {filtered.map((row) => (
                <tr key={row.id} className="border-b border-border/30 hover:bg-muted/20">
                  <td className="py-1 pr-2 text-muted-foreground whitespace-nowrap">
                    {formatRelativeTime(row.created_at)}
                  </td>
                  <td className="py-1 pr-2 font-mono text-muted-foreground">
                    {truncate(row.task_run_id, 10)}
                  </td>
                  <td className="py-1 pr-2">{row.iteration}</td>
                  <td className="py-1 pr-2">
                    <StatusBadge status={row.text_status} confidence={row.text_confidence} />
                  </td>
                  <td className="py-1 pr-2">
                    <StatusBadge status={row.wsm_status} confidence={row.wsm_confidence} />
                  </td>
                  <td className="py-1 pr-2 max-w-[280px] text-muted-foreground" title={row.intent}>
                    {truncate(row.intent, 80)}
                  </td>
                  <td
                    className="py-1 max-w-[320px] text-muted-foreground"
                    title={row.wsm_observations}
                  >
                    {truncate(row.wsm_observations, 120)}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {lastLoadedAt !== null && (
        <div className="text-[10px] text-muted-foreground text-right">
          {rows.length} row(s) loaded · auto-refresh every 10s
        </div>
      )}
    </div>
  );
}
