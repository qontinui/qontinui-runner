/**
 * VerificationTab.tsx
 *
 * Verification history tab - shows past verification runs.
 * To create and run new verifications, use the Library > Verifications section.
 */

import { useState, useCallback } from "react";
import { Activity } from "lucide-react";
import { VerificationHistory } from "./VerificationHistory";
import { VerificationReport } from "./VerificationReport";
import { useRunSelectionOptional } from "../../contexts/RunSelectionContext";

type VerificationView = "history" | "report";

interface VerificationTabProps {
  /** Optional callback when AI prompt should be shown */
  onShowAiPrompt?: (prompt: string) => void;
}

export function VerificationTab({ onShowAiPrompt }: VerificationTabProps) {
  const [activeView, setActiveView] = useState<VerificationView>("history");
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
          Select a run from the Run Dashboard to view verification history.
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
      <VerificationReport
        runId={selectedRunId}
        onBack={handleBackToHistory}
        onShowAiPrompt={onShowAiPrompt}
      />
    );
  }

  // Show history view directly
  return (
    <div className="h-full flex flex-col overflow-hidden">
      <VerificationHistory onSelectRun={handleViewReport} />
    </div>
  );
}
