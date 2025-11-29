/**
 * ImageLogTable Component
 *
 * Displays image recognition logs in a compact table format.
 * When a row is clicked, detailed information is shown in a modal.
 * Similar to ActionLogTable but for image recognition events.
 */

import { ImageRecognitionEntry } from "../managers/LogManager";

interface ImageLogTableProps {
  imageLogs: ImageRecognitionEntry[];
  onRowClick: (entry: ImageRecognitionEntry) => void;
}

export default function ImageLogTable({ imageLogs, onRowClick }: ImageLogTableProps) {
  if (imageLogs.length === 0) {
    return (
      <div className="text-center text-muted-foreground py-8">No image recognition logs yet.</div>
    );
  }

  return (
    <div className="overflow-x-auto">
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
        <tbody>
          {imageLogs.map((entry) => (
            <tr
              key={entry.id}
              onClick={() => onRowClick(entry)}
              className="border-b border-border hover:bg-accent/50 cursor-pointer transition-colors"
            >
              <td className="py-2 px-3 text-muted-foreground text-xs font-mono">
                {entry.timestamp}
              </td>
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
                      ? "bg-green-100 text-green-800 dark:bg-green-900/30 dark:text-green-400"
                      : "bg-red-100 text-red-800 dark:bg-red-900/30 dark:text-red-400"
                  }`}
                >
                  {entry.found ? "FOUND" : "NOT FOUND"}
                </span>
              </td>
              <td className="py-2 px-3">
                <span
                  className={`font-mono ${
                    entry.confidence >= entry.threshold
                      ? "text-green-600 dark:text-green-400"
                      : "text-red-600 dark:text-red-400"
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
          ))}
        </tbody>
      </table>
    </div>
  );
}
