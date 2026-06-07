/**
 * Memory Federation Dashboard — Phase 3 of the federation-dashboard plan.
 *
 * Surfaces reconcile reports from `memory-federation:reconcile` Tauri events
 * emitted at session-end. Accumulates reports since page mount and shows
 * summary tiles + expandable detail rows.
 *
 * Historical data from coord (`GET /coord/federation/reports`) is fetched
 * via the runner's Tauri IPC (`get_federation_reports` command) when
 * available.
 */

import { useEffect, useState, useMemo, useCallback } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface ReconcileReport {
  pushed: number;
  pulled: number;
  unchanged: number;
  failed: number;
  failed_names?: string[];
}

interface ReconcileEvent {
  session_id: string;
  account: string;
  report: ReconcileReport;
}

/** Row shape for both live events and coord-fetched historical reports. */
interface FederationReportRow {
  id: string;
  session_id: string;
  account_name: string;
  device_id?: string;
  pushed: number;
  pulled: number;
  unchanged: number;
  failed: number;
  failed_names?: string[];
  reported_at: string;
  metadata?: Record<string, unknown>;
  source: "live" | "coord";
}

type TimeRange = "1h" | "24h" | "7d" | "all";

const TIME_RANGE_LABELS: Record<TimeRange, string> = {
  "1h": "Last 1h",
  "24h": "Last 24h",
  "7d": "Last 7d",
  all: "All",
};

const TIME_RANGE_MS: Record<TimeRange, number | null> = {
  "1h": 60 * 60 * 1000,
  "24h": 24 * 60 * 60 * 1000,
  "7d": 7 * 24 * 60 * 60 * 1000,
  all: null,
};

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export function MemoryFederationPage() {
  const [liveReports, setLiveReports] = useState<FederationReportRow[]>([]);
  const [coordReports, setCoordReports] = useState<FederationReportRow[]>([]);
  const [coordError, setCoordError] = useState<string | null>(null);
  const [coordLoading, setCoordLoading] = useState(false);
  const [timeRange, setTimeRange] = useState<TimeRange>("24h");
  const [expandedId, setExpandedId] = useState<string | null>(null);

  // -----------------------------------------------------------------------
  // Live events from Tauri
  // -----------------------------------------------------------------------
  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    let counter = 0;
    (async () => {
      try {
        unlisten = await listen<ReconcileEvent>(
          "memory-federation:reconcile",
          (event) => {
            counter += 1;
            const row: FederationReportRow = {
              id: `live-${counter}-${Date.now()}`,
              session_id: event.payload.session_id,
              account_name: event.payload.account,
              pushed: event.payload.report.pushed,
              pulled: event.payload.report.pulled,
              unchanged: event.payload.report.unchanged,
              failed: event.payload.report.failed,
              failed_names: event.payload.report.failed_names,
              reported_at: new Date().toISOString(),
              source: "live",
            };
            setLiveReports((prev) => [row, ...prev]);
          },
        );
      } catch (e) {
        console.error("MemoryFederationPage: listen failed", e);
      }
    })();
    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  // -----------------------------------------------------------------------
  // Fetch historical reports from coord via Tauri command
  // -----------------------------------------------------------------------
  const fetchFromCoord = useCallback(async () => {
    setCoordLoading(true);
    setCoordError(null);
    try {
      const sinceMs = TIME_RANGE_MS[timeRange];
      const since = sinceMs
        ? new Date(Date.now() - sinceMs).toISOString()
        : undefined;

      const result = await invoke<{
        items: Array<{
          report_id: string;
          session_id: string;
          account_name: string;
          device_id?: string;
          pushed: number;
          pulled: number;
          unchanged: number;
          failed: number;
          failed_names?: string[];
          reported_at: string;
          metadata?: Record<string, unknown>;
        }>;
        // Tauri wraps the command's single struct parameter under its
        // declared name (`args`); the payload must mirror that or Tauri
        // rejects the call with "missing required key args" before any
        // HTTP request to coord is made.
      }>("get_federation_reports", { args: { since, limit: 200 } });

      const rows: FederationReportRow[] = (result?.items ?? []).map((item) => ({
        id: item.report_id,
        session_id: item.session_id,
        account_name: item.account_name,
        device_id: item.device_id,
        pushed: item.pushed,
        pulled: item.pulled,
        unchanged: item.unchanged,
        failed: item.failed,
        failed_names: item.failed_names,
        reported_at: item.reported_at,
        metadata: item.metadata,
        source: "coord" as const,
      }));
      setCoordReports(rows);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      // Swallow "unknown command" — the Tauri command may not be wired yet
      if (msg.includes("not found") || msg.includes("unknown")) {
        setCoordError(
          "Historical data unavailable (Tauri command not registered). " +
            "Showing live session reports only.",
        );
      } else if (msg.includes("invalid args") || msg.includes("missing required key")) {
        // Tauri rejected the IPC payload before any HTTP call — this is a
        // client-side contract bug, not a coord-side failure. Don't blame
        // coord.
        setCoordError(`Internal error loading history (IPC contract): ${msg}`);
      } else {
        setCoordError(`Failed to fetch from coord: ${msg}`);
      }
      setCoordReports([]);
    } finally {
      setCoordLoading(false);
    }
  }, [timeRange]);

  useEffect(() => {
    fetchFromCoord();
  }, [fetchFromCoord]);

  // -----------------------------------------------------------------------
  // Merge + filter
  // -----------------------------------------------------------------------
  const allReports = useMemo(() => {
    // Deduplicate: coord reports keyed by report_id, live reports have
    // synthetic ids. Merge by putting live first, then coord, dedup by
    // session_id + account to avoid double-counting events that were both
    // received live and then fetched from coord.
    const seen = new Set<string>();
    const merged: FederationReportRow[] = [];
    for (const row of liveReports) {
      const key = `${row.session_id}:${row.account_name}`;
      if (!seen.has(key)) {
        seen.add(key);
        merged.push(row);
      }
    }
    for (const row of coordReports) {
      const key = `${row.session_id}:${row.account_name}`;
      if (!seen.has(key)) {
        seen.add(key);
        merged.push(row);
      }
    }

    // Filter by time range
    const sinceMs = TIME_RANGE_MS[timeRange];
    if (sinceMs) {
      const cutoff = Date.now() - sinceMs;
      return merged.filter(
        (r) => new Date(r.reported_at).getTime() >= cutoff,
      );
    }
    return merged;
  }, [liveReports, coordReports, timeRange]);

  // Sort by reported_at desc
  const sortedReports = useMemo(
    () =>
      [...allReports].sort(
        (a, b) =>
          new Date(b.reported_at).getTime() -
          new Date(a.reported_at).getTime(),
      ),
    [allReports],
  );

  // -----------------------------------------------------------------------
  // Install shape — single-account (cross-device sync) vs multi-account
  // (cross-account federation). Derived purely from the data already
  // present: count DISTINCT account_name values across the merged reports.
  // The literal "default" account is a single-account install (no per-
  // account .claude-<name> dir), so it does not count toward distinctness.
  // -----------------------------------------------------------------------
  const mode = useMemo<"cross-device" | "cross-account">(() => {
    const distinctAccounts = new Set<string>();
    for (const r of allReports) {
      if (r.account_name && r.account_name !== "default") {
        distinctAccounts.add(r.account_name);
      }
    }
    return distinctAccounts.size >= 2 ? "cross-account" : "cross-device";
  }, [allReports]);

  const copy =
    mode === "cross-account"
      ? {
          subtitle:
            "Cross-account federation — memories merged across this tenant's Claude accounts.",
          empty:
            "No cross-account federation reports in the selected time range.",
        }
      : {
          subtitle:
            "Cross-device sync — memories synced across this account's machines.",
          empty: "No cross-device sync reports in the selected time range.",
        };

  // -----------------------------------------------------------------------
  // Summary
  // -----------------------------------------------------------------------
  const summary = useMemo(() => {
    let sessions = 0;
    let pushed = 0;
    let pulled = 0;
    let failed = 0;
    for (const r of sortedReports) {
      sessions += 1;
      pushed += r.pushed;
      pulled += r.pulled;
      failed += r.failed;
    }
    return { sessions, pushed, pulled, failed };
  }, [sortedReports]);

  // -----------------------------------------------------------------------
  // Render
  // -----------------------------------------------------------------------
  return (
    <div style={pageStyle}>
      {/* Header */}
      <div style={headerStyle}>
        <div>
          <h2 style={titleStyle}>Memory Federation</h2>
          <p style={subtitleStyle}>{copy.subtitle}</p>
        </div>
        <div style={headerActionsStyle}>
          {/* Time range selector */}
          <div style={timeRangeSelectorStyle}>
            {(Object.keys(TIME_RANGE_LABELS) as TimeRange[]).map((key) => (
              <button
                key={key}
                type="button"
                style={
                  timeRange === key
                    ? timeRangeBtnActiveStyle
                    : timeRangeBtnStyle
                }
                onClick={() => setTimeRange(key)}
              >
                {TIME_RANGE_LABELS[key]}
              </button>
            ))}
          </div>
          <button
            type="button"
            style={refreshBtnStyle}
            onClick={fetchFromCoord}
            disabled={coordLoading}
          >
            {coordLoading ? "Loading..." : "Refresh"}
          </button>
        </div>
      </div>

      {/* Coord error notice */}
      {coordError ? (
        <div style={noticeStyle}>{coordError}</div>
      ) : null}

      {/* Summary tiles */}
      <div style={tilesRowStyle}>
        <SummaryTile label="Sessions" value={summary.sessions} />
        <SummaryTile label="Pushed" value={summary.pushed} color="#22c55e" />
        <SummaryTile label="Pulled" value={summary.pulled} color="#3b82f6" />
        <SummaryTile
          label="Failed"
          value={summary.failed}
          color={summary.failed > 0 ? "#ef4444" : undefined}
        />
      </div>

      {/* Reports table */}
      <div style={tableContainerStyle}>
        {sortedReports.length === 0 ? (
          <div style={emptyStyle}>
            {coordLoading ? "Loading reports..." : copy.empty}
          </div>
        ) : (
          <table style={tableStyle}>
            <thead>
              <tr>
                <th style={thStyle}>Time</th>
                <th style={thStyle}>Device</th>
                <th style={thStyle}>Account</th>
                <th style={{ ...thStyle, textAlign: "right" }}>Push</th>
                <th style={{ ...thStyle, textAlign: "right" }}>Pull</th>
                <th style={{ ...thStyle, textAlign: "right" }}>Fail</th>
                <th style={{ ...thStyle, textAlign: "right" }}>Source</th>
              </tr>
            </thead>
            <tbody>
              {sortedReports.map((row) => {
                const isExpanded = expandedId === row.id;
                const hasFail = row.failed > 0;
                return (
                  <ReportRow
                    key={row.id}
                    row={row}
                    isExpanded={isExpanded}
                    hasFail={hasFail}
                    onToggle={() =>
                      setExpandedId(isExpanded ? null : row.id)
                    }
                  />
                );
              })}
            </tbody>
          </table>
        )}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Sub-components
// ---------------------------------------------------------------------------

function SummaryTile({
  label,
  value,
  color,
}: {
  label: string;
  value: number;
  color?: string;
}) {
  return (
    <div style={tileStyle}>
      <p style={tileLabelStyle}>{label}</p>
      <p style={{ ...tileValueStyle, ...(color ? { color } : {}) }}>
        {value}
      </p>
    </div>
  );
}

function ReportRow({
  row,
  isExpanded,
  hasFail,
  onToggle,
}: {
  row: FederationReportRow;
  isExpanded: boolean;
  hasFail: boolean;
  onToggle: () => void;
}) {
  const time = new Date(row.reported_at);
  const timeStr = time.toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
  const dateStr = time.toLocaleDateString([], {
    month: "short",
    day: "numeric",
  });

  const deviceShort = row.device_id
    ? row.device_id.slice(0, 8)
    : "local";

  return (
    <>
      <tr
        style={{
          ...trStyle,
          cursor: "pointer",
          ...(hasFail ? trFailStyle : {}),
        }}
        onClick={onToggle}
        title="Click to expand"
      >
        <td style={tdStyle}>
          <span style={dateLabelStyle}>{dateStr}</span>{" "}
          <span>{timeStr}</span>
        </td>
        <td style={tdStyle}>
          <code style={monoStyle}>{deviceShort}</code>
        </td>
        <td style={tdStyle}>
          <code style={monoStyle}>{row.account_name}</code>
        </td>
        <td style={{ ...tdStyle, textAlign: "right" }}>{row.pushed}</td>
        <td style={{ ...tdStyle, textAlign: "right" }}>{row.pulled}</td>
        <td
          style={{
            ...tdStyle,
            textAlign: "right",
            ...(hasFail ? { color: "#ef4444", fontWeight: 600 } : {}),
          }}
        >
          {row.failed}
        </td>
        <td style={{ ...tdStyle, textAlign: "right" }}>
          <span
            style={{
              ...sourceBadgeStyle,
              ...(row.source === "live" ? sourceLiveStyle : sourceCoordStyle),
            }}
          >
            {row.source}
          </span>
        </td>
      </tr>
      {isExpanded ? (
        <tr>
          <td colSpan={7} style={expandedCellStyle}>
            <div style={detailGridStyle}>
              <DetailField label="Session ID" value={row.session_id} />
              <DetailField label="Device ID" value={row.device_id ?? "n/a"} />
              <DetailField
                label="Unchanged"
                value={String(row.unchanged ?? 0)}
              />
              {row.failed_names && row.failed_names.length > 0 ? (
                <div style={{ gridColumn: "1 / -1" }}>
                  <span style={detailLabelStyle}>Failed names:</span>
                  <ul style={failedListStyle}>
                    {row.failed_names.map((name, i) => (
                      <li key={i} style={failedItemStyle}>
                        <code style={monoStyle}>{name}</code>
                      </li>
                    ))}
                  </ul>
                </div>
              ) : null}
              {row.metadata &&
              Object.keys(row.metadata).length > 0 ? (
                <div style={{ gridColumn: "1 / -1" }}>
                  <span style={detailLabelStyle}>Metadata:</span>
                  <pre style={metadataPreStyle}>
                    {JSON.stringify(row.metadata, null, 2)}
                  </pre>
                </div>
              ) : null}
            </div>
          </td>
        </tr>
      ) : null}
    </>
  );
}

function DetailField({
  label,
  value,
}: {
  label: string;
  value: string;
}) {
  return (
    <div>
      <span style={detailLabelStyle}>{label}</span>
      <code style={detailValueStyle}>{value}</code>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Styles (inline, matching MemoryFederationBanner conventions)
// ---------------------------------------------------------------------------

const pageStyle: React.CSSProperties = {
  height: "100%",
  display: "flex",
  flexDirection: "column",
  padding: 24,
  overflow: "auto",
  color: "var(--text-primary, #e4e4e7)",
  background: "var(--bg-primary, #1a1b26)",
};

const headerStyle: React.CSSProperties = {
  display: "flex",
  alignItems: "center",
  justifyContent: "space-between",
  marginBottom: 20,
  flexWrap: "wrap",
  gap: 12,
};

const titleStyle: React.CSSProperties = {
  margin: 0,
  fontSize: "1.25rem",
  fontWeight: 600,
  color: "var(--text-primary, #e4e4e7)",
};

const subtitleStyle: React.CSSProperties = {
  margin: "4px 0 0 0",
  fontSize: "0.8125rem",
  color: "var(--text-secondary, #a1a1aa)",
};

const headerActionsStyle: React.CSSProperties = {
  display: "flex",
  alignItems: "center",
  gap: 12,
};

const timeRangeSelectorStyle: React.CSSProperties = {
  display: "flex",
  gap: 2,
  background: "var(--bg-tertiary, #242837)",
  borderRadius: 6,
  padding: 2,
};

const timeRangeBtnStyle: React.CSSProperties = {
  background: "transparent",
  color: "var(--text-secondary, #a1a1aa)",
  border: "none",
  borderRadius: 4,
  padding: "4px 10px",
  fontSize: "0.75rem",
  cursor: "pointer",
};

const timeRangeBtnActiveStyle: React.CSSProperties = {
  ...timeRangeBtnStyle,
  background: "var(--accent, #6366f1)",
  color: "#fff",
};

const refreshBtnStyle: React.CSSProperties = {
  background: "var(--bg-tertiary, #242837)",
  color: "var(--text-secondary, #a1a1aa)",
  border: "1px solid var(--border, #3f3f46)",
  borderRadius: 6,
  padding: "4px 12px",
  fontSize: "0.75rem",
  cursor: "pointer",
};

const noticeStyle: React.CSSProperties = {
  marginBottom: 16,
  padding: "8px 12px",
  background: "rgba(251, 191, 36, 0.1)",
  border: "1px solid rgba(251, 191, 36, 0.3)",
  borderRadius: 6,
  color: "#fbbf24",
  fontSize: "0.8125rem",
};

const tilesRowStyle: React.CSSProperties = {
  display: "grid",
  gridTemplateColumns: "repeat(4, 1fr)",
  gap: 12,
  marginBottom: 20,
};

const tileStyle: React.CSSProperties = {
  background: "var(--bg-tertiary, #242837)",
  border: "1px solid var(--border, #3f3f46)",
  borderRadius: 8,
  padding: "14px 16px",
};

const tileLabelStyle: React.CSSProperties = {
  margin: 0,
  fontSize: "0.75rem",
  color: "var(--text-secondary, #a1a1aa)",
  textTransform: "uppercase",
  letterSpacing: "0.05em",
};

const tileValueStyle: React.CSSProperties = {
  margin: 0,
  marginTop: 4,
  fontSize: "1.5rem",
  fontWeight: 700,
  color: "var(--text-primary, #e4e4e7)",
};

const tableContainerStyle: React.CSSProperties = {
  flex: 1,
  overflow: "auto",
  background: "var(--bg-secondary, #1e2030)",
  border: "1px solid var(--border, #3f3f46)",
  borderRadius: 8,
};

const emptyStyle: React.CSSProperties = {
  padding: 32,
  textAlign: "center",
  color: "var(--text-secondary, #a1a1aa)",
  fontSize: "0.875rem",
};

const tableStyle: React.CSSProperties = {
  width: "100%",
  borderCollapse: "collapse",
  fontSize: "0.8125rem",
};

const thStyle: React.CSSProperties = {
  textAlign: "left",
  padding: "10px 12px",
  borderBottom: "1px solid var(--border, #3f3f46)",
  color: "var(--text-secondary, #a1a1aa)",
  fontSize: "0.75rem",
  fontWeight: 500,
  textTransform: "uppercase",
  letterSpacing: "0.05em",
  position: "sticky",
  top: 0,
  background: "var(--bg-secondary, #1e2030)",
  zIndex: 1,
};

const trStyle: React.CSSProperties = {
  borderBottom: "1px solid var(--border, #3f3f46)",
  transition: "background 0.1s",
};

const trFailStyle: React.CSSProperties = {
  background: "rgba(239, 68, 68, 0.06)",
};

const tdStyle: React.CSSProperties = {
  padding: "8px 12px",
  verticalAlign: "middle",
};

const dateLabelStyle: React.CSSProperties = {
  color: "var(--text-secondary, #a1a1aa)",
  fontSize: "0.7rem",
};

const monoStyle: React.CSSProperties = {
  background: "rgba(255, 255, 255, 0.08)",
  padding: "1px 4px",
  borderRadius: 3,
  fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
  fontSize: "0.75rem",
};

const sourceBadgeStyle: React.CSSProperties = {
  display: "inline-block",
  padding: "1px 6px",
  borderRadius: 3,
  fontSize: "0.6875rem",
  fontWeight: 500,
  textTransform: "uppercase",
};

const sourceLiveStyle: React.CSSProperties = {
  background: "rgba(34, 197, 94, 0.15)",
  color: "#22c55e",
};

const sourceCoordStyle: React.CSSProperties = {
  background: "rgba(99, 102, 241, 0.15)",
  color: "#6366f1",
};

const expandedCellStyle: React.CSSProperties = {
  padding: "12px 16px",
  background: "var(--bg-tertiary, #242837)",
  borderBottom: "1px solid var(--border, #3f3f46)",
};

const detailGridStyle: React.CSSProperties = {
  display: "grid",
  gridTemplateColumns: "repeat(3, 1fr)",
  gap: "8px 24px",
};

const detailLabelStyle: React.CSSProperties = {
  display: "block",
  fontSize: "0.6875rem",
  color: "var(--text-secondary, #a1a1aa)",
  marginBottom: 2,
  textTransform: "uppercase",
  letterSpacing: "0.04em",
};

const detailValueStyle: React.CSSProperties = {
  display: "block",
  fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
  fontSize: "0.75rem",
  wordBreak: "break-all",
};

const failedListStyle: React.CSSProperties = {
  margin: "4px 0 0 0",
  padding: 0,
  listStyle: "none",
  display: "flex",
  flexWrap: "wrap",
  gap: 4,
};

const failedItemStyle: React.CSSProperties = {
  background: "rgba(239, 68, 68, 0.1)",
  border: "1px solid rgba(239, 68, 68, 0.25)",
  borderRadius: 4,
  padding: "2px 6px",
};

const metadataPreStyle: React.CSSProperties = {
  margin: "4px 0 0 0",
  padding: 8,
  background: "rgba(0, 0, 0, 0.25)",
  borderRadius: 4,
  fontSize: "0.7rem",
  fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
  overflow: "auto",
  maxHeight: 120,
};
