/**
 * RunImageRecognitionTab.tsx
 *
 * Run-specific tab showing image recognition/pattern matching logs.
 * Uses RunSelectionContext to filter data for the selected run.
 */

import { Image, Activity } from "lucide-react";
import { useRunSelectionOptional } from "../../contexts/RunSelectionContext";
import ImageLogTable from "../ImageLogTable";
import type { ImageRecognitionEntry } from "../../managers/LogManager";

interface RunImageRecognitionTabProps {
  /** Image recognition log entries */
  imageLogs: ImageRecognitionEntry[];
  /** Callback when an image log row is clicked */
  onImageRowClick: (entry: ImageRecognitionEntry) => void;
  /** Image log count for display */
  imageLogCount: number;
}

export function RunImageRecognitionTab({
  imageLogs,
  onImageRowClick,
  imageLogCount,
}: RunImageRecognitionTabProps) {
  const runSelection = useRunSelectionOptional();
  const selectedRun = runSelection?.selectedRun;

  // No run selected state
  if (!selectedRun && runSelection) {
    return (
      <div className="h-full flex flex-col items-center justify-center text-muted-foreground p-8">
        <Activity className="w-12 h-12 mb-4 opacity-50" />
        <p className="text-lg font-medium">No Run Selected</p>
        <p className="text-sm mt-2 text-center max-w-md">
          Select a run from the Run Dashboard to view image recognition logs.
        </p>
      </div>
    );
  }

  return (
    <div className="h-full flex flex-col overflow-hidden">
      {/* Header */}
      <div className="flex-shrink-0 bg-background flex items-center justify-between border-b border-border p-3">
        <div className="flex items-center gap-2">
          <Image className="w-4 h-4 text-muted-foreground" />
          <span className="text-sm font-medium">Image Recognition</span>
          {imageLogCount > 0 && (
            <span className="px-1.5 py-0.5 text-xs rounded-full bg-muted text-muted-foreground">
              {imageLogCount}
            </span>
          )}
        </div>

        {/* Run info */}
        {selectedRun && (
          <div className="text-xs text-muted-foreground">
            Run: {selectedRun.workflow_name || "Unknown"}
          </div>
        )}
      </div>

      {/* Content */}
      <div className="flex-1 min-h-0 overflow-auto p-4">
        {imageLogs.length > 0 ? (
          <ImageLogTable imageLogs={imageLogs} onRowClick={onImageRowClick} />
        ) : (
          <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
            <Image className="w-8 h-8 mb-3 opacity-50" />
            <p className="text-sm">No image recognition data for this run</p>
          </div>
        )}
      </div>
    </div>
  );
}

export default RunImageRecognitionTab;
