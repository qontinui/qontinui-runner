/**
 * FlowExecutionWidget
 *
 * Full widget component for displaying Flow Designer execution progress.
 * Shows step history, current step, outputs, and execution controls.
 */

import { useState, useRef, useEffect } from "react";
import {
  Pause,
  CheckCircle,
  XCircle,
  Clock,
  ChevronDown,
  ChevronRight,
  GitBranch,
  Loader2,
  AlertCircle,
} from "lucide-react";
import { cn } from "../../../../lib/utils";
import { ScrollArea } from "../../../../components/ui/ScrollArea";
import type { FlowExecutionWidgetProps, FlowStepEntry } from "./types";

/**
 * Format duration in human-readable format.
 */
function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  if (ms < 60000) return `${(ms / 1000).toFixed(1)}s`;
  const minutes = Math.floor(ms / 60000);
  const seconds = ((ms % 60000) / 1000).toFixed(0);
  return `${minutes}m ${seconds}s`;
}

/**
 * Get status icon for execution status.
 */
function StatusIcon({ status }: { status: string }) {
  switch (status) {
    case "running":
      return <Loader2 className="w-4 h-4 animate-spin text-emerald-400" />;
    case "completed":
      return <CheckCircle className="w-4 h-4 text-green-400" />;
    case "failed":
      return <XCircle className="w-4 h-4 text-red-400" />;
    case "paused":
      return <Pause className="w-4 h-4 text-blue-400" />;
    case "waiting_for_input":
      return <AlertCircle className="w-4 h-4 text-orange-400" />;
    default:
      return <Clock className="w-4 h-4 text-gray-400" />;
  }
}

/**
 * Step history item component.
 */
function StepHistoryItem({
  entry,
  isSelected,
  onClick,
}: {
  entry: FlowStepEntry;
  isSelected: boolean;
  onClick: () => void;
}) {
  return (
    <button
      onClick={onClick}
      className={cn(
        "w-full flex items-center gap-2 p-2 rounded text-left text-sm transition-colors",
        isSelected
          ? "bg-emerald-500/10 border border-emerald-500/30"
          : "bg-muted/50 hover:bg-muted",
      )}
    >
      {entry.success ? (
        <CheckCircle className="w-4 h-4 text-green-400 flex-shrink-0" />
      ) : (
        <XCircle className="w-4 h-4 text-red-400 flex-shrink-0" />
      )}
      <div className="flex-1 min-w-0">
        <div className="text-foreground truncate">{entry.stepName}</div>
        <div className="text-muted-foreground text-xs">
          {entry.stepType} · {formatDuration(entry.durationMs)}
        </div>
      </div>
    </button>
  );
}

/**
 * Output viewer for selected step.
 */
function StepOutputViewer({ entry }: { entry: FlowStepEntry }) {
  const hasOutputs = Object.keys(entry.outputs).length > 0;

  return (
    <div className="space-y-2">
      <div className="text-sm font-medium text-muted-foreground">Output: {entry.stepName}</div>
      <div className="bg-muted/30 rounded p-3 text-sm">
        {hasOutputs ? (
          <pre className="text-foreground overflow-x-auto whitespace-pre-wrap font-mono text-xs">
            {JSON.stringify(entry.outputs, null, 2)}
          </pre>
        ) : (
          <div className="text-muted-foreground italic">No outputs</div>
        )}
        {entry.error && (
          <div className="mt-2 p-2 bg-red-500/10 border border-red-500/30 rounded text-red-400 text-xs">
            {entry.error}
          </div>
        )}
      </div>
    </div>
  );
}

/**
 * Full Flow Execution Widget.
 */
export function FlowExecutionWidget({
  isActive: _isActiveWidget,
  isSummary,
  status: _widgetStatus,
  data,
  className,
}: FlowExecutionWidgetProps) {
  const [selectedStepIndex, setSelectedStepIndex] = useState<number | null>(null);
  const [expandedSections, setExpandedSections] = useState({
    history: true,
    outputs: true,
    context: false,
  });
  const scrollRef = useRef<HTMLDivElement>(null);

  // Auto-scroll to latest step
  useEffect(() => {
    if (scrollRef.current && data.stepHistory.length > 0) {
      scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
    }
  }, [data.stepHistory.length]);

  // Summary mode is handled by separate component
  if (isSummary) {
    return null;
  }

  // No active execution
  if (!data.isActive && data.status === "idle") {
    return (
      <div className={cn("flex flex-col items-center justify-center h-full p-8", className)}>
        <GitBranch className="w-12 h-12 text-muted-foreground/30 mb-4" />
        <div className="text-muted-foreground text-center">
          <div className="font-medium">No Flow Running</div>
          <div className="text-sm mt-1">
            Start a flow from the Flow Designer to see execution here
          </div>
        </div>
      </div>
    );
  }

  const toggleSection = (section: keyof typeof expandedSections) => {
    setExpandedSections((prev) => ({
      ...prev,
      [section]: !prev[section],
    }));
  };

  const selectedEntry = selectedStepIndex !== null ? data.stepHistory[selectedStepIndex] : null;

  return (
    <div className={cn("flex flex-col h-full", className)}>
      {/* Header stats */}
      <div className="flex items-center gap-4 px-4 py-3 border-b border-border/50">
        <div className="flex items-center gap-2">
          <StatusIcon status={data.status} />
          <span className="text-sm font-medium capitalize">{data.status.replace("_", " ")}</span>
        </div>
        <div className="text-sm text-muted-foreground">{formatDuration(data.elapsedMs)}</div>
        <div className="text-sm text-muted-foreground">
          {data.stats.completedSteps} step{data.stats.completedSteps !== 1 ? "s" : ""} completed
        </div>
        {data.stats.failedSteps > 0 && (
          <div className="text-sm text-red-400">{data.stats.failedSteps} failed</div>
        )}
      </div>

      {/* Error message */}
      {data.error && (
        <div className="mx-4 mt-3 p-2 bg-red-500/10 border border-red-500/30 rounded text-sm text-red-400">
          {data.error}
        </div>
      )}

      {/* Main content */}
      <ScrollArea className="flex-1">
        <div className="p-4 space-y-4">
          {/* Flow info */}
          {data.flowName && (
            <div className="text-sm">
              <span className="text-muted-foreground">Flow:</span>{" "}
              <span className="font-medium">{data.flowName}</span>
            </div>
          )}

          {/* Step History Section */}
          <div className="border border-border/50 rounded-lg overflow-hidden">
            <button
              onClick={() => toggleSection("history")}
              className="w-full flex items-center gap-2 px-3 py-2 text-sm font-medium bg-muted/30 hover:bg-muted/50 transition-colors"
            >
              {expandedSections.history ? (
                <ChevronDown className="w-4 h-4" />
              ) : (
                <ChevronRight className="w-4 h-4" />
              )}
              Step History
              <span className="text-muted-foreground ml-auto">{data.stepHistory.length}</span>
            </button>

            {expandedSections.history && (
              <div ref={scrollRef} className="p-3 space-y-1 max-h-64 overflow-y-auto">
                {data.stepHistory.length === 0 ? (
                  <div className="text-muted-foreground text-sm text-center py-4">
                    {data.status === "running" ? "Waiting for steps..." : "No steps executed"}
                  </div>
                ) : (
                  data.stepHistory.map((entry, index) => (
                    <StepHistoryItem
                      key={`${entry.stepId}-${index}`}
                      entry={entry}
                      isSelected={selectedStepIndex === index}
                      onClick={() => {
                        setSelectedStepIndex(index);
                        setExpandedSections((prev) => ({ ...prev, outputs: true }));
                      }}
                    />
                  ))
                )}
              </div>
            )}
          </div>

          {/* Selected Step Outputs */}
          {selectedEntry && (
            <div className="border border-border/50 rounded-lg overflow-hidden">
              <button
                onClick={() => toggleSection("outputs")}
                className="w-full flex items-center gap-2 px-3 py-2 text-sm font-medium bg-muted/30 hover:bg-muted/50 transition-colors"
              >
                {expandedSections.outputs ? (
                  <ChevronDown className="w-4 h-4" />
                ) : (
                  <ChevronRight className="w-4 h-4" />
                )}
                Step Output
              </button>

              {expandedSections.outputs && (
                <div className="p-3">
                  <StepOutputViewer entry={selectedEntry} />
                </div>
              )}
            </div>
          )}

          {/* Context Variables */}
          {Object.keys(data.context).length > 0 && (
            <div className="border border-border/50 rounded-lg overflow-hidden">
              <button
                onClick={() => toggleSection("context")}
                className="w-full flex items-center gap-2 px-3 py-2 text-sm font-medium bg-muted/30 hover:bg-muted/50 transition-colors"
              >
                {expandedSections.context ? (
                  <ChevronDown className="w-4 h-4" />
                ) : (
                  <ChevronRight className="w-4 h-4" />
                )}
                Context Variables
                <span className="text-muted-foreground ml-auto">
                  {Object.keys(data.context).length}
                </span>
              </button>

              {expandedSections.context && (
                <div className="p-3">
                  <div className="bg-muted/30 rounded p-2 text-sm max-h-48 overflow-y-auto">
                    <pre className="text-foreground whitespace-pre-wrap font-mono text-xs">
                      {JSON.stringify(data.context, null, 2)}
                    </pre>
                  </div>
                </div>
              )}
            </div>
          )}
        </div>
      </ScrollArea>
    </div>
  );
}
