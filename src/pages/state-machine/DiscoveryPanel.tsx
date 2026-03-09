/**
 * DiscoveryPanel — Exploration configuration, execution, and history.
 *
 * Uses the State Explorer Tauri commands to configure, preview, run,
 * and review exploration tasks against the active state machine config.
 */

import { useState, useCallback } from "react";
import {
  Play,
  Eye,
  History,
  Trash2,
  CheckCircle2,
  XCircle,
  Clock,
  Loader2,
  RefreshCw,
  ChevronDown,
  ChevronRight,
  ArrowLeft,
  Settings2,
} from "lucide-react";
import { useStateExplorer } from "@/hooks/useStateExplorer";
import type {
  ExplorationConfig,
  ExplorationResult,
  ExplorationStatus,
} from "@/types/state-explorer";
import type { StateMachineState, StateMachineTransition } from "@qontinui/shared-types";

// =============================================================================
// Props & Types
// =============================================================================

interface DiscoveryPanelProps {
  configId: string;
  configName: string;
  states: StateMachineState[];
  transitions: StateMachineTransition[];
}

type PanelView = "configure" | "history" | "report";

// =============================================================================
// Helpers
// =============================================================================

const STATUS_STYLES: Record<string, { bg: string; text: string; icon: typeof CheckCircle2 }> = {
  passed: { bg: "bg-green-500/10", text: "text-green-400", icon: CheckCircle2 },
  failed: { bg: "bg-red-500/10", text: "text-red-400", icon: XCircle },
  error: { bg: "bg-red-500/10", text: "text-red-400", icon: XCircle },
  in_progress: { bg: "bg-blue-500/10", text: "text-blue-400", icon: Loader2 },
  pending: { bg: "bg-gray-500/10", text: "text-text-muted", icon: Clock },
  skipped: { bg: "bg-amber-500/10", text: "text-amber-400", icon: Clock },
};

function getStatusStyle(status: ExplorationStatus | string) {
  return STATUS_STYLES[status] ?? STATUS_STYLES.pending!;
}

function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  if (ms < 60000) return `${(ms / 1000).toFixed(1)}s`;
  const mins = Math.floor(ms / 60000);
  const secs = Math.round((ms % 60000) / 1000);
  return `${mins}m ${secs}s`;
}

// =============================================================================
// Component
// =============================================================================

export function DiscoveryPanel({ configId, configName, states, transitions }: DiscoveryPanelProps) {
  const explorer = useStateExplorer();
  const [view, setView] = useState<PanelView>("configure");
  const [showOptions, setShowOptions] = useState(false);
  const [lastResult, setLastResult] = useState<ExplorationResult | null>(null);

  // Config state
  const [strategy, setStrategy] = useState<ExplorationConfig["strategy"]>("smoke_test");
  const [maxStates, setMaxStates] = useState(0);
  const [captureScreenshots, setCaptureScreenshots] = useState(true);
  const [stopOnFirstFailure, setStopOnFirstFailure] = useState(false);
  const [stateDelayMs, setStateDelayMs] = useState(500);

  const buildConfig = useCallback(
    (): ExplorationConfig => ({
      config_path: `sm-config://${configId}`,
      strategy,
      max_states: maxStates,
      max_duration_seconds: 0,
      target_state_ids: [],
      target_transition_ids: [],
      capture_screenshots: captureScreenshots,
      capture_transition_screenshots: false,
      state_delay_ms: stateDelayMs,
      stop_on_first_failure: stopOnFirstFailure,
    }),
    [configId, strategy, maxStates, captureScreenshots, stopOnFirstFailure, stateDelayMs],
  );

  const handlePreview = useCallback(async () => {
    await explorer.previewPlan(buildConfig());
  }, [explorer, buildConfig]);

  const handleStart = useCallback(async () => {
    const result = await explorer.startExploration(buildConfig());
    if (result) {
      setLastResult(result);
    }
  }, [explorer, buildConfig]);

  const handleViewReport = useCallback(
    async (runId: string) => {
      await explorer.loadReport(runId);
      setView("report");
    },
    [explorer],
  );

  // ---------------------------------------------------------------------------
  // Configure View
  // ---------------------------------------------------------------------------

  if (view === "configure") {
    return (
      <div className="p-4 space-y-4">
        <div className="flex items-center justify-between">
          <h3 className="text-sm font-semibold text-text-primary">State Explorer</h3>
          <button
            onClick={() => {
              setView("history");
              explorer.refreshHistory();
            }}
            className="flex items-center gap-1.5 px-2.5 py-1 text-xs text-text-secondary hover:text-text-primary hover:bg-bg-tertiary rounded"
          >
            <History className="w-3.5 h-3.5" />
            History
          </button>
        </div>

        <div className="text-xs text-text-muted">
          Config: <span className="text-text-secondary font-medium">{configName}</span>
          {" \u00B7 "}
          {states.length} states, {transitions.length} transitions
        </div>

        {/* Strategy selector */}
        <div className="space-y-1.5">
          <label className="text-xs font-medium text-text-secondary">Strategy</label>
          <select
            value={strategy}
            onChange={(e) => setStrategy(e.target.value as ExplorationConfig["strategy"])}
            className="w-full px-2 py-1.5 text-sm bg-bg-tertiary border border-border-secondary rounded text-text-primary"
          >
            {explorer.strategies.length > 0 ? (
              explorer.strategies.map((s) => (
                <option key={s.id} value={s.id}>
                  {s.name} — {s.description}
                </option>
              ))
            ) : (
              <>
                <option value="smoke_test">Smoke Test</option>
                <option value="exhaustive">Exhaustive</option>
                <option value="regression">Regression</option>
                <option value="random_walk">Random Walk</option>
                <option value="targeted">Targeted</option>
              </>
            )}
          </select>
        </div>

        {/* Advanced options toggle */}
        <button
          onClick={() => setShowOptions(!showOptions)}
          className="flex items-center gap-1.5 text-xs text-text-secondary hover:text-text-primary"
        >
          {showOptions ? <ChevronDown className="w-3 h-3" /> : <ChevronRight className="w-3 h-3" />}
          <Settings2 className="w-3 h-3" />
          Advanced Options
        </button>

        {showOptions && (
          <div className="space-y-3 pl-2 border-l-2 border-border-secondary">
            <div className="space-y-1">
              <label className="text-xs text-text-secondary">Max states (0 = unlimited)</label>
              <input
                type="number"
                min={0}
                value={maxStates}
                onChange={(e) => setMaxStates(parseInt(e.target.value) || 0)}
                className="w-full px-2 py-1 text-sm bg-bg-tertiary border border-border-secondary rounded text-text-primary"
              />
            </div>
            <div className="space-y-1">
              <label className="text-xs text-text-secondary">State delay (ms)</label>
              <input
                type="number"
                min={0}
                step={100}
                value={stateDelayMs}
                onChange={(e) => setStateDelayMs(parseInt(e.target.value) || 500)}
                className="w-full px-2 py-1 text-sm bg-bg-tertiary border border-border-secondary rounded text-text-primary"
              />
            </div>
            <label className="flex items-center gap-2 text-xs text-text-secondary cursor-pointer">
              <input
                type="checkbox"
                checked={captureScreenshots}
                onChange={(e) => setCaptureScreenshots(e.target.checked)}
                className="rounded"
              />
              Capture screenshots
            </label>
            <label className="flex items-center gap-2 text-xs text-text-secondary cursor-pointer">
              <input
                type="checkbox"
                checked={stopOnFirstFailure}
                onChange={(e) => setStopOnFirstFailure(e.target.checked)}
                className="rounded"
              />
              Stop on first failure
            </label>
          </div>
        )}

        {/* Action buttons */}
        <div className="flex gap-2">
          <button
            onClick={handlePreview}
            disabled={explorer.previewLoading}
            className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium text-text-primary border border-border-secondary hover:border-border-primary rounded disabled:opacity-50"
          >
            {explorer.previewLoading ? (
              <Loader2 className="w-3.5 h-3.5 animate-spin" />
            ) : (
              <Eye className="w-3.5 h-3.5" />
            )}
            Preview Plan
          </button>
          <button
            onClick={handleStart}
            disabled={explorer.explorationRunning}
            className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium text-white bg-brand-primary hover:bg-brand-primary/90 rounded disabled:opacity-50"
          >
            {explorer.explorationRunning ? (
              <Loader2 className="w-3.5 h-3.5 animate-spin" />
            ) : (
              <Play className="w-3.5 h-3.5" />
            )}
            {explorer.explorationRunning ? "Running\u2026" : "Start Exploration"}
          </button>
        </div>

        {/* Plan preview */}
        {explorer.currentPlan && (
          <div className="p-3 bg-bg-tertiary border border-border-secondary rounded space-y-1.5">
            <h4 className="text-xs font-medium text-text-secondary">Exploration Plan</h4>
            <div className="grid grid-cols-2 gap-2 text-xs">
              <div>
                <span className="text-text-muted">States to visit:</span>{" "}
                <span className="text-text-primary font-medium">
                  {explorer.currentPlan.states_to_visit}
                </span>
                <span className="text-text-muted">
                  {" "}
                  / {explorer.currentPlan.total_states_in_config}
                </span>
              </div>
              <div>
                <span className="text-text-muted">Transitions:</span>{" "}
                <span className="text-text-primary font-medium">
                  {explorer.currentPlan.transitions_to_verify}
                </span>
                <span className="text-text-muted">
                  {" "}
                  / {explorer.currentPlan.total_transitions_in_config}
                </span>
              </div>
              <div>
                <span className="text-text-muted">Estimated cost:</span>{" "}
                <span className="text-text-primary font-medium">
                  {explorer.currentPlan.estimated_cost.toFixed(1)}
                </span>
              </div>
              <div>
                <span className="text-text-muted">Strategy:</span>{" "}
                <span className="text-text-primary font-medium">
                  {explorer.currentPlan.strategy}
                </span>
              </div>
            </div>
          </div>
        )}

        {/* Last result */}
        {lastResult && (
          <ExplorationResultCard result={lastResult} onViewReport={handleViewReport} />
        )}

        {/* Error */}
        {explorer.error && (
          <div className="p-2.5 text-xs text-red-400 bg-red-500/10 border border-red-500/20 rounded">
            {explorer.error}
            <button onClick={explorer.clearError} className="ml-2 underline">
              dismiss
            </button>
          </div>
        )}
      </div>
    );
  }

  // ---------------------------------------------------------------------------
  // History View
  // ---------------------------------------------------------------------------

  if (view === "history") {
    return (
      <div className="p-4 space-y-4">
        <div className="flex items-center justify-between">
          <h3 className="text-sm font-semibold text-text-primary">Exploration History</h3>
          <div className="flex items-center gap-1.5">
            <button
              onClick={() => explorer.refreshHistory()}
              disabled={explorer.historyLoading}
              className="p-1 text-text-secondary hover:text-text-primary hover:bg-bg-tertiary rounded"
            >
              <RefreshCw
                className={`w-3.5 h-3.5 ${explorer.historyLoading ? "animate-spin" : ""}`}
              />
            </button>
            <button
              onClick={() => setView("configure")}
              className="flex items-center gap-1.5 px-2.5 py-1 text-xs text-text-secondary hover:text-text-primary hover:bg-bg-tertiary rounded"
            >
              <ArrowLeft className="w-3.5 h-3.5" />
              Configure
            </button>
          </div>
        </div>

        {explorer.history.length === 0 ? (
          <p className="text-xs text-text-muted italic text-center py-8">No exploration runs yet</p>
        ) : (
          <div className="space-y-2">
            {explorer.history.map((item) => (
              <div
                key={item.run_id}
                className="p-2.5 bg-bg-tertiary border border-border-secondary rounded hover:border-border-primary transition-colors"
              >
                <div className="flex items-center justify-between mb-1">
                  <span className="text-xs font-medium text-text-primary truncate">
                    {item.config_name || item.run_id.slice(0, 8)}
                  </span>
                  <span className="text-[10px] text-text-muted">{item.strategy}</span>
                </div>
                <div className="text-[10px] text-text-muted mb-1.5">
                  {item.started_at ? new Date(item.started_at).toLocaleString() : "\u2014"}
                </div>
                {item.summary && (
                  <p className="text-[10px] text-text-secondary mb-1.5 line-clamp-2">
                    {item.summary}
                  </p>
                )}
                <button
                  onClick={() => handleViewReport(item.run_id)}
                  className="text-[10px] text-brand-primary hover:text-brand-primary/80"
                >
                  View Report \u2192
                </button>
              </div>
            ))}
          </div>
        )}

        {explorer.history.length > 0 && (
          <button
            onClick={() => explorer.clearHistory(30)}
            className="flex items-center gap-1.5 text-xs text-text-muted hover:text-red-400"
          >
            <Trash2 className="w-3 h-3" />
            Clear runs older than 30 days
          </button>
        )}
      </div>
    );
  }

  // ---------------------------------------------------------------------------
  // Report View
  // ---------------------------------------------------------------------------

  if (view === "report" && explorer.selectedReport) {
    const report = explorer.selectedReport;
    return (
      <div className="p-4 space-y-4">
        <div className="flex items-center justify-between">
          <h3 className="text-sm font-semibold text-text-primary">Exploration Report</h3>
          <button
            onClick={() => {
              explorer.clearReport();
              setView("history");
            }}
            className="flex items-center gap-1.5 px-2.5 py-1 text-xs text-text-secondary hover:text-text-primary hover:bg-bg-tertiary rounded"
          >
            <ArrowLeft className="w-3.5 h-3.5" />
            Back
          </button>
        </div>

        {/* Summary stats */}
        <div className="grid grid-cols-2 gap-2">
          {[
            {
              label: "States Passed",
              value: `${report.states_passed}/${report.states_visited}`,
              color: report.states_failed > 0 ? "text-amber-400" : "text-green-400",
            },
            {
              label: "Transitions Passed",
              value: `${report.transitions_passed}/${report.transitions_verified}`,
              color: report.transitions_failed > 0 ? "text-amber-400" : "text-green-400",
            },
            {
              label: "Duration",
              value: formatDuration(report.total_duration_ms),
              color: "text-text-primary",
            },
            {
              label: "Strategy",
              value: report.strategy,
              color: "text-text-primary",
            },
          ].map((stat) => (
            <div
              key={stat.label}
              className="p-2 bg-bg-tertiary border border-border-secondary rounded"
            >
              <div className="text-[10px] text-text-muted">{stat.label}</div>
              <div className={`text-sm font-semibold ${stat.color}`}>{stat.value}</div>
            </div>
          ))}
        </div>

        {/* State results */}
        {report.state_explorations.length > 0 && (
          <div className="space-y-1.5">
            <h4 className="text-xs font-medium text-text-secondary">State Results</h4>
            {report.state_explorations.map((se) => {
              const style = getStatusStyle(se.status);
              const StatusIcon = style.icon;
              return (
                <div
                  key={se.state_id}
                  className="flex items-center gap-2 p-2 bg-bg-tertiary border border-border-secondary rounded text-xs"
                >
                  <StatusIcon className={`w-3.5 h-3.5 shrink-0 ${style.text}`} />
                  <span className="text-text-primary font-medium truncate flex-1">
                    {se.state_name}
                  </span>
                  <span className="text-text-muted shrink-0">{formatDuration(se.duration_ms)}</span>
                </div>
              );
            })}
          </div>
        )}

        {/* Transition results */}
        {report.transition_explorations.length > 0 && (
          <div className="space-y-1.5">
            <h4 className="text-xs font-medium text-text-secondary">Transition Results</h4>
            {report.transition_explorations.map((te, i) => {
              const style = getStatusStyle(te.status);
              const StatusIcon = style.icon;
              return (
                <div
                  key={`${te.transition_id}-${i}`}
                  className="flex items-center gap-2 p-2 bg-bg-tertiary border border-border-secondary rounded text-xs"
                >
                  <StatusIcon className={`w-3.5 h-3.5 shrink-0 ${style.text}`} />
                  <span className="text-text-primary truncate flex-1">
                    {te.from_state_id} \u2192 {te.to_state_id}
                  </span>
                  <span className="text-text-muted shrink-0">{formatDuration(te.duration_ms)}</span>
                </div>
              );
            })}
          </div>
        )}

        {report.summary && <p className="text-xs text-text-secondary">{report.summary}</p>}
      </div>
    );
  }

  // Fallback — report loading or empty state
  return (
    <div className="p-4 flex items-center justify-center">
      {explorer.reportLoading ? (
        <Loader2 className="w-5 h-5 text-text-muted animate-spin" />
      ) : (
        <button
          onClick={() => setView("configure")}
          className="text-xs text-text-secondary hover:text-text-primary"
        >
          \u2190 Back to Configure
        </button>
      )}
    </div>
  );
}

// =============================================================================
// Sub-components
// =============================================================================

function ExplorationResultCard({
  result,
  onViewReport,
}: {
  result: ExplorationResult;
  onViewReport: (runId: string) => void;
}) {
  const isPassed = result.overall_status === "passed";
  return (
    <div
      className={`p-3 rounded border space-y-1.5 ${
        isPassed ? "bg-green-500/5 border-green-500/20" : "bg-red-500/5 border-red-500/20"
      }`}
    >
      <div className="flex items-center justify-between">
        <h4 className="text-xs font-medium text-text-secondary">Last Result</h4>
        <StatusBadge status={result.overall_status} />
      </div>
      <div className="grid grid-cols-2 gap-2 text-xs">
        <div>
          <span className="text-text-muted">States:</span>{" "}
          <span className="text-green-400">{result.states_passed}\u2713</span>
          {result.states_failed > 0 && (
            <span className="text-red-400"> {result.states_failed}\u2717</span>
          )}
          <span className="text-text-muted"> / {result.states_visited}</span>
        </div>
        <div>
          <span className="text-text-muted">Transitions:</span>{" "}
          <span className="text-green-400">{result.transitions_passed}\u2713</span>
          {result.transitions_failed > 0 && (
            <span className="text-red-400"> {result.transitions_failed}\u2717</span>
          )}
          <span className="text-text-muted"> / {result.transitions_verified}</span>
        </div>
        <div className="col-span-2">
          <span className="text-text-muted">Duration:</span>{" "}
          <span className="text-text-primary">{formatDuration(result.total_duration_ms)}</span>
        </div>
      </div>
      {result.run_id && (
        <button
          onClick={() => onViewReport(result.run_id)}
          className="text-xs text-brand-primary hover:text-brand-primary/80"
        >
          View Full Report \u2192
        </button>
      )}
    </div>
  );
}

function StatusBadge({ status }: { status: string }) {
  const style = getStatusStyle(status);
  return (
    <span className={`text-[10px] font-medium px-1.5 py-0.5 rounded ${style.bg} ${style.text}`}>
      {status}
    </span>
  );
}
