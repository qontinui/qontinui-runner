/**
 * LearningDashboard — Overview of adaptive learning system statistics.
 *
 * Shows playbook size, curated examples, template tracking, GEPA runs,
 * trend charts (lesson growth, domain distribution, helpfulness), and
 * auto-refreshes on learning-update events or every 60 seconds.
 */

import { useState, useEffect, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

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

interface LessonTrendPoint {
  day: string;
  count: number;
  severity: string;
}

interface ExampleTrendPoint {
  day: string;
  count: number;
  domain: string;
}

interface DomainDistribution {
  domain: string;
  count: number;
}

interface HelpfulnessTrendPoint {
  day: string;
  avg_helpfulness: number;
  total_applied: number;
}

interface LearningTrends {
  lesson_trends: LessonTrendPoint[];
  example_trends: ExampleTrendPoint[];
  domain_distribution: DomainDistribution[];
  helpfulness_trends: HelpfulnessTrendPoint[];
}

// ---------------------------------------------------------------------------
// Color helpers
// ---------------------------------------------------------------------------

const SEVERITY_COLORS: Record<string, string> = {
  critical: "#ef4444",
  high: "#f97316",
  medium: "#f59e0b",
  low: "#3b82f6",
  info: "#6b7280",
};

const DOMAIN_COLORS = [
  "#3b82f6",
  "#22c55e",
  "#f59e0b",
  "#ef4444",
  "#a855f7",
  "#06b6d4",
  "#ec4899",
  "#84cc16",
  "#f97316",
  "#6366f1",
];

function domainColor(index: number): string {
  return DOMAIN_COLORS[index % DOMAIN_COLORS.length];
}

function severityColor(severity: string): string {
  return SEVERITY_COLORS[severity] || "#6b7280";
}

// ---------------------------------------------------------------------------
// Chart components
// ---------------------------------------------------------------------------

function SimpleBarChart({
  data,
  width,
  height,
}: {
  data: { label: string; value: number; color: string }[];
  width: number;
  height: number;
}) {
  if (data.length === 0) {
    return (
      <svg width={width} height={height}>
        <text x={width / 2} y={height / 2} fill="#6b7280" fontSize="12" textAnchor="middle">
          No data
        </text>
      </svg>
    );
  }

  const maxVal = Math.max(...data.map((d) => d.value), 1);
  const barPadding = 2;
  const labelHeight = 16;
  const chartHeight = height - labelHeight;
  const barWidth = Math.max(4, (width - barPadding * data.length) / data.length);

  return (
    <svg width={width} height={height}>
      {data.map((d, i) => {
        const barH = (d.value / maxVal) * (chartHeight - 4);
        const x = i * (barWidth + barPadding);
        const y = chartHeight - barH;
        return (
          <g key={`${d.label}-${i}`}>
            <rect x={x} y={y} width={barWidth} height={barH} fill={d.color} rx={2} />
            {barWidth > 14 && (
              <text
                x={x + barWidth / 2}
                y={height - 2}
                fill="#6b7280"
                fontSize="9"
                textAnchor="middle"
              >
                {d.label}
              </text>
            )}
            <title>{`${d.label}: ${d.value}`}</title>
          </g>
        );
      })}
    </svg>
  );
}

function HorizontalBarChart({
  data,
  width,
  height,
}: {
  data: { label: string; value: number; color: string }[];
  width: number;
  height: number;
}) {
  if (data.length === 0) {
    return (
      <svg width={width} height={height}>
        <text x={width / 2} y={height / 2} fill="#6b7280" fontSize="12" textAnchor="middle">
          No data
        </text>
      </svg>
    );
  }

  const maxVal = Math.max(...data.map((d) => d.value), 1);
  const labelWidth = 80;
  const barAreaWidth = width - labelWidth - 40;
  const rowHeight = Math.min(28, height / data.length);

  return (
    <svg width={width} height={Math.max(height, data.length * rowHeight)}>
      {data.map((d, i) => {
        const barW = (d.value / maxVal) * barAreaWidth;
        const y = i * rowHeight;
        return (
          <g key={`${d.label}-${i}`}>
            <text
              x={labelWidth - 4}
              y={y + rowHeight / 2 + 4}
              fill="#9ca3af"
              fontSize="11"
              textAnchor="end"
            >
              {d.label.length > 12 ? d.label.slice(0, 11) + "\u2026" : d.label}
            </text>
            <rect
              x={labelWidth}
              y={y + 4}
              width={Math.max(barW, 2)}
              height={rowHeight - 8}
              fill={d.color}
              rx={3}
            />
            <text x={labelWidth + barW + 6} y={y + rowHeight / 2 + 4} fill="#9ca3af" fontSize="11">
              {d.value}
            </text>
            <title>{`${d.label}: ${d.value}`}</title>
          </g>
        );
      })}
    </svg>
  );
}

function SimpleLineChart({
  data,
  width,
  height,
}: {
  data: { label: string; value: number }[];
  width: number;
  height: number;
}) {
  if (data.length === 0) {
    return (
      <svg width={width} height={height}>
        <text x={width / 2} y={height / 2} fill="#6b7280" fontSize="12" textAnchor="middle">
          No data
        </text>
      </svg>
    );
  }

  const padding = { top: 8, right: 8, bottom: 20, left: 8 };
  const chartW = width - padding.left - padding.right;
  const chartH = height - padding.top - padding.bottom;

  const maxVal = Math.max(...data.map((d) => d.value), 0.01);
  const minVal = Math.min(...data.map((d) => d.value), 0);

  const range = maxVal - minVal || 1;

  const points = data.map((d, i) => {
    const x = padding.left + (data.length === 1 ? chartW / 2 : (i / (data.length - 1)) * chartW);
    const y = padding.top + chartH - ((d.value - minVal) / range) * chartH;
    return { x, y, ...d };
  });

  const pathD = points.map((p, i) => `${i === 0 ? "M" : "L"} ${p.x} ${p.y}`).join(" ");

  // Area fill path
  const areaD =
    pathD +
    ` L ${points[points.length - 1].x} ${padding.top + chartH}` +
    ` L ${points[0].x} ${padding.top + chartH} Z`;

  return (
    <svg width={width} height={height}>
      {/* grid lines */}
      {[0, 0.25, 0.5, 0.75, 1].map((frac) => {
        const y = padding.top + chartH * (1 - frac);
        return (
          <line
            key={frac}
            x1={padding.left}
            y1={y}
            x2={width - padding.right}
            y2={y}
            stroke="#1f2937"
            strokeWidth={1}
          />
        );
      })}
      {/* area */}
      <path d={areaD} fill="#3b82f6" opacity={0.15} />
      {/* line */}
      <path d={pathD} fill="none" stroke="#3b82f6" strokeWidth={2} />
      {/* dots */}
      {points.map((p, i) => (
        <g key={`${p.label}-${i}`}>
          <circle cx={p.x} cy={p.y} r={3} fill="#3b82f6" />
          <title>{`${p.label}: ${(p.value * 100).toFixed(0)}%`}</title>
        </g>
      ))}
      {/* x-axis labels - show first, middle, last */}
      {points.length > 0 &&
        [0, Math.floor(points.length / 2), points.length - 1]
          .filter((v, i, a) => a.indexOf(v) === i)
          .map((idx) => (
            <text
              key={`axis-${points[idx].label}`}
              x={points[idx].x}
              y={height - 2}
              fill="#6b7280"
              fontSize="9"
              textAnchor="middle"
            >
              {points[idx].label.slice(5)}
            </text>
          ))}
    </svg>
  );
}

// ---------------------------------------------------------------------------
// Sub-components
// ---------------------------------------------------------------------------

function StatCard({ label, value, detail }: { label: string; value: number; detail: string }) {
  return (
    <div
      style={{
        background: "#111827",
        borderRadius: "8px",
        padding: "16px",
        border: "1px solid #1f2937",
      }}
    >
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
    <span
      style={{
        background: c.bg,
        color: c.text,
        padding: "2px 8px",
        borderRadius: "4px",
        fontSize: "11px",
      }}
    >
      {source}
    </span>
  );
}

function ChartCard({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div
      style={{
        background: "#111827",
        borderRadius: "8px",
        padding: "12px",
        border: "1px solid #1f2937",
      }}
    >
      <div style={{ fontSize: "13px", color: "#9ca3af", marginBottom: "8px" }}>{title}</div>
      {children}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Main component
// ---------------------------------------------------------------------------

export function LearningDashboard() {
  const [stats, setStats] = useState<AdaptiveLearningStats | null>(null);
  const [templates, setTemplates] = useState<TemplatePerformance[]>([]);
  const [trends, setTrends] = useState<LearningTrends | null>(null);
  const [loading, setLoading] = useState(true);
  const intervalRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const loadData = useCallback(async () => {
    setLoading(true);
    try {
      const [statsResult, templatesResult, trendsResult] = await Promise.all([
        invoke<AdaptiveLearningStats>("get_adaptive_learning_stats"),
        invoke<TemplatePerformance[]>("get_template_performance"),
        invoke<LearningTrends>("get_learning_trends", { days: 30 }).catch(() => null),
      ]);
      setStats(statsResult);
      setTemplates(templatesResult);
      setTrends(trendsResult);
    } catch (err) {
      console.error("Failed to load learning dashboard:", err);
    } finally {
      setLoading(false);
    }
  }, []);

  // Auto-refresh: event listener + 60s interval. The initial loadData call is
  // wrapped in a microtask so the effect body itself doesn't synchronously
  // trigger setState inside loadData.
  useEffect(() => {
    let cancelled = false;
    void Promise.resolve().then(() => {
      if (!cancelled) void loadData();
    });

    let unlisten: UnlistenFn | null = null;
    listen("learning-update", () => {
      loadData();
    }).then((fn) => {
      unlisten = fn;
    });

    intervalRef.current = setInterval(loadData, 60_000);

    return () => {
      cancelled = true;
      if (unlisten) unlisten();
      if (intervalRef.current) clearInterval(intervalRef.current);
    };
  }, [loadData]);

  if (loading && !stats) {
    return <div style={{ padding: "16px", color: "#9ca3af" }}>Loading dashboard...</div>;
  }

  if (!stats) {
    return (
      <div style={{ padding: "16px", textAlign: "center", color: "#9ca3af" }}>
        Could not load adaptive learning data. Tables may not exist yet — run a workflow first.
        <br />
        <button
          onClick={loadData}
          style={{
            marginTop: "8px",
            padding: "4px 12px",
            borderRadius: "4px",
            background: "#3b82f6",
            color: "white",
            border: "none",
            cursor: "pointer",
          }}
        >
          Retry
        </button>
      </div>
    );
  }

  // Prepare chart data
  const lessonBarData = (trends?.lesson_trends ?? []).map((pt) => ({
    label: pt.day.slice(5),
    value: pt.count,
    color: severityColor(pt.severity),
  }));

  const domainBarData = (trends?.domain_distribution ?? []).map((d, i) => ({
    label: d.domain,
    value: d.count,
    color: domainColor(i),
  }));

  const helpfulnessLineData = (trends?.helpfulness_trends ?? []).map((pt) => ({
    label: pt.day,
    value: pt.avg_helpfulness,
  }));

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
        <h3 style={{ margin: 0 }}>Adaptive Learning</h3>
        <div style={{ display: "flex", alignItems: "center", gap: "8px" }}>
          {loading && <span style={{ fontSize: "11px", color: "#6b7280" }}>refreshing...</span>}
          <button
            onClick={loadData}
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

      {/* Stats Cards */}
      <div
        style={{
          display: "grid",
          gridTemplateColumns: "repeat(4, 1fr)",
          gap: "12px",
          marginBottom: "24px",
        }}
      >
        <StatCard
          label="Playbook Lessons"
          value={stats.playbook_entries}
          detail={`${stats.active_lessons} active, ${stats.staged_lessons} staged`}
        />
        <StatCard
          label="Curated Examples"
          value={stats.curated_examples}
          detail="Few-shot library"
        />
        <StatCard
          label="Templates Tracked"
          value={stats.templates_tracked}
          detail="Performance monitored"
        />
        <StatCard
          label="GEPA Optimizations"
          value={stats.gepa_runs}
          detail={`Avg helpfulness: ${(stats.avg_lesson_helpfulness * 100).toFixed(0)}%`}
        />
      </div>

      {/* Trend Charts */}
      {trends && (
        <div
          style={{
            display: "grid",
            gridTemplateColumns: "1fr 1fr",
            gap: "12px",
            marginBottom: "24px",
          }}
        >
          <ChartCard title="Lessons Added (30d, colored by severity)">
            <SimpleBarChart data={lessonBarData} width={400} height={140} />
          </ChartCard>

          <ChartCard title="Helpfulness Trend (30d)">
            <SimpleLineChart data={helpfulnessLineData} width={400} height={140} />
          </ChartCard>

          <ChartCard title="Domain Distribution">
            <HorizontalBarChart
              data={domainBarData}
              width={400}
              height={Math.max(100, domainBarData.length * 28)}
            />
          </ChartCard>

          <ChartCard title="Severity Legend">
            <div style={{ display: "flex", flexWrap: "wrap", gap: "8px", padding: "8px 0" }}>
              {Object.entries(SEVERITY_COLORS).map(([sev, color]) => (
                <span
                  key={sev}
                  style={{
                    display: "inline-flex",
                    alignItems: "center",
                    gap: "4px",
                    fontSize: "11px",
                    color: "#9ca3af",
                  }}
                >
                  <span
                    style={{
                      display: "inline-block",
                      width: "10px",
                      height: "10px",
                      borderRadius: "2px",
                      background: color,
                    }}
                  />
                  {sev}
                </span>
              ))}
            </div>
          </ChartCard>
        </div>
      )}

      {/* Template Performance Table */}
      {templates.length > 0 && (
        <div>
          <h4 style={{ margin: "0 0 8px 0", fontSize: "14px", color: "#9ca3af" }}>
            Template Performance
          </h4>
          <table style={{ width: "100%", borderCollapse: "collapse", fontSize: "13px" }}>
            <thead>
              <tr style={{ borderBottom: "1px solid #374151" }}>
                <th style={{ textAlign: "left", padding: "8px", color: "#9ca3af" }}>Template</th>
                <th style={{ textAlign: "left", padding: "8px", color: "#9ca3af" }}>Source</th>
                <th style={{ textAlign: "right", padding: "8px", color: "#9ca3af" }}>Success</th>
                <th style={{ textAlign: "right", padding: "8px", color: "#9ca3af" }}>Failure</th>
                <th style={{ textAlign: "right", padding: "8px", color: "#9ca3af" }}>Confidence</th>
                <th style={{ textAlign: "right", padding: "8px", color: "#9ca3af" }}>
                  Avg Quality
                </th>
              </tr>
            </thead>
            <tbody>
              {templates.slice(0, 20).map((t) => (
                <tr key={t.template_id} style={{ borderBottom: "1px solid #1f2937" }}>
                  <td style={{ padding: "6px 8px", color: "#e5e7eb" }}>{t.template_name}</td>
                  <td style={{ padding: "6px 8px" }}>
                    <SourceBadge source={t.source} />
                  </td>
                  <td style={{ padding: "6px 8px", textAlign: "right", color: "#22c55e" }}>
                    {t.success_count}
                  </td>
                  <td style={{ padding: "6px 8px", textAlign: "right", color: "#ef4444" }}>
                    {t.failure_count}
                  </td>
                  <td
                    style={{
                      padding: "6px 8px",
                      textAlign: "right",
                      color:
                        t.confidence > 0.7 ? "#22c55e" : t.confidence > 0.4 ? "#f59e0b" : "#ef4444",
                    }}
                  >
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

export default LearningDashboard;
