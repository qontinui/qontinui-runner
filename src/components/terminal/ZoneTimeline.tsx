import { useMemo } from "react";
import { X } from "lucide-react";
import type { TerminalTab } from "./useTerminalManager";
import type { SessionState, ZoneAssignments } from "./useZoneLayout";

interface ZoneTimelineProps {
  tabs: TerminalTab[];
  assignments: ZoneAssignments;
  sessionStates: Record<string, SessionState>;
  eventHistory: { time: number; type: string; session: string; zone?: number; color: string }[];
  onClose: () => void;
}

const STATE_COLORS: Record<string, string> = {
  idle: "#565f89",
  working: "#7aa2f7",
  "needs-input": "#e0af68",
  completed: "#9ece6a",
  error: "#f7768e",
};

// Map event types to canonical state names
function eventTypeToState(type: string): string {
  const lower = type.toLowerCase();
  if (lower === "started" || lower === "working") return "working";
  if (lower === "needs-input" || lower === "waiting") return "needs-input";
  if (lower === "completed" || lower === "done") return "completed";
  if (lower === "error") return "error";
  if (lower === "idle") return "idle";
  // Fallback: use the type as-is if it matches a known state
  if (STATE_COLORS[type]) return type;
  return "working";
}

interface TimelineSegment {
  state: string;
  startFrac: number;
  endFrac: number;
}

export function ZoneTimeline({
  tabs,
  assignments,
  sessionStates: _sessionStates,
  eventHistory,
  onClose,
}: ZoneTimelineProps) {
  // Build per-zone timeline data from eventHistory
  const { zoneRows, timeRange } = useMemo(() => {
    if (eventHistory.length === 0) {
      return { zoneRows: [], timeRange: { start: 0, end: 0 } };
    }

    const now = Date.now();
    const firstTime = eventHistory[0].time;
    const totalDuration = now - firstTime;
    const range = { start: firstTime, end: now };

    // Build reverse map: tabId -> zone index
    const tabToZone: Record<string, number> = {};
    for (const [zoneStr, tabId] of Object.entries(assignments)) {
      tabToZone[tabId] = Number(zoneStr);
    }

    // Group events by session (tabId is stored in `session` field — it's the tab title)
    // We need to match by tab title since eventHistory uses session titles
    const tabByTitle: Record<string, TerminalTab> = {};
    for (const tab of tabs) {
      tabByTitle[tab.title] = tab;
    }

    // Group events by zone
    const zoneEvents: Record<number, { time: number; state: string }[]> = {};

    for (const event of eventHistory) {
      // Try to find zone from event.zone directly
      let zone = event.zone;
      if (zone === undefined) {
        // Try to find by session title
        const tab = tabByTitle[event.session];
        if (tab && tabToZone[tab.id] !== undefined) {
          zone = tabToZone[tab.id];
        }
      }
      if (zone === undefined) continue;

      if (!zoneEvents[zone]) zoneEvents[zone] = [];
      zoneEvents[zone].push({ time: event.time, state: eventTypeToState(event.type) });
    }

    // Build rows for each occupied zone
    const rows: { zoneIndex: number; title: string; segments: TimelineSegment[] }[] = [];

    const occupiedZones = Object.entries(assignments)
      .map(([z, tabId]) => ({ zoneIndex: Number(z), tabId }))
      .sort((a, b) => a.zoneIndex - b.zoneIndex);

    for (const { zoneIndex, tabId } of occupiedZones) {
      const tab = tabs.find((t) => t.id === tabId);
      if (!tab) continue;

      const events = zoneEvents[zoneIndex];
      if (!events || events.length === 0) {
        // No events for this zone — show idle for the whole range
        rows.push({
          zoneIndex,
          title: tab.title,
          segments: [{ state: "idle", startFrac: 0, endFrac: 1 }],
        });
        continue;
      }

      // Sort events by time
      const sorted = [...events].sort((a, b) => a.time - b.time);

      // Build segments
      const segments: TimelineSegment[] = [];

      // If first event is after the timeline start, add idle segment before it
      if (sorted[0].time > firstTime && totalDuration > 0) {
        segments.push({
          state: "idle",
          startFrac: 0,
          endFrac: (sorted[0].time - firstTime) / totalDuration,
        });
      }

      for (let i = 0; i < sorted.length; i++) {
        const startTime = sorted[i].time;
        const endTime = i + 1 < sorted.length ? sorted[i + 1].time : now;
        if (totalDuration > 0) {
          segments.push({
            state: sorted[i].state,
            startFrac: (startTime - firstTime) / totalDuration,
            endFrac: (endTime - firstTime) / totalDuration,
          });
        }
      }

      rows.push({ zoneIndex, title: tab.title, segments });
    }

    return { zoneRows: rows, timeRange: range };
  }, [eventHistory, assignments, tabs]);

  // Format time labels for the axis
  const formatAxisTime = (ts: number): string => {
    const d = new Date(ts);
    return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  };

  const totalDurationMs = timeRange.end - timeRange.start;

  return (
    <div
      className="bg-[#13141f] border-b border-[#2a2d3d] shrink-0 relative"
      style={{ minHeight: 40 }}
    >
      {/* Close button */}
      <button
        onClick={onClose}
        className="absolute top-1 right-2 p-0.5 rounded hover:bg-[#2a2d3d] transition-colors text-[#565f89] hover:text-[#c0caf5] z-10"
        title="Close timeline"
      >
        <X className="w-3 h-3" />
      </button>

      {eventHistory.length === 0 ? (
        <div className="flex items-center justify-center h-10 text-[10px] text-[#565f89]">
          No state transitions recorded yet
        </div>
      ) : (
        <div className="px-3 py-1.5">
          {/* Timeline rows */}
          <div className="space-y-0.5">
            {zoneRows.map((row) => (
              <div key={row.zoneIndex} className="flex items-center gap-2 h-4">
                {/* Zone label */}
                <div
                  className="text-[9px] text-[#565f89] font-mono w-16 shrink-0 truncate"
                  title={`Zone ${row.zoneIndex + 1}: ${row.title}`}
                >
                  Z{row.zoneIndex + 1} {row.title}
                </div>
                {/* Timeline bar */}
                <div className="flex-1 h-3 rounded-sm overflow-hidden flex bg-[#1a1b26]">
                  {row.segments.map((seg, i) => {
                    const widthPct = (seg.endFrac - seg.startFrac) * 100;
                    if (widthPct <= 0) return null;
                    return (
                      <div
                        key={`seg-${seg.state}-${i}`}
                        className="h-full"
                        style={{
                          width: `${widthPct}%`,
                          backgroundColor: STATE_COLORS[seg.state] ?? STATE_COLORS.idle,
                          opacity: seg.state === "idle" ? 0.3 : 0.8,
                        }}
                        title={`${seg.state}: ${formatSegmentDuration(seg, totalDurationMs)}`}
                      />
                    );
                  })}
                </div>
              </div>
            ))}
          </div>

          {/* Time axis */}
          {totalDurationMs > 0 && (
            <div className="flex justify-between mt-1 text-[8px] text-[#565f89] font-mono px-[72px]">
              <span>{formatAxisTime(timeRange.start)}</span>
              {totalDurationMs > 60000 && (
                <span>{formatAxisTime(timeRange.start + totalDurationMs / 2)}</span>
              )}
              <span>now</span>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function formatSegmentDuration(seg: TimelineSegment, totalMs: number): string {
  const ms = (seg.endFrac - seg.startFrac) * totalMs;
  const sec = Math.floor(ms / 1000);
  if (sec < 60) return `${sec}s`;
  const min = Math.floor(sec / 60);
  if (min < 60) return `${min}m ${sec % 60}s`;
  const hr = Math.floor(min / 60);
  return `${hr}h ${min % 60}m`;
}
