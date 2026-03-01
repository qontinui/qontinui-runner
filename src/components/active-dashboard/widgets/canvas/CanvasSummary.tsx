/**
 * CanvasSummary Component
 *
 * Compact summary view for canvas widget.
 * Shows panel count and titles of first few panels.
 */

import { LayoutDashboard } from "lucide-react";
import { cn } from "../../../../lib/utils";
import { Badge } from "../../../ui";
import { getAccentColors } from "@/design-system";
import type { CanvasSummaryProps } from "./types";

/**
 * Compact summary widget for canvas panels.
 */
export function CanvasSummary({ data }: CanvasSummaryProps) {
  const { panels, panelCount } = data;
  const recentPanels = panels.slice(0, 3);
  const roseColors = getAccentColors("rose");

  return (
    <div className="space-y-3">
      {/* Panel count */}
      <div className="flex items-center gap-2">
        <LayoutDashboard className="h-4 w-4 text-rose-500" />
        <span className="text-sm font-medium text-foreground">
          {panelCount} {panelCount === 1 ? "panel" : "panels"}
        </span>
      </div>

      {/* Recent panels */}
      {recentPanels.length > 0 ? (
        <div className="space-y-1">
          {recentPanels.map((panel) => (
            <div key={panel.panel_id} className="flex items-center gap-2 py-0.5">
              <Badge
                className={cn(
                  "text-[10px] px-1.5 py-0 font-mono border flex-shrink-0",
                  roseColors.bg,
                  roseColors.text,
                  roseColors.border,
                )}
              >
                {panel.component}
              </Badge>
              <span className="text-xs text-muted-foreground truncate flex-1">{panel.title}</span>
            </div>
          ))}
          {panelCount > 3 && (
            <span className="text-xs text-muted-foreground">+{panelCount - 3} more</span>
          )}
        </div>
      ) : (
        <p className="text-xs text-muted-foreground">No panels yet</p>
      )}
    </div>
  );
}

export default CanvasSummary;
