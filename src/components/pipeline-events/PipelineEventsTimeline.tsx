/**
 * PipelineEventsTimeline.tsx
 *
 * Displays a chronological timeline of pipeline events for a task run,
 * showing event types, phases, durations, and token usage.
 */

import { Clock, Zap } from "lucide-react";
import type { PipelineEvent } from "../../hooks/useGraphAnalytics";

interface PipelineEventsTimelineProps {
  events?: PipelineEvent[];
  isLoading?: boolean;
}

const PHASE_COLORS: Record<string, string> = {
  discovery: "bg-blue-500",
  generation: "bg-green-500",
  validation: "bg-yellow-500",
  fixing: "bg-orange-500",
  hardening: "bg-purple-500",
  reflection: "bg-cyan-500",
};

function getPhaseColor(phase: string | null): string {
  if (!phase) return "bg-muted-foreground";
  for (const [key, color] of Object.entries(PHASE_COLORS)) {
    if (phase.toLowerCase().includes(key)) return color;
  }
  return "bg-muted-foreground";
}

function EventRow({ event }: { event: PipelineEvent }) {
  const created = new Date(event.created_at).toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });

  return (
    <div className="flex items-start gap-3 py-2">
      <div className="flex flex-col items-center mt-1">
        <div className={`w-2.5 h-2.5 rounded-full ${getPhaseColor(event.phase)}`} />
        <div className="w-px h-full bg-border" />
      </div>
      <div className="flex-1 min-w-0 pb-2">
        <div className="flex items-center gap-2">
          <span className="text-sm font-medium">{event.event_type}</span>
          {event.phase && (
            <span className="text-xs px-1.5 py-0.5 rounded bg-muted text-muted-foreground">
              {event.phase}
            </span>
          )}
          {event.iteration != null && (
            <span className="text-xs text-muted-foreground">iter {event.iteration}</span>
          )}
        </div>
        <div className="flex items-center gap-3 mt-0.5 text-xs text-muted-foreground">
          <span>{created}</span>
          {event.duration_ms != null && (
            <span className="flex items-center gap-1">
              <Clock className="w-3 h-3" />
              {event.duration_ms}ms
            </span>
          )}
          {event.token_count != null && event.token_count > 0 && (
            <span className="flex items-center gap-1">
              <Zap className="w-3 h-3" />
              {event.token_count} tokens
            </span>
          )}
          {event.validation_errors_before != null && event.validation_errors_after != null && (
            <span>
              errors: {event.validation_errors_before} &rarr; {event.validation_errors_after}
            </span>
          )}
        </div>
      </div>
    </div>
  );
}

export function PipelineEventsTimeline({ events = [], isLoading }: PipelineEventsTimelineProps) {
  return (
    <div className="bg-card rounded-lg border border-border p-4">
      <h3 className="font-semibold mb-4 flex items-center gap-2">
        <Zap className="w-4 h-4" />
        Pipeline Events
      </h3>

      {isLoading && (
        <div className="text-center text-muted-foreground py-8">Loading events...</div>
      )}

      {!isLoading && events.length === 0 && (
        <div className="text-center text-muted-foreground py-8">
          No pipeline events recorded
        </div>
      )}

      {events.length > 0 && (
        <div className="max-h-96 overflow-y-auto">
          {events.map((event) => (
            <EventRow key={event.id} event={event} />
          ))}
        </div>
      )}
    </div>
  );
}
