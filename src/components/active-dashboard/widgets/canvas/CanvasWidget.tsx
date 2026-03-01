/**
 * CanvasWidget Component
 *
 * Full widget view for canvas panels.
 * Renders agent-created panels using the component registry.
 */

import { useState } from "react";
import { LayoutDashboard, ChevronRight, ChevronDown, X } from "lucide-react";
import { cn } from "../../../../lib/utils";
import { Badge, ScrollArea } from "../../../ui";
import { getAccentColors } from "@/design-system";
import { CanvasComponentRegistry } from "./components/CanvasComponentRegistry";
import type { CanvasWidgetProps, CanvasPanel } from "./types";

/**
 * Individual panel card.
 */
function PanelCard({
  panel,
  isExpanded,
  onToggle,
}: {
  panel: CanvasPanel;
  isExpanded: boolean;
  onToggle: () => void;
}) {
  const roseColors = getAccentColors("rose");
  const Component = CanvasComponentRegistry.get(panel.component);

  return (
    <div className="border-b border-border/50 last:border-b-0">
      {/* Panel header */}
      <div
        className={cn(
          "flex items-center gap-3 px-4 py-2.5 cursor-pointer transition-colors",
          isExpanded
            ? "bg-muted/50 border-l-2 border-primary"
            : "hover:bg-muted/30 border-l-2 border-transparent",
        )}
        onClick={onToggle}
      >
        <div className="flex-shrink-0 text-muted-foreground">
          {isExpanded ? <ChevronDown className="h-4 w-4" /> : <ChevronRight className="h-4 w-4" />}
        </div>

        <Badge
          className={cn(
            "text-[10px] px-1.5 py-0 font-mono border",
            roseColors.bg,
            roseColors.text,
            roseColors.border,
          )}
        >
          {panel.component}
        </Badge>

        <span className="text-sm font-medium text-foreground flex-1 truncate">{panel.title}</span>

        {panel.size && panel.size !== "normal" && (
          <Badge variant="muted" className="text-[10px]">
            {panel.size}
          </Badge>
        )}
      </div>

      {/* Panel content */}
      {isExpanded && (
        <div
          className={cn(
            "border-l-2 border-primary bg-muted/20 px-4 py-3",
            panel.size === "compact"
              ? "max-h-[200px]"
              : panel.size === "large"
                ? "max-h-[600px]"
                : "max-h-[400px]",
            "overflow-y-auto",
          )}
        >
          <Component data={panel.data} size={panel.size} />
        </div>
      )}
    </div>
  );
}

/**
 * Full Canvas widget component.
 */
export function CanvasWidget({ isSummary, data, className }: CanvasWidgetProps) {
  const [expandedPanelId, setExpandedPanelId] = useState<string | null>(null);

  // If summary mode, don't render (use CanvasSummary instead)
  if (isSummary) {
    return null;
  }

  const handleToggle = (panelId: string) => {
    setExpandedPanelId((prev) => (prev === panelId ? null : panelId));
  };

  // Auto-expand first panel if only one
  const effectiveExpanded = data.panels.length === 1 ? data.panels[0].panel_id : expandedPanelId;

  return (
    <div className={cn("flex flex-col h-full overflow-hidden", className)}>
      {/* Header */}
      <div className="flex items-center justify-between border-b border-border px-4 py-2 bg-muted/10 flex-shrink-0">
        <div className="flex items-center gap-3">
          <LayoutDashboard className="h-4 w-4 text-rose-500" />
          <h3 className="text-sm font-semibold text-foreground">Canvas</h3>
          <span className="text-xs text-muted-foreground">Agent-rendered panels</span>
        </div>
        <Badge variant="muted" className="text-xs">
          {data.panelCount}
        </Badge>
      </div>

      {/* Panel list */}
      <ScrollArea className="flex-1">
        {data.panels.map((panel) => (
          <PanelCard
            key={panel.panel_id}
            panel={panel}
            isExpanded={panel.panel_id === effectiveExpanded}
            onToggle={() => handleToggle(panel.panel_id)}
          />
        ))}
        {data.panels.length === 0 && (
          <div className="flex items-center justify-center h-24 text-muted-foreground text-sm">
            No canvas panels yet
          </div>
        )}
      </ScrollArea>
    </div>
  );
}

export default CanvasWidget;
