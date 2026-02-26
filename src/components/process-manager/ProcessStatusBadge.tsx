import { cn } from "../../lib/utils";
import { Circle, Loader2, CheckCircle2, XCircle, Square, ArrowDown } from "lucide-react";

type ProcessState = "stopped" | "starting" | "running" | "healthy" | "stopping" | "failed";

interface ProcessStatusBadgeProps {
  state: ProcessState;
  className?: string;
}

const stateConfig: Record<ProcessState, { icon: React.ReactNode; label: string; classes: string }> =
  {
    stopped: {
      icon: <Square className="w-3 h-3" />,
      label: "Stopped",
      classes: "bg-zinc-800 text-zinc-400 border-zinc-700",
    },
    starting: {
      icon: <Loader2 className="w-3 h-3 animate-spin" />,
      label: "Starting",
      classes: "bg-yellow-900/30 text-yellow-400 border-yellow-800",
    },
    running: {
      icon: <Circle className="w-3 h-3 fill-current" />,
      label: "Running",
      classes: "bg-blue-900/30 text-blue-400 border-blue-800",
    },
    healthy: {
      icon: <CheckCircle2 className="w-3 h-3" />,
      label: "Healthy",
      classes: "bg-green-900/30 text-green-400 border-green-800",
    },
    stopping: {
      icon: <ArrowDown className="w-3 h-3" />,
      label: "Stopping",
      classes: "bg-orange-900/30 text-orange-400 border-orange-800",
    },
    failed: {
      icon: <XCircle className="w-3 h-3" />,
      label: "Failed",
      classes: "bg-red-900/30 text-red-400 border-red-800",
    },
  };

export function ProcessStatusBadge({ state, className }: ProcessStatusBadgeProps) {
  const config = stateConfig[state] || stateConfig.stopped;

  return (
    <span
      className={cn(
        "inline-flex items-center gap-1.5 px-2 py-0.5 rounded-full text-xs font-medium border",
        config.classes,
        className,
      )}
    >
      {config.icon}
      {config.label}
    </span>
  );
}
