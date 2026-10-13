import { MessageSquare } from "lucide-react";
import { type Ref } from "react";
import { useUIElement } from "@qontinui/ui-bridge";
import { setPromptsViewForAll, usePromptsViewGlobal } from "./promptsViewGlobal";

/**
 * Global prompts-view switch: shows or hides the operator's-prompts panel in
 * every session on every page tab at once. Per-session buttons still work on
 * top of it; flipping this one clears them (see `promptsViewGlobal.ts`).
 * Same as the `/prompts-view` slash command.
 */
export function PromptsViewToggle() {
  const { enabled } = usePromptsViewGlobal();

  const { ref } = useUIElement({
    id: "terminal-prompts-view-toggle",
    type: "button",
    label: enabled ? "Hide prompts in all sessions" : "Show prompts in all sessions",
  });

  return (
    <button
      ref={ref as Ref<HTMLButtonElement>}
      type="button"
      onClick={() => setPromptsViewForAll(!enabled)}
      aria-pressed={enabled}
      title={
        enabled
          ? "Hide my prompts in every session on every page (/prompts-view off)"
          : "Show my prompts in every session on every page (/prompts-view on)"
      }
      className={`flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] leading-none whitespace-nowrap transition-colors ${
        enabled
          ? "text-[#9ece6a] bg-[#9ece6a]/10 hover:bg-[#9ece6a]/20"
          : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50"
      }`}
    >
      <MessageSquare className="w-2.5 h-2.5" />
      Prompts
    </button>
  );
}
