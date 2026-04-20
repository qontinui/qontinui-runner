/**
 * SchedulerHistory
 *
 * Displays execution history for scheduled tasks.
 */

import { Clock, CheckCircle, XCircle, SkipForward, Loader2 } from "lucide-react";
import type { ScheduledTask, TaskExecutionRecord } from "../../types/scheduler";
import { getStatusColor } from "../../types/scheduler";
import { getAccentColors, getStatusColors } from "@/design-system";

interface SchedulerHistoryProps {
  tasks: ScheduledTask[];
  selectedTask: ScheduledTask | null;
  taskHistory: TaskExecutionRecord[];
  onSelectTask: (task: ScheduledTask | null) => void;
}

export function SchedulerHistory({
  tasks,
  selectedTask,
  taskHistory,
  onSelectTask,
}: SchedulerHistoryProps) {
  const getStatusIcon = (record: TaskExecutionRecord) => {
    switch (record.status) {
      case "running":
        return <Loader2 className={`w-4 h-4 ${getStatusColors("running").icon} animate-spin`} />;
      case "completed":
        return <CheckCircle className={`w-4 h-4 ${getStatusColors("success").icon}`} />;
      case "failed":
        return <XCircle className={`w-4 h-4 ${getStatusColors("error").icon}`} />;
      case "skipped":
        return <SkipForward className={`w-4 h-4 ${getStatusColors("warning").icon}`} />;
      default:
        return <Clock className="w-4 h-4 text-muted-foreground" />;
    }
  };

  const formatDuration = (record: TaskExecutionRecord): string | null => {
    if (!record.endedAt) return null;
    const start = new Date(record.startedAt).getTime();
    const end = new Date(record.endedAt).getTime();
    const diff = Math.floor((end - start) / 1000);

    if (diff < 60) return `${diff}s`;
    if (diff < 3600) return `${Math.floor(diff / 60)}m ${diff % 60}s`;
    return `${Math.floor(diff / 3600)}h ${Math.floor((diff % 3600) / 60)}m`;
  };

  return (
    <div className="flex gap-4 h-full">
      {/* Task selector */}
      <div className="w-64 shrink-0 border-r border-border pr-4">
        <h4 className="text-sm font-medium mb-3">Select Task</h4>
        <div className="space-y-1">
          {tasks.map((task) => (
            <button
              key={task.id}
              onClick={() => onSelectTask(task.id === selectedTask?.id ? null : task)}
              className={`w-full text-left px-3 py-2 rounded-lg text-sm transition-colors ${
                selectedTask?.id === task.id
                  ? "bg-primary text-primary-foreground"
                  : "hover:bg-muted"
              }`}
            >
              {task.name}
            </button>
          ))}
        </div>
      </div>

      {/* History list */}
      <div className="flex-1 min-w-0">
        {!selectedTask ? (
          <div className="flex flex-col items-center justify-center h-64 text-muted-foreground">
            <Clock className="w-12 h-12 mb-4 opacity-50" />
            <p>Select a task to view its execution history</p>
          </div>
        ) : taskHistory.length === 0 ? (
          <div className="flex flex-col items-center justify-center h-64 text-muted-foreground">
            <Clock className="w-12 h-12 mb-4 opacity-50" />
            <p>No execution history for this task</p>
          </div>
        ) : (
          <div className="space-y-2">
            <h4 className="text-sm font-medium mb-3">
              History for "{selectedTask.name}" ({taskHistory.length} runs)
            </h4>
            {taskHistory
              .slice()
              .reverse()
              .map((record) => (
                <div
                  key={record.executionId}
                  className="p-3 border border-border rounded-lg hover:bg-muted/50 transition-colors"
                >
                  <div className="flex items-start justify-between">
                    <div className="flex items-center gap-2">
                      {getStatusIcon(record)}
                      <span className={`font-medium ${getStatusColor(record.status)}`}>
                        {record.status.charAt(0).toUpperCase() + record.status.slice(1)}
                      </span>
                      {record.success && (
                        <span
                          className={`px-2 py-0.5 text-xs ${getAccentColors("green").bg} ${getAccentColors("green").text} rounded`}
                        >
                          Success
                        </span>
                      )}
                      {record.triggeredAutoFix && (
                        <span
                          className={`px-2 py-0.5 text-xs ${getAccentColors("orange").bg} ${getAccentColors("orange").text} rounded`}
                        >
                          Auto-fix triggered
                        </span>
                      )}
                    </div>
                    <div className="text-sm text-muted-foreground">
                      {formatDuration(record) && (
                        <span className="mr-3">Duration: {formatDuration(record)}</span>
                      )}
                    </div>
                  </div>
                  <div className="mt-2 grid grid-cols-2 gap-2 text-sm text-muted-foreground">
                    <div>Started: {new Date(record.startedAt).toLocaleString()}</div>
                    {record.endedAt && (
                      <div>Ended: {new Date(record.endedAt).toLocaleString()}</div>
                    )}
                  </div>
                  {record.errorMessage && (
                    <div className="mt-2 text-sm text-destructive">
                      Error: {record.errorMessage}
                    </div>
                  )}
                  {record.sessionId && (
                    <div className="mt-2 text-xs text-muted-foreground">
                      Session: {record.sessionId}
                    </div>
                  )}
                </div>
              ))}
          </div>
        )}
      </div>
    </div>
  );
}
