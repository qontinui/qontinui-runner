import { cn } from "../../lib/utils";
import {
  AlertTriangle,
  ArrowDown,
  CheckCircle2,
  Circle,
  Hammer,
  Link,
  Loader2,
  Square,
  XCircle,
} from "lucide-react";

export type ProcessState =
  | "stopped"
  | "starting"
  | "building"
  | "running"
  | "healthy"
  | "stopping"
  | "failed"
  | "externally_owned";

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
  /**
   * Tail of recent stderr lines for this process (last ~5, joined with
   * newlines). Surfaced verbatim in the tooltip when the badge is in the
   * amber port-dead state, so the user sees the uvicorn traceback /
   * import error inline without having to click into the row. When
   * empty/undefined the tooltip falls back to the static "worker crash"
   * copy.
   */
  stderrTail?: string;
  className?: string;
}

/** Seconds after spawn before `port_healthy: false` is treated as a problem. */
export const PORT_DEAD_GRACE_SECS = 30;

/** Shared predicate matching the badge's amber-state condition. */
export function isPortDead(
  state: ProcessState,
  portHealthy: boolean | null | undefined,
  uptimeSecs: number | null | undefined,
): boolean {
  return (
    state === "running" &&
    portHealthy === false &&
    (uptimeSecs ?? 0) > PORT_DEAD_GRACE_SECS
  );
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
    externally_owned: {
      icon: <Link className="w-3 h-3" />,
      label: "External",
      classes: "bg-violet-900/30 text-violet-400 border-violet-800",
    },
  };

const PORT_DEAD_STATIC_TOOLTIP =
  "The configured health port stopped accepting connections. The process is alive but not serving — typically a worker crash inside the wrapper. Restart to recover.";

export function ProcessStatusBadge({
  state,
  portHealthy,
  uptimeSecs,
  stderrTail,
  className,
}: ProcessStatusBadgeProps) {
  const portDead = isPortDead(state, portHealthy, uptimeSecs);

  const config = portDead
    ? {
        icon: <AlertTriangle className="w-3 h-3" />,
        label: "Running (port dead)",
        classes: "bg-amber-900/30 text-amber-400 border-amber-800",
      }
    : stateConfig[state] || stateConfig.stopped;

  // When port-dead, prefer the stderr tail (concrete traceback) over the
  // static "worker crash" copy. The browser native `title` attribute is
  // monospace-ish in most platforms and preserves newlines, which is what
  // we want for stack traces.
  const tooltip = portDead
    ? stderrTail && stderrTail.trim().length > 0
      ? `Port dead — recent stderr:\n${stderrTail}`
      : PORT_DEAD_STATIC_TOOLTIP
    : undefined;

  return (
    <span
      className={cn(
        "inline-flex items-center gap-1.5 px-2 py-0.5 rounded-full text-xs font-medium border",
        config.classes,
        className,
      )}
      title={tooltip}
    >
      {config.icon}
      {config.label}
    </span>
  );
}
