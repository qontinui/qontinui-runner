/**
 * TaskRunLivePanel — Real-time task run detail panel powered by GraphQL.
 *
 * Uses useTaskRunLive composite hook for zero-polling live updates:
 * - Task metadata and status
 * - Live progress events via WebSocket subscription
 * - Findings streaming in real-time
 * - Output with pagination
 *
 * Usage:
 *   <TaskRunLivePanel taskRunId="abc-123" />
 */

import { useMemo, type ReactElement } from "react";
import { useTaskRunLive, useTaskRunControls } from "@/hooks/graphql";
import {
  Loader2,
  Square,
  Pause,
  Play,
  Trash2,
  AlertCircle,
  CheckCircle2,
  Clock,
  ArrowUp,
} from "lucide-react";
import { List, useListRef, type RowComponentProps } from "react-window";
import { cn } from "@/lib/utils";
import { RunErrorSummary } from "../error-monitor/RunErrorSummary";

const OUTPUT_LINE_HEIGHT = 18;

interface OutputLineRowProps {
  lines: string[];
}

function OutputLineRow({
  index,
  style,
  lines,
}: RowComponentProps<OutputLineRowProps>): ReactElement {
  return (
    <div style={style} className="whitespace-nowrap overflow-hidden text-ellipsis">
      {lines[index]}
    </div>
  );
}

interface TaskRunLivePanelProps {
  taskRunId: string;
  className?: string;
  /** Compact mode — hide output and findings detail */
  compact?: boolean;
}

function StatusBadge({ status }: { status: string }) {
  const colors: Record<string, string> = {
    running: "bg-blue-500/20 text-blue-400 border-blue-500/30",
    complete: "bg-green-500/20 text-green-400 border-green-500/30",
    failed: "bg-red-500/20 text-red-400 border-red-500/30",
    stopped: "bg-yellow-500/20 text-yellow-400 border-yellow-500/30",
    paused: "bg-orange-500/20 text-orange-400 border-orange-500/30",
    pending: "bg-gray-500/20 text-gray-400 border-gray-500/30",
  };
  const icons: Record<string, React.ReactNode> = {
    running: <Loader2 className="h-3 w-3 animate-spin" />,
    complete: <CheckCircle2 className="h-3 w-3" />,
    failed: <AlertCircle className="h-3 w-3" />,
    stopped: <Square className="h-3 w-3" />,
    paused: <Pause className="h-3 w-3" />,
    pending: <Clock className="h-3 w-3" />,
  };

  return (
    <span
      className={cn(
        "inline-flex items-center gap-1 px-2 py-0.5 rounded-full text-xs border",
        colors[status] || colors.pending,
      )}
    >
      {icons[status] || icons.pending}
      {status}
    </span>
  );
}

export function TaskRunLivePanel({ taskRunId, className, compact = false }: TaskRunLivePanelProps) {
  const {
    taskRun,
    latestEvent,
    findings,
    summary,
    output,
    isRunning,
    isTerminal,
    loading,
    error,
    fetchMoreOutput,
  } = useTaskRunLive(taskRunId, { outputLimit: 4000 });
  const outputListRef = useListRef(null);

  // Split output content into lines for virtualized rendering
  const outputContent = output?.content;
  const outputLines = useMemo(() => {
    if (!outputContent) return [];
    // Show only the tail (last 2000 chars) split into lines
    const tail = outputContent.slice(-2000);
    return tail.split("\n");
  }, [outputContent]);

  const controls = useTaskRunControls();

  if (loading && !taskRun) {
    return (
      <div className={cn("flex items-center justify-center p-8 text-muted-foreground", className)}>
        <Loader2 className="h-5 w-5 animate-spin mr-2" />
        Loading task run...
      </div>
    );
  }

  if (error && !taskRun) {
    return (
      <div className={cn("p-4 text-red-400", className)}>
        <AlertCircle className="h-4 w-4 inline mr-1" />
        {error}
      </div>
    );
  }

  if (!taskRun) return null;

  return (
    <div className={cn("flex flex-col gap-3 p-4", className)}>
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <h3 className="font-medium text-sm truncate max-w-[300px]">{taskRun.taskName}</h3>
          <StatusBadge status={taskRun.status} />
        </div>

        {/* Controls */}
        <div className="flex items-center gap-1">
          {isRunning && taskRun.status === "running" && (
            <>
              <button
                onClick={() => controls.pause(taskRunId)}
                disabled={controls.loading}
                className="p-1 rounded hover:bg-muted text-muted-foreground hover:text-foreground"
                title="Pause"
              >
                <Pause className="h-3.5 w-3.5" />
              </button>
              <button
                onClick={() => controls.stop(taskRunId)}
                disabled={controls.loading}
                className="p-1 rounded hover:bg-muted text-muted-foreground hover:text-red-400"
                title="Stop"
              >
                <Square className="h-3.5 w-3.5" />
              </button>
            </>
          )}
          {taskRun.status === "paused" && (
            <button
              onClick={() => controls.unpause(taskRunId)}
              disabled={controls.loading}
              className="p-1 rounded hover:bg-muted text-muted-foreground hover:text-foreground"
              title="Resume"
            >
              <Play className="h-3.5 w-3.5" />
            </button>
          )}
          {isTerminal && (
            <button
              onClick={() => controls.remove(taskRunId)}
              disabled={controls.loading}
              className="p-1 rounded hover:bg-muted text-muted-foreground hover:text-red-400"
              title="Delete"
            >
              <Trash2 className="h-3.5 w-3.5" />
            </button>
          )}
        </div>
      </div>

      {/* Metadata */}
      <div className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs text-muted-foreground">
        {taskRun.workflowName && (
          <>
            <span>Workflow</span>
            <span className="text-foreground truncate">{taskRun.workflowName}</span>
          </>
        )}
        <span>Sessions</span>
        <span className="text-foreground">
          {taskRun.sessionsCount}
          {taskRun.maxSessions ? ` / ${taskRun.maxSessions}` : ""}
        </span>
        <span>Created</span>
        <span className="text-foreground">{new Date(taskRun.createdAt).toLocaleTimeString()}</span>
        {taskRun.completedAt && (
          <>
            <span>Completed</span>
            <span className="text-foreground">
              {new Date(taskRun.completedAt).toLocaleTimeString()}
            </span>
          </>
        )}
      </div>

      {/* Live event indicator */}
      {latestEvent && isRunning && (
        <div className="text-xs text-muted-foreground bg-muted/50 rounded px-2 py-1 flex items-center gap-1">
          <span className="h-1.5 w-1.5 rounded-full bg-blue-400 animate-pulse" />
          {latestEvent.__typename === "StepProgressEvent" && (
            <span>
              Step: {latestEvent.stepName} — {latestEvent.status}
            </span>
          )}
          {latestEvent.__typename === "TaskRunUpdateEvent" && (
            <span>Status: {latestEvent.status}</span>
          )}
          {latestEvent.__typename === "OrchestratorStateChangeEvent" && (
            <span>Phase: {latestEvent.phase}</span>
          )}
        </div>
      )}

      {/* Error message */}
      {taskRun.errorMessage && (
        <div className="text-xs text-red-400 bg-red-500/10 rounded px-2 py-1 border border-red-500/20">
          {taskRun.errorMessage}
        </div>
      )}

      {/* Error monitor summary for this run */}
      <RunErrorSummary taskRunId={taskRunId} taskRunName={taskRun?.taskName} />

      {/* Summary */}
      {taskRun.summary && (
        <div className="text-xs text-muted-foreground bg-muted/30 rounded px-2 py-1">
          {taskRun.summary}
        </div>
      )}

      {!compact && (
        <>
          {/* Findings summary */}
          {summary && summary.total > 0 && (
            <div className="border rounded p-2">
              <div className="text-xs font-medium mb-1">Findings ({summary.total})</div>
              <div className="flex gap-2 text-xs text-muted-foreground">
                {summary.outstandingCount > 0 && (
                  <span className="text-yellow-400">{summary.outstandingCount} outstanding</span>
                )}
                {summary.resolvedCount > 0 && (
                  <span className="text-green-400">{summary.resolvedCount} resolved</span>
                )}
                {summary.needsInputCount > 0 && (
                  <span className="text-orange-400">{summary.needsInputCount} needs input</span>
                )}
              </div>
              {/* Live findings list */}
              {findings.length > 0 && (
                <div className="mt-2 space-y-1 max-h-40 overflow-y-auto">
                  {findings.slice(0, 10).map((f) => (
                    <div key={f.id} className="text-xs flex items-center gap-1">
                      <span
                        className={cn(
                          "px-1 rounded",
                          f.severity === "CRITICAL"
                            ? "bg-red-500/20 text-red-300"
                            : f.severity === "HIGH"
                              ? "bg-orange-500/20 text-orange-300"
                              : "bg-muted text-muted-foreground",
                        )}
                      >
                        {f.severity}
                      </span>
                      <span className="truncate">{f.title}</span>
                    </div>
                  ))}
                  {findings.length > 10 && (
                    <div className="text-xs text-muted-foreground">
                      +{findings.length - 10} more
                    </div>
                  )}
                </div>
              )}
            </div>
          )}

          {/* Output preview */}
          {output && output.content && (
            <div className="border rounded p-2">
              <div className="flex items-center justify-between mb-1">
                <span className="text-xs font-medium">
                  Output ({(output.totalLength / 1024).toFixed(1)}KB)
                </span>
                <div className="flex items-center gap-2">
                  {output.hasMore && (
                    <button
                      onClick={() => fetchMoreOutput(output.offset + output.content.length)}
                      className="text-xs text-primary hover:underline flex items-center gap-1"
                    >
                      <ArrowUp className="w-3 h-3" />
                      Load more
                    </button>
                  )}
                </div>
              </div>
              <div
                className="text-xs text-muted-foreground bg-muted/30 rounded font-mono"
                style={{ height: Math.min(outputLines.length * OUTPUT_LINE_HEIGHT, 128) }}
              >
                <List
                  listRef={outputListRef}
                  rowCount={outputLines.length}
                  rowHeight={OUTPUT_LINE_HEIGHT}
                  rowComponent={OutputLineRow}
                  rowProps={{ lines: outputLines }}
                  overscanCount={5}
                  style={{
                    width: "100%",
                    height: Math.min(outputLines.length * OUTPUT_LINE_HEIGHT, 128),
                  }}
                />
              </div>
            </div>
          )}
        </>
      )}
    </div>
  );
}
