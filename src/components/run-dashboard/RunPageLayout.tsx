/**
 * RunPageLayout.tsx
 *
 * Shared layout wrapper for all Run Dashboard subpages.
 * Provides a consistent header with RunSelector dropdown and page title.
 */

import { ReactNode } from "react";
import { LucideIcon, Activity } from "lucide-react";
import { RunSelector } from "../run-selection/RunSelector";
import { useRunSelectionOptional } from "../../contexts/RunSelectionContext";

interface RunPageLayoutProps {
  /** Child content to render */
  children: ReactNode;
  /** Page title */
  title: string;
  /** Icon component to display */
  icon: LucideIcon;
  /** Optional badge count */
  badgeCount?: number;
}

/**
 * RunPageLayout - Wrapper for run subpages with shared RunSelector header
 */
export function RunPageLayout({ children, title, icon: Icon, badgeCount }: RunPageLayoutProps) {
  const runSelection = useRunSelectionOptional();
  const selectedRun = runSelection?.selectedRun;

  // No run selection context available
  if (!runSelection) {
    return (
      <div className="h-full flex flex-col overflow-hidden">
        <div className="flex-1 min-h-0 overflow-hidden">{children}</div>
      </div>
    );
  }

  // Show message when no run is selected and we have no recent runs to show
  const recentRuns = runSelection.recentRuns;
  const isLoadingRuns = runSelection.isLoadingRuns;

  // If still loading runs, show loading state
  if (isLoadingRuns && recentRuns.length === 0) {
    return (
      <div className="h-full flex flex-col items-center justify-center text-muted-foreground p-8">
        <Activity className="w-12 h-12 mb-4 opacity-50 animate-pulse" />
        <p className="text-lg font-medium">Loading runs...</p>
      </div>
    );
  }

  // If no runs exist yet
  if (recentRuns.length === 0) {
    return (
      <div className="h-full flex flex-col items-center justify-center text-muted-foreground p-8">
        <Activity className="w-12 h-12 mb-4 opacity-50" />
        <p className="text-lg font-medium">No Task Runs Yet</p>
        <p className="text-sm mt-2 text-center max-w-md">Start a task to see run data here.</p>
      </div>
    );
  }

  return (
    <div className="h-full flex flex-col overflow-hidden">
      {/* Header with RunSelector */}
      <div className="flex-shrink-0 bg-background border-b border-border px-4 py-3">
        <div className="flex items-center justify-between gap-4">
          {/* Page title */}
          <div className="flex items-center gap-2">
            <Icon className="w-4 h-4 text-muted-foreground" />
            <span className="text-sm font-medium">{title}</span>
            {badgeCount !== undefined && badgeCount > 0 && (
              <span className="px-1.5 py-0.5 text-xs rounded-full bg-muted text-muted-foreground">
                {badgeCount}
              </span>
            )}
          </div>

          {/* RunSelector dropdown */}
          <div className="w-72">
            <RunSelector />
          </div>
        </div>

        {/* Selected run info */}
        {selectedRun && (
          <div className="mt-2 text-xs text-muted-foreground">
            Viewing: <span className="font-medium">{selectedRun.task_name || "Unknown Task"}</span>
            {selectedRun.status === "running" && (
              <span className="ml-2 px-1.5 py-0.5 text-xs bg-blue-500/20 text-blue-400 rounded">
                In Progress
              </span>
            )}
          </div>
        )}
      </div>

      {/* Content area */}
      <div className="flex-1 min-h-0 overflow-hidden">{children}</div>
    </div>
  );
}

export default RunPageLayout;
