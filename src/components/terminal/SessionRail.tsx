/**
 * The Session Manager's rail mode — Chrome's collapse-to-icons for the
 * vertical tab strip (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 3).
 *
 * One item per session in the Live list's order: the status dot over a stripe
 * of the session's group color. The dot is the only status signal here, so
 * every item's accessible name carries the session name and BOTH status
 * words — never color alone. Hovering an item opens the session hover card
 * beside the rail (Phase 7): the card is the rail item's body, so the item
 * carries no native tooltip that would compete with it. Without a hover-card
 * binding the item falls back to the tooltip. An unseen session's item
 * carries the unseen mark too.
 */

import { PanelLeftOpen } from "lucide-react";

import { sessionCardName, sessionCardStatusDisplay } from "./SessionCard";
import { sessionOrgControlAttrs } from "./sessionOrgControls";
import type { SessionHoverCardController } from "./SessionHoverCard";
import { unseenAttr, type UnseenVerdict } from "./sessionUnseen";
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
  /** The panel's hover-card binding; absent = the native tooltip instead. */
  hoverCard?: SessionHoverCardController["bind"];
  /** A row's unseen-activity verdict; absent = not tracked. */
  unseenOf?: (session: UnifiedSession) => UnseenVerdict;
}

export function SessionRail({
  sessions,
  groupColorOf,
  selectedSessionId,
  onOpen,
  onExpand,
  width,
  hoverCard,
  unseenOf,
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
          const hover = hoverCard?.(session.sessionId);
          const unseen = unseenOf?.(session);
          const isUnseen = unseen?.state === "unseen";
          const label = `${title.replace("\n", ": ")}${isUnseen ? " (new activity)" : ""}`;
          return (
            <button
              key={session.sessionId}
              {...sessionOrgControlAttrs("railItem", session.sessionId)}
              data-session-id={session.sessionId}
              data-session-activity={status.activity}
              data-session-location={status.location}
              data-session-unseen={unseen ? unseenAttr(unseen) : "untracked"}
              onClick={() => onOpen(session)}
              onMouseEnter={hover?.onMouseEnter}
              onMouseLeave={hover?.onMouseLeave}
              title={hover ? undefined : title}
              aria-label={label}
              aria-current={session.sessionId === selectedSessionId ? "true" : undefined}
              style={{ borderLeftColor: color ?? "transparent" }}
              className={`relative flex items-center justify-center h-6 border-l-[3px] hover:bg-[#1a1b26] ${
                session.sessionId === selectedSessionId ? "bg-[#7aa2f7]/10" : ""
              }`}
            >
              <span
                className={`w-2 h-2 rounded-full ${status.pulse ? "animate-pulse" : ""}`}
                style={{ backgroundColor: status.color }}
              />
              {isUnseen && (
                <span
                  {...sessionOrgControlAttrs("unseenDot", session.sessionId)}
                  aria-hidden="true"
                  className="absolute top-0.5 right-1.5 w-1 h-1 rounded-full bg-brand-primary"
                />
              )}
            </button>
          );
        })}
      </div>
    </div>
  );
}
