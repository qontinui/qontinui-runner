/**
 * GeneralLogTab Component
 *
 * Renders the general log viewer with filtered logs.
 * Single responsibility: Display general logs.
 */

import { RefObject } from "react";
import type { LogEntry } from "../managers/LogManager";
import { getAccentColors, getStatusColors } from "@/design-system";

export interface GeneralLogTabProps {
  logs: LogEntry[];
  containerRef: RefObject<HTMLDivElement>;
}

export function GeneralLogTab({ logs, containerRef }: GeneralLogTabProps) {
  return (
    <div
      ref={containerRef}
      className="h-full font-mono text-xs space-y-0.5 overflow-y-auto scrollbar-dark rounded-md bg-muted/30 p-2"
    >
      {logs.length === 0 ? (
        <div className="empty-state py-8">
          <span className="text-sm">No logs yet</span>
          <span className="text-xs mt-1">Start a workflow to see execution logs</span>
        </div>
      ) : (
        logs.map((log) => (
          <div key={log.id} className="flex gap-2 py-0.5 hover:bg-muted/30 px-1 rounded">
            <span className="text-muted-foreground/70 flex-shrink-0">[{log.timestamp}]</span>
            <span
              className={`flex-shrink-0 font-medium ${
                log.level === "error"
                  ? getStatusColors("error").text
                  : log.level === "warning"
                    ? getStatusColors("warning").text
                    : log.level === "success"
                      ? getStatusColors("success").text
                      : log.level === "debug"
                        ? getAccentColors("blue").text
                        : "text-muted-foreground"
              }`}
            >
              [{log.level.toUpperCase()}]
            </span>
            <span className="break-all text-foreground/90">{log.message}</span>
          </div>
        ))
      )}
    </div>
  );
}
