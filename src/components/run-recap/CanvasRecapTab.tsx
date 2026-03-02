/**
 * CanvasRecapTab Component
 *
 * Displays historical canvas panels for a completed task run.
 * Uses the extracted PanelCard component in read-only mode.
 */

import { useState, useEffect, useCallback } from "react";
import { LayoutDashboard, Loader2 } from "lucide-react";
import { Badge } from "../ui";
import { PanelCard } from "../active-dashboard/widgets/canvas/components/PanelCard";
import type { CanvasPanel } from "@qontinui/shared-types";
import { getApiBase } from "@/lib/runner-api";

interface CanvasRecapTabProps {
  taskRunId: string;
}

export function CanvasRecapTab({ taskRunId }: CanvasRecapTabProps) {
  const [panels, setPanels] = useState<CanvasPanel[]>([]);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [expandedIds, setExpandedIds] = useState<Set<string>>(new Set());

  useEffect(() => {
    let cancelled = false;

    async function fetchPanels() {
      try {
        setIsLoading(true);
        const response = await fetch(`${getApiBase()}/canvas/panels/history/${taskRunId}`);
        if (!response.ok) {
          throw new Error(`HTTP ${response.status}`);
        }
        const result = await response.json();
        if (!cancelled && result.success && result.data) {
          const fetched = result.data as CanvasPanel[];
          setPanels(fetched);
          // Auto-expand all panels
          setExpandedIds(new Set(fetched.map((p) => p.panel_id)));
        }
      } catch (e) {
        if (!cancelled) {
          setError(e instanceof Error ? e.message : "Failed to load panels");
        }
      } finally {
        if (!cancelled) {
          setIsLoading(false);
        }
      }
    }

    fetchPanels();
    return () => {
      cancelled = true;
    };
  }, [taskRunId]);

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

  const handleExpandAll = useCallback(() => {
    setExpandedIds(new Set(panels.map((p) => p.panel_id)));
  }, [panels]);

  const handleCollapseAll = useCallback(() => {
    setExpandedIds(new Set());
  }, []);

  if (isLoading) {
    return (
      <div className="flex items-center justify-center p-8 text-muted-foreground">
        <Loader2 className="w-5 h-5 animate-spin mr-2" />
        <span className="text-sm">Loading canvas panels...</span>
      </div>
    );
  }

  if (error) {
    return (
      <div className="p-4 text-sm text-muted-foreground text-center">
        Failed to load canvas panels: {error}
      </div>
    );
  }

  if (panels.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center p-8 text-muted-foreground">
        <LayoutDashboard className="w-10 h-10 mb-3 opacity-40" />
        <p className="text-sm font-medium">No Canvas Panels</p>
        <p className="text-xs mt-1">This run did not produce any canvas panels</p>
      </div>
    );
  }

  return (
    <div className="space-y-2">
      {/* Header with controls */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <LayoutDashboard className="h-4 w-4 text-rose-500" />
          <span className="text-sm font-medium text-foreground">Canvas Panels</span>
          <Badge variant="muted" className="text-xs">
            {panels.length}
          </Badge>
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
        </div>
      </div>

      {/* Panels */}
      <div className="border border-border rounded-lg overflow-hidden">
        {panels.map((panel) => (
          <PanelCard
            key={panel.panel_id}
            panel={panel}
            isExpanded={expandedIds.has(panel.panel_id)}
            onToggle={() => handleToggle(panel.panel_id)}
            readOnly
          />
        ))}
      </div>
    </div>
  );
}
