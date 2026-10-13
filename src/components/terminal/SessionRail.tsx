/**
 * The Session Manager's rail mode — Chrome's collapse-to-icons for the
 * vertical tab strip (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 3).
 *
 * One item per session in the Live list's order: the status dot over a stripe
 * of the session's group color. The dot is the only status signal here, so
 * every item's tooltip and accessible name carry the session name and BOTH
 * status words — never color alone. Phase 7 replaces the tooltip with the
 * hover card.
 */

import { PanelLeftOpen } from "lucide-react";

import { sessionCardName, sessionCardStatusDisplay } from "./SessionCard";
import { sessionOrgControlAttrs } from "./sessionOrgControls";
import type { UnifiedSession } from "./useSessionManager";

/** The rail item's tooltip: name, then activity · location. */
export function railItemTitle(session: UnifiedSession): string {
  const status = sessionCardStatusDisplay(session.status);
  return `${sessionCardName(session)}\n${status.activityLabel} · ${status.locationLabel}`;
}

export interface SessionRailProps {
  /** Rows in display order (collapsed groups' members omitted). */
  sessions: readonly UnifiedSession[];
  /** The group color of a row (a CSS value), or null. */
  groupColorOf: (session: UnifiedSession) => string | null;
  selectedSessionId: string | null;
  onOpen: (session: UnifiedSession) => void;
  onExpand: () => void;
  width: number;
}

export function SessionRail({
  sessions,
  groupColorOf,
  selectedSessionId,
  onOpen,
  onExpand,
  width,
}: SessionRailProps) {
  return (
    <div
      style={{ width }}
      className="h-full flex flex-col items-center border-r border-[#2a2d3d] bg-[#13141f] shrink-0"
      data-session-panel-mode="rail"
    >
      <button
        {...sessionOrgControlAttrs("railToggle")}
        onClick={onExpand}
        title="Expand the Session Manager"
        aria-label="Expand the Session Manager"
        aria-pressed={true}
        className="my-1.5 p-1 rounded text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d]"
      >
        <PanelLeftOpen className="w-3.5 h-3.5" />
      </button>
      <div className="flex-1 w-full overflow-y-auto scrollbar-dark flex flex-col items-stretch gap-0.5 pb-2">
        {sessions.map((session) => {
          const status = sessionCardStatusDisplay(session.status);
          const title = railItemTitle(session);
          const color = groupColorOf(session);
          return (
            <button
              key={session.sessionId}
              {...sessionOrgControlAttrs("railItem", session.sessionId)}
              data-session-id={session.sessionId}
              data-session-activity={status.activity}
              data-session-location={status.location}
              onClick={() => onOpen(session)}
              title={title}
              aria-label={title.replace("\n", ": ")}
              aria-current={session.sessionId === selectedSessionId ? "true" : undefined}
              style={{ borderLeftColor: color ?? "transparent" }}
              className={`flex items-center justify-center h-6 border-l-[3px] hover:bg-[#1a1b26] ${
                session.sessionId === selectedSessionId ? "bg-[#7aa2f7]/10" : ""
              }`}
            >
              <span
                className={`w-2 h-2 rounded-full ${status.pulse ? "animate-pulse" : ""}`}
                style={{ backgroundColor: status.color }}
              />
            </button>
          );
        })}
      </div>
    </div>
  );
}
