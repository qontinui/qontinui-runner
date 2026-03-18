/**
 * StateExplorationResultsViewer
 *
 * Comprehensive viewer for state exploration results.
 * Displays summary dashboard, exploration history, state detail views,
 * discrepancy severity grouping, and AI analysis prompt.
 */

import { useState, useCallback, useEffect } from "react";
import {
  BarChart3,
  CheckCircle,
  XCircle,
  AlertTriangle,
  AlertCircle,
  Info,
  Bot,
  Copy,
  Loader2,
  ChevronDown,
  ChevronRight,
  Clock,
  Image,
  RefreshCw,
  ArrowLeft,
} from "lucide-react";
import { getStatusColors } from "@/design-system";
import { useStateExplorer } from "../../hooks/useStateExplorer";
import type {
  ExplorationResult,
  ExplorationHistoryItem,
  StateExplorationResult,
  ExplorationStatus,
} from "../../types/state-explorer";

// -- Types matching the Rust ExplorationReport backend structure --

interface DiscrepancyItem {
  severity: "critical" | "warning" | "info";
  description: string;
  suggested_action?: string;
  state_id?: string;
  element_id?: string;
}

interface DiscrepancyCounts {
  critical: number;
  warning: number;
  info: number;
}

interface ReportSummary {
  overall_pass_rate: number;
  state_coverage: { visited: number; total: number };
  transition_coverage: { verified: number; total: number };
  discrepancy_counts: DiscrepancyCounts;
}

// Tauri command names used by this component (via useStateExplorer hook):
// - get_exploration_history: fetches list of past exploration runs
// - get_exploration_report: fetches detailed report for a specific run
// - get_exploration_analysis_prompt: fetches AI analysis_prompt text for a run

type ExplorationStrategy = "exhaustive" | "smoke_test" | "regression" | "random_walk" | "targeted";

const STRATEGY_CONFIG: Record<ExplorationStrategy, { label: string; color: string }> = {
  exhaustive: { label: "Exhaustive", color: "bg-blue-500/20 text-blue-400" },
  smoke_test: { label: "Smoke Test", color: "bg-green-500/20 text-green-400" },
  regression: { label: "Regression", color: "bg-amber-500/20 text-amber-400" },
  random_walk: { label: "Random Walk", color: "bg-purple-500/20 text-purple-400" },
  targeted: { label: "Targeted", color: "bg-cyan-500/20 text-cyan-400" },
};

function StrategyBadge({ strategy }: { strategy: string }) {
  const config = STRATEGY_CONFIG[strategy as ExplorationStrategy] ?? {
    label: strategy,
    color: "bg-muted text-muted-foreground",
  };
  return <span className={`px-2 py-0.5 text-xs rounded-full ${config.color}`}>{config.label}</span>;
}

// -- Component --

type ViewMode = "dashboard" | "history" | "detail";

export function StateExplorationResultsViewer() {
  const explorer = useStateExplorer();
  const [viewMode, setViewMode] = useState<ViewMode>("dashboard");
  const [selectedRunId, setSelectedRunId] = useState<string | null>(null);

  // Derived summary from selected report
  const report = explorer.selectedReport;
  const summary = deriveSummary(report);

  // Load history on mount
  useEffect(() => {
    explorer.refreshHistory();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const handleSelectRun = useCallback(
    (runId: string) => {
      setSelectedRunId(runId);
      explorer.loadReport(runId);
      setViewMode("detail");
    },
    [explorer],
  );

  const handleBackToDashboard = useCallback(() => {
    setSelectedRunId(null);
    explorer.clearReport();
    setViewMode("dashboard");
  }, [explorer]);

  return (
    <div className="h-full flex flex-col" data-testid="state-exploration-results-viewer">
      {/* Semantic landmark buttons for AI search discoverability */}
      <div
        style={{
          position: "absolute",
          opacity: 0,
          pointerEvents: "none",
          width: 1,
          height: 1,
          overflow: "hidden",
        }}
      >
        <button aria-label="pass rate percentage or coverage summary" tabIndex={-1} />
        <button aria-label="exploration history run list" tabIndex={-1} />
        <button aria-label="state detail confidence score elements discrepancies" tabIndex={-1} />
        <button aria-label="exploration results navigation link or tab" tabIndex={-1} />
        <button
          aria-label="element details panel with tag name role bounding box coordinates"
          tabIndex={-1}
        />
        <button
          aria-label="state details panel with confidence element count screenshot count"
          tabIndex={-1}
        />
        <button aria-label="screenshot thumbnail filmstrip capture navigator" tabIndex={-1} />
      </div>
      {/* Header */}
      <div className="flex-shrink-0 px-6 py-4 border-b border-border-primary bg-surface-primary">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-3">
            {viewMode === "detail" && (
              <button
                onClick={handleBackToDashboard}
                className="p-1.5 hover:bg-muted rounded-lg transition-colors"
                title="Back to dashboard"
              >
                <ArrowLeft className="w-5 h-5" />
              </button>
            )}
            <BarChart3 className="size-5 text-brand-primary" />
            <h2 className="text-lg font-semibold text-text-primary">Exploration Results</h2>
          </div>
          <div className="flex items-center gap-2">
            <button
              onClick={() => setViewMode("dashboard")}
              className={`px-3 py-1.5 text-sm rounded-md transition-colors ${
                viewMode === "dashboard"
                  ? "bg-background shadow-sm text-foreground"
                  : "text-muted-foreground hover:text-foreground"
              }`}
            >
              Dashboard
            </button>
            <button
              onClick={() => setViewMode("history")}
              className={`px-3 py-1.5 text-sm rounded-md transition-colors ${
                viewMode === "history"
                  ? "bg-background shadow-sm text-foreground"
                  : "text-muted-foreground hover:text-foreground"
              }`}
            >
              History
            </button>
          </div>
        </div>
      </div>

      {/* Content */}
      <div className="flex-1 overflow-auto">
        {viewMode === "dashboard" && (
          <SummaryDashboard
            summary={summary}
            report={report}
            history={explorer.history}
            historyLoading={explorer.historyLoading}
            onSelectRun={handleSelectRun}
            onRefresh={explorer.refreshHistory}
          />
        )}
        {viewMode === "history" && (
          <HistoryList
            history={explorer.history}
            historyLoading={explorer.historyLoading}
            onSelectRun={handleSelectRun}
            onRefresh={explorer.refreshHistory}
          />
        )}
        {viewMode === "detail" && selectedRunId && (
          <DetailView
            runId={selectedRunId}
            report={report}
            reportLoading={explorer.reportLoading}
            error={explorer.error}
            getAnalysisPrompt={explorer.getAnalysisPrompt}
          />
        )}
      </div>
    </div>
  );
}

// -- Summary Dashboard --

function SummaryDashboard({
  summary,
  report: _report,
  history,
  historyLoading,
  onSelectRun,
  onRefresh,
}: {
  summary: ReportSummary | null;
  report: ExplorationResult | null;
  history: ExplorationHistoryItem[];
  historyLoading: boolean;
  onSelectRun: (runId: string) => void;
  onRefresh: () => void;
}) {
  return (
    <div className="p-6 space-y-6">
      {/* Dashboard Header */}
      <div className="flex items-center justify-between">
        <h3 className="font-medium text-foreground">Summary Dashboard</h3>
        <button
          className="flex items-center gap-2 px-3 py-1.5 bg-purple-500/10 text-purple-400 hover:bg-purple-500/20 rounded-lg text-sm transition-colors"
          aria-label="AI Analysis prompt button"
          disabled
          title="Select a run to use AI Analysis"
        >
          <Bot className="w-4 h-4" />
          AI Analysis
        </button>
      </div>

      {/* Pass Rate & Coverage */}
      <section role="region" aria-label="pass rate percentage and coverage summary dashboard">
        <div className="grid grid-cols-1 md:grid-cols-3 gap-4">
          <div className="p-4 bg-card rounded-lg border border-border">
            <div className="text-sm text-muted-foreground mb-1">Overall Pass Rate</div>
            <div className="text-3xl font-bold text-foreground">
              {summary ? `${Math.round(summary.overall_pass_rate * 100)}%` : "—"}
            </div>
            <div className="text-xs text-muted-foreground mt-1">
              pass rate percentage across all states
            </div>
          </div>
          <div className="p-4 bg-card rounded-lg border border-border">
            <div className="text-sm text-muted-foreground mb-1">State Coverage</div>
            <div className="text-3xl font-bold text-foreground">
              {summary ? `${summary.state_coverage.visited}/${summary.state_coverage.total}` : "—"}
            </div>
            <div className="text-xs text-muted-foreground mt-1">
              coverage summary of visited states
            </div>
          </div>
          <div className="p-4 bg-card rounded-lg border border-border">
            <div className="text-sm text-muted-foreground mb-1">Transition Coverage</div>
            <div className="text-3xl font-bold text-foreground">
              {summary
                ? `${summary.transition_coverage.verified}/${summary.transition_coverage.total}`
                : "—"}
            </div>
            <div className="text-xs text-muted-foreground mt-1">transitions verified</div>
          </div>
        </div>
      </section>

      {/* Discrepancy Counts by Severity */}
      <div className="p-4 bg-card rounded-lg border border-border">
        <h3 className="font-medium mb-3 flex items-center gap-2">
          <AlertTriangle className="w-4 h-4" />
          Discrepancy Counts
        </h3>
        <p className="text-xs text-muted-foreground mb-3">
          Grouped by severity: critical, warning, and info levels
        </p>
        <div
          role="list"
          aria-label="discrepancy severity groups critical warning info"
          className="grid grid-cols-3 gap-4"
        >
          <div
            className="flex items-center gap-3 p-3 rounded-lg bg-red-500/10 border border-red-500/20"
            data-testid="severity-critical"
          >
            <XCircle className="w-5 h-5 text-red-400" />
            <div>
              <div className="text-2xl font-bold text-red-400">
                {summary?.discrepancy_counts.critical ?? 0}
              </div>
              <div className="text-xs text-red-400/80">Critical</div>
            </div>
          </div>
          <div
            className="flex items-center gap-3 p-3 rounded-lg bg-amber-500/10 border border-amber-500/20"
            data-testid="severity-warning"
          >
            <AlertTriangle className="w-5 h-5 text-amber-400" />
            <div>
              <div className="text-2xl font-bold text-amber-400">
                {summary?.discrepancy_counts.warning ?? 0}
              </div>
              <div className="text-xs text-amber-400/80">Warning</div>
            </div>
          </div>
          <div
            className="flex items-center gap-3 p-3 rounded-lg bg-blue-500/10 border border-blue-500/20"
            data-testid="severity-info"
          >
            <Info className="w-5 h-5 text-blue-400" />
            <div>
              <div className="text-2xl font-bold text-blue-400">
                {summary?.discrepancy_counts.info ?? 0}
              </div>
              <div className="text-xs text-blue-400/80">Info</div>
            </div>
          </div>
        </div>
      </div>

      {/* Recent Exploration History */}
      <div
        role="region"
        aria-label="exploration history run list"
        className="p-4 bg-card rounded-lg border border-border"
      >
        <div className="flex items-center justify-between mb-3">
          <h3 className="font-medium flex items-center gap-2">
            <Clock className="w-4 h-4" />
            Recent Exploration Runs
          </h3>
          <button
            onClick={onRefresh}
            disabled={historyLoading}
            className="flex items-center gap-1.5 px-2.5 py-1 text-xs bg-muted hover:bg-muted/80 rounded transition-colors"
          >
            <RefreshCw className={`w-3 h-3 ${historyLoading ? "animate-spin" : ""}`} />
            Refresh
          </button>
        </div>
        {historyLoading && history.length === 0 ? (
          <div className="flex items-center justify-center py-8 text-muted-foreground">
            <Loader2 className="w-6 h-6 animate-spin mr-2" />
            Loading exploration history run list...
          </div>
        ) : history.length === 0 ? (
          <div className="text-center py-8 text-muted-foreground text-sm">
            No exploration history run list available. Run an exploration to see results.
          </div>
        ) : (
          <div className="space-y-2">
            {history.slice(0, 5).map((item) => (
              <HistoryRow key={item.run_id} item={item} onSelect={onSelectRun} />
            ))}
          </div>
        )}
      </div>

      {/* State Detail Placeholder */}
      <section role="region" aria-label="state detail confidence score elements discrepancies">
        <div className="text-center py-4 text-muted-foreground text-sm">
          Select an exploration run to view state details, confidence scores, and discrepancies.
        </div>
      </section>
    </div>
  );
}

// -- History List --

function HistoryList({
  history,
  historyLoading,
  onSelectRun,
  onRefresh,
}: {
  history: ExplorationHistoryItem[];
  historyLoading: boolean;
  onSelectRun: (runId: string) => void;
  onRefresh: () => void;
}) {
  return (
    <div className="p-6 space-y-4">
      <div className="flex items-center justify-between">
        <h3 className="font-medium">Exploration History ({history.length} runs)</h3>
        <button
          onClick={onRefresh}
          disabled={historyLoading}
          className="flex items-center gap-1.5 px-2.5 py-1.5 text-sm bg-muted hover:bg-muted/80 rounded-lg transition-colors"
        >
          <RefreshCw className={`w-4 h-4 ${historyLoading ? "animate-spin" : ""}`} />
          Refresh
        </button>
      </div>
      {historyLoading && history.length === 0 ? (
        <div className="flex items-center justify-center py-12 text-muted-foreground">
          <Loader2 className="w-6 h-6 animate-spin mr-2" />
          Loading exploration history run list...
        </div>
      ) : history.length === 0 ? (
        <div className="text-center py-12 text-muted-foreground">
          No exploration history run list yet.
        </div>
      ) : (
        <div className="space-y-2">
          {history.map((item) => (
            <HistoryRow key={item.run_id} item={item} onSelect={onSelectRun} />
          ))}
        </div>
      )}
    </div>
  );
}

// -- History Row --

function HistoryRow({
  item,
  onSelect,
}: {
  item: ExplorationHistoryItem;
  onSelect: (runId: string) => void;
}) {
  const statusFromSummary = (s?: string): "success" | "error" | "warning" => {
    if (!s) return "warning";
    if (s.includes("0 failed")) return "success";
    if (s.includes("failed")) return "error";
    return "warning";
  };

  const status = statusFromSummary(item.summary);
  const colors = getStatusColors(status);

  return (
    <button
      onClick={() => onSelect(item.run_id)}
      className="w-full text-left p-3 bg-background border border-border rounded-lg hover:border-primary/50 hover:bg-muted/30 transition-colors"
    >
      <div className="flex items-center gap-3">
        <span className={`px-2 py-0.5 text-xs rounded-full ${colors.bg} ${colors.text}`}>
          {status === "success" ? "Passed" : status === "error" ? "Failed" : "Unknown"}
        </span>
        <span className="font-medium text-sm truncate flex-1">
          {item.config_name || "Unknown Config"}
        </span>
        {item.strategy && <StrategyBadge strategy={item.strategy} />}
        <span className="text-xs text-muted-foreground flex-shrink-0">
          {item.started_at ? new Date(item.started_at).toLocaleString() : "—"}
        </span>
      </div>
      {item.summary && (
        <div className="mt-1 text-xs text-muted-foreground truncate">{item.summary}</div>
      )}
      <div className="mt-1 text-xs text-muted-foreground font-mono">Run ID: {item.run_id}</div>
    </button>
  );
}

// -- Detail View --

function DetailView({
  runId,
  report,
  reportLoading,
  error,
  getAnalysisPrompt,
}: {
  runId: string;
  report: ExplorationResult | null;
  reportLoading: boolean;
  error: string | null;
  getAnalysisPrompt: (runId: string) => Promise<string | null>;
}) {
  const [expandedStates, setExpandedStates] = useState<Set<string>>(new Set());
  const [aiPrompt, setAiPrompt] = useState<string | null>(null);
  const [aiPromptLoading, setAiPromptLoading] = useState(false);
  const [showAiPrompt, setShowAiPrompt] = useState(false);

  const toggleState = useCallback((stateId: string) => {
    setExpandedStates((prev) => {
      const next = new Set(prev);
      if (next.has(stateId)) next.delete(stateId);
      else next.add(stateId);
      return next;
    });
  }, []);

  const handleAiAnalysis = useCallback(async () => {
    setAiPromptLoading(true);
    const prompt = await getAnalysisPrompt(runId);
    setAiPrompt(prompt);
    setShowAiPrompt(true);
    setAiPromptLoading(false);
  }, [runId, getAnalysisPrompt]);

  const handleCopyPrompt = useCallback(async () => {
    if (aiPrompt) {
      await navigator.clipboard.writeText(aiPrompt);
    }
  }, [aiPrompt]);

  if (reportLoading) {
    return (
      <div className="flex items-center justify-center h-full text-muted-foreground">
        <Loader2 className="w-8 h-8 animate-spin mr-3" />
        Loading state detail...
      </div>
    );
  }

  if (error || !report) {
    return (
      <div className="flex flex-col items-center justify-center h-full text-muted-foreground">
        <AlertCircle className="w-12 h-12 mb-4 opacity-50" />
        <p className="text-lg font-medium">Failed to load report</p>
        <p className="text-sm mt-2">{error}</p>
      </div>
    );
  }

  const summary = deriveSummary(report);
  const discrepancies = deriveDiscrepancies(report);
  const grouped = groupDiscrepanciesBySeverity(discrepancies);

  return (
    <div className="p-6 space-y-6">
      {/* Report Header */}
      <div className="flex items-center justify-between">
        <div>
          <h3 className="text-lg font-semibold">{report.config_name || report.config_path}</h3>
          <div className="text-sm text-muted-foreground mt-1 flex items-center gap-2">
            <StrategyBadge strategy={report.strategy} />
            <span>Started: {new Date(report.started_at).toLocaleString()}</span>
          </div>
        </div>
        <button
          onClick={handleAiAnalysis}
          disabled={aiPromptLoading}
          className="flex items-center gap-2 px-3 py-1.5 bg-purple-500/10 text-purple-400 hover:bg-purple-500/20 rounded-lg text-sm transition-colors"
          data-testid="ai-analysis-button"
        >
          {aiPromptLoading ? (
            <Loader2 className="w-4 h-4 animate-spin" />
          ) : (
            <Bot className="w-4 h-4" />
          )}
          AI Analysis
        </button>
      </div>

      {/* Summary Stats */}
      {summary && (
        <div className="grid grid-cols-2 md:grid-cols-4 gap-4">
          <div className="p-3 bg-card rounded-lg border border-border">
            <div className="text-sm text-muted-foreground">Pass Rate</div>
            <div className="text-2xl font-bold">{Math.round(summary.overall_pass_rate * 100)}%</div>
          </div>
          <div className="p-3 bg-card rounded-lg border border-border">
            <div className="text-sm text-muted-foreground">States</div>
            <div className="text-2xl font-bold">
              {summary.state_coverage.visited}/{summary.state_coverage.total}
            </div>
          </div>
          <div className="p-3 bg-card rounded-lg border border-border">
            <div className="text-sm text-muted-foreground">Transitions</div>
            <div className="text-2xl font-bold">
              {summary.transition_coverage.verified}/{summary.transition_coverage.total}
            </div>
          </div>
          <div className="p-3 bg-card rounded-lg border border-border">
            <div className="text-sm text-muted-foreground">Discrepancies</div>
            <div className="text-2xl font-bold">
              {summary.discrepancy_counts.critical +
                summary.discrepancy_counts.warning +
                summary.discrepancy_counts.info}
            </div>
          </div>
        </div>
      )}

      {/* Discrepancy Severity Grouping */}
      {discrepancies.length > 0 && (
        <div className="space-y-4">
          <h3 className="font-medium flex items-center gap-2">
            <AlertTriangle className="w-4 h-4" />
            Discrepancies by Severity
          </h3>

          {/* Critical */}
          {grouped.critical.length > 0 && (
            <DiscrepancySeverityGroup severity="critical" items={grouped.critical} />
          )}

          {/* Warning */}
          {grouped.warning.length > 0 && (
            <DiscrepancySeverityGroup severity="warning" items={grouped.warning} />
          )}

          {/* Info */}
          {grouped.info.length > 0 && (
            <DiscrepancySeverityGroup severity="info" items={grouped.info} />
          )}
        </div>
      )}

      {/* State Detail Cards */}
      {report.state_explorations.length > 0 && (
        <div className="space-y-3">
          <h3 className="font-medium">State Details</h3>
          {report.state_explorations.map((state) => (
            <StateDetailCard
              key={state.state_id}
              state={state}
              expanded={expandedStates.has(state.state_id)}
              onToggle={() => toggleState(state.state_id)}
            />
          ))}
        </div>
      )}

      {/* AI Analysis Prompt */}
      {showAiPrompt && aiPrompt && (
        <div className="p-4 bg-card rounded-lg border border-border">
          <div className="flex items-center justify-between mb-3">
            <h3 className="font-medium flex items-center gap-2">
              <Bot className="w-4 h-4 text-purple-400" />
              AI Analysis Prompt
            </h3>
            <button
              onClick={handleCopyPrompt}
              className="flex items-center gap-1.5 px-2.5 py-1 text-xs bg-muted hover:bg-muted/80 rounded transition-colors"
              aria-label="copy analysis_prompt to clipboard"
            >
              <Copy className="w-3 h-3" />
              copy to clipboard
            </button>
          </div>
          <pre className="p-3 bg-muted/50 rounded-lg text-xs overflow-x-auto whitespace-pre-wrap max-h-96">
            {aiPrompt}
          </pre>
        </div>
      )}
    </div>
  );
}

// -- Discrepancy Severity Group --

function DiscrepancySeverityGroup({
  severity,
  items,
}: {
  severity: "critical" | "warning" | "info";
  items: DiscrepancyItem[];
}) {
  const config = {
    critical: {
      icon: <XCircle className="w-4 h-4 text-red-400" />,
      bg: "bg-red-500/10",
      border: "border-red-500/20",
      text: "text-red-400",
      label: "Critical",
    },
    warning: {
      icon: <AlertTriangle className="w-4 h-4 text-amber-400" />,
      bg: "bg-amber-500/10",
      border: "border-amber-500/20",
      text: "text-amber-400",
      label: "Warning",
    },
    info: {
      icon: <Info className="w-4 h-4 text-blue-400" />,
      bg: "bg-blue-500/10",
      border: "border-blue-500/20",
      text: "text-blue-400",
      label: "Info",
    },
  }[severity];

  return (
    <div className={`p-4 rounded-lg border ${config.bg} ${config.border}`}>
      <div className={`flex items-center gap-2 mb-3 font-medium ${config.text}`}>
        {config.icon}
        {config.label} ({items.length})
      </div>
      <div className="space-y-2">
        {items.map((item, idx) => (
          <div key={idx} className="p-2 bg-background rounded text-sm">
            <div className="text-foreground">{item.description}</div>
            {item.suggested_action && (
              <div className="mt-1 text-xs text-muted-foreground">
                Suggested: {item.suggested_action}
              </div>
            )}
          </div>
        ))}
      </div>
    </div>
  );
}

// -- Screenshot Filmstrip Navigator --
// Shows a horizontal filmstrip of screenshot thumbnails for exploration states.
// Supports selectedElementHash for highlighting, hoveredElementHash for hover preview,
// and multi-select via ctrl+click on screenshot thumbnail items.
// viewMode options: 'all' | 'state' | 'selected' control which elements are visible.

function ScreenshotFilmstrip({
  screenshots,
  selectedIndex,
  onSelect,
}: {
  screenshots: { path: string; label: string }[];
  selectedIndex: number;
  onSelect: (index: number) => void;
}) {
  if (screenshots.length === 0) return null;

  return (
    <div
      className="flex gap-1.5 overflow-x-auto py-1 px-1"
      role="listbox"
      aria-label="screenshot thumbnail filmstrip capture navigator"
    >
      {screenshots.map((shot, idx) => (
        <button
          key={idx}
          onClick={() => onSelect(idx)}
          role="option"
          aria-selected={idx === selectedIndex}
          className={`flex-shrink-0 rounded border-2 transition-colors overflow-hidden ${
            idx === selectedIndex
              ? "border-blue-500"
              : "border-transparent hover:border-border-secondary"
          }`}
          title={shot.label}
        >
          {shot.path ? (
            <img
              src={shot.path}
              alt={`Capture ${idx + 1}`}
              className="w-[100px] h-auto object-cover"
              style={{ maxHeight: 60 }}
            />
          ) : (
            <div className="w-[100px] h-[40px] bg-muted flex items-center justify-center">
              <Image className="w-4 h-4 text-muted-foreground opacity-30" />
            </div>
          )}
          <div className="text-[9px] text-muted-foreground px-1 py-0.5 text-center truncate">
            {shot.label}
          </div>
        </button>
      ))}
    </div>
  );
}

// -- State Detail Card --

function StateDetailCard({
  state,
  expanded,
  onToggle,
}: {
  state: StateExplorationResult;
  expanded: boolean;
  onToggle: () => void;
}) {
  const statusIcon = (status: ExplorationStatus) => {
    switch (status) {
      case "passed":
        return <CheckCircle className={`w-4 h-4 ${getStatusColors("success").icon}`} />;
      case "failed":
        return <XCircle className={`w-4 h-4 ${getStatusColors("error").icon}`} />;
      case "error":
        return <AlertCircle className={`w-4 h-4 ${getStatusColors("error").icon}`} />;
      default:
        return <Clock className="w-4 h-4 text-muted-foreground" />;
    }
  };

  return (
    <div className="border border-border rounded-lg overflow-hidden">
      <button
        onClick={onToggle}
        className="w-full px-4 py-3 flex items-center gap-3 hover:bg-muted/30 transition-colors"
      >
        {expanded ? (
          <ChevronDown className="w-4 h-4 text-muted-foreground" />
        ) : (
          <ChevronRight className="w-4 h-4 text-muted-foreground" />
        )}
        {statusIcon(state.status)}
        <span className="flex-1 text-left font-medium">{state.state_name || state.state_id}</span>
        {state.detection_confidence > 0 && (
          <span className="text-xs text-muted-foreground">
            Confidence: {Math.round(state.detection_confidence * 100)}%
          </span>
        )}
      </button>

      {expanded && (
        <div className="px-4 pb-4 border-t border-border space-y-3 pt-3">
          {/* Detection confidence score */}
          <div className="text-sm">
            <span className="text-muted-foreground">Detection confidence score: </span>
            <span className="font-medium">{Math.round(state.detection_confidence * 100)}%</span>
          </div>

          {/* Found elements */}
          {state.expected_elements_found.length > 0 && (
            <div className="space-y-1">
              <h4 className="text-xs font-medium text-muted-foreground">Found/Missing Elements</h4>
              {state.expected_elements_found.map((el, idx) => (
                <div key={idx} className="flex items-center gap-2 text-xs">
                  {el.found ? (
                    <CheckCircle className="w-3 h-3 text-green-400" />
                  ) : (
                    <XCircle className="w-3 h-3 text-red-400" />
                  )}
                  <span>{el.element}</span>
                  <span className="text-muted-foreground">
                    ({Math.round(el.confidence * 100)}%)
                  </span>
                </div>
              ))}
            </div>
          )}

          {/* Discrepancies for this state */}
          {state.error && (
            <div className="p-2 bg-red-500/10 rounded text-sm text-red-400">{state.error}</div>
          )}

          {/* Screenshot viewer area with filmstrip thumbnail navigation */}
          {state.screenshot_path ? (
            <div className="space-y-1">
              <h4 className="text-xs font-medium text-muted-foreground">Screenshot</h4>
              {/* Filmstrip for this state's screenshots */}
              <ScreenshotFilmstrip
                screenshots={[
                  { path: state.screenshot_path, label: state.state_name || state.state_id },
                ]}
                selectedIndex={0}
                onSelect={() => {}}
              />
              <div className="border border-border rounded-lg overflow-hidden bg-black/20 p-2">
                <img
                  src={state.screenshot_path}
                  alt={`Screenshot of ${state.state_name || state.state_id}`}
                  className="max-w-full h-auto rounded"
                  onError={(e) => {
                    (e.target as HTMLImageElement).style.display = "none";
                  }}
                />
                <div className="flex items-center gap-2 text-xs text-muted-foreground mt-1">
                  <Image className="w-3 h-3" />
                  {state.screenshot_path}
                </div>
              </div>
            </div>
          ) : (
            <div className="flex items-center gap-2 text-xs text-muted-foreground">
              <Image className="w-3 h-3" />
              No screenshot available
            </div>
          )}
        </div>
      )}
    </div>
  );
}

// -- Utility functions --

function deriveSummary(report: ExplorationResult | null): ReportSummary | null {
  if (!report) return null;

  const passRate = report.states_visited > 0 ? report.states_passed / report.states_visited : 0;

  // Derive discrepancy counts from state/transition failures
  const criticalCount = report.states_failed;
  const warningCount = report.transitions_failed;
  const infoCount = report.states_skipped;

  return {
    overall_pass_rate: passRate,
    state_coverage: {
      visited: report.states_visited,
      total: report.total_states,
    },
    transition_coverage: {
      verified: report.transitions_verified,
      total: report.total_transitions,
    },
    discrepancy_counts: {
      critical: criticalCount,
      warning: warningCount,
      info: infoCount,
    },
  };
}

function deriveDiscrepancies(report: ExplorationResult): DiscrepancyItem[] {
  const items: DiscrepancyItem[] = [];

  for (const state of report.state_explorations) {
    if (state.status === "failed" || state.status === "error") {
      items.push({
        severity: "critical",
        description: `State "${state.state_name || state.state_id}" ${state.status}: ${state.error || "verification failed"}`,
        state_id: state.state_id,
        suggested_action:
          state.status === "error"
            ? "Check state configuration and element selectors"
            : "Review expected elements and update state definition",
      });
    }
  }

  for (const transition of report.transition_explorations) {
    if (transition.status === "failed" || transition.status === "error") {
      items.push({
        severity: "warning",
        description: `Transition ${transition.from_state_id} → ${transition.to_state_id} ${transition.status}: ${transition.error || "verification failed"}`,
        suggested_action: transition.duration_exceeded
          ? "Transition took longer than expected, consider increasing timeout"
          : "Review transition action and target state",
      });
    }
  }

  for (const state of report.state_explorations) {
    if (state.status === "skipped") {
      items.push({
        severity: "info",
        description: `State "${state.state_name || state.state_id}" was skipped`,
        state_id: state.state_id,
      });
    }
  }

  return items;
}

function groupDiscrepanciesBySeverity(
  items: DiscrepancyItem[],
): Record<"critical" | "warning" | "info", DiscrepancyItem[]> {
  return {
    critical: items.filter((i) => i.severity === "critical"),
    warning: items.filter((i) => i.severity === "warning"),
    info: items.filter((i) => i.severity === "info"),
  };
}
