/**
 * HistoryPanel.tsx
 *
 * Collapsible panel showing recent automation runs.
 */

import { CheckCircle, History, RefreshCw, XCircle } from "lucide-react";
import { useAiBuilder } from "./AiBuilderContext";
import CollapsiblePanel from "../CollapsiblePanel";
import { getStatusColors } from "@/design-system";

export function HistoryPanel() {
  const { history, loadFromHistory, getHistorySummary } = useAiBuilder();

  return (
    <CollapsiblePanel
      title="Recent Runs"
      icon={<History className="w-4 h-4" />}
      defaultCollapsed={true}
      storageKey="ai-builder-history"
    >
      {history.length === 0 ? (
        <p className="text-sm text-muted-foreground p-3">No previous runs</p>
      ) : (
        <div className="space-y-2 max-h-64 overflow-y-auto">
          {history.slice(0, 10).map((entry) => (
            <button
              key={entry.id}
              onClick={() => loadFromHistory(entry)}
              className="w-full text-left p-3 bg-background rounded-md hover:bg-muted/30 transition-colors"
            >
              <div className="flex items-center gap-2">
                {entry.success === true && (
                  <CheckCircle className={`w-4 h-4 ${getStatusColors("success").text}`} />
                )}
                {entry.success === false && (
                  <XCircle className={`w-4 h-4 ${getStatusColors("error").text}`} />
                )}
                {entry.success === undefined && (
                  <RefreshCw className="w-4 h-4 text-muted-foreground" />
                )}
                <span className="text-sm font-medium truncate">{entry.goal}</span>
              </div>
              <div className="text-xs text-muted-foreground mt-1">
                {getHistorySummary(entry)}
                {" - "}
                {new Date(entry.timestamp).toLocaleString()}
              </div>
            </button>
          ))}
        </div>
      )}
    </CollapsiblePanel>
  );
}
