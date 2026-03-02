/**
 * RunPageLayout.tsx
 *
 * Shared layout wrapper for all Run Dashboard subpages.
 * Provides a consistent header with RunSelector dropdown and page title.
 */

import { ReactNode, useState, useCallback } from "react";
import { LucideIcon, Activity, ExternalLink, RotateCw } from "lucide-react";
import { RunSelector } from "../run-selection/RunSelector";
import { useRunSelectionOptional } from "../../contexts/RunSelectionContext";
import { getStatusColors } from "@/design-system";
import { getApiBase } from "@/lib/runner-api";

/** Maps runner page titles to web run-detail tab names */
const TAB_MAPPING: Record<string, string> = {
  Recap: "summary",
  Actions: "summary",
  "Image Recognition": "summary",
  Findings: "findings",
  "State Explorer": "summary",
  "Test Results": "tests",
  "AI Output": "ai-conversation",
  "AI Data Viewer": "data-logs",
  Statistics: "summary",
};

interface RunPageLayoutProps {
  /** Child content to render */
  children: ReactNode;
  /** Page title */
  title: string;
  /** Icon component to display */
  icon: LucideIcon;
  /** Optional badge count */
  badgeCount?: number;
  /** Navigate to the Active dashboard page */
  onNavigateToActive?: () => void;
}

/**
 * RunPageLayout - Wrapper for run subpages with shared RunSelector header
 */
export function RunPageLayout({
  children,
  title,
  icon: Icon,
  badgeCount,
  onNavigateToActive,
}: RunPageLayoutProps) {
  const runSelection = useRunSelectionOptional();
  const selectedRun = runSelection?.selectedRun;
  const [isRerunning, setIsRerunning] = useState(false);

  const handleRerun = useCallback(async () => {
    if (!selectedRun?.workflow_name || isRerunning) return;
    setIsRerunning(true);
    try {
      // Look up the workflow by name
      const searchRes = await fetch(
        `${getApiBase()}/unified-workflows/search?q=${encodeURIComponent(selectedRun.workflow_name)}`,
      );
      const searchData = await searchRes.json();
      const workflows = searchData?.data;
      if (!workflows?.length) {
        console.error("[Rerun] Workflow not found:", selectedRun.workflow_name);
        return;
      }
      // Find exact name match
      const workflow =
        workflows.find(
          (w: { name: string }) =>
            w.name.toLowerCase() === selectedRun.workflow_name!.toLowerCase(),
        ) || workflows[0];
      // Run the workflow
      await fetch(`${getApiBase()}/unified-workflows/${workflow.id}/run`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ monitor_index: 0 }),
      });
      // Navigate to Active page after starting
      setTimeout(() => onNavigateToActive?.(), 100);
    } catch (error) {
      console.error("[Rerun] Failed to rerun workflow:", error);
    } finally {
      setIsRerunning(false);
    }
  }, [selectedRun, isRerunning, onNavigateToActive]);

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

  const webTab = TAB_MAPPING[title] || "summary";

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
            {selectedRun && (
              <a
                href={`http://localhost:3001/runs/${selectedRun.id}?tab=${webTab}`}
                target="_blank"
                rel="noopener noreferrer"
                className="flex items-center gap-1 px-2 py-0.5 text-xs text-muted-foreground hover:text-foreground rounded hover:bg-muted transition-colors no-underline"
                title="View in Web (opens browser)"
              >
                <ExternalLink className="w-3 h-3" />
                <span>Open in Web</span>
              </a>
            )}
          </div>

          {/* RunSelector dropdown + rerun button */}
          <div className="flex items-center gap-2">
            <div className="w-96">
              <RunSelector />
            </div>
            {selectedRun && selectedRun.workflow_name && selectedRun.status !== "running" && (
              <button
                onClick={handleRerun}
                disabled={isRerunning}
                title="Rerun this workflow"
                className="flex items-center gap-1.5 px-3 py-2 text-sm font-medium bg-primary text-primary-foreground rounded-lg hover:bg-primary/90 disabled:opacity-50 disabled:cursor-not-allowed transition-colors whitespace-nowrap"
              >
                <RotateCw className={`w-4 h-4 ${isRerunning ? "animate-spin" : ""}`} />
                {isRerunning ? "Starting..." : "Rerun"}
              </button>
            )}
          </div>
        </div>

        {/* Selected run info */}
        {selectedRun && (
          <div className="mt-2 text-xs text-muted-foreground">
            Viewing: <span className="font-medium">{selectedRun.task_name || "Unknown Task"}</span>
            {selectedRun.status === "running" && (
              <span
                className={`ml-2 px-1.5 py-0.5 text-xs ${getStatusColors("running").bg} ${getStatusColors("running").text} rounded`}
              >
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
