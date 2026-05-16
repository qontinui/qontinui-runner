import { cn } from "../../lib/utils";
import {
  AlertTriangle,
  ArrowDown,
  CheckCircle2,
  Circle,
  Hammer,
  Loader2,
  Square,
  XCircle,
} from "lucide-react";

type ProcessState =
  | "stopped"
  | "starting"
  | "building"
  | "running"
  | "healthy"
  | "stopping"
  | "failed";

interface ProcessStatusBadgeProps {
  state: ProcessState;
  /**
   * Result of the manager's TCP probe against `health_port`. `null` if no
   * port is configured or the first probe hasn't fired yet (the loop ticks
   * every 3s). `false` after the first failing probe means "alive, but
   * nothing accepting connections on the port".
   */
  portHealthy?: boolean | null;
  /**
   * Process uptime in seconds. Used for the port-dead grace window — we
   * suppress the amber badge for the first 30s after start so normal
   * Python/Node startup time doesn't paint the row yellow.
   */
  uptimeSecs?: number | null;
  className?: string;
}

/** Seconds after spawn before `port_healthy: false` is treated as a problem. */
const PORT_DEAD_GRACE_SECS = 30;

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
    building: {
      icon: <Hammer className="w-3 h-3 animate-pulse" />,
      label: "Building",
      classes: "bg-amber-900/30 text-amber-400 border-amber-800",
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

export function ProcessStatusBadge({
  state,
  portHealthy,
  uptimeSecs,
  className,
}: ProcessStatusBadgeProps) {
  const portDead =
    state === "running" &&
    portHealthy === false &&
    (uptimeSecs ?? 0) > PORT_DEAD_GRACE_SECS;

  const config = portDead
    ? {
        icon: <AlertTriangle className="w-3 h-3" />,
        label: "Running (port dead)",
        classes: "bg-amber-900/30 text-amber-400 border-amber-800",
      }
    : stateConfig[state] || stateConfig.stopped;

  return (
    <span
      className={cn(
        "inline-flex items-center gap-1.5 px-2 py-0.5 rounded-full text-xs font-medium border",
        config.classes,
        className,
      )}
      title={
        portDead
          ? "The configured health port stopped accepting connections. The process is alive but not serving — typically a worker crash inside the wrapper. Restart to recover."
          : undefined
      }
    >
      {config.icon}
      {config.label}
    </span>
  );
}
