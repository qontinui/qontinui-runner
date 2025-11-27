/**
 * GeneralLogTab Component
 *
 * Renders the general log viewer with filtered logs.
 * Single responsibility: Display general logs.
 */

import { RefObject } from "react";
import type { LogEntry } from "../managers/LogManager";

export interface GeneralLogTabProps {
  logs: LogEntry[];
  containerRef: RefObject<HTMLDivElement>;
}

export function GeneralLogTab({ logs, containerRef }: GeneralLogTabProps) {
  return (
    <div
      ref={containerRef}
      className="log-container font-mono text-sm space-y-1"
      style={{ maxHeight: "400px", overflowY: "auto" }}
    >
      {logs.length === 0 ? (
        <div className="text-center text-muted-foreground py-8">
          No logs yet. Start a workflow to see execution logs.
        </div>
      ) : (
        logs.map((log) => (
          <div key={log.id} className="flex gap-2">
            <span className="text-muted-foreground flex-shrink-0">[{log.timestamp}]</span>
            <span
              className={`flex-shrink-0 ${
                log.level === "error"
                  ? "text-red-600"
                  : log.level === "warning"
                    ? "text-orange-600"
                    : log.level === "success"
                      ? "text-green-600"
                      : log.level === "debug"
                        ? "text-blue-600"
                        : "text-foreground"
              }`}
            >
              [{log.level.toUpperCase()}]
            </span>
            <span className="break-all">{log.message}</span>
          </div>
        ))
      )}
    </div>
  );
}
