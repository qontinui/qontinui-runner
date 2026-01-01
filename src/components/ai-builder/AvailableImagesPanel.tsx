/**
 * AvailableImagesPanel.tsx
 *
 * Collapsible panel showing available images from the configuration.
 */

import { Image as ImageIcon } from "lucide-react";
import { useAiBuilder } from "./AiBuilderContext";
import CollapsiblePanel from "../CollapsiblePanel";

export function AvailableImagesPanel() {
  const { images } = useAiBuilder();

  if (images.length === 0) {
    return null;
  }

  return (
    <CollapsiblePanel
      title={`Available Images (${images.length})`}
      icon={<ImageIcon className="w-4 h-4" />}
      defaultCollapsed={true}
      storageKey="ai-builder-images"
    >
      <div className="space-y-1 max-h-48 overflow-y-auto">
        {images.map((img) => (
          <div
            key={`${img.stateName}-${img.name}`}
            className="flex items-center justify-between text-sm p-2 bg-background rounded"
          >
            <span>{img.name}</span>
            <span className="text-xs text-muted-foreground">{img.stateName}</span>
          </div>
        ))}
      </div>
    </CollapsiblePanel>
  );
}
