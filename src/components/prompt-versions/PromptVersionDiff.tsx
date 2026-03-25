/**
 * PromptVersionDiff — Displays a unified or side-by-side diff between two
 * prompt template versions with syntax highlighting (green/red).
 */

import { useState, useMemo } from "react";
import { ArrowLeft, Loader2, Columns2, AlignJustify } from "lucide-react";
import { usePromptVersionDiff } from "@/hooks/usePromptVersions";

interface PromptVersionDiffProps {
  templateId: string;
  versionA: number;
  versionB: number;
  onBack: () => void;
}

type DiffViewMode = "inline" | "side-by-side";

interface DiffLine {
  type: "addition" | "deletion" | "context" | "header";
  content: string;
}

function parseDiffLines(unifiedDiff: string): DiffLine[] {
  return unifiedDiff.split("\n").map((line) => {
    if (line.startsWith("@@")) return { type: "header", content: line };
    if (line.startsWith("+")) return { type: "addition", content: line };
    if (line.startsWith("-")) return { type: "deletion", content: line };
    return { type: "context", content: line };
  });
}

function buildSideBySide(lines: DiffLine[]): { left: DiffLine[]; right: DiffLine[] } {
  const left: DiffLine[] = [];
  const right: DiffLine[] = [];

  for (const line of lines) {
    if (line.type === "header") {
      left.push(line);
      right.push(line);
    } else if (line.type === "deletion") {
      left.push(line);
      right.push({ type: "context", content: "" });
    } else if (line.type === "addition") {
      left.push({ type: "context", content: "" });
      right.push(line);
    } else {
      left.push(line);
      right.push(line);
    }
  }

  return { left, right };
}

const LINE_STYLES: Record<DiffLine["type"], string> = {
  addition: "bg-green-900/30 text-green-300",
  deletion: "bg-red-900/30 text-red-300",
  context: "text-muted-foreground",
  header: "text-primary bg-blue-900/20 font-semibold",
};

export function PromptVersionDiff({
  templateId,
  versionA,
  versionB,
  onBack,
}: PromptVersionDiffProps) {
  const { data: diff, isLoading, error } = usePromptVersionDiff(templateId, versionA, versionB);
  const [viewMode, setViewMode] = useState<DiffViewMode>("inline");

  const parsedLines = useMemo(
    () => (diff?.unified_diff ? parseDiffLines(diff.unified_diff) : []),
    [diff],
  );

  const sideBySide = useMemo(() => buildSideBySide(parsedLines), [parsedLines]);

  if (isLoading) {
    return (
      <div className="flex items-center justify-center h-40">
        <Loader2 className="w-5 h-5 animate-spin text-muted-foreground" />
      </div>
    );
  }

  if (error || !diff) {
    return (
      <div className="p-4 space-y-4">
        <button
          onClick={onBack}
          className="flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
        >
          <ArrowLeft className="w-4 h-4" />
          Back
        </button>
        <div className="text-red-400 text-sm">Failed to load diff.</div>
      </div>
    );
  }

  return (
    <div className="p-4 space-y-4">
      {/* Header */}
      <div className="flex items-center justify-between">
        <button
          onClick={onBack}
          className="flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
        >
          <ArrowLeft className="w-4 h-4" />
          Back to history
        </button>

        <div className="flex items-center gap-3">
          {/* Stats */}
          <span className="text-xs text-green-400">+{diff.additions}</span>
          <span className="text-xs text-red-400">-{diff.deletions}</span>

          {/* View mode toggle */}
          <div className="flex items-center bg-muted rounded overflow-hidden">
            <button
              onClick={() => setViewMode("inline")}
              className={`flex items-center gap-1 px-2 py-1 text-xs transition-colors ${
                viewMode === "inline"
                  ? "bg-muted/60 text-foreground"
                  : "text-muted-foreground hover:text-foreground"
              }`}
              title="Inline view"
            >
              <AlignJustify className="w-3.5 h-3.5" />
              Inline
            </button>
            <button
              onClick={() => setViewMode("side-by-side")}
              className={`flex items-center gap-1 px-2 py-1 text-xs transition-colors ${
                viewMode === "side-by-side"
                  ? "bg-muted/60 text-foreground"
                  : "text-muted-foreground hover:text-foreground"
              }`}
              title="Side-by-side view"
            >
              <Columns2 className="w-3.5 h-3.5" />
              Side-by-side
            </button>
          </div>
        </div>
      </div>

      {/* Version labels */}
      <div className="flex items-center gap-4 text-xs text-muted-foreground">
        <span>
          Comparing{" "}
          <span className="font-mono text-foreground px-1.5 py-0.5 rounded bg-muted">
            v{versionA}
          </span>{" "}
          with{" "}
          <span className="font-mono text-foreground px-1.5 py-0.5 rounded bg-muted">
            v{versionB}
          </span>
        </span>
      </div>

      {/* Diff content */}
      {parsedLines.length === 0 ? (
        <div className="text-muted-foreground text-sm py-8 text-center">No differences found.</div>
      ) : viewMode === "inline" ? (
        <div className="bg-card border border-border rounded-lg overflow-auto max-h-[32rem]">
          <pre className="text-xs font-mono p-0">
            {parsedLines.map((line, i) => (
              <div key={i} className={`px-4 py-0.5 ${LINE_STYLES[line.type]}`}>
                {line.content || "\u00A0"}
              </div>
            ))}
          </pre>
        </div>
      ) : (
        <div className="grid grid-cols-2 gap-1">
          {/* Left (version A) */}
          <div className="bg-card border border-border rounded-lg overflow-auto max-h-[32rem]">
            <div className="text-xs text-muted-foreground px-3 py-1 border-b border-border font-medium">
              v{versionA}
            </div>
            <pre className="text-xs font-mono p-0">
              {sideBySide.left.map((line, i) => (
                <div key={i} className={`px-4 py-0.5 ${LINE_STYLES[line.type]}`}>
                  {line.content || "\u00A0"}
                </div>
              ))}
            </pre>
          </div>

          {/* Right (version B) */}
          <div className="bg-card border border-border rounded-lg overflow-auto max-h-[32rem]">
            <div className="text-xs text-muted-foreground px-3 py-1 border-b border-border font-medium">
              v{versionB}
            </div>
            <pre className="text-xs font-mono p-0">
              {sideBySide.right.map((line, i) => (
                <div key={i} className={`px-4 py-0.5 ${LINE_STYLES[line.type]}`}>
                  {line.content || "\u00A0"}
                </div>
              ))}
            </pre>
          </div>
        </div>
      )}
    </div>
  );
}

export default PromptVersionDiff;
