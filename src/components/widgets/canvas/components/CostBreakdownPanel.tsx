/**
 * CostBreakdownPanel Component
 *
 * Horizontal bar chart showing estimated step costs.
 * Bars are colored by category, with side-effect indicators.
 *
 * Data schema:
 * {
 *   steps: [{ name, estimated_ms, category, has_side_effects }],
 *   total_estimated_ms: number
 * }
 *
 * Category -> color mapping:
 *   network         #60a5fa  (blue-400)
 *   ai_call         #c084fc  (purple-400)
 *   setup           #9ca3af  (gray-400)
 *   ui_interaction  #34d399  (emerald-400)
 */

import { cn } from "@/lib/utils";
import type { CanvasPanelComponentProps } from "./CanvasComponentRegistry";

// ============================================================================
// Data types
// ============================================================================

interface CostStep {
  name: string;
  estimated_ms: number;
  category: string;
  has_side_effects: boolean;
}

// ============================================================================
// Category color mapping
// ============================================================================

const CATEGORY_COLORS: Record<string, string> = {
  network: "#60a5fa",
  ai_call: "#c084fc",
  setup: "#9ca3af",
  ui_interaction: "#34d399",
};

const DEFAULT_COLOR = "#71717a";

// ============================================================================
// Duration formatting
// ============================================================================

function formatDuration(ms: number): string {
  if (ms >= 1000) {
    return `${(ms / 1000).toFixed(1)}s`;
  }
  return `${Math.round(ms)}ms`;
}

// ============================================================================
// Main component
// ============================================================================

export function CostBreakdownPanel({ data, size }: CanvasPanelComponentProps) {
  const steps = (data.steps as CostStep[] | undefined) ?? [];
  const totalEstimatedMs = (data.total_estimated_ms as number) ?? 0;
  const isCompact = size === "compact";

  if (steps.length === 0) {
    return <p className="text-sm text-muted-foreground italic">No cost data</p>;
  }

  const maxMs = Math.max(...steps.map((s) => s.estimated_ms), 1);
  const nameWidth = isCompact ? 100 : 130;
  const barHeight = isCompact ? "h-3.5" : "h-5";
  const fontSize = isCompact ? "text-[10px]" : "text-xs";

  return (
    <div className="space-y-1.5">
      {steps.map((step, idx) => {
        const widthPct = Math.max(1, (step.estimated_ms / maxMs) * 100);
        const barColor = CATEGORY_COLORS[step.category] ?? DEFAULT_COLOR;

        return (
          <div key={idx} className="flex items-center gap-2">
            {/* Step name + side-effect badge */}
            <div
              className={cn(
                "truncate text-muted-foreground flex-shrink-0 flex items-center gap-1",
                fontSize,
              )}
              style={{ width: `${nameWidth}px` }}
              title={step.name}
            >
              <span className="truncate">{step.name}</span>
              {step.has_side_effects && (
                <span className="flex-shrink-0 text-amber-400 font-bold" title="Has side effects">
                  !
                </span>
              )}
            </div>

            {/* Bar + duration label */}
            <div className="flex-1 flex items-center gap-1.5">
              <div
                className={cn("rounded-sm transition-all relative", barHeight)}
                style={{
                  width: `${widthPct}%`,
                  backgroundColor: barColor,
                }}
                title={`${step.name}: ${formatDuration(step.estimated_ms)} (${step.category})${step.has_side_effects ? " [side effects]" : ""}`}
              />
              <span className={cn("flex-shrink-0 font-mono text-muted-foreground", fontSize)}>
                {formatDuration(step.estimated_ms)}
              </span>
            </div>
          </div>
        );
      })}

      {/* Total */}
      {totalEstimatedMs > 0 && (
        <div
          className={cn(
            "flex items-center justify-between pt-1.5 mt-1.5 border-t border-border/50",
            fontSize,
          )}
        >
          <span className="text-muted-foreground font-medium">Total estimated</span>
          <span className="font-mono text-foreground font-semibold">
            {formatDuration(totalEstimatedMs)}
          </span>
        </div>
      )}

      {/* Category legend */}
      <div className="flex flex-wrap gap-x-3 gap-y-1 pt-1.5 mt-1 border-t border-border/30">
        {Object.entries(CATEGORY_COLORS).map(([category, color]) => (
          <div key={category} className="flex items-center gap-1 text-[10px]">
            <div className="w-2 h-2 rounded-sm" style={{ backgroundColor: color }} />
            <span className="text-zinc-400">{category.replace(/_/g, " ")}</span>
          </div>
        ))}
      </div>
    </div>
  );
}
