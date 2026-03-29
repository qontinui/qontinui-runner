/**
 * LearningDashboard — Overview of adaptive learning system statistics.
 *
 * Shows playbook size, curated examples, template tracking, and GEPA runs.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";

interface AdaptiveLearningStats {
  playbook_entries: number;
  active_lessons: number;
  staged_lessons: number;
  retired_lessons: number;
  curated_examples: number;
  templates_tracked: number;
  gepa_runs: number;
  avg_lesson_helpfulness: number;
}

interface TemplatePerformance {
  template_id: string;
  template_name: string;
  source: string;
  success_count: number;
  failure_count: number;
  confidence: number;
  avg_quality: number;
  last_used_at: string | null;
}

export function LearningDashboard() {
  const [stats, setStats] = useState<AdaptiveLearningStats | null>(null);
  const [templates, setTemplates] = useState<TemplatePerformance[]>([]);
  const [loading, setLoading] = useState(true);

  const loadData = useCallback(async () => {
    setLoading(true);
    try {
      const [statsResult, templatesResult] = await Promise.all([
        invoke<AdaptiveLearningStats>("get_adaptive_learning_stats"),
        invoke<TemplatePerformance[]>("get_template_performance"),
      ]);
      setStats(statsResult);
      setTemplates(templatesResult);
    } catch (err) {
      console.error("Failed to load learning dashboard:", err);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    loadData();
  }, [loadData]);

  if (loading) {
    return <div style={{ padding: "16px", color: "#9ca3af" }}>Loading dashboard...</div>;
  }

  if (!stats) {
    return (
      <div style={{ padding: "16px", textAlign: "center", color: "#9ca3af" }}>
        Could not load adaptive learning data. Tables may not exist yet — run a workflow first.
        <br />
        <button onClick={loadData} style={{ marginTop: "8px", padding: "4px 12px", borderRadius: "4px", background: "#3b82f6", color: "white", border: "none", cursor: "pointer" }}>
          Retry
        </button>
      </div>
    );
  }

  return (
    <div style={{ padding: "16px" }}>
      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: "16px" }}>
        <h3 style={{ margin: 0 }}>Adaptive Learning</h3>
        <button onClick={loadData} style={{ padding: "4px 12px", borderRadius: "4px", background: "#3b82f6", color: "white", border: "none", cursor: "pointer" }}>
          Refresh
        </button>
      </div>

      {/* Stats Cards */}
      <div style={{ display: "grid", gridTemplateColumns: "repeat(4, 1fr)", gap: "12px", marginBottom: "24px" }}>
        <StatCard label="Playbook Lessons" value={stats.playbook_entries} detail={`${stats.active_lessons} active, ${stats.staged_lessons} staged`} />
        <StatCard label="Curated Examples" value={stats.curated_examples} detail="Few-shot library" />
        <StatCard label="Templates Tracked" value={stats.templates_tracked} detail="Performance monitored" />
        <StatCard label="GEPA Optimizations" value={stats.gepa_runs} detail={`Avg helpfulness: ${(stats.avg_lesson_helpfulness * 100).toFixed(0)}%`} />
      </div>

      {/* Template Performance Table */}
      {templates.length > 0 && (
        <div>
          <h4 style={{ margin: "0 0 8px 0", fontSize: "14px", color: "#9ca3af" }}>Template Performance</h4>
          <table style={{ width: "100%", borderCollapse: "collapse", fontSize: "13px" }}>
            <thead>
              <tr style={{ borderBottom: "1px solid #374151" }}>
                <th style={{ textAlign: "left", padding: "8px", color: "#9ca3af" }}>Template</th>
                <th style={{ textAlign: "left", padding: "8px", color: "#9ca3af" }}>Source</th>
                <th style={{ textAlign: "right", padding: "8px", color: "#9ca3af" }}>Success</th>
                <th style={{ textAlign: "right", padding: "8px", color: "#9ca3af" }}>Failure</th>
                <th style={{ textAlign: "right", padding: "8px", color: "#9ca3af" }}>Confidence</th>
                <th style={{ textAlign: "right", padding: "8px", color: "#9ca3af" }}>Avg Quality</th>
              </tr>
            </thead>
            <tbody>
              {templates.slice(0, 20).map((t) => (
                <tr key={t.template_id} style={{ borderBottom: "1px solid #1f2937" }}>
                  <td style={{ padding: "6px 8px", color: "#e5e7eb" }}>{t.template_name}</td>
                  <td style={{ padding: "6px 8px" }}>
                    <SourceBadge source={t.source} />
                  </td>
                  <td style={{ padding: "6px 8px", textAlign: "right", color: "#22c55e" }}>{t.success_count}</td>
                  <td style={{ padding: "6px 8px", textAlign: "right", color: "#ef4444" }}>{t.failure_count}</td>
                  <td style={{ padding: "6px 8px", textAlign: "right", color: t.confidence > 0.7 ? "#22c55e" : t.confidence > 0.4 ? "#f59e0b" : "#ef4444" }}>
                    {(t.confidence * 100).toFixed(0)}%
                  </td>
                  <td style={{ padding: "6px 8px", textAlign: "right", color: "#9ca3af" }}>
                    {t.avg_quality.toFixed(2)}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}

function StatCard({ label, value, detail }: { label: string; value: number; detail: string }) {
  return (
    <div style={{ background: "#111827", borderRadius: "8px", padding: "16px", border: "1px solid #1f2937" }}>
      <div style={{ fontSize: "24px", fontWeight: "bold", color: "#e5e7eb" }}>{value}</div>
      <div style={{ fontSize: "13px", color: "#9ca3af", marginTop: "4px" }}>{label}</div>
      <div style={{ fontSize: "11px", color: "#6b7280", marginTop: "2px" }}>{detail}</div>
    </div>
  );
}

function SourceBadge({ source }: { source: string }) {
  const colors: Record<string, { bg: string; text: string }> = {
    manual: { bg: "#1e3a5f", text: "#60a5fa" },
    extracted: { bg: "#1e3a2f", text: "#34d399" },
    promoted: { bg: "#3b2f1e", text: "#fbbf24" },
    demoted: { bg: "#3b1e1e", text: "#f87171" },
    retired: { bg: "#1f2937", text: "#6b7280" },
  };
  const c = colors[source] || colors.manual;

  return (
    <span style={{ background: c.bg, color: c.text, padding: "2px 8px", borderRadius: "4px", fontSize: "11px" }}>
      {source}
    </span>
  );
}

export default LearningDashboard;
