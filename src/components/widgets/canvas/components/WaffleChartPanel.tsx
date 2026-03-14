/**
 * WaffleChartPanel Component
 *
 * Grid of small colored squares representing check coverage.
 * A percentage grid / waffle chart visualization.
 */

import { cn } from "@/lib/utils";
import type { CanvasPanelComponentProps } from "./CanvasComponentRegistry";

interface WaffleCell {
  label: string;
  status: "pass" | "fail" | "pending" | "running" | "skip";
}

const STATUS_COLORS: Record<string, string> = {
  pass: "#22c55e",
  fail: "#ef4444",
  pending: "#a3a3a3",
  running: "#3b82f6",
  skip: "#6b7280",
};

const STATUS_LABELS: Record<string, string> = {
  pass: "pass",
  fail: "fail",
  pending: "pending",
  running: "running",
  skip: "skip",
};

export function WaffleChartPanel({ data, size }: CanvasPanelComponentProps) {
  const cells = (data.cells as WaffleCell[]) ?? [];
  const columns = (data.columns as number) ?? 10;
  const isCompact = size === "compact";

  if (cells.length === 0) {
    return <p className="text-sm text-muted-foreground italic">No data</p>;
  }

  // Count statuses for summary
  const counts: Record<string, number> = {};
  for (const cell of cells) {
    const status = cell.status ?? "pending";
    counts[status] = (counts[status] ?? 0) + 1;
  }

  const cellSize = isCompact ? "w-3 h-3" : "w-4 h-4";
  const fontSize = isCompact ? "text-[10px]" : "text-xs";

  return (
    <div className="space-y-2">
      {/* Waffle grid */}
      <div
        className="gap-1"
        style={{
          display: "grid",
          gridTemplateColumns: `repeat(${columns}, 1fr)`,
        }}
      >
        {cells.map((cell, idx) => {
          const status = cell.status ?? "pending";
          const color = STATUS_COLORS[status] ?? STATUS_COLORS.pending;
          const isRunning = status === "running";

          return (
            <div
              key={idx}
              className={cn("rounded-sm", cellSize, isRunning && "animate-pulse")}
              style={{ backgroundColor: color }}
              title={cell.label}
            />
          );
        })}
      </div>

      {/* Summary counts */}
      <div className={cn("flex flex-wrap gap-x-1 text-muted-foreground", fontSize)}>
        {Object.entries(counts).map(([status, count], idx, arr) => (
          <span key={status} className="flex items-center gap-1">
            <span
              className="inline-block h-2 w-2 rounded-sm flex-shrink-0"
              style={{ backgroundColor: STATUS_COLORS[status] ?? STATUS_COLORS.pending }}
            />
            <span className="font-mono text-foreground">{count}</span>
            <span>{STATUS_LABELS[status] ?? status}</span>
            {idx < arr.length - 1 && <span className="mx-0.5">&middot;</span>}
          </span>
        ))}
      </div>
    </div>
  );
}
