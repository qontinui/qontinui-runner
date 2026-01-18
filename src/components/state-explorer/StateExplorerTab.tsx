/**
 * StateExplorerTab.tsx
 *
 * State exploration history tab - shows past exploration runs.
 * To create and run new explorations, use the Library > State Explorer section.
 */

import { useState, useCallback } from "react";
import { Activity } from "lucide-react";
import { ExplorationHistory } from "./ExplorationHistory";
import { ExplorationReport } from "./ExplorationReport";
import { useRunSelectionOptional } from "../../contexts/RunSelectionContext";

type ExplorationView = "history" | "report";

interface StateExplorerTabProps {
  /** Optional callback when AI prompt should be shown */
  onShowAiPrompt?: (prompt: string) => void;
}

export function StateExplorerTab({ onShowAiPrompt }: StateExplorerTabProps) {
  const [activeView, setActiveView] = useState<ExplorationView>("history");
  const [selectedRunId, setSelectedRunId] = useState<string | null>(null);

  // Use RunSelectionContext if available
  const runSelection = useRunSelectionOptional();
  const selectedRun = runSelection?.selectedRun;

  // No run selected state (when context is present but no run selected)
  if (runSelection && !selectedRun) {
    return (
      <div className="h-full flex flex-col items-center justify-center text-muted-foreground p-8">
        <Activity className="w-12 h-12 mb-4 opacity-50" />
        <p className="text-lg font-medium">No Run Selected</p>
        <p className="text-sm mt-2 text-center max-w-md">
          Select a run from the Run Dashboard to view exploration history.
        </p>
      </div>
    );
  }

  // Handle viewing a specific report
  const handleViewReport = useCallback((runId: string) => {
    setSelectedRunId(runId);
    setActiveView("report");
  }, []);

  // Handle going back from report to history
  const handleBackToHistory = useCallback(() => {
    setSelectedRunId(null);
    setActiveView("history");
  }, []);

  // If viewing a report, show the report view
  if (activeView === "report" && selectedRunId) {
    return (
      <ExplorationReport
        runId={selectedRunId}
        onBack={handleBackToHistory}
        onShowAiPrompt={onShowAiPrompt}
      />
    );
  }

  // Show history view directly
  return (
    <div className="h-full flex flex-col overflow-hidden">
      <ExplorationHistory onSelectRun={handleViewReport} />
    </div>
  );
}
