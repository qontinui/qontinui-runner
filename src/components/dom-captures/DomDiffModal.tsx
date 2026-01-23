/**
 * DOM Diff Modal Component
 *
 * Displays a side-by-side or unified diff view comparing two DOM captures.
 * Uses the 'diff' library for computing text differences.
 */

import { useState, useMemo } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { X, Copy, Download, ChevronLeft, ChevronRight, Layers } from "lucide-react";
import * as Diff from "diff";

interface DomDiffModalProps {
  isOpen: boolean;
  onClose: () => void;
  leftHtml: string;
  rightHtml: string;
  leftTitle: string;
  rightTitle: string;
  leftTimestamp?: string;
  rightTimestamp?: string;
}

type ViewMode = "unified" | "split";

export function DomDiffModal({
  isOpen,
  onClose,
  leftHtml,
  rightHtml,
  leftTitle,
  rightTitle,
  leftTimestamp,
  rightTimestamp,
}: DomDiffModalProps) {
  const [viewMode, setViewMode] = useState<ViewMode>("unified");
  const [showOnlyChanges, setShowOnlyChanges] = useState(true);

  // Compute the diff
  const diffResult = useMemo(() => {
    return Diff.diffLines(leftHtml, rightHtml);
  }, [leftHtml, rightHtml]);

  // Statistics
  const stats = useMemo(() => {
    let additions = 0;
    let deletions = 0;
    let unchanged = 0;

    diffResult.forEach((part) => {
      const lines = part.value.split("\n").filter((l) => l.length > 0).length;
      if (part.added) {
        additions += lines;
      } else if (part.removed) {
        deletions += lines;
      } else {
        unchanged += lines;
      }
    });

    return { additions, deletions, unchanged };
  }, [diffResult]);

  const copyDiff = () => {
    const diffText = diffResult
      .map((part) => {
        const prefix = part.added ? "+ " : part.removed ? "- " : "  ";
        return part.value
          .split("\n")
          .map((line) => (line ? prefix + line : ""))
          .join("\n");
      })
      .join("");

    navigator.clipboard.writeText(diffText);
  };

  const downloadDiff = () => {
    const diffText =
      `--- ${leftTitle} (${leftTimestamp || "Before"})\n+++ ${rightTitle} (${rightTimestamp || "After"})\n\n` +
      diffResult
        .map((part) => {
          const prefix = part.added ? "+" : part.removed ? "-" : " ";
          return part.value
            .split("\n")
            .map((line) => (line ? prefix + line : ""))
            .join("\n");
        })
        .join("");

    const blob = new Blob([diffText], { type: "text/plain" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = `dom-diff-${Date.now()}.diff`;
    a.click();
    URL.revokeObjectURL(url);
  };

  return (
    <Dialog.Root open={isOpen} onOpenChange={(open) => !open && onClose()}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 bg-black/70 z-50" />
        <Dialog.Content
          data-ui-id="dialog-dom-diff"
          className="fixed inset-4 bg-gray-900 rounded-lg shadow-2xl z-50 flex flex-col overflow-hidden">
          {/* Header */}
          <div className="flex items-center justify-between p-4 border-b border-gray-700">
            <div className="flex items-center gap-4">
              <Dialog.Title className="text-lg font-semibold text-white">
                DOM Comparison
              </Dialog.Title>
              <div className="flex items-center gap-2 text-sm">
                <span className="text-green-400">+{stats.additions}</span>
                <span className="text-red-400">-{stats.deletions}</span>
                <span className="text-gray-400">{stats.unchanged} unchanged</span>
              </div>
            </div>

            <div className="flex items-center gap-2">
              {/* View mode toggle */}
              <div className="flex rounded-md overflow-hidden border border-gray-600">
                <button
                  onClick={() => setViewMode("unified")}
                  className={`px-3 py-1.5 text-sm flex items-center gap-1 ${
                    viewMode === "unified"
                      ? "bg-blue-600 text-white"
                      : "bg-gray-800 text-gray-300 hover:bg-gray-700"
                  }`}
                >
                  <Layers className="w-4 h-4" />
                  Unified
                </button>
                <button
                  onClick={() => setViewMode("split")}
                  className={`px-3 py-1.5 text-sm flex items-center gap-1 ${
                    viewMode === "split"
                      ? "bg-blue-600 text-white"
                      : "bg-gray-800 text-gray-300 hover:bg-gray-700"
                  }`}
                >
                  <ChevronLeft className="w-3 h-3" />
                  <ChevronRight className="w-3 h-3" />
                  Split
                </button>
              </div>

              {/* Show only changes toggle */}
              <label className="flex items-center gap-2 text-sm text-gray-300 cursor-pointer">
                <input
                  type="checkbox"
                  checked={showOnlyChanges}
                  onChange={(e) => setShowOnlyChanges(e.target.checked)}
                  className="rounded border-gray-600 bg-gray-800 text-blue-500 focus:ring-blue-500"
                />
                Show only changes
              </label>

              <button
                onClick={copyDiff}
                className="p-2 text-gray-400 hover:text-white hover:bg-gray-700 rounded"
                title="Copy diff to clipboard"
              >
                <Copy className="w-4 h-4" />
              </button>
              <button
                onClick={downloadDiff}
                className="p-2 text-gray-400 hover:text-white hover:bg-gray-700 rounded"
                title="Download diff file"
              >
                <Download className="w-4 h-4" />
              </button>
              <Dialog.Close asChild>
                <button
                  data-ui-id="dialog-dom-diff-close-btn"
                  className="p-2 text-gray-400 hover:text-white hover:bg-gray-700 rounded"
                  title="Close"
                >
                  <X className="w-4 h-4" />
                </button>
              </Dialog.Close>
            </div>
          </div>

          {/* File headers */}
          <div className="flex border-b border-gray-700 text-sm">
            <div
              className={`flex-1 px-4 py-2 bg-red-900/20 text-red-300 ${viewMode === "split" ? "border-r border-gray-700" : ""}`}
            >
              <ChevronLeft className="w-4 h-4 inline mr-1" />
              {leftTitle}
              {leftTimestamp && <span className="text-gray-500 ml-2">({leftTimestamp})</span>}
            </div>
            {viewMode === "split" && (
              <div className="flex-1 px-4 py-2 bg-green-900/20 text-green-300">
                <ChevronRight className="w-4 h-4 inline mr-1" />
                {rightTitle}
                {rightTimestamp && <span className="text-gray-500 ml-2">({rightTimestamp})</span>}
              </div>
            )}
          </div>

          {/* Diff content */}
          <div className="flex-1 overflow-auto">
            {viewMode === "unified" ? (
              <UnifiedDiffView diffResult={diffResult} showOnlyChanges={showOnlyChanges} />
            ) : (
              <SplitDiffView diffResult={diffResult} showOnlyChanges={showOnlyChanges} />
            )}
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

interface DiffViewProps {
  diffResult: Diff.Change[];
  showOnlyChanges: boolean;
}

function UnifiedDiffView({ diffResult, showOnlyChanges }: DiffViewProps) {
  let lineNumber = 0;

  return (
    <div className="font-mono text-sm">
      {diffResult.map((part, partIndex) => {
        const lines = part.value.split("\n");
        // Remove last empty line from split
        if (lines[lines.length - 1] === "") {
          lines.pop();
        }

        if (showOnlyChanges && !part.added && !part.removed) {
          // Show context: first 3 and last 3 lines for unchanged sections
          if (lines.length > 6) {
            const contextLines = [
              ...lines.slice(0, 3),
              `... ${lines.length - 6} unchanged lines ...`,
              ...lines.slice(-3),
            ];
            lineNumber += lines.length;
            return (
              <div key={partIndex} className="text-gray-500">
                {contextLines.map((line, i) => (
                  <div key={i} className="px-4 py-0.5 hover:bg-gray-800/50">
                    <span className="inline-block w-12 text-gray-600 text-right mr-4 select-none">
                      {i === 3 ? "" : ""}
                    </span>
                    {line}
                  </div>
                ))}
              </div>
            );
          }
        }

        return (
          <div key={partIndex}>
            {lines.map((line, lineIndex) => {
              if (!part.removed) {
                lineNumber++;
              }

              const bgClass = part.added ? "bg-green-900/30" : part.removed ? "bg-red-900/30" : "";

              const textClass = part.added
                ? "text-green-300"
                : part.removed
                  ? "text-red-300"
                  : "text-gray-300";

              const prefix = part.added ? "+" : part.removed ? "-" : " ";

              return (
                <div key={lineIndex} className={`px-4 py-0.5 hover:bg-gray-800/50 ${bgClass}`}>
                  <span className="inline-block w-12 text-gray-600 text-right mr-4 select-none">
                    {!part.removed ? lineNumber : ""}
                  </span>
                  <span
                    className={`inline-block w-4 ${
                      part.added
                        ? "text-green-500"
                        : part.removed
                          ? "text-red-500"
                          : "text-gray-600"
                    }`}
                  >
                    {prefix}
                  </span>
                  <span className={textClass}>{line || " "}</span>
                </div>
              );
            })}
          </div>
        );
      })}
    </div>
  );
}

function SplitDiffView({ diffResult, showOnlyChanges }: DiffViewProps) {
  // Build left and right sides
  const { leftLines, rightLines } = useMemo(() => {
    const left: Array<{ line: string; type: "normal" | "removed" | "empty" }> = [];
    const right: Array<{ line: string; type: "normal" | "added" | "empty" }> = [];

    diffResult.forEach((part) => {
      const lines = part.value.split("\n");
      if (lines[lines.length - 1] === "") {
        lines.pop();
      }

      if (part.added) {
        lines.forEach((line) => {
          left.push({ line: "", type: "empty" });
          right.push({ line, type: "added" });
        });
      } else if (part.removed) {
        lines.forEach((line) => {
          left.push({ line, type: "removed" });
          right.push({ line: "", type: "empty" });
        });
      } else {
        lines.forEach((line) => {
          left.push({ line, type: "normal" });
          right.push({ line, type: "normal" });
        });
      }
    });

    return { leftLines: left, rightLines: right };
  }, [diffResult]);

  // Filter if showing only changes
  const displayLines = useMemo(() => {
    if (!showOnlyChanges) {
      return leftLines.map((left, i) => ({ left, right: rightLines[i], index: i }));
    }

    // Keep context around changes
    const result: Array<{
      left: (typeof leftLines)[0];
      right: (typeof rightLines)[0];
      index: number;
    }> = [];
    const isChanged = (i: number) =>
      leftLines[i]?.type !== "normal" || rightLines[i]?.type !== "normal";

    let lastAddedIndex = -10;
    for (let i = 0; i < leftLines.length; i++) {
      if (isChanged(i)) {
        // Add context before (3 lines)
        for (let j = Math.max(lastAddedIndex + 1, i - 3); j < i; j++) {
          if (!result.find((r) => r.index === j)) {
            result.push({ left: leftLines[j], right: rightLines[j], index: j });
          }
        }
        result.push({ left: leftLines[i], right: rightLines[i], index: i });
        lastAddedIndex = i;
      } else if (i <= lastAddedIndex + 3) {
        // Add context after (3 lines)
        result.push({ left: leftLines[i], right: rightLines[i], index: i });
      }
    }

    return result;
  }, [leftLines, rightLines, showOnlyChanges]);

  return (
    <div className="flex font-mono text-sm">
      {/* Left side */}
      <div className="flex-1 border-r border-gray-700">
        {displayLines.map(({ left, index }) => {
          const bgClass =
            left.type === "removed"
              ? "bg-red-900/30"
              : left.type === "empty"
                ? "bg-gray-800/30"
                : "";

          const textClass = left.type === "removed" ? "text-red-300" : "text-gray-300";

          return (
            <div
              key={index}
              className={`px-4 py-0.5 hover:bg-gray-800/50 ${bgClass} min-h-[1.5rem]`}
            >
              <span className="inline-block w-8 text-gray-600 text-right mr-4 select-none">
                {left.type !== "empty" ? index + 1 : ""}
              </span>
              <span className={textClass}>{left.line || " "}</span>
            </div>
          );
        })}
      </div>

      {/* Right side */}
      <div className="flex-1">
        {displayLines.map(({ right, index }) => {
          const bgClass =
            right.type === "added"
              ? "bg-green-900/30"
              : right.type === "empty"
                ? "bg-gray-800/30"
                : "";

          const textClass = right.type === "added" ? "text-green-300" : "text-gray-300";

          return (
            <div
              key={index}
              className={`px-4 py-0.5 hover:bg-gray-800/50 ${bgClass} min-h-[1.5rem]`}
            >
              <span className="inline-block w-8 text-gray-600 text-right mr-4 select-none">
                {right.type !== "empty" ? index + 1 : ""}
              </span>
              <span className={textClass}>{right.line || " "}</span>
            </div>
          );
        })}
      </div>
    </div>
  );
}
