/**
 * CanvasSummary Component
 *
 * Compact summary view for canvas widget.
 * Shows panel count and titles of first few panels.
 */

import { LayoutDashboard } from "lucide-react";
import { cn } from "@/lib/utils";
import { Badge } from "@/components/ui";
import { getAccentColors } from "@/design-system";
import type { CanvasSummaryProps } from "./types";

/**
 * Compact summary widget for canvas panels.
 */
export function CanvasSummary({ data }: CanvasSummaryProps) {
  const { panels, panelCount } = data;
  const recentPanels = panels.slice(0, 3);
  const roseColors = getAccentColors("rose");

  // Extract stats from SummaryStats or WaffleChart panels for mini visualization
  const statsPanel = panels.find((p) => p.component === "SummaryStats");
  const wafflePanel = panels.find((p) => p.component === "WaffleChart");
  const summaryData = statsPanel?.data as
    | { passed?: number; failed?: number; skipped?: number; total?: number }
    | undefined;
  const waffleCells = (wafflePanel?.data as { cells?: Array<{ status: string }> })?.cells;

  return (
    <div className="space-y-3">
      {/* Panel count */}
      <div className="flex items-center gap-2">
        <LayoutDashboard className="h-4 w-4 text-rose-500" />
        <span className="text-sm font-medium text-foreground">
          {panelCount} {panelCount === 1 ? "panel" : "panels"}
        </span>
      </div>

      {/* Mini stats bar from SummaryStats panel */}
      {summaryData && summaryData.total && summaryData.total > 0 && (
        <div className="space-y-1">
          <div className="flex h-2 rounded-full overflow-hidden bg-muted">
            {summaryData.passed ? (
              <div
                className="bg-green-500 h-full"
                style={{ width: `${(summaryData.passed / summaryData.total) * 100}%` }}
              />
            ) : null}
            {summaryData.failed ? (
              <div
                className="bg-red-500 h-full"
                style={{ width: `${(summaryData.failed / summaryData.total) * 100}%` }}
              />
            ) : null}
            {summaryData.skipped ? (
              <div
                className="bg-gray-400 h-full"
                style={{ width: `${((summaryData.skipped ?? 0) / summaryData.total) * 100}%` }}
              />
            ) : null}
          </div>
          <div className="flex gap-2 text-[10px]">
            {summaryData.passed ? (
              <span className="text-green-500">{summaryData.passed} pass</span>
            ) : null}
            {summaryData.failed ? (
              <span className="text-red-500">{summaryData.failed} fail</span>
            ) : null}
          </div>
        </div>
      )}

      {/* Mini waffle from WaffleChart panel */}
      {!summaryData && waffleCells && waffleCells.length > 0 && (
        <div className="flex flex-wrap gap-0.5">
          {waffleCells.slice(0, 30).map((cell, i) => {
            const statusColors: Record<string, string> = {
              pass: "bg-green-500",
              fail: "bg-red-500",
              pending: "bg-gray-400",
              running: "bg-blue-500 animate-pulse",
              skip: "bg-gray-500",
            };
            return (
              <div
                key={`${cell.status}-${i}`}
                className={cn("w-2 h-2 rounded-xs", statusColors[cell.status] ?? "bg-gray-400")}
              />
            );
          })}
          {waffleCells.length > 30 && (
            <span className="text-[9px] text-muted-foreground ml-1">
              +{waffleCells.length - 30}
            </span>
          )}
        </div>
      )}

      {/* Recent panels */}
      {recentPanels.length > 0 ? (
        <div className="space-y-1">
          {recentPanels.map((panel) => (
            <div key={panel.panel_id} className="flex items-center gap-2 py-0.5">
              <Badge
                className={cn(
                  "text-[10px] px-1.5 py-0 font-mono border shrink-0",
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
