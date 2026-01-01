/**
 * RunningIndicator.tsx
 *
 * Displayed when analysis is in progress.
 */

import { Loader2 } from "lucide-react";
import { useAiBuilder } from "./AiBuilderContext";

export function RunningIndicator() {
  const { isRunning } = useAiBuilder();

  if (!isRunning) {
    return null;
  }

  return (
    <div className="card p-4 space-y-3 border-primary/50">
      <div className="flex items-center gap-2">
        <Loader2 className="w-4 h-4 text-primary animate-spin" />
        <span className="font-medium">Analysis in Progress</span>
      </div>
      <p className="text-sm text-muted-foreground">
        View live output in the <strong>AI Output</strong> sub-tab.
      </p>
      <p className="text-xs text-muted-foreground">
        The AI is executing your automation steps and analyzing results.
      </p>
    </div>
  );
}
