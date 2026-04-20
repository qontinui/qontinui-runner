/**
 * SchedulerTaskList
 *
 * Displays a list of scheduled tasks with status badges and actions.
 */

import {
  Play,
  Pause,
  Pencil,
  Trash2,
  PlayCircle,
  Clock,
  CheckCircle,
  XCircle,
  SkipForward,
  Loader2,
} from "lucide-react";
import type { ScheduledTask } from "../../types/scheduler";
import { getAccentColors, getStatusColors } from "@/design-system";
import {
  describeSchedule,
  describeTaskType,
  getStatusColor,
  getTimeUntilNextRun,
  isTaskRunning,
} from "../../types/scheduler";

interface SchedulerTaskListProps {
  tasks: ScheduledTask[];
  selectedTask: ScheduledTask | null;
  onSelectTask: (task: ScheduledTask | null) => void;
  onEditTask: (task: ScheduledTask) => void;
  onDeleteTask: (task: ScheduledTask) => void;
  onRunNow: (id: string) => Promise<boolean>;
  onToggleEnabled: (task: ScheduledTask) => void;
  loading: boolean;
}

export function SchedulerTaskList({
  tasks,
  selectedTask,
  onSelectTask,
  onEditTask,
  onDeleteTask,
  onRunNow,
  onToggleEnabled,
  loading,
}: SchedulerTaskListProps) {
  if (tasks.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center h-64 text-muted-foreground">
        <Clock className="w-12 h-12 mb-4 opacity-50" />
        <p className="text-lg font-medium">No scheduled tasks</p>
        <p className="text-sm">Create a task to schedule workflows or prompts</p>
      </div>
    );
  }

  const getStatusIcon = (task: ScheduledTask) => {
    if (!task.lastRun) return null;

    const status = task.lastRun.status;
    switch (status) {
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

  return (
    <div className="space-y-2">
      {tasks.map((task) => {
        const isSelected = selectedTask?.id === task.id;
        const running = isTaskRunning(task);
        const timeUntil = getTimeUntilNextRun(task);

        return (
          <div
            key={task.id}
            onClick={() => onSelectTask(isSelected ? null : task)}
            onKeyDown={(e) => {
              if (e.key === "Enter" || e.key === " ") onSelectTask(isSelected ? null : task);
            }}
            role="button"
            tabIndex={0}
            className={`p-4 rounded-lg border cursor-pointer transition-all ${
              isSelected
                ? "border-primary bg-primary/5"
                : "border-border hover:border-primary/50 hover:bg-muted/50"
            } ${!task.enabled ? "opacity-60" : ""}`}
          >
            <div className="flex items-start justify-between">
              {/* Left side: Task info */}
              <div className="flex-1 min-w-0">
                <div className="flex items-center gap-2">
                  <span className="font-medium truncate">{task.name}</span>
                  {!task.enabled && (
                    <span className="px-2 py-0.5 text-xs bg-muted rounded">Disabled</span>
                  )}
                  {task.skipIfCompleted && task.lastRun?.success && (
                    <span
                      className={`px-2 py-0.5 text-xs ${getAccentColors("green").bg} ${getAccentColors("green").text} rounded`}
                    >
                      Completed
                    </span>
                  )}
                  {task.autoFixOnFailure && (
                    <span
                      className={`px-2 py-0.5 text-xs ${getAccentColors("orange").bg} ${getAccentColors("orange").text} rounded`}
                    >
                      Auto-Fix
                    </span>
                  )}
                </div>
                <div className="mt-1 text-sm text-muted-foreground">
                  {describeTaskType(task.task)}
                </div>
                <div className="mt-1 flex items-center gap-4 text-xs text-muted-foreground">
                  <span>{describeSchedule(task.schedule)}</span>
                  {timeUntil && task.enabled && (
                    <span className="text-primary">Next: {timeUntil}</span>
                  )}
                </div>
                {task.description && (
                  <div className="mt-2 text-sm text-muted-foreground line-clamp-1">
                    {task.description}
                  </div>
                )}
              </div>

              {/* Right side: Status and actions */}
              <div className="flex items-center gap-2 ml-4">
                {/* Last run status */}
                <div className="flex items-center gap-1">{getStatusIcon(task)}</div>

                {/* Actions */}
                <div className="flex items-center gap-1">
                  <button
                    onClick={(e) => {
                      e.stopPropagation();
                      onToggleEnabled(task);
                    }}
                    disabled={loading}
                    className="p-1.5 rounded hover:bg-muted transition-colors"
                    title={task.enabled ? "Disable" : "Enable"}
                  >
                    {task.enabled ? (
                      <Pause className="w-4 h-4 text-muted-foreground" />
                    ) : (
                      <Play className={`w-4 h-4 ${getAccentColors("green").text}`} />
                    )}
                  </button>
                  <button
                    onClick={(e) => {
                      e.stopPropagation();
                      onRunNow(task.id);
                    }}
                    disabled={loading || running}
                    className="p-1.5 rounded hover:bg-muted transition-colors"
                    title="Run Now"
                  >
                    <PlayCircle
                      className={`w-4 h-4 ${running ? `${getStatusColors("running").icon} animate-pulse` : "text-primary"}`}
                    />
                  </button>
                  <button
                    onClick={(e) => {
                      e.stopPropagation();
                      onEditTask(task);
                    }}
                    disabled={loading}
                    className="p-1.5 rounded hover:bg-muted transition-colors"
                    title="Edit"
                  >
                    <Pencil className="w-4 h-4 text-muted-foreground" />
                  </button>
                  <button
                    onClick={(e) => {
                      e.stopPropagation();
                      onDeleteTask(task);
                    }}
                    disabled={loading}
                    className="p-1.5 rounded hover:bg-destructive/10 transition-colors"
                    title="Delete"
                  >
                    <Trash2 className="w-4 h-4 text-destructive" />
                  </button>
                </div>
              </div>
            </div>

            {/* Expanded details when selected */}
            {isSelected && task.lastRun && (
              <div className="mt-4 pt-4 border-t border-border">
                <h4 className="text-sm font-medium mb-2">Last Execution</h4>
                <div className="grid grid-cols-2 gap-2 text-sm">
                  <div>
                    <span className="text-muted-foreground">Started:</span>{" "}
                    {new Date(task.lastRun.startedAt).toLocaleString()}
                  </div>
                  {task.lastRun.endedAt && (
                    <div>
                      <span className="text-muted-foreground">Ended:</span>{" "}
                      {new Date(task.lastRun.endedAt).toLocaleString()}
                    </div>
                  )}
                  <div>
                    <span className="text-muted-foreground">Status:</span>{" "}
                    <span className={getStatusColor(task.lastRun.status)}>
                      {task.lastRun.status}
                    </span>
                  </div>
                  <div>
                    <span className="text-muted-foreground">Success:</span>{" "}
                    <span
                      className={
                        task.lastRun.success
                          ? getAccentColors("green").text
                          : getAccentColors("red").text
                      }
                    >
                      {task.lastRun.success ? "Yes" : "No"}
                    </span>
                  </div>
                  {task.lastRun.errorMessage && (
                    <div className="col-span-2">
                      <span className="text-muted-foreground">Error:</span>{" "}
                      <span className="text-destructive">{task.lastRun.errorMessage}</span>
                    </div>
                  )}
                  {task.lastRun.triggeredAutoFix && (
                    <div className={`col-span-2 ${getAccentColors("orange").text}`}>
                      Auto-fix was triggered
                    </div>
                  )}
                </div>
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}
