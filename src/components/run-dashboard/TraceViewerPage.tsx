import React, { useMemo } from "react";
import { TraceViewerWidget } from "../widgets/trace-viewer";
import { useRunSelection } from "../../contexts/RunSelectionContext";

export const TraceViewerPage: React.FC = () => {
  const { selectedRunId, selectedRun, recentRuns } = useRunSelection();

  // Find baseline: the run immediately before the selected one with the same workflow
  const baselineExecutionId = useMemo(() => {
    if (!selectedRunId || !selectedRun) return null;

    const workflowName = selectedRun.workflow_name;
    const selectedIdx = recentRuns.findIndex((r) => r.id === selectedRunId);
    if (selectedIdx < 0) return null;

    // Look for the next older run with the same workflow
    for (let i = selectedIdx + 1; i < recentRuns.length; i++) {
      const run = recentRuns[i];
      if (run.id !== selectedRunId && (!workflowName || run.workflow_name === workflowName)) {
        return run.id;
      }
    }
    return null;
  }, [selectedRunId, selectedRun, recentRuns]);

  // Build available runs list for comparison dropdowns
  const availableRuns = useMemo(() => {
    return recentRuns.map((run) => ({
      id: run.id,
      label: `${run.task_name}${run.workflow_name ? ` (${run.workflow_name})` : ""} — ${new Date(run.created_at).toLocaleString()}`,
    }));
  }, [recentRuns]);

  return (
    <div className="flex flex-col h-full p-4 gap-4">
      <div className="flex-1 min-h-0">
        <TraceViewerWidget
          executionId={selectedRunId}
          baselineExecutionId={baselineExecutionId}
          availableRuns={availableRuns}
          height={window.innerHeight - 140}
        />
      </div>
    </div>
  );
};
