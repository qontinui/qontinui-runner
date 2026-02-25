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
} from "lucide-react";
import type {
  ReflectionFix,
  EffectivenessReport,
  ReflectionRunSummary,
  FixType,
  FixEffectiveness,
} from "../../types/reflection";

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
    const response = await fetch(url);
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
// Component
// ============================================================================

export function ReflectionDashboard() {
  const [workflowName, setWorkflowName] = useState<string>("");
  const [workflows, setWorkflows] = useState<string[]>([]);
  const [report, setReport] = useState<EffectivenessReport | null>(null);
  const [history, setHistory] = useState<ReflectionRunSummary[]>([]);
  const [fixes, setFixes] = useState<ReflectionFix[]>([]);
  const [loading, setLoading] = useState(false);
  const [expandedFixes, setExpandedFixes] = useState<Set<string>>(new Set());

  // Load available workflow names from recent task runs
  useEffect(() => {
    const controller = new AbortController();
    let cancelled = false;

    async function loadWorkflows() {
      try {
        const resp = await fetch("http://localhost:9876/task-runs?limit=100", {
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
      const [reportData, historyData, fixesData] = await Promise.all([
        fetchApi<EffectivenessReport>(
          `http://localhost:9876/reflection/effectiveness-report?workflow_name=${encodeURIComponent(workflowName)}`,
        ),
        fetchApi<ReflectionRunSummary[]>(
          `http://localhost:9876/reflection/history?workflow_name=${encodeURIComponent(workflowName)}`,
        ),
        fetchApi<ReflectionFix[]>(
          `http://localhost:9876/reflection-fixes?workflow_name=${encodeURIComponent(workflowName)}`,
        ),
      ]);
      setReport(reportData);
      setHistory(historyData || []);
      setFixes(fixesData || []);
    } finally {
      setLoading(false);
    }
  }, [workflowName]);

  useEffect(() => {
    loadData();
  }, [loadData]);

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
                        className="w-full px-4 py-2 flex items-center gap-3 text-left hover:bg-muted/50 transition-colors"
                      >
                        {isExpanded ? (
                          <ChevronDown className="w-3.5 h-3.5 flex-shrink-0" />
                        ) : (
                          <ChevronRight className="w-3.5 h-3.5 flex-shrink-0" />
                        )}
                        <span className="px-1.5 py-0.5 text-xs bg-purple-500/20 text-purple-300 rounded">
                          {FIX_TYPE_LABELS[fix.fix_type] || fix.fix_type}
                        </span>
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
                          {fix.effectiveness_evidence && (
                            <div>
                              <span className="font-medium">Evidence:</span>{" "}
                              {fix.effectiveness_evidence}
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
          </>
        )}
      </div>
    </div>
  );
}
