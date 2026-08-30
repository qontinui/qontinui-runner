import { Map as MapIcon } from "lucide-react";
import { type Ref } from "react";
import { useUIElement } from "@qontinui/ui-bridge";
import { useUIStateCx } from "./contexts";

/**
 * Minimap visibility toggle.
 *
 * The minimap is a floating overlay: it sits ON TOP of the zone grid rather
 * than in the layout, so the only alternative to a toggle is to give it a
 * gutter of reserved width — mostly empty space on a 3-column flow grid.
 * A toggle keeps the overlay AND gives the operator a way out when it is
 * over something they want to read.
 *
 * Its own X button hides it; before this control existed that was a one-way
 * door with nothing anywhere in the UI to undo it.
 */
export function MinimapToggle() {
  const { state, toggleMinimap } = useUIStateCx();
  const shown = state.showMinimap;

  const { ref } = useUIElement({
    id: "terminal-zone-minimap-toggle",
    type: "button",
    label: shown ? "Hide zone minimap" : "Show zone minimap",
  });

  return (
    <button
      ref={ref as Ref<HTMLButtonElement>}
      type="button"
      onClick={toggleMinimap}
      aria-pressed={shown}
      title={
        shown
          ? "Hide the zone minimap overlay"
          : "Show the zone minimap overlay (top-right of the grid)"
      }
      className={`flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] leading-none whitespace-nowrap transition-colors ${
        shown
          ? "text-[#7aa2f7] bg-[#7aa2f7]/10 hover:bg-[#7aa2f7]/20"
          : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50"
      }`}
    >
      <MapIcon className="w-2.5 h-2.5" />
      Minimap
    </button>
  );
}

