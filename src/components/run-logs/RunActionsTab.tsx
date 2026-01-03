/**
 * RunActionsTab.tsx
 *
 * Run-specific tab showing action execution logs.
 * Uses RunSelectionContext to filter data for the selected run.
 */

import { Zap, Loader2, AlertCircle, Activity } from "lucide-react";
import { useRunSelectionOptional } from "../../contexts/RunSelectionContext";
import ActionLogTable from "../ActionLogTable";
import type { ActionLogEntry } from "../../types/displayProfile";

interface RunActionsTabProps {
  /** Action log data from useActionLogView */
  actionLogData: {
    actions: ActionLogEntry[];
    visible_count: number;
  } | null;
  /** Whether action log is loading */
  actionLogLoading: boolean;
  /** Error message if loading failed */
  actionLogError: string | null;
  /** Callback when an action row is clicked */
  onActionRowClick: (action: ActionLogEntry) => void;
  /** Action count for display */
  actionCount: number;
}

export function RunActionsTab({
  actionLogData,
  actionLogLoading,
  actionLogError,
  onActionRowClick,
  actionCount,
}: RunActionsTabProps) {
  const runSelection = useRunSelectionOptional();
  const selectedRun = runSelection?.selectedRun;

  // No run selected state
  if (!selectedRun && runSelection) {
    return (
      <div className="h-full flex flex-col items-center justify-center text-muted-foreground p-8">
        <Activity className="w-12 h-12 mb-4 opacity-50" />
        <p className="text-lg font-medium">No Run Selected</p>
        <p className="text-sm mt-2 text-center max-w-md">
          Select a run from the Run Dashboard to view action logs.
        </p>
      </div>
    );
  }

  return (
    <div className="h-full flex flex-col overflow-hidden">
      {/* Header */}
      <div className="flex-shrink-0 bg-background flex items-center justify-between border-b border-border p-3">
        <div className="flex items-center gap-2">
          <Zap className="w-4 h-4 text-muted-foreground" />
          <span className="text-sm font-medium">Actions</span>
          {actionCount > 0 && (
            <span className="px-1.5 py-0.5 text-xs rounded-full bg-muted text-muted-foreground">
              {actionCount}
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
        {actionLogLoading && (
          <div className="flex items-center justify-center py-8 text-muted-foreground">
            <Loader2 className="w-5 h-5 animate-spin mr-2" />
            <span>Loading action log...</span>
          </div>
        )}

        {actionLogError && (
          <div className="flex items-center justify-center py-8 text-red-400">
            <AlertCircle className="w-5 h-5 mr-2" />
            <span>Error: {actionLogError}</span>
          </div>
        )}

        {!actionLogLoading && !actionLogError && actionLogData && (
          <ActionLogTable actions={actionLogData.actions} onRowClick={onActionRowClick} />
        )}

        {!actionLogLoading &&
          !actionLogError &&
          (!actionLogData || actionLogData.actions.length === 0) && (
            <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
              <Zap className="w-8 h-8 mb-3 opacity-50" />
              <p className="text-sm">No actions recorded for this run</p>
            </div>
          )}
      </div>
    </div>
  );
}

export default RunActionsTab;
