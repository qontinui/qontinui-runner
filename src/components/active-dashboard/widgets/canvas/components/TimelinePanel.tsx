/**
 * TimelinePanel Component
 *
 * Renders a vertical timeline of events with status indicators.
 */

import { Circle, CheckCircle, Loader2, XCircle } from "lucide-react";
import { cn } from "../../../../../lib/utils";
import type { CanvasPanelComponentProps } from "./CanvasComponentRegistry";

interface TimelineEvent {
  timestamp?: string;
  title: string;
  description?: string;
  status?: "pending" | "running" | "success" | "failed";
}

const STATUS_CONFIG: Record<string, { icon: typeof Circle; classes: string }> = {
  pending: { icon: Circle, classes: "text-muted-foreground" },
  running: { icon: Loader2, classes: "text-blue-400 animate-spin" },
  success: { icon: CheckCircle, classes: "text-green-500" },
  failed: { icon: XCircle, classes: "text-red-500" },
};

export function TimelinePanel({ data, size }: CanvasPanelComponentProps) {
  const events = (data.events as TimelineEvent[]) ?? [];
  const isCompact = size === "compact";

  if (events.length === 0) {
    return <p className="text-sm text-muted-foreground italic">No events</p>;
  }

  return (
    <div className="relative">
      {events.map((event, idx) => {
        const config = STATUS_CONFIG[event.status ?? "pending"] ?? STATUS_CONFIG.pending;
        const Icon = config.icon;
        const isLast = idx === events.length - 1;

        return (
          <div key={idx} className="flex gap-3">
            {/* Line + icon column */}
            <div className="flex flex-col items-center">
              <Icon className={cn("h-4 w-4 flex-shrink-0", config.classes)} />
              {!isLast && <div className="w-px flex-1 bg-border min-h-[16px]" />}
            </div>

            {/* Content */}
            <div className={cn("pb-3 min-w-0", isLast && "pb-0")}>
              <div className="flex items-center gap-2">
                <span className={cn("font-medium", isCompact ? "text-xs" : "text-sm")}>
                  {event.title}
                </span>
                {event.timestamp && (
                  <span className="text-[10px] text-muted-foreground font-mono flex-shrink-0">
                    {event.timestamp}
                  </span>
                )}
              </div>
              {event.description && (
                <p
                  className={cn(
                    "text-muted-foreground mt-0.5",
                    isCompact ? "text-[10px]" : "text-xs",
                  )}
                >
                  {event.description}
                </p>
              )}
            </div>
          </div>
        );
      })}
    </div>
  );
}
