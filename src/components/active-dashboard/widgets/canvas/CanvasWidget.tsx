/**
 * CanvasWidget Component
 *
 * Full widget view for canvas panels.
 * Renders agent-created panels using the component registry.
 */

import { useState, useEffect, useMemo, useCallback } from "react";
import { LayoutDashboard, X } from "lucide-react";
import * as Dialog from "@radix-ui/react-dialog";
import { cn } from "../../../../lib/utils";
import { Badge, ScrollArea } from "../../../ui";
import { PanelCard, PanelContent } from "./components/PanelCard";
import { EmptyState } from "../shared";
import type { CanvasWidgetProps, CanvasPanel } from "./types";

/**
 * Full Canvas widget component.
 */
export function CanvasWidget({
  isSummary,
  hideHeader,
  data,
  className,
}: CanvasWidgetProps & { hideHeader?: boolean }) {
  const [expandedIds, setExpandedIds] = useState<Set<string>>(new Set());
  const [maximizedPanelId, setMaximizedPanelId] = useState<string | null>(null);
  const [typeFilter, setTypeFilter] = useState<Set<string>>(new Set());

  // Auto-expand all panels when new ones arrive
  useEffect(() => {
    if (data.panels.length > 0) {
      setExpandedIds((prev) => {
        const next = new Set(prev);
        for (const panel of data.panels) {
          // Expand newly added panels
          if (!next.has(panel.panel_id) && prev.size < data.panels.length) {
            next.add(panel.panel_id);
          }
        }
        // If this is the initial load (nothing was expanded), expand all
        if (prev.size === 0) {
          for (const panel of data.panels) {
            next.add(panel.panel_id);
          }
        }
        return next;
      });
    }
  }, [data.panels]);

  const handleExpandAll = useCallback(() => {
    setExpandedIds(new Set(data.panels.map((p) => p.panel_id)));
  }, [data.panels]);

  const handleCollapseAll = useCallback(() => {
    setExpandedIds(new Set());
  }, []);

  // Compute available component types for filter chips
  const availableTypes = useMemo(() => {
    const types = new Set<string>();
    for (const panel of data.panels) {
      types.add(panel.component);
    }
    return Array.from(types).sort();
  }, [data.panels]);

  // Filter panels by type
  const filteredPanels = useMemo(() => {
    if (typeFilter.size === 0) return data.panels;
    return data.panels.filter((p) => typeFilter.has(p.component));
  }, [data.panels, typeFilter]);

  // Group panels by group field
  const { groupedPanels, ungroupedPanels } = useMemo(() => {
    const groups = new Map<string, CanvasPanel[]>();
    const ungrouped: CanvasPanel[] = [];
    for (const panel of filteredPanels) {
      const group = (panel as CanvasPanel & { group?: string }).group;
      if (group) {
        const arr = groups.get(group) || [];
        arr.push(panel);
        groups.set(group, arr);
      } else {
        ungrouped.push(panel);
      }
    }
    return { groupedPanels: groups, ungroupedPanels: ungrouped };
  }, [filteredPanels]);

  // If summary mode, don't render (use CanvasSummary instead)
  if (isSummary) {
    return null;
  }

  const handleToggle = (panelId: string) => {
    setExpandedIds((prev) => {
      const next = new Set(prev);
      if (next.has(panelId)) {
        next.delete(panelId);
      } else {
        next.add(panelId);
      }
      return next;
    });
  };

  const handleToggleTypeFilter = (type: string) => {
    setTypeFilter((prev) => {
      const next = new Set(prev);
      if (next.has(type)) {
        next.delete(type);
      } else {
        next.add(type);
      }
      return next;
    });
  };

  const maximizedPanel = maximizedPanelId
    ? data.panels.find((p) => p.panel_id === maximizedPanelId)
    : null;

  const renderPanelCard = (panel: CanvasPanel) => (
    <PanelCard
      key={panel.panel_id}
      panel={panel}
      isExpanded={expandedIds.has(panel.panel_id)}
      onToggle={() => handleToggle(panel.panel_id)}
      onMaximize={() => setMaximizedPanelId(panel.panel_id)}
    />
  );

  return (
    <div className={cn("flex flex-col h-full overflow-hidden", className)}>
      {/* Header */}
      {!hideHeader && (
        <div className="flex items-center justify-between border-b border-border px-4 py-2 bg-muted/10 flex-shrink-0">
          <div className="flex items-center gap-3">
            <LayoutDashboard className="h-4 w-4 text-rose-500" />
            <h3 className="text-sm font-semibold text-foreground">Canvas</h3>
            <span className="text-xs text-muted-foreground">Agent-rendered panels</span>
          </div>
          <div className="flex items-center gap-2">
            <button
              onClick={handleExpandAll}
              className="px-2 py-1 text-xs text-muted-foreground hover:text-foreground transition-colors"
            >
              Expand
            </button>
            <button
              onClick={handleCollapseAll}
              className="px-2 py-1 text-xs text-muted-foreground hover:text-foreground transition-colors"
            >
              Collapse
            </button>
            <Badge variant="muted" className="text-xs">
              {data.panelCount}
            </Badge>
          </div>
        </div>
      )}

      {/* Type filter chips */}
      {availableTypes.length >= 2 && (
        <div className="flex items-center gap-1.5 px-4 py-1.5 border-b border-border/50 flex-wrap bg-muted/5">
          {availableTypes.map((type) => {
            const isActive = typeFilter.has(type);
            return (
              <button
                key={type}
                onClick={() => handleToggleTypeFilter(type)}
                className={cn(
                  "text-[10px] px-2 py-0.5 rounded-full border transition-colors",
                  isActive
                    ? "bg-primary/20 border-primary/50 text-primary"
                    : "border-border/50 text-muted-foreground hover:text-foreground hover:border-border",
                )}
              >
                {type}
              </button>
            );
          })}
          {typeFilter.size > 0 && (
            <button
              onClick={() => setTypeFilter(new Set())}
              className="text-[10px] px-2 py-0.5 text-muted-foreground hover:text-foreground transition-colors"
            >
              Clear
            </button>
          )}
        </div>
      )}

      {/* Panel list */}
      <ScrollArea className="flex-1">
        {/* Grouped panels */}
        {Array.from(groupedPanels.entries()).map(([groupName, panels]) => (
          <div key={groupName}>
            <div className="px-4 py-1.5 bg-muted/30 border-b border-border/30">
              <span className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
                {groupName}
              </span>
              <Badge variant="muted" className="text-[10px] ml-2">
                {panels.length}
              </Badge>
            </div>
            {panels.map(renderPanelCard)}
          </div>
        ))}

        {/* Ungrouped panels */}
        {ungroupedPanels.map(renderPanelCard)}

        {filteredPanels.length === 0 && data.panels.length > 0 && (
          <div className="p-4 text-center text-sm text-muted-foreground">
            No panels match the selected filters
          </div>
        )}

        {data.panels.length === 0 && (
          <EmptyState
            icon={LayoutDashboard}
            title="No canvas panels"
            description="The agent can render rich visual panels here during execution"
          />
        )}
      </ScrollArea>

      {/* Maximize modal */}
      <Dialog.Root open={!!maximizedPanel} onOpenChange={() => setMaximizedPanelId(null)}>
        <Dialog.Portal>
          <Dialog.Overlay className="fixed inset-0 z-50 bg-black/50" />
          <Dialog.Content className="fixed inset-4 z-50 bg-background border rounded-lg flex flex-col overflow-hidden">
            {maximizedPanel && (
              <>
                <header className="flex items-center gap-3 px-4 py-3 border-b border-border bg-muted/10">
                  <Badge variant="muted" className="text-[10px] px-1.5 py-0 font-mono">
                    {maximizedPanel.component}
                  </Badge>
                  <h2 className="text-sm font-semibold text-foreground flex-1 truncate">
                    {maximizedPanel.title}
                  </h2>
                  <Dialog.Close asChild>
                    <button
                      type="button"
                      className="p-1.5 rounded text-muted-foreground hover:text-foreground hover:bg-muted/50 transition-colors"
                    >
                      <X className="h-4 w-4" />
                    </button>
                  </Dialog.Close>
                </header>
                <ScrollArea className="flex-1 p-4">
                  <PanelContent panel={maximizedPanel} />
                </ScrollArea>
              </>
            )}
          </Dialog.Content>
        </Dialog.Portal>
      </Dialog.Root>
    </div>
  );
}

export default CanvasWidget;
