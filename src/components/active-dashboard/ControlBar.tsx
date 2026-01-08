/**
 * ControlBar Component
 *
 * Top control bar for the active execution dashboard.
 * Displays workflow name, execution status, and playback controls.
 */

import { Play, Pause, Square, Settings } from "lucide-react";
import { Button, Badge } from "../ui";
import type { ControlBarProps, ExecutionStatus } from "./types";

const statusConfig: Record<ExecutionStatus, { label: string; className: string }> = {
  running: {
    label: "Running",
    className: "bg-blue-500/20 text-blue-400 border border-blue-500/50 animate-pulse",
  },
  paused: {
    label: "Paused",
    className: "bg-amber-500/20 text-amber-400 border border-amber-500/50",
  },
  stopped: {
    label: "Stopped",
    className: "bg-zinc-500/20 text-zinc-400 border border-zinc-500/50",
  },
  completed: {
    label: "Completed",
    className: "bg-green-500/20 text-green-400 border border-green-500/50",
  },
  failed: {
    label: "Failed",
    className: "bg-red-500/20 text-red-400 border border-red-500/50",
  },
  idle: {
    label: "Idle",
    className: "bg-zinc-500/20 text-zinc-400 border border-zinc-500/50",
  },
  timeout: {
    label: "Timeout",
    className: "bg-orange-500/20 text-orange-400 border border-orange-500/50",
  },
  cancelled: {
    label: "Cancelled",
    className: "bg-zinc-500/20 text-zinc-400 border border-zinc-500/50",
  },
};

export function ControlBar({
  workflowName,
  status,
  onPlayPause,
  onStop,
  onGoToExecute,
}: ControlBarProps) {
  return (
    <div className="flex h-14 items-center justify-between border-b border-border bg-card px-4">
      <div className="flex items-center gap-3">
        <Button variant="ghost" onClick={onGoToExecute} className="border-border hover:bg-muted">
          Go to Execute
        </Button>
        {workflowName && <span className="text-sm text-muted-foreground">{workflowName}</span>}
      </div>

      {/* Center: Status Indicator */}
      <Badge className={`px-4 py-1.5 text-sm font-medium ${statusConfig[status].className}`}>
        {statusConfig[status].label}
      </Badge>

      <div className="flex items-center gap-2">
        <Button
          size="sm"
          variant="outline"
          onClick={onPlayPause}
          disabled={status === "idle"}
          className="border-border bg-muted hover:bg-muted/80"
        >
          {status === "running" ? <Pause className="h-4 w-4" /> : <Play className="h-4 w-4" />}
        </Button>

        <Button
          size="sm"
          variant="outline"
          onClick={onStop}
          disabled={status === "idle" || status === "stopped"}
          className="border-border bg-muted hover:bg-muted/80"
        >
          <Square className="h-4 w-4" />
        </Button>

        <Button
          size="sm"
          variant="outline"
          className="ml-2 border-border bg-muted hover:bg-muted/80"
        >
          <Settings className="h-4 w-4" />
        </Button>
      </div>
    </div>
  );
}
