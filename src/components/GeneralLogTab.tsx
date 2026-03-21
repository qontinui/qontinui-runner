/**
 * GeneralLogTab Component
 *
 * Renders the general log viewer with filtered logs.
 * Single responsibility: Display general logs.
 * Uses virtual scrolling for large log lists (200+ items) to maintain performance.
 */

import { RefObject, useCallback, type CSSProperties } from "react";
import type { LogEntry } from "../managers/LogManager";
import { getAccentColors, getStatusColors } from "@/design-system";
import { FixedVirtualList } from "./ui";

/** Threshold for switching to virtual scrolling */
const VIRTUAL_SCROLL_THRESHOLD = 200;
/** Height of each log row in pixels */
const LOG_ROW_HEIGHT = 24;

export interface GeneralLogTabProps {
  logs: LogEntry[];
  containerRef: RefObject<HTMLDivElement | null>;
}

/**
 * Single log row component.
 */
function LogRow({ log }: { log: LogEntry }) {
  return (
    <div className="flex gap-2 py-0.5 hover:bg-muted/30 px-1 rounded">
      <span className="text-muted-foreground/70 shrink-0">[{log.timestamp}]</span>
      <span
        className={`shrink-0 font-medium ${
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
  );
}

export function GeneralLogTab({ logs, containerRef }: GeneralLogTabProps) {
  // Use virtual scrolling for large lists
  const useVirtualScroll = logs.length > VIRTUAL_SCROLL_THRESHOLD;

  // Render function for virtualized items
  const renderVirtualItem = useCallback(
    (log: LogEntry, _index: number, style: CSSProperties) => (
      <div style={style}>
        <LogRow log={log} />
      </div>
    ),
    [],
  );

  // Key extractor for virtual list
  const getItemKey = useCallback((log: LogEntry) => log.id, []);

  return (
    <div
      ref={containerRef}
      className="h-full font-mono text-xs scrollbar-dark rounded-md bg-muted/30 p-2"
    >
      {logs.length === 0 ? (
        <div className="empty-state py-8">
          <span className="text-sm">No logs yet</span>
          <span className="text-xs mt-1">Start a workflow to see execution logs</span>
        </div>
      ) : useVirtualScroll ? (
        /* Virtual scrolling for large lists */
        <FixedVirtualList
          items={logs}
          itemHeight={LOG_ROW_HEIGHT}
          renderItem={renderVirtualItem}
          getItemKey={getItemKey}
          overscanCount={20}
        />
      ) : (
        /* Regular scrolling for small lists */
        <div className="space-y-0.5 overflow-y-auto h-full">
          {logs.map((log) => (
            <LogRow key={log.id} log={log} />
          ))}
        </div>
      )}
    </div>
  );
}
