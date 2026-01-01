/**
 * VerificationTab.tsx
 *
 * Main verification tab that manages navigation between:
 * - VerificationPanel (run new verification)
 * - VerificationHistory (view past runs)
 * - VerificationReport (view specific report)
 */

import { useState, useCallback } from "react";
import * as Tabs from "@radix-ui/react-tabs";
import { Play, History } from "lucide-react";
import { VerificationPanel } from "./VerificationPanel";
import { VerificationHistory } from "./VerificationHistory";
import { VerificationReport } from "./VerificationReport";

type VerificationView = "run" | "history" | "report";

interface VerificationTabProps {
  /** Optional callback when AI prompt should be shown */
  onShowAiPrompt?: (prompt: string) => void;
}

export function VerificationTab({ onShowAiPrompt }: VerificationTabProps) {
  const [activeView, setActiveView] = useState<VerificationView>("run");
  const [selectedRunId, setSelectedRunId] = useState<string | null>(null);

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

  const tabTriggerClass = `
    flex items-center gap-2 px-4 py-2.5 text-sm font-medium
    border-b-2 transition-colors
    data-[state=active]:border-primary data-[state=active]:text-primary
    data-[state=inactive]:border-transparent data-[state=inactive]:text-muted-foreground
    data-[state=inactive]:hover:text-foreground data-[state=inactive]:hover:bg-muted/30
  `;

  return (
    <div className="h-full flex flex-col overflow-hidden">
      <Tabs.Root
        value={activeView}
        onValueChange={(v) => setActiveView(v as VerificationView)}
        className="flex-1 flex flex-col min-h-0"
      >
        {/* Tab Navigation */}
        <Tabs.List className="flex border-b border-border bg-muted/20 px-4 flex-shrink-0">
          <Tabs.Trigger value="run" className={tabTriggerClass}>
            <Play className="w-4 h-4" />
            Run Verification
          </Tabs.Trigger>
          <Tabs.Trigger value="history" className={tabTriggerClass}>
            <History className="w-4 h-4" />
            History
          </Tabs.Trigger>
        </Tabs.List>

        {/* Tab Content */}
        <div className="flex-1 min-h-0 overflow-hidden">
          <Tabs.Content
            value="run"
            className="h-full outline-none overflow-hidden data-[state=inactive]:hidden"
          >
            <VerificationPanel onViewReport={handleViewReport} />
          </Tabs.Content>

          <Tabs.Content
            value="history"
            className="h-full outline-none overflow-hidden data-[state=inactive]:hidden"
          >
            <VerificationHistory onSelectRun={handleViewReport} />
          </Tabs.Content>
        </div>
      </Tabs.Root>
    </div>
  );
}
