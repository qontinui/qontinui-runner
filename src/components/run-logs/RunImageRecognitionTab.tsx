/**
 * RunImageRecognitionTab.tsx
 *
 * Run-specific tab showing image recognition/pattern matching logs.
 * Header and run selection are provided by RunPageLayout wrapper.
 */

import { Image } from "lucide-react";
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
}: RunImageRecognitionTabProps) {
  return (
    <div data-ui-id="logs-image-recognition-tab" className="h-full flex flex-col overflow-hidden">
      {/* Content */}
      <div data-ui-id="logs-image-recognition-list" className="flex-1 min-h-0 overflow-auto">
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
