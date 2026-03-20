/**
 * Event Timeline View
 *
 * Displays a chronological timeline of UI Bridge events including
 * element registrations, actions, state changes, and transitions.
 */

import { useMemo, useState } from "react";
import { cn } from "../../lib/utils";
import { Badge } from "../ui/Badge";
import {
  Activity,
  MousePointer,
  Type,
  Eye,
  Navigation,
  AlertCircle,
  CheckCircle,
  XCircle,
  Clock,
  Filter,
  ChevronDown,
  ChevronRight,
} from "lucide-react";
import type { UIBridgeEvent } from "./inspector-types";

interface EventTimelineViewProps {
  events: UIBridgeEvent[];
  loading?: boolean;
  maxEvents?: number;
}

const EVENT_TYPE_CONFIG: Record<string, { icon: React.ReactNode; color: string; label: string }> = {
  element_registered: {
    icon: <Eye className="w-3.5 h-3.5" />,
    color: "text-blue-400",
    label: "Element Registered",
  },
  element_discovered: {
    icon: <Eye className="w-3.5 h-3.5" />,
    color: "text-blue-400",
    label: "Element Discovered",
  },
  element_selected: {
    icon: <Eye className="w-3.5 h-3.5" />,
    color: "text-blue-400",
    label: "Element Selected",
  },
  action_executed: {
    icon: <MousePointer className="w-3.5 h-3.5" />,
    color: "text-green-400",
    label: "Action Executed",
  },
  state_changed: {
    icon: <Activity className="w-3.5 h-3.5" />,
    color: "text-purple-400",
    label: "State Changed",
  },
  transition_executed: {
    icon: <Navigation className="w-3.5 h-3.5" />,
    color: "text-amber-400",
    label: "Transition Executed",
  },
  navigation_started: {
    icon: <Navigation className="w-3.5 h-3.5" />,
    color: "text-cyan-400",
    label: "Navigation Started",
  },
  navigation_completed: {
    icon: <Navigation className="w-3.5 h-3.5" />,
    color: "text-cyan-400",
    label: "Navigation Completed",
  },
  path_found: {
    icon: <Navigation className="w-3.5 h-3.5" />,
    color: "text-cyan-400",
    label: "Path Found",
  },
  picker_enabled: {
    icon: <MousePointer className="w-3.5 h-3.5" />,
    color: "text-amber-400",
    label: "Picker Enabled",
  },
  picker_disabled: {
    icon: <MousePointer className="w-3.5 h-3.5" />,
    color: "text-muted-foreground",
    label: "Picker Disabled",
  },
  error: {
    icon: <AlertCircle className="w-3.5 h-3.5" />,
    color: "text-red-400",
    label: "Error",
  },
};

const ACTION_ICONS: Record<string, React.ReactNode> = {
  click: <MousePointer className="w-3 h-3" />,
  type: <Type className="w-3 h-3" />,
  focus: <Eye className="w-3 h-3" />,
  blur: <Eye className="w-3 h-3" />,
};

export function EventTimelineView({
  events,
  loading = false,
  maxEvents = 100,
}: EventTimelineViewProps) {
  const [filterType, setFilterType] = useState<string | null>(null);
  const [expandedIds, setExpandedIds] = useState<Set<number>>(new Set());
  const [showFilters, setShowFilters] = useState(false);

  // Get unique event types for filtering
  const eventTypes = useMemo(() => {
    const types = new Set(events.map((e) => e.eventType));
    return Array.from(types).sort();
  }, [events]);

  // Filter and limit events
  const filteredEvents = useMemo(() => {
    let result = events;
    if (filterType) {
      result = result.filter((e) => e.eventType === filterType);
    }
    return result.slice(0, maxEvents);
  }, [events, filterType, maxEvents]);

  // Format timestamp
  const formatTime = (timestamp: number): string => {
    const date = new Date(timestamp);
    return date.toLocaleTimeString(undefined, {
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
      fractionalSecondDigits: 3,
    } as Intl.DateTimeFormatOptions);
  };

  // Format duration
  const formatDuration = (ms?: number): string => {
    if (ms === undefined) return "";
    if (ms < 1) return "<1ms";
    if (ms < 1000) return `${Math.round(ms)}ms`;
    return `${(ms / 1000).toFixed(2)}s`;
  };

  // Toggle event expansion
  const toggleExpand = (id: number) => {
    setExpandedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) {
        next.delete(id);
      } else {
        next.add(id);
      }
      return next;
    });
  };

  // Get config for event type
  const getEventConfig = (eventType: string) => {
    return (
      EVENT_TYPE_CONFIG[eventType] || {
        icon: <Activity className="w-3.5 h-3.5" />,
        color: "text-muted-foreground",
        label: eventType,
      }
    );
  };

  // Render event details
  const renderEventDetails = (event: UIBridgeEvent) => {
    const details: { label: string; value: string | React.ReactNode }[] = [];

    if (event.elementId) {
      details.push({ label: "Element", value: event.elementId });
    }
    if (event.stateId) {
      details.push({ label: "State", value: event.stateId });
    }
    if (event.transitionId) {
      details.push({ label: "Transition", value: event.transitionId });
    }
    if (event.action) {
      details.push({
        label: "Action",
        value: (
          <span className="flex items-center gap-1">
            {ACTION_ICONS[event.action] || null}
            {event.action}
          </span>
        ),
      });
    }
    if (event.params && Object.keys(event.params).length > 0) {
      details.push({
        label: "Params",
        value: (
          <code className="text-[10px] bg-muted/50 px-1 rounded">
            {JSON.stringify(event.params)}
          </code>
        ),
      });
    }
    if (event.result && Object.keys(event.result).length > 0) {
      details.push({
        label: "Result",
        value: (
          <code className="text-[10px] bg-muted/50 px-1 rounded">
            {JSON.stringify(event.result)}
          </code>
        ),
      });
    }
    if (event.errorMessage) {
      details.push({
        label: "Error",
        value: <span className="text-destructive">{event.errorMessage}</span>,
      });
    }

    return details;
  };

  if (loading && events.length === 0) {
    return (
      <div className="flex items-center justify-center h-full text-muted-foreground text-sm">
        Loading events...
      </div>
    );
  }

  if (events.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center h-full text-muted-foreground text-sm gap-2">
        <Activity className="w-8 h-8 opacity-50" />
        <p>No events recorded yet</p>
        <p className="text-xs">Events will appear here as actions are performed</p>
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full">
      {/* Filter bar */}
      <div className="flex items-center gap-2 mb-2">
        <button
          className="flex items-center gap-1 px-2 py-1.5 text-xs bg-muted/30 border border-border/50 rounded-md hover:bg-muted/50 transition-colors"
          onClick={() => setShowFilters(!showFilters)}
        >
          <Filter className="w-3.5 h-3.5" />
          Filter
          {filterType && (
            <Badge variant="default" className="ml-1 text-[10px]">
              1
            </Badge>
          )}
        </button>
        {filterType && (
          <button
            className="text-xs text-muted-foreground hover:text-foreground"
            onClick={() => setFilterType(null)}
          >
            Clear
          </button>
        )}
        <div className="flex-1" />
        <span className="text-xs text-muted-foreground">
          {filteredEvents.length} of {events.length} events
        </span>
      </div>

      {/* Filter dropdown */}
      {showFilters && (
        <div className="flex flex-wrap gap-1 mb-2 p-2 bg-muted/20 rounded-md">
          {eventTypes.map((type) => {
            const config = getEventConfig(type);
            return (
              <button
                key={type}
                className={cn(
                  "flex items-center gap-1 px-2 py-1 text-xs rounded-md transition-colors",
                  filterType === type
                    ? "bg-primary text-primary-foreground"
                    : "bg-muted/50 hover:bg-muted",
                )}
                onClick={() => setFilterType(filterType === type ? null : type)}
              >
                <span className={config.color}>{config.icon}</span>
                {config.label}
              </button>
            );
          })}
        </div>
      )}

      {/* Event list */}
      <div className="flex-1 overflow-auto">
        {filteredEvents.map((event) => {
          const config = getEventConfig(event.eventType);
          const isExpanded = expandedIds.has(event.id);
          const details = renderEventDetails(event);

          return (
            <div
              key={event.id}
              className={cn(
                "border-b border-border/30 last:border-b-0",
                !event.success && "bg-destructive/5",
              )}
            >
              {/* Event header */}
              <div
                className="flex items-center gap-2 py-2 px-2 cursor-pointer hover:bg-muted/30 transition-colors"
                onClick={() => toggleExpand(event.id)}
                onKeyDown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); toggleExpand(event.id); }}}
                role="button"
                tabIndex={0}
              >
                {/* Expand toggle */}
                <button className="w-4 h-4 flex items-center justify-center">
                  {isExpanded ? (
                    <ChevronDown className="w-3 h-3 text-muted-foreground" />
                  ) : (
                    <ChevronRight className="w-3 h-3 text-muted-foreground" />
                  )}
                </button>

                {/* Event type icon */}
                <span className={config.color}>{config.icon}</span>

                {/* Event description */}
                <span className="flex-1 text-xs truncate">
                  <span className="font-medium">{config.label}</span>
                  {event.elementId && (
                    <span className="text-muted-foreground ml-1">on {event.elementId}</span>
                  )}
                  {event.action && (
                    <span className="text-muted-foreground ml-1">({event.action})</span>
                  )}
                </span>

                {/* Status indicator */}
                {event.success ? (
                  <CheckCircle className="w-3.5 h-3.5 text-green-400" />
                ) : (
                  <XCircle className="w-3.5 h-3.5 text-red-400" />
                )}

                {/* Duration */}
                {event.durationMs !== undefined && (
                  <span className="text-[10px] text-muted-foreground flex items-center gap-0.5">
                    <Clock className="w-3 h-3" />
                    {formatDuration(event.durationMs)}
                  </span>
                )}

                {/* Timestamp */}
                <span className="text-[10px] text-muted-foreground font-mono">
                  {formatTime(event.timestamp)}
                </span>
              </div>

              {/* Event details */}
              {isExpanded && details.length > 0 && (
                <div className="px-8 pb-2">
                  <div className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-xs">
                    {details.map(({ label, value }, i) => (
                      <div key={`${label}-${i}`} className="contents">
                        <span className="text-muted-foreground">{label}:</span>
                        <span className="font-mono">{value}</span>
                      </div>
                    ))}
                  </div>
                </div>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}

export default EventTimelineView;
