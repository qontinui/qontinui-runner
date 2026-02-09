/**
 * RunPlanTab.tsx
 *
 * Form page for creating and submitting structured implementation plans
 * to the runner's execute-plan endpoint. Supports both manual form entry
 * and raw JSON import modes. Shows recent plan execution history.
 */

import { useState, useEffect, useCallback } from "react";
import {
  ListChecks,
  Plus,
  Trash2,
  Play,
  Loader2,
  Code,
  FileText,
  Clock,
  CheckCircle2,
  XCircle,
  AlertCircle,
  StopCircle,
  ExternalLink,
} from "lucide-react";
import { Button } from "../ui";
import { getStatusColors } from "@/design-system";
import type { TaskRun, TaskRunStatus } from "@/types/aiData";

const API_BASE = "http://localhost:9876";
const REQUEST_TIMEOUT_MS = 30_000;
const HISTORY_POLL_INTERVAL_MS = 10_000;
const HISTORY_LIMIT = 20;

interface Phase {
  name: string;
  prompt: string;
}

interface RunPlanTabProps {
  onNavigateToActive: () => void;
  onGoToRecap?: (taskRunId: string) => void;
}

interface PlanPayload {
  plan_name: string;
  plan_overview: string;
  phases: Phase[];
  next_steps_sweep: boolean;
  max_next_steps_iterations: number;
  timeout_seconds: number | null;
}

// ============================================================================
// Helpers
// ============================================================================

function formatRelativeTime(dateStr: string): string {
  const date = new Date(dateStr);
  const now = new Date();
  const diffMs = now.getTime() - date.getTime();
  const diffSec = Math.floor(diffMs / 1000);
  if (diffSec < 60) return "just now";
  const diffMin = Math.floor(diffSec / 60);
  if (diffMin < 60) return `${diffMin}m ago`;
  const diffHr = Math.floor(diffMin / 60);
  if (diffHr < 24) return `${diffHr}h ago`;
  const diffDay = Math.floor(diffHr / 24);
  return `${diffDay}d ago`;
}

function formatDuration(startStr: string, endStr: string | null | undefined): string {
  if (!endStr) return "in progress";
  const start = new Date(startStr).getTime();
  const end = new Date(endStr).getTime();
  const diffSec = Math.floor((end - start) / 1000);
  if (diffSec < 60) return `${diffSec}s`;
  const min = Math.floor(diffSec / 60);
  const sec = diffSec % 60;
  if (min < 60) return `${min}m ${sec}s`;
  const hr = Math.floor(min / 60);
  return `${hr}h ${min % 60}m`;
}

function getStatusIcon(status: TaskRunStatus) {
  switch (status) {
    case "running":
      return <Loader2 className="w-3.5 h-3.5 animate-spin" />;
    case "complete":
      return <CheckCircle2 className="w-3.5 h-3.5" />;
    case "failed":
      return <XCircle className="w-3.5 h-3.5" />;
    case "stopped":
      return <StopCircle className="w-3.5 h-3.5" />;
    default:
      return <AlertCircle className="w-3.5 h-3.5" />;
  }
}

function mapStatusToDesignSystem(status: TaskRunStatus): string {
  switch (status) {
    case "running":
      return "running";
    case "complete":
      return "success";
    case "failed":
      return "error";
    case "stopped":
      return "cancelled";
    default:
      return "pending";
  }
}

// ============================================================================
// Main Component
// ============================================================================

export function RunPlanTab({ onNavigateToActive, onGoToRecap }: RunPlanTabProps) {
  // Form state
  const [planName, setPlanName] = useState("");
  const [planOverview, setPlanOverview] = useState("");
  const [phases, setPhases] = useState<Phase[]>([{ name: "", prompt: "" }]);
  const [nextStepsSweep, setNextStepsSweep] = useState(true);
  const [maxSweepIterations, setMaxSweepIterations] = useState(5);
  const [timeoutSeconds, setTimeoutSeconds] = useState("");
  const [jsonMode, setJsonMode] = useState(false);
  const [jsonInput, setJsonInput] = useState("");
  const [isSubmitting, setIsSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Plan history state
  const [planRuns, setPlanRuns] = useState<TaskRun[]>([]);
  const [historyLoading, setHistoryLoading] = useState(true);

  const clearError = useCallback(() => {
    if (error) setError(null);
  }, [error]);

  // ---------------------------------------------------------------------------
  // Plan history polling
  // ---------------------------------------------------------------------------
  const fetchPlanHistory = useCallback(async () => {
    try {
      const res = await fetch(`${API_BASE}/task-runs?workflow_type=plan&limit=${HISTORY_LIMIT}`);
      if (res.ok) {
        const runs: TaskRun[] = await res.json();
        setPlanRuns(runs);
      }
    } catch {
      // Silently ignore fetch errors for history polling
    } finally {
      setHistoryLoading(false);
    }
  }, []);

  useEffect(() => {
    fetchPlanHistory();
    const interval = setInterval(fetchPlanHistory, HISTORY_POLL_INTERVAL_MS);
    return () => clearInterval(interval);
  }, [fetchPlanHistory]);

  // ---------------------------------------------------------------------------
  // Form helpers
  // ---------------------------------------------------------------------------
  const addPhase = () => {
    setPhases((prev) => [...prev, { name: "", prompt: "" }]);
  };

  const removePhase = (index: number) => {
    setPhases((prev) => prev.filter((_, i) => i !== index));
  };

  const updatePhase = (index: number, field: keyof Phase, value: string) => {
    setPhases((prev) =>
      prev.map((phase, i) => (i === index ? { ...phase, [field]: value } : phase)),
    );
    clearError();
  };

  const buildPayload = (): PlanPayload | null => {
    if (jsonMode) {
      try {
        const parsed = JSON.parse(jsonInput);
        // Validate required fields
        if (!parsed.plan_name || !parsed.plan_overview || !Array.isArray(parsed.phases)) {
          setError("JSON must contain plan_name, plan_overview, and phases array.");
          return null;
        }
        for (const phase of parsed.phases) {
          if (!phase.name || !phase.prompt) {
            setError("Each phase must have a name and prompt.");
            return null;
          }
        }
        return {
          plan_name: parsed.plan_name,
          plan_overview: parsed.plan_overview,
          phases: parsed.phases,
          next_steps_sweep: parsed.next_steps_sweep ?? true,
          max_next_steps_iterations: parsed.max_next_steps_iterations ?? 5,
          timeout_seconds: parsed.timeout_seconds ?? null,
        };
      } catch {
        setError("Invalid JSON. Please check the format and try again.");
        return null;
      }
    }

    // Validate form fields
    if (!planName.trim()) {
      setError("Plan name is required.");
      return null;
    }
    if (!planOverview.trim()) {
      setError("Plan overview is required.");
      return null;
    }
    if (phases.length === 0) {
      setError("At least one phase is required.");
      return null;
    }
    for (let i = 0; i < phases.length; i++) {
      if (!phases[i].name.trim()) {
        setError(`Phase ${i + 1} name is required.`);
        return null;
      }
      if (!phases[i].prompt.trim()) {
        setError(`Phase ${i + 1} prompt is required.`);
        return null;
      }
    }

    const timeout = timeoutSeconds.trim() ? parseInt(timeoutSeconds.trim(), 10) : null;
    if (timeout !== null && (isNaN(timeout) || timeout <= 0)) {
      setError("Timeout must be a positive number.");
      return null;
    }

    return {
      plan_name: planName.trim(),
      plan_overview: planOverview.trim(),
      phases: phases.map((p) => ({
        name: p.name.trim(),
        prompt: p.prompt.trim(),
      })),
      next_steps_sweep: nextStepsSweep,
      max_next_steps_iterations: maxSweepIterations,
      timeout_seconds: timeout,
    };
  };

  const handleSubmit = async () => {
    setError(null);
    const payload = buildPayload();
    if (!payload) return;

    setIsSubmitting(true);
    const controller = new AbortController();
    const timeoutId = setTimeout(() => controller.abort(), REQUEST_TIMEOUT_MS);

    try {
      const response = await fetch(`${API_BASE}/execute-plan`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(payload),
        signal: controller.signal,
      });

      if (!response.ok) {
        const errorData = await response.json().catch(() => null);
        const detail = errorData?.detail || errorData?.message || `status ${response.status}`;
        if (response.status >= 500) {
          setError(`Server error: ${detail}. Try again or check runner logs.`);
        } else {
          setError(`Invalid request: ${detail}`);
        }
        return;
      }

      // Refresh history immediately after successful submission
      fetchPlanHistory();
      onNavigateToActive();
    } catch (err) {
      if (err instanceof DOMException && err.name === "AbortError") {
        setError("Request timed out. The runner may be busy or unresponsive.");
      } else if (err instanceof TypeError) {
        // fetch throws TypeError for network failures
        setError("Cannot connect to runner on port 9876. Is the runner application running?");
      } else {
        setError(
          err instanceof Error
            ? `Unexpected error: ${err.message}`
            : "An unexpected error occurred.",
        );
      }
    } finally {
      clearTimeout(timeoutId);
      setIsSubmitting(false);
    }
  };

  return (
    <div className="flex flex-col items-center w-full h-full overflow-y-auto p-6">
      <div className="w-full max-w-3xl space-y-6">
        {/* Header */}
        <div className="flex items-center gap-3">
          <div className="p-2.5 rounded-lg bg-primary/10">
            <ListChecks className="w-6 h-6 text-primary" />
          </div>
          <div>
            <h1 className="text-xl font-semibold text-foreground">Run Plan</h1>
            <p className="text-sm text-muted-foreground">
              Create a structured implementation plan with phases and submit it for execution.
            </p>
          </div>
        </div>

        {/* Mode toggle */}
        <div className="flex items-center gap-2 border-b border-border pb-4">
          <button
            type="button"
            onClick={() => {
              setJsonMode(false);
              clearError();
            }}
            className={`flex items-center gap-1.5 px-3 py-1.5 text-sm rounded-md transition-colors ${
              !jsonMode
                ? "bg-primary/10 text-primary font-medium"
                : "text-muted-foreground hover:text-foreground hover:bg-muted/50"
            }`}
          >
            <FileText className="w-4 h-4" />
            Form
          </button>
          <button
            type="button"
            onClick={() => {
              setJsonMode(true);
              clearError();
            }}
            className={`flex items-center gap-1.5 px-3 py-1.5 text-sm rounded-md transition-colors ${
              jsonMode
                ? "bg-primary/10 text-primary font-medium"
                : "text-muted-foreground hover:text-foreground hover:bg-muted/50"
            }`}
          >
            <Code className="w-4 h-4" />
            JSON Import
          </button>
        </div>

        {/* Error display */}
        {error && (
          <div className="px-4 py-3 rounded-lg bg-destructive/10 border border-destructive/30 text-destructive text-sm">
            {error}
          </div>
        )}

        {jsonMode ? (
          /* JSON import mode */
          <div className="space-y-3">
            <label className="block text-sm font-medium text-foreground">Plan JSON</label>
            <textarea
              value={jsonInput}
              onChange={(e) => {
                setJsonInput(e.target.value);
                clearError();
              }}
              placeholder={`{
  "plan_name": "My Plan",
  "plan_overview": "What this plan does...",
  "phases": [
    { "name": "Phase 1", "prompt": "Do something..." },
    { "name": "Phase 2", "prompt": "Do something else..." }
  ],
  "next_steps_sweep": true,
  "timeout_seconds": 300
}`}
              className="w-full h-80 px-3 py-2 rounded-lg bg-muted border border-border text-foreground text-sm font-mono placeholder:text-muted-foreground/50 focus:outline-none focus:ring-2 focus:ring-primary/50 resize-y"
            />
            <p className="text-xs text-muted-foreground">
              Paste a full plan JSON object. Required fields: plan_name, plan_overview, phases
              (array of {"{"} name, prompt {"}"}).
            </p>
          </div>
        ) : (
          /* Form mode */
          <div className="space-y-6">
            {/* Plan name */}
            <div className="space-y-1.5">
              <label className="block text-sm font-medium text-foreground">
                Plan Name <span className="text-destructive">*</span>
              </label>
              <input
                type="text"
                value={planName}
                onChange={(e) => {
                  setPlanName(e.target.value);
                  clearError();
                }}
                placeholder="e.g., Refactor authentication module"
                className="w-full px-3 py-2 rounded-lg bg-muted border border-border text-foreground text-sm placeholder:text-muted-foreground/50 focus:outline-none focus:ring-2 focus:ring-primary/50"
              />
            </div>

            {/* Plan overview */}
            <div className="space-y-1.5">
              <label className="block text-sm font-medium text-foreground">
                Plan Overview <span className="text-destructive">*</span>
              </label>
              <textarea
                value={planOverview}
                onChange={(e) => {
                  setPlanOverview(e.target.value);
                  clearError();
                }}
                placeholder="Describe the overall goal and context for this plan..."
                rows={3}
                className="w-full px-3 py-2 rounded-lg bg-muted border border-border text-foreground text-sm placeholder:text-muted-foreground/50 focus:outline-none focus:ring-2 focus:ring-primary/50 resize-y"
              />
            </div>

            {/* Phases */}
            <div className="space-y-3">
              <div className="flex items-center justify-between">
                <label className="text-sm font-medium text-foreground">
                  Phases <span className="text-destructive">*</span>
                </label>
                <button
                  type="button"
                  onClick={addPhase}
                  className="flex items-center gap-1 px-2.5 py-1 text-xs font-medium rounded-md text-primary hover:bg-primary/10 transition-colors"
                >
                  <Plus className="w-3.5 h-3.5" />
                  Add Phase
                </button>
              </div>

              <div className="space-y-3">
                {phases.map((phase, index) => (
                  <div
                    key={index}
                    className="relative rounded-lg border border-border bg-card p-4 space-y-3"
                  >
                    {/* Phase header */}
                    <div className="flex items-center justify-between">
                      <span className="text-xs font-semibold text-muted-foreground uppercase tracking-wider">
                        Phase {index + 1}
                      </span>
                      {phases.length > 1 && (
                        <button
                          type="button"
                          onClick={() => removePhase(index)}
                          className="p-1 rounded-md text-muted-foreground hover:text-destructive hover:bg-destructive/10 transition-colors"
                          title="Remove phase"
                        >
                          <Trash2 className="w-3.5 h-3.5" />
                        </button>
                      )}
                    </div>

                    {/* Phase name */}
                    <input
                      type="text"
                      value={phase.name}
                      onChange={(e) => updatePhase(index, "name", e.target.value)}
                      placeholder="Phase name"
                      className="w-full px-3 py-1.5 rounded-md bg-muted border border-border text-foreground text-sm placeholder:text-muted-foreground/50 focus:outline-none focus:ring-2 focus:ring-primary/50"
                    />

                    {/* Phase prompt */}
                    <textarea
                      value={phase.prompt}
                      onChange={(e) => updatePhase(index, "prompt", e.target.value)}
                      placeholder="Describe what this phase should accomplish..."
                      rows={3}
                      className="w-full px-3 py-1.5 rounded-md bg-muted border border-border text-foreground text-sm placeholder:text-muted-foreground/50 focus:outline-none focus:ring-2 focus:ring-primary/50 resize-y"
                    />
                  </div>
                ))}
              </div>
            </div>

            {/* Options row */}
            <div className="flex flex-col sm:flex-row gap-4 pt-2">
              {/* Next steps sweep toggle */}
              <label className="flex items-center gap-2.5 cursor-pointer">
                <input
                  type="checkbox"
                  checked={nextStepsSweep}
                  onChange={(e) => setNextStepsSweep(e.target.checked)}
                  className="w-4 h-4 rounded border-border text-primary focus:ring-primary/50"
                />
                <span className="text-sm text-foreground">Next steps sweep</span>
              </label>

              {/* Max sweep iterations (visible when sweep enabled) */}
              {nextStepsSweep && (
                <div className="flex items-center gap-2">
                  <label className="text-sm text-foreground whitespace-nowrap">
                    Max iterations
                  </label>
                  <input
                    type="number"
                    value={maxSweepIterations}
                    onChange={(e) => {
                      const val = parseInt(e.target.value, 10);
                      if (!isNaN(val)) setMaxSweepIterations(Math.max(1, Math.min(20, val)));
                    }}
                    min={1}
                    max={20}
                    className="w-20 px-3 py-1.5 rounded-md bg-muted border border-border text-foreground text-sm placeholder:text-muted-foreground/50 focus:outline-none focus:ring-2 focus:ring-primary/50"
                  />
                </div>
              )}

              {/* Timeout */}
              <div className="flex items-center gap-2">
                <label className="text-sm text-foreground whitespace-nowrap">
                  Timeout (seconds)
                </label>
                <input
                  type="number"
                  value={timeoutSeconds}
                  onChange={(e) => setTimeoutSeconds(e.target.value)}
                  placeholder="Default"
                  min={1}
                  className="w-28 px-3 py-1.5 rounded-md bg-muted border border-border text-foreground text-sm placeholder:text-muted-foreground/50 focus:outline-none focus:ring-2 focus:ring-primary/50"
                />
              </div>
            </div>
          </div>
        )}

        {/* Submit button */}
        <div className="pt-2 pb-4">
          <Button
            variant="primary"
            size="lg"
            onClick={handleSubmit}
            disabled={isSubmitting}
            className="w-full"
          >
            {isSubmitting ? (
              <>
                <Loader2 className="w-5 h-5 animate-spin" />
                Submitting Plan...
              </>
            ) : (
              <>
                <Play className="w-5 h-5" />
                Execute Plan
              </>
            )}
          </Button>
        </div>

        {/* ================================================================ */}
        {/* Recent Plan Runs */}
        {/* ================================================================ */}
        <div className="border-t border-border pt-6 pb-8 space-y-4">
          <div className="flex items-center gap-2">
            <Clock className="w-4 h-4 text-muted-foreground" />
            <h2 className="text-sm font-semibold text-foreground">Recent Plan Runs</h2>
          </div>

          {historyLoading ? (
            <div className="flex items-center justify-center py-8 text-muted-foreground text-sm">
              <Loader2 className="w-4 h-4 animate-spin mr-2" />
              Loading history...
            </div>
          ) : planRuns.length === 0 ? (
            <p className="text-sm text-muted-foreground py-4 text-center">
              No plan runs yet. Submit a plan above to get started.
            </p>
          ) : (
            <div className="space-y-2">
              {planRuns.map((run) => {
                const colors = getStatusColors(mapStatusToDesignSystem(run.status));
                const summaryExcerpt = run.summary
                  ? run.summary.length > 120
                    ? run.summary.slice(0, 120) + "..."
                    : run.summary
                  : null;

                return (
                  <button
                    key={run.id}
                    onClick={() => onGoToRecap?.(run.id)}
                    disabled={!onGoToRecap}
                    className={`w-full text-left rounded-lg border ${colors.border} bg-card hover:bg-muted/50 transition-colors p-3 space-y-1.5 ${
                      onGoToRecap ? "cursor-pointer" : "cursor-default"
                    }`}
                  >
                    {/* Top row: name, status badge, time */}
                    <div className="flex items-center justify-between gap-2">
                      <div className="flex items-center gap-2 min-w-0">
                        <span className="text-sm font-medium text-foreground truncate">
                          {run.task_name}
                        </span>
                      </div>
                      <div className="flex items-center gap-2 flex-shrink-0">
                        <span
                          className={`inline-flex items-center gap-1 px-2 py-0.5 rounded-full text-xs font-medium ${colors.bg} ${colors.text}`}
                        >
                          {getStatusIcon(run.status)}
                          {run.status}
                        </span>
                        <span className="text-xs text-muted-foreground whitespace-nowrap">
                          {formatRelativeTime(run.created_at)}
                        </span>
                      </div>
                    </div>

                    {/* Second row: duration, goal achieved, view link */}
                    <div className="flex items-center gap-3 text-xs text-muted-foreground">
                      <span>{formatDuration(run.created_at, run.completed_at)}</span>
                      {run.goal_achieved != null && (
                        <span
                          className={`inline-flex items-center gap-0.5 ${
                            run.goal_achieved ? "text-green-500" : "text-red-400"
                          }`}
                        >
                          {run.goal_achieved ? (
                            <CheckCircle2 className="w-3 h-3" />
                          ) : (
                            <XCircle className="w-3 h-3" />
                          )}
                          {run.goal_achieved ? "Goal achieved" : "Goal not achieved"}
                        </span>
                      )}
                      {onGoToRecap && (
                        <span className="ml-auto inline-flex items-center gap-0.5 text-primary hover:underline">
                          <ExternalLink className="w-3 h-3" />
                          View recap
                        </span>
                      )}
                    </div>

                    {/* Summary excerpt */}
                    {summaryExcerpt && (
                      <p className="text-xs text-muted-foreground line-clamp-2">{summaryExcerpt}</p>
                    )}
                  </button>
                );
              })}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
