/**
 * RunActionsTab.tsx
 *
 * Run-specific tab showing action execution logs.
 * Header and run selection are provided by RunPageLayout wrapper.
 */

import { Zap, Loader2, AlertCircle } from "lucide-react";
import ActionLogTable from "../ActionLogTable";
import { getStatusColors } from "@/design-system";
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
}: RunActionsTabProps) {
  return (
    <div className="h-full flex flex-col overflow-hidden">
      {/* Content */}
      <div className="flex-1 min-h-0 overflow-auto">
        {actionLogLoading && (
          <div className="flex items-center justify-center py-8 text-muted-foreground">
            <Loader2 className="w-5 h-5 animate-spin mr-2" />
            <span>Loading action log...</span>
          </div>
        )}

        {actionLogError && (
          <div className={`flex items-center justify-center py-8 ${getStatusColors("error").text}`}>
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
