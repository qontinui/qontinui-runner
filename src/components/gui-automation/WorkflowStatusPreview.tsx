/**
 * WorkflowStatusPreview Component
 *
 * Simple status display component showing execution state with animated indicator.
 * Shows: ready (gray), running (amber with pulse), complete (green)
 */

import { cn } from "../../lib/utils";

export type ExecutionStatus = "ready" | "running" | "complete" | "error";

interface WorkflowStatusPreviewProps {
  status: ExecutionStatus;
  workflowName?: string;
  className?: string;
}

const statusConfig: Record<
  ExecutionStatus,
  { label: string; dotColor: string; pulse: boolean }
> = {
  ready: {
    label: "Ready to start...",
    dotColor: "bg-muted-foreground",
    pulse: false,
  },
  running: {
    label: "Executing...",
    dotColor: "bg-amber-500",
    pulse: true,
  },
  complete: {
    label: "Complete",
    dotColor: "bg-green-500",
    pulse: false,
  },
  error: {
    label: "Error",
    dotColor: "bg-red-500",
    pulse: false,
  },
};

export function WorkflowStatusPreview({
  status,
  workflowName,
  className,
}: WorkflowStatusPreviewProps) {
  const config = statusConfig[status];

  return (
    <div
      className={cn(
        "flex items-center gap-3 px-4 py-3 rounded-lg border border-border bg-secondary/30",
        className,
      )}
    >
      {/* Status dot with optional pulse animation */}
      <span className="relative flex h-2.5 w-2.5">
        {config.pulse && (
          <span
            className={cn(
              "absolute inline-flex h-full w-full animate-ping rounded-full opacity-75",
              config.dotColor,
            )}
          />
        )}
        <span
          className={cn("relative inline-flex h-2.5 w-2.5 rounded-full", config.dotColor)}
        />
      </span>

      {/* Status text */}
      <span className="text-sm font-mono text-muted-foreground">
        {workflowName && status === "running" ? `Running: ${workflowName}` : config.label}
      </span>
    </div>
  );
}
