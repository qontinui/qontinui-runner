/**
 * PromptEvolutionPanel — History of GEPA prompt optimization runs.
 *
 * Shows per-domain optimization history with before/after scores.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";

interface GepaRun {
  id: string;
  domain: string;
  old_score: number | null;
  new_score: number | null;
  improvement: number | null;
  status: string;
  created_at: string;
}

export function PromptEvolutionPanel() {
  const [runs, setRuns] = useState<GepaRun[]>([]);
  const [loading, setLoading] = useState(true);

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

  useEffect(() => {
    loadRuns();
  }, [loadRuns]);

  // Group by domain
  const domains = [...new Set(runs.map((r) => r.domain))];
  const domainStats = domains.map((domain) => {
    const domainRuns = runs.filter((r) => r.domain === domain);
    const totalImprovement = domainRuns
      .filter((r) => r.improvement !== null && r.improvement > 0)
      .reduce((sum, r) => sum + (r.improvement || 0), 0);
    const successfulRuns = domainRuns.filter((r) => r.status === "completed" && (r.improvement || 0) > 0).length;
    return { domain, runs: domainRuns, totalImprovement, successfulRuns };
  });

  return (
    <div style={{ padding: "16px" }}>
      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: "16px" }}>
        <h3 style={{ margin: 0 }}>Prompt Evolution (GEPA)</h3>
        <button onClick={loadRuns} style={{ padding: "4px 12px", borderRadius: "4px", background: "#3b82f6", color: "white", border: "none", cursor: "pointer" }}>
          Refresh
        </button>
      </div>

      {loading ? (
        <div style={{ textAlign: "center", padding: "32px", color: "#9ca3af" }}>Loading...</div>
      ) : runs.length === 0 ? (
        <div style={{ textAlign: "center", padding: "32px", color: "#9ca3af" }}>
          No GEPA optimization runs yet. Runs are triggered automatically after enough workflow executions.
        </div>
      ) : (
        <>
          {/* Domain Summary Cards */}
          <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(200px, 1fr))", gap: "12px", marginBottom: "24px" }}>
            {domainStats.map((ds) => (
              <div key={ds.domain} style={{ background: "#111827", borderRadius: "8px", padding: "12px", border: "1px solid #1f2937" }}>
                <div style={{ fontSize: "14px", fontWeight: "bold", color: "#e5e7eb", textTransform: "capitalize" }}>{ds.domain}</div>
                <div style={{ fontSize: "11px", color: "#9ca3af", marginTop: "4px" }}>
                  {ds.runs.length} runs, {ds.successfulRuns} improved
                </div>
                <div style={{ fontSize: "18px", fontWeight: "bold", color: ds.totalImprovement > 0 ? "#22c55e" : "#9ca3af", marginTop: "4px" }}>
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
                <th style={{ textAlign: "right", padding: "8px", color: "#9ca3af" }}>Improvement</th>
                <th style={{ textAlign: "center", padding: "8px", color: "#9ca3af" }}>Status</th>
              </tr>
            </thead>
            <tbody>
              {runs.map((run) => (
                <tr key={run.id} style={{ borderBottom: "1px solid #1f2937" }}>
                  <td style={{ padding: "6px 8px", color: "#6b7280", fontSize: "12px" }}>
                    {new Date(run.created_at).toLocaleDateString()}
                  </td>
                  <td style={{ padding: "6px 8px", color: "#e5e7eb", textTransform: "capitalize" }}>{run.domain}</td>
                  <td style={{ padding: "6px 8px", textAlign: "right", color: "#9ca3af" }}>
                    {run.old_score !== null ? (run.old_score * 100).toFixed(1) + "%" : "-"}
                  </td>
                  <td style={{ padding: "6px 8px", textAlign: "right", color: "#e5e7eb" }}>
                    {run.new_score !== null ? (run.new_score * 100).toFixed(1) + "%" : "-"}
                  </td>
                  <td style={{ padding: "6px 8px", textAlign: "right", color: (run.improvement || 0) > 0 ? "#22c55e" : "#ef4444" }}>
                    {run.improvement !== null ? (run.improvement > 0 ? "+" : "") + (run.improvement * 100).toFixed(1) + "%" : "-"}
                  </td>
                  <td style={{ padding: "6px 8px", textAlign: "center" }}>
                    <StatusBadge status={run.status} />
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </>
      )}
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
    <span style={{ background: c.bg, color: c.text, padding: "2px 8px", borderRadius: "4px", fontSize: "11px" }}>
      {status}
    </span>
  );
}

export default PromptEvolutionPanel;
