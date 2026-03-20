/**
 * TerminalPanel Component
 *
 * Renders monospace terminal output.
 */

import { cn } from "@/lib/utils";
import type { CanvasPanelComponentProps } from "./types";

export function TerminalPanel({ data, size }: CanvasPanelComponentProps) {
  const lines = (data.lines as string[]) ?? [];
  const maxLines = (data.max_lines as number) ?? 500;

  const displayLines = lines.slice(-maxLines);
  const isCompact = size === "compact";

  if (displayLines.length === 0) {
    return <p className="text-sm text-muted-foreground italic">No output</p>;
  }

  const truncated = lines.length > maxLines;

  return (
    <div className="space-y-1">
      {truncated && (
        <p className="text-[10px] text-muted-foreground">
          Showing last {maxLines} of {lines.length} lines
        </p>
      )}
      <pre
        className={cn(
          "font-mono bg-black/80 text-green-400 rounded p-3 overflow-auto",
          isCompact ? "text-[10px] max-h-[150px]" : "text-xs max-h-[400px]",
        )}
      >
        {displayLines.map((line, idx) => (
          <div key={`line-${idx}`} className="whitespace-pre-wrap break-all">
            {line}
          </div>
        ))}
      </pre>
    </div>
  );
}
