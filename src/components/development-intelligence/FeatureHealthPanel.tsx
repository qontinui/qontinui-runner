/**
 * FeatureHealthPanel
 *
 * Four-column Kanban-style status board (Active | Stale | Spec Drift | Abandoned)
 * with filtering, timeline view, and pie chart distribution.
 */

import { useState, useMemo } from "react";
import { ResponsiveContainer, PieChart, Pie, Cell } from "recharts";
import { Activity, Clock, AlertTriangle, Archive, GitCommit, FileText } from "lucide-react";
import type {
  FeatureHealth,
  FeatureHealthResult,
} from "@/lib/development-intelligence/dead-feature-detector";
import {
  getStatusColor,
  getStatusLabel,
  groupByStatus,
  getStatusDistribution,
} from "@/lib/development-intelligence/dead-feature-detector";

interface FeatureHealthPanelProps {
  data: FeatureHealthResult | null;
  loading: boolean;
  onRefresh: () => void;
}

// =============================================================================
// Feature Card
// =============================================================================

function FeatureCard({ feature }: { feature: FeatureHealth }) {
  const statusColor = getStatusColor(feature.status);

  return (
    <div className="border rounded-lg p-2.5 text-xs bg-card hover:bg-muted/30 transition-colors">
      <div className="flex items-center gap-1.5 mb-1.5">
        <div className="w-2 h-2 rounded-full shrink-0" style={{ background: statusColor }} />
        <span className="font-medium truncate">{feature.page}</span>
      </div>

      <div className="space-y-1 text-muted-foreground">
        <div className="flex items-center gap-1.5">
          <GitCommit className="w-3 h-3" />
          <span>
            Code: {feature.codeAge === 0 ? "today" : `${Math.round(feature.codeAge)}d ago`}
          </span>
          {feature.codeCommitCount30d > 0 && (
            <span className="text-foreground">({feature.codeCommitCount30d} commits/30d)</span>
          )}
        </div>
        <div className="flex items-center gap-1.5">
          <FileText className="w-3 h-3" />
          <span>
            Spec: {feature.specAge === 0 ? "today" : `${Math.round(feature.specAge)}d ago`}
          </span>
        </div>
      </div>

      {feature.signals.length > 0 && (
        <div className="mt-1.5 space-y-0.5">
          {feature.signals.map((signal, i) => (
            <div
              key={i}
              className="text-[10px] text-muted-foreground bg-muted/50 rounded px-1.5 py-0.5"
            >
              {signal}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

// =============================================================================
// Status Column
// =============================================================================

const STATUS_ICONS: Record<FeatureHealth["status"], typeof Activity> = {
  active: Activity,
  stale: Clock,
  "spec-drift": AlertTriangle,
  abandoned: Archive,
};

function StatusColumn({
  status,
  features,
}: {
  status: FeatureHealth["status"];
  features: FeatureHealth[];
}) {
  const Icon = STATUS_ICONS[status];
  const color = getStatusColor(status);

  return (
    <div className="flex-1 min-w-0 flex flex-col">
      <div className="flex items-center gap-1.5 px-2 py-1.5 border-b">
        <Icon className="w-3.5 h-3.5" style={{ color }} />
        <span className="text-xs font-medium">{getStatusLabel(status)}</span>
        <span
          className="text-xs px-1.5 py-0.5 rounded-full ml-auto"
          style={{ color, background: `${color}20` }}
        >
          {features.length}
        </span>
      </div>
      <div className="flex-1 overflow-auto p-2 space-y-2">
        {features.length === 0 ? (
          <div className="text-xs text-muted-foreground text-center py-4">None</div>
        ) : (
          features.map((f) => <FeatureCard key={f.page} feature={f} />)
        )}
      </div>
    </div>
  );
}

// =============================================================================
// Timeline View
// =============================================================================

function TimelineView({ features }: { features: FeatureHealth[] }) {
  const sorted = useMemo(
    () =>
      [...features].sort(
        (a, b) => new Date(b.lastCodeChange).getTime() - new Date(a.lastCodeChange).getTime(),
      ),
    [features],
  );

  const [now] = useState(() => Date.now());
  const maxAge = 180; // 180 days max display

  return (
    <div className="overflow-auto p-4">
      <div className="text-xs text-muted-foreground mb-2">
        Timeline (last {maxAge} days) — Blue: code changes, Green: spec changes
      </div>
      <div className="space-y-1">
        {sorted.map((f) => {
          const codeDaysAgo = Math.min(
            maxAge,
            (now - new Date(f.lastCodeChange).getTime()) / (1000 * 60 * 60 * 24),
          );
          const specDaysAgo = Math.min(
            maxAge,
            (now - new Date(f.lastSpecChange).getTime()) / (1000 * 60 * 60 * 24),
          );
          const codeLeft = ((maxAge - codeDaysAgo) / maxAge) * 100;
          const specLeft = ((maxAge - specDaysAgo) / maxAge) * 100;
          const statusColor = getStatusColor(f.status);

          // Gap bar between spec and code (red if code changed but spec didn't)
          const gapStart = Math.min(codeLeft, specLeft);
          const gapEnd = Math.max(codeLeft, specLeft);
          const hasGap = f.status === "spec-drift" && gapEnd - gapStart > 2;

          return (
            <div key={f.page} className="flex items-center gap-2 h-6">
              <div
                className="w-28 shrink-0 text-xs truncate font-medium"
                style={{ color: statusColor }}
              >
                {f.page}
              </div>
              <div className="flex-1 h-4 bg-muted/30 rounded relative">
                {/* Gap indicator */}
                {hasGap && (
                  <div
                    className="absolute top-0 bottom-0 bg-destructive/15 rounded"
                    style={{
                      left: `${gapStart}%`,
                      width: `${gapEnd - gapStart}%`,
                    }}
                  />
                )}
                {/* Code dot */}
                <div
                  className="absolute top-1/2 -translate-y-1/2 w-2.5 h-2.5 rounded-full bg-blue-500 border border-background"
                  style={{ left: `${codeLeft}%` }}
                  title={`Code: ${f.lastCodeChange}`}
                />
                {/* Spec dot */}
                <div
                  className="absolute top-1/2 -translate-y-1/2 w-2.5 h-2.5 rounded-full bg-emerald-500 border border-background"
                  style={{ left: `${specLeft}%` }}
                  title={`Spec: ${f.lastSpecChange}`}
                />
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}

// =============================================================================
// Main Panel
// =============================================================================

export function FeatureHealthPanel({
  data,
  loading,
  onRefresh: _onRefresh,
}: FeatureHealthPanelProps) {
  const [view, setView] = useState<"board" | "timeline">("board");
  const [filter, setFilter] = useState<FeatureHealth["status"] | "all">("all");

  const grouped = useMemo(() => (data ? groupByStatus(data.features) : null), [data]);

  const distribution = useMemo(() => (data ? getStatusDistribution(data.features) : []), [data]);

  const filteredFeatures = useMemo(() => {
    if (!data) return [];
    if (filter === "all") return data.features;
    return data.features.filter((f) => f.status === filter);
  }, [data, filter]);

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64 text-muted-foreground text-sm">
        Analyzing feature health...
      </div>
    );
  }

  if (!data) {
    return (
      <div className="flex flex-col items-center justify-center h-64 text-muted-foreground text-sm gap-2">
        <Activity className="w-8 h-8" />
        <span>No feature health data. Click refresh to analyze.</span>
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full">
      {/* Summary bar */}
      <div className="flex items-center gap-4 px-4 py-3 border-b bg-card">
        <div className="flex items-center gap-1.5">
          <Activity className="w-4 h-4 text-muted-foreground" />
          <span className="text-sm font-medium">Feature Health</span>
        </div>

        {/* Mini pie chart */}
        <div className="w-8 h-8">
          <ResponsiveContainer width="100%" height="100%">
            <PieChart>
              <Pie
                data={distribution}
                dataKey="value"
                cx="50%"
                cy="50%"
                innerRadius={8}
                outerRadius={14}
                strokeWidth={0}
              >
                {distribution.map((d, i) => (
                  <Cell key={i} fill={d.color} />
                ))}
              </Pie>
            </PieChart>
          </ResponsiveContainer>
        </div>

        <div className="flex items-center gap-3 text-xs text-muted-foreground">
          <span>{data.summary.total} features</span>
          <span className="text-emerald-500">{data.summary.active} active</span>
          <span className="text-yellow-500">{data.summary.stale} stale</span>
          <span className="text-orange-500">{data.summary.specDrift} drift</span>
          <span className="text-destructive">{data.summary.abandoned} abandoned</span>
        </div>

        <div className="ml-auto flex items-center gap-1">
          <select
            className="text-xs bg-muted rounded px-1.5 py-1 border-0"
            value={filter}
            onChange={(e) => setFilter(e.target.value as FeatureHealth["status"] | "all")}
          >
            <option value="all">All</option>
            <option value="active">Active</option>
            <option value="stale">Stale</option>
            <option value="spec-drift">Spec Drift</option>
            <option value="abandoned">Abandoned</option>
          </select>
          <button
            className={`px-2 py-1 text-xs rounded ${view === "board" ? "bg-primary text-primary-foreground" : "bg-muted"}`}
            onClick={() => setView("board")}
          >
            Board
          </button>
          <button
            className={`px-2 py-1 text-xs rounded ${view === "timeline" ? "bg-primary text-primary-foreground" : "bg-muted"}`}
            onClick={() => setView("timeline")}
          >
            Timeline
          </button>
        </div>
      </div>

      {/* Content */}
      {view === "board" ? (
        <div className="flex-1 flex min-h-0 divide-x">
          {filter === "all" ? (
            <>
              <StatusColumn status="active" features={grouped!.active} />
              <StatusColumn status="stale" features={grouped!.stale} />
              <StatusColumn status="spec-drift" features={grouped!["spec-drift"]} />
              <StatusColumn status="abandoned" features={grouped!.abandoned} />
            </>
          ) : (
            <StatusColumn status={filter} features={filteredFeatures} />
          )}
        </div>
      ) : (
        <div className="flex-1 min-h-0 overflow-auto">
          <TimelineView features={filteredFeatures} />
        </div>
      )}
    </div>
  );
}
