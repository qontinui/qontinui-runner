/**
 * StatisticsTab.tsx
 *
 * Main statistics dashboard showing:
 * - Config health overview (success rate, runs, timing)
 * - Flaky items list (unreliable transitions/templates)
 * - Run history with trend
 * - Error patterns
 * - Run details slide-out panel
 */

import { useState } from "react";
import { BarChart3, AlertTriangle, RefreshCw } from "lucide-react";
import { useConfigStatistics, useFlakyItems, useRecentRuns } from "../../hooks/useStatistics";
import { ConfigHealthPanel } from "./ConfigHealthPanel";
import { FlakyItemsList } from "./FlakyItemsList";
import { RunHistoryPanel } from "./RunHistoryPanel";
import { ErrorPatternsPanel } from "./ErrorPatternsPanel";
import { RunDetailsPanel } from "./RunDetailsPanel";
import type { RunDetails } from "../../types/statistics";

interface StatisticsTabProps {
  configId: string | null;
  configName?: string;
}

export function StatisticsTab({ configId, configName }: StatisticsTabProps) {
  const [selectedRun, setSelectedRun] = useState<RunDetails | null>(null);
  const { data: stats, isLoading, error, refetch } = useConfigStatistics(configId);
  const { data: flakyItems } = useFlakyItems(configId);
  const { data: recentRuns } = useRecentRuns(configId, 50);

  if (!configId) {
    return (
      <div className="h-full flex items-center justify-center text-muted-foreground">
        <div className="text-center">
          <BarChart3 className="w-12 h-12 mx-auto mb-4 opacity-50" />
          <p>Load a configuration to view statistics</p>
        </div>
      </div>
    );
  }

  if (isLoading) {
    return (
      <div className="h-full flex items-center justify-center">
        <RefreshCw className="w-6 h-6 animate-spin text-muted-foreground" />
      </div>
    );
  }

  if (error || !stats) {
    return (
      <div className="h-full flex items-center justify-center text-muted-foreground">
        <div className="text-center">
          <AlertTriangle className="w-12 h-12 mx-auto mb-4 opacity-50" />
          <p>No statistics available for this config</p>
          <p className="text-sm mt-2">Run some workflows to start collecting data</p>
        </div>
      </div>
    );
  }

  return (
    <div className="h-full flex overflow-hidden">
      {/* Main content */}
      <div
        className={`flex-1 overflow-auto p-4 space-y-4 transition-all ${selectedRun ? "pr-0" : ""}`}
      >
        {/* Header */}
        <div className="flex items-center justify-between">
          <div>
            <h2 className="text-lg font-semibold">Statistics</h2>
            <p className="text-sm text-muted-foreground">{configName || configId}</p>
          </div>
          <button
            onClick={() => refetch()}
            className="p-2 rounded-md hover:bg-muted transition-colors"
            title="Refresh statistics"
          >
            <RefreshCw className="w-4 h-4" />
          </button>
        </div>

        {/* Grid layout */}
        <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
          {/* Config Health */}
          <ConfigHealthPanel stats={stats} />

          {/* Flaky Items */}
          <FlakyItemsList items={flakyItems || []} />

          {/* Run History */}
          <RunHistoryPanel runs={recentRuns || []} onRunClick={setSelectedRun} />

          {/* Error Patterns */}
          <ErrorPatternsPanel patterns={stats.error_patterns} />
        </div>
      </div>

      {/* Run Details Slide-out */}
      {selectedRun && (
        <div className="w-96 border-l border-border bg-background shadow-lg flex-shrink-0">
          <RunDetailsPanel run={selectedRun} onClose={() => setSelectedRun(null)} />
        </div>
      )}
    </div>
  );
}
