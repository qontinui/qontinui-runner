/**
 * CodeDiffPanel Component
 *
 * Renders a unified diff view with basic syntax coloring.
 */

import { cn } from "../../../../../lib/utils";
import type { CanvasPanelComponentProps } from "./CanvasComponentRegistry";

export function CodeDiffPanel({ data }: CanvasPanelComponentProps) {
  const filePath = (data.file_path as string) ?? "";
  const language = (data.language as string) ?? "";
  const unifiedDiff = (data.unified_diff as string) ?? "";
  const oldContent = (data.old_content as string) ?? "";
  const newContent = (data.new_content as string) ?? "";

  // Prefer unified diff if provided
  const diffText = unifiedDiff || generateSimpleDiff(oldContent, newContent);

  if (!diffText && !oldContent && !newContent) {
    return <p className="text-sm text-muted-foreground italic">No diff data</p>;
  }

  const lines = diffText.split("\n");

  return (
    <div className="space-y-2">
      {/* File path header */}
      {filePath && (
        <div className="flex items-center gap-2 text-xs text-muted-foreground">
          <span className="font-mono">{filePath}</span>
          {language && <span className="text-[10px] px-1.5 py-0 bg-muted rounded">{language}</span>}
        </div>
      )}

      {/* Diff content */}
      <pre className="text-xs font-mono overflow-auto bg-muted/30 rounded p-2">
        {lines.map((line, idx) => (
          <div
            key={idx}
            className={cn(
              "px-1",
              line.startsWith("+") && !line.startsWith("+++") && "bg-green-500/10 text-green-400",
              line.startsWith("-") && !line.startsWith("---") && "bg-red-500/10 text-red-400",
              line.startsWith("@@") && "text-cyan-400",
              line.startsWith("diff ") && "text-muted-foreground font-bold",
            )}
          >
            {line}
          </div>
        ))}
      </pre>
    </div>
  );
}

/**
 * Generate a simple diff when only old/new content is provided.
 */
function generateSimpleDiff(oldContent: string, newContent: string): string {
  if (!oldContent && !newContent) return "";

  const lines: string[] = [];
  if (oldContent) {
    for (const line of oldContent.split("\n")) {
      lines.push(`- ${line}`);
    }
  }
  if (newContent) {
    for (const line of newContent.split("\n")) {
      lines.push(`+ ${line}`);
    }
  }
  return lines.join("\n");
}
