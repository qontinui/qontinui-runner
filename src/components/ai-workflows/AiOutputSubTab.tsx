/**
 * AiOutputSubTab.tsx
 *
 * AI Output sub-tab - wraps AiOutputTab functionality
 * for displaying AI conversation output.
 */

import { AiOutputTab } from "../AiOutputTab";
import type { AiOutputLine } from "../AiOutputTab";

interface AiOutputSubTabProps {
  aiOutputLines: AiOutputLine[];
  onClearAiOutput: () => void;
}

export function AiOutputSubTab({ aiOutputLines, onClearAiOutput }: AiOutputSubTabProps) {
  return (
    <div className="h-full p-4">
      <div className="h-full card p-4">
        <AiOutputTab lines={aiOutputLines} onClear={onClearAiOutput} />
      </div>
    </div>
  );
}
