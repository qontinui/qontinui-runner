/**
 * ReflectionDashboard.tsx
 *
 * Aggregated effectiveness data for reflection fixes across workflows.
 * Shows an effectiveness report summary, recent reflection runs, and a
 * filterable list of individual fixes with expandable details.
 */

import { useState, useEffect, useCallback } from "react";
import {
  RotateCcw,
  CheckCircle,
  XCircle,
  AlertTriangle,
  HelpCircle,
  ChevronDown,
  ChevronRight,
  Loader2,
  RefreshCw,
  TrendingUp,
} from "lucide-react";
import {
  LineChart,
  Line,
  XAxis,
  YAxis,
  Tooltip,
  ResponsiveContainer,
  Bar,
  BarChart,
} from "recharts";
import type {
  ReflectionFix,
  EffectivenessReport,
  ReflectionRunSummary,
  FixType,
  FixEffectiveness,
  ConvergenceStatus,
} from "../../types/reflection";
import type {
  WorkflowTrends,
  EffectivenessOverTime,
  EffectivenessBucket,
} from "../../types/architecture";
import { getApiBase, tracedFetch } from "@/lib/runner-api";
import { CrossRunPatternsPanel } from "./CrossRunPatternsPanel";
import { StepProvenanceTimeline } from "./StepProvenanceTimeline";
import { PipelineEventsTimeline } from "../pipeline-events/PipelineEventsTimeline";
import { RuleInfluencePanel } from "../rule-influence/RuleInfluencePanel";
import { WorkflowVersionLineage } from "../workflow-versions/WorkflowVersionLineage";
import { usePipelineEvents, useIneffectiveRules } from "../../hooks/useGraphAnalytics";

// ============================================================================
// Constants
// ============================================================================

const EFFECTIVENESS_COLORS: Record<
  FixEffectiveness,
  { bg: string; text: string; icon: typeof CheckCircle }
> = {
  effective: { bg: "bg-green-500/20", text: "text-green-400", icon: CheckCircle },
  ineffective: { bg: "bg-red-500/20", text: "text-red-400", icon: XCircle },
  caused_regression: { bg: "bg-orange-500/20", text: "text-orange-400", icon: AlertTriangle },
  inconclusive: { bg: "bg-gray-500/20", text: "text-gray-400", icon: HelpCircle },
};

const FIX_TYPE_LABELS: Record<FixType, string> = {
  knowledge_base_update: "Knowledge Update",
  workflow_step_rewrite: "Step Rewrite",
  selector_fix: "Selector Fix",
  tool_config_update: "Tool Config",
  context_addition: "Context Addition",
  instruction_clarification: "Instruction Fix",
  project_environment: "Environment",
  project_architecture: "Architecture",
  project_test_pattern: "Test Pattern",
  project_recurring_issue: "Recurring Issue",
};

// ============================================================================
// Helpers
// ============================================================================

interface ApiResponse<T> {
  success: boolean;
  data?: T;
  error?: string;
}

async function fetchApi<T>(url: string): Promise<T | null> {
  try {
    const response = await tracedFetch(url);
    if (!response.ok) return null;
    const json: ApiResponse<T> = await response.json();
    return json.success ? (json.data ?? null) : null;
  } catch {
    return null;
  }
}

function formatDate(dateStr: string): string {
  const d = new Date(dateStr);
  const now = new Date();
  const diffMs = now.getTime() - d.getTime();
  const diffMins = Math.floor(diffMs / 60000);
  if (diffMins < 60) return `${diffMins}m ago`;
  const diffHours = Math.floor(diffMins / 60);
  if (diffHours < 24) return `${diffHours}h ago`;
  const diffDays = Math.floor(diffHours / 24);
  return `${diffDays}d ago`;
}

// ============================================================================
// Chart Helpers
// ============================================================================

function formatShortDate(label: unknown): string {
  const d = new Date(String(label));
  return `${d.getMonth() + 1}/${d.getDate()}`;
}

function TrendTooltip({
  active,
  payload,
  label,
  valueLabel,
  valueSuffix = "",
}: {
  active?: boolean;
  payload?: { value: number }[];
  label?: string;
  valueLabel: string;
  valueSuffix?: string;
}) {
  if (active && payload && payload.length) {
    return (
      <div className="bg-popover border border-border rounded-lg p-2 shadow-lg text-xs">
        <p className="font-medium">{label}</p>
        <p className="text-purple-400">
          {valueLabel}: {payload[0].value.toFixed(1)}
          {valueSuffix}
        </p>
      </div>
    );
  }
  return null;
}

function EffectivenessChartTooltip({
  active,
  payload,
}: {
  active?: boolean;
  payload?: { payload: EffectivenessBucket & { ratePercent: number } }[];
}) {
  if (active && payload && payload.length) {
    const d = payload[0].payload;
    return (
      <div className="bg-popover border border-border rounded-lg p-2 shadow-lg text-xs">
        <p className="font-medium">{d.bucket}</p>
        <p className="text-green-400">
          Effective: {d.effective}/{d.total} ({d.ratePercent.toFixed(1)}%)
        </p>
        {d.ineffective > 0 && <p className="text-yellow-400">Ineffective: {d.ineffective}</p>}
        {d.regression > 0 && <p className="text-red-400">Regression: {d.regression}</p>}
      </div>
    );
  }
  return null;
}

const CONVERGENCE_STYLES: Record<
  string,
  { border: string; bg: string; text: string; icon: typeof CheckCircle }
> = {
  converging: {
    border: "border-green-500/50",
    bg: "bg-green-500/10",
    text: "text-green-400",
    icon: CheckCircle,
  },
  stalled: {
    border: "border-red-500/50",
    bg: "bg-red-500/10",
    text: "text-red-400",
    icon: XCircle,
  },
  improving: {
    border: "border-blue-500/50",
    bg: "bg-blue-500/10",
    text: "text-blue-400",
    icon: TrendingUp,
  },
  regressing: {
    border: "border-orange-500/50",
    bg: "bg-orange-500/10",
    text: "text-orange-400",
    icon: AlertTriangle,
  },
  insufficient_data: {
    border: "border-gray-500/50",
    bg: "bg-gray-500/10",
    text: "text-gray-400",
    icon: HelpCircle,
  },
};

const AXIS_TICK_STYLE = {
  fontSize: 9,
  fill: "hsl(var(--muted-foreground))",
};

// ============================================================================
// Graph Insights Sub-Tabs
// ============================================================================

type GraphTab = "patterns" | "rules" | "pipeline" | "provenance" | "versions";

const GRAPH_TABS: { id: GraphTab; label: string }[] = [
  { id: "patterns", label: "Cross-Run Patterns" },
  { id: "rules", label: "Rule Influence" },
  { id: "pipeline", label: "Pipeline Events" },
  { id: "provenance", label: "Step Provenance" },
  { id: "versions", label: "Versions" },
];

function GraphInsightsSection({
  workflowName,
  pipelineEvents,
  eventsLoading,
}: {
  workflowName: string;
  pipelineEvents: import("../../hooks/useGraphAnalytics").PipelineEvent[];
  eventsLoading: boolean;
}) {
  const [activeTab, setActiveTab] = useState<GraphTab>("patterns");

  return (
    <div className="space-y-3" data-tutorial-id="graph-insights-section">
      <h3 className="text-sm font-semibold text-muted-foreground uppercase tracking-wider">
        Graph Insights
      </h3>
      <div
        className="flex gap-1 border-b border-border pb-px"
        data-tutorial-id="graph-insights-tabs"
      >
        {GRAPH_TABS.map((tab) => (
          <button
            key={tab.id}
            onClick={() => setActiveTab(tab.id)}
            data-tutorial-id={`graph-tab-${tab.id}`}
            className={`px-3 py-1.5 text-xs font-medium rounded-t transition-colors ${
              activeTab === tab.id
                ? "bg-card text-foreground border border-border border-b-transparent -mb-px"
                : "text-muted-foreground hover:text-foreground"
            }`}
          >
            {tab.label}
          </button>
        ))}
      </div>
      <div>
        {activeTab === "patterns" && <CrossRunPatternsPanel workflowName={workflowName} />}
        {activeTab === "rules" && <RuleInfluencePanel />}
        {activeTab === "pipeline" && (
          <PipelineEventsTimeline events={pipelineEvents} isLoading={eventsLoading} />
        )}
        {activeTab === "provenance" && <StepProvenanceTimeline workflowId={workflowName} />}
        {activeTab === "versions" && <WorkflowVersionLineage />}
      </div>
    </div>
  );
}

// ============================================================================
// Component
// ============================================================================

export function ReflectionDashboard() {
  const [workflowName, setWorkflowName] = useState<string>("");
  const [workflows, setWorkflows] = useState<string[]>([]);
  const [report, setReport] = useState<EffectivenessReport | null>(null);
  const [history, setHistory] = useState<ReflectionRunSummary[]>([]);
  const [fixes, setFixes] = useState<ReflectionFix[]>([]);
  const [trends, setTrends] = useState<WorkflowTrends | null>(null);
  const [effectivenessTrend, setEffectivenessTrend] = useState<EffectivenessOverTime | null>(null);
  const [convergenceStatus, setConvergenceStatus] = useState<ConvergenceStatus | null>(null);
  const [loading, setLoading] = useState(false);
  const [expandedFixes, setExpandedFixes] = useState<Set<string>>(new Set());

  // Load available workflow names from recent task runs
  useEffect(() => {
    const controller = new AbortController();
    let cancelled = false;

    async function loadWorkflows() {
      try {
        const resp = await tracedFetch(`${getApiBase()}/task-runs?limit=100`, {
          signal: controller.signal,
        });
        const json = await resp.json();
        if (!cancelled && json.success && json.data) {
          const names = new Set<string>();
          for (const run of json.data) {
            if (run.workflow_name) names.add(run.workflow_name);
          }
          const sorted = Array.from(names).sort();
          setWorkflows(sorted);
          if (sorted.length > 0 && !workflowName) {
            setWorkflowName(sorted[0]);
          }
        }
      } catch {
        // ignore - runner may not be available or request was aborted
      }
    }
    loadWorkflows();
    return () => {
      cancelled = true;
      controller.abort();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const loadData = useCallback(async () => {
    if (!workflowName) return;
    setLoading(true);
    try {
      const encoded = encodeURIComponent(workflowName);
      const [reportData, historyData, fixesData, trendsData, effTrendData, convergenceData] =
        await Promise.all([
          fetchApi<EffectivenessReport>(
            `${getApiBase()}/reflection/effectiveness-report?workflow_name=${encoded}`,
          ),
          fetchApi<ReflectionRunSummary[]>(
            `${getApiBase()}/reflection/history?workflow_name=${encoded}`,
          ),
          fetchApi<ReflectionFix[]>(`${getApiBase()}/reflection-fixes?workflow_name=${encoded}`),
          fetchApi<WorkflowTrends>(
            `${getApiBase()}/reflection/trends?workflow_name=${encoded}&time_range=30d`,
          ),
          fetchApi<EffectivenessOverTime>(
            `${getApiBase()}/reflection/trends/effectiveness?workflow_name=${encoded}&bucket=week&time_range=30d`,
          ),
          fetchApi<ConvergenceStatus>(
            `${getApiBase()}/reflection/convergence-status?workflow_name=${encoded}`,
          ),
        ]);
      setReport(reportData);
      setHistory(historyData || []);
      setFixes(fixesData || []);
      setTrends(trendsData);
      setEffectivenessTrend(effTrendData);
      setConvergenceStatus(convergenceData);
    } finally {
      setLoading(false);
    }
  }, [workflowName]);

  // Wrap loadData() in async IIFE so its synchronous setState calls aren't
  // the first statements reached from the effect body
  // (react-hooks/set-state-in-effect).
  useEffect(() => {
    let cancelled = false;
    (async () => {
      if (cancelled) return;
      await loadData();
    })();
    return () => {
      cancelled = true;
    };
  }, [loadData]);

  // Graph Insights data
  const latestTaskRunId = history.length > 0 ? history[0].task_run_id : "";
  const { data: pipelineEvents, isLoading: eventsLoading } = usePipelineEvents(latestTaskRunId);
  const { data: _ineffectiveRules, isLoading: _rulesLoading } = useIneffectiveRules();

  const toggleFix = (id: string) => {
    setExpandedFixes((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  return (
    <div className="h-full flex flex-col overflow-hidden p-4 space-y-4">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <RotateCcw className="w-5 h-5 text-purple-400" />
          <h2 className="text-lg font-semibold">Reflection Effectiveness</h2>
        </div>
        <div className="flex items-center gap-2">
          <select
            value={workflowName}
            onChange={(e) => setWorkflowName(e.target.value)}
            disabled={workflows.length === 0}
            aria-label="Select workflow"
            className="px-3 py-1.5 text-sm bg-muted border border-border rounded-md"
          >
            {workflows.length === 0 && <option value="">No workflows found</option>}
            {workflows.map((name) => (
              <option key={name} value={name}>
                {name}
              </option>
            ))}
          </select>
          <button
            onClick={loadData}
            disabled={loading}
            aria-label="Refresh data"
            className="p-1.5 rounded hover:bg-muted transition-colors"
          >
            <RefreshCw className={`w-4 h-4 ${loading ? "animate-spin" : ""}`} />
          </button>
        </div>
      </div>

      {/* Content */}
      <div className="flex-1 overflow-y-auto space-y-4">
        {loading && !report ? (
          <div className="flex items-center justify-center py-12">
            <Loader2 className="w-6 h-6 animate-spin text-muted-foreground" />
          </div>
        ) : !report || report.total_fixes === 0 ? (
          <div className="text-center py-12 text-muted-foreground">
            <RotateCcw className="w-8 h-8 mx-auto mb-2 opacity-50" />
            <p>No reflection fixes found for this workflow.</p>
            <p className="text-xs mt-1">
              Reflection runs will appear here after workflow executions.
            </p>
          </div>
        ) : (
          <>
            {/* Summary Cards */}
            <div className="grid grid-cols-2 md:grid-cols-4 gap-3">
              <div className="bg-card border border-border rounded-lg p-3">
                <div className="text-2xl font-bold">{report.total_fixes}</div>
                <div className="text-xs text-muted-foreground">Total Fixes</div>
              </div>
              <div className="bg-card border border-border rounded-lg p-3">
                <div className="text-2xl font-bold text-green-400">{report.effective_count}</div>
                <div className="text-xs text-muted-foreground">Effective</div>
              </div>
              <div className="bg-card border border-border rounded-lg p-3">
                <div className="text-2xl font-bold text-red-400">{report.ineffective_count}</div>
                <div className="text-xs text-muted-foreground">Ineffective</div>
              </div>
              <div className="bg-card border border-border rounded-lg p-3">
                <div className="text-2xl font-bold text-purple-400">
                  {report.effectiveness_rate != null
                    ? `${Math.round(report.effectiveness_rate * 100)}%`
                    : "N/A"}
                </div>
                <div className="text-xs text-muted-foreground">Effectiveness Rate</div>
              </div>
            </div>

            {/* Convergence Status */}
            {convergenceStatus &&
              convergenceStatus.status !== "insufficient_data" &&
              (() => {
                const style =
                  CONVERGENCE_STYLES[convergenceStatus.status] ||
                  CONVERGENCE_STYLES.insufficient_data;
                const StatusIcon = style.icon;
                return (
                  <div className={`border ${style.border} ${style.bg} rounded-lg p-3`}>
                    <div className="flex items-center gap-2 mb-1">
                      <StatusIcon className={`w-4 h-4 ${style.text}`} />
                      <span className={`text-sm font-semibold ${style.text} capitalize`}>
                        {convergenceStatus.status}
                      </span>
                      {convergenceStatus.convergence_score > 0 && (
                        <span className="text-xs text-muted-foreground ml-auto">
                          Score: {(convergenceStatus.convergence_score * 100).toFixed(0)}%
                        </span>
                      )}
                    </div>
                    <p className="text-xs text-muted-foreground">{convergenceStatus.reason}</p>
                    {convergenceStatus.stall_findings.length > 0 && (
                      <div className="mt-1.5">
                        <span className="text-xs font-medium">Recurring findings:</span>
                        <ul className="mt-0.5 text-xs text-muted-foreground list-disc list-inside">
                          {convergenceStatus.stall_findings.slice(0, 5).map((f, i) => (
                            <li key={`finding-${i}-${f.slice(0, 20)}`}>{f}</li>
                          ))}
                        </ul>
                      </div>
                    )}
                    {convergenceStatus.recommendation && (
                      <p className="mt-1.5 text-xs italic text-muted-foreground">
                        {convergenceStatus.recommendation}
                      </p>
                    )}
                  </div>
                );
              })()}

            {/* Trend Charts */}
            {trends && trends.snapshot_count > 1 && (
              <div className="grid grid-cols-1 md:grid-cols-3 gap-3">
                {/* Convergence Score */}
                <div className="bg-card border border-border rounded-lg p-3">
                  <h4 className="text-[11px] font-medium text-muted-foreground mb-1">
                    Convergence Score
                  </h4>
                  <div className="h-24">
                    <ResponsiveContainer width="100%" height="100%">
                      <LineChart
                        data={trends.convergence}
                        margin={{ left: 0, right: 4, top: 4, bottom: 0 }}
                      >
                        <XAxis
                          dataKey="timestamp"
                          tick={AXIS_TICK_STYLE}
                          tickLine={false}
                          axisLine={false}
                          interval="preserveStartEnd"
                          tickFormatter={formatShortDate}
                        />
                        <YAxis
                          tick={AXIS_TICK_STYLE}
                          tickLine={false}
                          axisLine={false}
                          width={32}
                          domain={[0, 1]}
                          tickFormatter={(v: number) => `${(v * 100).toFixed(0)}%`}
                        />
                        <Tooltip
                          content={<TrendTooltip valueLabel="Convergence" valueSuffix="" />}
                          labelFormatter={formatShortDate}
                        />
                        <Line
                          type="monotone"
                          dataKey="value"
                          stroke="#a78bfa"
                          strokeWidth={1.5}
                          dot={{ r: 2, fill: "#a78bfa" }}
                          activeDot={{ r: 4, fill: "#a78bfa" }}
                        />
                      </LineChart>
                    </ResponsiveContainer>
                  </div>
                </div>

                {/* Effectiveness Rate + Volume */}
                {effectivenessTrend &&
                  effectivenessTrend.buckets.length > 0 &&
                  (() => {
                    const chartData = effectivenessTrend.buckets.map((b) => ({
                      ...b,
                      ratePercent: b.effectiveness_rate * 100,
                    }));
                    const firstRate = chartData[0]?.ratePercent ?? 0;
                    const lastRate = chartData[chartData.length - 1]?.ratePercent ?? 0;
                    const trendColor = lastRate >= firstRate ? "#22c55e" : "#ef4444";
                    return (
                      <div className="bg-card border border-border rounded-lg p-3">
                        <h4 className="text-[11px] font-medium text-muted-foreground mb-1">
                          Effectiveness Rate
                        </h4>
                        <div className="h-24">
                          <ResponsiveContainer width="100%" height="100%">
                            <LineChart
                              data={chartData}
                              margin={{ left: 0, right: 4, top: 4, bottom: 0 }}
                            >
                              <XAxis
                                dataKey="bucket"
                                tick={AXIS_TICK_STYLE}
                                tickLine={false}
                                axisLine={false}
                                interval="preserveStartEnd"
                              />
                              <YAxis
                                tick={AXIS_TICK_STYLE}
                                tickLine={false}
                                axisLine={false}
                                width={32}
                                domain={[0, 100]}
                                tickFormatter={(v: number) => `${v}%`}
                              />
                              <Tooltip content={<EffectivenessChartTooltip />} />
                              <Line
                                type="monotone"
                                dataKey="ratePercent"
                                stroke={trendColor}
                                strokeWidth={1.5}
                                dot={{ r: 2, fill: trendColor }}
                                activeDot={{ r: 4, fill: trendColor }}
                              />
                            </LineChart>
                          </ResponsiveContainer>
                        </div>
                      </div>
                    );
                  })()}

                {/* Fix Velocity */}
                <div className="bg-card border border-border rounded-lg p-3">
                  <h4 className="text-[11px] font-medium text-muted-foreground mb-1">
                    Fix Velocity
                  </h4>
                  <div className="h-24">
                    <ResponsiveContainer width="100%" height="100%">
                      <BarChart
                        data={trends.velocity}
                        margin={{ left: 0, right: 4, top: 4, bottom: 0 }}
                      >
                        <XAxis
                          dataKey="timestamp"
                          tick={AXIS_TICK_STYLE}
                          tickLine={false}
                          axisLine={false}
                          interval="preserveStartEnd"
                          tickFormatter={formatShortDate}
                        />
                        <YAxis
                          tick={AXIS_TICK_STYLE}
                          tickLine={false}
                          axisLine={false}
                          width={24}
                        />
                        <Tooltip
                          content={<TrendTooltip valueLabel="Velocity" />}
                          labelFormatter={formatShortDate}
                        />
                        <Bar dataKey="value" fill="#6366f1" radius={[2, 2, 0, 0]} />
                      </BarChart>
                    </ResponsiveContainer>
                  </div>
                </div>
              </div>
            )}

            {/* Reflection History */}
            {history.length > 0 && (
              <div className="bg-card border border-border rounded-lg">
                <div className="px-4 py-3 border-b border-border">
                  <h3 className="text-sm font-medium">Recent Reflection Runs</h3>
                </div>
                <div className="divide-y divide-border">
                  {history.slice(0, 5).map((run) => (
                    <div
                      key={run.task_run_id}
                      className="px-4 py-2 flex items-center justify-between"
                    >
                      <div className="flex items-center gap-2">
                        <RotateCcw className="w-3.5 h-3.5 text-purple-400" />
                        <span className="text-sm truncate max-w-[200px]">
                          {run.task_run_id.slice(0, 8)}...
                        </span>
                        <span
                          className={`px-1.5 py-0.5 text-xs rounded ${
                            run.status === "complete"
                              ? "bg-green-500/20 text-green-400"
                              : run.status === "running"
                                ? "bg-blue-500/20 text-blue-400"
                                : "bg-red-500/20 text-red-400"
                          }`}
                        >
                          {run.status}
                        </span>
                      </div>
                      <div className="flex items-center gap-3 text-xs text-muted-foreground">
                        <span>{run.fix_count} fixes</span>
                        <span>{formatDate(run.created_at)}</span>
                      </div>
                    </div>
                  ))}
                </div>
              </div>
            )}

            {/* Fix List */}
            <div className="bg-card border border-border rounded-lg">
              <div className="px-4 py-3 border-b border-border">
                <h3 className="text-sm font-medium">All Fixes ({fixes.length})</h3>
              </div>
              <div className="divide-y divide-border">
                {fixes.map((fix) => {
                  const isExpanded = expandedFixes.has(fix.id);
                  const effConfig = fix.effectiveness
                    ? EFFECTIVENESS_COLORS[fix.effectiveness]
                    : null;
                  const EffIcon = effConfig?.icon || HelpCircle;

                  return (
                    <div key={fix.id}>
                      <button
                        onClick={() => toggleFix(fix.id)}
                        aria-expanded={isExpanded}
                        className="w-full px-4 py-2 flex items-center gap-3 text-left hover:bg-muted/50 transition-colors"
                      >
                        {isExpanded ? (
                          <ChevronDown className="w-3.5 h-3.5 shrink-0" />
                        ) : (
                          <ChevronRight className="w-3.5 h-3.5 shrink-0" />
                        )}
                        <span className="px-1.5 py-0.5 text-xs bg-purple-500/20 text-purple-300 rounded">
                          {FIX_TYPE_LABELS[fix.fix_type] || fix.fix_type}
                        </span>
                        {fix.reflection_scope === "universal" && (
                          <span className="px-1.5 py-0.5 text-xs bg-blue-500/20 text-blue-300 rounded">
                            Universal
                          </span>
                        )}
                        <span className="flex-1 text-sm truncate">{fix.fix_description}</span>
                        <span
                          className={`px-1.5 py-0.5 text-xs rounded ${
                            fix.confidence === "high"
                              ? "bg-green-500/20 text-green-400"
                              : fix.confidence === "medium"
                                ? "bg-yellow-500/20 text-yellow-400"
                                : "bg-gray-500/20 text-gray-400"
                          }`}
                        >
                          {fix.confidence}
                        </span>
                        {fix.effectiveness && effConfig && (
                          <span
                            className={`inline-flex items-center gap-1 px-1.5 py-0.5 text-xs rounded ${effConfig.bg} ${effConfig.text}`}
                          >
                            <EffIcon className="w-3 h-3" />
                            {fix.effectiveness}
                          </span>
                        )}
                      </button>
                      {isExpanded && (
                        <div className="px-4 py-2 ml-8 space-y-1 text-xs text-muted-foreground bg-muted/30">
                          {fix.file_changed && (
                            <div>
                              <span className="font-medium">File:</span> {fix.file_changed}
                            </div>
                          )}
                          <div>
                            <span className="font-medium">Status:</span> {fix.status}
                          </div>
                          <div>
                            <span className="font-medium">Applied:</span>{" "}
                            {formatDate(fix.applied_at)}
                          </div>
                          {fix.applicability_context && (
                            <div>
                              <span className="font-medium">Applies to:</span>{" "}
                              {fix.applicability_context}
                            </div>
                          )}
                          {fix.effectiveness_evidence && (
                            <div>
                              <span className="font-medium">Evidence:</span>{" "}
                              {fix.effectiveness_evidence}
                            </div>
                          )}
                          {fix.reasoning && (
                            <div className="mt-1">
                              <span className="font-medium">Reasoning:</span>
                              <p className="mt-0.5 text-xs leading-relaxed">{fix.reasoning}</p>
                            </div>
                          )}
                          {fix.alternatives_considered && (
                            <div className="mt-1">
                              <span className="font-medium">Alternatives considered:</span>
                              <p className="mt-0.5 text-xs leading-relaxed">
                                {fix.alternatives_considered}
                              </p>
                            </div>
                          )}
                          {fix.old_value && (
                            <div className="mt-1">
                              <span className="font-medium">Old value:</span>
                              <pre className="mt-0.5 p-1 bg-muted rounded text-xs overflow-x-auto">
                                {fix.old_value}
                              </pre>
                            </div>
                          )}
                          {fix.new_value && (
                            <div className="mt-1">
                              <span className="font-medium">New value:</span>
                              <pre className="mt-0.5 p-1 bg-muted rounded text-xs overflow-x-auto">
                                {fix.new_value}
                              </pre>
                            </div>
                          )}
                        </div>
                      )}
                    </div>
                  );
                })}
                {fixes.length === 0 && (
                  <div className="px-4 py-4 text-sm text-muted-foreground text-center">
                    No fixes recorded yet.
                  </div>
                )}
              </div>
            </div>

            {/* Graph Insights Section — sub-tabbed for focus */}
            {workflowName && (
              <GraphInsightsSection
                workflowName={workflowName}
                pipelineEvents={pipelineEvents || []}
                eventsLoading={eventsLoading}
              />
            )}
          </>
        )}
      </div>
    </div>
  );
}
