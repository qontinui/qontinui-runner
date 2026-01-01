/**
 * VerificationReport.tsx
 *
 * Detailed view of a verification report with state verifications,
 * discrepancies, screenshots, and AI analysis prompt.
 */

import { useState, useCallback, useEffect } from "react";
import {
  ArrowLeft,
  CheckCircle,
  XCircle,
  AlertCircle,
  Clock,
  Image,
  ChevronDown,
  ChevronRight,
  Bot,
  Copy,
  Loader2,
  FileText,
  ArrowRight,
  SkipForward,
} from "lucide-react";
import { useVerificationAgent } from "../../hooks/useVerificationAgent";
import type {
  StateVerification as StateVerificationType,
  TransitionVerification as TransitionVerificationType,
  VerificationStatus,
} from "../../types/verification-agent";

interface VerificationReportProps {
  /** Run ID to display */
  runId: string;
  /** Callback to go back to history */
  onBack: () => void;
  /** Callback to show AI analysis prompt */
  onShowAiPrompt?: (prompt: string) => void;
}

export function VerificationReport({ runId, onBack, onShowAiPrompt }: VerificationReportProps) {
  const { selectedReport, reportLoading, loadReport, getAnalysisPrompt, error } =
    useVerificationAgent();

  const [expandedStates, setExpandedStates] = useState<Set<string>>(new Set());
  const [expandedTransitions, setExpandedTransitions] = useState<Set<string>>(new Set());
  const [showAiPrompt, setShowAiPrompt] = useState(false);
  const [aiPrompt, setAiPrompt] = useState<string | null>(null);
  const [aiPromptLoading, setAiPromptLoading] = useState(false);

  // Load report when runId changes
  useEffect(() => {
    loadReport(runId);
  }, [runId, loadReport]);

  // Toggle state expansion
  const toggleState = useCallback((stateId: string) => {
    setExpandedStates((prev) => {
      const next = new Set(prev);
      if (next.has(stateId)) {
        next.delete(stateId);
      } else {
        next.add(stateId);
      }
      return next;
    });
  }, []);

  // Toggle transition expansion
  const toggleTransition = useCallback((transitionId: string) => {
    setExpandedTransitions((prev) => {
      const next = new Set(prev);
      if (next.has(transitionId)) {
        next.delete(transitionId);
      } else {
        next.add(transitionId);
      }
      return next;
    });
  }, []);

  // Get AI analysis prompt
  const handleGetAiPrompt = useCallback(async () => {
    setAiPromptLoading(true);
    const prompt = await getAnalysisPrompt(runId);
    setAiPrompt(prompt);
    setShowAiPrompt(true);
    setAiPromptLoading(false);
    if (prompt && onShowAiPrompt) {
      onShowAiPrompt(prompt);
    }
  }, [runId, getAnalysisPrompt, onShowAiPrompt]);

  // Copy prompt to clipboard
  const handleCopyPrompt = useCallback(async () => {
    if (aiPrompt) {
      await navigator.clipboard.writeText(aiPrompt);
    }
  }, [aiPrompt]);

  // Status helpers
  const getStatusIcon = (status: VerificationStatus) => {
    switch (status) {
      case "passed":
        return <CheckCircle className="w-4 h-4 text-green-500" />;
      case "failed":
        return <XCircle className="w-4 h-4 text-red-500" />;
      case "skipped":
        return <SkipForward className="w-4 h-4 text-yellow-500" />;
      case "error":
        return <AlertCircle className="w-4 h-4 text-red-500" />;
      case "in_progress":
        return <Loader2 className="w-4 h-4 text-blue-500 animate-spin" />;
      default:
        return <Clock className="w-4 h-4 text-muted-foreground" />;
    }
  };

  const getStatusBadge = (status: VerificationStatus) => {
    const colors: Record<VerificationStatus, string> = {
      passed: "bg-green-500/10 text-green-500",
      failed: "bg-red-500/10 text-red-500",
      skipped: "bg-yellow-500/10 text-yellow-500",
      error: "bg-red-500/10 text-red-500",
      in_progress: "bg-blue-500/10 text-blue-500",
      pending: "bg-muted text-muted-foreground",
    };
    return (
      <span className={`px-2 py-0.5 text-xs rounded-full ${colors[status]}`}>
        {status.replace("_", " ")}
      </span>
    );
  };

  const formatDuration = (ms: number): string => {
    if (ms < 1000) return `${ms}ms`;
    const seconds = Math.floor(ms / 1000);
    if (seconds < 60) return `${seconds}s`;
    const minutes = Math.floor(seconds / 60);
    return `${minutes}m ${seconds % 60}s`;
  };

  if (reportLoading) {
    return (
      <div className="h-full flex flex-col items-center justify-center text-muted-foreground">
        <Loader2 className="w-8 h-8 mb-3 animate-spin" />
        <p>Loading report...</p>
      </div>
    );
  }

  if (error || !selectedReport) {
    return (
      <div className="h-full flex flex-col items-center justify-center">
        <AlertCircle className="w-12 h-12 mb-4 text-destructive opacity-50" />
        <p className="text-lg font-medium text-destructive">Failed to load report</p>
        <p className="text-sm text-muted-foreground mt-2">{error}</p>
        <button
          onClick={onBack}
          className="mt-4 flex items-center gap-2 px-4 py-2 bg-muted hover:bg-muted/80 rounded-lg text-sm"
        >
          <ArrowLeft className="w-4 h-4" />
          Back to History
        </button>
      </div>
    );
  }

  const report = selectedReport;

  return (
    <div className="h-full flex flex-col">
      {/* Header */}
      <div className="flex-shrink-0 p-4 border-b border-border">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-3">
            <button
              onClick={onBack}
              className="p-1.5 hover:bg-muted rounded-lg transition-colors"
              title="Back to history"
            >
              <ArrowLeft className="w-5 h-5" />
            </button>
            <FileText className="w-5 h-5 text-primary" />
            <h2 className="text-lg font-semibold">Verification Report</h2>
            {getStatusBadge(report.overall_status)}
          </div>
          <button
            onClick={handleGetAiPrompt}
            disabled={aiPromptLoading}
            className="flex items-center gap-2 px-3 py-1.5 bg-purple-500/10 text-purple-500 hover:bg-purple-500/20 rounded-lg text-sm transition-colors"
          >
            {aiPromptLoading ? (
              <Loader2 className="w-4 h-4 animate-spin" />
            ) : (
              <Bot className="w-4 h-4" />
            )}
            Get AI Analysis
          </button>
        </div>
      </div>

      {/* Content */}
      <div className="flex-1 overflow-auto p-4 space-y-6">
        {/* Summary Card */}
        <div className="p-4 bg-card rounded-lg border border-border">
          <h3 className="font-medium mb-3">Summary</h3>
          <div className="grid grid-cols-2 lg:grid-cols-4 gap-4 text-sm">
            <div>
              <span className="text-muted-foreground">Config:</span>
              <div className="font-medium truncate">{report.config_name || report.config_path}</div>
            </div>
            <div>
              <span className="text-muted-foreground">Strategy:</span>
              <div className="font-medium">{report.strategy}</div>
            </div>
            <div>
              <span className="text-muted-foreground">Duration:</span>
              <div className="font-medium">{formatDuration(report.total_duration_ms)}</div>
            </div>
            <div>
              <span className="text-muted-foreground">Started:</span>
              <div className="font-medium">{new Date(report.started_at).toLocaleString()}</div>
            </div>
          </div>

          {/* Stats */}
          <div className="mt-4 pt-4 border-t border-border">
            <div className="grid grid-cols-2 lg:grid-cols-4 gap-4">
              <StatCard
                label="States Visited"
                value={`${report.states_visited}/${report.total_states}`}
                color="blue"
              />
              <StatCard label="States Passed" value={report.states_passed} color="green" />
              <StatCard label="States Failed" value={report.states_failed} color="red" />
              <StatCard
                label="Transitions"
                value={`${report.transitions_passed}/${report.transitions_verified}`}
                color="purple"
              />
            </div>
          </div>

          {/* Summary Text */}
          <div className="mt-4 p-3 bg-muted/50 rounded-lg text-sm">{report.summary}</div>
        </div>

        {/* Discrepancies */}
        {(report.states_failed > 0 || report.transitions_failed > 0) && (
          <div className="p-4 bg-red-500/5 rounded-lg border border-red-500/20">
            <h3 className="font-medium text-red-500 mb-3 flex items-center gap-2">
              <AlertCircle className="w-4 h-4" />
              Discrepancies Found
            </h3>
            <div className="space-y-2">
              {report.state_verifications
                .filter((s) => s.status === "failed" || s.status === "error")
                .map((state) => (
                  <div key={state.state_id} className="p-3 bg-background rounded-lg text-sm">
                    <div className="flex items-center gap-2">
                      {getStatusIcon(state.status)}
                      <span className="font-medium">{state.state_name || state.state_id}</span>
                    </div>
                    {state.error && <p className="mt-1 text-red-400 text-xs">{state.error}</p>}
                  </div>
                ))}
              {report.transition_verifications
                .filter((t) => t.status === "failed" || t.status === "error")
                .map((transition) => (
                  <div
                    key={transition.transition_id}
                    className="p-3 bg-background rounded-lg text-sm"
                  >
                    <div className="flex items-center gap-2">
                      {getStatusIcon(transition.status)}
                      <span className="font-medium">
                        {transition.from_state_id} <ArrowRight className="w-3 h-3 inline" />{" "}
                        {transition.to_state_id}
                      </span>
                    </div>
                    {transition.error && (
                      <p className="mt-1 text-red-400 text-xs">{transition.error}</p>
                    )}
                  </div>
                ))}
            </div>
          </div>
        )}

        {/* State Verifications */}
        {report.state_verifications.length > 0 && (
          <div className="space-y-2">
            <h3 className="font-medium">State Verifications</h3>
            {report.state_verifications.map((state) => (
              <StateVerificationCard
                key={state.state_id}
                state={state}
                expanded={expandedStates.has(state.state_id)}
                onToggle={() => toggleState(state.state_id)}
              />
            ))}
          </div>
        )}

        {/* Transition Verifications */}
        {report.transition_verifications.length > 0 && (
          <div className="space-y-2">
            <h3 className="font-medium">Transition Verifications</h3>
            {report.transition_verifications.map((transition) => (
              <TransitionVerificationCard
                key={transition.transition_id}
                transition={transition}
                expanded={expandedTransitions.has(transition.transition_id)}
                onToggle={() => toggleTransition(transition.transition_id)}
              />
            ))}
          </div>
        )}

        {/* AI Prompt Display */}
        {showAiPrompt && aiPrompt && (
          <div className="p-4 bg-card rounded-lg border border-border">
            <div className="flex items-center justify-between mb-3">
              <h3 className="font-medium flex items-center gap-2">
                <Bot className="w-4 h-4 text-purple-500" />
                AI Analysis Prompt
              </h3>
              <button
                onClick={handleCopyPrompt}
                className="flex items-center gap-1.5 px-2.5 py-1 text-xs bg-muted hover:bg-muted/80 rounded transition-colors"
              >
                <Copy className="w-3 h-3" />
                Copy
              </button>
            </div>
            <pre className="p-3 bg-muted/50 rounded-lg text-xs overflow-x-auto whitespace-pre-wrap max-h-96">
              {aiPrompt}
            </pre>
          </div>
        )}
      </div>
    </div>
  );
}

/**
 * Stat Card Component
 */
function StatCard({
  label,
  value,
  color,
}: {
  label: string;
  value: string | number;
  color: "blue" | "green" | "red" | "purple";
}) {
  const colorClasses = {
    blue: "bg-blue-500/10 text-blue-500",
    green: "bg-green-500/10 text-green-500",
    red: "bg-red-500/10 text-red-500",
    purple: "bg-purple-500/10 text-purple-500",
  };

  return (
    <div className={`p-3 rounded-lg ${colorClasses[color]}`}>
      <div className="text-2xl font-bold">{value}</div>
      <div className="text-xs opacity-80">{label}</div>
    </div>
  );
}

/**
 * State Verification Card Component
 */
function StateVerificationCard({
  state,
  expanded,
  onToggle,
}: {
  state: StateVerificationType;
  expanded: boolean;
  onToggle: () => void;
}) {
  const getStatusIcon = (status: VerificationStatus) => {
    switch (status) {
      case "passed":
        return <CheckCircle className="w-4 h-4 text-green-500" />;
      case "failed":
        return <XCircle className="w-4 h-4 text-red-500" />;
      case "skipped":
        return <SkipForward className="w-4 h-4 text-yellow-500" />;
      case "error":
        return <AlertCircle className="w-4 h-4 text-red-500" />;
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
        {getStatusIcon(state.status)}
        <span className="flex-1 text-left font-medium">{state.state_name || state.state_id}</span>
        <span className="text-xs text-muted-foreground">{state.duration_ms}ms</span>
        {state.detection_confidence > 0 && (
          <span className="text-xs text-muted-foreground">
            {Math.round(state.detection_confidence * 100)}% confidence
          </span>
        )}
      </button>

      {expanded && (
        <div className="px-4 pb-4 border-t border-border space-y-3">
          {/* Timestamps */}
          <div className="pt-3 text-xs text-muted-foreground">
            Started: {new Date(state.started_at).toLocaleString()}
            {state.completed_at && (
              <> | Completed: {new Date(state.completed_at).toLocaleString()}</>
            )}
          </div>

          {/* Error */}
          {state.error && (
            <div className="p-2 bg-red-500/10 rounded text-sm text-red-400">{state.error}</div>
          )}

          {/* AI Notes */}
          {state.ai_notes && (
            <div className="p-2 bg-purple-500/10 rounded text-sm">
              <span className="text-purple-400">AI Notes: </span>
              {state.ai_notes}
            </div>
          )}

          {/* Screenshot */}
          {state.screenshot_path && (
            <div className="flex items-center gap-2 text-sm text-muted-foreground">
              <Image className="w-4 h-4" />
              <span className="truncate">{state.screenshot_path}</span>
            </div>
          )}

          {/* Element Checks */}
          {state.expected_elements_found.length > 0 && (
            <div className="space-y-1">
              <h4 className="text-xs font-medium text-muted-foreground">Expected Elements</h4>
              {state.expected_elements_found.map((el, idx) => (
                <div key={idx} className="flex items-center gap-2 text-xs">
                  {el.found ? (
                    <CheckCircle className="w-3 h-3 text-green-500" />
                  ) : (
                    <XCircle className="w-3 h-3 text-red-500" />
                  )}
                  <span>{el.element}</span>
                  <span className="text-muted-foreground">
                    ({Math.round(el.confidence * 100)}%)
                  </span>
                </div>
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

/**
 * Transition Verification Card Component
 */
function TransitionVerificationCard({
  transition,
  expanded,
  onToggle,
}: {
  transition: TransitionVerificationType;
  expanded: boolean;
  onToggle: () => void;
}) {
  const getStatusIcon = (status: VerificationStatus) => {
    switch (status) {
      case "passed":
        return <CheckCircle className="w-4 h-4 text-green-500" />;
      case "failed":
        return <XCircle className="w-4 h-4 text-red-500" />;
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
        {getStatusIcon(transition.status)}
        <span className="flex-1 text-left">
          <span className="font-medium">{transition.from_state_id}</span>
          <ArrowRight className="w-3 h-3 mx-2 inline text-muted-foreground" />
          <span className="font-medium">{transition.to_state_id}</span>
        </span>
        <span className="text-xs text-muted-foreground">{transition.duration_ms}ms</span>
        {transition.duration_exceeded && <span className="text-xs text-amber-500">Slow</span>}
      </button>

      {expanded && (
        <div className="px-4 pb-4 border-t border-border space-y-3">
          {/* Timestamps */}
          <div className="pt-3 text-xs text-muted-foreground">
            Started: {new Date(transition.started_at).toLocaleString()}
            {transition.completed_at && (
              <> | Completed: {new Date(transition.completed_at).toLocaleString()}</>
            )}
          </div>

          {/* Duration Info */}
          {transition.expected_duration_ms && (
            <div className="text-xs text-muted-foreground">
              Expected: {transition.expected_duration_ms}ms | Actual: {transition.duration_ms}ms
              {transition.duration_exceeded && (
                <span className="text-amber-500 ml-2">
                  (+{transition.duration_ms - transition.expected_duration_ms}ms)
                </span>
              )}
            </div>
          )}

          {/* Error */}
          {transition.error && (
            <div className="p-2 bg-red-500/10 rounded text-sm text-red-400">{transition.error}</div>
          )}

          {/* Notes */}
          {transition.notes && (
            <div className="p-2 bg-muted/50 rounded text-sm">{transition.notes}</div>
          )}

          {/* Screenshots */}
          {(transition.screenshot_before || transition.screenshot_after) && (
            <div className="space-y-1 text-sm text-muted-foreground">
              {transition.screenshot_before && (
                <div className="flex items-center gap-2">
                  <Image className="w-4 h-4" />
                  <span>Before: {transition.screenshot_before}</span>
                </div>
              )}
              {transition.screenshot_after && (
                <div className="flex items-center gap-2">
                  <Image className="w-4 h-4" />
                  <span>After: {transition.screenshot_after}</span>
                </div>
              )}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
