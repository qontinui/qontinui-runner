/**
 * The maximized view's header: "Maximized", the session's title and info, its
 * prompts / review toggles, previous / next terminal, and delete-and-forward.
 *
 * It sits over the maximized zone's cell (`ZoneCell` with the `maximized`
 * presentation) in place of the zone chrome, so the maximized view is the
 * same keyed cell list as the grid — maximizing never remounts a terminal —
 * while keeping the header it has always had.
 */
import type { ReactNode } from "react";
import { MessageSquare, ChevronLeft, ChevronRight, Trash2 } from "lucide-react";
import type { TerminalTab } from "../useTerminalManager";
import { TabTitle } from "../displayTitle";
import { SessionInfoDropdown } from "../SessionInfoDropdown";
import { MAXIMIZED_HEADER_HEIGHT_PX } from "./constants";

/** Neighbour navigation within the maximized view. */
export interface MaximizedNav {
  /** The previous / next OCCUPIED zone, or `null` when there is none. */
  prevZone: number | null;
  nextZone: number | null;
  /** Stay maximized, on `zone`. */
  onGoToZone: (zone: number) => void;
  /** Close the maximized terminal and maximize the next one. */
  onDeleteAndForward: () => void;
}

interface MaximizedHeaderProps {
  tab: TerminalTab;
  zoneIdx: number;
  nav: MaximizedNav;
  /** Absent when the session has no prompts panel to offer. */
  onTogglePrompts?: () => void;
  promptsVisible: boolean;
  /** The review toggle, when the session offers the review panel. */
  reviewSlot?: ReactNode;
  /** The zone action cluster (restore, restart, label, export, send-to-window,
   *  close), rendered inline at the end of the header. */
  actionsSlot?: ReactNode;
}

const NAV_BUTTON =
  "p-0.5 rounded shrink-0 text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50 disabled:opacity-30 disabled:pointer-events-none ";

export function MaximizedHeader({
  tab,
  zoneIdx,
  nav,
  onTogglePrompts,
  promptsVisible,
  reviewSlot,
  actionsSlot,
}: MaximizedHeaderProps) {
  const { prevZone, nextZone, onGoToZone, onDeleteAndForward } = nav;
  return (
    <div
      className="absolute top-0 left-0 right-0 flex items-center gap-2 px-3 bg-[#13141f]/90 backdrop-blur-sm z-20 cursor-pointer"
      style={{ height: `${MAXIMIZED_HEADER_HEIGHT_PX}px` }}
    >
      <span className="text-[10px] text-[#7aa2f7] font-medium">Maximized</span>
      <span className="text-[10px] text-[#a9b1d6]">
        <TabTitle tab={tab} />
      </span>
      <SessionInfoDropdown claudeSessionId={tab.claudeSessionId} zoneIndex={zoneIdx} />
      {onTogglePrompts && (
        <button
          onClick={(e) => {
            e.stopPropagation();
            onTogglePrompts();
          }}
          onMouseDown={(e) => e.stopPropagation()}
          onDoubleClick={(e) => e.stopPropagation()}
          className={`p-0.5 rounded transition-colors shrink-0 ${
            promptsVisible
              ? "text-[#9ece6a] bg-[#9ece6a]/15"
              : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50"
          }`}
          title={promptsVisible ? "Hide my prompts" : "Show my prompts"}
          aria-pressed={promptsVisible}
        >
          <MessageSquare className="w-3 h-3" />
        </button>
      )}
      {reviewSlot}
      <div className="flex items-center gap-0.5 ml-auto">
        <button
          onClick={(e) => {
            e.stopPropagation();
            if (prevZone !== null) onGoToZone(prevZone);
          }}
          onMouseDown={(e) => e.stopPropagation()}
          onDoubleClick={(e) => e.stopPropagation()}
          disabled={prevZone === null}
          className={NAV_BUTTON}
          title="Previous terminal"
          aria-label="Previous terminal"
        >
          <ChevronLeft className="w-3 h-3" />
        </button>
        <button
          onClick={(e) => {
            e.stopPropagation();
            if (nextZone !== null) onGoToZone(nextZone);
          }}
          onMouseDown={(e) => e.stopPropagation()}
          onDoubleClick={(e) => e.stopPropagation()}
          disabled={nextZone === null}
          className={NAV_BUTTON}
          title="Next terminal"
          aria-label="Next terminal"
        >
          <ChevronRight className="w-3 h-3" />
        </button>
        <button
          onClick={(e) => {
            e.stopPropagation();
            onDeleteAndForward();
          }}
          onMouseDown={(e) => e.stopPropagation()}
          onDoubleClick={(e) => e.stopPropagation()}
          className={`${NAV_BUTTON}hover:!text-[#f7768e]`}
          title="Delete this terminal and go to the next"
          aria-label="Delete and forward"
        >
          <Trash2 className="w-3 h-3" />
        </button>
      </div>
      <span className="text-[9px] text-[#565f89]">Esc or double-click to restore</span>
      {actionsSlot}
    </div>
  );
}
