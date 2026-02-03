/**
 * ImageLogTable Component
 *
 * Displays image recognition logs in a compact table format.
 * When a row is clicked, detailed information is shown in a modal.
 * Similar to ActionLogTable but for image recognition events.
 * Uses virtual scrolling for large log lists (100+ items) to maintain performance.
 */

import { useCallback, type CSSProperties } from "react";
import { ImageRecognitionEntry } from "../managers/LogManager";
import { getStatusColors } from "@/design-system";
import { FixedVirtualList } from "./ui";

/** Threshold for switching to virtual scrolling */
const VIRTUAL_SCROLL_THRESHOLD = 100;
/** Height of each log row in pixels */
const LOG_ROW_HEIGHT = 52;

interface ImageLogTableProps {
  imageLogs: ImageRecognitionEntry[];
  onRowClick: (entry: ImageRecognitionEntry) => void;
}

/**
 * Single row component for image log entry.
 */
function ImageLogRow({
  entry,
  onRowClick,
}: {
  entry: ImageRecognitionEntry;
  onRowClick: (entry: ImageRecognitionEntry) => void;
}) {
  return (
    <tr
      onClick={() => onRowClick(entry)}
      className="border-b border-border hover:bg-accent/50 cursor-pointer transition-colors"
    >
      <td className="py-2 px-3 text-muted-foreground text-xs font-mono">{entry.timestamp}</td>
      <td className="py-2 px-3">
        {(entry.imageData || entry.templatePath) && (
          <img
            src={
              entry.imageData
                ? `data:image/png;base64,${entry.imageData}`
                : `file://${entry.templatePath}`
            }
            alt={entry.template}
            className="w-12 h-8 object-contain border border-border rounded"
          />
        )}
      </td>
      <td className="py-2 px-3 font-medium truncate max-w-[200px]" title={entry.template}>
        {entry.template}
      </td>
      <td className="py-2 px-3">
        <span
          className={`inline-flex items-center px-2 py-0.5 rounded-full text-xs font-medium ${
            entry.found
              ? `${getStatusColors("success").bg} ${getStatusColors("success").text}`
              : `${getStatusColors("error").bg} ${getStatusColors("error").text}`
          }`}
        >
          {entry.found ? "FOUND" : "NOT FOUND"}
        </span>
      </td>
      <td className="py-2 px-3">
        <span
          className={`font-mono ${
            entry.confidence >= entry.threshold
              ? getStatusColors("success").text
              : getStatusColors("error").text
          }`}
        >
          {(entry.confidence * 100).toFixed(1)}%
        </span>
        {!entry.found && entry.percentOff !== undefined && (
          <span className="text-xs text-muted-foreground ml-2">
            ({entry.percentOff.toFixed(1)}% off)
          </span>
        )}
      </td>
      <td className="py-2 px-3 font-mono text-xs text-muted-foreground">
        {entry.location
          ? `(${entry.location.x}, ${entry.location.y})`
          : entry.bestMatchLocation || "-"}
      </td>
    </tr>
  );
}

export default function ImageLogTable({ imageLogs, onRowClick }: ImageLogTableProps) {
  // Use virtual scrolling for large lists
  const useVirtualScroll = imageLogs.length > VIRTUAL_SCROLL_THRESHOLD;

  // Render function for virtualized items
  const renderVirtualItem = useCallback(
    (entry: ImageRecognitionEntry, _index: number, style: CSSProperties) => (
      <div style={style}>
        <table className="w-full text-sm">
          <tbody>
            <ImageLogRow entry={entry} onRowClick={onRowClick} />
          </tbody>
        </table>
      </div>
    ),
    [onRowClick],
  );

  // Key extractor for virtual list
  const getItemKey = useCallback((entry: ImageRecognitionEntry) => entry.id, []);

  if (imageLogs.length === 0) {
    return (
      <div className="text-center text-muted-foreground py-8">No image recognition logs yet.</div>
    );
  }

  return (
    <div className="overflow-x-auto">
      {/* Header - always visible */}
      <table className="w-full text-sm">
        <thead>
          <tr className="border-b border-border">
            <th className="text-left py-2 px-3 font-medium text-muted-foreground">Time</th>
            <th className="text-left py-2 px-3 font-medium text-muted-foreground w-[60px]">
              Preview
            </th>
            <th className="text-left py-2 px-3 font-medium text-muted-foreground">Template</th>
            <th className="text-left py-2 px-3 font-medium text-muted-foreground">Result</th>
            <th className="text-left py-2 px-3 font-medium text-muted-foreground">Confidence</th>
            <th className="text-left py-2 px-3 font-medium text-muted-foreground">Location</th>
          </tr>
        </thead>
      </table>

      {/* Body - virtualized or regular */}
      {useVirtualScroll ? (
        /* Virtual scrolling for large lists */
        <div className="h-[400px]">
          <FixedVirtualList
            items={imageLogs}
            itemHeight={LOG_ROW_HEIGHT}
            renderItem={renderVirtualItem}
            getItemKey={getItemKey}
            overscanCount={10}
          />
        </div>
      ) : (
        /* Regular scrolling for small lists */
        <table className="w-full text-sm">
          <tbody>
            {imageLogs.map((entry) => (
              <ImageLogRow key={entry.id} entry={entry} onRowClick={onRowClick} />
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}
