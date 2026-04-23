/**
 * PromptEvolutionPanel — History of GEPA prompt optimization runs.
 *
 * Shows per-domain optimization history with before/after scores,
 * detail drill-down with prompt diffs, auto-refresh, and domain filtering.
 */

import { useState, useEffect, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

interface GepaRun {
  id: string;
  domain: string;
  old_score: number | null;
  new_score: number | null;
  improvement: number | null;
  status: string;
  created_at: string;
}

interface GepaRunDetail {
  id: string;
  domain: string;
  old_instructions: string;
  new_instructions: string;
  old_score: number;
  new_score: number;
  improvement: number;
  status: string;
  created_at: string;
}

const AUTO_REFRESH_MS = 60_000;

export function PromptEvolutionPanel() {
  const [runs, setRuns] = useState<GepaRun[]>([]);
  const [loading, setLoading] = useState(true);
  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [detail, setDetail] = useState<GepaRunDetail | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);
  const [domainFilter, setDomainFilter] = useState<string>("all");
  const intervalRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const loadRuns = useCallback(async () => {
    setLoading(true);
    try {
      const result = await invoke<GepaRun[]>("get_gepa_runs", { limit: 50 });
      setRuns(result);
    } catch (err) {
      console.error("Failed to load GEPA runs:", err);
    } finally {
      setLoading(false);
    }
  }, []);

  // Initial load + auto-refresh interval. Async IIFE to avoid invoking a
  // setState-bearing callback synchronously in the effect body
  // (set-state-in-effect).
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      if (cancelled) return;
      await loadRuns();
    })();
    intervalRef.current = setInterval(loadRuns, AUTO_REFRESH_MS);
    return () => {
      cancelled = true;
      if (intervalRef.current) clearInterval(intervalRef.current);
    };
  }, [loadRuns]);

  // Listen for learning-update events
  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    listen("learning-update", () => {
      loadRuns();
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      unlisten?.();
    };
  }, [loadRuns]);

  // Load detail when a row is expanded
  const toggleDetail = useCallback(
    async (id: string) => {
      if (expandedId === id) {
        setExpandedId(null);
        setDetail(null);
        return;
      }
      setExpandedId(id);
      setDetail(null);
      setDetailLoading(true);
      try {
        const result = await invoke<GepaRunDetail>("get_gepa_run_detail", { id });
        setDetail(result);
      } catch (err) {
        console.error("Failed to load GEPA run detail:", err);
      } finally {
        setDetailLoading(false);
      }
    },
    [expandedId],
  );

  // Compute domains for filter dropdown and summary cards
  const allDomains = [...new Set(runs.map((r) => r.domain))].sort();

  const filteredRuns =
    domainFilter === "all" ? runs : runs.filter((r) => r.domain === domainFilter);

  const domainStats = allDomains.map((domain) => {
    const domainRuns = runs.filter((r) => r.domain === domain);
    const totalImprovement = domainRuns
      .filter((r) => r.improvement !== null && r.improvement > 0)
      .reduce((sum, r) => sum + (r.improvement || 0), 0);
    const successfulRuns = domainRuns.filter(
      (r) => r.status === "completed" && (r.improvement || 0) > 0,
    ).length;
    return { domain, runs: domainRuns, totalImprovement, successfulRuns };
  });

  return (
    <div style={{ padding: "16px" }}>
      {/* Header */}
      <div
        style={{
          display: "flex",
          justifyContent: "space-between",
          alignItems: "center",
          marginBottom: "16px",
        }}
      >
        <h3 style={{ margin: 0 }}>Prompt Evolution (GEPA)</h3>
        <div style={{ display: "flex", gap: "8px", alignItems: "center" }}>
          {/* Domain filter */}
          <select
            value={domainFilter}
            onChange={(e) => setDomainFilter(e.target.value)}
            style={{
              padding: "4px 8px",
              borderRadius: "4px",
              background: "#1f2937",
              color: "#e5e7eb",
              border: "1px solid #374151",
              fontSize: "13px",
              cursor: "pointer",
            }}
          >
            <option value="all">All domains</option>
            {allDomains.map((d) => (
              <option key={d} value={d}>
                {d.charAt(0).toUpperCase() + d.slice(1)}
              </option>
            ))}
          </select>
          <button
            onClick={loadRuns}
            style={{
              padding: "4px 12px",
              borderRadius: "4px",
              background: "#3b82f6",
              color: "white",
              border: "none",
              cursor: "pointer",
            }}
          >
            Refresh
          </button>
        </div>
      </div>

      {loading ? (
        <div style={{ textAlign: "center", padding: "32px", color: "#9ca3af" }}>Loading...</div>
      ) : runs.length === 0 ? (
        <div style={{ textAlign: "center", padding: "32px", color: "#9ca3af" }}>
          No GEPA optimization runs yet. Runs are triggered automatically after enough workflow
          executions.
        </div>
      ) : (
        <>
          {/* Domain Summary Cards */}
          <div
            style={{
              display: "grid",
              gridTemplateColumns: "repeat(auto-fill, minmax(200px, 1fr))",
              gap: "12px",
              marginBottom: "24px",
            }}
          >
            {domainStats.map((ds) => (
              <div
                key={ds.domain}
                onClick={() => setDomainFilter(domainFilter === ds.domain ? "all" : ds.domain)}
                style={{
                  background: "#111827",
                  borderRadius: "8px",
                  padding: "12px",
                  border: domainFilter === ds.domain ? "1px solid #3b82f6" : "1px solid #1f2937",
                  cursor: "pointer",
                  transition: "border-color 0.15s",
                }}
              >
                <div
                  style={{
                    fontSize: "14px",
                    fontWeight: "bold",
                    color: "#e5e7eb",
                    textTransform: "capitalize",
                  }}
                >
                  {ds.domain}
                </div>
                <div style={{ fontSize: "11px", color: "#9ca3af", marginTop: "4px" }}>
                  {ds.runs.length} runs, {ds.successfulRuns} improved
                </div>
                <div
                  style={{
                    fontSize: "18px",
                    fontWeight: "bold",
                    color: ds.totalImprovement > 0 ? "#22c55e" : "#9ca3af",
                    marginTop: "4px",
                  }}
                >
                  +{(ds.totalImprovement * 100).toFixed(1)}%
                </div>
              </div>
            ))}
          </div>

          {/* Run History Table */}
          <table style={{ width: "100%", borderCollapse: "collapse", fontSize: "13px" }}>
            <thead>
              <tr style={{ borderBottom: "1px solid #374151" }}>
                <th style={{ textAlign: "left", padding: "8px", color: "#9ca3af" }}>Date</th>
                <th style={{ textAlign: "left", padding: "8px", color: "#9ca3af" }}>Domain</th>
                <th style={{ textAlign: "right", padding: "8px", color: "#9ca3af" }}>Old Score</th>
                <th style={{ textAlign: "right", padding: "8px", color: "#9ca3af" }}>New Score</th>
                <th style={{ textAlign: "right", padding: "8px", color: "#9ca3af" }}>
                  Improvement
                </th>
                <th style={{ textAlign: "center", padding: "8px", color: "#9ca3af" }}>Status</th>
              </tr>
            </thead>
            <tbody>
              {filteredRuns.map((run) => (
                <RunRow
                  key={run.id}
                  run={run}
                  isExpanded={expandedId === run.id}
                  detail={expandedId === run.id ? detail : null}
                  detailLoading={expandedId === run.id && detailLoading}
                  onToggle={() => toggleDetail(run.id)}
                />
              ))}
            </tbody>
          </table>
        </>
      )}
    </div>
  );
}

/* ------------------------------------------------------------------ */
/*  Run Row + expandable detail                                       */
/* ------------------------------------------------------------------ */

function RunRow({
  run,
  isExpanded,
  detail,
  detailLoading,
  onToggle,
}: {
  run: GepaRun;
  isExpanded: boolean;
  detail: GepaRunDetail | null;
  detailLoading: boolean;
  onToggle: () => void;
}) {
  return (
    <>
      <tr
        onClick={onToggle}
        style={{
          borderBottom: isExpanded ? "none" : "1px solid #1f2937",
          cursor: "pointer",
          background: isExpanded ? "#0d1117" : "transparent",
          transition: "background 0.1s",
        }}
        onMouseEnter={(e) => {
          if (!isExpanded) (e.currentTarget as HTMLTableRowElement).style.background = "#111827";
        }}
        onMouseLeave={(e) => {
          if (!isExpanded)
            (e.currentTarget as HTMLTableRowElement).style.background = "transparent";
        }}
      >
        <td style={{ padding: "6px 8px", color: "#6b7280", fontSize: "12px" }}>
          <span
            style={{
              marginRight: "6px",
              display: "inline-block",
              transform: isExpanded ? "rotate(90deg)" : "rotate(0deg)",
              transition: "transform 0.15s",
            }}
          >
            &#9654;
          </span>
          {new Date(run.created_at).toLocaleDateString()}
        </td>
        <td
          style={{
            padding: "6px 8px",
            color: "#e5e7eb",
            textTransform: "capitalize",
          }}
        >
          {run.domain}
        </td>
        <td style={{ padding: "6px 8px", textAlign: "right", color: "#9ca3af" }}>
          {run.old_score !== null ? (run.old_score * 100).toFixed(1) + "%" : "-"}
        </td>
        <td style={{ padding: "6px 8px", textAlign: "right", color: "#e5e7eb" }}>
          {run.new_score !== null ? (run.new_score * 100).toFixed(1) + "%" : "-"}
        </td>
        <td
          style={{
            padding: "6px 8px",
            textAlign: "right",
            color: (run.improvement || 0) > 0 ? "#22c55e" : "#ef4444",
          }}
        >
          {run.improvement !== null
            ? (run.improvement > 0 ? "+" : "") + (run.improvement * 100).toFixed(1) + "%"
            : "-"}
        </td>
        <td style={{ padding: "6px 8px", textAlign: "center" }}>
          <StatusBadge status={run.status} />
        </td>
      </tr>

      {/* Expanded detail row */}
      {isExpanded && (
        <tr style={{ borderBottom: "1px solid #1f2937" }}>
          <td colSpan={6} style={{ padding: 0 }}>
            {detailLoading ? (
              <div
                style={{
                  padding: "24px",
                  textAlign: "center",
                  color: "#9ca3af",
                  background: "#0d1117",
                }}
              >
                Loading detail...
              </div>
            ) : detail ? (
              <RunDetail detail={detail} />
            ) : (
              <div
                style={{
                  padding: "24px",
                  textAlign: "center",
                  color: "#6b7280",
                  background: "#0d1117",
                }}
              >
                Failed to load detail.
              </div>
            )}
          </td>
        </tr>
      )}
    </>
  );
}

/* ------------------------------------------------------------------ */
/*  Detail panel with before/after diff                               */
/* ------------------------------------------------------------------ */

function RunDetail({ detail }: { detail: GepaRunDetail }) {
  const improvementPct = (detail.improvement * 100).toFixed(1);
  const isPositive = detail.improvement > 0;

  return (
    <div style={{ padding: "16px 24px 20px", background: "#0d1117" }}>
      {/* Score delta indicator */}
      <div
        style={{
          display: "flex",
          alignItems: "center",
          gap: "16px",
          marginBottom: "16px",
          padding: "12px 16px",
          background: "#111827",
          borderRadius: "8px",
          border: "1px solid #1f2937",
        }}
      >
        <ScoreBox label="Before" score={detail.old_score} color="#ef4444" />
        <div
          style={{
            fontSize: "20px",
            color: "#6b7280",
            userSelect: "none",
          }}
        >
          &rarr;
        </div>
        <ScoreBox label="After" score={detail.new_score} color="#22c55e" />
        <div
          style={{
            marginLeft: "auto",
            textAlign: "center",
          }}
        >
          <div style={{ fontSize: "11px", color: "#9ca3af", marginBottom: "2px" }}>Delta</div>
          <div
            style={{
              fontSize: "22px",
              fontWeight: "bold",
              color: isPositive ? "#22c55e" : "#ef4444",
            }}
          >
            {isPositive ? "+" : ""}
            {improvementPct}%
          </div>
        </div>
      </div>

      {/* Before / After instruction boxes */}
      <div
        style={{
          display: "grid",
          gridTemplateColumns: "1fr 1fr",
          gap: "12px",
        }}
      >
        {/* Before */}
        <div
          style={{
            borderRadius: "8px",
            border: "1px solid #7f1d1d",
            overflow: "hidden",
          }}
        >
          <div
            style={{
              padding: "6px 12px",
              background: "#450a0a",
              color: "#fca5a5",
              fontSize: "12px",
              fontWeight: "bold",
            }}
          >
            Before
          </div>
          <div
            style={{
              padding: "12px",
              background: "#1a0a0a",
              color: "#d1d5db",
              fontSize: "13px",
              lineHeight: "1.5",
              whiteSpace: "pre-wrap",
              wordBreak: "break-word",
              maxHeight: "300px",
              overflowY: "auto",
            }}
          >
            {detail.old_instructions}
          </div>
        </div>

        {/* After */}
        <div
          style={{
            borderRadius: "8px",
            border: "1px solid #14532d",
            overflow: "hidden",
          }}
        >
          <div
            style={{
              padding: "6px 12px",
              background: "#052e16",
              color: "#86efac",
              fontSize: "12px",
              fontWeight: "bold",
            }}
          >
            After
          </div>
          <div
            style={{
              padding: "12px",
              background: "#0a1a0a",
              color: "#d1d5db",
              fontSize: "13px",
              lineHeight: "1.5",
              whiteSpace: "pre-wrap",
              wordBreak: "break-word",
              maxHeight: "300px",
              overflowY: "auto",
            }}
          >
            {detail.new_instructions}
          </div>
        </div>
      </div>

      {/* Metadata footer */}
      <div
        style={{
          marginTop: "12px",
          fontSize: "11px",
          color: "#6b7280",
          display: "flex",
          gap: "16px",
        }}
      >
        <span>
          Domain:{" "}
          <strong style={{ color: "#9ca3af", textTransform: "capitalize" }}>{detail.domain}</strong>
        </span>
        <span>
          Status: <StatusBadge status={detail.status} />
        </span>
        <span>Created: {new Date(detail.created_at).toLocaleString()}</span>
      </div>
    </div>
  );
}

/* ------------------------------------------------------------------ */
/*  Small helper components                                           */
/* ------------------------------------------------------------------ */

function ScoreBox({ label, score, color }: { label: string; score: number; color: string }) {
  return (
    <div style={{ textAlign: "center" }}>
      <div style={{ fontSize: "11px", color: "#9ca3af", marginBottom: "2px" }}>{label}</div>
      <div style={{ fontSize: "18px", fontWeight: "bold", color }}>{(score * 100).toFixed(1)}%</div>
    </div>
  );
}

function StatusBadge({ status }: { status: string }) {
  const colors: Record<string, { bg: string; text: string }> = {
    completed: { bg: "#1e3a2f", text: "#34d399" },
    pending: { bg: "#1e3a5f", text: "#60a5fa" },
    failed: { bg: "#3b1e1e", text: "#f87171" },
    canary: { bg: "#3b2f1e", text: "#fbbf24" },
  };
  const c = colors[status] || colors.pending;

  return (
    <span
      style={{
        background: c.bg,
        color: c.text,
        padding: "2px 8px",
        borderRadius: "4px",
        fontSize: "11px",
      }}
    >
      {status}
    </span>
  );
}

export default PromptEvolutionPanel;
