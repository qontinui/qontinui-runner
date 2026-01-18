/**
 * ErrorPatternsPanel.tsx
 *
 * Shows common error patterns with frequency.
 */

import { Bug } from "lucide-react";
import { getAccentColors } from "@/design-system";
import type { ErrorPattern } from "../../types/statistics";

interface ErrorPatternsPanelProps {
  patterns: Record<string, ErrorPattern>;
}

export function ErrorPatternsPanel({ patterns }: ErrorPatternsPanelProps) {
  const patternList = Object.entries(patterns)
    .map(([type, pattern]) => ({ type, ...pattern }))
    .sort((a, b) => b.count - a.count);

  if (patternList.length === 0) {
    return (
      <div className="bg-card rounded-lg border border-border p-4">
        <h3 className="font-semibold mb-4 flex items-center gap-2">
          <Bug className="w-4 h-4" />
          Error Patterns
        </h3>
        <div className="text-center text-muted-foreground py-4">
          <p>No error patterns detected</p>
        </div>
      </div>
    );
  }

  const formatDate = (iso: string | undefined) => {
    if (!iso) return "Unknown";
    return new Date(iso).toLocaleDateString();
  };

  return (
    <div className="bg-card rounded-lg border border-border p-4">
      <h3 className="font-semibold mb-4 flex items-center gap-2">
        <Bug className={`w-4 h-4 ${getAccentColors("red").text}`} />
        Error Patterns
        <span
          className={`text-xs ${getAccentColors("red").bg} ${getAccentColors("red").text} px-2 py-0.5 rounded-full`}
        >
          {patternList.length}
        </span>
      </h3>

      <div className="space-y-2 max-h-64 overflow-auto">
        {patternList.map((pattern) => (
          <div key={pattern.type} className="p-3 rounded-md bg-muted/30">
            <div className="flex items-start justify-between gap-2">
              <div className="min-w-0">
                <p className="text-sm font-medium">{pattern.type}</p>
                <p className="text-xs text-muted-foreground">
                  Last seen: {formatDate(pattern.last_seen)}
                </p>
              </div>
              <div className="text-right flex-shrink-0">
                <div className={`text-lg font-bold ${getAccentColors("red").text}`}>
                  {pattern.count}
                </div>
                <span className="text-xs text-muted-foreground">occurrences</span>
              </div>
            </div>
            {pattern.contexts.length > 0 && (
              <div className="mt-2 text-xs text-muted-foreground">
                <details>
                  <summary className="cursor-pointer hover:text-foreground">
                    Recent contexts ({pattern.contexts.length})
                  </summary>
                  <ul className="mt-1 space-y-1 pl-4">
                    {pattern.contexts.slice(0, 3).map((ctx, idx) => (
                      <li key={idx} className="truncate">
                        {ctx}
                      </li>
                    ))}
                  </ul>
                </details>
              </div>
            )}
          </div>
        ))}
      </div>
    </div>
  );
}
