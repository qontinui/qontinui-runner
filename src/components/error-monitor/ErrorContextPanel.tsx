/**
 * ErrorContextPanel.tsx
 *
 * Modal panel that fetches and displays log lines surrounding an error,
 * using the existing `get_process_log_context` Tauri command.
 */

import { useEffect, useState, useMemo } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Loader2, X } from "lucide-react";
import { cn } from "../../lib/utils";

interface ProcessSessionOutputLine {
  id: number;
  session_id: string;
  timestamp: string;
  stream: string;
  line: string;
}

interface ErrorContextPanelProps {
  processName: string;
  timestamp: string;
  onClose: () => void;
  before?: number;
  after?: number;
}

export function ErrorContextPanel({
  processName,
  timestamp,
  onClose,
  before = 20,
  after = 20,
}: ErrorContextPanelProps) {
  // Track which params the current result belongs to
  const [resultKey, setResultKey] = useState<string | null>(null);
  const [lines, setLines] = useState<ProcessSessionOutputLine[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  const fetchKey = `${processName}:${timestamp}:${before}:${after}`;
  const loading = resultKey !== fetchKey;

  useEffect(() => {
    let cancelled = false;
    invoke<ProcessSessionOutputLine[]>("get_process_log_context", {
      processName,
      aroundTimestamp: timestamp,
      before,
      after,
    })
      .then((result) => {
        if (cancelled) return;
        setLines(result ?? []);
        setError(null);
        setResultKey(`${processName}:${timestamp}:${before}:${after}`);
      })
      .catch((e) => {
        if (cancelled) return;
        setError(typeof e === "string" ? e : JSON.stringify(e));
        setLines(null);
        setResultKey(`${processName}:${timestamp}:${before}:${after}`);
      });
    return () => {
      cancelled = true;
    };
  }, [processName, timestamp, before, after]);

  // Determine index of the line at or closest (just before/equal) to the
  // target timestamp, so we can highlight "where" the error is.
  const highlightIndex = useMemo(() => {
    if (!lines || lines.length === 0) return -1;
    const target = Date.parse(timestamp);
    if (Number.isNaN(target)) return -1;
    let bestIdx = -1;
    let bestDelta = Number.POSITIVE_INFINITY;
    lines.forEach((l, i) => {
      const t = Date.parse(l.timestamp);
      if (Number.isNaN(t)) return;
      const delta = Math.abs(t - target);
      if (delta < bestDelta) {
        bestDelta = delta;
        bestIdx = i;
      }
    });
    return bestIdx;
  }, [lines, timestamp]);

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4"
      role="dialog"
      aria-modal="true"
      onClick={onClose}
    >
      <div
        className="bg-background border border-border rounded-lg shadow-xl w-full max-w-4xl max-h-[85vh] flex flex-col"
        onClick={(e) => e.stopPropagation()}
      >
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-3 border-b border-border">
          <div className="min-w-0">
            <h2 className="text-sm font-semibold">Log context</h2>
            <p className="text-xs text-muted-foreground truncate">
              <span className="font-medium">{processName}</span>
              {" · "}
              around {timestamp}
            </p>
          </div>
          <button
            onClick={onClose}
            className="p-1 rounded hover:bg-muted transition-colors"
            aria-label="Close"
          >
            <X className="w-4 h-4" />
          </button>
        </div>

        {/* Body */}
        <div className="flex-1 overflow-auto">
          {loading && (
            <div className="flex items-center justify-center gap-2 py-12 text-sm text-muted-foreground">
              <Loader2 className="w-4 h-4 animate-spin" />
              Loading context...
            </div>
          )}

          {!loading && error && (
            <div className="p-4 text-sm text-red-500">Failed to load context: {error}</div>
          )}

          {!loading && !error && lines && lines.length === 0 && (
            <div className="p-6 text-sm text-muted-foreground text-center">
              No log context available — the error may be older than the log retention window.
            </div>
          )}

          {!loading && !error && lines && lines.length > 0 && (
            <div className="font-mono text-xs">
              {lines.map((l, i) => {
                const isHighlight = i === highlightIndex;
                const isStderr = l.stream?.toLowerCase() === "stderr";
                return (
                  <div
                    key={l.id}
                    className={cn(
                      "flex gap-3 px-4 py-0.5 whitespace-pre-wrap break-all border-l-2",
                      isHighlight
                        ? "bg-yellow-500/15 border-yellow-500"
                        : "border-transparent hover:bg-muted/30",
                    )}
                  >
                    <span className="shrink-0 text-muted-foreground/60 select-none">
                      {l.timestamp}
                    </span>
                    <span
                      className={cn(
                        "shrink-0 w-12 select-none",
                        isStderr ? "text-red-500" : "text-muted-foreground/60",
                      )}
                    >
                      {l.stream}
                    </span>
                    <span className="flex-1">{l.line}</span>
                  </div>
                );
              })}
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="px-4 py-2 border-t border-border text-xs text-muted-foreground flex items-center justify-between">
          <span>
            {lines ? `${lines.length} line${lines.length === 1 ? "" : "s"}` : ""}
            {highlightIndex >= 0 && lines && <> · highlighted line is closest to error timestamp</>}
          </span>
          <button
            onClick={onClose}
            className="px-3 py-1 text-xs bg-muted rounded hover:bg-muted/70 transition-colors"
          >
            Close
          </button>
        </div>
      </div>
    </div>
  );
}
