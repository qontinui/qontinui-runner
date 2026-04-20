/**
 * ComplexityDashboard
 *
 * Treemap visualization of page complexity, trend sparklines per page,
 * drift alerts section, and score breakdown on click.
 */

import { useState, useMemo } from "react";
import {
  ResponsiveContainer,
  Treemap,
  LineChart,
  Line,
  XAxis,
  YAxis,
  Tooltip,
  BarChart,
  Bar,
  Cell,
} from "recharts";
import { TrendingUp, AlertTriangle, Layers } from "lucide-react";
import type {
  ComplexityScore,
  ComplexityAnalysisResult,
  DriftAlert,
} from "@/lib/development-intelligence/complexity-scorer";
import { getTierColor, getScoreBreakdown } from "@/lib/development-intelligence/complexity-scorer";

interface ComplexityDashboardProps {
  data: ComplexityAnalysisResult | null;
  loading: boolean;
  onRefresh: () => void;
}

// =============================================================================
// Custom Treemap Content
// =============================================================================

function TreemapContent(props: {
  x: number;
  y: number;
  width: number;
  height: number;
  name: string;
  tier: string;
  composite: number;
}) {
  const { x, y, width, height, name, tier, composite } = props;
  const color = getTierColor(tier as ComplexityScore["tier"]);

  if (width < 40 || height < 30) return null;

  return (
    <g>
      <rect
        x={x}
        y={y}
        width={width}
        height={height}
        rx={4}
        fill={`${color}30`}
        stroke={color}
        strokeWidth={1.5}
      />
      {width > 60 && (
        <>
          <text
            x={x + width / 2}
            y={y + height / 2 - 6}
            textAnchor="middle"
            fill="currentColor"
            fontSize={11}
            fontWeight={600}
          >
            {name.length > width / 8 ? name.slice(0, Math.floor(width / 8)) + "…" : name}
          </text>
          <text
            x={x + width / 2}
            y={y + height / 2 + 10}
            textAnchor="middle"
            fill={color}
            fontSize={10}
          >
            {composite}
          </text>
        </>
      )}
    </g>
  );
}

// =============================================================================
// Drift Alert Card
// =============================================================================

function DriftAlertCard({ alert }: { alert: DriftAlert }) {
  return (
    <div className="flex items-start gap-2 p-2 rounded-md border border-yellow-500/30 bg-yellow-500/5 text-xs">
      <AlertTriangle className="w-3.5 h-3.5 text-yellow-500 mt-0.5 shrink-0" />
      <div>
        <span className="font-medium">{alert.page}</span>
        <span className="text-muted-foreground ml-1">({alert.route})</span>
        <div className="text-muted-foreground mt-0.5">{alert.warning}</div>
      </div>
      <div className="ml-auto text-right shrink-0">
        <div className="font-medium">{alert.currentComposite}</div>
        <div className="text-muted-foreground">
          {alert.velocityPerWeek > 0 ? "+" : ""}
          {alert.velocityPerWeek}%/wk
        </div>
      </div>
    </div>
  );
}

// =============================================================================
// Score Breakdown
// =============================================================================

function ScoreBreakdownPanel({ score }: { score: ComplexityScore }) {
  const breakdown = getScoreBreakdown(score.scores);
  const tierColor = getTierColor(score.tier);

  return (
    <div className="p-3 border-t bg-muted/30">
      <div className="flex items-center gap-2 mb-2">
        <span className="text-xs font-medium">{score.page}</span>
        <span
          className="text-xs px-1.5 py-0.5 rounded"
          style={{ color: tierColor, background: `${tierColor}20` }}
        >
          {score.tier} ({score.composite})
        </span>
      </div>

      <div className="h-32">
        <ResponsiveContainer width="100%" height="100%">
          <BarChart
            data={breakdown}
            layout="vertical"
            margin={{ left: 70, right: 8, top: 4, bottom: 4 }}
          >
            <XAxis type="number" tick={{ fontSize: 9 }} />
            <YAxis type="category" dataKey="label" tick={{ fontSize: 9 }} width={65} />
            <Tooltip
              content={({ active, payload }) => {
                if (!active || !payload?.length) return null;
                const d = payload[0].payload;
                return (
                  <div className="bg-popover border border-border rounded-lg p-2 text-xs">
                    <p className="font-medium">{d.label}</p>
                    <p>
                      Value: {d.value} × {d.weight} = {d.weighted}
                    </p>
                  </div>
                );
              }}
            />
            <Bar dataKey="weighted" radius={[0, 4, 4, 0]}>
              {breakdown.map((_, i) => (
                <Cell key={i} fill={tierColor} fillOpacity={0.7} />
              ))}
            </Bar>
          </BarChart>
        </ResponsiveContainer>
      </div>

      {score.drift && (
        <div className="mt-2">
          <div className="text-xs text-muted-foreground mb-1">Complexity Trend</div>
          <div className="h-16">
            <ResponsiveContainer width="100%" height="100%">
              <LineChart data={score.drift.complexityTrend.map((v, i) => ({ i, v }))}>
                <Line
                  type="monotone"
                  dataKey="v"
                  stroke={tierColor}
                  strokeWidth={1.5}
                  dot={false}
                />
              </LineChart>
            </ResponsiveContainer>
          </div>
          {score.drift.warning && (
            <div className="text-xs text-yellow-500 mt-1">{score.drift.warning}</div>
          )}
        </div>
      )}
    </div>
  );
}

// =============================================================================
// Main Dashboard
// =============================================================================

export function ComplexityDashboard({
  data,
  loading,
  onRefresh: _onRefresh,
}: ComplexityDashboardProps) {
  const [selectedPage, setSelectedPage] = useState<string | null>(null);

  const treemapData = useMemo(() => {
    if (!data) return [];
    return data.scores.map((s) => ({
      name: s.page,
      size: Math.max(s.composite, 5),
      tier: s.tier,
      composite: s.composite,
    }));
  }, [data]);

  const selectedScore = useMemo(
    () => data?.scores.find((s) => s.page === selectedPage),
    [data, selectedPage],
  );

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64 text-muted-foreground text-sm">
        Computing complexity scores...
      </div>
    );
  }

  if (!data) {
    return (
      <div className="flex flex-col items-center justify-center h-64 text-muted-foreground text-sm gap-2">
        <Layers className="w-8 h-8" />
        <span>No complexity data. Click refresh to analyze.</span>
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full">
      {/* Summary bar */}
      <div className="flex items-center gap-4 px-4 py-3 border-b bg-card">
        <div className="flex items-center gap-1.5">
          <Layers className="w-4 h-4 text-muted-foreground" />
          <span className="text-sm font-medium">Complexity Scores</span>
        </div>
        <div className="flex items-center gap-3 text-xs text-muted-foreground">
          <span>{data.summary.totalPages} pages</span>
          <span style={{ color: getTierColor("simple") }}>{data.summary.simple} simple</span>
          <span style={{ color: getTierColor("moderate") }}>{data.summary.moderate} moderate</span>
          <span style={{ color: getTierColor("complex") }}>{data.summary.complex} complex</span>
          <span style={{ color: getTierColor("critical") }}>{data.summary.critical} critical</span>
          <span>Avg: {Math.round(data.summary.averageComposite)}</span>
        </div>
      </div>

      {/* Drift alerts */}
      {data.summary.driftAlerts.length > 0 && (
        <div className="px-4 py-2 border-b space-y-1.5">
          <div className="flex items-center gap-1.5 text-xs font-medium text-yellow-500">
            <TrendingUp className="w-3.5 h-3.5" />
            Drift Alerts ({data.summary.driftAlerts.length})
          </div>
          {data.summary.driftAlerts.map((alert) => (
            <DriftAlertCard key={alert.page} alert={alert} />
          ))}
        </div>
      )}

      {/* Treemap */}
      <div className="flex-1 min-h-0 flex">
        <div className="flex-1 p-4">
          <div className="text-xs text-muted-foreground mb-2">
            Page Complexity (size = score, color = tier)
          </div>
          <div className="h-[calc(100%-24px)]">
            <ResponsiveContainer width="100%" height="100%">
              <Treemap
                data={treemapData}
                dataKey="size"
                aspectRatio={4 / 3}
                content={
                  <TreemapContent x={0} y={0} width={0} height={0} name="" tier="" composite={0} />
                }
                onClick={(node: { name?: string }) => {
                  if (node?.name) setSelectedPage(node.name);
                }}
              />
            </ResponsiveContainer>
          </div>
        </div>

        {/* Detail sidebar */}
        {selectedScore && (
          <div className="w-72 border-l overflow-auto">
            <ScoreBreakdownPanel score={selectedScore} />
          </div>
        )}
      </div>

      {/* Page table */}
      <div className="border-t max-h-48 overflow-auto">
        <table className="w-full text-xs">
          <thead className="sticky top-0 bg-card border-b">
            <tr>
              <th className="text-left px-3 py-1.5 font-medium">Page</th>
              <th className="text-right px-3 py-1.5 font-medium">Score</th>
              <th className="text-left px-3 py-1.5 font-medium">Tier</th>
              <th className="text-right px-3 py-1.5 font-medium">Elements</th>
              <th className="text-right px-3 py-1.5 font-medium">Interactions</th>
              <th className="text-right px-3 py-1.5 font-medium">API Calls</th>
            </tr>
          </thead>
          <tbody>
            {data.scores.map((s) => {
              const color = getTierColor(s.tier);
              return (
                <tr
                  key={s.page}
                  className={`border-b hover:bg-muted/50 cursor-pointer ${selectedPage === s.page ? "bg-muted/30" : ""}`}
                  onClick={() => setSelectedPage(s.page)}
                >
                  <td className="px-3 py-1.5 font-medium">{s.page}</td>
                  <td className="text-right px-3 py-1.5">
                    <span style={{ color }}>{s.composite}</span>
                  </td>
                  <td className="px-3 py-1.5">
                    <span
                      className="px-1.5 py-0.5 rounded text-xs"
                      style={{ color, background: `${color}20` }}
                    >
                      {s.tier}
                    </span>
                  </td>
                  <td className="text-right px-3 py-1.5">{s.scores.elementCount}</td>
                  <td className="text-right px-3 py-1.5">{s.scores.interactionCount}</td>
                  <td className="text-right px-3 py-1.5">{s.scores.apiCallCount}</td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
    </div>
  );
}
